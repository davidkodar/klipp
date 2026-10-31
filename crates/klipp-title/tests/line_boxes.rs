//! #3237: a backing drawn per line has a box for each line as laid: a
//! text wrapped in its box gets one per wrapped line, and edited to fewer
//! words, fewer; lines with and without descenders are as tall.

use klipp_timeline::{LineBoxes, TextBox, TextFit, TitleBackground, TitleData};

fn layout(text: &str) -> klipp_title::TitleLayout {
    let mut td = TitleData::single_text(TextBox { text: text.into(), font_size: 60.0, fit: TextFit::Wrap, ..Default::default() });
    td.objects[0].transform.size.x = 0.3;
    td.background = TitleBackground::Color { color: [0, 0, 0, 200], padding: 12.0, lines: Some(LineBoxes::default()) };
    klipp_title::TitleRenderer::new().render_layout_at_time(&td, 1920, 1080, None)
}

#[test]
fn a_box_for_each_line_as_wrapped() {
    let long = layout("one two three four five six seven eight nine ten");
    assert!(long.line_boxes.len() >= 3, "wrapped in a narrow box: {} lines", long.line_boxes.len());
    let short = layout("one two");
    assert_eq!(short.line_boxes.len(), 1, "edited shorter: one line");
    // "ace" has no ascender or descender, "Hkl" ascenders: as tall.
    let two = layout("ace\nHkl");
    assert_eq!(two.line_boxes.len(), 2);
    let h = |b: [f32; 4]| b[3] - b[1];
    assert!((h(two.line_boxes[0]) - h(two.line_boxes[1])).abs() < 1.0, "lines as tall: {:?}", two.line_boxes);
    // Without a backing per line, none are worked out.
    let mut td = TitleData::single_text(TextBox { text: "ace\ngyp".into(), ..Default::default() });
    td.background = TitleBackground::Color { color: [0, 0, 0, 200], padding: 12.0, lines: None };
    assert!(klipp_title::TitleRenderer::new().render_layout_at_time(&td, 1920, 1080, None).line_boxes.is_empty());
}
