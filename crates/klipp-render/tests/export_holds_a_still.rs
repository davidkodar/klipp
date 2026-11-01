//! A still image on the timeline exports as its picture for the whole
//! clip, wherever the clip's in point is (#978). A PNG is one frame at
//! time 0; the export opened it as a video, sought to the clip's in point
//! and skipped every frame before it, so any in point above zero skipped
//! the only frame and the clip came out black, with the export reporting
//! success. A clip whose file gives no picture at all now fails the
//! export by name instead.
//!
//! Needs the ffmpeg CLI to write the fixtures; skips without it.

use klipp_core::{EncoderPreset, ExportAudioCodec, ExportFormat, ExportSettings, ExportVideoCodec, Fps, HdrMode, MediaId, OdtChoice, RateControl, RationalTime, Resolution, ToneMapOp, WorkingColorspace};
use klipp_render::snapshot::{MediaFile, SequenceSnapshot, VideoTimeline};
use klipp_render::{ExportJob, RenderError};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

/// Frames exported: 2 s at 25 fps.
const N: i64 = 50;

fn scratch(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-still-export", name)
}

fn have_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok()
}

/// A 64×64 PNG of one flat orange.
fn orange_png() -> Option<PathBuf> {
    static PNG: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    PNG.get_or_init(|| {
        if !have_ffmpeg() {
            return None;
        }
        let out = scratch("orange.png");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "color=c=0xE0602A:s=64x64", "-frames:v", "1"])
            .arg(&out)
            .status()
            .ok()?
            .success();
        ok.then_some(out)
    })
    .clone()
}

/// One second of flat grey as FFV1 in AVI: a video file, for a clip
/// trimmed past its end.
fn one_second_video() -> Option<PathBuf> {
    if !have_ffmpeg() {
        return None;
    }
    let out = scratch("grey-1s.avi");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "color=c=gray:s=64x64:r=25:d=1", "-c:v", "ffv1"])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

/// Export a 2 s single-clip timeline of `media` from `source_in_frames`.
fn export(media: &Path, source_in_frames: i64, out: &Path) -> Result<(), RenderError> {
    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 25);
    let clip = klipp_timeline::Clip {
        kind: klipp_timeline::ClipKind::Media { media_id: id },
        source_in: tc(source_in_frames),
        source_out: tc(source_in_frames + N),
        ..klipp_timeline::Clip::gap(tc(0), tc(N))
    };
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: 64, height: 64 };
    let mut track = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    track.clips = vec![clip];
    video.sequence.tracks.push(track);
    video.media.insert(id, MediaFile { path: media.to_path_buf(), log_format: None, source_rotation: 0, ..Default::default() });
    let snapshot = SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0, fps_num: 25, fps_den: 1, width: 64, height: 64, sample_rate: 48000,
        duration_frames: N as u64, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    let settings = ExportSettings {
        pan_law: Default::default(),
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
        resolution: None, fps: None, crf: 18, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
        bit_depth: 0, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
        alpha: false,
        mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
    };
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: Default::default(),
        total_frames: N as u64,
        analyser: None,
    }
    .run()
}

/// The exported frame at `secs`, as its mean R, G and B.
fn mean_rgb_at(file: &Path, secs: f64) -> [f64; 3] {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-ss", &format!("{secs}"), "-i"])
        .arg(file)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .expect("ffmpeg reads the export");
    assert!(!out.stdout.is_empty(), "a frame at {secs} s");
    let px = out.stdout.len() / 3;
    let mut sum = [0f64; 3];
    for p in out.stdout.chunks_exact(3) {
        for c in 0..3 {
            sum[c] += p[c] as f64;
        }
    }
    sum.map(|s| s / px as f64)
}

#[test]
fn a_still_holds_its_picture_from_any_in_point() {
    let Some(png) = orange_png() else { return };
    for (in_frames, name) in [(0, "still-from-0.mp4"), (5, "still-from-0.2s.mp4")] {
        let out = scratch(name);
        let _ = std::fs::remove_file(&out);
        export(&png, in_frames, &out).expect("a still exports");
        for secs in [0.1, 1.0, 1.9] {
            let [r, g, b] = mean_rgb_at(&out, secs);
            // 0xE0602A: red high, green middling, blue low; black is all near 16.
            assert!(r > 180.0 && (60.0..140.0).contains(&g) && b < 90.0, "{name} at {secs} s is the orange, not black: {r:.0} {g:.0} {b:.0}");
        }
    }
}

#[test]
fn a_clip_whose_file_gives_no_picture_fails_the_export_by_name() {
    let Some(video) = one_second_video() else { return };
    let out = scratch("past-the-end.mp4");
    let _ = std::fs::remove_file(&out);
    // In at 4 s of a 1 s file: nothing to show anywhere in the clip.
    let err = export(&video, 100, &out).expect_err("an export with no picture for a clip must not report success");
    let RenderError::Source(msg) = &err else { panic!("wrong error: {err}") };
    assert!(msg.contains("grey-1s.avi"), "the error names the file: {msg}");
}
