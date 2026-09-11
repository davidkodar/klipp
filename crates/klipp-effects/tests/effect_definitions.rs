//! What the built-in effects do, pinned on single colours. The CPU and
//! GPU agree (`cpu_gpu_effect_parity.rs`), so these run on the CPU.
//!
//! The perceptual adjustments work on the display-encoded picture, as in
//! other editors; the rest on linear light.

use klipp_effects::{apply_effects, linear_f32_rgba_to_srgb_u8, srgb_u8_to_linear_f32_rgba, BuiltinEffect, ColorFadeMode};

/// One effect on a 3×3 frame of one sRGB colour; the centre pixel back as
/// sRGB bytes.
fn on(rgb: [u8; 3], effect: BuiltinEffect) -> [u8; 3] {
    let px = [rgb[0], rgb[1], rgb[2], 255].repeat(9);
    let out = apply_effects(&[effect], srgb_u8_to_linear_f32_rgba(&px), 3, 3);
    let b = linear_f32_rgba_to_srgb_u8(&out);
    [b[16], b[17], b[18]]
}

fn near(got: [u8; 3], want: [u8; 3], label: &str) {
    for c in 0..3 {
        assert!((got[c] as i32 - want[c] as i32).abs() <= 1, "{label}: got {got:?}, want {want:?}");
    }
}

/// Invert is the photo negative: 255 − the displayed value. (In linear
/// light 18% grey became a near-white 233.)
#[test]
fn invert_is_the_negative_of_what_is_displayed() {
    near(on([118, 40, 200], BuiltinEffect::Invert { mix: 1.0 }), [137, 215, 55], "invert");
    near(on([118, 118, 118], BuiltinEffect::Invert { mix: 0.0 }), [118, 118, 118], "no mix");
}

/// Posterize bands evenly on the display scale, and white stays white.
#[test]
fn posterize_bands_evenly_on_the_display_scale() {
    let p = |v| on([v, v, v], BuiltinEffect::Posterize { levels: 5.0 })[0];
    assert_eq!(p(255), 255, "white stays white");
    assert_eq!(p(0), 0);
    // floor(100/255 × 5) = 1 of 4 steps: 64.
    assert!((p(100) as i32 - 64).abs() <= 1, "{}", p(100));
    assert!((p(180) as i32 - 191).abs() <= 1, "{}", p(180));
}

/// Levels' points are on the displayed scale: in black 0.1 is sRGB 25.5.
#[test]
fn levels_points_are_on_the_displayed_scale() {
    let l = |v| on([v, v, v], BuiltinEffect::Levels { in_black: 0.1, in_white: 0.9, gamma: 1.0, out_black: 0.0, out_white: 1.0 })[0];
    assert_eq!(l(25), 0);
    assert_eq!(l(230), 255);
    // Mid-range: (128/255 − 0.1) / 0.8 = 0.5024 → 128.
    assert!((l(128) as i32 - 128).abs() <= 1, "{}", l(128));
}

/// Gamma is a power on the displayed value: gamma 2 lifts 128 to 181.
#[test]
fn gamma_is_a_power_on_the_displayed_value() {
    near(on([128, 128, 128], BuiltinEffect::Gamma { gamma: 2.0 }), [181, 181, 181], "gamma 2");
}

/// A colour fade starts at the colour and mixes in linear light.
#[test]
fn a_colour_fade_mixes_the_colour_in_linear_light() {
    let fade = |progress| BuiltinEffect::ColorFade { color: [0, 0, 0, 255], duration_secs: 1.0, mode: ColorFadeMode::In, progress };
    near(on([200, 100, 50], fade(0.0)), [0, 0, 0], "start");
    // Half of white's light: linear 0.5, sRGB 188.
    near(on([255, 255, 255], fade(0.5)), [188, 188, 188], "half way");
}

/// Vibrance lifts the saturation of dull colours more than of vivid ones
/// (on the GPU it did nothing at all).
#[test]
fn vibrance_favours_dull_colours() {
    let sat = |c: [u8; 3]| (*c.iter().max().unwrap() - *c.iter().min().unwrap()) as f32;
    let dull = [140, 120, 110];
    let vivid = [200, 60, 60];
    let v = BuiltinEffect::Vibrance { amount: 0.8 };
    let (d, s) = (on(dull, v.clone()), on(vivid, v));
    let (dull_gain, vivid_gain) = (sat(d) / sat(dull), sat(s) / sat(vivid));
    assert!(dull_gain > 1.3, "dull {dull:?} -> {d:?}");
    assert!(dull_gain > vivid_gain + 0.1, "dull x{dull_gain:.2}, vivid x{vivid_gain:.2}");
}

/// Film halation spreads light in its own colour: at hue 0 deep red,
/// adding to red alone. (The GPU ignored the hue.)
#[test]
fn film_halation_glows_in_its_own_colour() {
    let h = |hue| on([200, 200, 200], BuiltinEffect::FilmHalation { threshold: 0.05, radius: 2.0, intensity: 0.8, hue });
    let red = h(0.0);
    assert!(red[0] > 210 && red[1] <= 201 && red[2] <= 201, "hue 0: {red:?}");
    let warm = h(1.0);
    assert!(warm[1] > red[1] + 3, "hue 1 adds green: {warm:?} vs {red:?}");
}

/// Diffusion's desaturation takes the colour out of the glow.
#[test]
fn diffusion_desaturates_its_glow() {
    let d = |desaturation| on([240, 120, 120], BuiltinEffect::Diffusion { amount: 0.8, radius: 2.0, threshold: 0.1, desaturation });
    let (coloured, neutral) = (d(0.0), d(1.0));
    let spread = |c: [u8; 3]| c[0] as i32 - c[2] as i32;
    assert!(spread(neutral) < spread(coloured), "desaturated {neutral:?} vs {coloured:?}");
}

/// Radial zoom blur spreads samples over a fraction of the distance to
/// the centre: the centre stays sharp, the edges blur.
#[test]
fn zoom_blur_grows_with_distance_from_the_centre() {
    let (w, h) = (41u32, 41u32);
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            // Stripes: zoom samples along lines through the centre,
            // which cross them.
            let _ = y;
            let v = if (x / 2) % 2 == 0 { 0.9 } else { 0.1 };
            px.extend_from_slice(&[v, v, v, 1.0]);
        }
    }
    let out = apply_effects(&[BuiltinEffect::RadialBlur { center_x: 0.5, center_y: 0.5, amount: 20.0, samples: 16, mode: 0 }], px.clone(), w, h);
    let at = |p: &[f32], x: u32, y: u32| p[((y * w + x) * 4) as usize];
    assert!((at(&out, 20, 20) - at(&px, 20, 20)).abs() < 1e-6, "the centre moves");
    let edge = (at(&out, 1, 1) - 0.5).abs();
    assert!(edge < 0.3, "the corner is not blurred: {}", at(&out, 1, 1));
}
