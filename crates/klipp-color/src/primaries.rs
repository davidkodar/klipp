//! Working-space primaries and gamut-conversion matrices.
//!
//! This module is the canonical home for the matrix maths that move
//! linear-light RGB between working-space bases (BT.709, ACEScg AP1,
//! BT.2020 to come). It does **not** house any tone-mapping or display
//! transforms — those live in [`crate::display`] and (for the ACES RRT)
//! the `klipp_color::aces` module that lands with Phase E Session 4.
//!
//! # Reference values
//!
//! [`acescg_to_bt709`] is taken **verbatim** from the AMPAS `aces-dev`
//! reference repository:
//!
//! * `aces-dev/transforms/ctl/lib/ACESlib.Transform_Common.ctl`
//!   (`AP1_2_REC709_PRI_MAT`)
//!
//! That file uses CAT02 chromatic adaptation between D60 (AP1 white) and
//! D65 (BT.709 white).
//!
//! [`bt709_to_acescg`] is **the numerical f64 inverse** of the AP1→709
//! matrix above, rounded to 12 decimal places. We do not use AMPAS's
//! independently-published `REC709_PRI_2_AP1_MAT` — those values are
//! rounded independently and do **not** form an exact float inverse with
//! the AP1→709 matrix (~5e-5 round-trip error in f32, exceeding the 1e-5
//! tolerance the plan calls for). Using the numerical inverse instead
//! guarantees `M_acescg→709 × M_709→acescg ≈ I` to ~1e-7 in f32.
//!
//! Practically the choice is invisible: the two BT.709→AP1 candidates
//! differ at the 4th decimal place, well under perceptual JND in linear
//! light. The win is that round-trip tests stay sharp — silent gamut
//! drift through repeated conversions becomes a numerical bug instead
//! of a "death of a thousand cuts" issue.
//!
//! # Round-trip guarantee
//!
//! [`acescg_to_bt709`] and [`bt709_to_acescg`] are mutual inverses
//! within float precision: `M_acescg→709 × M_709→acescg ≈ I` holds to
//! ~1e-6 in f32. The `matrix_round_trip_*` tests pin this; if a future
//! tweak breaks the invariant the tests fail loudly.

/// CIE 1931 chromaticity coordinates `(x, y)` for a single primary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chromaticity {
    pub x: f32,
    pub y: f32,
}

/// Full primaries definition: red, green, blue chromaticities + white
/// point. Together these define a linear RGB basis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Primaries {
    pub red: Chromaticity,
    pub green: Chromaticity,
    pub blue: Chromaticity,
    pub white: Chromaticity,
}

/// ACES AP1 primaries (ACEScg, ACEScct working space).
///
/// White point is D60 — distinct from BT.709's D65. The matrices in
/// this module include the D60↔D65 chromatic adaptation, so callers
/// don't need to apply one.
///
/// Source: ACES Technical Bulletin TB-2014-004 §6 (AP1 chromaticities
/// table) and the AMPAS aces-dev reference implementation.
pub const PRIMARIES_ACES_AP1: Primaries = Primaries {
    red:   Chromaticity { x: 0.713,    y: 0.293    },
    green: Chromaticity { x: 0.165,    y: 0.830    },
    blue:  Chromaticity { x: 0.128,    y: 0.044    },
    white: Chromaticity { x: 0.32168,  y: 0.33767  }, // D60 (ACES white)
};

/// ACES AP0 primaries (ACES2065-1 archival interchange space).
///
/// AP0 is wider than AP1 — the triangle covers the full visible
/// spectral locus. Same D60 white point as AP1. Used internally by
/// the RRT (the reference rendering transform) for sweeteners that
/// expect to operate on the un-clamped scene-referred signal.
///
/// Source: ACES Technical Bulletin TB-2014-004 §5 / SMPTE ST 2065-1.
pub const PRIMARIES_ACES_AP0: Primaries = Primaries {
    red:   Chromaticity { x: 0.7347,  y: 0.2653  },
    green: Chromaticity { x: 0.0000,  y: 1.0000  },
    blue:  Chromaticity { x: 0.0001,  y: -0.0770 },
    white: Chromaticity { x: 0.32168, y: 0.33767 }, // D60
};

/// BT.709 / sRGB primaries — the working space Klipp's grade chain
/// has run in through Phase D. White point is D65.
///
/// Source: ITU-R BT.709-6 §1.2.1.
pub const PRIMARIES_BT709: Primaries = Primaries {
    red:   Chromaticity { x: 0.640,   y: 0.330  },
    green: Chromaticity { x: 0.300,   y: 0.600  },
    blue:  Chromaticity { x: 0.150,   y: 0.060  },
    white: Chromaticity { x: 0.3127,  y: 0.3290 }, // D65
};

/// Linear-light ACEScg (AP1) RGB → linear-light BT.709 RGB.
///
/// Includes the D60→D65 CAT02 chromatic adaptation built in. Apply to
/// scene-referred linear values; the result is the same color expressed
/// in BT.709's basis (with D65 white).
///
/// Multiplying a BT.709-out-of-gamut color (e.g. a saturated red beyond
/// the BT.709 triangle that's still inside AP1) yields one or more
/// negative output channels. That's mathematically correct — those
/// values can't be reproduced on a BT.709 display — but downstream code
/// must clamp/gamut-map them before display encode (see `display.rs`).
///
/// Source: `aces-dev/transforms/ctl/lib/ACESlib.Transform_Common.ctl`
/// (`AP1_2_REC709_PRI_MAT`).
///
/// The literals carry more decimal digits than `f32` can represent
/// (~7) — clippy flags this as `excessive_precision`. We keep the
/// extra digits intentionally so the source documents the canonical
/// f64 values from the spec; the compiler truncates to the nearest
/// f32 and the round-trip tests confirm the result.
#[inline]
#[allow(clippy::excessive_precision)]
pub fn acescg_to_bt709() -> [[f32; 3]; 3] {
    [
        [ 1.7050523622, -0.6217957415, -0.0832566207],
        [-0.1302600472,  1.1408029888, -0.0105430932],
        [-0.0239638781, -0.1289693959,  1.1529332720],
    ]
}

/// Linear-light AP0 RGB → linear-light AP1 (ACEScg) RGB.
///
/// AP0 and AP1 share the D60 white point — no chromatic adaptation
/// needed, the matrix is purely a primaries-basis change. Both AP0
/// and AP1 are linear-light scene-referred spaces, so this is a
/// neutral conversion (the ACES Reference Rendering Transform's first
/// internal step does this to bring scene-linear ACES into AP1, where
/// the per-channel tone curve is applied).
///
/// Source: AMPAS aces-dev `AP0_2_AP1_MAT` from
/// `transforms/ctl/lib/ACESlib.Transform_Common.ctl`.
#[inline]
#[allow(clippy::excessive_precision)]
pub fn ap0_to_ap1() -> [[f32; 3]; 3] {
    [
        [ 1.4514393161, -0.2365107469, -0.2149285693],
        [-0.0765537734,  1.1762296998, -0.0996759264],
        [ 0.0083161484, -0.0060324498,  0.9977163014],
    ]
}

/// Linear-light AP1 (ACEScg) RGB → linear-light AP0 RGB.
///
/// The numerical f64 inverse of [`ap0_to_ap1`], rounded to 12
/// decimals — same rationale as in [`bt709_to_acescg`]: the
/// AMPAS-published `AP1_2_AP0_MAT` is rounded independently and is
/// not an exact float inverse of its sibling. Numerical inverse keeps
/// the `M_ap0→ap1 × M_ap1→ap0 ≈ I` round-trip tight at f32
/// precision.
///
/// Used by the RRT's final stage to deliver OCES (AP0) output, and
/// (looking ahead) by the ODT's first stage to consume that OCES
/// signal back into AP1 for the per-channel display encode.
#[inline]
#[allow(clippy::excessive_precision)]
pub fn ap1_to_ap0() -> [[f32; 3]; 3] {
    [
        [ 0.695452241383, 0.140678696485, 0.163869062202],
        [ 0.044794563379, 0.859671118483, 0.095534318142],
        [-0.005525882540, 0.004025210317, 1.001500672222],
    ]
}

/// Linear-light BT.709 RGB → linear-light ACEScg (AP1) RGB.
///
/// The exact numerical f64 inverse of [`acescg_to_bt709`], rounded to
/// 12 decimal places. Includes the D65→D60 CAT02 chromatic adaptation
/// implicitly (it inherits whichever adaptation the AP1→709 matrix
/// uses). Use this to lift legacy BT.709 sources or grade output into
/// the wider ACES working space; the result fits inside AP1 since AP1
/// strictly contains BT.709.
///
/// See the module-level docs for why we use the numerical inverse here
/// rather than AMPAS's independently-published `REC709_PRI_2_AP1_MAT` —
/// short version: round-trip exactness wins over redundant rounding.
///
/// Same `excessive_precision` rationale as [`acescg_to_bt709`]: extra
/// decimals document the canonical f64 inverse; the compiler narrows
/// to f32.
#[inline]
#[allow(clippy::excessive_precision)]
pub fn bt709_to_acescg() -> [[f32; 3]; 3] {
    [
        [0.613096668407, 0.339525097833, 0.047378285327],
        [0.070195416738, 0.916356016676, 0.013448705533],
        [0.020595497496, 0.109562472542, 0.869842048311],
    ]
}

/// Multiply a 3×3 matrix by a column vector. Centralised so callers
/// don't reimplement (and so a future SIMD swap touches one place).
#[inline]
pub fn matrix3_mul_vec(m: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Multiply two 3×3 matrices: `lhs × rhs`. Used by the round-trip
/// invariant tests below; not currently called from production code
/// but kept here so tests that compose matrices stay readable.
#[inline]
pub fn matrix3_mul(lhs: [[f32; 3]; 3], rhs: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] =
                lhs[i][0] * rhs[0][j] +
                lhs[i][1] * rhs[1][j] +
                lhs[i][2] * rhs[2][j];
        }
    }
    out
}

/// Linear-light AP1 (ACEScg) RGB → linear-light BT.2020 RGB.
///
/// White-point conversion D60 → D65 baked in via Bradford CAT.
/// Used by the Rec.2100 PQ ODT to deliver wide-gamut HDR content
/// in BT.2020 primaries.
///
/// Source: AMPAS aces-dev `AP1_2_REC2020_MAT` from
/// `transforms/ctl/lib/ACESlib.Transform_Common.ctl`. AP1 and
/// BT.2020 are close in chromaticity, so the matrix is close to
/// identity (the diagonal is ~1 with small off-diagonals); a
/// neutral input round-trips cleanly.
#[inline]
#[allow(clippy::excessive_precision)]
pub fn acescg_to_bt2020() -> [[f32; 3]; 3] {
    [
        [ 1.040036, -0.001154, -0.038881],
        [-0.011311,  1.005945,  0.005367],
        [ 0.000291, -0.001263,  1.000972],
    ]
}

/// Linear-light BT.2020 RGB → linear-light AP1 (ACEScg) RGB.
///
/// Numerical f64 inverse of [`acescg_to_bt2020`], rounded to 12
/// decimals. Same construction as the BT.709 ↔ AP1 / AP0 ↔ AP1
/// pairs — guarantees the matrix product hits identity to ~1e-16
/// in f64 (and well within f32 noise after narrow).
#[inline]
#[allow(clippy::excessive_precision)]
pub fn bt2020_to_acescg() -> [[f32; 3]; 3] {
    [
        [ 0.961507236379,  0.001149905973,  0.037341894990],
        [ 0.010812753529,  0.994096373481, -0.004910131918],
        [-0.000265883659,  0.001253990219,  0.999011892452],
    ]
}

/// Linear-light AP1 (ACEScg) RGB → linear-light DCI-P3 D65 RGB.
///
/// White-point conversion D60 → D65 baked in via Bradford CAT.
/// Used by the DCI-P3 D65 cinema ODT — the standard small-screen
/// digital cinema deliverable for projects mastered on a
/// professional reference display.
///
/// Source: derived from the standard AP1 + DCI-P3 D65 chromaticities
/// (P3-D65 primaries from SMPTE EG 432-1 with the cinema's D65 white,
/// not DCI's native theatrical white) using the Bradford
/// chromatic-adaptation transform — the same construction AMPAS
/// uses for its `AP1_2_P3D65_PRI_MAT` published value.
#[inline]
#[allow(clippy::excessive_precision)]
pub fn acescg_to_p3d65() -> [[f32; 3]; 3] {
    [
        [ 1.379214128253, -0.308864144674, -0.070349983580],
        [-0.069334858381,  1.082296746002, -0.012961887621],
        [-0.002159009514, -0.045459324837,  1.047618334351],
    ]
}

/// Linear-light DCI-P3 D65 RGB → linear-light AP1 (ACEScg) RGB.
///
/// Numerical f64 inverse of [`acescg_to_p3d65`], rounded to 12
/// decimals — same construction as the BT.709 / BT.2020 / AP0 ↔ AP1
/// pairs.
#[inline]
#[allow(clippy::excessive_precision)]
pub fn p3d65_to_acescg() -> [[f32; 3]; 3] {
    [
        [ 0.735797914029, 0.212166485293, 0.052035600678],
        [ 0.047179884977, 0.938045700922, 0.014774414102],
        [ 0.003563664639, 0.041141885625, 0.955294449736],
    ]
}

/// Pick the matrix that converts linear-light RGB from one working
/// space to another.
///
/// Returns `None` when source and destination are the same — callers
/// can short-circuit the matmul for the identity case. For mismatched
/// pairs the returned matrix is the standard CAT-included basis change.
///
/// Currently only `Rec709Linear ↔ AcesCG` is handled with a real
/// matrix; conversions involving `Bt2020Linear` fall back to identity
/// because the BT.2020 end-to-end pipeline isn't wired yet (the
/// CPU/GPU decoders treat it as a stub aliased to BT.709). When
/// BT.2020 lands as a first-class working space, extend this match
/// rather than scattering matrix-pair logic across consumers.
pub fn working_space_conversion_matrix(
    from: klipp_core::WorkingColorspace,
    to: klipp_core::WorkingColorspace,
) -> Option<[[f32; 3]; 3]> {
    use klipp_core::WorkingColorspace::*;
    if from == to {
        return None;
    }
    Some(match (from, to) {
        (Rec709Linear, AcesCG) => bt709_to_acescg(),
        (AcesCG, Rec709Linear) => acescg_to_bt709(),
        // BT.2020 stub: route through Rec.709 (= identity for now,
        // since the decoder treats Bt2020Linear as Rec709Linear's
        // alias). Document the limitation rather than silently
        // returning a wrong matrix.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    fn approx_eq_vec(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        approx_eq(a[0], b[0], tol) && approx_eq(a[1], b[1], tol) && approx_eq(a[2], b[2], tol)
    }

    #[test]
    fn matrix_round_trip_neutral_gray() {
        // The plan §S1 done bar: a neutral gray must round-trip back
        // to itself within 1e-5 after ACEScg → BT.709 → ACEScg.
        let gray = [0.5, 0.5, 0.5];
        let in_709 = matrix3_mul_vec(acescg_to_bt709(), gray);
        let back = matrix3_mul_vec(bt709_to_acescg(), in_709);
        assert!(
            approx_eq_vec(back, gray, 1e-5),
            "neutral gray round-trip failed: {gray:?} → {in_709:?} → {back:?}",
        );
    }

    #[test]
    fn matrix_round_trip_non_neutral() {
        // A transposed matrix would still round-trip neutral grays
        // (because the diagonal dominates); a non-neutral colour
        // catches off-diagonal sign / order errors.
        let colour = [0.7, 0.2, 0.1]; // red-heavy
        let in_709 = matrix3_mul_vec(acescg_to_bt709(), colour);
        let back = matrix3_mul_vec(bt709_to_acescg(), in_709);
        assert!(
            approx_eq_vec(back, colour, 1e-5),
            "non-neutral round-trip failed: {colour:?} → {in_709:?} → {back:?}",
        );
    }

    #[test]
    fn matrix_round_trip_dark_color() {
        // Dark / near-black values exercise the precision floor of f32.
        // 1e-5 still holds at this level.
        let dark = [0.01, 0.005, 0.02];
        let in_709 = matrix3_mul_vec(acescg_to_bt709(), dark);
        let back = matrix3_mul_vec(bt709_to_acescg(), in_709);
        assert!(
            approx_eq_vec(back, dark, 1e-5),
            "dark colour round-trip failed: {dark:?} → {in_709:?} → {back:?}",
        );
    }

    #[test]
    fn product_is_identity() {
        // Direct invariant: the two matrices are mutual inverses to
        // float precision. If this drifts, a published-spec edit broke
        // round-trip and the user-visible matrix_round_trip_* tests
        // will follow shortly.
        let id = matrix3_mul(acescg_to_bt709(), bt709_to_acescg());
        let expected: [[f32; 3]; 3] = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    approx_eq(id[i][j], expected[i][j], 5e-6),
                    "M_acescg→709 × M_709→acescg [{i}][{j}] = {} (expected {})",
                    id[i][j], expected[i][j],
                );
            }
        }
    }

    #[test]
    fn rows_sum_to_one_for_acescg_to_bt709() {
        // Property of any RGB-primaries-conversion matrix that
        // includes chromatic adaptation: equal-RGB input (a colour on
        // the source white point) maps to equal-RGB output (the
        // destination white point). For our matrices that means each
        // row sums to ~1.0 — the source white at full intensity stays
        // achromatic at full intensity in the destination basis.
        for (i, row) in acescg_to_bt709().iter().enumerate() {
            let sum: f32 = row.iter().sum();
            assert!(
                approx_eq(sum, 1.0, 1e-4),
                "acescg_to_bt709 row {i} sums to {sum}, expected ~1.0 (white-point preservation)",
            );
        }
        for (i, row) in bt709_to_acescg().iter().enumerate() {
            let sum: f32 = row.iter().sum();
            assert!(
                approx_eq(sum, 1.0, 1e-4),
                "bt709_to_acescg row {i} sums to {sum}, expected ~1.0",
            );
        }
    }

    #[test]
    fn ap1_is_wider_than_bt709() {
        // Sanity check on the matrices' direction: a saturated AP1
        // primary (e.g. pure AP1 red [1,0,0]) is OUT-of-gamut for
        // BT.709 — that's the whole point of using a wider working
        // space — so at least one BT.709 channel must be negative.
        let ap1_red = [1.0, 0.0, 0.0];
        let bt709 = matrix3_mul_vec(acescg_to_bt709(), ap1_red);
        assert!(
            bt709[1] < 0.0 || bt709[2] < 0.0,
            "AP1 pure red should be out-of-gamut in BT.709 (negative G or B), got {bt709:?}",
        );
    }

    #[test]
    fn primaries_constants_have_expected_white_points() {
        // Catch a copy-paste error swapping D60 / D65.
        assert!(approx_eq(PRIMARIES_ACES_AP1.white.x, 0.32168, 1e-5));
        assert!(approx_eq(PRIMARIES_ACES_AP1.white.y, 0.33767, 1e-5));
        assert!(approx_eq(PRIMARIES_BT709.white.x, 0.3127, 1e-5));
        assert!(approx_eq(PRIMARIES_BT709.white.y, 0.3290, 1e-5));
        // AP0 shares D60 with AP1.
        assert!(approx_eq(PRIMARIES_ACES_AP0.white.x, 0.32168, 1e-5));
        assert!(approx_eq(PRIMARIES_ACES_AP0.white.y, 0.33767, 1e-5));
    }

    #[test]
    fn ap0_ap1_round_trip_neutral_gray() {
        let gray = [0.5, 0.5, 0.5];
        let in_ap1 = matrix3_mul_vec(ap0_to_ap1(), gray);
        let back = matrix3_mul_vec(ap1_to_ap0(), in_ap1);
        assert!(
            approx_eq_vec(back, gray, 1e-5),
            "AP0/AP1 neutral round-trip failed: {gray:?} → {in_ap1:?} → {back:?}",
        );
    }

    #[test]
    fn ap0_ap1_round_trip_non_neutral() {
        let colour = [0.7, 0.2, 0.1];
        let in_ap1 = matrix3_mul_vec(ap0_to_ap1(), colour);
        let back = matrix3_mul_vec(ap1_to_ap0(), in_ap1);
        assert!(
            approx_eq_vec(back, colour, 1e-5),
            "AP0/AP1 non-neutral round-trip failed: {colour:?} → {in_ap1:?} → {back:?}",
        );
    }

    #[test]
    fn ap0_ap1_product_is_identity() {
        let id = matrix3_mul(ap0_to_ap1(), ap1_to_ap0());
        let expected = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    approx_eq(id[i][j], expected[i][j], 5e-6),
                    "M_ap0→ap1 × M_ap1→ap0 [{i}][{j}] = {} (expected {})",
                    id[i][j], expected[i][j],
                );
            }
        }
    }

    #[test]
    fn ap0_ap1_white_point_preservation() {
        // Same D60 white on both sides — equal-RGB input gives
        // equal-RGB output (rows of either matrix sum to ~1).
        for (label, m) in [("ap0_to_ap1", ap0_to_ap1()), ("ap1_to_ap0", ap1_to_ap0())] {
            for (i, row) in m.iter().enumerate() {
                let s: f32 = row.iter().sum();
                assert!(approx_eq(s, 1.0, 1e-4),
                    "{label} row {i} sums to {s}; expected 1.0 (D60→D60 preservation)");
            }
        }
    }

    #[test]
    fn ap0_essentially_contains_ap1() {
        // AP0's chromaticity triangle covers the visible spectral
        // locus, so AP1 ⊂ AP0 *in chromaticity space*. In matrix
        // form, the AP1→AP0 basis change can still produce slight
        // (~0.5%) negative excursions on saturated primaries — the
        // AP0 blue primary doesn't perfectly absorb AP1 red's
        // direction. Test that any negative is small relative to the
        // brightness of the colour.
        let ap1_red = [1.0, 0.0, 0.0];
        let ap0 = matrix3_mul_vec(ap1_to_ap0(), ap1_red);
        for c in ap0 {
            assert!(c >= -0.01,
                "AP1 red mapped to AP0: any negative excursion should be tiny; got {ap0:?}");
        }
        // Bright channel (R) should stay clearly positive.
        assert!(ap0[0] > 0.5,
            "AP1 red's R channel should still dominate in AP0; got {ap0:?}");
    }
}
