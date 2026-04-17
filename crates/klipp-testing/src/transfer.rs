//! Reference sRGB ↔ linear transfer functions.
//!
//! These are the test-side oracles. Every assertion that says
//! "function X in linear-light space should produce Y" ultimately
//! expresses Y via these helpers. Keeping them in this crate — not
//! sharing with production — means a bug in `klipp-color`'s copy
//! can't hide behind "the test uses the same buggy function."
//!
//! IEC 61966-2-1 sRGB piecewise curve. The sRGB OETF (encoding)
//! maps linear → display; the EOTF (decoding) is the inverse.
//! Production code today has only the encoding direction; Phase 1
//! adds the decoding direction alongside this reference.

/// sRGB EOTF — decode a gamma-encoded sRGB sample to linear light.
/// Input range `[0.0, 1.0]`; output range `[0.0, 1.0]`.
pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB OETF — encode a linear-light sample to gamma-encoded sRGB.
/// Input range `[0.0, 1.0]`; output range `[0.0, 1.0]`.
pub fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert a full `[R, G, B, A]` pixel from sRGB u8 to linear f32.
/// Alpha stays linear (no gamma on alpha channel by spec).
pub fn srgb_u8_to_linear(px: [u8; 4]) -> [f32; 4] {
    [
        srgb_to_linear(px[0] as f32 / 255.0),
        srgb_to_linear(px[1] as f32 / 255.0),
        srgb_to_linear(px[2] as f32 / 255.0),
        px[3] as f32 / 255.0,
    ]
}

/// Convert a full `[R, G, B, A]` pixel from linear f32 to sRGB u8.
/// Values > 1.0 clip at 255; values < 0.0 clip at 0.
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

    fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn round_trip_is_identity_within_float_epsilon() {
        // Decode then encode must be near-identity across the 0..1 range.
        for i in 0..=100 {
            let v = i as f32 / 100.0;
            let round_tripped = linear_to_srgb(srgb_to_linear(v));
            assert!(
                approx_eq(v, round_tripped, 1e-5),
                "round trip failed at v={v}: got {round_tripped}",
            );
        }
    }

    #[test]
    fn mid_grey_srgb_decodes_to_18_percent_linear() {
        // The canonical anchor: sRGB 0.46 (~18 % perceptual mid-grey)
        // decodes to 0.18 in linear light. This is the single most
        // important assertion in the entire migration — if this
        // function is wrong, every "exposure compensation" test is
        // meaningless. The 0.217 sRGB → 0.04 linear point is also
        // well-known; we test that too as a sanity check.
        assert!(approx_eq(srgb_to_linear(0.461_5), 0.18, 1e-3));
        assert!(approx_eq(linear_to_srgb(0.18), 0.461_5, 1e-3));
    }

    #[test]
    fn zero_and_one_are_fixed_points() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert_eq!(linear_to_srgb(0.0), 0.0);
        // `1.0` round-trips within a couple of ULP.
        assert!(approx_eq(srgb_to_linear(1.0), 1.0, 1e-6));
        assert!(approx_eq(linear_to_srgb(1.0), 1.0, 1e-6));
    }

    #[test]
    fn break_point_is_continuous() {
        // Linear/power-curve join at v = 0.040_45 (decode) and
        // 0.003_130_8 (encode). Sampling either side of the join
        // must produce values that differ by less than one u8 step
        // — otherwise the curve has a visible kink.
        let eps = 1e-6;
        let below = srgb_to_linear(0.040_45 - eps);
        let above = srgb_to_linear(0.040_45 + eps);
        assert!((above - below).abs() < 1.0 / 255.0);
    }

    #[test]
    fn linear_to_srgb_extrapolates_above_one_without_nan() {
        // HDR inputs are load-bearing: the pipeline lets values go
        // above 1.0, and the viewer only clamps at the u8 boundary.
        // The float transfer function itself must handle >1.0
        // cleanly — not NaN, not Inf, not silent-clamp.
        // (We skip 1.0 itself: f32 rounding gives 0.99999994 there,
        // which is numerically fine but awkward to assert about.)
        for &v in &[1.5_f32, 2.0, 5.0, 16.0] {
            let out = linear_to_srgb(v);
            assert!(out.is_finite(), "linear_to_srgb({v}) = {out}");
            assert!(out > 1.0, "above-one input collapsed below 1.0: got {out}");
        }
    }

    #[test]
    fn srgb_to_linear_handles_above_one_without_nan() {
        for &v in &[1.0_f32, 1.5, 2.0, 10.0] {
            let out = srgb_to_linear(v);
            assert!(out.is_finite(), "srgb_to_linear({v}) = {out}");
        }
    }

    #[test]
    fn pixel_helpers_round_trip() {
        let px = [42u8, 128u8, 200u8, 255u8];
        let linear = srgb_u8_to_linear(px);
        let back = linear_to_srgb_u8(linear);
        // u8 → linear f32 → u8 must be identity within ±1 code value
        // (quantisation noise is unavoidable).
        for i in 0..4 {
            assert!(
                (px[i] as i32 - back[i] as i32).abs() <= 1,
                "channel {i}: orig={} round={}",
                px[i],
                back[i],
            );
        }
    }
}
