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
