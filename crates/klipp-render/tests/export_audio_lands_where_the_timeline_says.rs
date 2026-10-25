//! The export mixdown places audio by timeline position. Three ways it
//! used to be wrong, each pinned on a file whose sample values *are*
//! the source time — a ramp, `v = t/4 - 1`, lossless in FLAC — so a
//! test can read "which second of the source is this?" straight off a
//! sample.
//!
//! - A clip trimmed to start at 1.0 s played from the FLAC frame before
//!   it: the seek lands on a packet boundary and nothing discarded the
//!   lead-in, so the audio started early and ran early to the end.
//! - A ranged export starting mid-clip played the clip from its first
//!   sample: the read cursor began wherever the buffer began.
//! - NTSC rendered 1602 samples per frame instead of 1601.6, drifting
//!   25 ms per 100 s.
//!
//! Needs the ffmpeg CLI to write the fixture; skips without it.

use klipp_render::snapshot::{AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot};
use klipp_render::AudioMixdown;
use std::path::PathBuf;

const RATE: u32 = 48000;
const SECS: f64 = 8.0;

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// An 8 s stereo FLAC whose sample at time `t` is `t/4 - 1` on both
/// channels (stereo, so no mono upmix gain touches the value; the
/// ramp stays within ±1 to the end). FLAC frames are 4096 samples, so
/// a seek to 1.0 s lands 2944 samples early.
fn ramp() -> Option<PathBuf> {
    // Written once per process: the tests run in parallel, and one
    // re-encoding the file while another decodes it reads as corrupt
    // FLAC frames.
    static RAMP: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    RAMP.get_or_init(|| {
        if !ffmpeg_ok() {
            return None;
        }
        let out = klipp_testing::scratch::scratch_file("klipp-export-audio", "ramp.flac");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("aevalsrc=t/4-1|t/4-1:s={RATE}:d={SECS}"))
            .args(["-c:a", "flac", "-sample_fmt", "s16"])
            .arg(&out)
            .status()
            .ok()?
            .success();
        ok.then_some(out)
    })
    .clone()
}

/// The source time a sample value encodes.
fn secs_of(v: f32) -> f64 {
    (v as f64 + 1.0) * 4.0
}

fn snapshot(fps_num: u32, fps_den: u32, clip: AudioClipSnapshot) -> SequenceSnapshot {
    SequenceSnapshot {
        fps: fps_num as f64 / fps_den as f64,
        fps_num,
        fps_den,
        width: 64,
        height: 64,
        sample_rate: RATE,
        duration_frames: 10_000,
        export_range: None,
        video: Default::default(),
        audio_tracks: vec![AudioTrackSnapshot { volume: 1.0, pan: 0.0, muted: false, solo: false, clips: vec![clip], automation: Default::default() , effects: Vec::new()}],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![],
        chapters: vec![],
        working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    }
}

fn clip(path: PathBuf, start_frame: u64, end_frame: u64, source_start_secs: f64) -> AudioClipSnapshot {
    AudioClipSnapshot {
        timeline_start_frame: start_frame,
        timeline_end_frame: end_frame,
        source_start_secs,
        path,
        audio_effects: vec![],
        audio_effect_keyframes: Default::default(),
        shape: Default::default(),
        varispeed: None,
        exact_secs: None,
    }
}

/// One sample is 1/48000 s; the ramp's 16-bit quantisation is 2/65536
/// in value, 122 µs in time. Allow 0.5 ms — a wrong answer is off by
/// tens of milliseconds.
const TOL_SECS: f64 = 0.0005;

/// The source time at a clip's first frame. The mixdown ramps the first
/// two milliseconds of a clip up from silence, as playback does against
/// clicks, so the start is read a little past that and moved back.
fn start_of(frame: &[f32]) -> f64 {
    const PAST: usize = 200;
    secs_of(frame[PAST * 2]) - PAST as f64 / RATE as f64
}

#[test]
fn a_trimmed_clip_starts_at_its_in_point() {
    let Some(path) = ramp() else { return };
    let mut mix = AudioMixdown::new(&snapshot(24, 1, clip(path, 0, 96, 1.0))).unwrap();
    let f0 = mix.get_frame_audio(0);
    assert!((start_of(&f0) - 1.0).abs() < TOL_SECS, "frame 0 starts at source {:.4}s, not 1.0s", start_of(&f0));
    let f48 = mix.get_frame_audio(48);
    assert!((secs_of(f48[0]) - 3.0).abs() < TOL_SECS, "frame 48 starts at source {:.4}s, not 3.0s", secs_of(f48[0]));
}

#[test]
fn a_ranged_export_starts_mid_clip_where_the_range_starts() {
    let Some(path) = ramp() else { return };
    let mut mix = AudioMixdown::new(&snapshot(24, 1, clip(path, 0, 120, 0.0))).unwrap();
    // The first frame a ranged export asks for is the range's start.
    let f72 = mix.get_frame_audio(72);
    assert!((secs_of(f72[0]) - 3.0).abs() < TOL_SECS, "frame 72 starts at source {:.4}s, not 3.0s", secs_of(f72[0]));
    // And the frames after it follow on.
    let f73 = mix.get_frame_audio(73);
    let expected = 3.0 + 1.0 / 24.0;
    assert!((secs_of(f73[0]) - expected).abs() < TOL_SECS, "frame 73 starts at source {:.4}s, not {expected:.4}s", secs_of(f73[0]));
}

#[test]
fn a_clip_placed_later_on_the_timeline_starts_at_its_in_point() {
    let Some(path) = ramp() else { return };
    let mut mix = AudioMixdown::new(&snapshot(25, 1, clip(path, 50, 150, 2.0))).unwrap();
    assert!(mix.get_frame_audio(49).iter().all(|&s| s == 0.0), "silence before the clip");
    let f50 = mix.get_frame_audio(50);
    assert!((start_of(&f50) - 2.0).abs() < TOL_SECS, "frame 50 starts at source {:.4}s, not 2.0s", start_of(&f50));
    let f100 = mix.get_frame_audio(100);
    assert!((secs_of(f100[0]) - 4.0).abs() < TOL_SECS, "frame 100 starts at source {:.4}s, not 4.0s", secs_of(f100[0]));
}

#[test]
fn ntsc_audio_stays_with_the_picture() {
    let Some(path) = ramp() else { return };
    let mut mix = AudioMixdown::new(&snapshot(30000, 1001, clip(path, 0, 180, 0.0))).unwrap();
    // Frame 150 begins at 150 * 1001 / 30000 s. With 1602 samples per
    // frame it would begin 60 samples (1.25 ms) later in the source.
    let expected = 150.0 * 1001.0 / 30000.0;
    let mut total = 0usize;
    for f in 0..150 {
        total += mix.get_frame_audio(f).len() / 2;
    }
    let f150 = mix.get_frame_audio(150);
    assert!((secs_of(f150[0]) - expected).abs() < TOL_SECS, "frame 150 starts at source {:.5}s, not {expected:.5}s", secs_of(f150[0]));
    assert_eq!(total, (150u64 * 48000 * 1001 / 30000) as usize, "the frames before it do not add up to its start");
}

/// The same ramp as 16-bit PCM in a WAV, which is what a music bed or a
/// sound effect usually is.
fn ramp_wav() -> Option<PathBuf> {
    static RAMP: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    RAMP.get_or_init(|| {
        if !ffmpeg_ok() {
            return None;
        }
        let out = klipp_testing::scratch::scratch_file("klipp-export-audio", "ramp.wav");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("aevalsrc=t/4-1|t/4-1:s={RATE}:d={SECS}"))
            .args(["-c:a", "pcm_s16le"])
            .arg(&out)
            .status()
            .ok()?
            .success();
        ok.then_some(out)
    })
    .clone()
}

/// A WAV on the timeline is heard in the render, at the right place. A
/// PCM decoder's frames name no channel layout; the resampler took
/// each as a change of input and refused it, so every WAV rendered as
/// silence — found when a music bed of one exported as nothing at all.
#[test]
fn a_wav_is_heard_and_lands_at_its_in_point() {
    let Some(path) = ramp_wav() else { return };
    let mut mix = AudioMixdown::new(&snapshot(24, 1, clip(path, 0, 96, 1.0))).unwrap();
    let f0 = mix.get_frame_audio(0);
    assert!(f0.iter().any(|s| *s != 0.0), "the WAV rendered as silence");
    assert!((start_of(&f0) - 1.0).abs() < TOL_SECS, "frame 0 starts at source {:.4}s, not 1.0s", start_of(&f0));
    let f48 = mix.get_frame_audio(48);
    assert!((secs_of(f48[0]) - 3.0).abs() < TOL_SECS, "frame 48 starts at source {:.4}s, not 3.0s", secs_of(f48[0]));
}

/// #642: a retimed clip's sound is read at its speed, so every frame's
/// sound is the source moment its picture shows. At 2x a second of
/// timeline is two of source; reversed, the clip walks back from its out
/// point. The ramp says, sample by sample, which source moment it is.
#[test]
fn a_retimed_clip_is_heard_at_its_speed_both_ways() {
    let Some(path) = ramp() else { return };
    let read = |speed: f64, (source_in, source_out): (f64, f64)| {
        let v = klipp_render::snapshot::VarispeedRead { speed, source_in, source_out, head_secs: 0.0, keep_pitch: false };
        let frames = ((source_out - source_in) / speed.abs() * 24.0).round() as u64;
        let mut c = clip(path.clone(), 0, frames, v.decode_from());
        c.varispeed = Some(v);
        AudioMixdown::new(&snapshot(24, 1, c)).unwrap()
    };
    // 2x over source 1..5: two seconds of timeline.
    let mut fast = read(2.0, (1.0, 5.0));
    let f24 = fast.get_frame_audio(24);
    assert!((secs_of(f24[0]) - 3.0).abs() < TOL_SECS, "2x: frame 24 starts at source {:.4}s, not 3.0s", secs_of(f24[0]));
    // Within the frame the source runs at twice the rate.
    let step = secs_of(f24[200]) - secs_of(f24[0]);
    assert!((step - 2.0 * 100.0 / RATE as f64).abs() < TOL_SECS, "2x: 100 samples moved {step:.5}s of source");
    // Reversed over 1..5: frame 24 is a second back from 5.
    let mut back = read(-1.0, (1.0, 5.0));
    let b24 = back.get_frame_audio(24);
    assert!((secs_of(b24[0]) - 4.0).abs() < TOL_SECS, "reversed: frame 24 starts at source {:.4}s, not 4.0s", secs_of(b24[0]));
    assert!(secs_of(b24[200]) < secs_of(b24[0]), "reversed: the source runs backward within the frame");
    // Half speed: frame 48 (2 s) is one second into the source.
    let mut slow = read(0.5, (1.0, 3.0));
    let s48 = slow.get_frame_audio(48);
    assert!((secs_of(s48[0]) - 2.0).abs() < TOL_SECS, "0.5x: frame 48 starts at source {:.4}s, not 2.0s", secs_of(s48[0]));
}

/// #1588: a sound placed by the sample, at 4.15 s on a 24 fps timeline
/// (frame 99.6), starts on its own sample inside frame 99, silent before
/// it in that frame, and ends on its own inside its last; at 48 and at
/// 44.1 kHz. A frame grid started it at 4.1667 s.
#[test]
fn a_sound_placed_by_the_sample_starts_on_its_sample() {
    let Some(path) = ramp() else { return };
    for rate in [48_000u32, 44_100] {
        let (start, end) = (4.15, 5.15);
        let mut c = clip(path.clone(), (start * 24.0f64).floor() as u64, (end * 24.0f64).ceil() as u64, 2.0);
        c.exact_secs = Some((start, end));
        let mut snap = snapshot(24, 1, c);
        snap.sample_rate = rate;
        let mut mix = AudioMixdown::new(&snap).unwrap();
        assert!(mix.get_frame_audio(98).iter().all(|&s| s == 0.0), "{rate}: silence before the sound's frame");
        let f99 = mix.get_frame_audio(99);
        let frame_first = (99u64 * rate as u64 / 24) as usize;
        let lead = (start * rate as f64).floor() as usize - frame_first;
        assert!(f99[..lead * 2].iter().all(|&s| s == 0.0), "{rate}: silent in frame 99 before 4.15 s");
        assert!(f99[lead * 2 + 2..].iter().any(|&s| s != 0.0), "{rate}: sounding from 4.15 s");
        // Past the two-millisecond anti-click ramp it reads its source:
        // 2.0 s plus how far into it the sample is.
        let past = 200usize;
        let at = secs_of(f99[(lead + past) * 2]) - past as f64 / rate as f64;
        assert!((at - 2.0).abs() < TOL_SECS, "{rate}: starts at source {at:.5} s, not 2.0 s");
        let f123 = mix.get_frame_audio(123);
        let last_first = (123u64 * rate as u64 / 24) as usize;
        let tail = (end * rate as f64).floor() as usize - last_first;
        assert!(f123[tail * 2..].iter().all(|&s| s == 0.0), "{rate}: silent after 5.15 s");
        assert!(f123[..tail * 2].iter().any(|&s| s != 0.0), "{rate}: sounding up to 5.15 s");
    }
}
