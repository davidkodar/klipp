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
//!
//! Mixed-direction text and complex scripts (#1502): each line fontdue
//! broke, in the text's logical order, is cut into runs of one face, one
//! bidi level and one script; the runs are put in visual order by the
//! Unicode bidi algorithm, the paragraph's base direction the text
//! box's; and each is shaped in its own direction and script, so Arabic
//! joins, Indic scripts reorder and form conjuncts, and a Hebrew line's
//! English words and numbers read in their own order. Text with no
//! right-to-left letters in a left-to-right box is set as before.

use std::borrow::Cow;
use std::sync::Arc;

use crate::colour::{ColourBitmap, ColourBitmaps};
use crate::variable::Instance;

use fontdue::layout::{
    CharacterData, CoordinateSystem, GlyphPosition, GlyphRasterConfig, HorizontalAlign, Layout,
    LayoutSettings, TextStyle,
};
use fontdue::{Font, Metrics, OutlineBounds};
use harfrust::{Direction, Feature, FontRef, ShapeOptions, ShaperData, ShaperInstance, UnicodeBuffer};

/// Bumped whenever shaping changes where glyphs land, so a glyph cache
/// laid out before does not match (#1501).
pub const SHAPING_VERSION: u32 = 2;

/// A glyph turned a quarter clockwise, as a sideways run of vertical
/// text is (#1507), carries its size negated in its key: its coverage is
/// the upright glyph's turned, and its key apart from it. No other glyph
/// has a size below zero.
pub(crate) fn sideways_px(px: f32) -> f32 {
    -px
}

/// A font loaded for titles: fontdue's face for metrics and drawing,
/// and the bytes and shaping tables harfrust shapes with. `shaper` is
/// `None` for a file harfrust cannot read; its text is set unshaped.
/// `colour` holds the colour bitmaps of an emoji font (#1503).
/// `instance` is a variable face's instance away from its default
/// (#1504): shaped at its coordinates, its glyphs drawn there.
pub(crate) struct LoadedFont {
    pub(crate) font: Font,
    data: Cow<'static, [u8]>,
    shaper: Option<ShaperData>,
    colour: Option<ColourBitmaps>,
    instance: Option<(Instance, ShaperInstance)>,
}

impl LoadedFont {
    pub(crate) fn new(data: Cow<'static, [u8]>) -> Option<Self> {
        Self::at(data, &[])
    }

    /// The font in `data` at `variations`, each an axis's tag and
    /// value (#1504); its default instance for a static face, or for
    /// variations that leave every axis at its default.
    pub(crate) fn at(data: Cow<'static, [u8]>, variations: &[(String, f32)]) -> Option<Self> {
        let font = Font::from_bytes(&*data, fontdue::FontSettings::default()).ok()?;
        let shaper = FontRef::from_index(&data, 0).ok().map(|f| ShaperData::new(&f));
        let colour = ColourBitmaps::of(&data);
        let instance = Instance::new(&data, variations).and_then(|i| {
            let font = FontRef::from_index(&data, 0).ok()?;
            let shaping = ShaperInstance::from_variations(&font, i.variations());
            Some((i, shaping))
        });
        Some(Self { font, data, shaper, colour, instance })
    }

    /// Glyph `gid`'s outline as contours in frame pixels, `px` pixels
    /// per em with its baseline origin at `origin`, at the font's
    /// instance (#1512); `None` for a glyph without one.
    pub(crate) fn outline(&self, gid: u16, px: f32, origin: [f32; 2], tolerance: f32) -> Option<Vec<crate::text3d_mesh::Contour>> {
        let face = match &self.instance {
            Some((i, _)) => i.face(&self.data)?,
            None => ttf_parser::Face::parse(&self.data, 0).ok()?,
        };
        let scale = px / face.units_per_em() as f32;
        crate::text3d_mesh::glyph_contours(&face, ttf_parser::GlyphId(gid), scale, origin, tolerance)
    }

    pub(crate) fn coverage_outline(&self,gid:u16)->Option<Vec<[f32;4]>> {
        if let Some((instance,_))=&self.instance {
            return crate::outline_2d::curve_bins(&instance.face(&self.data)?,gid);
        }
        let face=ttf_parser::Face::parse(&self.data,0).ok()?;
        crate::outline_2d::row_bins(crate::outline_2d::edges(&face,gid)?)
    }

    /// The hash glyph keys carry for this font: fontdue's hash of its
    /// file, and for an instance its coordinates' too (#1504).
    pub(crate) fn hash(&self) -> usize {
        match &self.instance {
            Some((i, _)) => self.font.file_hash() ^ i.hash as usize,
            None => self.font.file_hash(),
        }
    }

    /// Glyph `gid` as a colour bitmap at `px`, for a glyph the font
    /// draws in colour (#1503).
    pub(crate) fn colour_glyph(&self, gid: u16, px: f32) -> Option<Arc<ColourBitmap>> {
        self.colour.as_ref()?.glyph(&self.data, gid, px)
    }

    pub(crate) fn colour_source(&self,gid:u16,px:f32)->Option<crate::colour::ColourSource> {
        self.colour.as_ref()?.source(&self.data,gid,px)
    }

    /// fontdue's metrics for glyph `gid` at `px`, with a colour glyph's
    /// box its bitmap's (#1503): fontdue sees no outline there.
    pub(crate) fn metrics_indexed(&self, gid: u16, px: f32) -> Metrics {
        if let Some((i, _)) = &self.instance {
            return i.metrics(&self.data, gid, px);
        }
        let mut m = self.font.metrics_indexed(gid, px);
        if let Some(c) = self.colour_source(gid, px) {
            m.xmin = c.xmin as i32;
            m.ymin = c.ymin as i32;
            m.width = c.width;
            m.height = c.height;
            m.bounds = OutlineBounds { xmin: c.xmin, ymin: c.ymin, width: c.width as f32, height: c.height as f32 };
        }
        m
    }

    /// Glyph `gid`'s coverage at `px`: fontdue's raster of its outline,
    /// a colour glyph's alpha (#1503), or an instance's glyph (#1504).
    pub(crate) fn coverage_indexed(&self, gid: u16, px: f32) -> (Metrics, Vec<u8>) {
        if let Some((i, _)) = &self.instance {
            return (*i.glyph(&self.data, gid, px)).clone();
        }
        match self.colour_glyph(gid, px) {
            Some(c) => (self.metrics_indexed(gid, px), c.rgba.chunks_exact(4).map(|p| p[3]).collect()),
            None => self.font.rasterize_indexed(gid, px),
        }
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

impl Face<'_> {
    /// The coverage of the glyph `key` names, as fontdue's
    /// `rasterize_config` gives it, or a colour glyph's alpha (#1503).
    pub(crate) fn coverage(&self, key: GlyphRasterConfig) -> (Metrics, Vec<u8>) {
        if key.px < 0.0 {
            let (m, cov) = self.coverage(GlyphRasterConfig { px: -key.px, ..key });
            return turned(m, &cov);
        }
        if self.font.colour.is_none() && self.font.instance.is_none() {
            return self.font.font.rasterize_config(key);
        }
        self.font.coverage_indexed(key.glyph_index, key.px)
    }

    /// [`LoadedFont::colour_glyph`].
    pub(crate) fn colour_glyph(&self, gid: u16, px: f32) -> Option<Arc<ColourBitmap>> {
        self.font.colour_glyph(gid, px)
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
    /// Where the line's baseline is, down the layout (#1508).
    pub(crate) baseline: f32,
    /// How far in from the box's left and right edges the line is set
    /// (#1721): its paragraph's indents.
    pub(crate) left: f32,
    pub(crate) right: f32,
    /// Whether the line is its paragraph's first (#1721).
    pub(crate) starts_paragraph: bool,
}

/// A layout's paragraph settings in its pixels (#1721): each line's
/// indents from the box's start and end edges, a paragraph's first
/// line's past the start's, and the space between paragraphs.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Paragraphs {
    pub(crate) start: f32,
    pub(crate) end: f32,
    pub(crate) first: f32,
    pub(crate) before: f32,
    pub(crate) after: f32,
    /// Tab stops from the start indent, in order, and the step of the
    /// stops past them (#1721).
    pub(crate) tabs: Vec<(f32, klipp_timeline::TabAlign)>,
    pub(crate) tab_step: f32,
    /// The fewest words a wrapped paragraph's last line may have (#1721).
    pub(crate) min_last_words: u8,
}

impl Paragraphs {
    /// The stop a tab at `x` (from the start indent) moves to: the first
    /// set past it; else the start indent itself, for a first line hung
    /// out of it; else the next of the stops every `tab_step`.
    fn stop_after(&self, x: f32) -> (f32, klipp_timeline::TabAlign) {
        if let Some(&stop) = self.tabs.iter().find(|(at, _)| *at > x + 0.5) {
            return stop;
        }
        if x < -0.5 {
            return (0.0, klipp_timeline::TabAlign::Left);
        }
        let step = self.tab_step.max(1.0);
        (((x + 0.5) / step).floor() * step + step, klipp_timeline::TabAlign::Left)
    }
}

/// A piece of the text appended to a layout: where in the source text
/// it starts, its own text, size and face, and how many characters it
/// has (fontdue lays one glyph per character).
struct Appended {
    base: usize,
    /// Where the piece ends in the source text.
    end: usize,
    text: String,
    px: f32,
    face: usize,
    chars: usize,
    /// A line break this layout put in where a line breaks by its
    /// shaped width (#1693); not the text's, and drawn as nothing.
    synthetic: bool,
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
    /// The pieces asked for, `(start, end, px, face)`, in order.
    pieces: Vec<(usize, usize, f32, usize)>,
    glyphs: Vec<GlyphPosition>,
    /// Whether each glyph reads right to left (#1693), for the caret.
    glyph_rtl: Vec<bool>,
    lines: Vec<Line>,
    /// The source text the pieces are of, for the bidi algorithm.
    source: String,
    /// Whether a paragraph's base direction is right to left (#1502).
    rtl: bool,
    /// Indents and the space between paragraphs (#1721).
    paragraphs: Paragraphs,
}

/// A run of one line's characters set together: fontdue's glyphs
/// `from..to` of one piece, at one bidi level, in one script; or a
/// control character alone.
struct Segment {
    from: usize,
    to: usize,
    level: u8,
    control: bool,
}

impl ShapedLayout {
    pub(crate) fn new() -> Self {
        Self {
            inner: Layout::new(CoordinateSystem::PositiveYDown),
            settings: LayoutSettings::default(),
            appended: Vec::new(),
            pieces: Vec::new(),
            glyphs: Vec::new(),
            glyph_rtl: Vec::new(),
            lines: Vec::new(),
            source: String::new(),
            rtl: false,
            paragraphs: Paragraphs::default(),
        }
    }

    /// Set every paragraph's base direction: right to left when `rtl`
    /// (#1502). Kept across a reset.
    pub(crate) fn set_rtl(&mut self, rtl: bool) {
        self.rtl = rtl;
    }

    /// Set the text's paragraphs so (#1721), before anything is laid.
    pub(crate) fn set_paragraphs(&mut self, paragraphs: Paragraphs) {
        self.paragraphs = paragraphs;
    }

    /// How far in from the box's left and right a line is set: its
    /// paragraph's indents, the first line's its own, the start edge
    /// the right in right-to-left text.
    fn pads(&self, first_line: bool) -> (f32, f32) {
        let p = &self.paragraphs;
        let start = p.start + if first_line { p.first } else { 0.0 };
        if self.rtl { (p.end, start) } else { (start, p.end) }
    }

    pub(crate) fn reset(&mut self, settings: &LayoutSettings) {
        self.settings = *settings;
        self.inner.reset(&Self::unwrapped(settings));
        self.appended.clear();
        self.pieces.clear();
        self.glyphs.clear();
        self.lines.clear();
    }

    /// Lay `text[start..end]` at `px` in `faces[face]` after what is
    /// laid already.
    pub(crate) fn append(&mut self, faces: &[Face], text: &str, (start, end, px, face): (usize, usize, f32, usize)) {
        if start >= end {
            return;
        }
        if self.source != text {
            self.source = text.to_string();
        }
        self.pieces.push((start, end, px, face));
        self.relayout(faces);
        self.shape(faces);
    }

    /// fontdue's settings with no width to wrap at: this layout breaks
    /// the lines itself, and gives fontdue the breaks (#1693).
    fn unwrapped(settings: &LayoutSettings) -> LayoutSettings {
        LayoutSettings { max_width: None, ..*settings }
    }

    /// Lay every piece again in fontdue, a line break put in wherever a
    /// line breaks by its shaped width (#1693): fontdue's own rule, at
    /// fontdue's own break opportunities, with each character's advance
    /// the shaper's. Text whose shaped advances are fontdue's breaks
    /// where fontdue broke it, and fontdue still sets each line's height
    /// and baseline.
    fn relayout(&mut self, faces: &[Face]) {
        let starts = self.shaped_breaks(faces);
        self.inner.reset(&Self::unwrapped(&self.settings));
        self.appended.clear();
        let fonts: Vec<&Font> = faces.iter().map(|f| &**f).collect();
        let source = std::mem::take(&mut self.source);
        let mut next_break = starts.iter().peekable();
        for &(start, end, px, face) in &self.pieces {
            let mut from = start;
            loop {
                // A break inside the piece, or at its start.
                while next_break.peek().is_some_and(|&&b| b < from) {
                    next_break.next();
                }
                let cut = next_break.peek().map(|&&b| b).filter(|&b| b < end).unwrap_or(end);
                if cut == from && cut < end {
                    self.inner.append(&fonts, &TextStyle::new("\n", px, face));
                    self.appended.push(Appended { base: from, end: from, text: "\n".into(), px, face, chars: 1, synthetic: true });
                    next_break.next();
                    continue;
                }
                let piece = &source[from..cut];
                self.inner.append(&fonts, &TextStyle::new(piece, px, face));
                self.appended.push(Appended { base: from, end: cut, text: piece.to_string(), px, face, chars: piece.chars().count(), synthetic: false });
                if cut >= end {
                    break;
                }
                from = cut;
            }
        }
        self.source = source;
    }

    /// Where lines start that fontdue's soft breaks would start, the
    /// pieces measured shaped: the source byte of each line's first
    /// character, past the first line, other than after a hard break.
    fn shaped_breaks(&self, faces: &[Face]) -> Vec<usize> {
        use klipp_linebreak::{LinebreakData, Linebreaker, LINEBREAK_NONE};
        let Some(max_width) = self.settings.max_width else { return Vec::new() };
        let mask = LinebreakData::from_mask(self.settings.wrap_style == fontdue::layout::WrapStyle::Word, self.settings.wrap_hard_breaks, true);
        let mut breaker = Linebreaker::new();
        let (mut current, mut start, mut prev, mut at_pos, mut at_idx) = (0.0f32, 0.0f32, LINEBREAK_NONE, 0.0f32, 0usize);
        // Each line as wide as the box less its paragraph's indents.
        let width = |first_line: bool| {
            let (l, r) = self.pads(first_line);
            (max_width - l - r).max(1.0)
        };
        let mut first_line = true;
        let mut bytes: Vec<usize> = Vec::new();
        let mut chars: Vec<(char, f32)> = Vec::new();
        let mut starts = Vec::new();
        for &(a, b, px, face) in &self.pieces {
            let advances = shaped_advances(&faces[face], &self.source[a..b], px);
            for ((i, c), advance) in self.source[a..b].char_indices().zip(advances) {
                let linebreak = breaker.next(c).mask(mask);
                if linebreak >= prev {
                    prev = linebreak;
                    at_pos = current;
                    at_idx = bytes.len().saturating_sub(1);
                }
                if linebreak.is_hard() || current - start + advance > width(first_line) {
                    prev = LINEBREAK_NONE;
                    if !linebreak.is_hard() {
                        starts.push(at_idx + 1);
                    }
                    first_line = linebreak.is_hard();
                    start = at_pos;
                }
                bytes.push(a + i);
                chars.push((c, advance));
                current += advance;
            }
        }
        if self.paragraphs.min_last_words > 1 {
            balance_last_lines(&mut starts, &chars, self.paragraphs.min_last_words as usize, width(false));
        }
        // As source bytes: where the character a line starts with is.
        let end = self.pieces.last().map_or(0, |p| p.1);
        let mut out: Vec<usize> = starts.into_iter().map(|g| bytes.get(g).copied().unwrap_or(end)).collect();
        out.dedup();
        out
    }

    pub(crate) fn glyphs(&self) -> &Vec<GlyphPosition> {
        &self.glyphs
    }

    /// Whether each of [`Self::glyphs`] reads right to left (#1693).
    pub(crate) fn glyph_rtl(&self) -> &[bool] {
        &self.glyph_rtl
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
        self.glyph_rtl.clear();
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
        // The bidi algorithm, when a paragraph is right to left or holds
        // right-to-left letters; else every character is at level 0.
        let base = if self.rtl { unicode_bidi::Level::rtl() } else { unicode_bidi::Level::ltr() };
        let right_to_left = |c: char| {
            use unicode_bidi::BidiClass::*;
            matches!(unicode_bidi::bidi_class(c), R | AL | RLE | RLO | RLI)
        };
        let bidi = (self.rtl || self.appended.iter().any(|a| a.text.chars().any(right_to_left)))
            .then(|| unicode_bidi::BidiInfo::new(&self.source, Some(base)));
        let source_at = |j: usize| self.appended[owner[j]].base + laid[j].byte_offset;
        let mut idx = 0;
        // Whether the line starts a paragraph, and how far down the
        // space between paragraphs has moved it (#1721).
        let (mut first_line, mut drop) = (true, 0.0f32);
        let gap = self.paragraphs.before + self.paragraphs.after;
        for (n, line) in lines.iter().enumerate() {
            if first_line && n > 0 {
                drop += gap;
            }
            let (left, right) = self.pads(first_line);
            let mut hard_break = false;
            let end = (line.glyph_end + 1).min(laid.len());
            // The line's levels, its trailing spaces at the paragraph's.
            let levels = bidi.as_ref().filter(|_| idx < end).and_then(|b| {
                let start = source_at(idx);
                let stop = self.appended[owner[end - 1]].end.min(source_at(end - 1) + laid[end - 1].parent.len_utf8());
                let para = b.paragraphs.iter().find(|p| p.range.contains(&start))?;
                Some((b.reordered_levels(para, start..stop.max(start + 1)), para.level.number()))
            });
            let level_at = |j: usize| levels.as_ref().map_or(0, |(l, _)| l.get(source_at(j)).map_or(0, |l| l.number()));
            let mut segments: Vec<Segment> = Vec::new();
            while idx < end {
                if laid[idx].char_data.is_control() {
                    // A break this layout put in is not the text's.
                    if self.appended[owner[idx]].synthetic {
                        idx += 1;
                        continue;
                    }
                    hard_break |= laid[idx].parent == '\n';
                    let level = levels.as_ref().map_or(0, |(_, p)| *p);
                    segments.push(Segment { from: idx, to: idx + 1, level, control: true });
                    idx += 1;
                    continue;
                }
                let (k, level) = (owner[idx], level_at(idx));
                let mut script = real_script(laid[idx].parent);
                let mut next = idx + 1;
                while next < end && owner[next] == k && !laid[next].char_data.is_control() && level_at(next) == level {
                    match (script, real_script(laid[next].parent)) {
                        (Some(a), Some(b)) if a != b => break,
                        (None, b) => script = b,
                        _ => {}
                    }
                    next += 1;
                }
                segments.push(Segment { from: idx, to: next, level, control: false });
                idx = next;
            }
            let first = self.glyphs.len();
            let mut pen = 0.0f32;
            for s in visual_order(&segments) {
                let s = &segments[s];
                let piece = &self.appended[owner[s.from]];
                if s.control {
                    // fontdue gives a control character no size and no
                    // advance; it stays where the pen is.
                    let mut g = laid[s.from];
                    g.x = pen.floor();
                    g.byte_offset += piece.base;
                    self.glyphs.push(g);
                    self.glyph_rtl.push(s.level % 2 == 1);
                    continue;
                }
                let from = laid[s.from].byte_offset;
                let to = laid[s.to - 1].byte_offset + laid[s.to - 1].parent.len_utf8();
                let face = &faces[piece.face];
                set_run(face, piece, from, to, s.level % 2 == 1, line.baseline_y, &mut pen, &laid[s.from..s.to], &mut self.glyphs);
                self.glyph_rtl.resize(self.glyphs.len(), s.level % 2 == 1);
            }
            // What follows each tab to its stop (#1721), measured from the
            // start indent: a first line's own indent is its offset from it.
            if !self.rtl {
                let from_indent = if first_line { self.paragraphs.first } else { 0.0 };
                pen = set_tabs(&mut self.glyphs, &self.paragraphs, first, pen, from_indent);
            }
            let x = self.settings.x + left + ((max_width - left - right - pen) * align).floor();
            for g in &mut self.glyphs[first..] {
                g.x += x;
                g.y += drop;
            }
            self.lines.push(Line { glyph_start: first, glyph_end: self.glyphs.len().saturating_sub(1), baseline: line.baseline_y + drop, left, right, starts_paragraph: first_line });
            first_line = hard_break;
        }
    }
}

/// Shape `piece.text[from..to]` in `face`, right to left when `rtl`, and
/// push its glyphs left to right, the pen starting at `pen` on a line
/// whose baseline is at `baseline`. A face the shaper cannot read sets
/// the characters fontdue laid (`laid`) one by one, unshaped, reversed
/// for right to left.
#[allow(clippy::too_many_arguments)]
fn set_run(face: &Face, piece: &Appended, from: usize, to: usize, rtl: bool, baseline: f32, pen: &mut f32, laid: &[GlyphPosition], out: &mut Vec<GlyphPosition>) {
    let px = piece.px;
    let font: &Font = face;
    let scale = font.scale_factor(px);
    let place = |gid: u16, byte: usize, pen: f32, dx: f32, dy: f32| {
        let parent = piece.text[byte..].chars().next().unwrap_or(' ');
        // A colour glyph's box is its bitmap's (#1503).
        let m = face.font.metrics_indexed(gid, px);
        GlyphPosition {
            key: GlyphRasterConfig { glyph_index: gid, px, font_hash: face.font.hash() },
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
    let Some(shaped) = shape(face, &piece.text[from..to], rtl) else {
        let in_order: Box<dyn Iterator<Item = &GlyphPosition>> = if rtl { Box::new(laid.iter().rev()) } else { Box::new(laid.iter()) };
        for g in in_order {
            out.push(place(g.key.glyph_index, g.byte_offset, *pen, 0.0, 0.0));
            *pen += face.font.metrics_indexed(g.key.glyph_index, px).advance_width.ceil();
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

/// `text` shaped in `face`, left to right or right to left, in its
/// script (#1502), with the shaper's default features (kern, liga,
/// clig, calt, mark, mkmk among them) and the face's own, at its
/// instance (#1504); its glyphs in visual order, left to right. `None`
/// for a font the shaper cannot read.
fn shape(face: &Face, text: &str, rtl: bool) -> Option<harfrust::GlyphBuffer> {
    let data = face.font.shaper.as_ref()?;
    let font = FontRef::from_index(&face.font.data, 0).ok()?;
    let shaper = data.shaper(&font).instance(face.font.instance.as_ref().map(|(_, s)| s)).build();
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    // The script and language from the text, which is of one script;
    // the direction the bidi algorithm gave it.
    buffer.guess_segment_properties();
    buffer.set_direction(if rtl { Direction::RightToLeft } else { Direction::LeftToRight });
    Some(shaper.shape(buffer, ShapeOptions::new().features(&face.features)))
}

/// `c`'s script, `None` for the characters every script shares
/// (spaces, punctuation, digits, combining marks), which take the
/// script of the run they are in.
fn real_script(c: char) -> Option<unicode_script::Script> {
    use unicode_script::{Script, UnicodeScript};
    Some(c.script()).filter(|s| !matches!(s, Script::Common | Script::Inherited | Script::Unknown))
}

/// Each character's advance in `text` set at `px` in `face`, as a line
/// sets it (#1693): the shaper's, each glyph's rounded up as the line
/// rounds it and given to the character its cluster starts at, the
/// rest of a cluster nothing; a line break's nothing. Unshaped, the
/// face's own advance, rounded up as fontdue rounds it. Script by
/// script, left to right: the width is the same either way.
fn shaped_advances(face: &Face, text: &str, px: f32) -> Vec<f32> {
    let scale = face.font.font.scale_factor(px);
    let mut out = vec![0.0f32; text.chars().count()];
    let index: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let at = |byte: usize| index.partition_point(|&i| i < byte);
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut script = None;
    for (i, c) in text.char_indices() {
        let s = real_script(c);
        match runs.last_mut() {
            Some((_, e)) if s.is_none() || script.is_none() || s == script => *e = i + c.len_utf8(),
            _ => runs.push((i, i + c.len_utf8())),
        }
        script = s.or(script);
    }
    for (a, b) in runs {
        let run = &text[a..b];
        match shape(face, run, false) {
            Some(shaped) => {
                for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
                    if let Some(k) = out.get_mut(at(a + info.cluster as usize)) {
                        *k += (scale * pos.x_advance as f32).ceil();
                    }
                }
            }
            None => {
                for (i, c) in run.char_indices() {
                    if !c.is_control() {
                        out[at(a + i)] = face.font.metrics_indexed(face.font.font.lookup_glyph_index(c), px).advance_width.ceil();
                    }
                }
            }
        }
    }
    // A line break has no width.
    for (k, c) in text.chars().enumerate() {
        if c.is_control() {
            out[k] = 0.0;
        }
    }
    out
}

/// Move what follows each tab on the line whose glyphs start at
/// `first` to its stop (#1721), the line laid from 0 and `pen` wide,
/// its start `from_indent` past the start indent: each tab's text up
/// to the next tab starts, centres, ends or puts its decimal point
/// on the stop, never moving left. The line's new width.
fn set_tabs(glyphs: &mut [GlyphPosition], paragraphs: &Paragraphs, first: usize, pen: f32, from_indent: f32) -> f32 {
    use klipp_timeline::TabAlign;
    let tabs: Vec<usize> = (first..glyphs.len()).filter(|&i| glyphs[i].parent == '\t').collect();
    let mut moved = 0.0f32;
    for (k, &t) in tabs.iter().enumerate() {
        let next = tabs.get(k + 1).copied();
        let start = glyphs[t].x;
        let end = next.map_or(pen + moved, |n| glyphs[n].x);
        let width = (end - start).max(0.0);
        let (stop, align) = paragraphs.stop_after(start + from_indent);
        let stop = stop - from_indent;
        let field = t + 1..next.unwrap_or(glyphs.len());
        let point = field.clone().find(|&i| glyphs[i].parent == '.').map(|i| glyphs[i].x - start);
        let target = match align {
            TabAlign::Left => stop,
            TabAlign::Center => stop - width * 0.5,
            TabAlign::Right => stop - width,
            TabAlign::Decimal => point.map_or(stop - width, |d| stop - d),
        };
        let shift = (target - start).max(0.0).round();
        if shift > 0.0 {
            for g in &mut glyphs[t + 1..] {
                g.x += shift;
            }
            moved += shift;
        }
    }
    pen + moved
}

/// Give each wrapped paragraph's last line at least `min_words` words
/// (#1721): where it has fewer, it starts that many words earlier on the
/// line above, if the line still fits `width` and the line above keeps a
/// word. `starts` are the characters soft-broken lines start at, in
/// order; `chars` every character with its advance.
fn balance_last_lines(starts: &mut [usize], chars: &[(char, f32)], min_words: usize, width: f32) {
    let space = |i: usize| chars[i].0.is_whitespace();
    let words = |from: usize, to: usize| (from..to).filter(|&i| !space(i) && (i == from || space(i - 1))).count();
    for k in 0..starts.len() {
        let s = starts[k];
        // The paragraph's end: its line break, or the text's end.
        let end = (s..chars.len()).find(|&i| chars[i].0 == '\n').unwrap_or(chars.len());
        if starts.get(k + 1).is_some_and(|&n| n < end) {
            continue;
        }
        let have = words(s, end);
        if have == 0 || have >= min_words {
            continue;
        }
        // Where the line above starts: the previous soft break in the
        // paragraph, or the paragraph's start.
        let para = (0..s).rev().find(|&i| chars[i].0 == '\n').map_or(0, |i| i + 1);
        let above = if k > 0 && starts[k - 1] >= para { starts[k - 1] } else { para };
        let word_starts: Vec<usize> = (above + 1..s).filter(|&i| !space(i) && space(i - 1)).collect();
        let Some(&from) = word_starts.iter().rev().nth(min_words - have - 1).or(word_starts.first()) else { continue };
        let last = (from..end).rev().find(|&i| !space(i)).map_or(from, |i| i + 1);
        let wide: f32 = chars[from..last].iter().map(|c| c.1).sum();
        if wide <= width {
            starts[k] = from;
        }
    }
}

/// The widest of the lines `source` was typed on, set as `pieces`
/// (`(start, end, px, face)` byte ranges of it): each character's shaped
/// advance, as the layout measures a line where it breaks one (#1722).
pub(crate) fn widest_typed_line(faces: &[Face], source: &str, pieces: &[(usize, usize, f32, usize)]) -> f32 {
    let (mut widest, mut line) = (0.0f32, 0.0f32);
    for &(a, b, px, face) in pieces {
        let Some(f) = faces.get(face) else { continue };
        for (c, advance) in source[a..b].chars().zip(shaped_advances(f, &source[a..b], px)) {
            if c == '\n' {
                line = 0.0;
            } else {
                line += advance;
                widest = widest.max(line);
            }
        }
    }
    widest
}

/// The order `segments` are drawn in, left to right: the Unicode bidi
/// algorithm's rule L2, every run at a level or higher reversed, from
/// the highest level down to the lowest odd one.
fn visual_order(segments: &[Segment]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..segments.len()).collect();
    let max = segments.iter().map(|s| s.level).max().unwrap_or(0);
    let Some(lowest_odd) = segments.iter().map(|s| s.level).filter(|l| l % 2 == 1).min() else { return order };
    for level in (lowest_odd..=max).rev() {
        let mut i = 0;
        while i < order.len() {
            if segments[order[i]].level < level {
                i += 1;
                continue;
            }
            let start = i;
            while i < order.len() && segments[order[i]].level >= level {
                i += 1;
            }
            order[start..i].reverse();
        }
    }
    order
}

/// A glyph's coverage turned a quarter clockwise (#1507): its top to
/// the right, its box `height` wide and `width` tall.
fn turned(m: Metrics, cov: &[u8]) -> (Metrics, Vec<u8>) {
    let (w, h) = (m.width, m.height);
    let mut out = vec![0u8; w * h];
    for r in 0..h {
        for c in 0..w {
            out[c * h + (h - 1 - r)] = cov[r * w + c];
        }
    }
    (Metrics { width: h, height: w, ..m }, out)
}

/// `text` shaped top to bottom in `face` (#1507), its vertical forms
/// (`vert`, `vrt2`) and vertical advances the font's: each glyph's
/// offset from where it is set is from its vertical origin, the centre
/// of its top. `None` for a font the shaper cannot read.
pub(crate) fn shape_vertical(face: &Face, text: &str) -> Option<harfrust::GlyphBuffer> {
    let data = face.font.shaper.as_ref()?;
    let font = FontRef::from_index(&face.font.data, 0).ok()?;
    let shaper = data.shaper(&font).instance(face.font.instance.as_ref().map(|(_, s)| s)).build();
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    buffer.set_direction(Direction::TopToBottom);
    Some(shaper.shape(buffer, ShapeOptions::new().features(&face.features)))
}

/// `text` shaped left to right or right to left in `face`, as a line's
/// runs are (#1507's sideways runs).
pub(crate) fn shape_horizontal(face: &Face, text: &str, rtl: bool) -> Option<harfrust::GlyphBuffer> {
    shape(face, text, rtl)
}

/// [`turned`] on a bare coverage, for the vertical text tests: the
/// turned box's width and height and its coverage.
#[cfg(test)]
pub(crate) fn turned_for_test(w: usize, h: usize, cov: &[u8]) -> ((usize, usize), Vec<u8>) {
    let m = Metrics { width: w, height: h, ..Metrics::default() };
    let (m, out) = turned(m, cov);
    ((m.width, m.height), out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(name: &str) -> LoadedFont {
        let path = format!("{}/tests/fonts/{name}", env!("CARGO_MANIFEST_DIR"));
        LoadedFont::new(Cow::Owned(std::fs::read(path).unwrap())).unwrap()
    }

    fn latin() -> LoadedFont {
        LoadedFont::new(Cow::Borrowed(include_bytes!("../assets/NotoSans-Regular.ttf"))).unwrap()
    }

    /// `text` laid on one line, each `(start, end, face)` piece in its face.
    fn lay(faces: &[Face], text: &str, pieces: &[(usize, usize, usize)], rtl: bool) -> Vec<GlyphPosition> {
        let mut l = ShapedLayout::new();
        l.reset(&LayoutSettings::default());
        l.set_rtl(rtl);
        for &(a, b, face) in pieces {
            l.append(faces, text, (a, b, 48.0, face));
        }
        l.glyphs().clone()
    }

    fn x_of(glyphs: &[GlyphPosition], text: &str, c: char) -> f32 {
        let at = text.find(c).unwrap();
        glyphs.iter().find(|g| g.byte_offset == at).unwrap_or_else(|| panic!("no glyph for {c}")).x
    }

    /// #1502: an Arabic word's letters take their joined forms, not their
    /// isolated ones, and read right to left; lam before alef takes its
    /// lam-alef form.
    #[test]
    fn arabic_letters_join_and_read_right_to_left() {
        let arabic = font("NotoSansArabic-subset.ttf");
        let faces = [Face::new(&arabic, &[])];
        let word = "سلام";
        let glyphs = lay(&faces, word, &[(0, word.len(), 0)], false);
        let isolated: Vec<u16> = word.chars().map(|c| arabic.font.lookup_glyph_index(c)).collect();
        let joined = glyphs.iter().filter(|g| !isolated.contains(&g.key.glyph_index)).count();
        assert!(joined >= 3, "the joining letters take other forms: {:?} vs {isolated:?}", glyphs.iter().map(|g| g.key.glyph_index).collect::<Vec<_>>());
        let xs: Vec<f32> = word.chars().map(|c| x_of(&glyphs, word, c)).collect();
        assert!(xs.windows(2).all(|w| w[0] > w[1]), "each letter left of the one before it: {xs:?}");
        let lam_alef = "لا";
        let glyphs = lay(&faces, lam_alef, &[(0, lam_alef.len(), 0)], false);
        let lam = arabic.font.lookup_glyph_index('ل');
        assert!(!glyphs.is_empty() && glyphs.iter().all(|g| g.key.glyph_index != lam), "lam takes its form before alef: {:?}", glyphs.iter().map(|g| g.key.glyph_index).collect::<Vec<_>>());
    }

    /// #1502: Devanagari forms a conjunct and reorders a vowel sign that
    /// is written before the consonant it follows.
    #[test]
    fn devanagari_forms_conjuncts_and_reorders() {
        let deva = font("NotoSansDevanagari-subset.ttf");
        let faces = [Face::new(&deva, &[])];
        let ksha = "क्ष";
        let glyphs = lay(&faces, ksha, &[(0, ksha.len(), 0)], false);
        let virama = deva.font.lookup_glyph_index('्');
        assert!(glyphs.len() < 3 && glyphs.iter().all(|g| g.key.glyph_index != virama), "one conjunct, no visible virama: {:?}", glyphs.iter().map(|g| g.key.glyph_index).collect::<Vec<_>>());
        let ki = "कि";
        let glyphs = lay(&faces, ki, &[(0, ki.len(), 0)], false);
        let i_sign = deva.font.lookup_glyph_index('ि');
        let sign = glyphs.iter().find(|g| g.key.glyph_index != deva.font.lookup_glyph_index('क')).unwrap();
        let ka = glyphs.iter().find(|g| g.key.glyph_index == deva.font.lookup_glyph_index('क')).unwrap();
        assert!(sign.x < ka.x, "the i sign ({} as {i_sign}) left of ka: {} vs {}", sign.key.glyph_index, sign.x, ka.x);
    }

    /// #1502: a Hebrew line with English and a number reads in its own
    /// order. In a right-to-left paragraph the English and the number
    /// after it are one left-to-right run (a number takes the direction
    /// of the letters before it), left of the Hebrew, which reads right
    /// to left. In a left-to-right paragraph the Hebrew comes first,
    /// still right to left inside.
    /// #3237: a right-to-left line's box for a backing per line spans
    /// its ink as a left-to-right one's does, whatever order its glyphs
    /// were laid in; and its height is the font's ascent to descent, so a
    /// line of Hebrew and one of Latin with descenders stand as tall as
    /// their faces say.
    #[test]
    fn a_right_to_left_lines_box_spans_its_ink() {
        let (hebrew, latin) = (font("NotoSansHebrew-subset.ttf"), latin());
        let faces = [Face::new(&hebrew, &[]), Face::new(&latin, &[])];
        let text = "שלום עולם";
        let glyphs = lay(&faces, text, &[(0, text.len(), 0)], true);
        let bands: Vec<[f32; 4]> = crate::backing::line_bands(&faces, &glyphs, &[0], false).into_iter().map(|(b, _)| b).collect();
        assert_eq!(bands.len(), 1);
        let ink = glyphs.iter().filter(|g| g.width > 0).fold([f32::MAX, f32::MIN], |e, g| [e[0].min(g.x), e[1].max(g.x + g.width as f32)]);
        assert_eq!([bands[0][0], bands[0][2]], ink, "across: the line's ink");
        let m = hebrew.font.horizontal_line_metrics(48.0).unwrap();
        assert!((bands[0][3] - bands[0][1] - (m.ascent - m.descent)).abs() < 0.5, "down: the face's ascent to descent, {:?}", bands[0]);
    }

    #[test]
    fn a_mixed_hebrew_english_line_reads_in_order() {
        let (hebrew, latin) = (font("NotoSansHebrew-subset.ttf"), latin());
        let faces = [Face::new(&hebrew, &[]), Face::new(&latin, &[])];
        let text = "שלום world 123";
        let split = "שלום".len();
        let pieces = [(0, split, 0), (split, text.len(), 1)];
        let glyphs = lay(&faces, text, &pieces, true);
        let x = |c| x_of(&glyphs, text, c);
        assert!(x('w') < x('o') && x('o') < x('d') && x('d') < x('1') && x('1') < x('2') && x('2') < x('3'), "the English and its number, left to right");
        assert!(x('3') < x('ם') && x('ם') < x('ו') && x('ו') < x('ל') && x('ל') < x('ש'), "then the Hebrew, right to left");
        // A number after Hebrew alone is its own run, still left to right.
        let short = "שלום 123";
        let glyphs = lay(&faces, short, &[(0, split, 0), (split, short.len(), 1)], true);
        let x = |c| x_of(&glyphs, short, c);
        assert!(x('1') < x('2') && x('2') < x('3') && x('3') < x('ם') && x('ש') > x('ל'), "the number reads left to right, left of the Hebrew");
        let glyphs = lay(&faces, text, &pieces, false);
        let x = |c| x_of(&glyphs, text, c);
        assert!(x('ם') < x('ש') && x('ש') < x('w') && x('d') < x('1'), "Hebrew first, right to left inside, then the rest");
    }

    /// Text with no right-to-left letters in a left-to-right box lays as
    /// it did, and mixing scripts in one face's piece shapes each in its
    /// own: a Devanagari conjunct after Latin letters still forms.
    #[test]
    fn left_to_right_text_is_set_as_before_and_scripts_shape_apart() {
        let latin = latin();
        let faces = [Face::new(&latin, &[])];
        let text = "Ward (fi) 12";
        let a = lay(&faces, text, &[(0, text.len(), 0)], false);
        assert!(a.windows(2).all(|w| w[0].x <= w[1].x && w[0].byte_offset < w[1].byte_offset), "in order");
        let deva = font("NotoSansDevanagari-subset.ttf");
        let faces = [Face::new(&deva, &[])];
        let text = "() क्ष";
        let glyphs = lay(&faces, text, &[(0, text.len(), 0)], false);
        let virama = deva.font.lookup_glyph_index('्');
        assert!(glyphs.iter().all(|g| g.key.glyph_index != virama), "the conjunct forms after the brackets");
    }

    /// #1693: lines break by their shaped widths. A line that fits only
    /// once kerned stays one line, where fontdue alone, measuring it
    /// unkerned, wrapped it; a line wider than the box even kerned wraps.
    #[test]
    fn a_line_that_fits_kerned_stays_whole_and_one_too_wide_wraps() {
        let latin = latin();
        let faces = [Face::new(&latin, &[])];
        let text = "AVAVAVAVAV AWAWAWAW";
        let unkerned: f32 = text.chars().map(|c| latin.font.metrics_indexed(latin.font.lookup_glyph_index(c), 48.0).advance_width.ceil()).sum();
        let kerned: f32 = shaped_advances(&faces[0], text, 48.0).iter().sum();
        assert!(kerned + 8.0 < unkerned, "the pairs kern: {kerned} vs {unkerned}");
        let lines_at = |width: f32| {
            let mut l = ShapedLayout::new();
            l.reset(&LayoutSettings { max_width: Some(width), ..LayoutSettings::default() });
            l.append(&faces, text, (0, text.len(), 48.0, 0));
            (l.lines().map_or(0, |v| v.len()), l.glyphs().len())
        };
        let between = (kerned + unkerned) / 2.0;
        assert_eq!(lines_at(between), (1, text.chars().count()), "fits kerned: one line, every glyph");
        let mut fontdue_alone = Layout::new(CoordinateSystem::PositiveYDown);
        fontdue_alone.reset(&LayoutSettings { max_width: Some(between), ..LayoutSettings::default() });
        fontdue_alone.append(&[&latin.font], &TextStyle::new(text, 48.0, 0));
        assert_eq!(fontdue_alone.lines().map_or(0, |v| v.len()), 2, "fontdue alone wraps it");
        assert_eq!(lines_at(kerned - 4.0), (2, text.chars().count()), "too wide even kerned: it wraps, no glyph lost or added");
        // Unkerned text breaks where fontdue breaks it.
        let plain = "one two three four five six seven eight nine ten";
        for width in [120.0f32, 200.0, 333.0] {
            let mut l = ShapedLayout::new();
            l.reset(&LayoutSettings { max_width: Some(width), ..LayoutSettings::default() });
            l.append(&faces, plain, (0, plain.len(), 30.0, 0));
            let mut f = Layout::new(CoordinateSystem::PositiveYDown);
            f.reset(&LayoutSettings { max_width: Some(width), ..LayoutSettings::default() });
            f.append(&[&latin.font], &TextStyle::new(plain, 30.0, 0));
            let ours: Vec<(usize, f32)> = l.lines().unwrap().iter().map(|line| (l.glyphs()[line.glyph_start].byte_offset, line.baseline)).collect();
            let theirs: Vec<(usize, f32)> = f.lines().unwrap().iter().map(|line| (f.glyphs()[line.glyph_start].byte_offset, line.baseline_y)).collect();
            assert_eq!(ours, theirs, "at {width} px the lines start and sit where fontdue's do");
        }
    }


    /// #1693: the in-place editor's caret reads right to left where the
    /// text does. A Hebrew word's characters are marked right to left,
    /// each one's start its right edge; after the word is its left edge;
    /// a Latin word beside it reads left to right.
    #[test]
    fn the_caret_sits_at_a_right_to_left_characters_reading_edge() {
        let mut r = crate::TitleRenderer::new();
        r.system_fallback = false;
        let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/NotoSansHebrew-subset.ttf"));
        let traits = crate::variable::face_traits(&std::fs::read(&path).unwrap()).unwrap();
        let info = crate::font_discovery::FontInfo { family: "Hebrew Test".into(), style: "Regular".into(), path, weight: traits.weight, italic: traits.italic, axes: traits.axes };
        r.font_registry = Arc::new([("Hebrew Test".to_string(), vec![info])].into_iter().collect());
        let title = |text: &str| {
            let tb = klipp_timeline::TextBox { text: text.into(), font_size: 80.0, font_name: "Hebrew Test".into(), text_direction: klipp_timeline::TextDirection::Rtl, ..Default::default() };
            klipp_timeline::TitleData::single_text_fx(tb, klipp_timeline::LayerFx::default())
        };
        let at = r.compute_char_positions(&title("שלום"), 1280, 720);
        assert_eq!(at.len(), 5, "four letters and the end");
        assert!(at[..4].iter().all(|p| p.rtl), "each letter reads right to left");
        // The caret before each letter, at its right edge, steps left.
        let caret: Vec<f32> = at[..4].iter().map(|p| p.x + p.advance).collect();
        assert!(caret.windows(2).all(|w| w[0] > w[1]), "the caret walks right to left: {caret:?}");
        assert!((at[4].x - at[3].x).abs() < 0.01, "after the word: the last letter's left edge");
        let at = r.compute_char_positions(&title("abc שלום"), 1280, 720);
        assert!(!at[0].rtl && !at[1].rtl && at[4].rtl, "Latin reads left to right, the Hebrew right to left");
    }


    /// #1693: a drop cap follows the box's direction. Right to left, the
    /// cap stands at the box's right and the body beside it wraps on its
    /// left; left to right, as before, the cap at the left.
    #[test]
    fn a_drop_cap_stands_at_the_start_of_a_right_to_left_box() {
        let hebrew = font("NotoSansHebrew-subset.ttf");
        let faces = [Face::new(&hebrew, &[])];
        let text = "שלום עולם שלום עולם שלום עולם שלום עולם שלום עולם שלום עולם";
        let cap_end = text.chars().next().unwrap().len_utf8();
        let lay = |rtl: bool| crate::layout_drop_cap_wrap(&faces, text, cap_end, (150.0, 0), 40.0, |from| vec![(from, text.len(), 40.0, 0)], 600.0, rtl).0;
        let rtl = lay(true);
        let cap = rtl.iter().find(|g| g.byte_offset == 0).unwrap();
        assert!((cap.x + cap.width as f32 - 600.0).abs() < 40.0, "the cap at the box's right: {} + {}", cap.x, cap.width);
        let cap_bottom = cap.y + cap.height as f32;
        let beside: Vec<_> = rtl.iter().filter(|g| g.byte_offset > 0 && g.width > 0 && g.y + (g.height as f32) < cap_bottom).collect();
        assert!(!beside.is_empty() && beside.iter().all(|g| g.x + g.width as f32 <= cap.x), "the body beside the cap is left of it");
        let ltr = lay(false);
        let cap = ltr.iter().find(|g| g.byte_offset == 0).unwrap();
        assert!(cap.x < 20.0, "left to right, the cap at the left: {}", cap.x);
    }

}
