//! An export whose encoder takes no Y'CbCr planes made on the GPU (ProRes)
//! or whose picture is letterboxed composites every frame on the GPU stack
//! all the same (#2488): it went the CPU composition, effects downloaded
//! per layer and generators rastered on the CPU, where the 4:2:0 deliveries
//! and playback had none. Needs ffmpeg to make the source; a missing GPU
//! is unverified, and fails with `KLIPP_TEST_REQUIRE_GPU=1`.

use klipp_core::*;
use klipp_effects::BuiltinEffect as E;
use klipp_render::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const N: i64 = 6;

fn source(dir: &Path) -> Option<PathBuf> {
    let out = dir.join("narrow.mp4");
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=240x180:r=24:d=0.5", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&out)
        .status()
        .ok()?
        .success()
        .then_some(out)
}

/// The export's timings for a 320x180 timeline of `media` under the
/// effects the issue named and a moving generator over it.
fn exported(media: &Path, out: &Path, codec: ExportVideoCodec, format: ExportFormat, resolution: Option<Resolution>) -> ExportTimings {
    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 24);
    let clip = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(N), ..klipp_timeline::Clip::gap(tc(0), tc(N)) };
    let clip = klipp_timeline::test_clip::TestClip::of(clip)
        .effects([
            E::FillBars { blur: 0.07, zoom: 1.0, darken: 0.25 },
            E::Vignette { amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.7 },
            E::Sharpen { amount: 0.4 },
            E::FilmGrain { amount: 0.2, size: 1.0, color: 0, shadows: 0.3, seed: 1 },
        ])
        .build();
    let gradient = GeneratorData::LivingGradient { a: [12, 23, 32, 120], b: [29, 60, 65, 120], angle_deg: 25.0, drift_deg_per_sec: 1.5, glow: 0.18, grain: 0.0 };
    let over = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Generator(gradient), source_in: tc(0), source_out: tc(N), ..klipp_timeline::Clip::gap(tc(0), tc(N)) };
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(24, 1);
    video.sequence.resolution = Resolution { width: 320, height: 180 };
    let mut top = klipp_timeline::Track::new("G", klipp_timeline::TrackKind::Video);
    top.clips = vec![over];
    let mut base = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    base.clips = vec![clip];
    video.sequence.tracks = vec![top, base];
    video.media.insert(id, MediaFile { path: media.to_path_buf(), log_format: None, source_rotation: 0, ..Default::default() });
    let snapshot = SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 24.0, fps_num: 24, fps_den: 1, width: 320, height: 180, sample_rate: 48000,
        duration_frames: N as u64, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: GradeEncoding::Bt1886,
    };
    let settings = ExportSettings { output_path: out.to_path_buf(), format, video_codec: codec, audio_codec: ExportAudioCodec::Copy, preset: EncoderPreset::Fast, burn_in_subtitles: false, resolution, ..Default::default() };
    let timings: Arc<std::sync::Mutex<Option<ExportTimings>>> = Default::default();
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: timings.clone(),
        total_frames: N as u64, analyser: None,
    }
    .run()
    .unwrap();
    let t = timings.lock().unwrap().clone();
    t.expect("the export's timings")
}

#[test]
fn an_export_without_gpu_planes_composites_on_the_gpu() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-export-gpu-linear");
    let Some(media) = source(&dir) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    for (what, codec, format, out, resolution) in [
        ("ProRes", ExportVideoCodec::ProRes422, ExportFormat::Mov, "prores.mov", None),
        ("a letterboxed H.264", ExportVideoCodec::H264, ExportFormat::Mp4, "boxed.mp4", Some(Resolution { width: 180, height: 180 })),
    ] {
        let t = exported(&media, &dir.join(out), codec, format, resolution);
        if t.gpu_composited == 0 && !required {
            eprintln!("UNVERIFIED: no frame was composited on the GPU ({:?})", t.fallbacks);
            return;
        }
        assert_eq!((t.gpu_composited, t.cpu_composited), (N as u64, 0), "{what}: every frame on the GPU stack: {:?}", t.fallbacks);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
