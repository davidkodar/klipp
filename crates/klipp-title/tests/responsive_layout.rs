use klipp_timeline::{
    default_text_transform, Shape, ShapeFit, ShapeKind, TextBox, TitleData,
    TitleObjectContent, TitlePosition,
};
use klipp_title::{glyph_layout_hash, TitleRenderer};

fn title(text: &str) -> TitleData {
    TitleData::single_text(TextBox { text: text.into(), font_size: 72.0, ..Default::default() })
}

#[test]
fn explicit_box_wraps_and_invalidates_cursor_layout() {
    let mut renderer = TitleRenderer::new();
    let mut td = title("one two three four five six seven eight nine ten");
    let wide = renderer.render_layout(&td, 960, 540);
    let hash = glyph_layout_hash(&td, 960, 540);
    td.objects[0].transform.size.x = 0.25;
    let narrow = renderer.render_layout(&td, 960, 540);
    assert!(narrow.text_right - narrow.text_left <= 241.0);
    assert!(narrow.text_bottom - narrow.text_top > wide.text_bottom - wide.text_top);
    assert_ne!(hash, glyph_layout_hash(&td, 960, 540));
    let cursor = renderer.compute_char_positions(&td, 960, 540);
    assert!(!cursor.is_empty());
}

#[test]
fn default_full_frame_retains_legacy_margin() {
    let mut renderer = TitleRenderer::new();
    let mut td = title("one two three four five six seven eight nine ten eleven twelve");
    let legacy = renderer.render(&td, 960, 540);
    td.objects[0].transform.size = TitlePosition { x: 0.9, y: 0.9 };
    assert_eq!(legacy, renderer.render(&td, 960, 540));
}

fn with_backing(text: &str) -> TitleData {
    let mut td = title(text);
    let target = td.objects[0].id;
    td.add_object(TitleObjectContent::Shape(Shape {
        kind: ShapeKind::Rectangle { corner_radius: 0.0 },
        fill: Some([255, 0, 0, 255]), gradient: None, stroke: None,
        fit: Some(ShapeFit { target, padding: TitlePosition { x: 20.0, y: 12.0 } }),
    }), default_text_transform());
    // Backing precedes text: bounds must not depend on paint order.
    td.objects.swap(0, 1);
    td
}

fn red_bounds(pixels: &[u8], width: usize) -> (usize, usize, usize, usize) {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, p) in pixels.chunks_exact(4).enumerate() {
        if p[0] == 255 && p[1] == 0 && p[2] == 0 && p[3] == 255 {
            x0 = x0.min(i % width); y0 = y0.min(i / width);
            x1 = x1.max(i % width + 1); y1 = y1.max(i / width + 1);
        }
    }
    (x0, y0, x1, y1)
}

#[test]
fn backing_follows_text_edits_padding_and_position() {
    let mut renderer = TitleRenderer::new();
    let mut td = with_backing("John Doe");
    let short = red_bounds(&renderer.render(&td, 960, 540), 960);
    td.primary_text_mut().unwrap().text = "Dr. Elizabeth Montgomery-Smith".into();
    let long = red_bounds(&renderer.render(&td, 960, 540), 960);
    assert!(long.2 - long.0 > short.2 - short.0);
    let layout = renderer.render_layout(&td, 960, 540);
    assert!((long.0 as f32 - (layout.text_left - 10.0)).abs() <= 1.0);
    assert!((long.1 as f32 - (layout.text_top - 6.0)).abs() <= 1.0);
    td.objects[1].transform.center.y += 0.1;
    let moved = red_bounds(&renderer.render(&td, 960, 540), 960);
    assert!((moved.1 as i32 - long.1 as i32 - 54).abs() <= 1);
    td.objects[1].visible = false;
    assert!(renderer.render(&td, 960, 540).iter().all(|&v| v == 0));
    td.objects.pop();
    assert!(renderer.render(&td, 960, 540).iter().all(|&v| v == 0));
}

#[test]
fn fit_round_trips_and_legacy_shapes_stay_fixed() {
    let td = with_backing("Name");
    let json = serde_json::to_string(&td).unwrap();
    let restored: TitleData = serde_json::from_str(&json).unwrap();
    assert_eq!(td, restored);
    let old: Shape = serde_json::from_str(r#"{"kind":{"Rectangle":{"corner_radius":4.0}},"fill":[0,0,0,255],"stroke":null}"#).unwrap();
    assert!(old.fit.is_none());
}

#[test]
fn styled_text_uses_the_same_wrapping_box() {
    let mut renderer = TitleRenderer::new();
    let mut td = title("one two three four five six seven eight nine ten");
    td.objects[0].transform.size.x = 0.25;
    let plain = renderer.render(&td, 960, 540);
    let tb = td.primary_text_mut().unwrap();
    tb.style_runs.push(klipp_timeline::StyleRun {
        font_size: Some(tb.font_size), ..klipp_timeline::StyleRun::default_for_range(0, tb.text.len())
    });
    assert_eq!(plain, renderer.render(&td, 960, 540));
}

#[test]
fn backing_follows_animated_target_position() {
    let mut renderer = TitleRenderer::new();
    let mut td = with_backing("Name");
    td.objects[1].transform.center_x_keyframes = vec![
        klipp_core::Keyframe { time_secs: 0.0, value: 0.3, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0) },
        klipp_core::Keyframe { time_secs: 1.0, value: 0.7, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0) },
    ];
    let a = red_bounds(&renderer.render_with_time(&td, 960, 540, None, 0.0), 960);
    let b = red_bounds(&renderer.render_with_time(&td, 960, 540, None, 1.0), 960);
    assert!((b.0 as i32 - a.0 as i32 - 384).abs() <= 1);
}
