//! A text whose position is keyed, plain or styled, is laid out afresh
//! where it is at each frame on the GPU's glyph route, as the CPU raster
//! lays it out (#2335): the same picture, and no title rastered per
//! frame while it travels.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{Interpolation, Keyframe, RationalTime, Resolution};
use klipp_timeline::*;

struct NoMedia;
impl FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

fn renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}

fn styles(keyed: bool) -> LayerFx {
    LayerFx {
        glows: vec![TextGlow { enabled: true, color: [80, 150, 255, 220], radius: 15.0, radius_keyframes: if keyed { vec![key(0.0, 8.0), key(2.0, 24.0)] } else { vec![] }, intensity: 0.9, intensity_keyframes: vec![] }],
        shadows: vec![TitleShadow { enabled: true, color: [0, 0, 0, 180], offset_x: 4.0, offset_y: 6.0, blur: 5.0, offset_x_keyframes: vec![], offset_y_keyframes: vec![], blur_keyframes: vec![] }],
        outlines: vec![TextOutline { enabled: true, color: [240, 245, 255, 230], width: 3.5, width_keyframes: vec![] }],
        ..Default::default()
    }
}

#[test]
fn a_travelling_text_is_laid_out_where_it_is_on_the_gpu() {
    let (w, h) = (640, 360);
    let text = || TextBox { text: "Travel".into(), font_size: 160.0, color: [235, 210, 120, 255], ..Default::default() };
    for (name, fx) in [("plain", LayerFx::default()), ("still styles", styles(false)), ("keyed styles", styles(true))] {
        let mut title = TitleData::single_text_fx(text(), fx);
        let t = &mut title.objects[0].transform;
        t.center_x_keyframes = vec![key(0.0, 0.3), key(3.0, 0.7)];
        t.center_y_keyframes = vec![key(0.0, 0.45), key(3.0, 0.55)];
        let mut seq = Sequence::new_default();
        seq.resolution = Resolution { width: w, height: h };
        let mut track = Track::new("title", TrackKind::Video);
        track.clips.push(Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(4.0, 30)) });
        seq.tracks = vec![track];
        let mut gpu = renderer(true);
        if gpu.executor().gpu_pipeline_or_init().is_none() {
            assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
            return;
        }
        let mut cpu = renderer(false);
        let mut seen = None;
        for t in [0.2, 1.1, 1.7, 2.9] {
            let at = RationalTime::from_secs_f64(t, 30);
            let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
            let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
            let (got, _) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()).expect("resident title");
            let events = klipp_effects::residency::last_frame().events;
            assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "{name} at {t}: {events:?}");
            let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(worst <= 3, "{name} at {t}: the GPU differs from the CPU by {worst} levels");
            // It does travel: each frame is another.
            if let Some(last) = seen.replace(got.clone()) {
                assert!(got != last, "{name} at {t}: moved");
            }
            assert_eq!(gpu.title_renderer().resting_renders(), 0, "{name}: no title rastered per frame");
        }
    }
}
