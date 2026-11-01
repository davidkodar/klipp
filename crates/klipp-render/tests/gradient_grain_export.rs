//! A dark, soft gradient stays smooth in the exported file (#2073): an
//! 8-bit H.264 encoder at a delivery setting smooths the display dither
//! out of dark, flat areas, and the gradient's own grain is what keeps
//! the steps broken up. Exported through the real job at CRF 23, decoded
//! with the ffmpeg CLI and measured on the luma plane, as a viewer's
//! player would show it. Needs the ffmpeg CLI to decode; skips without it.

use klipp_core::*;
use klipp_render::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const W: u32 = 1920;
const H: u32 = 1080;
const FPS: u32 = 30;
const FRAMES: u64 = 45;

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// The "A Little Space" background: dark teal, a dozen code values across
/// the frame, a slow drift and a faint light.
fn living(grain: f32) -> GeneratorData {
    GeneratorData::LivingGradient { a: [12, 23, 32, 255], b: [29, 60, 65, 255], angle_deg: 25.0, drift_deg_per_sec: 1.5, glow: 0.18, grain }
}

/// A still two-point gradient as dark and as shallow, left to right.
fn gradient(grain: f32) -> GeneratorData {
    GeneratorData::Gradient { top: [12, 23, 32, 255], bottom: [29, 60, 65, 255], angle_deg: 90.0, four_point: None, grain }
}

fn snapshot(data: GeneratorData) -> SequenceSnapshot {
    let tc = |f| RationalTime::from_frames(f, FPS);
    let clip = klipp_timeline::Clip {
        kind: klipp_timeline::ClipKind::Generator(data),
        source_in: tc(0),
        source_out: tc(FRAMES as i64),
        ..klipp_timeline::Clip::gap(tc(0), tc(FRAMES as i64))
    };
    let mut v = VideoTimeline::default();
    v.sequence.fps = Fps::new(FPS, 1);
    v.sequence.resolution = Resolution { width: W, height: H };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![clip];
    v.sequence.tracks.push(t);
    SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: FPS as f64, fps_num: FPS, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: FRAMES, export_range: None, video: v,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: GradeEncoding::Bt1886,
    }
}

/// 8-bit H.264 at the export's default quality: the software encoder,
/// CRF 23, the medium preset.
fn settings(out: &Path) -> ExportSettings {
    ExportSettings {
        pan_law: Default::default(),
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
        resolution: None, fps: None, crf: 23, preset: EncoderPreset::Medium, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
        bit_depth: 8, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
        alpha: false,
        mastering_peak_nits: DEFAULT_MASTERING_PEAK_NITS,
    }
}

/// What the delivered file shows of a gradient's steps.
#[derive(Debug, Clone, Copy)]
struct Banding {
    /// The widest run of equal luma along any measured row, in pixels.
    widest_run: usize,
    /// The median number of distinct luma levels on a measured row.
    levels: usize,
    /// The fine texture left on the measured rows: the standard deviation
    /// of luma about its mean over the 15 pixels around it, in code values.
    texture: f32,
    /// The file's size, in bytes.
    bytes: u64,
}

/// `data` exported and decoded; rows every 54 px of a frame in the
/// middle and the last frame, so the file's I-frame is not all that is
/// measured.
fn exported(name: &str, data: GeneratorData) -> Banding {
    let out: PathBuf = klipp_testing::scratch::scratch_file("klipp-gradient-grain-export", name);
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.clone(), settings: settings(&out), snapshot: snapshot(data),
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: Default::default(),
        analyser: None,
        total_frames: FRAMES,
    }
    .run()
    .unwrap();
    let decoded = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&out)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .unwrap();
    assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
    let frame = (W * H * 3 / 2) as usize;
    assert_eq!(decoded.stdout.len(), frame * FRAMES as usize, "{name}: 8-bit 4:2:0 frames");
    let (mut widest_run, mut levels) = (0, Vec::new());
    let (mut sq, mut n) = (0.0f64, 0usize);
    for f in [FRAMES as usize / 2, FRAMES as usize - 1] {
        let luma = &decoded.stdout[f * frame..f * frame + (W * H) as usize];
        for y in (0..H as usize).step_by(54) {
            let row = &luma[y * W as usize..(y + 1) * W as usize];
            let mut run = 1;
            for x in 1..row.len() {
                run = if row[x] == row[x - 1] { run + 1 } else { 1 };
                widest_run = widest_run.max(run);
            }
            levels.push(row.iter().collect::<std::collections::BTreeSet<_>>().len());
            for x in 7..row.len() - 7 {
                let mean = row[x - 7..=x + 7].iter().map(|&v| f64::from(v)).sum::<f64>() / 15.0;
                sq += (f64::from(row[x]) - mean).powi(2);
                n += 1;
            }
        }
    }
    levels.sort_unstable();
    let bytes = std::fs::metadata(&out).unwrap().len();
    let _ = std::fs::remove_file(&out);
    Banding { widest_run, levels: levels[levels.len() / 2], texture: (sq / n as f64).sqrt() as f32, bytes }
}

#[test]
fn a_dark_gradient_stays_smooth_through_an_8bit_export() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let grain = DEFAULT_GENERATOR_GRAIN;
    // Measured (#2073): the living gradient 512 px / 26 levels without
    // grain, 19 px / 34 with; the still gradient 72 px / 29 against
    // 25 px / 36 (the display dither holds up better on a picture that
    // does not move, but not enough).
    for (what, make, run_cut) in [("living gradient", living as fn(f32) -> GeneratorData, 8), ("gradient", gradient, 2)] {
        let off = exported(&format!("{what}-off.mp4"), make(0.0));
        let on = exported(&format!("{what}-on.mp4"), make(grain));
        eprintln!("{what}: grain 0 {off:?}, grain {grain} {on:?}");
        assert!(on.widest_run * run_cut <= off.widest_run, "{what}: the widest flat run is {} px with grain, {} without", on.widest_run, off.widest_run);
        assert!(on.levels >= off.levels + 4, "{what}: {} levels a row with grain, {} without", on.levels, off.levels);
        // Subtle: what is left in the file is about a code value of fine
        // texture. Arch Linux ARM's x264 keeps a little more of it: 2.07
        // measured for the living gradient on an M1.
        let most = if cfg!(target_arch = "aarch64") { 2.2 } else { 2.0 };
        assert!(on.texture < most, "{what}: {} code values of texture", on.texture);
    }
}

/// The sweep the default was chosen from: each amount through the same
/// export. `cargo test -p klipp-render --test all gradient_grain_sweep -- --ignored --nocapture`
#[test]
#[ignore = "measurement"]
fn gradient_grain_sweep() {
    for grain in [0.0, 1.0, 1.5, 2.0, 2.5, 3.0] {
        let b = exported(&format!("sweep-{grain}.mp4"), living(grain));
        let g = exported(&format!("sweep-g-{grain}.mp4"), gradient(grain));
        // The grain costs bits: the encoder keeps it, which is the point.
        eprintln!("grain {grain}: living {b:?} ({} kB), gradient {g:?} ({} kB)", b.bytes / 1000, g.bytes / 1000);
    }
}
