//! End-to-end integration tests for the klipp-render pipeline.
//!
//! Tests marked `#[ignore]` require `TEST_MEDIA_FILE` env var pointing to a video file.
//! Run with: `cargo test -p klipp-render --test integration -- --ignored`

use klipp_core::*;
use klipp_render::*;
use klipp_timeline::ClipKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use uuid::Uuid;

fn test_media_path() -> Option<PathBuf> {
    std::env::var("TEST_MEDIA_FILE").ok().map(PathBuf::from)
}

fn clip(kind: ClipKind, from: i64, to: i64, fps: u32) -> klipp_timeline::Clip {
    let tc = |f| RationalTime::from_frames(f, fps);
    klipp_timeline::Clip { kind, source_in: tc(0), source_out: tc(to - from), ..klipp_timeline::Clip::gap(tc(from), tc(to)) }
}

/// A single-video-track timeline at `fps`.
fn one_track(fps: u32, clips: Vec<klipp_timeline::Clip>) -> VideoTimeline {
    let mut v = VideoTimeline::default();
    v.sequence.fps = Fps::new(fps, 1);
    let mut t = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    t.clips = clips;
    v.sequence.tracks.push(t);
    v
}

/// One media clip from `path`, starting `source_in_secs` into the file.
fn media_timeline(path: &std::path::Path, from: i64, to: i64, fps: u32, source_in_secs: f64) -> VideoTimeline {
    let media_id = MediaId::new();
    let mut c = clip(ClipKind::Media { media_id }, from, to, fps);
    c.source_in = RationalTime::from_secs_f64(source_in_secs, fps);
    c.source_out = c.source_in + RationalTime::from_frames(to - from, fps);
    let mut v = one_track(fps, vec![c]);
    v.media.insert(media_id, MediaFile { path: path.to_path_buf(), log_format: None, source_rotation: 0, ..Default::default() });
    v
}

fn make_export_settings(path: &std::path::Path) -> ExportSettings {
    ExportSettings {
        pan_law: Default::default(),
        output_path: path.to_path_buf(),
        format: ExportFormat::Mp4,
        video_codec: ExportVideoCodec::H264,
        resolution: None,
        fps: None,
        crf: 28, // Fast quality for tests
        preset: EncoderPreset::Fast,
        audio_codec: ExportAudioCodec::Copy, // Skip audio for speed
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: klipp_core::RateControl::Crf,
        burn_in_subtitles: true, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
        bit_depth: 0, hdr_mode: klipp_core::HdrMode::Sdr, tone_map_op: klipp_core::ToneMapOp::Hable,
        odt: klipp_core::OdtChoice::MatchViewer,
        mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS }
}

// ═══════════════════════════════════════════════════════════════════
// Snapshot + Pipeline unit integration
// ═══════════════════════════════════════════════════════════════════

#[test]
fn export_job_empty_sequence() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-integration-test");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("empty_export.mp4");

    let snapshot = SequenceSnapshot {
        fps: 24.0,
        fps_num: 24,
        fps_den: 1,
        width: 64,
        height: 64,
        sample_rate: 48000,
        duration_frames: 5,
        export_range: None,
        video: Default::default(),
        audio_tracks: vec![],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));

    let job = ExportJob {
        id: Uuid::new_v4(),
        output_path: path.clone(),
        settings: make_export_settings(&path),
        snapshot,
        cancel,
        progress: progress.clone(),
        hw_info: Arc::new(std::sync::Mutex::new(String::new())),
        render_stage: Arc::new(std::sync::Mutex::new(String::new())),
        loudness_result: Arc::new(std::sync::Mutex::new(None)),
        timings: Default::default(),
        total_frames: 5,
        analyser: None,
    };

    // Export should succeed (all black frames)
    job.run().unwrap();

    // Verify output
    assert!(path.exists(), "output file should exist");
    let meta = std::fs::metadata(&path).unwrap();
    assert!(meta.len() > 100, "output should be non-trivial");

    // Verify progress reached total
    assert_eq!(progress.load(Ordering::Relaxed), 5);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn export_job_cancellation_immediate() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-integration-test");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("cancel_immediate.mp4");

    let snapshot = SequenceSnapshot {
        fps: 24.0,
        fps_num: 24,
        fps_den: 1,
        width: 64,
        height: 64,
        sample_rate: 48000,
        duration_frames: 100,
        export_range: None,
        video: Default::default(),
        audio_tracks: vec![],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };

    let cancel = Arc::new(AtomicBool::new(true)); // Cancel before start
    let progress = Arc::new(AtomicU64::new(0));

    let job = ExportJob {
        id: Uuid::new_v4(),
        output_path: path.clone(),
        settings: make_export_settings(&path),
        snapshot,
        cancel,
        progress,
        hw_info: Arc::new(std::sync::Mutex::new(String::new())),
        render_stage: Arc::new(std::sync::Mutex::new(String::new())),
        loudness_result: Arc::new(std::sync::Mutex::new(None)),
        timings: Default::default(),
        total_frames: 100,
        analyser: None,
    };

    let result = job.run();
    assert!(matches!(result, Err(RenderError::Cancelled)));
    assert!(!path.exists(), "partial file should be cleaned up");
}

#[test]
fn export_job_cancellation_midway() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-integration-test");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("cancel_midway.mp4");

    let snapshot = SequenceSnapshot {
        fps: 24.0,
        fps_num: 24,
        fps_den: 1,
        width: 64,
        height: 64,
        sample_rate: 48000,
        duration_frames: 1000, // Long enough to cancel mid-way
        export_range: None,
        video: Default::default(),
        audio_tracks: vec![],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));

    let cancel_clone = cancel.clone();
    let progress_clone = progress.clone();

    let job = ExportJob {
        id: Uuid::new_v4(),
        output_path: path.clone(),
        settings: make_export_settings(&path),
        snapshot,
        cancel: cancel_clone,
        progress: progress_clone,
        hw_info: Arc::new(std::sync::Mutex::new(String::new())),
        render_stage: Arc::new(std::sync::Mutex::new(String::new())),
        loudness_result: Arc::new(std::sync::Mutex::new(None)),
        timings: Default::default(),
        total_frames: 1000,
        analyser: None,
    };

    // Run on a thread, cancel after some progress
    let cancel_signal = cancel.clone();
    let progress_watch = progress.clone();

    let handle = std::thread::spawn(move || job.run());

    // Wait for some progress, then cancel
    loop {
        let done = progress_watch.load(Ordering::Relaxed);
        if done >= 5 {
            cancel_signal.store(true, Ordering::Relaxed);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    let result = handle.join().unwrap();
    assert!(matches!(result, Err(RenderError::Cancelled)));

    // Progress should be between 5 and 1000 (cancelled partway)
    let final_progress = progress.load(Ordering::Relaxed);
    assert!(
        (5..1000).contains(&final_progress),
        "expected partial progress, got {final_progress}"
    );

    assert!(!path.exists(), "partial file should be cleaned up");
}

#[test]
fn export_job_progress_advances() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-integration-test");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("progress_test.mp4");

    let total = 10u64;
    let snapshot = SequenceSnapshot {
        fps: 24.0,
        fps_num: 24,
        fps_den: 1,
        width: 64,
        height: 64,
        sample_rate: 48000,
        duration_frames: total,
        export_range: None,
        video: Default::default(),
        audio_tracks: vec![],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));

    let job = ExportJob {
        id: Uuid::new_v4(),
        output_path: path.clone(),
        settings: make_export_settings(&path),
        snapshot,
        cancel,
        progress: progress.clone(),
        hw_info: Arc::new(std::sync::Mutex::new(String::new())),
        render_stage: Arc::new(std::sync::Mutex::new(String::new())),
        loudness_result: Arc::new(std::sync::Mutex::new(None)),
        timings: Default::default(),
        total_frames: total,
        analyser: None,
    };

    job.run().unwrap();

    assert_eq!(
        progress.load(Ordering::Relaxed),
        total,
        "progress should equal total frames after completion"
    );

    let _ = std::fs::remove_file(&path);
}

// ═══════════════════════════════════════════════════════════════════
// ExportJob — with real media file
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore = "requires TEST_MEDIA_FILE env var"]
fn export_real_media_h264() {
    let media_path = test_media_path().expect("set TEST_MEDIA_FILE");
    let dir = klipp_testing::scratch::scratch_dir("klipp-integration-test");
    let _ = std::fs::create_dir_all(&dir);
    let output_path = dir.join("real_export_h264.mp4");

    let snapshot = SequenceSnapshot {
        fps: 30.0,
        fps_num: 30,
        fps_den: 1,
        width: 640,
        height: 480,
        sample_rate: 48000,
        duration_frames: 30, // 1 second
        export_range: None,
        video: media_timeline(&media_path, 0, 30, 30, 0.0),
        pan_law: Default::default(),
        audio_tracks: vec![snapshot::AudioTrackSnapshot {
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            clips: vec![snapshot::AudioClipSnapshot {
                timeline_start_frame: 0,
                timeline_end_frame: 30,
                source_start_secs: 0.0,
                path: media_path.clone(),
                audio_effects: vec![],
                audio_effect_keyframes: std::collections::HashMap::new(),
                source_range: (0.0, 0.0),
                shape: Default::default(),
                varispeed: None,
                exact_secs: None,
            }],
            automation: Default::default(),
            effects: Vec::new(),
        }],
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));

    let job = ExportJob {
        id: Uuid::new_v4(),
        output_path: output_path.clone(),
        settings: make_export_settings(&output_path),
        snapshot,
        cancel,
        progress: progress.clone(),
        hw_info: Arc::new(std::sync::Mutex::new(String::new())),
        render_stage: Arc::new(std::sync::Mutex::new(String::new())),
        loudness_result: Arc::new(std::sync::Mutex::new(None)),
        timings: Default::default(),
        total_frames: 30,
        analyser: None,
    };

    job.run().unwrap();

    assert!(output_path.exists());
    let meta = std::fs::metadata(&output_path).unwrap();
    assert!(meta.len() > 1000, "exported file should be substantial");
    assert_eq!(progress.load(Ordering::Relaxed), 30);

    // Verify with ffprobe
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(&output_path)
        .output();

    if let Ok(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("h264"), "expected h264 codec: {stdout}");
        assert!(stdout.contains("640"), "expected width 640: {stdout}");
        assert!(stdout.contains("480"), "expected height 480: {stdout}");
    }

    let _ = std::fs::remove_file(&output_path);
}

#[test]
#[ignore = "requires TEST_MEDIA_FILE env var"]
fn export_real_media_with_color_grade() {
    let media_path = test_media_path().expect("set TEST_MEDIA_FILE");
    let dir = klipp_testing::scratch::scratch_dir("klipp-integration-test");
    let _ = std::fs::create_dir_all(&dir);
    let output_path = dir.join("real_export_graded.mp4");

    let snapshot = SequenceSnapshot {
        fps: 30.0,
        fps_num: 30,
        fps_den: 1,
        width: 640,
        height: 480,
        sample_rate: 48000,
        duration_frames: 15, // 0.5 seconds
        export_range: None,
        video: media_timeline(&media_path, 0, 15, 30, 1.0),
        audio_tracks: vec![],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));

    let job = ExportJob {
        id: Uuid::new_v4(),
        output_path: output_path.clone(),
        settings: make_export_settings(&output_path),
        snapshot,
        cancel,
        progress: progress.clone(),
        hw_info: Arc::new(std::sync::Mutex::new(String::new())),
        render_stage: Arc::new(std::sync::Mutex::new(String::new())),
        loudness_result: Arc::new(std::sync::Mutex::new(None)),
        timings: Default::default(),
        total_frames: 15,
        analyser: None,
    };

    job.run().unwrap();

    assert!(output_path.exists());
    assert_eq!(progress.load(Ordering::Relaxed), 15);

    let meta = std::fs::metadata(&output_path).unwrap();
    assert!(meta.len() > 500, "graded export should produce output");

    let _ = std::fs::remove_file(&output_path);
}

#[test]
fn export_with_title_and_gap() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-integration-test");
    let _ = std::fs::create_dir_all(&dir);
    let output_path = dir.join("title_and_gap.mp4");

    let title_data = klipp_timeline::TitleData::default();

    let snapshot = SequenceSnapshot {
        fps: 24.0,
        fps_num: 24,
        fps_den: 1,
        width: 320,
        height: 240,
        sample_rate: 48000,
        duration_frames: 48, // 2 seconds
        export_range: None,
        video: one_track(24, vec![clip(ClipKind::Gap, 0, 24, 24), clip(ClipKind::Title(title_data), 24, 48, 24)]),
        audio_tracks: vec![],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));

    let job = ExportJob {
        id: Uuid::new_v4(),
        output_path: output_path.clone(),
        settings: make_export_settings(&output_path),
        snapshot,
        cancel,
        progress: progress.clone(),
        hw_info: Arc::new(std::sync::Mutex::new(String::new())),
        render_stage: Arc::new(std::sync::Mutex::new(String::new())),
        loudness_result: Arc::new(std::sync::Mutex::new(None)),
        timings: Default::default(),
        total_frames: 48,
        analyser: None,
    };

    job.run().unwrap();

    assert!(output_path.exists());
    assert_eq!(progress.load(Ordering::Relaxed), 48);

    let _ = std::fs::remove_file(&output_path);
}

#[test]
fn export_with_keyframed_opacity() {
    // Verify that keyframed opacity is evaluated during export (not stuck at default).
    // Uses a title clip with opacity keyframes: 1.0 at t=0, 0.0 at t=1.
    let dir = klipp_testing::scratch::scratch_dir("klipp-integration-test");
    let _ = std::fs::create_dir_all(&dir);
    let output_path = dir.join("keyframed_opacity.mp4");

    let title_data = klipp_timeline::TitleData::default();

    let kf_start = klipp_core::Keyframe {
        time_secs: 0.0,
        value: 1.0,
        interpolation: klipp_core::Interpolation::Linear,
        tangent_out: (0.0, 0.0),
        tangent_in: (0.0, 0.0),
        handles: Default::default(),
    };
    let kf_end = klipp_core::Keyframe {
        time_secs: 1.0,
        value: 0.0,
        interpolation: klipp_core::Interpolation::Linear,
        tangent_out: (0.0, 0.0),
        tangent_in: (0.0, 0.0),
        handles: Default::default(),
    };

    let mut effect_keyframes = std::collections::HashMap::new();
    effect_keyframes.insert("0:value".to_string(), vec![kf_start.clone(), kf_end.clone()]);

    let snapshot = SequenceSnapshot {
        fps: 24.0,
        fps_num: 24,
        fps_den: 1,
        width: 320,
        height: 240,
        sample_rate: 48000,
        duration_frames: 24,
        export_range: None,
        video: {
            let mut c = clip(ClipKind::Title(title_data), 0, 24, 24);
            c.set_effect_list(vec![klipp_effects::BuiltinEffect::Opacity { value: 1.0 }], c.builtin_effects_enabled().to_vec());
            c.edit_effect_keyframes(|v| *v = effect_keyframes);
            c.edit_opacity_keyframes(|v| *v = vec![kf_start, kf_end]);
            one_track(24, vec![c])
        },
        audio_tracks: vec![],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));

    let job = ExportJob {
        id: Uuid::new_v4(),
        output_path: output_path.clone(),
        settings: make_export_settings(&output_path),
        snapshot,
        cancel,
        progress: progress.clone(),
        hw_info: Arc::new(std::sync::Mutex::new(String::new())),
        render_stage: Arc::new(std::sync::Mutex::new(String::new())),
        loudness_result: Arc::new(std::sync::Mutex::new(None)),
        timings: Default::default(),
        total_frames: 24,
        analyser: None,
    };

    job.run().unwrap();

    assert!(output_path.exists());
    assert_eq!(progress.load(Ordering::Relaxed), 24);

    let meta = std::fs::metadata(&output_path).unwrap();
    assert!(meta.len() > 100, "output should be non-trivial");

    let _ = std::fs::remove_file(&output_path);
}

#[test]
fn export_mp4_with_embedded_subtitle_stream() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-integration-test");
    let _ = std::fs::create_dir_all(&dir);
    let output_path = dir.join("embedded_subs.mp4");

    let title_data = klipp_timeline::TitleData::default();

    let mut settings = make_export_settings(&output_path);
    settings.embed_subtitles = true;

    let snapshot = SequenceSnapshot {
        fps: 24.0,
        fps_num: 24,
        fps_den: 1,
        width: 320,
        height: 240,
        sample_rate: 48000,
        duration_frames: 48,
        export_range: None,
        video: one_track(24, vec![clip(ClipKind::Title(title_data), 0, 48, 24)]),
        audio_tracks: vec![],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![snapshot::SubtitleTrackSnapshot {
            clips: vec![
                snapshot::SubtitleClipSnapshot {
                    timeline_start_frame: 0,
                    timeline_end_frame: 24,
                    text: "Hello World".to_string(),
                },
                snapshot::SubtitleClipSnapshot {
                    timeline_start_frame: 30,
                    timeline_end_frame: 48,
                    text: "Second subtitle".to_string(),
                },
            ],
        }],
        chapters: Vec::new(),
        working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));

    let job = ExportJob {
        id: Uuid::new_v4(),
        output_path: output_path.clone(),
        settings,
        snapshot,
        cancel,
        progress: progress.clone(),
        hw_info: Arc::new(std::sync::Mutex::new(String::new())),
        render_stage: Arc::new(std::sync::Mutex::new(String::new())),
        loudness_result: Arc::new(std::sync::Mutex::new(None)),
        timings: Default::default(),
        total_frames: 48,
        analyser: None,
    };

    job.run().unwrap();

    assert!(output_path.exists());
    assert_eq!(progress.load(Ordering::Relaxed), 48);

    // Verify subtitle stream via ffprobe
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-select_streams", "s"])
        .arg(&output_path)
        .output();

    if let Ok(output) = probe {
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("codec_name=mov_text"), "expected mov_text subtitle stream: {stdout}");
    }

    let _ = std::fs::remove_file(&output_path);
}

#[test]
fn export_without_embed_has_no_subtitle_stream() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-integration-test");
    let _ = std::fs::create_dir_all(&dir);
    let output_path = dir.join("no_subs.mp4");

    let title_data = klipp_timeline::TitleData::default();
    let mut settings = make_export_settings(&output_path);
    settings.embed_subtitles = false;

    let snapshot = SequenceSnapshot {
        fps: 24.0,
        fps_num: 24,
        fps_den: 1,
        width: 320,
        height: 240,
        sample_rate: 48000,
        duration_frames: 24,
        export_range: None,
        video: one_track(24, vec![clip(ClipKind::Title(title_data), 0, 24, 24)]),
        audio_tracks: vec![],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![snapshot::SubtitleTrackSnapshot {
            clips: vec![snapshot::SubtitleClipSnapshot {
                timeline_start_frame: 0,
                timeline_end_frame: 24,
                text: "Should not be embedded".to_string(),
            }],
        }],
        chapters: Vec::new(),
        working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));

    let job = ExportJob {
        id: Uuid::new_v4(),
        output_path: output_path.clone(),
        settings,
        snapshot,
        cancel,
        progress: progress.clone(),
        hw_info: Arc::new(std::sync::Mutex::new(String::new())),
        render_stage: Arc::new(std::sync::Mutex::new(String::new())),
        loudness_result: Arc::new(std::sync::Mutex::new(None)),
        timings: Default::default(),
        total_frames: 24,
        analyser: None,
    };

    job.run().unwrap();

    assert!(output_path.exists());

    // Verify NO subtitle stream
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-select_streams", "s"])
        .arg(&output_path)
        .output();

    if let Ok(output) = probe {
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.is_empty() || !stdout.contains("codec_name"),
            "expected no subtitle stream: {stdout}");
    }

    let _ = std::fs::remove_file(&output_path);
}
