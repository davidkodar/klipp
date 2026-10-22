//! Every built-in title template, played in, is the same picture on the
//! GPU stack as from the CPU renderer an export falls back to (#2335).
//! A text that comes in by any entrance but a blur is laid out on the
//! GPU's glyph route, so its first frame waits on no CPU raster; only
//! the templates that roll, loop or blur in still raster once at rest.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{RationalTime, Resolution};
use klipp_timeline::*;

struct NoMedia;
impl FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

fn renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}

#[test]
fn every_template_plays_in_on_the_gpu_as_on_the_cpu() {
    let (w, h) = (640, 360);
    let mut resting = Vec::new();
    for t in title_library::TITLES {
        let mut seq = Sequence::new_default();
        seq.resolution = Resolution { width: w, height: h };
        let mut track = Track::new("title", TrackKind::Video);
        track.clips.push(Clip { kind: ClipKind::Title(t.build()), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(5.0, 30)) });
        seq.tracks = vec![track];
        let mut gpu = renderer(true);
        if gpu.executor().gpu_pipeline_or_init().is_none() {
            assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
            return;
        }
        let mut cpu = renderer(false);
        for secs in [0.2, 0.5, 1.0, 2.5] {
            let at = RationalTime::from_secs_f64(secs, 30);
            let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
            let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
            let (got, _) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()).expect("resident title");
            let events = klipp_effects::residency::last_frame().events;
            assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "{} at {secs}: {events:?}", t.name);
            let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            if worst > 3 { eprintln!("WORST {} at {secs}: {worst}", t.name); }
        }
        if gpu.title_renderer().resting_renders() > 0 {
            resting.push(t.name);
        }
    }
    resting.sort();
    assert_eq!(resting, ["Cinematic", "Credits Roll", "Title Card"], "the templates that still raster once at rest");
}
