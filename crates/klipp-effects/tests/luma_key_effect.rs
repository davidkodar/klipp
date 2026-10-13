//! The Luma Key clip effect (#1638): a clip keyed by brightness, its
//! matte taken into alpha. The range `[low, high]` is keyed out, `soft`
//! fades it back in past either end, `invert` keeps the range instead;
//! the colour is never touched.
//!
//! Its matte is the grade's LumaKey node's (`klipp_color::luma_key`), run
//! the other way round: what the node selects, the effect keys out. The
//! CPU twin is transcribed in `cpu.rs` (this crate sits below
//! klipp-color), so a test here holds it to the node's function, and
//! another holds the GPU's run of the node's shader to it.

use klipp_color::luma_key::luma_key_value;
use klipp_core::LumaKeySettings;
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::{apply_effects, apply_effects_with_luma, BuiltinEffect};

const BT709: [f32; 3] = [0.2126, 0.7152, 0.0722];
const BT2020: [f32; 3] = [0.2627, 0.6780, 0.0593];
const AP1: [f32; 3] = [0.2722, 0.6741, 0.0537];

fn key(low: f32, high: f32, soft: f32, invert: bool) -> BuiltinEffect {
    BuiltinEffect::LumaKey { low, high, soft, invert }
}

fn encode(c: f32) -> f32 {
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn decode(c: f32) -> f32 {
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// One pixel, given as its encoded grey, through the effect: its alpha.
fn alpha_of_grey(effect: &BuiltinEffect, encoded: f32) -> f32 {
    let v = decode(encoded);
    apply_effects(std::slice::from_ref(effect), vec![v, v, v, 1.0], 1, 1)[3]
}

/// A frame of ramps, 64 wide: a neutral row running past white, rows
/// pushing each channel, and a part-transparent copy of each, so the
/// working space's weights and the alpha it already has both show.
fn frame() -> (Vec<f32>, u32, u32) {
    let (w, h) = (64u32, 8u32);
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let t = x as f32 / (w - 1) as f32 * 1.2;
            let (r, g, b) = match y % 4 {
                0 => (t, t, t),
                1 => (t, t * 0.2, t * 0.1),
                2 => (t * 0.2, t, t * 0.3),
                _ => (t * 0.1, t * 0.3, t),
            };
            let a = if y < 4 { 1.0 } else { 0.6 };
            v.extend_from_slice(&[r, g, b, a]);
        }
    }
    (v, w, h)
}

#[test]
fn black_is_keyed_and_white_kept_at_the_defaults() {
    let fx = key(0.0, 0.1, 0.05, false);
    assert_eq!(alpha_of_grey(&fx, 0.0), 0.0, "black keyed out");
    assert_eq!(alpha_of_grey(&fx, 0.08), 0.0, "inside the range keyed out");
    assert_eq!(alpha_of_grey(&fx, 1.0), 1.0, "white kept");
    assert_eq!(alpha_of_grey(&fx, 0.5), 1.0, "mid grey kept");
    // Control: a range that holds no black keys none of it.
    assert_eq!(alpha_of_grey(&key(0.5, 0.6, 0.0, false), 0.0), 1.0);
}

#[test]
fn the_softness_ramps_back_in_past_the_range() {
    // Keyed to 0.2, back in over 0.2 more: a quarter, half and three
    // quarters of the way along the ramp keep that much of the pixel.
    let fx = key(0.0, 0.2, 0.2, false);
    for (grey, kept) in [(0.25, 0.25), (0.3, 0.5), (0.35, 0.75), (0.4, 1.0)] {
        let a = alpha_of_grey(&fx, grey);
        assert!((a - kept).abs() < 2e-3, "grey {grey}: alpha {a}, want {kept}");
    }
    // Below a range that starts above black, the same ramp the other way.
    let band = key(0.5, 0.6, 0.2, false);
    assert!((alpha_of_grey(&band, 0.4) - 0.5).abs() < 2e-3);
    // Control: without softness the edge is a hard step.
    assert_eq!(alpha_of_grey(&key(0.0, 0.2, 0.0, false), 0.25), 1.0);
}

#[test]
fn invert_keeps_the_range_and_keys_the_rest() {
    let (plain, inverted) = (key(0.0, 0.1, 0.05, false), key(0.0, 0.1, 0.05, true));
    for grey in [0.0, 0.05, 0.12, 0.13, 0.5, 1.0] {
        let (a, b) = (alpha_of_grey(&plain, grey), alpha_of_grey(&inverted, grey));
        assert!((a + b - 1.0).abs() < 1e-6, "grey {grey}: {a} and {b} are not each other's inverse");
    }
    assert_eq!(alpha_of_grey(&inverted, 0.0), 1.0, "black kept");
    assert_eq!(alpha_of_grey(&inverted, 1.0), 0.0, "white keyed out");
}

#[test]
fn only_alpha_changes_and_it_multiplies_what_was_there() {
    let (input, w, h) = frame();
    let out = apply_effects(&[key(0.0, 0.3, 0.1, false)], input.clone(), w, h);
    let mut keyed = 0;
    for (i, o) in input.chunks_exact(4).zip(out.chunks_exact(4)) {
        assert_eq!(&i[..3], &o[..3], "the colour is untouched");
        assert!(o[3] <= i[3] + 1e-6, "alpha only ever goes down");
        keyed += usize::from(o[3] < i[3]);
    }
    // Control: it did key something, and kept something.
    assert!(keyed > 0 && keyed < (w * h) as usize, "{keyed} of {} keyed", w * h);
    // A part-transparent black is keyed out as an opaque one is, and a
    // part-transparent white keeps the alpha it had.
    let row4 = &out[(4 * w * 4) as usize..(5 * w * 4) as usize];
    assert_eq!((row4[3], row4[row4.len() - 1]), (0.0, 0.6));
}

/// The effect keeps what the grade's LumaKey node does not select, on
/// every working space's weights, to the last bit: the CPU twin is the
/// node's function.
#[test]
fn the_cpu_key_is_the_grade_nodes_matte_turned_round() {
    let (input, w, h) = frame();
    for (low, high, soft) in [(0.0, 0.1, 0.05), (0.3, 0.6, 0.15), (0.8, 1.0, 0.0)] {
        for invert in [false, true] {
            for yw in [BT709, BT2020, AP1] {
                let out = apply_effects_with_luma(&[key(low, high, soft, invert)], input.clone(), w, h, yw);
                let node = LumaKeySettings { enabled: true, low, high, soft, invert: !invert };
                for (i, o) in input.chunks_exact(4).zip(out.chunks_exact(4)) {
                    let m = luma_key_value([encode(i[0]), encode(i[1]), encode(i[2])], &node, yw);
                    assert!((o[3] - i[3] * m).abs() < 1e-6, "{low}..{high} soft {soft} invert {invert} {yw:?}: {i:?} gave {} not {}", o[3], i[3] * m);
                }
            }
        }
    }
    // Control: the weights matter on this frame, so the loop above is
    // telling the spaces apart.
    let k = [key(0.3, 0.6, 0.05, false)];
    let (a, b) = (apply_effects_with_luma(&k, input.clone(), w, h, BT709), apply_effects_with_luma(&k, input, w, h, AP1));
    assert_ne!(a, b, "BT.709 and AP1 key the saturated rows alike");
}

/// The GPU runs the grade's luma key shader into alpha; it gives the
/// CPU's picture, alpha included, on every working space's weights.
#[test]
fn the_luma_key_effect_matches_between_cpu_and_gpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let (input, w, h) = frame();
    let mut worst = 0.0f32;
    let mut failures = Vec::new();
    for (low, high, soft) in [(0.0, 0.1, 0.05), (0.3, 0.6, 0.15), (0.8, 1.0, 0.0), (0.0, 0.5, 0.5)] {
        for invert in [false, true] {
            for yw in [BT709, BT2020, AP1] {
                let fx = [key(low, high, soft, invert)];
                gpu.set_luma_coefs(yw);
                let on_gpu = gpu.apply_effects(&fx, &input, w, h);
                let cpu = apply_effects_with_luma(&fx, input.clone(), w, h, yw);
                let d = cpu.iter().zip(&on_gpu).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
                worst = worst.max(d);
                if d > 1e-4 {
                    failures.push(format!("{low}..{high} soft {soft} invert {invert} {yw:?}: CPU and GPU differ by {d}"));
                }
                // Control: the GPU keyed something, so a pass-through on
                // both sides cannot match.
                if !on_gpu.chunks_exact(4).zip(input.chunks_exact(4)).any(|(o, i)| (o[3] - i[3]).abs() > 0.1) {
                    failures.push(format!("{low}..{high} soft {soft} invert {invert}: the GPU keyed nothing"));
                }
            }
        }
    }
    gpu.set_luma_coefs(BT709);
    eprintln!("luma key CPU vs GPU worst difference {worst:e}");
    assert!(failures.is_empty(), "{failures:#?}");
}
