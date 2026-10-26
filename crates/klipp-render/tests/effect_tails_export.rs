//! A clip's reverb and delay ring on past its out point (#2780): into
//! the silence after it, for as long as their tail, and not from what the
//! file holds past the out point. A ranged export that starts inside the
//! tail has it too.

use klipp_render::snapshot::{AudioClipShape, AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot};
use klipp_render::AudioMixdown;
use std::path::PathBuf;

const FPS: u32 = 25;
/// The clip is the file's first second; the sequence runs on in silence.
const CLIP_FRAMES: u64 = FPS as u64;
const SEQ_FRAMES: u64 = FPS as u64 * 6;

/// Three seconds of a steady 440 Hz tone at −12 dBFS: what sounds past
/// the clip's out point is the clip's effects or a leak of the file.
fn tone() -> Option<PathBuf> {
    let ok = std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success());
    if !ok {
        return None;
    }
    let out = klipp_testing::scratch::scratch_file("klipp-effect-tails", "tone.wav");
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i", "aevalsrc=0.25*sin(2*PI*440*t):s=48000:d=3", "-ac", "2", "-c:a", "pcm_f32le"])
        .arg(&out)
        .status()
        .ok()?
        .success()
        .then_some(out)
}

fn effect(kind: &str) -> klipp_core::AudioEffectInstance {
    klipp_core::AudioEffectInstance { effect_type: kind.into(), enabled: true, wet_dry: 1.0, params: vec![] }
}

fn clip(path: PathBuf, effects: Vec<klipp_core::AudioEffectInstance>, fade_out_secs: f32) -> AudioClipSnapshot {
    AudioClipSnapshot {
        timeline_start_frame: 0,
        timeline_end_frame: CLIP_FRAMES,
        source_start_secs: 0.0,
        path,
        audio_effects: effects,
        audio_effect_keyframes: Default::default(),
        source_range: (0.0, 1.0),
        shape: AudioClipShape { clip_duration_secs: 1.0, fade_out_secs, ..Default::default() },
        varispeed: None,
        exact_secs: None,
    }
}

/// The sequence exported from `from`: its interleaved stereo, frame by frame.
fn export(clip: AudioClipSnapshot, from: u64) -> Vec<Vec<f32>> {
    let snapshot = SequenceSnapshot {
        fps: FPS as f64,
        fps_num: FPS,
        fps_den: 1,
        width: 64,
        height: 64,
        sample_rate: 48000,
        duration_frames: SEQ_FRAMES,
        export_range: None,
        video: Default::default(),
        audio_tracks: vec![AudioTrackSnapshot { volume: 1.0, pan: 0.0, muted: false, solo: false, clips: vec![clip], automation: Default::default(), effects: Vec::new() }],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![],
        chapters: vec![],
        working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
        grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    let mut mix = AudioMixdown::new(&snapshot).unwrap();
    (from..SEQ_FRAMES).map(|f| mix.get_frame_audio(f)).collect()
}

fn db(frame: &[f32]) -> f32 {
    let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt();
    20.0 * rms.max(1e-9).log10()
}

/// Measured: the level of the frames after the out point, against the
/// tone's −15 dB RMS in the clip.
#[test]
fn a_reverb_and_a_delay_ring_past_the_out_point() {
    let Some(path) = tone() else {
        eprintln!("skipping: ffmpeg unavailable");
        return;
    };
    // Without effects the sound stops at the out point: nothing of the
    // file past it leaks into the gap.
    let dry = export(clip(path.clone(), vec![], 0.0), 0);
    assert!(db(&dry[CLIP_FRAMES as usize - 2]) > -20.0, "the clip sounds: {:.1} dB", db(&dry[CLIP_FRAMES as usize - 2]));
    assert!(dry[CLIP_FRAMES as usize..].iter().all(|f| db(f) < -150.0), "silence after a dry clip");

    for kind in ["reverb", "delay"] {
        let wet = export(clip(path.clone(), vec![effect(kind)], 0.0), 0);
        let after: Vec<f32> = wet[CLIP_FRAMES as usize..].iter().map(|f| db(f)).collect();
        // Rings on, falling away; and done before the sequence ends,
        // its defaults' tails being about two seconds.
        let first_half_sec = after[..FPS as usize / 2].iter().cloned().fold(f32::MIN, f32::max);
        assert!(first_half_sec > -40.0, "{kind} rings past the out point: loudest {first_half_sec:.1} dB in the first half second");
        let late = after[FPS as usize * 4..].iter().cloned().fold(f32::MIN, f32::max);
        assert!(late < -100.0, "{kind} has rung out four seconds on: {late:.1} dB");
        // A ranged export from inside the tail has the same tail.
        let from = CLIP_FRAMES + FPS as u64 / 5;
        let ranged = export(clip(path.clone(), vec![effect(kind)], 0.0), from);
        let full = &wet[from as usize..];
        let worst = ranged.iter().zip(full).flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| (x - y).abs())).fold(0.0f32, f32::max);
        assert!(worst < 1e-6, "{kind}: a ranged export from inside the tail differs by {worst}");
    }
}

/// Measured: a fade-out on the clip goes in before its reverb, so the
/// reverb still rings after a clip that fades to silence.
#[test]
fn a_fade_out_lets_the_reverb_ring() {
    let Some(path) = tone() else { return };
    let faded = export(clip(path.clone(), vec![effect("reverb")], 0.5), 0);
    let at_out = db(&faded[CLIP_FRAMES as usize - 1]);
    let after = db(&faded[CLIP_FRAMES as usize + 2]);
    assert!(after > -60.0, "the reverb rings on after the fade: {after:.1} dB just past the out point");
    assert!(after > at_out - 30.0, "and is not cut with the fade: {after:.1} dB against {at_out:.1} dB at the out point");
}
