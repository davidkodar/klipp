//! Every built-in effect gives the same picture on the CPU as on the GPU.
//!
//! The GPU shaders define what an effect does: they work on the linear-
//! light frame (chroma key and the LUT, deliberately, on its sRGB
//! encoding). The CPU runs when there is no usable GPU, and for chains
//! with an effect the GPU cannot run. Each case here renders one effect
//! both ways on the same frame and compares the display bytes.

use std::sync::Arc;

use klipp_effects::colour_transform::{CstTables, LutTable, Prepared, CURVE_SAMPLES};
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::{apply_effects, linear_f32_rgba_to_srgb_u8, BuiltinEffect, ColorFadeMode, SpillMethod};

const W: u32 = 160;
const H: u32 = 90;

/// A coarse look, as a LUT file gives one once read (#1746): a curve on
/// red, a lift on blue, between primaries other than the working space's.
fn look_lut() -> LutTable {
    let n = 9;
    let mut data = Vec::with_capacity(n * n * n * 3);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let [r, g, b] = [r, g, b].map(|i| i as f32 / (n - 1) as f32);
                data.extend_from_slice(&[r.powf(0.8) * 0.95, g * 0.9 + 0.05 * b, b * 0.85 + 0.1]);
            }
        }
    }
    let m = [[0.9, 0.08, 0.02], [0.03, 0.95, 0.02], [0.01, 0.04, 0.95]];
    LutTable { size: n, data, pre: Some(m), post: None }
}

/// A log-like curve's linear values at the table's codes.
fn log_curve() -> Vec<f32> {
    (0..CURVE_SAMPLES).map(|i| (2f32.powf(i as f32 / (CURVE_SAMPLES - 1) as f32 * 10.0) - 1.0) / 64.0).collect()
}

/// A colour-space transform, read through the curve or written through
/// it, with a gamut matrix.
fn transform(decode: bool, encode: bool) -> CstTables {
    let m = [[1.2, -0.15, -0.05], [-0.08, 1.1, -0.02], [0.0, -0.1, 1.1]];
    CstTables { matrix: Some(m), decode: decode.then(log_curve), encode: encode.then(log_curve) }
}

/// Gradients, hard edges, saturated patches, a near-white highlight and a
/// near-black shadow: something for every kind of effect to act on.
fn frame() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / (W - 1) as f32, y as f32 / (H - 1) as f32);
            let mut px = [fx, fy, 1.0 - fx * fy, 1.0];
            // Saturated patches.
            if (20..40).contains(&x) && (10..30).contains(&y) {
                px = [0.9, 0.05, 0.05, 1.0];
            }
            if (60..80).contains(&x) && (10..30).contains(&y) {
                px = [0.05, 0.8, 0.1, 1.0];
            }
            if (100..120).contains(&x) && (50..70).contains(&y) {
                px = [0.02, 0.03, 0.9, 1.0];
            }
            // A highlight and a shadow.
            if (130..150).contains(&x) && (10..30).contains(&y) {
                px = [0.98, 0.97, 0.95, 1.0];
            }
            if (10..30).contains(&x) && (60..80).contains(&y) {
                px = [0.004, 0.003, 0.005, 1.0];
            }
            // Fine checker for edge filters.
            if (60..90).contains(&x) && (50..80).contains(&y) && (x / 3 + y / 3) % 2 == 0 {
                px = [0.6, 0.6, 0.6, 1.0];
            }
            v.extend_from_slice(&px);
        }
    }
    v
}

fn cases() -> Vec<(&'static str, BuiltinEffect)> {
    use BuiltinEffect::*;
    vec![
        ("Opacity", Opacity { value: 0.5 }),
        ("ColorFade", ColorFade { color: [20, 40, 200, 255], duration_secs: 1.0, mode: ColorFadeMode::In, progress: 0.4 }),
        ("Crop", Crop { left: 0.1, right: 0.05, top: 0.1, bottom: 0.2, feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5}),
        ("Crop feathered", Crop { left: 0.1, right: 0.05, top: 0.1, bottom: 0.2, feather: 0.05, aspect: 0.0, focus_x: 0.5, focus_y: 0.5}),
        ("GaussianBlur", GaussianBlur { radius: 4.0 }),
        ("FocusBlur down", FocusBlur { radius: 5.0, vertical: true, band: 0.3 }),
        ("FocusBlur across", FocusBlur { radius: 3.0, vertical: false, band: 0.5 }),
        ("Sharpen", Sharpen { amount: 1.0 }),
        ("CornerPin", CornerPin { tl: [0.1, 0.15], tr: [0.85, 0.05], br: [0.8, 0.9], bl: [0.2, 0.8] }),
        ("Transform", Transform {
            position_x: 10.0, position_y: -5.0, scale_x: 0.8, scale_y: 0.8, rotation: 10.0,
            anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
        }),
        ("ChromaKey", ChromaKey {
            key_color: [13, 230, 26], tolerance: 0.3, soft_edge: 0.1, spill_method: SpillMethod::None, spill_strength: 0.0,
        }),
        ("Bloom", Bloom { threshold: 0.7, radius: 6.0, intensity: 0.8 }),
        ("Glow", Glow { radius: 6.0, intensity: 0.6, threshold: 0.0 }),
        // Round the highlights alone (#2763).
        ("Glow over a threshold", Glow { radius: 6.0, intensity: 0.6, threshold: 0.5 }),
        ("LightRays", LightRays { center_x: 0.5, center_y: 0.3, threshold: 0.6, samples: 32, decay: 0.95, intensity: 0.5 }),
        ("Vignette", Vignette { amount: 0.6, size: 0.5, softness: 0.5, roundness: 0.5 }),
        ("Diffusion", Diffusion { amount: 0.5, radius: 5.0, threshold: 0.4, desaturation: 0.2 }),
        ("LensFlare", LensFlare {
            position_x: 0.7, position_y: 0.3, brightness: 0.8, flare_size: 0.3, ghost_count: 4,
            ghost_intensity: 0.4, chromatic_aberration: 0.3,
        }),
        ("LightWrap", LightWrap { width: 6.0, intensity: 0.5, softness: 0.5 }),
        ("AnamorphicStreak", AnamorphicStreak { threshold: 0.7, length: 60.0, intensity: 0.6, tint: 0.5 }),
        ("FilmHalation", FilmHalation { threshold: 0.6, radius: 6.0, intensity: 0.5, hue: 0.05 }),
        ("Invert", Invert { mix: 0.7 }),
        ("Posterize", Posterize { levels: 5.0 }),
        ("Mosaic", Mosaic { block_size: 8.0 }),
        ("EdgeDetect", EdgeDetect { strength: 1.0, mix: 0.8 }),
        ("ChannelMixer", ChannelMixer {
            red_red: 0.8, red_green: 0.2, red_blue: 0.0, green_red: 0.1, green_green: 0.8, green_blue: 0.1,
            blue_red: 0.0, blue_green: 0.3, blue_blue: 0.7,
        }),
        ("DirectionalBlur", DirectionalBlur { angle: 30.0, distance: 8.0, samples: 12 }),
        ("Emboss", Emboss { angle: 45.0, strength: 1.0, mix: 0.7 }),
        ("Mirror", Mirror { horizontal: 1, vertical: 0 }),
        ("LensDistortion", LensDistortion { distortion: 0.3, scale: 1.0 }),
        ("Displace", Displace { amount: 6.0, size: 24.0, speed: 1.0, direction: 0, seed: 3, time: 0.4 }),
        ("Reflection", Reflection { waterline: 0.55, gap: 0.0, ripple_amount: 3.0, ripple_size: 12.0, ripple_speed: 1.0, breakup: 0.5, smear: 0.3, fade: 0.4, opacity: 0.8, darken: 0.0, contact: 0.0, blend: 0, tint: [20, 40, 60], tint_amount: 0.2, tint_auto: false, darken_auto: false, water: [0, 0, 0], water_key: 0, submerge: 1.0, ripple_from: 0, seed: 3, time: 0.4 }),
        ("Dilate", Dilate { radius: 2 }),
        ("Erode", Erode { radius: 2 }),
        ("Denoise", Denoise { spatial_sigma: 2.0, color_sigma: 0.1, iterations: 1 }),
        ("Gamma", Gamma { gamma: 1.6 }),
        ("HSVAdjust", HSVAdjust { hue_shift: 40.0, saturation: 1.3, value: 0.9 }),
        ("Vibrance", Vibrance { amount: 0.5 }),
        ("NoiseGenerator", NoiseGenerator { amount: 0.2, monochrome: 0, seed: 7 }),
        ("FilmGrain mono", FilmGrain { amount: 0.5, size: 2.0, color: 0, shadows: 0.6, seed: 11 }),
        ("FilmGrain colour", FilmGrain { amount: 0.5, size: 1.0, color: 1, shadows: 0.0, seed: 12 }),
        ("TiltShift", TiltShift { center: 0.5, width: 0.3, softness: 0.2, radius: 6.0 }),
        ("OldFilm", OldFilm { flicker: 0.5, scratches: 1.0, dust: 1.0, weave: 0.5, sepia: 0.6, seed: 13, amount: 1.0, age: 0.0, mono: 0.0, stains: 0.0, frame_lines: 0.0, leaks: 0.0, burn: 0.0 }),
        ("OldFilm aged", OldFilm { flicker: 0.8, scratches: 1.0, dust: 2.0, weave: 0.5, sepia: 0.3, seed: 41, amount: 1.5, age: 0.8, mono: 0.4, stains: 0.8, frame_lines: 0.7, leaks: 0.6, burn: 0.7 }),
        ("Vhs", Vhs { tracking: 0.6, bleed: 0.6, noise: 0.5, scanlines: 0.6, seed: 14 }),
        ("MedianFilter", MedianFilter { radius: 2 }),
        ("RadialBlur zoom", RadialBlur { center_x: 0.5, center_y: 0.5, amount: 15.0, samples: 16, mode: 0 }),
        ("RadialBlur spin", RadialBlur { center_x: 0.5, center_y: 0.5, amount: 10.0, samples: 16, mode: 1 }),
        ("Levels", Levels { in_black: 0.1, in_white: 0.9, gamma: 1.3, out_black: 0.05, out_white: 0.95 }),
        ("Equalize", Equalize { strength: 1.0 }),
        // The gain is what the compositor would have set for a frame
        // measured 18% under the shot's smoothed level.
        ("Deflicker", Deflicker { strength: 1.0, window_secs: 1.0, gain: 1.18 }),
        ("Deflicker half", Deflicker { strength: 0.5, window_secs: 1.0, gain: 0.82 }),
        ("Letterbox", Letterbox { aspect_ratio: 2.39, bar_color: 0, opacity: 1.0 }),
        ("ChromaticAberration", ChromaticAberration { red_shift: 3.0, blue_shift: -3.0, center_x: 0.5, center_y: 0.5 }),
        ("Lut3D", Lut3D { path: "look.cube".into(), opacity: 0.8, tetrahedral: true, prepared: Some(Prepared(Arc::new(look_lut()))) }),
        ("Lut3D trilinear", Lut3D { path: "look.cube".into(), opacity: 1.0, tetrahedral: false, prepared: Some(Prepared(Arc::new(look_lut()))) }),
        ("ColourSpaceTransform", ColourSpaceTransform {
            input_gamut: 5, input_curve: 2, output_gamut: 1, output_curve: 0, prepared: Some(Prepared(Arc::new(transform(true, false)))),
        }),
        ("PictureTransform", PictureTransform { position_x: 12.0, position_y: -6.0, scale_x: 0.8, scale_y: 0.7, rotation: 15.0, anchor_x: 0.4, anchor_y: 0.6 }),
        ("Generator solid", Generator { shape: 0, color_a: [200, 60, 30, 200], color_b: [0, 0, 0, 255], angle: 0.0, center_x: 0.5, center_y: 0.5, radius: 0.5 }),
        ("Generator linear", Generator { shape: 1, color_a: [20, 40, 220, 255], color_b: [250, 230, 90, 128], angle: 35.0, center_x: 0.5, center_y: 0.5, radius: 0.5 }),
        ("Generator radial", Generator { shape: 2, color_a: [255, 255, 255, 255], color_b: [10, 80, 30, 255], angle: 0.0, center_x: 0.3, center_y: 0.6, radius: 0.4 }),
        ("ColourSpaceTransform to a curve", ColourSpaceTransform {
            input_gamut: 0, input_curve: 0, output_gamut: 9, output_curve: 4, prepared: Some(Prepared(Arc::new(transform(false, true)))),
        }),
        // Flicker (#989), each target, mid-jump and eased.
        ("Flicker opacity", Flicker { amount: 0.8, speed: 8.0, smoothness: 0.5, hold: 0.0, seed: 3, target: 0, time: 0.43 }),
        ("Flicker exposure", Flicker { amount: 0.6, speed: 12.0, smoothness: 0.0, hold: 0.0, seed: 5, target: 1, time: 1.27 }),
        ("Flicker gamma", Flicker { amount: 0.7, speed: 6.0, smoothness: 1.0, hold: 0.0, seed: 9, target: 2, time: 0.81 }),
        ("BinocularVignette", BinocularVignette { amount: -0.9, spacing: 0.4, width: 0.8, height: 0.7, softness: 0.2, fringe: 3.0 }),
        // Particles (#1141): each kind, part way through their lives,
        // more of them than the preset: the frame here is small.
        ("Particles dust", klipp_effects::particles::preset(0).with_time(1.7).denser(4.0)),
        ("Particles snow", klipp_effects::particles::preset(1).with_time(3.1).denser(6.0)),
        ("Particles rain", klipp_effects::particles::preset(2).with_time(0.6).denser(6.0)),
        ("Particles sparks", klipp_effects::particles::preset(3).with_time(0.9).denser(6.0)),
        ("Particles embers", klipp_effects::particles::preset(4).with_time(2.2).denser(6.0)),
        // Blown up and left an hour in: the slide wrapped, held small.
        ("Particles dust an hour in", klipp_effects::particles::preset(0).with_time(3600.3).denser(4.0)),
        ("BinocularVignette lit", BinocularVignette { amount: 0.5, spacing: 0.6, width: 0.5, height: 0.9, softness: 0.05, fringe: -2.5 }),
    ]
}

/// (worst, mean) channel difference in display bytes.
fn diff(a: &[f32], b: &[f32]) -> (i32, f64) {
    let (a, b) = (linear_f32_rgba_to_srgb_u8(a), linear_f32_rgba_to_srgb_u8(b));
    let mut worst = 0;
    let mut sum = 0u64;
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        let d = (*x as i32 - *y as i32).abs();
        if i % 4 == 3 {
            continue;
        }
        worst = worst.max(d);
        sum += d as u64;
    }
    (worst, sum as f64 / (a.len() / 4 * 3) as f64)
}

/// A coarse checker over ramps, `w` by `h`: hard edges everywhere, where a
/// sample that lands on the wrong pixel shows.
fn checker(w: u32, h: u32) -> Vec<f32> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let check = if (x / 8 + y / 8) % 2 == 0 { 0.25 } else { 0.0 };
            v.extend_from_slice(&[fx * 0.8 + check, fy * 0.7, (1.0 - fx) * 0.6 + check, 1.0]);
        }
    }
    v
}

/// The settings where an operation that reads the same on both routes
/// rounded differently on the GPU, and a last-bit difference picked another
/// pixel or cell. Each drew more than a level apart before its fix, on this
/// frame at this size:
/// - Radial Blur at its sample settings: 16 levels at 160x90, 12 at
///   320x180, its zoom positions landing on halves (nearest-pixel samples,
///   the GPU's fused multiply-add; #1440).
/// - Film Grain of a fractional cell, as a size between two keys gives
///   (1.2 pixels at 160x90, 2.4 at 320x180): 144 levels, the GPU's
///   division putting an edge pixel in the next cell with an unrelated
///   grain value (#1441).
/// - Chromatic Aberration at its sample settings: 4 levels at a few edge
///   pixels (nearest-pixel samples; #1442).
#[test]
fn route_knife_edges_agree() {
    use BuiltinEffect::*;
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let radial = RadialBlur { center_x: 0.5, center_y: 0.5, amount: 10.0, samples: 12, mode: 0 };
    let cases = [
        ((160, 90), "RadialBlur sample", radial.clone()),
        ((320, 180), "RadialBlur sample", radial),
        ((320, 180), "RadialBlur 20", RadialBlur { center_x: 0.5, center_y: 0.5, amount: 20.0, samples: 16, mode: 0 }),
        ((160, 90), "FilmGrain 1.2-pixel cell", FilmGrain { amount: 0.5, size: 14.4, color: 0, shadows: 0.6, seed: 13 }),
        ((320, 180), "FilmGrain 2.4-pixel cell", FilmGrain { amount: 0.5, size: 14.4, color: 0, shadows: 0.6, seed: 13 }),
        ((320, 180), "ChromaticAberration sample", ChromaticAberration { red_shift: 2.0, blue_shift: -2.0, center_x: 0.5, center_y: 0.5 }),
    ];
    let mut failures = Vec::new();
    for ((w, h), name, effect) in cases {
        let input = checker(w, h);
        let fx = [effect];
        let (cpu, on_gpu) = (apply_effects(&fx, input.clone(), w, h), gpu.apply_effects(&fx, &input, w, h));
        let (worst, mean) = diff(&cpu, &on_gpu);
        eprintln!("{name:28} {w}x{h} {fx:?} CPU vs GPU worst {worst:3} mean {mean:6.3}");
        if worst > 1 {
            failures.push(format!("{name} at {w}x{h} ({fx:?}): CPU and GPU differ by {worst} levels"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Posterize at fractional levels, as between two keys, over values spaced
/// finely from black to two and a half times white. A value within a few
/// ULPs of a band edge fell in the next band on the GPU, whose `pow`
/// encodes only approximately: 7, 6, 6 and 4 levels apart at these levels
/// before both routes found bands against one set of edges (#1443). A
/// sweep of 348 levels from 2 to 256 found 39 apart then, and none now.
#[test]
fn posterize_band_edges_agree() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let (w, h) = (320u32, 180u32);
    let n = (w * h) as usize;
    let ramp: Vec<f32> = (0..n).flat_map(|i| {
        let v = i as f32 / n as f32 * 2.5;
        [v, v * 0.97 + 0.001, v * 1.03, 1.0]
    }).collect();
    let mut failures = Vec::new();
    for levels in [36.357014f32, 39.28101, 45.86, 75.831024] {
        let fx = [BuiltinEffect::Posterize { levels }];
        let (worst, mean) = diff(&apply_effects(&fx, ramp.clone(), w, h), &gpu.apply_effects(&fx, &ramp, w, h));
        eprintln!("Posterize {levels:10} CPU vs GPU worst {worst:3} mean {mean:6.3}");
        if worst > 1 {
            failures.push(format!("Posterize at {levels} levels: CPU and GPU differ by {worst} levels"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn every_effect_matches_between_cpu_and_gpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let input = frame();
    let mut failures = Vec::new();
    for (name, effect) in cases() {
        let fx = [effect];
        let cpu = apply_effects(&fx, input.clone(), W, H);
        let on_gpu = gpu.apply_effects(&fx, &input, W, H);
        let (worst, mean) = diff(&cpu, &on_gpu);
        // And each effect does something: a pass-through on one side
        // would match a pass-through on the other.
        let (_, change) = diff(&input, &on_gpu);
        eprintln!("{name:20} CPU vs GPU worst {worst:3} mean {mean:6.3} | change {change:7.3}");
        if worst > 1 {
            failures.push(format!("{name}: CPU and GPU differ by {worst} levels"));
        }
        if change < 0.05 {
            failures.push(format!("{name}: leaves the frame as it was"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// What the two interpolations mean: both read the LUT exactly on its
/// grid points, and between them trilinear pulls the sample towards the
/// whole cell while tetrahedral follows the four corners around it. On a
/// coarse LUT — the kind a look ships as — the difference is plain.
#[test]
fn the_interpolations_agree_on_the_grid_and_differ_between_it() {
    let n = 5usize;
    let step = 1.0 / (n - 1) as f32;
    let mut data = Vec::with_capacity(n * n * n * 3);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let (rv, gv, bv) = (r as f32 * step, g as f32 * step, b as f32 * step);
                // Strong cross-talk, so the cell interiors disagree.
                data.extend_from_slice(&[
                    (rv * 0.55 + 0.55 * gv * bv).clamp(0.0, 1.0),
                    (gv * 0.6 + 0.5 * rv * (1.0 - bv)).clamp(0.0, 1.0),
                    (bv * 0.5 + 0.6 * rv * gv).clamp(0.0, 1.0),
                ]);
            }
        }
    }
    // Exactly on the grid — the LUT is indexed on the display-encoded
    // value, so the input is that grid value decoded to light.
    let light = |encoded: f32| klipp_color::transfer::srgb_to_linear(encoded);
    let on_grid: Vec<f32> = (0..n)
        .flat_map(|i| {
            let v = light(i as f32 * step);
            [v, v, v, 1.0]
        })
        .collect();
    let tri = klipp_effects::apply_3d_lut(on_grid.clone(), &data, n as u32, 1.0, false);
    let tet = klipp_effects::apply_3d_lut(on_grid.clone(), &data, n as u32, 1.0, true);
    for (i, (a, b)) in tri.iter().zip(&tet).enumerate() {
        assert!((a - b).abs() < 1e-5, "grid point {i}: {a} vs {b}");
    }
    // Between the grid points: measurably different.
    let mid: Vec<f32> = (0..n - 1)
        .flat_map(|i| {
            let v = (i as f32 + 0.5) * step;
            [light(v), light(1.0 - v), light(0.5), 1.0]
        })
        .collect();
    let tri = klipp_effects::apply_3d_lut(mid.clone(), &data, n as u32, 1.0, false);
    let tet = klipp_effects::apply_3d_lut(mid.clone(), &data, n as u32, 1.0, true);
    let (worst, mean) = diff(&tri, &tet);
    eprintln!("mid-cell, 5-point LUT: trilinear vs tetrahedral worst {worst} mean {mean:.2}");
    assert!(worst >= 2, "the interpolations barely differ mid-cell (worst {worst})");
}

/// The global LUT, both ways and both interpolations: a warm, contrasty
/// 17-point LUT at 70%, with cross-talk between the channels as a look
/// LUT has — a LUT whose channels are independent is interpolated
/// identically by trilinear and tetrahedral, and would hide the
/// difference between them.
#[test]
fn the_lut_matches_between_cpu_and_gpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let n = 17usize;
    let mut data = Vec::with_capacity(n * n * n * 3);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let s = |i: usize| i as f32 / (n - 1) as f32;
                let curve = |v: f32| (v * v * (3.0 - 2.0 * v)).clamp(0.0, 1.0);
                let (rv, gv, bv) = (s(r), s(g), s(b));
                // A warm look with a teal shadow pull: each output
                // channel reads the other two.
                data.extend_from_slice(&[
                    (curve(rv) * 1.05 + 0.10 * gv * bv).clamp(0.0, 1.0),
                    (curve(gv) + 0.06 * rv * bv).clamp(0.0, 1.0),
                    (curve(bv) * 0.9 + 0.12 * gv * (1.0 - rv)).clamp(0.0, 1.0),
                ]);
            }
        }
    }
    let input = frame();
    for tetrahedral in [false, true] {
        let cpu = klipp_effects::apply_3d_lut(input.clone(), &data, n as u32, 0.7, tetrahedral);
        let on_gpu = gpu.apply_3d_lut_gpu(&input, W, H, &data, n as u32, 0.7, tetrahedral, (None, None));
        let (worst, mean) = diff(&cpu, &on_gpu);
        let (_, change) = diff(&input, &on_gpu);
        let how = if tetrahedral { "tetrahedral" } else { "trilinear" };
        eprintln!("LUT {how}: CPU vs GPU worst {worst} mean {mean:.3}, change {change:.2}");
        assert!(worst <= 1, "LUT ({how}) differs by {worst} levels");
        assert!(change > 1.0);
    }
    // The two interpolations differ inside the cells and agree on the
    // grid points, so the same LUT gives a different picture.
    let tri = klipp_effects::apply_3d_lut(input.clone(), &data, n as u32, 1.0, false);
    let tet = klipp_effects::apply_3d_lut(input.clone(), &data, n as u32, 1.0, true);
    let (worst, mean) = diff(&tri, &tet);
    eprintln!("trilinear vs tetrahedral: worst {worst} mean {mean:.3}");
    assert!(worst > 0, "the two interpolations gave the same picture");
}

/// The brightness-gated effects follow the *working space's* luminance
/// weights, on both paths.
///
/// The test above cannot catch this: both sides default to BT.709, so it
/// would pass just as well if the weights were ignored entirely. This one
/// runs the same effects under BT.2020's weights and AP1's, and asserts
/// two things — that CPU and GPU still agree, and that the picture
/// actually *moved*, which is what proves the numbers are being read.
///
/// Why it matters: an effect that gates on brightness mis-estimates it in
/// any other basis, and the weights differ materially. BT.2020's green is
/// 0.678 against BT.709's 0.715, AP1's 0.674. Before this the *grade*
/// tools read the working space's luma and the effects did not.
#[test]
fn the_brightness_gated_effects_follow_the_working_space() {
    use BuiltinEffect::*;
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let input = frame();

    // BT.709, BT.2020 and ACES AP1. The first is the default, so it also
    // pins that installing weights explicitly changes nothing.
    const BT709: [f32; 3] = [0.2126, 0.7152, 0.0722];
    const BT2020: [f32; 3] = [0.2627, 0.6780, 0.0593];
    const AP1: [f32; 3] = [0.2722, 0.6741, 0.0537];

    // The effects that touch the luma, and whether the weights can reach
    // the picture. Glow is the interesting `false`: its screen blend
    // desaturates the layer *towards* the luma, and Glow passes
    // desaturation 0, so the luma term cancels exactly. Keeping it here
    // with the expectation inverted says that out loud — if Glow ever
    // starts moving with the working space, something has changed about
    // what it means.
    //
    // LightRays keeps the parameters the test above uses. With a shorter
    // ray and a higher intensity its CPU and GPU accumulations diverge by
    // three levels, which is pre-existing — the BT.709 column is the
    // arithmetic this change replaced, byte for byte — and not this
    // test's to chase.
    let cases: Vec<(&str, BuiltinEffect, bool)> = vec![
        ("Bloom", Bloom { threshold: 0.5, radius: 4.0, intensity: 0.8 }, true),
        ("Glow", Glow { radius: 4.0, intensity: 0.6, threshold: 0.0 }, false),
        ("Diffusion", Diffusion { threshold: 0.45, radius: 5.0, amount: 0.7, desaturation: 0.3 }, true),
        ("LightRays", LightRays {
            center_x: 0.5, center_y: 0.3, threshold: 0.6,
            samples: 32, decay: 0.95, intensity: 0.5,
        }, true),
        ("AnamorphicStreak", AnamorphicStreak {
            threshold: 0.5, length: 20.0, intensity: 0.8, tint: 0.4,
        }, true),
        ("FilmHalation", FilmHalation {
            threshold: 0.5, radius: 5.0, intensity: 0.8, hue: 0.05,
        }, true),
        ("EdgeDetect", EdgeDetect { strength: 1.0, mix: 0.8 }, true),
        ("Denoise", Denoise { spatial_sigma: 2.0, color_sigma: 0.2, iterations: 1 }, true),
        ("Vibrance", Vibrance { amount: 0.8 }, true),
    ];

    let mut failures = Vec::new();
    for (name, effect, reads_luma) in cases {
        let fx = [effect];
        let mut pictures = Vec::new();
        for (space, luma) in [("BT.709", BT709), ("BT.2020", BT2020), ("AP1", AP1)] {
            gpu.set_luma_coefs(luma);
            let on_gpu = gpu.apply_effects(&fx, &input, W, H);
            let cpu = klipp_effects::apply_effects_with_luma(
                &fx,
                input.clone(),
                W,
                H,
                luma,
            );
            let (worst, mean) = diff(&cpu, &on_gpu);
            eprintln!("{name:18} {space:8} CPU vs GPU worst {worst:3} mean {mean:6.3}");
            if worst > 1 {
                failures.push(format!("{name} under {space}: CPU and GPU differ by {worst}"));
            }
            pictures.push((space, on_gpu));
        }

        // The weights have to reach the arithmetic. Comparing BT.709
        // against AP1 — the widest separation of the three — the picture
        // must move somewhere, or the effect is ignoring them and every
        // agreement above is agreement about a constant.
        let (worst, _) = diff(&pictures[0].1, &pictures[2].1);
        eprintln!("{name:18} BT.709 vs AP1: worst {worst} levels");
        if reads_luma && worst == 0 {
            failures.push(format!(
                "{name}: identical under BT.709 and AP1, so it is not reading the weights"
            ));
        }
        if !reads_luma && worst != 0 {
            failures.push(format!(
                "{name} moved by {worst} levels with the weights, but its luma term \
                 should cancel — has what it means changed?"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
