//! A slowed clip's frame blend on export (M4 step 7) reads its next frame
//! from the clip's own stream, the one its frame came from: a sequential
//! export opens one reader a clip, never a second to seek for the next
//! frame, and the frames between two media frames are their mix.
//!
//! The source is a grey ramp, one level per frame. Generated with the
//! ffmpeg CLI; the test skips without it.

use std::collections::HashMap;
use std::path::PathBuf;

use klipp_compose::{Inspect, RenderOptions, Renderer};
use klipp_core::*;
use klipp_effects::BuiltinEffect;
use klipp_render::*;

const W: u32 = 16;
const H: u32 = 16;
const FPS: u32 = 24;

/// Two seconds at 24 fps, frame n a flat grey of 20 + 4n, intra-only.
fn ramp() -> Option<PathBuf> {
    let path = klipp_testing::scratch::scratch_file("klipp-frame-blend-stream", "ramp.mkv");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color=c=black:s={W}x{H}:r={FPS}:d=2,geq=lum='20+4*N':cb=128:cr=128"))
        .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(path)
}

fn tc(frames: i64) -> RationalTime {
    RationalTime::from_frames(frames, FPS)
}

fn mean(linear: &[f32]) -> f32 {
    linear.chunks_exact(4).map(|p| p[0]).sum::<f32>() / (linear.len() / 4) as f32
}

#[test]
fn a_blended_export_reads_the_next_frame_from_its_stream() {
    let Some(path) = ramp() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let id = MediaId::new();
    let media = HashMap::from([(id, MediaFile { path, log_format: None, source_rotation: 0, duration_secs: Some(2.0), fps: Some((FPS, 1)), resolution: None, scene_transfer: None })]);
    // Half speed: twenty timeline frames over ten media frames.
    let mut clip = klipp_timeline::Clip::media(id, tc(0), tc(20), tc(0), tc(10));
    clip.speed = 0.5;
    clip.set_effect_list(
        vec![BuiltinEffect::SpeedRamp { speed: 0.5, blend_mode: 1 }],
        vec![true],
    );
    let mut seq = klipp_timeline::Sequence::new_default();
    seq.fps = Fps::new(FPS, 1);
    seq.resolution = Resolution { width: W, height: H };
    let mut track = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    let mut src = ExportFrameSource::new(&media, FPS as f64, WorkingColorspace::Rec709Linear);
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let frames: Vec<f32> = (0..20).map(|f| mean(&r.render_inspected(&seq, tc(f), W, H, &mut src, RenderOptions::default(), Inspect::default()).0)).collect();
    assert_eq!(src.readers_opened(), 1, "one stream, its next frames read from it");
    // Each odd frame falls halfway between two media frames: their mean,
    // to a tenth of the step between them (the file's millisecond times
    // put a frame a little off its 24th of a second). Never the later
    // frame whole: a halfway time can be nearer it.
    for f in (1..19).step_by(2) {
        let (a, b) = (frames[f - 1], frames[f + 1]);
        assert!(b > a, "the ramp rises: {a} then {b}");
        assert!((frames[f] - (a + b) / 2.0).abs() < (b - a) / 10.0, "frame {f}: {} isn't halfway from {a} to {b}", frames[f]);
    }
}
