//! Standalone difference-key matte producer — generates a matte by
//! comparing each live-frame pixel against a clean-plate reference
//! image. Pixels that differ from the plate by more than `tolerance`
//! enter the matte; pixels matching the plate are excluded.
//!
//! Both the live frame and the clean plate are passed as f32
//! linear-light RGBA (the executor's natural form). The plate is
//! loaded from disk and sRGB→linear-converted at decode time, then
//! cached + resized to live-frame dimensions via nearest-neighbour
//! before being handed to this function — same resampling shape as
//! `klipp-nodes::ExternalMatte`. Mattes read better with hard edges
//! than with bilinear blur, and the cost is amortised across paint
//! cycles via the executor's image cache.
//!
//! Produces an `Vec<f32>` of one matte value per pixel (NOT one per
//! RGBA component) — same shape as `luma_key` and `qualifier`, so the
//! executor's matte-expand step (`R=G=B=matte, A=1`) works unchanged.

use klipp_core::{DifferenceKeyMatchSpace, DifferenceKeySettings};

/// Generate a difference-key matte from a live frame plus a clean
/// plate, both in f32 linear-light RGBA (interleaved). The plate is
/// expected to already be at live-frame dimensions — resampling is
/// the executor's responsibility.
pub fn generate_difference_key_matte(
    live_linear: &[f32],
    plate_linear: &[f32],
    width: u32,
    height: u32,
    settings: &DifferenceKeySettings,
    luma: [f32; 3],
) -> Vec<f32> {
    let pixel_count = (width * height) as usize;

    if !settings.enabled || !settings.has_plate() {
        // Disabled OR no plate chosen → empty matte (nothing
        // selected). Same passthrough shape as ExternalMatte's
        // missing-image fallback so a freshly-added node doesn't
        // accidentally select-everything before the user picks a
        // plate.
        return vec![0.0f32; pixel_count];
    }

    // Defensive: malformed buffers are a programmer bug, but better
    // to ship a passthrough matte than to panic in the render path.
    if live_linear.len() != pixel_count * 4 || plate_linear.len() != pixel_count * 4 {
        tracing::warn!(
            "difference_key: buffer-size mismatch (live={}, plate={}, expected={})",
            live_linear.len(),
            plate_linear.len(),
            pixel_count * 4,
        );
        return vec![0.0f32; pixel_count];
    }

    let mut matte = vec![0.0f32; pixel_count];
    let upper = settings.tolerance + settings.soft;

    // Per pixel and independent, so it runs across cores like every
    // other full-frame pass here. It was a serial loop, which cost 9.9 ms
    // on a 4K frame all by itself.
    use rayon::prelude::*;
    matte.par_iter_mut().enumerate().for_each(|(i, out)| {
        let lo = i * 4;
        let lr = live_linear[lo];
        let lg = live_linear[lo + 1];
        let lb = live_linear[lo + 2];
        let pr = plate_linear[lo];
        let pg = plate_linear[lo + 1];
        let pb = plate_linear[lo + 2];

        let d = match settings.match_space {
            DifferenceKeyMatchSpace::Rgb => {
                // Chebyshev / L∞ — most discriminating. Per-channel
                // independent so a colour-only difference (red shirt
                // against grey wall) registers even though luminance
                // matches.
                let dr = (lr - pr).abs();
                let dg = (lg - pg).abs();
                let db = (lb - pb).abs();
                dr.max(dg).max(db)
            }
            DifferenceKeyMatchSpace::Luminance => {
                // Weighted luma difference. Faster, more forgiving of
                // plate chroma drift; suits security-cam style
                // fixed-camera shots.
                //
                // The working space's own luminance row, not Rec.709's:
                // both sides of this comparison are linear working-space
                // values, so an ACEScg project needs AP1's weights or the
                // tolerance means a different distance than the
                // colourist set.
                let yl = luma[0] * lr + luma[1] * lg + luma[2] * lb;
                let yp = luma[0] * pr + luma[1] * pg + luma[2] * pb;
                (yl - yp).abs()
            }
        };

        let mut m = soft_step(d, settings.tolerance, upper);
        if settings.invert {
            m = 1.0 - m;
        }
        *out = m;
    });

    matte
}

/// `0.0` below `lo`; ramps `0 → 1` linearly across `[lo, hi]`; `1.0`
/// above. When `hi <= lo + 1e-4` collapses to a hard step at `lo`.
/// Mirror of `luma_key::soft_range` but single-threshold (delta is
/// always ≥ 0, so we only need an upper edge).
fn soft_step(val: f32, lo: f32, hi: f32) -> f32 {
    if val <= lo {
        return 0.0;
    }
    if hi - lo <= 0.0001 {
        return 1.0;
    }
    if val >= hi {
        return 1.0;
    }
    ((val - lo) / (hi - lo)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests fix the axis to Rec.709 so the numbers read as the
    /// published weights; the per-working-space behaviour is covered
    /// by the GPU parity test.
    const REC709: [f32; 3] = [0.2126, 0.7152, 0.0722];
    use std::path::PathBuf;

    fn settings(tolerance: f32, soft: f32, invert: bool, space: DifferenceKeyMatchSpace) -> DifferenceKeySettings {
        DifferenceKeySettings {
            enabled: true,
            plate_path: Some(PathBuf::from("/tmp/test_plate.png")),
            plate_frame: None,
            tolerance,
            soft,
            invert,
            match_space: space,
        }
    }

    /// Build a 1-pixel f32 linear-light RGBA from sRGB 0..255 bytes
    /// (matches both the live-frame and plate buffer convention
    /// after the executor's load + colour-space conversion).
    fn linear_pixel_from_srgb(r: u8, g: u8, b: u8) -> [f32; 4] {
        [
            crate::transfer::srgb_to_linear(r as f32 / 255.0),
            crate::transfer::srgb_to_linear(g as f32 / 255.0),
            crate::transfer::srgb_to_linear(b as f32 / 255.0),
            1.0,
        ]
    }

    /// Convenience for tests: same colour space and shape on both
    /// sides — the only difference between "live" and "plate" is
    /// what they represent in the user's mental model.
    fn live_pixel(r: u8, g: u8, b: u8) -> [f32; 4] {
        linear_pixel_from_srgb(r, g, b)
    }
    fn plate_pixel_srgb(r: u8, g: u8, b: u8) -> [f32; 4] {
        linear_pixel_from_srgb(r, g, b)
    }

    #[test]
    fn disabled_returns_empty_matte() {
        // Default settings: enabled=false. Must return zero matte
        // (nothing selected) — opposite of LumaKey because a missing
        // plate / disabled key would otherwise select the entire
        // frame as "everything differs from a missing plate".
        let live = live_pixel(255, 0, 0);
        let plate = plate_pixel_srgb(0, 255, 0);
        let s = DifferenceKeySettings::default();
        let m = generate_difference_key_matte(&live, &plate, 1, 1, &s, REC709);
        assert!(m[0] < 1e-6);
    }

    #[test]
    fn missing_plate_returns_empty_matte() {
        // Enabled but plate_path is None — same fallback as
        // ExternalMatte. Don't ship a "everything matches" matte
        // when there's nothing to compare against.
        let live = live_pixel(255, 0, 0);
        let plate = plate_pixel_srgb(0, 255, 0);
        let mut s = settings(0.05, 0.02, false, DifferenceKeyMatchSpace::Rgb);
        s.plate_path = None;
        let m = generate_difference_key_matte(&live, &plate, 1, 1, &s, REC709);
        assert!(m[0] < 1e-6);
    }

    #[test]
    fn identical_pixel_to_plate_excluded() {
        // Live pixel matches plate exactly → distance = 0 → matte = 0
        // (this pixel is "background, nothing to select").
        let live = live_pixel(128, 64, 200);
        let plate = plate_pixel_srgb(128, 64, 200);
        let s = settings(0.05, 0.02, false, DifferenceKeyMatchSpace::Rgb);
        let m = generate_difference_key_matte(&live, &plate, 1, 1, &s, REC709);
        assert!(m[0] < 1e-6, "got {}", m[0]);
    }

    #[test]
    fn very_different_pixel_selected() {
        // White vs black → distance ≈ 1.0 in linear light → matte = 1
        // (subject is here).
        let live = live_pixel(255, 255, 255);
        let plate = plate_pixel_srgb(0, 0, 0);
        let s = settings(0.05, 0.02, false, DifferenceKeyMatchSpace::Rgb);
        let m = generate_difference_key_matte(&live, &plate, 1, 1, &s, REC709);
        assert!((m[0] - 1.0).abs() < 1e-3, "got {}", m[0]);
    }

    #[test]
    fn tolerance_threshold_respected() {
        // Distance just below tolerance → matte ≈ 0; just above →
        // ramping into the soft band.
        let live_close = live_pixel(130, 130, 130);
        let plate_close = plate_pixel_srgb(128, 128, 128); // tiny diff
        let s_strict = settings(0.10, 0.0, false, DifferenceKeyMatchSpace::Rgb);
        let m_close = generate_difference_key_matte(&live_close, &plate_close, 1, 1, &s_strict, REC709);
        assert!(m_close[0] < 1e-6, "small diff under tolerance: got {}", m_close[0]);

        // Big jump well past tolerance.
        let live_far = live_pixel(255, 255, 255);
        let plate_far = plate_pixel_srgb(0, 0, 0);
        let m_far = generate_difference_key_matte(&live_far, &plate_far, 1, 1, &s_strict, REC709);
        assert!((m_far[0] - 1.0).abs() < 1e-3, "huge diff over tolerance: got {}", m_far[0]);
    }

    #[test]
    fn soft_edge_ramps_proportionally() {
        // tolerance=0.1, soft=0.4 → upper bound = 0.5. A delta of
        // ~0.3 sits halfway through the ramp → matte ≈ 0.5.
        // Build live + plate so the per-channel max delta is about 0.3
        // in linear-light. Easiest: same green/blue, bump red by ~0.3.
        // 0.3 in linear → sRGB ≈ 144 (using the inverse curve).
        let plate = plate_pixel_srgb(0, 0, 0);
        // Find an sRGB byte whose linear value is ≈ 0.3.
        let live = live_pixel(149, 0, 0); // 149/255 in sRGB ≈ 0.30 linear
        let s = settings(0.1, 0.4, false, DifferenceKeyMatchSpace::Rgb);
        let m = generate_difference_key_matte(&live, &plate, 1, 1, &s, REC709);
        // Generous tolerance — the linear-light midpoint is approximate.
        assert!((m[0] - 0.5).abs() < 0.2, "got {} (expected ~0.5)", m[0]);
    }

    #[test]
    fn invert_flips_matte() {
        // Identical pixel: normal → 0 (background); invert → 1
        // (everything-NOT-different).
        let live = live_pixel(128, 64, 200);
        let plate = plate_pixel_srgb(128, 64, 200);
        let normal = generate_difference_key_matte(&live, &plate, 1, 1,
            &settings(0.05, 0.02, false, DifferenceKeyMatchSpace::Rgb), REC709);
        let inverted = generate_difference_key_matte(&live, &plate, 1, 1,
            &settings(0.05, 0.02, true, DifferenceKeyMatchSpace::Rgb), REC709);
        assert!(normal[0] < 1e-6, "normal: {}", normal[0]);
        assert!((inverted[0] - 1.0).abs() < 1e-6, "inverted: {}", inverted[0]);
    }

    #[test]
    fn match_space_luminance_ignores_chroma() {
        // RGB-distance picks up colour-only diff; Luminance-distance
        // doesn't. Same brightness, opposite hues — Luminance reads
        // 0 (matching), RGB reads big (different).
        // Use values whose Rec.709 luma matches but RGB differs.
        // Easy: pure red vs pure green don't match in luma (R weight
        // 0.21, G weight 0.71 — green is much brighter). Use cyan
        // (G+B = 0.93) vs yellow (R+G = 0.93) — both ~equal luma,
        // very different RGB.
        let live = live_pixel(255, 255, 0);  // yellow
        let plate = plate_pixel_srgb(0, 255, 255); // cyan
        // Yellow Y = 0.21+0.71 = 0.92; cyan Y = 0.71+0.07 = 0.78.
        // Not perfectly equal but within ~0.14. Loose tolerance for
        // Luminance space catches this as match-ish; RGB sees big diff.
        let s_lum = settings(0.2, 0.0, false, DifferenceKeyMatchSpace::Luminance);
        let m_lum = generate_difference_key_matte(&live, &plate, 1, 1, &s_lum, REC709);
        let s_rgb = settings(0.2, 0.0, false, DifferenceKeyMatchSpace::Rgb);
        let m_rgb = generate_difference_key_matte(&live, &plate, 1, 1, &s_rgb, REC709);
        // Luminance: under tolerance → matte 0
        assert!(m_lum[0] < 0.5, "Luminance should ~match yellow vs cyan: {}", m_lum[0]);
        // RGB: big delta on R and B channels → matte 1
        assert!(m_rgb[0] > 0.5, "RGB should NOT match yellow vs cyan: {}", m_rgb[0]);
    }

    #[test]
    fn batch_pixels_independent() {
        // Two pixels: [matches plate, differs hugely]. Verifies the
        // per-pixel iteration writes to the correct matte slot —
        // identical-shape contract to luma_key's batch test.
        let live_a = live_pixel(128, 64, 200);
        let live_b = live_pixel(255, 255, 255);
        let plate_a = plate_pixel_srgb(128, 64, 200);
        let plate_b = plate_pixel_srgb(0, 0, 0);
        let live: [f32; 8] = [
            live_a[0], live_a[1], live_a[2], live_a[3],
            live_b[0], live_b[1], live_b[2], live_b[3],
        ];
        let plate: [f32; 8] = [
            plate_a[0], plate_a[1], plate_a[2], plate_a[3],
            plate_b[0], plate_b[1], plate_b[2], plate_b[3],
        ];
        let s = settings(0.05, 0.02, false, DifferenceKeyMatchSpace::Rgb);
        let m = generate_difference_key_matte(&live, &plate, 2, 1, &s, REC709);
        assert_eq!(m.len(), 2);
        assert!(m[0] < 1e-6, "pixel 0 matches plate: {}", m[0]);
        assert!((m[1] - 1.0).abs() < 1e-3, "pixel 1 differs hugely: {}", m[1]);
    }

    #[test]
    fn buffer_mismatch_returns_empty_matte() {
        // Defensive: oversized buffers shouldn't panic — just return
        // empty matte and let the warn log surface the bug.
        let live = vec![0.0f32; 16]; // 4 pixels
        let plate = vec![0.0f32; 8]; // 2 pixels
        let s = settings(0.05, 0.02, false, DifferenceKeyMatchSpace::Rgb);
        let m = generate_difference_key_matte(&live, &plate, 4, 1, &s, REC709);
        assert_eq!(m.len(), 4);
        assert!(m.iter().all(|&v| v < 1e-6));
    }

    #[test]
    fn serialize_round_trip() {
        let original = DifferenceKeySettings {
            enabled: true,
            plate_path: Some(PathBuf::from("/tmp/clean_plate.png")),
            plate_frame: Some(2.5),
            tolerance: 0.123,
            soft: 0.034,
            invert: true,
            match_space: DifferenceKeyMatchSpace::Luminance,
        };
        let json = serde_json::to_string(&original).unwrap();
        let round_trip: DifferenceKeySettings = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip.enabled, original.enabled);
        assert_eq!(round_trip.plate_path, original.plate_path);
        assert_eq!(round_trip.plate_frame, original.plate_frame);
        assert!((round_trip.tolerance - original.tolerance).abs() < 1e-6);
        assert!((round_trip.soft - original.soft).abs() < 1e-6);
        assert_eq!(round_trip.invert, original.invert);
        assert_eq!(round_trip.match_space, original.match_space);
    }

    #[test]
    fn missing_match_space_defaults_to_rgb() {
        // Back-compat: an older save with no match_space field
        // should deserialize as Rgb (the default). Pin so a future
        // refactor of the enum can't silently change the default.
        let json = r#"{"enabled":true,"tolerance":0.05,"soft":0.02,"invert":false}"#;
        let s: DifferenceKeySettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.match_space, DifferenceKeyMatchSpace::Rgb);
    }
}
