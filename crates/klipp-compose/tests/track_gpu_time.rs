//! A track's GPU time (#3207): its own binary, since timestamps measure
//! the GPU's clock and another test's work on the shared device between
//! two marks would count in them.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode, WorkingColorspace};
use klipp_effects::BuiltinEffect;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn media(from: f64, to: f64) -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(from), tc(to)) }
}

fn track(name: &str, kind: TrackKind, clips: Vec<Clip>) -> Track {
    let mut t = Track::new(name, kind);
    t.clips = clips;
    t
}

fn sequence(tracks: Vec<Track>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.tracks = tracks;
    seq
}

/// #3207: on a device that times its work, a timed frame gives each track
/// on the stack its GPU time, read back a frame or more later without
/// waiting; a track under a wide blur takes longer than a plain one.
#[test]
fn a_timed_frame_gives_each_track_its_gpu_time() {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r.set_working_colorspace(WorkingColorspace::Rec709Linear);
    if r.executor().gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    if !r.gpu_times_tracks() {
        eprintln!("UNVERIFIED: this device doesn't time its work: the readout says not measured");
        assert!(r.take_track_gpu_ms().is_none());
        return;
    }
    let heavy = TestClip::of(media(0.0, 2.0)).effects(vec![BuiltinEffect::GaussianBlur { radius: 60.0 }]).build();
    let mut seq = sequence(vec![track("V2", TrackKind::Video, vec![heavy]), track("V1", TrackKind::Video, vec![media(0.0, 2.0)])]);
    let (w, h) = (1280, 720);
    seq.resolution = klipp_core::Resolution { width: w, height: h };
    let (blurred, plain) = (seq.tracks[0].id, seq.tracks[1].id);
    // One picture made once, as a decoder hands frames over: what is timed
    // is the GPU's work, not this test making pixels.
    struct Ready(Arc<Vec<f32>>);
    impl FrameSource for Ready {
        fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
            Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(&self.0)), width: w, height: h, pts: None })
        }
    }
    let mut src = Ready(Arc::new((0..w * h).flat_map(|i| [(i % 251) as f32 / 251.0, 0.4, 0.6, 1.0]).collect()));
    // Warm: shaders built, buffers made.
    let _ = r.render_display_u8_on_gpu(&seq, tc(0.5), w, h, &mut src, RenderOptions::default());
    r.time_tracks_next_frame();
    r.render_display_u8_on_gpu(&seq, tc(0.5), w, h, &mut src, RenderOptions::default()).expect("drawn on the stack");
    let started = std::time::Instant::now();
    let ms = loop {
        if let Some(ms) = r.take_track_gpu_ms() {
            break ms;
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "the times came back");
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    eprintln!("MEASURE GPU time a frame at {w}x{h}: blurred track {:.3} ms, plain track {:.3} ms", ms[&blurred], ms[&plain]);
    assert!(ms[&blurred] > 0.0 && ms[&plain] > 0.0, "{ms:?}");
    assert!(ms[&blurred] > ms[&plain], "the blur costs more: {ms:?}");
    // One timed frame at a time: the next is timed only when asked again.
    r.render_display_u8_on_gpu(&seq, tc(0.6), w, h, &mut src, RenderOptions::default()).expect("drawn");
    assert!(r.take_track_gpu_ms().is_none(), "nothing timed unasked");
}
