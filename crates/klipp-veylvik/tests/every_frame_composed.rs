//! Every frame of a clip at an NTSC rate shows through its composition,
//! the first after Veylvik's clip sources were made again included. Its
//! time reached Veylvik rounded to microseconds, and every third frame at
//! 23.976 named the frame before: Veylvik used the footage of the frame
//! before when it still held it, and when it didn't (a dropped frame in
//! playback, the media pool synced) the composition failed and the clip
//! blinked to its plain media.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Time};

const W: u32 = 64;
const H: u32 = 36;
const FPS: f64 = 24000.0 / 1001.0;

/// A picture that says which frame it is.
struct Numbered;
impl FrameSource for Numbered {
    fn frame(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let index = (secs * FPS + 1e-6).floor() as f32;
        let pixels = (0..w * h).flat_map(|i| [index / 64.0, (i % w) as f32 / w as f32, 0.3, 1.0]).collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: None })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(FPS)
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

fn frame(r: &mut Renderer, seq: &Sequence, index: i64) -> Vec<f32> {
    let at = RationalTime::from_secs_f64(index as f64 * 1001.0 / 24000.0, 24000);
    r.render(seq, at, W, H, &mut Numbered, RenderOptions::default())
}

#[test]
fn every_frame_shows_through_its_composition() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    assert!(klipp_veylvik::install());
    let id = CompositionId(uuid::Uuid::from_u128(1));
    // Half the picture's width, so a composed frame differs from its media.
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W / 2, H], Time { ticks: 24000, rate: 1001 }, 96).unwrap();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let plain: Vec<Vec<f32>> = (0..24).map(|i| frame(&mut r, &sequence(None), i)).collect();
    klipp_veylvik::set_project_compositions([(id, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
    let composed = sequence(Some(id));
    let in_a_row: Vec<Vec<f32>> = (0..24).map(|i| frame(&mut r, &composed, i)).collect();
    for (i, (shown, plain)) in in_a_row.iter().zip(&plain).enumerate() {
        assert!(worst(shown, plain) > 0.05, "frame {i}: composed");
    }
    // Each frame the first its clip source makes: what playback meets
    // after a dropped frame or a media pool sync.
    for (i, shown) in in_a_row.iter().enumerate() {
        klipp_veylvik::media_changed();
        let first = frame(&mut r, &composed, i as i64);
        assert!(worst(&first, &plain[i]) > 0.05, "frame {i}, the first after the media changed: composed");
        assert!(worst(&first, shown) < 1e-6, "frame {i}: its own footage either way");
    }
    klipp_veylvik::set_project_compositions(Compositions::new());
}
