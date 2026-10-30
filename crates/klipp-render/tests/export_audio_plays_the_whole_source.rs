//! The export mixdown plays a clip's whole source whatever its rate and
//! channel count (#1564). A 24 kHz mono narration take exported as its
//! first half and then silence: each resampled frame gave back only as
//! many samples as went in, and the resampler kept the rest. Viewer
//! playback, which sizes its own frames, played the whole line.
//!
//! Each case is a 1 kHz tone; the export must stay up over every frame of
//! the clip, at the tone's own pitch, for a whole file and for clips
//! trimmed to a range inside one (a music bed spliced from one track).
//!
//! Needs the ffmpeg CLI to write the fixtures; skips without it.

use klipp_render::snapshot::{AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot};
use klipp_render::AudioMixdown;
use std::path::PathBuf;

const FPS: u32 = 24;
const SECS: u32 = 3;
const FRAMES: u64 = (FPS * SECS) as u64;

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A `secs` long 1 kHz tone WAV at `rate` with `channels` channels.
fn tone(rate: u32, channels: u32, secs: u32) -> Option<PathBuf> {
    if !ffmpeg_ok() {
        return None;
    }
    let out = klipp_testing::scratch::scratch_file("klipp-export-whole-source", &format!("tone-{rate}-{channels}ch-{secs}s.wav"));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=1000:sample_rate={rate}:duration={secs}"))
        .args(["-ac", &channels.to_string(), "-c:a", "pcm_s16le"])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

/// A clip of `path` at `start..end` on the timeline (frames), from
/// `source_start_secs` in the file.
fn clip(path: PathBuf, start: u64, end: u64, source_start_secs: f64) -> AudioClipSnapshot {
    AudioClipSnapshot {
        timeline_start_frame: start,
        timeline_end_frame: end,
        source_start_secs,
        path,
        audio_effects: vec![],
        audio_effect_keyframes: Default::default(),
        source_range: (0.0, 0.0),
        shape: Default::default(),
        varispeed: None,
        exact_secs: None,
    }
}

fn mixdown(clips: Vec<AudioClipSnapshot>) -> AudioMixdown {
    let end = clips.iter().map(|c| c.timeline_end_frame).max().unwrap_or(0);
    let snapshot = SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: FPS as f64,
        fps_num: FPS,
        fps_den: 1,
        width: 64,
        height: 64,
        sample_rate: 48000,
        duration_frames: end + 24,
        export_range: None,
        video: Default::default(),
        audio_tracks: vec![AudioTrackSnapshot { sends: Vec::new(), volume: 1.0, pan: 0.0, muted: false, solo: false, clips, automation: Default::default() , effects: Vec::new()}],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![],
        chapters: vec![],
        working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
        grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    AudioMixdown::new(&snapshot).unwrap()
}

/// The left channel's RMS over one frame of interleaved stereo.
fn rms(frame: &[f32]) -> f32 {
    let left: Vec<f32> = frame.iter().step_by(2).copied().collect();
    (left.iter().map(|s| s * s).sum::<f32>() / left.len().max(1) as f32).sqrt()
}

/// Upward zero crossings of the left channel: a 1 kHz tone gives
/// 1000 / 24 ≈ 41.7 a frame.
fn cycles(frame: &[f32]) -> usize {
    let left: Vec<f32> = frame.iter().step_by(2).copied().collect();
    left.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
}

/// Every frame of `frames`, a clip's first and last aside (the mixdown
/// ramps a clip's edges against clicks), is as loud as the clip's
/// middle and at the tone's pitch.
fn stays_up(mix: &mut AudioMixdown, frames: std::ops::Range<u64>, what: &str) {
    // Read in order, as an export does.
    let read: Vec<Vec<f32>> = frames.clone().map(|f| mix.get_frame_audio(f)).collect();
    let middle = rms(&read[read.len() / 2]);
    assert!(middle > 0.01, "{what}: the clip's middle is silent (RMS {middle})");
    for (i, frame) in read.iter().enumerate().take(read.len() - 1).skip(1) {
        let level = rms(frame);
        assert!(
            level > middle * 0.9,
            "{what}: frame {} ({:.2} s into the clip) has RMS {level:.4}, the middle {middle:.4}",
            frames.start + i as u64,
            i as f64 / FPS as f64
        );
        let n = cycles(frame);
        assert!((40..=43).contains(&n), "{what}: frame {} holds {n} cycles of the 1 kHz tone, not about 42", frames.start + i as u64);
    }
}

/// A whole 3 s file as one clip.
fn plays_whole(rate: u32, channels: u32) {
    let Some(path) = tone(rate, channels, SECS) else { return };
    let mut mix = mixdown(vec![clip(path, 0, FRAMES, 0.0)]);
    stays_up(&mut mix, 0..FRAMES, &format!("{rate} Hz {channels} ch"));
}

/// Two 3 s clips of one 9 s file back to back: source 5.9-8.9 s, then 1-4 s.
/// Decoded from 5.9 s and short by the rate ratio, the first stopped early:
/// at 7.45 s from 24 kHz, at 8.65 s from 44.1 kHz.
fn plays_trimmed_ranges(rate: u32, channels: u32) {
    let Some(path) = tone(rate, channels, 9) else { return };
    let mut mix = mixdown(vec![clip(path.clone(), 0, FRAMES, 5.9), clip(path, FRAMES, 2 * FRAMES, 1.0)]);
    stays_up(&mut mix, 0..FRAMES, &format!("{rate} Hz {channels} ch, source 5.9-8.9 s"));
    stays_up(&mut mix, FRAMES..2 * FRAMES, &format!("{rate} Hz {channels} ch, source 1-4 s"));
}

#[test]
fn a_24khz_mono_clip_plays_to_its_end() {
    plays_whole(24000, 1);
}

#[test]
fn a_48khz_mono_clip_plays_to_its_end() {
    plays_whole(48000, 1);
}

#[test]
fn a_44100hz_stereo_clip_plays_to_its_end() {
    plays_whole(44100, 2);
}

#[test]
fn a_16khz_mono_clip_plays_to_its_end() {
    plays_whole(16000, 1);
}

#[test]
fn a_24khz_mono_clip_trimmed_mid_file_plays_its_whole_range() {
    plays_trimmed_ranges(24000, 1);
}

#[test]
fn a_44100hz_stereo_clip_trimmed_mid_file_plays_its_whole_range() {
    plays_trimmed_ranges(44100, 2);
}

/// #1594: a clip's decode stops where it stops playing. A ranged export
/// that starts mid-clip still plays the clip to its end: the decode is
/// bounded by the clip, not by where reading began.
#[test]
fn a_clip_read_from_its_middle_plays_to_its_end() {
    let Some(path) = tone(48000, 2, 9) else { return };
    let mut mix = mixdown(vec![clip(path, 0, FRAMES, 5.9)]);
    stays_up(&mut mix, FRAMES / 2..FRAMES, "48 kHz stereo, read from the clip's middle");
}

/// #1594: a clip at 2x with its pitch kept reads twice its length of
/// source, and the stretcher a window ahead of that: its last frames
/// still sound.
#[test]
fn a_kept_pitch_double_speed_clip_sounds_to_its_end() {
    // Mono: each test's tone is a file of its own, named by its rate,
    // channels and length; two tests sharing one rewrote it under each
    // other.
    let Some(path) = tone(48000, 1, 9) else { return };
    let varispeed = klipp_render::snapshot::VarispeedRead { speed: 2.0, source_in: 1.0, source_out: 7.0, head_secs: 0.0, keep_pitch: true };
    let mut fast = clip(path, 0, FRAMES, varispeed.decode_from());
    fast.varispeed = Some(varispeed);
    let mut mix = mixdown(vec![fast]);
    let read: Vec<Vec<f32>> = (0..FRAMES).map(|f| mix.get_frame_audio(f)).collect();
    let middle = rms(&read[read.len() / 2]);
    assert!(middle > 0.01, "the clip's middle is silent (RMS {middle})");
    // The last frame ramps against a click; the one before it is whole.
    let late = rms(&read[read.len() - 2]);
    assert!(late > middle * 0.5, "the clip's second-to-last frame has RMS {late:.4}, its middle {middle:.4}");
}
