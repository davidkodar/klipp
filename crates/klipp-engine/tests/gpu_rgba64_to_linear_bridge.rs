//! `GpuEffectsPipeline::bridge_rgba64_to_linear_into_chain` against the
//! CPU expansion it replaces.
//!
//! The sibling of `gpu_u8_to_linear_bridge`, one sample wider. This is
//! the export's input transform: a software decoder's scaler hands back
//! sRGB-encoded RGBA64, and rather than expanding 8.3 million pixels
//! into a 133 MB f32 buffer on the CPU and uploading that, the samples
//! go up as they stand and this shader expands them in place.
//!
//! Why 16 bits and not 8, when the footage is 8-bit: YUV→RGB is a
//! matrix, so its output does not fit back into 8 bits. Measured on real
//! 8-bit H.264, only 27% of the scaler's samples come back
//! byte-replicated and all 256 low-byte values occur. Narrowing here
//! would halve the upload again and quantise the delivered frame — the
//! low byte carries real picture. `the_low_byte_is_not_noise` below
//! pins that, so nobody re-derives the tempting wrong answer.

use klipp_effects::{gpu::GpuEffectsPipeline, gpu_context::GpuContext};

const W: u32 = 16;
const H: u32 = 16;

const IDENTITY: [[f32; 4]; 3] =
    [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];

/// Ramps in all three channels, with low bytes that differ from their
/// high bytes so a narrowing bug shows up rather than cancelling out.
/// The `0x0000 / 0x0A3F / 0x0A40 / 0xFFFF` spread straddles the sRGB
/// transfer's piecewise knee.
fn test_pattern(width: u32, height: u32) -> Vec<u16> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        for col in 0..width {
            let i = row * width + col;
            let r = (i * 257 + 13) as u16;
            let g = ((i * 631) & 0xFFFF) as u16;
            let b = (0xFFFF_u32 - (i * 211) % 0xFFFF) as u16;
            out.extend_from_slice(&[r, g, b, u16::MAX]);
        }
    }
    // The knee, exactly: 0.04045 * 65535 ≈ 2651 = 0x0A5B.
    out[0..4].copy_from_slice(&[0, 2651, 2652, u16::MAX]);
    out
}

/// What the CPU path computes: sRGB decode per channel over the full
/// 16-bit range, alpha left linear.
fn cpu_linear(rgba: &[u16], m: [[f32; 4]; 3]) -> Vec<f32> {
    let mut out = vec![0.0f32; rgba.len()];
    for (dst, px) in out.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
        let lin: Vec<f32> = (0..3)
            .map(|c| klipp_color::transfer::srgb_to_linear(px[c] as f32 / 65535.0))
            .collect();
        for row in 0..3 {
            dst[row] = m[row][0] * lin[0] + m[row][1] * lin[1] + m[row][2] * lin[2];
        }
        dst[3] = px[3] as f32 / 65535.0;
    }
    out
}

fn bridge(rgba: &[u16], m: [[f32; 4]; 3]) -> Option<Vec<f32>> {
    bridge_sized(rgba, W, H, m)
}

fn bridge_sized(rgba: &[u16], w: u32, h: u32, m: [[f32; 4]; 3]) -> Option<Vec<f32>> {
    // Dedicated context: cargo runs integration tests in parallel and
    // `shared()` caches one across them, which races descriptor-pool
    // exhaustion on NVIDIA Vulkan.
    let ctx = GpuContext::new()?;
    let mut pipeline = GpuEffectsPipeline::with_context(&ctx)?;

    let bytes: &[u8] = bytemuck::cast_slice(rgba);
    let input = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rgba64_bridge_test_input"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.queue.write_buffer(&input, 0, bytes);
    ctx.device.poll(wgpu::Maintain::Poll);

    assert!(
        pipeline.bridge_rgba64_to_linear_into_chain(&input, w, h, m),
        "bridge call must succeed"
    );
    Some(pipeline.download_chain_output_linear(true, (w * h) as usize))
}

/// The GPU expansion is the CPU expansion, to the bit: the bridge looks
/// each code up in the CPU's own decode of it, where the shader's `pow`
/// was up to 12 ULP off (#1451). A frame seeded on the GPU then meets a
/// step in the chain (a Posterize band edge on a code) as the CPU does.
#[test]
fn rgba64_to_linear_gpu_matches_cpu() {
    let rgba = test_pattern(W, H);
    let Some(gpu) = bridge(&rgba, IDENTITY) else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let cpu = cpu_linear(&rgba, IDENTITY);
    assert_eq!(gpu.len(), cpu.len(), "length mismatch");
    for (i, (g, c)) in gpu.iter().zip(cpu.iter()).enumerate() {
        assert!(g.to_bits() == c.to_bits(), "sample {i}: gpu={g:e} cpu={c:e}, not the CPU's to the bit");
    }
}

/// Every 16-bit code, in each colour channel and in alpha, decodes on
/// the GPU to the CPU's value exactly.
#[test]
fn every_code_decodes_as_on_the_cpu() {
    let rgba: Vec<u16> = (0..=u16::MAX).flat_map(|c| [c, u16::MAX - c, c.wrapping_mul(7), c]).collect();
    let Some(gpu) = bridge_sized(&rgba, 256, 256, IDENTITY) else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let cpu = cpu_linear(&rgba, IDENTITY);
    let off: Vec<_> = gpu.iter().zip(&cpu).enumerate().filter(|(_, (g, c))| g.to_bits() != c.to_bits()).map(|(i, (g, c))| (i, *g, *c)).collect();
    assert!(off.is_empty(), "{} samples the GPU decodes otherwise: {:?}", off.len(), &off[..off.len().min(8)]);
}

/// The basis change is applied, not carried and dropped. Without this
/// the shader would look right in a Rec.709 project and silently render
/// a wide-gamut one in the wrong primaries — the bug that made the
/// viewer's own display fast path 234 levels off.
#[test]
fn the_source_to_working_matrix_is_applied() {
    // A deliberately lopsided matrix: no row is a permutation of the
    // identity, so any mix-up of rows and columns changes the answer.
    let m = [
        [0.7, 0.2, 0.1, 0.0],
        [0.1, 0.8, 0.1, 0.0],
        [0.05, 0.15, 0.8, 0.0],
    ];
    let rgba = test_pattern(W, H);
    let Some(gpu) = bridge(&rgba, m) else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let cpu = cpu_linear(&rgba, m);
    for (i, (g, c)) in gpu.iter().zip(cpu.iter()).enumerate() {
        assert!((g - c).abs() < 0.003, "sample {i}: gpu={g:.6} cpu={c:.6}");
    }

    // And it is not the identity in disguise.
    let plain = cpu_linear(&rgba, IDENTITY);
    let changed = gpu.iter().zip(plain.iter()).filter(|(g, p)| (*g - *p).abs() > 0.01).count();
    assert!(changed > gpu.len() / 4, "the matrix barely moved the frame: {changed} samples");
}

#[test]
fn rgba64_to_linear_handles_zero_pixels() {
    let Some(ctx) = GpuContext::new() else { return };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else { return };
    let dummy = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    assert!(pipeline.bridge_rgba64_to_linear_into_chain(&dummy, 0, 0, IDENTITY));
}

/// Eight bytes a pixel, not four. A buffer sized for the u8 bridge must
/// be refused rather than read half a frame of garbage.
#[test]
fn rgba64_to_linear_rejects_short_input() {
    let Some(ctx) = GpuContext::new() else { return };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else { return };
    let four_bytes_per_pixel = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (W * H * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    assert!(!pipeline.bridge_rgba64_to_linear_into_chain(&four_bytes_per_pixel, W, H, IDENTITY));
}

/// The low byte of an RGBA64 sample carries picture, so the export
/// cannot narrow to 8 bits on the way to the GPU.
///
/// This is arithmetic on the sRGB transfer rather than a measurement of
/// ffmpeg, so it holds without a fixture: two 16-bit samples one low
/// byte apart are further apart in linear light than one 8-bit output
/// level in the darks, which is where banding is visible.
#[test]
fn the_low_byte_is_not_noise() {
    // Two samples sharing a high byte, differing only in the low byte.
    let a = klipp_color::transfer::srgb_to_linear(0x2000 as f32 / 65535.0);
    let b = klipp_color::transfer::srgb_to_linear(0x20FF as f32 / 65535.0);
    // One 8-bit display level around that brightness.
    let lo = klipp_color::transfer::srgb_to_linear(32.0 / 255.0);
    let hi = klipp_color::transfer::srgb_to_linear(33.0 / 255.0);
    assert!(
        (b - a).abs() > (hi - lo).abs() * 0.5,
        "dropping the low byte moves a dark sample by {:.6}, over half of the {:.6} \
         that one 8-bit level spans — it is signal, not rounding",
        (b - a).abs(),
        (hi - lo).abs(),
    );
}
