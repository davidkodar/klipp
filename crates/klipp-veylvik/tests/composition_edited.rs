//! A composition edited while its clip is shown (in the Veylvik tab, every
//! change a new revision) shows as it is now from the next frame, in the
//! viewer too: never the clip's media while it is made again. Every edit
//! made its clip sources again, and the viewer showed the media until
//! they were.

use std::sync::Arc;
use std::time::{Duration, Instant};

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Time};

const W: u32 = 64;
const H: u32 = 36;

struct Ramp;
impl FrameSource for Ramp {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new((0..w * h).flat_map(|i| [(i % w) as f32 / w as f32, 0.5, 0.2, 1.0]).collect())), width: w, height: h, pts: None })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(24.0)
    }
    fn shows_live(&mut self) -> bool {
        true
    }
}

fn bytes(width: u32) -> Arc<[u8]> {
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [width, H], Time { ticks: 24, rate: 1 }, 24).unwrap();
    serde_json::to_vec(&c).unwrap().into()
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn an_edited_composition_shows_from_the_next_frame_never_the_media() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let _ = klipp_veylvik::install();
    let id = CompositionId(uuid::Uuid::from_u128(1));
    let at = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut clip = Clip::media(MediaId(uuid::Uuid::from_u128(9)), at(0.0), at(1.0), at(0.0), at(1.0));
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip.clone()];
    seq.tracks = vec![track.clone()];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let media = r.render(&seq, at(0.5), W, H, &mut Ramp, RenderOptions::default());
    clip.composition = Some(id);
    track.clips = vec![clip];
    seq.tracks = vec![track];

    // Half the width, ready.
    klipp_veylvik::set_project_compositions([(id, bytes(W / 2))].into());
    let deadline = Instant::now() + Duration::from_secs(120);
    let half = loop {
        let p = r.render(&seq, at(0.5), W, H, &mut Ramp, RenderOptions::default());
        if worst(&p, &media) > 0.05 {
            break p;
        }
        assert!(Instant::now() < deadline, "composed within 120 s");
        std::thread::sleep(Duration::from_millis(20));
    };
    // Edited, ten times, each shown on the frame after it.
    for edit in 0..10u32 {
        let width = if edit % 2 == 0 { W / 4 } else { W / 2 };
        let revision = klipp_veylvik::revision();
        klipp_veylvik::set_project_compositions([(id, bytes(width))].into());
        let shown = r.render(&seq, at(0.5), W, H, &mut Ramp, RenderOptions::default());
        assert!(worst(&shown, &media) > 0.05, "edit {edit}: the media was shown while it was made again");
        if width == W / 2 {
            assert!(worst(&shown, &half) < 1e-6, "edit {edit}: as it is now");
        } else {
            assert!(worst(&shown, &half) > 0.05, "edit {edit}: the edit, not the one before it");
        }
        assert_eq!(klipp_veylvik::revision(), revision, "edit {edit}: not prepared again");
    }
    klipp_veylvik::set_project_compositions(Compositions::new());
}
