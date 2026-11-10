//! A built-in template's text, made long, stays inside the frame: its lines
//! wrap within the picture, whatever the template's own letter spacing.

use klipp_timeline::title_library::TITLES;

#[test]
fn a_long_cinematic_title_stays_inside_the_frame() {
    let mut td = TITLES.iter().find(|t| t.name == "Cinematic").expect("the Cinematic template").build_for(1920, 1080);
    let tb = td.primary_text_mut().expect("a text");
    eprintln!("tracking {}, size {}, fit {:?}, box {:?}", tb.tracking, tb.font_size, tb.fit, ());
    tb.text = "The harbour wakes up slowly every morning while the boats come back".into();
    let o = td.objects.iter().find(|o| matches!(o.content, klipp_timeline::TitleObjectContent::TextBox(_))).unwrap();
    eprintln!("transform {:?}", o.transform);
    let mut single = td.clone();
    single.objects.retain(|x| x.id == o.id);
    let layout = klipp_title::TitleRenderer::new().render_layout(&single, 1920, 1080);
    eprintln!("ink {:.0}..{:.0} x {:.0}..{:.0}", layout.text_left, layout.text_right, layout.text_top, layout.text_bottom);
    assert!(layout.text_left >= 0.0 && layout.text_right <= 1920.0, "the text {:.0}..{:.0} runs past the frame", layout.text_left, layout.text_right);
}

/// Two centred lines with tracking stay centred over each other, and
/// two right-aligned ones end together: tracking counted along the whole
/// text pushed each line after the first further right (the titles walk:
/// "Harbour Light" over "Evening Tide" pulled apart sideways).
#[test]
fn tracked_lines_keep_their_alignment() {
    use klipp_timeline::{LineBoxes, TextAlignment, TextBox, TitleBackground, TitleData};
    for (alignment, edge) in [(TextAlignment::Center, 0.5f32), (TextAlignment::Right, 1.0)] {
        let mut td = TitleData::single_text(TextBox { text: "Harbour Light\nEvening Tide".into(), font_size: 72.0, tracking: 20.0, alignment, ..Default::default() });
        td.background = TitleBackground::Color { color: [0, 0, 0, 200], padding: 0.0, lines: Some(LineBoxes::default()) };
        let layout = klipp_title::TitleRenderer::new().render_layout_at_time(&td, 1920, 1080, None);
        assert_eq!(layout.line_boxes.len(), 2, "two lines");
        let at = |b: [f32; 4]| b[0] + (b[2] - b[0]) * edge;
        let (a, b) = (at(layout.line_boxes[0]), at(layout.line_boxes[1]));
        assert!((a - b).abs() < 12.0, "{alignment:?}: the lines' {} at {a:.0} and {b:.0}: {:?}", if edge == 0.5 { "centres" } else { "right ends" }, layout.line_boxes);
    }
}
