//! A text whose glows, shadows or outlines are keyed, coming in with an
//! entrance, is drawn on the GPU's glyph route (#2335): its glyphs posed
//! by the motion and its styles drawn from them at that time, as the CPU
//! raster draws them, with no title rastered per frame.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::title_motion::{Motion, MotionKind, StaggerUnit};
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

#[test]
fn keyed_styles_coming_in_are_drawn_on_the_gpu() {
    let (w, h) = (640, 360);
    let motions = [
        ("rise", Motion::new(MotionKind::Rise).duration(1.0)),
        ("fade", Motion::new(MotionKind::Fade).duration(1.0)),
        ("scale up", Motion::new(MotionKind::ScaleUp).duration(1.0)),
        ("pop by letter", Motion::new(MotionKind::Pop).duration(0.6).stagger(StaggerUnit::Char, 0.05)),
    ];
    for (name, motion) in motions {
        let fx = LayerFx {
            motion: TitleMotion::enter(motion),
            glows: vec![TextGlow { enabled: true, color: [80, 150, 255, 220], radius: 15.0, radius_keyframes: vec![key(0.0, 8.0), key(2.0, 24.0)], intensity: 0.9, intensity_keyframes: vec![] }],
            shadows: vec![TitleShadow { enabled: true, color: [0, 0, 0, 180], offset_x: 4.0, offset_y: 6.0, blur: 5.0, offset_x_keyframes: vec![key(0.0, 0.0), key(2.0, 8.0)], offset_y_keyframes: vec![], blur_keyframes: vec![] }],
            outlines: vec![TextOutline { enabled: true, color: [240, 245, 255, 230], width: 3.5, width_keyframes: vec![] }],
            ..Default::default()
        };
        let title = TitleData::single_text_fx(TextBox { text: "Arrive".into(), font_size: 160.0, color: [235, 210, 120, 255], ..Default::default() }, fx);
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
        for t in [0.1, 0.4, 0.7, 1.5] {
            let at = RationalTime::from_secs_f64(t, 30);
            let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
            let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
            let (got, _) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()).expect("resident title");
            let events = klipp_effects::residency::last_frame().events;
            assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "{name} at {t}: {events:?}");
            let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            if worst > 3 {
                let i = got.iter().zip(&want).position(|(a, b)| a.abs_diff(*b) == worst).unwrap() / 4;
                let bad = got.chunks_exact(4).zip(want.chunks_exact(4)).filter(|(a, b)| a.iter().zip(*b).any(|(x, y)| x.abs_diff(*y) > 3)).count();
                eprintln!("{name} at {t}: worst {worst} at {},{} got {:?} want {:?}, {bad} pixels over 3", i % w as usize, i / w as usize, &got[i * 4..i * 4 + 4], &want[i * 4..i * 4 + 4]);
            }
            assert!(worst <= 3, "{name} at {t}: the GPU differs from the CPU by {worst} levels");
            assert_eq!(gpu.title_renderer().resting_renders(), 0, "{name}: no title rastered per frame");
        }
    }
}
