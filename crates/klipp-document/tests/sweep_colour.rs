//! Measured sweep of the colour controls: each grade control, qualifier,
//! zone and a look from each category, through its op at a middle
//! setting, rendered on the export's CPU path, measured to move the
//! picture the way it says and to leave alone what it does not touch.
//! Report: `target/sweep/colour.txt`.

mod sweep_common;

use klipp_testing::sweep::Sweep;
use serde_json::{json, Value};
use sweep_common::*;

/// Items known to fail, and the issue that holds each.
const KNOWN: &[(&str, u32)] = &[];

/// A region's mean colour as shown: `x` and `y` ranges as frame fractions.
fn region(f: &[f32], x: (f32, f32), y: (f32, f32)) -> [f32; 3] {
    let mut s = [0.0f32; 3];
    let mut n = 0.0;
    for py in (y.0 * H as f32) as u32..(y.1 * H as f32) as u32 {
        for px in (x.0 * W as f32) as u32..(x.1 * W as f32) as u32 {
            let i = ((py * W + px) * 4) as usize;
            (0..3).for_each(|c| s[c] += display(f[i + c]));
            n += 1.0;
        }
    }
    s.map(|v| v / n)
}

/// The scene's parts (see `sweep_common::scene_at`).
fn sky(f: &[f32]) -> [f32; 3] {
    region(f, (0.05, 0.5), (0.05, 0.4))
}
fn patch(f: &[f32], k: u32) -> [f32; 3] {
    region(f, ((k as f32 + 0.15) / 6.0, (k as f32 + 0.85) / 6.0), (0.6, 0.95))
}
fn skin(f: &[f32]) -> [f32; 3] {
    patch(f, 0)
}
fn leaves(f: &[f32]) -> [f32; 3] {
    patch(f, 1)
}
fn shadow(f: &[f32]) -> [f32; 3] {
    patch(f, 4)
}
fn whole(f: &[f32]) -> [f32; 3] {
    region(f, (0.0, 1.0), (0.0, 1.0))
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}
fn chroma(c: [f32; 3]) -> f32 {
    c.iter().cloned().fold(f32::MIN, f32::max) - c.iter().cloned().fold(f32::MAX, f32::min)
}
fn warmth(c: [f32; 3]) -> f32 {
    c[0] - c[2]
}
fn magenta(c: [f32; 3]) -> f32 {
    (c[0] + c[2]) / 2.0 - c[1]
}
fn moved(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).abs()).fold(0.0, f32::max)
}

/// The luma spread of a frame as shown.
fn contrast(f: &[f32]) -> f32 {
    let v: Vec<f32> = f.chunks(4).map(|p| luma([display(p[0]), display(p[1]), display(p[2])])).collect();
    let m = v.iter().sum::<f32>() / v.len() as f32;
    (v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
}

/// The mean chroma over the lower half, where the colours are.
fn saturation(f: &[f32]) -> f32 {
    (0..6).filter(|k| *k != 3 && *k != 4).map(|k| chroma(patch(f, k))).sum::<f32>() / 4.0
}

#[test]
fn sweep_colour() {
    let mut sweep = Sweep::new("colour", KNOWN);
    let plain = render(&with_ops(&[]).unwrap(), 1.0);
    let after = |sweep: &mut Sweep, item: &str, op: Value| -> Option<Vec<f32>> {
        match with_ops(&[op]) {
            Ok(seq) => Some(render(&seq, 1.0)),
            Err(why) => {
                sweep.check(item, format!("refused: {why}"), false);
                None
            }
        }
    };
    let check = |sweep: &mut Sweep, item: &str, op: Value, measure: &dyn Fn(&[f32]) -> (f32, String), pass: &dyn Fn(f32) -> bool| {
        if let Some(f) = after(sweep, item, op) {
            let (v, text) = measure(&f);
            sweep.check(item, text, pass(v));
        }
    };
    let base_luma = luma(whole(&plain));
    let lum = move |f: &[f32]| {
        let d = luma(whole(f)) - base_luma;
        (d, format!("frame luma {d:+.3}"))
    };
    check(&mut sweep, "grade exposure +1", json!({"op": "grade", "exposure": 1.0}), &lum, &|d| d > 0.05);
    check(&mut sweep, "grade exposure -1", json!({"op": "grade", "exposure": -1.0}), &lum, &|d| d < -0.05);
    let base_contrast = contrast(&plain);
    let con = move |f: &[f32]| {
        let r = contrast(f) / base_contrast;
        (r, format!("luma spread x{r:.2}"))
    };
    check(&mut sweep, "grade contrast 1.3", json!({"op": "grade", "contrast": 1.3}), &con, &|r| r > 1.05);
    check(&mut sweep, "grade contrast 0.7", json!({"op": "grade", "contrast": 0.7}), &con, &|r| r < 0.95);
    let base_sat = saturation(&plain);
    let sat = move |f: &[f32]| {
        let r = saturation(f) / base_sat;
        (r, format!("chroma x{r:.2}"))
    };
    check(&mut sweep, "grade saturation 1.5", json!({"op": "grade", "saturation": 1.5}), &sat, &|r| r > 1.1);
    check(&mut sweep, "grade saturation 0.3", json!({"op": "grade", "saturation": 0.3}), &sat, &|r| r < 0.7);
    check(&mut sweep, "grade vibrance +0.5", json!({"op": "grade", "vibrance": 0.5}), &sat, &|r| r > 1.05);
    let base_warm = warmth(whole(&plain));
    let warm = move |f: &[f32]| {
        let d = warmth(whole(f)) - base_warm;
        (d, format!("red - blue {d:+.3}"))
    };
    check(&mut sweep, "grade temperature +25 warms", json!({"op": "grade", "temperature": 25.0}), &warm, &|d| d > 0.02);
    check(&mut sweep, "grade temperature -25 cools", json!({"op": "grade", "temperature": -25.0}), &warm, &|d| d < -0.02);
    let base_mag = magenta(whole(&plain));
    let mag = move |f: &[f32]| {
        let d = magenta(whole(f)) - base_mag;
        (d, format!("magenta - green {d:+.3}"))
    };
    check(&mut sweep, "grade tint +25 is magenta", json!({"op": "grade", "tint": 25.0}), &mag, &|d| d > 0.01);
    check(&mut sweep, "grade tint -25 is green", json!({"op": "grade", "tint": -25.0}), &mag, &|d| d < -0.01);

    // Lift, gamma, gain: each moves its own part of the range most.
    let parts = |f: &[f32]| [luma(shadow(f)), luma(patch(f, 5)), luma(sky(f))];
    let base_parts = parts(&plain);
    for (item, op, at) in [
        ("grade lift +0.05 shadows most", json!({"op": "grade", "lift": 0.05}), 0),
        ("grade gamma +0.05 mids most", json!({"op": "grade", "gamma": 0.05}), 1),
        ("grade gain +0.05 highs most", json!({"op": "grade", "gain": 0.05}), 2),
    ] {
        if let Some(f) = after(&mut sweep, item, op) {
            let d: Vec<f32> = parts(&f).iter().zip(&base_parts).map(|(a, b)| a - b).collect();
            // Gamma bends the middle: more there than at the top, and
            // in display terms the shadows move with it.
            let most = d[at] > 0.005 && if at == 1 { d[1] > d[2] } else { d.iter().all(|v| d[at] >= *v - 1e-4) };
            sweep.check(item, format!("shadow {:+.3}, mid {:+.3}, sky {:+.3}", d[0], d[1], d[2]), most);
        }
    }

    // Shadows and highlights reach their end of the range, not the frame.
    if let Some(f) = after(&mut sweep, "grade shadows +0.3", json!({"op": "grade", "shadows": 0.3})) {
        let (s, k) = (luma(shadow(&f)) - luma(shadow(&plain)), luma(sky(&f)) - luma(sky(&plain)));
        sweep.check("grade shadows +0.3", format!("shadow {s:+.3}, sky {k:+.3}"), s > 0.02 && s < 0.15 && k.abs() < 0.03);
    }
    if let Some(f) = after(&mut sweep, "grade highlights -0.3", json!({"op": "grade", "highlights": -0.3})) {
        let (k, s) = (luma(sky(&f)) - luma(sky(&plain)), luma(shadow(&f)) - luma(shadow(&plain)));
        sweep.check("grade highlights -0.3", format!("sky {k:+.3}, shadow {s:+.3}"), k < -0.02 && k > -0.15 && s.abs() < 0.03);
    }

    // Qualifiers: what they key moves, the rest stays.
    for (item, op, keyed, other) in [
        ("qualify skin temperature +25", json!({"op": "qualify", "target": "skin", "temperature": 25.0}), skin as fn(&[f32]) -> [f32; 3], sky as fn(&[f32]) -> [f32; 3]),
        ("qualify foliage hue_shift 40", json!({"op": "qualify", "target": "foliage", "hue_shift": 40.0}), leaves, sky),
        ("qualify sky saturation 1.6", json!({"op": "qualify", "target": "sky", "saturation": 1.6}), sky, skin),
    ] {
        if let Some(f) = after(&mut sweep, item, op) {
            let (m, o) = (moved(keyed(&f), keyed(&plain)), moved(other(&f), other(&plain)));
            sweep.check(item, format!("keyed moved {m:.3}, the rest {o:.3}"), m > 0.01 && o < 0.01);
        }
    }
    // The qualifier's temperature on the grade's scale, mireds: warmer.
    if let Some(f) = after(&mut sweep, "qualify temperature in mireds", json!({"op": "qualify", "target": "skin", "temperature": 25.0})) {
        let d = warmth(skin(&f)) - warmth(skin(&plain));
        sweep.check("qualify temperature in mireds", format!("skin red - blue {d:+.3}"), d > 0.01);
    }

    // Zones: by tone.
    if let Some(f) = after(&mut sweep, "zones highlight -0.7", json!({"op": "zones", "zone": "highlight", "exposure": -0.7})) {
        let (k, s) = (luma(sky(&f)) - luma(sky(&plain)), luma(shadow(&f)) - luma(shadow(&plain)));
        sweep.check("zones highlight -0.7", format!("sky {k:+.3}, shadow {s:+.3}"), k < -0.01 && s.abs() < 0.01);
    }
    if let Some(f) = after(&mut sweep, "zones shadow +0.7", json!({"op": "zones", "zone": "shadow", "exposure": 0.7})) {
        let (s, k) = (luma(shadow(&f)) - luma(shadow(&plain)), luma(sky(&f)) - luma(sky(&plain)));
        sweep.check("zones shadow +0.7", format!("shadow {s:+.3}, sky {k:+.3}"), s > 0.01 && k.abs() < 0.02);
    }

    // A look from each bundled category: it changes the picture without
    // washing it out.
    let luts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/luts");
    let mut categories: Vec<_> = std::fs::read_dir(&luts).map(|d| d.flatten().filter(|e| e.path().is_dir()).map(|e| e.path()).collect()).unwrap_or_default();
    categories.sort();
    for dir in categories {
        let mut files: Vec<_> = std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "cube")).collect()).unwrap_or_default();
        files.sort();
        let Some(lut) = files.first() else { continue };
        let item = format!("look {}", dir.file_name().unwrap().to_string_lossy());
        let lut_path = lut.canonicalize().unwrap_or(lut.clone());
        if let Some(f) = after(&mut sweep, &item, json!({"op": "look", "lut_path": lut_path, "opacity": 1.0})) {
            // Washed out is contrast lost (a black and white look may
            // brighten saturated colours as it greys them).
            let (d, c) = (mean_diff(&plain, &f), contrast(&f) / base_contrast);
            sweep.check(&item, format!("{}: change {d:.3}, contrast x{c:.2}", lut.file_name().unwrap().to_string_lossy()), d > 0.01 && c > 0.6);
        }
    }
    sweep.finish();
}
