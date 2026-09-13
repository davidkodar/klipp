//! `GpuEffectsPipeline::encode_grade_to_texture` byte-identity test.
//!
//! Proves that running the chain's final linear → sRGB-u8 encode
//! directly into a `wgpu::Texture` yields the same bytes the current
//! `linear_rgba_to_u8_gpu` produces via a CPU round-trip. This is
//! the missing piece for handing a graded frame to the viewer with
//! zero PCIe download on the return path.

use klipp_effects::{
    gpu::GpuEffectsPipeline,
    gpu_context::GpuContext,
    linear_f32_rgba_to_srgb_u8,
};

// Width chosen so the byte pitch (W*4) is a multiple of
// `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT` (256) — required by
// `copy_buffer_to_texture`. Real viewer frames are always
// ≥ 1920 px so this constraint is moot in production.
const W: u32 = 64;
const H: u32 = 32;

/// Synthetic linear-f32 RGBA, varied in every channel the encode
/// touches — including alpha, which the encode treats differently from
/// the other three.
///
/// This fixture used to hold `let a = if col == 0 && row == 0 { 1.0 }
/// else { 1.0 }` — both arms the same, so alpha was constant across the
/// frame and its handling was untested. The comment above it ("one
/// highlight pixel to force clamp") had drifted off the line below,
/// which is the one that actually forces a clamp. Blue was a constant
/// 0.5, and only red ever left [0, 1], so the clamp was tested on one
/// channel of four.
///
/// What each part is for:
/// * the two ramps span [0, 1] so the sRGB curve's piecewise split is
///   crossed, with an explicit band of small values around the knee at
///   0.0031308 — a ramp over 64 columns steps straight past it;
/// * each of R, G and B exceeds 1.0 somewhere and goes negative
///   somewhere, so the clamp is checked per channel rather than on red
///   alone. Negatives are real here: a wide-gamut colour outside the
///   display gamut arrives below zero;
/// * alpha varies, and is deliberately pushed out of range too. The
///   encode clamps alpha but must *not* put it through the sRGB curve —
///   it is a coefficient, not a colour — so a constant alpha could not
///   tell the two treatments apart.
fn test_linear(width: u32, height: u32) -> Vec<f32> {
    let mut v = Vec::with_capacity((width * height * 4) as usize);
    let last_col = (width - 1).max(1);
    let last_row = (height - 1).max(1);
    for row in 0..height {
        for col in 0..width {
            let fx = (col as f32) / (last_col as f32);
            let fy = (row as f32) / (last_row as f32);
            let mut r = fx;
            let mut g = fy;
            // Blue varies rather than sitting at 0.5, so it exercises the
            // curve like the other two.
            let mut b = 1.0 - (fx + fy) * 0.5;
            // Alpha varies independently of the colour channels, so a
            // shader that derived it from one of them would show up.
            let mut a = 0.25 + 0.75 * (1.0 - fy);

            // Row 0: the knee. Values either side of 0.0031308, where the
            // sRGB curve changes from linear to a power.
            if row == 0 {
                let knee = [0.0, 0.001, 0.003, 0.0031308, 0.0032, 0.004, 0.01, 0.05];
                let k = knee[(col as usize) % knee.len()];
                r = k;
                g = k;
                b = k;
            }

            // Last column: above one in every channel, so the top clamp
            // is checked on each.
            if col == last_col {
                r = 1.5;
                g = 2.2;
                b = 8.0;
                a = 1.4;
            }
            // Last row: below zero in every channel, so the bottom clamp
            // is too.
            if row == last_row {
                r = -0.3;
                g = -0.01;
                b = -2.0;
                a = -0.5;
            }
            v.extend_from_slice(&[r, g, b, a]);
        }
    }
    v
}

#[test]
fn encode_grade_to_texture_matches_cpu() {
    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else {
        eprintln!("skipping: GpuEffectsPipeline unavailable");
        return;
    };

    let linear = test_linear(W, H);

    // Expected bytes via the CPU reference encoder.
    let expected = linear_f32_rgba_to_srgb_u8(&linear);

    // Seed `buf_a` with the linear data, exactly like the executor's
    // chain does at the end of a grade pass.
    let byte_size = (W * H * 16) as u64;
    pipeline.ensure_chain_buffers(byte_size);
    pipeline.upload_chain_input_linear(&linear);

    // Target texture: Rgba8Unorm so our already-sRGB-encoded bytes
    // aren't double-decoded.
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("grade_to_texture_test_target"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    assert!(
        pipeline.encode_grade_to_texture(true, W, H, &texture),
        "encode_grade_to_texture must succeed on a well-formed target"
    );

    // Copy the texture back to a staging buffer for read-verify.
    let staging_size = (W * H * 4) as u64;
    let staging = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("grade_to_texture_test_staging"),
        size: staging_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = ctx.device.create_command_encoder(
        &wgpu::CommandEncoderDescriptor { label: Some("grade_to_texture_test_readback") },
    );
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &staging,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(W * 4),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    ctx.queue.submit(std::iter::once(encoder.finish()));

    let slice = staging.slice(..staging_size);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    ctx.device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let data = slice.get_mapped_range();
    let actual: Vec<u8> = data.to_vec();
    drop(data);
    staging.unmap();

    assert_eq!(actual.len(), expected.len(), "texture byte size mismatch");
    // The GPU shader does `pow` in f32; the CPU reference does it in
    // f64 → rounded. Allow 1 LSB of sRGB code value.
    let mut max_delta = 0i32;
    for (i, (a, b)) in actual.iter().zip(expected.iter()).enumerate() {
        let d = (*a as i32 - *b as i32).abs();
        if d > max_delta { max_delta = d; }
        assert!(
            d <= 1,
            "byte {i}: gpu-texture={a} cpu-ref={b} delta={d} > 1 LSB"
        );
    }
    eprintln!("encode_grade_to_texture identity: max byte delta = {max_delta}");
}

#[test]
fn encode_grade_to_texture_rejects_wrong_format() {
    let Some(ctx) = GpuContext::new() else { return; };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else { return; };
    pipeline.ensure_chain_buffers((W * H * 16) as u64);
    pipeline.upload_chain_input_linear(&vec![0.5_f32; (W * H * 4) as usize]);

    // sRGB format — wrong (would cause double-decode on sample).
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    assert!(
        !pipeline.encode_grade_to_texture(true, W, H, &texture),
        "sRGB-format target must be rejected"
    );
}
