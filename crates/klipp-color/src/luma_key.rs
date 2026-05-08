//! Standalone luminance-threshold key — generates a matte from
//! Rec.709 luma alone. Simpler than [`crate::qualifier`], which
//! evaluates HSL jointly. Mirrors the qualifier's u8 RGBA → matte
//! contract so the executor's existing matte-consumer wiring
//! (`MaskCutout`, `CombineMattes`, `ApplyMatte`) works unchanged.

use klipp_core::LumaKeySettings;

/// Generate a luminance-threshold matte (0.0–1.0 per pixel) from
/// `[low, high]` with `soft` falloff.
///
/// **Colorspace note:** uses Rec.709 luma weights
/// `0.2126 R + 0.7152 G + 0.0722 B`. Technically wrong for AP1
/// (ACEScg working colorspace) where the correct weights are
/// approximately `0.272 / 0.674 / 0.054`. Matches
/// [`crate::qualifier::generate_qualifier_matte`]'s behaviour;
/// both will be re-tuned per working_colorspace in a post-Phase-E
/// follow-up.
///
/// Returns an `Vec<f32>` with one matte value per pixel (NOT one
/// per RGBA component) — same shape as the qualifier helper, so
/// the executor's matte-expand step in `klipp-nodes` works
/// unchanged.
pub fn generate_luma_key_matte(
    rgba: &[u8],
    width: u32,
    height: u32,
    k: &LumaKeySettings,
) -> Vec<f32> {
    let pixel_count = (width * height) as usize;
    let mut matte = vec![0.0f32; pixel_count];

    if !k.enabled {
        // Disabled → pass everything through. Matches the
        // executor's "no settings" fallback so an enabled-toggle
        // shows live results immediately.
        return vec![1.0f32; pixel_count];
    }

    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;

        let mut m = soft_range(y, k.low, k.high, k.soft);
        if k.invert {
            m = 1.0 - m;
        }
        matte[i] = m;
    }

    matte
}

/// `1.0` inside `[lo, hi]`; ramps from `1.0` to `0.0` across `soft`
/// distance outside the range; `0.0` past that. Tiny `soft` (≤
/// 1e-4) collapses to a hard step. Matches the qualifier's helper
/// of the same name; re-implemented here so `qualifier.rs` stays
/// independent.
fn soft_range(val: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    if val >= lo && val <= hi {
        return 1.0;
    }
    if soft <= 0.0001 {
        return 0.0;
    }
    if val < lo {
        return ((1.0 - (lo - val) / soft).clamp(0.0, 1.0)) as f32;
    }
    ((1.0 - (val - hi) / soft).clamp(0.0, 1.0)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(low: f32, high: f32, soft: f32, invert: bool) -> LumaKeySettings {
        LumaKeySettings { enabled: true, low, high, soft, invert }
    }

    #[test]
    fn pure_white_selected_at_high() {
        // White (Rec.709 luma = 1.0). Range [0.5..1.0]. Inside → 1.0.
        let rgba = [255u8, 255, 255, 255];
        let m = generate_luma_key_matte(&rgba, 1, 1, &settings(0.5, 1.0, 0.05, false));
        assert!((m[0] - 1.0).abs() < 1e-3, "got {}", m[0]);
    }

    #[test]
    fn pure_black_excluded_at_low() {
        // Black (Rec.709 luma = 0.0). Range [0.5..1.0] with soft=0.05.
        // 0.0 is far below `low - soft = 0.45`, so matte = 0.0.
        let rgba = [0u8, 0, 0, 255];
        let m = generate_luma_key_matte(&rgba, 1, 1, &settings(0.5, 1.0, 0.05, false));
        assert!(m[0] < 1e-3, "got {}", m[0]);
    }

    #[test]
    fn midpoint_soft_edge_ramps_proportionally() {
        // Mid grey (Rec.709 luma ≈ 0.5). Range [0.7..1.0] with soft=0.4.
        // 0.5 sits 0.2 below `low`; soft=0.4 → matte ≈ 1 - 0.2/0.4 = 0.5.
        let mid = 128u8;
        let rgba = [mid, mid, mid, 255];
        let m = generate_luma_key_matte(&rgba, 1, 1, &settings(0.7, 1.0, 0.4, false));
        // Allow a generous tolerance — the actual mid-luma value
        // depends on Rec.709 weights, not exactly 0.5 from u8 mid.
        assert!((m[0] - 0.5).abs() < 0.1, "got {}", m[0]);
    }

    #[test]
    fn invert_flips_matte() {
        // White, range [0.5..1.0], invert=true. Without invert → 1.0;
        // with invert → 0.0.
        let rgba = [255u8, 255, 255, 255];
        let normal = generate_luma_key_matte(&rgba, 1, 1, &settings(0.5, 1.0, 0.05, false));
        let inverted = generate_luma_key_matte(&rgba, 1, 1, &settings(0.5, 1.0, 0.05, true));
        assert!((normal[0] - 1.0).abs() < 1e-3);
        assert!(inverted[0] < 1e-3, "got {}", inverted[0]);
    }

    #[test]
    fn disabled_returns_full_matte() {
        // Default settings: enabled=false. Must return full-on
        // matte so a freshly-added (not-yet-tuned) node doesn't
        // black out the viewer downstream.
        let rgba = [0u8, 0, 0, 255]; // any colour
        let s = LumaKeySettings::default();
        let m = generate_luma_key_matte(&rgba, 1, 1, &s);
        assert!((m[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn batch_pixels_independent() {
        // Two pixels: white + black. Range [0.9..1.0] with soft=0.05.
        // White (luma=1.0) → 1.0; black (luma=0.0) → 0.0. Verifies
        // the per-pixel iteration writes to the correct matte slot.
        let rgba = [255u8, 255, 255, 255, 0, 0, 0, 255];
        let m = generate_luma_key_matte(&rgba, 2, 1, &settings(0.9, 1.0, 0.05, false));
        assert_eq!(m.len(), 2);
        assert!((m[0] - 1.0).abs() < 1e-3, "white: {}", m[0]);
        assert!(m[1] < 1e-3, "black: {}", m[1]);
    }
}
