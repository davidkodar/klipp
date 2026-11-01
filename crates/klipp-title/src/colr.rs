//! COLR colour glyphs drawn on the CPU (#3238): a glyph's paint graph,
//! walked by skrifa, drawn into a premultiplied RGBA raster at a size,
//! which the colour-glyph path then shows as it shows a bitmap strike,
//! on the CPU route and the GPU's alike.
//!
//! The raster is the font's own drawing: each layer clipped to the
//! outline of the glyph it names, filled with its palette colour (the
//! text's own for palette index 0xFFFF) and laid over the layers below,
//! in sRGB as FreeType and the browsers blend them. COLR v1's gradients
//! (linear, two-point radial and sweep, padded, repeated or reflected,
//! their stops blended premultiplied) and its 28 composite modes (the
//! Porter-Duff operators and the W3C blend modes) are drawn as Skia draws
//! them.

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
    let glyphs = font.color_glyphs();
    let id = GlyphId::new(gid.into());
    // A COLR v1 font may keep some glyphs in its version 0 list.
    let glyph = glyphs.get_with_format(id, format).or_else(|| matches!(format, ColorGlyphFormat::ColrV1).then(|| glyphs.get_with_format(id, ColorGlyphFormat::ColrV0)).flatten())?;
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
        let paint = match brush {
            Brush::Solid { palette_index, alpha } => Paint::Solid(self.colour(palette_index, alpha)),
            Brush::LinearGradient { p0, p1, color_stops, extend } => match self.stops(color_stops) {
                Some(stops) => Paint::Linear { p0: (p0.x, p0.y), p1: (p1.x, p1.y), stops, extend },
                None => return,
            },
            Brush::RadialGradient { c0, r0, c1, r1, color_stops, extend } => match self.stops(color_stops) {
                Some(stops) => Paint::Radial { c0: (c0.x, c0.y), r0, c1: (c1.x, c1.y), r1, stops, extend },
                None => return,
            },
            Brush::SweepGradient { c0, start_angle, end_angle, color_stops, extend } => match self.stops(color_stops) {
                Some(stops) => Paint::Sweep { c0: (c0.x, c0.y), start: start_angle, end: end_angle, stops, extend },
                None => return,
            },
        };
        // A pixel's centre back in the glyph's units, for a gradient.
        let to_glyph = invert(&compose(&Transform { xx: self.scale, yx: 0.0, xy: 0.0, yy: -self.scale, dx: -(self.left as f32), dy: self.top as f32 }, &self.top_transform()));
        let (width, mask) = (self.width, self.clips.last().cloned().flatten());
        let Some(layer) = self.layers.last_mut() else { return };
        for (i, d) in layer.iter_mut().enumerate() {
            let m = mask.as_ref().map_or(1.0, |m| m[i]);
            if m <= 0.0 {
                continue;
            }
            let src = match (&paint, to_glyph) {
                (Paint::Solid(c), _) => *c,
                (_, Some(t)) => {
                    let (x, y) = apply(&t, (i % width) as f32 + 0.5, (i / width) as f32 + 0.5);
                    paint.at(x, y)
                }
                // A transform that flattens the plane: nothing to show.
                (_, None) => continue,
            };
            over(d, src.map(|v| v * m));
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
        if let Some(under) = self.layers.last_mut() {
            under.iter_mut().zip(top).for_each(|(d, s)| *d = composite(composite_mode, s, *d));
        }
    }
}

impl Canvas<'_> {
    /// A gradient's stops in premultiplied colour, by offset; `None` for
    /// none.
    fn stops(&self, stops: &[skrifa::color::ColorStop]) -> Option<Vec<(f32, [f32; 4])>> {
        let mut out: Vec<(f32, [f32; 4])> = stops.iter().map(|s| (s.offset, self.colour(s.palette_index, s.alpha))).collect();
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        (!out.is_empty()).then_some(out)
    }
}

/// What a fill lays: a colour, or a gradient in the glyph's units.
enum Paint {
    Solid([f32; 4]),
    Linear { p0: (f32, f32), p1: (f32, f32), stops: Vec<(f32, [f32; 4])>, extend: skrifa::color::Extend },
    Radial { c0: (f32, f32), r0: f32, c1: (f32, f32), r1: f32, stops: Vec<(f32, [f32; 4])>, extend: skrifa::color::Extend },
    Sweep { c0: (f32, f32), start: f32, end: f32, stops: Vec<(f32, [f32; 4])>, extend: skrifa::color::Extend },
}

impl Paint {
    /// The colour, premultiplied, at `(x, y)` in the glyph's units.
    fn at(&self, x: f32, y: f32) -> [f32; 4] {
        match self {
            Self::Solid(c) => *c,
            Self::Linear { p0, p1, stops, extend } => {
                let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
                let len = dx * dx + dy * dy;
                if len <= f32::EPSILON {
                    return stops.last().map_or([0.0; 4], |s| s.1);
                }
                line_colour(stops, *extend, ((x - p0.0) * dx + (y - p0.1) * dy) / len)
            }
            Self::Radial { c0, r0, c1, r1, stops, extend } => match conical_t((x, y), *c0, *r0, *c1, *r1) {
                Some(t) => line_colour(stops, *extend, t),
                None => [0.0; 4],
            },
            Self::Sweep { c0, start, end, stops, extend } => {
                // In the glyph's units y is up, so the angle counted
                // anticlockwise there is the clockwise one skrifa gives
                // for a canvas whose y runs down.
                let angle = (y - c0.1).atan2(x - c0.0).to_degrees().rem_euclid(360.0);
                if (end - start).abs() <= f32::EPSILON {
                    // Coincident angles: the first stop before, the last
                    // after, padded; nothing between to blend.
                    let first = stops.first().map_or([0.0; 4], |s| s.1);
                    let last = stops.last().map_or([0.0; 4], |s| s.1);
                    return match extend {
                        skrifa::color::Extend::Pad => if angle < *start { first } else { last },
                        _ => [0.0; 4],
                    };
                }
                line_colour(stops, *extend, (angle - start) / (end - start))
            }
        }
    }
}

/// The colour line `stops` at `t`, extended past its ends by `extend`.
fn line_colour(stops: &[(f32, [f32; 4])], extend: skrifa::color::Extend, t: f32) -> [f32; 4] {
    use skrifa::color::Extend;
    let t = match extend {
        Extend::Repeat => t - t.floor(),
        Extend::Reflect => {
            let u = t.rem_euclid(2.0);
            if u > 1.0 { 2.0 - u } else { u }
        }
        _ => t.clamp(0.0, 1.0),
    };
    let Some(&(first_at, first)) = stops.first() else { return [0.0; 4] };
    if t <= first_at {
        return first;
    }
    for pair in stops.windows(2) {
        let ((a, ca), (b, cb)) = (pair[0], pair[1]);
        if t <= b {
            let k = if b - a <= f32::EPSILON { 1.0 } else { (t - a) / (b - a) };
            return std::array::from_fn(|c| ca[c] + (cb[c] - ca[c]) * k);
        }
    }
    stops.last().map_or([0.0; 4], |s| s.1)
}

/// Where `p` lies on the two-point conical gradient from circle `c0`,
/// `r0` to circle `c1`, `r1`: the largest `t` whose circle, of a radius
/// not below zero, passes through it; `None` where none does.
fn conical_t(p: (f32, f32), c0: (f32, f32), r0: f32, c1: (f32, f32), r1: f32) -> Option<f32> {
    let (cdx, cdy, dr) = (c1.0 - c0.0, c1.1 - c0.1, r1 - r0);
    let (pdx, pdy) = (p.0 - c0.0, p.1 - c0.1);
    let a = cdx * cdx + cdy * cdy - dr * dr;
    let b = pdx * cdx + pdy * cdy + r0 * dr;
    let c = pdx * pdx + pdy * pdy - r0 * r0;
    let valid = |t: f32| t.is_finite() && r0 + t * dr >= 0.0;
    if a.abs() <= 1e-6 * (cdx * cdx + cdy * cdy + dr * dr).max(1.0) {
        let t = c / (2.0 * b);
        return valid(t).then_some(t);
    }
    let disc = b * b - a * c;
    if disc < 0.0 {
        return None;
    }
    let root = disc.sqrt();
    let (t1, t2) = ((b + root) / a, (b - root) / a);
    let (hi, lo) = if t1 > t2 { (t1, t2) } else { (t2, t1) };
    if valid(hi) {
        Some(hi)
    } else if valid(lo) {
        Some(lo)
    } else {
        None
    }
}

/// `t`'s inverse; `None` for a transform that flattens the plane.
fn invert(t: &Transform) -> Option<Transform> {
    let det = t.xx * t.yy - t.xy * t.yx;
    if det.abs() <= f32::EPSILON * 1e-3 || !det.is_finite() {
        return None;
    }
    let (xx, xy, yx, yy) = (t.yy / det, -t.xy / det, -t.yx / det, t.xx / det);
    Some(Transform { xx, yx, xy, yy, dx: -(xx * t.dx + xy * t.dy), dy: -(yx * t.dx + yy * t.dy) })
}

/// Layer `s` merged onto `d` by `mode`, both premultiplied: the
/// Porter-Duff operators, and the W3C blend modes laid source over.
fn composite(mode: CompositeMode, s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    use CompositeMode as M;
    let (sa, da) = (s[3], d[3]);
    let pd = |fs: f32, fd: f32| -> [f32; 4] { std::array::from_fn(|c| (s[c] * fs + d[c] * fd).min(1.0)) };
    match mode {
        M::Clear => [0.0; 4],
        M::Src => s,
        M::Dest => d,
        M::SrcOver => pd(1.0, 1.0 - sa),
        M::DestOver => pd(1.0 - da, 1.0),
        M::SrcIn => pd(da, 0.0),
        M::DestIn => pd(0.0, sa),
        M::SrcOut => pd(1.0 - da, 0.0),
        M::DestOut => pd(0.0, 1.0 - sa),
        M::SrcAtop => pd(da, 1.0 - sa),
        M::DestAtop => pd(1.0 - da, sa),
        M::Xor => pd(1.0 - da, 1.0 - sa),
        M::Plus => pd(1.0, 1.0),
        _ => {
            // Straight colours for the blend, then the W3C formula: the
            // blend where both are, each alone where only it is.
            let un = |p: [f32; 4]| -> [f32; 3] { if p[3] > 0.0 { [p[0] / p[3], p[1] / p[3], p[2] / p[3]] } else { [0.0; 3] } };
            let (cs, cb) = (un(s), un(d));
            let b = blend(mode, cs, cb);
            let mut out = [0.0; 4];
            for c in 0..3 {
                out[c] = (s[c] * (1.0 - da) + d[c] * (1.0 - sa) + sa * da * b[c]).clamp(0.0, 1.0);
            }
            out[3] = sa + da - sa * da;
            out
        }
    }
}

/// The W3C blend `mode` of source `cs` over backdrop `cb`, straight
/// colours 0 to 1.
fn blend(mode: CompositeMode, cs: [f32; 3], cb: [f32; 3]) -> [f32; 3] {
    use CompositeMode as M;
    let each = |f: fn(f32, f32) -> f32| -> [f32; 3] { std::array::from_fn(|c| f(cs[c], cb[c])) };
    let hard = |s: f32, b: f32| if s <= 0.5 { b * 2.0 * s } else { let s2 = 2.0 * s - 1.0; b + s2 - b * s2 };
    match mode {
        M::Screen => each(|s, b| s + b - s * b),
        M::Overlay => each(|s, b| if b <= 0.5 { s * 2.0 * b } else { let b2 = 2.0 * b - 1.0; s + b2 - s * b2 }),
        M::Darken => each(f32::min),
        M::Lighten => each(f32::max),
        M::ColorDodge => each(|s, b| if b <= 0.0 { 0.0 } else if s >= 1.0 { 1.0 } else { (b / (1.0 - s)).min(1.0) }),
        M::ColorBurn => each(|s, b| if b >= 1.0 { 1.0 } else if s <= 0.0 { 0.0 } else { 1.0 - ((1.0 - b) / s).min(1.0) }),
        M::HardLight => std::array::from_fn(|c| hard(cs[c], cb[c])),
        M::SoftLight => each(|s, b| {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }),
        M::Difference => each(|s, b| (s - b).abs()),
        M::Exclusion => each(|s, b| s + b - 2.0 * s * b),
        M::Multiply => each(|s, b| s * b),
        M::HslHue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        M::HslSaturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        M::HslColor => set_lum(cs, lum(cb)),
        M::HslLuminosity => set_lum(cb, lum(cs)),
        _ => cs,
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    let c = c.map(|v| v + d);
    let (l, n, x) = (lum(c), c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
    c.map(|v| {
        let v = if n < 0.0 && l - n > f32::EPSILON { l + (v - l) * l / (l - n) } else { v };
        if x > 1.0 && x - l > f32::EPSILON { l + (v - l) * (1.0 - l) / (x - l) } else { v }
    })
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let (max, min) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    if max - min <= f32::EPSILON {
        return [0.0; 3];
    }
    c.map(|v| (v - min) * s / (max - min))
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

    /// COLR v1 glyphs, one or more for each kind of paint (each gradient
    /// and extend, sweeps with coincident stops and angles, transforms,
    /// all 28 composite modes, the text's colour, clip boxes, nested
    /// glyphs), are drawn as Skia draws them, in the same place (#3238).
    #[test]
    fn colr_v1_glyphs_are_drawn_as_skia_draws_them() {
        let index: serde_json::Value = serde_json::from_slice(&test_file("goldens/colrv1/origins.json")).unwrap();
        let font = test_file(&format!("fonts/{}", index["font"].as_str().unwrap()));
        let px = index["px"].as_f64().unwrap() as f32;
        let mut failures = Vec::new();
        for (name, o) in index["origins"].as_object().unwrap() {
            let (left, top, code) = (o[0].as_i64().unwrap() as i32, o[1].as_i64().unwrap() as i32, o[2].as_u64().unwrap() as u32);
            let png = image::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/goldens/colrv1/{name}.png"))).unwrap().to_rgba8();
            let mut golden: Vec<u8> = [png.width() as i32, png.height() as i32, left, top].iter().flat_map(|v| v.to_le_bytes()).collect();
            golden.extend_from_slice(png.as_raw());
            let gid = gid_of(&font, char::from_u32(code).unwrap());
            let (worst, mean) = match draw(&font, gid, px, ColorGlyphFormat::ColrV1, [0, 0, 0, 255]) {
                Some(r) => against_golden(r.width, r.height, r.left, r.top, &straight(&r.rgba), &golden),
                // Drawn as nothing: held to an empty raster.
                None => against_golden(0, 0, 0, 0, &[], &golden),
            };
            eprintln!("{name:55} worst {worst:3} mean {mean:6.3}");
            if mean > 1.5 || worst > 64 {
                failures.push(format!("{name}: worst {worst}, mean {mean:.3}"));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
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
