use klipp_core::HueSatCurves;

/// Apply all six Hue/Sat curves to an RGBA frame.
pub fn apply_huesat_curves(rgba: &mut [u8], curves: &HueSatCurves, mix: f32) {
    if curves.is_all_identity() {
        return;
    }

    // Pre-build float LUTs for non-empty curves (identity_y = 0.5 for relative curves)
    let hvh_lut = if !curves.hue_vs_hue.is_empty() {
        Some(crate::curves::build_curve_lut_f32(&curves.hue_vs_hue))
    } else { None };
    let hvs_lut = if !curves.hue_vs_sat.is_empty() {
        Some(crate::curves::build_curve_lut_f32(&curves.hue_vs_sat))
    } else { None };
    let hvl_lut = if !curves.hue_vs_lum.is_empty() {
        Some(crate::curves::build_curve_lut_f32(&curves.hue_vs_lum))
    } else { None };
    let lvs_lut = if !curves.lum_vs_sat.is_empty() {
        Some(crate::curves::build_curve_lut_f32(&curves.lum_vs_sat))
    } else { None };
    let svs_lut = if !curves.sat_vs_sat.is_empty() {
        Some(crate::curves::build_curve_lut_f32(&curves.sat_vs_sat))
    } else { None };
    let ovs_lut = if !curves.orange_vs_sat.is_empty() {
        Some(crate::curves::build_curve_lut_f32(&curves.orange_vs_sat))
    } else { None };

    for chunk in rgba.chunks_exact_mut(4) {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;

        let (h, s, l) = rgb_to_hsl(r, g, b);
        let mut new_h = h;
        let mut new_s = s;
        let mut new_l = l;

        // Hue vs Hue: shift hue based on input hue (0.5 = no change)
        if let Some(ref lut) = hvh_lut {
            let idx = ((h * 255.0) as usize).min(255);
            let delta = (lut[idx] - 0.5) * 2.0; // ±1.0 range
            new_h = (h + delta).rem_euclid(1.0);
        }

        // Hue vs Sat: modulate saturation by input hue (0.5 = no change)
        if let Some(ref lut) = hvs_lut {
            let idx = ((h * 255.0) as usize).min(255);
            new_s = (s * lut[idx] * 2.0).clamp(0.0, 1.0);
        }

        // Hue vs Lum: adjust luminance by input hue (0.5 = no change)
        if let Some(ref lut) = hvl_lut {
            let idx = ((h * 255.0) as usize).min(255);
            let delta = (lut[idx] - 0.5) * 2.0;
            new_l = (l + delta * 0.5).clamp(0.0, 1.0);
        }

        // Lum vs Sat: modulate saturation by luminance (0.5 = no change)
        if let Some(ref lut) = lvs_lut {
            let idx = ((l * 255.0) as usize).min(255);
            new_s = (new_s * lut[idx] * 2.0).clamp(0.0, 1.0);
        }

        // Sat vs Sat: remap saturation
        if let Some(ref lut) = svs_lut {
            let idx = ((s * 255.0) as usize).min(255);
            new_s = lut[idx].clamp(0.0, 1.0);
        }

        // Orange vs Sat: targeted saturation around orange/skin hue (~30°/360 ≈ 0.083)
        if let Some(ref lut) = ovs_lut {
            let orange_dist = (h - 0.083).abs().min((h - 0.083 + 1.0).abs()).min((h - 0.083 - 1.0).abs());
            let weight = (1.0 - orange_dist * 8.0).clamp(0.0, 1.0);
            if weight > 0.001 {
                let idx = ((s * 255.0) as usize).min(255);
                let target_s = lut[idx];
                new_s = s + (target_s - s) * weight;
            }
        }

        // Convert back to RGB
        let (nr, ng, nb) = hsl_to_rgb(new_h, new_s, new_l);

        // Apply mix
        let out_r = r + (nr - r) * mix;
        let out_g = g + (ng - g) * mix;
        let out_b = b + (nb - b) * mix;

        chunk[0] = (out_r.clamp(0.0, 1.0) * 255.0) as u8;
        chunk[1] = (out_g.clamp(0.0, 1.0) * 255.0) as u8;
        chunk[2] = (out_b.clamp(0.0, 1.0) * 255.0) as u8;
    }
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let l = (max + min) * 0.5;
    if delta < 0.0001 {
        return (0.0, 0.0, l);
    }
    let s = delta / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / delta).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };
    (h, s.clamp(0.0, 1.0), l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s < 0.0001 {
        return (l, l, l);
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let m = l - c * 0.5;
    let (r, g, b) = match (h * 6.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}
