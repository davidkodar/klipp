//! Temperature and tint as a white balance: a 3×3 matrix on linear light.
//!
//! The sliders used to add a fixed amount to red and blue (temperature)
//! or green (tint). An offset is not a colour balance: it tints blacks,
//! barely moves highlights, and on scene-linear footage — camera log, PQ,
//! HLG, where the picture spans values far above 1 — it did a tenth of
//! what it did on SDR. A white balance scales the channels instead, so it
//! acts the same at every exposure: black stays black, and a stop brighter
//! is balanced exactly as a stop darker.
//!
//! The balance is a chromatic adaptation, as a camera's is. The slider
//! names the light the scene is assumed to be lit by, relative to the
//! working space's white: `temperature` moves it along the Planckian
//! locus, one unit per mired (warmer picture = bluer assumed light), and
//! `tint` off the locus, towards green for a more magenta picture (#2769:
//! a positive tint is magenta, as a positive temperature is warmer). Bradford
//! then adapts that white to the working white. The matrix is normalised
//! so a neutral keeps its luminance: the sliders change colour, not
//! exposure.
//!
//! The scale was chosen so mid-grey moves about as far as it did under
//! the old offsets (Lab Δb ≈ 5 at +25, 10 at +50): SDR grades keep their
//! look in the mid-tones, and log / HDR ones get the full effect.

use klipp_core::WorkingColorspace;

use crate::primaries::{Primaries, PRIMARIES_ACES_AP1, PRIMARIES_BT2020, PRIMARIES_BT709};

/// Mireds of assumed-light shift per temperature unit.
pub const MIRED_PER_TEMPERATURE: f64 = 1.0;

/// Duv (distance from the Planckian locus, CIE 1960 uv) per tint unit.
pub const DUV_PER_TINT: f64 = 0.000_28;

/// The locus approximation's range, K.
const MIN_CCT: f64 = 1_667.0;
const MAX_CCT: f64 = 25_000.0;

type M3 = [[f64; 3]; 3];

/// The white balance for these slider values in `ws`, as a matrix on
/// linear working-space RGB; `None` when both are zero (identity).
pub fn white_balance_matrix(temperature: f32, tint: f32, ws: WorkingColorspace) -> Option<[[f32; 3]; 3]> {
    if temperature == 0.0 && tint == 0.0 {
        return None;
    }
    let primaries = working_primaries(ws);
    let to_xyz = rgb_to_xyz(&primaries);
    let from_xyz = inverse(&to_xyz);
    // The working white's own temperature is only the origin: source and
    // destination both sit on the locus approximation, so zero is exact
    // identity whatever the white's true offset from the locus.
    let white = (primaries.white.x as f64, primaries.white.y as f64);
    let base_mired = 1e6 / cct_of(white);
    let mired = (base_mired - temperature as f64 * MIRED_PER_TEMPERATURE).clamp(1e6 / MAX_CCT, 1e6 / MIN_CCT);
    let src = off_locus(1e6 / mired, (tint as f64) * DUV_PER_TINT);
    let dst = off_locus(1e6 / base_mired, 0.0);
    let m = mul(&from_xyz, &mul(&bradford(src, dst), &to_xyz));
    // Keep a neutral's luminance.
    let one = [m[0][0] + m[0][1] + m[0][2], m[1][0] + m[1][1] + m[1][2], m[2][0] + m[2][1] + m[2][2]];
    let y = to_xyz[1][0] * one[0] + to_xyz[1][1] * one[1] + to_xyz[1][2] * one[2];
    let white_y = to_xyz[1][0] + to_xyz[1][1] + to_xyz[1][2];
    let k = white_y / y;
    Some(m.map(|row| row.map(|v| (v * k) as f32)))
}

/// Apply a white-balance matrix to linear RGBA, in place. Alpha untouched.
pub fn apply_white_balance(rgba: &mut [f32], m: &[[f32; 3]; 3]) {
    use rayon::prelude::*;
    rgba.par_chunks_exact_mut(4).for_each(|px| {
        let (r, g, b) = (px[0], px[1], px[2]);
        px[0] = m[0][0] * r + m[0][1] * g + m[0][2] * b;
        px[1] = m[1][0] * r + m[1][1] * g + m[1][2] * b;
        px[2] = m[2][0] * r + m[2][1] * g + m[2][2] * b;
    });
}

/// The working space's primaries and white. BT.2020 shares BT.709's D65
/// white, so a project in either has the same base CCT; only the gamut
/// the balance is expressed in differs.
fn working_primaries(ws: WorkingColorspace) -> Primaries {
    match ws {
        WorkingColorspace::AcesCG => PRIMARIES_ACES_AP1,
        WorkingColorspace::Rec709Linear => PRIMARIES_BT709,
        WorkingColorspace::Bt2020Linear => PRIMARIES_BT2020,
    }
}

/// The Planckian locus, xy, 1667-25000 K (Kim et al. 2002).
fn planck_xy(t: f64) -> (f64, f64) {
    let t = t.clamp(MIN_CCT, MAX_CCT);
    let (t2, t3) = (t * t, t * t * t);
    let x = if t <= 4000.0 {
        -0.266_123_9e9 / t3 - 0.234_358_9e6 / t2 + 0.877_695_6e3 / t + 0.179_910
    } else {
        -3.025_846_9e9 / t3 + 2.107_037_9e6 / t2 + 0.222_634_7e3 / t + 0.240_390
    };
    let (x2, x3) = (x * x, x * x * x);
    let y = if t <= 2222.0 {
        -1.106_381_4 * x3 - 1.348_110_20 * x2 + 2.185_558_32 * x - 0.202_196_83
    } else if t <= 4000.0 {
        -0.954_947_6 * x3 - 1.374_185_93 * x2 + 2.091_370_15 * x - 0.167_488_67
    } else {
        3.081_758_0 * x3 - 5.873_386_70 * x2 + 3.751_129_97 * x - 0.370_014_83
    };
    (x, y)
}

fn xy_to_uv((x, y): (f64, f64)) -> (f64, f64) {
    let d = -2.0 * x + 12.0 * y + 3.0;
    (4.0 * x / d, 6.0 * y / d)
}

fn uv_to_xy((u, v): (f64, f64)) -> (f64, f64) {
    let d = 2.0 * u - 8.0 * v + 4.0;
    (3.0 * u / d, 2.0 * v / d)
}

/// The point `duv` from the locus at `t`, along the normal in CIE 1960 uv;
/// positive is above the locus, towards green.
fn off_locus(t: f64, duv: f64) -> (f64, f64) {
    let (u, v) = xy_to_uv(planck_xy(t));
    if duv == 0.0 {
        return uv_to_xy((u, v));
    }
    let (u1, v1) = xy_to_uv(planck_xy(t * 1.001));
    let (du, dv) = (u1 - u, v1 - v);
    let n = (du * du + dv * dv).sqrt();
    let (mut nu, mut nv) = (-dv / n, du / n);
    if nv < 0.0 {
        (nu, nv) = (-nu, -nv);
    }
    uv_to_xy((u + nu * duv, v + nv * duv))
}

/// The correlated colour temperature of a white near the locus: the
/// locus point closest to it in uv, by golden-section search on mireds.
fn cct_of(xy: (f64, f64)) -> f64 {
    let (u0, v0) = xy_to_uv(xy);
    let dist = |mired: f64| {
        let (u, v) = xy_to_uv(planck_xy(1e6 / mired));
        (u - u0).powi(2) + (v - v0).powi(2)
    };
    let (mut a, mut b) = (1e6 / MAX_CCT, 1e6 / MIN_CCT);
    let g = 0.618_033_988_75;
    for _ in 0..100 {
        let (c, d) = (b - g * (b - a), a + g * (b - a));
        if dist(c) < dist(d) { b = d } else { a = c }
    }
    1e6 / ((a + b) / 2.0)
}

/// Bradford chromatic adaptation from white `src` to white `dst`, on XYZ.
fn bradford(src: (f64, f64), dst: (f64, f64)) -> M3 {
    const B: M3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let xyz = |(x, y): (f64, f64)| [x / y, 1.0, (1.0 - x - y) / y];
    let (s, d) = (mat_vec(&B, xyz(src)), mat_vec(&B, xyz(dst)));
    let scale = [[d[0] / s[0], 0.0, 0.0], [0.0, d[1] / s[1], 0.0], [0.0, 0.0, d[2] / s[2]]];
    mul(&inverse(&B), &mul(&scale, &B))
}

/// RGB → XYZ for a set of primaries and white (the standard construction).
fn rgb_to_xyz(p: &Primaries) -> M3 {
    let xyz = |x: f32, y: f32| [x as f64 / y as f64, 1.0, (1.0 - x as f64 - y as f64) / y as f64];
    let (r, g, b) = (xyz(p.red.x, p.red.y), xyz(p.green.x, p.green.y), xyz(p.blue.x, p.blue.y));
    let prim = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = mat_vec(&inverse(&prim), xyz(p.white.x, p.white.y));
    prim.map(|row| [row[0] * s[0], row[1] * s[1], row[2] * s[2]])
}

fn mat_vec(m: &M3, v: [f64; 3]) -> [f64; 3] {
    m.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
}

fn mul(a: &M3, b: &M3) -> M3 {
    let mut out = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn inverse(m: &M3) -> M3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    [
        [(e * i - f * h) / det, (c * h - b * i) / det, (b * f - c * e) / det],
        [(f * g - d * i) / det, (a * i - c * g) / det, (c * d - a * f) / det],
        [(d * h - e * g) / det, (b * g - a * h) / det, (a * e - b * d) / det],
    ]
}

/// CIE Lab a and b of linear BT.709 RGB, D65: the axes the colour-match
/// and auto-white-balance solvers work in.
fn lab_ab(c: [f32; 3]) -> (f32, f32) {
    let [r, g, b] = c.map(|v| v as f64);
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;
    let f = |t: f64| if t > 0.008_856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
    ((500.0 * (fx - fy)) as f32, (200.0 * (fy - fz)) as f32)
}

/// The temperature and tint whose balance moves `rgb` (linear BT.709) by
/// `(da, db)` in Lab a / b: Newton's method on the two slider values,
/// clamped to their ±100 range.
///
/// The sliders are a chromatic adaptation, so there is no closed form for
/// "which slider values produce this colour shift" — solving it is what
/// keeps the auto white balance and the shot match honest whatever the
/// scale of the sliders.
pub fn white_balance_for_lab_shift(rgb: [f32; 3], da: f32, db: f32, ws: WorkingColorspace) -> (f32, f32) {
    let ab = |t: f32, g: f32| match white_balance_matrix(t, g, ws) {
        Some(m) => lab_ab(m.map(|row| row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2])),
        None => lab_ab(rgb),
    };
    let (a0, b0) = ab(0.0, 0.0);
    let (want_a, want_b) = (a0 + da, b0 + db);
    let (mut t, mut g) = (0.0f32, 0.0f32);
    for _ in 0..8 {
        let (a, b) = ab(t, g);
        let (ea, eb) = (want_a - a, want_b - b);
        if ea.abs() < 0.01 && eb.abs() < 0.01 {
            break;
        }
        let h = 1.0;
        let (at, bt) = ab(t + h, g);
        let (ag, bg) = ab(t, g + h);
        let (dadt, dbdt, dadg, dbdg) = ((at - a) / h, (bt - b) / h, (ag - a) / h, (bg - b) / h);
        let det = dadt * dbdg - dadg * dbdt;
        if det.abs() < 1e-6 {
            break;
        }
        t = (t + (ea * dbdg - eb * dadg) / det).clamp(-100.0, 100.0);
        g = (g + (eb * dadt - ea * dbdt) / det).clamp(-100.0, 100.0);
    }
    (t, g)
}

/// The temperature and tint that make `rgb` (linear BT.709) neutral —
/// the grey-world auto white balance.
pub fn white_balance_to_neutralise(rgb: [f32; 3], ws: WorkingColorspace) -> (f32, f32) {
    let (a, b) = lab_ab(rgb);
    white_balance_for_lab_shift(rgb, -a, -b, ws)
}

#[cfg(test)]
mod tests {
    use super::*;
    use WorkingColorspace::*;

    fn apply(m: &[[f32; 3]; 3], c: [f32; 3]) -> [f32; 3] {
        m.map(|row| row[0] * c[0] + row[1] * c[1] + row[2] * c[2])
    }

    /// CIE Lab of linear BT.709 RGB, D65.
    fn lab(c: [f32; 3]) -> [f32; 3] {
        let [r, g, b] = c.map(|v| v as f64);
        let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
        let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
        let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;
        let f = |t: f64| if t > 0.008_856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
        let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
        [(116.0 * fy - 16.0) as f32, (500.0 * (fx - fy)) as f32, (200.0 * (fy - fz)) as f32]
    }

    /// `apply_white_balance` multiplies by the matrix it is given —
    /// checked on a *colour*, because the matrix is not diagonal.
    ///
    /// The function had no test at all. Everything else in this module
    /// checks `white_balance_matrix`, the construction; this checks the
    /// application, which is where a cross-channel mix-up lives. On a
    /// grey it would be untestable: `r == g == b` makes only the row
    /// sums observable, and permuting the columns leaves those alone.
    #[test]
    fn apply_white_balance_multiplies_by_the_whole_matrix() {
        let m = white_balance_matrix(40.0, -15.0, Rec709Linear).unwrap();
        // Leaning on one channel at a time, so each column is visible.
        for colour in [[0.8f32, 0.1, 0.1], [0.1, 0.8, 0.1], [0.1, 0.1, 0.8]] {
            let mut px = vec![colour[0], colour[1], colour[2], 0.42];
            apply_white_balance(&mut px, &m);
            let want = apply(&m, colour);
            for c in 0..3 {
                assert!(
                    (px[c] - want[c]).abs() < 1e-6,
                    "channel {c} of {colour:?}: got {}, want {}", px[c], want[c],
                );
            }
            assert_eq!(px[3], 0.42, "alpha must be left alone");
        }

        // And the colours above can actually see a column permutation,
        // which is the bug this is for. Stated so the fixture cannot
        // quietly become a grey later.
        let permuted = m.map(|r| [r[2], r[0], r[1]]);
        let colour = [0.8f32, 0.1, 0.1];
        let gap = (0..3)
            .map(|i| (apply(&m, colour)[i] - apply(&permuted, colour)[i]).abs())
            .fold(0.0f32, f32::max);
        assert!(gap > 0.1, "the fixture cannot see the matrix's columns (gap {gap:e})");
    }

    #[test]
    fn zero_is_identity() {
        assert!(white_balance_matrix(0.0, 0.0, Rec709Linear).is_none());
        // And the smallest move is a small change, not a jump.
        let m = white_balance_matrix(0.01, 0.0, Rec709Linear).unwrap();
        for (i, row) in m.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let want = if i == j { 1.0 } else { 0.0 };
                assert!((v - want).abs() < 1e-3, "{m:?}");
            }
        }
    }

    /// The point of the change: a white balance scales, so it does the
    /// same at every exposure — black stays black, and the balance of a
    /// grey four stops over is that of grey, only brighter.
    #[test]
    fn it_scales_with_exposure() {
        let m = white_balance_matrix(40.0, -20.0, Rec709Linear).unwrap();
        assert_eq!(apply(&m, [0.0; 3]), [0.0; 3]);
        let grey = apply(&m, [0.18; 3]);
        let bright = apply(&m, [0.18 * 16.0; 3]);
        for c in 0..3 {
            assert!((bright[c] / grey[c] - 16.0).abs() < 1e-3, "{grey:?} vs {bright:?}");
        }
    }

    /// Warm is warmer, cool cooler, tint greens or magentas, and a neutral
    /// keeps its luminance throughout.
    #[test]
    fn directions_and_luminance() {
        let y = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        for (t, tint) in [(50.0, 0.0), (-50.0, 0.0), (0.0, 50.0), (0.0, -50.0), (80.0, 60.0)] {
            let m = white_balance_matrix(t, tint, Rec709Linear).unwrap();
            let c = apply(&m, [0.18; 3]);
            assert!((y(c) - 0.18).abs() < 1e-5, "({t}, {tint}) moved luminance: {c:?}");
            let [_, a, b] = lab(c);
            if t > 0.0 { assert!(b > 5.0, "warm should be yellower: b {b}"); }
            if t < 0.0 { assert!(b < -5.0, "cool should be bluer: b {b}"); }
            if tint > 0.0 { assert!(a > 3.0, "tint + should be more magenta: a {a}"); }
            if tint < 0.0 { assert!(a < -3.0, "tint - should be greener: a {a}"); }
        }
    }

    /// The calibration: at mid-grey the new balance moves as far as the old
    /// offsets did, so SDR grades keep their look. The old offset's +
    /// was green; tint's green is - now (#2769), so the two are compared
    /// at opposite signs, the same move.
    #[test]
    fn mid_grey_moves_as_far_as_before() {
        let old = |t: f32, tint: f32| lab([0.18 + t / 1000.0, 0.18 + tint / 1000.0, 0.18 - t / 1000.0]);
        for s in [25.0f32, 50.0] {
            let new_t = lab(apply(&white_balance_matrix(s, 0.0, Rec709Linear).unwrap(), [0.18; 3]));
            let (o, n) = (old(s, 0.0)[2], new_t[2]);
            assert!((n - o).abs() / o < 0.15, "temperature {s}: Lab b {n} vs the old {o}");
            let new_g = lab(apply(&white_balance_matrix(0.0, -s, Rec709Linear).unwrap(), [0.18; 3]));
            let (o, n) = (old(0.0, s)[1], new_g[1]);
            assert!((n - o).abs() / o.abs() < 0.15, "tint {s}: Lab a {n} vs the old {o}");
        }
    }

    /// The same slider gives the same picture in an ACEScg project: its
    /// white is D60, but the balance is relative to the working white.
    #[test]
    fn acescg_balances_like_rec709() {
        let to_ap1 = crate::primaries::bt709_to_acescg();
        let to_709 = crate::primaries::acescg_to_bt709();
        let c709 = [0.3f32, 0.2, 0.1];
        let in_709 = apply(&white_balance_matrix(40.0, 20.0, Rec709Linear).unwrap(), c709);
        let via_ap1 = apply(&to_709, apply(&white_balance_matrix(40.0, 20.0, AcesCG).unwrap(), apply(&to_ap1, c709)));
        for c in 0..3 {
            assert!((in_709[c] - via_ap1[c]).abs() < 0.01, "{in_709:?} vs {via_ap1:?}");
        }
    }

    /// Auto white balance: the values it returns really do neutralise the
    /// colour it was given.
    #[test]
    fn neutralising_a_cast_leaves_a_neutral() {
        for cast in [[0.18, 0.19, 0.26], [0.30, 0.20, 0.12], [0.5, 0.5, 0.5]] {
            let (t, tint) = white_balance_to_neutralise(cast, Rec709Linear);
            let corrected = match white_balance_matrix(t, tint, Rec709Linear) {
                Some(m) => apply(&m, cast),
                None => cast,
            };
            let [_, a, b] = lab(corrected);
            eprintln!("{cast:?} -> temperature {t:.1} tint {tint:.1} -> Lab a {a:.2} b {b:.2}");
            assert!(a.abs() < 1.0 && b.abs() < 1.0, "{cast:?} left a cast: a {a}, b {b}");
        }
    }

    #[test]
    fn working_whites_are_near_the_locus() {
        let d65 = cct_of((0.3127, 0.3290));
        assert!((d65 - 6504.0).abs() < 60.0, "D65 at {d65} K");
        let d60 = cct_of((0.32168, 0.33767));
        assert!((d60 - 6000.0).abs() < 60.0, "ACES white at {d60} K");
    }
}

#[cfg(test)]
mod matrix_reality_tests {
    use super::*;

    /// The knob is mireds of assumed light, and the app's own slider
    /// runs to 50: at 25 a neutral must move about five Lab units. An
    /// op that writes tenths here is writing nothing, which is what the
    /// grade op did until 2026-09-20.
    #[test]
    fn twenty_five_mireds_moves_a_neutral_about_five_lab_units() {
        let ws = WorkingColorspace::default();
        let m = white_balance_matrix(25.0, 0.0, ws).expect("a matrix");
        let grey = [0.18f32, 0.18, 0.18];
        let out = [
            m[0][0] * grey[0] + m[0][1] * grey[1] + m[0][2] * grey[2],
            m[1][0] * grey[0] + m[1][1] * grey[1] + m[1][2] * grey[2],
            m[2][0] * grey[0] + m[2][1] * grey[1] + m[2][2] * grey[2],
        ];
        assert!(out[0] > grey[0] && out[2] < grey[2], "warmer light means a redder picture: {out:?}");
        // Five Lab units, give or take, on mid grey.
        let lab = |c: [f32; 3]| {
            let f = |t: f32| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
            let x = 0.4124564 * c[0] + 0.3575761 * c[1] + 0.1804375 * c[2];
            let y = 0.2126729 * c[0] + 0.7151522 * c[1] + 0.0721750 * c[2];
            let z = 0.0193339 * c[0] + 0.1191920 * c[1] + 0.9503041 * c[2];
            (200.0 * (f(y) - f(z / 1.08883)), 500.0 * (f(x / 0.95047) - f(y)))
        };
        let (b0, _) = lab(grey);
        let (b1, _) = lab(out);
        assert!((3.0..8.0).contains(&(b1 - b0)), "b* moved {:.1} at 25 mireds", b1 - b0);
        assert!(white_balance_matrix(0.2, 0.0, ws).is_some_and(|m| (m[0][0] - 1.0).abs() < 0.001), "a fifth of a mired is nothing, and that is the point of the range");
    }
}
