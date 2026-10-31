//! A title's backing drawn one box per line (#3237): each line's box
//! fitted to its width, as social-video captions are, joined into one
//! shape with rounded inside corners or kept apart by a gap.
//!
//! The geometry is worked out here once, in frame pixels, and drawn the
//! same way on both routes: the CPU from [`BackingLines::coverage`], the
//! GPU stack's title-styles shader from the same rectangles with the same
//! signed distances (`title_styles.wgsl`, ops 55 and 56).

use klipp_timeline::LineBoxes;

use crate::shaping::Face;

/// The boxes of a backing drawn per line, in frame pixels, top first (or,
/// for vertical text, in column order): each `[x0, y0, x1, y1]`, padded;
/// their corners' radius; and whether they are joined into one shape.
#[derive(Debug, Clone, PartialEq)]
pub struct BackingLines {
    pub rects: Vec<[f32; 4]>,
    pub radius: f32,
    pub joined: bool,
}

/// Each line's band, in the layout's pixels, from its glyphs laid out:
/// across, its ink from the first drawn glyph to the last; down, the
/// font's ascent to descent round its baseline, so a line with a
/// descender and one without are as tall. Vertical text's columns are
/// their ink both ways. `line_starts` are the glyphs that start a line.
pub(crate) fn line_bands(fonts: &[Face], glyphs: &[fontdue::layout::GlyphPosition], line_starts: &[usize], vertical: bool) -> Vec<[f32; 4]> {
    let mut starts: Vec<usize> = line_starts.iter().copied().filter(|&s| s < glyphs.len()).collect();
    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    starts.dedup();
    let mut out = Vec::new();
    for (n, &from) in starts.iter().enumerate() {
        let to = starts.get(n + 1).copied().unwrap_or(glyphs.len());
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for g in glyphs.get(from..to).unwrap_or_default() {
            if g.width == 0 || g.height == 0 || g.parent.is_whitespace() {
                continue;
            }
            b[0] = b[0].min(g.x);
            b[2] = b[2].max(g.x + g.width as f32);
            if vertical {
                b[1] = b[1].min(g.y);
                b[3] = b[3].max(g.y + g.height as f32);
                continue;
            }
            let Some(font) = fonts.get(g.font_index).map(|f| f.font) else { continue };
            let m = font.metrics_indexed(g.key.glyph_index, g.key.px);
            let baseline = g.y + m.height as f32 + m.ymin as f32;
            let (ascent, descent) = font.font.horizontal_line_metrics(g.key.px).map_or((g.key.px * 0.8, -g.key.px * 0.2), |l| (l.ascent, l.descent));
            b[1] = b[1].min(baseline - ascent);
            b[3] = b[3].max(baseline - descent);
        }
        if b[2] > b[0] && b[3] > b[1] {
            out.push(b);
        }
    }
    out
}

/// The boxes `lines` draws round `bands` (each line's, in frame pixels),
/// `padding` and the gap and radius being pixels at 1080p, at `scale`.
/// Joined (a gap of 0 or less), neighbouring boxes are made to meet, by
/// `-gap` more where it is below 0; apart, they are kept `gap` apart.
pub fn backing_lines(lines: &LineBoxes, padding: f32, bands: &[[f32; 4]], vertical: bool, scale: f32) -> Option<BackingLines> {
    if bands.is_empty() {
        return None;
    }
    let pad_across = padding.max(0.0) * scale;
    let pad_down = lines.padding_y.unwrap_or(padding).max(0.0) * scale;
    let (pad_x, pad_y) = if vertical { (pad_down, pad_across) } else { (pad_across, pad_down) };
    let mut rects: Vec<[f32; 4]> = bands.iter().map(|b| [b[0] - pad_x, b[1] - pad_y, b[2] + pad_x, b[3] + pad_y]).collect();
    let gap = lines.gap * scale;
    let joined = lines.gap <= 0.0;
    // Neighbours meet along the axis the lines step in: down for lines,
    // across for vertical text's columns.
    let (lo, hi) = if vertical { (0, 2) } else { (1, 3) };
    for n in 1..rects.len() {
        let (a, b) = (rects[n - 1], rects[n]);
        // Which of the two comes first along the axis.
        let (first, second) = if a[lo] <= b[lo] { (n - 1, n) } else { (n, n - 1) };
        let (end, start) = (rects[first][hi], rects[second][lo]);
        let mid = (end + start) * 0.5;
        if joined {
            let overlap = -gap * 0.5;
            if end - start < 2.0 * overlap {
                rects[first][hi] = mid + overlap;
                rects[second][lo] = mid - overlap;
            }
        } else if end + gap > start {
            rects[first][hi] = mid - gap * 0.5;
            rects[second][lo] = mid + gap * 0.5;
        }
    }
    Some(BackingLines { rects, radius: lines.radius.max(0.0) * scale, joined })
}

impl BackingLines {
    /// The frame pixels the boxes reach, `[x0, y0, x1, y1]`.
    pub fn bounds(&self) -> [f32; 4] {
        self.rects.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, r| [b[0].min(r[0]), b[1].min(r[1]), b[2].max(r[2]), b[3].max(r[3])])
    }

    /// How much of the pixel whose centre is `(x, y)` the boxes cover, 0
    /// to 1: one pixel's antialiasing at the edge of their signed
    /// distance, each box's corners rounded, joined ones blended so their
    /// inside corners round as their outside ones do.
    pub fn coverage(&self, x: f32, y: f32) -> f32 {
        let mut d = f32::MAX;
        for r in &self.rects {
            let e = rounded_box(x, y, r, self.radius);
            d = if d == f32::MAX { e } else if self.joined { smooth_min(d, e, self.radius) } else { d.min(e) };
        }
        (0.5 - d).clamp(0.0, 1.0)
    }
}

/// The signed distance from `(x, y)` to the box `r`, its corners rounded
/// by `radius` (no more than half its shorter side): below 0 inside.
pub(crate) fn rounded_box(x: f32, y: f32, r: &[f32; 4], radius: f32) -> f32 {
    let (hw, hh) = ((r[2] - r[0]) * 0.5, (r[3] - r[1]) * 0.5);
    let rad = radius.min(hw).min(hh).max(0.0);
    let (qx, qy) = ((x - (r[0] + hw)).abs() - hw + rad, (y - (r[1] + hh)).abs() - hh + rad);
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - rad
}

/// A minimum of two distances blended so the corner where two shapes
/// meet is filled with a quarter circle of radius `k`: a circular
/// smooth minimum.
pub(crate) fn smooth_min(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - k * 0.5 * (1.0 + h - (1.0 - h * (h - 2.0)).sqrt())
}

/// A title's per-line backing, when it has one.
pub(crate) trait BackgroundLines {
    fn background_lines(&self, bands: &[[f32; 4]], vertical: bool, scale: f32) -> Option<BackingLines>;
}

impl BackgroundLines for klipp_timeline::TitleData {
    /// The boxes of this title's backing round `bands`, when it is drawn
    /// one per line and there are lines to draw it round.
    fn background_lines(&self, bands: &[[f32; 4]], vertical: bool, scale: f32) -> Option<BackingLines> {
        match &self.background {
            klipp_timeline::TitleBackground::Color { padding, lines: Some(lines), .. } => backing_lines(lines, *padding, bands, vertical, scale),
            _ => None,
        }
    }
}
