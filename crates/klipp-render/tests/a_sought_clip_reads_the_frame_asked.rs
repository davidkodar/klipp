//! A clip's decoder moved back or far ahead ([`ExportFrameSource::seek`])
//! gives the very frame a decoder opened there gives: the frame asked
//! for, not the keyframe before it, one either side, or one held from
//! where it was.
//!
//! The source is a grey ramp, one level per frame, with a keyframe every
//! 12 frames, so every seek lands between keyframes. Generated with the
//! ffmpeg CLI; the test skips without it.

use std::collections::HashMap;
use std::path::PathBuf;

use klipp_compose::{FrameSource, SourcePixels};
use klipp_core::*;
use klipp_render::*;

const W: u32 = 160;
const H: u32 = 96;
const FPS: u32 = 24;

/// Three seconds at 24 fps, frame n a flat grey of 16 + 3n, a keyframe
/// every 12 frames.
fn ramp() -> Option<PathBuf> {
    let path = klipp_testing::scratch::scratch_file("klipp-sought-clip", "ramp.mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color=c=black:s={W}x{H}:r={FPS}:d=3,geq=lum='16+3*N':cb=128:cr=128"))
        .args(["-c:v", "libx264", "-qp", "0", "-g", "12", "-keyint_min", "12", "-sc_threshold", "0", "-bf", "2", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(path)
}

/// The frame's mean red, read back wherever it was made.
fn level(source: &mut ExportFrameSource, clip: &klipp_timeline::Clip, frame: u32) -> f32 {
    let secs = (f64::from(frame) + 0.25) / f64::from(FPS);
    let f = source.frame(clip, secs, W, H).unwrap_or_else(|| panic!("a frame at {frame}"));
    let px: Vec<f32> = match f.pixels {
        SourcePixels::Gpu(g) => g.read().to_vec(),
        SourcePixels::Linear(p) => p.to_vec(),
        SourcePixels::SceneReferred { .. } => panic!("an SDR ramp is not scene-referred"),
    };
    px.chunks_exact(4).map(|p| p[0]).sum::<f32>() / (px.len() / 4) as f32
}

#[test]
fn a_sought_clip_reads_the_frame_asked_for() {
    let Some(path) = ramp() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let id = MediaId::new();
    let media = HashMap::from([(id, MediaFile { path, log_format: None, source_rotation: 0, duration_secs: Some(3.0), fps: Some((FPS, 1)), resolution: None, scene_transfer: None })]);
    let t = |f: i64| RationalTime::from_frames(f, FPS);
    let clip = klipp_timeline::Clip::media(id, t(0), t(72), t(0), t(72));
    // What a decoder opened at the frame gives: each one alone.
    let fresh = |frame: u32| level(&mut ExportFrameSource::new(&media, f64::from(FPS), WorkingColorspace::Rec709Linear), &clip, frame);

    let mut source = ExportFrameSource::new(&media, f64::from(FPS), WorkingColorspace::Rec709Linear);
    for f in 0..30 {
        level(&mut source, &clip, f);
    }
    let opened = source.readers_opened();
    // Back past a keyframe, back inside the same stretch, far ahead, back
    // to the first frame, and on from each.
    for frame in [17u32, 26, 5, 60, 0, 47] {
        source.seek(&clip, (f64::from(frame) + 0.25) / f64::from(FPS));
        for f in frame..frame + 3 {
            let got = level(&mut source, &clip, f);
            let want = fresh(f);
            assert!((got - want).abs() < 1e-6, "frame {f} after a seek to {frame}: {got}, a fresh decoder gives {want}");
        }
    }
    // Neighbouring frames differ: a frame off would have been seen.
    assert!((fresh(17) - fresh(18)).abs() > 1e-3);
    assert_eq!(source.readers_opened(), opened, "the decoder is kept, not opened again");
}
