//! A reversed clip exports its frames last to first (#617).
//!
//! The source is a grey ramp, one level per frame, so each exported
//! frame says which source frame it is. Generated with the ffmpeg CLI;
//! the test skips without it.

use klipp_core::*;
use klipp_render::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 64;

fn tmp(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-reverse-export", name)
}

/// Ten frames at 25 fps, frame n a flat grey of 20 + 20n, with B-frames
/// and one keyframe, so reading it backward means decoding out of order.
fn ramp_source() -> Option<PathBuf> {
    let path = tmp("ramp.mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color=c=black:s={W}x{H}:r=25:d=0.4,geq=lum='20+20*N':cb=128:cr=128"))
        .args(["-c:v", "libx264", "-bf", "2", "-g", "250", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(path)
}

/// The mean luma (limited range code) of every frame of `path`.
fn frame_lumas(path: &Path) -> Vec<f64> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"]).arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout.chunks_exact((W * H) as usize).map(|f| f.iter().map(|&v| v as f64).sum::<f64>() / f.len() as f64).collect()
}

fn export(clip: klipp_timeline::Clip, media: (MediaId, MediaFile), frames: u64, out: &Path) {
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: W, height: H };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![clip];
    video.sequence.tracks.push(t);
    video.media.insert(media.0, media.1);
    let snapshot = SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0, fps_num: 25, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: frames, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    let settings = ExportSettings {
        pan_law: Default::default(),
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
        resolution: None, fps: None, crf: 1, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
        bit_depth: 0, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
        mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
    };
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: Default::default(),
        analyser: None,
        total_frames: frames,
    }
    .run()
    .unwrap();
}

/// Source frames 2..8 of the ramp, reversed: the export shows 7, 6, … 2,
/// each once, at full speed; at half speed each twice.
#[test]
fn a_reversed_clip_exports_last_frame_first() {
    let Some(src) = ramp_source() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let level = |n: i64| 20.0 + 20.0 * n as f64;
    let tc = |f| RationalTime::from_frames(f, 25);
    for (speed, frames) in [(-1.0, 6u64), (-0.5, 12)] {
        let id = MediaId::new();
        let mut clip = klipp_timeline::Clip::media(id, tc(0), tc(frames as i64), tc(2), tc(8));
        clip.speed = speed;
        let out = tmp(&format!("reversed-{frames}.mp4"));
        export(clip, (id, MediaFile { path: src.clone(), log_format: None, source_rotation: 0, ..Default::default() }), frames, &out);
        let lumas = frame_lumas(&out);
        assert_eq!(lumas.len(), frames as usize);
        let per = (1.0 / speed.abs()) as usize;
        let want: Vec<i64> = (2..8).rev().flat_map(|n| std::iter::repeat_n(n, per)).collect();
        let got: Vec<i64> = lumas.iter().map(|l| ((l - 20.0) / 20.0).round() as i64).collect();
        assert_eq!(got, want, "speed {speed}: lumas {lumas:?}, want levels {:?}", want.iter().map(|n| level(*n)).collect::<Vec<_>>());
    }
}

/// #2772: reversed at half speed with its in-between frames blended or
/// followed by their motion, every exported frame is a new picture: the
/// ones between two source frames lie between their levels, as forward,
/// rather than repeating the frame before.
#[test]
fn a_reversed_slowed_clip_draws_its_in_between_frames() {
    let Some(src) = ramp_source() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let tc = |f| RationalTime::from_frames(f, 25);
    for blend_mode in [1, 2] {
        let id = MediaId::new();
        let mut clip = klipp_timeline::Clip::media(id, tc(0), tc(12), tc(2), tc(8));
        clip.speed = -0.5;
        let clip = klipp_timeline::test_clip::TestClip::of(clip).effects([klipp_effects::BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode }]).build();
        let out = tmp(&format!("reversed-blend-{blend_mode}.mp4"));
        export(clip, (id, MediaFile { path: src.clone(), log_format: None, source_rotation: 0, ..Default::default() }), 12, &out);
        let lumas = frame_lumas(&out);
        assert_eq!(lumas.len(), 12);
        // The last frame shows source frame 2, the first of the range,
        // with nothing before it to blend toward; every other step moves.
        let repeats = lumas.windows(2).filter(|w| (w[0] - w[1]).abs() < 4.0).count();
        assert!(repeats <= 1, "blend mode {blend_mode}: {repeats} repeated frames in {lumas:?}");
        assert!(lumas.windows(2).all(|w| w[1] <= w[0] + 1.0), "blend mode {blend_mode}: it walks back: {lumas:?}");
    }
}
