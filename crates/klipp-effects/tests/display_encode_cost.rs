//! What the viewer's display encode costs at 4K, GPU completion included:
//! the frame already in the chain, encoded into an `Rgba8Unorm` texture as
//! the zero-copy viewer takes it, no bytes read back. An explicit
//! measurement, not a timing assertion on shared hardware (#2072 measured
//! its dither with it).
//!
//! `cargo test -p klipp-effects --test display_encode_cost -- --ignored --nocapture`

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_context::GpuContext;
use wgpu::util::DeviceExt;

const W: u32 = 3840;
const H: u32 = 2160;

#[test]
#[ignore = "4K performance measurement"]
fn measure_4k_display_encode() {
    let Some(mut gpu) = GpuContext::shared().and_then(GpuEffectsPipeline::with_context) else {
        eprintln!("unverified: no usable GPU");
        return;
    };
    let device = gpu.device();
    let picture: Vec<f32> = (0..H)
        .flat_map(|y| (0..W).flat_map(move |x| {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            [0.02 + 0.03 * fx, 0.03 + 0.02 * fy, 0.04, 1.0]
        }))
        .collect();
    let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("display_encode_cost_input"),
        contents: bytemuck::cast_slice(&picture),
        usage: wgpu::BufferUsages::COPY_SRC,
    });
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewer"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    });
    assert!(gpu.copy_linear_into_chain(&input, W, H));
    device.poll(wgpu::Maintain::Wait);
    let mut samples = Vec::new();
    for n in 0..60 {
        let start = std::time::Instant::now();
        let none = gpu.encode_chain_to_display(true, W, H, None, false, None, Some(&target), false).expect("the encode ran");
        assert!(none.is_none());
        device.poll(wgpu::Maintain::Wait);
        if n >= 10 {
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    samples.sort_by(f64::total_cmp);
    let p = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    eprintln!(
        "4K display encode into the viewer texture, GPU completion included, {} frames: median {:.3} ms, p95 {:.3} ms",
        samples.len(),
        p(0.5),
        p(0.95),
    );
}
