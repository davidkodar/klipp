//! OpenType shaping for title text (#1501): kerning, ligatures,
//! contextual alternates, mark positioning, and the features a text box
//! or a style run turns on or off ("smcp", "onum", "ss01", "-liga").
//!
//! The shaper is harfrust, a pure-Rust port of HarfBuzz. fontdue still
//! breaks the lines and draws the glyphs: [`ShapedLayout`] lays the text
//! out with fontdue's own `Layout` for where each line breaks and where
//! its baseline sits, then sets each line again, piece by piece, in the
//! glyphs and positions the shaper gives, and aligns it by its shaped
//! width. A line breaks where the text unshaped would break it; kerning
//! and ligatures mostly narrow a line, so a shaped line fits its box.
//!
//! Text the shaper changes nothing in (no kerning pair, no ligature, no
//! feature) lays out exactly as fontdue alone laid it out: each glyph's
//! advance is rounded up and its box placed as fontdue's layout does.

use std::borrow::Cow;

use fontdue::layout::{
    CharacterData, CoordinateSystem, GlyphPosition, GlyphRasterConfig, HorizontalAlign, Layout,
    LayoutSettings, TextStyle,
};
use fontdue::Font;
use harfrust::{Direction, Feature, FontRef, ShapeOptions, ShaperData, UnicodeBuffer};

/// Bumped whenever shaping changes where glyphs land, so a glyph cache
/// laid out before does not match (#1501).
pub const SHAPING_VERSION: u32 = 1;

/// A font loaded for titles: fontdue's face for metrics and drawing,
/// and the bytes and shaping tables harfrust shapes with. `shaper` is
/// `None` for a file harfrust cannot read; its text is set unshaped.
pub(crate) struct LoadedFont {
    pub(crate) font: Font,
    data: Cow<'static, [u8]>,
    shaper: Option<ShaperData>,
}

impl LoadedFont {
    pub(crate) fn new(data: Cow<'static, [u8]>) -> Option<Self> {
        let font = Font::from_bytes(&*data, fontdue::FontSettings::default()).ok()?;
        let shaper = FontRef::from_index(&data, 0).ok().map(|f| ShaperData::new(&f));
        Some(Self { font, data, shaper })
    }
}

/// A face a text box is set in: a loaded font and the OpenType
/// features its text is shaped with, on top of the shaper's defaults.
/// Dereferences to the fontdue font, for metrics and drawing.
pub(crate) struct Face<'a> {
    pub(crate) font: &'a LoadedFont,
    features: Vec<Feature>,
}

impl<'a> Face<'a> {
    pub(crate) fn new(font: &'a LoadedFont, features: &[String]) -> Self {
        Self { font, features: parse_features(features) }
    }
}

impl std::ops::Deref for Face<'_> {
    type Target = Font;
    fn deref(&self) -> &Font {
        &self.font.font
    }
}

/// Feature settings in HarfBuzz's syntax: "smcp" or "+smcp" turns a
/// feature on, "-liga" off, "ss01=1" sets a value. One that does not
/// parse is left out.
pub(crate) fn parse_features(features: &[String]) -> Vec<Feature> {
    features.iter().filter_map(|f| f.trim().parse().ok()).collect()
}

/// A line of a [`ShapedLayout`]: its first and last glyph's index, as
/// fontdue's `LinePosition` gives them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Line {
    pub(crate) glyph_start: usize,
    pub(crate) glyph_end: usize,
}

/// A piece of the text appended to a layout: where in the source text
/// it starts, its own text, size and face, and how many characters it
/// has (fontdue lays one glyph per character).
struct Appended {
    base: usize,
    text: String,
    px: f32,
    face: usize,
    chars: usize,
}

/// fontdue's `Layout`, with each line set in shaped glyphs. Text is
/// appended as pieces of a source text, `(start, end, px, face)` byte
/// ranges, and every glyph's `byte_offset` indexes that source text.
/// A ligature is one glyph whose `byte_offset` and `parent` are its
/// first character's; the characters after it in its cluster have no
/// glyph of their own.
pub(crate) struct ShapedLayout {
    inner: Layout,
    settings: LayoutSettings,
    appended: Vec<Appended>,
    glyphs: Vec<GlyphPosition>,
    lines: Vec<Line>,
}

impl ShapedLayout {
    pub(crate) fn new() -> Self {
        Self {
            inner: Layout::new(CoordinateSystem::PositiveYDown),
            settings: LayoutSettings::default(),
            appended: Vec::new(),
            glyphs: Vec::new(),
            lines: Vec::new(),
        }
    }

    pub(crate) fn reset(&mut self, settings: &LayoutSettings) {
        self.settings = *settings;
        self.inner.reset(settings);
        self.appended.clear();
        self.glyphs.clear();
        self.lines.clear();
    }

    /// Lay `text[start..end]` at `px` in `faces[face]` after what is
    /// laid already.
    pub(crate) fn append(&mut self, faces: &[Face], text: &str, (start, end, px, face): (usize, usize, f32, usize)) {
        if start >= end {
            return;
        }
        let piece = &text[start..end];
        let fonts: Vec<&Font> = faces.iter().map(|f| &**f).collect();
        self.inner.append(&fonts, &TextStyle::new(piece, px, face));
        self.appended.push(Appended { base: start, text: piece.to_string(), px, face, chars: piece.chars().count() });
        self.shape(faces);
    }

    pub(crate) fn glyphs(&self) -> &Vec<GlyphPosition> {
        &self.glyphs
    }

    /// The lines laid, `None` before any glyph is.
    pub(crate) fn lines(&self) -> Option<&Vec<Line>> {
        if self.glyphs.is_empty() {
            None
        } else {
            Some(&self.lines)
        }
    }

    /// Set every line fontdue laid again in shaped glyphs: each run of
    /// one piece's characters on a line is shaped on its own, a control
    /// character (a line break) kept as fontdue placed it, and the line
    /// aligned in the box by its shaped width as fontdue aligns it.
    fn shape(&mut self, faces: &[Face]) {
        self.glyphs.clear();
        self.lines.clear();
        let laid = self.inner.glyphs();
        let Some(lines) = self.inner.lines() else { return };
        // Which appended piece each of fontdue's glyphs came from.
        let owner: Vec<usize> = self.appended.iter().enumerate().flat_map(|(k, a)| std::iter::repeat_n(k, a.chars)).collect();
        let align = match (self.settings.max_width, self.settings.horizontal_align) {
            (None, _) | (_, HorizontalAlign::Left) => 0.0,
            (_, HorizontalAlign::Center) => 0.5,
            (_, HorizontalAlign::Right) => 1.0,
        };
        let max_width = self.settings.max_width.unwrap_or(f32::MAX);
        let mut idx = 0;
        for line in lines {
            let first = self.glyphs.len();
            let mut pen = 0.0f32;
            while idx <= line.glyph_end && idx < laid.len() {
                let k = owner[idx];
                let piece = &self.appended[k];
                if laid[idx].char_data.is_control() {
                    // fontdue gives a control character no size and no
                    // advance; it stays where the pen is.
                    let mut g = laid[idx];
                    g.x = pen.floor();
                    g.byte_offset += piece.base;
                    self.glyphs.push(g);
                    idx += 1;
                    continue;
                }
                let mut next = idx + 1;
                while next <= line.glyph_end && next < laid.len() && owner[next] == k && !laid[next].char_data.is_control() {
                    next += 1;
                }
                let from = laid[idx].byte_offset;
                let to = laid[next - 1].byte_offset + laid[next - 1].parent.len_utf8();
                let face = &faces[piece.face];
                set_run(face, piece, from, to, line.baseline_y, &mut pen, &laid[idx..next], &mut self.glyphs);
                idx = next;
            }
            let x = self.settings.x + ((max_width - pen) * align).floor();
            for g in &mut self.glyphs[first..] {
                g.x += x;
            }
            self.lines.push(Line { glyph_start: first, glyph_end: self.glyphs.len().saturating_sub(1) });
        }
    }
}

/// Shape `piece.text[from..to]` in `face` and push its glyphs, the pen
/// starting at `pen` on a line whose baseline is at `baseline`. A face
/// the shaper cannot read sets the characters fontdue laid (`laid`)
/// one by one, unshaped.
#[allow(clippy::too_many_arguments)]
fn set_run(face: &Face, piece: &Appended, from: usize, to: usize, baseline: f32, pen: &mut f32, laid: &[GlyphPosition], out: &mut Vec<GlyphPosition>) {
    let px = piece.px;
    let font: &Font = face;
    let scale = font.scale_factor(px);
    let place = |gid: u16, byte: usize, pen: f32, dx: f32, dy: f32| {
        let parent = piece.text[byte..].chars().next().unwrap_or(' ');
        let m = font.metrics_indexed(gid, px);
        GlyphPosition {
            key: GlyphRasterConfig { glyph_index: gid, px, font_hash: font.file_hash() },
            font_index: piece.face,
            parent,
            x: (pen + dx + m.bounds.xmin).floor(),
            y: (-m.bounds.height - m.bounds.ymin - dy).floor() + baseline,
            width: m.width,
            height: m.height,
            byte_offset: piece.base + byte,
            char_data: CharacterData::classify(parent, gid),
            user_data: (),
        }
    };
    let Some(shaped) = shape(face, &piece.text[from..to]) else {
        for g in laid {
            out.push(place(g.key.glyph_index, g.byte_offset, *pen, 0.0, 0.0));
            *pen += font.metrics_indexed(g.key.glyph_index, px).advance_width.ceil();
        }
        return;
    };
    for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        let gid = u16::try_from(info.glyph_id).ok().filter(|&g| g < font.glyph_count()).unwrap_or(0);
        let byte = from + info.cluster as usize;
        out.push(place(gid, byte, *pen, scale * pos.x_offset as f32, scale * pos.y_offset as f32));
        // Rounded up as fontdue rounds each advance.
        *pen += (scale * pos.x_advance as f32).ceil();
    }
}

/// `text` shaped left to right in `face`, with the shaper's default
/// features (kern, liga, clig, calt, mark, mkmk among them) and the
/// face's own; `None` for a font the shaper cannot read.
fn shape(face: &Face, text: &str) -> Option<harfrust::GlyphBuffer> {
    let data = face.font.shaper.as_ref()?;
    let font = FontRef::from_index(&face.font.data, 0).ok()?;
    let shaper = data.shaper(&font).build();
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    buffer.set_direction(Direction::LeftToRight);
    Some(shaper.shape(buffer, ShapeOptions::new().features(&face.features)))
}
