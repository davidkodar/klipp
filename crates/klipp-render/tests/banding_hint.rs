//! The 8-bit banding suggestion (#2074): a dark, soft gradient exported
//! to an 8-bit format is told to go 10-bit or keep its grain; the same to
//! a 10-bit format isn't, and nor is ordinary footage. The footage cases
//! need the ffmpeg CLI to write their source; they skip without it.

use klipp_core::*;
use klipp_render::banding::{bands, export_hint, sample, sample_unless, CalledOff, SUGGESTION};
use klipp_render::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const W: u32 = 1920;
const H: u32 = 1080;
const FPS: u32 = 30;
const FRAMES: i64 = 45;

fn tc(f: i64) -> RationalTime {
    RationalTime::from_frames(f, FPS)
}

/// The dark teal living background of #2073, with `grain`.
fn living(grain: f32) -> GeneratorData {
    GeneratorData::LivingGradient { a: [12, 23, 32, 255], b: [29, 60, 65, 255], angle_deg: 25.0, drift_deg_per_sec: 1.5, glow: 0.18, grain }
}

fn snapshot_of(clip: klipp_timeline::Clip, media: HashMap<MediaId, MediaFile>) -> SequenceSnapshot {
    let mut v = VideoTimeline { media, ..VideoTimeline::default() };
    v.sequence.fps = Fps::new(FPS, 1);
    v.sequence.resolution = Resolution { width: W, height: H };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![clip];
    v.sequence.tracks.push(t);
    SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: FPS as f64, fps_num: FPS, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: FRAMES as u64, export_range: None, video: v,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: GradeEncoding::Bt1886,
    }
}

fn generated(data: GeneratorData) -> SequenceSnapshot {
    let clip = klipp_timeline::Clip {
        kind: klipp_timeline::ClipKind::Generator(data),
        source_in: tc(0),
        source_out: tc(FRAMES),
        ..klipp_timeline::Clip::gap(tc(0), tc(FRAMES))
    };
    snapshot_of(clip, HashMap::new())
}

/// A clip of a file `lavfi` makes, or `None` without the ffmpeg CLI.
fn footage(name: &str, lavfi: &str) -> Option<SequenceSnapshot> {
    let path: PathBuf = klipp_testing::scratch::scratch_file("klipp-banding-hint", name);
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", lavfi, "-c:v", "ffv1", "-pix_fmt", "yuv444p"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        return None;
    }
    let id = MediaId::new();
    let secs = FRAMES as f64 / FPS as f64;
    let media = HashMap::from([(id, MediaFile { path, log_format: None, source_rotation: 0, duration_secs: Some(secs), fps: Some((FPS, 1)), resolution: None, scene_transfer: None })]);
    Some(snapshot_of(klipp_timeline::Clip::media(id, tc(0), tc(FRAMES), tc(0), tc(FRAMES)), media))
}

/// A target: `codec` at `bit_depth`.
fn target(codec: ExportVideoCodec, bit_depth: u8) -> ExportSettings {
    ExportSettings { video_codec: codec, bit_depth, ..ExportSettings::default() }
}

/// A dark LivingGradient without its grain, to 8-bit H.264, H.265 or
/// DNxHD: the suggestion. The same to HEVC Main10, ProRes or H.264 at 10
/// bits: none.
#[test]
fn a_dark_gradient_without_grain_is_told_to_go_ten_bit_only_when_eight() {
    let snap = generated(living(0.0));
    for (codec, depth) in [(ExportVideoCodec::H264, 8), (ExportVideoCodec::H265, 0), (ExportVideoCodec::DNxHD115, 0)] {
        assert_eq!(export_hint(&snap, &target(codec, depth)), Some(SUGGESTION), "{codec:?} at {depth}");
    }
    for (codec, depth) in [(ExportVideoCodec::H265, 10), (ExportVideoCodec::H264, 10), (ExportVideoCodec::ProRes422, 0), (ExportVideoCodec::ProRes4444, 0)] {
        assert_eq!(export_hint(&snap, &target(codec, depth)), None, "{codec:?} at {depth}");
    }
    let found = sample(&snap).expect("it bands");
    assert!(found.levels < 20, "{found:?}");
}

/// The same gradient with the built-in grain at its default (#2073)
/// stays smooth in an 8-bit file: no suggestion. Its control is the
/// grain-0 case above.
#[test]
fn a_dark_gradient_with_its_default_grain_is_not_told() {
    assert_eq!(export_hint(&generated(living(DEFAULT_GENERATOR_GRAIN)), &target(ExportVideoCodec::H264, 8)), None);
}

/// A check called off stops before its next frame and says so, rather
/// than answering as if the frames were clean; left alone it answers.
#[test]
fn a_called_off_check_stops_and_says_so() {
    let snap = generated(living(0.0));
    let stop = std::sync::atomic::AtomicBool::new(true);
    assert_eq!(sample_unless(&snap, &stop), Err(CalledOff));
    stop.store(false, std::sync::atomic::Ordering::Release);
    assert!(matches!(sample_unless(&snap, &stop), Ok(Some(_))));
}

/// A bright, wide gradient spans enough code values not to band; the
/// dark one beside it does.
#[test]
fn a_wide_gradient_does_not_band() {
    let wide = GeneratorData::LivingGradient { a: [10, 10, 10, 255], b: [240, 220, 200, 255], angle_deg: 0.0, drift_deg_per_sec: 0.0, glow: 0.0, grain: 2.5 };
    assert_eq!(export_hint(&generated(wide), &target(ExportVideoCodec::H264, 8)), None);
}

/// Ordinary footage, with the texture a camera's picture has: no
/// suggestion. Its control: the same smooth dark ramp without the
/// texture, decoded through the same path, does band.
#[test]
fn ordinary_footage_is_not_told() {
    let d = FRAMES as f64 / FPS as f64;
    let ramp = format!("color=c=black:s=1280x720:r={FPS}:d={d},geq=lum='20+12*X/W':cb=128:cr=128");
    let (Some(smooth), Some(textured)) = (
        footage("ramp.mkv", &ramp),
        footage("footage.mkv", &format!("testsrc2=s=1280x720:r={FPS}:d={d},noise=alls=12:allf=t")),
    ) else {
        eprintln!("skipping: ffmpeg could not write the sources");
        return;
    };
    let eight = target(ExportVideoCodec::H264, 8);
    assert_eq!(export_hint(&smooth, &eight), Some(SUGGESTION), "the control: a smooth dark ramp on file bands");
    assert_eq!(export_hint(&textured, &eight), None, "footage");
}

/// The measure itself: a shallow ramp over the frame bands, a steep one,
/// a flat colour and noise don't.
#[test]
fn bands_counts_the_levels_of_a_smooth_run() {
    let (w, h) = (320usize, 8usize);
    let frame = |f: &dyn Fn(usize, usize) -> u8| -> Vec<u8> {
        (0..h).flat_map(|y| (0..w).flat_map(move |x| { let v = f(x, y); [v, v, v, 255] })).collect()
    };
    assert_eq!(bands(&frame(&|x, _| 20 + (x * 12 / w) as u8), w, h), Some(6), "twelve levels across, six over half the width");
    assert_eq!(bands(&frame(&|x, _| (x * 200 / w) as u8), w, h), None, "two hundred levels");
    assert_eq!(bands(&frame(&|x, _| (x * 50 / w) as u8), w, h), None, "twenty-five over half the width");
    assert_eq!(bands(&frame(&|x, _| (x * 36 / w) as u8), w, h), Some(18), "eighteen over half the width");
    assert_eq!(bands(&frame(&|_, _| 40), w, h), None, "flat");
    let noise = |x: usize, y: usize| 40 + ((x * 7919 + y * 104_729) % 13) as u8;
    assert_eq!(bands(&frame(&noise), w, h), None, "texture");
}

/// What the check costs: run with `--ignored --nocapture` to print it.
#[test]
#[ignore]
fn banding_sample_cost() {
    let snap = generated(living(0.0));
    let t = std::time::Instant::now();
    for _ in 0..5 {
        assert!(sample(&snap).is_some());
    }
    eprintln!("COST living gradient: {:.1} ms a check", t.elapsed().as_secs_f64() * 1000.0 / 5.0);
    let d = FRAMES as f64 / FPS as f64;
    if let Some(f) = footage("cost.mkv", &format!("testsrc2=s=1920x1080:r={FPS}:d={d},noise=alls=12:allf=t")) {
        let t = std::time::Instant::now();
        for _ in 0..5 {
            assert!(sample(&f).is_none());
        }
        eprintln!("COST 1080p footage: {:.1} ms a check", t.elapsed().as_secs_f64() * 1000.0 / 5.0);
    }
}

/// An export's wall time, alone and with a check that was running when it
/// started, called off a UI frame later as the app does: run with
/// `--ignored --nocapture` to print the medians of five runs each,
/// alternated.
#[test]
#[ignore]
fn export_time_beside_the_check() {
    let export = |snap: &SequenceSnapshot, beside: bool| -> f64 {
        let out = klipp_testing::scratch::scratch_file("klipp-banding-hint", "cost.mp4");
        let settings = ExportSettings { output_path: out.clone(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264, crf: 23, preset: EncoderPreset::Medium, audio_codec: ExportAudioCodec::Copy, bit_depth: 8, ..ExportSettings::default() };
        let job = ExportJob {
            id: uuid::Uuid::new_v4(), output_path: out.clone(), settings, snapshot: snap.clone(),
            cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
            hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: Default::default(),
            analyser: None,
            total_frames: FRAMES as u64,
        };
        // A check already running when the render starts, called off a
        // UI frame later, as the app's queue does when a job starts.
        let stop = Arc::new(AtomicBool::new(false));
        let check = beside.then(|| {
            let (s, halt) = (snap.clone(), stop.clone());
            std::thread::spawn(move || { let _ = sample_unless(&s, &halt); })
        });
        let calling_off = beside.then(|| {
            let halt = stop.clone();
            std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_millis(16)); halt.store(true, std::sync::atomic::Ordering::Release); })
        });
        let t = std::time::Instant::now();
        job.run().unwrap();
        let secs = t.elapsed().as_secs_f64();
        for h in [check, calling_off].into_iter().flatten() {
            h.join().unwrap();
        }
        let _ = std::fs::remove_file(&out);
        secs
    };
    let d = FRAMES as f64 / FPS as f64;
    let mut cases = vec![("living gradient", generated(living(0.0)))];
    if let Some(f) = footage("cost-export.mkv", &format!("testsrc2=s=1920x1080:r={FPS}:d={d},noise=alls=12:allf=t")) {
        cases.push(("1080p footage", f));
    }
    for (name, snap) in &cases {
        export(snap, false);
        let (mut alone, mut beside) = (Vec::new(), Vec::new());
        for _ in 0..5 {
            alone.push(export(snap, false));
            beside.push(export(snap, true));
        }
        let median = |v: &mut Vec<f64>| { v.sort_by(f64::total_cmp); v[v.len() / 2] * 1000.0 };
        eprintln!("COST export of {name}, {FRAMES} frames: {:.0} ms alone, {:.0} ms with a check called off as it starts", median(&mut alone), median(&mut beside));
    }
}
