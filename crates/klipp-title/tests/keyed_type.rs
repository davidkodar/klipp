//! A text's size, tracking and line spacing keyed over its clip (#1511):
//! each moment draws as the text set still at that moment's values, the
//! same whether the frame is asked for as a frame (the viewer) or
//! rendered whole (an export).

use klipp_core::{Interpolation, Keyframe};
use klipp_timeline::{TextAlignment, TextBox, TitleData};
use klipp_title::{TitleFrame, TitleRenderer};

const W: u32 = 1920;
const H: u32 = 1080;

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

fn title(tb: TextBox) -> TitleData {
    let mut td = TitleData::single_text(TextBox { alignment: TextAlignment::Left, ..tb });
    td.objects[0].transform.center = klipp_timeline::TitlePosition { x: 0.05, y: 0.5 };
    td.objects[0].transform.anchor_norm = klipp_timeline::TitlePosition { x: 0.0, y: 0.5 };
    td
}

/// The frame the viewer is handed at `t`, as pixels: a keyed text is
/// rendered or its cached glyphs posed for its time.
fn viewer(r: &mut TitleRenderer, td: &TitleData, t: f32) -> Vec<u8> {
    match r.frame_with_window(td, W, H, None, t, Some(4.0)) {
        TitleFrame::Rendered { pixels, .. } => pixels.to_bytes(),
        TitleFrame::Glyphs { sprite, poses, rows } => klipp_title::pose_sprite_glyphs(&sprite.pixels, W, H, sprite.units.as_ref().unwrap(), &poses, rows)
            .into_iter().map(|v| v.round().clamp(0.0, 255.0) as u8).collect(),
        _ => panic!("at t={t} a keyed text must be evaluated"),
    }
}

/// The rightmost and bottom-most inked columns and rows.
fn ink_extent(px: &[u8]) -> (usize, usize) {
    let mut out = (0, 0);
    for (i, p) in px.chunks_exact(4).enumerate() {
        if p[3] > 128 {
            out = (out.0.max(i % W as usize), out.1.max(i / W as usize));
        }
    }
    out
}

/// Tracking keyed from 0 to 200 opens the letters over time: each moment
/// is the text set still at that moment's tracking, the viewer's frame
/// and the export's alike.
#[test]
fn tracking_keyed_from_0_to_200_opens_the_letters_over_time() {
    let base = TextBox { text: "KEY".into(), font_size: 60.0, ..Default::default() };
    let keyed = title(TextBox { tracking_keyframes: vec![key(0.0, 0.0), key(2.0, 200.0)], ..base.clone() });
    let mut r = TitleRenderer::new();
    let mut rights = Vec::new();
    for (t, tracking) in [(0.0f32, 0.0f32), (0.5, 50.0), (1.0, 100.0), (1.5, 150.0), (2.0, 200.0), (3.0, 200.0)] {
        let export = r.render_with_window(&keyed, W, H, None, t, Some(4.0));
        assert_eq!(viewer(&mut r, &keyed, t), export, "at t={t} the viewer's frame is the export's");
        let still = r.render_with_window(&title(TextBox { tracking, ..base.clone() }), W, H, None, t, Some(4.0));
        assert_eq!(export, still, "at t={t} the text is set at tracking {tracking}");
        rights.push(ink_extent(&export).0);
    }
    assert!(rights.windows(2).take(4).all(|p| p[1] > p[0]), "the letters open: {rights:?}");
    assert!((rights[4] - rights[0]) as f32 >= 2.0 * 200.0 - 8.0, "the last of three letters moves twice the tracking: {rights:?}");
}

/// Size and line spacing keyed the same way: each moment is the text set
/// still at that moment's values.
#[test]
fn size_and_line_spacing_keyed_set_the_text_at_each_moment() {
    let base = TextBox { text: "Two\nlines".into(), font_size: 40.0, ..Default::default() };
    let keyed = title(TextBox {
        font_size_keyframes: vec![key(0.0, 40.0), key(2.0, 80.0)],
        line_spacing_keyframes: vec![key(0.0, 1.0), key(2.0, 2.0)],
        ..base.clone()
    });
    let mut r = TitleRenderer::new();
    let mut bottoms = Vec::new();
    for (t, size, spacing) in [(0.0f32, 40.0f32, 1.0f32), (1.0, 60.0, 1.5), (2.0, 80.0, 2.0)] {
        let export = r.render_with_window(&keyed, W, H, None, t, Some(4.0));
        assert_eq!(viewer(&mut r, &keyed, t), export, "at t={t}");
        let still = r.render_with_window(&title(TextBox { font_size: size, line_spacing: spacing, ..base.clone() }), W, H, None, t, Some(4.0));
        assert_eq!(export, still, "at t={t} the text is set at {size} and {spacing}");
        bottoms.push(ink_extent(&export));
    }
    assert!(bottoms.windows(2).all(|p| p[1].0 > p[0].0), "the text grows: {bottoms:?}");
}

/// A text with no keys still hands the viewer its resting render placed,
/// so the keyed path costs a title nothing until it is keyed.
#[test]
fn a_text_without_keys_keeps_its_resting_render() {
    let td = title(TextBox { text: "Still".into(), ..Default::default() });
    let mut r = TitleRenderer::new();
    assert!(matches!(r.frame_with_window(&td, W, H, None, 1.0, Some(4.0)), TitleFrame::Placed { .. }));
}

/// The keys come back through the title's own save, and a text without
/// them writes none.
#[test]
fn the_keys_save_with_the_text_and_none_are_written_without_them() {
    let keyed = TextBox { tracking_keyframes: vec![key(0.0, 0.0), key(2.0, 200.0)], font_size_keyframes: vec![key(1.0, 50.0)], ..Default::default() };
    let back: TextBox = serde_json::from_str(&serde_json::to_string(&keyed).unwrap()).unwrap();
    assert_eq!(back, keyed);
    let plain = serde_json::to_string(&TextBox::default()).unwrap();
    assert!(!plain.contains("_keyframes\":[]") && !plain.contains("tracking_keyframes"), "{plain}");
}

/// A colour keyed from red to blue passes through the blend of their
/// light, not of their sRGB numbers: halfway it is the colour whose light
/// is half of each, (188, 0, 188), and the text is drawn in it, the
/// viewer's frame and the export's alike.
#[test]
fn a_colour_keyed_from_red_to_blue_blends_their_light() {
    let base = TextBox { text: "COLOUR".into(), font_size: 90.0, color: [255, 0, 0, 255], ..Default::default() };
    let mut colour_keys = klipp_timeline::ColourKeys::default();
    colour_keys.set(0.0, [255, 0, 0, 255]);
    colour_keys.set(2.0, [0, 0, 255, 255]);
    let keyed = title(TextBox { color_keyframes: colour_keys, ..base.clone() });
    let mut r = TitleRenderer::new();
    for (t, colour) in [(0.0f32, [255u8, 0, 0, 255]), (1.0, [188, 0, 188, 255]), (2.0, [0, 0, 255, 255])] {
        let export = r.render_with_window(&keyed, W, H, None, t, Some(4.0));
        assert_eq!(viewer(&mut r, &keyed, t), export, "at t={t}");
        let still = r.render_with_window(&title(TextBox { color: colour, ..base.clone() }), W, H, None, t, Some(4.0));
        assert_eq!(export, still, "at t={t} the text is drawn in {colour:?}");
        let solid = export.chunks_exact(4).find(|p| p[3] == 255).expect("a solid pixel");
        assert_eq!(&solid[..3], &colour[..3], "at t={t}");
    }
}

/// A gradient's stop colour keyed draws, at each moment, as the still
/// gradient with that moment's colour, in the viewer's frame and the
/// export's alike; halfway from red to blue the stop is their light
/// blended (#1511).
#[test]
fn a_gradient_stop_keyed_draws_its_colour_at_each_moment() {
    use klipp_timeline::{ColourKeys, GradientStop, TextGradient};
    let stops = |top: [u8; 4]| [GradientStop { position: 0.0, color: top }, GradientStop { position: 0.6, color: [255, 255, 0, 255] }, GradientStop { position: 1.0, color: [0, 0, 0, 255] }];
    let base = TextBox { text: "GRADE".into(), font_size: 120.0, ..Default::default() };
    let mut keys = ColourKeys::default();
    keys.set(0.0, [255, 0, 0, 255]);
    keys.set(2.0, [0, 0, 255, 255]);
    let mut keyed_gradient = TextGradient::with_stops(&stops([255, 0, 0, 255]), Default::default(), true);
    keyed_gradient.stop_keyframes = vec![keys];
    let keyed = title(TextBox { gradient: Some(keyed_gradient), ..base.clone() });
    let mut r = TitleRenderer::new();
    for (t, top) in [(0.0f32, [255u8, 0, 0, 255]), (1.0, [188, 0, 188, 255]), (2.0, [0, 0, 255, 255])] {
        let export = r.render_with_window(&keyed, W, H, None, t, Some(4.0));
        assert_eq!(viewer(&mut r, &keyed, t), export, "at t={t}");
        let still = title(TextBox { gradient: Some(TextGradient::with_stops(&stops(top), Default::default(), true)), ..base.clone() });
        assert_eq!(export, r.render_with_window(&still, W, H, None, t, Some(4.0)), "at t={t} the first stop is {top:?}");
    }
    assert_ne!(r.render_with_window(&keyed, W, H, None, 0.0, Some(4.0)), r.render_with_window(&keyed, W, H, None, 2.0, Some(4.0)));
}
