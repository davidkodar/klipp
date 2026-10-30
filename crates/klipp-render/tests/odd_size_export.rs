//! An export asked for at an odd size, or of an odd-sized sequence,
//! encodes at even sides instead of aborting the process (hunt #2).
//!
//! 4:2:0 video halves the colour planes, and ffmpeg's picture code
//! asserts on an odd side; the export job rounded the size and the
//! encoder did not. Needs the ffmpeg CLI; skips without it.

use klipp_core::*;
use klipp_render::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

fn tmp(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-odd-size-export", name)
}

fn source() -> Option<PathBuf> {
    let path = tmp("grey.mp4");
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "color=c=gray:s=64x64:r=25:d=0.2"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success())
        .then_some(path)
}

/// The coded size of the video stream in `path`.
fn size_of(path: &Path) -> (u32, u32) {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text.trim().split(',').map(|v| v.parse::<u32>().unwrap());
    (it.next().unwrap(), it.next().unwrap())
}

fn export(src: &Path, seq: (u32, u32), asked: Option<(u32, u32)>, out: &Path) {
    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 25);
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: seq.0, height: seq.1 };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![klipp_timeline::Clip::media(id, tc(0), tc(5), tc(0), tc(5))];
    video.sequence.tracks.push(t);
    video.media.insert(id, MediaFile { path: src.to_path_buf(), log_format: None, source_rotation: 0, ..Default::default() });
    let snapshot = SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0, fps_num: 25, fps_den: 1, width: seq.0, height: seq.1, sample_rate: 48000,
        duration_frames: 5, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    let settings = ExportSettings {
        output_path: out.to_path_buf(),
        format: ExportFormat::Mp4,
        video_codec: ExportVideoCodec::H264,
        audio_codec: ExportAudioCodec::Copy,
        resolution: asked.map(|(width, height)| Resolution { width, height }),
        ..Default::default()
    };
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: Default::default(),
        total_frames: 5,
        analyser: None,
    }
    .run()
    .unwrap();
}

#[test]
fn an_odd_size_exports_at_even_sides() {
    let Some(src) = source() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    // Asked for an odd size, of an even sequence.
    let out = tmp("asked-odd.mp4");
    export(&src, (64, 64), Some((81, 47)), &out);
    assert_eq!(size_of(&out), (80, 46));
    // An odd sequence, its own size.
    let out = tmp("odd-sequence.mp4");
    export(&src, (65, 63), None, &out);
    assert_eq!(size_of(&out), (64, 62));
}
