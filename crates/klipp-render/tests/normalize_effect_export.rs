//! The Normalize effect in Clip mode (#2778): one gain over the whole
//! clip from its integrated loudness, so the export lands on the target
//! with the clip's dynamics kept; the export measures a clip the app has
//! not measured yet exactly as the app does, so the two play the same;
//! and a change before the effect, or to the clip's range, asks for a
//! new measurement.

use klipp_render::normalize::{measure_normalize_input, set_normalize_measured, stale_normalizes, with_normalizes_measured};
use klipp_render::snapshot::{AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot};
use klipp_render::AudioMixdown;
use std::path::PathBuf;

const FPS: u32 = 24;
const SECS: u32 = 8;

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A crescendo: a 220 Hz tone growing from about −40 to −30 dBFS. Made
/// once: tests running side by side wrote the one file over each other.
fn crescendo() -> Option<PathBuf> {
    static MADE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    MADE.get_or_init(make_crescendo).clone()
}

fn make_crescendo() -> Option<PathBuf> {
    if !ffmpeg_ok() {
        return None;
    }
    let out = klipp_testing::scratch::scratch_file("klipp-normalize-effect", "crescendo.wav");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("aevalsrc=0.01*(1+0.27*t)*sin(2*PI*220*t):s=48000:d={SECS}"))
        .args(["-ac", "2", "-c:a", "pcm_s16le"])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

fn normalize() -> klipp_core::AudioEffectInstance {
    klipp_core::AudioEffectInstance { effect_type: "normalize".into(), enabled: true, wet_dry: 1.0, params: vec![("target_lufs".into(), -14.0), ("mode".into(), 1.0)] }
}

fn clip(path: PathBuf, effects: Vec<klipp_core::AudioEffectInstance>) -> AudioClipSnapshot {
    AudioClipSnapshot {
        timeline_start_frame: 0,
        timeline_end_frame: (FPS * SECS) as u64,
        source_start_secs: 0.0,
        path,
        audio_effects: effects,
        audio_effect_keyframes: Default::default(),
        source_range: (0.0, SECS as f64),
        shape: Default::default(),
        varispeed: None,
        exact_secs: None,
    }
}

/// The clip exported: its interleaved stereo, frame by frame.
fn export(clip: AudioClipSnapshot) -> Vec<Vec<f32>> {
    let snapshot = SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: FPS as f64,
        fps_num: FPS,
        fps_den: 1,
        width: 64,
        height: 64,
        sample_rate: 48000,
        duration_frames: (FPS * SECS) as u64,
        export_range: None,
        video: Default::default(),
        audio_tracks: vec![AudioTrackSnapshot { sends: Vec::new(), volume: 1.0, pan: 0.0, muted: false, solo: false, clips: vec![clip], automation: Default::default(), effects: Vec::new() }],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![],
        chapters: vec![],
        working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
        grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    let mut mix = AudioMixdown::new(&snapshot).unwrap();
    (0..(FPS * SECS) as u64).map(|f| mix.get_frame_audio(f)).collect()
}

fn lufs(frames: &[Vec<f32>]) -> f32 {
    let mut m = klipp_audio::lufs::LufsMeter::new(48000.0, false);
    for f in frames {
        m.process(f);
    }
    m.integrated_lufs
}

fn rms(frame: &[f32]) -> f32 {
    (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt()
}

#[test]
fn a_clip_normalize_lands_on_its_target_with_one_gain() {
    let Some(path) = crescendo() else {
        eprintln!("skipping: ffmpeg unavailable");
        return;
    };
    let dry = export(clip(path.clone(), vec![]));
    let wet = export(clip(path.clone(), vec![normalize()]));
    let (before, after) = (lufs(&dry), lufs(&wet));
    assert!(before < -24.0, "the crescendo is at {before:.1} LUFS");
    assert!((after + 14.0).abs() < 0.5, "it exported at {after:.2} LUFS, wanted -14 within 0.5");
    // One gain over all of it: the crescendo stays a crescendo.
    let gains: Vec<f32> = dry.iter().zip(&wet).skip(2).take(dry.len() - 4).map(|(d, w)| 20.0 * (rms(w) / rms(d)).log10()).collect();
    let (lo, hi) = gains.iter().fold((f32::MAX, f32::MIN), |(l, h), g| (l.min(*g), h.max(*g)));
    assert!(hi - lo < 0.05, "the gain moved from {lo:+.2} to {hi:+.2} dB over the clip");
}

#[test]
fn the_export_measures_as_the_app_does() {
    let Some(path) = crescendo() else { return };
    let range = (0.0, SECS as f64);
    let kfs = Default::default();
    // The app's measurement, as its worker makes it.
    let mut measured = normalize();
    let stale = stale_normalizes(&path, range.0, range.1, std::slice::from_ref(&measured), &kfs);
    assert_eq!(stale.len(), 1, "not measured yet");
    let lufs_in = measure_normalize_input(&path, range.0, range.1, std::slice::from_ref(&measured), &kfs, 0);
    set_normalize_measured(&mut measured, lufs_in, stale[0].1);
    assert!(stale_normalizes(&path, range.0, range.1, std::slice::from_ref(&measured), &kfs).is_empty(), "measured");
    // The export's own, for a clip the app has not measured: the same.
    let by_export = with_normalizes_measured(&path, range.0, range.1, &[normalize()], &kfs);
    assert_eq!(by_export[0].params, measured.params, "the export measures exactly as the app");
    // And so a clip plays out the same, measured by either.
    assert_eq!(export(clip(path.clone(), vec![measured.clone()])), export(clip(path.clone(), vec![normalize()])));

    // A change before it, or to the clip's range, is a new measurement;
    // its own target is not.
    let gain = klipp_core::AudioEffectInstance { effect_type: "eq".into(), enabled: true, wet_dry: 1.0, params: vec![] };
    assert_eq!(stale_normalizes(&path, range.0, range.1, &[gain, measured.clone()], &kfs).len(), 1, "an effect added before it");
    assert_eq!(stale_normalizes(&path, 1.0, range.1, std::slice::from_ref(&measured), &kfs).len(), 1, "a new range");
    let mut louder = measured.clone();
    louder.params[0].1 = -16.0;
    assert!(stale_normalizes(&path, range.0, range.1, &[louder], &kfs).is_empty(), "its own target");
}
