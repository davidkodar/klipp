//! Vertical title text (#1507): text set top to bottom in columns, the
//! columns from the right as Japanese and Chinese are set (or from the
//! left). Characters that stand upright in vertical text (UAX #50) are
//! shaped top to bottom, so punctuation, brackets and the long vowel
//! mark take their vertical forms and the font's vertical advances;
//! runs that lie sideways (Latin, digits, Hebrew, Arabic) are shaped as
//! a line, in their bidi order, and turned a quarter clockwise. A column
//! breaks at the box's height, between any two upright characters or at
//! a space in a sideways run, keeping closing punctuation off a
//! column's top and opening brackets off its foot.

use fontdue::layout::{CharacterData, GlyphPosition, GlyphRasterConfig};

use crate::shaping::{shape_horizontal, shape_vertical, sideways_px, Face};

/// A piece of the text in one face at one size: `(start, end, px, face)`.
type Piece = (usize, usize, f32, usize);

/// What a column must not start with: closing punctuation, the small
/// kana and the long vowel mark.
const NO_START: &str = "、。，．・：；？！」』）〕］｝〉》】〙〗〟’”ーぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮヵヶ々";
/// What a column must not end with: opening brackets.
const NO_END: &str = "「『（〔［｛〈《【〘〖〝‘“";

/// How vertical text is set.
pub(crate) struct Columns {
    /// The column's length, the box's height in pixels.
    pub(crate) length: f32,
    /// From one column's middle to the next's.
    pub(crate) pitch: f32,
    /// Extra space after each character, along the column.
    pub(crate) tracking: f32,
    /// Columns from the left, not the right.
    pub(crate) left_to_right: bool,
    /// The bidi base direction of a sideways run.
    pub(crate) rtl: bool,
}

/// A glyph set in a column: its place from the column's middle and
/// the column's top, and what it is.
struct Set {
    gid: u16,
    face: usize,
    px: f32,
    byte: usize,
    sideways: bool,
    /// Its drawing origin, across from the column's middle and down from
    /// the column's top, before the column is placed.
    x: f32,
    y: f32,
}

/// A piece of a column that does not break: an upright cluster, or a
/// word of a sideways run. Its glyphs are set from its top.
struct Item {
    glyphs: Vec<Set>,
    advance: f32,
    first: char,
    last: char,
    /// A hard line break after it.
    breaks: bool,
}

/// The text laid in columns: its glyphs, in its order, in frame pixels
/// from the block's top left, and where each column's glyphs start.
pub(crate) fn lay(faces: &[Face], text: &str, pieces: &[Piece], how: &Columns) -> (Vec<GlyphPosition>, Vec<usize>) {
    let items = items(faces, text, pieces, how);
    // Columns: items until the next would pass the column's foot.
    let mut columns: Vec<Vec<Item>> = vec![Vec::new()];
    let mut used = 0.0f32;
    for item in items {
        let column = columns.last_mut().expect("a column");
        let fits = used + item.advance <= how.length + 0.5 || column.is_empty();
        // Closing punctuation stays at the foot of the column before.
        let held = !fits && NO_START.contains(item.first);
        if fits || held {
            used += item.advance;
            let breaks = item.breaks;
            column.push(item);
            if breaks {
                columns.push(Vec::new());
                used = 0.0;
            }
            continue;
        }
        // An opening bracket goes down with what it opens.
        let mut carried = Vec::new();
        while column.len() > 1 && column.last().is_some_and(|i| NO_END.contains(i.last)) {
            carried.push(column.pop().expect("an item"));
        }
        carried.reverse();
        used = carried.iter().map(|i| i.advance).sum::<f32>() + item.advance;
        let breaks = item.breaks;
        carried.push(item);
        columns.push(carried);
        if breaks {
            columns.push(Vec::new());
            used = 0.0;
        }
    }
    while columns.len() > 1 && columns.last().is_some_and(|c| c.is_empty()) {
        columns.pop();
    }
    let n = columns.len();
    let mut glyphs = Vec::new();
    let mut starts = Vec::new();
    for (k, column) in columns.iter().enumerate() {
        let slot = if how.left_to_right { k } else { n - 1 - k };
        let middle = slot as f32 * how.pitch + how.pitch * 0.5;
        starts.push(glyphs.len());
        let mut top = 0.0f32;
        for item in column {
            for g in &item.glyphs {
                glyphs.push(position(faces, text, g, middle + g.x, top + g.y));
            }
            top += item.advance;
        }
    }
    (glyphs, starts)
}

/// A glyph's box placed with its drawing origin at `(x, y)`: upright,
/// the font's box; sideways, the box turned a quarter clockwise about
/// the origin, the glyph's top to the right.
fn position(faces: &[Face], text: &str, g: &Set, x: f32, y: f32) -> GlyphPosition {
    let face = &faces[g.face];
    let m = face.font.metrics_indexed(g.gid, g.px);
    let parent = text[g.byte..].chars().next().unwrap_or(' ');
    let (gx, gy, width, height) = if g.sideways {
        // Upright, the box runs from xmin across and from ymin up; a
        // quarter turn clockwise takes across to down and up to right.
        (x + m.bounds.ymin, y + m.bounds.xmin, m.height, m.width)
    } else {
        (x + m.bounds.xmin, y - m.bounds.height - m.bounds.ymin, m.width, m.height)
    };
    GlyphPosition {
        key: GlyphRasterConfig { glyph_index: g.gid, px: if g.sideways { sideways_px(g.px) } else { g.px }, font_hash: face.font.hash() },
        font_index: g.face,
        parent,
        x: gx.floor(),
        y: gy.floor(),
        width,
        height,
        byte_offset: g.byte,
        char_data: CharacterData::classify(parent, g.gid),
        user_data: (),
    }
}

/// Whether `c` is set upright in vertical text: UAX #50's U, Tu and Tr,
/// shaped top to bottom so the transformed ones (brackets, the long
/// vowel mark, small kana) take the font's vertical forms. Only R, the
/// rest, lies sideways.
fn upright(c: char) -> bool {
    !matches!(unicode_vo::char_orientation(c), unicode_vo::Orientation::Rotated)
}

/// The text's items, in its order: each piece cut into runs upright or
/// sideways, each run shaped, an upright run's clusters and a sideways
/// run's words each an item.
fn items(faces: &[Face], text: &str, pieces: &[Piece], how: &Columns) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    for &(start, end, px, face_i) in pieces {
        let face = &faces[face_i];
        let piece = &text[start..end];
        let mut runs: Vec<(usize, usize, bool)> = Vec::new();
        for (i, c) in piece.char_indices() {
            let up = upright(c) || c == '\n';
            match runs.last_mut() {
                Some((_, e, u)) if *u == up => *e = i + c.len_utf8(),
                _ => runs.push((i, i + c.len_utf8(), up)),
            }
        }
        for (a, b, up) in runs {
            if up {
                upright_items(face, face_i, px, piece, start, a, b, how, &mut out);
            } else {
                sideways_items(face, face_i, px, piece, start, a, b, how, &mut out);
            }
        }
    }
    out
}

/// An upright run's clusters as items, each glyph set about the
/// column's middle from its vertical origin.
#[allow(clippy::too_many_arguments)]
fn upright_items(face: &Face, face_i: usize, px: f32, piece: &str, base: usize, a: usize, b: usize, how: &Columns, out: &mut Vec<Item>) {
    let run = &piece[a..b];
    let scale = face.font.font.scale_factor(px);
    let mut push = |byte: usize, glyphs: Vec<Set>, advance: f32| {
        let c = piece[byte - base..].chars().next().unwrap_or(' ');
        if c == '\n' {
            if let Some(last) = out.last_mut() {
                last.breaks = true;
            } else {
                out.push(Item { glyphs: Vec::new(), advance: 0.0, first: ' ', last: ' ', breaks: true });
            }
            return;
        }
        out.push(Item { glyphs, advance: advance + how.tracking, first: c, last: c, breaks: false });
    };
    let Some(shaped) = shape_vertical(face, run) else {
        // Unshaped: each character upright, an em apart.
        for (i, c) in run.char_indices() {
            let gid = face.font.font.lookup_glyph_index(c);
            let m = face.font.metrics_indexed(gid, px);
            let byte = base + a + i;
            push(byte, vec![Set { gid, face: face_i, px, byte, sideways: false, x: -m.advance_width * 0.5, y: px * 0.88 }], px);
        }
        return;
    };
    // Glyphs by cluster, each cluster an item.
    let mut cluster: Option<(u32, Vec<Set>, f32)> = None;
    for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        let gid = u16::try_from(info.glyph_id).unwrap_or(0);
        let byte = base + a + info.cluster as usize;
        let advance = -(pos.y_advance as f32) * scale;
        // From the vertical origin, y up in the font: across from the
        // middle, and down from the pen.
        let set = Set { gid, face: face_i, px, byte, sideways: false, x: pos.x_offset as f32 * scale, y: -(pos.y_offset as f32) * scale };
        match &mut cluster {
            Some((c, glyphs, adv)) if *c == info.cluster => {
                // A mark set after its base: from the base's pen.
                glyphs.push(Set { y: set.y + *adv, ..set });
                *adv += advance;
            }
            _ => {
                if let Some((c, glyphs, adv)) = cluster.take() {
                    push(base + a + c as usize, glyphs, adv);
                }
                cluster = Some((info.cluster, vec![set], advance));
            }
        }
    }
    if let Some((c, glyphs, adv)) = cluster {
        push(base + a + c as usize, glyphs, adv);
    }
}

/// A sideways run's words as items: the run shaped as a line in its
/// bidi order, then turned a quarter clockwise, its middle on the
/// column's.
#[allow(clippy::too_many_arguments)]
fn sideways_items(face: &Face, face_i: usize, px: f32, piece: &str, base: usize, a: usize, b: usize, how: &Columns, out: &mut Vec<Item>) {
    let run = &piece[a..b];
    let scale = face.font.font.scale_factor(px);
    // The em box's middle on the column's: its baseline left of it by
    // half of ascent plus descent.
    let (ascent, descent) = face.font.font.horizontal_line_metrics(px).map_or((px * 0.8, -px * 0.2), |m| (m.ascent, m.descent));
    let baseline = -(ascent + descent) * 0.5;
    // The run's bidi order: its visual runs, each in its direction.
    let level = if how.rtl { unicode_bidi::Level::rtl() } else { unicode_bidi::Level::ltr() };
    let bidi = unicode_bidi::BidiInfo::new(run, Some(level));
    let mut line: Vec<(Set, f32)> = Vec::new();
    for para in &bidi.paragraphs {
        let (levels, visual) = bidi.visual_runs(para, para.range.clone());
        for range in visual {
            let rtl = levels[range.start].is_rtl();
            let text = &run[range.clone()];
            match shape_horizontal(face, text, rtl) {
                Some(shaped) => {
                    for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
                        let gid = u16::try_from(info.glyph_id).unwrap_or(0);
                        let byte = base + a + range.start + info.cluster as usize;
                        let set = Set { gid, face: face_i, px, byte, sideways: true, x: baseline + pos.y_offset as f32 * scale, y: pos.x_offset as f32 * scale };
                        line.push((set, (pos.x_advance as f32 * scale).ceil()));
                    }
                }
                None => {
                    for (i, c) in text.char_indices() {
                        let gid = face.font.font.lookup_glyph_index(c);
                        let advance = face.font.metrics_indexed(gid, px).advance_width.ceil();
                        line.push((Set { gid, face: face_i, px, byte: base + a + range.start + i, sideways: true, x: baseline, y: 0.0 }, advance));
                    }
                }
            }
        }
    }
    // Words, broken at spaces, each set from its own top.
    let mut word: Vec<Set> = Vec::new();
    let mut pen = 0.0f32;
    let flush = |word: &mut Vec<Set>, pen: &mut f32, out: &mut Vec<Item>| {
        if word.is_empty() && *pen == 0.0 {
            return;
        }
        let first = word.first().map_or(' ', |g| piece[g.byte - base..].chars().next().unwrap_or(' '));
        let last = word.last().map_or(' ', |g| piece[g.byte - base..].chars().next().unwrap_or(' '));
        out.push(Item { glyphs: std::mem::take(word), advance: *pen, first, last, breaks: false });
        *pen = 0.0;
    };
    for (g, advance) in line {
        let c = piece[g.byte - base..].chars().next().unwrap_or(' ');
        word.push(Set { y: g.y + pen, ..g });
        pen += advance + how.tracking;
        if c == ' ' {
            flush(&mut word, &mut pen, out);
        }
    }
    flush(&mut word, &mut pen, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaping::LoadedFont;
    use std::borrow::Cow;

    fn font(name: &str) -> LoadedFont {
        let path = format!("{}/tests/fonts/{name}", env!("CARGO_MANIFEST_DIR"));
        LoadedFont::new(Cow::Owned(std::fs::read(path).unwrap())).unwrap()
    }

    fn how(length: f32) -> Columns {
        Columns { length, pitch: 48.0, tracking: 0.0, left_to_right: false, rtl: false }
    }

    fn glyph_of<'a>(glyphs: &'a [GlyphPosition], text: &str, c: char) -> &'a GlyphPosition {
        let at = text.find(c).unwrap();
        glyphs.iter().find(|g| g.byte_offset == at).unwrap_or_else(|| panic!("no glyph for {c}"))
    }

    /// #1507's done-when: a two-column Japanese title draws with vertical
    /// punctuation forms, its columns right to left, each top to bottom,
    /// and a column never starts with closing punctuation.
    #[test]
    fn a_two_column_japanese_title_takes_vertical_forms_right_to_left() {
        let jp = font("NotoSansCJKjp-subset.otf");
        let faces = [Face::new(&jp, &[])];
        let text = "縦書きの題、「かぎ」。ーテスト";
        let (glyphs, starts) = lay(&faces, text, &[(0, text.len(), 40.0, 0)], &how(40.0 * 8.0));
        assert_eq!(starts.len(), 2, "two columns: {starts:?}");
        for c in ['、', '。', '「', '」', 'ー'] {
            assert_ne!(glyph_of(&glyphs, text, c).key.glyph_index, jp.font.lookup_glyph_index(c), "{c} takes its vertical form");
        }
        assert_eq!(glyph_of(&glyphs, text, '縦').key.glyph_index, jp.font.lookup_glyph_index('縦'), "a kanji stands as it is");
        let (first, second) = (&glyphs[..starts[1]], &glyphs[starts[1]..]);
        let right_of = |a: &[GlyphPosition], b: &[GlyphPosition]| a.iter().map(|g| g.x).fold(f32::MAX, f32::min) > b.iter().map(|g| g.x + g.width as f32).fold(f32::MIN, f32::max);
        assert!(right_of(first, second), "the first column on the right");
        for column in [first, second] {
            let tops: Vec<f32> = column.iter().filter(|g| g.height > 0).map(|g| g.y).collect();
            assert!(tops.windows(2).all(|w| w[0] < w[1] + 1.0), "top to bottom: {tops:?}");
        }
        let starts_with = text[second[0].byte_offset..].chars().next().unwrap();
        assert!(!NO_START.contains(starts_with), "a column does not start with {starts_with}");
        assert!(glyphs.iter().all(|g| g.key.px > 0.0), "every character here stands upright");

        // From the left when asked.
        let (glyphs, starts) = lay(&faces, text, &[(0, text.len(), 40.0, 0)], &Columns { left_to_right: true, ..how(40.0 * 8.0) });
        assert!(right_of(&glyphs[starts[1]..], &glyphs[..starts[1]]), "the first column on the left");
    }

    /// Where a column breaks, a closing mark stays at the foot of the
    /// column before, and an opening bracket goes down with what it opens.
    #[test]
    fn closing_marks_stay_up_and_opening_brackets_go_down() {
        let jp = font("NotoSansCJKjp-subset.otf");
        let faces = [Face::new(&jp, &[])];
        // Five kanji fill a column of five ems; the full stop stays.
        let text = "縦書きの題。テスト";
        let advance = |t: &str| lay(&faces, t, &[(0, t.len(), 40.0, 0)], &how(10_000.0)).0.len();
        assert_eq!(advance("縦"), 1);
        let em = {
            let (g, _) = lay(&faces, "縦書", &[(0, "縦書".len(), 40.0, 0)], &how(10_000.0));
            g[1].y - g[0].y
        };
        let (glyphs, starts) = lay(&faces, text, &[(0, text.len(), 40.0, 0)], &how(em * 5.0));
        assert_eq!(text[glyphs[starts[1]].byte_offset..].chars().next(), Some('テ'), "the full stop held at the foot");
        // A bracket that would end the first column starts the second.
        let text = "縦書きの「かぎ」";
        let (glyphs, starts) = lay(&faces, text, &[(0, text.len(), 40.0, 0)], &how(em * 5.0));
        assert_eq!(text[glyphs[starts[1]].byte_offset..].chars().next(), Some('「'), "the bracket goes down: {starts:?}");
    }

    /// Latin in vertical text lies sideways, turned a quarter clockwise,
    /// reading down; a Hebrew word does too, in its own order.
    #[test]
    fn latin_and_hebrew_runs_lie_sideways_in_their_order() {
        let (jp, hebrew) = (font("NotoSansCJKjp-subset.otf"), font("NotoSansHebrew-subset.ttf"));
        let faces = [Face::new(&jp, &[]), Face::new(&hebrew, &[])];
        let text = "縦ABC題";
        let (glyphs, _) = lay(&faces, text, &[(0, text.len(), 40.0, 0)], &how(1000.0));
        let (a, b, c) = (glyph_of(&glyphs, text, 'A'), glyph_of(&glyphs, text, 'B'), glyph_of(&glyphs, text, 'C'));
        assert!([a, b, c].iter().all(|g| g.key.px < 0.0), "turned");
        let upright_a = jp.font.metrics_indexed(a.key.glyph_index, 40.0);
        assert_eq!((a.width, a.height), (upright_a.height, upright_a.width), "its box turned");
        assert!(a.y < b.y && b.y < c.y, "reading down");
        assert!(glyph_of(&glyphs, text, '縦').y < a.y && c.y < glyph_of(&glyphs, text, '題').y, "between the kanji, in order");

        let text = "שלום";
        let (glyphs, _) = lay(&faces, text, &[(0, text.len(), 40.0, 1)], &Columns { rtl: true, ..how(1000.0) });
        assert!(glyph_of(&glyphs, text, 'ם').y < glyph_of(&glyphs, text, 'ש').y, "a right-to-left word reads up the column, its last letter on top");
    }

    /// A turned glyph's coverage is the upright one's, a quarter turn
    /// clockwise: its top row on the right.
    #[test]
    fn a_sideways_glyph_is_drawn_turned() {
        let (m, cov) = crate::shaping::turned_for_test(3, 2, &[1, 2, 3, 4, 5, 6]);
        assert_eq!((m.0, m.1), (2, 3));
        // Rows of the upright: [1 2 3] over [4 5 6]; turned clockwise, the
        // left column reads 4 5 6 down and the right 1 2 3.
        assert_eq!(cov, [4, 1, 5, 2, 6, 3]);
    }

    /// The renderer sets a vertical box this way: two columns of ink, the
    /// first on the right.
    #[test]
    fn a_vertical_title_renders_in_columns() {
        let mut r = crate::TitleRenderer::new();
        r.system_fallback = false;
        let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/NotoSansCJKjp-subset.otf"));
        let traits = crate::variable::face_traits(&std::fs::read(&path).unwrap()).unwrap();
        let info = crate::font_discovery::FontInfo { family: "JP Test".into(), style: "Regular".into(), path, weight: traits.weight, italic: traits.italic, axes: traits.axes };
        r.font_registry = std::sync::Arc::new([("JP Test".to_string(), vec![info])].into_iter().collect());
        let tb = klipp_timeline::TextBox { text: "縦書きの題\nテスト".into(), font_size: 60.0, font_name: "JP Test".into(), vertical_layout: true, ..Default::default() };
        let td = klipp_timeline::TitleData::single_text_fx(tb, klipp_timeline::LayerFx::default());
        let layout = r.render_layout(&td, 1920, 1080);
        assert!(layout.mask_h > layout.mask_w, "taller than wide: {}x{}", layout.mask_w, layout.mask_h);
        let ink_x: Vec<usize> = (0..layout.mask_w as usize).filter(|&x| (0..layout.mask_h as usize).any(|y| layout.alpha_mask[y * layout.mask_w as usize + x] > 0)).collect();
        let gaps = ink_x.windows(2).filter(|w| w[1] > w[0] + 4).count();
        assert_eq!(gaps, 1, "two columns of ink");
    }
}
