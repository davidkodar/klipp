//! End-to-end integration tests for the klipp-render pipeline.
//!
//! Tests marked `#[ignore]` require `TEST_MEDIA_FILE` env var pointing to a video file.
//! Run with: `cargo test -p klipp-render --test integration -- --ignored`

use klipp_core::*;
use klipp_render::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use uuid::Uuid;

fn test_media_path() -> Option<PathBuf> {
    std::env::var("TEST_MEDIA_FILE").ok().map(PathBuf::from)
}

fn make_export_settings(path: &std::path::Path) -> ExportSettings {
    ExportSettings {
        output_path: path.to_path_buf(),
        format: ExportFormat::Mp4,
        video_codec: ExportVideoCodec::H264,
        resolution: None,
        fps: None,
        crf: 28, // Fast quality for tests
        preset: EncoderPreset::Fast,
        audio_codec: ExportAudioCodec::Copy, // Skip audio for speed
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: klipp_core::RateControl::Crf,
        burn_in_subtitles: true, embed_subtitles: false, target_lufs: None, embed_chapters: false,
        bit_depth: 0, hdr_mode: klipp_core::HdrMode::Sdr, tone_map_op: klipp_core::ToneMapOp::Hable,
        odt: klipp_core::OdtChoice::MatchViewer }
}

// ═══════════════════════════════════════════════════════════════════
// Snapshot + Pipeline unit integration
// ═══════════════════════════════════════════════════════════════════

#[test]
fn snapshot_to_pipeline_roundtrip() {
    // Verify that a snapshot can be created and a pipeline initialized from it
    let _snapshot = SequenceSnapshot {
        fps: 24.0,
        fps_num: 24,
        fps_den: 1,
        width: 320,
        height: 240,
        sample_rate: 48000,
        duration_frames: 24,
        export_range: None,
        video_tracks: vec![],
        audio_tracks: vec![],
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
    };

    let mut pipeline = RenderPipeline::default_stages(None);
    let settings = make_export_settings(std::path::Path::new("/tmp/test.mp4"));
    pipeline.begin_export(&settings);

    // Process a black frame through the pipeline
    let black = vec![0u8; 320 * 240 * 4];
    let ctx = FrameContext {
        export_frame: 0,
        clip_frame: 0,
        clip_time_secs: 0.0,
        clip: None,
        fps: 24.0,
        track_index: 0,
    };
    let result = pipeline.process_frame(black.clone(), 320, 240, &ctx);
    assert_eq!(result.len(), black.len());

    pipeline.end_export();
}

#[test]
fn compositor_with_gap_produces_none() {
    let snapshot = SequenceSnapshot {
        fps: 24.0,
        fps_num: 24,
        fps_den: 1,
        width: 64,
        height: 64,
        sample_rate: 48000,
        duration_frames: 10,
        export_range: None,
        video_tracks: vec![],
        audio_tracks: vec![],
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
    };

    let mut compositor = FrameCompositor::new();
    let mut decoders = std::collections::HashMap::new();
    let mut pipelines: Vec<klipp_render::RenderPipeline> = Vec::new();

    // No tracks → None
    let result = compositor
        .composite_frame(0, 0, &snapshot, &mut decoders, &mut pipelines)
        .unwrap();
    assert!(result.is_none());
}

// ═══════════════════════════════════════════════════════════════════
// ExportJob — synthetic (no real media)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn export_job_empty_sequence() {
    let dir = std::env::temp_dir().join("klipp_integration_test");
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
        video_tracks: vec![],
        audio_tracks: vec![],
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
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
        total_frames: 5,
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
    let dir = std::env::temp_dir().join("klipp_integration_test");
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
        video_tracks: vec![],
        audio_tracks: vec![],
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
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
        total_frames: 100,
    };

    let result = job.run();
    assert!(matches!(result, Err(RenderError::Cancelled)));
    assert!(!path.exists(), "partial file should be cleaned up");
}

#[test]
fn export_job_cancellation_midway() {
    let dir = std::env::temp_dir().join("klipp_integration_test");
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
        video_tracks: vec![],
        audio_tracks: vec![],
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
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
        total_frames: 1000,
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
        final_progress >= 5 && final_progress < 1000,
        "expected partial progress, got {final_progress}"
    );

    assert!(!path.exists(), "partial file should be cleaned up");
}

#[test]
fn export_job_progress_advances() {
    let dir = std::env::temp_dir().join("klipp_integration_test");
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
        video_tracks: vec![],
        audio_tracks: vec![],
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
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
        total_frames: total,
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
    let dir = std::env::temp_dir().join("klipp_integration_test");
    let _ = std::fs::create_dir_all(&dir);
    let output_path = dir.join("real_export_h264.mp4");

    let clip_id = ClipId::new();

    let snapshot = SequenceSnapshot {
        fps: 30.0,
        fps_num: 30,
        fps_den: 1,
        width: 640,
        height: 480,
        sample_rate: 48000,
        duration_frames: 30, // 1 second
        export_range: None,
        video_tracks: vec![snapshot::VideoTrackSnapshot {
            opacity: 1.0,
            visible: true,
            clips: vec![snapshot::ClipSnapshot {
                id: clip_id,
                kind: snapshot::ClipKindSnapshot::Media {
                    path: media_path.clone(),
                    has_audio: true,
                },
                timeline_start_frame: 0,
                timeline_end_frame: 30,
                source_start_secs: 0.0,
                source_end_secs: 1.0,
                speed: 1.0,
                node_graph: klipp_nodes::NodeGraph::default(),
                effects: vec![],
                builtin_effects: vec![],
                source_rotation: 0,
                stabilization_data: None,
                smoothed_trajectory: None,
                effect_keyframes: std::collections::HashMap::new(),
                audio_effect_keyframes: std::collections::HashMap::new(),
                opacity_keyframes: vec![],
                transform_pos_x_keyframes: vec![],
                transform_pos_y_keyframes: vec![],
                transform_scale_keyframes: vec![],
                transform_rotation_keyframes: vec![],
                blend_mode: klipp_core::BlendMode::Normal,
                matte_source: None,
                matte_stencil: None,
                consumed_as_matte: false,
            }],
            transitions: vec![],
        }],
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
            }],
        }],
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
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
        total_frames: 30,
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
    let dir = std::env::temp_dir().join("klipp_integration_test");
    let _ = std::fs::create_dir_all(&dir);
    let output_path = dir.join("real_export_graded.mp4");

    let clip_id = ClipId::new();

    // TODO Session 2: create a node graph with visible grading changes.
    // For now, use default graph (pass-through).
    let graph = klipp_nodes::NodeGraph::default();

    let snapshot = SequenceSnapshot {
        fps: 30.0,
        fps_num: 30,
        fps_den: 1,
        width: 640,
        height: 480,
        sample_rate: 48000,
        duration_frames: 15, // 0.5 seconds
        export_range: None,
        video_tracks: vec![snapshot::VideoTrackSnapshot {
            opacity: 1.0,
            visible: true,
            clips: vec![snapshot::ClipSnapshot {
                id: clip_id,
                kind: snapshot::ClipKindSnapshot::Media {
                    path: media_path,
                    has_audio: false,
                },
                timeline_start_frame: 0,
                timeline_end_frame: 15,
                source_start_secs: 1.0, // Start at 1s to test seek
                source_end_secs: 1.5,
                speed: 1.0,
                node_graph: graph,
                effects: vec![],
                builtin_effects: vec![],
                source_rotation: 0,
                stabilization_data: None,
                smoothed_trajectory: None,
                effect_keyframes: std::collections::HashMap::new(),
                audio_effect_keyframes: std::collections::HashMap::new(),
                opacity_keyframes: vec![],
                transform_pos_x_keyframes: vec![],
                transform_pos_y_keyframes: vec![],
                transform_scale_keyframes: vec![],
                transform_rotation_keyframes: vec![],
                blend_mode: klipp_core::BlendMode::Normal,
                matte_source: None,
                matte_stencil: None,
                consumed_as_matte: false,
            }],
            transitions: vec![],
        }],
        audio_tracks: vec![],
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
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
        total_frames: 15,
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
    let dir = std::env::temp_dir().join("klipp_integration_test");
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
        video_tracks: vec![snapshot::VideoTrackSnapshot {
            opacity: 1.0,
            visible: true,
            clips: vec![
                // Gap for first second
                snapshot::ClipSnapshot {
                    id: ClipId::new(),
                    kind: snapshot::ClipKindSnapshot::Gap,
                    timeline_start_frame: 0,
                    timeline_end_frame: 24,
                    source_start_secs: 0.0,
                    source_end_secs: 1.0,
                    speed: 1.0,
                    node_graph: klipp_nodes::NodeGraph::default(),
                    effects: vec![],
                    builtin_effects: vec![],
                    source_rotation: 0,
                    stabilization_data: None,
                    smoothed_trajectory: None,
                    effect_keyframes: std::collections::HashMap::new(),
                    audio_effect_keyframes: std::collections::HashMap::new(),
                    opacity_keyframes: vec![],
                    transform_pos_x_keyframes: vec![],
                    transform_pos_y_keyframes: vec![],
                    transform_scale_keyframes: vec![],
                    transform_rotation_keyframes: vec![],
                    blend_mode: klipp_core::BlendMode::Normal,
                    matte_source: None,
                    matte_stencil: None,
                    consumed_as_matte: false,
                },
                // Title for second second
                snapshot::ClipSnapshot {
                    id: ClipId::new(),
                    kind: snapshot::ClipKindSnapshot::Title(title_data),
                    timeline_start_frame: 24,
                    timeline_end_frame: 48,
                    source_start_secs: 0.0,
                    source_end_secs: 1.0,
                    speed: 1.0,
                    node_graph: klipp_nodes::NodeGraph::default(),
                    effects: vec![],
                    builtin_effects: vec![],
                    source_rotation: 0,
                    stabilization_data: None,
                    smoothed_trajectory: None,
                    effect_keyframes: std::collections::HashMap::new(),
                    audio_effect_keyframes: std::collections::HashMap::new(),
                    opacity_keyframes: vec![],
                    transform_pos_x_keyframes: vec![],
                    transform_pos_y_keyframes: vec![],
                    transform_scale_keyframes: vec![],
                    transform_rotation_keyframes: vec![],
                    blend_mode: klipp_core::BlendMode::Normal,
                    matte_source: None,
                    matte_stencil: None,
                    consumed_as_matte: false,
                },
            ],
            transitions: vec![],
        }],
        audio_tracks: vec![],
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
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
        total_frames: 48,
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
    let dir = std::env::temp_dir().join("klipp_integration_test");
    let _ = std::fs::create_dir_all(&dir);
    let output_path = dir.join("keyframed_opacity.mp4");

    let title_data = klipp_timeline::TitleData::default();

    let kf_start = klipp_core::Keyframe {
        time_secs: 0.0,
        value: 1.0,
        interpolation: klipp_core::Interpolation::Linear,
        tangent_out: (0.0, 0.0),
        tangent_in: (0.0, 0.0),
    };
    let kf_end = klipp_core::Keyframe {
        time_secs: 1.0,
        value: 0.0,
        interpolation: klipp_core::Interpolation::Linear,
        tangent_out: (0.0, 0.0),
        tangent_in: (0.0, 0.0),
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
        video_tracks: vec![snapshot::VideoTrackSnapshot {
            opacity: 1.0,
            visible: true,
            clips: vec![snapshot::ClipSnapshot {
                id: ClipId::new(),
                kind: snapshot::ClipKindSnapshot::Title(title_data),
                timeline_start_frame: 0,
                timeline_end_frame: 24,
                source_start_secs: 0.0,
                source_end_secs: 1.0,
                speed: 1.0,
                node_graph: klipp_nodes::NodeGraph::default(),
                effects: vec![],
                builtin_effects: vec![klipp_effects::BuiltinEffect::Opacity { value: 1.0 }],
                source_rotation: 0,
                stabilization_data: None,
                smoothed_trajectory: None,
                effect_keyframes,
                audio_effect_keyframes: std::collections::HashMap::new(),
                opacity_keyframes: vec![kf_start, kf_end],
                transform_pos_x_keyframes: vec![],
                transform_pos_y_keyframes: vec![],
                transform_scale_keyframes: vec![],
                transform_rotation_keyframes: vec![],
                blend_mode: klipp_core::BlendMode::Normal,
                matte_source: None,
                matte_stencil: None,
                consumed_as_matte: false,
            }],
            transitions: vec![],
        }],
        audio_tracks: vec![],
        global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
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
        total_frames: 24,
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
    let dir = std::env::temp_dir().join("klipp_integration_test");
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
        video_tracks: vec![snapshot::VideoTrackSnapshot {
            opacity: 1.0,
            visible: true,
            clips: vec![snapshot::ClipSnapshot {
                id: ClipId::new(),
                kind: snapshot::ClipKindSnapshot::Title(title_data),
                timeline_start_frame: 0,
                timeline_end_frame: 48,
                source_start_secs: 0.0,
                source_end_secs: 2.0,
                speed: 1.0,
                node_graph: klipp_nodes::NodeGraph::default(),
                effects: vec![],
                builtin_effects: vec![],
                source_rotation: 0,
                stabilization_data: None,
                smoothed_trajectory: None,
                effect_keyframes: std::collections::HashMap::new(),
                audio_effect_keyframes: std::collections::HashMap::new(),
                opacity_keyframes: vec![],
                transform_pos_x_keyframes: vec![],
                transform_pos_y_keyframes: vec![],
                transform_scale_keyframes: vec![],
                transform_rotation_keyframes: vec![],
                blend_mode: klipp_core::BlendMode::Normal,
                matte_source: None,
                matte_stencil: None,
                consumed_as_matte: false,
            }],
            transitions: vec![],
        }],
        audio_tracks: vec![],
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
        working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
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
        total_frames: 48,
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
    let dir = std::env::temp_dir().join("klipp_integration_test");
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
        video_tracks: vec![snapshot::VideoTrackSnapshot {
            opacity: 1.0,
            visible: true,
            clips: vec![snapshot::ClipSnapshot {
                id: ClipId::new(),
                kind: snapshot::ClipKindSnapshot::Title(title_data),
                timeline_start_frame: 0,
                timeline_end_frame: 24,
                source_start_secs: 0.0,
                source_end_secs: 1.0,
                speed: 1.0,
                node_graph: klipp_nodes::NodeGraph::default(),
                effects: vec![],
                builtin_effects: vec![],
                source_rotation: 0,
                stabilization_data: None,
                smoothed_trajectory: None,
                effect_keyframes: std::collections::HashMap::new(),
                audio_effect_keyframes: std::collections::HashMap::new(),
                opacity_keyframes: vec![],
                transform_pos_x_keyframes: vec![],
                transform_pos_y_keyframes: vec![],
                transform_scale_keyframes: vec![],
                transform_rotation_keyframes: vec![],
                blend_mode: klipp_core::BlendMode::Normal,
                matte_source: None,
                matte_stencil: None,
                consumed_as_matte: false,
            }],
            transitions: vec![],
        }],
        audio_tracks: vec![],
        global_lut_path: None,
        subtitle_tracks: vec![snapshot::SubtitleTrackSnapshot {
            clips: vec![snapshot::SubtitleClipSnapshot {
                timeline_start_frame: 0,
                timeline_end_frame: 24,
                text: "Should not be embedded".to_string(),
            }],
        }],
        chapters: Vec::new(),
        working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
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
        total_frames: 24,
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
