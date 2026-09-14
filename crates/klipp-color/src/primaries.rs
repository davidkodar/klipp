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

/// BT.2020 primaries — the wide-gamut basis of every HDR delivery, and
/// a working space in its own right for HDR work that does not want the
/// ACES tone curve. White point is D65, the same as BT.709's, so no
/// chromatic adaptation is involved in moving between the two.
///
/// Source: ITU-R BT.2020-2 Table 1. Red sits exactly on the spectral
/// locus (x + y = 1).
pub const PRIMARIES_BT2020: Primaries = Primaries {
    red:   Chromaticity { x: 0.708,   y: 0.292  },
    green: Chromaticity { x: 0.170,   y: 0.797  },
    blue:  Chromaticity { x: 0.131,   y: 0.046  },
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
///
/// Derived from the AMPAS chromaticities — AP1 and `REC2020_PRI` from
/// `ACESlib.Utilities_Color.ctl` — through the standard `RGBtoXYZ`
/// construction, i.e. `XYZ→Rec.2020 · D60→D65 CAT · AP1→XYZ`. Matches
/// both OpenColorIO's `ACEScg → Linear Rec.2020` processor and the
/// published AP1→Rec.2020 values to 4e-8.
///
/// AP1 and BT.2020 are close in chromaticity, so the matrix sits near
/// identity: diagonal ≈ 1 with small off-diagonals.
///
/// The previous values here were wrong by up to 3.3e-2 and cited an
/// `AP1_2_REC2020_MAT` constant that does not exist anywhere in
/// aces-dev. They survived because their rows still summed to 1, so
/// neutral round-tripped and nothing looked obviously broken — which is
/// exactly why the `bt2020_matrix_matches_published_values` test pins
/// the off-diagonals and not just the neutral axis.
#[inline]
#[allow(clippy::excessive_precision)]
pub fn acescg_to_bt2020() -> [[f32; 3]; 3] {
    [
        [ 1.025824748000, -0.020053191000, -0.005771557000],
        [-0.002234370000,  1.004586502000, -0.002352132000],
        [-0.005013351000, -0.025290072000,  1.030303423000],
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
        [ 0.974894977615,  0.019599108791,  0.005505913595],
        [ 0.002179563260,  0.995535468784,  0.002284967956],
        [ 0.004797239252,  0.024532016813,  0.970670743934],
    ]
}

/// Linear-light BT.709 RGB → linear-light BT.2020 RGB (the BT.2087
/// matrix). The f64 inverse of the colour-science BT.2020 → BT.709 table
/// the decoders use, so a BT.2020 source round-trips through a BT.709
/// working space.
#[inline]
#[allow(clippy::excessive_precision)]
pub fn bt709_to_bt2020() -> [[f32; 3]; 3] {
    [
        [0.6274038960, 0.3292830384, 0.0433130657],
        [0.0690972894, 0.9195403952, 0.0113623156],
        [0.0163914389, 0.0880133079, 0.8955952535],
    ]
}

/// Linear-light BT.2020 RGB → linear-light BT.709 RGB.
///
/// The exact table the decoders use for a BT.2020 source
/// ([`crate::camera_log::primaries_to_bt709`] with
/// [`klipp_core::ColorPrimaries::Bt2020`]), repeated here so the gamut
/// maths reads in one place; `bt2020_matrix_matches_the_decoders_table`
/// pins the two together. That makes it the exact inverse of
/// [`bt709_to_bt2020`], which was built as that table's f64 inverse.
///
/// A BT.2020 primary has no BT.709 expression, so the result goes
/// negative — that is the matrix being right, not a bug. Anything
/// heading for an 8-bit display has to clamp or gamut-map after this.
#[inline]
#[allow(clippy::excessive_precision)]
pub fn bt2020_to_bt709() -> [[f32; 3]; 3] {
    [
        [ 1.660491002,   -0.5876411388,  -0.07284986332],
        [-0.1245504745,   1.132899897,   -0.008349422604],
        [-0.01815076335, -0.100578898,    1.118729661],
    ]
}

/// Linear working-space RGB → linear BT.2020, the primaries of every HDR
/// delivery. A BT.2020-linear project is already there.
pub fn working_to_bt2020(ws: klipp_core::WorkingColorspace) -> [[f32; 3]; 3] {
    match ws {
        klipp_core::WorkingColorspace::AcesCG => acescg_to_bt2020(),
        klipp_core::WorkingColorspace::Rec709Linear => bt709_to_bt2020(),
        klipp_core::WorkingColorspace::Bt2020Linear => IDENTITY,
    }
}

/// The 3×3 that changes nothing.
pub const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Linear RGB in `p`'s basis → CIE XYZ, with `p`'s own white point.
///
/// The standard derivation: each primary contributes a column proportional
/// to its `(X, Y, Z)` at unit `y`, and the three scale factors are whatever
/// makes `RGB = (1, 1, 1)` come out as the white point's XYZ. No chromatic
/// adaptation — the result is referred to `p.white`, which is the point of
/// it: a D60 space's colours belong at their D60 chromaticities, and
/// adapting them to D65 first would move every one of them.
pub fn rgb_to_xyz_matrix(p: &Primaries) -> [[f32; 3]; 3] {
    // A chromaticity as XYZ at unit luminance.
    let xyz_of = |c: &Chromaticity| [c.x / c.y, 1.0, (1.0 - c.x - c.y) / c.y];
    let (r, g, b) = (xyz_of(&p.red), xyz_of(&p.green), xyz_of(&p.blue));
    let white = xyz_of(&p.white);
    // Columns of the unscaled matrix, then solve for the scales.
    let unscaled = [
        [r[0], g[0], b[0]],
        [r[1], g[1], b[1]],
        [r[2], g[2], b[2]],
    ];
    let scale = matrix3_mul_vec(invert3(unscaled), white);
    [
        [r[0] * scale[0], g[0] * scale[1], b[0] * scale[2]],
        [r[1] * scale[0], g[1] * scale[1], b[1] * scale[2]],
        [r[2] * scale[0], g[2] * scale[1], b[2] * scale[2]],
    ]
}

/// CIE XYZ → linear RGB in `p`'s basis. Inverse of [`rgb_to_xyz_matrix`].
pub fn xyz_to_rgb_matrix(p: &Primaries) -> [[f32; 3]; 3] {
    invert3(rgb_to_xyz_matrix(p))
}

/// Inverse of a 3x3 matrix, by cofactors. A colour basis is never
/// singular — three primaries that shared a line would not span a gamut —
/// so a zero determinant means the caller built something that is not a
/// basis, and the identity keeps that visible rather than producing NaNs.
pub fn invert3(m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let c = [
        [
            m[1][1] * m[2][2] - m[1][2] * m[2][1],
            m[0][2] * m[2][1] - m[0][1] * m[2][2],
            m[0][1] * m[1][2] - m[0][2] * m[1][1],
        ],
        [
            m[1][2] * m[2][0] - m[1][0] * m[2][2],
            m[0][0] * m[2][2] - m[0][2] * m[2][0],
            m[0][2] * m[1][0] - m[0][0] * m[1][2],
        ],
        [
            m[1][0] * m[2][1] - m[1][1] * m[2][0],
            m[0][1] * m[2][0] - m[0][0] * m[2][1],
            m[0][0] * m[1][1] - m[0][1] * m[1][0],
        ],
    ];
    let det = m[0][0] * c[0][0] + m[0][1] * c[1][0] + m[0][2] * c[2][0];
    if det.abs() < 1e-12 {
        return IDENTITY;
    }
    c.map(|row| row.map(|v| v / det))
}

/// The primaries a working space is defined by.
pub fn working_primaries(ws: klipp_core::WorkingColorspace) -> Primaries {
    match ws {
        klipp_core::WorkingColorspace::AcesCG => PRIMARIES_ACES_AP1,
        klipp_core::WorkingColorspace::Rec709Linear => PRIMARIES_BT709,
        klipp_core::WorkingColorspace::Bt2020Linear => PRIMARIES_BT2020,
    }
}

/// Relative luminance of linear RGB in a working space's primaries: the Y
/// row of its RGB → XYZ matrix.
#[allow(clippy::excessive_precision)]
pub fn luminance_weights(ws: klipp_core::WorkingColorspace) -> [f32; 3] {
    match ws {
        klipp_core::WorkingColorspace::AcesCG => [0.2722287168, 0.6740817658, 0.0536895174],
        klipp_core::WorkingColorspace::Rec709Linear => [0.2126, 0.7152, 0.0722],
        // BT.2020 NCL, ITU-R BT.2020-2 Table 4.
        klipp_core::WorkingColorspace::Bt2020Linear => [0.2627, 0.6780, 0.0593],
    }
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
/// Every pair of working spaces has its matrix here; this is the one
/// place that knows how the bases relate, so a consumer never composes
/// its own. The BT.709 ↔ BT.2020 pair shares D65 white and needs no
/// chromatic adaptation; the AP1 pairs carry a D60 ↔ D65 CAT.
///
/// Converting into a narrower gamut takes colours outside it negative.
/// That is correct and reversible in linear light — only a display
/// encode has to decide what to do about it.
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
        (Rec709Linear, Bt2020Linear) => bt709_to_bt2020(),
        (Bt2020Linear, Rec709Linear) => bt2020_to_bt709(),
        (AcesCG, Bt2020Linear) => acescg_to_bt2020(),
        (Bt2020Linear, AcesCG) => bt2020_to_acescg(),
        // Unreachable — `from == to` returned above — but spelling the
        // pairs out keeps the match exhaustive, so adding a working
        // space is a compile error here rather than a silent identity.
        (Rec709Linear, Rec709Linear) | (AcesCG, AcesCG) | (Bt2020Linear, Bt2020Linear) => return None,
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
    fn bt2020_product_is_identity() {
        let id = matrix3_mul(acescg_to_bt2020(), bt2020_to_acescg());
        for i in 0..3 {
            for j in 0..3 {
                let expected = if i == j { 1.0 } else { 0.0 };
                assert!(
                    approx_eq(id[i][j], expected, 5e-6),
                    "M_acescg→2020 × M_2020→acescg [{i}][{j}] = {} (expected {expected})",
                    id[i][j],
                );
            }
        }
    }

    #[test]
    fn bt2020_matrix_matches_published_values() {
        // Pins the off-diagonals, not just the neutral axis. The values
        // this replaced were wrong by up to 3.3e-2 yet still summed to 1
        // per row, so every neutral-grey check passed and the error only
        // showed on saturated colour.
        let published: [[f32; 3]; 3] = [
            [ 1.0258247, -0.0200531, -0.0057715],
            [-0.0022343,  1.0045865, -0.0023521],
            [-0.0050133, -0.0252900,  1.0303034],
        ];
        let got = acescg_to_bt2020();
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    approx_eq(got[i][j], published[i][j], 1e-6),
                    "acescg_to_bt2020[{i}][{j}] = {} (published {})",
                    got[i][j], published[i][j],
                );
            }
        }
    }

    #[test]
    fn bt2020_neutral_stays_neutral_both_ways() {
        for m in [acescg_to_bt2020(), bt2020_to_acescg()] {
            let out = matrix3_mul_vec(m, [1.0, 1.0, 1.0]);
            for c in out {
                assert!(
                    approx_eq(c, 1.0, 1e-5),
                    "neutral must map to neutral, got {out:?}",
                );
            }
        }
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

    #[test]
    fn bt709_to_bt2020_inverts_the_decoders_bt2020_to_bt709() {
        // A BT.2020 source decoded into a BT.709 working space and
        // exported as BT.2020 must come back as it went in.
        for rgb in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.3, 0.6, 0.1]] {
            let in_709 = crate::camera_log::primaries_to_bt709(rgb, klipp_core::ColorPrimaries::Bt2020);
            let back = matrix3_mul_vec(bt709_to_bt2020(), in_709);
            assert!(approx_eq_vec(back, rgb, 1e-5), "{rgb:?} came back as {back:?}");
        }
    }

    #[test]
    fn luminance_weights_are_the_y_of_each_space() {
        // The same colour has the same luminance in either working space,
        // up to the few thousandths the D65 → D60 adaptation moves a
        // primary's Y; white exactly.
        use klipp_core::WorkingColorspace::*;
        let dot = |w: [f32; 3], c: [f32; 3]| w[0] * c[0] + w[1] * c[1] + w[2] * c[2];
        for c in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 1.0, 1.0]] {
            let y709 = dot(luminance_weights(Rec709Linear), c);
            let yap1 = dot(luminance_weights(AcesCG), matrix3_mul_vec(bt709_to_acescg(), c));
            let tol = if c == [1.0; 3] { 1e-4 } else { 5e-3 };
            assert!(approx_eq(y709, yap1, tol), "{c:?}: {y709} vs {yap1}");
        }
    }
    #[test]
    fn bt2020_to_bt709_is_the_table_the_decoders_use() {
        // The gamut maths and the decoder must agree on what BT.2020 is,
        // or a BT.2020 source lands somewhere other than where a BT.2020
        // project says it is. Pinned elementwise so a regenerated
        // decoder table cannot drift away from this copy unnoticed.
        for basis in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            let decoder = crate::camera_log::primaries_to_bt709(basis, klipp_core::ColorPrimaries::Bt2020);
            let ours = matrix3_mul_vec(bt2020_to_bt709(), basis);
            assert!(approx_eq_vec(decoder, ours, 1e-6), "{basis:?}: {decoder:?} vs {ours:?}");
        }
    }

    #[test]
    fn bt709_and_bt2020_are_mutual_inverses() {
        for rgb in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.18, 0.18, 0.18], [0.3, 0.6, 0.1]] {
            let there = matrix3_mul_vec(bt709_to_bt2020(), rgb);
            let back = matrix3_mul_vec(bt2020_to_bt709(), there);
            assert!(approx_eq_vec(back, rgb, 1e-5), "{rgb:?} → {there:?} → {back:?}");
        }
    }

    #[test]
    fn a_bt2020_primary_has_no_bt709_expression() {
        // BT.2020's gamut contains BT.709's, so its primaries lie outside
        // BT.709 and the conversion must go negative. A matrix that kept
        // everything positive would be quietly clipping the gamut instead
        // of converting it.
        for (i, primary) in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].iter().enumerate() {
            let in_709 = matrix3_mul_vec(bt2020_to_bt709(), *primary);
            assert!(
                in_709.iter().any(|c| *c < -1e-3),
                "BT.2020 primary {i} came back inside BT.709: {in_709:?}",
            );
        }
        // The other direction cannot: BT.709's primaries are inside
        // BT.2020, so every channel stays positive.
        for primary in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            let in_2020 = matrix3_mul_vec(bt709_to_bt2020(), primary);
            assert!(in_2020.iter().all(|c| *c >= 0.0), "{primary:?} → {in_2020:?}");
        }
    }

    #[test]
    fn white_is_white_in_both_gamuts() {
        // BT.709 and BT.2020 share D65, so neutral needs no adaptation
        // and must survive the matrix untouched in both directions.
        for m in [bt709_to_bt2020(), bt2020_to_bt709()] {
            let white = matrix3_mul_vec(m, [1.0, 1.0, 1.0]);
            assert!(approx_eq_vec(white, [1.0, 1.0, 1.0], 1e-5), "white moved to {white:?}");
        }
    }

    #[test]
    fn every_pair_of_working_spaces_round_trips() {
        use klipp_core::WorkingColorspace::*;
        let spaces = [Rec709Linear, AcesCG, Bt2020Linear];
        for from in spaces {
            for to in spaces {
                let (Some(there), Some(back)) = (
                    working_space_conversion_matrix(from, to),
                    working_space_conversion_matrix(to, from),
                ) else {
                    assert_eq!(from, to, "{from:?} → {to:?} has no matrix");
                    continue;
                };
                let rgb = [0.3, 0.55, 0.12];
                let round = matrix3_mul_vec(back, matrix3_mul_vec(there, rgb));
                assert!(approx_eq_vec(round, rgb, 1e-4), "{from:?} → {to:?} → {from:?}: {round:?}");
            }
        }
    }

    #[test]
    fn a_bt2020_project_is_already_at_the_delivery_primaries() {
        // Every HDR delivery is BT.2020. A BT.2020 project is there
        // already, so this must be identity — converting again would
        // widen the gamut a second time and desaturate the whole file.
        let m = working_to_bt2020(klipp_core::WorkingColorspace::Bt2020Linear);
        assert_eq!(m, IDENTITY);
        // And the other two agree with the pairwise matrices.
        assert_eq!(working_to_bt2020(klipp_core::WorkingColorspace::Rec709Linear), bt709_to_bt2020());
        assert_eq!(working_to_bt2020(klipp_core::WorkingColorspace::AcesCG), acescg_to_bt2020());
    }

    #[test]
    fn bt2020_luminance_weights_are_the_bt2020_row() {
        use klipp_core::WorkingColorspace::*;
        // ITU-R BT.2020-2 Table 4, and the same colour carries the same
        // luminance whichever space it is expressed in.
        assert_eq!(luminance_weights(Bt2020Linear), [0.2627, 0.6780, 0.0593]);
        let dot = |w: [f32; 3], c: [f32; 3]| w[0] * c[0] + w[1] * c[1] + w[2] * c[2];
        for c in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 1.0, 1.0]] {
            let y709 = dot(luminance_weights(Rec709Linear), c);
            let y2020 = dot(luminance_weights(Bt2020Linear), matrix3_mul_vec(bt709_to_bt2020(), c));
            assert!(approx_eq(y709, y2020, 1e-3), "{c:?}: {y709} vs {y2020}");
        }
    }

    /// The derivation has to reproduce the sRGB → XYZ matrix everyone
    /// quotes (IEC 61966-2-1 / Lindbloom).
    ///
    /// Two assertions, because the published matrix is derived from D65 at
    /// more digits than BT.709 itself quotes. Given the full-precision
    /// white it reproduces to 2.2e-7 — that is the derivation being right.
    /// Given [`PRIMARIES_BT709`]'s four-decimal white, blue's Z comes out
    /// 0.950532 against the published 0.950304: a 2.3e-4 disagreement that
    /// belongs to the spec's own rounding, not to this function, and sits
    /// far below anything visible.
    #[test]
    fn bt709_primaries_give_the_published_xyz_matrix() {
        let published = [
            [0.4124564, 0.3575761, 0.1804375],
            [0.2126729, 0.7151522, 0.0721750],
            [0.0193339, 0.1191920, 0.9503041],
        ];
        let worst = |m: [[f32; 3]; 3]| {
            let mut worst = 0.0f32;
            for r in 0..3 {
                for c in 0..3 {
                    worst = worst.max((m[r][c] - published[r][c]).abs());
                }
            }
            worst
        };

        let precise_d65 = Primaries {
            white: Chromaticity { x: 0.3127266, y: 0.3290231 },
            ..PRIMARIES_BT709
        };
        let exact = worst(rgb_to_xyz_matrix(&precise_d65));
        assert!(exact < 1e-5, "with full-precision D65 the worst error is {exact:.3e}");

        let rounded = worst(rgb_to_xyz_matrix(&PRIMARIES_BT709));
        assert!(
            rounded < 3e-4,
            "with BT.709's own rounded white the worst error is {rounded:.3e}",
        );
    }

    /// And its Y row is the luminance row, which is written out by hand
    /// elsewhere in this module — two independent statements of the same
    /// three numbers, which is the point of checking.
    #[test]
    fn the_y_row_is_the_luminance_row_in_every_working_space() {
        use klipp_core::WorkingColorspace::*;
        for ws in [Rec709Linear, Bt2020Linear, AcesCG] {
            let m = rgb_to_xyz_matrix(&working_primaries(ws));
            let want = luminance_weights(ws);
            for c in 0..3 {
                assert!(
                    (m[1][c] - want[c]).abs() < 5e-4,
                    "{ws:?} channel {c}: XYZ matrix says {}, luminance_weights says {}",
                    m[1][c],
                    want[c],
                );
            }
        }
    }

    /// White in, white point out — in each space's own white, not D65's.
    #[test]
    fn unit_rgb_lands_on_the_spaces_own_white_point() {
        use klipp_core::WorkingColorspace::*;
        for ws in [Rec709Linear, Bt2020Linear, AcesCG] {
            let p = working_primaries(ws);
            let xyz = matrix3_mul_vec(rgb_to_xyz_matrix(&p), [1.0, 1.0, 1.0]);
            let sum = xyz[0] + xyz[1] + xyz[2];
            let (x, y) = (xyz[0] / sum, xyz[1] / sum);
            assert!(
                (x - p.white.x).abs() < 1e-4 && (y - p.white.y).abs() < 1e-4,
                "{ws:?}: white lands at ({x:.5}, {y:.5}), primaries say ({}, {})",
                p.white.x,
                p.white.y,
            );
        }
    }

    /// Each primary lands on its own chromaticity — the claim a CIE
    /// diagram's gamut triangle rests on.
    #[test]
    fn each_primary_lands_on_its_own_chromaticity() {
        use klipp_core::WorkingColorspace::*;
        for ws in [Rec709Linear, Bt2020Linear, AcesCG] {
            let p = working_primaries(ws);
            let m = rgb_to_xyz_matrix(&p);
            for (i, want) in [p.red, p.green, p.blue].iter().enumerate() {
                let mut rgb = [0.0; 3];
                rgb[i] = 1.0;
                let xyz = matrix3_mul_vec(m, rgb);
                let sum = xyz[0] + xyz[1] + xyz[2];
                let (x, y) = (xyz[0] / sum, xyz[1] / sum);
                assert!(
                    (x - want.x).abs() < 1e-4 && (y - want.y).abs() < 1e-4,
                    "{ws:?} primary {i}: ({x:.5}, {y:.5}) vs ({}, {})",
                    want.x,
                    want.y,
                );
            }
        }
    }

    #[test]
    fn xyz_round_trips_back_to_rgb() {
        let p = PRIMARIES_BT2020;
        let m = matrix3_mul(xyz_to_rgb_matrix(&p), rgb_to_xyz_matrix(&p));
        for r in 0..3 {
            for c in 0..3 {
                let want = if r == c { 1.0 } else { 0.0 };
                assert!((m[r][c] - want).abs() < 1e-5, "row {r} col {c} = {}", m[r][c]);
            }
        }
    }

    #[test]
    fn inverting_a_degenerate_basis_gives_the_identity_rather_than_nans() {
        let flat = [[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [1.0, 0.0, 1.0]];
        assert_eq!(invert3(flat), IDENTITY);
    }
}
