//! One strength scale for every picture effect's `amount` (#2762): each
//! kind the `effect` op takes, at 0.3, 0.5 and 1, over a bright, a dark
//! and a busy frame, measured as its visible change from the frame. At
//! 0.5 every effect shows and none wrecks the picture, and more amount is
//! never less effect. The op's mapping is what is measured, run on the
//! CPU kernels (tied to the GPU by cpu_gpu_effect_parity).

use klipp_document::ops::named_picture_effect;

const W: u32 = 192;
const H: u32 = 108;

/// The frames: bright (a sunlit scene with the sun in it), dark (night,
/// a few lights), busy (fine detail everywhere).
fn frames() -> Vec<(&'static str, Vec<f32>)> {
    let make = |f: &dyn Fn(f32, f32, u32, u32) -> [f32; 3]| -> Vec<f32> {
        (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).flat_map(|(x, y)| {
            let c = f(x as f32 / W as f32, y as f32 / H as f32, x, y);
            [c[0], c[1], c[2], 1.0]
        }).collect()
    };
    let bright = make(&|fx, fy, _, _| {
        let sun = (-((fx - 0.7).powi(2) + (fy - 0.25).powi(2)) * 400.0).exp() * 8.0;
        let base = if fy < 0.45 { [0.4 + 0.3 * fy, 0.55 + 0.2 * fy, 0.9] } else { [0.6 - 0.2 * fy, 0.5 - 0.2 * fy, 0.35] };
        base.map(|c| c + sun)
    });
    let dark = make(&|fx, fy, _, _| {
        let lamp = |x: f32, y: f32| (-((fx - x).powi(2) + (fy - y).powi(2)) * 2500.0).exp() * 3.0;
        let l = lamp(0.2, 0.4) + lamp(0.55, 0.35) + lamp(0.8, 0.45);
        [0.01 + 0.02 * fy + l, 0.012 + 0.015 * fy + l * 0.8, 0.02 + l * 0.5]
    });
    // Not periodic: a blur as long as a repeating pattern's period would
    // leave it as it was.
    let busy = make(&|fx, fy, x, y| {
        let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B);
        h ^= h >> 15;
        h = h.wrapping_mul(0x2C1B_3C6D);
        h ^= h >> 12;
        let t = (h % 1000) as f32 / 1000.0 * 0.4 - 0.2;
        [0.25 + t + 0.2 * fx, 0.3 + t, 0.2 + t + 0.2 * fy]
    });
    vec![("bright", bright), ("dark", dark), ("busy", busy)]
}

fn shown(v: f32) -> f32 {
    v.clamp(0.0, 1.0).powf(1.0 / 2.2)
}

/// The visible change: the mean difference as shown, and the share of
/// pixels moved by more than two levels (an effect that works locally).
fn change(a: &[f32], b: &[f32]) -> (f32, f32) {
    let (mut sum, mut moved) = (0.0f32, 0usize);
    for (p, q) in a.chunks(4).zip(b.chunks(4)) {
        let d = (0..3).map(|c| (shown(p[c]) - shown(q[c])).abs()).fold(0.0f32, f32::max);
        sum += (0..3).map(|c| (shown(p[c]) - shown(q[c])).abs()).sum::<f32>() / 3.0;
        moved += (d > 2.0 / 255.0) as usize;
    }
    (sum / (W * H) as f32, moved as f32 / (W * H) as f32)
}

/// The kinds the op takes on a picture of its own (the ones drawn round
/// a smaller layer, and the keys, are measured elsewhere, and the two
/// flickers change over time, which one frame cannot show).
const KINDS: &[&str] = &[
    "vignette", "grain", "glow", "bloom", "diffusion", "halation", "light_rays", "lens_flare", "anamorphic", "light_wrap", "blur", "radial_blur",
    "spin_blur", "directional_blur", "sharpen", "chromatic_aberration", "tilt_shift", "mosaic", "posterize", "invert", "mirror", "lens_distortion",
    "displace", "emboss", "edge_detect", "denoise", "vhs", "old_film", "binocular", "dust", "snow", "rain", "sparks",
    "embers",
];

/// Those that make a new picture of it: a large change is the point.
const REPLACES: &[&str] = &["invert", "mirror", "emboss", "edge_detect", "binocular"];

/// Visible at 0.5: on the frame where it shows best, a mean change of at
/// least this, or at least this share of the pixels moved.
const VISIBLE: f32 = 0.012;
const VISIBLE_SHARE: f32 = 0.03;
/// Not wrecked at 0.5: a mean change of at most this on any frame.
const WRECKED: f32 = 0.2;

#[test]
fn every_effect_shows_at_half_and_none_wrecks_the_picture() {
    let frames = frames();
    let mut wrong = Vec::new();
    for kind in KINDS {
        let mut best = (0.0f32, 0.0f32);
        let mut worst = 0.0f32;
        let mut by_amount = Vec::new();
        for amount in [0.3f32, 0.5, 1.0] {
            let (mut fx, _) = named_picture_effect(kind, Some(amount)).unwrap_or_else(|(why, _)| panic!("{kind}: {why}"));
            // Particles as they are two seconds into a clip, the way they
            // play, not on the frame they start on.
            if let klipp_effects::BuiltinEffect::Particles { time, .. } = &mut fx {
                *time = 2.0;
            }
            let mut total = 0.0;
            for (name, frame) in &frames {
                let out = klipp_effects::apply_effects(std::slice::from_ref(&fx), frame.clone(), W, H);
                let (mean, share) = change(frame, &out);
                total += mean;
                if amount == 0.5 {
                    best = (best.0.max(mean), best.1.max(share));
                    worst = worst.max(mean);
                }
                println!("{kind:<22} {amount:.1} {name:<6} mean {mean:.3} moved {:.1}%", share * 100.0);
            }
            by_amount.push(total);
        }
        if !(best.0 >= VISIBLE || best.1 >= VISIBLE_SHARE) {
            wrong.push(format!("{kind}: hardly shows at 0.5 (mean {:.3}, {:.1}% moved)", best.0, best.1 * 100.0));
        }
        if worst > WRECKED && !REPLACES.contains(kind) {
            wrong.push(format!("{kind}: wrecks the picture at 0.5 (mean {worst:.3})"));
        }
        // Never less with more: within a hundredth, where an effect has
        // reached its full look.
        if by_amount.windows(2).any(|w| w[1] < w[0] * 0.99 - 1e-4) {
            wrong.push(format!("{kind}: less effect with more amount {by_amount:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The refusal names every kind the op takes.
#[test]
fn the_refusal_names_every_kind() {
    let (_, fix) = named_picture_effect("no_such_effect", None).unwrap_err();
    for kind in KINDS.iter().chain(&["corners", "shadow", "outer_glow", "fill_bars", "reflection", "chroma_key", "luma_key", "flicker", "exposure_flicker"]) {
        assert!(fix.contains(kind), "the refusal leaves out {kind}: {fix}");
    }
}
