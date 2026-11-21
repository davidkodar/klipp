//! A sync of the media pool that changes nothing under a composed clip
//! keeps its composition ready: no frame shown as media while it is made
//! again. One media's change remakes that media's footage alone. Every
//! sync made every clip's footage again, and with it prepared in the
//! background, every composed clip showed its media for a while after any
//! edit to the pool (a tag, a rating).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Time};

const W: u32 = 64;
const H: u32 = 36;

/// A grey picture, for a live view.
struct Grey;
impl FrameSource for Grey {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new((0..w * h).flat_map(|i| [0.4, (i % w) as f32 / w as f32, 0.2, 1.0]).collect())), width: w, height: h, pts: None })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(24.0)
    }
    fn shows_live(&mut self) -> bool {
        true
    }
}

fn media(n: u128) -> MediaId {
    MediaId(uuid::Uuid::from_u128(n))
}

/// Two composed clips, one on each media, side by side in time.
fn sequence(id: CompositionId) -> Sequence {
    let at = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut a = Clip::media(media(10), at(0.0), at(1.0), at(0.0), at(1.0));
    let mut b = Clip::media(media(11), at(1.0), at(2.0), at(0.0), at(1.0));
    a.composition = Some(id);
    b.composition = Some(id);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![a, b];
    seq.tracks = vec![track];
    seq
}

/// Whether the live view showed the clip at `secs` composed: a composed
/// picture is half the frame's width, so not the media's.
fn composed(r: &mut Renderer, seq: &Sequence, secs: f64) -> bool {
    let at = RationalTime::from_secs_f64(secs, 24);
    let mut plain = seq.clone();
    plain.tracks[0].clips.iter_mut().for_each(|c| c.composition = None);
    let media = r.render(&plain, at, W, H, &mut Grey, RenderOptions::default());
    let picture = r.render(seq, at, W, H, &mut Grey, RenderOptions::default());
    picture.iter().zip(&media).any(|(a, b)| (a - b).abs() > 0.05)
}

fn wait_ready(r: &mut Renderer, seq: &Sequence, secs: f64) {
    let deadline = Instant::now() + Duration::from_secs(180);
    while !composed(r, seq, secs) {
        assert!(Instant::now() < deadline, "not composed within 180 s");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_sync_that_changes_nothing_keeps_compositions_ready_and_one_change_is_that_medias() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let _ = klipp_veylvik::install();
    let id = CompositionId(uuid::Uuid::from_u128(1));
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W / 2, H], Time { ticks: 24, rate: 1 }, 24).unwrap();
    let states = |a: u64, b: u64| HashMap::from([(media(10), a), (media(11), b)]);
    klipp_veylvik::set_media_states(states(1, 1));
    klipp_veylvik::set_project_compositions([(id, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let seq = sequence(id);
    wait_ready(&mut r, &seq, 0.5);
    wait_ready(&mut r, &seq, 1.5);

    // The same states again, as a sync after a tag or a rating says them.
    for _ in 0..5 {
        klipp_veylvik::set_media_states(states(1, 1));
        assert!(composed(&mut r, &seq, 0.5) && composed(&mut r, &seq, 1.5), "kept ready through a sync that changed nothing");
    }

    // The first media's file changed: its clip made again, the other kept.
    klipp_veylvik::set_media_states(states(2, 1));
    assert!(!composed(&mut r, &seq, 0.5), "the changed media's clip is made again");
    assert!(composed(&mut r, &seq, 1.5), "the other media's clip kept ready");
    wait_ready(&mut r, &seq, 0.5);
    klipp_veylvik::set_project_compositions(Compositions::new());
}
