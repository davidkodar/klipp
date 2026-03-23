use klipp_core::QualifierSettings;

/// Generate a matte (0.0-1.0 per pixel) from an HSL qualifier.
/// Returns a Vec<f32> with one value per pixel.
pub fn generate_qualifier_matte(
    rgba: &[u8],
    width: u32,
    height: u32,
    q: &QualifierSettings,
) -> Vec<f32> {
    let pixel_count = (width * height) as usize;
    let mut matte = vec![0.0f32; pixel_count];

    let hue_center = q.hue_center / 360.0; // normalize to 0-1
    let hue_width = q.hue_width / 360.0;
    let hue_soft = q.hue_soft / 360.0;

    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;

        // RGB → HSL
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let l = (max + min) * 0.5;
        let s = if delta < 0.0001 {
            0.0
        } else {
            (delta / (1.0 - (2.0 * l - 1.0).abs())).clamp(0.0, 1.0)
        };
        let h = if delta < 0.0001 {
            0.0
        } else if max == r {
            ((g - b) / delta).rem_euclid(6.0) / 6.0
        } else if max == g {
            ((b - r) / delta + 2.0) / 6.0
        } else {
            ((r - g) / delta + 4.0) / 6.0
        };

        // Hue mask: circular distance with soft edge
        let hue_dist = circular_distance(h, hue_center);
        let hue_mask = soft_range_centered(hue_dist, hue_width, hue_soft);

        // Saturation mask: range with soft edges
        let sat_mask = soft_range(s, q.sat_low, q.sat_high, q.sat_soft);

        // Luminance mask: range with soft edges
        let lum_mask = soft_range(l, q.lum_low, q.lum_high, q.lum_soft);

        // Combined matte
        let mut m = hue_mask * sat_mask * lum_mask;
        if q.invert {
            m = 1.0 - m;
        }
        matte[i] = m;
    }

    matte
}

/// Apply a secondary grade through a matte.
/// For each pixel: output = original * (1 - matte) + graded * matte
pub fn apply_qualified_grade(
    rgba: &mut [u8],
    matte: &[f32],
    q: &QualifierSettings,
) {
    let exposure_factor = 2.0_f32.powf(q.exposure);
    let hue_shift = q.hue_shift / 360.0;

    for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
        let m = matte.get(i).copied().unwrap_or(0.0);
        if m < 0.001 { continue; }

        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;

        // Apply secondary adjustments
        let mut nr = r * exposure_factor;
        let mut ng = g * exposure_factor;
        let mut nb = b * exposure_factor;

        // Saturation
        if (q.saturation - 1.0).abs() > 0.001 {
            let luma = 0.2126 * nr + 0.7152 * ng + 0.0722 * nb;
            nr = luma + (nr - luma) * q.saturation;
            ng = luma + (ng - luma) * q.saturation;
            nb = luma + (nb - luma) * q.saturation;
        }

        // Temperature
        if q.temperature.abs() > 0.1 {
            let t = q.temperature / 100.0 * 0.1;
            nr += t;
            nb -= t;
        }

        // Hue shift (simple RGB rotation approximation)
        if hue_shift.abs() > 0.001 {
            let angle = hue_shift * std::f32::consts::TAU;
            let cos_a = angle.cos();
            let sin_a = angle.sin();
            let sqrt3 = 3.0_f32.sqrt();
            let tr = nr * (cos_a + (1.0 - cos_a) / 3.0)
                + ng * ((1.0 - cos_a) / 3.0 - sqrt3 / 3.0 * sin_a)
                + nb * ((1.0 - cos_a) / 3.0 + sqrt3 / 3.0 * sin_a);
            let tg = nr * ((1.0 - cos_a) / 3.0 + sqrt3 / 3.0 * sin_a)
                + ng * (cos_a + (1.0 - cos_a) / 3.0)
                + nb * ((1.0 - cos_a) / 3.0 - sqrt3 / 3.0 * sin_a);
            let tb = nr * ((1.0 - cos_a) / 3.0 - sqrt3 / 3.0 * sin_a)
                + ng * ((1.0 - cos_a) / 3.0 + sqrt3 / 3.0 * sin_a)
                + nb * (cos_a + (1.0 - cos_a) / 3.0);
            nr = tr;
            ng = tg;
            nb = tb;
        }

        // Blend with original using matte
        let out_r = r + (nr - r) * m;
        let out_g = g + (ng - g) * m;
        let out_b = b + (nb - b) * m;

        chunk[0] = (out_r.clamp(0.0, 1.0) * 255.0) as u8;
        chunk[1] = (out_g.clamp(0.0, 1.0) * 255.0) as u8;
        chunk[2] = (out_b.clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Circular distance on a 0-1 hue wheel.
fn circular_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).abs();
    d.min(1.0 - d)
}

/// Soft threshold: 1.0 when distance < width, fades to 0 over soft region.
fn soft_range_centered(distance: f32, width: f32, soft: f32) -> f32 {
    if distance <= width {
        1.0
    } else if soft > 0.0001 && distance < width + soft {
        1.0 - (distance - width) / soft
    } else {
        0.0
    }
}

/// Soft range: 1.0 when value is within [low, high], fades over soft edges.
fn soft_range(value: f32, low: f32, high: f32, soft: f32) -> f32 {
    if value >= low && value <= high {
        1.0
    } else if soft > 0.0001 {
        if value < low {
            let d = low - value;
            if d < soft { 1.0 - d / soft } else { 0.0 }
        } else {
            let d = value - high;
            if d < soft { 1.0 - d / soft } else { 0.0 }
        }
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_qual(hue_center: f32, hue_width: f32, hue_soft: f32) -> QualifierSettings {
        QualifierSettings {
            enabled: true,
            hue_center, hue_width, hue_soft,
            sat_low: 0.0, sat_high: 1.0, sat_soft: 0.0,
            lum_low: 0.0, lum_high: 1.0, lum_soft: 0.0,
            invert: false,
            exposure: 0.0, saturation: 1.0, hue_shift: 0.0, temperature: 0.0,
        }
    }

    fn single_pixel_matte(r: u8, g: u8, b: u8, q: &QualifierSettings) -> f32 {
        let rgba = vec![r, g, b, 255];
        let matte = generate_qualifier_matte(&rgba, 1, 1, q);
        matte[0]
    }

    #[test]
    fn qualifier_pure_red_selected() {
        // Pure red (hue=0°) with hue selection centred at 0°, width ±15°
        let q = make_qual(0.0, 30.0, 10.0);
        let m = single_pixel_matte(255, 0, 0, &q);
        // Red is dead centre of the selection → matte should be fully inside
        assert!(m >= 0.95, "pure red should be selected: matte={m}");
    }

    #[test]
    fn qualifier_pure_blue_excluded() {
        // Pure blue (hue=240°) with red selection (hue=0°, width=30°)
        let q = make_qual(0.0, 30.0, 10.0);
        let m = single_pixel_matte(0, 0, 255, &q);
        // Blue is 240° away from red → completely outside
        assert!(m <= 0.05, "pure blue should be excluded: matte={m}");
    }

    #[test]
    fn qualifier_soft_edge_midpoint() {
        // soft_range_centered: dist <= width → 1.0, dist > width+soft → 0.0
        // width=30/360=0.0833, soft=10/360=0.0278
        // Need a pixel at hue distance > width but < width+soft from centre
        // That means hue ~35° from centre → ~35° hue
        // Hue 35° ≈ orange. Pure orange at full sat: (255, 148, 0) ≈ hue=35°
        // Distance = 35/360 = 0.0972, width=0.0833 → in soft zone
        let q = make_qual(0.0, 30.0, 10.0);
        let m = single_pixel_matte(255, 148, 0, &q);
        // Should be in the soft falloff zone (between 0.1 and 0.9)
        assert!(m > 0.1 && m < 0.95,
            "pixel in soft zone should have partial matte: matte={m}");
    }

    #[test]
    fn qualifier_saturation_range() {
        // Grey pixel (128,128,128) has zero saturation.
        // Qualifier requires sat ≥ 0.3 → grey should be excluded.
        let mut q = make_qual(0.0, 180.0, 0.0); // wide hue to not exclude by hue
        q.sat_low = 0.3;
        q.sat_high = 1.0;
        q.sat_soft = 0.0;
        let m = single_pixel_matte(128, 128, 128, &q);
        assert!(m <= 0.05, "grey pixel should be excluded by sat range: matte={m}");
    }

    #[test]
    fn qualifier_luma_range() {
        // Very dark pixel (10,10,10), luma ≈ 0.04
        // Qualifier requires lum ≥ 0.5 → should be excluded
        let mut q = make_qual(0.0, 180.0, 0.0);
        q.lum_low = 0.5;
        q.lum_high = 1.0;
        q.lum_soft = 0.0;
        let m = single_pixel_matte(10, 10, 10, &q);
        assert!(m <= 0.05, "dark pixel should be excluded by lum range: matte={m}");
    }

    #[test]
    fn qualifier_invert_flag() {
        // Same as pure_red_selected but inverted → red should now be excluded
        let mut q = make_qual(0.0, 30.0, 10.0);
        q.invert = true;
        let m = single_pixel_matte(255, 0, 0, &q);
        assert!(m <= 0.05, "inverted: red should be excluded: matte={m}");
    }

    #[test]
    fn qualifier_combined_hue_sat_lum() {
        // Fully saturated red at medium brightness — should pass all three masks
        let mut q = make_qual(0.0, 30.0, 10.0);
        q.sat_low = 0.3;
        q.sat_high = 1.0;
        q.sat_soft = 0.1;
        q.lum_low = 0.1;
        q.lum_high = 0.9;
        q.lum_soft = 0.1;
        // Pure red (255,0,0): hue=0°, sat=1.0, lum=0.5 → all inside
        let m = single_pixel_matte(255, 0, 0, &q);
        assert!(m >= 0.9, "fully qualifying pixel: matte={m}");
    }
}
