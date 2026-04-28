//! Linear-light colour operations.
//!
//! Every function here operates on `&mut [f32]` RGBA where each
//! channel holds *physically linear* intensity: 0.18 is 18 %
//! perceptual mid-grey, 1.0 is diffuse white, and values may legally
//! exceed 1.0 (highlight headroom). Alpha stays linear per the sRGB
//! convention — never gamma-encoded.
//!
//! The matching u8 sRGB helpers in [`crate::transforms`] are retained
//! so downstream crates can flip call sites one at a time. Once every
//! consumer reads linear frames directly (see
//! [`klipp_engine::LinearFrame`]), the u8 copies go away.
//!
//! # Why there are no `.clamp(0.0, 1.0)` calls
//!
//! In linear light, values above 1.0 are legitimate — a bright
//! specular highlight can easily sit at 5.0 or 10.0 linear. Clamping
//! at every operation is what destroys highlight recovery. These
//! functions never clamp; the viewer applies a tone map and the
//! encoder encodes to the target colour space — those are the only
//! legitimate places to resolve out-of-range values.
//!
//! # Working-colorspace audit (Phase E S5)
//!
//! Each operation in this module is one of:
//!
//! 1. **Gamut-agnostic** — the math is the same function applied
//!    independently to each channel (or to a basis-independent space
//!    like HSV). Output behaviour is correct under any working
//!    colorspace. Examples: [`apply_exposure`] (per-channel multiply
//!    by 2^stops), [`apply_lift_gamma_gain`] / `_offset` (per-channel
//!    affine + gamma), [`apply_temperature_tint`] (additive/scalar
//!    R/B trim), [`apply_global_mix`] (mix between two RGBA buffers).
//!
//! 2. **Luma-weighted** — the math computes a "luminance" scalar from
//!    the pixel and uses it to pull channels toward / away from
//!    neutral. The luminance coefficients are working-space-specific
//!    (`Yr·R + Yg·G + Yb·B` with `(Yr, Yg, Yb)` being the second row
//!    of the working-space's RGB→XYZ matrix). Examples:
//!    [`apply_saturation`], [`apply_vibrance`], [`apply_clarity`],
//!    [`apply_tone_controls`] (highlights/shadows/whites/blacks
//!    keying), [`apply_log_wheels`], [`apply_wheel_sat_bri`].
//!
//! Group 2 ops shipped with BT.709 luma hardcoded — fine while the
//! pipeline only had `Rec709Linear` working space. Under `AcesCG`
//! they would compute "BT.709-weighted luma" instead of
//! "AP1-weighted luma" — the saturation axis tilts off neutral
//! relative to the AP1 white point.
//!
//! New `_ws` variants accept a [`klipp_core::WorkingColorspace`] and
//! pick the appropriate luma vector via [`luma_coefs`]. The
//! BT.709-named functions are retained as thin wrappers so existing
//! callers stay byte-identical until [`klipp_engine`] flips them on
//! the active project's working space (a separate session — wiring
//! through the executor is bigger than fits here).

use klipp_core::WorkingColorspace;

/// Per-working-space luminance coefficients `(Yr, Yg, Yb)`.
///
/// Source: row 1 of each working space's RGB → CIE XYZ matrix. The
/// BT.709 / sRGB row is the canonical "Rec.709 luma" used throughout
/// the existing pipeline. The AP1 row matches the AMPAS aces-dev
/// `AP1_RGB2Y` constant.
///
/// `Bt2020Linear` uses the BT.2020 NCL row — included so the variant
/// has a defensible answer even though the end-to-end pipeline isn't
/// wired for BT.2020 yet. Unknown future variants would fall back to
/// BT.709 via `WorkingColorspace::from_u8`'s safe-default behaviour.
#[inline]
pub fn luma_coefs(ws: WorkingColorspace) -> [f32; 3] {
    match ws {
        WorkingColorspace::Rec709Linear => [0.2126, 0.7152, 0.0722],
        WorkingColorspace::AcesCG       => [0.272_228_72, 0.674_081_77, 0.053_689_52],
        WorkingColorspace::Bt2020Linear => [0.262_700, 0.678_000, 0.059_300],
    }
}

/// Compute the working-space-correct luma of a single linear-light
/// RGBA pixel (alpha is ignored).
#[inline]
pub fn luma_of(pixel: &[f32], ws: WorkingColorspace) -> f32 {
    let c = luma_coefs(ws);
    c[0] * pixel[0] + c[1] * pixel[1] + c[2] * pixel[2]
}

/// Exposure compensation: multiply by 2^stops.
///
/// Identity when `stops == 0.0`. +1 stop on 0.18 linear produces 0.36
/// linear (one additional photographic stop — literally doubles the
/// light). Unlike the sRGB u8 version, this doesn't clip at 1.0;
/// highlights can exceed 1.0 and will be resolved at the display or
/// encode boundary.
pub fn apply_exposure(rgba: &mut [f32], stops: f32) {
    if stops == 0.0 {
        return;
    }
    let factor = 2.0f32.powf(stops);
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[0] *= factor;
        pixel[1] *= factor;
        pixel[2] *= factor;
        // Alpha is not affected by exposure.
    }
}

/// Contrast: S-curve pivoting at 18 % perceptual mid-grey (0.18
/// linear).
///
/// `factor > 0.0` steepens the curve (higher contrast); `factor < 0.0`
/// flattens it. `factor == 0.0` is identity. Pivot at 0.18 keeps
/// perceptual mid-grey fixed — the sRGB u8 version pivoted at 0.5
/// sRGB (≈ 0.21 linear) which visibly shifted mid-grey every time
/// the slider moved.
///
/// Unlike the sRGB u8 version this doesn't clamp — highlights that
/// cross 1.0 through contrast expansion are preserved for later
/// stages (tone-mapping at display, colour-space encode at export).
pub fn apply_contrast(rgba: &mut [f32], factor: f32) {
    if factor == 0.0 {
        return;
    }
    let scale = 1.0 + factor;
    const PIVOT: f32 = 0.18;
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[0] = (pixel[0] - PIVOT) * scale + PIVOT;
        pixel[1] = (pixel[1] - PIVOT) * scale + PIVOT;
        pixel[2] = (pixel[2] - PIVOT) * scale + PIVOT;
    }
}

/// Vibrance: saturation-weighted boost. Muted colours get a stronger
/// boost; already-saturated colours (including skin tones) are left
/// mostly alone.
///
/// `amount == 0.0` is identity. Range roughly −1.0..+1.0 (−1 collapses
/// to grey, +1 pushes hard). No clamp — out-of-range channels are
/// legal linear light. Luma vector follows [`luma_coefs`] for the
/// active working colorspace.
pub fn apply_vibrance_ws(rgba: &mut [f32], amount: f32, ws: WorkingColorspace) {
    if amount == 0.0 {
        return;
    }
    let c = luma_coefs(ws);
    for pixel in rgba.chunks_exact_mut(4) {
        let max = pixel[0].max(pixel[1]).max(pixel[2]);
        let min = pixel[0].min(pixel[1]).min(pixel[2]);
        let sat = max - min;
        let luma = c[0] * pixel[0] + c[1] * pixel[1] + c[2] * pixel[2];
        let weight = 1.0 - sat.clamp(0.0, 1.0);
        let boost = 1.0 + amount * weight;
        pixel[0] = luma + (pixel[0] - luma) * boost;
        pixel[1] = luma + (pixel[1] - luma) * boost;
        pixel[2] = luma + (pixel[2] - luma) * boost;
    }
}

/// Back-compat wrapper for [`apply_vibrance_ws`] pinned to
/// [`WorkingColorspace::Rec709Linear`].
#[inline]
pub fn apply_vibrance(rgba: &mut [f32], amount: f32) {
    apply_vibrance_ws(rgba, amount, WorkingColorspace::Rec709Linear)
}

/// Clarity — midtone detail enhancement via unsharp-mask on the
/// linear luma channel.
///
/// Box-blur the luma, take `detail = luma − blurred`, add a
/// midtone-weighted scaled version of `detail` back into each channel.
/// Done in linear so the blur respects physical light (gamma-averaged
/// edges are darker than linear-averaged edges, which is why blurs
/// in sRGB look wrong at boundaries).
///
/// Midtone weight peaks around linear 0.18 — the perceptually
/// mid-grey point — so the enhancement targets the region where the
/// human visual system resolves most detail. The sRGB u8 version
/// peaked at 0.5 sRGB, which landed around linear 0.21.
pub fn apply_clarity(rgba: &mut [f32], width: u32, height: u32, amount: f32) {
    if amount == 0.0 || width == 0 || height == 0 {
        return;
    }
    let w = width as usize;
    let h = height as usize;
    let pixel_count = w * h;

    // 1. Extract linear luma.
    let mut luma = vec![0.0f32; pixel_count];
    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        luma[i] = 0.2126 * chunk[0] + 0.7152 * chunk[1] + 0.0722 * chunk[2];
    }

    // 2. Box blur — two-pass separable, radius 20 — same parameters
    // as the sRGB version so the spatial frequency response matches.
    let radius: usize = 20;
    let mut temp = vec![0.0f32; pixel_count];
    for y in 0..h {
        let row = y * w;
        let mut sum = 0.0f32;
        let mut count = 0u32;
        for x in 0..=radius.min(w - 1) {
            sum += luma[row + x];
            count += 1;
        }
        temp[row] = sum / count as f32;
        for x in 1..w {
            let right = x + radius;
            if right < w {
                sum += luma[row + right];
                count += 1;
            }
            if x > radius {
                sum -= luma[row + x - radius - 1];
                count -= 1;
            }
            temp[row + x] = sum / count as f32;
        }
    }
    let mut blurred = temp;
    let mut temp = vec![0.0f32; pixel_count];
    for x in 0..w {
        let mut sum = 0.0f32;
        let mut count = 0u32;
        for y in 0..=radius.min(h - 1) {
            sum += blurred[y * w + x];
            count += 1;
        }
        temp[x] = sum / count as f32;
        for y in 1..h {
            let bottom = y + radius;
            if bottom < h {
                sum += blurred[bottom * w + x];
                count += 1;
            }
            if y > radius {
                sum -= blurred[(y - radius - 1) * w + x];
                count -= 1;
            }
            temp[y * w + x] = sum / count as f32;
        }
    }
    blurred = temp;

    // 3. Detail enhancement. Midtone weight: smooth bell around 0.18
    // linear. `1 - |2*linear_luma - 1|` peaked at 0.5 in the sRGB
    // version; for linear we centre on 0.18 so the enhancement lives
    // where the eye resolves most detail.
    for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
        let detail = luma[i] - blurred[i];
        let midtone_weight = (1.0 - ((luma[i] / 0.18) - 1.0).abs()).max(0.0);
        let adjustment = detail * amount * midtone_weight;
        chunk[0] += adjustment;
        chunk[1] += adjustment;
        chunk[2] += adjustment;
    }
}

/// Temperature / tint shift.
/// `temperature > 0` = warmer (more red, less blue).
/// `tint > 0` = more green; `< 0` = more magenta.
/// Both `0.0` is identity. Input range roughly ±100.
///
/// No clamp — pushing temperature past the gamut is legal; later
/// gamut mapping at display/export resolves it.
pub fn apply_temperature_tint(rgba: &mut [f32], temperature: f32, tint: f32) {
    if temperature == 0.0 && tint == 0.0 {
        return;
    }
    let t = temperature / 100.0 * 0.1;
    let g = tint / 100.0 * 0.1;
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[0] += t;
        pixel[1] += g;
        pixel[2] -= t;
    }
}

/// Tone controls — highlights, shadows, whites, blacks.
///
/// Each parameter is an adjustment in `−1.0..+1.0`; `0.0` is identity.
/// Weights are computed from linear luma; band centres at
/// `highlights=0.21`, `whites=0.52`, `shadows=0.21`, `blacks=0.032`
/// — the linear-light equivalents of the sRGB 0.5 / 0.8 / 0.2 thresh-
/// olds the u8 version used, so the slider positions feel roughly
/// the same.
pub fn apply_tone_controls(
    rgba: &mut [f32],
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
) {
    if highlights == 0.0 && shadows == 0.0 && whites == 0.0 && blacks == 0.0 {
        return;
    }
    // sRGB thresholds converted to linear via srgb_to_linear:
    //   0.5 → ≈ 0.2140
    //   0.8 → ≈ 0.6038
    //   0.2 → ≈ 0.0331
    const HI_CENTRE: f32 = 0.214;
    const WHITE_CENTRE: f32 = 0.604;
    const SHADOW_CENTRE: f32 = 0.214;
    const BLACK_CENTRE: f32 = 0.033;

    for pixel in rgba.chunks_exact_mut(4) {
        let luma = 0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2];

        let hi_w = (luma - HI_CENTRE).max(0.0) * (1.0 / (1.0 - HI_CENTRE));
        let lo_w = (SHADOW_CENTRE - luma).max(0.0) * (1.0 / SHADOW_CENTRE);
        let white_w = (luma - WHITE_CENTRE).max(0.0) * (1.0 / (1.0 - WHITE_CENTRE));
        let black_w = (BLACK_CENTRE - luma).max(0.0) * (1.0 / BLACK_CENTRE);

        let adj = highlights * hi_w + shadows * lo_w + whites * white_w + blacks * black_w;
        pixel[0] += adj;
        pixel[1] += adj;
        pixel[2] += adj;
    }
}

/// Lift / gamma / gain colour-wheel adjustments, weighted by tonal band.
///
/// Lift affects shadows (weight `1 − luma`), gamma affects midtones
/// (weight `sin(π·luma)`), gain affects highlights (weight `luma`).
/// Each parameter is a pre-converted `(r, g, b)` shift from
/// `wheel_offset_to_rgb`.
pub fn apply_lift_gamma_gain(
    rgba: &mut [f32],
    lift: (f32, f32, f32),
    gamma: (f32, f32, f32),
    gain: (f32, f32, f32),
) {
    let is_zero = |v: (f32, f32, f32)| v.0.abs() < 0.0001 && v.1.abs() < 0.0001 && v.2.abs() < 0.0001;
    if is_zero(lift) && is_zero(gamma) && is_zero(gain) {
        return;
    }
    for pixel in rgba.chunks_exact_mut(4) {
        let luma = 0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2];
        let lift_w = (1.0 - luma).max(0.0);
        let gain_w = luma;
        let gamma_w = (luma * std::f32::consts::PI).sin().max(0.0);
        pixel[0] += lift.0 * lift_w + gamma.0 * gamma_w + gain.0 * gain_w;
        pixel[1] += lift.1 * lift_w + gamma.1 * gamma_w + gain.1 * gain_w;
        pixel[2] += lift.2 * lift_w + gamma.2 * gamma_w + gain.2 * gain_w;
    }
}

/// [`apply_lift_gamma_gain`] plus a uniform offset applied to every
/// channel regardless of luma.
pub fn apply_lift_gamma_gain_offset(
    rgba: &mut [f32],
    lift: (f32, f32, f32),
    gamma: (f32, f32, f32),
    gain: (f32, f32, f32),
    offset: (f32, f32, f32),
) {
    let is_zero = |v: (f32, f32, f32)| v.0.abs() < 0.0001 && v.1.abs() < 0.0001 && v.2.abs() < 0.0001;
    if is_zero(lift) && is_zero(gamma) && is_zero(gain) && is_zero(offset) {
        return;
    }
    for pixel in rgba.chunks_exact_mut(4) {
        let luma = 0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2];
        let lift_w = (1.0 - luma).max(0.0);
        let gain_w = luma;
        let gamma_w = (luma * std::f32::consts::PI).sin().max(0.0);
        pixel[0] += lift.0 * lift_w + gamma.0 * gamma_w + gain.0 * gain_w + offset.0;
        pixel[1] += lift.1 * lift_w + gamma.1 * gamma_w + gain.1 * gain_w + offset.1;
        pixel[2] += lift.2 * lift_w + gamma.2 * gamma_w + gain.2 * gain_w + offset.2;
    }
}

/// Log-domain colour wheels — Low / Mid / High with Gaussian weights
/// in log space. Smoother overlap than linear lift/gamma/gain wheels;
/// matches how colourists think about HDR and log-encoded footage.
pub fn apply_log_wheels(
    rgba: &mut [f32],
    low: (f32, f32, f32),
    mid: (f32, f32, f32),
    high: (f32, f32, f32),
) {
    let is_zero = |v: (f32, f32, f32)| v.0.abs() < 0.0001 && v.1.abs() < 0.0001 && v.2.abs() < 0.0001;
    if is_zero(low) && is_zero(mid) && is_zero(high) {
        return;
    }
    for pixel in rgba.chunks_exact_mut(4) {
        let luma = (0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2]).max(0.001);
        // Map ~0.001..1.0 linear → ~0..1 via log2.
        let log_luma = ((luma + 0.001).log2() + 10.0) / 10.0;

        let low_w = (-(log_luma - 0.15).powi(2) / (2.0 * 0.15_f32.powi(2))).exp();
        let mid_w = (-(log_luma - 0.50).powi(2) / (2.0 * 0.20_f32.powi(2))).exp();
        let high_w = (-(log_luma - 0.85).powi(2) / (2.0 * 0.15_f32.powi(2))).exp();

        pixel[0] += low.0 * low_w + mid.0 * mid_w + high.0 * high_w;
        pixel[1] += low.1 * low_w + mid.1 * mid_w + high.1 * high_w;
        pixel[2] += low.2 * low_w + mid.2 * mid_w + high.2 * high_w;
    }
}

/// Per-wheel saturation and brightness — brightness is an additive
/// shift weighted by tonal band; saturation lerps toward the
/// post-brightness luma with a band-weighted factor.
#[allow(clippy::too_many_arguments)]
pub fn apply_wheel_sat_bri(
    rgba: &mut [f32],
    lift_sat: f32, lift_bri: f32,
    gamma_sat: f32, gamma_bri: f32,
    gain_sat: f32, gain_bri: f32,
    offset_sat: f32, offset_bri: f32,
) {
    let all_zero = lift_sat.abs() < 0.0001 && lift_bri.abs() < 0.0001
        && gamma_sat.abs() < 0.0001 && gamma_bri.abs() < 0.0001
        && gain_sat.abs() < 0.0001 && gain_bri.abs() < 0.0001
        && offset_sat.abs() < 0.0001 && offset_bri.abs() < 0.0001;
    if all_zero {
        return;
    }
    for pixel in rgba.chunks_exact_mut(4) {
        let luma = 0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2];
        let lift_w = (1.0 - luma).max(0.0);
        let gamma_w = (luma * std::f32::consts::PI).sin().max(0.0);
        let gain_w = luma;

        let sat_adj = lift_sat * lift_w + gamma_sat * gamma_w + gain_sat * gain_w + offset_sat;
        let bri_adj = lift_bri * lift_w + gamma_bri * gamma_w + gain_bri * gain_w + offset_bri;

        let r = pixel[0] + bri_adj;
        let g = pixel[1] + bri_adj;
        let b = pixel[2] + bri_adj;

        let luma2 = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let sat_factor = 1.0 + sat_adj;
        pixel[0] = luma2 + (r - luma2) * sat_factor;
        pixel[1] = luma2 + (g - luma2) * sat_factor;
        pixel[2] = luma2 + (b - luma2) * sat_factor;
    }
}

/// Hue rotation in degrees (0-360). Rotates every pixel's hue by the
/// given amount.
///
/// Hue rotation lives in a chromaticity space — doing it in RGB
/// (linear *or* sRGB) is a convention. The current implementation
/// converts linear RGB → HSL on clamped-to-[0,1] values, rotates,
/// and converts back. Values above 1.0 keep their magnitude via a
/// scale factor. Clamping inside the HSL calculation is required
/// because HSL's `delta / (1 - |2L-1|)` formula diverges at L > 1.
/// A future pass could move this to OKLab / OKHSL for a perceptually
/// uniform rotation, but the pragmatic translation stays here.
pub fn apply_hue_rotation(rgba: &mut [f32], degrees: f32) {
    if degrees.abs() < 0.01 {
        return;
    }
    let angle = degrees / 360.0;
    for pixel in rgba.chunks_exact_mut(4) {
        let r_raw = pixel[0];
        let g_raw = pixel[1];
        let b_raw = pixel[2];

        // HSL requires bounded inputs; scale brightness back in
        // proportionally so HDR pixels survive the rotation.
        let magnitude = r_raw.max(g_raw).max(b_raw).max(1.0);
        let r = r_raw / magnitude;
        let g = g_raw / magnitude;
        let b = b_raw / magnitude;

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let l = (max + min) * 0.5;
        if delta < 0.0001 {
            continue;
        }
        let s = delta / (1.0 - (2.0 * l - 1.0).abs());
        let h = if max == r {
            ((g - b) / delta).rem_euclid(6.0) / 6.0
        } else if max == g {
            ((b - r) / delta + 2.0) / 6.0
        } else {
            ((r - g) / delta + 4.0) / 6.0
        };
        let h = (h + angle).rem_euclid(1.0);

        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
        let m = l - c * 0.5;
        let (nr, ng, nb) = match (h * 6.0) as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        pixel[0] = (nr + m) * magnitude;
        pixel[1] = (ng + m) * magnitude;
        pixel[2] = (nb + m) * magnitude;
    }
}

/// Global mix — blend the graded buffer with the ungraded original.
/// `mix == 1.0` keeps the graded version; `0.0` restores the
/// original; values between do a straight lerp in linear space.
pub fn apply_global_mix(graded: &mut [f32], original: &[f32], mix: f32) {
    if (mix - 1.0).abs() < 0.001 || graded.len() != original.len() {
        return;
    }
    for (g, o) in graded.chunks_exact_mut(4).zip(original.chunks_exact(4)) {
        g[0] = o[0] + (g[0] - o[0]) * mix;
        g[1] = o[1] + (g[1] - o[1]) * mix;
        g[2] = o[2] + (g[2] - o[2]) * mix;
    }
}

/// Input device transform — convert camera log encoding to linear.
///
/// Unlike the u8 version, this stays in linear after decode — the
/// old u8 path re-encoded to sRGB before handing off, defeating
/// every downstream grade.
pub fn apply_input_transform(rgba: &mut [f32], log_format: klipp_core::LogFormat) {
    use klipp_core::LogFormat;
    if matches!(log_format, LogFormat::Srgb | LogFormat::Linear) {
        return;
    }
    let needs_matrix = matches!(log_format, LogFormat::AcesCG | LogFormat::Aces2065);

    for pixel in rgba.chunks_exact_mut(4) {
        // Input can be either sRGB (u8 path round-tripped through
        // linear) or a log encoding the decoder didn't decode. The
        // function operates on the value as log-encoded, maps to
        // linear, and applies any colour-primary matrix.
        let (mut lr, mut lg, mut lb) = (
            crate::transforms::log_to_linear(pixel[0], log_format),
            crate::transforms::log_to_linear(pixel[1], log_format),
            crate::transforms::log_to_linear(pixel[2], log_format),
        );

        if needs_matrix {
            let (nr, ng, nb) = match log_format {
                LogFormat::AcesCG => (
                     1.705051 * lr - 0.621792 * lg - 0.083259 * lb,
                    -0.130256 * lr + 1.140805 * lg - 0.010549 * lb,
                    -0.024003 * lr - 0.128969 * lg + 1.152972 * lb,
                ),
                LogFormat::Aces2065 => (
                     2.521401 * lr - 1.134038 * lg - 0.387363 * lb,
                    -0.276214 * lr + 1.372595 * lg - 0.096381 * lb,
                    -0.015315 * lr - 0.152836 * lg + 1.168151 * lb,
                ),
                _ => (lr, lg, lb),
            };
            lr = nr;
            lg = ng;
            lb = nb;
        }

        pixel[0] = lr;
        pixel[1] = lg;
        pixel[2] = lb;
    }
}

/// Saturation: linear lerp between the pixel colour and its luma.
///
/// `factor == 1.0` is identity; `0.0` collapses to greyscale at the
/// working-space-correct luma; `> 1.0` oversaturates. The luma
/// coefficients come from [`luma_coefs`] for the active working
/// colorspace — BT.709 for `Rec709Linear`, AP1 for `AcesCG`. Using
/// the wrong coefficients tilts the desaturation axis off the working
/// space's white point, producing a colour cast at low saturation.
///
/// No clamp — oversaturation that pushes channels above 1.0 or
/// below 0.0 is legal linear light (especially for negative values,
/// it corresponds to "imaginary" primaries outside the working-space
/// gamut and must be preserved for downstream gamut mapping).
pub fn apply_saturation_ws(rgba: &mut [f32], factor: f32, ws: WorkingColorspace) {
    if factor == 1.0 {
        return;
    }
    let c = luma_coefs(ws);
    for pixel in rgba.chunks_exact_mut(4) {
        let luma = c[0] * pixel[0] + c[1] * pixel[1] + c[2] * pixel[2];
        pixel[0] = luma + (pixel[0] - luma) * factor;
        pixel[1] = luma + (pixel[1] - luma) * factor;
        pixel[2] = luma + (pixel[2] - luma) * factor;
    }
}

/// Back-compat wrapper for [`apply_saturation_ws`] pinned to
/// [`WorkingColorspace::Rec709Linear`]. Matches the long-standing
/// pre-Phase-E behaviour byte-for-byte. Existing callers don't need
/// to change; new callers that know the project's working space
/// should use [`apply_saturation_ws`] directly.
#[inline]
pub fn apply_saturation(rgba: &mut [f32], factor: f32) {
    apply_saturation_ws(rgba, factor, WorkingColorspace::Rec709Linear)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_frame(pixels: &[[f32; 4]]) -> Vec<f32> {
        let mut out = Vec::with_capacity(pixels.len() * 4);
        for p in pixels {
            out.extend_from_slice(p);
        }
        out
    }

    fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    // ─── exposure ──────────────────────────────────────────────────

    #[test]
    fn exposure_zero_is_bit_identical() {
        let mut frame = make_frame(&[[0.1, 0.2, 0.3, 1.0], [0.5, 0.6, 0.7, 0.8]]);
        let original = frame.clone();
        apply_exposure(&mut frame, 0.0);
        assert_eq!(frame, original);
    }

    #[test]
    fn exposure_plus_one_stop_on_mid_grey_lands_on_point_36() {
        let mut frame = make_frame(&[[0.18, 0.18, 0.18, 1.0]]);
        apply_exposure(&mut frame, 1.0);
        for i in 0..3 {
            assert!(
                approx_eq(frame[i], 0.36, 1e-4),
                "channel {i}: got {} expected 0.36",
                frame[i],
            );
        }
        assert_eq!(frame[3], 1.0, "alpha must not move under exposure");
    }

    #[test]
    fn exposure_minus_one_stop_on_mid_grey_halves() {
        let mut frame = make_frame(&[[0.18, 0.18, 0.18, 1.0]]);
        apply_exposure(&mut frame, -1.0);
        for i in 0..3 {
            assert!(approx_eq(frame[i], 0.09, 1e-4));
        }
    }

    #[test]
    fn exposure_preserves_highlights_above_one() {
        // +2 stops on 0.6 linear gives 2.4 — not clamped.
        let mut frame = make_frame(&[[0.6, 0.6, 0.6, 1.0]]);
        apply_exposure(&mut frame, 2.0);
        for i in 0..3 {
            assert!(
                approx_eq(frame[i], 2.4, 1e-4),
                "channel {i}: got {} (expected 2.4, not clipped)",
                frame[i],
            );
        }
    }

    // ─── contrast ──────────────────────────────────────────────────

    #[test]
    fn contrast_zero_is_identity() {
        let mut frame = make_frame(&[[0.1, 0.5, 0.9, 1.0]]);
        let original = frame.clone();
        apply_contrast(&mut frame, 0.0);
        assert_eq!(frame, original);
    }

    #[test]
    fn contrast_does_not_shift_mid_grey() {
        let mut frame = make_frame(&[[0.18, 0.18, 0.18, 1.0]]);
        apply_contrast(&mut frame, 0.5);
        for i in 0..3 {
            assert!(
                approx_eq(frame[i], 0.18, 1e-6),
                "channel {i}: mid-grey shifted to {}",
                frame[i],
            );
        }
    }

    #[test]
    fn contrast_expansion_pushes_highlights_past_one() {
        // 0.8 linear with +1.0 contrast factor (scale 2.0) around 0.18
        // pivot: (0.8 - 0.18) * 2.0 + 0.18 = 1.42. Not clipped.
        let mut frame = make_frame(&[[0.8, 0.8, 0.8, 1.0]]);
        apply_contrast(&mut frame, 1.0);
        for i in 0..3 {
            assert!(frame[i] > 1.0, "channel {i}: contrast did not exceed 1.0");
            assert!(approx_eq(frame[i], 1.42, 1e-4));
        }
    }

    // ─── saturation ────────────────────────────────────────────────

    #[test]
    fn saturation_one_is_identity() {
        let mut frame = make_frame(&[[0.8, 0.2, 0.5, 1.0]]);
        let original = frame.clone();
        apply_saturation(&mut frame, 1.0);
        assert_eq!(frame, original);
    }

    #[test]
    fn saturation_zero_pure_red_goes_to_bt709_luma() {
        // Pure linear red (1, 0, 0) → luma = 0.2126.
        let mut frame = make_frame(&[[1.0, 0.0, 0.0, 1.0]]);
        apply_saturation(&mut frame, 0.0);
        for i in 0..3 {
            assert!(
                approx_eq(frame[i], 0.2126, 1e-4),
                "channel {i}: got {}",
                frame[i],
            );
        }
    }

    #[test]
    fn saturation_zero_on_grey_is_invariant() {
        // Grey = R = G = B; its luma equals the original, so any
        // saturation factor leaves it unchanged.
        let mut frame = make_frame(&[[0.5, 0.5, 0.5, 1.0]]);
        let original = frame.clone();
        apply_saturation(&mut frame, 0.0);
        assert_eq!(frame, original);
        apply_saturation(&mut frame, 2.0);
        assert_eq!(frame, original);
    }

    #[test]
    fn saturation_does_not_clamp_overshoot() {
        // Over-saturating a colour that's already far from grey can
        // push one channel > 1.0 or < 0.0. We keep the out-of-range
        // values for later gamut mapping rather than silently clamp.
        let mut frame = make_frame(&[[0.9, 0.1, 0.1, 1.0]]);
        apply_saturation(&mut frame, 3.0);
        assert!(
            frame[1] < 0.0 || frame[2] < 0.0,
            "oversaturation should push the low channels below 0.0: {frame:?}",
        );
    }

    // ─── vibrance ──────────────────────────────────────────────────

    #[test]
    fn vibrance_zero_is_identity() {
        let mut frame = make_frame(&[[0.3, 0.6, 0.2, 1.0]]);
        let original = frame.clone();
        apply_vibrance(&mut frame, 0.0);
        assert_eq!(frame, original);
    }

    #[test]
    fn vibrance_uses_a_higher_boost_on_lower_saturation() {
        // A muted colour (max − min ≈ 0.1) should get a larger
        // *saturation-boost factor* than an already-saturated one
        // (max − min ≈ 0.8). The *absolute* channel delta can go
        // either way depending on the |pixel − luma| distance, so
        // we test the factor directly: measure chroma before/after
        // and confirm the muted one gains more chroma per unit of
        // `amount`.
        let muted_before = [0.50_f32, 0.45, 0.40];
        let saturated_before = [0.90_f32, 0.20, 0.10];
        let chroma = |p: [f32; 3]| p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]);

        let mut muted = make_frame(&[[muted_before[0], muted_before[1], muted_before[2], 1.0]]);
        let mut sat = make_frame(&[[saturated_before[0], saturated_before[1], saturated_before[2], 1.0]]);
        apply_vibrance(&mut muted, 0.5);
        apply_vibrance(&mut sat, 0.5);

        let muted_gain = chroma([muted[0], muted[1], muted[2]]) - chroma(muted_before);
        let sat_gain = chroma([sat[0], sat[1], sat[2]]) - chroma(saturated_before);
        let muted_rel = muted_gain / chroma(muted_before);
        let sat_rel = sat_gain / chroma(saturated_before);

        assert!(
            muted_rel > sat_rel,
            "muted pixel should gain *relatively* more saturation: \
             muted {muted_rel:.3}× vs saturated {sat_rel:.3}×",
        );
    }

    // ─── clarity ───────────────────────────────────────────────────

    #[test]
    fn clarity_zero_is_identity() {
        let mut frame = make_frame(&[[0.3, 0.6, 0.2, 1.0]; 16]);
        let original = frame.clone();
        apply_clarity(&mut frame, 4, 4, 0.0);
        assert_eq!(frame, original);
    }

    #[test]
    fn clarity_on_flat_frame_is_identity() {
        // A flat frame has no detail, so detail = 0 everywhere,
        // so clarity of any amount must be no-op.
        let mut frame = make_frame(&[[0.5, 0.5, 0.5, 1.0]; 64]);
        let original = frame.clone();
        apply_clarity(&mut frame, 8, 8, 0.5);
        for (a, b) in original.iter().zip(frame.iter()) {
            assert!((a - b).abs() < 1e-3, "flat frame changed: {a} vs {b}");
        }
    }

    // ─── temperature / tint ───────────────────────────────────────

    #[test]
    fn temperature_zero_is_identity() {
        let mut frame = make_frame(&[[0.3, 0.6, 0.9, 1.0]]);
        let original = frame.clone();
        apply_temperature_tint(&mut frame, 0.0, 0.0);
        assert_eq!(frame, original);
    }

    #[test]
    fn temperature_positive_shifts_red_up_blue_down() {
        let mut frame = make_frame(&[[0.5, 0.5, 0.5, 1.0]]);
        apply_temperature_tint(&mut frame, 50.0, 0.0);
        assert!(frame[0] > 0.5, "red should warm up");
        assert!(frame[2] < 0.5, "blue should cool down");
        assert_eq!(frame[1], 0.5, "green unchanged by temperature");
    }

    // ─── tone controls ────────────────────────────────────────────

    #[test]
    fn tone_controls_all_zero_is_identity() {
        let mut frame = make_frame(&[[0.1, 0.5, 0.9, 1.0]]);
        let original = frame.clone();
        apply_tone_controls(&mut frame, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(frame, original);
    }

    #[test]
    fn tone_controls_shadows_lift_dark_pixels_more() {
        // A dark pixel (0.05) and a bright pixel (0.8). Shadows +0.1
        // should lift the dark one substantially and barely touch
        // the bright one.
        let mut dark = make_frame(&[[0.05, 0.05, 0.05, 1.0]]);
        let mut bright = make_frame(&[[0.80, 0.80, 0.80, 1.0]]);
        apply_tone_controls(&mut dark, 0.0, 0.1, 0.0, 0.0);
        apply_tone_controls(&mut bright, 0.0, 0.1, 0.0, 0.0);
        let dark_delta = dark[0] - 0.05;
        let bright_delta = bright[0] - 0.80;
        assert!(
            dark_delta.abs() > bright_delta.abs() * 2.0,
            "shadow lift should target shadows: dark Δ={dark_delta} bright Δ={bright_delta}",
        );
    }

    // ─── lift / gamma / gain ───────────────────────────────────────

    #[test]
    fn lift_gamma_gain_all_zero_is_identity() {
        let mut frame = make_frame(&[[0.1, 0.5, 0.9, 1.0]]);
        let original = frame.clone();
        apply_lift_gamma_gain(&mut frame, (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0));
        assert_eq!(frame, original);
    }

    #[test]
    fn lift_targets_shadows() {
        // Positive red-lift should raise the red channel of a dark
        // pixel noticeably more than a bright one.
        let mut dark = make_frame(&[[0.05, 0.05, 0.05, 1.0]]);
        let mut bright = make_frame(&[[0.95, 0.95, 0.95, 1.0]]);
        apply_lift_gamma_gain(&mut dark, (0.2, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0));
        apply_lift_gamma_gain(&mut bright, (0.2, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0));
        assert!(dark[0] - 0.05 > 0.15, "lift should raise shadows: {}", dark[0]);
        assert!(bright[0] - 0.95 < 0.05, "lift should not raise highlights much: {}", bright[0]);
    }

    #[test]
    fn lift_gamma_gain_offset_uniform_offset_moves_all_tones() {
        // Offset of +0.1 should raise every channel of every pixel
        // by exactly 0.1, irrespective of its tonal band.
        let mut frame = make_frame(&[[0.1, 0.5, 0.9, 1.0]]);
        apply_lift_gamma_gain_offset(
            &mut frame,
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
            (0.0, 0.0, 0.0),
            (0.1, 0.1, 0.1),
        );
        assert!((frame[0] - 0.2).abs() < 1e-4);
        assert!((frame[1] - 0.6).abs() < 1e-4);
        assert!((frame[2] - 1.0).abs() < 1e-4);
    }

    // ─── log wheels ────────────────────────────────────────────────

    #[test]
    fn log_wheels_all_zero_is_identity() {
        let mut frame = make_frame(&[[0.1, 0.5, 0.9, 1.0]]);
        let original = frame.clone();
        apply_log_wheels(&mut frame, (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0));
        assert_eq!(frame, original);
    }

    #[test]
    fn log_wheels_bands_peak_at_expected_log_luma() {
        // The Gaussian bands live in log-luma space with centres at
        // 0.15 / 0.5 / 0.85. Linearly those correspond to roughly
        // 0.003 / 0.031 / 0.354 linear. Test that each wheel change
        // peaks at its own band — dark pixel for low, very-dark
        // midtone for mid, near-diffuse-white for high. This
        // preserves the sRGB u8 behaviour (which was calibrated for
        // log-encoded footage) bit-for-bit in linear domain.
        let mut low_target = make_frame(&[[0.003, 0.003, 0.003, 1.0]]);
        let mut mid_target = make_frame(&[[0.031, 0.031, 0.031, 1.0]]);
        let mut high_target = make_frame(&[[0.354, 0.354, 0.354, 1.0]]);
        apply_log_wheels(
            &mut low_target,
            (0.2, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
        );
        apply_log_wheels(
            &mut mid_target,
            (0.0, 0.0, 0.0), (0.2, 0.0, 0.0), (0.0, 0.0, 0.0),
        );
        apply_log_wheels(
            &mut high_target,
            (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.2, 0.0, 0.0),
        );
        assert!(low_target[0] > 0.003 + 0.15, "low band missed: {}", low_target[0]);
        assert!(mid_target[0] > 0.031 + 0.15, "mid band missed: {}", mid_target[0]);
        assert!(high_target[0] > 0.354 + 0.15, "high band missed: {}", high_target[0]);
    }

    // ─── wheel sat/bri ────────────────────────────────────────────

    #[test]
    fn wheel_sat_bri_all_zero_is_identity() {
        let mut frame = make_frame(&[[0.3, 0.6, 0.9, 1.0]]);
        let original = frame.clone();
        apply_wheel_sat_bri(&mut frame, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(frame, original);
    }

    #[test]
    fn wheel_offset_bri_shifts_all_pixels() {
        let mut frame = make_frame(&[[0.2, 0.5, 0.8, 1.0]]);
        apply_wheel_sat_bri(&mut frame, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.1);
        // Uniform offset_bri of +0.1 raises every channel by 0.1
        // before the saturation pass (which is 0 here, identity).
        assert!((frame[0] - 0.3).abs() < 1e-4);
        assert!((frame[1] - 0.6).abs() < 1e-4);
        assert!((frame[2] - 0.9).abs() < 1e-4);
    }

    // ─── hue rotation ──────────────────────────────────────────────

    #[test]
    fn hue_rotation_zero_is_identity() {
        let mut frame = make_frame(&[[0.8, 0.2, 0.3, 1.0]]);
        let original = frame.clone();
        apply_hue_rotation(&mut frame, 0.0);
        assert_eq!(frame, original);
    }

    #[test]
    fn hue_rotation_360_returns_to_original() {
        let mut frame = make_frame(&[[0.8, 0.2, 0.3, 1.0]]);
        let original = frame.clone();
        apply_hue_rotation(&mut frame, 360.0);
        for (a, b) in original.iter().zip(frame.iter()) {
            assert!((a - b).abs() < 1e-4, "360° rotation not identity: {a} vs {b}");
        }
    }

    #[test]
    fn hue_rotation_on_grey_is_no_op() {
        let mut frame = make_frame(&[[0.5, 0.5, 0.5, 1.0]]);
        let original = frame.clone();
        apply_hue_rotation(&mut frame, 120.0);
        assert_eq!(frame, original, "grey has no hue to rotate");
    }

    // ─── global mix ────────────────────────────────────────────────

    #[test]
    fn global_mix_one_keeps_graded() {
        let mut graded = make_frame(&[[0.5, 0.5, 0.5, 1.0]]);
        let original = make_frame(&[[0.0, 0.0, 0.0, 1.0]]);
        let before = graded.clone();
        apply_global_mix(&mut graded, &original, 1.0);
        assert_eq!(graded, before);
    }

    #[test]
    fn global_mix_zero_restores_original() {
        let mut graded = make_frame(&[[0.5, 0.5, 0.5, 1.0]]);
        let original = make_frame(&[[0.1, 0.2, 0.3, 1.0]]);
        apply_global_mix(&mut graded, &original, 0.0);
        for i in 0..3 {
            assert!((graded[i] - original[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn global_mix_half_is_midpoint() {
        let mut graded = make_frame(&[[0.8, 0.8, 0.8, 1.0]]);
        let original = make_frame(&[[0.2, 0.2, 0.2, 1.0]]);
        apply_global_mix(&mut graded, &original, 0.5);
        for i in 0..3 {
            assert!((graded[i] - 0.5).abs() < 1e-4);
        }
    }

    // ─── input transform ──────────────────────────────────────────

    #[test]
    fn input_transform_srgb_is_identity() {
        let mut frame = make_frame(&[[0.3, 0.6, 0.9, 1.0]]);
        let original = frame.clone();
        apply_input_transform(&mut frame, klipp_core::LogFormat::Srgb);
        assert_eq!(frame, original);
    }

    #[test]
    fn input_transform_linear_is_identity() {
        let mut frame = make_frame(&[[0.3, 0.6, 0.9, 1.0]]);
        let original = frame.clone();
        apply_input_transform(&mut frame, klipp_core::LogFormat::Linear);
        assert_eq!(frame, original);
    }

    #[test]
    fn input_transform_slog3_mid_grey_lands_near_point_18() {
        // S-Log3's middle-grey is around 0.41 encoded; decodes to
        // ~0.18 linear. The u8 version then re-encoded to sRGB
        // (≈118/255), defeating every downstream grade. We now
        // stay in linear.
        let mut frame = make_frame(&[[0.41, 0.41, 0.41, 1.0]]);
        apply_input_transform(&mut frame, klipp_core::LogFormat::SLog3);
        for i in 0..3 {
            assert!(
                (frame[i] - 0.18).abs() < 0.02,
                "channel {i}: expected ~0.18 linear, got {}",
                frame[i],
            );
        }
    }

    // ─── Working-colorspace luma plumbing (Phase E S5) ──────────

    #[test]
    fn luma_coefs_rec709_match_published_values() {
        let c = luma_coefs(WorkingColorspace::Rec709Linear);
        assert!(approx_eq(c[0], 0.2126, 1e-6));
        assert!(approx_eq(c[1], 0.7152, 1e-6));
        assert!(approx_eq(c[2], 0.0722, 1e-6));
    }

    #[test]
    fn luma_coefs_acescg_match_ap1_rgb2y() {
        // Sourced from AMPAS aces-dev `AP1_RGB2Y`. Same values used
        // by `klipp_color::aces`'s saturation matrix.
        let c = luma_coefs(WorkingColorspace::AcesCG);
        assert!(approx_eq(c[0], 0.272_228_72, 1e-6));
        assert!(approx_eq(c[1], 0.674_081_77, 1e-6));
        assert!(approx_eq(c[2], 0.053_689_52, 1e-6));
    }

    #[test]
    fn luma_coefs_sum_to_one() {
        // Property of any RGB-to-Y row: equal-RGB white → equal-Y
        // (full white). Per channel the coefficients sum to 1.0.
        for ws in [
            WorkingColorspace::Rec709Linear,
            WorkingColorspace::AcesCG,
            WorkingColorspace::Bt2020Linear,
        ] {
            let c = luma_coefs(ws);
            let s: f32 = c.iter().sum();
            assert!(approx_eq(s, 1.0, 5e-4),
                "{ws:?} luma coefs sum to {s}, expected 1.0");
        }
    }

    #[test]
    fn saturation_ws_rec709_default_is_byte_identical_to_legacy() {
        // The back-compat wrapper must produce the same output as
        // calling the `_ws` variant with `Rec709Linear`. If this
        // drifts, every existing caller of `apply_saturation` is
        // silently changing behaviour.
        let pixels: Vec<[f32; 4]> = vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.5, 0.3, 0.7, 1.0],
            [-0.05, 0.4, 1.5, 0.5],
        ];
        for factor in [0.0, 0.5, 1.5, 2.0] {
            let mut a = make_frame(&pixels);
            let mut b = make_frame(&pixels);
            apply_saturation(&mut a, factor);
            apply_saturation_ws(&mut b, factor, WorkingColorspace::Rec709Linear);
            assert_eq!(a, b, "factor={factor}");
        }
    }

    #[test]
    fn vibrance_ws_rec709_default_is_byte_identical_to_legacy() {
        let pixels: Vec<[f32; 4]> = vec![
            [1.0, 0.5, 0.5, 1.0],
            [0.5, 0.3, 0.7, 1.0],
            [0.18, 0.18, 0.18, 1.0],
        ];
        for amount in [-0.5, 0.0, 0.3, 0.8] {
            let mut a = make_frame(&pixels);
            let mut b = make_frame(&pixels);
            apply_vibrance(&mut a, amount);
            apply_vibrance_ws(&mut b, amount, WorkingColorspace::Rec709Linear);
            assert_eq!(a, b, "amount={amount}");
        }
    }

    #[test]
    fn saturation_ws_zero_neutral_grey_invariant_for_all_working_spaces() {
        // Grey (R=G=B) has equal-channel luma; any saturation factor
        // leaves it untouched regardless of which working-space's
        // luma vector is used.
        for ws in [
            WorkingColorspace::Rec709Linear,
            WorkingColorspace::AcesCG,
            WorkingColorspace::Bt2020Linear,
        ] {
            let mut frame = make_frame(&[[0.5, 0.5, 0.5, 1.0]]);
            let original = frame.clone();
            apply_saturation_ws(&mut frame, 0.0, ws);
            for i in 0..4 {
                assert!(approx_eq(frame[i], original[i], 1e-6),
                    "{ws:?} grey channel {i}: drift {} → {}", original[i], frame[i]);
            }
        }
    }

    #[test]
    fn saturation_zero_collapses_to_working_space_luma() {
        // Pure red under different working spaces → channels equal
        // the working-space's R coefficient (Yr).
        for ws in [
            WorkingColorspace::Rec709Linear,
            WorkingColorspace::AcesCG,
            WorkingColorspace::Bt2020Linear,
        ] {
            let mut frame = make_frame(&[[1.0, 0.0, 0.0, 1.0]]);
            apply_saturation_ws(&mut frame, 0.0, ws);
            let yr = luma_coefs(ws)[0];
            for i in 0..3 {
                assert!(approx_eq(frame[i], yr, 1e-5),
                    "{ws:?} channel {i}: expected Yr={yr}, got {}", frame[i]);
            }
        }
    }

    /// The plan §S5 invariant for gamut-aware ops: saturation under
    /// AcesCG must produce a different (correct-for-AP1) result than
    /// saturation under Rec.709. If `apply_saturation_ws(.., AcesCG)`
    /// produced the same numbers as `apply_saturation_ws(.., Rec709Linear)`,
    /// the working-space switch isn't actually doing anything.
    #[test]
    fn saturation_ws_rec709_and_acescg_diverge_on_non_grey() {
        let mut a = make_frame(&[[1.0, 0.0, 0.0, 1.0]]);
        let mut b = make_frame(&[[1.0, 0.0, 0.0, 1.0]]);
        apply_saturation_ws(&mut a, 0.0, WorkingColorspace::Rec709Linear);
        apply_saturation_ws(&mut b, 0.0, WorkingColorspace::AcesCG);
        // The two should differ: AcesCG's Yr (0.272) > Rec.709's Yr
        // (0.213), so the achromatic projection of pure red is
        // brighter under AcesCG.
        assert!(b[0] > a[0],
            "AcesCG saturation should make pure red's grey-projection brighter than Rec.709's; got Rec.709={}, AcesCG={}", a[0], b[0]);
    }
}
