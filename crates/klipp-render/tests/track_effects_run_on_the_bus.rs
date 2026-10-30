//! A track's own effects run on the track's summed clips, before its
//! fader (#2560). A limiter on a track put its ceiling on each clip before
//! the clip's gain: a voice at +8 dB clip gain under a compressor and a
//! limiter at a -2 dB ceiling read +3.0 dBTP in the export. And a track
//! compressor over two clips that overlap acts once on their sum.
use klipp_core::AudioEffectInstance;
use klipp_render::snapshot::{AudioClipShape, AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot};
use klipp_render::AudioMixdown;
use std::path::PathBuf;

const RATE: u32 = 48000;
const FRAMES: u64 = 100;

/// Four seconds of speech-like bursts over a low tone, stereo, written
/// once; `None` without ffmpeg.
fn voice() -> Option<PathBuf> {
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one_writer = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let out = klipp_testing::scratch::scratch_file("klipp-track-bus", "voice.wav");
    if out.exists() {
        return Some(out);
    }
    let part = out.with_extension("part.wav");
    let wave = r"0.15*sin(2*PI*140*t)+if(lt(mod(t\,0.4)\,0.2)\,0.5*sin(2*PI*620*t)*sin(PI*mod(t\,0.4)/0.2)\,0)";
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("aevalsrc={wave}|{wave}:s=48000:d=4"))
        .args(["-c:a", "pcm_f32le"])
        .arg(&part)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        return None;
    }
    std::fs::rename(&part, &out).ok()?;
    Some(out)
}

fn clip(path: &std::path::Path, from: u64, to: u64, gain: f32) -> AudioClipSnapshot {
    AudioClipSnapshot {
        timeline_start_frame: from,
        timeline_end_frame: to,
        source_start_secs: 0.0,
        path: path.to_path_buf(),
        audio_effects: vec![],
        audio_effect_keyframes: Default::default(),
        source_range: (0.0, 0.0),
        shape: AudioClipShape { gain, clip_duration_secs: (to - from) as f64 / 25.0, ..Default::default() },
        varispeed: None,
        exact_secs: None,
    }
}

fn snapshot(clips: Vec<AudioClipSnapshot>, effects: Vec<AudioEffectInstance>) -> SequenceSnapshot {
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
        audio_tracks: vec![AudioTrackSnapshot { sends: Vec::new(), volume: 1.0, pan: 0.0, muted: false, solo: false, clips, automation: Default::default(), effects }],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![],
        chapters: vec![],
        working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
        grade_encoding: klipp_core::GradeEncoding::Bt1886,
    }
}

fn effect(kind: &str, params: &[(&str, f32)]) -> AudioEffectInstance {
    AudioEffectInstance { effect_type: kind.into(), enabled: true, wet_dry: 1.0, params: params.iter().map(|(n, v)| (n.to_string(), *v)).collect() }
}

fn mixdown(snap: &SequenceSnapshot) -> Vec<f32> {
    let mut m = AudioMixdown::new(snap).unwrap();
    (0..FRAMES).flat_map(|f| m.get_frame_audio_unclamped(f)).collect()
}

fn true_peak_db(samples: &[f32]) -> f32 {
    let mut tp = klipp_audio::TruePeakDetector::new(RATE);
    let peak = samples.chunks(2).map(|s| tp.process(s[0], s[1])).fold(0.0f32, f32::max);
    20.0 * peak.max(1e-9).log10()
}

/// #2560's case: the limiter's ceiling holds on what the track puts out.
#[test]
fn a_track_limiter_holds_its_ceiling_over_a_hot_clip() {
    let Some(path) = voice() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let hot = 10f32.powf(8.0 / 20.0);
    let bare = mixdown(&snapshot(vec![clip(&path, 0, FRAMES, hot)], vec![]));
    assert!(true_peak_db(&bare) > 0.0, "the clip is hot without the track's effects: {:.1} dBTP", true_peak_db(&bare));
    let chain = vec![effect("compressor", &[("threshold", -20.0), ("ratio", 3.0)]), effect("limiter", &[("threshold", -3.0), ("ceiling", -2.0)])];
    // The limiter alone where track effects used to run: on the clip,
    // ahead of its gain, which then took it over the ceiling. On the
    // track it holds.
    let limiter = vec![effect("limiter", &[("threshold", -3.0), ("ceiling", -2.0)])];
    let mut on_clip = clip(&path, 0, FRAMES, hot);
    on_clip.audio_effects = limiter.clone();
    let before = true_peak_db(&mixdown(&snapshot(vec![on_clip], vec![])));
    assert!(before > 0.0, "on the clip, ahead of its gain, the track's limiter let {before:.2} dBTP through");
    let alone = true_peak_db(&mixdown(&snapshot(vec![clip(&path, 0, FRAMES, hot)], limiter)));
    assert!(alone <= -2.0 + 0.1, "the limiter alone on the track reads {alone:.2} dBTP");
    let limited = mixdown(&snapshot(vec![clip(&path, 0, FRAMES, hot)], chain));
    let tp = true_peak_db(&limited);
    assert!(tp <= -2.0 + 0.1, "the track reads {tp:.2} dBTP over a -2 dB ceiling");
}

/// Two clips overlapping on the track: its compressor acts once on what
/// the two sum to, not on each, as a mixer's insert does.
#[test]
fn a_track_compressor_acts_once_on_the_clips_sum() {
    let Some(path) = voice() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let clips = || vec![clip(&path, 0, 70, 1.0), clip(&path, 30, FRAMES, 1.0)];
    let sum = mixdown(&snapshot(clips(), vec![]));
    let comp = effect("compressor", &[("threshold", -24.0), ("ratio", 4.0)]);
    let on_bus = mixdown(&snapshot(clips(), vec![comp.clone()]));
    // The same chain run once over the sum, frame by frame.
    let mut chain = klipp_audio::dsp::AudioEffectChain::from_instances(&[comp], RATE);
    chain.prepare(2);
    let mut want = Vec::new();
    let mut m = AudioMixdown::new(&snapshot(clips(), vec![])).unwrap();
    for f in 0..FRAMES {
        let mut block = m.get_frame_audio_unclamped(f);
        chain.process(&mut block, &klipp_audio::dsp::AudioProcessContext { sample_rate: RATE, channels: 2, clip_time_secs: 0.0 });
        want.extend(block);
    }
    assert_eq!(on_bus.len(), want.len());
    let worst = on_bus.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-5, "the track's compressor is not its sum's: {worst}");
    assert!(on_bus.iter().zip(&sum).any(|(a, b)| (a - b).abs() > 1e-3), "the compressor did nothing");
}

/// The playback mixer runs a track's effects as the export does: the same
/// summed clips through the same chain, in the mixer's blocks, before the
/// fader, come out as the export's track.
#[test]
fn playback_runs_a_tracks_effects_as_the_export_does() {
    let Some(path) = voice() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let clips = || vec![clip(&path, 0, 70, 1.0), clip(&path, 30, FRAMES, 1.0)];
    let chain = vec![effect("compressor", &[("threshold", -24.0), ("ratio", 4.0)]), effect("limiter", &[("threshold", -6.0), ("ceiling", -4.0)])];
    let exported = mixdown(&snapshot(clips(), chain.clone()));
    let sum = mixdown(&snapshot(clips(), vec![]));
    let mut mixer = klipp_audio::AudioMixer::new(1);
    mixer.set_sample_rate(RATE);
    mixer.set_track_inserts(0, &chain, 1024);
    let mut played = Vec::new();
    for block in sum.chunks(1024) {
        played.extend(mixer.mix(&[(0, block)]));
    }
    assert_eq!(played.len(), exported.len());
    let worst = played.iter().zip(&exported).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-5, "playback and export part by {worst}");
}
