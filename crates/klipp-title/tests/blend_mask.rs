//! Title layers that blend and mask one another (#1513): each blend
//! mode mixes a layer with what is below it by its formula, in linear
//! light; a layer masked by another shows only where the mask lets it,
//! and the mask itself is not drawn; and a title whose layers do neither
//! is laid layer over layer exactly as before.

use klipp_core::BlendMode;
use klipp_timeline::*;
use klipp_title::{TitleFrame, TitleRenderer};

const W: u32 = 320;
const H: u32 = 180;

fn empty() -> TitleData {
    TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() }
}

/// A rectangle filled with `fill`, its centre and size as fractions of
/// the frame.
fn rect(td: &mut TitleData, fill: [u8; 4], centre: (f32, f32), size: (f32, f32)) -> TitleObjectId {
    let mut xf = default_text_transform();
    xf.center = TitlePosition { x: centre.0, y: centre.1 };
    xf.size = TitlePosition { x: size.0, y: size.1 };
    td.add_object(TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fill: Some(fill), gradient: None, stroke: None, fit: None }), xf)
}

fn full(td: &mut TitleData, fill: [u8; 4]) -> TitleObjectId {
    rect(td, fill, (0.5, 0.5), (1.0, 1.0))
}

fn word(td: &mut TitleData) -> TitleObjectId {
    let mut xf = default_text_transform();
    xf.size = TitlePosition { x: 1.0, y: 1.0 };
    td.add_object(TitleObjectContent::TextBox(TextBox { text: "HMWH".into(), font_size: 300.0, color: [240, 230, 60, 255], ..Default::default() }), xf)
}

fn render(td: &TitleData) -> Vec<u8> {
    TitleRenderer::new().render(td, W, H)
}

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

fn decode(v: u8) -> f64 {
    let c = v as f64 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn encode(c: f64) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (v * 255.0).round() as u8
}

/// The separable modes as the compositing literature writes them, `b`
/// the backdrop and `s` the source, here apart from the renderer's own.
fn formula(mode: BlendMode, b: f64, s: f64) -> f64 {
    let hard = |b: f64, s: f64| if s <= 0.5 { 2.0 * b * s } else { 1.0 - 2.0 * (1.0 - b) * (1.0 - s) };
    let v = match mode {
        BlendMode::Normal => s,
        BlendMode::Add => b + s,
        BlendMode::Multiply => b * s,
        BlendMode::Screen => b + s - b * s,
        BlendMode::Overlay => hard(s, b),
        BlendMode::HardLight => hard(b, s),
        BlendMode::SoftLight => {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }
        BlendMode::Difference => (b - s).abs(),
        BlendMode::Exclusion => b + s - 2.0 * b * s,
        BlendMode::ColorDodge => if s >= 1.0 { 1.0 } else { (b / (1.0 - s)).min(1.0) },
        BlendMode::ColorBurn => if s <= 0.0 { 0.0 } else { (1.0 - (1.0 - b) / s).max(0.0) },
        BlendMode::Darken => b.min(s),
        BlendMode::Lighten => b.max(s),
    };
    v.clamp(0.0, 1.0)
}

/// Every mode, over an opaque backdrop, opaque and at half alpha: the
/// pixel is the formula of the two colours in linear light, laid over
/// the backdrop by the layer's alpha.
#[test]
fn each_blend_mode_mixes_by_its_formula() {
    let backdrop = [180u8, 90, 30, 255];
    for alpha in [255u8, 128] {
        let source = [60u8, 200, 140, alpha];
        for mode in BlendMode::ALL {
            let mut td = empty();
            full(&mut td, backdrop);
            let top = full(&mut td, source);
            td.object_mut(top).unwrap().blend = mode;
            let got = px(&render(&td), W / 2, H / 2);
            let sa = alpha as f64 / 255.0;
            for c in 0..3 {
                let (b, s) = (decode(backdrop[c]), decode(source[c]));
                let want = encode(formula(mode, b, s) * sa + b * (1.0 - sa));
                let off = (got[c] as i32 - want as i32).abs();
                if mode == BlendMode::Normal {
                    // Normal is the plain "over" titles always used, in
                    // the encoded values, unchanged.
                    let over = (source[c] as f64 * sa + backdrop[c] as f64 * (1.0 - sa)).round() as i32;
                    assert!((got[c] as i32 - over).abs() <= 1, "Normal at alpha {alpha}, channel {c}: {got:?}");
                } else {
                    assert!(off <= 1, "{mode:?} at alpha {alpha}, channel {c}: got {} want {want}", got[c]);
                }
            }
            assert_eq!(got[3], 255, "{mode:?}: over an opaque backdrop the result is opaque");
        }
    }
}

/// A layer that blends over nothing is itself: the mode mixes only
/// where there is something below.
#[test]
fn a_blend_over_nothing_is_the_layer() {
    let mut normal = empty();
    full(&mut normal, [10, 10, 10, 255]);
    let top = rect(&mut normal, [60, 200, 140, 255], (0.3, 0.5), (0.2, 0.4));
    normal.object_mut(top).unwrap().opacity = 0.8;
    normal.object_mut(normal.objects[0].id).unwrap().visible = false;
    for mode in BlendMode::ALL {
        let mut td = normal.clone();
        td.object_mut(top).unwrap().blend = mode;
        let off = render(&td).iter().zip(render(&normal).iter()).filter(|(a, b)| a != b).count();
        assert_eq!(off, 0, "{mode:?} over nothing: {off} bytes differ");
    }
}

/// A text layer masked by a shape: with an alpha mask the word shows
/// inside the shape exactly as it is drawn and nowhere outside it;
/// inverted, the other way round. The shape is a matte and is not drawn.
#[test]
fn a_text_masked_by_a_shape_shows_only_inside_it() {
    let mut td = empty();
    let shape = rect(&mut td, [255, 255, 255, 255], (0.25, 0.5), (0.5, 1.0));
    let text = word(&mut td);
    let mut plain = td.clone();
    plain.object_mut(shape).unwrap().visible = false;
    let plain = render(&plain);
    assert!(plain.chunks_exact(4).filter(|p| p[3] > 0).count() > 2000, "the word covers the frame");
    for (kind, inside) in [(LayerMaskKind::Alpha, true), (LayerMaskKind::InvertedAlpha, false)] {
        td.object_mut(text).unwrap().mask = Some(LayerMask { source: shape, kind });
        let masked = render(&td);
        let (mut shown, mut hidden) = (0, 0);
        for y in 0..H {
            for x in (0..W).filter(|x| x.abs_diff(W / 2) > 1) {
                let in_shape = x < W / 2;
                if in_shape == inside {
                    assert_eq!(px(&masked, x, y), px(&plain, x, y), "{kind:?}: the word as drawn at {x},{y}");
                    shown += usize::from(px(&plain, x, y)[3] > 0);
                } else {
                    assert_eq!(px(&masked, x, y)[3], 0, "{kind:?}: nothing at {x},{y}");
                    hidden += usize::from(px(&plain, x, y)[3] > 0);
                }
            }
        }
        assert!(shown > 500 && hidden > 500, "{kind:?}: the word on both sides of the edge ({shown} shown, {hidden} hidden)");
    }
}

/// The issue's case, a shape revealed only inside a word: the shape's
/// colour, the word's coverage, and the word itself not drawn.
#[test]
fn a_shape_masked_by_a_word_is_the_word_in_its_colour() {
    let mut td = empty();
    let text = word(&mut td);
    let fill = full(&mut td, [30, 120, 250, 255]);
    let mut word_alone = td.clone();
    word_alone.object_mut(fill).unwrap().visible = false;
    let alone = render(&word_alone);
    td.object_mut(fill).unwrap().mask = Some(LayerMask { source: text, kind: LayerMaskKind::Alpha });
    let masked = render(&td);
    for (m, w) in masked.chunks_exact(4).zip(alone.chunks_exact(4)) {
        assert_eq!(m[3], w[3], "the word's coverage");
        if m[3] > 0 {
            assert_eq!(&m[..3], &[30, 120, 250], "in the shape's colour");
        }
    }
}

/// A luma mask shows the layer by how bright the matte is, in linear
/// light: white all of it, black none, a mid grey its luminance.
#[test]
fn a_luma_mask_shows_the_layer_by_the_mattes_brightness() {
    for grey in [0u8, 128, 200, 255] {
        for (kind, inverted) in [(LayerMaskKind::Luma, false), (LayerMaskKind::InvertedLuma, true)] {
            let mut td = empty();
            let matte = full(&mut td, [grey, grey, grey, 255]);
            let layer = full(&mut td, [200, 40, 40, 255]);
            td.object_mut(layer).unwrap().mask = Some(LayerMask { source: matte, kind });
            let got = px(&render(&td), W / 2, H / 2);
            let luma = decode(grey);
            let want = (255.0 * if inverted { 1.0 - luma } else { luma }).round() as u8;
            assert_eq!(got[3], want, "{kind:?} by grey {grey}");
            if want > 0 {
                assert_eq!(&got[..3], &[200, 40, 40], "the layer's own colour");
            }
        }
    }
}

/// Normal and no mask: the layers laid one over the next in the encoded
/// values, each exactly as it draws alone, the stack rounded once at the
/// end and not after every layer (#190).
#[test]
fn normal_layers_are_laid_over_each_other_and_rounded_once() {
    let mut td = empty();
    let a = rect(&mut td, [30, 60, 200, 255], (0.5, 0.5), (0.6, 0.3));
    let b = word(&mut td);
    let c = rect(&mut td, [200, 30, 30, 255], (0.35, 0.4), (0.3, 0.5));
    td.object_mut(b).unwrap().opacity = 0.6;
    td.object_mut(c).unwrap().opacity = 0.55;
    let alone = |keep: TitleObjectId| {
        let mut t = td.clone();
        for o in &mut t.objects {
            o.visible = o.id == keep;
        }
        render(&t)
    };
    let mut stack: Vec<f32> = alone(a).iter().map(|&v| v as f32).collect();
    for layer in [alone(b), alone(c)] {
        for (d, s) in stack.chunks_exact_mut(4).zip(layer.chunks_exact(4)) {
            let sa = s[3] as f32 / 255.0;
            if sa <= 0.0 {
                continue;
            }
            let da = d[3] / 255.0;
            let oa = sa + da * (1.0 - sa);
            for i in 0..3 {
                d[i] = ((s[i] as f32 * sa + d[i] * da * (1.0 - sa)) / oa).clamp(0.0, 255.0);
            }
            d[3] = (oa * 255.0).clamp(0.0, 255.0);
        }
    }
    let want: Vec<u8> = stack.iter().map(|v| v.round().clamp(0.0, 255.0) as u8).collect();
    let got = render(&td);
    // The renderer hands its stack on in half floats (#190), which can
    // move a value that sits on a rounding tie by a level; nothing more.
    let far = got.iter().zip(&want).filter(|(g, w)| g.abs_diff(**w) > 1).count();
    let ties = got.iter().zip(&want).filter(|(g, w)| g != w).count();
    assert_eq!(far, 0, "{far} bytes differ by more than a level from the layers laid over each other");
    assert!(ties * 1000 <= got.len(), "{ties} of {} bytes on a tie: more than rounding", got.len());
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

/// Shapes over a full-frame card with a translucent layer between: the
/// frame is the one the renderer made before blends and masks existed,
/// to the byte (hash taken on the build before #1513).
#[test]
fn a_title_without_blends_or_masks_draws_as_before() {
    let mut td = empty();
    td.background = TitleBackground::FullFrame { color: [12, 14, 20, 255] };
    rect(&mut td, [30, 60, 200, 255], (0.5, 0.5), (0.6, 0.3));
    let mid = rect(&mut td, [200, 30, 30, 140], (0.35, 0.4), (0.3, 0.5));
    rect(&mut td, [250, 250, 250, 90], (0.7, 0.6), (0.25, 0.25));
    td.object_mut(mid).unwrap().opacity = 0.7;
    let mut r = TitleRenderer::new();
    assert_eq!(fnv(&r.render(&td, W, H)), 0xa36cfd9dc4dac105, "rendered still");
    assert_eq!(fnv(&r.render_with_window(&td, W, H, None, 1.0, Some(4.0))), 0x1c90a09f7c946505, "placed in its clip, the card's fill laid under");
}

/// A title whose layers mix is placed only at rest and unmoved: the
/// pose of a sprite would move the matte and the mixed picture with the
/// layer. While its layer rises, it is rastered at its time.
#[test]
fn a_title_that_mixes_is_rastered_while_a_layer_moves() {
    use klipp_core::title_motion::{Motion, MotionKind};
    let mut td = empty();
    let shape = rect(&mut td, [255, 255, 255, 255], (0.25, 0.5), (0.5, 1.0));
    let text = word(&mut td);
    td.object_mut(text).unwrap().mask = Some(LayerMask { source: shape, kind: LayerMaskKind::Alpha });
    td.object_mut(text).unwrap().fx_mut().motion = TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.5));
    let mut r = TitleRenderer::new();
    assert!(matches!(r.frame_with_window(&td, W, H, None, 0.1, Some(4.0)), TitleFrame::Rendered { .. }), "rising, rastered");
    match r.frame_with_window(&td, W, H, None, 2.0, Some(4.0)) {
        TitleFrame::Placed { dx, dy, opacity, scale, under, .. } => {
            assert_eq!((dx, dy, opacity, scale, under), (0, 0, 1.0, 1.0, None), "at rest, the render as it is");
        }
        _ => panic!("at rest the resting render is placed"),
    }
    // A card's fill is mixed into the render, not laid under it.
    td.background = TitleBackground::FullFrame { color: [0, 0, 0, 255] };
    td.object_mut(shape).unwrap().blend = BlendMode::Multiply;
    match r.frame_with_window(&td, W, H, None, 2.0, Some(4.0)) {
        TitleFrame::Placed { under, .. } => assert_eq!(under, None),
        _ => panic!("placed at rest"),
    }
}
