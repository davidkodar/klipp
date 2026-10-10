//! Temporal noise reduction runs as its clip's source-boundary node (M4
//! step 6): straight after the Source, before the grade, against the
//! source frames either side, once a frame, and nowhere else.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, LinearFrameProviderFn, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::TestClip;

const W: u32 = 8;
const H: u32 = 4;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// A frame of flat `v`, opaque.
fn flat(v: f32) -> Vec<f32> {
    [v, v, v, 1.0].repeat((W * H) as usize)
}

/// The current frame a grey of 0.5, the one before it 0.48 and after it
/// 0.52, all within the threshold the clip uses, so the denoiser averages
/// them. Neighbour times it was asked for are kept.
struct Known {
    asked: Arc<std::sync::Mutex<Vec<f64>>>,
    /// The media's rate, when the source says it.
    fps: Option<f64>,
}

impl FrameSource for Known {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(flat(0.5))), width: w, height: h, pts: None })
    }
    fn temporal_frames_linear(&mut self, clip: &Clip, _: WorkingColorspace) -> Option<LinearFrameProviderFn> {
        let asked = Arc::clone(&self.asked);
        let now = clip.source_in.to_secs_f64() + 0.5;
        Some(Box::new(move |t| {
            asked.lock().unwrap().push(t);
            let source = clip_source_in() + t;
            let v = if source < now { 0.48 } else { 0.52 };
            Some((flat(v), W, H))
        }))
    }
    fn media_range(&mut self, _: &Clip) -> Option<(f64, f64)> {
        Some((0.0, 10.0))
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        self.fps
    }
}

fn clip_source_in() -> f64 {
    1.0
}

fn sequence(denoise: Option<(f32, f32)>) -> Sequence {
    let mut c = Clip::media(MediaId::new(), tc(0.0), tc(2.0), tc(clip_source_in()), tc(clip_source_in() + 2.0));
    if let Some((strength, threshold)) = denoise {
        c = TestClip::of(c).effect(E::TemporalDenoise { strength, threshold }).build();
    }
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

/// The frame denoised against its neighbours read from the source either
/// side of it, once a frame, and the old stage gone with nothing else to
/// run it.
#[test]
fn the_source_node_denoises_against_its_neighbours_once_a_frame() {
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let seq = sequence(Some((1.0, 0.1)));
    let mut src = Known { asked: Arc::clone(&asked), fps: None };
    let out = r.render_inspected(&seq, tc(0.5), W, H, &mut src, RenderOptions::default(), Inspect::default()).0;
    let want = klipp_effects::temporal_denoise::denoise_frame(&flat(0.5), &[&flat(0.48), &flat(0.52)], 0.1, 1.0);
    let apart = out.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(apart < 1e-5, "{apart} from the expected denoise");
    assert_eq!(r.source_node_runs(), 1, "once for the frame");
    // The two neighbours, a frame either side of the frame's source time,
    // asked in the readers' time (from the source in-point).
    let mut times = asked.lock().unwrap().clone();
    times.sort_by(f64::total_cmp);
    assert_eq!(times.len(), 2, "{times:?}");
    assert!((times[0] - (0.5 - 1.0 / 24.0)).abs() < 1e-9 && (times[1] - (0.5 + 1.0 / 24.0)).abs() < 1e-9, "{times:?}");
    let _ = r.render_inspected(&seq, tc(0.5 + 1.0 / 24.0), W, H, &mut src, RenderOptions::default(), Inspect::default());
    assert_eq!(r.source_node_runs(), 2, "once more for the next");
}

/// Media at half the sequence's rate: the neighbours are its captures
/// either side, one of its frames away, not one of the sequence's.
#[test]
fn the_neighbours_are_the_medias_frames_either_side() {
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let seq = sequence(Some((1.0, 0.1)));
    let mut src = Known { asked: Arc::clone(&asked), fps: Some(12.0) };
    let _ = r.render_inspected(&seq, tc(0.5), W, H, &mut src, RenderOptions::default(), Inspect::default());
    let mut times = asked.lock().unwrap().clone();
    times.sort_by(f64::total_cmp);
    assert_eq!(times.len(), 2, "{times:?}");
    assert!((times[0] - (0.5 - 1.0 / 12.0)).abs() < 1e-9 && (times[1] - (0.5 + 1.0 / 12.0)).abs() < 1e-9, "{times:?}");
}

/// Strength 0, or no denoiser at all: no source node runs, and the frame
/// is the source.
#[test]
fn without_a_denoiser_no_source_node_runs() {
    for seq in [sequence(Some((0.0, 0.1))), sequence(None)] {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(false);
        let mut src = Known { asked: Default::default(), fps: None };
        let out = r.render_inspected(&seq, tc(0.5), W, H, &mut src, RenderOptions::default(), Inspect::default()).0;
        assert_eq!(r.source_node_runs(), 0);
        assert!(out.chunks_exact(4).all(|p| (p[0] - 0.5).abs() < 1e-5), "the source unchanged");
    }
}
