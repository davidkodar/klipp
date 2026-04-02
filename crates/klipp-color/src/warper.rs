/// Color Warper: per-sector hue and saturation shifting.
/// 12 sectors at 30-degree intervals. Each sector can shift hue and saturation.
/// Interpolation between sectors uses cosine blending for smooth transitions.
use crate::huesat::{hsl_to_rgb, rgb_to_hsl};
pub use klipp_core::ColorWarperParams;

/// Apply color warper to an RGBA buffer (CPU path).
pub fn apply_color_warper(rgba: &mut [u8], width: u32, height: u32, params: &ColorWarperParams) {
    if params.is_identity() {
        return;
    }

    let total = (width * height) as usize;
    for i in 0..total {
        let idx = i * 4;
        if idx + 3 >= rgba.len() {
            break;
        }

        let r = rgba[idx] as f32 / 255.0;
        let g = rgba[idx + 1] as f32 / 255.0;
        let b = rgba[idx + 2] as f32 / 255.0;

        // RGB to HSL (huesat uses 0..1 hue, we need 0..360 for sector math)
        let (h01, s, l) = rgb_to_hsl(r, g, b);
        let h = h01 * 360.0;

        // Find which sector this hue falls in and interpolate
        let sector_f = h / 30.0; // 0..12
        let sector_low = (sector_f.floor() as usize) % 12;
        let sector_high = (sector_low + 1) % 12;
        let t = sector_f.fract();

        // Cosine interpolation for smooth blending between sectors
        let blend = (1.0 - (t * std::f32::consts::PI).cos()) * 0.5;

        let hue_shift =
            params.hue_shifts[sector_low] * (1.0 - blend) + params.hue_shifts[sector_high] * blend;
        let sat_shift =
            params.sat_shifts[sector_low] * (1.0 - blend) + params.sat_shifts[sector_high] * blend;

        // Weight by saturation -- desaturated pixels shouldn't be affected much
        let sat_weight = s.clamp(0.0, 1.0);

        // Apply shifts (convert hue shift from degrees to 0..1 range for hsl_to_rgb)
        let new_h = (h + hue_shift * sat_weight).rem_euclid(360.0) / 360.0;
        let new_s = (s + sat_shift * sat_weight).clamp(0.0, 1.0);

        // HSL to RGB
        let (nr, ng, nb) = hsl_to_rgb(new_h, new_s, l);

        rgba[idx] = (nr * 255.0).clamp(0.0, 255.0) as u8;
        rgba[idx + 1] = (ng * 255.0).clamp(0.0, 255.0) as u8;
        rgba[idx + 2] = (nb * 255.0).clamp(0.0, 255.0) as u8;
    }
}
