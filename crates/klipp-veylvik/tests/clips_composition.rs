//! A composition on a precomposition is given each of its scene's clips:
//! that clip's footage where it is in the scene, at the media time its
//! trim gives, and clear where it isn't. In Klipp's renders on the GPU
//! stack and on the CPU's alike.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions};

const W: u32 = 64;
const H: u32 = 36;

const TOP: MediaId = MediaId(uuid::Uuid::from_u128(7));
const LOW: MediaId = MediaId(uuid::Uuid::from_u128(8));

/// The top media red; the low one green, its blue its media time in
/// tenths of a second: a picture says which frame of it was read.
struct Timed;
impl FrameSource for Timed {
    fn frame(&mut self, c: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let px = if c.media_id() == Some(TOP) { [0.6, 0.1, 0.1, 1.0] } else { [0.1, 0.5, secs as f32 / 10.0, 1.0] };
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new((0..w * h).flat_map(|_| px).collect())), width: w, height: h, pts: None })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(24.0)
    }
}

#[test]
fn a_composition_on_a_precomposition_is_given_its_clips_at_their_media_time() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let _ = klipp_veylvik::install();
    let at = |s: f64| RationalTime::from_secs_f64(s, 24);
    // The scene: the low clip on V1 from 0.5 s, 2 s into its media; the
    // top clip over the whole scene on V2, hiding it in the scene's own
    // picture.
    let mut scene = Sequence::new_default();
    scene.resolution = klipp_core::Resolution { width: W, height: H };
    let low = Clip::media(LOW, at(0.5), at(1.0), at(2.0), at(2.5));
    let top = Clip::media(TOP, at(0.0), at(1.0), at(0.0), at(1.0));
    let (mut v2, mut v1) = (Track::new("V2", TrackKind::Video), Track::new("V1", TrackKind::Video));
    v2.clips = vec![top];
    v1.clips = vec![low.clone()];
    scene.tracks = vec![v2, v1];
    // The composition shows the low clip alone: given it, it shows; not
    // given it, the scene (the top clip) would.
    let made = klipp_veylvik::clips::of_clips(&scene, |c| format!("{:?}", c.media_id())).unwrap();
    let c: Composition = Composition::from_host_footage(&klipp_veylvik::clips::clip_key(low.id), "Low", None, [W, H], klipp_veylvik::Time { ticks: 24, rate: 1 }, 24).unwrap();
    c.validate().unwrap();
    assert_eq!(klipp_veylvik::clips::clip_keys(&made).len(), 2, "both clips laid in by the composition Klipp makes");
    let id = CompositionId(uuid::Uuid::from_u128(1));
    let bytes: Arc<[u8]> = serde_json::to_vec(&c).unwrap().into();
    klipp_veylvik::set_project_compositions([(id, bytes)].into());

    let mut pre = Clip::precomposition(scene.id, at(0.0), at(1.0));
    pre.composition = Some(id);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![pre];
    seq.tracks = vec![v];
    // Resolved as the app's renders resolve it: the scene in the clip,
    // its clips renamed for this instance.
    let seq = klipp_timeline::flatten::flatten_sequence(&seq, &[seq.clone(), scene.clone()]);
    assert!(seq.tracks[0].clips[0].precomp.as_ref().is_some_and(|s| s.all_clips().all(|c| c.id != low.id)), "renamed in the instance");

    let middle = ((H / 2 * W + W / 2) * 4) as usize;
    let draw = |gpu: bool, t: f64| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let frame = if gpu {
            r.render_linear_on_gpu(&seq, at(t), W, H, &mut Timed, RenderOptions::default()).expect("the GPU stack takes the frame")
        } else {
            r.render(&seq, at(t), W, H, &mut Timed, RenderOptions::default())
        };
        frame[middle..middle + 4].to_vec()
    };
    for gpu in [true, false] {
        let path = if gpu { "GPU" } else { "CPU" };
        // 0.75 s into the scene is 0.25 s into the low clip: 2.25 s into
        // its media.
        let px = draw(gpu, 0.75);
        assert!((px[0] - 0.1).abs() < 2e-3 && (px[1] - 0.5).abs() < 2e-3, "the low clip, {path}: {px:?}");
        assert!((px[2] - 0.225).abs() < 6e-3, "at its media's 2.25 s, {path}: {px:?}");
        // Before it starts it isn't in play: clear, not the top clip.
        let px = draw(gpu, 0.25);
        assert!(px[0] < 2e-3 && px[1] < 2e-3, "nothing before the low clip starts, {path}: {px:?}");
    }
    klipp_veylvik::set_project_compositions(Compositions::new());
}
