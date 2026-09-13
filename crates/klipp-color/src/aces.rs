//! ACES Reference Rendering Transform (RRT) and Output Device
//! Transforms (ODTs) — CPU oracles.
//!
//! Ports of the AMPAS aces-dev reference (commit 1.3,
//! `transforms/ctl/rrt/RRT.ctl` and `transforms/ctl/odt/sdr/`),
//! licensed BSD-3-Clause. The math here is intentionally **line-for-
//! line** with the reference: every helper (`rgb_2_saturation`,
//! `rgb_2_yc`, `sigmoid_shaper`, `glow_fwd`, `cubic_basis_shaper`,
//! `segmented_spline_c5_fwd` for the RRT;
//! `segmented_spline_c9_fwd`, `dark_surround_to_dim_surround` for the
//! ODTs) mirrors its CTL counterpart so future spec changes can be
//! tracked one-to-one. Deviations risk subtle color-science bugs that
//! only surface against a Resolve parity render.
//!
//! # Pipeline position
//!
//! ```text
//!     working ACEScg (AP1)  →  AP1→AP0  →  apply_rrt  →  OCES (AP0)  →  apply_odt_*  →  display-encoded
//!                                          ─────────  this module  ─────────────────
//! ```
//!
//! [`apply_rrt`] takes ACES2065-1 (AP0, scene-referred linear) input
//! and produces OCES (AP0, post-tonemap linear). Each ODT
//! ([`apply_odt_rec709_100_nits_dim`], plus future PQ / DCI-P3
//! variants) consumes that OCES signal and produces a display-
//! encoded RGB buffer ready for the target's transfer function.
//! Working-space callers convert AP1 → AP0 via
//! [`crate::primaries::ap1_to_ap0`] before calling [`apply_rrt`].
//!
//! # ODT coverage
//!
//! Four output transforms ship: the SDR Rec.709 100-nit-dim one, the
//! DCI-P3 D65 48-nit cinema one, and the Rec.2100 PQ pair at 1000 and
//! 4000 nits. All four are checked against OpenColorIO's ACES 1.3
//! transforms in `tests/ocio_parity.rs` — 190 samples each, a 5x5x5
//! lattice plus a grey ramp, worst 5.0e-4 — so the numbers here are
//! verified against an independent implementation of the spec and not
//! only against themselves.
//!
//! All four are also on the GPU, off data baked by
//! [`aces_odt_gpu_data`]: `klipp-engine`'s `aces_gpu_matches_cpu` holds
//! the shader to these functions within a code value.

use crate::primaries::{ap0_to_ap1, ap1_to_ap0, matrix3_mul_vec};

// ─── RRT constants (verbatim from RRT.ctl) ──────────────────────────

const RRT_GLOW_GAIN: f32 = 0.05;
const RRT_GLOW_MID: f32 = 0.08;

const RRT_RED_SCALE: f32 = 0.82;
const RRT_RED_PIVOT: f32 = 0.03;
const RRT_RED_HUE: f32 = 0.0;
const RRT_RED_WIDTH: f32 = 135.0;

/// Global desaturation factor — pulls saturated highlights toward
/// luminance-preserved neutrals before the tone curve, cleaning up
/// what the AMPAS reference describes as "over-saturated, glowy
/// look" of straight scene-linear ACES.
const RRT_SAT_FACTOR: f32 = 0.96;

/// AP1 → CIE XYZ row 1 (the Y row) — the per-channel luminance
/// coefficients in the AP1 basis. Used to weight the global-
/// desaturation matrix.
///
/// Source: AMPAS aces-dev `AP1_RGB2Y` constant (= row 1 of
/// `AP1_2_XYZ_MAT`).
const AP1_RGB2Y: [f32; 3] = [0.272_228_72, 0.674_081_77, 0.053_689_52];

// ─── Numerical sentinels ────────────────────────────────────────────

const TINY: f32 = 1.0e-10;
/// Smallest positive normalised f16. Used in place of `HALF_MIN` from
/// the reference — the RRT spline takes its log of `max(x, HALF_MIN)`
/// to avoid `-inf` near zero.
const HALF_MIN: f32 = 6.103_515_6e-5;
/// Largest finite f16. The reference clamps AP1 to this after the
/// AP0→AP1 matrix so a wild input cannot produce `inf` downstream.
const HALF_MAX: f32 = 65_504.0;
/// Quadratic-monomial coefficient matrix used by both
/// `cubic_basis_shaper` (for the red modifier) and the segmented
/// spline. Values from `Tonecurves.h` in aces-dev — a canonical
/// 3×3 quadratic basis transform.
#[allow(clippy::approx_constant)]
const M_QUAD_BASIS: [[f32; 3]; 3] = [
    [ 0.5, -1.0, 0.5],
    [-1.0,  1.0, 0.5],
    [ 0.5,  0.0, 0.0],
];

/// 4×4 cubic-monomial basis matrix used by `cubic_basis_shaper` for
/// the red modifier hue weighting. From AMPAS `Tonecurves.h`.
const M_CUBIC_BASIS: [[f32; 4]; 4] = [
    [-1.0/6.0,  3.0/6.0, -3.0/6.0,  1.0/6.0],
    [ 3.0/6.0, -6.0/6.0,  3.0/6.0,  0.0/6.0],
    [-3.0/6.0,  0.0/6.0,  3.0/6.0,  0.0/6.0],
    [ 1.0/6.0,  4.0/6.0,  1.0/6.0,  0.0/6.0],
];

// ─── Spline tone curve — segmented_spline_c5_fwd ────────────────────
//
// Five-segment quadratic spline in log-log space. The fixed knot
// points (`MIN_POINT`, `MID_POINT`, `MAX_POINT`) and the per-segment
// coefficients (`COEFS_LOW`, `COEFS_HIGH`) come from the AMPAS
// reference. `segmented_spline_c5_fwd(0.18) = 4.8` is the canonical
// "18% mid-grey maps to OCES mid-grey at 4.8 cd/m² scene-linear"
// anchor — the test below pins it to two decimals.

const SPLINE_MIN_POINT: [f32; 2] = [0.18 * (1.0 / 32_768.0) /* 2^-15 */, 0.000_1];
const SPLINE_MID_POINT: [f32; 2] = [0.18, 4.8];
const SPLINE_MAX_POINT: [f32; 2] = [0.18 * 262_144.0 /* 2^18 */, 10_000.0];
const SPLINE_SLOPE_LOW: f32 = 0.0;
const SPLINE_SLOPE_HIGH: f32 = 0.0;

const SPLINE_COEFS_LOW: [f32; 6] = [
    -4.0,
    -4.0,
    -3.157_376_577_3,
    -0.485_249_995_8,
     1.847_732_470_6,
     1.847_732_470_6,
];
const SPLINE_COEFS_HIGH: [f32; 6] = [
    -0.718_548_242_5,
     2.081_030_717_2,
     3.668_124_123_7,
     4.0,
     4.0,
     4.0,
];

// ─── Helper functions (each mirrors a CTL utility) ──────────────────

#[inline]
fn max3(rgb: [f32; 3]) -> f32 { rgb[0].max(rgb[1]).max(rgb[2]) }

#[inline]
fn min3(rgb: [f32; 3]) -> f32 { rgb[0].min(rgb[1]).min(rgb[2]) }

/// AMPAS `rgb_2_saturation`. Distance between the brightest and
/// darkest channel, normalised by the brightest.
#[inline]
fn rgb_2_saturation(rgb: [f32; 3]) -> f32 {
    let mx = max3(rgb).max(TINY);
    let mn = min3(rgb).max(TINY);
    let denom = max3(rgb).max(1.0e-2);
    (mx - mn) / denom
}

/// AMPAS `rgb_2_yc`. A chroma-aware "Y constant" used as the
/// shadow-glow lookup key — boosts saturation for low-chroma
/// shadows where the glow effect would otherwise be invisible.
#[inline]
fn rgb_2_yc(rgb: [f32; 3]) -> f32 {
    const YC_RADIUS_WEIGHT: f32 = 1.75;
    let r = rgb[0]; let g = rgb[1]; let b = rgb[2];
    let chroma = (b * (b - g) + g * (g - r) + r * (r - b)).max(0.0).sqrt();
    (b + g + r + YC_RADIUS_WEIGHT * chroma) / 3.0
}

/// AMPAS `rgb_2_hue`. Returns hue in degrees `[0, 360)`. Returns 0
/// for any neutral (R == G == B); the centered-hue + cubic-basis
/// pipeline handles the neutral case via a zero shaper weight.
#[inline]
fn rgb_2_hue(rgb: [f32; 3]) -> f32 {
    if rgb[0] == rgb[1] && rgb[1] == rgb[2] {
        return 0.0;
    }
    let y = (3.0_f32).sqrt() * (rgb[1] - rgb[2]);
    let x = 2.0 * rgb[0] - rgb[1] - rgb[2];
    let mut hue = y.atan2(x).to_degrees();
    if hue < 0.0 { hue += 360.0; }
    hue
}

/// AMPAS `center_hue`. Centres a hue value around the modifier's
/// reference hue (`RRT_RED_HUE` = 0°), wrapping through ±180°.
#[inline]
fn center_hue(hue: f32, center_h: f32) -> f32 {
    let mut h = hue - center_h;
    if h < -180.0 { h += 360.0; }
    else if h > 180.0 { h -= 360.0; }
    h
}

/// AMPAS `sigmoid_shaper`. C2-continuous shaper from `[-2, 2]` →
/// `[0, 1]`. Used to fade the glow gain across the saturation
/// transition band `[0.4, 0.6]`.
#[inline]
fn sigmoid_shaper(x: f32) -> f32 {
    let t = (1.0 - (x / 2.0).abs()).max(0.0);
    let y = 1.0 + x.signum() * (1.0 - t * t);
    y / 2.0
}

/// AMPAS `glow_fwd`. Piecewise glow gain modulated by `yc_in` (the
/// chroma-aware shadow brightness): full gain in deep shadows,
/// linearly fading to zero by `2 × glow_mid`, no boost above.
#[inline]
fn glow_fwd(yc_in: f32, glow_gain_in: f32, glow_mid: f32) -> f32 {
    if yc_in <= (2.0 / 3.0) * glow_mid {
        glow_gain_in
    } else if yc_in >= 2.0 * glow_mid {
        0.0
    } else {
        glow_gain_in * (glow_mid / yc_in - 0.5)
    }
}

/// AMPAS `cubic_basis_shaper`. Cubic B-spline kernel of full width
/// `w`, evaluating to a rounded bump of height ~1.5 at `x = 0`. Used
/// to weight the red-modifier influence by hue distance from the
/// reference red.
fn cubic_basis_shaper(x: f32, w: f32) -> f32 {
    let knots = [-w / 2.0, -w / 4.0, 0.0, w / 4.0, w / 2.0];
    if !(x > knots[0] && x < knots[4]) {
        return 0.0;
    }
    let knot_coord = (x - knots[0]) / (knots[4] - knots[0]) * 4.0;
    let j = knot_coord.floor() as i32;
    let t = knot_coord - j as f32;
    let monomials = [t * t * t, t * t, t, 1.0];
    // Pick the column of `M_CUBIC_BASIS` for this segment. The CTL
    // does this with a chained `if (j == 3)` etc — we follow the same
    // shape so the index → segment mapping is obviously correct.
    let col = match j {
        0 => 3,
        1 => 2,
        2 => 1,
        3 => 0,
        _ => return 0.0,
    };
    let y = M_CUBIC_BASIS[0][col] * monomials[0]
          + M_CUBIC_BASIS[1][col] * monomials[1]
          + M_CUBIC_BASIS[2][col] * monomials[2]
          + M_CUBIC_BASIS[3][col] * monomials[3];
    y * 1.5
}

/// AMPAS `segmented_spline_c5_fwd`. The forward RRT tone curve.
/// Five-segment quadratic spline in log-log space, anchored on
/// `(MIN_POINT, MID_POINT, MAX_POINT)` with linear extrapolation
/// (slope 0) outside.
fn segmented_spline_c5_fwd(x: f32) -> f32 {
    let logx = x.max(HALF_MIN).log10();
    let log_min = SPLINE_MIN_POINT[0].log10();
    let log_mid = SPLINE_MID_POINT[0].log10();
    let log_max = SPLINE_MAX_POINT[0].log10();

    // `N_KNOTS - 1 = 3` is the per-side spline multiplier from the
    // AMPAS reference (`N_KNOTS_LOW = N_KNOTS_HIGH = 4`). Three
    // segments per side, each accessing `coefs[j..j+3]`. The 6-coef
    // arrays are sized so `j` ∈ `[0, 2]` covers segments 0..2; the
    // last entry is the slope=0 boundary condition's redundant copy
    // (the AMPAS arrays' final two values are intentionally equal so
    // a future "soft slope" tweak only changes one coefficient).
    let knots_minus_one = 3.0_f32;
    let logy = if logx <= log_min {
        // Low extrapolation. Slope 0 makes this a constant.
        logx * SPLINE_SLOPE_LOW + (SPLINE_MIN_POINT[1].log10() - SPLINE_SLOPE_LOW * log_min)
    } else if logx > log_min && logx < log_mid {
        let knot_coord = knots_minus_one * (logx - log_min) / (log_mid - log_min);
        // Safety clamp: float rounding could push knot_coord just
        // past the loop's terminating boundary; cap j at 2 so the
        // `j+2` index can never exceed the last coef slot.
        let j = (knot_coord.floor() as usize).min(2);
        let t = knot_coord - j as f32;
        let cf = [SPLINE_COEFS_LOW[j], SPLINE_COEFS_LOW[j + 1], SPLINE_COEFS_LOW[j + 2]];
        let monomials = [t * t, t, 1.0];
        eval_spline_segment(cf, monomials)
    } else if logx >= log_mid && logx < log_max {
        let knot_coord = knots_minus_one * (logx - log_mid) / (log_max - log_mid);
        let j = (knot_coord.floor() as usize).min(2);
        let t = knot_coord - j as f32;
        let cf = [SPLINE_COEFS_HIGH[j], SPLINE_COEFS_HIGH[j + 1], SPLINE_COEFS_HIGH[j + 2]];
        let monomials = [t * t, t, 1.0];
        eval_spline_segment(cf, monomials)
    } else {
        // High extrapolation.
        logx * SPLINE_SLOPE_HIGH + (SPLINE_MAX_POINT[1].log10() - SPLINE_SLOPE_HIGH * log_max)
    };

    10.0_f32.powf(logy)
}

/// Inner helper for `segmented_spline_c5_fwd`: evaluates one segment
/// using the quadratic basis matrix `M_QUAD_BASIS` against the
/// segment's three-coefficient vector and the precomputed monomials.
#[inline]
fn eval_spline_segment(cf: [f32; 3], monomials: [f32; 3]) -> f32 {
    let row0 = M_QUAD_BASIS[0][0] * cf[0] + M_QUAD_BASIS[1][0] * cf[1] + M_QUAD_BASIS[2][0] * cf[2];
    let row1 = M_QUAD_BASIS[0][1] * cf[0] + M_QUAD_BASIS[1][1] * cf[1] + M_QUAD_BASIS[2][1] * cf[2];
    let row2 = M_QUAD_BASIS[0][2] * cf[0] + M_QUAD_BASIS[1][2] * cf[1] + M_QUAD_BASIS[2][2] * cf[2];
    row0 * monomials[0] + row1 * monomials[1] + row2 * monomials[2]
}

/// AMPAS `calc_sat_adjust_matrix`, evaluated for `RRT_SAT_FACTOR` and
/// `AP1_RGB2Y`. Returns the "global desaturation" matrix the RRT
/// applies to AP1 RGB before the per-channel tone curve.
///
/// In the column-vector convention this crate uses (`out = M · in`):
///
/// `out_i = sat * in_i + (1 - sat) * (Yr * in_r + Yg * in_g + Yb * in_b)`
fn rrt_sat_matrix() -> [[f32; 3]; 3] {
    let s = RRT_SAT_FACTOR;
    let yr = AP1_RGB2Y[0];
    let yg = AP1_RGB2Y[1];
    let yb = AP1_RGB2Y[2];
    let one_minus_s = 1.0 - s;
    [
        [one_minus_s * yr + s, one_minus_s * yg,     one_minus_s * yb    ],
        [one_minus_s * yr,     one_minus_s * yg + s, one_minus_s * yb    ],
        [one_minus_s * yr,     one_minus_s * yg,     one_minus_s * yb + s],
    ]
}

// ─── The RRT itself ─────────────────────────────────────────────────

/// Apply the full ACES Reference Rendering Transform.
///
/// **Input:** linear-light scene-referred RGB in AP0 primaries
/// (ACES2065-1). Negative values are tolerated through the
/// sweeteners but clipped to zero before the tone curve.
///
/// **Output:** linear-light OCES (AP0) — post-tonemap, but still
/// scene-referred and unclamped. The ODT (next milestone) takes this
/// and produces a display-encoded signal.
///
/// ACEScg working space callers: convert AP1 → AP0 via
/// [`crate::primaries::ap1_to_ap0`] before calling, then
/// AP0 → AP1 via [`crate::primaries::ap0_to_ap1`] on the output if
/// the next pipeline stage works in AP1. An end-to-end ODT chain
/// will fold both conversions into the ODT's first step.
pub fn apply_rrt(aces: [f32; 3]) -> [f32; 3] {
    apply_rrt_with(aces, &segmented_spline_c5_fwd)
}

/// [`apply_rrt`] with the tone curve supplied.
///
/// The GPU cannot run the segmented spline — it is a knot search over
/// coefficient tables — so it reads a [`ToneCurveTable`] baked from
/// that spline instead. Both callers share this one chain so the only
/// thing that can differ between them is the curve, which is the thing
/// the parity test measures.
fn apply_rrt_with(aces: [f32; 3], curve: &dyn Fn(f32) -> f32) -> [f32; 3] {
    // ─── Sweeteners (glow, red modifier, AP1, desaturation) ────
    let ap1_desat = rrt_sweeteners(aces);

    // ─── Per-channel tone curve ────────────────────────────────
    let rgb_post = [curve(ap1_desat[0]), curve(ap1_desat[1]), curve(ap1_desat[2])];

    // ─── AP1 → AP0 (OCES output) ───────────────────────────────
    matrix3_mul_vec(ap1_to_ap0(), rgb_post)
}

/// AMPAS `rrt_sweeteners` — everything the RRT does *except* the
/// per-channel tone curve, leaving the result in AP1.
///
/// Split out because the ACES 1.1 HDR output transforms are
/// **single-stage**: they apply these same sweeteners and then run one
/// combined tone scale (the SSTS) straight to display luminance,
/// rather than the ACES 1.0 two-stage `RRT → OCES → ODT` path that
/// [`apply_rrt`] plus a separate ODT tone curve implements. Both
/// callers must share this code or the two paths drift.
///
fn rrt_sweeteners(aces: [f32; 3]) -> [f32; 3] {
    // ─── ACES Glow modulation ──────────────────────────────────
    // Saturation gates the glow gain through a sigmoid; ycIn (a
    // chroma-aware "shadow brightness") modulates it further via
    // `glow_fwd`. The upshot: dim shadows with some saturation get
    // a small RGB-uniform boost, brighter or fully-neutral pixels
    // pass through unchanged.
    let saturation = rgb_2_saturation(aces);
    let yc_in = rgb_2_yc(aces);
    let s = sigmoid_shaper((saturation - 0.4) / 0.2);
    let added_glow = 1.0 + glow_fwd(yc_in, RRT_GLOW_GAIN * s, RRT_GLOW_MID);
    let mut a = [aces[0] * added_glow, aces[1] * added_glow, aces[2] * added_glow];

    // ─── Red modifier ──────────────────────────────────────────
    // Pulls the red channel toward `RRT_RED_PIVOT` for hues near
    // pure red (the RRT_RED_HUE = 0° axis), weighted by the cubic
    // shaper's bell over `RRT_RED_WIDTH = 135°` and by the pixel's
    // own saturation. Tames the over-saturated reds straight scene-
    // linear ACES tends to produce on skin and sunset content.
    let hue = rgb_2_hue(a);
    let centered_hue = center_hue(hue, RRT_RED_HUE);
    let hue_weight = cubic_basis_shaper(centered_hue, RRT_RED_WIDTH);
    a[0] += hue_weight * saturation * (RRT_RED_PIVOT - a[0]) * (1.0 - RRT_RED_SCALE);

    // ─── ACES → AP1 rendering space ────────────────────────────
    // Clip AP0 negatives *before* the matrix, per the reference. A
    // saturated source can land outside AP0, and letting a negative
    // channel through the matrix redistributes it into the other two —
    // which shows up as a green cast on pure reds.
    let a = [a[0].max(0.0), a[1].max(0.0), a[2].max(0.0)];
    let mut ap1 = matrix3_mul_vec(ap0_to_ap1(), a);

    // Clip again after the matrix. AP1 still has some out-of-gamut
    // excursion from very-pure spectral colours it can't represent
    // without a negative channel, and tone-mapping a negative through
    // log10 explodes the math. The HALF_MAX ceiling matches the
    // reference's guard against a wild input reaching `inf`.
    ap1 = [
        ap1[0].clamp(0.0, HALF_MAX),
        ap1[1].clamp(0.0, HALF_MAX),
        ap1[2].clamp(0.0, HALF_MAX),
    ];

    // ─── Global desaturation ───────────────────────────────────
    matrix3_mul_vec(rrt_sat_matrix(), ap1)
}

// ─── ODT — Output Device Transform (display-encoded RGB) ────────────
//
// Pipeline: OCES (AP0)
//   → AP0 → AP1
//   → segmented_spline_c9_fwd  (C9 tone curve, ODT_48nits cinema params)
//   → Y_2_linCV (normalise to display range)
//   → dark_surround_to_dim_surround (per-target gamma tweak)
//   → ODT_SAT_MAT (saturation compensation)
//   → AP1 → display primaries (with chromatic adaptation)
//   → clamp [0, 1]
//   → display OETF (target gamma)
//
// `apply_odt_rec709_100_nits_dim` is the only ODT shipped in this
// module today; PQ (Rec.2100) and DCI-P3 D65 follow the same shape
// with different output primaries / OETFs / tone-curve params and
// land as separate sessions.

// ─── ODT constants ──────────────────────────────────────────────────

/// Saturation factor applied after the tone scale to compensate for
/// the apparent saturation lift caused by the per-channel curve.
const ODT_SAT_FACTOR: f32 = 0.93;

/// Dim-surround gamma. Compensates perceptual luminance for the
/// "dim" viewing environment (typical home / studio editing
/// conditions, between cinema-dark and a bright office).
const DIM_SURROUND_GAMMA: f32 = 0.9811;

/// Cinema reference luminances. White is the peak the C9 spline's
/// maxPoint targets (48 cd/m² in `ODT_48nits` params). Black is the
/// floor. `Y_2_linCV` normalises post-spline luminance through these.
const CINEMA_WHITE: f32 = 48.0;
const CINEMA_BLACK: f32 = 0.02;

/// BT.1886 gamma for SDR Rec.709 displays. The corresponding OETF is
/// `pow(L, 1/gamma)` with `L_W=1, L_B=0` (per AMPAS).
const SDR_DISP_GAMMA: f32 = 2.4;

/// C9 tone-scale spline coefficients (low-side, then high-side) from
/// AMPAS `Tonecurves.h` `ODT_48nits` parameter set. Cinema-style
/// 48-nit peak. The arrays carry 10 entries each; `j` ranges 0..7
/// (multiplier `N_KNOTS - 1 = 7`), so the loop never indexes past
/// `j+2 = 9`.
const ODT_C9_COEFS_LOW: [f32; 10] = [
    -1.698_970_004_3,
    -1.698_970_004_3,
    -1.477_900_000_0,
    -1.229_100_000_0,
    -0.864_800_000_0,
    -0.448_000_000_0,
     0.005_180_000_0,
     0.451_108_033_4,
     0.911_374_441_4,
     0.911_374_441_4,
];
const ODT_C9_COEFS_HIGH: [f32; 10] = [
     0.515_438_696_5,
     0.847_043_778_3,
     1.135_800_000_0,
     1.380_200_000_0,
     1.519_700_000_0,
     1.598_500_000_0,
     1.646_700_000_0,
     1.674_609_135_7,
     1.687_873_339_0,
     1.687_873_339_0,
];

/// C9 spline knot points. `MIN_POINT[0]` and `MAX_POINT[0]` are
/// derived from the C5 (RRT) spline's evaluation at scene-referred
/// values 0.18·2⁻⁶·⁵ and 0.18·2⁶·⁵ respectively — i.e. the OCES
/// values 6.5 stops below and above mid-grey. The C9 spline takes
/// OCES → display-linear cd/m².
fn odt_c9_min_point() -> [f32; 2] {
    [segmented_spline_c5_fwd(0.18 * 2.0_f32.powf(-6.5)), 0.02]
}
fn odt_c9_mid_point() -> [f32; 2] { [segmented_spline_c5_fwd(0.18), 4.8] }
fn odt_c9_max_point() -> [f32; 2] {
    [segmented_spline_c5_fwd(0.18 * 2.0_f32.powf(6.5)), 48.0]
}

// ─── ODT helpers ────────────────────────────────────────────────────

/// AMPAS `segmented_spline_c9_fwd`. The forward ODT tone curve.
/// Nine quadratic segments per side in log-log space anchored on
/// `(MIN_POINT, MID_POINT, MAX_POINT)`. Same structural shape as the
/// C5 spline but with a wider per-segment array (10 coefs per side
/// instead of 6) — the input domain (OCES) is much wider than the
/// scene-linear domain the C5 covers.
fn segmented_spline_c9_fwd(x: f32) -> f32 {
    let logx = x.max(HALF_MIN).log10();
    let log_min = odt_c9_min_point()[0].log10();
    let log_mid = odt_c9_mid_point()[0].log10();
    let log_max = odt_c9_max_point()[0].log10();
    // C9: `N_KNOTS - 1 = 7`. Nine segments per side, but `j` only
    // goes up to 7 because each segment uses `coefs[j..j+3]`.
    let knots_minus_one = 7.0_f32;
    let logy = if logx <= log_min {
        odt_c9_min_point()[1].log10()
    } else if logx > log_min && logx < log_mid {
        let knot_coord = knots_minus_one * (logx - log_min) / (log_mid - log_min);
        let j = (knot_coord.floor() as usize).min(7);
        let t = knot_coord - j as f32;
        let cf = [ODT_C9_COEFS_LOW[j], ODT_C9_COEFS_LOW[j + 1], ODT_C9_COEFS_LOW[j + 2]];
        eval_spline_segment(cf, [t * t, t, 1.0])
    } else if logx >= log_mid && logx < log_max {
        let knot_coord = knots_minus_one * (logx - log_mid) / (log_max - log_mid);
        let j = (knot_coord.floor() as usize).min(7);
        let t = knot_coord - j as f32;
        let cf = [ODT_C9_COEFS_HIGH[j], ODT_C9_COEFS_HIGH[j + 1], ODT_C9_COEFS_HIGH[j + 2]];
        eval_spline_segment(cf, [t * t, t, 1.0])
    } else {
        odt_c9_max_point()[1].log10()
    };
    10.0_f32.powf(logy)
}

/// AMPAS `Y_2_linCV`. Linearly remap `Y ∈ [Y_min, Y_max]` →
/// `linCV ∈ [0, 1]`.
#[inline]
fn y_to_lin_cv(y: f32, y_max: f32, y_min: f32) -> f32 {
    (y - y_min) / (y_max - y_min)
}

/// AMPAS `bt1886_r`. The reverse of the BT.1886 EOTF — turns
/// display-linear `[0, 1]` into encoded RGB via gamma `1/γ`.
/// For our SDR ODT the tradeoff parameters reduce to `a=1, b=0`,
/// leaving a clean `pow(max(L, 0), 1/γ)`.
#[inline]
fn bt1886_inverse(l: f32, gamma: f32, l_w: f32, l_b: f32) -> f32 {
    let inv_g = 1.0 / gamma;
    let a = (l_w.powf(inv_g) - l_b.powf(inv_g)).powf(gamma);
    let denom = l_w.powf(inv_g) - l_b.powf(inv_g);
    // `denom = 0` means `l_w == l_b`; in any sane ODT that's a bug
    // and we return the input untouched rather than NaN.
    let b = if denom.abs() > 1e-12 { l_b.powf(inv_g) / denom } else { 0.0 };
    (l / a).max(0.0).powf(inv_g) - b
}

/// AMPAS `darkSurround_to_dimSurround`. Goes through CIE xyY,
/// raises the Y channel by `DIM_SURROUND_GAMMA`, returns to RGB.
/// Preserves chromaticity; only the perceived brightness curve
/// shifts to match the dim-surround viewing environment.
fn dark_surround_to_dim_surround(rec709_lin: [f32; 3]) -> [f32; 3] {
    // Rec.709 D65 → XYZ_D65. Standard matrix.
    let xyz = matrix3_mul_vec(rec709_to_xyz_d65(), rec709_lin);
    let sum = xyz[0] + xyz[1] + xyz[2];
    if sum < 1e-12 {
        return rec709_lin;
    }
    let x_chrom = xyz[0] / sum;
    let y_chrom = xyz[1] / sum;
    let big_y = xyz[1].max(0.0).powf(DIM_SURROUND_GAMMA);
    // xyY → XYZ. Standard formula: X = x·Y/y, Y = Y, Z = (1-x-y)·Y/y.
    let xyz_back = if y_chrom < 1e-12 {
        [0.0, big_y, 0.0]
    } else {
        let inv_y = 1.0 / y_chrom;
        [x_chrom * big_y * inv_y, big_y, (1.0 - x_chrom - y_chrom) * big_y * inv_y]
    };
    matrix3_mul_vec(xyz_d65_to_rec709(), xyz_back)
}

/// Rec.709 D65 → CIE XYZ (D65). Standard ITU-R BT.709 matrix.
#[inline]
#[allow(clippy::excessive_precision)]
fn rec709_to_xyz_d65() -> [[f32; 3]; 3] {
    [
        [0.4123907993, 0.3575843394, 0.1804807884],
        [0.2126390059, 0.7151686788, 0.0721923154],
        [0.0193308187, 0.1191947798, 0.9505321522],
    ]
}

/// CIE XYZ (D65) → Rec.709 D65. Standard.
#[inline]
#[allow(clippy::excessive_precision)]
fn xyz_d65_to_rec709() -> [[f32; 3]; 3] {
    [
        [ 3.2409699419, -1.5373831776, -0.4986107603],
        [-0.9692436363,  1.8759675015,  0.0415550574],
        [ 0.0556300797, -0.2039769589,  1.0569715142],
    ]
}

/// `calc_sat_adjust_matrix(0.93, AP1_RGB2Y)`, in column-vector form.
fn odt_sat_matrix() -> [[f32; 3]; 3] {
    let s = ODT_SAT_FACTOR;
    let yr = AP1_RGB2Y[0];
    let yg = AP1_RGB2Y[1];
    let yb = AP1_RGB2Y[2];
    let one_minus_s = 1.0 - s;
    [
        [one_minus_s * yr + s, one_minus_s * yg,     one_minus_s * yb    ],
        [one_minus_s * yr,     one_minus_s * yg + s, one_minus_s * yb    ],
        [one_minus_s * yr,     one_minus_s * yg,     one_minus_s * yb + s],
    ]
}

// ─── apply_odt_rec709_100_nits_dim ──────────────────────────────────

/// Apply the SDR Rec.709 / 100-nits-dim Output Device Transform.
///
/// **Input:** OCES (AP0, post-RRT scene-linear). The `apply_rrt`
/// output feeds directly here.
///
/// **Output:** Display-encoded Rec.709 RGB in `[0, 1]`. Channels are
/// gamma-encoded per BT.1886 (γ = 2.4) and clamped to the display
/// gamut. Ready to be quantised to 8-bit / 10-bit / etc. for the
/// destination signal carrier.
///
/// **On the tone curve, which used to be flagged here as wrong:** this
/// uses the AMPAS `ODT_48nits` C9 coefficients with `CINEMA_WHITE` 48
/// and `CINEMA_BLACK` 0.02, and that is what the reference
/// `ODT.Academy.Rec709_100nits_dim` does — the "100 nits" names the
/// display the transform is *for*, not a separate coefficient set. The
/// dim-surround compensation below is what accounts for the viewing
/// environment.
///
/// This docstring previously called that a caveat and said a
/// "100-nit-specific" curve was still owed. It is not: `ocio_parity.rs`
/// puts this function within **5.0e-4** of OpenColorIO's ACES 1.3
/// Rec.709 output over 190 samples, an eighth of a 255 code value. A
/// wrong tone curve could not agree with an independent implementation
/// to eight decimal places of a code value, so swapping the
/// coefficients would break verified parity rather than restore it.
pub fn apply_odt_rec709_100_nits_dim(oces: [f32; 3]) -> [f32; 3] {
    apply_odt_rec709_100_nits_dim_with(oces, &segmented_spline_c9_fwd)
}

/// [`apply_odt_rec709_100_nits_dim`] with the tone curve supplied —
/// see [`apply_rrt_with`].
fn apply_odt_rec709_100_nits_dim_with(oces: [f32; 3], curve: &dyn Fn(f32) -> f32) -> [f32; 3] {
    // OCES → AP1 rendering space.
    let rgb_pre = matrix3_mul_vec(ap0_to_ap1(), oces);

    // Per-channel C9 tone scale (OCES → display cd/m²).
    let rgb_post = [curve(rgb_pre[0]), curve(rgb_pre[1]), curve(rgb_pre[2])];

    // Display-luminance → linear CV (normalise to [0, 1]).
    let lin_cv = [
        y_to_lin_cv(rgb_post[0], CINEMA_WHITE, CINEMA_BLACK),
        y_to_lin_cv(rgb_post[1], CINEMA_WHITE, CINEMA_BLACK),
        y_to_lin_cv(rgb_post[2], CINEMA_WHITE, CINEMA_BLACK),
    ];

    // AP1 → Rec.709 D65 (with D60→D65 chromatic adaptation baked in).
    // We can do this BEFORE the dim-surround adjustment (which runs
    // in Rec.709 linear space) because both are linear matrices. The
    // saturation step also folds in here for numerical hygiene —
    // applying it on AP1 values vs. Rec.709 values produces the same
    // result since both are linear and the matrix is luminance-
    // preserving, but the AMPAS reference applies it on AP1 so we
    // do too.
    let lin_cv_desat = matrix3_mul_vec(odt_sat_matrix(), lin_cv);
    let rec709_lin = matrix3_mul_vec(crate::primaries::acescg_to_bt709(), lin_cv_desat);

    // Dim-surround compensation (per-channel via xyY).
    let dim_adjusted = dark_surround_to_dim_surround(rec709_lin);

    // Clamp to display gamut.
    let clamped = [
        dim_adjusted[0].clamp(0.0, 1.0),
        dim_adjusted[1].clamp(0.0, 1.0),
        dim_adjusted[2].clamp(0.0, 1.0),
    ];

    // BT.1886 inverse OETF — display-linear → encoded.
    [
        bt1886_inverse(clamped[0], SDR_DISP_GAMMA, 1.0, 0.0),
        bt1886_inverse(clamped[1], SDR_DISP_GAMMA, 1.0, 0.0),
        bt1886_inverse(clamped[2], SDR_DISP_GAMMA, 1.0, 0.0),
    ]
}

/// Convenience: end-to-end `working ACEScg (AP1)` → display-encoded
/// Rec.709 SDR. Wraps the full RRT + ODT chain in one call. Use this
/// from the engine's display path; `apply_rrt` and
/// `apply_odt_rec709_100_nits_dim` are exposed separately for tests
/// that need to inspect intermediate stages.
pub fn acescg_to_rec709_sdr(acescg: [f32; 3]) -> [f32; 3] {
    let oces = apply_rrt(matrix3_mul_vec(ap1_to_ap0(), acescg));
    apply_odt_rec709_100_nits_dim(oces)
}

// ─── Tone curves as tables, for the GPU ─────────────────────────────
//
// The ACES output transform is not a 3D LUT. That was measured rather
// than assumed: a shaper plus a 3D LUT baked from this file, sampled
// trilinearly, is out by up to 51 code values at 33³ and still 22 at
// 97³ (14.6 MB), and the error tracks *saturation* — the red modifier,
// the glow and the ODT's gamut limit change too fast in saturated
// mid-tones for any practical grid. Narrowing the shaper's domain
// barely moves it.
//
// What it *is* is mostly closed-form arithmetic with two per-channel
// tone curves in the middle. Those two curves are the drift-prone part
// — knot searches over coefficient tables lifted from the AMPAS CTL —
// so they are baked here, from this file, and everything else is
// transcribed. A table generated from the authority cannot drift from
// it; a hand-copied spline can, which is presumably why the last WGSL
// copy of this was removed.

/// One of the ACES tone curves, sampled for a consumer that cannot
/// evaluate the spline itself.
///
/// Sampled uniformly in `log10(x)` and storing `log10(y)`, because
/// that is the space the spline is piecewise-quadratic in — so linear
/// interpolation between entries is accurate where it matters.
/// Outside `[log_x_min, log_x_max]` both curves are *constant*
/// (`SPLINE_SLOPE_LOW` and `SPLINE_SLOPE_HIGH` are zero, and the C9
/// extrapolations are its anchor values), so clamping the lookup is
/// not an approximation.
#[derive(Clone, Debug)]
pub struct ToneCurveTable {
    pub log_x_min: f32,
    pub log_x_max: f32,
    /// `log10(curve(x))` at `log10(x)` stepping uniformly from
    /// `log_x_min` to `log_x_max`.
    pub log_y: Vec<f32>,
}

impl ToneCurveTable {
    fn bake(curve: &dyn Fn(f32) -> f32, log_x_min: f32, log_x_max: f32, n: usize) -> Self {
        let log_y = (0..n)
            .map(|i| {
                let t = i as f32 / (n - 1) as f32;
                let x = 10.0_f32.powf(log_x_min + t * (log_x_max - log_x_min));
                curve(x).max(1e-10).log10()
            })
            .collect();
        Self { log_x_min, log_x_max, log_y }
    }

    /// The curve at `x`, by interpolating the table — what the shader
    /// does, so the CPU can be measured against the same arithmetic.
    pub fn eval(&self, x: f32) -> f32 {
        let logx = x.max(HALF_MIN).log10();
        let span = self.log_x_max - self.log_x_min;
        let t = ((logx - self.log_x_min) / span).clamp(0.0, 1.0) * (self.log_y.len() - 1) as f32;
        let i = (t.floor() as usize).min(self.log_y.len() - 2);
        let f = t - i as f32;
        let logy = self.log_y[i] * (1.0 - f) + self.log_y[i + 1] * f;
        10.0_f32.powf(logy)
    }
}

/// The RRT's tone curve, over the range it is not constant on.
pub fn rrt_tone_curve_table(n: usize) -> ToneCurveTable {
    ToneCurveTable::bake(
        &segmented_spline_c5_fwd,
        SPLINE_MIN_POINT[0].log10(),
        SPLINE_MAX_POINT[0].log10(),
        n,
    )
}

/// The Rec.709 SDR ODT's tone curve, likewise.
pub fn odt_rec709_tone_curve_table(n: usize) -> ToneCurveTable {
    ToneCurveTable::bake(
        &segmented_spline_c9_fwd,
        odt_c9_min_point()[0].log10(),
        odt_c9_max_point()[0].log10(),
        n,
    )
}

/// [`acescg_to_rec709_sdr`] with both tone curves read from tables —
/// the arithmetic the GPU performs, so a parity test can separate "is
/// the table accurate" from "is the shader faithful".
pub fn acescg_to_rec709_sdr_tabled(
    acescg: [f32; 3],
    rrt: &ToneCurveTable,
    odt: &ToneCurveTable,
) -> [f32; 3] {
    let oces = apply_rrt_with(matrix3_mul_vec(ap1_to_ap0(), acescg), &|x| rrt.eval(x));
    apply_odt_rec709_100_nits_dim_with(oces, &|x| odt.eval(x))
}

// ─── ODT — Rec.2100 PQ HDR (ACES 1.1 single-stage output transform) ──
//
// The HDR deliveries do NOT reuse the ACES 1.0 two-stage
// `RRT → OCES → ODT` structure the SDR paths above use. ACES 1.1
// replaced the HDR chain with a *single-stage* tone scale — the SSTS
// (`ACESlib.SSTS.ctl`), parameterised by three luminance anchors
// (black, mid-grey, peak) — composed in `outputTransform()`
// (`ACESlib.OutputTransforms.ctl`) as:
//
//     rrt_sweeteners → ssts(per channel) → Y_2_linCV
//         → AP1→XYZ → gamut limit → D60→D65 CAT → XYZ→display
//         → clip → ST-2084 encode
//
// The two deliverables Klipp exposes are the AMPAS transforms
// `RRTODT.Academy.Rec2020_1000nits_15nits_ST2084` and its 4000-nit
// sibling. They differ in exactly one parameter — `Y_MAX` — and both
// carry `Y_MID = 15 cd/m²`. That is why PQ mid-grey lands on the same
// code value for both: PQ encodes *absolute* luminance, so the peak
// changes highlight rolloff, never mid-grey.
//
// Verified against OpenColorIO's ACES 1.3 HDR output transforms; see
// `tests/ocio_parity.rs`.

// ─── SSTS constants, verbatim from ACESlib.SSTS.ctl ─────────────────

const MIN_STOP_SDR: f32 = -6.5;
const MAX_STOP_SDR: f32 = 6.5;
const MIN_STOP_RRT: f32 = -15.0;
const MAX_STOP_RRT: f32 = 18.0;

const MIN_LUM_SDR: f32 = 0.02;
const MAX_LUM_SDR: f32 = 48.0;
const MIN_LUM_RRT: f32 = 0.0001;
const MAX_LUM_RRT: f32 = 10_000.0;

/// Black / mid-grey luminance shared by both PQ deliverables.
const HDR_PQ_Y_MIN: f32 = 0.0001;
const HDR_PQ_Y_MID: f32 = 15.0;

/// AMPAS `interpolate1D` specialised to a two-entry table (every SSTS
/// table has exactly two rows). Linear between the points, clamped to
/// the endpoint values outside.
fn interpolate1d_2(table: [[f32; 2]; 2], p: f32) -> f32 {
    let (x0, y0) = (table[0][0], table[0][1]);
    let (x1, y1) = (table[1][0], table[1][1]);
    if p <= x0 {
        y0
    } else if p >= x1 {
        y1
    } else {
        y0 + (y1 - y0) * (p - x0) / (x1 - x0)
    }
}

/// AMPAS `lookup_ACESmin` — the ACES value the tone scale's black
/// point sits at, for a given display black luminance.
fn lookup_aces_min(min_lum: f32) -> f32 {
    let table = [
        [MIN_LUM_RRT.log10(), MIN_STOP_RRT],
        [MIN_LUM_SDR.log10(), MIN_STOP_SDR],
    ];
    0.18 * 2.0_f32.powf(interpolate1d_2(table, min_lum.log10()))
}

/// AMPAS `lookup_ACESmax` — the ACES value the tone scale's peak sits
/// at, for a given display peak luminance.
fn lookup_aces_max(max_lum: f32) -> f32 {
    let table = [
        [MAX_LUM_SDR.log10(), MAX_STOP_SDR],
        [MAX_LUM_RRT.log10(), MAX_STOP_RRT],
    ];
    0.18 * 2.0_f32.powf(interpolate1d_2(table, max_lum.log10()))
}

/// One anchor of the tone scale: an ACES value, the display luminance
/// it maps to, and the curve's slope there.
#[derive(Clone, Copy)]
struct TsPoint {
    x: f32,
    y: f32,
    slope: f32,
}

/// A fully-resolved SSTS. Cheap to copy but computed once per ODT
/// configuration — see [`hdr_pq_params`].
#[derive(Clone, Copy)]
struct TsParams {
    min: TsPoint,
    mid: TsPoint,
    max: TsPoint,
    coefs_low: [f32; 6],
    coefs_high: [f32; 6],
}

/// AMPAS `init_coefsLow`. Two outer coefficients straddle each anchor
/// (linear extrapolation of the anchor's slope); the middle one sets
/// how sharply the curve bends and is interpolated from a fixed table.
fn init_coefs_low(low: TsPoint, mid: TsPoint) -> [f32; 5] {
    let log_low_x = low.x.log10();
    let log_mid_x = mid.x.log10();
    let knot_inc = (log_mid_x - log_low_x) / 3.0;
    let mut c = [0.0_f32; 5];

    c[0] = low.slope * (log_low_x - 0.5 * knot_inc) + (low.y.log10() - low.slope * log_low_x);
    c[1] = low.slope * (log_low_x + 0.5 * knot_inc) + (low.y.log10() - low.slope * log_low_x);
    c[3] = mid.slope * (log_mid_x - 0.5 * knot_inc) + (mid.y.log10() - mid.slope * log_mid_x);
    c[4] = mid.slope * (log_mid_x + 0.5 * knot_inc) + (mid.y.log10() - mid.slope * log_mid_x);

    let bends = [[MIN_STOP_RRT, 0.18], [MIN_STOP_SDR, 0.35]];
    let pct = interpolate1d_2(bends, (low.x / 0.18).log2());
    c[2] = low.y.log10() + pct * (mid.y.log10() - low.y.log10());
    c
}

/// AMPAS `init_coefsHigh`. Mirror of [`init_coefs_low`] across
/// mid-grey, with its own bend table.
fn init_coefs_high(mid: TsPoint, max: TsPoint) -> [f32; 5] {
    let log_mid_x = mid.x.log10();
    let log_max_x = max.x.log10();
    let knot_inc = (log_max_x - log_mid_x) / 3.0;
    let mut c = [0.0_f32; 5];

    c[0] = mid.slope * (log_mid_x - 0.5 * knot_inc) + (mid.y.log10() - mid.slope * log_mid_x);
    c[1] = mid.slope * (log_mid_x + 0.5 * knot_inc) + (mid.y.log10() - mid.slope * log_mid_x);
    c[3] = max.slope * (log_max_x - 0.5 * knot_inc) + (max.y.log10() - max.slope * log_max_x);
    c[4] = max.slope * (log_max_x + 0.5 * knot_inc) + (max.y.log10() - max.slope * log_max_x);

    let bends = [[MAX_STOP_SDR, 0.89], [MAX_STOP_RRT, 0.90]];
    let pct = interpolate1d_2(bends, (max.x / 0.18).log2());
    c[2] = mid.y.log10() + pct * (max.y.log10() - mid.y.log10());
    c
}

/// AMPAS `shift` — slide an ACES value by `exp_shift` stops.
#[inline]
fn ts_shift(x: f32, exp_shift: f32) -> f32 {
    2.0_f32.powf(x.log2() - exp_shift)
}

/// AMPAS `init_TsParams`. Note the ordering the reference relies on:
/// the spline coefficients are derived from the *unshifted* anchors,
/// and only then are the anchor x-positions shifted. Reversing that
/// changes the curve.
fn init_ts_params(min_lum: f32, max_lum: f32, exp_shift: f32) -> TsParams {
    let mut min_pt = TsPoint { x: lookup_aces_min(min_lum), y: min_lum, slope: 0.0 };
    let mut mid_pt = TsPoint { x: 0.18, y: 4.8, slope: 1.55 };
    let mut max_pt = TsPoint { x: lookup_aces_max(max_lum), y: max_lum, slope: 0.0 };

    let c_low = init_coefs_low(min_pt, mid_pt);
    let c_high = init_coefs_high(mid_pt, max_pt);

    min_pt.x = ts_shift(lookup_aces_min(min_lum), exp_shift);
    mid_pt.x = ts_shift(0.18, exp_shift);
    max_pt.x = ts_shift(lookup_aces_max(max_lum), exp_shift);

    TsParams {
        min: min_pt,
        mid: mid_pt,
        max: max_pt,
        // The final coefficient is a deliberate duplicate of the one
        // before it — the slope=0 boundary condition, matching the
        // reference's array layout.
        coefs_low: [c_low[0], c_low[1], c_low[2], c_low[3], c_low[4], c_low[4]],
        coefs_high: [c_high[0], c_high[1], c_high[2], c_high[3], c_high[4], c_high[4]],
    }
}

/// AMPAS `ssts` — the single-stage tone scale. Maps an AP1 channel
/// value to absolute display luminance in cd/m². Three quadratic
/// segments per side in log-log space, linear extrapolation outside.
fn ssts(x: f32, c: &TsParams) -> f32 {
    // N_KNOTS_LOW = N_KNOTS_HIGH = 4, so the per-side multiplier is 3.
    let knots_minus_one = 3.0_f32;

    // Deviation from the CTL, deliberate: the reference floors at
    // `HALF_MIN` (6.1e-5) purely to keep `log10` finite. But these HDR
    // curves put their min anchor at x ~ 2.5e-6, *below* that floor —
    // so under `HALF_MIN` the low-extrapolation branch below is
    // unreachable and ACES 0.0 renders at 2.3e-4 cd/m² instead of
    // `Y_MIN`, i.e. PQ code ~0.0019 (about two 10-bit code values of
    // lifted black) i.e. exactly what `STRETCH_BLACK` exists to
    // prevent. Flooring at `TINY` instead lets black reach `Y_MIN` and
    // encode to PQ 0, which is what OpenColorIO produces and what the
    // reference's own comment says the intent is. Values at or above
    // `HALF_MIN` take the same branch either way, so nothing else in
    // the curve moves.
    let logx = x.max(TINY).log10();
    let log_min_x = c.min.x.log10();
    let log_mid_x = c.mid.x.log10();
    let log_max_x = c.max.x.log10();

    let logy = if logx <= log_min_x {
        logx * c.min.slope + (c.min.y.log10() - c.min.slope * log_min_x)
    } else if logx < log_mid_x {
        let knot_coord = knots_minus_one * (logx - log_min_x) / (log_mid_x - log_min_x);
        // Cap at the last segment: float rounding can push
        // `knot_coord` to exactly 3.0 at the boundary, and segment 2
        // evaluated at t = 1 is precisely the mid anchor.
        let j = (knot_coord.floor() as usize).min(2);
        let t = knot_coord - j as f32;
        let cf = [c.coefs_low[j], c.coefs_low[j + 1], c.coefs_low[j + 2]];
        eval_spline_segment(cf, [t * t, t, 1.0])
    } else if logx < log_max_x {
        let knot_coord = knots_minus_one * (logx - log_mid_x) / (log_max_x - log_mid_x);
        let j = (knot_coord.floor() as usize).min(2);
        let t = knot_coord - j as f32;
        let cf = [c.coefs_high[j], c.coefs_high[j + 1], c.coefs_high[j + 2]];
        eval_spline_segment(cf, [t * t, t, 1.0])
    } else {
        logx * c.max.slope + (c.max.y.log10() - c.max.slope * log_max_x)
    };

    10.0_f32.powf(logy)
}

/// Solve `ssts(x, c) == y` for `x`.
///
/// The reference inverts the quadratic spline analytically
/// (`inv_ssts`). We bisect instead: `ssts` is monotonically
/// increasing, and this is called **once per ODT configuration**
/// rather than per pixel (see [`hdr_pq_params`]), so the only thing
/// an analytic inverse would buy is a second 70-line spline
/// implementation with exactly one caller. Bisection runs in log2
/// space so convergence is uniform across the curve's huge domain.
fn inv_ssts_bisect(y: f32, c: &TsParams) -> f32 {
    let mut lo = c.min.x.max(HALF_MIN).log2();
    let mut hi = c.max.x.log2();
    for _ in 0..64 {
        let mid = 0.5 * (lo + hi);
        if ssts(2.0_f32.powf(mid), c) < y {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    2.0_f32.powf(0.5 * (lo + hi))
}

/// Resolve the SSTS for one PQ deliverable.
///
/// Reproduces the reference's two-pass `expShift` derivation: build
/// the curve with no shift, ask where mid-grey luminance actually
/// landed, then rebuild shifted so `Y_MID` sits on ACES 0.18. The
/// reference calls this "a bit of a hack"; it is nonetheless what
/// defines the shipped transforms.
fn init_hdr_pq_params(y_max: f32) -> TsParams {
    let unshifted = init_ts_params(HDR_PQ_Y_MIN, y_max, 0.0);
    let exp_shift = inv_ssts_bisect(HDR_PQ_Y_MID, &unshifted).log2() - 0.18_f32.log2();
    init_ts_params(HDR_PQ_Y_MIN, y_max, exp_shift)
}

/// Cached parameters. Deriving them costs a bisection over the spline;
/// doing that per pixel would be absurd, and they only depend on the
/// deliverable.
fn hdr_pq_params(y_max: f32) -> &'static TsParams {
    static P1000: std::sync::OnceLock<TsParams> = std::sync::OnceLock::new();
    static P4000: std::sync::OnceLock<TsParams> = std::sync::OnceLock::new();
    if y_max >= 4000.0 {
        P4000.get_or_init(|| init_hdr_pq_params(4000.0))
    } else {
        P1000.get_or_init(|| init_hdr_pq_params(1000.0))
    }
}

// ─── HDR ODT matrices ──────────────────────────────────────────────
//
// Derived from the AMPAS chromaticities (AP1 and `REC2020_PRI` from
// `ACESlib.Utilities_Color.ctl`) via the standard `RGBtoXYZ`
// construction, and cross-checked two ways: `ap1_to_xyz()`'s middle
// row reproduces this crate's `AP1_RGB2Y` to 4e-9, and the composed
// `xyz_to_rec2020 · d60_to_d65_cat · ap1_to_xyz` matches OCIO's
// ACEScg → Linear Rec.2020 processor to 4e-8.
//
// Kept here rather than in `primaries` because the HDR ODT needs the
// individual factors — the reference clips to the limiting primaries
// in XYZ, *between* the two halves — so a pre-composed AP1→Rec.2020
// matrix cannot express it.

/// Linear AP1 (ACEScg) → CIE XYZ, ACES white (D60).
#[inline]
#[allow(clippy::excessive_precision)]
fn ap1_to_xyz() -> [[f32; 3]; 3] {
    [
        [ 0.662454181109,  0.134004206456,  0.156187687005],
        [ 0.272228716781,  0.674081765811,  0.053689517408],
        [-0.005574649490,  0.004060733529,  1.010339100313],
    ]
}

/// CIE XYZ (D65) → linear BT.2020.
#[inline]
#[allow(clippy::excessive_precision)]
fn xyz_to_rec2020() -> [[f32; 3]; 3] {
    [
        [ 1.716651187971, -0.355670783776, -0.253366281374],
        [-0.666684351832,  1.616481236635,  0.015768545814],
        [ 0.017639857445, -0.042770613258,  0.942103121235],
    ]
}

/// Linear BT.2020 → CIE XYZ (D65).
#[inline]
#[allow(clippy::excessive_precision)]
fn rec2020_to_xyz() -> [[f32; 3]; 3] {
    [
        [ 0.636958048301,  0.144616903586,  0.168880975164],
        [ 0.262700212011,  0.677998071519,  0.059301716470],
        [ 0.000000000000,  0.028072693049,  1.060985057711],
    ]
}

/// Bradford chromatic adaptation, ACES white (D60) → D65. Matches
/// AMPAS `D60_2_D65_CAT` to 8 decimals.
#[inline]
#[allow(clippy::excessive_precision)]
fn d60_to_d65_cat() -> [[f32; 3]; 3] {
    [
        [ 0.987224008703, -0.006113228607,  0.015953288336],
        [-0.007598371812,  1.001861484740,  0.005330035791],
        [ 0.003072577059, -0.005095961511,  1.081680603066],
    ]
}

/// AMPAS `linCV_2_Y` — the inverse of [`y_to_lin_cv`].
#[inline]
fn lin_cv_to_y(lin_cv: f32, y_max: f32, y_min: f32) -> f32 {
    lin_cv * (y_max - y_min) + y_min
}

/// AMPAS `Y_2_ST2084` — absolute luminance in cd/m² → PQ code value.
#[inline]
fn y_to_st2084(y: f32) -> f32 {
    crate::camera_log::linear_to_pq(y / 10_000.0)
}

/// AMPAS `limit_to_primaries` for Rec.2020. Round-trips XYZ through
/// the limiting primaries, clipping to `[0, 1]` on the way, which
/// discards anything the deliverable's gamut cannot hold.
fn limit_to_rec2020_primaries(xyz: [f32; 3]) -> [f32; 3] {
    let rgb = matrix3_mul_vec(xyz_to_rec2020(), xyz);
    let clipped = [
        rgb[0].clamp(0.0, 1.0),
        rgb[1].clamp(0.0, 1.0),
        rgb[2].clamp(0.0, 1.0),
    ];
    matrix3_mul_vec(rec2020_to_xyz(), clipped)
}

/// AMPAS `outputTransform` specialised to the Rec.2020 / ST-2084
/// deliverables: `EOTF = 0` (PQ), `SURROUND = 0` (dark),
/// `STRETCH_BLACK = true`, `D60_SIM = false`, `LEGAL_RANGE = false`,
/// limiting primaries == display primaries == Rec.2020.
///
/// **Input is ACES AP0 scene-linear, not OCES.** The single-stage
/// design has no OCES intermediate — that is the whole point of it.
fn output_transform_rec2100_pq(aces_ap0: [f32; 3], y_max: f32) -> [f32; 3] {
    let params = hdr_pq_params(y_max);

    // Glow / red modifier / AP1 / desaturation — shared with the SDR
    // path so the two renderings cannot drift apart.
    let rgb_pre = rrt_sweeteners(aces_ap0);

    // The tone scale, per channel. Output is absolute display
    // luminance in cd/m², still on AP1 primaries.
    let rgb_post = [
        ssts(rgb_pre[0], params),
        ssts(rgb_pre[1], params),
        ssts(rgb_pre[2], params),
    ];

    // Absolute luminance → linear code value.
    let lin_cv = [
        y_to_lin_cv(rgb_post[0], y_max, HDR_PQ_Y_MIN),
        y_to_lin_cv(rgb_post[1], y_max, HDR_PQ_Y_MIN),
        y_to_lin_cv(rgb_post[2], y_max, HDR_PQ_Y_MIN),
    ];

    // AP1 → CIE XYZ. Still D60-adapted at this point.
    let mut xyz = matrix3_mul_vec(ap1_to_xyz(), lin_cv);

    // SURROUND = 0 (dark): the tone scale is designed for a dark
    // surround, so no gamma adjustment. The reference's dim-surround
    // block is inactive for HDR regardless.

    // Gamut-limit to the limiting primaries. The reference limits
    // unconditionally, even when limiting == display.
    //
    // NOTE: this clips in a D65-primaries space applied to XYZ that is
    // still D60-adapted — the CAT only runs afterwards. That
    // white-point mismatch is what the reference does; "fixing" it
    // breaks parity.
    xyz = limit_to_rec2020_primaries(xyz);

    // ACES white (D60) → display white (D65), since D60_SIM = false.
    xyz = matrix3_mul_vec(d60_to_d65_cat(), xyz);

    // XYZ → display encoding primaries, then clip values that project
    // outside them.
    let rgb = matrix3_mul_vec(xyz_to_rec2020(), xyz);
    let rgb = [rgb[0].max(0.0), rgb[1].max(0.0), rgb[2].max(0.0)];

    // ST-2084 encode. STRETCH_BLACK = true means the trip back to
    // absolute luminance uses Ymin = 0, so the darkest values quantise
    // to a PQ code value of 0 instead of floating above it.
    [
        y_to_st2084(lin_cv_to_y(rgb[0], y_max, 0.0).max(0.0)),
        y_to_st2084(lin_cv_to_y(rgb[1], y_max, 0.0).max(0.0)),
        y_to_st2084(lin_cv_to_y(rgb[2], y_max, 0.0).max(0.0)),
    ]
}

/// End-to-end ACEScg → Rec.2100 PQ HDR, 1000 cd/m² mastering display.
/// Equivalent to AMPAS
/// `RRTODT.Academy.Rec2020_1000nits_15nits_ST2084`.
pub fn acescg_to_rec2100_pq_1000_nit(acescg: [f32; 3]) -> [f32; 3] {
    output_transform_rec2100_pq(matrix3_mul_vec(ap1_to_ap0(), acescg), 1000.0)
}

/// End-to-end ACEScg → Rec.2100 PQ HDR, 4000 cd/m² mastering display.
/// Equivalent to AMPAS
/// `RRTODT.Academy.Rec2020_4000nits_15nits_ST2084`.
pub fn acescg_to_rec2100_pq_4000_nit(acescg: [f32; 3]) -> [f32; 3] {
    output_transform_rec2100_pq(matrix3_mul_vec(ap1_to_ap0(), acescg), 4000.0)
}

// ─── ODT — DCI-P3 D65 cinema (48 nit) ───────────────────────────────
//
// Standard small-screen digital-cinema deliverable. Same overall
// shape as the SDR Rec.709 ODT shipped earlier — the 48-nit
// cinema target IS where the C9 spline naturally lands, no HDR
// scaling needed — but with two differences:
//
//   * Output primaries are DCI-P3 D65 (not Rec.709). This is the
//     SMPTE EG 432-1 "Display P3" gamut with the cinema's D65
//     white, not DCI's native theatrical-white DCI-P3.
//   * Display OETF is pure gamma 2.6 (the DCI cinema gamma), not
//     BT.1886. We reuse `bt1886_inverse` with γ = 2.6 and
//     L_W = 1, L_B = 0; that formula collapses to `pow(L, 1/γ)`
//     when L_B = 0, which IS the DCI gamma encode.
//
// Dim-surround compensation is also dropped: cinema viewing is by
// convention dark, not dim.

const DCI_DISP_GAMMA: f32 = 2.6;

/// Apply the DCI-P3 D65 cinema (48-nit) Output Device Transform.
///
/// **Input:** OCES (AP0, post-RRT scene-linear).
/// **Output:** DCI gamma-2.6-encoded P3-D65 RGB in `[0, 1]`. Ready
/// to quantise to 12-bit DCDM (the canonical cinema deliverable
/// format) or any P3-D65-tagged container.
pub fn apply_odt_dcip3_d65_48_nits(oces: [f32; 3]) -> [f32; 3] {
    apply_odt_dcip3_d65_48_nits_with(oces, &segmented_spline_c9_fwd)
}

/// [`apply_odt_dcip3_d65_48_nits`] with the tone curve supplied — see
/// [`apply_rrt_with`].
fn apply_odt_dcip3_d65_48_nits_with(
    oces: [f32; 3],
    curve: &dyn Fn(f32) -> f32,
) -> [f32; 3] {
    // OCES → AP1.
    let rgb_pre = matrix3_mul_vec(ap0_to_ap1(), oces);

    // Per-channel C9 tone scale → cinema-range cd/m² (mid 4.8,
    // peak 48 — the 48-nit cinema deliverable's natural range).
    let rgb_post = [curve(rgb_pre[0]), curve(rgb_pre[1]), curve(rgb_pre[2])];

    // Display Y → linear CV (cinema 0.02 → 48).
    let lin_cv = [
        y_to_lin_cv(rgb_post[0], CINEMA_WHITE, CINEMA_BLACK),
        y_to_lin_cv(rgb_post[1], CINEMA_WHITE, CINEMA_BLACK),
        y_to_lin_cv(rgb_post[2], CINEMA_WHITE, CINEMA_BLACK),
    ];

    // No ODT saturation matrix here, and no dim-surround adjustment.
    //
    // Both belong to the *video* ODTs: `ODT_SAT_MAT` exists to
    // compensate for the apparent saturation lift that
    // `darkSurround_to_dimSurround` introduces, so the reference
    // cinema ODT (`ODT.Academy.P3D65_48nits`) applies neither — a
    // cinema is a dark surround, not a dim one. Applying the
    // desaturation without its partner mixed channels toward luma and
    // put positive blue into saturated reds and greens where the
    // display gamut should have clipped them to zero (max error 0.237
    // against OpenColorIO, on saturated AP1 green).

    // AP1 → DCI-P3 D65 (with D60→D65 chromatic adaptation).
    let p3d65_lin = matrix3_mul_vec(crate::primaries::acescg_to_p3d65(), lin_cv);

    // Clamp to display gamut.
    let clamped = [
        p3d65_lin[0].clamp(0.0, 1.0),
        p3d65_lin[1].clamp(0.0, 1.0),
        p3d65_lin[2].clamp(0.0, 1.0),
    ];

    // DCI gamma 2.6 inverse OETF: `pow(L, 1/2.6)`. The shared
    // `bt1886_inverse` reduces to exactly this at L_W=1, L_B=0.
    [
        bt1886_inverse(clamped[0], DCI_DISP_GAMMA, 1.0, 0.0),
        bt1886_inverse(clamped[1], DCI_DISP_GAMMA, 1.0, 0.0),
        bt1886_inverse(clamped[2], DCI_DISP_GAMMA, 1.0, 0.0),
    ]
}

/// End-to-end ACEScg → DCI-P3 D65 cinema. Wraps RRT + ODT for the
/// engine's display path.
pub fn acescg_to_dcip3_d65_48nit(acescg: [f32; 3]) -> [f32; 3] {
    let oces = apply_rrt(matrix3_mul_vec(ap1_to_ap0(), acescg));
    apply_odt_dcip3_d65_48_nits(oces)
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    fn approx_vec(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        approx(a[0], b[0], tol) && approx(a[1], b[1], tol) && approx(a[2], b[2], tol)
    }

    // ─── Helper-level tests pinning specific reference values ─

    #[test]
    fn rgb_2_saturation_neutral_is_zero() {
        // Equal channels → zero saturation, by construction.
        assert!(approx(rgb_2_saturation([0.5, 0.5, 0.5]), 0.0, 1e-6));
    }

    #[test]
    fn rgb_2_saturation_bright_red_is_close_to_one() {
        // Pure red (1, 0, 0): max=1, min=TINY, denom=1 → ~1.
        assert!(approx(rgb_2_saturation([1.0, 0.0, 0.0]), 1.0, 1e-6));
    }

    #[test]
    fn rgb_2_yc_neutral_grey_equals_grey_value() {
        // Neutral input has zero chroma; rgb_2_yc reduces to the
        // arithmetic mean.
        assert!(approx(rgb_2_yc([0.18, 0.18, 0.18]), 0.18, 1e-6));
    }

    #[test]
    fn rgb_2_hue_pure_red_is_zero_pure_green_is_120_pure_blue_is_240() {
        // The hue function is anchored on red at 0°.
        assert!(approx(rgb_2_hue([1.0, 0.0, 0.0]), 0.0, 1e-3));
        assert!(approx(rgb_2_hue([0.0, 1.0, 0.0]), 120.0, 1e-3));
        assert!(approx(rgb_2_hue([0.0, 0.0, 1.0]), 240.0, 1e-3));
    }

    #[test]
    fn center_hue_wraps_through_negative_180() {
        // 350° centred on 0° is "near red on the negative side" =
        // -10°, not 350°.
        assert!(approx(center_hue(350.0, 0.0), -10.0, 1e-6));
        assert!(approx(center_hue(10.0, 0.0), 10.0, 1e-6));
    }

    #[test]
    fn sigmoid_shaper_anchor_points() {
        // Documented endpoints: f(-2) = 0, f(0) = 0.5, f(2) = 1.
        assert!(approx(sigmoid_shaper(-2.0), 0.0, 1e-6));
        assert!(approx(sigmoid_shaper(0.0), 0.5, 1e-6));
        assert!(approx(sigmoid_shaper(2.0), 1.0, 1e-6));
    }

    #[test]
    fn glow_fwd_low_returns_full_gain() {
        // `yc_in` deep in shadows → return the full `glow_gain_in`.
        assert!(approx(glow_fwd(0.01, 0.05, RRT_GLOW_MID), 0.05, 1e-6));
    }

    #[test]
    fn glow_fwd_high_returns_zero() {
        // Above 2 × glow_mid the function drops to 0.
        assert!(approx(glow_fwd(0.20, 0.05, RRT_GLOW_MID), 0.0, 1e-6));
    }

    #[test]
    fn cubic_basis_shaper_peak_at_zero_with_height_1_5() {
        // Standard property of the cubic B-spline kernel: integer-
        // sample-aligned centre evaluates to 1.5 (in our scaled form
        // — the unscaled kernel peak is 1.0; the function applies a
        // 3/2 multiplier).
        assert!(approx(cubic_basis_shaper(0.0, 135.0), 1.0, 1e-4));
    }

    #[test]
    fn cubic_basis_shaper_outside_returns_zero() {
        // Beyond ± width/2 the kernel is identically zero.
        assert!(approx(cubic_basis_shaper(70.0, 135.0), 0.0, 1e-6));
        assert!(approx(cubic_basis_shaper(-70.0, 135.0), 0.0, 1e-6));
    }

    #[test]
    fn segmented_spline_c5_fwd_anchor_18_grey() {
        // Canonical anchor: 0.18 scene-linear maps to OCES 4.8.
        // The mid-point is set exactly at this value, so the spline
        // hits it on the nose.
        assert!(approx(segmented_spline_c5_fwd(0.18), 4.8, 0.01));
    }

    #[test]
    fn segmented_spline_c5_fwd_zero_input_clamps_to_half_min() {
        // The reference spline's input clamp is `HALF_MIN`
        // (= ~6.1e-5), which is *above* `MIN_POINT[0]` (~5.5e-6).
        // A pure-zero input therefore lands in the low spline body,
        // not the low-extrapolation flat — output is ~1.6e-4 (the
        // spline's value at `HALF_MIN`), not 0.0001. The
        // low-extrapolation branch is consequently unreachable for
        // any non-negative input, but we keep the dead code present
        // because the AMPAS reference has it and stripping it would
        // make a future spec change harder to track. This test pins
        // the subtlety so a future "let me clean up the floor"
        // refactor can't silently change behaviour at zero.
        let y = segmented_spline_c5_fwd(0.0);
        assert!(approx(y, 0.000_16, 2e-5),
            "zero input should clamp to HALF_MIN and produce ~1.6e-4, got {y}");
    }

    #[test]
    fn segmented_spline_c5_fwd_above_max_clips_to_10000() {
        // Above `MAX_POINT[0]` (~47000) the curve is a horizontal
        // line at `MAX_POINT[1] = 10000`. A clearly-out-of-range
        // input pins to 10000.
        assert!(approx(segmented_spline_c5_fwd(100_000.0), 10_000.0, 1e-3));
    }

    // ─── End-to-end RRT tests ──────────────────────────────────

    #[test]
    fn apply_rrt_neutral_grey_18_lands_on_oces_4_8() {
        // Pure neutral input: glow + red modifier are no-ops (zero
        // saturation, zero hue weight cancellation), the saturation
        // matrix preserves grey (rows sum to 1 by construction), and
        // the tone curve hits its mid-point.
        let oces = apply_rrt([0.18, 0.18, 0.18]);
        // The AP1↔AP0 round-trip on neutrals is perfect to ~1e-4 in
        // f32 (rows sum to ~1.0 on both matrices); the spline alone
        // contributes the slack.
        assert!(
            approx_vec(oces, [4.8, 4.8, 4.8], 0.02),
            "0.18 grey should map to OCES ≈ 4.8 on each channel; got {oces:?}",
        );
    }

    #[test]
    fn apply_rrt_neutral_black_lands_near_half_min_floor() {
        // The spline's input clamp is `HALF_MIN`, not zero — see
        // `segmented_spline_c5_fwd_zero_input_clamps_to_half_min`.
        // Pure black flows through the saturation matrix unchanged
        // (rows sum to 1), then each channel hits the spline at zero
        // → ~1.6e-4.
        let oces = apply_rrt([0.0, 0.0, 0.0]);
        assert!(
            approx_vec(oces, [0.000_16, 0.000_16, 0.000_16], 2e-5),
            "neutral black should map to ~1.6e-4 (HALF_MIN-clamped spline floor); got {oces:?}",
        );
    }

    #[test]
    fn apply_rrt_neutral_preserves_neutrality() {
        // For *any* neutral input, R == G == B should be preserved
        // exactly (within ~1e-4 f32 noise on the AP0↔AP1 round-trip).
        for v in [0.001, 0.01, 0.05, 0.18, 0.5, 1.0, 5.0] {
            let oces = apply_rrt([v, v, v]);
            let mean = (oces[0] + oces[1] + oces[2]) / 3.0;
            assert!(approx(oces[0], mean, 1e-3),
                "neutral input v={v} should give neutral output; got {oces:?}");
            assert!(approx(oces[1], mean, 1e-3));
            assert!(approx(oces[2], mean, 1e-3));
        }
    }

    #[test]
    fn apply_rrt_monotonic_along_grey_axis() {
        // Brighter input → brighter OCES output along the grey axis.
        // The tone curve is monotonic by design.
        let dim = apply_rrt([0.05, 0.05, 0.05]);
        let mid = apply_rrt([0.18, 0.18, 0.18]);
        let bright = apply_rrt([1.0, 1.0, 1.0]);
        assert!(dim[0] < mid[0], "0.05 → {} should be less than 0.18 → {}", dim[0], mid[0]);
        assert!(mid[0] < bright[0], "0.18 → {} should be less than 1.0 → {}", mid[0], bright[0]);
    }

    #[test]
    fn apply_rrt_high_saturation_red_stays_finite_and_bounded() {
        // Saturated AP0 red is the canonical "RRT sweeteners" stress
        // test — without the red modifier, the tone curve crushes
        // pure red into clipped highlights.
        //
        // Slightly-negative output channels are expected here, *not*
        // a bug: the post-tone-curve AP1 values are mostly in the red
        // corner, and the AP1→AP0 matrix has negative off-diagonals,
        // so the OCES blue channel can dip a few percent below zero.
        // This represents a colour that AP0's basis can't quite hold
        // without a negative channel — the downstream ODT will gamut-
        // map it into the display's reproducible range. We just check
        // that the values are finite, the bright channel is in a
        // reasonable highlight range, and any negatives are tiny.
        let oces = apply_rrt([1.0, 0.0, 0.0]);
        for c in oces {
            assert!(c.is_finite(), "RRT produced non-finite output: {oces:?}");
            assert!(c < 100.0, "RRT highlight unreasonably high: {oces:?}");
            assert!(c > -1.0, "RRT negative excursion too large: {oces:?}");
        }
        // The dominant (R) channel should be much brighter than mid
        // grey (4.8) — saturated red after the tone curve.
        assert!(oces[0] > 4.8, "saturated red R channel should exceed mid-grey OCES; got {}", oces[0]);
    }

    #[test]
    fn apply_rrt_negatives_are_tolerated() {
        // A small negative ACES value can come out of the linearisation
        // step on noisy log-camera footage. The RRT should not blow
        // up — the AP0→AP1 step's `clamp` is what protects the tone
        // curve.
        let oces = apply_rrt([-0.01, 0.18, 0.18]);
        for c in oces {
            assert!(c.is_finite(), "RRT mishandled negative input: {oces:?}");
        }
    }

    // ─── ODT helper-level tests ────────────────────────────────

    #[test]
    fn segmented_spline_c9_fwd_anchor_18_grey_maps_to_4_8_cd_m2() {
        // Mid-point of the C9 spline: OCES 4.8 (from the C5 spline
        // at 0.18 scene-linear) maps to 4.8 cd/m² display luminance
        // by construction. This is the identity-ish anchor the AMPAS
        // designed in.
        let mid_oces = segmented_spline_c5_fwd(0.18); // ≈ 4.8
        let display_y = segmented_spline_c9_fwd(mid_oces);
        assert!(approx(display_y, 4.8, 0.02),
            "C9 mid-point: OCES {mid_oces} should map to ~4.8 cd/m² display, got {display_y}");
    }

    #[test]
    fn segmented_spline_c9_fwd_max_clips_to_48() {
        // Above the spline's maxPoint, output saturates at 48 cd/m².
        let display_y = segmented_spline_c9_fwd(1_000_000.0);
        assert!(approx(display_y, 48.0, 0.5),
            "C9 above-max should plateau at 48, got {display_y}");
    }

    #[test]
    fn segmented_spline_c9_fwd_zero_clips_to_min() {
        // Below the spline's minPoint, output clamps at 0.02 cd/m².
        let display_y = segmented_spline_c9_fwd(0.0);
        assert!(display_y > 0.0 && display_y < 0.05,
            "C9 zero input should land near 0.02 floor, got {display_y}");
    }

    #[test]
    fn y_to_lin_cv_white_point_maps_to_one() {
        assert!(approx(y_to_lin_cv(48.0, 48.0, 0.02), 1.0, 1e-6));
    }

    #[test]
    fn y_to_lin_cv_black_point_maps_to_zero() {
        assert!(approx(y_to_lin_cv(0.02, 48.0, 0.02), 0.0, 1e-6));
    }

    #[test]
    fn bt1886_inverse_simplifies_for_l_b_zero() {
        // With L_B = 0, L_W = 1, the BT.1886 inverse is just
        // pow(L, 1/γ).
        let l = 0.5;
        let v = bt1886_inverse(l, 2.4, 1.0, 0.0);
        let expected = l.powf(1.0 / 2.4);
        assert!(approx(v, expected, 1e-6),
            "BT.1886 inverse at L_B=0 should equal pow(L, 1/γ): got {v}, expected {expected}");
    }

    #[test]
    fn bt1886_inverse_endpoints() {
        assert!(approx(bt1886_inverse(0.0, 2.4, 1.0, 0.0), 0.0, 1e-6));
        assert!(approx(bt1886_inverse(1.0, 2.4, 1.0, 0.0), 1.0, 1e-6));
    }

    #[test]
    fn dark_surround_preserves_chromaticity_on_neutral() {
        // Neutral grey has equal RGB. The xyY transform preserves
        // chromaticity, so the output also has equal RGB (modulo
        // f32 precision noise from going through XYZ).
        let out = dark_surround_to_dim_surround([0.5, 0.5, 0.5]);
        assert!(approx(out[0], out[1], 1e-5));
        assert!(approx(out[1], out[2], 1e-5));
        // Y₀.₉₈¹¹ at Y=0.5 ≈ 0.506 — slight brightening.
        assert!(out[0] > 0.5 && out[0] < 0.51,
            "dim-surround on 0.5 grey should brighten slightly, got {out:?}");
    }

    #[test]
    fn rec709_xyz_round_trip_neutral() {
        let in_v = [0.5, 0.5, 0.5];
        let xyz = matrix3_mul_vec(rec709_to_xyz_d65(), in_v);
        let back = matrix3_mul_vec(xyz_d65_to_rec709(), xyz);
        for k in 0..3 {
            assert!(approx(back[k], in_v[k], 1e-4));
        }
    }

    // ─── End-to-end ODT tests ──────────────────────────────────

    #[test]
    fn apply_odt_neutral_grey_18_yields_visible_grey() {
        // Path: scene 0.18 → RRT → OCES 4.8 (neutral) →
        // C9 spline → 4.8 cd/m² → linCV ≈ (4.8 - 0.02)/(48 - 0.02) ≈ 0.10
        // → AP1→Rec.709 (neutral preserved) → dim-surround (slight
        // brighten to ~0.105) → BT.1886 inverse (γ=2.4) → ~0.396.
        // We just need a clearly-mid grey, not a hand-calc to 4 decimals.
        let scene = [0.18, 0.18, 0.18];
        let oces = apply_rrt(scene);
        let display = apply_odt_rec709_100_nits_dim(oces);
        // Equal channels → equal RGB output.
        assert!(approx(display[0], display[1], 1e-3));
        assert!(approx(display[1], display[2], 1e-3));
        // Should land clearly above black (>0.2) and clearly below
        // peak (<0.7) — that's "mid grey" territory after the BT.1886
        // gamma encode.
        assert!(display[0] > 0.2 && display[0] < 0.7,
            "0.18 scene → ODT should be a mid grey, got {display:?}");
    }

    #[test]
    fn apply_odt_neutral_black_clamps_near_zero() {
        let oces = apply_rrt([0.0, 0.0, 0.0]);
        let display = apply_odt_rec709_100_nits_dim(oces);
        for c in display {
            assert!(c >= 0.0 && c < 0.05,
                "scene black → ODT should be near display black, got {display:?}");
        }
    }

    #[test]
    fn apply_odt_bright_white_clamps_at_one() {
        // 100× scene mid-grey input — well above the spline's
        // maxPoint. Output should saturate at the display peak.
        let oces = apply_rrt([18.0, 18.0, 18.0]);
        let display = apply_odt_rec709_100_nits_dim(oces);
        for c in display {
            assert!(c > 0.95,
                "very bright scene → ODT should hit display white, got {display:?}");
        }
    }

    #[test]
    fn apply_odt_monotonic_along_grey_axis() {
        // Brighter input → brighter display output, end-to-end.
        let dim = apply_odt_rec709_100_nits_dim(apply_rrt([0.05, 0.05, 0.05]));
        let mid = apply_odt_rec709_100_nits_dim(apply_rrt([0.18, 0.18, 0.18]));
        let bright = apply_odt_rec709_100_nits_dim(apply_rrt([1.0, 1.0, 1.0]));
        assert!(dim[0] < mid[0], "0.05 ({}) should be dimmer than 0.18 ({})", dim[0], mid[0]);
        assert!(mid[0] < bright[0], "0.18 ({}) should be dimmer than 1.0 ({})", mid[0], bright[0]);
    }

    #[test]
    fn apply_odt_output_stays_in_unit_range() {
        // Adversarial inputs: very bright, very saturated, slightly
        // negative. The display-encoded output must always be a
        // valid `[0, 1]` RGB triple — anything else is undefined for
        // BT.1886 / display drivers.
        let cases = [
            [1.0, 0.0, 0.0],   // saturated red
            [0.0, 1.0, 0.0],   // saturated green
            [0.0, 0.0, 1.0],   // saturated blue
            [10.0, 10.0, 10.0],// very bright
            [-0.05, 0.5, 0.5], // slight negative
            [0.0, 0.0, 0.0],   // black
        ];
        for input in cases {
            let oces = apply_rrt(input);
            let display = apply_odt_rec709_100_nits_dim(oces);
            for c in display {
                assert!(c.is_finite(),
                    "ODT non-finite for input {input:?} → {display:?}");
                assert!((0.0..=1.0).contains(&c),
                    "ODT out-of-range for input {input:?} → {display:?}");
            }
        }
    }

    #[test]
    fn acescg_to_rec709_sdr_end_to_end_smoke() {
        // The convenience wrapper. Acescg neutral 0.18 should
        // produce the same display value as going through
        // rrt + odt explicitly — it's literally the composition.
        let acescg = [0.18, 0.18, 0.18];
        let direct = acescg_to_rec709_sdr(acescg);
        let oces = apply_rrt(matrix3_mul_vec(ap1_to_ap0(), acescg));
        let manual = apply_odt_rec709_100_nits_dim(oces);
        for k in 0..3 {
            assert!(approx(direct[k], manual[k], 1e-6),
                "wrapper diverges from manual chain: direct={direct:?}, manual={manual:?}");
        }
    }

    // ─── Rec.2100 PQ HDR output-transform tests ────────────────
    //
    // These take ACEScg, not OCES: the ACES 1.1 HDR path is a single
    // stage with no OCES intermediate.

    #[test]
    fn pq_neutral_grey_produces_neutral_pq_output() {
        // Neutral in stays neutral out: the sweeteners' sat matrix,
        // AP1→XYZ, the CAT and XYZ→Rec.2020 all preserve white, and
        // the tone scale is per-channel identical.
        let pq_out = acescg_to_rec2100_pq_1000_nit([0.18, 0.18, 0.18]);
        assert!(approx(pq_out[0], pq_out[1], 1e-4));
        assert!(approx(pq_out[1], pq_out[2], 1e-4));
    }

    #[test]
    fn pq_mid_grey_is_invariant_to_peak_luminance() {
        // Both deliverables carry Y_MID = 15 cd/m², and PQ encodes
        // absolute luminance — so mid-grey must land on the SAME code
        // value regardless of the mastering peak. The peak changes
        // highlight rolloff only.
        //
        // This is the invariant the previous implementation broke: it
        // scaled the whole 48-nit cinema curve by peak_nits / 48, which
        // dragged mid-grey up with the peak.
        let mid = [0.18, 0.18, 0.18];
        let p1k = acescg_to_rec2100_pq_1000_nit(mid);
        let p4k = acescg_to_rec2100_pq_4000_nit(mid);
        for k in 0..3 {
            assert!(approx(p1k[k], p4k[k], 1e-4),
                "mid-grey PQ code drifted with peak luminance: \
                 1000-nit {p1k:?} vs 4000-nit {p4k:?}");
        }
    }

    #[test]
    fn pq_4000_nit_holds_more_highlight_than_1000_nit() {
        // Where the peak *does* show up: a bright highlight well above
        // diffuse white encodes higher on the 4000-nit deliverable,
        // because that curve keeps mapping luminance where the
        // 1000-nit one has already reached its peak.
        let hot = [40.0, 40.0, 40.0];
        let p1k = acescg_to_rec2100_pq_1000_nit(hot);
        let p4k = acescg_to_rec2100_pq_4000_nit(hot);
        assert!(p4k[0] > p1k[0],
            "4000-nit highlight ({}) should exceed 1000-nit ({})",
            p4k[0], p1k[0]);
    }

    #[test]
    fn pq_black_encodes_to_zero() {
        // STRETCH_BLACK = true exists precisely so that black reaches a
        // PQ code value of 0 rather than floating above it. The old
        // implementation left black at ~0.178.
        let black = acescg_to_rec2100_pq_1000_nit([0.0, 0.0, 0.0]);
        for c in black {
            assert!(c < 1e-3, "black should encode at PQ 0, got {black:?}");
        }
    }

    #[test]
    fn pq_monotonic_along_grey_axis() {
        let dim = acescg_to_rec2100_pq_1000_nit([0.02, 0.02, 0.02]);
        let mid = acescg_to_rec2100_pq_1000_nit([0.18, 0.18, 0.18]);
        let bright = acescg_to_rec2100_pq_1000_nit([4.0, 4.0, 4.0]);
        assert!(dim[0] < mid[0],
            "0.02 ACEScg ({}) should be dimmer than 0.18 ({})", dim[0], mid[0]);
        assert!(mid[0] < bright[0],
            "0.18 ACEScg ({}) should be dimmer than 4.0 ({})", mid[0], bright[0]);
    }

    #[test]
    fn pq_output_stays_in_unit_range() {
        // The PQ codepoint is by construction in [0, 1]. Hostile
        // inputs (very bright, saturated, slight negative) must never
        // break this invariant.
        let cases = [
            [0.18, 0.18, 0.18],
            [1000.0, 0.0, 0.0],
            [10000.0, 5000.0, 0.0],
            [-0.05, 0.18, 0.18],
            [0.0, 0.0, 0.0],
        ];
        for acescg in cases {
            for out in [
                acescg_to_rec2100_pq_1000_nit(acescg),
                acescg_to_rec2100_pq_4000_nit(acescg),
            ] {
                for c in out {
                    assert!(c.is_finite(),
                        "PQ output non-finite for ACEScg {acescg:?} → {out:?}");
                    assert!((0.0..=1.0).contains(&c),
                        "PQ codepoint out-of-range for ACEScg {acescg:?} → {out:?}");
                }
            }
        }
    }

    #[test]
    fn ssts_anchors_hit_their_target_luminances() {
        // The tone scale is defined by three anchors. After the
        // expShift pass, ACES 0.18 must render at Y_MID = 15 cd/m²,
        // and the curve must reach its peak at the top.
        for y_max in [1000.0_f32, 4000.0] {
            let p = hdr_pq_params(y_max);
            let at_mid = ssts(0.18, p);
            assert!((at_mid - HDR_PQ_Y_MID).abs() < 0.05,
                "ACES 0.18 should render at {HDR_PQ_Y_MID} cd/m² on the \
                 {y_max}-nit curve, got {at_mid}");
            let at_peak = ssts(p.max.x, p);
            assert!((at_peak - y_max).abs() < y_max * 0.01,
                "curve peak should reach {y_max} cd/m², got {at_peak}");
        }
    }

    #[test]
    fn ssts_is_monotonic_over_its_domain() {
        let p = hdr_pq_params(1000.0);
        let mut prev = 0.0_f32;
        // Sweep 24 stops around mid-grey.
        for i in 0..=240 {
            let stops = -15.0 + (i as f32) * (33.0 / 240.0);
            let x = 0.18 * 2.0_f32.powf(stops);
            let y = ssts(x, p);
            assert!(y.is_finite(), "ssts non-finite at x={x}");
            assert!(y >= prev - 1e-4,
                "ssts not monotonic at {stops} stops (x={x}): {y} < {prev}");
            prev = y;
        }
    }

    #[test]
    fn linear_to_pq_round_trips_with_pq_to_linear() {
        // Sanity on the PQ EOTF pair we shipped — needed by every
        // other PQ test transitively.
        for v in [0.0, 0.01, 0.1, 0.5, 0.9, 1.0] {
            let encoded = crate::camera_log::linear_to_pq(v);
            let decoded = crate::camera_log::pq_to_linear(encoded);
            assert!((v - decoded).abs() < 1e-4,
                "PQ round-trip drift at v={v}: encoded={encoded}, decoded={decoded}");
        }
    }

    // ─── DCI-P3 D65 cinema ODT tests ───────────────────────────

    #[test]
    fn apply_odt_dcip3_neutral_grey_18_yields_visible_grey() {
        let oces = apply_rrt([0.18, 0.18, 0.18]);
        let display = apply_odt_dcip3_d65_48_nits(oces);
        // Equal channels — neutrality preserved through AP1 → P3-D65
        // and the per-channel γ encode.
        assert!(approx(display[0], display[1], 1e-3));
        assert!(approx(display[1], display[2], 1e-3));
        // Mid range. DCI γ 2.6 is steeper than BT.1886 γ 2.4, so the
        // mid-grey codepoint sits a touch lower than the SDR Rec.709
        // ODT's; still clearly visible mid-grey territory.
        assert!(display[0] > 0.15 && display[0] < 0.65,
            "DCI-P3 0.18 mid-grey should be visibly mid-range, got {display:?}");
    }

    #[test]
    fn apply_odt_dcip3_black_clamps_near_zero() {
        let oces = apply_rrt([0.0, 0.0, 0.0]);
        let display = apply_odt_dcip3_d65_48_nits(oces);
        for c in display {
            assert!(c >= 0.0 && c < 0.05,
                "scene black → DCI-P3 should be near display black, got {display:?}");
        }
    }

    #[test]
    fn apply_odt_dcip3_bright_white_clamps_at_one() {
        let oces = apply_rrt([18.0, 18.0, 18.0]);
        let display = apply_odt_dcip3_d65_48_nits(oces);
        for c in display {
            assert!(c > 0.95,
                "very bright scene → DCI-P3 should hit display white, got {display:?}");
        }
    }

    #[test]
    fn apply_odt_dcip3_monotonic_along_grey_axis() {
        let dim = apply_odt_dcip3_d65_48_nits(apply_rrt([0.05, 0.05, 0.05]));
        let mid = apply_odt_dcip3_d65_48_nits(apply_rrt([0.18, 0.18, 0.18]));
        let bright = apply_odt_dcip3_d65_48_nits(apply_rrt([1.0, 1.0, 1.0]));
        assert!(dim[0] < mid[0]);
        assert!(mid[0] < bright[0]);
    }

    #[test]
    fn apply_odt_dcip3_output_stays_in_unit_range() {
        let cases = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [10.0, 10.0, 10.0],
            [-0.05, 0.5, 0.5],
            [0.0, 0.0, 0.0],
        ];
        for input in cases {
            let oces = apply_rrt(input);
            let display = apply_odt_dcip3_d65_48_nits(oces);
            for c in display {
                assert!(c.is_finite() && (0.0..=1.0).contains(&c),
                    "DCI-P3 ODT out-of-range for input {input:?} → {display:?}");
            }
        }
    }

    #[test]
    fn acescg_to_dcip3_wrapper_composition() {
        let acescg = [0.18, 0.18, 0.18];
        let direct = acescg_to_dcip3_d65_48nit(acescg);
        let oces = apply_rrt(matrix3_mul_vec(ap1_to_ap0(), acescg));
        let manual = apply_odt_dcip3_d65_48_nits(oces);
        for k in 0..3 {
            assert!(approx(direct[k], manual[k], 1e-6),
                "DCI-P3 wrapper drifts: direct={direct:?}, manual={manual:?}");
        }
    }

    /// DCI-P3 D65 vs Rec.709 ODTs on saturated AP1 red — both
    /// produce DIFFERENT outputs (would be a regression if they
    /// matched), but exact "which one looks more saturated" depends
    /// on the saturation-matrix + gamut-clamp interaction; that
    /// comparison belongs in S9 colorimetric parity, not here.
    #[test]
    fn apply_odt_dcip3_diverges_from_rec709_on_saturated_input() {
        let oces = apply_rrt([1.0, 0.0, 0.0]);
        let p3 = apply_odt_dcip3_d65_48_nits(oces);
        let r709 = apply_odt_rec709_100_nits_dim(oces);
        // Channel-wise difference must be measurable — different
        // primaries + different OETF γ guarantee divergence on any
        // non-grey input.
        let drift = (p3[0] - r709[0]).abs() + (p3[1] - r709[1]).abs() + (p3[2] - r709[2]).abs();
        assert!(drift > 0.05,
            "DCI-P3 and Rec.709 ODTs should diverge on saturated red; \
             P3={p3:?} Rec709={r709:?} (total drift {drift})");
    }
}

#[cfg(test)]
mod gpu_odt_data_tests {
    use super::*;

    /// Each variant's blob is the length its layout says, and the header
    /// describes it. A GPU host cannot see `gpu_layout` — `klipp-effects`
    /// does not depend on this crate — so it reads the curve sizes from
    /// the header and indexes off fixed offsets.
    #[test]
    fn every_variant_lays_out_as_the_shader_expects() {
        for odt in [
            GpuOdt::Rec709Sdr,
            GpuOdt::DciP3D65_48,
            GpuOdt::Rec2100Pq1000,
            GpuOdt::Rec2100Pq4000,
        ] {
            let n = 256;
            let d = aces_odt_gpu_data(odt, klipp_core::WorkingColorspace::AcesCG, n);
            let hdr = odt.hdr_peak().is_some();
            assert_eq!(
                d.len(),
                gpu_layout::CURVES + if hdr { 0 } else { n * 2 },
                "{odt:?}: blob length",
            );
            assert_eq!(d[gpu_layout::VARIANT], if hdr { 1.0 } else { 0.0 }, "{odt:?}: variant");
            // The curve descriptors: sizes the shader reads, zero when
            // the variant ships no curves.
            let want_n = if hdr { 0.0 } else { n as f32 };
            assert_eq!(d[2], want_n, "{odt:?}: rrt entries");
            assert_eq!(d[5], want_n, "{odt:?}: odt entries");
            // Only the video ODT compensates for a dim surround.
            assert_eq!(
                d[gpu_layout::DIM_SURROUND],
                if odt == GpuOdt::Rec709Sdr { 1.0 } else { 0.0 },
                "{odt:?}: dim surround",
            );
            assert_eq!(
                d[gpu_layout::Y_WHITE],
                odt.hdr_peak().unwrap_or(CINEMA_WHITE),
                "{odt:?}: linCV white",
            );
            // The SSTS block is present exactly for the HDR variants: a
            // resolved curve has a rising mid anchor, a zeroed one does
            // not.
            let mid_y = d[gpu_layout::SSTS + 4];
            assert_eq!(mid_y > 0.0, hdr, "{odt:?}: SSTS block presence");
            if hdr {
                // The mid anchor is 4.8 cd/m² for both peaks — `Y_MID`
                // (15) is what the two-pass `expShift` derivation aims
                // mid-grey at, not the anchor itself, which is why PQ
                // mid-grey lands on the same code value at either peak.
                assert_eq!(mid_y, 4.8, "{odt:?}: the SSTS mid anchor");
                assert_eq!(
                    d[gpu_layout::SSTS + 7],
                    odt.hdr_peak().unwrap(),
                    "{odt:?}: the SSTS peak anchor",
                );
                // And the peak is the only thing that differs between
                // the two, which is the claim `GpuOdt` is built on.
                let other = if odt == GpuOdt::Rec2100Pq1000 {
                    GpuOdt::Rec2100Pq4000
                } else {
                    GpuOdt::Rec2100Pq1000
                };
                let o = aces_odt_gpu_data(other, klipp_core::WorkingColorspace::AcesCG, n);
                assert_ne!(
                    d[gpu_layout::SSTS..gpu_layout::CURVES],
                    o[gpu_layout::SSTS..gpu_layout::CURVES],
                    "{odt:?}: the two peaks resolve to the same SSTS",
                );
                assert_eq!(
                    d[gpu_layout::MATRICES..gpu_layout::SSTS],
                    o[gpu_layout::MATRICES..gpu_layout::SSTS],
                    "{odt:?}: the two peaks should share every matrix",
                );
            }
        }
    }

    /// The wire tags are stable and total.
    #[test]
    fn gpu_odt_tags_round_trip() {
        for odt in [
            GpuOdt::Rec709Sdr,
            GpuOdt::DciP3D65_48,
            GpuOdt::Rec2100Pq1000,
            GpuOdt::Rec2100Pq4000,
        ] {
            assert_eq!(GpuOdt::from_tag(odt.tag()), Some(odt));
        }
        assert_eq!(GpuOdt::from_tag(4), None, "an unknown tag must not resolve");
    }

    /// A non-AcesCG project is walked into AP1 before the ODT, and that
    /// happens in its own matrix slot.
    ///
    /// It cannot be folded into the ap1→ap0 slot the chain starts with,
    /// because the chain applies that slot a *second* time to the
    /// post-curve values — where the project's basis has no business.
    #[test]
    fn a_foreign_basis_gets_its_own_matrix_slot() {
        use klipp_core::WorkingColorspace as W;
        let n = 64;
        let acescg = aces_odt_gpu_data(GpuOdt::Rec709Sdr, W::AcesCG, n);
        let rec709 = aces_odt_gpu_data(GpuOdt::Rec709Sdr, W::Rec709Linear, n);
        let slot = |d: &[f32], i: usize| {
            d[gpu_layout::MATRICES + i * 9..gpu_layout::MATRICES + i * 9 + 9].to_vec()
        };
        // An AcesCG project needs no walk.
        assert_eq!(slot(&acescg, gpu_layout::SOURCE_TO_AP1), identity3().concat());
        // A BT.709 one does, and only that slot moves.
        assert_ne!(slot(&rec709, gpu_layout::SOURCE_TO_AP1), identity3().concat());
        for i in 0..gpu_layout::SOURCE_TO_AP1 {
            assert_eq!(slot(&acescg, i), slot(&rec709, i), "matrix slot {i} should not move");
        }
        // And the reference agrees: the same working values render
        // differently once the basis is declared, because they mean a
        // different colour.
        let c = [0.6f32, 0.2, 0.1];
        let curves = OdtCurves::new(1024);
        let a = acescg_to_display_tabled(c, GpuOdt::Rec709Sdr, W::AcesCG, &curves);
        let b = acescg_to_display_tabled(c, GpuOdt::Rec709Sdr, W::Rec709Linear, &curves);
        assert!(
            (0..3).any(|k| (a[k] - b[k]).abs() > 0.02),
            "declaring the basis changed nothing: {a:?} vs {b:?}",
        );
    }

    /// The tabled reference is the real transform, per deliverable.
    ///
    /// `acescg_to_display_tabled` is what a shader parity test measures
    /// against, so it has to be the deliverable and not a fifth
    /// transform of its own. 1024 entries a curve, the size
    /// `table_size_against_accuracy` measures at 0.041 code values.
    #[test]
    fn the_tabled_reference_is_the_deliverable() {
        let exact: [(GpuOdt, fn([f32; 3]) -> [f32; 3]); 4] = [
            (GpuOdt::Rec709Sdr, acescg_to_rec709_sdr),
            (GpuOdt::DciP3D65_48, acescg_to_dcip3_d65_48nit),
            (GpuOdt::Rec2100Pq1000, acescg_to_rec2100_pq_1000_nit),
            (GpuOdt::Rec2100Pq4000, acescg_to_rec2100_pq_4000_nit),
        ];
        let mut st = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            (st >> 11) as f32 / (1u64 << 53) as f32
        };
        let cases: Vec<[f32; 3]> = (0..4096)
            .map(|_| {
                let base = 2.0f32.powf(next() * 18.0 - 12.0);
                [
                    base * (0.2 + 1.6 * next()),
                    base * (0.2 + 1.6 * next()),
                    base * (0.2 + 1.6 * next()),
                ]
            })
            .collect();
        let curves = OdtCurves::new(1024);
        for (odt, f) in exact {
            let mut worst = 0.0f32;
            for c in &cases {
                let (a, b) = (
                    acescg_to_display_tabled(
                        *c,
                        odt,
                        klipp_core::WorkingColorspace::AcesCG,
                        &curves,
                    ),
                    f(*c),
                );
                for i in 0..3 {
                    worst = worst.max((a[i] - b[i]).abs());
                }
            }
            let codes = worst * 1023.0;
            eprintln!("{odt:?}: tabled vs exact worst {codes:.4} of 1023");
            assert!(codes < 0.5, "{odt:?}: the tabled reference is {codes:.3} code values out");
        }
    }
}

#[cfg(test)]
mod tone_curve_table_tests {
    use super::*;

    /// Pseudo-random AcesCG values across eighteen stops and the full
    /// saturation range — the same shape of input that defeated a 3D
    /// LUT, so the comparison is like for like.
    fn samples(n: usize) -> Vec<[f32; 3]> {
        let mut st = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            (st >> 11) as f32 / (1u64 << 53) as f32
        };
        (0..n)
            .map(|_| {
                let base = 2.0f32.powf(next() * 18.0 - 12.0);
                [
                    base * (0.2 + 1.6 * next()),
                    base * (0.2 + 1.6 * next()),
                    base * (0.2 + 1.6 * next()),
                ]
            })
            .collect()
    }

    fn worst_levels(n_entries: usize) -> f32 {
        let rrt = rrt_tone_curve_table(n_entries);
        let odt = odt_rec709_tone_curve_table(n_entries);
        samples(20_000)
            .iter()
            .flat_map(|s| {
                let want = acescg_to_rec709_sdr(*s);
                let got = acescg_to_rec709_sdr_tabled(*s, &rrt, &odt);
                (0..3).map(move |c| (got[c] - want[c]).abs() * 255.0).collect::<Vec<_>>()
            })
            .fold(0.0f32, f32::max)
    }

    /// Tabling the two tone curves costs almost nothing, where tabling
    /// the whole three-dimensional transform cost up to 51 code values.
    /// That difference is the whole reason for this shape.
    #[test]
    fn the_tabled_chain_matches_the_spline_chain() {
        let worst = worst_levels(1024);
        eprintln!("1024-entry tables: worst {worst:.3} levels of 255");
        assert!(worst < 0.5, "tabled chain is out by {worst:.3} levels");
    }

    /// What the table size buys, so the choice is a measurement and not
    /// a guess.
    #[test]
    fn table_size_against_accuracy() {
        for n in [64usize, 256, 1024, 4096] {
            eprintln!("{n:>5} entries: worst {:.3} levels", worst_levels(n));
        }
    }

    /// Outside the knots both curves are constant, so the clamped
    /// lookup is exact there rather than approximate — which is what
    /// lets the table cover only the interesting range.
    #[test]
    fn the_curves_are_flat_outside_the_table() {
        let rrt = rrt_tone_curve_table(1024);
        let below = SPLINE_MIN_POINT[0] * 0.5;
        let far_below = SPLINE_MIN_POINT[0] * 0.001;
        assert!(
            (segmented_spline_c5_fwd(below) - segmented_spline_c5_fwd(far_below)).abs() < 1e-9,
            "the RRT curve is not flat below its first knot",
        );
        assert!(
            (rrt.eval(below) - segmented_spline_c5_fwd(below)).abs() < 1e-6,
            "the clamped lookup disagrees below the first knot",
        );
        let above = SPLINE_MAX_POINT[0] * 2.0;
        assert!(
            (rrt.eval(above) - segmented_spline_c5_fwd(above)).abs() < 1e-6,
            "the clamped lookup disagrees above the last knot",
        );
    }
}

// ─── Everything the GPU needs, as one buffer ────────────────────────

/// Offsets into [`aces_rec709_gpu_data`], in floats.
///
/// The shader reads the same layout. Keeping them here and the shader's
/// copy adjacent in the source is the best that can be done without
/// generating the WGSL; a mismatch is not subtle — the parity test goes
/// from a fraction of a level to visibly wrong.
pub mod gpu_layout {
    /// `rrt_log_x_min, rrt_log_x_max, rrt_n, odt_log_x_min, odt_log_x_max,
    /// odt_n, variant, disp_gamma, dim_surround, y_white, y_black, pad`.
    ///
    /// `variant` is 0 for the two-stage SDR tail (RRT then the C9 ODT)
    /// and 1 for the single-stage HDR one (the SSTS). `disp_gamma` and
    /// `dim_surround` belong to the SDR tail; `y_white` and `y_black` are
    /// the luminances `Y_2_linCV` normalises through, which differ per
    /// deliverable.
    pub const HEADER: usize = 0;
    pub const HEADER_LEN: usize = 12;
    pub const VARIANT: usize = 6;
    pub const DISP_GAMMA: usize = 7;
    pub const DIM_SURROUND: usize = 8;
    pub const Y_WHITE: usize = 9;
    pub const Y_BLACK: usize = 10;
    /// Twelve 3x3 matrices, row-major. The first seven are the SDR
    /// chain's, in the order it applies them — ap1→ap0, ap0→ap1, rrt_sat,
    /// odt_sat, ap1→display, display→xyz, xyz→display — then four for the
    /// HDR chain: ap1→xyz, xyz→bt2020, bt2020→xyz, D60→D65. Slots a
    /// variant does not use carry an identity.
    ///
    /// The twelfth is the working space's basis into AP1, applied once at
    /// the very start. It cannot be folded into the ap1→ap0 slot, which
    /// the chain applies a second time to the post-curve values.
    pub const MATRICES: usize = HEADER_LEN;
    pub const SOURCE_TO_AP1: usize = 11;
    pub const MATRIX_COUNT: usize = 12;
    pub const MATRICES_LEN: usize = MATRIX_COUNT * 9;
    /// The SSTS, HDR only: min/mid/max as `(x, y, slope)`, then six
    /// low-side and six high-side spline coefficients.
    pub const SSTS: usize = MATRICES + MATRICES_LEN;
    pub const SSTS_LEN: usize = 21;
    /// The two tone curves, RRT first. Empty for the HDR variant, whose
    /// single stage is the SSTS above.
    pub const CURVES: usize = SSTS + SSTS_LEN;
}

/// Which output transform a GPU blob describes.
///
/// One per deliverable `OdtChoice` offers on the AcesCG basis. They are
/// four configurations of two chains rather than four transforms: the
/// SDR pair differ only in their output matrix, their display gamma and
/// whether the dim-surround compensation runs, and the HDR pair only in
/// their peak luminance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GpuOdt {
    /// [`acescg_to_rec709_sdr`].
    Rec709Sdr,
    /// [`acescg_to_dcip3_d65_48nit`].
    DciP3D65_48,
    /// [`acescg_to_rec2100_pq_1000_nit`].
    Rec2100Pq1000,
    /// [`acescg_to_rec2100_pq_4000_nit`].
    Rec2100Pq4000,
}

impl GpuOdt {
    /// Wire-stable tag, so a target struct can name an ODT without
    /// `klipp-effects` having to know what one is. Discriminants are
    /// pinned: `gpu_odt_tags_round_trip` holds them.
    pub fn tag(self) -> u32 {
        match self {
            Self::Rec709Sdr => 0,
            Self::DciP3D65_48 => 1,
            Self::Rec2100Pq1000 => 2,
            Self::Rec2100Pq4000 => 3,
        }
    }

    /// The inverse of [`Self::tag`]; `None` for an unknown tag.
    pub fn from_tag(tag: u32) -> Option<Self> {
        Some(match tag {
            0 => Self::Rec709Sdr,
            1 => Self::DciP3D65_48,
            2 => Self::Rec2100Pq1000,
            3 => Self::Rec2100Pq4000,
            _ => return None,
        })
    }

    /// The peak the HDR deliverables are mastered to, cd/m².
    fn hdr_peak(self) -> Option<f32> {
        match self {
            Self::Rec2100Pq1000 => Some(1000.0),
            Self::Rec2100Pq4000 => Some(4000.0),
            _ => None,
        }
    }
}

/// The identity, for a matrix slot this variant does not use.
fn identity3() -> [[f32; 3]; 3] {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}

/// The matrices, constants and curves for one output transform, flat,
/// ready to upload — [`aces_rec709_gpu_data`] generalised to the four
/// deliverables.
///
/// `curve_entries` sizes the two SDR tone curves and is ignored for the
/// HDR variants, which ship the SSTS's twenty-one resolved numbers
/// instead of a sampling of it. That difference is deliberate: the SSTS
/// feeds `Y_2_linCV`, which subtracts `Y_MIN` from values that *are*
/// `Y_MIN` near black, and a table's own log round-trip is all that
/// survives the cancellation — about two 10-bit code values of lifted
/// black once PQ magnifies it, which is what `STRETCH_BLACK` exists to
/// prevent. The spline is thirty lines of arithmetic; the part that
/// could drift is `init_TsParams`, and that is what gets shipped.
pub fn aces_odt_gpu_data(
    odt: GpuOdt,
    working: klipp_core::WorkingColorspace,
    curve_entries: usize,
) -> Vec<f32> {
    let hdr = odt.hdr_peak();
    // The HDR variant's single stage is the SSTS, so it carries no curves.
    let (rrt, odt_curve) = match hdr {
        Some(_) => (None, None),
        None => (
            Some(rrt_tone_curve_table(curve_entries)),
            Some(odt_rec709_tone_curve_table(curve_entries)),
        ),
    };
    let desc = |t: &Option<ToneCurveTable>| match t {
        Some(t) => [t.log_x_min, t.log_x_max, t.log_y.len() as f32],
        None => [0.0, 0.0, 0.0],
    };

    let mut out = Vec::with_capacity(gpu_layout::CURVES + curve_entries * 2);
    out.extend_from_slice(&desc(&rrt));
    out.extend_from_slice(&desc(&odt_curve));
    out.push(if hdr.is_some() { 1.0 } else { 0.0 });
    out.push(match odt {
        GpuOdt::DciP3D65_48 => DCI_DISP_GAMMA,
        _ => SDR_DISP_GAMMA,
    });
    // Only the video ODT compensates for a dim surround. The cinema one
    // does not, and neither does its saturation partner — applying one
    // without the other put positive blue into saturated reds.
    out.push(if odt == GpuOdt::Rec709Sdr { 1.0 } else { 0.0 });
    let (y_white, y_black) = match hdr {
        Some(peak) => (peak, HDR_PQ_Y_MIN),
        None => (CINEMA_WHITE, CINEMA_BLACK),
    };
    out.push(y_white);
    out.push(y_black);
    out.push(0.0);
    debug_assert_eq!(out.len(), gpu_layout::MATRICES);

    let sdr = hdr.is_none();
    for m in [
        ap1_to_ap0(),
        ap0_to_ap1(),
        rrt_sat_matrix(),
        // The cinema ODT applies no saturation matrix.
        if odt == GpuOdt::Rec709Sdr { odt_sat_matrix() } else { identity3() },
        match odt {
            GpuOdt::DciP3D65_48 => crate::primaries::acescg_to_p3d65(),
            _ if sdr => crate::primaries::acescg_to_bt709(),
            _ => identity3(),
        },
        if sdr { rec709_to_xyz_d65() } else { identity3() },
        if sdr { xyz_d65_to_rec709() } else { identity3() },
        if sdr { identity3() } else { ap1_to_xyz() },
        if sdr { identity3() } else { xyz_to_rec2020() },
        if sdr { identity3() } else { rec2020_to_xyz() },
        if sdr { identity3() } else { d60_to_d65_cat() },
        // The project's basis into AP1, which the ODTs are defined on.
        // An AcesCG project needs nothing; any other walks there first,
        // or the ODT sees foreign values and shifts hue.
        crate::primaries::working_space_conversion_matrix(
            working,
            klipp_core::WorkingColorspace::AcesCG,
        )
        .unwrap_or_else(identity3),
    ] {
        for row in m {
            out.extend_from_slice(&row);
        }
    }
    debug_assert_eq!(out.len(), gpu_layout::SSTS);

    match hdr {
        Some(peak) => {
            let p = hdr_pq_params(peak);
            for pt in [p.min, p.mid, p.max] {
                out.extend_from_slice(&[pt.x, pt.y, pt.slope]);
            }
            out.extend_from_slice(&p.coefs_low);
            out.extend_from_slice(&p.coefs_high);
        }
        None => out.extend_from_slice(&[0.0; gpu_layout::SSTS_LEN]),
    }
    debug_assert_eq!(out.len(), gpu_layout::CURVES);

    if let (Some(rrt), Some(odt_curve)) = (rrt, odt_curve) {
        out.extend_from_slice(&rrt.log_y);
        out.extend_from_slice(&odt_curve.log_y);
    }
    out
}

/// The CPU answer for `odt`, computed the way the GPU computes it — the
/// two SDR curves read from tables of `curve_entries`, everything else
/// exact. The reference a shader parity test measures against, so that
/// "the table is coarse" and "the shader is wrong" stay separable.
pub fn acescg_to_display_tabled(
    working_rgb: [f32; 3],
    odt: GpuOdt,
    working: klipp_core::WorkingColorspace,
    curves: &OdtCurves,
) -> [f32; 3] {
    let acescg = match crate::primaries::working_space_conversion_matrix(
        working,
        klipp_core::WorkingColorspace::AcesCG,
    ) {
        Some(m) => matrix3_mul_vec(m, working_rgb),
        None => working_rgb,
    };
    match odt {
        // The HDR pair read no table: their SSTS is evaluated, on the
        // GPU as here, so these *are* the exact transforms.
        GpuOdt::Rec2100Pq1000 => acescg_to_rec2100_pq_1000_nit(acescg),
        GpuOdt::Rec2100Pq4000 => acescg_to_rec2100_pq_4000_nit(acescg),
        GpuOdt::Rec709Sdr => acescg_to_rec709_sdr_tabled(acescg, &curves.rrt, &curves.c9),
        GpuOdt::DciP3D65_48 => {
            let oces =
                apply_rrt_with(matrix3_mul_vec(ap1_to_ap0(), acescg), &|x| curves.rrt.eval(x));
            apply_odt_dcip3_d65_48_nits_with(oces, &|x| curves.c9.eval(x))
        }
    }
}

/// The two tone curves an SDR ODT reads, baked once.
///
/// Passed in rather than built inside [`acescg_to_display_tabled`],
/// which is a per-pixel call: baking them there cost a 4K frame
/// twenty-five *seconds*, since each pixel rebuilt two 1024-entry
/// splines.
pub struct OdtCurves {
    pub rrt: ToneCurveTable,
    pub c9: ToneCurveTable,
}

impl OdtCurves {
    /// `entries` a curve — 1024 is the size `table_size_against_accuracy`
    /// measures at 0.041 code values.
    pub fn new(entries: usize) -> Self {
        Self { rrt: rrt_tone_curve_table(entries), c9: odt_rec709_tone_curve_table(entries) }
    }
}

/// The matrices and tone curves for the ACES Rec.709 SDR transform,
/// flat, ready to upload.
///
/// The matrices are computed here rather than written into the shader
/// because they are *derived* — `rrt_sat_matrix` from a saturation
/// factor and the AP1 luma weights, the primaries conversions from
/// chromaticities. Transcribing derived numbers is how two
/// implementations drift; the scalar constants that remain in the
/// shader are single literals the parity test would catch instantly.
pub fn aces_rec709_gpu_data(curve_entries: usize) -> Vec<f32> {
    aces_odt_gpu_data(GpuOdt::Rec709Sdr, klipp_core::WorkingColorspace::AcesCG, curve_entries)
}
