//! A composition is made ready on a thread of its own: the viewer shows
//! the clip's media meanwhile, never waiting on it, and draws it composed
//! once it is ready; a render that isn't live (an export) waits for it
//! and has the composed picture from its first frame. The first frame used
//! to wait many seconds on the render thread, the viewer stopped with it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Time};

/// The project's compositions are the process's: one test at a time.
static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

const W: u32 = 64;
const H: u32 = 36;
const FPS: f64 = 24000.0 / 1001.0;

/// A picture that says which frame it is, for a live view or not.
struct Numbered {
    live: bool,
}
impl FrameSource for Numbered {
    fn frame(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let index = (secs * FPS + 1e-6).floor() as f32;
        let pixels = (0..w * h).flat_map(|i| [index / 64.0, (i % w) as f32 / w as f32, 0.3, 1.0]).collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: None })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(FPS)
    }
    fn shows_live(&mut self) -> bool {
        self.live
    }
}

fn sequence(composition: Option<CompositionId>) -> Sequence {
    let at = |s: f64| RationalTime::from_secs_f64(s, 24000);
    let mut clip = Clip::media(MediaId(uuid::Uuid::from_u128(9)), at(0.0), at(4.0), at(0.0), at(4.0));
    clip.composition = composition;
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn frame(r: &mut Renderer, seq: &Sequence, live: bool) -> Vec<f32> {
    let at = RationalTime::from_secs_f64(5.0 * 1001.0 / 24000.0, 24000);
    r.render(seq, at, W, H, &mut Numbered { live }, RenderOptions::default())
}

#[test]
fn the_viewer_shows_the_media_until_its_composition_is_ready_and_an_export_waits() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = klipp_veylvik::install();
    let id = CompositionId(uuid::Uuid::from_u128(1));
    // Half the picture's width, so a composed frame differs from its media.
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W / 2, H], Time { ticks: 24000, rate: 1001 }, 96).unwrap();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let plain = frame(&mut r, &sequence(None), true);
    klipp_veylvik::set_project_compositions([(id, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
    let composed = sequence(Some(id));

    // Live: the media at once, said to be preparing.
    let before = klipp_veylvik::revision();
    let asked = Instant::now();
    let first = frame(&mut r, &composed, true);
    let took = asked.elapsed();
    assert!(worst(&first, &plain) < 1e-6, "the media while it is prepared");
    eprintln!("live first frame: {:.1} ms", took.as_secs_f64() * 1000.0);
    assert!(took < Duration::from_secs(2), "the viewer waited {took:?} on a composition being prepared");

    // Ready: the revision moves on and the next live frame is composed.
    let deadline = Instant::now() + Duration::from_secs(180);
    while klipp_veylvik::revision() == before {
        assert!(Instant::now() < deadline, "not prepared within 180 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!("prepared after {:.2} s", asked.elapsed().as_secs_f64());
    assert!(!klipp_veylvik::preparing(), "none still preparing");
    let live = frame(&mut r, &composed, true);
    assert!(worst(&live, &plain) > 0.05, "composed once ready");

    // Not live: made again (the media changed) and waited for, composed
    // from its first frame, the same picture.
    klipp_veylvik::media_changed();
    let asked = Instant::now();
    let exported = frame(&mut r, &composed, false);
    eprintln!("waited first frame: {:.2} s", asked.elapsed().as_secs_f64());
    assert!(worst(&exported, &plain) > 0.05, "an export's first frame composed");
    assert!(worst(&exported, &live) < 1e-6, "the same picture either way");
    klipp_veylvik::set_project_compositions(Compositions::new());
}

/// A composition that can't be made shows the media and stops being
/// prepared, live or not: no render waits on it again.
#[test]
fn a_composition_that_cannot_be_made_shows_the_media() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = klipp_veylvik::install();
    let id = CompositionId(uuid::Uuid::from_u128(2));
    // Footage under a key the clip's host doesn't hold.
    let c = Composition::from_host_footage("not-the-clip", "Clip", None, [W / 2, H], Time { ticks: 24000, rate: 1001 }, 96).unwrap();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let plain = frame(&mut r, &sequence(None), false);
    klipp_veylvik::set_project_compositions([(id, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
    let asked = Instant::now();
    let shown = frame(&mut r, &sequence(Some(id)), false);
    assert!(worst(&shown, &plain) < 1e-6, "the media");
    assert!(asked.elapsed() < Duration::from_secs(120), "gave up only at the deadline");
    let again = Instant::now();
    let live = frame(&mut r, &sequence(Some(id)), true);
    assert!(worst(&live, &plain) < 1e-6, "the media, live");
    assert!(again.elapsed() < Duration::from_secs(2), "not prepared again");
    assert!(!klipp_veylvik::preparing());
    klipp_veylvik::set_project_compositions(Compositions::new());
}
