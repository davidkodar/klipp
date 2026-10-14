//! What a clip with temporal noise reduction costs an export at 4K
//! (#1980): a probe, not a pass/fail test. Run with
//! `cargo test -p klipp-render --test all tnr_export_cost -- --ignored --nocapture`.
//! Exports 25 frames of a 3840x2160 FFV1 clip with and without the
//! denoise and prints the time a frame and the readers opened.

use klipp_core::*;
use klipp_render::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const FRAMES: u64 = 25;

fn tmp(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-tnr-export-cost", name)
}

fn source() -> Option<PathBuf> {
    let path = tmp("uhd.mkv");
    if path.exists() {
        return Some(path);
    }
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=3840x2160:r=25:d=1.5"])
        .args(["-vf", "format=yuv420p10le", "-c:v", "ffv1"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success())
        .then_some(path)
}

fn snapshot(src: &std::path::Path, denoise: bool) -> SequenceSnapshot {
    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 25);
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: 3840, height: 2160 };
    let mut clip = klipp_timeline::Clip::media(id, tc(0), tc(FRAMES as i64), tc(2), tc(2 + FRAMES as i64));
    if denoise {
        clip = klipp_timeline::test_clip::TestClip::of(clip).effect(klipp_effects::BuiltinEffect::TemporalDenoise { strength: 0.8, threshold: 0.05 }).build();
    }
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![clip];
    video.sequence.tracks.push(t);
    video.media.insert(id, MediaFile { path: src.to_path_buf(), log_format: None, source_rotation: 0, duration_secs: Some(1.5), fps: Some((25, 1)), ..Default::default() });
    SequenceSnapshot {
        fps: 25.0, fps_num: 25, fps_den: 1, width: 3840, height: 2160, sample_rate: 48000,
        duration_frames: FRAMES, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    }
}

#[test]
#[ignore = "perf probe: run explicitly"]
fn tnr_export_cost() {
    let Some(src) = source() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    // The job's own per-frame breakdown, in its log.
    let _ = tracing_subscriber::fmt().with_env_filter("klipp_render=info").with_test_writer().try_init();
    for denoise in [false, true] {
        let out = tmp(if denoise { "tnr.mp4" } else { "plain.mp4" });
        let settings = ExportSettings { output_path: out.clone(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264, audio_codec: ExportAudioCodec::Copy, ..Default::default() };
        let t0 = std::time::Instant::now();
        ExportJob {
            id: uuid::Uuid::new_v4(), output_path: out, settings, snapshot: snapshot(&src, denoise),
            cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
            hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(),
            total_frames: FRAMES,
            analyser: None,
        }
        .run()
        .unwrap();
        let ms = t0.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;
        eprintln!("{}: {ms:.1} ms a frame over {FRAMES} frames at 3840x2160", if denoise { "temporal NR" } else { "plain" });
    }
}
