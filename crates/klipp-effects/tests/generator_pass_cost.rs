//! What the native living gradient's generator pass costs at 4K, GPU
//! completion included: the pass alone, into a stack layer, no bytes read
//! back. An explicit measurement, not a timing assertion on shared
//! hardware (#2073 measured its grain with it: grain 0 skips the grain
//! entirely, so the two rows are the pass without and with it).
//!
//! `cargo test -p klipp-effects --test generator_pass_cost -- --ignored --nocapture`

use klipp_effects::generator_grain::DEFAULT_GENERATOR_GRAIN;
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_context::GpuContext;

const W: u32 = 3840;
const H: u32 = 2160;

#[test]
#[ignore = "4K performance measurement"]
fn measure_4k_living_gradient_pass() {
    let Some(mut gpu) = GpuContext::shared().and_then(GpuEffectsPipeline::with_context) else {
        eprintln!("unverified: no usable GPU");
        return;
    };
    let device = gpu.device();
    for round in 0..3 {
        for grain in [0.0, DEFAULT_GENERATOR_GRAIN] {
            let mut samples = Vec::new();
            for n in 0..60 {
                assert!(gpu.stack_begin(W, H, 1));
                let start = std::time::Instant::now();
                assert!(gpu.stack_layer_living_gradient(0, [12, 23, 32, 255], [29, 60, 65, 255], 25.0, 1.5, 0.18, grain, n as f64 / 30.0));
                gpu.stack_end();
                device.poll(wgpu::Maintain::Wait);
                if n >= 10 {
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                }
            }
            samples.sort_by(f64::total_cmp);
            let p = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
            eprintln!(
                "round {round}: 4K living gradient pass, grain {grain}, GPU completion included, {} frames: median {:.3} ms, p95 {:.3} ms",
                samples.len(),
                p(0.5),
                p(0.95),
            );
        }
    }
}
