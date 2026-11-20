//! Installed once, the hook shows a clip through its composition in every
//! render, with no wrapper at the call site: through `render` (which runs
//! `render_inspected` inside it, so a source wrapped twice would show) and
//! the GPU display path alike. A project without compositions renders as
//! it always did, and Veylvik's renderer is not started for it.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Time};

const W: u32 = 64;
const H: u32 = 36;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

struct Chart;
impl FrameSource for Chart {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let pixels = (0..w * h).flat_map(|i| [(i % w) as f32 / w as f32 * 1.5, (i / w) as f32 / h as f32, 0.3, 1.0]).collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: None })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(24.0)
    }
}

fn sequence(composition: Option<CompositionId>) -> Sequence {
    let mut clip = Clip::media(MediaId(uuid::Uuid::from_u128(9)), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    clip.composition = composition;
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

fn render(seq: &Sequence, gpu: bool) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render(seq, tc(1.0), W, H, &mut Chart, RenderOptions::default())
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn every_render_shows_a_clip_through_its_composition() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    assert!(klipp_veylvik::install());
    assert!(!klipp_veylvik::install(), "once");
    let plain: Vec<_> = [true, false].map(|gpu| render(&sequence(None), gpu)).into();

    // No compositions: nothing is wrapped and Veylvik isn't started.
    let id = CompositionId(uuid::Uuid::from_u128(1));
    for (gpu, plain) in [true, false].iter().zip(&plain) {
        assert_eq!(&render(&sequence(Some(id)), *gpu), plain);
    }

    // A composition that halves the picture's width, so it shows.
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W / 2, H], Time { ticks: 24, rate: 1 }, 48).unwrap();
    let held: Compositions = [(id, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into();
    klipp_veylvik::set_project_compositions(held);
    for (gpu, plain) in [true, false].iter().zip(&plain) {
        let shown = render(&sequence(Some(id)), *gpu);
        assert!(worst(&shown, plain) > 0.05, "gpu={gpu}: the composition shows");
        // A clip without one in the same project is as it was.
        assert_eq!(&render(&sequence(None), *gpu), plain, "gpu={gpu}");
    }

    // `render` runs `render_inspected` inside it: wrapped at both, the
    // halved picture would be halved again and the two would differ.
    let compositor = klipp_veylvik::Compositor::shared().expect("started by the renders above");
    for gpu in [true, false] {
        let before = compositor.renders();
        let nested = render(&sequence(Some(id)), gpu);
        assert_eq!(compositor.renders() - before, 1, "gpu={gpu}: one picture asked for one frame");
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let single = r.render_inspected(&sequence(Some(id)), tc(1.0), W, H, &mut Chart, RenderOptions::default(), Default::default()).0;
        assert_eq!(nested, single, "gpu={gpu}: wrapped once");
    }

    // The project closed: back to the media.
    klipp_veylvik::set_project_compositions(Compositions::new());
    assert_eq!(render(&sequence(Some(id)), true), plain[0]);
}
