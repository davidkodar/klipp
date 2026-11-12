//! An LV2 plug-in's settings are saved with the project and come back
//! when it is opened again (#1191): a control set on a clip's plug-in is
//! in the reopened project and in what the export plays, and playback
//! runs the reopened plug-in as the export does. Skipped where LSP's
//! delay compensator or ffmpeg is not installed.
use klipp_core::AudioEffectInstance;
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_render::snapshot::{AudioClipShape, AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot};
use klipp_render::AudioMixdown;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Sequence, Track, TrackKind};
use std::path::{Path, PathBuf};

const RATE: u32 = 48000;
const FRAMES: u64 = 100;
/// A stereo plug-in whose output gain is a plain factor, 1 by default.
const LSP: &str = "lv2:http://lsp-plug.in/plugins/lv2/comp_delay_stereo";

/// Four seconds of a 440 Hz tone, stereo, written once; `None` without
/// ffmpeg.
fn tone() -> Option<PathBuf> {
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one_writer = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let out = klipp_testing::scratch::scratch_file("klipp-plugin-reopen", "tone.wav");
    if out.exists() {
        return Some(out);
    }
    let part = out.with_extension("part.wav");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i", "sine=f=440:r=48000:d=4", "-ac", "2", "-c:a", "pcm_f32le"])
        .arg(&part)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        return None;
    }
    std::fs::rename(&part, &out).ok()?;
    Some(out)
}

fn snapshot(path: &Path, clip_effects: Vec<AudioEffectInstance>, track_effects: Vec<AudioEffectInstance>) -> SequenceSnapshot {
    let clip = AudioClipSnapshot {
        timeline_start_frame: 0,
        timeline_end_frame: FRAMES,
        source_start_secs: 0.0,
        path: path.to_path_buf(),
        audio_effects: clip_effects,
        audio_effect_keyframes: Default::default(),
        source_range: (0.0, 0.0),
        shape: AudioClipShape { gain: 1.0, clip_duration_secs: FRAMES as f64 / 25.0, ..Default::default() },
        varispeed: None,
        exact_secs: None,
    };
    SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0,
        fps_num: 25,
        fps_den: 1,
        width: 64,
        height: 64,
        sample_rate: RATE,
        duration_frames: FRAMES,
        export_range: None,
        video: Default::default(),
        audio_tracks: vec![AudioTrackSnapshot { sends: Vec::new(), volume: 1.0, pan: 0.0, muted: false, solo: false, clips: vec![clip], automation: Default::default(), effects: track_effects }],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![],
        chapters: vec![],
        working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
        grade_encoding: klipp_core::GradeEncoding::Bt1886,
    }
}

fn mixdown(snap: &SequenceSnapshot) -> Vec<f32> {
    let mut m = AudioMixdown::new(snap).unwrap();
    (0..FRAMES).flat_map(|f| m.get_frame_audio_unclamped(f)).collect()
}

/// The level of the second half, past any ramp the plug-in starts with.
fn rms(samples: &[f32]) -> f32 {
    let tail = &samples[samples.len() / 2..];
    (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt()
}

/// The clip's plug-in effects in the project saved to `dir` and opened
/// again.
fn reopened(dir: &Path, effects: Vec<AudioEffectInstance>) -> Vec<AudioEffectInstance> {
    let mut clip = TestClip::media(4.0).build();
    clip.audio_effects = effects;
    let mut track = Track::new("Music", TrackKind::Audio);
    track.clips.push(clip);
    let mut seq = Sequence::new_default();
    seq.tracks.push(track);
    save_project(dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(dir).unwrap();
    let effects = sequences[0].all_clips().find(|c| !c.audio_effects.is_empty()).map(|c| c.audio_effects.clone());
    effects.unwrap_or_default()
}

#[test]
fn a_plugins_control_comes_back_with_the_project_and_plays_as_exported() {
    if klipp_audio::lv2::instantiate_from_type_id(LSP, RATE, &[]).is_none() {
        eprintln!("skipped: LSP's delay compensator is not installed");
        return;
    }
    let Some(path) = tone() else {
        eprintln!("skipped: no ffmpeg");
        return;
    };
    let dir = std::env::temp_dir().join(format!("klipp-plugin-reopen-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let set = AudioEffectInstance { effect_type: LSP.into(), enabled: true, wet_dry: 1.0, params: vec![("g_out".into(), 0.25)] };
    let back = reopened(&dir, vec![set.clone()]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(back, vec![set], "the plug-in and its control are in the reopened project");

    // The reopened plug-in plays its control: a quarter of what it plays
    // left at its defaults.
    let default = AudioEffectInstance { params: vec![], ..back[0].clone() };
    let at_default = rms(&mixdown(&snapshot(&path, vec![default], vec![])));
    let reopened = rms(&mixdown(&snapshot(&path, back.clone(), vec![])));
    // ffmpeg's sine is an eighth of full scale: 0.088 RMS, less what the
    // mix's pan law takes.
    assert!(at_default > 0.02, "the tone plays through the plug-in at {at_default}");
    let ratio = reopened / at_default;
    assert!((ratio - 0.25).abs() < 0.01, "the reopened plug-in plays at {ratio} of its default");

    // Playback runs the reopened plug-in as the export does, in the
    // mixer's blocks.
    let exported = mixdown(&snapshot(&path, vec![], back.clone()));
    let dry = mixdown(&snapshot(&path, vec![], vec![]));
    let mut mixer = klipp_audio::AudioMixer::new(1);
    mixer.set_sample_rate(RATE);
    mixer.set_track_inserts(0, &back, 1024);
    let mut played = Vec::new();
    for block in dry.chunks(1024) {
        played.extend(mixer.mix(&[(0, block)]));
    }
    assert_eq!(played.len(), exported.len());
    let worst = played.iter().zip(&exported).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-5, "playback and export part by {worst}");
}
