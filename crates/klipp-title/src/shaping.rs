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

    /// fontdue's metrics for glyph `gid` at `px`, with a colour glyph's
    /// box its bitmap's (#1503): fontdue sees no outline there.
    pub(crate) fn metrics_indexed(&self, gid: u16, px: f32) -> Metrics {
        if let Some((i, _)) = &self.instance {
            return i.glyph(&self.data, gid, px).0;
        }
        let mut m = self.font.metrics_indexed(gid, px);
        if let Some(c) = self.colour_glyph(gid, px) {
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
        if self.font.colour.is_none() && self.font.instance.is_none() {
            return self.font.font.rasterize_config(key);
        }
        self.font.coverage_indexed(key.glyph_index, key.px)
    }

    /// [`LoadedFont::coverage_indexed`].
    pub(crate) fn coverage_indexed(&self, gid: u16, px: f32) -> (Metrics, Vec<u8>) {
        self.font.coverage_indexed(gid, px)
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
    /// The source text the pieces are of, for the bidi algorithm.
    source: String,
    /// Whether a paragraph's base direction is right to left (#1502).
    rtl: bool,
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
            glyphs: Vec::new(),
            lines: Vec::new(),
            source: String::new(),
            rtl: false,
        }
    }

    /// Set every paragraph's base direction: right to left when `rtl`
    /// (#1502). Kept across a reset.
    pub(crate) fn set_rtl(&mut self, rtl: bool) {
        self.rtl = rtl;
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
        if self.source != text {
            self.source = text.to_string();
        }
        self.appended.push(Appended { base: start, end, text: piece.to_string(), px, face, chars: piece.chars().count() });
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
        for line in lines {
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
                    continue;
                }
                let from = laid[s.from].byte_offset;
                let to = laid[s.to - 1].byte_offset + laid[s.to - 1].parent.len_utf8();
                let face = &faces[piece.face];
                set_run(face, piece, from, to, s.level % 2 == 1, line.baseline_y, &mut pen, &laid[s.from..s.to], &mut self.glyphs);
            }
            let x = self.settings.x + ((max_width - pen) * align).floor();
            for g in &mut self.glyphs[first..] {
                g.x += x;
            }
            self.lines.push(Line { glyph_start: first, glyph_end: self.glyphs.len().saturating_sub(1) });
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
}
