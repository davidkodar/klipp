//! A clip shown through a composition that only reads its footage renders
//! in Klipp exactly as the clip without one: the picture Veylvik gives back
//! goes through the clip's grade and effects and the composite as the
//! media's own frame would. On the GPU stack and the CPU path, in each
//! working space. Controls: a composition that changes the picture is seen
//! in the frame, and one that can't be read shows the media.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Compositor, Time};

const W: u32 = 96;
const H: u32 = 54;
const FPS: u32 = 24;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, FPS)
}

/// A chart with light above 1, half-transparent columns and dark
/// channels: Klipp's decoded frame.
struct Chart;
impl FrameSource for Chart {
    fn frame(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let a = if x % 3 == 1 { 0.5 } else { 1.0 };
                pixels.extend_from_slice(&[2.5 * x as f32 / w as f32, y as f32 / h as f32, 0.05 + secs as f32 * 0.1, a]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: Some(secs) })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(f64::from(FPS))
    }
}

fn sequence(composition: Option<CompositionId>) -> Sequence {
    let mut clip = Clip::media(MediaId(uuid_of(7)), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    clip.composition = composition;
    clip.gain = 1.0;
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

fn uuid_of(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(n)
}

fn render(seq: &Sequence, src: &mut dyn FrameSource, gpu: bool, working: WorkingColorspace) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.set_working_colorspace(working);
    r.render_inspected(seq, tc(1.0), W, H, src, RenderOptions::default(), Inspect::default()).0
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn composition_bytes(size: [u32; 2]) -> Arc<[u8]> {
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, size, Time { ticks: i64::from(FPS), rate: 1 }, 48).expect("a composition");
    serde_json::to_vec(&c).expect("written").into()
}

#[test]
fn a_pass_through_composition_renders_as_the_clip_without_one() {
    let Some(compositor) = Compositor::shared() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let (pass, changes, broken) = (CompositionId(uuid_of(1)), CompositionId(uuid_of(2)), CompositionId(uuid_of(3)));
    let held: Compositions = [(pass, composition_bytes([W, H])), (changes, composition_bytes([W / 2, H])), (broken, Arc::from(&b"{ not a composition"[..]))].into();
    compositor.set_compositions(held);

    for working in [WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG, WorkingColorspace::Bt2020Linear] {
        for gpu in [true, false] {
            let plain = render(&sequence(None), &mut Chart, gpu, working);
            let mut chart = Chart;
            let through = render(&sequence(Some(pass)), &mut compositor.source(&mut chart, working), gpu, working);
            let off = worst(&plain, &through);
            eprintln!("{working:?} gpu={gpu}: pass-through differs by {off}");
            // Through Rec.709 and back in f32: a few ulps of light up to 2.5.
            assert!(off <= 1e-5, "{working:?} gpu={gpu}: {off}");

            let mut chart = Chart;
            let changed = render(&sequence(Some(changes)), &mut compositor.source(&mut chart, working), gpu, working);
            assert!(worst(&plain, &changed) > 0.05, "{working:?} gpu={gpu}: the control, a composition of another size, must show");

            let mut chart = Chart;
            let unread = render(&sequence(Some(broken)), &mut compositor.source(&mut chart, working), gpu, working);
            assert_eq!(worst(&plain, &unread), 0.0, "{working:?} gpu={gpu}: one that can't be read shows the media");
        }
    }
}
