//! The viewer's frames of a clip and the export's are not the same pixels
//! (the viewer's streams are 8-bit, the export decodes in float), so a
//! picture Veylvik made of one is never handed out for the other: an
//! export after the viewer showed the same frame is made of the export's
//! own footage. They shared the clip source and its names, and an export
//! could be made of the viewer's 8-bit footage.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Time};

const W: u32 = 64;
const H: u32 = 36;

/// The same frame of the same media, as a live view or an export gives
/// it: a slightly different picture each way.
struct Frames {
    live: bool,
}
impl FrameSource for Frames {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let red = if self.live { 0.50 } else { 0.52 };
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new((0..w * h).flat_map(|_| [red, 0.3, 0.2, 1.0]).collect())), width: w, height: h, pts: None })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(24.0)
    }
    fn shows_live(&mut self) -> bool {
        self.live
    }
}

#[test]
fn an_export_is_made_of_its_own_footage_after_the_viewer_showed_the_frame() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let _ = klipp_veylvik::install();
    let id = CompositionId(uuid::Uuid::from_u128(1));
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W, H], Time { ticks: 24, rate: 1 }, 24).unwrap();
    klipp_veylvik::set_project_compositions([(id, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
    let at = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut clip = Clip::media(MediaId(uuid::Uuid::from_u128(9)), at(0.0), at(1.0), at(0.0), at(1.0));
    clip.composition = Some(id);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let middle = ((H / 2 * W + W / 2) * 4) as usize;
    // The export first, so the composition is ready; then the viewer, then
    // the export again, at the same frame.
    let first = r.render(&seq, at(0.5), W, H, &mut Frames { live: false }, RenderOptions::default());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let viewer = loop {
        let v = r.render(&seq, at(0.5), W, H, &mut Frames { live: true }, RenderOptions::default());
        if !klipp_veylvik::preparing() && (v[middle] - 0.50).abs() < 1e-4 {
            break v;
        }
        assert!(std::time::Instant::now() < deadline, "the viewer's own frame within 120 s: {}", v[middle]);
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let again = r.render(&seq, at(0.5), W, H, &mut Frames { live: false }, RenderOptions::default());
    assert!((first[middle] - 0.52).abs() < 1e-4, "the export's footage: {}", first[middle]);
    assert!((viewer[middle] - 0.50).abs() < 1e-4, "the viewer's footage: {}", viewer[middle]);
    assert!((again[middle] - 0.52).abs() < 1e-4, "the export's own footage again, not the viewer's: {}", again[middle]);
    klipp_veylvik::set_project_compositions(Compositions::new());
}
