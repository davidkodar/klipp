//! What the box passes of a Gaussian blur and a glow cost at 1080p and
//! 4K on the GPU, against one plain pass over the same frame: the frame
//! resident, GPU completion included, nothing read back. An explicit
//! measurement, ignored in the suite so timing on shared hardware can't
//! fail it.
//!
//! On the Apple GPU (M1, Mesa) every tap was a read from the frame, and
//! the op's default blur (a sigma of 10: boxes 9, 9 and 10, six passes)
//! cost 31 ms at 1080p, 22 plain passes; a glow 40 ms. The passes now
//! read their stretch of the frame once into the workgroup's memory.
//!
//! `cargo test --release -p klipp-effects --test box_blur_cost -- --ignored --nocapture`

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_context::GpuContext;
use klipp_effects::BuiltinEffect as E;

/// The median of 32 timed runs of `fx`, after 8 to warm up.
fn median_ms(gpu: &mut GpuEffectsPipeline, fx: &[E], w: u32, h: u32) -> f64 {
    let device = gpu.device();
    let mut ms = Vec::new();
    for n in 0..40 {
        let start = std::time::Instant::now();
        gpu.begin_chain();
        let ran = gpu.chain_run_effects(fx, true, w, h);
        gpu.submit_chain();
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        assert!(ran.complete, "{fx:?}");
        if n >= 8 {
            ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    ms.sort_by(f64::total_cmp);
    ms[ms.len() / 2]
}

/// A sigma-10 blur, six box passes, costs no more than ten plain passes.
#[test]
#[ignore = "GPU performance measurement"]
fn a_blur_costs_about_its_passes() {
    let Some(mut gpu) = GpuContext::shared().and_then(GpuEffectsPipeline::with_context) else {
        eprintln!("unverified: no usable GPU");
        return;
    };
    let mut default_ratio = 0.0;
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        gpu.ensure_chain_buffers(u64::from(w * h) * 16);
        gpu.ensure_recording_buffers(w, h);
        let px: Vec<f32> = (0..w * h).flat_map(|i| {
            let v = (i % 997) as f32 / 997.0;
            [v, 0.5 * v, 1.0 - v, 1.0]
        }).collect();
        gpu.upload_chain_input_linear(&px);
        let plain = median_ms(&mut gpu, &[E::Opacity { value: 0.9 }], w, h);
        eprintln!("{w}x{h} one plain pass: {plain:.2} ms");
        for sigma in [3.0f32, 6.0, 8.0, 10.0, 20.0, 40.0] {
            let ms = median_ms(&mut gpu, &[E::GaussianBlur { radius: sigma }], w, h);
            let boxes = klipp_effects::blur::boxes_for_gaussian(sigma);
            eprintln!("{w}x{h} blur sigma {sigma:4} boxes {boxes:?}: {ms:.2} ms, {:.1} plain passes", ms / plain);
            if w == 1920 && sigma == 10.0 {
                default_ratio = ms / plain;
            }
        }
        let glow = median_ms(&mut gpu, &[E::Glow { radius: 30.0, intensity: 0.4, threshold: 0.6 }], w, h);
        eprintln!("{w}x{h} glow (the op's default): {glow:.2} ms, {:.1} plain passes", glow / plain);
    }
    assert!(default_ratio <= 10.0, "a sigma-10 blur at 1080p cost {default_ratio:.1} plain passes");
}
