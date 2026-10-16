//! What the Directional Blur's pass costs at 4K on the GPU, completion
//! included: the frame resident, one recorded pass, no bytes read back.
//! An explicit measurement, not a timing assertion on shared hardware
//! (#2021 measured its premultiplied mean with it, on an opaque frame and
//! one with transparency).
//!
//! `cargo test -p klipp-effects --test directional_blur_cost -- --ignored --nocapture`

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_context::GpuContext;
use klipp_effects::BuiltinEffect;

const W: u32 = 3840;
const H: u32 = 2160;

#[test]
#[ignore = "4K performance measurement"]
fn measure_4k_directional_blur_pass() {
    let Some(mut gpu) = GpuContext::shared().and_then(GpuEffectsPipeline::with_context) else {
        eprintln!("unverified: no usable GPU");
        return;
    };
    let device = gpu.device();
    let blur = [BuiltinEffect::DirectionalBlur { angle: 20.0, distance: 40.0, samples: 24 }];
    gpu.ensure_chain_buffers(u64::from(W * H) * 16);
    gpu.ensure_recording_buffers(W, H);
    for round in 0..3 {
        for (what, alpha) in [("opaque", 1.0f32), ("half transparent", 0.5)] {
            let px: Vec<f32> = (0..W * H).flat_map(|i| {
                let v = (i % 997) as f32 / 997.0;
                [v, 0.5 * v, 1.0 - v, if i % 3 == 0 { alpha } else { 1.0 }]
            }).collect();
            gpu.upload_chain_input_linear(&px);
            let mut samples = Vec::new();
            for n in 0..60 {
                let start = std::time::Instant::now();
                gpu.begin_chain();
                let ran = gpu.chain_run_effects(&blur, true, W, H);
                gpu.submit_chain();
                device.poll(wgpu::Maintain::Wait);
                assert!(ran.complete);
                if n >= 10 {
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                }
            }
            samples.sort_by(f64::total_cmp);
            let p = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
            eprintln!("round {round}: 4K directional blur (24 taps), {what}, GPU completion included, {} frames: median {:.3} ms, p95 {:.3} ms", samples.len(), p(0.5), p(0.95));
        }
    }
}
