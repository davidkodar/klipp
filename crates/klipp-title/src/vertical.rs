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
    /// A drop cap's room (#1704): the first this many columns start
    /// this far down, below the cap.
    pub(crate) indent: (usize, f32),
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
#[cfg(test)]
pub(crate) fn lay(faces: &[Face], text: &str, pieces: &[Piece], how: &Columns) -> (Vec<GlyphPosition>, Vec<usize>) {
    let (glyphs, starts, _) = lay_cells(faces, text, pieces, how);
    (glyphs, starts)
}

/// [`lay`], and each glyph's cell `[left, top, length, width]`: the
/// stretch of its column it takes, from its column's left edge and its
/// top, down the column and across it. The caret goes along a cell's
/// top and foot (#1704).
pub(crate) fn lay_cells(faces: &[Face], text: &str, pieces: &[Piece], how: &Columns) -> (Vec<GlyphPosition>, Vec<usize>, Vec<[f32; 4]>) {
    let items = items(faces, text, pieces, how);
    // Columns: items until the next would pass the column's foot.
    let mut columns: Vec<Vec<Item>> = vec![Vec::new()];
    let mut used = 0.0f32;
    // A column under the drop cap is shorter by the cap.
    let room = |k: usize| if k < how.indent.0 { (how.length - how.indent.1).max(0.0) } else { how.length };
    for item in items {
        let k = columns.len() - 1;
        let column = columns.last_mut().expect("a column");
        let fits = used + item.advance <= room(k) + 0.5 || column.is_empty();
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
    let mut cells = Vec::new();
    for (k, column) in columns.iter().enumerate() {
        let slot = if how.left_to_right { k } else { n - 1 - k };
        let middle = slot as f32 * how.pitch + how.pitch * 0.5;
        starts.push(glyphs.len());
        let mut top = if k < how.indent.0 { how.indent.1 } else { 0.0 };
        for item in column {
            for (i, g) in item.glyphs.iter().enumerate() {
                glyphs.push(position(faces, text, g, middle + g.x, top + g.y));
                // A sideways word's glyph from its pen to the next's; an
                // upright cluster or pair shares the item's cell.
                let cell = if g.sideways {
                    let next = item.glyphs.get(i + 1).map_or(item.advance, |n| n.y);
                    [top + g.y, (next - g.y).max(0.0)]
                } else {
                    [top, item.advance]
                };
                cells.push([middle - how.pitch * 0.5, cell[0], cell[1], how.pitch]);
            }
            top += item.advance;
        }
    }
    (glyphs, starts, cells)
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
            } else if upright_pair(&piece[a..b]) {
                pair_item(face, face_i, px, piece, start, a, b, how, &mut out);
            } else {
                sideways_items(face, face_i, px, piece, start, a, b, how, &mut out);
            }
        }
    }
    out
}

/// Whether a run that would lie sideways stands upright instead, side
/// by side in one cell of the column (tate-chu-yoko, #1704): one or two
/// ASCII digits or letters, such as "12" or "AB".
fn upright_pair(run: &str) -> bool {
    (1..=2).contains(&run.chars().count()) && run.chars().all(|c| c.is_ascii_alphanumeric())
}

/// A short run set upright and side by side in one em of the column
/// (#1704): shaped as a line, squeezed to the em when wider, its middle
/// on the column's and its em box in the cell's middle.
#[allow(clippy::too_many_arguments)]
fn pair_item(face: &Face, face_i: usize, px: f32, piece: &str, base: usize, a: usize, b: usize, how: &Columns, out: &mut Vec<Item>) {
    let run = &piece[a..b];
    let scale = face.font.font.scale_factor(px);
    // Each glyph and its advance at full size.
    let mut line: Vec<(u16, usize, f32, f32)> = Vec::new();
    match shape_horizontal(face, run, false) {
        Some(shaped) => {
            for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
                line.push((u16::try_from(info.glyph_id).unwrap_or(0), base + a + info.cluster as usize, pos.x_offset as f32 * scale, pos.x_advance as f32 * scale));
            }
        }
        None => {
            for (i, c) in run.char_indices() {
                let gid = face.font.font.lookup_glyph_index(c);
                line.push((gid, base + a + i, 0.0, face.font.metrics_indexed(gid, px).advance_width));
            }
        }
    }
    let wide: f32 = line.iter().map(|g| g.3).sum();
    let k = if wide > px { px / wide } else { 1.0 };
    let small = px * k;
    let (ascent, descent) = face.font.font.horizontal_line_metrics(small).map_or((small * 0.8, -small * 0.2), |m| (m.ascent, m.descent));
    let baseline = px * 0.5 + (ascent + descent) * 0.5;
    let mut pen = -wide * k * 0.5;
    let mut glyphs = Vec::with_capacity(line.len());
    for (gid, byte, offset, advance) in line {
        glyphs.push(Set { gid, face: face_i, px: small, byte, sideways: false, x: pen + offset * k, y: baseline });
        pen += advance * k;
    }
    let first = run.chars().next().unwrap_or(' ');
    let last = run.chars().last().unwrap_or(' ');
    out.push(Item { glyphs, advance: px + how.tracking, first, last, breaks: false });
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
        Columns { length, pitch: 48.0, tracking: 0.0, left_to_right: false, rtl: false, indent: (0, 0.0) }
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
        let mut r = jp_renderer();
        let tb = klipp_timeline::TextBox { text: "縦書きの題\nテスト".into(), font_size: 60.0, font_name: "JP Test".into(), vertical_layout: true, ..Default::default() };
        let td = klipp_timeline::TitleData::single_text_fx(tb, klipp_timeline::LayerFx::default());
        let layout = r.render_layout(&td, 1920, 1080);
        assert!(layout.mask_h > layout.mask_w, "taller than wide: {}x{}", layout.mask_w, layout.mask_h);
        let ink_x: Vec<usize> = (0..layout.mask_w as usize).filter(|&x| (0..layout.mask_h as usize).any(|y| layout.alpha_mask[y * layout.mask_w as usize + x] > 0)).collect();
        let gaps = ink_x.windows(2).filter(|w| w[1] > w[0] + 4).count();
        assert_eq!(gaps, 1, "two columns of ink");
    }

    /// A renderer whose only face is the Japanese test font, "JP Test".
    fn jp_renderer() -> crate::TitleRenderer {
        let mut r = crate::TitleRenderer::new();
        r.system_fallback = false;
        let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/NotoSansCJKjp-subset.otf"));
        let traits = crate::variable::face_traits(&std::fs::read(&path).unwrap()).unwrap();
        let info = crate::font_discovery::FontInfo { family: "JP Test".into(), style: "Regular".into(), path, weight: traits.weight, italic: traits.italic, axes: traits.axes };
        r.font_registry = std::sync::Arc::new([("JP Test".to_string(), vec![info])].into_iter().collect());
        r
    }

    /// `text` in a vertical "JP Test" box, 60 px, `set` on it.
    fn vertical_title(text: &str, set: impl FnOnce(&mut klipp_timeline::TextBox)) -> klipp_timeline::TitleData {
        let mut tb = klipp_timeline::TextBox { text: text.into(), font_size: 60.0, font_name: "JP Test".into(), vertical_layout: true, ..Default::default() };
        set(&mut tb);
        klipp_timeline::TitleData::single_text_fx(tb, klipp_timeline::LayerFx::default())
    }

    /// A box's middle.
    fn mid(b: [i32; 4]) -> (f32, f32) {
        ((b[0] + b[2]) as f32 * 0.5, (b[1] + b[3]) as f32 * 0.5)
    }

    /// #1704: two digits or two Latin letters between kanji stand
    /// upright, side by side in one cell of the column, its middle on the
    /// column's; three lie sideways as before.
    #[test]
    fn two_digits_stand_upright_in_one_cell() {
        let jp = font("NotoSansCJKjp-subset.otf");
        let faces = [Face::new(&jp, &[])];
        let em = 40.0;
        let one = |text: &str| lay(&faces, text, &[(0, text.len(), em, 0)], &how(10_000.0)).0;
        for (text, a, b) in [("縦20題", '2', '0'), ("縦AB題", 'A', 'B')] {
            let glyphs = one(text);
            let (first, second) = (glyph_of(&glyphs, text, a), glyph_of(&glyphs, text, b));
            assert!(first.key.px > 0.0 && second.key.px > 0.0, "{text}: upright");
            assert!(first.x + first.width as f32 <= second.x + 1.0, "{text}: side by side, in reading order");
            assert!((first.y + first.height as f32 * 0.5 - (second.y + second.height as f32 * 0.5)).abs() < 3.0, "{text}: on one row");
            // In the column: its middle the column's (pitch 48, so 24).
            let (left, right) = (first.x, second.x + second.width as f32);
            assert!(((left + right) * 0.5 - 24.0).abs() < 3.0 && right - left <= em + 1.0, "{text}: centred within an em: {left}..{right}");
            // One cell: the kanji after it one em further down than
            // the pair is below the kanji before.
            let (before, after) = (glyph_of(&glyphs, text, '縦'), glyph_of(&glyphs, text, '題'));
            assert!(((after.y - before.y) - 2.0 * em).abs() <= 3.0, "{text}: the pair takes one em: {} to {}", before.y, after.y);
            assert!(before.y + before.height as f32 <= first.y && second.y + second.height as f32 <= after.y, "{text}: between the kanji");
        }
        // Three letters lie sideways.
        let glyphs = one("縦ABC題");
        assert!(glyph_of(&glyphs, "縦ABC題", 'A').key.px < 0.0);
    }

    /// #1704: ruby, a run's reading gloss, at half the run's size: down
    /// the right of its column in vertical text, over its text in
    /// horizontal text, centred on what it reads, drawn in the run's
    /// colour.
    #[test]
    fn ruby_sits_beside_its_base_text() {
        let mut r = jp_renderer();
        let text = "縦書の題";
        let base = "縦書".len();
        let glossed = |vertical: bool| {
            vertical_title(text, |tb| {
                tb.vertical_layout = vertical;
                tb.style_runs.push(klipp_timeline::StyleRun { ruby: Some("かき".into()), color: Some([255, 0, 0, 255]), ..klipp_timeline::StyleRun::default_for_range(0, base) });
            })
        };
        for vertical in [true, false] {
            let layout = r.render_layout(&glossed(vertical), 1920, 1080);
            let chars = text.chars().count();
            assert_eq!(layout.glyph_count, chars + 2, "the text and its two-letter gloss");
            let (kanji, gloss) = (&layout.glyph_boxes[..2], &layout.glyph_boxes[chars..]);
            assert_eq!(&layout.glyph_parents[chars..], ['か', 'き']);
            let height = |b: &[i32; 4]| b[3] - b[1];
            assert!(gloss.iter().all(|g| height(g) < height(&kanji[0]) * 2 / 3), "small: {gloss:?} by {kanji:?}");
            let span = |bs: &[[i32; 4]]| (bs.iter().map(|b| b[0]).min().unwrap(), bs.iter().map(|b| b[1]).min().unwrap(), bs.iter().map(|b| b[2]).max().unwrap(), bs.iter().map(|b| b[3]).max().unwrap());
            let (k, g) = (span(kanji), span(gloss));
            if vertical {
                assert!(g.0 >= k.2, "right of the column: gloss {g:?}, base {k:?}");
                assert!(g.0 - k.2 < 30, "close by: {g:?}, {k:?}");
                assert!(((g.1 + g.3) - (k.1 + k.3)).abs() <= 8, "centred down the base: {g:?}, {k:?}");
                assert!(g.1 > k.1 && g.3 < k.3, "a gloss shorter than its base sits within it: {g:?}, {k:?}");
            } else {
                assert!(g.3 <= k.1, "above the base: gloss {g:?}, base {k:?}");
                assert!(k.1 - g.3 < 30, "close by: {g:?}, {k:?}");
                assert!(((g.0 + g.2) - (k.0 + k.2)).abs() <= 8, "centred over the base: {g:?}, {k:?}");
            }
            // Drawn, in the run's colour.
            let px = r.render(&glossed(vertical), 1920, 1080);
            let (cx, cy) = mid(gloss[0]);
            let red = (g.1..g.3).flat_map(|y| (g.0..g.2).map(move |x| (x, y))).filter(|&(x, y)| {
                let p = &px[((y as usize) * 1920 + x as usize) * 4..][..4];
                p[3] > 128 && p[0] > 200 && p[1] < 60
            });
            assert!(red.count() > 20, "the gloss is drawn in red near ({cx}, {cy})");
        }
        // A run without a gloss adds nothing.
        let plain = r.render_layout(&vertical_title(text, |_| {}), 1920, 1080);
        assert_eq!(plain.glyph_count, text.chars().count());
    }

    /// #1704: a vertical drop cap stands at the top of the first column,
    /// as many columns wide as it is; the body starts below it in those
    /// columns and at the top in the ones after.
    #[test]
    fn a_vertical_drop_cap_stands_atop_the_first_column() {
        let mut r = jp_renderer();
        let text = "縦書きの題\nかきあいう\nかきあいう\nかきあいう\nかきあいう\nかきあいう";
        let capped = r.render_layout(&vertical_title(text, |tb| tb.drop_cap_scale = 3.0), 1920, 1080);
        let plain = r.render_layout(&vertical_title(text, |_| {}), 1920, 1080);
        let (cap, rest) = (capped.glyph_boxes[0], &capped.glyph_boxes[1..]);
        let inked: Vec<[i32; 4]> = rest.iter().copied().filter(|b| b[2] > b[0]).collect();
        let (top, right) = (inked.iter().map(|b| b[1]).min().unwrap(), inked.iter().map(|b| b[2]).max().unwrap());
        // Three times the size, at the top right of the block.
        let size = |b: [i32; 4]| b[3] - b[1];
        assert!(size(cap) > size(plain.glyph_boxes[0]) * 5 / 2, "a cap: {cap:?} vs {:?}", plain.glyph_boxes[0]);
        assert!((cap[1] - top).abs() < 30 && cap[1] <= top + 30, "at the top: {cap:?}, the body from {top}");
        assert!((cap[2] - right).abs() < 40, "over the first column, on the right: {cap:?}, the body to {right}");
        // The body's first character goes below the cap, in its column.
        let first = capped.glyph_boxes[1];
        assert!(first[1] >= cap[3], "below the cap: {first:?} under {cap:?}");
        assert!(mid(first).0 > cap[0] as f32 && mid(first).0 < cap[2] as f32, "in a column under the cap: {first:?}, {cap:?}");
        // A column past the cap's starts at the top, left of it.
        let beside = inked.iter().filter(|b| b[2] <= cap[0] && (b[1] - top).abs() < 10).count();
        assert!(beside >= 1, "the body beside the cap, from the top: {inked:?}");
        // Without a drop cap, the first character is a body character.
        assert!(size(plain.glyph_boxes[0]) < 80);
    }

    /// #1704: a vertical box on a drawn path follows it with its columns:
    /// down a column is along the path, so a column of long vowel marks
    /// (each upright a vertical bar) lies along a level path as a line,
    /// the characters in their order from the path's start.
    #[test]
    fn a_vertical_text_follows_a_drawn_path_with_its_columns() {
        use klipp_core::vector_path::{PathContour, PathPose, PathVertex};
        use klipp_timeline::{ObjectTransform, PathTextAlign, Shape, ShapeKind, TextPath, TitleData, TitleObjectContent, TitlePosition, VectorPath};
        let level = |p: [f32; 2]| PathVertex { point: p, incoming: p, outgoing: p };
        let pose = PathPose { stroke_glow: None, contours: vec![PathContour { closed: false, vertices: vec![level([0.1, 0.5]), level([0.9, 0.5])] }], fill: [0.0; 4], stroke: [0.0; 4], stroke_width: 0.0 };
        let on_path = |text: &str, vertical: bool| {
            let mut td = TitleData::default();
            td.objects.clear();
            let whole = ObjectTransform { center: TitlePosition { x: 0.5, y: 0.5 }, size: TitlePosition { x: 1.0, y: 1.0 }, ..klipp_timeline::default_text_transform() };
            let layer = td.add_object(TitleObjectContent::Shape(Shape { kind: ShapeKind::Path(VectorPath { trim: None, pose: pose.clone(), keyframes: Vec::new() }), fill: None, gradient: None, stroke: None, fit: None }), whole);
            let tb = klipp_timeline::TextBox {
                text: text.into(),
                font_size: 60.0,
                font_name: "JP Test".into(),
                vertical_layout: vertical,
                text_path: Some(TextPath::Layer { layer, contour: 0, offset: 0.1, offset_keyframes: Vec::new(), align: PathTextAlign::Start, flip: false }),
                ..Default::default()
            };
            td.add_object(TitleObjectContent::TextBox(tb), klipp_timeline::default_text_transform());
            td
        };
        let mut r = jp_renderer();
        let ink = |px: &[u8]| {
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
            for (i, p) in px.chunks_exact(4).enumerate() {
                if p[3] > 100 {
                    let (x, y) = (i % 1920, i / 1920);
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
            }
            (x0, y0, x1, y1)
        };
        // Off the path, the marks stand in a column: tall and thin.
        let column = ink(&r.render(&vertical_title("ーーー", |_| {}), 1920, 1080));
        assert!(column.3 - column.1 > (column.2 - column.0) * 3, "a column: {column:?}");
        // On the level path, the column lies along it: long and low,
        // about the path's height.
        let laid = r.render(&on_path("ーーー", true), 1920, 1080);
        let (x0, y0, x1, y1) = ink(&laid);
        assert!(x1 - x0 > (y1 - y0) * 3, "along the path: {:?}", (x0, y0, x1, y1));
        // Each mark turned to lie along it: the ink a line, not three
        // standing bars.
        let inked_cols = (x0..=x1).filter(|&x| (y0..=y1).any(|y| laid[(y * 1920 + x) * 4 + 3] > 100)).count();
        assert!(inked_cols as f32 > (x1 - x0) as f32 * 0.7, "a line along the path: {inked_cols} of {}", x1 - x0);
        assert!(((y0 + y1) as f32 * 0.5 - 540.0).abs() < 20.0, "on it: {y0}..{y1}");
        assert!((x0 as f32 - 1920.0 * 0.1 - 1920.0 * 0.8 * 0.1).abs() < 40.0, "from where the text starts on it: {x0}");
        // The characters in their order from the path's start.
        let layout = r.render_layout(&on_path("縦書題", true), 1920, 1080);
        let xs: Vec<f32> = layout.glyph_boxes.iter().map(|b| mid(*b).0).collect();
        assert!(xs.windows(2).all(|w| w[0] + 30.0 < w[1]), "in order along the path: {xs:?}");
        assert!(layout.glyph_boxes.iter().all(|b| (mid(*b).1 - 540.0).abs() < 15.0), "each on the path: {:?}", layout.glyph_boxes);
        // A horizontal text keeps its own way: the marks lie along the
        // path too, each a level bar.
        let flat = ink(&r.render(&on_path("ーーー", false), 1920, 1080));
        assert!(flat.2 - flat.0 > (flat.3 - flat.1) * 3, "{flat:?}");
    }

    /// #1704: in vertical text the editor's characters are where the
    /// columns set them: each cell holds its glyph, down its column; a
    /// line break starts the next column, to the left, at the top.
    #[test]
    fn the_caret_positions_follow_the_columns() {
        let mut r = jp_renderer();
        let text = "縦書き\nの題";
        let td = vertical_title(text, |_| {});
        let at = r.compute_char_positions(&td, 1920, 1080);
        assert_eq!(at.len(), text.chars().count() + 1, "every character and the end");
        assert!(at.iter().all(|p| p.vertical));
        let layout = r.render_layout(&td, 1920, 1080);
        // Glyph k is character k, but for the line break (character 3).
        for (k, ci) in [0usize, 1, 2, 4, 5].into_iter().enumerate() {
            let (cx, cy) = mid(layout.glyph_boxes[k]);
            let p = &at[ci];
            assert!(cx > p.x && cx < p.x + p.height && cy > p.y && cy < p.y + p.advance, "character {ci} holds its glyph: {p:?} vs {:?}", layout.glyph_boxes[k]);
        }
        assert!(at[0].y < at[1].y && at[1].y < at[2].y && at[0].x == at[2].x, "down the first column");
        assert!(at[4].x + at[4].height <= at[0].x + 1.0 && (at[4].y - at[0].y).abs() < 1.0, "the next column, left, from the top");
        assert!((at[6].y - (at[5].y + at[5].advance)).abs() < 1e-3, "the end after the last");
        assert_ne!(crate::glyph_layout_hash(&td, 1920, 1080), crate::glyph_layout_hash(&vertical_title(text, |tb| tb.vertical_layout = false), 1920, 1080), "the cached positions follow the switch");
    }
}
