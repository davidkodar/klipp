//! A title of several layers whose line draws itself on beside a dot that
//! later fades (#2095) draws on the viewer's GPU stack what the CPU
//! render draws, frame by frame through one renderer that keeps the dot,
//! within a level: while the line draws on, and while the dot fades.

use klipp_core::{RationalTime, Resolution};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 640;
const H: u32 = 360;

struct NoMedia;
impl klipp_compose::FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<klipp_compose::SourceFrame> {
        None
    }
}

/// The dot and the line of #2095 over a mid grey, so the dark dot shows,
/// 1.2 s long.
fn sequence() -> Sequence {
    let td: klipp_timeline::TitleData = serde_json::from_str(r#"{"animation_type": "Static", "background": {"Color": {"color": [0, 0, 0, 0], "padding": 0.3}}, "next_object_id": 2, "objects": [{"content": {"TextBox": {"alignment": "Center", "ass_style_name": null, "baseline_shift": 0.0, "case_transform": "None", "char_animation": null, "chromatic_aberration_px": 0.0, "color": [9, 7, 19, 255], "columns": 1, "drop_cap_scale": 0.0, "extrusion": null, "font_name": "Inter", "font_size": 4.0, "font_style": "Regular", "font_weight": 600, "glitch_intensity": 0.0, "gradient": null, "line_spacing": 1.0, "outline_write_on": 1.0, "pattern_fill": null, "pattern_fill_enabled": true, "strikethrough": false, "style_runs": [], "text": ".", "text_direction": "Ltr", "text_path": null, "texture_fill": null, "tracking": 0.0, "underline": false, "vertical_layout": false, "write_on": 1.0}}, "fx": {"motion": {"enter": null, "exit": {"amount": null, "anchor": null, "delay_secs": 0.0, "distance": 0.6, "duration_secs": 0.18, "easing": "InCubic", "kind": "Fade", "stagger": "None", "stagger_secs": 0.05}}, "outlines": [{"color": [0, 0, 0, 255], "enabled": true, "width": 0.0, "width_keyframes": []}]}, "id": 0, "locked": false, "name": "Text 1", "opacity": 1.0, "opacity_keyframes": [], "transform": {"anchor_norm": {"x": 0.5, "y": 0.5}, "anchor_x_keyframes": [], "anchor_y_keyframes": [], "center": {"x": 0.5, "y": 0.5}, "center_x_keyframes": [], "center_y_keyframes": [], "rotation": 0.0, "rotation_keyframes": [], "size": {"x": 1.0, "y": 1.0}, "size_x_keyframes": [], "size_y_keyframes": []}, "visible": true}, {"content": {"Shape": {"fill": null, "kind": {"Path": {"keyframes": [], "pose": {"contours": [{"closed": false, "vertices": [{"incoming": [0.3, 0.5], "outgoing": [0.3, 0.5], "point": [0.3, 0.5]}, {"incoming": [0.68, 0.5], "outgoing": [0.68, 0.5], "point": [0.68, 0.5]}]}], "fill": [0.0, 0.0, 0.0, 0.0], "stroke": [0.6, 0.45, 1.0, 1.0], "stroke_glow": {"radius": 14.0, "strength": 0.7}, "stroke_width": 2.0}, "trim": {"keyframes": [{"easing": "Linear", "range": {"end": 0.0, "offset": 0.0, "start": 0.0}, "time_secs": 0.0}, {"easing": "Linear", "range": {"end": 1.0, "offset": 0.0, "start": 0.0}, "time_secs": 0.65}], "range": {"end": 0.0, "offset": 0.0, "start": 0.0}}}}, "stroke": null}}, "fx": {}, "id": 1, "locked": false, "name": "Shape 2", "opacity": 1.0, "opacity_keyframes": [], "transform": {"anchor_norm": {"x": 0.5, "y": 0.5}, "anchor_x_keyframes": [], "anchor_y_keyframes": [], "center": {"x": 0.5, "y": 0.5}, "center_x_keyframes": [], "center_y_keyframes": [], "rotation": 0.0, "rotation_keyframes": [], "size": {"x": 1.0, "y": 1.0}, "size_x_keyframes": [], "size_y_keyframes": []}, "visible": true}]}"#).unwrap();
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: W, height: H };
    seq.fps = klipp_core::Fps::new(30, 1);
    let tc = |f: i64| RationalTime::from_frames(f, 30);
    let mut text = Track::new("Text", TrackKind::Text);
    text.clips.push(Clip { kind: ClipKind::Title(td), source_in: tc(0), source_out: tc(36), ..Clip::gap(tc(0), tc(36)) });
    let mut bg = Track::new("Background", TrackKind::Video);
    bg.clips.push(Clip { kind: ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [120, 120, 120, 255] }), source_in: tc(0), source_out: tc(36), ..Clip::gap(tc(0), tc(36)) });
    seq.tracks = vec![text, bg];
    seq
}

#[test]
fn a_kept_dot_and_a_drawing_line_draw_alike_on_the_gpu() {
    let seq = sequence();
    let mut gpu = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let mut cpu = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let mut frames = Vec::new();
    for f in 0..36 {
        let at = RationalTime::from_frames(f, 30);
        let (linear, _) = cpu.render_inspected(&seq, at, W, H, &mut NoMedia, Default::default(), Default::default());
        let expected = klipp_color::display::display_transform_ws_dithered_to(&linear, klipp_core::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
        let Some((actual, _)) = gpu.render_display_u8_on_gpu(&seq, at, W, H, &mut NoMedia, Default::default()) else {
            assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
            eprintln!("UNVERIFIED: no GPU stack");
            return;
        };
        let worst = actual.iter().zip(&expected).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(worst <= 1, "frame {f}: the GPU stack is {worst} levels from the CPU render");
        frames.push(actual);
    }
    // The control: the line has moved between frames 4 and 12, and the
    // dot faded between 30 and 35, by more than the bound.
    for (a, b) in [(4, 12), (30, 35)] {
        let d = frames[a].iter().zip(&frames[b]).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        assert!(d > 1, "frames {a} and {b} draw alike: the bound tells nothing apart");
    }
}
