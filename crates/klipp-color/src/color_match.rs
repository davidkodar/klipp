/// Color match between shots using CIE Lab colour space statistics.
///
/// The algorithm:
/// 1. Convert both source and reference frames to Lab
/// 2. Compute mean and standard deviation of L, a, b channels
/// 3. Map Lab statistics to grade adjustments (exposure, temperature, tint, saturation)
/// 4. Return adjustments as regular grade parameters (undoable)

/// Result of a color match analysis — grade adjustments to apply.
#[derive(Debug, Clone)]
pub struct ColorMatchResult {
    pub exposure_adjust: f32,
    pub temperature_adjust: f32,
    pub tint_adjust: f32,
    pub saturation_adjust: f32,
}

#[derive(Debug)]
struct LabStats {
    l_mean: f32,
    a_mean: f32,
    b_mean: f32,
    chroma_mean: f32,
    /// Mean colour, linear BT.709.
    linear_mean: [f32; 3],
}

/// Compute color match adjustments from source and reference RGBA buffers.
/// Both buffers are standard u8 RGBA. Samples every 4th pixel for performance.
pub fn match_colors(
    source_rgba: &[u8],
    reference_rgba: &[u8],
) -> ColorMatchResult {
    let src = rgba_to_lab_stats(source_rgba);
    let ref_ = rgba_to_lab_stats(reference_rgba);

    // L channel difference → exposure adjustment
    // Lab L is 0-100, our exposure is in stops. Scale factor:
    // A 1-stop change ≈ 15 L units (rough approximation)
    let exposure_adjust = (ref_.l_mean - src.l_mean) / 15.0;

    // Lab a / b → tint / temperature: the white balance that moves the
    // source's average colour (at the matched exposure) by the difference
    // between the two frames' average a and b.
    let exposed = src.linear_mean.map(|c| c * 2f32.powf(exposure_adjust.clamp(-3.0, 3.0)));
    let (temperature_adjust, tint_adjust) =
        white_balance_for_shift(exposed, ref_.a_mean - src.a_mean, ref_.b_mean - src.b_mean);

    // Chroma ratio → saturation multiplier
    let saturation_adjust = if src.chroma_mean > 0.5 {
        ref_.chroma_mean / src.chroma_mean
    } else {
        1.0 // source is nearly achromatic, don't boost
    };

    ColorMatchResult {
        exposure_adjust: exposure_adjust.clamp(-3.0, 3.0),
        temperature_adjust: temperature_adjust.clamp(-100.0, 100.0),
        tint_adjust: tint_adjust.clamp(-100.0, 100.0),
        saturation_adjust: saturation_adjust.clamp(0.1, 3.0),
    }
}

/// The temperature and tint whose white balance moves `rgb` (linear
/// BT.709) by `(da, db)` in Lab a / b: Newton's method on the two slider
/// values, clamped to their ±100 range.
fn white_balance_for_shift(rgb: [f32; 3], da: f32, db: f32) -> (f32, f32) {
    use crate::white_balance::white_balance_matrix;
    let ab = |t: f32, g: f32| {
        let c = match white_balance_matrix(t, g, klipp_core::WorkingColorspace::Rec709Linear) {
            Some(m) => m.map(|row| row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]),
            None => rgb,
        };
        let (_, a, b) = linear_to_lab(c[0], c[1], c[2]);
        (a, b)
    };
    let (a0, b0) = ab(0.0, 0.0);
    let (want_a, want_b) = (a0 + da, b0 + db);
    let (mut t, mut g) = (0.0f32, 0.0f32);
    for _ in 0..8 {
        let (a, b) = ab(t, g);
        let (ea, eb) = (want_a - a, want_b - b);
        if ea.abs() < 0.01 && eb.abs() < 0.01 {
            break;
        }
        let h = 1.0;
        let (at, bt) = ab(t + h, g);
        let (ag, bg) = ab(t, g + h);
        let (dadt, dbdt, dadg, dbdg) = ((at - a) / h, (bt - b) / h, (ag - a) / h, (bg - b) / h);
        let det = dadt * dbdg - dadg * dbdt;
        if det.abs() < 1e-6 {
            break;
        }
        t = (t + (ea * dbdg - eb * dadg) / det).clamp(-100.0, 100.0);
        g = (g + (eb * dadt - ea * dbdt) / det).clamp(-100.0, 100.0);
    }
    (t, g)
}

fn rgba_to_lab_stats(rgba: &[u8]) -> LabStats {
    let mut l_sum = 0.0f64;
    let mut a_sum = 0.0f64;
    let mut b_sum = 0.0f64;
    let mut chroma_sum = 0.0f64;
    let mut linear_sum = [0.0f64; 3];
    let mut count = 0u64;

    // Sample every 4th pixel for performance
    for chunk in rgba.chunks_exact(4).step_by(4) {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;

        let (l, la, lb) = rgb_to_lab(r, g, b);
        for (acc, v) in linear_sum.iter_mut().zip([r, g, b]) {
            *acc += srgb_decode(v) as f64;
        }
        l_sum += l as f64;
        a_sum += la as f64;
        b_sum += lb as f64;
        chroma_sum += (la * la + lb * lb).sqrt() as f64;
        count += 1;
    }

    if count == 0 {
        return LabStats { l_mean: 50.0, a_mean: 0.0, b_mean: 0.0, chroma_mean: 0.0, linear_mean: [0.18; 3] };
    }

    let n = count as f64;
    LabStats {
        l_mean: (l_sum / n) as f32,
        a_mean: (a_sum / n) as f32,
        b_mean: (b_sum / n) as f32,
        chroma_mean: (chroma_sum / n) as f32,
        linear_mean: linear_sum.map(|v| (v / n) as f32),
    }
}

fn srgb_decode(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// sRGB → CIE XYZ (D65) → CIE Lab
fn rgb_to_lab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    linear_to_lab(srgb_decode(r), srgb_decode(g), srgb_decode(b))
}

/// Linear BT.709 → CIE XYZ (D65) → CIE Lab
fn linear_to_lab(rl: f32, gl: f32, bl: f32) -> (f32, f32, f32) {
    // Linear RGB → XYZ (sRGB/D65 matrix)
    let x = 0.4124564 * rl + 0.3575761 * gl + 0.1804375 * bl;
    let y = 0.2126729 * rl + 0.7151522 * gl + 0.0721750 * bl;
    let z = 0.0193339 * rl + 0.1191920 * gl + 0.9503041 * bl;

    // D65 white point
    let xn = 0.95047;
    let yn = 1.00000;
    let zn = 1.08883;

    let f = |t: f32| -> f32 {
        if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 }
    };

    let fx = f(x / xn);
    let fy = f(y / yn);
    let fz = f(z / zn);

    let l = 116.0 * fy - 16.0;
    let a = 500.0 * (fx - fy);
    let b_val = 200.0 * (fy - fz);

    (l, a, b_val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_frames_produce_zero_adjustment() {
        let frame: Vec<u8> = (0..100).flat_map(|i| {
            let v = (i * 2) as u8;
            [v, v, v, 255]
        }).collect();
        let result = match_colors(&frame, &frame);
        assert!(result.exposure_adjust.abs() < 0.01, "exposure should be ~0: {}", result.exposure_adjust);
        assert!(result.temperature_adjust.abs() < 0.5, "temp should be ~0: {}", result.temperature_adjust);
        assert!(result.tint_adjust.abs() < 0.5, "tint should be ~0: {}", result.tint_adjust);
        assert!((result.saturation_adjust - 1.0).abs() < 0.1, "sat should be ~1: {}", result.saturation_adjust);
    }

    #[test]
    fn brighter_reference_increases_exposure() {
        let dark: Vec<u8> = (0..100).flat_map(|_| [60, 60, 60, 255]).collect();
        let bright: Vec<u8> = (0..100).flat_map(|_| [200, 200, 200, 255]).collect();
        let result = match_colors(&dark, &bright);
        assert!(result.exposure_adjust > 0.5, "should increase exposure: {}", result.exposure_adjust);
    }

    #[test]
    fn warm_reference_increases_temperature() {
        let neutral: Vec<u8> = (0..100).flat_map(|_| [128, 128, 128, 255]).collect();
        let warm: Vec<u8> = (0..100).flat_map(|_| [180, 128, 80, 255]).collect();
        let result = match_colors(&neutral, &warm);
        assert!(result.temperature_adjust > 5.0, "should increase temperature: {}", result.temperature_adjust);
    }

    #[test]
    fn lab_conversion_black_white() {
        let (l, _a, _b) = rgb_to_lab(0.0, 0.0, 0.0);
        assert!(l.abs() < 1.0, "black L should be ~0: {l}");
        let (l, _a, _b) = rgb_to_lab(1.0, 1.0, 1.0);
        assert!((l - 100.0).abs() < 1.0, "white L should be ~100: {l}");
    }

    #[test]
    fn lab_grey_is_achromatic() {
        let (_l, a, b) = rgb_to_lab(0.5, 0.5, 0.5);
        assert!(a.abs() < 1.0, "grey a should be ~0: {a}");
        assert!(b.abs() < 1.0, "grey b should be ~0: {b}");
    }

    #[test]
    fn color_match_warm_to_cool() {
        // Warm frame (elevated R) vs cool frame (elevated B)
        let warm: Vec<u8> = (0..100).flat_map(|_| [200, 128, 80, 255]).collect();
        let cool: Vec<u8> = (0..100).flat_map(|_| [80, 128, 200, 255]).collect();
        let result = match_colors(&warm, &cool);
        // Matching warm→cool should produce negative temperature (shift toward cool)
        assert!(result.temperature_adjust < -5.0,
            "warm→cool should decrease temperature: {}", result.temperature_adjust);
    }

    /// Shot match finds the white balance that was applied: a reference
    /// that is the source through temperature +30, tint -10 matches back
    /// to about those values.
    #[test]
    fn match_recovers_a_white_balance() {
        let m = crate::white_balance::white_balance_matrix(30.0, -10.0, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let grey = srgb_decode(120.0 / 255.0);
        let enc = |v: f32| {
            let e = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
            (e.clamp(0.0, 1.0) * 255.0).round() as u8
        };
        let shifted = m.map(|row| enc((row[0] + row[1] + row[2]) * grey));
        let source: Vec<u8> = (0..400).flat_map(|_| [120, 120, 120, 255]).collect();
        let reference: Vec<u8> = (0..400).flat_map(|_| [shifted[0], shifted[1], shifted[2], 255]).collect();
        let r = match_colors(&source, &reference);
        assert!((r.temperature_adjust - 30.0).abs() < 3.0, "temperature {}", r.temperature_adjust);
        assert!((r.tint_adjust + 10.0).abs() < 3.0, "tint {}", r.tint_adjust);
    }

    #[test]
    fn color_match_dark_to_bright() {
        let dark: Vec<u8> = (0..200).flat_map(|_| [50, 50, 50, 255]).collect();
        let bright: Vec<u8> = (0..200).flat_map(|_| [180, 180, 180, 255]).collect();
        let result = match_colors(&dark, &bright);
        assert!(result.exposure_adjust > 0.3,
            "dark→bright should increase exposure: {}", result.exposure_adjust);
    }

    #[test]
    fn color_match_oversaturated_to_neutral() {
        let saturated: Vec<u8> = (0..100).flat_map(|_| [255, 50, 50, 255]).collect();
        let neutral: Vec<u8> = (0..100).flat_map(|_| [140, 130, 120, 255]).collect();
        let result = match_colors(&saturated, &neutral);
        assert!(result.saturation_adjust < 1.0,
            "saturated→neutral should desaturate: {}", result.saturation_adjust);
    }

    #[test]
    fn color_match_apply_produces_closer_match() {
        // Use frames that differ primarily in brightness — the match algorithm
        // handles this well since exposure is the most reliable adjustment
        let source: Vec<u8> = (0..200).flat_map(|_| [80, 80, 80, 255]).collect();
        let reference: Vec<u8> = (0..200).flat_map(|_| [170, 170, 170, 255]).collect();

        // Compute Lab distance before matching
        let src_stats = rgba_to_lab_stats(&source);
        let ref_stats = rgba_to_lab_stats(&reference);
        let dist_before = ((src_stats.l_mean - ref_stats.l_mean).powi(2)
            + (src_stats.a_mean - ref_stats.a_mean).powi(2)
            + (src_stats.b_mean - ref_stats.b_mean).powi(2)).sqrt();

        // Apply match
        let result = match_colors(&source, &reference);

        // Simulate applying the match to the source
        let factor = 2.0f32.powf(result.exposure_adjust);
        let t = result.temperature_adjust / 100.0 * 0.1;
        let tn = result.tint_adjust / 100.0 * 0.1;
        let matched: Vec<u8> = source.chunks_exact(4).flat_map(|px| {
            let mut r = px[0] as f32 / 255.0 * factor;
            let mut g = px[1] as f32 / 255.0 * factor;
            let mut b = px[2] as f32 / 255.0 * factor;
            // Temperature + tint
            r += t; g += tn; b -= t;
            // Saturation
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            r = luma + (r - luma) * result.saturation_adjust;
            g = luma + (g - luma) * result.saturation_adjust;
            b = luma + (b - luma) * result.saturation_adjust;
            [(r.clamp(0.0, 1.0) * 255.0) as u8, (g.clamp(0.0, 1.0) * 255.0) as u8,
             (b.clamp(0.0, 1.0) * 255.0) as u8, 255]
        }).collect();

        let matched_stats = rgba_to_lab_stats(&matched);
        let dist_after = ((matched_stats.l_mean - ref_stats.l_mean).powi(2)
            + (matched_stats.a_mean - ref_stats.a_mean).powi(2)
            + (matched_stats.b_mean - ref_stats.b_mean).powi(2)).sqrt();

        assert!(dist_after < dist_before,
            "match should reduce Lab distance: before={dist_before:.2}, after={dist_after:.2}");
    }
}
