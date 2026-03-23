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

    // b channel → temperature (positive b = yellow/warm, negative b = blue/cool)
    // Scale: Lab b range is roughly ±128, temperature range is ±100
    let temperature_adjust = (ref_.b_mean - src.b_mean) * (100.0 / 40.0);

    // a channel → tint (positive a = magenta, negative a = green)
    // Tint is inverted: positive tint = green, so negate
    let tint_adjust = (ref_.a_mean - src.a_mean) * -(100.0 / 40.0);

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

fn rgba_to_lab_stats(rgba: &[u8]) -> LabStats {
    let mut l_sum = 0.0f64;
    let mut a_sum = 0.0f64;
    let mut b_sum = 0.0f64;
    let mut chroma_sum = 0.0f64;
    let mut count = 0u64;

    // Sample every 4th pixel for performance
    for chunk in rgba.chunks_exact(4).step_by(4) {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;

        let (l, la, lb) = rgb_to_lab(r, g, b);
        l_sum += l as f64;
        a_sum += la as f64;
        b_sum += lb as f64;
        chroma_sum += (la * la + lb * lb).sqrt() as f64;
        count += 1;
    }

    if count == 0 {
        return LabStats { l_mean: 50.0, a_mean: 0.0, b_mean: 0.0, chroma_mean: 0.0 };
    }

    let n = count as f64;
    LabStats {
        l_mean: (l_sum / n) as f32,
        a_mean: (a_sum / n) as f32,
        b_mean: (b_sum / n) as f32,
        chroma_mean: (chroma_sum / n) as f32,
    }
}

/// sRGB → CIE XYZ (D65) → CIE Lab
fn rgb_to_lab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    // sRGB → linear
    let linearize = |v: f32| -> f32 {
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let rl = linearize(r);
    let gl = linearize(g);
    let bl = linearize(b);

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
        let (l, a, b) = rgb_to_lab(0.0, 0.0, 0.0);
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
}
