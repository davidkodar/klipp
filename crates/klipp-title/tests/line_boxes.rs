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

/// #3237: a title whose glyphs a stagger poses, with a backing per line,
/// has a resting render without its backing (the boxes go under the
/// posed glyphs at each frame) and frames that carry the boxes.
#[test]
fn a_staggered_titles_boxes_go_under_its_posed_glyphs() {
    use klipp_timeline::{Motion, MotionKind, StaggerUnit};
    let mut td = TitleData::single_text(TextBox { text: "One\nTwo".into(), font_size: 60.0, ..Default::default() });
    td.background = TitleBackground::Color { color: [0, 0, 0, 200], padding: 12.0, lines: Some(LineBoxes::default()) };
    td.objects[0].fx_mut().motion.enter = Some(Motion::new(MotionKind::Rise).duration(0.5).stagger(StaggerUnit::Line, 0.6));
    let resting = klipp_title::resting_title(&td);
    assert!(matches!(&resting.background, TitleBackground::Color { color, lines: Some(_), .. } if color[3] == 0), "the resting render's backing clear, its lines kept");
    let mut r = klipp_title::TitleRenderer::new();
    match r.frame_with_window(&td, 1920, 1080, None, 0.3, Some(4.0)) {
        klipp_title::TitleFrame::Glyphs { sprite, backing, .. } => {
            let (lines, colour) = backing.expect("the boxes under the glyphs");
            assert_eq!(colour, [0, 0, 0, 200]);
            // Inside the first line's box, clear of its letters: the sprite
            // holds nothing there.
            let b = lines.rects[0];
            let (x, y) = ((b[0] + 3.0) as usize, ((b[1] + b[3]) * 0.5) as usize);
            let at = sprite.pixels.to_bytes();
            assert_eq!(at[(y * 1920 + x) * 4 + 3], 0, "no backing in the resting sprite");
        }
        _ => panic!("a staggered title's glyphs posed"),
    }
}

/// #3237: the bundled Boxed Caption template draws its backing one box
/// per line, a box for each of its two lines.
#[test]
fn the_boxed_caption_template_has_a_box_per_line() {
    let t = klipp_timeline::title_library::builtin("Boxed Caption").expect("the Boxed Caption template");
    let td = t.data();
    assert!(matches!(&td.background, TitleBackground::Color { lines: Some(_), .. }), "{:?}", td.background);
    let layout = klipp_title::TitleRenderer::new().render_layout_at_time(&td, 1920, 1080, None);
    assert_eq!(layout.line_boxes.len(), 2, "a box for each line");
}
