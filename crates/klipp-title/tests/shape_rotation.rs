//! A rectangle turns with its transform's rotation, about its anchor,
//! the way a text box and a path do.

use klipp_timeline::*;
use klipp_title::TitleRenderer;

const W: u32 = 320;
const H: u32 = 180;

fn rect(size: (f32, f32), rotation: f32, anchor: (f32, f32)) -> Vec<u8> {
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
    let mut xf = default_text_transform();
    xf.center = TitlePosition { x: 0.5, y: 0.5 };
    xf.size = TitlePosition { x: size.0, y: size.1 };
    xf.rotation = rotation;
    xf.anchor_norm = TitlePosition { x: anchor.0, y: anchor.1 };
    td.add_object(TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fill: Some([40, 80, 200, 255]), gradient: None, stroke: None, fit: None }), xf);
    TitleRenderer::new().render(&td, W, H)
}

fn alpha(buf: &[u8], x: u32, y: u32) -> u8 {
    buf[((y * W + x) * 4 + 3) as usize]
}

/// A wide bar turned a quarter stands upright through the centre.
#[test]
fn a_bar_turned_a_quarter_stands_upright() {
    let flat = rect((0.5, 0.1), 0.0, (0.5, 0.5));
    assert_eq!(alpha(&flat, 100, 90), 255);
    assert_eq!(alpha(&flat, 160, 30), 0);

    let upright = rect((0.5, 0.1), 90.0, (0.5, 0.5));
    assert_eq!(alpha(&upright, 160, 30), 255, "the bar reaches up");
    assert_eq!(alpha(&upright, 160, 150), 255, "and down");
    assert_eq!(alpha(&upright, 100, 90), 0, "and no longer sideways");
}

/// Turned half round its top-left corner, the box swings up and left.
#[test]
fn rotation_pivots_on_the_anchor() {
    let swung = rect((0.25, 0.25), 180.0, (0.0, 0.0));
    // Unrotated it spans x 120..200, y 67.5..112.5; the corner stays put.
    assert_eq!(alpha(&swung, 80, 45), 255);
    assert_eq!(alpha(&swung, 160, 90), 0);
}
