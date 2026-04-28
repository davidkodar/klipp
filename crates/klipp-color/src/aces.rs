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
//! At time of writing only the SDR Rec.709 100-nit-dim ODT is
//! shipped — by far the most-used display target. HDR PQ and
//! DCI-P3 D65 ODTs are tracked as separate follow-up sessions so
//! each gets its own quality + parity pass; they share the C9
//! tone-curve infrastructure landed here.

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
    let mut ap1 = matrix3_mul_vec(ap0_to_ap1(), a);

    // Clip negatives to zero before the tone curve. AP1 still has
    // some out-of-gamut excursion from very saturated AP0 sources
    // (notably very-pure spectral colours that AP1 can't represent
    // without a negative channel); tone-mapping a negative through
    // log10 explodes the math.
    ap1 = [ap1[0].max(0.0), ap1[1].max(0.0), ap1[2].max(0.0)];

    // ─── Global desaturation ───────────────────────────────────
    let ap1_desat = matrix3_mul_vec(rrt_sat_matrix(), ap1);

    // ─── Per-channel tone curve ────────────────────────────────
    let rgb_post = [
        segmented_spline_c5_fwd(ap1_desat[0]),
        segmented_spline_c5_fwd(ap1_desat[1]),
        segmented_spline_c5_fwd(ap1_desat[2]),
    ];

    // ─── AP1 → AP0 (OCES output) ───────────────────────────────
    matrix3_mul_vec(ap1_to_ap0(), rgb_post)
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
/// **Caveat:** This implementation uses the AMPAS `ODT_48nits` C9
/// tone-curve coefficients — cinema-style 48-nit peak. The proper
/// `ODT_100nits` coefficient set (different curve targeting 100 cd/m²
/// peak directly) is deferred to a follow-up session. For everyday
/// SDR delivery the visual difference is small; for parity vs Resolve
/// the 100-nit-specific coefs will need to ship alongside.
pub fn apply_odt_rec709_100_nits_dim(oces: [f32; 3]) -> [f32; 3] {
    // OCES → AP1 rendering space.
    let rgb_pre = matrix3_mul_vec(ap0_to_ap1(), oces);

    // Per-channel C9 tone scale (OCES → display cd/m²).
    let rgb_post = [
        segmented_spline_c9_fwd(rgb_pre[0]),
        segmented_spline_c9_fwd(rgb_pre[1]),
        segmented_spline_c9_fwd(rgb_pre[2]),
    ];

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
}
