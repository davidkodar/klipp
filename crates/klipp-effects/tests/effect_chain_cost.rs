//! L6 (node-graph M2): what an eight-effect chain costs at 4K through
//! `apply_effects`, less the upload and download it shares with a
//! one-effect chain. Recorded effects (step 7) make the chain one submit; this shows
//! that is no slower than one submit per effect was.
//!
//! At the step 7 port (RTX 4080, 4K, three runs each, median of 15): eight
//! effects 30.05 ms in 9 submits before, 29.93 ms in 2 after. The upload
//! and download dominate this path; the submits saved show where the
//! chain stays resident.
//!
//! An ignored probe: `cargo test -p klipp-effects --test effect_chain_cost -- --ignored --nocapture`.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::BuiltinEffect as E;

#[test]
#[ignore]
fn eight_effect_chain_at_4k() {
    let Some(mut g) = GpuEffectsPipeline::new() else {
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let (w, h) = (3840, 2160);
    let px: Vec<f32> = (0..u64::from(w * h * 4)).map(|i| ((i * 7919) % 1000) as f32 / 1000.0).collect();
    let chain = [
        E::Gamma { gamma: 1.1 },
        E::HSVAdjust { hue_shift: 6.0, saturation: 1.1, value: 1.0 },
        E::Vignette { amount: 0.3, size: 0.7, softness: 0.4, roundness: 0.5 },
        E::Sharpen { amount: 0.4 },
        E::ChromaticAberration { red_shift: 1.0, blue_shift: -1.0, center_x: 0.5, center_y: 0.5 },
        E::Levels { in_black: 0.02, in_white: 0.98, gamma: 1.0, out_black: 0.0, out_white: 1.0 },
        E::Mirror { horizontal: 1, vertical: 0 },
        E::Opacity { value: 0.9 },
    ];
    let mut time = |fx: &[E]| {
        for _ in 0..3 {
            let _ = g.apply_effects(fx, &px, w, h);
        }
        let submits = g.submit_count();
        let mut ms: Vec<f64> = (0..15)
            .map(|_| {
                let t = std::time::Instant::now();
                let _ = g.apply_effects(fx, &px, w, h);
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        (ms[ms.len() / 2], (g.submit_count() - submits) / 15)
    };
    // An empty list never reaches the GPU, so the base is one effect:
    // the same upload and download, and one pass.
    let (one, one_submits) = time(&chain[7..]);
    let (full, full_submits) = time(&chain);
    eprintln!("L6 4K: one effect {one:.2} ms ({one_submits} submits), eight {full:.2} ms ({full_submits} submits), the seven more {:.2} ms", full - one);
}
