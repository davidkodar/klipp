//! `GpuEffectsPipeline::bridge_u8_to_linear_into_chain` byte-identity
//! against the CPU sRGB-u8 → linear-f32 path. This bridge is the
//! missing link for phase 4d's GPU-resident decode → grade flow: the
//! decoder hands off an `Arc<wgpu::Buffer>` of u8 RGBA; the bridge
//! converts it into the grade chain's `buf_a` on-GPU, no CPU
//! round-trip.

use klipp_effects::{
    gpu::GpuEffectsPipeline,
    gpu_context::GpuContext,
    srgb_u8_to_linear_f32_rgba as u8_rgba_to_linear,
};

const W: u32 = 16;
const H: u32 = 16;

/// A deterministic sRGB pattern — ramps in all three channels plus a
/// known-boundary row (0, 127, 128, 255) that stresses the sRGB
/// decode transfer's piecewise branch.
fn test_pattern(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        for col in 0..width {
            let r = ((col * 17 + row) & 0xFF) as u8;
            let g = ((col * 3 + row * 13) & 0xFF) as u8;
            let b = ((col + row * 23) & 0xFF) as u8;
            out.extend_from_slice(&[r, g, b, 255]);
        }
    }
    out
}

#[test]
fn u8_to_linear_gpu_matches_cpu_exactly() {
    // Dedicated context — cargo runs integration tests in parallel
    // and `shared()` caches a single one across tests, which races
    // the descriptor-pool exhaustion on NVIDIA Vulkan.
    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else {
        eprintln!("skipping: GpuEffectsPipeline unavailable");
        return;
    };

    let bytes = test_pattern(W, H);

    // CPU reference: what the decoder's downstream code does today.
    let cpu_linear = u8_rgba_to_linear(&bytes);

    // Upload u8 bytes to an external GPU buffer that imitates the
    // NV12 converter's on-GPU output (STORAGE | COPY_SRC, no
    // MAP_READ — callers bind it as a compute-shader input).
    let input_size = bytes.len() as u64;
    let input_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("u8_bridge_test_input"),
        size: input_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.queue.write_buffer(&input_buf, 0, &bytes);
    ctx.device.poll(wgpu::Maintain::Poll);

    // Bridge into buf_a (linear f32), then download for comparison.
    assert!(
        pipeline.bridge_u8_to_linear_into_chain(&input_buf, W, H),
        "bridge call must succeed"
    );
    let gpu_linear = pipeline.download_chain_output_linear(true, (W * H) as usize);

    assert_eq!(gpu_linear.len(), cpu_linear.len(), "length mismatch");

    // Bit for bit: the bridge looks each code up in the CPU's own decode
    // of it, where the shader's `pow` was only near it. A frame seeded on
    // the GPU then meets a step in the chain (a Posterize band edge on a
    // code) exactly as the CPU does.
    for (i, (g, c)) in gpu_linear.iter().zip(cpu_linear.iter()).enumerate() {
        assert!(g.to_bits() == c.to_bits(), "value {i}: gpu={g:e} cpu={c:e}, not the CPU's to the bit");
    }
}

/// Every 8-bit code, in each channel, decodes on the GPU to the CPU's
/// value exactly.
#[test]
fn every_code_decodes_as_on_the_cpu() {
    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else {
        eprintln!("skipping: GpuEffectsPipeline unavailable");
        return;
    };
    let bytes: Vec<u8> = (0..=255u8).flat_map(|c| [c, 255 - c, c.wrapping_mul(7), c]).collect();
    let cpu_linear = u8_rgba_to_linear(&bytes);
    let input_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("u8_bridge_codes"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.queue.write_buffer(&input_buf, 0, &bytes);
    ctx.device.poll(wgpu::Maintain::Poll);
    assert!(pipeline.bridge_u8_to_linear_into_chain(&input_buf, 256, 1), "bridge call must succeed");
    let gpu_linear = pipeline.download_chain_output_linear(true, 256);
    let off: Vec<_> = gpu_linear.iter().zip(&cpu_linear).enumerate().filter(|(_, (g, c))| g.to_bits() != c.to_bits()).map(|(i, (g, c))| (i, *g, *c)).collect();
    assert!(off.is_empty(), "codes the GPU decodes otherwise: {:?}", &off[..off.len().min(8)]);
}

#[test]
fn u8_to_linear_handles_zero_pixels() {
    let Some(ctx) = GpuContext::new() else { return; };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else { return; };

    let dummy = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16, // non-zero to satisfy wgpu's buffer min size
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    assert!(pipeline.bridge_u8_to_linear_into_chain(&dummy, 0, 0));
}

#[test]
fn u8_to_linear_rejects_short_input() {
    let Some(ctx) = GpuContext::new() else { return; };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else { return; };

    // Declare 16×16 (= 1024 bytes), supply only 4 bytes.
    let tiny = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 4,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    assert!(!pipeline.bridge_u8_to_linear_into_chain(&tiny, 16, 16));
}
