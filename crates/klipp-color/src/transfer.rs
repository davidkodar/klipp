//! sRGB ↔ linear-light transfer functions.
//!
//! IEC 61966-2-1 piecewise curves. [`srgb_to_linear`] is the EOTF
//! (decode) — turns a gamma-encoded sRGB sample into linear light.
//! [`linear_to_srgb`] is the OETF (encode) — goes the other way.
//!
//! These are the canonical building blocks for the linear-light
//! pipeline migration (see crate-level docs for the broader picture).
//! Input and output ranges are notionally `[0.0, 1.0]` but both
//! functions extrapolate cleanly above 1.0, which is load-bearing:
//! the whole point of the migration is to let pixel values exceed
//! 1.0 until the very last display-transform step.

/// sRGB EOTF — decode a gamma-encoded sRGB sample to linear light.
#[inline]
pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB OETF — encode a linear-light sample to gamma-encoded sRGB.
#[inline]
pub fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Convenience: sRGB u8 pixel → linear f32 pixel. Alpha stays linear
/// (the sRGB spec does not gamma-encode alpha).
#[inline]
pub fn srgb_u8_to_linear(px: [u8; 4]) -> [f32; 4] {
    [
        srgb_to_linear(px[0] as f32 / 255.0),
        srgb_to_linear(px[1] as f32 / 255.0),
        srgb_to_linear(px[2] as f32 / 255.0),
        px[3] as f32 / 255.0,
    ]
}

/// Convenience: linear f32 pixel → sRGB u8 pixel. Values outside
/// `[0.0, 1.0]` are clamped at the byte boundary (not before —
/// intermediate passes retain the full float range).
#[inline]
pub fn linear_to_srgb_u8(px: [f32; 4]) -> [u8; 4] {
    [
        (linear_to_srgb(px[0]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        (linear_to_srgb(px[1]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        (linear_to_srgb(px[2]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        (px[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_is_identity_within_float_epsilon() {
        for i in 0..=100 {
            let v = i as f32 / 100.0;
            let round_tripped = linear_to_srgb(srgb_to_linear(v));
            assert!(
                (v - round_tripped).abs() < 1e-5,
                "round trip failed at v={v}: got {round_tripped}",
            );
        }
    }

    #[test]
    fn mid_grey_anchor() {
        // sRGB 0.4615 ≈ linear 0.18 (18 % perceptual mid-grey).
        assert!((srgb_to_linear(0.461_5) - 0.18).abs() < 1e-3);
        assert!((linear_to_srgb(0.18) - 0.461_5).abs() < 1e-3);
    }

    #[test]
    fn zero_maps_to_zero() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert_eq!(linear_to_srgb(0.0), 0.0);
    }

    #[test]
    fn one_maps_to_one_within_rounding() {
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn above_one_does_not_nan_or_clamp() {
        // HDR-range inputs are load-bearing. The float function must
        // extrapolate cleanly — not NaN, not silent clamp.
        for &v in &[1.5_f32, 2.0, 5.0, 16.0] {
            let enc = linear_to_srgb(v);
            let dec = srgb_to_linear(v);
            assert!(enc.is_finite() && enc > 1.0, "linear_to_srgb({v}) = {enc}");
            assert!(dec.is_finite(), "srgb_to_linear({v}) = {dec}");
        }
    }

    #[test]
    fn break_point_is_continuous() {
        // Piecewise-curve joins at 0.040_45 (decode) and 0.003_130_8
        // (encode). Sampling either side of each join must stay
        // within one u8 code value to avoid a visible kink.
        let eps = 1e-6;
        let below = srgb_to_linear(0.040_45 - eps);
        let above = srgb_to_linear(0.040_45 + eps);
        assert!((above - below).abs() < 1.0 / 255.0);
        let below = linear_to_srgb(0.003_130_8 - eps);
        let above = linear_to_srgb(0.003_130_8 + eps);
        assert!((above - below).abs() < 1.0 / 255.0);
    }

    #[test]
    fn pixel_helpers_round_trip_within_one_code_value() {
        // u8 → linear → u8 must be identity within ±1 code value.
        let px = [42u8, 128u8, 200u8, 255u8];
        let linear = srgb_u8_to_linear(px);
        let back = linear_to_srgb_u8(linear);
        for i in 0..4 {
            assert!(
                (px[i] as i32 - back[i] as i32).abs() <= 1,
                "channel {i}: orig={} round={}",
                px[i],
                back[i],
            );
        }
    }

    #[test]
    fn matches_reference_impl_across_representative_samples() {
        // Cross-check against the test-crate reference (bit-identical
        // implementations; this catches us accidentally introducing
        // a tiny formula divergence between the two copies).
        for i in 0..=255u8 {
            let v = i as f32 / 255.0;
            assert_eq!(srgb_to_linear(v), klipp_testing::transfer::srgb_to_linear(v));
            assert_eq!(linear_to_srgb(v), klipp_testing::transfer::linear_to_srgb(v));
        }
    }
}
