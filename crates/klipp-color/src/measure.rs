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
///
/// The band is set from measurement, not from the textbook. An X-Rite
/// ColorChecker photographed under studio light (its own neutral
/// patches at a* 0.06, b* 0.08 — a properly balanced photograph) reads
/// its two skin patches at 44.8° and 37.8°, where the published sRGB
/// values for the same patches give 49.2° and 51.0°: real photography
/// renders skin lower than the ideal. Twelve studio and daylight
/// portraits of people of many skin tones measured 25–47° as shot.
/// What is not skin sits well outside: a magenta-cast face from an old
/// tape reads 359°, a green cast reads past 100°, jaundiced yellow
/// past 75°. Hence 25–70, and 45 to aim at — the photographed chart's
/// own skin patch.
pub const SKIN_HUE_LOW: f32 = 25.0;
pub const SKIN_HUE_HIGH: f32 = 70.0;
pub const SKIN_HUE_CENTRE: f32 = 45.0;
pub const SKIN_CHROMA_LOW: f32 = 12.0;
pub const SKIN_CHROMA_HIGH: f32 = 45.0;
pub const SKIN_L_LOW: f32 = 18.0;
pub const SKIN_L_HIGH: f32 = 85.0;

/// Lab → XYZ (D65) → linear BT.709, the way back.
pub fn lab_to_linear(c: Lab) -> [f32; 3] {
    let fy = (c.l + 16.0) / 116.0;
    let fx = fy + c.a / 500.0;
    let fz = fy - c.b / 200.0;
    let g = |f: f32| if f * f * f > 216.0 / 24389.0 { f * f * f } else { (116.0 * f - 16.0) * 27.0 / 24389.0 };
    let (x, y, z) = (g(fx) * D65[0], g(fy) * D65[1], g(fz) * D65[2]);
    [
        3.2404542 * x - 1.5371385 * y - 0.4985314 * z,
        -0.9692660 * x + 1.8760108 * y + 0.0415560 * z,
        0.0556434 * x - 0.2040259 * y + 1.0572252 * z,
    ]
}

/// A Lab colour as sRGB 8-bit.
pub fn lab_to_srgb8(c: Lab) -> [u8; 3] {
    let enc = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round().clamp(0.0, 255.0) as u8
    };
    let lin = lab_to_linear(c);
    [enc(lin[0]), enc(lin[1]), enc(lin[2])]
}

/// The hue of an sRGB colour in the 0–360 degrees a colour wheel and
/// the qualifier node use (red 0, green 120, blue 240) — not Lab's.
pub fn srgb_hue(rgb: [u8; 3]) -> f32 {
    let (r, g, b) = (rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0);
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let d = mx - mn;
    if d <= 1e-6 {
        return 0.0;
    }
    let h = if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0).rem_euclid(360.0)
}

/// The Lab colour at a hue, holding a lightness and chroma: where a
/// face's colour should sit once its hue is put right.
pub fn lab_at(hue_deg: f32, chroma: f32, l: f32) -> Lab {
    let r = hue_deg.to_radians();
    Lab { l, a: chroma * r.cos(), b: chroma * r.sin() }
}

/// The signed difference between two hue angles, -180 to 180: hues are
/// a circle, and 358° is two degrees under 0, not 358 above it.
pub fn hue_diff(from: f32, to: f32) -> f32 {
    let d = (to - from).rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

/// How far a hue sits outside the skin band, in degrees; 0 inside it.
pub fn hue_off_skin_band(hue: f32) -> f32 {
    if hue_diff(SKIN_HUE_LOW, hue) >= 0.0 && hue_diff(hue, SKIN_HUE_HIGH) >= 0.0 {
        return 0.0;
    }
    hue_diff(SKIN_HUE_LOW, hue).abs().min(hue_diff(SKIN_HUE_HIGH, hue).abs())
}

/// How much of a face's grid must hold skin for its reading to be
/// worth anything: a quarter of the patches, never under three.
pub const MIN_SKIN_PATCHES: u32 = 3;

/// A face's colour, measured over a grid of patches inside its region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceMeasure {
    /// The region measured, as fractions of the frame (x, y, w, h).
    pub region: [f32; 4],
    /// Patches that held skin, of the grid the face's size asked for
    /// (8×8 for a face that fills the frame, down to 3×3 for a small one).
    pub patches: u32,
    /// The grid's side: 8, 5 or 3.
    pub grid: u32,
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
    /// The hue with the scene's own cast taken off, and how far that
    /// sits outside the band. Skin turns with the light: under a cool
    /// lamp every face in the room rotates toward magenta, and the fault
    /// is the white balance, not the skin. Judged on this whenever the
    /// frame's neutrals carry a cast; `hue` and `hue_off_band` when they
    /// do not.
    #[serde(default)]
    pub hue_wb: f32,
    #[serde(default)]
    pub hue_off_band_wb: f32,
}

// ─── The frame ───────────────────────────────────────────────────────

/// A frame's readings, in the numbers the rules use.
///
/// `Default` is every reading zero — a black frame with nothing in it.
/// It exists so a test can state the two or three numbers a rule turns
/// on without writing out twenty that do not matter; nothing in the
/// program builds a measurement that way.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// Whether the cast was read off pixels that are actually neutral
    /// (chroma under 8), or only off the least-coloured quarter of the
    /// frame because nothing in it is neutral. A frame of grass and sky
    /// has no white to balance to, and its "cast" is the grass.
    #[serde(default)]
    pub neutral_solid: bool,
    /// The noise floor: the lightness deviation in the flattest parts of
    /// the picture, in L*. Grain, sensor noise, compression. A colourist
    /// reads this before lifting shadows, because lifting multiplies it.
    #[serde(default)]
    pub noise_l: f32,
    /// How much of the flat picture shows the staircase of banding: a
    /// run of identical values and then a step. Per cent of flat tiles.
    #[serde(default)]
    pub banding_pct: f32,
    /// Pixels with a channel pinned at the rail while the others are
    /// not: a colour pushed past what the delivery can hold, which the
    /// parade shows as a channel flat against the top or the bottom.
    #[serde(default)]
    pub gamut_clip_pct: f32,
    pub faces: Vec<FaceMeasure>,
    /// Every skin-coloured pixel in the frame read together, for a frame
    /// with no detected person: rougher than a face (clothes and wood
    /// can pass the gate), so advisory.
    pub skin_all: Option<SkinRead>,
    /// Pixels read (the frame is sub-sampled).
    pub sampled: u32,
}

/// Skin-coloured pixels read as one: their median hue, chroma and
/// lightness, and how much of the frame they are.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SkinRead {
    pub hue: f32,
    pub chroma: f32,
    pub l: f32,
    pub pct: f32,
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

/// Whether a pixel inside a known face can carry its colour: anything
/// but the eyes, the teeth, a specular and the shadows. No hue test —
/// the hue is what is being measured.
fn in_face_candidate(c: Lab) -> bool {
    c.l >= 12.0 && c.l <= 92.0 && c.chroma() >= 4.0
}

/// Whether a Lab colour could be skin at all: the wide gate, for
/// searching a person's box and the frame, where nothing says the
/// pixels are a face.
fn skin_candidate(c: Lab) -> bool {
    let (h, ch) = (c.hue(), c.chroma());
    ch >= 8.0 && (20.0..=90.0).contains(&h) && c.l >= 12.0 && c.l <= 92.0
        // Lips: redder and more saturated than skin.
        && !(h < 30.0 && ch > 35.0)
}

/// Measure an sRGB8 RGBA frame. `faces` are regions (x, y, w, h as
/// fractions of the frame) known to be skin — the cheek band of a
/// detected face, or where a skin search found one. They are read as
/// they are, not filtered by hue: a face with the wrong colour is the
/// thing we are looking for, and a hue gate would throw it away and
/// report no face. Each is
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
    let (mut skin_h, mut skin_c, mut skin_l): (Vec<f32>, Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new(), Vec::new());
    let (mut clip_hi, mut clip_lo, mut gamut_clip, mut n) = (0usize, 0usize, 0usize, 0usize);
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
        // One channel on the rail while another is nowhere near it: a
        // colour the delivery could not hold, clamped on its way out.
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        if (mx == 255 && mn < 240) || (mn == 0 && mx > 15) {
            gamut_clip += 1;
        }
        if c.l >= 20.0 && c.l <= 90.0 {
            mids.push((c.chroma(), c.a, c.b));
        }
        if skin_candidate(c) && c.chroma() >= SKIN_CHROMA_LOW {
            skin_h.push(c.hue());
            skin_c.push(c.chroma());
            skin_l.push(c.l);
        }
        n += 1;
        i += step;
    }
    let skin_all = (skin_h.len() as f32 >= 0.005 * n.max(1) as f32 && skin_h.len() >= 50).then(|| SkinRead { hue: median(&mut skin_h), chroma: median(&mut skin_c), l: median(&mut skin_l), pct: skin_h.len() as f32 / n.max(1) as f32 * 100.0 });
    // What is actually neutral, first: pixels with almost no colour in
    // them are what a white balance is read from. Only when the frame
    // holds too few — a close-up of grass, a neon sign — does the
    // reading fall back to its least-coloured quarter, and then it says
    // so, because that quarter may be the grass.
    let solid: Vec<&(f32, f32, f32)> = mids.iter().filter(|(c, _, _)| *c < 8.0).collect();
    let enough = solid.len() as f32 >= 0.01 * n.max(1) as f32 && solid.len() >= 40;
    let (mut na, mut nb): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    if enough {
        for (_, a, b) in solid {
            na.push(*a);
            nb.push(*b);
        }
    } else {
        let mut chroma_sorted: Vec<f32> = mids.iter().map(|m| m.0).collect();
        chroma_sorted.sort_by(|a, b| a.total_cmp(b));
        let gate = percentile(&chroma_sorted, 0.25).clamp(6.0, 25.0);
        for (c, a, b) in &mids {
            if *c <= gate {
                na.push(*a);
                nb.push(*b);
            }
        }
    }
    // A grey wall under a tungsten lamp is not "neutral" by chroma any
    // more — the cast moved it — but every part of it moved the same
    // way, and that agreement is what makes it a white reference. Grass
    // against sky never agrees, and its "cast" is the grass.
    let (spread_a, spread_b) = (iqr(&mut na.clone()), iqr(&mut nb.clone()));
    // …and it has to be a plausible white balance. Every lamp from
    // candle to shade moves a grey surface by something under twenty
    // Lab units; a teal backdrop sits further out than that and is a
    // backdrop, not a white reference.
    let candidate_chroma = median(&mut na.iter().zip(&nb).map(|(a, b)| (a * a + b * b).sqrt()).collect::<Vec<_>>());
    let neutral_solid = !na.is_empty() && (enough || (spread_a < 6.0 && spread_b < 6.0 && candidate_chroma < 18.0));
    ls.sort_by(|a, b| a.total_cmp(b));
    chromas.sort_by(|a, b| a.total_cmp(b));
    let nn = n.max(1) as f32;
    let mut faces: Vec<FaceMeasure> = faces.iter().map(|f| measure_face(rgba, w, h, *f)).collect();
    let (noise_l, banding_pct) = flat_field(rgba, w, h);
    // Skin judged as a colourist does: the scene's cast off first.
    let (ca, cb) = (median(&mut na.clone()), median(&mut nb.clone()));
    for f in faces.iter_mut().filter(|f| f.patches > 0) {
        let wb = Lab { l: f.median.l, a: f.median.a - ca, b: f.median.b - cb };
        f.hue_wb = wb.hue();
        f.hue_off_band_wb = hue_off_skin_band(f.hue_wb);
    }
    FrameMeasure {
        l_p1: percentile(&ls, 0.01),
        l_p5: percentile(&ls, 0.05),
        l_p50: percentile(&ls, 0.5),
        l_p95: percentile(&ls, 0.95),
        l_p99: percentile(&ls, 0.99),
        contrast_range: percentile(&ls, 0.95) - percentile(&ls, 0.05),
        clip_hi_pct: clip_hi as f32 / nn * 100.0,
        clip_lo_pct: clip_lo as f32 / nn * 100.0,
        noise_l,
        banding_pct,
        gamut_clip_pct: gamut_clip as f32 / nn * 100.0,
        chroma_mean: if chromas.is_empty() { 0.0 } else { chromas.iter().sum::<f32>() / chromas.len() as f32 },
        chroma_p95: percentile(&chromas, 0.95),
        cast_a: median(&mut na),
        cast_b: median(&mut nb),
        neutral_pct: na.len() as f32 / nn * 100.0,
        neutral_solid,
        faces,
        skin_all,
        sampled: n as u32,
    }
}

/// The white reference in a frame: the average colour, in linear
/// light, of the pixels that are neutral surfaces under the scene's
/// light, chosen as [`measure_frame`] chooses them for its cast reading.
/// Pixels with almost no colour first; failing enough of those, the
/// least-coloured quarter, taken only when it agrees on one cast — a
/// grey wall under a lamp does, grass against sky does not. `None` when
/// the frame has no such surface, and then there is nothing a white
/// balance can honestly be read from.
pub fn neutral_reference(rgba: &[u8], width: u32, height: u32) -> Option<[f32; 3]> {
    neutral_pick(rgba, width, height, false)
}

/// The white reference to *act* on, as Auto White Balance does: what
/// [`neutral_reference`] reads, but a least-coloured quarter counts only
/// when it is nearly grey and not skin coloured. Pale skin, beige sand
/// and a cream wall are smooth surfaces of one colour, as a grey wall
/// under a lamp is, and colour alone cannot tell them apart; reporting
/// that as a possible cast is fine, greying a face is not (#403). A
/// strong cast is then refused rather than guessed at.
pub fn balance_reference(rgba: &[u8], width: u32, height: u32) -> Option<[f32; 3]> {
    neutral_pick(rgba, width, height, true)
}

fn neutral_pick(rgba: &[u8], width: u32, height: u32, acting: bool) -> Option<[f32; 3]> {
    let total = (width as usize) * (height as usize);
    let step = (total / 250_000).max(1);
    // (chroma, a*, b*, linear rgb, skin coloured) of the mid-lightness pixels.
    let mut mids: Vec<(f32, f32, f32, [f32; 3], bool)> = Vec::new();
    let mut n = 0usize;
    let mut i = 0;
    while i < total && rgba.len() >= (i + 1) * 4 {
        let p = i * 4;
        let c = srgb8_to_lab(rgba[p], rgba[p + 1], rgba[p + 2]);
        if c.l >= 20.0 && c.l <= 90.0 {
            let lin = [rgba[p], rgba[p + 1], rgba[p + 2]].map(|v| crate::transfer::srgb_to_linear(v as f32 / 255.0));
            mids.push((c.chroma(), c.a, c.b, lin, skin_candidate(c)));
        }
        n += 1;
        i += step;
    }
    // Skin can never be the white reference of a balance that is
    // applied: it would grey the face.
    let candidates: Vec<_> = mids.iter().filter(|m| !(acting && m.4)).collect();
    let solid: Vec<_> = candidates.iter().copied().filter(|m| m.0 < 8.0).collect();
    let enough = solid.len() as f32 >= 0.01 * n.max(1) as f32 && solid.len() >= 40;
    let picked: Vec<_> = if enough {
        if acting && against_the_frame(&solid, &mids) {
            return None;
        }
        solid
    } else {
        let quarter = least_coloured_quarter(&candidates, 25.0);
        let (mut a, mut b): (Vec<f32>, Vec<f32>) = quarter.iter().map(|m| (m.1, m.2)).unzip();
        let chroma = median(&mut a.iter().zip(&b).map(|(a, b)| (a * a + b * b).sqrt()).collect::<Vec<_>>());
        let ceiling = if acting { 10.0 } else { 18.0 };
        if quarter.is_empty() || iqr(&mut a) >= 6.0 || iqr(&mut b) >= 6.0 || chroma >= ceiling {
            return None;
        }
        quarter
    };
    if picked.is_empty() {
        return None;
    }
    // The picked pixel nearest their median a*/b*: the reference is the
    // typical neutral, as the cast reading is. Averaging them in linear
    // light let the brightest dominate — an L* 89 pixel weighs about
    // seventeen times an L* 25 one — so one white door set the balance
    // of a warm grey room, and a press did not land where `frame_stats`
    // then read the frame as neutral (#405).
    let (mut a, mut b): (Vec<f32>, Vec<f32>) = picked.iter().map(|m| (m.1, m.2)).unzip();
    let (ma, mb) = (median(&mut a), median(&mut b));
    picked
        .iter()
        .min_by(|x, y| ((x.1 - ma).powi(2) + (x.2 - mb).powi(2)).total_cmp(&((y.1 - ma).powi(2) + (y.2 - mb).powi(2))))
        .map(|m| m.3)
}

type Mid = (f32, f32, f32, [f32; 3], bool);

/// The least-coloured quarter of `mids`: every pixel at or under the
/// quarter-way chroma, held between 6 and `most`.
fn least_coloured_quarter<'a>(mids: &[&'a Mid], most: f32) -> Vec<&'a Mid> {
    let mut chroma_sorted: Vec<f32> = mids.iter().map(|m| m.0).collect();
    chroma_sorted.sort_by(|a, b| a.total_cmp(b));
    let gate = percentile(&chroma_sorted, 0.25).clamp(6.0, most);
    mids.iter().copied().filter(|m| m.0 <= gate).collect()
}

/// Whether the near-grey pixels lean the opposite way to the frame's
/// least-coloured surface when that surface is all one colour. Under a
/// strong tungsten cast the grey wall is too orange to count as grey
/// and a pair of jeans lands near neutral, a little blue; balancing on
/// the jeans would warm a frame that is already orange (#403). A cream
/// wall beside a truly grey card looks the same in colour, but a grey
/// card asks for almost nothing, so only a clear lean is refused.
fn against_the_frame(solid: &[&Mid], mids: &[Mid]) -> bool {
    let all: Vec<&Mid> = mids.iter().collect();
    // A strong cast puts a grey wall past chroma 25, so this looks
    // further than the fallback does.
    let quarter = least_coloured_quarter(&all, 45.0);
    let (mut qa, mut qb): (Vec<f32>, Vec<f32>) = quarter.iter().map(|m| (m.1, m.2)).unzip();
    if quarter.is_empty() || iqr(&mut qa) >= 6.0 || iqr(&mut qb) >= 6.0 {
        return false;
    }
    let (mut sa, mut sb): (Vec<f32>, Vec<f32>) = solid.iter().map(|m| (m.1, m.2)).unzip();
    let (sa, sb, qa, qb) = (median(&mut sa), median(&mut sb), median(&mut qa), median(&mut qb));
    let (s_len, q_len) = (sa.hypot(sb), qa.hypot(qb));
    s_len >= 2.0 && q_len > s_len && (sa * qa + sb * qb) / (s_len * q_len) < -0.3
}

/// The flat parts of a picture, where noise and banding show: the
/// lightness spread of the flattest tiles (the noise floor, in L*), and
/// how many of them carry the staircase of banding — a run of identical
/// values ending in a step. Grain hides banding and banding hides in
/// grain, so both are read in the same places.
pub fn flat_field(rgba: &[u8], w: usize, h: usize) -> (f32, f32) {
    const TILE: usize = 24;
    if w < TILE * 2 || h < TILE * 2 || rgba.len() < w * h * 4 {
        return (0.0, 0.0);
    }
    // At most a few hundred tiles, spread over the picture.
    let step_x = (w / TILE / 16).max(1) * TILE;
    let step_y = (h / TILE / 16).max(1) * TILE;
    let luma = |x: usize, y: usize| {
        let p = (y * w + x) * 4;
        srgb8_to_lab(rgba[p], rgba[p + 1], rgba[p + 2]).l
    };
    // Pass one: where the flat tiles are, and how quiet they are.
    let mut flat: Vec<(usize, usize, f32)> = Vec::new();
    let mut y0 = 0;
    while y0 + TILE <= h {
        let mut x0 = 0;
        while x0 + TILE <= w {
            let mut ls = Vec::with_capacity(TILE * TILE);
            for y in y0..y0 + TILE {
                for x in x0..x0 + TILE {
                    ls.push(luma(x, y));
                }
            }
            let mean = ls.iter().sum::<f32>() / ls.len() as f32;
            let sd = (ls.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / ls.len() as f32).sqrt();
            // A tile with detail in it says nothing about noise.
            if sd < 4.0 && mean > 6.0 && mean < 94.0 {
                flat.push((x0, y0, sd));
            }
            x0 += step_x;
        }
        y0 += step_y;
    }
    if flat.is_empty() {
        return (0.0, 0.0);
    }
    let mut spreads: Vec<f32> = flat.iter().map(|t| t.2).collect();
    spreads.sort_by(|a, b| a.total_cmp(b));
    // The floor, not the average: the quietest fifth of the flat tiles.
    let noise = percentile(&spreads, 0.2);

    // Pass two: the staircase. Along a row, a run of one value and then
    // a step out of it.
    //
    // The step has to be large *against the picture's noise floor* for
    // the band to be visible at all — that is what dithering exploits,
    // and why a 0.4 L* step inside grain of 1 L* cannot be seen.
    // Judged against a fixed 0.35 instead, this counted every smooth,
    // clean, low-noise area as banded: twelve pristine reference
    // portraits read between 1% and 46%, which is a measurement of
    // smoothness and not of banding.
    //
    // The floor is the reference rather than each tile's own spread,
    // because a banded tile's steps are most of its spread — judging a
    // step against it would hide exactly what is being looked for.
    let visible_step = (noise * 2.5).max(0.35);
    let mut banded_tiles = 0usize;
    for (x0, y0, _) in &flat {
        let mut banded = false;
        for y in (*y0..*y0 + TILE).step_by(4) {
            let (mut run, mut last) = (0usize, luma(*x0, y));
            for x in *x0 + 1..*x0 + TILE {
                let v = luma(x, y);
                let d = (v - last).abs();
                if d < 0.02 {
                    run += 1;
                } else {
                    if run >= 6 && d > visible_step {
                        banded = true;
                    }
                    run = 0;
                }
                last = v;
            }
        }
        if banded {
            banded_tiles += 1;
        }
    }
    (noise, banded_tiles as f32 / flat.len() as f32 * 100.0)
}

/// A box of a frame as its own picture, for measuring one region: a
/// face, a sky, a product. `region` is (x, y, w, h) in fractions.
pub fn crop(rgba: &[u8], w: usize, h: usize, region: [f32; 4]) -> (Vec<u8>, u32, u32) {
    let x0 = ((region[0] * w as f32).round().max(0.0) as usize).min(w.saturating_sub(1));
    let y0 = ((region[1] * h as f32).round().max(0.0) as usize).min(h.saturating_sub(1));
    let x1 = (((region[0] + region[2]) * w as f32).round() as usize).clamp(x0 + 1, w);
    let y1 = (((region[1] + region[3]) * h as f32).round() as usize).clamp(y0 + 1, h);
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut out = vec![0u8; cw * ch * 4];
    for y in 0..ch {
        let src = ((y0 + y) * w + x0) * 4;
        out[y * cw * 4..(y + 1) * cw * 4].copy_from_slice(&rgba[src..src + cw * 4]);
    }
    (out, cw as u32, ch as u32)
}

/// Whether a picture looks like camera log with no input transform on
/// it: flat, desaturated, black sitting well off zero, nothing near
/// white. Grading that as a finished picture is the classic mistake —
/// contrast and saturation instead of the transform the footage asks
/// for, which throws away the latitude the format exists to keep.
pub fn looks_like_log(m: &FrameMeasure) -> bool {
    // Measured: a graded studio portrait reads contrast 78 chroma 12; the
    // same picture flattened the way a camera log curve flattens it reads
    // contrast 49 chroma 2 with its black sitting at L* 20 and nothing
    // near white.
    m.contrast_range < 55.0
        && m.chroma_mean < 13.0
        && m.l_p1 > 12.0
        && m.l_p99 < 88.0
        && m.clip_hi_pct < 0.05
}

/// Where the craft rules sit, measured on real pictures rather than
/// chosen: a clean studio JPEG reads noise 0.46 L* and the same picture
/// with sensor noise on it 1.49; an 8-bit gradient bands over a quarter
/// of its flat tiles where a dithered one bands none; a portrait pushed
/// to saturation 3.2 pins a channel on 17.8% of its pixels where the
/// original pins none.
pub const NOISE_WARN: f32 = 1.2;
pub const NOISE_FAIL: f32 = 2.5;
/// Banding thresholds, set by what real material reads once a step is
/// judged against the picture's noise floor rather than a fixed number.
///
/// Twelve pristine reference portraits read 0–2%. An 8-bit ffmpeg
/// gradient, which genuinely bands, reads 29%; the dithered version of
/// the same ramp reads 0%. There is no material in between, so the
/// thresholds sit in the gap rather than near either end.
///
/// The earlier pair, 25 and 45, were set against the synthetic gradient
/// alone and never held against a clean photograph. With the old fixed
/// step they made a smooth studio backdrop an *error* on every shot of
/// an ordinary 8-bit interview — found by running the tools end to end
/// on real footage, which is the only way that kind of fault shows.
pub const BANDING_WARN: f32 = 10.0;
pub const BANDING_FAIL: f32 = 25.0;
pub const GAMUT_WARN: f32 = 2.0;
pub const GAMUT_FAIL: f32 = 8.0;

/// One face region as a grid of patches: each patch the median of the
/// skin it holds, kept when enough of it is skin. The region is taken
/// on trust — it is a face — so only what cannot be skin at all is
/// dropped: too dark, too bright, or colourless (eyes, teeth, a
/// specular). Its colour, right or wrong, is what comes back.
pub fn measure_face(rgba: &[u8], w: usize, h: usize, region: [f32; 4]) -> FaceMeasure {
    // A face far away is a few dozen pixels across: an 8×8 grid over it
    // would be patches of four pixels, too few to take a median of. The
    // grid follows the face's size so a patch is always a real sample.
    let px = (region[2] * w as f32).max(1.0) * (region[3] * h as f32).max(1.0);
    let grid: usize = if px >= 120.0 * 120.0 { 8 } else if px >= 40.0 * 40.0 { 5 } else { 3 };
    let x0 = ((region[0] * w as f32).round().max(0.0) as usize).min(w.saturating_sub(1));
    let y0 = ((region[1] * h as f32).round().max(0.0) as usize).min(h.saturating_sub(1));
    let x1 = (((region[0] + region[2]) * w as f32).round() as usize).clamp(x0 + 1, w);
    let y1 = (((region[1] + region[3]) * h as f32).round() as usize).clamp(y0 + 1, h);
    let (rw, rh) = (x1 - x0, y1 - y0);
    // Enough pixels per patch to take a median of, never more than needed.
    let stride = ((rw * rh) / (grid * grid * 64)).max(1);
    let mut patch_l: Vec<f32> = Vec::new();
    let mut patch_h: Vec<f32> = Vec::new();
    let mut patch_c: Vec<f32> = Vec::new();
    let mut patch_ab: Vec<(f32, f32)> = Vec::new();
    for gy in 0..grid {
        for gx in 0..grid {
            let (px0, px1) = (x0 + rw * gx / grid, x0 + rw * (gx + 1) / grid);
            let (py0, py1) = (y0 + rh * gy / grid, y0 + rh * (gy + 1) / grid);
            let (mut ls, mut hs, mut cs, mut as_, mut bs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let mut seen = 0usize;
            let mut k = 0usize;
            for y in py0..py1 {
                for x in px0..px1 {
                    k += 1;
                    if !k.is_multiple_of(stride) {
                        continue;
                    }
                    let p = (y * w + x) * 4;
                    if p + 3 >= rgba.len() {
                        continue;
                    }
                    seen += 1;
                    let c = srgb8_to_lab(rgba[p], rgba[p + 1], rgba[p + 2]);
                    if in_face_candidate(c) {
                        ls.push(c.l);
                        hs.push(c.hue());
                        cs.push(c.chroma());
                        as_.push(c.a);
                        bs.push(c.b);
                    }
                }
            }
            if seen == 0 || (ls.len() as f32) < 0.35 * seen as f32 {
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
        return FaceMeasure { region, patches, grid: grid as u32, median: Lab::default(), hue: 0.0, chroma: 0.0, hue_iqr: 0.0, l_iqr: 0.0, worst_hue: 0.0, mixed_light: false, hue_off_band: 0.0, hue_wb: 0.0, hue_off_band_wb: 0.0 };
    }
    let mut a_all: Vec<f32> = patch_ab.iter().map(|p| p.0).collect();
    let mut b_all: Vec<f32> = patch_ab.iter().map(|p| p.1).collect();
    // The median of a* and b* rather than of the hues: an average of
    // angles either side of 0 is meaningless.
    let median_lab = Lab { l: median(&mut patch_l.clone()), a: median(&mut a_all), b: median(&mut b_all) };
    let hue = median_lab.hue();
    let worst_hue = patch_h.iter().copied().max_by(|a, b| hue_diff(SKIN_HUE_CENTRE, *a).abs().total_cmp(&hue_diff(SKIN_HUE_CENTRE, *b).abs())).unwrap_or(hue);
    let mut about_median: Vec<f32> = patch_h.iter().map(|h| hue_diff(hue, *h)).collect();
    let hue_iqr = iqr(&mut about_median);
    let l_iqr = iqr(&mut patch_l.clone());
    let hue_off_band = hue_off_skin_band(hue);
    FaceMeasure { region, patches, grid: grid as u32, median: median_lab, hue, chroma: median_lab.chroma(), hue_iqr, l_iqr, worst_hue, mixed_light: hue_iqr > 18.0 && patches >= 8, hue_off_band, hue_wb: hue, hue_off_band_wb: hue_off_band }
}

impl FaceMeasure {
    /// Whether the reading stands on enough of the face to judge.
    pub fn readable(&self) -> bool {
        self.patches >= MIN_SKIN_PATCHES.max(self.grid * self.grid / 4)
    }
}

/// The band of a face a colourist samples: the cheeks and the nose,
/// between the eyes and the mouth, as wide as the eyes are apart. It
/// holds almost nothing but skin at any size of face — no hair, no
/// background, no eyes, no lips — which a box round the whole face does
/// not. `landmarks` are the face model's five: right eye, left eye,
/// nose, right mouth corner, left mouth corner, as fractions of the
/// frame. `None` when they do not look like a face.
pub fn skin_band(landmarks: &[(f32, f32)]) -> Option<[f32; 4]> {
    let [re, le, _nose, rm, lm] = landmarks.get(..5)? else { return None };
    let eye_y = (re.1 + le.1) / 2.0;
    let mouth_y = (rm.1 + lm.1) / 2.0;
    let drop = mouth_y - eye_y;
    let span = (le.0 - re.0).abs().max((lm.0 - rm.0).abs());
    if !(drop > 0.0005) || !(span > 0.0005) {
        return None;
    }
    // Down from just under the eyes to just above the lips, and a little
    // wider than the eyes are apart: cheek, nose, cheek.
    let (top, bottom) = (eye_y + drop * 0.18, mouth_y - drop * 0.18);
    let cx = (re.0 + le.0) / 2.0;
    let w = (span * 1.15).min(1.0);
    let x = (cx - w / 2.0).clamp(0.0, 1.0 - w);
    let h = (bottom - top).max(0.004).min(1.0 - top.clamp(0.0, 1.0));
    Some([x, top.clamp(0.0, 1.0), w, h])
}

/// Where the skin is inside a detected person's box: a coarse grid of
/// patches over the box, the skin patches' cluster in its upper part
/// (the face, for a person facing the camera; a window with a video of
/// one, too). `None` when the box holds no skin cluster worth reading.
pub fn skin_region(rgba: &[u8], width: u32, height: u32, person: [f32; 4]) -> Option<[f32; 4]> {
    const GRID: usize = 12;
    let (w, h) = (width as usize, height as usize);
    let [px, py, pw, ph] = person;
    let (x0, y0) = (((px * w as f32).max(0.0)) as usize, ((py * h as f32).max(0.0)) as usize);
    let (x1, y1) = ((((px + pw) * w as f32) as usize).clamp(x0 + 1, w), (((py + ph) * h as f32) as usize).clamp(y0 + 1, h));
    let (rw, rh) = (x1 - x0, y1 - y0);
    let stride = ((rw * rh) / (GRID * GRID * 48)).max(1);
    let mut skin = [false; GRID * GRID];
    for gy in 0..GRID {
        for gx in 0..GRID {
            let (ax, bx) = (x0 + rw * gx / GRID, x0 + rw * (gx + 1) / GRID);
            let (ay, by) = (y0 + rh * gy / GRID, y0 + rh * (gy + 1) / GRID);
            let (mut seen, mut hit, mut k) = (0usize, 0usize, 0usize);
            for y in ay..by {
                for x in ax..bx {
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
                    if skin_candidate(c) && c.chroma() >= SKIN_CHROMA_LOW {
                        hit += 1;
                    }
                }
            }
            skin[gy * GRID + gx] = seen > 0 && hit as f32 >= 0.5 * seen as f32;
        }
    }
    // The largest connected cluster of skin patches, the upper one when
    // two are alike (a face over hands).
    let mut seen = [false; GRID * GRID];
    let mut best: Option<(usize, f32, [usize; 4])> = None;
    for start in 0..GRID * GRID {
        if !skin[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        let mut cells = Vec::new();
        seen[start] = true;
        while let Some(c) = stack.pop() {
            cells.push(c);
            let (cx, cy) = (c % GRID, c / GRID);
            for (nx, ny) in [(cx.wrapping_sub(1), cy), (cx + 1, cy), (cx, cy.wrapping_sub(1)), (cx, cy + 1)] {
                if nx < GRID && ny < GRID {
                    let n = ny * GRID + nx;
                    if skin[n] && !seen[n] {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        let (mut gx0, mut gy0, mut gx1, mut gy1) = (GRID, GRID, 0, 0);
        for c in &cells {
            gx0 = gx0.min(c % GRID);
            gx1 = gx1.max(c % GRID);
            gy0 = gy0.min(c / GRID);
            gy1 = gy1.max(c / GRID);
        }
        let centre_y = (gy0 + gy1) as f32 / 2.0;
        let score = cells.len() as f32 * (1.0 + (GRID as f32 - centre_y) / GRID as f32 * 0.5);
        if best.is_none_or(|(_, s, _)| score > s) {
            best = Some((cells.len(), score, [gx0, gy0, gx1 + 1, gy1 + 1]));
        }
    }
    let (n, _, [gx0, gy0, gx1, gy1]) = best?;
    if n < 3 {
        return None;
    }
    let cell = |g: usize, len: usize| g as f32 / GRID as f32 * len as f32;
    Some([
        (x0 as f32 + cell(gx0, rw)) / w as f32,
        (y0 as f32 + cell(gy0, rh)) / h as f32,
        cell(gx1 - gx0, rw) / w as f32,
        cell(gy1 - gy0, rh) / h as f32,
    ])
}

/// The head of a detected person: the upper part of the box, the
/// middle of its width. The guess when the picture is not to hand;
/// `skin_region` reads where the skin is.
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
    /// How light a face should be, L*. With a face in the picture this
    /// is what the exposure is set by — a portrait against a dark
    /// backdrop has a low median by design, and driving that median to
    /// a number blows the face. The frame's median only leads when
    /// there is no face to expose for.
    #[serde(default)]
    pub skin_l: Option<f32>,
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
            "broadcast_neutral" | "broadcast" | "neutral" => LookTarget { black_l: Some(4.0), white_l: Some(95.0), mid_l: Some(48.0), contrast_range: Some(65.0), chroma_mean: Some(22.0), cast_a: Some(0.0), cast_b: Some(0.0), skin_l: Some(62.0), ..base("broadcast_neutral") },
            "web_punchy" | "punchy" | "web" => LookTarget { black_l: Some(3.0), white_l: Some(97.0), mid_l: Some(50.0), contrast_range: Some(75.0), chroma_mean: Some(30.0), cast_a: Some(0.0), cast_b: Some(0.0), skin_l: Some(63.0), ..base("web_punchy") },
            "film_soft" | "film" | "filmic" => LookTarget { black_l: Some(8.0), white_l: Some(92.0), mid_l: Some(46.0), contrast_range: Some(55.0), chroma_mean: Some(18.0), cast_a: Some(1.0), cast_b: Some(3.0), skin_l: Some(58.0), ..base("film_soft") },
            "warm_interview" | "warm" | "interview" => LookTarget { black_l: Some(5.0), white_l: Some(94.0), mid_l: Some(50.0), contrast_range: Some(62.0), chroma_mean: Some(24.0), cast_a: Some(2.0), cast_b: Some(5.0), skin_hue: Some(50.0), skin_l: Some(62.0), ..base("warm_interview") },
            "cool_tech" | "cool" | "tech" => LookTarget { black_l: Some(4.0), white_l: Some(95.0), mid_l: Some(47.0), contrast_range: Some(68.0), chroma_mean: Some(20.0), cast_a: Some(-1.0), cast_b: Some(-5.0), skin_l: Some(60.0), ..base("cool_tech") },
            "screencast" | "screen" => LookTarget { cast_a: Some(0.0), cast_b: Some(0.0), screencast: true, ..base("screencast") },
            // No brief: only what is right for every scene — nothing
            // clipped or crushed, neutrals neutral, black near black, a
            // face exposed as a face. A median, a white point, a contrast
            // range and a saturation are choices a scene makes: a noon
            // beach is bright and pale, a night street dark, and judging
            // both against a studio key failed every shot of a correctly
            // exposed exterior (#328).
            "unbriefed" => LookTarget { black_l: Some(4.0), cast_a: Some(0.0), cast_b: Some(0.0), skin_l: Some(62.0), ..base("unbriefed") },
            _ => return None,
        })
    }

    pub const PRESETS: [&'static str; 6] = ["broadcast_neutral", "web_punchy", "film_soft", "warm_interview", "cool_tech", "screencast"];

    /// What a frame is judged against when the piece has no brief: see
    /// the `unbriefed` preset. Not in [`Self::PRESETS`], which lists the
    /// looks a brief can ask for.
    pub fn unbriefed() -> LookTarget {
        LookTarget::preset("unbriefed").expect("the unbriefed preset exists")
    }

    /// A target lifted from measured frames: the mean of their readings.
    pub fn from_measures(name: &str, measures: &[FrameMeasure]) -> Option<LookTarget> {
        if measures.is_empty() {
            return None;
        }
        let n = measures.len() as f32;
        let mean = |f: &dyn Fn(&FrameMeasure) -> f32| measures.iter().map(f).sum::<f32>() / n;
        let skin: Vec<f32> = measures.iter().flat_map(|m| m.faces.iter().filter(|f| f.readable()).map(|f| f.hue)).collect();
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
            skin_l: {
                let ls: Vec<f32> = measures.iter().flat_map(|m| m.faces.iter().filter(|f| f.readable()).map(|f| f.median.l)).collect();
                (!ls.is_empty()).then(|| ls.iter().sum::<f32>() / ls.len() as f32)
            },
            screencast: false,
        })
    }
}

/// How far two shots are from matching, in ΔE-like units.
///
/// "Matching" is the job that fills most of a colourist's day and it is
/// not the same as "graded well": two shots can each be defensible on
/// their own and still cut badly, because the eye tracks a change
/// across a cut far more readily than an absolute error inside a shot.
/// So the number that matters between neighbours is a *difference*, not
/// a distance from a target.
///
/// **What is compared is the light, not the people.** A scene holds an
/// interview and its reverse, a two-shot, a cutaway — different faces,
/// legitimately different complexions. Comparing two faces' colours
/// directly would call a correctly lit pair of shots a bad match
/// because one person is darker than the other, which is both wrong and
/// the kind of wrong that is worth being careful about. (This was the
/// first version of this function, and twelve real portraits caught
/// it: it scored two well-exposed shots from one session 40 ΔE apart.)
///
/// So the face enters as a difference of *errors*: how far each face
/// sits off the skin band once the scene's own cast is taken off. Two
/// faces both sitting on the band match, whoever they belong to; the
/// same face on the band in one shot and 10° off it in the next does
/// not. That is what a colourist actually checks — balance to neutral,
/// then see that skin falls where skin falls.
///
/// What the eye notices across a cut, in the order it notices it:
///
/// 1. **Exposure.** A jump in level reads as a cut to a different take.
///    About 1 L* is the threshold on a smooth pan and 3 L* across a
///    hard cut, so L* enters at roughly its own scale.
/// 2. **Skin sitting differently against its band**, weighted highest
///    per degree, because a face that has gone ruddy in one shot and
///    sallow in the next is the fault a viewer names even when they
///    cannot say why.
/// 3. **The cast on the neutrals** — the wall, the paper, the shirt.
///    Forgiven more: nobody knows what colour the wall was.
/// 4. **Contrast**, which the eye is most forgiving of, and which is
///    partly a property of the shot's own content rather than its grade.
///
/// The result is comparable with a ΔE: under [`MATCH_GOOD`] the cut is
/// invisible, over [`MATCH_WARN`] a viewer sees the join.
/// The lightness a match should read the exposure from, for a *pair* of
/// shots — the same kind of reading from both, or none.
///
/// The frame's median is what fills most of the picture, so it moves
/// when the *framing* moves and not only when the light does. Measured
/// on the test footage, a big close-up of the same subject under the
/// same lamp read 10.6 ΔE from its own source shot on the median
/// alone, past the scene split, so the two would have been called
/// different setups. The subject's own level did not move at all:
/// 51.1 against 51.2.
///
/// Two readings of the subject exist and they are **not
/// interchangeable**: the detected face is the median of the cheek
/// band, and the skin fallback is the median of every skin-coloured
/// pixel, which includes foreheads and specular highlights and so runs
/// systematically lighter. Comparing one against the other invents a
/// difference. So the pair has to agree on a basis: both faces, else
/// both skin, else neither and the frame leads.
///
/// The skin fallback earns its place because a face that *fills* the
/// frame defeats the detector, which is trained on smaller ones — and
/// that is precisely the framing that breaks the median.
fn subject_pair(a: &FrameMeasure, b: &FrameMeasure) -> Option<(f32, f32)> {
    let face = |m: &FrameMeasure| m.faces.iter().find(|f| f.readable()).map(|f| f.median.l);
    if let (Some(x), Some(y)) = (face(a), face(b)) {
        return Some((x, y));
    }
    let skin = |m: &FrameMeasure| m.skin_all.as_ref().filter(|s| s.pct >= 2.0).map(|s| s.l);
    match (skin(a), skin(b)) {
        (Some(x), Some(y)) => Some((x, y)),
        _ => None,
    }
}

/// How far two shots are from matching, in ΔE-like units.
///
/// "Matching" is the job that fills most of a colourist's day and it is
/// not the same as "graded well": two shots can each be defensible on
/// their own and still cut badly, because the eye tracks a change
/// across a cut far more readily than an absolute error inside a shot.
/// So the number that matters between neighbours is a *difference*, not
/// a distance from a target.
///
/// **What is compared is the light, not the people, and not the
/// framing.** Two blind spots have to be worked around, and each of
/// them was found by measuring real footage rather than by reasoning:
///
/// *A different person is not a mismatch.* A scene holds an interview
/// and its reverse, a two-shot, a cutaway. The first version of this
/// compared two faces' colours in ΔE2000 directly and scored two
/// well-exposed shots from one session 40 ΔE apart because one person
/// was darker than the other. The face now enters as a difference of
/// *errors*: how far each sits off the skin band once the scene's own
/// cast is taken off. Two faces both on the band match, whoever they
/// belong to.
///
/// *A different framing is not a mismatch either.* The frame's median
/// is whatever fills the picture, so it moves when the camera moves in
/// and not only when the light does: on the test footage a big
/// close-up of one subject under one lamp read 10.6 ΔE from its own
/// source shot, while the subject's own level did not move at all.
/// The exposure is read two ways, from the subject and from the whole
/// frame, and the smaller difference is the one that counts. Each
/// reading has its own blind spot — re-framing fools the frame, a new
/// subject fools the face — and neither fools both, so a difference is
/// only believed when both ways of looking agree there is one. A
/// genuine change in the light moves both.
///
/// *A cast can only be compared where there are neutrals to read it
/// off.* When a face fills the frame the "neutrals" are the skin, and
/// the cast reading becomes a reading of the complexion. `neutral_solid`
/// already says when that has happened, so the cast term drops out
/// rather than lying; the skin-band term still catches a light that has
/// changed colour, because skin turns with it.
///
/// What the eye notices across a cut, in the order it notices it:
///
/// 1. **Exposure.** A jump in level reads as a cut to a different take.
///    About 1 L* is the threshold on a smooth pan and 3 L* across a
///    hard cut, so L* enters at roughly its own scale.
/// 2. **Skin sitting differently against its band**, weighted highest
///    per degree, because a face that has gone ruddy in one shot and
///    sallow in the next is the fault a viewer names even when they
///    cannot say why.
/// 3. **The cast on the neutrals** — the wall, the paper, the shirt.
///    Forgiven more: nobody knows what colour the wall was.
/// 4. **Contrast**, which the eye is most forgiving of, and which is
///    partly a property of the shot's own content rather than its grade.
///
/// The result is comparable with a ΔE: under [`MATCH_GOOD`] the cut is
/// invisible, over [`MATCH_WARN`] a viewer sees the join. A 2 ΔE shift
/// on skin crosses the first threshold on its own; the same shift on
/// the neutrals does not.
pub fn match_distance(a: &FrameMeasure, b: &FrameMeasure) -> f32 {
    let face_a = a.faces.iter().find(|f| f.readable());
    let face_b = b.faces.iter().find(|f| f.readable());
    let face = match (face_a, face_b) {
        (Some(x), Some(y)) => {
            // How differently each face sits against the band, with the
            // scene's own cast taken off — a difference of errors, so
            // two different people both graded right come out at zero.
            let off = (x.hue_off_band_wb - y.hue_off_band_wb).abs();
            // Skin saturation does vary by complexion, but a grade
            // moves it far more than a person does, so it enters
            // lightly rather than not at all.
            let chroma = (x.chroma - y.chroma).abs();
            off + chroma * 0.25
        }
        // Only one shot has a face: the faces cannot be compared, so the
        // rest of the picture carries the whole judgement.
        _ => 0.0,
    };
    let frame_exposure = (a.l_p50 - b.l_p50).abs();
    let exposure = match subject_pair(a, b) {
        Some((x, y)) => (x - y).abs().min(frame_exposure),
        None => frame_exposure,
    };
    let cast = if a.neutral_solid && b.neutral_solid {
        let (da, db) = (a.cast_a - b.cast_a, a.cast_b - b.cast_b);
        (da * da + db * db).sqrt()
    } else {
        0.0
    };
    let contrast = (a.contrast_range - b.contrast_range).abs();
    // The weights are the order above, scaled so the result reads like
    // a ΔE. A 3 L* step in exposure lands between the two thresholds,
    // which is where a hard cut starts to read as a change rather than
    // an edit. Skin counts about twice the neutrals per unit of error,
    // which is the long-standing finding about memory colours: skin,
    // sky and foliage carry tighter acceptability tolerances than an
    // arbitrary colour, because the viewer is comparing them with a
    // memory rather than with the shot beside them. Contrast is
    // discounted hardest: much of it is the shot's own content.
    face * 1.2 + exposure + cast * 0.6 + contrast * 0.2
}

/// Two shots this close cut together without the join showing. A ΔE2000
/// of about 1 is the textbook just-noticeable difference for patches
/// side by side, and a cut is the hardest side-by-side there is.
pub const MATCH_GOOD: f32 = 2.0;
/// Past this a viewer sees the cut as a change, not as an edit.
pub const MATCH_WARN: f32 = 4.0;
/// Past this the two shots are not the same lighting setup at all, and
/// asking one to match the other is asking for a broken grade rather
/// than a matched one — they belong to different scenes.
pub const SCENE_SPLIT: f32 = 9.0;

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
        // With a face in the picture the exposure is judged on the face,
        // not on the frame: a portrait against a dark backdrop has a low
        // median by design, and a grade that chases the median blows the
        // face off the top.
        let face_l = m.faces.iter().find(|f| f.readable()).map(|f| f.median.l);
        match (face_l, t.skin_l) {
            (Some(l), Some(want)) => out.push(graded("skin_exposure", l, want, 7.0, 14.0, format!("the face is L* {l:.0} (target {want:.0}; the frame's median is {:.0})", m.l_p50), "exposure, until the face sits right — the frame's median follows the picture, not a number")),
            _ => {
                if let Some(mid) = t.mid_l {
                    out.push(graded("mid_grey", m.l_p50, mid, 6.0, 12.0, format!("median L* {:.0} (target {mid:.0})", m.l_p50), "exposure or gamma"));
                }
            }
        }
        if let Some(c) = t.contrast_range {
            out.push(graded("contrast", m.contrast_range, c, 8.0, 16.0, format!("5–95 lightness range {:.0} (target {c:.0})", m.contrast_range), "contrast"));
        }
        // Likewise saturation: a face's own chroma says more than the
        // frame's, which a red jumper or a green wall dominates.
        if face_l.is_none() {
            if let Some(c) = t.chroma_mean {
                out.push(graded("saturation", m.chroma_mean, c, 5.0, 10.0, format!("mean chroma {:.0} (target {c:.0})", m.chroma_mean), "saturation, or vibrance to keep skin"));
            }
        }
    }
    if let (Some(ta), Some(tb)) = (t.cast_a, t.cast_b) {
        if m.neutral_pct >= 1.0 {
            let off = ((m.cast_a - ta).powi(2) + (m.cast_b - tb).powi(2)).sqrt();
            let dir = cast_name(m.cast_a - ta, m.cast_b - tb);
            // Without a true neutral in the frame there is nothing to
            // balance to, so the reading is a note, never a failure.
            let verdict = if !m.neutral_solid {
                if off > 3.0 { Verdict::Warn } else { Verdict::Pass }
            } else if off > 8.0 {
                Verdict::Fail
            } else if off > 3.0 {
                Verdict::Warn
            } else {
                Verdict::Pass
            };
            let how = if m.neutral_solid {
                format!("{:.0}% of the frame is neutral", m.neutral_pct)
            } else {
                "nothing in the frame is neutral, so this is its least-coloured quarter and may be the subject's own colour".to_string()
            };
            out.push(Finding { rule: "cast", verdict, reading: off, target: 0.0, tolerance: 3.0, what: format!("neutrals sit {off:.1} toward {dir} (a* {:.1}, b* {:.1}; {how})", m.cast_a, m.cast_b), fix: "temperature (b*) and tint (a*) until the neutrals read a* b* near the target" });
        }
    }
    // Camera log with no input transform on it: everything below would
    // be read against a picture that is not meant to be looked at yet.
    if looks_like_log(m) {
        out.push(Finding {
            rule: "looks_like_log",
            verdict: Verdict::Warn,
            reading: m.contrast_range,
            target: 0.0,
            tolerance: 0.0,
            what: format!("this looks like camera log with no input transform: flat (5–95 range {:.0}), almost no colour (chroma {:.0}), black sitting at L* {:.0} and nothing near white", m.contrast_range, m.chroma_mean, m.l_p1),
            fix: "set the clip's log format so the transform is applied, then grade; contrast and saturation on top of log throw away the latitude the format exists to keep",
        });
    }
    let level = |v: f32, warn: f32, fail: f32| if v > fail { Verdict::Fail } else if v > warn { Verdict::Warn } else { Verdict::Pass };
    if m.noise_l > NOISE_WARN {
        out.push(Finding { rule: "noise", verdict: level(m.noise_l, NOISE_WARN, NOISE_FAIL), reading: m.noise_l, target: 0.0, tolerance: NOISE_WARN, what: format!("the flat parts of the picture carry {:.1} L* of noise", m.noise_l), fix: "denoise before lifting the shadows — a lift multiplies what is already there" });
    }
    if m.banding_pct > BANDING_WARN {
        out.push(Finding { rule: "banding", verdict: level(m.banding_pct, BANDING_WARN, BANDING_FAIL), reading: m.banding_pct, target: 0.0, tolerance: BANDING_WARN, what: format!("{:.0}% of the flat picture steps in bands", m.banding_pct), fix: "less stretch in the shadows, or a touch of grain to break the steps" });
    }
    if m.gamut_clip_pct > GAMUT_WARN {
        out.push(Finding { rule: "gamut", verdict: level(m.gamut_clip_pct, GAMUT_WARN, GAMUT_FAIL), reading: m.gamut_clip_pct, target: 0.0, tolerance: GAMUT_WARN, what: format!("{:.1}% of the picture has a channel pinned at the rail — colour the delivery cannot hold", m.gamut_clip_pct), fix: "saturation down, or pull the offending colour back with a secondary" });
    }
    let skin_target = t.skin_hue.unwrap_or(SKIN_HUE_CENTRE);
    // A whole-frame skin read is advisory, and never on a screen
    // recording, whose orange buttons are not skin.
    if !t.screencast && m.faces.iter().all(|f| !f.readable()) {
        if let Some(s) = m.skin_all {
            let off = hue_off_skin_band(s.hue);
            if s.pct >= 2.0 && off > 6.0 {
                out.push(Finding { rule: "skin_hue", verdict: Verdict::Warn, reading: s.hue, target: skin_target, tolerance: 6.0, what: format!("skin-coloured pixels ({:.0}% of the frame, no face detected) sit at hue {:.0}°, {off:.0}° outside the skin band", s.pct, s.hue), fix: "if a face is in the picture, analyze with the detector for a proper read; else temperature and tint" });
            }
        }
    }
    for (i, f) in m.faces.iter().enumerate() {
        if !f.readable() {
            continue;
        }
        let who = if m.faces.len() > 1 { format!("face {}", i + 1) } else { "the face".to_string() };
        // Judged as shot. The hue with the scene's cast taken off is
        // reported beside it, never instead of it: that correction is
        // only as good as the neutral estimate, and a room with no true
        // neutral in it — wood, warm walls, a beer — gives a bad one.
        let scene_cast = m.cast() >= 3.0;
        let (hue, off) = (f.hue, f.hue_off_band);
        let with_cast = if scene_cast { format!(" (the scene carries a {:.0} cast; without it the skin would read {:.0}°)", m.cast(), f.hue_wb) } else { String::new() };
        // Off the band is what matters; inside it, the distance to the
        // look's own skin hue is advisory.
        let (verdict, what) = if f.mixed_light {
            (Verdict::Warn, format!("{who} is lit by two colours of light (hue spread {:.0}°): median hue {:.0}°", f.hue_iqr, f.hue))
        } else if off > 10.0 {
            (Verdict::Fail, format!("{who}: skin hue {hue:.0}°, {off:.0}° outside the skin band ({SKIN_HUE_LOW:.0}–{SKIN_HUE_HIGH:.0}°){with_cast}"))
        } else if off > 4.0 {
            (Verdict::Warn, format!("{who}: skin hue {hue:.0}°, {off:.0}° outside the skin band{with_cast}"))
        } else {
            (Verdict::Pass, format!("{who}: skin hue {hue:.0}° on the band (chroma {:.0}, L* {:.0}, {} patches){with_cast}", f.chroma, f.median.l, f.patches))
        };
        out.push(Finding { rule: "skin_hue", verdict, reading: hue, target: skin_target, tolerance: 4.0, what, fix: if scene_cast { "put the white balance right first (temperature and tint, or grade_to_target); skin_balance for what is left" } else { "skin_balance" } });
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
    // Never brighten a picture that is already losing its highlights:
    // chasing a mid-grey target by exposure alone blows the window and
    // the sky, and no colourist trades detail for a number.
    let clipping = m.clip_hi_pct > 1.0;
    if !t.screencast {
        if let Some(b) = t.black_l {
            // L* at the bottom: about 0.012 lift per L*.
            s.lift = ((b - m.l_p1) * 0.012 * DAMP).clamp(-0.3, 0.3);
        }
        if let Some(wv) = t.white_l {
            s.gain = ((wv - m.l_p99) * 0.010 * DAMP).clamp(-0.3, 0.3);
        }
        // Expose for the face when there is one, else for the frame.
        let face = m.faces.iter().find(|f| f.readable());
        let (have, want) = match (face, t.skin_l) {
            (Some(f), Some(l)) => (Some(f.median.l), Some(l)),
            _ => (Some(m.l_p50), t.mid_l),
        };
        if let (Some(have), Some(want)) = (have, want) {
            // A stop of exposure moves a lightness by about 15 L*.
            s.exposure = ((want - have) / 15.0 * DAMP).clamp(-1.5, 1.5);
            if clipping {
                s.exposure = s.exposure.min(0.0);
                s.gain = s.gain.min(0.0);
            }
        }
        match face {
            // Saturation metered on the face: skin that has gone past
            // the band it can live in is what "over-cooked" means, and
            // the frame's mean chroma is whatever the subject wore.
            Some(f) if !(SKIN_CHROMA_LOW..=SKIN_CHROMA_HIGH).contains(&f.chroma) && f.chroma > 1.0 => {
                let want = (SKIN_CHROMA_LOW + SKIN_CHROMA_HIGH) / 2.0;
                s.saturation = (want / f.chroma - 1.0) * DAMP;
            }
            Some(_) => {}
            None => {
                if let Some(c) = t.chroma_mean {
                    if m.chroma_mean > 1.0 {
                        s.saturation = (c / m.chroma_mean - 1.0) * DAMP;
                    }
                }
            }
        }
    }
    if let (Some(ta), Some(tb)) = (t.cast_a, t.cast_b) {
        if m.neutral_pct >= 1.0 && m.neutral_solid {
            // In mireds of assumed light, as the knobs take them: the
            // white balance moves a neutral about 5 Lab units per 25, so
            // five mireds a unit. b* is warmth (temperature), a* is
            // green–magenta (tint).
            s.temperature = ((tb - m.cast_b) * 5.0 * DAMP).clamp(-50.0, 50.0);
            s.tint = ((ta - m.cast_a) * 5.0 * DAMP).clamp(-50.0, 50.0);
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

    /// Hues are a circle: a face at 358° is two degrees under red, not
    /// 358 above it, and it is 42° outside the skin band.
    #[test]
    fn hue_distances_go_round_the_circle() {
        assert!((hue_diff(358.0, 2.0) - 4.0).abs() < 1e-4);
        assert!((hue_diff(2.0, 358.0) + 4.0).abs() < 1e-4);
        assert!((hue_diff(SKIN_HUE_CENTRE, 45.0)).abs() < 1e-4);
        assert_eq!(hue_off_skin_band(50.0), 0.0);
        // The old tape's magenta face, 26° under the band's floor.
        assert!((hue_off_skin_band(359.0) - 26.0).abs() < 1e-4, "{}", hue_off_skin_band(359.0));
        assert!((hue_off_skin_band(85.0) - 15.0).abs() < 1e-4);
        assert!((hue_off_skin_band(15.0) - 10.0).abs() < 1e-4);
    }

    /// The wheel's hue and Lab's are different circles: a skin
    /// secondary keyed on the wrong one selects nothing. Lab 52° is the
    /// wheel's low twenties; a magenta-cast face is up near 340.
    #[test]
    fn a_lab_hue_becomes_the_wheel_hue_a_qualifier_keys_on() {
        let skin = lab_at(SKIN_HUE_CENTRE, 25.0, 60.0);
        let wheel = srgb_hue(lab_to_srgb8(skin));
        assert!((10.0..40.0).contains(&wheel), "skin on the wheel: {wheel}");
        let cast = srgb8_to_lab(200, 156, 169);
        let cast_wheel = srgb_hue([200, 156, 169]);
        assert!(cast.hue() > 340.0, "the tape's pink in Lab: {}", cast.hue());
        assert!(cast_wheel > 320.0 && cast_wheel < 360.0, "and on the wheel: {cast_wheel}");
        // Round trip: a Lab colour survives being turned into sRGB.
        let back = srgb8_to_lab(lab_to_srgb8(skin)[0], lab_to_srgb8(skin)[1], lab_to_srgb8(skin)[2]);
        assert!(delta_e2000(skin, back) < 1.0, "{skin:?} vs {back:?}");
    }

    /// With a face in the picture the exposure and the saturation are
    /// metered on it, not on the frame: a portrait against a dark
    /// backdrop is not under-exposed, and a red jumper is not
    /// over-saturated skin.
    #[test]
    fn a_face_meters_the_exposure_and_the_saturation() {
        let (w, h) = (200, 120);
        let face = [0.4f32, 0.35, 0.2, 0.3];
        // A dark room, a well-lit face: the median is low by design.
        let frame = frame_of(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            if fx >= face[0] && fx < face[0] + face[2] && fy >= face[1] && fy < face[1] + face[3] { [175, 138, 118] } else { [28, 28, 30] }
        });
        let m = measure_frame(&frame, w as u32, h as u32, &[face]);
        let look = LookTarget::preset("broadcast_neutral").unwrap();
        assert!(m.l_p50 < 20.0 && m.faces[0].median.l > 60.0, "dark frame, bright face: {:.0} {:.0}", m.l_p50, m.faces[0].median.l);
        let step = primaries_toward(&m, &look);
        assert!(step.exposure.abs() < 0.25, "the face is already right, so the exposure holds: {:+.2}", step.exposure);
        let findings = judge(&m, &look);
        assert!(findings.iter().any(|f| f.rule == "skin_exposure" && f.verdict == Verdict::Pass), "{findings:?}");
        assert!(findings.iter().all(|f| f.rule != "mid_grey"), "the frame's median is not the rule when a face is there");
        // A face cooked past its band pulls the saturation down.
        let hot = frame_of(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            if fx >= face[0] && fx < face[0] + face[2] && fy >= face[1] && fy < face[1] + face[3] { [255, 120, 60] } else { [28, 28, 30] }
        });
        let m2 = measure_frame(&hot, w as u32, h as u32, &[face]);
        assert!(m2.faces[0].chroma > SKIN_CHROMA_HIGH, "cooked: {:.0}", m2.faces[0].chroma);
        assert!(primaries_toward(&m2, &look).saturation < -0.1, "pull it back");
    }

    /// Noise, banding and out-of-gamut colour, on pictures made to
    /// carry exactly those faults. The numbers come from real ones:
    /// see the constants.
    #[test]
    fn noise_banding_and_gamut_read_what_they_are_named_for() {
        let (w, h) = (240, 160);
        // A clean ramp: no noise, but 8-bit steps across a dark gradient
        // are what banding is.
        //
        // The plateaus are wider than a tile, which is what a real
        // banded gradient looks like: most tiles sit inside one flat
        // step, so the picture's noise floor is measured off them and
        // is near zero, and the tiles that straddle a boundary show the
        // step against it. A ramp steep enough to put a step inside
        // every tile has no quiet tiles to set the floor from, and its
        // steps are then not visible — which the detector is right to
        // pass, and which is why this ramp is gentle.
        let ramp = frame_of(w, h, |x, _| {
            let v = 20 + (x / 40) as u8;
            [v, v, (v as f32 * 1.1) as u8]
        });
        let m = measure_frame(&ramp, w as u32, h as u32, &[]);
        assert!(m.noise_l < 0.6, "a clean ramp has no noise: {:.2}", m.noise_l);
        assert!(m.banding_pct > BANDING_WARN, "an 8-bit ramp bands: {:.0}%", m.banding_pct);
        // The same ramp with a pixel of dither: the steps break up.
        let dithered = frame_of(w, h, |x, y| {
            let v = (20 + (x / 40) as i32 + ((x * 7 + y * 13) % 3) as i32 - 1).clamp(0, 255) as u8;
            [v, v, (v as f32 * 1.1) as u8]
        });
        let m2 = measure_frame(&dithered, w as u32, h as u32, &[]);
        assert!(m2.banding_pct < m.banding_pct, "dither breaks the staircase: {:.0}% vs {:.0}%", m2.banding_pct, m.banding_pct);
        assert!(m2.noise_l > m.noise_l, "and reads as noise instead");
        // A colour past what the delivery holds: a channel on the rail
        // while the others are nowhere near it.
        let hot = frame_of(w, h, |x, _| if x < w / 2 { [255, 40, 30] } else { [120, 120, 120] });
        let m3 = measure_frame(&hot, w as u32, h as u32, &[]);
        assert!(m3.gamut_clip_pct > GAMUT_FAIL, "{:.1}%", m3.gamut_clip_pct);
        assert!(judge(&m3, &LookTarget::preset("broadcast_neutral").unwrap()).iter().any(|f| f.rule == "gamut" && f.verdict == Verdict::Fail));
    }

    /// Camera log with no input transform is named as such, and a
    /// graded picture is not.
    #[test]
    fn log_without_its_transform_is_recognised() {
        let (w, h) = (200, 120);
        // Flat, desaturated, black off the floor, nothing near white.
        let log = frame_of(w, h, |x, y| {
            let v = (95.0 + (x as f32 / w as f32) * 90.0 + ((y % 5) as f32)) as u8;
            [v, v, (v as f32 * 0.99) as u8]
        });
        let m = measure_frame(&log, w as u32, h as u32, &[]);
        assert!(looks_like_log(&m), "{m:?}");
        assert!(judge(&m, &LookTarget::preset("broadcast_neutral").unwrap()).iter().any(|f| f.rule == "looks_like_log"));
        // A normal picture: blacks near zero, whites near the top, colour in it.
        let graded = frame_of(w, h, |x, y| {
            let v = ((x as f32 / w as f32) * 250.0) as u8;
            if y < h / 2 { [v, (v as f32 * 0.6) as u8, 30] } else { [v, v, v] }
        });
        let m2 = measure_frame(&graded, w as u32, h as u32, &[]);
        assert!(!looks_like_log(&m2), "{m2:?}");
    }

    /// A region reads on its own: the face, not the frame it sits in.
    #[test]
    fn a_region_is_measured_by_itself() {
        let (w, h) = (200, 120);
        let face = [0.4f32, 0.35, 0.2, 0.3];
        let frame = frame_of(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            if fx >= face[0] && fx < face[0] + face[2] && fy >= face[1] && fy < face[1] + face[3] { [194, 150, 130] } else { [60, 60, 62] }
        });
        let whole = measure_frame(&frame, w as u32, h as u32, &[]);
        let (cut, cw, ch) = crop(&frame, w, h, face);
        let region = measure_frame(&cut, cw, ch, &[]);
        assert!(region.l_p50 > 60.0 && whole.l_p50 < 40.0, "the face is lighter than the frame: {:.0} vs {:.0}", region.l_p50, whole.l_p50);
        // The frame has a neutral to read, and reads neutral; the face
        // on its own has none, and reads as its own warm colour.
        assert!(whole.neutral_solid && whole.cast() < 2.0, "the frame: {:?}", (whole.cast_a, whole.cast_b));
        assert!(!region.neutral_solid && region.cast_b > 5.0, "the face alone: {:?}", (region.cast_a, region.cast_b));
    }

    /// A picture that is already clipping is never brightened further,
    /// whatever the mid-grey target says.
    #[test]
    fn the_step_does_not_brighten_what_is_already_clipping() {
        let (w, h) = (60, 40);
        // Dark overall, with a blown window in the corner.
        let frame = frame_of(w, h, |x, y| if x > 48 && y < 12 { [255, 255, 255] } else { [40, 40, 42] });
        let m = measure_frame(&frame, w as u32, h as u32, &[]);
        assert!(m.clip_hi_pct > 1.0 && m.l_p50 < 30.0, "clipping {:.1}% median {:.0}", m.clip_hi_pct, m.l_p50);
        let step = primaries_toward(&m, &LookTarget::preset("broadcast_neutral").unwrap());
        assert!(step.exposure <= 0.0 && step.gain <= 0.0, "not brighter: {step:?}");
        // The same picture without the window: brightening is right.
        let dark = frame_of(w, h, |_, _| [40, 40, 42]);
        let m2 = measure_frame(&dark, w as u32, h as u32, &[]);
        assert!(primaries_toward(&m2, &LookTarget::preset("broadcast_neutral").unwrap()).exposure > 0.3);
    }

    /// A frame with real neutrals in it reads its cast off them; one
    /// with none says so and does not fail for it.
    #[test]
    fn a_cast_is_read_off_true_neutrals_or_not_trusted() {
        let (w, h) = (120, 80);
        // Half grey, half skin: a real neutral to balance to.
        let grey = frame_of(w, h, |x, _| if x < w / 2 { [128, 128, 128] } else { [194, 150, 130] });
        let m = measure_frame(&grey, w as u32, h as u32, &[]);
        assert!(m.neutral_solid, "grey is neutral");
        assert!(m.cast() < 1.5, "{} {}", m.cast_a, m.cast_b);
        // The same, warmed: still neutral enough to read, and the warmth
        // is named. (A "neutral" is under chroma 8, so a readable cast
        // off true neutrals is a few units, not twenty.)
        let warm = frame_of(w, h, |x, _| if x < w / 2 { [137, 131, 124] } else { [210, 150, 110] });
        let m2 = measure_frame(&warm, w as u32, h as u32, &[]);
        assert!(m2.neutral_solid && m2.cast() > 3.5 && m2.cast_b > 0.0, "warm: {:?}", (m2.cast_a, m2.cast_b, m2.neutral_solid));
        assert!(judge(&m2, &LookTarget::preset("broadcast_neutral").unwrap()).iter().any(|f| f.rule == "cast" && f.verdict != Verdict::Pass));
        // Nothing neutral at all: grass and sky. The reading is kept but
        // never a failure, and it does not drive the grade.
        let colourful = frame_of(w, h, |x, _| if x < w / 2 { [40, 120, 40] } else { [60, 90, 190] });
        let m3 = measure_frame(&colourful, w as u32, h as u32, &[]);
        assert!(!m3.neutral_solid, "no true neutral here");
        assert!(judge(&m3, &LookTarget::preset("broadcast_neutral").unwrap()).iter().all(|f| f.rule != "cast" || f.verdict != Verdict::Fail));
        assert_eq!(primaries_toward(&m3, &LookTarget::preset("broadcast_neutral").unwrap()).temperature, 0.0);
    }

    /// A face with a magenta cast is read and reported off the band,
    /// not thrown away for not looking like skin.
    #[test]
    fn a_face_with_the_wrong_colour_is_still_read() {
        let (w, h) = (200, 120);
        let face = [0.4f32, 0.4, 0.2, 0.2];
        // The washed-out pink an old tape gives skin: a* 18, b* 0.
        let frame = frame_of(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            if fx >= face[0] && fx < face[0] + face[2] && fy >= face[1] && fy < face[1] + face[3] { [200, 156, 169] } else { [120, 120, 120] }
        });
        let m = measure_frame(&frame, w as u32, h as u32, &[face]);
        let f = &m.faces[0];
        assert!(f.readable(), "{} of {}x{} patches", f.patches, f.grid, f.grid);
        assert!(f.hue > 340.0 || f.hue < 20.0, "a magenta face: {}", f.hue);
        assert!(f.hue_off_band > 20.0, "far outside the band: {}", f.hue_off_band);
        let findings = judge(&m, &LookTarget::preset("broadcast_neutral").unwrap());
        let skin = findings.iter().find(|x| x.rule == "skin_hue").expect("a skin finding");
        assert_eq!(skin.verdict, Verdict::Fail, "{skin:?}");
    }

    /// The cheek band sits under the eyes, over the lips, and between
    /// the cheeks; bad landmarks give none.
    #[test]
    fn the_cheek_band_is_between_the_eyes_and_the_mouth() {
        let marks = [(0.40, 0.30), (0.52, 0.30), (0.46, 0.38), (0.42, 0.46), (0.50, 0.46)];
        let [x, y, w, h] = skin_band(&marks).expect("a band");
        assert!(y > 0.30 && y < 0.34, "under the eyes: {y}");
        assert!(y + h > 0.42 && y + h < 0.46, "over the lips: {}", y + h);
        assert!(x < 0.40 && x + w > 0.52, "wider than the eyes: {x}..{}", x + w);
        assert!(skin_band(&[(0.4, 0.4), (0.5, 0.4), (0.45, 0.4), (0.42, 0.39), (0.48, 0.39)]).is_none(), "a mouth above the eyes is not a face");
        assert!(skin_band(&marks[..3]).is_none());
    }

    /// A person box that is a window with a face in its lower middle:
    /// the skin search finds the face where the head guess finds none.
    #[test]
    fn the_skin_search_finds_a_face_inside_a_window() {
        let (w, h) = (320, 240);
        let person = [0.3f32, 0.1, 0.4, 0.8];
        let face = [0.42f32, 0.45, 0.16, 0.2];
        let frame = frame_of(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            if fx >= face[0] && fx < face[0] + face[2] && fy >= face[1] && fy < face[1] + face[3] { [194, 150, 130] } else if fx >= person[0] && fx < person[0] + person[2] && fy >= person[1] && fy < person[1] + person[3] { [30, 30, 40] } else { [120, 120, 120] }
        });
        let guess = measure_face(&frame, w, h, head_region(person));
        assert!(guess.patches < 4, "the head guess reads the window's top: {}", guess.patches);
        let region = skin_region(&frame, w as u32, h as u32, person).expect("a skin cluster");
        assert!((region[0] - face[0]).abs() < 0.06 && (region[1] - face[1]).abs() < 0.08, "{region:?}");
        let read = measure_face(&frame, w, h, region);
        assert!(read.readable() && (SKIN_HUE_LOW..=SKIN_HUE_HIGH).contains(&read.hue), "{read:?}");
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
        assert!(f.readable(), "{} of {}x{} patches", f.patches, f.grid, f.grid);
        assert!((SKIN_HUE_LOW..=SKIN_HUE_HIGH).contains(&f.hue), "hue {}", f.hue);
        assert!(m.cast() < 1.5, "neutral grey reads neutral: {} {}", m.cast_a, m.cast_b);
        let findings = judge(&m, &LookTarget::preset("broadcast_neutral").unwrap());
        assert!(findings.iter().all(|f| f.rule != "skin_hue" || f.verdict == Verdict::Pass), "{findings:?}");
        // Under a blue cast: the walls stay neutral enough to balance to,
        // and the face turns with the light.
        let cast = frame_of(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            if fx >= face[0] && fx < face[0] + face[2] && fy >= face[1] && fy < face[1] + face[3] { [150, 150, 190] } else { [116, 120, 132] }
        });
        let m2 = measure_frame(&cast, w as u32, h as u32, &[face]);
        assert!(m2.neutral_solid && m2.cast_b < -4.0, "a blue cast reads as negative b*: {} (solid {})", m2.cast_b, m2.neutral_solid);
        assert_eq!(cast_name(m2.cast_a, m2.cast_b), "blue (cool)");
        let step = primaries_toward(&m2, &LookTarget::preset("broadcast_neutral").unwrap());
        assert!(step.temperature > 0.0, "warm it up: {step:?}");
    }
}

#[cfg(test)]
mod match_tests {
    use super::*;

    fn flat(l: f32, a: f32, b: f32) -> FrameMeasure {
        FrameMeasure {
            l_p1: (l - 30.0).max(0.0),
            l_p50: l,
            l_p99: (l + 30.0).min(100.0),
            contrast_range: 50.0,
            chroma_mean: 12.0,
            cast_a: a,
            cast_b: b,
            neutral_solid: true,
            ..Default::default()
        }
    }

    fn with_face(mut m: FrameMeasure, skin: Lab) -> FrameMeasure {
        m.faces = vec![FaceMeasure {
            region: [0.4, 0.2, 0.2, 0.3],
            patches: 9,
            grid: 3,
            median: skin,
            hue: skin.hue(),
            chroma: skin.chroma(),
            hue_iqr: 3.0,
            l_iqr: 4.0,
            worst_hue: skin.hue(),
            mixed_light: false,
            hue_off_band: hue_off_skin_band(skin.hue()),
            hue_wb: skin.hue(),
            hue_off_band_wb: hue_off_skin_band(skin.hue()),
        }];
        m
    }

    #[test]
    fn a_shot_matches_itself() {
        let m = with_face(flat(50.0, 0.0, 0.0), Lab { l: 60.0, a: 14.0, b: 16.0 });
        assert!(match_distance(&m, &m) < 0.001);
        assert!(match_distance(&m, &m) < MATCH_GOOD);
    }

    #[test]
    fn two_different_people_lit_the_same_way_match() {
        // The fault twelve real portraits caught in the first version
        // of this: an interview and its reverse hold two complexions,
        // and calling that a bad match is both wrong and the kind of
        // wrong worth being careful about. Both faces sit on the band,
        // so both are right, so the shots match.
        let pale = with_face(flat(50.0, 0.0, 0.0), Lab { l: 72.0, a: 12.0, b: 15.0 });
        let dark = with_face(flat(50.0, 0.0, 0.0), Lab { l: 34.0, a: 13.0, b: 16.0 });
        assert!(pale.faces[0].hue_off_band_wb == 0.0 && dark.faces[0].hue_off_band_wb == 0.0, "both on the band to begin with");
        let d = match_distance(&pale, &dark);
        assert!(d < MATCH_GOOD, "two complexions, one light: {d:.2}");
    }

    #[test]
    fn one_face_off_the_band_and_one_on_it_does_not_match() {
        // The same person, ruddy in one shot and not in the next: the
        // fault a viewer names without being able to say why.
        let ok = with_face(flat(50.0, 0.0, 0.0), Lab { l: 60.0, a: 14.0, b: 16.0 });
        let ruddy = with_face(flat(50.0, 0.0, 0.0), Lab { l: 60.0, a: 30.0, b: 6.0 });
        assert!(ruddy.faces[0].hue_off_band_wb > 8.0, "the fixture is off the band: {}", ruddy.faces[0].hue_off_band_wb);
        assert!(match_distance(&ok, &ruddy) > MATCH_GOOD, "{}", match_distance(&ok, &ruddy));
    }

    #[test]
    fn a_wall_that_changes_colour_is_forgiven_more_than_a_face_that_does() {
        // Per degree of error: nobody knows what colour the wall was.
        let base = with_face(flat(50.0, 0.0, 0.0), Lab { l: 60.0, a: 14.0, b: 16.0 });
        let face_off = with_face(flat(50.0, 0.0, 0.0), Lab { l: 60.0, a: 26.0, b: 8.0 });
        let wall_off = with_face(flat(50.0, 8.0, 0.0), Lab { l: 60.0, a: 14.0, b: 16.0 });
        let off_deg = face_off.faces[0].hue_off_band_wb;
        let (df, dw) = (match_distance(&base, &face_off), match_distance(&base, &wall_off));
        assert!(df / off_deg > dw / 8.0, "per degree the face costs more: face {df:.2} over {off_deg:.1}°, wall {dw:.2} over 8");
    }

    #[test]
    fn an_exposure_step_across_a_cut_is_seen() {
        // Three L* between neighbours is about where a hard cut starts
        // to read as a change rather than an edit.
        let a = flat(50.0, 0.0, 0.0);
        let far = flat(58.0, 0.0, 0.0);
        assert!(match_distance(&a, &far) > MATCH_WARN, "{}", match_distance(&a, &far));
        let near = flat(51.0, 0.0, 0.0);
        assert!(match_distance(&a, &near) < MATCH_GOOD, "{}", match_distance(&a, &near));
    }

    #[test]
    fn re_framing_the_same_shot_is_not_a_mismatch() {
        // A big close-up of the subject and a wider shot of the same
        // setup: the frame's median moves a long way because the median
        // is whatever fills the frame, and the light has not changed at
        // all. Measured on the test footage this read 10.6 ΔE on the
        // median alone, past the scene split.
        let wide = with_face(flat(30.0, 0.0, 0.0), Lab { l: 51.0, a: 14.0, b: 16.0 });
        let close = with_face(flat(44.0, 0.0, 0.0), Lab { l: 51.0, a: 14.0, b: 16.0 });
        let d = match_distance(&wide, &close);
        assert!(d < MATCH_GOOD, "the same light framed two ways: {d:.2}");
    }

    #[test]
    fn a_real_exposure_change_still_moves_both_readings() {
        // The subject and the frame darken together, so the smaller of
        // the two differences is still a real one.
        let lit = with_face(flat(30.0, 0.0, 0.0), Lab { l: 51.0, a: 14.0, b: 16.0 });
        let dim = with_face(flat(25.0, 0.0, 0.0), Lab { l: 46.0, a: 14.0, b: 16.0 });
        let d = match_distance(&lit, &dim);
        assert!(d > MATCH_WARN, "half a stop down is a mismatch: {d:.2}");
    }

    #[test]
    fn a_frame_with_no_neutrals_is_not_judged_on_its_cast() {
        // A face filling the frame makes the "neutrals" the skin, and
        // the cast reading becomes a reading of the complexion. The
        // measurement already says when that has happened.
        let wall = flat(50.0, 0.0, 0.0);
        let mut no_wall = flat(50.0, 9.0, 4.0);
        no_wall.neutral_solid = false;
        assert!(match_distance(&wall, &no_wall) < MATCH_GOOD, "a cast that cannot be read is not compared");
        // …and where both frames do hold neutrals, it is.
        let real_cast = flat(50.0, 9.0, 4.0);
        assert!(match_distance(&wall, &real_cast) > MATCH_GOOD);
    }

    #[test]
    fn skin_carries_the_exposure_when_the_detector_misses_the_face() {
        // A face that fills the frame defeats the detector, which is
        // trained on smaller ones — and that is precisely the framing
        // that breaks the frame median, so the fallback matters.
        let skin = |l: f32, pct: f32| Some(SkinRead { hue: 40.0, chroma: 20.0, l, pct });
        let mut wide = flat(30.0, 0.0, 0.0);
        wide.skin_all = skin(55.0, 12.0);
        let mut close = flat(44.0, 0.0, 0.0);
        close.skin_all = skin(55.0, 44.0);
        assert!(match_distance(&wide, &close) < MATCH_GOOD, "{}", match_distance(&wide, &close));
        // Too little skin to be the subject: the frame leads again.
        let mut sliver = flat(44.0, 0.0, 0.0);
        sliver.skin_all = skin(55.0, 0.4);
        assert!(match_distance(&wide, &sliver) > MATCH_WARN);
    }

    #[test]
    fn a_face_reading_is_never_compared_with_a_skin_reading() {
        // The cheek-band median and the all-skin median are different
        // measurements — the second includes foreheads and highlights
        // and runs lighter — so comparing one against the other invents
        // a difference. With no basis in common the frame leads, which
        // here means the two are judged different, as they should be.
        let detected = with_face(flat(30.0, 0.0, 0.0), Lab { l: 51.0, a: 14.0, b: 16.0 });
        let mut fallback = flat(48.0, 0.0, 0.0);
        fallback.skin_all = Some(SkinRead { hue: 40.0, chroma: 20.0, l: 51.0, pct: 44.0 });
        // 18 L* apart on the frame, and no common subject reading to
        // overrule it: the pair is not quietly called a match.
        assert!(match_distance(&detected, &fallback) > MATCH_WARN);
    }

    #[test]
    fn a_different_lighting_setup_reads_as_a_different_scene() {
        // Daylight interview against a tungsten one: not a match to be
        // nudged, a scene of its own.
        let day = with_face(flat(55.0, -1.0, -6.0), Lab { l: 62.0, a: 12.0, b: 14.0 });
        let tungsten = with_face(flat(42.0, 6.0, 18.0), Lab { l: 52.0, a: 20.0, b: 30.0 });
        assert!(match_distance(&day, &tungsten) > SCENE_SPLIT, "{}", match_distance(&day, &tungsten));
    }

    #[test]
    fn contrast_is_forgiven_more_than_level() {
        let a = flat(50.0, 0.0, 0.0);
        let mut contrasty = flat(50.0, 0.0, 0.0);
        contrasty.contrast_range = 62.0;
        let mut brighter = flat(50.0, 0.0, 0.0);
        brighter.l_p50 = 62.0;
        assert!(match_distance(&a, &contrasty) < match_distance(&a, &brighter));
    }

    #[test]
    fn one_face_and_no_other_leaves_the_judgement_to_the_rest() {
        // A cutaway with nobody in it cannot be matched on skin; the
        // comparison must not silently score it as a perfect face match
        // nor invent an error.
        let a = with_face(flat(50.0, 0.0, 0.0), Lab { l: 60.0, a: 14.0, b: 16.0 });
        let b = flat(50.0, 0.0, 0.0);
        assert!(match_distance(&a, &b) < 0.001);
        let c = flat(56.0, 0.0, 0.0);
        assert!((match_distance(&a, &c) - match_distance(&b, &c)).abs() < 0.001, "the missing face changes nothing");
    }
}

#[cfg(test)]
mod unbriefed_tests {
    use super::*;

    /// #328: a correctly exposed bright exterior — median L* 65, pale,
    /// white point short of 95 — fails the broadcast target and passes
    /// with no brief, which still catches clipping and a cast.
    #[test]
    fn a_bright_exterior_passes_without_a_brief_and_faults_still_show() {
        let beach: Vec<u8> = (0..100 * 100)
            .flat_map(|i| if i < 4000 { [150u8, 190, 230, 255] } else { [200, 190, 172, 255] })
            .collect();
        let m = measure_frame(&beach, 100, 100, &[]);
        let fails = |t: &LookTarget| judge(&m, t).into_iter().filter(|f| f.verdict == Verdict::Fail).map(|f| f.rule).collect::<Vec<_>>();
        assert!(!fails(&LookTarget::preset("broadcast_neutral").unwrap()).is_empty(), "the studio target fails it");
        let unbriefed: Vec<_> = judge(&m, &LookTarget::unbriefed()).into_iter().filter(|f| f.verdict != Verdict::Pass).map(|f| f.rule).collect();
        for rule in ["mid_grey", "white_point", "contrast", "saturation"] {
            assert!(!unbriefed.contains(&rule), "{rule} judged with no brief: {unbriefed:?}");
        }

        let blown: Vec<u8> = [255u8, 255, 255, 255].repeat(100 * 100);
        let m = measure_frame(&blown, 100, 100, &[]);
        assert!(judge(&m, &LookTarget::unbriefed()).iter().any(|f| f.rule == "clipped_white" && f.verdict == Verdict::Fail));
    }
}
