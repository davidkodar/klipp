//! An export says where its time went and how its frames were composited
//! (#1924): per-frame render, decode, encode and sound times, the frames
//! made on the GPU stack and on the CPU and why, and the encoders it
//! opened, kept once it ends. Needs the ffmpeg CLI; skips without it.

use klipp_core::*;
use klipp_render::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

fn tmp(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-export-status-timings", name)
}

fn source() -> Option<PathBuf> {
    let path = tmp("grey.mp4");
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=128x72:r=25:d=0.5"])
        .args(["-f", "lavfi", "-i", "sine=f=440:d=0.5"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success())
        .then_some(path)
}

/// What the job left: its timings and its hardware line.
struct Ran {
    timings: ExportTimings,
    hardware: String,
}

/// Export ten frames of `src` at the sequence's size, or `asked`.
fn export(src: &Path, asked: Option<(u32, u32)>, out: &Path) -> Ran {
    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 25);
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: 128, height: 72 };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![klipp_timeline::Clip::media(id, tc(0), tc(10), tc(0), tc(10))];
    video.sequence.tracks.push(t);
    video.media.insert(id, MediaFile { path: src.to_path_buf(), log_format: None, source_rotation: 0, ..Default::default() });
    let snapshot = SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0, fps_num: 25, fps_den: 1, width: 128, height: 72, sample_rate: 48000,
        duration_frames: 10, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    let settings = ExportSettings {
        output_path: out.to_path_buf(),
        format: ExportFormat::Mp4,
        video_codec: ExportVideoCodec::H264,
        audio_codec: ExportAudioCodec::Aac,
        resolution: asked.map(|(width, height)| Resolution { width, height }),
        ..Default::default()
    };
    let timings: Arc<Mutex<Option<ExportTimings>>> = Default::default();
    let hw: Arc<Mutex<String>> = Default::default();
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Arc::clone(&hw), render_stage: Default::default(), loudness_result: Default::default(), timings: Arc::clone(&timings),
        total_frames: 10,
        analyser: None,
    }
    .run()
    .unwrap();
    let timings = timings.lock().unwrap().clone().expect("the job left its timings");
    let hardware = hw.lock().unwrap().clone();
    Ran { timings, hardware }
}

/// The figures every export leaves: each frame counted once, on the GPU
/// or the CPU, the times finite, the encoders named.
fn holds(t: &ExportTimings, label: &str) {
    assert_eq!(t.frames, 10, "{label}: {t:?}");
    assert_eq!(t.gpu_composited + t.cpu_composited, t.frames, "{label}: every frame composited once: {t:?}");
    for (name, ms) in [("render", t.render_ms), ("decode", t.decode_ms), ("convert", t.convert_ms), ("pad", t.pad_ms), ("encode", t.encode_ms), ("audio", t.audio_ms)] {
        assert!(ms.is_finite() && ms >= 0.0, "{label}: {name} {ms}");
    }
    assert!(t.wall_secs > 0.0, "{label}: {t:?}");
    assert!(t.video_encoder.as_deref().is_some_and(|e| !e.is_empty()), "{label}: {t:?}");
    assert_eq!(t.audio_encoder.as_deref(), Some("aac"), "{label}: {t:?}");
}

#[test]
fn an_export_says_where_its_time_went_and_how_it_was_composited() {
    let Some(src) = source() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };

    // Its own size: made on the GPU where there is one, and the encoder
    // named whichever way the frames went.
    let full = export(&src, None, &tmp("full.mp4"));
    holds(&full.timings, "full size");
    assert!(full.hardware.contains("Encode:"), "the hardware line, whichever way the frames went: {:?}", full.hardware);
    // Letterboxed in an output of another shape: composited on the GPU
    // all the same, and padded after (#2488).
    let boxed = export(&src, Some((128, 128)), &tmp("boxed.mp4"));
    holds(&boxed.timings, "letterboxed");
    if full.timings.gpu_composited == 0 {
        eprintln!("UNVERIFIED: no frame was composited on the GPU here ({:?}); the GPU half is not checked", full.timings.compositing());
        return;
    }
    assert_eq!(full.timings.compositing().as_deref(), Some("GPU"), "{:?}", full.timings);
    assert_eq!(boxed.timings.compositing().as_deref(), Some("GPU"), "{:?}", boxed.timings);
}
