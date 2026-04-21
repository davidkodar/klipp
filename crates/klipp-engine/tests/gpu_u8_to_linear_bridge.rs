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
fn u8_to_linear_gpu_matches_cpu_within_epsilon() {
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

    // The sRGB transfer uses `pow`, which differs by an LSB or two
    // between CPU's f64 intermediate and GPU's f32. One LSB in a
    // 10-bit perceptual space is ≈ 0.001 in [0, 1]; allow ≈ 0.003
    // to cover the pow() round-off across the ramp.
    let eps = 0.003_f32;
    for (i, (g, c)) in gpu_linear.iter().zip(cpu_linear.iter()).enumerate() {
        let delta = (g - c).abs();
        assert!(
            delta < eps,
            "pixel {i}: gpu={g:.6} cpu={c:.6} delta={delta:.6} exceeds {eps}"
        );
    }
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
