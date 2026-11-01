//! What an export does when a clip's file does not decode all the way
//! through. Frames that fail to decode fail the export, naming the
//! file, and leave no `.partial` behind; a file that simply ends before
//! the clip does holds its last frame and the export completes. It used
//! to treat both alike — a warning in the log, the last good frame held
//! to the end of the clip, and a finished-looking file with a still
//! where the motion should be.
//!
//! Needs the ffmpeg CLI to write the fixtures; skips without it.

use klipp_core::{EncoderPreset, ExportAudioCodec, ExportFormat, ExportSettings, ExportVideoCodec, Fps, HdrMode, MediaId, OdtChoice, RateControl, RationalTime, Resolution, ToneMapOp, WorkingColorspace};
use klipp_render::snapshot::{MediaFile, SequenceSnapshot, VideoTimeline};
use klipp_render::{ExportJob, RenderError};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const N: usize = 50;

fn scratch(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-broken-source", name)
}

/// 50 flat 64×64 frames at 25 fps as FFV1 in AVI: every frame is a
/// `00dc` chunk that can be found and destroyed without touching the
/// container, and FFV1 reports a destroyed frame as a decode error
/// instead of concealing it.
fn ffv1_avi() -> Option<PathBuf> {
    // Written once per process: both tests run at the same time, and one
    // re-encoding the file while the other reads it is a torn fixture.
    static RAMP: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    RAMP.get_or_init(|| {
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
            return None;
        }
        let (w, h) = (64usize, 64usize);
        let mut bytes = Vec::with_capacity(N * w * h * 3 / 2);
        for i in 0..N {
            bytes.extend(std::iter::repeat_n((16 + i * 219 / (N - 1)) as u8, w * h));
            bytes.extend(std::iter::repeat_n(128u8, w * h / 2));
        }
        let raw = scratch("ramp.yuv");
        let out = scratch("ramp.avi");
        std::fs::write(&raw, bytes).ok()?;
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", "64x64", "-r", "25", "-i"])
            .arg(&raw)
            .args(["-c:v", "ffv1"])
            .arg(&out)
            .status()
            .ok()?
            .success();
        ok.then_some(out)
    })
    .clone()
}

/// `src` with the data of every `00dc` chunk from the `from`-th on
/// overwritten, as a new file.
fn frames_destroyed_from(src: &Path, from: usize, name: &str) -> PathBuf {
    let mut bytes = std::fs::read(src).unwrap();
    let movi = bytes.windows(4).position(|w| w == b"movi").expect("an AVI movi list");
    let chunks: Vec<usize> = bytes[movi..].windows(4).enumerate().filter(|(_, w)| *w == b"00dc").map(|(i, _)| movi + i).collect();
    for chunk in chunks.into_iter().skip(from) {
        let size = u32::from_le_bytes(bytes[chunk + 4..chunk + 8].try_into().unwrap()) as usize;
        let end = (chunk + 8 + size).min(bytes.len());
        for b in &mut bytes[chunk + 8..end] {
            *b = 0xff;
        }
    }
    let out = scratch(name);
    std::fs::write(&out, bytes).unwrap();
    out
}

/// `src` cut exactly where its `n`-th video packet ends, so the demuxer
/// meets a clean end of file.
fn cut_after_packet(src: &Path, n: usize, name: &str) -> PathBuf {
    let mut input = ffmpeg_next::format::input(src).unwrap();
    let video = input.streams().best(ffmpeg_next::media::Type::Video).unwrap().index();
    let mut ends = Vec::new();
    for (stream, packet) in input.packets() {
        if stream.index() == video {
            ends.push((packet.position() as usize) + packet.size());
        }
    }
    let bytes = std::fs::read(src).unwrap();
    let out = scratch(name);
    std::fs::write(&out, &bytes[..ends[n]]).unwrap();
    out
}

/// Export a 2 s single-clip timeline of `media` to `out`.
fn export(media: &Path, out: &Path) -> Result<(), RenderError> {
    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 25);
    let clip = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(N as i64), ..klipp_timeline::Clip::gap(tc(0), tc(N as i64)) };
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
        analyser: None,
        total_frames: N as u64,
    }
    .run()
}

#[test]
fn frames_that_fail_to_decode_fail_the_export_by_name() {
    let Some(good) = ffv1_avi() else { return };
    let broken = frames_destroyed_from(&good, 10, "broken-from-10.avi");
    let out = scratch("broken.mp4");
    let _ = std::fs::remove_file(&out);

    let err = export(&broken, &out).expect_err("the export must not complete over undecodable frames");
    let RenderError::Source(msg) = &err else { panic!("wrong error: {err}") };
    assert!(msg.contains("broken-from-10.avi"), "the error names the file: {msg}");
    assert!(!out.exists(), "no output file for a failed export");
    let partial = out.with_extension("mp4.partial");
    let leftovers: Vec<_> = std::fs::read_dir(out.parent().unwrap()).unwrap().flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("broken") && n.contains("partial")).collect();
    assert!(leftovers.is_empty(), "a failed export leaves no partial behind ({partial:?}): {leftovers:?}");
}

#[test]
fn a_file_that_ends_early_holds_its_last_frame_and_the_export_completes() {
    let Some(good) = ffv1_avi() else { return };
    let short = cut_after_packet(&good, 20, "cut-after-20.avi");
    let out = scratch("short.mp4");
    let _ = std::fs::remove_file(&out);

    export(&short, &out).expect("a short file is held, not an error");
    assert!(out.exists());
}
