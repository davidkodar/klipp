//! A text box's paragraphs (#1721): space between them, and the indents
//! every line, the first line and a hanging first line take, in the
//! render and the caret alike; a box without them sets as it did.
use klipp_timeline::{Paragraph, TextAlignment, TextBox, TitleData, TitlePosition};
use klipp_title::{CharPosition, TitleRenderer};

const W: u32 = 1920;
const H: u32 = 1080;
const PARA: &str = "The quick brown fox jumps over the lazy dog and runs far away from here";

/// A left-aligned box half the frame wide at 1080p, so lengths are pixels.
fn boxed(text: &str, align: TextAlignment, p: Paragraph) -> TitleData {
    let mut td = TitleData::single_text(TextBox { text: text.into(), font_size: 48.0, alignment: align, paragraph: p, ..Default::default() });
    td.objects[0].transform.size = TitlePosition { x: 0.5, y: 0.9 };
    td
}

/// Where each laid line is, starts and ends, left to right, from the
/// caret: (its lowest glyph top, start, end). A line begins where the
/// caret steps back to the left. Spaces are left out: a line's last
/// one hangs past its end.
fn lines(text: &str, chars: &[CharPosition]) -> Vec<(f32, f32, f32)> {
    let mut out: Vec<(f32, f32, f32)> = Vec::new();
    let mut last_x = f32::MAX;
    let seen = |c: &&CharPosition| c.advance > 0.0 && !text[c.byte_offset..].starts_with(char::is_whitespace);
    for c in chars.iter().filter(seen) {
        match out.last_mut() {
            Some((y, x0, x1)) if c.x >= last_x - 1.0 => {
                *y = y.min(c.y);
                *x0 = x0.min(c.x);
                *x1 = x1.max(c.x + c.advance);
            }
            _ => out.push((c.y, c.x, c.x + c.advance)),
        }
        last_x = c.x;
    }
    out
}

fn caret(td: &TitleData) -> Vec<(f32, f32, f32)> {
    let text = td.primary_text().unwrap().text.clone();
    lines(&text, &TitleRenderer::new().compute_char_positions(td, W, H))
}

#[test]
fn space_before_and_after_open_the_paragraphs_apart() {
    let text = "First paragraph\nSecond one\nThird";
    let plain = caret(&boxed(text, TextAlignment::Left, Paragraph::default()));
    let spaced = caret(&boxed(text, TextAlignment::Left, Paragraph { space_before: 10.0, space_after: 20.0, ..Default::default() }));
    assert_eq!((plain.len(), spaced.len()), (3, 3));
    assert!((spaced[0].0 - plain[0].0 - (spaced[1].0 - plain[1].0 - 30.0)).abs() < 1.5, "the first paragraph has nothing above it");
    let gap = |l: &[(f32, f32, f32)], i: usize| l[i + 1].0 - l[i].0;
    for i in 0..2 {
        assert!((gap(&spaced, i) - gap(&plain, i) - 30.0).abs() < 1.5, "paragraph {i}: {} vs {}", gap(&spaced, i), gap(&plain, i));
    }
    // The drawn text is as much taller.
    let mut r = TitleRenderer::new();
    let (a, b) = (r.render_layout(&boxed(text, TextAlignment::Left, Paragraph::default()), W, H), r.render_layout(&boxed(text, TextAlignment::Left, Paragraph { space_before: 10.0, space_after: 20.0, ..Default::default() }), W, H));
    assert!(((b.text_bottom - b.text_top) - (a.text_bottom - a.text_top) - 60.0).abs() < 2.0);
}

#[test]
fn a_first_line_indents_and_a_hanging_first_line_hangs() {
    let indented = caret(&boxed(PARA, TextAlignment::Left, Paragraph { indent_first: 60.0, ..Default::default() }));
    assert!(indented.len() >= 2);
    assert!((indented[0].1 - indented[1].1 - 60.0).abs() < 1.5, "the first line starts 60 px in: {indented:?}");
    let hanging = caret(&boxed(PARA, TextAlignment::Left, Paragraph { indent_start: 60.0, indent_first: -60.0, ..Default::default() }));
    assert!((hanging[1].1 - hanging[0].1 - 60.0).abs() < 1.5, "the lines under a hanging first line sit 60 px in: {hanging:?}");
    // The hanging line is where an unindented box's lines start.
    let plain = caret(&boxed(PARA, TextAlignment::Left, Paragraph::default()));
    assert!((hanging[0].1 - plain[0].1).abs() < 1.5);
    // The render's ink agrees with the caret.
    let ink = TitleRenderer::new().render_layout(&boxed(PARA, TextAlignment::Left, Paragraph { indent_start: 60.0, indent_first: -60.0, ..Default::default() }), W, H);
    assert!((ink.text_left - hanging[0].1).abs() < 4.0, "{} vs {}", ink.text_left, hanging[0].1);
}

#[test]
fn indents_narrow_the_lines_and_move_them_in() {
    let plain = caret(&boxed(PARA, TextAlignment::Left, Paragraph::default()));
    let inset = caret(&boxed(PARA, TextAlignment::Left, Paragraph { indent_start: 80.0, indent_end: 200.0, ..Default::default() }));
    assert!(inset.len() > plain.len(), "a narrower measure takes more lines: {} vs {}", inset.len(), plain.len());
    for l in &inset {
        assert!(l.1 - plain[0].1 >= 79.0, "each line starts 80 px in: {l:?}");
        assert!(l.2 - plain[0].1 <= 960.0 - 200.0 + 1.0, "and ends inside the end indent: {l:?}");
    }
    // Justified, each line but the last fills the measure to its end indent.
    let just = caret(&boxed(PARA, TextAlignment::Justify, Paragraph { indent_start: 80.0, indent_end: 200.0, ..Default::default() }));
    let base = just[0].1 - 80.0;
    for l in &just[..just.len() - 1] {
        assert!((l.2 - base - (960.0 - 200.0)).abs() < 6.0, "justified to the end indent: {l:?} from {base}");
    }
}

#[test]
fn a_box_without_paragraph_settings_sets_as_before_and_saves_none() {
    let td = boxed(PARA, TextAlignment::Left, Paragraph::default());
    let json = serde_json::to_string(&td).unwrap();
    assert!(!json.contains("paragraph"), "{json}");
    let p = Paragraph { space_after: 12.0, indent_first: -30.0, indent_start: 30.0, ..Default::default() };
    let td = boxed(PARA, TextAlignment::Left, p.clone());
    let back: TitleData = serde_json::from_str(&serde_json::to_string(&td).unwrap()).unwrap();
    assert_eq!(back, td);
}

#[test]
fn a_keyed_indent_moves_with_time() {
    use klipp_core::title_effect_keys as k;
    let mut tb = TextBox { text: PARA.into(), ..Default::default() };
    for (t, v) in [(0.0, 0.0), (1.0, 100.0)] {
        klipp_core::keyframe::upsert_keyframe(tb.effect_keyframes_mut_for(k::TEXT_INDENT_FIRST).unwrap(), t, v);
    }
    assert_eq!(tb.at(0.5).paragraph.indent_first, 50.0);
    assert!(tb.at(0.5).paragraph.indent_first_keyframes.is_empty(), "evaluated, its keys spent");
    // A first line never hangs past the box's edge.
    let p = Paragraph { indent_start: 20.0, indent_first: -80.0, ..Default::default() };
    assert_eq!(p.at(0.0).indent_first, -20.0);
}

/// In right-to-left text a paragraph starts at the right: its first
/// line's indent is in from the right edge.
#[test]
fn a_right_to_left_first_line_indents_from_the_right() {
    let hebrew = "שלום עולם זהו טקסט ארוך מאוד שנכתב כדי לבדוק שבירת שורות בעברית ועוד מילים רבות כאן";
    let rtl = |p: Paragraph| {
        let mut td = boxed(hebrew, TextAlignment::Right, p);
        td.primary_text_mut().unwrap().text_direction = klipp_timeline::TextDirection::Rtl;
        TitleRenderer::new().compute_char_positions(&td, W, H)
    };
    // Each line's right end: the caret runs right to left along a line
    // and steps back to the right where the next begins.
    let rights = |chars: &[CharPosition]| {
        let mut out: Vec<f32> = Vec::new();
        let mut last_x = f32::MIN;
        for c in chars.iter().filter(|c| c.advance > 0.0 && !hebrew[c.byte_offset..].starts_with(' ')) {
            match out.last_mut() {
                Some(r) if c.x <= last_x + 1.0 => *r = r.max(c.x + c.advance),
                _ => out.push(c.x + c.advance),
            }
            last_x = c.x;
        }
        out
    };
    let plain = rights(&rtl(Paragraph::default()));
    let indented = rights(&rtl(Paragraph { indent_first: 60.0, ..Default::default() }));
    assert!(plain.len() >= 2 && indented.len() >= 2, "{plain:?}");
    assert!((plain[0] - plain[1]).abs() < 2.0, "flush right: {plain:?}");
    assert!((indented[1] - indented[0] - 60.0).abs() < 2.0, "the first line ends 60 px in from the right: {indented:?}");
}
