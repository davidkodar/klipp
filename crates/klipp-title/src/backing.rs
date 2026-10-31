//! A title's backing drawn one box per line (#3237): each line's box
//! fitted to its width, as social-video captions are, joined into one
//! shape with rounded inside corners or kept apart by a gap.
//!
//! The geometry is worked out here once, in frame pixels, and drawn the
//! same way on both routes: the CPU from [`BackingLines::coverage`], the
//! GPU stack's title-styles shader from the same rectangles with the same
//! signed distances (`title_styles.wgsl`, ops 55 and 56).
//!
//! While the text moves, each line's box moves with its glyphs as posed
//! ([`line_poses`]): across, it spans the ink of the glyphs shown; down,
//! it moves by their mean offset and grows by their mean scale; it shows
//! as much as its most visible glyph. A line stagger brings the boxes in
//! with their lines, and a typewriter's box grows as its letters come.

use klipp_timeline::LineBoxes;

use crate::shaping::Face;

/// The boxes of a backing drawn per line, in frame pixels, top first (or,
/// for vertical text, in column order): each `[x0, y0, x1, y1]`, padded,
/// and how much it shows, 0 to 1; their corners' radius; and whether they
/// are joined into one shape.
#[derive(Debug, Clone, PartialEq)]
pub struct BackingLines {
    pub rects: Vec<[f32; 4]>,
    pub alphas: Vec<f32>,
    pub radius: f32,
    pub joined: bool,
}

/// A glyph as a motion poses it: its index in the layout, its move in
/// frame pixels, its scale and how much it shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GlyphPose {
    pub index: usize,
    pub dx: f32,
    pub dy: f32,
    pub scale: f32,
    pub alpha: f32,
}

/// A line as its glyphs are posed: the ink of those shown, along the line
/// (across for lines, down for vertical text's columns), `None` when none
/// shows; its mean move across the line's run; its mean scale; and how
/// much its most visible glyph shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinePose {
    pub extent: Option<[f32; 2]>,
    pub shift: f32,
    pub scale: f32,
    pub alpha: f32,
}

/// Each line's pose (#3237), the lines being `ranges` of the glyphs
/// whose resting ink boxes, in frame pixels, are `rest` (with their
/// characters, `parents`), and `poses` the glyphs drawn.
pub(crate) fn line_poses(ranges: &[(usize, usize)], rest: &[[f32; 4]], parents: &[char], poses: &[GlyphPose], vertical: bool) -> Vec<LinePose> {
    ranges
        .iter()
        .map(|&(from, to)| {
            let mut extent: Option<[f32; 2]> = None;
            let (mut shift, mut scale, mut alpha, mut n) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for p in poses.iter().filter(|p| (from..to).contains(&p.index) && p.alpha > 0.001) {
                let Some(r) = rest.get(p.index).filter(|r| r[2] > r[0] && r[3] > r[1]) else { continue };
                if parents.get(p.index).is_some_and(|c| c.is_whitespace()) {
                    continue;
                }
                // The glyph's box at rest, moved and scaled about its centre.
                let (cx, cy) = ((r[0] + r[2]) * 0.5 + p.dx, (r[1] + r[3]) * 0.5 + p.dy);
                let (along, half) = if vertical { (cy, (r[3] - r[1]) * 0.5 * p.scale) } else { (cx, (r[2] - r[0]) * 0.5 * p.scale) };
                extent = Some(extent.map_or([along - half, along + half], |e| [e[0].min(along - half), e[1].max(along + half)]));
                shift += if vertical { p.dx } else { p.dy };
                scale += p.scale;
                alpha = alpha.max(p.alpha);
                n += 1.0;
            }
            if n == 0.0 {
                return LinePose { extent: None, shift: 0.0, scale: 1.0, alpha: 0.0 };
            }
            LinePose { extent, shift: shift / n, scale: scale / n, alpha: alpha.min(1.0) }
        })
        .collect()
}

/// `glyphs`' resting ink boxes in frame pixels, laid out at `offset`, and
/// their characters: what [`line_poses`] measures a pose from.
pub(crate) fn rest_boxes(glyphs: &[fontdue::layout::GlyphPosition], offset: [f32; 2]) -> (Vec<[f32; 4]>, Vec<char>) {
    glyphs.iter().map(|g| ([g.x + offset[0], g.y + offset[1], g.x + offset[0] + g.width as f32, g.y + offset[1] + g.height as f32], g.parent)).unzip()
}

/// The boxes drawn under what `buf` holds (straight alpha, sRGB-encoded
/// byte units, `width` wide), in `color`: where a staggered title's
/// glyphs are posed from a resting render without its backing, the
/// backing goes under them (#3237). Mixed in linear light, as the GPU
/// stack mixes it under the layer it has placed, so the two agree where
/// a letter's soft edge lies over a box.
pub fn paint_under(buf: &mut [f32], width: u32, height: u32, lines: &BackingLines, color: [u8; 4]) {
    let lin = |code: f32| {
        let c = (code / 255.0).clamp(0.0, 1.0);
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let code = |l: f32| 255.0 * if l <= 0.0031308 { l * 12.92 } else { 1.055 * l.max(0.0).powf(1.0 / 2.4) - 0.055 };
    let under = [lin(color[0] as f32), lin(color[1] as f32), lin(color[2] as f32)];
    let w = width as usize;
    let b = lines.bounds();
    let (x0, y0) = ((b[0].floor() as i32).max(0) as usize, (b[1].floor() as i32).max(0) as usize);
    let (x1, y1) = ((b[2].ceil() as i32).clamp(0, width as i32) as usize, (b[3].ceil() as i32).clamp(0, height as i32) as usize);
    for y in y0..y1 {
        for x in x0..x1 {
            let ba = color[3] as f32 / 255.0 * lines.coverage(x as f32 + 0.5, y as f32 + 0.5);
            if ba <= 0.0 {
                continue;
            }
            let i = (y * w + x) * 4;
            let da = buf[i + 3] / 255.0;
            let out = da + ba * (1.0 - da);
            for c in 0..3 {
                buf[i + c] = code((lin(buf[i + c]) * da + under[c] * ba * (1.0 - da)) / out).clamp(0.0, 255.0);
            }
            buf[i + 3] = (out * 255.0).clamp(0.0, 255.0);
        }
    }
}

/// Each band comes with its line's glyphs, `from..to`.
pub(crate) fn line_bands(fonts: &[Face], glyphs: &[fontdue::layout::GlyphPosition], line_starts: &[usize], vertical: bool) -> Vec<([f32; 4], (usize, usize))> {
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
            out.push((b, (from, to)));
        }
    }
    out
}

/// The boxes `lines` draws round `bands` (each line's, in frame pixels),
/// `padding` and the gap and radius being pixels at 1080p, at `scale`.
/// Joined (a gap of 0 or less), neighbouring boxes are made to meet, by
/// `-gap` more where it is below 0; apart, they are kept `gap` apart.
/// With `poses` (one a band), each box is then laid where its line is
/// posed; a line with nothing shown has none.
pub fn backing_lines(lines: &LineBoxes, padding: f32, bands: &[[f32; 4]], poses: &[LinePose], vertical: bool, scale: f32) -> Option<BackingLines> {
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
    let mut alphas = vec![1.0; rects.len()];
    if poses.len() == rects.len() {
        // Joined or parted at rest, then each moved with its line: across
        // its run as it moves, along it as far as its ink shown reaches.
        let (along_lo, along_hi, pad_along) = if vertical { (1, 3, pad_y) } else { (0, 2, pad_x) };
        for (n, pose) in poses.iter().enumerate() {
            let r = &mut rects[n];
            match pose.extent {
                Some(e) => {
                    r[along_lo] = e[0] - pad_along;
                    r[along_hi] = e[1] + pad_along;
                    let (c, h) = ((r[lo] + r[hi]) * 0.5 + pose.shift, (r[hi] - r[lo]) * 0.5 * pose.scale);
                    r[lo] = c - h;
                    r[hi] = c + h;
                    alphas[n] = pose.alpha;
                }
                None => alphas[n] = 0.0,
            }
        }
        let shown: Vec<usize> = (0..rects.len()).filter(|&n| alphas[n] > 0.0).collect();
        rects = shown.iter().map(|&n| rects[n]).collect();
        alphas = shown.iter().map(|&n| alphas[n]).collect();
        if rects.is_empty() {
            return None;
        }
    }
    Some(BackingLines { rects, alphas, radius: lines.radius.max(0.0) * scale, joined })
}

impl BackingLines {
    /// The frame pixels the boxes reach, `[x0, y0, x1, y1]`.
    pub fn bounds(&self) -> [f32; 4] {
        self.rects.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, r| [b[0].min(r[0]), b[1].min(r[1]), b[2].max(r[2]), b[3].max(r[3])])
    }

    /// How much of the pixel whose centre is `(x, y)` the boxes cover, 0
    /// to 1: one pixel's antialiasing at the edge of their signed
    /// distance, each box's corners rounded, joined ones blended so their
    /// inside corners round as their outside ones do; times how much the
    /// box nearest the pixel shows.
    pub fn coverage(&self, x: f32, y: f32) -> f32 {
        let (mut d, mut nearest, mut alpha) = (f32::MAX, f32::MAX, 1.0);
        for (r, a) in self.rects.iter().zip(&self.alphas) {
            let e = rounded_box(x, y, r, self.radius);
            if e < nearest {
                (nearest, alpha) = (e, *a);
            }
            d = if d == f32::MAX { e } else if self.joined { smooth_min(d, e, self.radius) } else { d.min(e) };
        }
        (0.5 - d).clamp(0.0, 1.0) * alpha
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
    fn background_lines(&self, bands: &[[f32; 4]], poses: &[LinePose], vertical: bool, scale: f32) -> Option<BackingLines>;
}

impl BackgroundLines for klipp_timeline::TitleData {
    /// The boxes of this title's backing round `bands`, posed by `poses`
    /// while the text moves, when it is drawn one per line and there are
    /// lines to draw it round.
    fn background_lines(&self, bands: &[[f32; 4]], poses: &[LinePose], vertical: bool, scale: f32) -> Option<BackingLines> {
        match &self.background {
            klipp_timeline::TitleBackground::Color { padding, lines: Some(lines), .. } => backing_lines(lines, *padding, bands, poses, vertical, scale),
            _ => None,
        }
    }
}
