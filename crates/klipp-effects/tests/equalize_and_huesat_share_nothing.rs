//! Hue/sat curves and the equalize effect, on one frame.
//!
//! The two are unrelated — one is a grade stage reached through
//! `apply_huesat_gpu`, the other a `BuiltinEffect` — and until this
//! file there was nothing that ran both. The parity suites take one
//! effect, or one node kind, at a time.
//!
//! That mattered because they used to share a GPU buffer: equalize
//! wrote its 256-entry CDF over the start of the hue/sat LUT. Equalize
//! has its own buffer now.
//!
//! What these tests do NOT do is catch that sharing. Checked, not
//! assumed: with the buffer re-shared *and* the flush in `upload`
//! removed, both still pass. Two independent things keep the hazard
//! out of reach, and only one of them was ever written down —
//!
//!   * `upload` flushes before every write, so a recorded hue/sat
//!     dispatch is submitted before the CDF lands on the LUT; and
//!   * the equalize arm's first step was a `download` of the frame to
//!     build its histogram, which has to submit and wait for all prior
//!     work anyway. (Since #1215 the chain builds the CDF on the GPU;
//!     only the break path still reads the frame back.)
//!
//! So the sharing was safe twice over, by accident twice over. The
//! buffers are separate now because an invariant resting on two
//! incidental behaviours, neither stated where the sharing happened,
//! is a trap rather than a design. These tests are the combination
//! coverage that was missing, not a guard on that invariant; the guard
//! is that `equalize_cdf_buf` exists and is read, which the compiler
//! and `-D warnings` enforce on anyone who tries to re-share it.

use klipp_effects::cpu_grade::{huesat, HUESAT_LUT_LEN};
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::{apply_effects, BuiltinEffect};

const W: u32 = 128;
const H: u32 = 72;

/// A hue sweep across, brightness down: the hue/sat curves have every
/// hue to act on and the equalise histogram has a spread to flatten.
fn frame() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let h = x as f32 / (W - 1) as f32 * 6.0;
            let l = 0.08 + 0.7 * (y as f32 / (H - 1) as f32);
            let i = h.floor() as i32 % 6;
            let f = h - h.floor();
            let (r, g, b) = match i {
                0 => (1.0, f, 0.0),
                1 => (1.0 - f, 1.0, 0.0),
                2 => (0.0, 1.0, f),
                3 => (0.0, 1.0 - f, 1.0),
                4 => (f, 0.0, 1.0),
                _ => (1.0, 0.0, 1.0 - f),
            };
            v.extend_from_slice(&[r * l, g * l, b * l, 1.0]);
        }
    }
    v
}

/// Seven curves, 256 entries each. Curve 0 shifts hue, the rest sit at
/// their identity value — enough to make the LUT's contents matter
/// without needing the whole grade model here.
fn hue_shift_lut() -> [f32; HUESAT_LUT_LEN] {
    let mut lut = [0.5f32; HUESAT_LUT_LEN];
    for (i, v) in lut.iter_mut().enumerate().take(256) {
        // Hue vs hue: a gentle S, so different hues move differently
        // and a clobbered LUT cannot pass for this one.
        let t = i as f32 / 255.0;
        *v = (0.5 + 0.18 * (t * std::f32::consts::TAU).sin()).clamp(0.0, 1.0);
    }
    lut
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max)
}

#[test]
fn a_huesat_grade_followed_by_equalize_matches_the_cpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let input = frame();
    let lut = hue_shift_lut();
    let fx = [BuiltinEffect::Equalize { strength: 1.0 }];

    // GPU: the grade stage, then the effect — the order a clip with
    // both actually runs them, and the order that used to leave the
    // effect's CDF sitting on the grade's LUT.
    let graded = gpu.apply_huesat_gpu(&input, W, H, 1.0, &lut, 0b1);
    let on_gpu = gpu.apply_effects(&fx, &graded, W, H);

    // CPU: the same two, through their twins.
    let mut cpu = input.clone();
    huesat(&mut cpu, &lut, 1.0, 0b1);
    let cpu = apply_effects(&fx, cpu, W, H);

    let d = worst(&cpu, &on_gpu);
    eprintln!("huesat then equalize: worst CPU vs GPU = {d:e} linear");
    assert!(d < 2e-3, "CPU and GPU disagree by {d}");

    // Both stages have to have done something, or two pass-throughs
    // would agree and prove nothing.
    assert!(
        worst(&input, &graded) > 0.01,
        "the hue/sat curves left the frame alone",
    );
    assert!(
        worst(&graded, &on_gpu) > 0.01,
        "equalize left the graded frame alone",
    );
}

/// A grade gives the same picture whether or not an equalize ran in
/// between.
///
/// This is the property the separate buffers guarantee. It cannot fail
/// today — see the note at the top of this file — so read it as the
/// invariant written down in executable form rather than as a trap
/// waiting to spring.
#[test]
fn the_two_passes_survive_each_other_in_either_order() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let input = frame();
    let lut = hue_shift_lut();
    let fx = [BuiltinEffect::Equalize { strength: 1.0 }];

    // Grade alone, as the reference for what the curves do.
    let graded_alone = gpu.apply_huesat_gpu(&input, W, H, 1.0, &lut, 0b1);

    // Run equalize first, then the grade, then the grade again, so the
    // two buffers are written in both orders and more than once.
    let equalized = gpu.apply_effects(&fx, &input, W, H);
    let _ = gpu.apply_huesat_gpu(&equalized, W, H, 1.0, &lut, 0b1);
    let graded_again = gpu.apply_huesat_gpu(&input, W, H, 1.0, &lut, 0b1);

    let d = worst(&graded_alone, &graded_again);
    eprintln!("grade before vs after an equalize: worst = {d:e} linear");
    assert!(
        d < 1e-6,
        "the same grade gave a different picture after an equalize ran: {d}",
    );
}
