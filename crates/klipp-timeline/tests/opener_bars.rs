//! The Opener composition's two bars, in each layout it ships (16:9,
//! 9:16 and 1:1): a letterbox, the full width of the frame, one on its
//! top edge and one on its bottom edge, the title between them.

use klipp_timeline::composition_template::builtin_templates;
use klipp_timeline::TitleObjectContent;

#[test]
fn the_opener_bars_sit_on_the_frame_edges_in_every_layout() {
    let opener = builtin_templates().into_iter().find(|t| t.name == "Opener").expect("the Opener");
    for (w, h) in [(1920, 1080), (1080, 1920), (1080, 1080)] {
        let layout = opener.composition_for(w, h).expect("a layout");
        let td = layout.tracks.iter().flat_map(|t| &t.clips).find_map(|c| c.title_data()).expect("its title");
        let bars: Vec<_> = td.objects.iter().filter(|o| matches!(o.content, TitleObjectContent::Shape(_))).map(|o| &o.transform).collect();
        let title = td.objects.iter().find(|o| matches!(o.content, TitleObjectContent::TextBox(_))).expect("its title text");
        assert_eq!(bars.len(), 2, "{w}x{h}: two bars");
        let (top, bottom) = (bars[0], bars[1]);
        let edge = |t: &klipp_timeline::ObjectTransform| (t.center.y - t.anchor_norm.y * t.size.y, t.center.y + (1.0 - t.anchor_norm.y) * t.size.y);
        let (top_from, top_to) = edge(top);
        let (bottom_from, bottom_to) = edge(bottom);
        assert!(top.size.x >= 1.0 && bottom.size.x >= 1.0, "{w}x{h}: the bars as wide as the frame");
        assert!(top_from.abs() < 1e-4 && top_to > 0.0, "{w}x{h}: the top bar on the top edge, {top_from}..{top_to}");
        assert!((bottom_to - 1.0).abs() < 1e-4 && bottom_from < 1.0, "{w}x{h}: the bottom bar on the bottom edge, {bottom_from}..{bottom_to}");
        assert!(top_to < title.transform.center.y && title.transform.center.y < bottom_from, "{w}x{h}: the title between the bars");
    }
}
