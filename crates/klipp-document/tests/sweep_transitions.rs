//! Measured sweep of the transitions: every kind at its default
//! settings, on the export's CPU kernel (tied to the GPU by
//! cpu_gpu_transition_parity), from a scene to a checkerboard. Each starts
//! on the outgoing shot and ends on the incoming one, is neither at its
//! middle, is not a flat colour or a wash there unless it goes through
//! one, and does more than a plain cross dissolve would. Report:
//! `target/sweep/transitions.txt`.

mod sweep_common;

use klipp_core::TRANSITION_SPECS;
use klipp_effects::render_transition_linear;
use klipp_testing::sweep::Sweep;
use sweep_common::*;

/// Items known to fail, and the issue that holds each; 0: found by the
/// sweep, not filed yet.
const KNOWN: &[(&str, u32)] = &[
    ("LightLeak picture", 2765),
    ("ColorizeFlash picture", 2765),
    ("AdditiveDissolve picture", 2765),
    ("LuminanceDissolve ends", 2765),
    ("ZoomOutDissolve ends", 2765),
];

/// Those that pass through a flat colour on purpose.
const THROUGH_A_COLOUR: &[&str] = &["DipToBlack", "DipToWhite", "Flash", "FadeToColor", "SqueezeH", "SqueezeV", "NegativeFlash"];

/// Those that are a dissolve by name: alike to the cross dissolve is right.
const DISSOLVES: &[&str] = &["CrossDissolve", "FilmDissolve", "AdditiveDissolve"];

fn frames() -> (Vec<f32>, Vec<f32>) {
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for y in 0..H {
        for x in 0..W {
            let c = scene_at(x as f32 / W as f32, y as f32 / H as f32);
            a.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
            let check = if (x / 8 + y / 8).is_multiple_of(2) { 0.7 } else { 0.1 };
            b.extend_from_slice(&[0.08, check * 0.6, check, 1.0]);
        }
    }
    (a, b)
}

/// The spread of a frame's shown brightness: a flat colour has none.
fn spread(a: &[f32]) -> f32 {
    let v: Vec<f32> = a.chunks(4).map(|p| (display(p[0]) + display(p[1]) + display(p[2])) / 3.0).collect();
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    (v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
}

fn brightness(a: &[f32]) -> f32 {
    (mean_channel(a, 0) + mean_channel(a, 1) + mean_channel(a, 2)) / 3.0
}

#[test]
fn sweep_transitions() {
    let mut sweep = Sweep::new("transitions", KNOWN);
    let (from, to) = frames();
    let at = |kind: u32, p: f32| {
        let params = &TRANSITION_SPECS[kind as usize].default_params;
        render_transition_linear(&from, &to, kind, p, W, H, params.param0, params.param1, params.param2, params.param3, params.color)
    };
    let dissolve = TRANSITION_SPECS.iter().find(|s| s.string_key == "CrossDissolve").expect("a cross dissolve").kind_id as u32;
    let dissolved: Vec<Vec<f32>> = [0.25, 0.5, 0.75].iter().map(|p| at(dissolve, *p)).collect();
    let wash = brightness(&from).max(brightness(&to)) + 0.2;
    for spec in TRANSITION_SPECS.iter() {
        let (key, kind) = (spec.string_key, spec.kind_id as u32);
        if key == "SmoothCut" {
            sweep.skip(key, "morphs two takes of one framing, not two shots");
            continue;
        }
        let (start, mid, end) = (at(kind, 0.0), at(kind, 0.5), at(kind, 1.0));
        if !(finite(&start) && finite(&mid) && finite(&end)) {
            sweep.check(&format!("{key} finite"), "not a number in the frame", false);
            continue;
        }
        let ends = (mean_diff(&start, &from), mean_diff(&end, &to));
        sweep.check(&format!("{key} ends"), format!("start off the outgoing {:.3}, end off the incoming {:.3}", ends.0, ends.1), ends.0 < 0.02 && ends.1 < 0.02);
        let middle = (mean_diff(&mid, &from), mean_diff(&mid, &to));
        sweep.check(&format!("{key} middle"), format!("off the outgoing {:.3}, the incoming {:.3}", middle.0, middle.1), middle.0 > 0.02 && middle.1 > 0.02);
        if !THROUGH_A_COLOUR.contains(&key) {
            let (s, b) = (spread(&mid), brightness(&mid));
            sweep.check(&format!("{key} picture"), format!("spread {s:.3}, brightness {b:.3} (wash over {wash:.3})"), s > 0.05 && b < wash);
        }
        if !DISSOLVES.contains(&key) {
            // Where it differs most from a dissolve at the same point.
            let own = [0.25, 0.5, 0.75].iter().zip(&dissolved).map(|(p, d)| mean_diff(&at(kind, *p), d)).fold(0.0f32, f32::max);
            sweep.check(&format!("{key} own look"), format!("off a cross dissolve {own:.3}"), own > 0.015);
        }
    }
    sweep.finish();
}
