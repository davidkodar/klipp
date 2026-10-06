//! An export's frames are the shared decision's (#1156): what `ExportJob`
//! encodes is what `ExportFrameMaker` makes, so anything that renders
//! through the maker (the real-project pixel check) renders exactly what an
//! export would. Needs the ffmpeg CLI to decode; skips without it.

use klipp_core::*;
use klipp_render::export_frames::{export_renderer, ExportFrame, ExportFrameMaker};
use klipp_render::*;
use klipp_timeline::GradeAccess;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const W: u32 = 320;
const H: u32 = 180;
const FRAMES: u64 = 5;

fn tmp(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-export-frames-shared", name)
}

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A W×H, 25 fps timeline of one graded, moving generator clip: a frame
/// the GPU converts to the encoder's planes when the encoder takes them.
fn snapshot() -> SequenceSnapshot {
    let tc = |f| RationalTime::from_frames(f, 25);
    let gradient = GeneratorData::LivingGradient { a: [200, 40, 30, 255], b: [20, 60, 220, 255], angle_deg: 30.0, drift_deg_per_sec: 90.0, glow: 0.4 };
    let mut clip = klipp_timeline::Clip {
        kind: klipp_timeline::ClipKind::Generator(gradient),
        source_in: tc(0),
        source_out: tc(FRAMES as i64),
        ..klipp_timeline::Clip::gap(tc(0), tc(FRAMES as i64))
    };
    clip.edit_grade(|g| {
        let c = g.ensure_corrector();
        g.nodes.iter_mut().find(|n| n.id == c).unwrap().params.exposure = Some(0.3);
    });
    let mut v = VideoTimeline::default();
    v.sequence.fps = Fps::new(25, 1);
    v.sequence.resolution = Resolution { width: W, height: H };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![clip];
    v.sequence.tracks.push(t);
    SequenceSnapshot {
        fps: 25.0, fps_num: 25, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: FRAMES, export_range: None, video: v,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: GradeEncoding::Bt1886,
    }
}

/// 10-bit H.264: always the software encoder (deterministic, on every
/// machine), and near-lossless.
fn settings(out: &Path) -> ExportSettings {
    ExportSettings {
        pan_law: Default::default(),
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
        resolution: None, fps: None, crf: 1, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false,
        bit_depth: 10, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
        mastering_peak_nits: DEFAULT_MASTERING_PEAK_NITS,
    }
}

/// The ten-bit samples of a little-endian planar 4:2:0 buffer.
fn samples(bytes: &[u8]) -> Vec<u16> {
    bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect()
}

#[test]
fn an_exports_frames_are_the_shared_decisions_frames() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let out = tmp("shared.mp4");
    let s = settings(&out);
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.clone(), settings: s.clone(), snapshot: snapshot(),
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(),
        total_frames: FRAMES,
    }
    .run()
    .unwrap();
    let decoded = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&out)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output()
        .unwrap();
    assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
    let size = (W * H * 3 / 2) as usize * 2;
    assert_eq!(decoded.stdout.len(), size * FRAMES as usize, "five 10-bit 4:2:0 frames");

    // The same frames through the shared decision, set up as the job sets
    // it up: its renderer, its encoder's answer, its frame source.
    let snap = snapshot();
    let probe = tmp("probe.mp4");
    let encoder = Encoder::open(&probe, &s, &snap).unwrap();
    assert_eq!(encoder.video_format(), ffmpeg_next::format::Pixel::YUV420P10LE, "the software encoder's planar 10-bit");
    let mut maker = ExportFrameMaker::new(&s, &snap, &encoder, "test");
    drop(encoder);
    let _ = std::fs::remove_file(&probe);
    assert!(maker.converts_on_gpu(), "a one-clip export is converted on the GPU");
    let mut renderer = export_renderer(&snap, "test");
    let mut source = ExportFrameSource::new(&snap.video.media, snap.fps, snap.working_colorspace);
    let mut worst = 0;
    for f in 0..FRAMES {
        let time = RationalTime { value: f as i64, rate: 25 };
        let ExportFrame::Planes { planes, .. } = maker.frame(&mut renderer, &snap.video.sequence, time, &mut source).unwrap() else {
            panic!("frame {f} came back on the CPU");
        };
        assert_eq!(planes.len(), size, "frame {f}: the encoder's planes");
        let job = samples(&decoded.stdout[f as usize * size..(f as usize + 1) * size]);
        let made = samples(&planes);
        let d = job.iter().zip(&made).map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs()).max().unwrap();
        worst = worst.max(d);
    }
    // CRF 1 at ten bits: measured at 2 levels of codec error. A different
    // picture is well past 6: the frame one step later on this drifting,
    // graded gradient is already 15 levels off.
    assert!(worst <= 6, "the job's frames differ from the shared decision's by up to {worst} ten-bit levels");
}
