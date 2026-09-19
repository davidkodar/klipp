//! Measuring a picture the way a colourist judges it: in CIELAB, with
//! published tolerances. Every judgement here is a number against a
//! target, so an agent — or the check — can say what is off and by how
//! much without anyone's eye. The chart test at the bottom is what makes
//! the numbers trustworthy: the X-Rite ColorChecker's published values
//! come out of this code within the tolerance of its 8-bit rendering.

use serde::{Deserialize, Serialize};

/// A CIELAB colour, D65.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Lab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

impl Lab {
    pub fn chroma(&self) -> f32 {
        (self.a * self.a + self.b * self.b).sqrt()
    }
    /// Hue angle in degrees, 0..360.
    pub fn hue(&self) -> f32 {
        let h = self.b.atan2(self.a).to_degrees();
        if h < 0.0 { h + 360.0 } else { h }
    }
}

fn srgb_decode(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Linear BT.709 / sRGB → XYZ (D65).
pub fn linear_to_xyz(r: f32, g: f32, b: f32) -> [f32; 3] {
    [
        0.4124564 * r + 0.3575761 * g + 0.1804375 * b,
        0.2126729 * r + 0.7151522 * g + 0.0721750 * b,
        0.0193339 * r + 0.1191920 * g + 0.9503041 * b,
    ]
}

/// XYZ → Lab under a white point.
pub fn xyz_to_lab(xyz: [f32; 3], white: [f32; 3]) -> Lab {
    let f = |t: f32| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(xyz[0] / white[0]), f(xyz[1] / white[1]), f(xyz[2] / white[2]));
    Lab { l: 116.0 * fy - 16.0, a: 500.0 * (fx - fy), b: 200.0 * (fy - fz) }
}

pub const D65: [f32; 3] = [0.95047, 1.00000, 1.08883];
pub const D50: [f32; 3] = [0.96422, 1.00000, 0.82521];

/// Bradford adaptation of an XYZ from D65 to D50, for comparison with
/// values published under D50 (the ColorChecker's).
pub fn xyz_d65_to_d50(xyz: [f32; 3]) -> [f32; 3] {
    let m = [
        [1.0478112, 0.0228866, -0.0501270],
        [0.0295424, 0.9904844, -0.0170491],
        [-0.0092345, 0.0150436, 0.7521316],
    ];
    [
        m[0][0] * xyz[0] + m[0][1] * xyz[1] + m[0][2] * xyz[2],
        m[1][0] * xyz[0] + m[1][1] * xyz[1] + m[1][2] * xyz[2],
        m[2][0] * xyz[0] + m[2][1] * xyz[1] + m[2][2] * xyz[2],
    ]
}

/// An sRGB 8-bit pixel as Lab (D65).
pub fn srgb8_to_lab(r: u8, g: u8, b: u8) -> Lab {
    let xyz = linear_to_xyz(srgb_decode(r as f32 / 255.0), srgb_decode(g as f32 / 255.0), srgb_decode(b as f32 / 255.0));
    xyz_to_lab(xyz, D65)
}

/// A linear BT.709 pixel as Lab (D65).
pub fn linear_to_lab(r: f32, g: f32, b: f32) -> Lab {
    xyz_to_lab(linear_to_xyz(r, g, b), D65)
}

/// CIEDE2000, the colour difference standard (Sharma et al. 2005).
pub fn delta_e2000(x: Lab, y: Lab) -> f32 {
    let (l1, a1, b1, l2, a2, b2) = (x.l as f64, x.a as f64, x.b as f64, y.l as f64, y.a as f64, y.b as f64);
    let c1 = (a1 * a1 + b1 * b1).sqrt();
    let c2 = (a2 * a2 + b2 * b2).sqrt();
    let cm = (c1 + c2) / 2.0;
    let g = 0.5 * (1.0 - (cm.powi(7) / (cm.powi(7) + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = (a1 * (1.0 + g), a2 * (1.0 + g));
    let c1p = (a1p * a1p + b1 * b1).sqrt();
    let c2p = (a2p * a2p + b2 * b2).sqrt();
    let hp = |a: f64, b: f64| if a == 0.0 && b == 0.0 { 0.0 } else { let h = b.atan2(a).to_degrees(); if h < 0.0 { h + 360.0 } else { h } };
    let (h1p, h2p) = (hp(a1p, b1), hp(a2p, b2));
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let dhp = if c1p * c2p == 0.0 { 0.0 } else {
        let d = h2p - h1p;
        if d.abs() <= 180.0 { d } else if d > 180.0 { d - 360.0 } else { d + 360.0 }
    };
    let dh = 2.0 * (c1p * c2p).sqrt() * (dhp.to_radians() / 2.0).sin();
    let lm = (l1 + l2) / 2.0;
    let cmp = (c1p + c2p) / 2.0;
    let hm = if c1p * c2p == 0.0 { h1p + h2p } else {
        let s = h1p + h2p;
        if (h1p - h2p).abs() <= 180.0 { s / 2.0 } else if s < 360.0 { (s + 360.0) / 2.0 } else { (s - 360.0) / 2.0 }
    };
    let t = 1.0 - 0.17 * (hm - 30.0).to_radians().cos() + 0.24 * (2.0 * hm).to_radians().cos() + 0.32 * (3.0 * hm + 6.0).to_radians().cos() - 0.20 * (4.0 * hm - 63.0).to_radians().cos();
    let dtheta = 30.0 * (-((hm - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cmp.powi(7) / (cmp.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (lm - 50.0).powi(2) / (20.0 + (lm - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cmp;
    let sh = 1.0 + 0.015 * cmp * t;
    let rt = -(2.0 * dtheta).to_radians().sin() * rc;
    ((dl / sl).powi(2) + (dc / sc).powi(2) + (dh / sh).powi(2) + rt * (dc / sc) * (dh / sh)).sqrt() as f32
}

// ─── Skin ────────────────────────────────────────────────────────────

/// Where real skin sits, whatever the person: the hue band in CIELAB
/// (red-yellow), the chroma a face carries, the lightness that is
/// neither blown nor lost. Hue is the invariant; the rest varies with
/// the person and the light.
pub const SKIN_HUE_LOW: f32 = 40.0;
pub const SKIN_HUE_HIGH: f32 = 65.0;
pub const SKIN_HUE_CENTRE: f32 = 52.0;
pub const SKIN_CHROMA_LOW: f32 = 12.0;
pub const SKIN_CHROMA_HIGH: f32 = 45.0;
pub const SKIN_L_LOW: f32 = 18.0;
pub const SKIN_L_HIGH: f32 = 85.0;

/// A face's colour, measured over a grid of patches inside its region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceMeasure {
    /// The region measured, as fractions of the frame (x, y, w, h).
    pub region: [f32; 4],
    /// Patches that held skin, of the grid's 64.
    pub patches: u32,
    /// The face's colour: the median of the patches.
    pub median: Lab,
    pub hue: f32,
    pub chroma: f32,
    /// Spread across the patches: inter-quartile ranges.
    pub hue_iqr: f32,
    pub l_iqr: f32,
    /// The patch furthest from the band's centre in hue.
    pub worst_hue: f32,
    /// A hue spread wide enough to mean two lights on one face.
    pub mixed_light: bool,
    /// How far the median hue sits outside the band, in degrees (0 inside).
    pub hue_off_band: f32,
}

// ─── The frame ───────────────────────────────────────────────────────

/// A frame's readings, in the numbers the rules use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameMeasure {
    /// L* percentiles: 1, 5, 50, 95, 99.
    pub l_p1: f32,
    pub l_p5: f32,
    pub l_p50: f32,
    pub l_p95: f32,
    pub l_p99: f32,
    /// The 5–95 lightness range: the picture's contrast.
    pub contrast_range: f32,
    /// Pixels at or past white and black, as percentages.
    pub clip_hi_pct: f32,
    pub clip_lo_pct: f32,
    /// Mean and 95th-percentile chroma over the mid-lightness pixels.
    pub chroma_mean: f32,
    pub chroma_p95: f32,
    /// The cast on the neutral candidates: median a*, b*; and how much
    /// of the frame was neutral.
    pub cast_a: f32,
    pub cast_b: f32,
    pub neutral_pct: f32,
    pub faces: Vec<FaceMeasure>,
    /// Pixels read (the frame is sub-sampled).
    pub sampled: u32,
}

impl FrameMeasure {
    /// The cast as a magnitude in Lab units.
    pub fn cast(&self) -> f32 {
        (self.cast_a * self.cast_a + self.cast_b * self.cast_b).sqrt()
    }
}

fn percentile(sorted: &[f32], q: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f32 - 1.0) * q).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(|a, b| a.total_cmp(b));
    percentile(v, 0.5)
}

fn iqr(v: &mut [f32]) -> f32 {
    v.sort_by(|a, b| a.total_cmp(b));
    percentile(v, 0.75) - percentile(v, 0.25)
}

/// Whether a Lab colour could be skin at all: the wide gate, before the
/// patches decide.
fn skin_candidate(c: Lab) -> bool {
    let (h, ch) = (c.hue(), c.chroma());
    ch >= 8.0 && (20.0..=90.0).contains(&h) && c.l >= 12.0 && c.l <= 92.0
        // Lips: redder and more saturated than skin.
        && !(h < 30.0 && ch > 35.0)
}

/// Measure an sRGB8 RGBA frame. `faces` are regions (x, y, w, h as
/// fractions of the frame) to read skin in — a person's head; each is
/// laid out as an 8×8 grid of patches and the patches that hold skin
/// decide. The whole frame is read sub-sampled to about a quarter of a
/// million pixels, so a 4K frame costs a few milliseconds.
pub fn measure_frame(rgba: &[u8], width: u32, height: u32, faces: &[[f32; 4]]) -> FrameMeasure {
    let (w, h) = (width as usize, height as usize);
    let total = w * h;
    let step = (total / 250_000).max(1);
    let mut ls: Vec<f32> = Vec::with_capacity(total / step + 1);
    let mut chromas: Vec<f32> = Vec::new();
    // Neutral candidates: the least saturated quarter of the mid-lightness
    // pixels. Relative, not an absolute chroma, so a scene under a cast
    // still has "neutrals" to read the cast from — under a blue light the
    // greys are blue, and that is the finding.
    let mut mids: Vec<(f32, f32, f32)> = Vec::new();
    let (mut clip_hi, mut clip_lo, mut n) = (0usize, 0usize, 0usize);
    let mut i = 0;
    while i < total && rgba.len() >= (i + 1) * 4 {
        let p = i * 4;
        let (r, g, b) = (rgba[p], rgba[p + 1], rgba[p + 2]);
        let c = srgb8_to_lab(r, g, b);
        ls.push(c.l);
        if r.max(g).max(b) >= 254 {
            clip_hi += 1;
        }
        if r.min(g).min(b) <= 1 {
            clip_lo += 1;
        }
        if c.l >= 15.0 && c.l <= 85.0 {
            chromas.push(c.chroma());
        }
        if c.l >= 20.0 && c.l <= 90.0 {
            mids.push((c.chroma(), c.a, c.b));
        }
        n += 1;
        i += step;
    }
    let mut chroma_sorted: Vec<f32> = mids.iter().map(|m| m.0).collect();
    chroma_sorted.sort_by(|a, b| a.total_cmp(b));
    let gate = percentile(&chroma_sorted, 0.25).max(6.0).min(25.0);
    let (mut na, mut nb): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    for (c, a, b) in &mids {
        if *c <= gate {
            na.push(*a);
            nb.push(*b);
        }
    }
    ls.sort_by(|a, b| a.total_cmp(b));
    chromas.sort_by(|a, b| a.total_cmp(b));
    let nn = n.max(1) as f32;
    let faces = faces.iter().map(|f| measure_face(rgba, w, h, *f)).collect();
    FrameMeasure {
        l_p1: percentile(&ls, 0.01),
        l_p5: percentile(&ls, 0.05),
        l_p50: percentile(&ls, 0.5),
        l_p95: percentile(&ls, 0.95),
        l_p99: percentile(&ls, 0.99),
        contrast_range: percentile(&ls, 0.95) - percentile(&ls, 0.05),
        clip_hi_pct: clip_hi as f32 / nn * 100.0,
        clip_lo_pct: clip_lo as f32 / nn * 100.0,
        chroma_mean: if chromas.is_empty() { 0.0 } else { chromas.iter().sum::<f32>() / chromas.len() as f32 },
        chroma_p95: percentile(&chromas, 0.95),
        cast_a: median(&mut na),
        cast_b: median(&mut nb),
        neutral_pct: na.len() as f32 / nn * 100.0,
        faces,
        sampled: n as u32,
    }
}

/// One face region as a grid of 8×8 patches: each patch the median of
/// its skin candidates, kept when at least 40% of it is skin.
pub fn measure_face(rgba: &[u8], w: usize, h: usize, region: [f32; 4]) -> FaceMeasure {
    const GRID: usize = 8;
    let x0 = ((region[0] * w as f32).round().max(0.0) as usize).min(w.saturating_sub(1));
    let y0 = ((region[1] * h as f32).round().max(0.0) as usize).min(h.saturating_sub(1));
    let x1 = (((region[0] + region[2]) * w as f32).round() as usize).clamp(x0 + 1, w);
    let y1 = (((region[1] + region[3]) * h as f32).round() as usize).clamp(y0 + 1, h);
    let (rw, rh) = (x1 - x0, y1 - y0);
    // Enough pixels per patch to take a median of, never more than needed.
    let stride = ((rw * rh) / (GRID * GRID * 64)).max(1);
    let mut patch_l: Vec<f32> = Vec::new();
    let mut patch_h: Vec<f32> = Vec::new();
    let mut patch_c: Vec<f32> = Vec::new();
    let mut patch_ab: Vec<(f32, f32)> = Vec::new();
    for gy in 0..GRID {
        for gx in 0..GRID {
            let (px0, px1) = (x0 + rw * gx / GRID, x0 + rw * (gx + 1) / GRID);
            let (py0, py1) = (y0 + rh * gy / GRID, y0 + rh * (gy + 1) / GRID);
            let (mut ls, mut hs, mut cs, mut as_, mut bs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let mut seen = 0usize;
            let mut k = 0usize;
            for y in py0..py1 {
                for x in px0..px1 {
                    k += 1;
                    if k % stride != 0 {
                        continue;
                    }
                    let p = (y * w + x) * 4;
                    if p + 3 >= rgba.len() {
                        continue;
                    }
                    seen += 1;
                    let c = srgb8_to_lab(rgba[p], rgba[p + 1], rgba[p + 2]);
                    if skin_candidate(c) {
                        ls.push(c.l);
                        hs.push(c.hue());
                        cs.push(c.chroma());
                        as_.push(c.a);
                        bs.push(c.b);
                    }
                }
            }
            if seen == 0 || (ls.len() as f32) < 0.4 * seen as f32 {
                continue;
            }
            patch_l.push(median(&mut ls));
            patch_h.push(median(&mut hs));
            patch_c.push(median(&mut cs));
            patch_ab.push((median(&mut as_), median(&mut bs)));
        }
    }
    let patches = patch_l.len() as u32;
    if patches == 0 {
        return FaceMeasure { region, patches, median: Lab::default(), hue: 0.0, chroma: 0.0, hue_iqr: 0.0, l_iqr: 0.0, worst_hue: 0.0, mixed_light: false, hue_off_band: 0.0 };
    }
    let mut a_all: Vec<f32> = patch_ab.iter().map(|p| p.0).collect();
    let mut b_all: Vec<f32> = patch_ab.iter().map(|p| p.1).collect();
    let median_lab = Lab { l: median(&mut patch_l.clone()), a: median(&mut a_all), b: median(&mut b_all) };
    let hue = median_lab.hue();
    let worst_hue = patch_h.iter().copied().max_by(|a, b| (a - SKIN_HUE_CENTRE).abs().total_cmp(&(b - SKIN_HUE_CENTRE).abs())).unwrap_or(hue);
    let hue_iqr = iqr(&mut patch_h.clone());
    let l_iqr = iqr(&mut patch_l.clone());
    let hue_off_band = if hue < SKIN_HUE_LOW { SKIN_HUE_LOW - hue } else if hue > SKIN_HUE_HIGH { hue - SKIN_HUE_HIGH } else { 0.0 };
    FaceMeasure { region, patches, median: median_lab, hue, chroma: median_lab.chroma(), hue_iqr, l_iqr, worst_hue, mixed_light: hue_iqr > 18.0 && patches >= 8, hue_off_band }
}

/// The head of a detected person: the upper part of the box, the
/// middle of its width.
pub fn head_region(person: [f32; 4]) -> [f32; 4] {
    let [x, y, w, h] = person;
    let head_h = (h * 0.32).max(0.02);
    let head_w = (w * 0.6).max(0.02);
    [x + (w - head_w) / 2.0, y + h * 0.02, head_w, head_h]
}

// ─── Targets and judgement ───────────────────────────────────────────

/// The look a piece is graded to, as numbers: what the readings are
/// measured against. Every field is optional so a target can name only
/// what it cares about.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LookTarget {
    pub name: String,
    /// L* of the black point (1st percentile) and white point (99th).
    #[serde(default)]
    pub black_l: Option<f32>,
    #[serde(default)]
    pub white_l: Option<f32>,
    /// L* of the median.
    #[serde(default)]
    pub mid_l: Option<f32>,
    /// The 5–95 lightness range.
    #[serde(default)]
    pub contrast_range: Option<f32>,
    /// Mean chroma over the mid-lightness pixels.
    #[serde(default)]
    pub chroma_mean: Option<f32>,
    /// The cast the look wants on neutrals (a*, b*); none for neutral.
    #[serde(default)]
    pub cast_a: Option<f32>,
    #[serde(default)]
    pub cast_b: Option<f32>,
    /// Where skin hue should sit; the band's centre when left out.
    #[serde(default)]
    pub skin_hue: Option<f32>,
    /// A screen recording: blacks and whites are by design; only the
    /// cast, the clipping a grade adds, and skin are judged.
    #[serde(default)]
    pub screencast: bool,
}

impl LookTarget {
    /// The named looks: numbers, not LUTs.
    pub fn preset(name: &str) -> Option<LookTarget> {
        let n = name.trim().to_lowercase();
        let base = |name: &str| LookTarget { name: name.to_string(), ..Default::default() };
        Some(match n.as_str() {
            "broadcast_neutral" | "broadcast" | "neutral" => LookTarget { black_l: Some(4.0), white_l: Some(95.0), mid_l: Some(48.0), contrast_range: Some(65.0), chroma_mean: Some(22.0), cast_a: Some(0.0), cast_b: Some(0.0), ..base("broadcast_neutral") },
            "web_punchy" | "punchy" | "web" => LookTarget { black_l: Some(3.0), white_l: Some(97.0), mid_l: Some(50.0), contrast_range: Some(75.0), chroma_mean: Some(30.0), cast_a: Some(0.0), cast_b: Some(0.0), ..base("web_punchy") },
            "film_soft" | "film" | "filmic" => LookTarget { black_l: Some(8.0), white_l: Some(92.0), mid_l: Some(46.0), contrast_range: Some(55.0), chroma_mean: Some(18.0), cast_a: Some(1.0), cast_b: Some(3.0), ..base("film_soft") },
            "warm_interview" | "warm" | "interview" => LookTarget { black_l: Some(5.0), white_l: Some(94.0), mid_l: Some(50.0), contrast_range: Some(62.0), chroma_mean: Some(24.0), cast_a: Some(2.0), cast_b: Some(5.0), skin_hue: Some(50.0), ..base("warm_interview") },
            "cool_tech" | "cool" | "tech" => LookTarget { black_l: Some(4.0), white_l: Some(95.0), mid_l: Some(47.0), contrast_range: Some(68.0), chroma_mean: Some(20.0), cast_a: Some(-1.0), cast_b: Some(-5.0), ..base("cool_tech") },
            "screencast" | "screen" => LookTarget { cast_a: Some(0.0), cast_b: Some(0.0), screencast: true, ..base("screencast") },
            _ => return None,
        })
    }

    pub const PRESETS: [&'static str; 6] = ["broadcast_neutral", "web_punchy", "film_soft", "warm_interview", "cool_tech", "screencast"];

    /// A target lifted from measured frames: the mean of their readings.
    pub fn from_measures(name: &str, measures: &[FrameMeasure]) -> Option<LookTarget> {
        if measures.is_empty() {
            return None;
        }
        let n = measures.len() as f32;
        let mean = |f: &dyn Fn(&FrameMeasure) -> f32| measures.iter().map(f).sum::<f32>() / n;
        let skin: Vec<f32> = measures.iter().flat_map(|m| m.faces.iter().filter(|f| f.patches >= 4).map(|f| f.hue)).collect();
        Some(LookTarget {
            name: name.to_string(),
            black_l: Some(mean(&|m| m.l_p1)),
            white_l: Some(mean(&|m| m.l_p99)),
            mid_l: Some(mean(&|m| m.l_p50)),
            contrast_range: Some(mean(&|m| m.contrast_range)),
            chroma_mean: Some(mean(&|m| m.chroma_mean)),
            cast_a: Some(mean(&|m| m.cast_a)),
            cast_b: Some(mean(&|m| m.cast_b)),
            skin_hue: (!skin.is_empty()).then(|| skin.iter().sum::<f32>() / skin.len() as f32),
            screencast: false,
        })
    }
}

/// How far off a reading is, and how bad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Warn,
    Fail,
}

/// One rule's outcome: the reading, the tolerance, the verdict, the knob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub rule: &'static str,
    pub verdict: Verdict,
    pub reading: f32,
    pub target: f32,
    pub tolerance: f32,
    pub what: String,
    pub fix: &'static str,
}

fn graded(rule: &'static str, reading: f32, target: f32, warn: f32, fail: f32, what: String, fix: &'static str) -> Finding {
    let off = (reading - target).abs();
    let verdict = if off > fail { Verdict::Fail } else if off > warn { Verdict::Warn } else { Verdict::Pass };
    Finding { rule, verdict, reading, target, tolerance: warn, what, fix }
}

/// Judge a frame against a target. Every rule that has a target and a
/// reading answers; the list is the colour report for the frame.
pub fn judge(m: &FrameMeasure, t: &LookTarget) -> Vec<Finding> {
    let mut out = Vec::new();
    // Clipping is a fault in every look, but a screen recording's pure
    // whites and blacks are its own: a higher bar there.
    let (hi_warn, hi_fail) = if t.screencast { (30.0, 60.0) } else { (0.5, 2.0) };
    out.push(Finding { rule: "clipped_white", verdict: if m.clip_hi_pct > hi_fail { Verdict::Fail } else if m.clip_hi_pct > hi_warn { Verdict::Warn } else { Verdict::Pass }, reading: m.clip_hi_pct, target: 0.0, tolerance: hi_warn, what: format!("{:.1}% of the frame is at white", m.clip_hi_pct), fix: "highlights or gain down until nothing clips" });
    out.push(Finding { rule: "crushed_black", verdict: if m.clip_lo_pct > hi_fail { Verdict::Fail } else if m.clip_lo_pct > hi_warn { Verdict::Warn } else { Verdict::Pass }, reading: m.clip_lo_pct, target: 0.0, tolerance: hi_warn, what: format!("{:.1}% of the frame is at black", m.clip_lo_pct), fix: "shadows or lift up until the blacks hold detail" });
    if !t.screencast {
        if let Some(b) = t.black_l {
            out.push(graded("black_point", m.l_p1, b, 4.0, 9.0, format!("black point L* {:.0} (target {b:.0})", m.l_p1), "lift"));
        }
        if let Some(wv) = t.white_l {
            out.push(graded("white_point", m.l_p99, wv, 4.0, 9.0, format!("white point L* {:.0} (target {wv:.0})", m.l_p99), "gain"));
        }
        if let Some(mid) = t.mid_l {
            out.push(graded("mid_grey", m.l_p50, mid, 6.0, 12.0, format!("median L* {:.0} (target {mid:.0})", m.l_p50), "exposure or gamma"));
        }
        if let Some(c) = t.contrast_range {
            out.push(graded("contrast", m.contrast_range, c, 8.0, 16.0, format!("5–95 lightness range {:.0} (target {c:.0})", m.contrast_range), "contrast"));
        }
        if let Some(c) = t.chroma_mean {
            out.push(graded("saturation", m.chroma_mean, c, 5.0, 10.0, format!("mean chroma {:.0} (target {c:.0})", m.chroma_mean), "saturation, or vibrance to keep skin"));
        }
    }
    if let (Some(ta), Some(tb)) = (t.cast_a, t.cast_b) {
        if m.neutral_pct >= 1.0 {
            let off = ((m.cast_a - ta).powi(2) + (m.cast_b - tb).powi(2)).sqrt();
            let dir = cast_name(m.cast_a - ta, m.cast_b - tb);
            let verdict = if off > 8.0 { Verdict::Fail } else if off > 3.0 { Verdict::Warn } else { Verdict::Pass };
            out.push(Finding { rule: "cast", verdict, reading: off, target: 0.0, tolerance: 3.0, what: format!("neutrals sit {off:.1} toward {dir} (a* {:.1}, b* {:.1}; {:.0}% of the frame is neutral)", m.cast_a, m.cast_b, m.neutral_pct), fix: "temperature (b*) and tint (a*) until the neutrals read a* b* near the target" });
        }
    }
    let skin_target = t.skin_hue.unwrap_or(SKIN_HUE_CENTRE);
    for (i, f) in m.faces.iter().enumerate() {
        if f.patches < 4 {
            continue;
        }
        let who = if m.faces.len() > 1 { format!("face {}", i + 1) } else { "the face".to_string() };
        // Off the band is what matters; inside it, the distance to the
        // look's own skin hue is advisory.
        let (verdict, what) = if f.mixed_light {
            (Verdict::Warn, format!("{who} is lit by two colours of light (hue spread {:.0}°): median hue {:.0}°", f.hue_iqr, f.hue))
        } else if f.hue_off_band > 10.0 {
            (Verdict::Fail, format!("{who}: skin hue {:.0}°, {:.0}° outside the skin band ({SKIN_HUE_LOW:.0}–{SKIN_HUE_HIGH:.0}°)", f.hue, f.hue_off_band))
        } else if f.hue_off_band > 4.0 {
            (Verdict::Warn, format!("{who}: skin hue {:.0}°, {:.0}° outside the skin band", f.hue, f.hue_off_band))
        } else {
            (Verdict::Pass, format!("{who}: skin hue {:.0}° on the band (chroma {:.0}, L* {:.0}, {} patches)", f.hue, f.chroma, f.median.l, f.patches))
        };
        out.push(Finding { rule: "skin_hue", verdict, reading: f.hue, target: skin_target, tolerance: 4.0, what, fix: "skin_balance, or if the neutrals carry the same cast, temperature and tint" });
        let chroma_verdict = if f.chroma < SKIN_CHROMA_LOW - 4.0 || f.chroma > SKIN_CHROMA_HIGH + 6.0 { Verdict::Fail } else if f.chroma < SKIN_CHROMA_LOW || f.chroma > SKIN_CHROMA_HIGH { Verdict::Warn } else { Verdict::Pass };
        if chroma_verdict != Verdict::Pass {
            out.push(Finding { rule: "skin_chroma", verdict: chroma_verdict, reading: f.chroma, target: (SKIN_CHROMA_LOW + SKIN_CHROMA_HIGH) / 2.0, tolerance: 0.0, what: format!("{who}: skin chroma {:.0} ({} the {SKIN_CHROMA_LOW:.0}–{SKIN_CHROMA_HIGH:.0} band)", f.chroma, if f.chroma < SKIN_CHROMA_LOW { "under" } else { "over" }), fix: if f.chroma < SKIN_CHROMA_LOW { "vibrance up, or a skin secondary with chroma up" } else { "saturation down, or a skin secondary with chroma down" } });
        }
        let l_verdict = if f.median.l > SKIN_L_HIGH || f.median.l < SKIN_L_LOW { Verdict::Fail } else { Verdict::Pass };
        if l_verdict != Verdict::Pass {
            out.push(Finding { rule: "skin_lightness", verdict: l_verdict, reading: f.median.l, target: 55.0, tolerance: 0.0, what: format!("{who}: skin L* {:.0} ({})", f.median.l, if f.median.l > SKIN_L_HIGH { "blown" } else { "lost in the dark" }), fix: if f.median.l > SKIN_L_HIGH { "highlights or exposure down" } else { "shadows or exposure up" } });
        }
    }
    out
}

/// The name of a cast direction in a*/b*.
pub fn cast_name(a: f32, b: f32) -> &'static str {
    let h = b.atan2(a).to_degrees();
    let h = if h < 0.0 { h + 360.0 } else { h };
    // CIELAB hue: red ~40°, yellow ~100°, green ~140°, cyan ~200°, blue ~300°.
    match h as u32 {
        0..=20 | 345..=360 => "magenta-red",
        21..=70 => "orange (warm)",
        71..=110 => "yellow",
        111..=170 => "green",
        171..=230 => "cyan",
        231..=320 => "blue (cool)",
        _ => "magenta",
    }
}

/// The first-order primaries that move a reading to a target: what the
/// grade-to-target loop applies before it measures again. Lift from the
/// black point, gain from the white point, exposure from the median,
/// temperature and tint from the cast, saturation from the chroma. Each
/// is scaled from the readings' units to the knobs' and damped so a
/// round never overshoots.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct PrimaryStep {
    pub exposure: f32,
    pub lift: f32,
    pub gain: f32,
    pub temperature: f32,
    pub tint: f32,
    pub saturation: f32,
}

pub fn primaries_toward(m: &FrameMeasure, t: &LookTarget) -> PrimaryStep {
    const DAMP: f32 = 0.7;
    let mut s = PrimaryStep::default();
    if !t.screencast {
        if let Some(b) = t.black_l {
            // L* at the bottom: about 0.012 lift per L*.
            s.lift = ((b - m.l_p1) * 0.012 * DAMP).clamp(-0.3, 0.3);
        }
        if let Some(wv) = t.white_l {
            s.gain = ((wv - m.l_p99) * 0.010 * DAMP).clamp(-0.3, 0.3);
        }
        if let Some(mid) = t.mid_l {
            // A stop of exposure moves the median by about 15 L*.
            s.exposure = ((mid - m.l_p50) / 15.0 * DAMP).clamp(-1.5, 1.5);
        }
        if let Some(c) = t.chroma_mean {
            if m.chroma_mean > 1.0 {
                s.saturation = (c / m.chroma_mean - 1.0) * DAMP;
            }
        }
    }
    if let (Some(ta), Some(tb)) = (t.cast_a, t.cast_b) {
        if m.neutral_pct >= 1.0 {
            // b* is warmth: temperature; a* is green–magenta: tint. About
            // 0.03 of the knob per unit.
            s.temperature = ((tb - m.cast_b) * 0.03 * DAMP).clamp(-0.5, 0.5);
            s.tint = ((ta - m.cast_a) * 0.03 * DAMP).clamp(-0.5, 0.5);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ColorChecker's published Lab values (X-Rite, D50), against
    /// the sRGB rendering of the same patches: within the tolerance of
    /// an 8-bit rendering everywhere, tighter on the two skin patches.
    #[test]
    fn the_colorchecker_reads_as_published() {
        let published_d50: [[f32; 3]; 24] = [
            [37.99, 13.56, 14.06], [65.71, 18.13, 17.81], [49.93, -4.88, -21.93], [43.14, -13.10, 21.91], [55.11, 8.84, -25.40], [70.72, -33.40, -0.20],
            [62.66, 36.07, 57.10], [40.02, 10.41, -45.96], [51.12, 48.24, 16.25], [30.33, 22.98, -21.59], [72.53, -23.71, 57.26], [71.94, 19.36, 67.86],
            [28.78, 14.18, -50.30], [55.26, -38.34, 31.37], [42.10, 53.38, 28.19], [81.73, 4.04, 79.82], [51.94, 49.99, -14.57], [51.04, -28.63, -28.64],
            [96.54, -0.43, 1.19], [81.26, -0.64, -0.34], [66.77, -0.73, -0.50], [50.87, -0.15, -0.27], [35.66, -0.42, -1.23], [20.46, -0.08, -0.97],
        ];
        let chart = klipp_testing::fixtures::color_checker_24_linear();
        let mut worst = 0.0f32;
        for (i, p) in chart.iter().enumerate() {
            let xyz = xyz_d65_to_d50(linear_to_xyz(p[0], p[1], p[2]));
            let got = xyz_to_lab(xyz, D50);
            let want = Lab { l: published_d50[i][0], a: published_d50[i][1], b: published_d50[i][2] };
            let de = delta_e2000(got, want);
            // The chart's 8-bit sRGB rendering is least exact on its
            // saturated primaries (patches 12–17).
            let limit = if i < 2 { 2.5 } else if (12..18).contains(&i) { 5.0 } else { 4.0 };
            assert!(de <= limit, "patch {i}: read {got:?} vs published {want:?}, dE2000 {de:.2} > {limit}");
            worst = worst.max(de);
        }
        assert!(worst < 5.0, "worst patch {worst:.2}");
        // The two skin patches sit inside the skin band.
        for i in 0..2 {
            let l = srgb8_to_lab_from_linear(chart[i]);
            assert!((SKIN_HUE_LOW..=SKIN_HUE_HIGH).contains(&l.hue()), "skin patch {i} hue {:.1}", l.hue());
            assert!((SKIN_CHROMA_LOW..=SKIN_CHROMA_HIGH).contains(&l.chroma()));
        }
    }

    fn srgb8_to_lab_from_linear(p: [f32; 4]) -> Lab {
        linear_to_lab(p[0], p[1], p[2])
    }

    #[test]
    fn delta_e2000_matches_the_published_pairs() {
        // Sharma's test data, pairs 1, 7 and 17.
        let cases = [
            ((50.0, 2.6772, -79.7751), (50.0, 0.0, -82.7485), 2.0425),
            ((50.0, 0.0, 0.0), (50.0, -1.0, 2.0), 2.3669),
            ((50.0, 2.5, 0.0), (73.0, 25.0, -18.0), 27.1492),
        ];
        for ((l1, a1, b1), (l2, a2, b2), want) in cases {
            let got = delta_e2000(Lab { l: l1, a: a1, b: b1 }, Lab { l: l2, a: a2, b: b2 });
            assert!((got - want).abs() < 0.01, "got {got} want {want}");
        }
    }

    fn frame_of(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let c = f(x, y);
                let p = (y * w + x) * 4;
                v[p..p + 3].copy_from_slice(&c);
                v[p + 3] = 255;
            }
        }
        v
    }

    /// A face-coloured patch reads on the band; the same patch under a
    /// blue cast reads off it and the neutrals say the scene is cast.
    #[test]
    fn a_face_patch_is_read_and_a_cast_is_named() {
        let (w, h) = (320, 180);
        let face = [0.4f32, 0.2, 0.2, 0.4];
        let frame = frame_of(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            if fx >= face[0] && fx < face[0] + face[2] && fy >= face[1] && fy < face[1] + face[3] { [194, 150, 130] } else { [120, 120, 120] }
        });
        let m = measure_frame(&frame, w as u32, h as u32, &[face]);
        assert_eq!(m.faces.len(), 1);
        let f = &m.faces[0];
        assert!(f.patches >= 60, "{}", f.patches);
        assert!((SKIN_HUE_LOW..=SKIN_HUE_HIGH).contains(&f.hue), "hue {}", f.hue);
        assert!(m.cast() < 1.5, "neutral grey reads neutral: {} {}", m.cast_a, m.cast_b);
        let findings = judge(&m, &LookTarget::preset("broadcast_neutral").unwrap());
        assert!(findings.iter().all(|f| f.rule != "skin_hue" || f.verdict == Verdict::Pass), "{findings:?}");
        // Under a strong blue cast: skin off the band, neutrals cast toward blue.
        let cast = frame_of(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            if fx >= face[0] && fx < face[0] + face[2] && fy >= face[1] && fy < face[1] + face[3] { [150, 150, 190] } else { [100, 110, 160] }
        });
        let m2 = measure_frame(&cast, w as u32, h as u32, &[face]);
        assert!(m2.cast_b < -10.0, "blue cast reads as negative b*: {}", m2.cast_b);
        assert_eq!(cast_name(m2.cast_a, m2.cast_b), "blue (cool)");
        let step = primaries_toward(&m2, &LookTarget::preset("broadcast_neutral").unwrap());
        assert!(step.temperature > 0.0, "warm it up: {step:?}");
    }
}
