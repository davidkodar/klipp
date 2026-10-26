//! Measured sweep of the picture effects: each one through the `effect`
//! op at amount 0.5, rendered on the export's CPU path (tied to the GPU
//! by cpu_gpu_effect_parity), measured to change the frame visibly
//! without wrecking it. Report: `target/sweep/picture.txt`.

mod sweep_common;

use klipp_testing::sweep::Sweep;
use serde_json::json;
use sweep_common::*;

/// Items known to fail, and the issue that holds each.
const KNOWN: &[(&str, u32)] = &[("dust", 2762), ("rain", 2762), ("sparks", 2762), ("embers", 2762)];

/// The effect kinds the op takes, by their own names (aliases left out).
const KINDS: &[&str] = &[
    "vignette", "grain", "glow", "bloom", "diffusion", "halation", "light_rays", "lens_flare", "anamorphic", "light_wrap", "blur", "radial_blur",
    "spin_blur", "directional_blur", "sharpen", "chromatic_aberration", "tilt_shift", "mosaic", "posterize", "invert", "mirror", "lens_distortion",
    "displace", "emboss", "edge_detect", "denoise", "vhs", "old_film", "flicker", "exposure_flicker", "binocular", "dust", "snow", "rain", "sparks",
    "embers",
];

/// The ones drawn round a layer smaller than the frame.
const LAYER: &[&str] = &["corners", "shadow", "outer_glow", "fill_bars", "reflection"];

/// The ones that make a new picture of it, or black out most of it, for
/// which a large change is the point.
const REPLACES: &[&str] = &["invert", "mirror", "emboss", "edge_detect", "binocular"];

/// Visible: a mean change as shown of at least this (about 2.5 levels),
/// or at least this share of the pixels changed (a local effect).
const VISIBLE: f32 = 0.01;
const VISIBLE_SHARE: f32 = 0.01;
/// Not wrecked: at most this, but for those that replace the picture.
const WRECKED: f32 = 0.25;

#[test]
fn sweep_picture() {
    let mut sweep = Sweep::new("picture", KNOWN);
    let plain = render(&with_ops(&[]).unwrap(), 1.0);
    // A layer smaller than the frame over mid grey, for the effects drawn round it.
    let grey = json!({"op": "generator", "data": {"SolidColor": {"color": [128, 128, 128, 255]}}, "at": {"secs": 0}, "secs": 4});
    let card = json!({"op": "compose", "poses": [{"width": 0.6, "height": 0.6}]});
    let small = render(&with_ops(&[grey.clone(), card.clone()]).unwrap(), 1.0);
    let measure = |sweep: &mut Sweep, item: &str, ops: Vec<serde_json::Value>, base: &[f32], replaces: bool| {
        let seq = match with_ops(&ops) {
            Ok(seq) => seq,
            Err(why) => return sweep.check(item, format!("refused: {why}"), false),
        };
        let frame = render(&seq, 1.0);
        let (d, share) = (mean_diff(base, &frame), changed_share(base, &frame));
        // Corners round only the card's four corners.
        let least = if item == "corners" { 0.002 } else { VISIBLE_SHARE };
        let ok_size = (d >= VISIBLE || share >= least) && (replaces || d <= WRECKED);
        sweep.check(item, format!("mean change {d:.3}, {:.1}% of pixels{}", share * 100.0, if finite(&frame) { "" } else { ", NOT FINITE" }), ok_size && finite(&frame));
    };
    for kind in KINDS.iter().filter(|k| **k != "denoise") {
        measure(&mut sweep, kind, vec![json!({"op": "effect", "kind": kind, "amount": 0.5})], &plain, REPLACES.contains(kind));
    }
    for kind in LAYER {
        measure(&mut sweep, kind, vec![grey.clone(), card.clone(), json!({"op": "effect", "kind": kind, "amount": 0.5})], &small, *kind == "fill_bars");
    }
    // Denoise takes grain back toward the clean picture.
    let grainy = render(&with_ops(&[json!({"op": "effect", "kind": "grain", "amount": 0.5})]).unwrap(), 1.0);
    match with_ops(&[json!({"op": "effect", "kind": "grain", "amount": 0.5}), json!({"op": "effect", "kind": "denoise", "amount": 0.5})]) {
        Ok(seq) => {
            let cleaned = render(&seq, 1.0);
            let (before, after) = (mean_diff(&plain, &grainy), mean_diff(&plain, &cleaned));
            sweep.check("denoise", format!("grain off the clean frame {before:.3} to {after:.3}"), after < before * 0.8);
        }
        Err(why) => sweep.check("denoise", format!("refused: {why}"), false),
    }
    // Keys take out what they key, the grey under the clip showing there:
    // the green patch, the dark one (0.17 as shown, so keyed to 0.25).
    let backed = render(&with_ops(std::slice::from_ref(&grey)).unwrap(), 1.0);
    for (item, op) in [("chroma_key", json!({"op": "effect", "kind": "chroma_key"})), ("luma_key", json!({"op": "effect", "kind": "luma_key", "high": 0.25}))] {
        match with_ops(&[grey.clone(), op]) {
            Ok(seq) => {
                let share = changed_share(&backed, &render(&seq, 1.0));
                sweep.check(item, format!("{:.1}% of pixels keyed to the grey under it", share * 100.0), (0.03..0.5).contains(&share));
            }
            Err(why) => sweep.check(item, format!("refused: {why}"), false),
        }
    }
    sweep.finish();
}
