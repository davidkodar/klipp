//! Lists (#1721): each paragraph with text gets a marker at the start
//! indent and its text `indent` past it; numbered items count from their
//! start, skipping empty paragraphs; the caret follows the text.
use klipp_timeline::{ListMarker, ListStyle, NumberStyle, Paragraph, TextAlignment, TextBox, TitleData, TitlePosition};
use klipp_title::TitleRenderer;

const W: u32 = 1920;
const H: u32 = 1080;

fn boxed(text: &str, list: Option<ListStyle>) -> TitleData {
    let mut td = TitleData::single_text(TextBox { text: text.into(), font_size: 40.0, alignment: TextAlignment::Left, paragraph: Paragraph { list, ..Default::default() }, ..Default::default() });
    td.objects[0].transform.size = TitlePosition { x: 0.6, y: 0.9 };
    td
}

fn numbered(start: u32, indent: f32) -> Option<ListStyle> {
    Some(ListStyle { marker: ListMarker::Numbered { style: NumberStyle::Arabic, start, suffix: ".".into() }, indent })
}

/// The ink's columns, left to right: the first and last with ink.
fn ink_columns(px: &[u8]) -> (usize, usize) {
    let (mut l, mut r) = (usize::MAX, 0);
    for (i, p) in px.chunks_exact(4).enumerate() {
        if p[3] > 40 {
            l = l.min(i % W as usize);
            r = r.max(i % W as usize);
        }
    }
    (l, r)
}

#[test]
fn numbers_count_in_their_style() {
    let n = |style, k| style_label(style, k);
    assert_eq!((n(NumberStyle::Arabic, 1), n(NumberStyle::Arabic, 12)), ("1".into(), "12".into()));
    assert_eq!((n(NumberStyle::LowerAlpha, 1), n(NumberStyle::LowerAlpha, 26), n(NumberStyle::LowerAlpha, 27)), ("a".into(), "z".into(), "aa".into()));
    assert_eq!(n(NumberStyle::UpperAlpha, 28), "AB");
    assert_eq!((n(NumberStyle::LowerRoman, 4), n(NumberStyle::LowerRoman, 1994), n(NumberStyle::UpperRoman, 9)), ("iv".into(), "mcmxciv".into(), "IX".into()));
    let m = ListMarker::Numbered { style: NumberStyle::LowerAlpha, start: 3, suffix: ")".into() };
    assert_eq!((m.label(0), m.label(1)), ("c)".into(), "d)".into()));
    assert_eq!(ListMarker::Bullet { mark: "•".into() }.label(5), "•");
}

fn style_label(style: NumberStyle, n: usize) -> String {
    style.count(n)
}

#[test]
fn a_numbered_list_sets_its_text_past_its_markers() {
    let text = "Apples\nBananas\nCherries";
    let mut r = TitleRenderer::new();
    let caret = |td: &TitleData, r: &mut TitleRenderer, c: char| {
        let t = &td.primary_text().unwrap().text;
        let b = t.find(c).unwrap();
        r.compute_char_positions(td, W, H).into_iter().find(|p| p.byte_offset == b).unwrap().x
    };
    let list = boxed(text, numbered(1, 60.0));
    let (a, b, ch) = (caret(&list, &mut r, 'A'), caret(&list, &mut r, 'B'), caret(&list, &mut r, 'C'));
    assert!((a - b).abs() < 3.0 && (b - ch).abs() < 3.0, "the items' text lines up: {a} {b} {ch}");
    // The text sits 60 px in from where the markers start: the drawn
    // ink starts left of the caret by the indent, less the marker's
    // side bearing.
    let (ink_left, _) = ink_columns(&r.render(&list, W, H));
    assert!(((a - ink_left as f32) - 60.0).abs() < 6.0, "markers 60 px left of the text: ink at {ink_left}, text at {a}");
    // Without a list the text starts where the ink does.
    let plain = boxed(text, None);
    let (plain_left, _) = ink_columns(&r.render(&plain, W, H));
    assert!((caret(&plain, &mut r, 'A') - plain_left as f32).abs() < 4.0);
    // The markers draw: there is more ink.
    let sum = |px: Vec<u8>| px.chunks_exact(4).map(|p| p[3] as u64).sum::<u64>();
    assert!(sum(r.render(&list, W, H)) > sum(r.render(&plain, W, H)));
}

#[test]
fn an_empty_paragraph_is_not_an_item() {
    let mut r = TitleRenderer::new();
    // From 9 the count shows in the digits: "9." and "10." if the empty
    // paragraph is skipped, "9." and "11." if it were counted, as many
    // glyphs either way, so the list is set against one with no gap.
    let mut glyphs = |text: &str, start| r.render_layout(&boxed(text, numbered(start, 60.0)), W, H).glyph_count;
    let with_gap = glyphs("A\n\nB", 9);
    let without = glyphs("A\nB", 9);
    // The gap's own line break is a glyph; a marker would be two more.
    assert_eq!(with_gap, without + 1, "an empty paragraph has no marker and takes no number");
    // And a three-item list from 9 has "9.", "10." and "11.".
    assert_eq!(glyphs("A\nB\nC", 9) - glyphs("A\nB\nC", 1), 2, "two more digits than 1., 2., 3.");
}

#[test]
fn a_bullet_marks_each_item_and_a_missing_one_falls_back() {
    let mut r = TitleRenderer::new();
    let bullets = |mark: &str| boxed("One\nTwo", Some(ListStyle { marker: ListMarker::Bullet { mark: mark.into() }, indent: 50.0 }));
    let plain = r.render_layout(&boxed("One\nTwo", None), W, H).glyph_count;
    assert_eq!(r.render_layout(&bullets("•"), W, H).glyph_count, plain + 2, "a bullet each");
    // A mark the face cannot draw is drawn as "-", one glyph each.
    assert_eq!(r.render_layout(&bullets("\u{10FFFD}"), W, H).glyph_count, plain + 2);
}

#[test]
fn a_right_to_left_list_marks_its_items_on_the_right() {
    let mut r = TitleRenderer::new();
    let hebrew = "שלום\nעולם";
    let rtl = |list: Option<ListStyle>| {
        let mut td = boxed(hebrew, list);
        let tb = td.primary_text_mut().unwrap();
        tb.text_direction = klipp_timeline::TextDirection::Rtl;
        tb.alignment = TextAlignment::Right;
        td
    };
    let (_, plain_right) = ink_columns(&r.render(&rtl(None), W, H));
    let (_, list_right) = ink_columns(&r.render(&rtl(numbered(1, 60.0)), W, H));
    let caret_right = r.compute_char_positions(&rtl(numbered(1, 60.0)), W, H).iter().map(|c| c.x + c.advance).fold(f32::MIN, f32::max);
    assert!(list_right as f32 > caret_right + 30.0, "the markers sit right of the text: ink to {list_right}, text to {caret_right}");
    assert!((list_right as i32 - plain_right as i32).abs() < 6, "the box keeps its right edge: {list_right} vs {plain_right}");
}

#[test]
fn a_list_round_trips_and_none_is_unwritten() {
    let td = boxed("A\nB", Some(ListStyle { marker: ListMarker::Numbered { style: NumberStyle::UpperRoman, start: 2, suffix: ")".into() }, indent: 48.0 }));
    let back: TitleData = serde_json::from_str(&serde_json::to_string(&td).unwrap()).unwrap();
    assert_eq!(back, td);
    assert!(!serde_json::to_string(&boxed("A", None)).unwrap().contains("list"));
}
