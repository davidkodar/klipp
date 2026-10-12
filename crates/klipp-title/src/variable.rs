//! Every weight of a family, and variable fonts, for titles (#1504).
//!
//! A face's weight and slope come from its OS/2 table and a variable
//! face's axes from its fvar table, read with ttf-parser. A text box
//! that names a weight is set in the family's face nearest it, picked
//! as CSS picks one ([`nearest_face`]).
//!
//! fontdue draws a variable face's default instance only. A face set
//! elsewhere on its axes is an `Instance`: harfrust shapes it at the
//! instance's coordinates, and its glyphs are drawn here, each outline
//! read at those coordinates and filled with a coverage rasteriser of
//! the kind fontdue's is (signed area accumulated along each row).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fontdue::{Metrics, OutlineBounds};
use serde::{Deserialize, Serialize};
use ttf_parser::{GlyphId, OutlineBuilder, Tag};

/// Instance glyphs cached per font, before the cache is emptied.
const CACHE_CAP: usize = 1024;

/// One axis of a variable face, as the font picker offers it: its
/// four-letter tag ("wght", "wdth", "slnt", "opsz") and its range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariationAxis {
    pub tag: String,
    pub min: f32,
    pub default: f32,
    pub max: f32,
}

/// What a face is within its family: its weight, 100 to 900, whether
/// it is italic or oblique, and its axes (none for a static face).
#[derive(Debug, Clone, PartialEq)]
pub struct FaceTraits {
    pub weight: u16,
    pub italic: bool,
    pub axes: Vec<VariationAxis>,
}

impl FaceTraits {
    /// The weights the face draws: its own, or a variable face's weight
    /// axis's range.
    pub fn weights(&self) -> (u16, u16) {
        match self.axes.iter().find(|a| a.tag == "wght") {
            Some(a) => (a.min.round().clamp(1.0, 1000.0) as u16, a.max.round().clamp(1.0, 1000.0) as u16),
            None => (self.weight, self.weight),
        }
    }
}

/// The traits of the face in `data`; `None` for a file ttf-parser
/// cannot read.
pub fn face_traits(data: &[u8]) -> Option<FaceTraits> {
    let face = ttf_parser::Face::parse(data, 0).ok()?;
    let axes = face
        .variation_axes()
        .into_iter()
        .map(|a| VariationAxis { tag: tag_name(a.tag), min: a.min_value, default: a.def_value, max: a.max_value })
        .collect();
    Some(FaceTraits { weight: face.weight().to_number().clamp(1, 1000), italic: face.is_italic() || face.is_oblique(), axes })
}

fn tag_name(tag: Tag) -> String {
    String::from_utf8_lossy(&tag.to_bytes()).into_owned()
}

/// The face of `faces` CSS picks for `weight` and `italic` (CSS Fonts
/// 4, font matching): faces of the slope asked for if the family has
/// any, else of the other; then the face whose weight is `weight`, or
/// else for a weight from 400 to 500 the nearest up to 500, then the
/// nearest below, then the nearest above 500; for a weight under 400
/// the nearest below, then the nearest above; for one over 500 the
/// nearest above, then the nearest below. A face that draws a range of
/// weights (a variable face) draws any weight in it. `None` for none.
pub fn nearest_face(faces: &[FaceTraits], weight: u16, italic: bool) -> Option<usize> {
    let slope = if faces.iter().any(|f| f.italic == italic) { italic } else { !italic };
    let w = weight as i32;
    faces
        .iter()
        .enumerate()
        .filter(|(_, f)| f.italic == slope)
        .min_by_key(|(_, f)| {
            let (lo, hi) = f.weights();
            let e = w.clamp(lo as i32, hi as i32);
            match w {
                400..=500 if (w..=500).contains(&e) => (0, e - w),
                400..=500 if e < w => (1, w - e),
                400..=500 => (2, e),
                _ if w < 400 && e <= w => (0, w - e),
                _ if w < 400 => (1, e - w),
                _ if e >= w => (0, e - w),
                _ => (1, w - e),
            }
        })
        .map(|(i, _)| i)
}

/// A variable face set on its axes away from its default instance:
/// the coordinates, in each axis's units, and the glyphs drawn there.
pub(crate) struct Instance {
    coords: Vec<(Tag, f32)>,
    /// Mixed into the glyph keys of the instance's glyphs, so a glyph
    /// drawn at one instance is not taken for another's.
    pub(crate) hash: u32,
    cache: Mutex<HashMap<(u16, u32), Arc<(Metrics, Vec<u8>)>>>,
}

impl Instance {
    /// The face in `data` at `variations`, each `(tag, value)` an axis
    /// it has, clamped to the axis's range, the last for a tag set
    /// twice winning. `None` for a static face, or one every axis of
    /// which is left at its default: fontdue draws that instance.
    pub(crate) fn new(data: &[u8], variations: &[(String, f32)]) -> Option<Self> {
        let face = ttf_parser::Face::parse(data, 0).ok()?;
        let mut coords: Vec<(Tag, f32)> = Vec::new();
        for axis in face.variation_axes() {
            let Some((_, value)) = variations.iter().rev().find(|(t, _)| *t == tag_name(axis.tag)) else { continue };
            let value = value.clamp(axis.min_value, axis.max_value);
            if value != axis.def_value {
                coords.push((axis.tag, value));
            }
        }
        if coords.is_empty() {
            return None;
        }
        let hash = coords
            .iter()
            .flat_map(|(t, v)| t.to_bytes().into_iter().chain(v.to_bits().to_le_bytes()))
            .fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
        Some(Self { coords, hash, cache: Mutex::new(HashMap::new()) })
    }

    /// The coordinates, for the shaper.
    pub(crate) fn variations(&self) -> Vec<(harfrust::Tag, f32)> {
        self.coords.iter().map(|(t, v)| (harfrust::Tag::new(&t.to_bytes()), *v)).collect()
    }

    pub(crate) fn face<'a>(&self, data: &'a [u8]) -> Option<ttf_parser::Face<'a>> {
        let mut face = ttf_parser::Face::parse(data, 0).ok()?;
        for &(tag, value) in &self.coords {
            face.set_variation(tag, value);
        }
        Some(face)
    }

    /// Glyph `gid` of the face in `data` at the instance, at `px`
    /// pixels per em: its metrics as fontdue gives a glyph's, the
    /// advance the instance's, and its coverage. A glyph without an
    /// outline (a space) has an empty box.
    pub(crate) fn glyph(&self, data: &[u8], gid: u16, px: f32) -> Arc<(Metrics, Vec<u8>)> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.get(&(gid, px.to_bits())) {
            return hit.clone();
        }
        if cache.len() >= CACHE_CAP {
            cache.clear();
        }
        let drawn = Arc::new(self.face(data).map(|face| draw(&face, gid, px)).unwrap_or_default());
        cache.insert((gid, px.to_bits()), drawn.clone());
        drawn
    }
}

/// Glyph `gid` of `face` drawn at `px`: its outline scaled to pixels
/// (y up), its box the outline's whole pixels out from the outline as
/// fontdue's metrics give it, and its coverage rows top down.
fn draw(face: &ttf_parser::Face, gid: u16, px: f32) -> (Metrics, Vec<u8>) {
    let scale = px / face.units_per_em().max(1) as f32;
    let advance_width = face.glyph_hor_advance(GlyphId(gid)).unwrap_or(0) as f32 * scale;
    let mut outline = Outline { scale, lines: Vec::new(), start: (0.0, 0.0), pen: (0.0, 0.0) };
    face.outline_glyph(GlyphId(gid), &mut outline);
    let mut lines = outline.lines;
    if lines.is_empty() {
        return (Metrics { advance_width, ..Metrics::default() }, Vec::new());
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &((ax, ay), (bx, by)) in &lines {
        x0 = x0.min(ax).min(bx);
        y0 = y0.min(ay).min(by);
        x1 = x1.max(ax).max(bx);
        y1 = y1.max(ay).max(by);
    }
    let (xmin, ymin) = (x0.floor(), y0.floor());
    let (width, height) = ((x1.ceil() - xmin) as usize, (y1.ceil() - ymin) as usize);
    let top = ymin + height as f32;
    // Into the bitmap: x from its left edge, y down from its top.
    for line in &mut lines {
        for p in [&mut line.0, &mut line.1] {
            *p = (p.0 - xmin, top - p.1);
        }
    }
    let metrics = Metrics {
        xmin: xmin as i32,
        ymin: ymin as i32,
        width,
        height,
        advance_width,
        advance_height: 0.0,
        bounds: OutlineBounds { xmin: x0, ymin: y0, width: x1 - x0, height: y1 - y0 },
    };
    (metrics, fill(&lines, width, height))
}

/// An outline flattened to lines, in pixels.
struct Outline {
    scale: f32,
    lines: Vec<((f32, f32), (f32, f32))>,
    start: (f32, f32),
    pen: (f32, f32),
}

impl Outline {
    fn to(&mut self, p: (f32, f32)) {
        self.lines.push((self.pen, p));
        self.pen = p;
    }

    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.scale, y * self.scale)
    }
}

/// Pieces a curve whose control points stray `dev` pixels from its
/// chord is flattened into: a small fraction of a pixel off at most.
fn pieces(dev: f32) -> usize {
    1 + (3.0 * dev * dev).sqrt().sqrt().min(64.0) as usize
}

impl OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.start = self.at(x, y);
        self.pen = self.start;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.to(p);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (p0, p1, p2) = (self.pen, self.at(x1, y1), self.at(x, y));
        let n = pieces(((p0.0 - 2.0 * p1.0 + p2.0).powi(2) + (p0.1 - 2.0 * p1.1 + p2.1).powi(2)).sqrt());
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            self.to((u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0, u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1));
        }
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (p0, p1, p2, p3) = (self.pen, self.at(x1, y1), self.at(x2, y2), self.at(x, y));
        let dev = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| ((a.0 - 2.0 * b.0 + c.0).powi(2) + (a.1 - 2.0 * b.1 + c.1).powi(2)).sqrt();
        let n = pieces(1.5 * dev(p0, p1, p2).max(dev(p1, p2, p3)));
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.to((a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0, a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1));
        }
    }

    fn close(&mut self) {
        if self.pen != self.start {
            self.to(self.start);
        }
    }
}

/// The coverage of `lines` (closed contours, in a `width` x `height`
/// box, y down) as 8-bit alpha: each line adds the signed area it
/// covers to the cells it crosses, and a running sum along the rows
/// fills between the contours, an overlap clamped to full.
fn fill(lines: &[((f32, f32), (f32, f32))], width: usize, height: usize) -> Vec<u8> {
    // A cell past the last, for a line on the right edge.
    let mut acc = vec![0.0f32; width * height + 2];
    for &(p0, p1) in lines {
        if (p0.1 - p1.1).abs() <= f32::EPSILON {
            continue;
        }
        let (dir, p0, p1) = if p0.1 < p1.1 { (1.0, p0, p1) } else { (-1.0, p1, p0) };
        let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
        let mut x = p0.0;
        let y_start = p0.1.max(0.0) as usize;
        if p0.1 < 0.0 {
            x -= p0.1 * dxdy;
        }
        let y_end = (p1.1.ceil().max(0.0) as usize).min(height);
        for y in y_start..y_end {
            let row = y * width;
            let dy = ((y + 1) as f32).min(p1.1) - (y as f32).max(p0.1);
            let x_next = x + dxdy * dy;
            let d = dy * dir;
            let (xa, xb) = if x < x_next { (x, x_next) } else { (x_next, x) };
            let (xa, xb) = (xa.clamp(0.0, width as f32), xb.clamp(0.0, width as f32));
            let xa_floor = xa.floor();
            let xa_i = xa_floor as usize;
            let xb_i = xb.ceil() as usize;
            if xb_i <= xa_i + 1 {
                // Within one cell: its area split there and the next.
                let mid = 0.5 * (xa + xb) - xa_floor;
                acc[row + xa_i] += d - d * mid;
                acc[row + xa_i + 1] += d * mid;
            } else {
                let s = (xb - xa).recip();
                let xa_f = xa - xa_floor;
                let a0 = 0.5 * s * (1.0 - xa_f) * (1.0 - xa_f);
                let xb_f = xb - xb.ceil() + 1.0;
                let am = 0.5 * s * xb_f * xb_f;
                acc[row + xa_i] += d * a0;
                if xb_i == xa_i + 2 {
                    acc[row + xa_i + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - xa_f);
                    acc[row + xa_i + 1] += d * (a1 - a0);
                    for xi in xa_i + 2..xb_i - 1 {
                        acc[row + xi] += d * s;
                    }
                    let a2 = a1 + (xb_i - xa_i - 3) as f32 * s;
                    acc[row + xb_i - 1] += d * (1.0 - a2 - am);
                }
                acc[row + xb_i] += d * am;
            }
            x = x_next;
        }
    }
    let mut sum = 0.0f32;
    acc[..width * height]
        .iter()
        .map(|a| {
            sum += a;
            (sum.abs().min(1.0) * 255.0 + 0.5) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(weight: u16, italic: bool) -> FaceTraits {
        FaceTraits { weight, italic, axes: Vec::new() }
    }

    /// CSS's order: from 400 to 500 up to 500 first, then down, then
    /// up past 500; under 400 down first; over 500 up first.
    #[test]
    fn the_nearest_weight_is_the_one_css_picks() {
        let faces: Vec<FaceTraits> = [300, 400, 600, 700, 900].into_iter().map(|w| face(w, false)).collect();
        let pick = |w| faces[nearest_face(&faces, w, false).unwrap()].weight;
        assert_eq!(pick(650), 700, "over 500, the nearest heavier first");
        assert_eq!(pick(800), 900);
        assert_eq!(pick(950), 900, "nothing heavier: the nearest lighter");
        assert_eq!(pick(450), 400, "nothing up to 500: the nearest lighter");
        assert_eq!(pick(500), 400);
        assert_eq!(pick(350), 300, "under 400, the nearest lighter first");
        assert_eq!(pick(200), 300, "nothing lighter: the nearest heavier");
        assert_eq!(pick(400), 400);
        let light: Vec<FaceTraits> = [100, 600].into_iter().map(|w| face(w, false)).collect();
        assert_eq!(light[nearest_face(&light, 450, false).unwrap()].weight, 100, "below before past 500");
        let mid: Vec<FaceTraits> = [400, 500, 700].into_iter().map(|w| face(w, false)).collect();
        assert_eq!(mid[nearest_face(&mid, 450, false).unwrap()].weight, 500, "up to 500 before below");
    }

    /// An italic is picked from the italics, a family without one
    /// gives its upright faces; a variable face draws any weight in
    /// its range.
    #[test]
    fn the_slope_comes_first_and_a_range_draws_its_weights() {
        let faces = vec![face(400, false), face(700, false), face(400, true)];
        assert_eq!(nearest_face(&faces, 700, true), Some(2));
        assert_eq!(nearest_face(&faces[..2], 700, true), Some(1));
        let wght = VariationAxis { tag: "wght".into(), min: 100.0, default: 400.0, max: 900.0 };
        let faces = vec![face(700, false), FaceTraits { weight: 400, italic: false, axes: vec![wght] }];
        assert_eq!(nearest_face(&faces, 650, false), Some(1));
        assert_eq!(nearest_face(&[], 400, false), None);
    }

    /// A glyph drawn here is drawn as fontdue draws it: the same box
    /// and advance, and coverage close to fontdue's.
    #[test]
    fn a_glyph_draws_as_fontdue_draws_it() {
        for (family, style) in [("Inter", "Regular"), ("Libre Baskerville", "Italic"), ("Bebas Neue", "Regular")] {
            let data = crate::bundled::lookup(family, style).unwrap();
            let font = fontdue::Font::from_bytes(data, fontdue::FontSettings::default()).unwrap();
            let face = ttf_parser::Face::parse(data, 0).unwrap();
            for c in "agQ&8%W".chars() {
                for px in [13.0, 48.0, 101.5] {
                    let gid = font.lookup_glyph_index(c);
                    let (theirs, their_cov) = font.rasterize_indexed(gid, px);
                    let (ours, our_cov) = draw(&face, gid, px);
                    assert_eq!((ours.xmin, ours.ymin, ours.width, ours.height), (theirs.xmin, theirs.ymin, theirs.width, theirs.height), "{family} {c:?} at {px}");
                    assert!((ours.advance_width - theirs.advance_width).abs() < 1e-3, "{family} {c:?} at {px}");
                    // fontdue flattens curves coarser: its edges differ
                    // on a curve, by a few levels on average, and a
                    // pixel here and there at a sharp turn by more.
                    let diff: Vec<i32> = our_cov.iter().zip(&their_cov).map(|(&a, &b)| (a as i32 - b as i32).abs()).collect();
                    let mean = diff.iter().sum::<i32>() as f32 / diff.len().max(1) as f32;
                    let far = diff.iter().filter(|&&d| d > 32).count();
                    assert!(mean < 10.0 && far * 50 <= diff.len(), "{family} {c:?} at {px}: {mean} levels off on average, {far} pixels far off");
                }
            }
        }
    }

    /// A square drawn on whole pixels covers them fully, half a pixel
    /// in covers half, and two overlapping squares are no darker.
    #[test]
    fn the_fill_covers_the_outline() {
        let square = |x0: f32, y0: f32, x1: f32, y1: f32| vec![((x0, y0), (x0, y1)), ((x0, y1), (x1, y1)), ((x1, y1), (x1, y0)), ((x1, y0), (x0, y0))];
        let cov = fill(&square(1.0, 1.0, 3.0, 3.0), 4, 4);
        assert_eq!(cov, [0, 0, 0, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 0, 0, 0]);
        let cov = fill(&square(0.5, 0.0, 2.0, 1.0), 2, 1);
        assert_eq!(cov, [128, 255]);
        let mut both = square(0.0, 0.0, 2.0, 2.0);
        both.extend(square(1.0, 0.0, 2.0, 2.0));
        assert!(fill(&both, 2, 2).iter().all(|&c| c == 255));
    }
}
