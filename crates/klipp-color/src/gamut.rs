//! Gamut mapping from BT.2020 into BT.709, in the manner of ITU-R
//! BT.2407: hue is kept, and chroma is compressed into the narrower
//! gamut instead of being clipped against it.
//!
//! The alternative, and what Klipp did before this module, is to convert
//! with the matrix and clamp whatever comes out negative. That flattens
//! every colour outside BT.709 onto the gamut hull: a saturated red and a
//! *more* saturated one land on the same pixel, so the picture loses the
//! distinction rather than rendering it narrower. On an SDR monitor of a
//! BT.2020 project that is the difference between judging a grade and
//! guessing at it.
//!
//! # The method
//!
//! Per pixel, in CIELAB (D65, the white both gamuts share). Hue is left
//! alone — BT.2407's requirement, and the reason the result reads as the
//! same colour rendered less saturated rather than as a different
//! colour. Within the plane of that one hue:
//!
//! 1. Chroma within 80% of BT.709's reach passes through untouched, so
//!    colour well inside the gamut never moves.
//! 2. Beyond it, the span out to the *source* gamut's boundary is
//!    compressed linearly onto the span out to BT.709's. The most
//!    saturated colour BT.2020 can express lands exactly on the BT.709
//!    hull, and everything below it stays ordered.
//! 3. Lightness is held, except where holding it would cost the colour
//!    nearly all its chroma, and then only by as much as the chroma
//!    needs and never more than a few units — see
//!    [`MAX_LIGHTNESS_TRADED`].
//!
//! That third point is the one departure from a plain reading of
//! BT.2407 Annex 2, and it is there for the light saturated colours.
//! BT.2020 yellow sits at lightness 97.7 and BT.709 holds a chroma of
//! about 13 that high — the gamut solid has narrowed almost to the
//! white point — while its own yellow, one and a half units darker,
//! carries 97. Holding lightness exactly therefore renders every
//! saturated yellow as milky cream. Coming down those few units
//! instead keeps a chroma of 88, and a yellow that still reads as
//! yellow. Below the cusp, which is every dark saturated colour and
//! every neutral in the picture, lightness does not move at all.
//!
//! How far each gamut reaches is the expensive part — the chroma a gamut
//! holds at a lightness and hue has no closed form — so each is found
//! once by bisection over a 96 × 192 grid of lightness and hue and read
//! back with bilinear interpolation. Both tables build in about ten
//! milliseconds on first use and are shared from there on.
//!
//! Wherever the source gamut holds no more than BT.709 does — the
//! neutral axis and its surroundings, blacks, whites and greys — there
//! is nothing to compress and the colour is returned untouched, so a
//! wide-gamut project's black and white levels come back bit-exact.
//!
//! Both tables come from [`crate::primaries`]' own chromaticities, which
//! carry the specs' four-decimal white points: derived that way, BT.709
//! blue's Z is 0.950532 against the published 0.950304. That 2.3e-4 is
//! the spec's rounding rather than an error in the derivation, it is far
//! under the tables' own grid error, and it cancels where it counts
//! because source and target hulls are built the same way — but it is
//! worth knowing before measuring these hulls against a reference and
//! finding a fourth-decimal disagreement.
//!
//! # Scope
//!
//! The input is linear light already converted into the BT.709 basis,
//! which is where an out-of-gamut colour shows up as a negative channel.
//! A pixel brighter than white is scaled into range before the mapping
//! and back afterwards, so this changes colour and not exposure; the
//! display tone map still owns brightness.
//!
//! Only a BT.2020 project pays for any of this, and only where the
//! picture has colour in it: a pixel whose chroma is under the least
//! BT.709 holds at its lightness leaves after one matrix and one
//! CIELAB conversion. On a 4K frame the display encode costs about
//! 15 ms more than a BT.709 project's for ordinary colour, and 32 ms
//! more for a frame of wholly saturated colour.

/// Where the compression starts, as a fraction of the target gamut's
/// reach. BT.2407 Annex 2 uses 80%.
const KNEE: f32 = 0.8;

/// Chroma below which a colour is treated as neutral: a half unit in
/// CIELAB, well under the roughly one unit a viewer can see.
const CHROMA_FLOOR: f32 = 0.5;

/// The most lightness the mapping will spend to keep a colour's chroma,
/// in CIELAB units. A colour lighter than its hue's cusp can reach more
/// chroma by coming down, and near white — above the yellow cusp, say —
/// coming down is the only way to stay the colour it was. But lightness
/// is what a grade is judged on, so the mapping spends only what the
/// chroma needs and never more than this: five units is a shift a viewer
/// reads as the colour's own shade rather than as a change of exposure.
const MAX_LIGHTNESS_TRADED: f32 = 5.0;

/// Grid of the reach table: lightness × hue.
const H_STEPS: usize = 192;
const L_STEPS: usize = 96;

/// The chroma a gamut holds at each lightness and hue.
struct Reach {
    /// `[l * H_STEPS + h]`, lightness 0..100, hue 0..360.
    level: Vec<f32>,
    /// The least chroma in each lightness row, over every hue: a lower
    /// bound on [`Reach::at`] that costs no `atan2` to read.
    row_min: Vec<f32>,
}

/// How far a gamut reaches along one line out from the neutral axis:
/// the *first* crossing of its boundary, given a test for whether it
/// holds the point at a distance.
///
/// First rather than furthest, and the distinction is not academic —
/// neither gamut is star-shaped in CIELAB. At the yellow hue a straight
/// line out from the axis at lightness 96 leaves BT.709 at chroma 41,
/// where R runs past 1.0, and touches it again only at the corner,
/// chroma 97. Measuring to the corner would let the mapping put colours
/// in the gap between, which the display then clamps — a saturated red
/// measured that way lands 0.4 outside the cube, and clamping 0.4 away
/// is exactly the flattening this module exists to stop. Measuring to
/// the first crossing keeps the whole segment from the axis outwards
/// inside the gamut, so anything the compression lands on is a colour
/// the display can show.
///
/// What it gives up is the chroma in those gaps, which is why the
/// mapping spends a little lightness to get at them instead — see
/// `MAX_LIGHTNESS_TRADED`.
fn reach(holds: &dyn Fn(f32) -> bool) -> f32 {
    const LIMIT: f32 = 240.0;
    // The axis itself is inside every gamut sharing this white, so the
    // low end always holds and the bisection is well-founded.
    if holds(LIMIT) {
        return LIMIT;
    }
    let (mut lo, mut hi) = (0.0f32, LIMIT);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if holds(mid) { lo = mid } else { hi = mid }
    }
    lo
}

impl Reach {
    /// Bisect for the chroma the gamut holds at every grid point.
    /// `inside` says whether the gamut holds a Lab colour.
    ///
    /// Deliberately serial, although it is the one embarrassingly
    /// parallel thing in this module. The display transform calls the
    /// mapping from inside a rayon parallel loop, so the build runs on a
    /// rayon worker; if it wanted workers of its own while its siblings
    /// sat blocked on the same `OnceLock`, the pool could wait on
    /// itself — which it did, hanging the crate's own test run. Ten
    /// milliseconds once is not worth that risk.
    fn build(inside: &dyn Fn([f32; 3]) -> bool) -> Self {
        let mut level = vec![0.0f32; L_STEPS * H_STEPS];
        for li in 0..L_STEPS {
            let l = level_of(li);
            for hi in 0..H_STEPS {
                let h = hi as f32 / H_STEPS as f32 * 360.0;
                level[li * H_STEPS + hi] = reach(&|c| inside(lch_to_lab(l, c, h)));
            }
        }
        let row_min = level
            .chunks_exact(H_STEPS)
            .map(|row| row.iter().copied().fold(f32::INFINITY, f32::min))
            .collect();
        Self { level, row_min }
    }

    /// The least chroma this gamut holds anywhere near lightness `l`.
    /// Chroma under it is inside the gamut whatever the hue, which is
    /// what lets the mapping leave without finding the hue at all.
    fn least_at(&self, l: f32) -> f32 {
        let lf = (l.clamp(0.0, 100.0) / 100.0) * (L_STEPS - 1) as f32;
        let l0 = (lf as usize).min(L_STEPS - 2);
        self.row_min[l0].min(self.row_min[l0 + 1])
    }

    /// The chroma held at `(l, h)`, bilinear between grid points.
    fn at(&self, l: f32, h: f32) -> f32 {
        let lf = (l.clamp(0.0, 100.0) / 100.0) * (L_STEPS - 1) as f32;
        let l0 = (lf as usize).min(L_STEPS - 2);
        let lt = lf - l0 as f32;
        let (h0, h1, ht) = hue_cell(h);
        let read = |r: usize| {
            let a = self.level[r * H_STEPS + h0];
            let b = self.level[r * H_STEPS + h1];
            a + (b - a) * ht
        };
        let (a, b) = (read(l0), read(l0 + 1));
        a + (b - a) * lt
    }
}

/// The lightness of grid row `li`.
fn level_of(li: usize) -> f32 {
    li as f32 / (L_STEPS - 1) as f32 * 100.0
}

/// The two grid hues either side of `h`, and how far between them it sits.
fn hue_cell(h: f32) -> (usize, usize, f32) {
    let hf = h.rem_euclid(360.0) / 360.0 * H_STEPS as f32;
    let h0 = (hf as usize) % H_STEPS;
    (h0, (h0 + 1) % H_STEPS, hf - (hf as usize) as f32)
}

/// Whether `p` holds a Lab colour, by the sign of its linear RGB.
fn holds_lab(p: &crate::primaries::Primaries) -> impl Fn([f32; 3]) -> bool {
    let m = crate::primaries::xyz_to_rgb_matrix(p);
    move |lab| {
        let rgb = crate::primaries::matrix3_mul_vec(m, lab_to_xyz(lab));
        rgb.iter().all(|v| *v >= -1e-4 && *v <= 1.0 + 1e-4)
    }
}

/// Where each grid hue holds the most chroma — the gamut's cusp, as
/// `(lightness, chroma)`. The lightness is the anchor the compression
/// aims at above the cusp; the chroma says how much colour is at stake
/// at that hue, which is how the mapping decides whether spending
/// lightness is worth it.
fn cusps(target: &Reach) -> Vec<(f32, f32)> {
    (0..H_STEPS)
        .map(|hi| {
            let mut best = (50.0f32, 0.0f32);
            for li in 0..L_STEPS {
                let c = target.level[li * H_STEPS + hi];
                if c > best.1 {
                    best = (level_of(li), c);
                }
            }
            best
        })
        .collect()
}

/// The cusp at `h`, linear between grid hues.
fn cusp_at(cusps: &[(f32, f32)], h: f32) -> (f32, f32) {
    let (h0, h1, ht) = hue_cell(h);
    let (a, b) = (cusps[h0], cusps[h1]);
    (a.0 + (b.0 - a.0) * ht, a.1 + (b.1 - a.1) * ht)
}

/// BT.709's cusps, and both gamuts' reach measured from them.
fn reaches() -> &'static (Vec<(f32, f32)>, Reach, Reach) {
    static TABLES: std::sync::OnceLock<(Vec<(f32, f32)>, Reach, Reach)> = std::sync::OnceLock::new();
    TABLES.get_or_init(|| {
        let target = Reach::build(&holds_lab(&crate::primaries::PRIMARIES_BT709));
        let source = Reach::build(&holds_lab(&crate::primaries::PRIMARIES_BT2020));
        let cusps = cusps(&target);
        (cusps, target, source)
    })
}

/// The BT.709 basis change, built once. Inverting a 3×3 per pixel — what
/// calling [`crate::primaries::xyz_to_rgb_matrix`] in the loop costs — is
/// the kind of thing that ends up dominating everything else here.
fn bt709_matrices() -> &'static ([[f32; 3]; 3], [[f32; 3]; 3]) {
    static M: std::sync::OnceLock<([[f32; 3]; 3], [[f32; 3]; 3])> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        let to_xyz = crate::primaries::rgb_to_xyz_matrix(&crate::primaries::PRIMARIES_BT709);
        (to_xyz, crate::primaries::invert3(to_xyz))
    })
}

/// Bring a colour converted from BT.2020 into the BT.709 gamut, keeping
/// its hue. Colour well inside BT.709 is returned unchanged, a neutral
/// exactly so; a pixel brighter than white keeps its brightness.
pub fn compress_bt2020_to_bt709(rgb: [f32; 3]) -> [f32; 3] {
    // The clamp is a backstop for the reach tables' own interpolation
    // error, nothing more: `the_mapping_lands_inside_the_gamut` measures
    // what it actually has to absorb.
    compress_unclamped(rgb).map(|v| v.max(0.0))
}

/// [`compress_bt2020_to_bt709`] without the closing clamp, so a test can
/// see how far outside BT.709 the mapping ever lands.
fn compress_unclamped(rgb: [f32; 3]) -> [f32; 3] {
    // Brightness is the tone map's business: scale a highlight into range
    // for the mapping and put it back afterwards.
    let scale = rgb[0].max(rgb[1]).max(rgb[2]).max(1.0);
    let n = if scale > 1.0 { rgb.map(|v| v / scale) } else { rgb };

    let [l, a, b] = rgb_to_lab(n);
    let c = (a * a + b * b).sqrt();
    if c <= CHROMA_FLOOR {
        // Nothing a viewer could call colour: no hue to keep, no chroma
        // to compress, and leaving here rather than through the tables
        // means a grey comes back bit-exact instead of carrying the
        // rounding of a trip through CIELAB.
        return rgb;
    }
    let (cusps, target, source) = reaches();
    // The first test is against the least chroma BT.709 holds at this
    // lightness whatever the hue: passing it means the pixel cannot be
    // outside its own knee, and the hue never has to be found.
    if c <= target.least_at(l) * KNEE {
        return rgb;
    }
    let h = b.atan2(a).to_degrees();
    let held = target.at(l, h);
    // Inside the knee — most of a picture — nothing moves, at the
    // lightness it arrived with. The knee starts *inside* the gamut on
    // purpose: compressing only what falls outside makes the mapping
    // jump at the boundary, and a gradient crossing it steps instead of
    // rolling off.
    if c <= held * KNEE {
        return rgb;
    }
    let (cusp_l, cusp_c) = cusp_at(cusps, h);
    // Past what this lightness can hold, coming down towards the hue's
    // cusp buys chroma — and above the yellow cusp, where BT.709 holds
    // almost none, it is the only way for the colour to stay yellow at
    // all. Spend the least lightness that covers the chroma, and never
    // more than `MAX_LIGHTNESS_TRADED`; a colour at or below its cusp,
    // which is every dark saturated one and every neutral, keeps its
    // lightness exactly.
    let lightness = if c > held && l > cusp_l {
        let want = c.min(cusp_c);
        let floor = cusp_l.max(l - MAX_LIGHTNESS_TRADED);
        // Chroma grows as lightness falls towards the cusp, so the
        // lightness wanted is the highest one that holds `want`: `floor`
        // does or nothing in range does, and `l` does not.
        let mut lo = floor;
        let mut hi = l;
        for _ in 0..8 {
            let mid = 0.5 * (lo + hi);
            if target.at(mid, h) >= want { lo = mid } else { hi = mid }
        }
        lo
    } else {
        l
    };
    let t = target.at(lightness, h);
    let knee = t * KNEE;
    let s = source.at(lightness, h);
    if s <= t * (1.0 + 1e-4) && lightness == l {
        // The source gamut holds no more here than BT.709 does — the
        // neutral axis and its surroundings, where there is nothing to
        // compress. Leaving early keeps those colours bit-exact rather
        // than nudged by the table's own interpolation error.
        return rgb;
    }
    let mapped_c = if s > knee {
        (knee + (c - knee) * ((t - knee) / (s - knee))).min(t)
    } else {
        c.min(t)
    };
    // Scaling `a` and `b` together scales chroma and leaves the hue angle
    // exactly where it was — no trigonometry, and no drift from a round
    // trip through it.
    let k = mapped_c.max(0.0) / c;
    let mapped =
        crate::primaries::matrix3_mul_vec(bt709_matrices().1, lab_to_xyz([lightness, a * k, b * k]));
    mapped.map(|v| v * scale)
}

// ─── CIELAB, D65 ──────────────────────────────────────────────────────

/// D65 white in XYZ, the white point BT.709 and BT.2020 share.
const WHITE: [f32; 3] = [0.950_47, 1.0, 1.088_83];

fn rgb_to_lab(rgb: [f32; 3]) -> [f32; 3] {
    xyz_to_lab(crate::primaries::matrix3_mul_vec(bt709_matrices().0, rgb))
}

fn xyz_to_lab(xyz: [f32; 3]) -> [f32; 3] {
    let f = |t: f32| {
        if t > 0.008_856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 }
    };
    let (fx, fy, fz) = (f(xyz[0] / WHITE[0]), f(xyz[1] / WHITE[1]), f(xyz[2] / WHITE[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_to_xyz(lab: [f32; 3]) -> [f32; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    let g = |t: f32| {
        let cube = t * t * t;
        if cube > 0.008_856 { cube } else { (t - 16.0 / 116.0) / 7.787 }
    };
    [g(fx) * WHITE[0], g(fy) * WHITE[1], g(fz) * WHITE[2]]
}

fn lch_to_lab(l: f32, c: f32, h: f32) -> [f32; 3] {
    let r = h.to_radians();
    [l, c * r.cos(), c * r.sin()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primaries::{matrix3_mul_vec, PRIMARIES_BT2020, PRIMARIES_BT709};

    /// A BT.2020 colour in the BT.709 basis — what the display transform
    /// hands the mapping.
    fn as_709(rgb2020: [f32; 3]) -> [f32; 3] {
        let m = crate::primaries::matrix3_mul(
            crate::primaries::xyz_to_rgb_matrix(&PRIMARIES_BT709),
            crate::primaries::rgb_to_xyz_matrix(&PRIMARIES_BT2020),
        );
        matrix3_mul_vec(m, rgb2020)
    }

    /// A Lab colour in the BT.709 basis, whatever that takes — the
    /// inverse of what the mapping reads.
    fn from_lab(l: f32, c: f32, h: f32) -> [f32; 3] {
        matrix3_mul_vec(bt709_matrices().1, lab_to_xyz(lch_to_lab(l, c, h)))
    }

    /// A colour as lightness, chroma and hue — how the mapping's own
    /// promises are phrased, and so how the tests check them. The
    /// mapping itself never needs the polar form: it scales `a` and `b`
    /// instead.
    fn lch(rgb: [f32; 3]) -> (f32, f32, f32) {
        let [l, a, b] = rgb_to_lab(rgb);
        (l, (a * a + b * b).sqrt(), b.atan2(a).to_degrees())
    }

    #[test]
    fn a_neutral_is_untouched() {
        for v in [0.0f32, 0.18, 0.5, 1.0, 4.0] {
            let grey = [v; 3];
            assert_eq!(compress_bt2020_to_bt709(grey), grey);
        }
    }

    /// Colour inside the knee — most of a picture — passes through
    /// untouched. Colour beyond it rolls off, which is what makes the
    /// mapping continuous at the gamut boundary rather than stepped, and
    /// the roll-off is gentle for colour already inside BT.709.
    #[test]
    fn colour_inside_the_knee_is_untouched() {
        for rgb in [[0.4f32, 0.3, 0.25], [0.2, 0.5, 0.35], [0.3, 0.3, 0.6], [0.9, 0.88, 0.85]] {
            assert_eq!(compress_bt2020_to_bt709(rgb), rgb, "{rgb:?} moved");
        }
        // Near the hull: moved, but barely. A bright tint counts as near
        // the hull however gentle it looks — the gamut solid pinches to a
        // point at white, so at lightness 98 BT.709 holds a chroma of
        // about four, and [1.0, 0.95, 0.9] carries five.
        for (rgb, tolerance) in [([0.95f32, 0.05, 0.02], 0.12f32), ([2.0, 1.9, 1.8], 0.02)] {
            let mapped = compress_bt2020_to_bt709(rgb);
            let shift = (0..3).map(|i| (mapped[i] - rgb[i]).abs()).fold(0.0f32, f32::max);
            assert!(shift > 0.0 && shift < tolerance, "{rgb:?} → {mapped:?}");
        }
    }

    /// Every BT.2020 primary and secondary lands inside BT.709 at the hue
    /// it came with, keeping its lightness to within the little the
    /// compression spends and losing only the chroma BT.709 cannot hold.
    #[test]
    fn the_wide_primaries_land_inside_at_their_own_hue() {
        for rgb2020 in [
            [1.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0], [0.0, 1.0, 1.0], [1.0, 0.0, 1.0],
            [0.6, 0.1, 0.0], [0.0, 0.5, 0.2],
        ] {
            let converted = as_709(rgb2020);
            let mapped = compress_bt2020_to_bt709(converted);
            assert!(mapped.iter().all(|v| *v >= -1e-3), "{rgb2020:?} still outside: {mapped:?}");
            let (l0, c0, h0) = lch(converted);
            let (l1, c1, h1) = lch(mapped);
            let dh = (h1 - h0).rem_euclid(360.0).min((h0 - h1).rem_euclid(360.0));
            eprintln!("{rgb2020:?}: L {l0:.1}→{l1:.1}, C {c0:.1}→{c1:.1}, hue drift {dh:.2}°");
            assert!(dh < 1.5, "hue moved {dh}° for {rgb2020:?}");
            assert!(c1 < c0 + 0.5, "chroma grew for {rgb2020:?}: {c0} → {c1}");
            // Lightness is not held exactly — the compression trades a
            // little of it for chroma — but no colour may change
            // brightness enough to read as a different exposure.
            assert!((l1 - l0).abs() < 6.0, "lightness moved {l0:.1}→{l1:.1} for {rgb2020:?}");
        }
    }

    /// The light saturated colours are the reason for aiming at the cusp.
    /// Holding lightness exactly leaves BT.2020 yellow with a fifth of
    /// its chroma, which reads as milky cream; it has to keep most of
    /// what BT.709's own yellow carries.
    #[test]
    fn a_wide_yellow_stays_yellow() {
        let (_, mapped, _) = lch(compress_bt2020_to_bt709(as_709([1.0, 1.0, 0.0])));
        let (_, bt709_yellow, _) = lch([1.0, 1.0, 0.0]);
        eprintln!("wide yellow → C {mapped:.1}, BT.709 yellow itself C {bt709_yellow:.1}");
        assert!(
            mapped > bt709_yellow * 0.75,
            "the wide yellow went milky: C {mapped:.1} against BT.709's own {bt709_yellow:.1}",
        );
    }

    /// The point of the exercise: colour the clip flattened onto one
    /// pixel stays apart, and nothing collapses.
    #[test]
    fn colours_outside_the_gamut_stay_distinct() {
        let clip = |rgb: [f32; 3]| rgb.map(|v: f32| v.clamp(0.0, 1.0));
        let apart = |a: [f32; 3], b: [f32; 3]| (0..3).map(|i| (a[i] - b[i]).abs()).fold(0.0f32, f32::max);
        // Two reds the clip cannot tell apart: both sit outside BT.709 on
        // the same side, so both clamp to the same pixel.
        let (a, b) = ([0.9f32, 0.05, 0.0], [1.0f32, 0.0, 0.0]);
        let clipped = apart(clip(as_709(a)), clip(as_709(b)));
        let mapped = apart(compress_bt2020_to_bt709(as_709(a)), compress_bt2020_to_bt709(as_709(b)));
        eprintln!("two wide reds: clipped {clipped:.4} apart, mapped {mapped:.4}");
        assert!(clipped < 0.005, "the clip used to separate these; pick a harder pair");
        assert!(mapped > 0.02, "the mapping left them flat: {mapped}");
        // And across a spread of wide colours: nothing collapses, and
        // nothing loses most of its separation. Compression does narrow
        // the widest gaps — BT.709 is a smaller gamut, so colours a whole
        // channel apart cannot all stay that far apart — but the pairs
        // the clip flattened gain far more than the others lose.
        let wide = [
            [1.0f32, 0.0, 0.0], [0.8, 0.1, 0.0], [0.0, 1.0, 0.0], [0.1, 0.8, 0.2],
            [0.0, 0.0, 1.0], [0.2, 0.1, 0.9], [1.0, 1.0, 0.0], [0.0, 1.0, 1.0],
        ];
        for (i, a) in wide.iter().enumerate() {
            for b in &wide[i + 1..] {
                let clipped = apart(clip(as_709(*a)), clip(as_709(*b)));
                let mapped = apart(compress_bt2020_to_bt709(as_709(*a)), compress_bt2020_to_bt709(as_709(*b)));
                let floor = if clipped < 0.05 { 0.02 } else { (clipped * 0.5).max(0.05) };
                assert!(mapped >= floor, "{a:?} vs {b:?}: clipped {clipped:.3}, mapped {mapped:.3}");
            }
        }
    }

    /// Along a hue, more chroma in gives no less chroma out: a reversal
    /// would read as a band of flat colour across a gradient.
    #[test]
    fn the_mapping_is_monotone_along_a_hue() {
        let to_2020 = crate::primaries::xyz_to_rgb_matrix(&PRIMARIES_BT2020);
        for l in [30.0f32, 50.0, 70.0, 90.0] {
            for h in [0.0f32, 60.0, 120.0, 180.0, 240.0, 300.0] {
                let mut previous = -1.0f32;
                for i in 0..=60 {
                    let c = i as f32;
                    // Only chroma BT.2020 itself can hold: beyond its own
                    // boundary there is no colour to map.
                    let in_2020 = matrix3_mul_vec(to_2020, lab_to_xyz(lch_to_lab(l, c, h)));
                    if in_2020.iter().any(|v| *v < 0.0 || *v > 1.0) {
                        break;
                    }
                    let (_, chroma, _) = lch(compress_bt2020_to_bt709(from_lab(l, c, h)));
                    assert!(
                        chroma >= previous - 0.2,
                        "L {l}, hue {h}: chroma dipped at C {c}: {previous} → {chroma}",
                    );
                    previous = chroma;
                }
            }
        }
    }

    /// What the mapping produces is a colour BT.709 can show, so the
    /// clamp that closes it has nothing to do. It cannot be exactly
    /// nothing — the reach tables interpolate between grid points — but
    /// an excursion worth more than a code at 8 bits would mean colour
    /// being flattened against the hull again, which is the fault this
    /// module exists to remove.
    #[test]
    fn the_mapping_lands_inside_the_gamut() {
        let mut worst = (0.0f32, [0.0f32; 3]);
        let mut n = 0;
        for ri in 0..=20 {
            for gi in 0..=20 {
                for bi in 0..=20 {
                    let rgb2020 = [ri as f32 / 20.0, gi as f32 / 20.0, bi as f32 / 20.0];
                    let input = as_709(rgb2020);
                    // A colour brighter than white keeps its brightness,
                    // so measure the excursion against its own scale
                    // rather than against 1.0.
                    let scale = input[0].max(input[1]).max(input[2]).max(1.0);
                    let mapped = compress_unclamped(input).map(|v| v / scale);
                    let out = mapped.iter().fold(0.0f32, |acc, v| acc.max(-v).max(v - 1.0));
                    if out > worst.0 {
                        worst = (out, rgb2020);
                    }
                    n += 1;
                }
            }
        }
        eprintln!("worst excursion over {n} colours: {:.4} at {:?}", worst.0, worst.1);
        assert!(worst.0 < 0.004, "{:?} landed {:.4} outside BT.709", worst.1, worst.0);
    }

    /// A pixel brighter than white keeps its brightness: this maps
    /// colour, the tone map owns exposure.
    #[test]
    fn brightness_survives_the_mapping() {
        let wide = as_709([1.0, 0.0, 0.0]);
        let dim = compress_bt2020_to_bt709(wide);
        let bright = compress_bt2020_to_bt709(wide.map(|v| v * 6.0));
        for i in 0..3 {
            assert!((bright[i] - dim[i] * 6.0).abs() < 0.02, "{dim:?} vs {bright:?}");
        }
    }

    /// A barely-tinted colour keeps its lightness. The compression only
    /// spends lightness where there is chroma worth saving, so a grade's
    /// black and white levels survive it — including the near-neutrals
    /// either side of them, where a shift would read as a lift or a
    /// crush rather than as a colour change.
    #[test]
    fn a_near_neutral_keeps_its_lightness() {
        for l in [2.0f32, 10.0, 50.0, 90.0, 98.0] {
            for c in [0.2f32, 1.0, 4.0] {
                for h in [20.0f32, 100.0, 200.0, 300.0] {
                    let (out, _, _) = lch(compress_bt2020_to_bt709(from_lab(l, c, h)));
                    assert!((out - l).abs() < 0.5, "L {l}, C {c}, hue {h}: lightness moved to {out:.2}");
                }
            }
        }
    }
}




