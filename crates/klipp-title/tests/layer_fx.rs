//! A shape or a picture carries the same stack a text does, and moves
//! the same way: glow, shadow, outline and inner shadow drawn around
//! and inside its own coverage; an entrance, an exit and the loops
//! posing the whole layer. What a text does per glyph, a layer that is
//! not glyphs does as one.

use klipp_core::title_motion::{Motion, MotionKind, StaggerUnit, TitleLoop};
use klipp_timeline::*;
use klipp_title::TitleRenderer;

const W: u32 = 320;
const H: u32 = 180;

/// A blue rectangle a third of the frame wide and tall, in the middle.
fn card(fx: LayerFx) -> TitleData {
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
    let mut xf = default_text_transform();
    xf.size = TitlePosition { x: 1.0 / 3.0, y: 1.0 / 3.0 };
    let id = td.add_object(TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fill: Some([40, 80, 200, 255]), gradient: None, stroke: None, fit: None }), xf);
    td.object_mut(id).unwrap().fx = Some(fx);
    td
}

fn render(td: &TitleData, t: Option<f32>) -> Vec<u8> {
    let mut r = TitleRenderer::new();
    match t {
        Some(t) => r.render_with_window(td, W, H, None, t, Some(4.0)),
        None => r.render(td, W, H),
    }
}

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

/// Rows carrying any alpha, as (first, last exclusive).
fn rows(buf: &[u8]) -> (u32, u32) {
    let lit: Vec<u32> = (0..H).filter(|&y| (0..W).any(|x| px(buf, x, y)[3] > 0)).collect();
    (*lit.first().unwrap_or(&0), lit.last().map_or(0, |y| y + 1))
}

fn covered(buf: &[u8]) -> usize {
    buf.chunks_exact(4).filter(|p| p[3] > 0).count()
}

/// The card alone covers its box and nothing else; the pixel just
/// outside its edge is where the stacks show.
#[test]
fn a_bare_card_is_its_box() {
    let plain = render(&card(LayerFx::default()), None);
    assert_eq!(rows(&plain), (60, 120));
    assert_eq!(px(&plain, 160, 90), [40, 80, 200, 255]);
    assert_eq!(px(&plain, 100, 90)[3], 0, "nothing beside the card");
}

#[test]
fn a_glow_spills_outside_the_card() {
    let glow = LayerFx { glows: vec![TextGlow { enabled: true, color: [255, 200, 0, 255], radius: 24.0, radius_keyframes: vec![], intensity: 1.0, intensity_keyframes: vec![] }], ..Default::default() };
    let lit = render(&card(glow), None);
    let beside = px(&lit, 100, 90);
    assert!(beside[3] > 0 && beside[0] > beside[2], "the glow's colour lies beside the card: {beside:?}");
    assert_eq!(px(&lit, 160, 90), [40, 80, 200, 255], "the card's face is its own");
    assert!(covered(&lit) > covered(&render(&card(LayerFx::default()), None)));
}

#[test]
fn a_shadow_falls_where_it_is_offset_to() {
    let shadow = LayerFx { shadows: vec![TitleShadow { enabled: true, color: [0, 0, 0, 255], offset_x: 60.0, offset_x_keyframes: vec![], offset_y: 60.0, offset_y_keyframes: vec![], blur: 0.0, blur_keyframes: vec![] }], ..Default::default() };
    let lit = render(&card(shadow), None);
    // 60 px at 1080p is 10 px at this height: below-right of the card.
    assert_eq!(px(&lit, 215, 125), [0, 0, 0, 255], "the shadow, below and to the right");
    assert_eq!(px(&lit, 100, 60)[3], 0, "and not above-left");
}

#[test]
fn an_outline_rings_the_card_and_an_inner_shadow_darkens_its_edge() {
    let both = LayerFx {
        outlines: vec![TextOutline { enabled: true, color: [255, 255, 255, 255], width: 18.0, width_keyframes: vec![] }],
        inner_shadows: vec![InnerShadow { enabled: true, color: [0, 0, 0, 255], offset_x: 0.0, offset_x_keyframes: vec![], offset_y: 30.0, offset_y_keyframes: vec![], blur: 0.0, blur_keyframes: vec![] }],
        ..Default::default()
    };
    let lit = render(&card(both), None);
    assert_eq!(px(&lit, 105, 90), [255, 255, 255, 255], "the outline, just outside the edge");
    let top_inside = px(&lit, 160, 62);
    assert!(top_inside[0] < 20 && top_inside[3] == 255, "the inner shadow darkens the top edge inside: {top_inside:?}");
    assert_eq!(px(&lit, 160, 90), [40, 80, 200, 255], "the middle keeps its colour");
}

/// A rise: early in the entrance the card sits lower and fainter than
/// at rest, and at rest it is exactly the still card.
#[test]
fn a_rise_brings_the_card_up_into_place() {
    let rise = LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.5)), ..Default::default() };
    let td = card(rise);
    let early = render(&td, Some(0.1));
    let settled = render(&td, Some(2.0));
    let still = render(&card(LayerFx::default()), Some(2.0));
    assert!(rows(&early).0 > 60, "early in the rise the card is below its place: rows {:?}", rows(&early));
    assert!(px(&early, 160, 100)[3] < 200, "and fainter");
    assert_eq!(settled, still, "at rest it is the still card");
}

/// A fade exit near the end of the window; a pulse that never rests.
#[test]
fn an_exit_fades_and_a_loop_keeps_moving() {
    let fx = LayerFx {
        motion: TitleMotion { enter: None, exit: Some(Motion::new(MotionKind::Fade).duration(0.5)) },
        loops: vec![TitleLoop::Pulse { rate_hz: 1.0, min_opacity: 0.2 }],
        ..Default::default()
    };
    let td = card(fx);
    // A quarter period apart: a 1 Hz pulse is at different points of
    // its swing, whichever curve it follows.
    let mid = render(&td, Some(1.0));
    let mid_later = render(&td, Some(1.25));
    assert_ne!(px(&mid, 160, 90)[3], px(&mid_later, 160, 90)[3], "the pulse changes the alpha between two moments");
    let leaving = render(&td, Some(3.9));
    assert!(px(&leaving, 160, 90)[3] < 80, "near the end the exit has nearly faded it: {:?}", px(&leaving, 160, 90));
}

/// A stagger is per glyph; on a card it is nothing, and the card moves
/// as one exactly as with no stagger.
#[test]
fn a_stagger_on_a_card_is_played_whole() {
    let whole = LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.5)), ..Default::default() };
    let staggered = LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.5).stagger(StaggerUnit::Char, 0.3)), ..Default::default() };
    for t in [0.1f32, 0.3, 1.0] {
        assert_eq!(render(&card(staggered.clone()), Some(t)), render(&card(whole.clone()), Some(t)), "at {t}");
    }
}

/// A picture takes the same stack and the same pose.
#[test]
fn a_picture_fades_in_with_a_glow() {
    let path = std::env::temp_dir().join(format!("klipp_layer_fx_{}.png", std::process::id()));
    let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([200, 40, 40, 255]));
    img.save(&path).unwrap();
    let make = |fx: LayerFx| {
        let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
        let mut xf = default_text_transform();
        xf.size = TitlePosition { x: 0.2, y: 0.2 };
        let id = td.add_object(TitleObjectContent::Image(ImageLayer { path: path.to_string_lossy().into_owned(), fit: Default::default() }), xf);
        td.object_mut(id).unwrap().fx = Some(fx);
        td
    };
    let fx = LayerFx {
        motion: TitleMotion::enter(Motion::new(MotionKind::Fade).duration(1.0)),
        glows: vec![TextGlow { enabled: true, color: [0, 255, 0, 255], radius: 30.0, radius_keyframes: vec![], intensity: 1.0, intensity_keyframes: vec![] }],
        ..Default::default()
    };
    let plain = render(&make(LayerFx::default()), Some(2.0));
    let dressed = render(&make(fx.clone()), Some(2.0));
    let half = render(&make(fx), Some(0.5));
    assert_eq!(px(&plain, 160, 90)[..3], [200, 40, 40], "the picture is there");
    assert!(covered(&dressed) > covered(&plain), "the glow spills beyond the picture");
    assert!(px(&half, 160, 90)[3] < px(&dressed, 160, 90)[3], "half-way through the fade it is fainter");
    let _ = std::fs::remove_file(&path);
}
