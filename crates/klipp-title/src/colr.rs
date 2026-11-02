//! COLR colour glyphs drawn on the CPU (#3238): a glyph's paint graph,
//! walked by skrifa, drawn into a premultiplied RGBA raster at a size,
//! which the colour-glyph path then shows as it shows a bitmap strike,
//! on the CPU route and the GPU's alike.
//!
//! The raster is the font's own drawing: each layer clipped to the
//! outline of the glyph it names, filled with its palette colour (the
//! text's own for palette index 0xFFFF) and laid over the layers below,
//! in sRGB as FreeType and the browsers blend them. COLR v1's gradients
//! and its other composite modes are not drawn yet: a fill with one of
//! them, and a layer merged by a mode other than source over, draw as if
//! absent.

use skrifa::color::{Brush, ColorGlyphFormat, ColorPainter, CompositeMode, Transform};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::types::BoundingBox;
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// The most pixels a glyph's raster may have: 2048 square. A font's
/// transforms can scale a layer without bound.
const MAX_PIXELS: usize = 2048 * 2048;

/// The most fills a glyph may make, each over its whole raster.
const MAX_FILLS: usize = 4096;

/// The most line segments a glyph's outlines may give a raster.
const MAX_SEGMENTS: usize = 1 << 20;

/// A colour glyph drawn: premultiplied RGBA, and its top left corner from
/// the glyph's origin in pixels, y up.
pub(crate) struct ColrRaster {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) rgba: Vec<u8>,
}

/// Glyph `gid` of the font in `data`, a COLR glyph of `format`, drawn at
/// `px` pixels per em with palette 0, `foreground` (straight sRGB RGBA)
/// for the text's own colour. `None` for a glyph with no such paint, one
/// that draws nothing, or one past the limits above.
pub(crate) fn draw(data: &[u8], gid: u16, px: f32, format: ColorGlyphFormat, foreground: [u8; 4]) -> Option<ColrRaster> {
    if !px.is_finite() || px <= 0.0 {
        return None;
    }
    let font = FontRef::from_index(data, 0).ok()?;
    let glyph = font.color_glyphs().get_with_format(GlyphId::new(gid.into()), format)?;
    let upem = font.head().ok()?.units_per_em();
    let scale = px / f32::from(upem.max(1));
    let location = LocationRef::default();

    // The raster's box: where the fills land, each clipped as it is.
    let mut bounds = Bounds { font: &font, scale, transforms: vec![Transform::IDENTITY], clips: vec![None], drawn: None, fills: 0 };
    glyph.paint(location, &mut bounds).ok()?;
    let (x0, y0, x1, y1) = bounds.drawn?;
    let (left, top) = (x0.floor() as i32 - 1, y1.ceil() as i32 + 1);
    let (width, height) = ((x1.ceil() as i32 + 1 - left).max(1) as usize, (top - (y0.floor() as i32 - 1)).max(1) as usize);
    if width.checked_mul(height)? > MAX_PIXELS || bounds.fills > MAX_FILLS {
        return None;
    }

    let palette: Vec<[f32; 4]> = font
        .color_palettes()
        .get(0)
        .map(|p| p.colors().iter().map(|c| premultiplied([c.red, c.green, c.blue, c.alpha])).collect())
        .unwrap_or_default();
    let mut canvas = Canvas {
        font: &font,
        scale,
        width,
        height,
        left,
        top,
        palette,
        foreground: premultiplied(foreground),
        transforms: vec![Transform::IDENTITY],
        clips: vec![None],
        layers: vec![vec![[0.0; 4]; width * height]],
        segments: 0,
    };
    glyph.paint(location, &mut canvas).ok()?;
    let layer = canvas.layers.pop()?;
    let rgba = layer.iter().flat_map(|p| p.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)).collect();
    Some(ColrRaster { width, height, left, top, rgba })
}

/// An sRGB RGBA byte colour, premultiplied, 0 to 1.
fn premultiplied(c: [u8; 4]) -> [f32; 4] {
    let a = f32::from(c[3]) / 255.0;
    [f32::from(c[0]) / 255.0 * a, f32::from(c[1]) / 255.0 * a, f32::from(c[2]) / 255.0 * a, a]
}

/// `a` then `b`: `b` applied to what `a` gives.
fn compose(a: &Transform, b: &Transform) -> Transform {
    Transform {
        xx: a.xx * b.xx + a.xy * b.yx,
        yx: a.yx * b.xx + a.yy * b.yx,
        xy: a.xx * b.xy + a.xy * b.yy,
        yy: a.yx * b.xy + a.yy * b.yy,
        dx: a.xx * b.dx + a.xy * b.dy + a.dx,
        dy: a.yx * b.dx + a.yy * b.dy + a.dy,
    }
}

fn apply(t: &Transform, x: f32, y: f32) -> (f32, f32) {
    (t.xx * x + t.xy * y + t.dx, t.yx * x + t.yy * y + t.dy)
}

/// A glyph's outline, in font units, as points through `t` and `place`.
struct Points<'t, F: FnMut(f32, f32) -> (f32, f32)> {
    t: &'t Transform,
    place: F,
    out: Vec<Vec<(f32, f32)>>,
    at: (f32, f32),
    count: usize,
}

impl<F: FnMut(f32, f32) -> (f32, f32)> Points<'_, F> {
    fn to(&mut self, x: f32, y: f32) -> (f32, f32) {
        let (fx, fy) = apply(self.t, x, y);
        (self.place)(fx, fy)
    }

    fn push(&mut self, p: (f32, f32)) {
        if self.count < MAX_SEGMENTS {
            self.count += 1;
            if let Some(c) = self.out.last_mut() {
                c.push(p);
            }
        }
        self.at = p;
    }
}

impl<F: FnMut(f32, f32) -> (f32, f32)> OutlinePen for Points<'_, F> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.to(x, y);
        self.out.push(vec![p]);
        self.at = p;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.to(x, y);
        self.push(p);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (p0, c, p1) = (self.at, self.to(cx, cy), self.to(x, y));
        let dd = ((p0.0 - 2.0 * c.0 + p1.0).hypot(p0.1 - 2.0 * c.1 + p1.1) * 3.0).sqrt();
        let n = (1.0 + dd.floor()).min(64.0) as usize;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            self.push((mt * mt * p0.0 + 2.0 * mt * t * c.0 + t * t * p1.0, mt * mt * p0.1 + 2.0 * mt * t * c.1 + t * t * p1.1));
        }
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (p0, c0, c1, p1) = (self.at, self.to(cx0, cy0), self.to(cx1, cy1), self.to(x, y));
        let d0 = (p0.0 - 2.0 * c0.0 + c1.0).hypot(p0.1 - 2.0 * c0.1 + c1.1);
        let d1 = (c0.0 - 2.0 * c1.0 + p1.0).hypot(c0.1 - 2.0 * c1.1 + p1.1);
        let n = (1.0 + (d0.max(d1) * 6.0).sqrt().floor()).min(64.0) as usize;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
            self.push((a * p0.0 + b * c0.0 + c * c1.0 + d * p1.0, a * p0.1 + b * c0.1 + c * c1.1 + d * p1.1));
        }
    }

    fn close(&mut self) {}
}

/// Glyph `gid`'s outline in font units through `t` and `place`: closed
/// contours of points.
fn outline<F: FnMut(f32, f32) -> (f32, f32)>(font: &FontRef, gid: GlyphId, t: &Transform, place: F) -> Option<Vec<Vec<(f32, f32)>>> {
    let glyph = font.outline_glyphs().get(gid)?;
    let mut pen = Points { t, place, out: Vec::new(), at: (0.0, 0.0), count: 0 };
    glyph.draw(DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut pen).ok()?;
    Some(pen.out)
}

/// A clip box's corners through `t`, a contour.
fn box_contour(b: &BoundingBox<f32>, t: &Transform) -> Vec<(f32, f32)> {
    [(b.x_min, b.y_min), (b.x_min, b.y_max), (b.x_max, b.y_max), (b.x_max, b.y_min)].iter().map(|&(x, y)| apply(t, x, y)).collect()
}

type Rect = (f32, f32, f32, f32);

fn rect_of(contours: &[Vec<(f32, f32)>]) -> Option<Rect> {
    let mut r: Option<Rect> = None;
    for &(x, y) in contours.iter().flatten() {
        r = Some(r.map_or((x, y, x, y), |(a, b, c, d)| (a.min(x), b.min(y), c.max(x), d.max(y))));
    }
    r
}

fn intersect(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3))).filter(|r| r.0 < r.2 && r.1 < r.3),
        (a, None) | (None, a) => a,
    }
}

/// The first walk: the box, in pixels y up, that the fills cover.
struct Bounds<'a> {
    font: &'a FontRef<'a>,
    scale: f32,
    transforms: Vec<Transform>,
    /// Each clip's box, intersected with those under it; `None` for none
    /// yet, the whole plane.
    clips: Vec<Option<Rect>>,
    drawn: Option<Rect>,
    fills: usize,
}

impl Bounds<'_> {
    fn top(&self) -> Transform {
        self.transforms.last().copied().unwrap_or(Transform::IDENTITY)
    }

    fn push(&mut self, r: Option<Rect>) {
        let under = self.clips.last().copied().flatten();
        // An empty intersection clips everything: a box with no area.
        let clip = match (under, r) {
            (Some(_), Some(_)) => intersect(under, r).or(Some((0.0, 0.0, 0.0, 0.0))),
            _ => r.or(under),
        };
        self.clips.push(clip);
    }
}

impl ColorPainter for Bounds<'_> {
    fn push_transform(&mut self, transform: Transform) {
        let t = compose(&self.top(), &transform);
        self.transforms.push(t);
    }

    fn pop_transform(&mut self) {
        if self.transforms.len() > 1 {
            self.transforms.pop();
        }
    }

    fn push_clip_glyph(&mut self, glyph_id: GlyphId) {
        let s = self.scale;
        let r = outline(self.font, glyph_id, &self.top(), |x, y| (x * s, y * s)).and_then(|c| rect_of(&c));
        // A glyph with no outline clips everything away.
        self.push(r.or(Some((0.0, 0.0, 0.0, 0.0))));
    }

    fn push_clip_box(&mut self, clip_box: BoundingBox<f32>) {
        let s = self.scale;
        let c: Vec<(f32, f32)> = box_contour(&clip_box, &self.top()).into_iter().map(|(x, y)| (x * s, y * s)).collect();
        self.push(rect_of(&[c]));
    }

    fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
        }
    }

    fn fill(&mut self, _brush: Brush<'_>) {
        self.fills += 1;
        if let Some(r) = self.clips.last().copied().flatten().filter(|r| r.0 < r.2 && r.1 < r.3) {
            self.drawn = Some(self.drawn.map_or(r, |d| (d.0.min(r.0), d.1.min(r.1), d.2.max(r.2), d.3.max(r.3))));
        }
    }

    fn push_layer(&mut self, _composite_mode: CompositeMode) {}
}

/// The second walk: the raster drawn.
struct Canvas<'a> {
    font: &'a FontRef<'a>,
    scale: f32,
    width: usize,
    height: usize,
    left: i32,
    top: i32,
    palette: Vec<[f32; 4]>,
    foreground: [f32; 4],
    transforms: Vec<Transform>,
    /// Each clip's coverage, multiplied with those under it; `None` for
    /// the whole raster.
    clips: Vec<Option<Vec<f32>>>,
    layers: Vec<Vec<[f32; 4]>>,
    segments: usize,
}

impl Canvas<'_> {
    fn top_transform(&self) -> Transform {
        self.transforms.last().copied().unwrap_or(Transform::IDENTITY)
    }

    /// Font units through the transform to raster pixels, y down.
    fn place(&self) -> impl Fn(f32, f32) -> (f32, f32) {
        let (s, l, t) = (self.scale, self.left as f32, self.top as f32);
        move |x, y| (x * s - l, t - y * s)
    }

    fn push_mask(&mut self, contours: Option<Vec<Vec<(f32, f32)>>>) {
        let mut mask = match contours {
            Some(c) => {
                self.segments += c.iter().map(Vec::len).sum::<usize>();
                if self.segments > MAX_SEGMENTS {
                    vec![0.0; self.width * self.height]
                } else {
                    coverage(&c, self.width, self.height)
                }
            }
            None => vec![0.0; self.width * self.height],
        };
        if let Some(Some(under)) = self.clips.last() {
            mask.iter_mut().zip(under).for_each(|(m, u)| *m *= u);
        }
        self.clips.push(Some(mask));
    }

    fn colour(&self, palette_index: u16, alpha: f32) -> [f32; 4] {
        let c = if palette_index == 0xFFFF { self.foreground } else { self.palette.get(usize::from(palette_index)).copied().unwrap_or([0.0; 4]) };
        let a = alpha.clamp(0.0, 1.0);
        c.map(|v| v * a)
    }
}

impl ColorPainter for Canvas<'_> {
    fn push_transform(&mut self, transform: Transform) {
        let t = compose(&self.top_transform(), &transform);
        self.transforms.push(t);
    }

    fn pop_transform(&mut self) {
        if self.transforms.len() > 1 {
            self.transforms.pop();
        }
    }

    fn push_clip_glyph(&mut self, glyph_id: GlyphId) {
        let contours = outline(self.font, glyph_id, &self.top_transform(), self.place());
        self.push_mask(contours);
    }

    fn push_clip_box(&mut self, clip_box: BoundingBox<f32>) {
        let place = self.place();
        let contour = box_contour(&clip_box, &self.top_transform()).into_iter().map(|(x, y)| place(x, y)).collect();
        self.push_mask(Some(vec![contour]));
    }

    fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
        }
    }

    fn fill(&mut self, brush: Brush<'_>) {
        let Brush::Solid { palette_index, alpha } = brush else { return };
        let src = self.colour(palette_index, alpha);
        let Some(layer) = self.layers.last_mut() else { return };
        match self.clips.last() {
            Some(Some(mask)) => layer.iter_mut().zip(mask).for_each(|(d, &m)| over(d, src.map(|v| v * m))),
            _ => layer.iter_mut().for_each(|d| over(d, src)),
        }
    }

    fn push_layer(&mut self, _composite_mode: CompositeMode) {
        self.layers.push(vec![[0.0; 4]; self.width * self.height]);
    }

    fn pop_layer_with_mode(&mut self, composite_mode: CompositeMode) {
        if self.layers.len() < 2 {
            return;
        }
        let top = self.layers.pop().unwrap_or_default();
        if composite_mode != CompositeMode::SrcOver {
            return;
        }
        if let Some(under) = self.layers.last_mut() {
            under.iter_mut().zip(top).for_each(|(d, s)| over(d, s));
        }
    }
}

/// `src` laid over `dst`, both premultiplied.
fn over(dst: &mut [f32; 4], src: [f32; 4]) {
    let k = 1.0 - src[3];
    for c in 0..4 {
        dst[c] = src[c] + dst[c] * k;
    }
}

/// The coverage, 0 to 1, of `contours` (raster pixels, y down) over a
/// `width` x `height` raster, non-zero: each pixel's signed area
/// accumulated along each edge, then summed along the raster.
fn coverage(contours: &[Vec<(f32, f32)>], width: usize, height: usize) -> Vec<f32> {
    let mut acc = vec![0.0f32; width * height + width + 2];
    let w = width as f32;
    for contour in contours {
        let Some(&first) = contour.first() else { continue };
        let mut prev = first;
        for &p in contour.iter().skip(1).chain(std::iter::once(&first)) {
            for (a, b) in clip_x(prev, p, w) {
                line(&mut acc, width, height, a, b);
            }
            prev = p;
        }
    }
    let mut sum = 0.0f32;
    acc.truncate(width * height);
    for v in &mut acc {
        sum += *v;
        *v = sum.abs().min(1.0);
    }
    acc
}

/// The segment `a`-`b` with what lies left of the raster moved onto its
/// left edge and what lies right of it onto its right edge: an edge's
/// winding still counts there, and nothing lands outside the rows.
fn clip_x(a: (f32, f32), b: (f32, f32), w: f32) -> Vec<((f32, f32), (f32, f32))> {
    let mut cuts = vec![0.0f32, 1.0];
    for edge in [0.0, w] {
        if (a.0 - edge) * (b.0 - edge) < 0.0 {
            cuts.push((edge - a.0) / (b.0 - a.0));
        }
    }
    cuts.sort_by(f32::total_cmp);
    let at = |t: f32| ((a.0 + (b.0 - a.0) * t).clamp(0.0, w), a.1 + (b.1 - a.1) * t);
    cuts.windows(2).map(|c| (at(c[0]), at(c[1]))).collect()
}

/// One edge's signed area into `acc`, as font-rs accumulates it. `x` is
/// within `0..=width`.
fn line(acc: &mut [f32], width: usize, height: usize, p0: (f32, f32), p1: (f32, f32)) {
    if (p0.1 - p1.1).abs() <= f32::EPSILON {
        return;
    }
    let (dir, p0, p1) = if p0.1 < p1.1 { (1.0, p0, p1) } else { (-1.0, p1, p0) };
    let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
    let mut x = p0.0;
    if p0.1 < 0.0 {
        x -= p0.1 * dxdy;
    }
    let y_start = p0.1.max(0.0) as usize;
    let y_end = (p1.1.ceil().max(0.0) as usize).min(height);
    for y in y_start..y_end {
        let row = y * width;
        let dy = ((y + 1) as f32).min(p1.1) - (y as f32).max(p0.1);
        let xnext = x + dxdy * dy;
        let d = dy * dir;
        let (x0, x1) = if x < xnext { (x, xnext) } else { (xnext, x) };
        let x0floor = x0.floor();
        let x0i = x0floor.max(0.0) as usize;
        let x1ceil = x1.ceil();
        let x1i = x1ceil.max(0.0) as usize;
        if x1i <= x0i + 1 {
            let xmf = 0.5 * (x + xnext) - x0floor;
            acc[row + x0i] += d - d * xmf;
            acc[row + x0i + 1] += d * xmf;
        } else {
            let s = (x1 - x0).recip();
            let x0f = x0 - x0floor;
            let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
            let x1f = x1 - x1ceil + 1.0;
            let am = 0.5 * s * x1f * x1f;
            acc[row + x0i] += d * a0;
            if x1i == x0i + 2 {
                acc[row + x0i + 1] += d * (1.0 - a0 - am);
            } else {
                let a1 = s * (1.5 - x0f);
                acc[row + x0i + 1] += d * (a1 - a0);
                for xi in x0i + 2..x1i - 1 {
                    acc[row + xi] += d * s;
                }
                let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                acc[row + x1i - 1] += d * (1.0 - a2 - am);
            }
            acc[row + x1i] += d * am;
        }
        x = xnext;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn test_file(path: &str) -> Vec<u8> {
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join(path)).unwrap()
    }

    fn gid_of(data: &[u8], c: char) -> u16 {
        FontRef::from_index(data, 0).unwrap().charmap().map(c).unwrap().to_u32() as u16
    }

    /// The worst and the mean difference, by channel of straight RGBA,
    /// between a raster and a FreeType reference, each placed by its own
    /// origin and nothing where either has no pixel.
    pub(crate) fn against_golden(width: usize, height: usize, left: i32, top: i32, straight: &[u8], golden: &[u8]) -> (u8, f64) {
        let head = |i: usize| i32::from_le_bytes(golden[i * 4..i * 4 + 4].try_into().unwrap());
        let (gw, gh, gl, gt) = (head(0) as usize, head(1) as usize, head(2), head(3));
        let gpx = &golden[16..];
        let at = |px: &[u8], w: usize, h: usize, l: i32, t: i32, x: i32, y: i32| -> [u8; 4] {
            let (cx, cy) = (x - l, t - y);
            if cx < 0 || cy < 0 || cx as usize >= w || cy as usize >= h {
                return [0; 4];
            }
            let i = (cy as usize * w + cx as usize) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        let (x0, x1) = (left.min(gl), (left + width as i32).max(gl + gw as i32));
        let (y0, y1) = ((top - height as i32).min(gt - gh as i32), top.max(gt));
        let (mut worst, mut sum, mut n) = (0u8, 0u64, 0u64);
        for y in y0..y1 {
            for x in x0..x1 {
                let (a, b) = (at(straight, width, height, left, top, x, y), at(gpx, gw, gh, gl, gt, x, y));
                // Colour where neither is clear; coverage everywhere.
                let channels = if a[3] > 16 && b[3] > 16 { 0..4 } else { 3..4 };
                for c in channels {
                    let d = a[c].abs_diff(b[c]);
                    worst = worst.max(d);
                    sum += u64::from(d);
                    n += 1;
                }
            }
        }
        (worst, sum as f64 / n.max(1) as f64)
    }

    fn straight(premultiplied: &[u8]) -> Vec<u8> {
        premultiplied
            .chunks_exact(4)
            .flat_map(|p| {
                let a = u32::from(p[3]);
                let un = |c: u8| (u32::from(c) * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8);
                [un(p[0]), un(p[1]), un(p[2]), p[3]]
            })
            .collect()
    }

    /// A COLR v0 glyph of three palette layers at half opacity and one in
    /// the text's colour is drawn as FreeType draws it, at three sizes,
    /// in the same place from its origin (#3238).
    #[test]
    fn a_colr_v0_glyph_is_drawn_as_freetype_draws_it() {
        let font = test_file("fonts/KlippTestColrV0.ttf");
        let gid = gid_of(&font, 'A');
        for px in [24, 64, 150] {
            let r = draw(&font, gid, px as f32, ColorGlyphFormat::ColrV0, [0, 0, 0, 255]).expect("drawn");
            let golden = test_file(&format!("goldens/KlippTestColrV0-A-{px}.rgba"));
            let (worst, mean) = against_golden(r.width, r.height, r.left, r.top, &straight(&r.rgba), &golden);
            eprintln!("COLR v0 at {px} px: worst {worst}, mean {mean:.3} levels from FreeType");
            assert!(worst <= 12 && mean < 1.0, "at {px} px: worst {worst}, mean {mean:.3}");
        }
    }

    /// A glyph with no COLR record, and one past the font's glyphs, draw
    /// nothing here; they are drawn as outlines.
    #[test]
    fn a_glyph_without_colour_layers_is_not_drawn_here() {
        let font = test_file("fonts/KlippTestColrV0.ttf");
        assert!(draw(&font, gid_of(&font, 'B'), 64.0, ColorGlyphFormat::ColrV0, [0, 0, 0, 255]).is_none());
        assert!(draw(&font, 4000, 64.0, ColorGlyphFormat::ColrV0, [0, 0, 0, 255]).is_none());
        assert!(draw(&font, gid_of(&font, 'A'), 64.0, ColorGlyphFormat::ColrV1, [0, 0, 0, 255]).is_none());
    }

    /// A font arrives with a shared project, so its COLR and CPAL tables
    /// are read as untrusted: overwritten from several offsets, they draw
    /// something or nothing, within the raster's limits, and never panic.
    #[test]
    fn a_damaged_colr_table_never_panics() {
        let good = test_file("fonts/KlippTestColrV0.ttf");
        let gid = gid_of(&good, 'A');
        let records = FontRef::from_index(&good, 0).unwrap().table_directory.table_records().to_vec();
        for tag in [b"COLR", b"CPAL"] {
            let record = records.iter().find(|r| r.tag() == skrifa::raw::types::Tag::new(tag)).unwrap();
            let (start, len) = (record.offset() as usize, record.length() as usize);
            for fill in [0xFFu8, 0x00, 0x7F] {
                for from in [2, 4, 8, 14, len / 2] {
                    let mut bad = good.clone();
                    bad[start + from.min(len)..start + len].fill(fill);
                    if let Some(r) = draw(&bad, gid, 64.0, ColorGlyphFormat::ColrV0, [0, 0, 0, 255]) {
                        assert!(r.width * r.height <= MAX_PIXELS && r.rgba.len() == r.width * r.height * 4);
                    }
                }
            }
        }
    }

    /// A square's coverage is whole inside, nothing outside and half on
    /// a half-covered column; one hanging off the raster on every side
    /// covers the raster whole, and nothing lands outside it.
    #[test]
    fn coverage_is_the_area_inside() {
        let square = vec![vec![(1.0, 1.0), (1.0, 3.0), (3.5, 3.0), (3.5, 1.0)]];
        let c = coverage(&square, 5, 4);
        let at = |x: usize, y: usize| c[y * 5 + x];
        assert!((at(1, 1) - 1.0).abs() < 1e-5 && (at(2, 2) - 1.0).abs() < 1e-5);
        assert!((at(3, 1) - 0.5).abs() < 1e-5, "{}", at(3, 1));
        assert!(at(0, 0) < 1e-5 && at(4, 1) < 1e-5 && at(1, 3) < 1e-5);
        let big = vec![vec![(-10.0, -10.0), (-10.0, 20.0), (20.0, 20.0), (20.0, -10.0)]];
        assert!(coverage(&big, 5, 4).iter().all(|v| (v - 1.0).abs() < 1e-5));
        // Wound the other way, the same.
        let back = vec![vec![(1.0, 1.0), (3.5, 1.0), (3.5, 3.0), (1.0, 3.0)]];
        assert_eq!(coverage(&back, 5, 4), c);
    }
}
