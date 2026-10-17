//! A title of several layers keeps a text layer that doesn't change
//! (#2095): a line drawing itself on beside a dot, its trim keyed, is
//! rendered afresh each frame, and its dot was rasterised with it every
//! time, 50 ms at 4K. Kept, the dot is drawn once; its whole-layer fade is
//! its alpha scaled. Each frame is the picture drawn without the kept
//! layer, to the byte while the line draws on and within a level while
//! the dot fades.

use klipp_timeline::{TitleData, TitleMotion};

const W: u32 = 640;
const H: u32 = 360;
/// The clip's length, so the exit counts back from its end.
const WINDOW: f32 = 1.2;

/// A "." and a line drawn on beside it over 0.65 s, the dot fading out
/// in the clip's last 0.18 s: one of the titles of #2095, as saved.
fn dot_and_line() -> TitleData {
    serde_json::from_str(r#"{"animation_type": "Static", "background": {"Color": {"color": [0, 0, 0, 0], "padding": 0.3}}, "next_object_id": 2, "objects": [{"content": {"TextBox": {"alignment": "Center", "ass_style_name": null, "baseline_shift": 0.0, "case_transform": "None", "char_animation": null, "chromatic_aberration_px": 0.0, "color": [9, 7, 19, 255], "columns": 1, "drop_cap_scale": 0.0, "extrusion": null, "font_name": "Inter", "font_size": 4.0, "font_style": "Regular", "font_weight": 600, "glitch_intensity": 0.0, "gradient": null, "line_spacing": 1.0, "outline_write_on": 1.0, "pattern_fill": null, "pattern_fill_enabled": true, "strikethrough": false, "style_runs": [], "text": ".", "text_direction": "Ltr", "text_path": null, "texture_fill": null, "tracking": 0.0, "underline": false, "vertical_layout": false, "write_on": 1.0}}, "fx": {"motion": {"enter": null, "exit": {"amount": null, "anchor": null, "delay_secs": 0.0, "distance": 0.6, "duration_secs": 0.18, "easing": "InCubic", "kind": "Fade", "stagger": "None", "stagger_secs": 0.05}}, "outlines": [{"color": [0, 0, 0, 255], "enabled": true, "width": 0.0, "width_keyframes": []}]}, "id": 0, "locked": false, "name": "Text 1", "opacity": 1.0, "opacity_keyframes": [], "transform": {"anchor_norm": {"x": 0.5, "y": 0.5}, "anchor_x_keyframes": [], "anchor_y_keyframes": [], "center": {"x": 0.5, "y": 0.5}, "center_x_keyframes": [], "center_y_keyframes": [], "rotation": 0.0, "rotation_keyframes": [], "size": {"x": 1.0, "y": 1.0}, "size_x_keyframes": [], "size_y_keyframes": []}, "visible": true}, {"content": {"Shape": {"fill": null, "kind": {"Path": {"keyframes": [], "pose": {"contours": [{"closed": false, "vertices": [{"incoming": [0.3, 0.5], "outgoing": [0.3, 0.5], "point": [0.3, 0.5]}, {"incoming": [0.68, 0.5], "outgoing": [0.68, 0.5], "point": [0.68, 0.5]}]}], "fill": [0.0, 0.0, 0.0, 0.0], "stroke": [0.6, 0.45, 1.0, 1.0], "stroke_glow": {"radius": 14.0, "strength": 0.7}, "stroke_width": 2.0}, "trim": {"keyframes": [{"easing": "Linear", "range": {"end": 0.0, "offset": 0.0, "start": 0.0}, "time_secs": 0.0}, {"easing": "Linear", "range": {"end": 1.0, "offset": 0.0, "start": 0.0}, "time_secs": 0.65}], "range": {"end": 0.0, "offset": 0.0, "start": 0.0}}}}, "stroke": null}}, "fx": {}, "id": 1, "locked": false, "name": "Shape 2", "opacity": 1.0, "opacity_keyframes": [], "transform": {"anchor_norm": {"x": 0.5, "y": 0.5}, "anchor_x_keyframes": [], "anchor_y_keyframes": [], "center": {"x": 0.5, "y": 0.5}, "center_x_keyframes": [], "center_y_keyframes": [], "rotation": 0.0, "rotation_keyframes": [], "size": {"x": 1.0, "y": 1.0}, "size_x_keyframes": [], "size_y_keyframes": []}, "visible": true}]}"#).unwrap()
}

fn frame(r: &mut klipp_title::TitleRenderer, td: &TitleData, f: u32) -> Vec<u8> {
    r.render_with_window(td, W, H, None, f as f32 / 30.0, Some(WINDOW))
}

fn worst(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

/// One renderer through the title's frames draws the dot once, and every
/// frame is the one a fresh renderer, keeping nothing, draws: the same
/// bytes while the line draws on, within a level while the dot fades.
/// The control: a fresh renderer draws the dot again for every frame.
#[test]
fn a_dot_beside_a_line_drawing_on_is_drawn_once() {
    let td = dot_and_line();
    let mut warm = klipp_title::TitleRenderer::new();
    let (mut drawn_fresh, mut faded) = (0, 0);
    for f in 0..36 {
        let kept = frame(&mut warm, &td, f);
        let mut cold = klipp_title::TitleRenderer::new();
        let fresh = frame(&mut cold, &td, f);
        drawn_fresh += cold.text_layers_rendered();
        if f < 30 {
            assert_eq!(kept, fresh, "frame {f}: the kept dot draws another picture");
        } else {
            let d = worst(&kept, &fresh);
            assert!(d <= 1, "frame {f}: the faded kept dot is {d} levels from the dot drawn fading");
            faded += 1;
        }
    }
    assert!(faded > 0);
    assert_eq!(warm.text_layers_rendered(), 1, "the dot was drawn {} times", warm.text_layers_rendered());
    assert!(drawn_fresh >= 20, "the control: fresh renderers drew the dot {drawn_fresh} times");
}

/// The kept layer is the layer's own: a dot of another colour, or
/// another title's dot, is drawn for itself and not taken for the one
/// kept. The control: the two pictures differ.
#[test]
fn a_kept_layer_is_never_another_layers() {
    let red = dot_and_line();
    let mut blue = red.clone();
    let mut v = serde_json::to_value(&blue).unwrap();
    v["objects"][0]["content"]["TextBox"]["color"] = serde_json::json!([20, 40, 250, 255]);
    v["objects"][0]["content"]["TextBox"]["font_size"] = serde_json::json!(60.0);
    blue = serde_json::from_value(v).unwrap();
    let mut warm = klipp_title::TitleRenderer::new();
    for f in [5, 6, 31, 32] {
        for td in [&red, &blue] {
            let kept = frame(&mut warm, td, f);
            let fresh = frame(&mut klipp_title::TitleRenderer::new(), td, f);
            assert!(worst(&kept, &fresh) <= 1, "frame {f}: a kept layer stood in for another");
        }
    }
    let (r, b) = (frame(&mut warm, &red, 5), frame(&mut warm, &blue, 5));
    assert!(worst(&r, &b) > 10, "the control: the two dots draw alike");
}

/// A lower third of two text layers with shadows, each fading in (the
/// second a beat later) and out, as the stress project's (#2069).
fn lower_third() -> TitleData {
    use klipp_timeline::{LayerFx, Motion, MotionKind, TextBox, TitleMotion, TitleObjectId, TitleShadow};
    let layer = |text: &str, y: f32, delay: f32, id: u32| {
        let fx = LayerFx {
            motion: TitleMotion::both(Motion::new(MotionKind::Fade).duration(0.4).delay(delay), Motion::new(MotionKind::Fade).duration(0.4)),
            shadows: vec![TitleShadow { enabled: true, color: [0, 0, 0, 200], offset_x: 3.0, offset_x_keyframes: vec![], offset_y: 3.0, offset_y_keyframes: vec![], blur: 6.0, blur_keyframes: vec![] }],
            ..Default::default()
        };
        let mut td = TitleData::single_text_fx(TextBox { text: text.into(), font_size: 48.0, ..Default::default() }, fx);
        let mut o = td.objects.remove(0);
        o.id = TitleObjectId(id);
        o.transform.center.y = y;
        o
    };
    let mut td = TitleData::single_text_fx(TextBox::default(), LayerFx::default());
    td.objects = vec![layer("Stress Test", 0.75, 0.0, 0), layer("Titles section", 0.85, 0.1, 1)];
    td
}

/// Two layers fading, played into rather than parked on: each layer's
/// resting render is drawn once and every frame of the fades is those
/// renders with their fades as their opacities, the layers faded whole,
/// shadow and all, as a one-layer title's placed render fades. Rastered
/// afresh, each frame of the fades cost 75–105 ms at 4K (#2069).
#[test]
fn layers_fading_are_drawn_once_and_faded_whole() {
    const LONG: f32 = 4.0;
    let td = lower_third();
    let mut r = klipp_title::TitleRenderer::new();
    for f in (0..18).chain(105..120) {
        let t = f as f32 / 30.0;
        let got = r.render_with_window(&td, W, H, None, t, Some(LONG));
        // The reference: the resting layers, each at its fade's alpha as
        // its opacity, nothing moving.
        let mut want_td = td.clone();
        for o in &mut want_td.objects {
            let fx = o.fx().clone();
            let (pe, px) = fx.motion.progress(t, Some(LONG), 0);
            o.opacity = fx.motion.enter.map_or(1.0, |m| m.pose(pe, 0.0).0) * fx.motion.exit.map_or(1.0, |m| m.pose(px, 0.0).0);
            o.fx_mut().motion = TitleMotion::default();
        }
        let want = klipp_title::TitleRenderer::new().render_with_window(&want_td, W, H, None, t, Some(LONG));
        // As it shows: premultiplied. A pixel all but transparent keeps
        // whatever colour it is stored with.
        let shown = |px: &[u8]| px.chunks_exact(4).flat_map(|p| [p[0] as u32 * p[3] as u32 / 255, p[1] as u32 * p[3] as u32 / 255, p[2] as u32 * p[3] as u32 / 255, p[3] as u32]).collect::<Vec<u32>>();
        let d = shown(&got).iter().zip(shown(&want)).map(|(a, b)| a.abs_diff(b)).max().unwrap_or(0);
        assert!(d <= 1, "frame {f}: {d} levels from the resting layers at their fades");
    }
    assert_eq!(r.text_layers_rendered(), 2, "each layer drawn once at rest, not each frame: {}", r.text_layers_rendered());
}
