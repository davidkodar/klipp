//! A lone text with a loop that only moves or fades it (a crawl, a
//! scroll, a drift, a shake, a ticker) is its resting render posed on
//! the GPU, as the CPU places it (#2335): the same picture, rastered
//! once, where folding the loop into its position rastered a new title
//! on every frame.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::title_motion::TitleLoop;
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
fn a_travelling_loop_poses_one_resting_render() {
    let (w, h) = (640, 360);
    let travel = ["Crawl", "Scroll", "Drift", "Shake", "Ticker", "Sway"];
    let loops: Vec<_> = TitleLoop::presets().into_iter().filter(|(n, _)| travel.contains(n)).collect();
    assert!(loops.len() >= 5, "the travelling loops are there: {:?}", loops.iter().map(|(n, _)| *n).collect::<Vec<_>>());
    for (name, l) in loops {
        let mut title = TitleData::single_text(TextBox { text: "On the move".into(), font_size: 120.0, color: [235, 210, 120, 255], ..Default::default() });
        title.objects[0].fx_mut().loops = vec![l];
        let mut seq = Sequence::new_default();
        seq.resolution = Resolution { width: w, height: h };
        let mut track = Track::new("title", TrackKind::Video);
        track.clips.push(Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(6.0, 30)) });
        seq.tracks = vec![track];
        let mut gpu = renderer(true);
        if gpu.executor().gpu_pipeline_or_init().is_none() {
            assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
            return;
        }
        let mut cpu = renderer(false);
        for t in [0.5, 1.3, 2.2, 3.1, 4.6] {
            let at = RationalTime::from_secs_f64(t, 30);
            let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
            let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
            let (got, _) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()).expect("resident title");
            let events = klipp_effects::residency::last_frame().events;
            assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "{name} at {t}: {events:?}");
            let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(worst <= 3, "{name} at {t}: the GPU differs from the CPU by {worst} levels");
        }
        assert_eq!(gpu.title_renderer().resting_renders(), 1, "{name}: rastered once, then posed");
    }
}
