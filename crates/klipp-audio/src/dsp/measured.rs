//! The voice effects measured against what they are for (#2778): a pitch
//! shift lands on the interval asked, a normalize on its loudness, a
//! de-click takes the clicks out, a de-esser the sibilants, a noise
//! reduction the floor between words, each leaving the rest alone. The
//! older tests only checked that an effect changes something.

use super::*;
use crate::lufs::LufsMeter;

const SR: u32 = 48_000;

fn ctx() -> AudioProcessContext {
    AudioProcessContext { sample_rate: SR, channels: 2, clip_time_secs: 0.0 }
}

/// Mono samples as interleaved stereo.
fn stereo(mono: &[f32]) -> Vec<f32> {
    mono.iter().flat_map(|&s| [s, s]).collect()
}

/// The left channel.
fn left(stereo: &[f32]) -> Vec<f32> {
    stereo.iter().step_by(2).copied().collect()
}

/// `fx` with `params` run over `mono`, in blocks of 512 frames as the
/// players feed it: the left channel back.
fn run(fx: &str, params: &[(&str, f32)], mono: &[f32]) -> Vec<f32> {
    let mut e = create_effect(fx, SR).expect("an effect");
    for (k, v) in params {
        e.set_param(k, *v);
    }
    let mut buf = stereo(mono);
    for block in buf.chunks_mut(1024) {
        e.process(block, &ctx());
    }
    left(&buf)
}

fn sine(freq: f32, amp: f32, secs: f32) -> Vec<f32> {
    (0..(secs * SR as f32) as usize).map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / SR as f32).sin()).collect()
}

/// A repeatable noise, -1..1.
fn noise(n: usize, seed: u32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(2_654_435_761).max(1);
    (0..n).map(|_| {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (s as f32 / u32::MAX as f32) * 2.0 - 1.0
    }).collect()
}

/// `x` through a band-pass of `lo`..`hi` Hz (two cascaded biquads each
/// way), for band levels.
fn band(x: &[f32], lo: f32, hi: f32) -> Vec<f32> {
    let mut y = x.to_vec();
    for (kind, f) in [(biquad::FilterType::HighPass, lo), (biquad::FilterType::HighPass, lo), (biquad::FilterType::LowPass, hi), (biquad::FilterType::LowPass, hi)] {
        let mut b = biquad::BiquadFilter::new();
        b.set_params(kind, f, 0.0, 0.707, SR as f32);
        for s in y.iter_mut() {
            *s = b.process(*s);
        }
    }
    y
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

/// Integrated loudness of a mono signal heard on both channels.
fn lufs(mono: &[f32]) -> f32 {
    let mut m = LufsMeter::new(SR as f32, false);
    m.process(&stereo(mono));
    m.integrated_lufs
}

/// A tone's pitch: the median, over 40 ms windows, of the period its
/// autocorrelation peaks at (parabola-refined), as a pitch tracker reads
/// it. A shifter's crossfades modulate the level at the rate its taps
/// wrap, which puts sidebands beside the tone in a spectrum and stray
/// zero crossings in it; the period between them is the pitch heard.
fn frequency(x: &[f32]) -> f32 {
    let win = (0.04 * SR as f32) as usize;
    let (lo, hi) = ((SR / 3000) as usize, (SR / 60) as usize);
    let mut found: Vec<f32> = x.chunks(win).filter(|w| w.len() == win).filter_map(|w| {
        let r = |lag: usize| -> f32 {
            let n = win - lag;
            let (mut xy, mut xx, mut yy) = (0.0f32, 0.0f32, 0.0f32);
            for i in 0..n {
                xy += w[i] * w[i + lag];
                xx += w[i] * w[i];
                yy += w[i + lag] * w[i + lag];
            }
            xy / (xx * yy).sqrt().max(1e-12)
        };
        let rs: Vec<f32> = (lo..=hi).map(r).collect();
        // The first lag past the zero-lag lobe that comes near the best.
        let top = rs.iter().copied().fold(f32::MIN, f32::max);
        let k = (1..rs.len() - 1).find(|&k| rs[k] > 0.9 * top && rs[k] >= rs[k - 1] && rs[k] >= rs[k + 1])?;
        let (a, b, c) = (rs[k - 1], rs[k], rs[k + 1]);
        let shift = 0.5 * (a - c) / (a - 2.0 * b + c);
        Some(SR as f32 / ((lo + k) as f32 + shift))
    }).collect();
    found.sort_by(f32::total_cmp);
    found[found.len() / 2]
}

/// The 10th percentile of 50 ms RMS: the floor between words.
fn floor_db(x: &[f32]) -> f32 {
    let mut r: Vec<f32> = x.chunks(SR as usize / 20).map(rms).collect();
    r.sort_by(f32::total_cmp);
    db(r[r.len() / 10])
}

/// A speech-like voice: a 130 Hz buzz with its harmonics, in syllables of
/// 0.18 s with 0.12 s gaps, and a breath of sibilant noise (4.5–8.5 kHz)
/// at the end of every third syllable.
fn voice(secs: f32, level: f32, sibilants: f32) -> Vec<f32> {
    let n = (secs * SR as f32) as usize;
    let hiss = band(&noise(n, 7), 4500.0, 8500.0);
    (0..n).map(|i| {
        let t = i as f32 / SR as f32;
        let syll = (t / 0.3).floor() as usize;
        let at = t % 0.3;
        let on = at < 0.18;
        let env = if on { (at / 0.02).min(1.0) * ((0.18 - at) / 0.02).min(1.0) } else { 0.0 };
        let buzz: f32 = (1..12).map(|h| (2.0 * std::f32::consts::PI * 130.0 * h as f32 * t).sin() / h as f32).sum();
        let sib = if syll % 3 == 2 && (0.12..0.18).contains(&at) { hiss[i] * sibilants } else { 0.0 };
        level * (buzz * 0.3 * env) + sib
    }).collect()
}

#[test]
fn a_pitch_shift_lands_on_the_interval_asked() {
    let tone = sine(440.0, 0.5, 2.0);
    for (st, want) in [(5.0, 587.33), (-5.0, 329.63), (12.0, 880.0), (-12.0, 220.0), (7.0, 659.26)] {
        let out = run("pitch_shift", &[("semitones", st)], &tone);
        let got = frequency(&out[SR as usize / 2..]);
        assert!((got / want - 1.0).abs() < 0.01, "{st:+} semitones: {got:.1} Hz, wanted {want:.1}");
        // A shift keeps a voice's level. (A pure tone's two taps are in
        // step, so its level swings with their crossfade; a voice's are
        // not.)
        let speech = voice(3.0, 0.3, 0.02);
        let shifted = run("pitch_shift", &[("semitones", st)], &speech);
        let level = db(rms(&shifted[SR as usize / 2..])) - db(rms(&speech[SR as usize / 2..]));
        assert!(level.abs() < 1.5, "{st:+} semitones moved a voice's level {level:.1} dB");
    }
    // A voice moves as a tone does: its 130 Hz buzz a fourth up.
    let speech = voice(4.0, 0.3, 0.0);
    let up = run("pitch_shift", &[("semitones", 5.0)], &speech);
    let ratio = frequency(&up[SR as usize / 2..]) / frequency(&speech);
    assert!((ratio / 1.3348 - 1.0).abs() < 0.01, "a voice moved x{ratio:.3}, wanted x1.335");
    // The top of a voice is kept, not dulled by the reading between
    // samples: its 4.5–8.5 kHz band, shifted by a semitone, about as loud.
    let speech = voice(3.0, 0.3, 0.05);
    let out = run("pitch_shift", &[("semitones", 1.0)], &speech);
    let kept = db(rms(&band(&out, 4500.0, 8500.0))) - db(rms(&band(&speech, 4500.0, 8500.0)));
    assert!(kept > -1.5, "the top of a voice lost {:.1} dB", -kept);
}

/// Clip mode: a clip at −24 LUFS integrated, measured, lands within
/// 0.5 LU of −14 by one gain held over all of it; unmeasured, it is
/// left as it is.
#[test]
fn a_normalize_holds_one_gain_to_its_target() {
    let x = voice(10.0, 0.05, 0.02);
    let before = lufs(&x);
    let scale = 10.0_f32.powf((-24.0 - before) / 20.0);
    let x: Vec<f32> = x.iter().map(|s| s * scale).collect();
    let measured = lufs(&x);
    assert!((measured + 24.0).abs() < 0.1, "the clip is at {measured:.1}");
    let out = run("normalize", &[("target_lufs", -14.0), ("measured_lufs", measured)], &x);
    assert!((lufs(&out) + 14.0).abs() < 0.5, "it ended at {:.2} LUFS, wanted -14", lufs(&out));
    // One gain over the whole clip: a crescendo stays a crescendo.
    let gains: Vec<f32> = out.iter().zip(&x).filter(|(_, b)| b.abs() > 1e-4).map(|(a, b)| a / b).collect();
    let (lo, hi) = gains.iter().fold((f32::MAX, f32::MIN), |(l, h), g| (l.min(*g), h.max(*g)));
    assert!(hi / lo < 1.0001, "the gain moved from {lo} to {hi}");
    // Not measured yet: unity.
    let out = run("normalize", &[("target_lufs", -14.0)], &x);
    assert_eq!(out, x);
}

/// Ride mode, the leveler: a quiet tone and voice are brought toward the
/// target as they play.
#[test]
fn a_normalize_rides_a_quiet_voice_and_a_tone_toward_their_target() {
    for (what, signal) in [("tone", sine(440.0, 0.005, 16.0)), ("voice", voice(16.0, 0.05, 0.0))] {
        let before = lufs(&signal);
        let out = run("normalize", &[("target_lufs", -14.0), ("mode", 0.0)], &signal);
        let after = lufs(&out[out.len() / 2..]);
        assert!((after + 14.0).abs() < 1.5, "{what} from {before:.1} LUFS ended at {after:.1}, wanted -14");
    }
}

#[test]
fn a_declick_takes_the_clicks_out() {
    let mut x = voice(6.0, 0.4, 0.0);
    // 0.4 ms clicks at -3 dBFS every 0.37 s.
    let click = (0.0004 * SR as f32) as usize;
    let mut at = SR as usize / 10;
    while at + click < x.len() {
        for s in x[at..at + click].iter_mut() {
            *s += 0.7;
        }
        at += (0.37 * SR as f32) as usize;
    }
    let jumps = |y: &[f32]| y.windows(2).filter(|w| (w[1] - w[0]).abs() > 0.3).count();
    let before = jumps(&x);
    let out = run("declick", &[], &x);
    let after = jumps(&out);
    assert!(before > 25 && after * 10 < before, "clicks: {before} jumps before, {after} after");
    // The voice is given back as it was, once its lookahead is allowed
    // for: what is not a click is untouched.
    let latency = create_effect("declick", SR).unwrap().latency_samples() as usize;
    let clean = voice(6.0, 0.4, 0.0);
    let off = out[latency..].iter().zip(&clean).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(off < 0.35, "the voice moved by up to {off:.2} at the repairs");
    let between = out[latency..].iter().zip(&clean).filter(|(a, b)| (*a - *b).abs() > 1e-6).count();
    assert!(between < clean.len() / 50, "{between} samples changed besides the clicks");
}

#[test]
fn a_deesser_takes_down_the_sibilants_and_leaves_the_vowels() {
    // A voice at about -24 LUFS, its sibilants as loud as speech has them.
    let x = voice(6.0, 0.2, 0.06);
    let out = run("deesser", &[], &x);
    let sib = db(rms(&band(&out, 4500.0, 8500.0))) - db(rms(&band(&x, 4500.0, 8500.0)));
    let vowels = db(rms(&band(&out, 100.0, 1500.0))) - db(rms(&band(&x, 100.0, 1500.0)));
    assert!(sib < -3.0, "the sibilants moved {sib:.1} dB at the defaults, wanted at least -3");
    assert!(vowels.abs() < 0.5, "the vowels moved {vowels:.1} dB");
}

#[test]
fn a_noise_reduction_lowers_the_floor_and_not_the_voice() {
    let speech = voice(8.0, 0.3, 0.02);
    let hiss = noise(speech.len(), 3);
    let x: Vec<f32> = speech.iter().zip(&hiss).map(|(s, n)| s + n * 0.01).collect();
    let out = run("noise_reduction", &[], &x);
    let floor = floor_db(&out) - floor_db(&x);
    let level = lufs(&out) - lufs(&x);
    assert!(floor < -6.0, "the floor moved {floor:.1} dB, wanted at least -6");
    assert!(level < 0.5 && level > -2.0, "the level moved {level:+.1} LU");
    // Untouched where nothing is gated: no colour of its own.
    let clean = run("noise_reduction", &[("reduction", 0.0)], &x);
    let colour = db(rms(&clean)) - db(rms(&x));
    assert!(colour.abs() < 0.2, "with no reduction it coloured the sound by {colour:.1} dB");
}

/// The four voice effects on real recordings, reported, not asserted:
/// `KLIPP_AUDIO_REPORT_DIR=<dir>` of mono 48 kHz little-endian f32 files
/// (`ffmpeg -i in -ac 1 -ar 48000 -f f32le out.f32`), run with
/// `--nocapture`.
#[test]
fn real_voices_report() {
    let Ok(dir) = std::env::var("KLIPP_AUDIO_REPORT_DIR") else { return };
    let mut paths: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "f32")).collect();
    paths.sort();
    // The loud 10% of 50 ms windows: speech, not the floor.
    let speech_db = |x: &[f32]| {
        let mut r: Vec<f32> = x.chunks(SR as usize / 20).map(rms).collect();
        r.sort_by(f32::total_cmp);
        db(r[r.len() * 9 / 10])
    };
    // The 95th percentile of 20 ms windows of a band: its peaks.
    let peaks_db = |x: &[f32]| {
        let mut r: Vec<f32> = x.chunks(SR as usize / 50).map(rms).collect();
        r.sort_by(f32::total_cmp);
        db(r[r.len() * 95 / 100])
    };
    for p in paths {
        let bytes = std::fs::read(&p).unwrap();
        let x: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let nr = run("noise_reduction", &[], &x);
        eprintln!("{name}: noise_reduction floor {:+.1} dB, loudness {:+.1} LU, speech {:+.2} dB", floor_db(&nr) - floor_db(&x), lufs(&nr) - lufs(&x), speech_db(&nr) - speech_db(&x));
        let de = run("deesser", &[], &x);
        let sib = |y: &[f32]| peaks_db(&band(y, 4500.0, 8500.0));
        let low = |y: &[f32]| speech_db(&band(y, 100.0, 3000.0));
        eprintln!("{name}: deesser sibilant peaks {:+.1} dB, speech below 3 kHz {:+.2} dB, 4.5-8.5k overall {:+.1} dB", sib(&de) - sib(&x), low(&de) - low(&x), db(rms(&band(&de, 4500.0, 8500.0))) - db(rms(&band(&x, 4500.0, 8500.0))));
        let jumps = |y: &[f32]| y.windows(2).filter(|w| (w[1] - w[0]).abs() > 0.3).count();
        let dc = run("declick", &[], &x);
        eprintln!("{name}: declick jumps {} -> {}", jumps(&x), jumps(&dc));
        let up = run("pitch_shift", &[("semitones", 5.0)], &x[..x.len().min(SR as usize * 6)]);
        eprintln!("{name}: pitch +5 F0 {:.1} -> {:.1} Hz (x{:.3}, wanted x1.335)", frequency(&x[..x.len().min(SR as usize * 6)]), frequency(&up), frequency(&up) / frequency(&x[..x.len().min(SR as usize * 6)]));
    }
}

/// Playback feeds a chain a block at a time and export the whole clip at
/// once: each of these effects gives the same sound either way.
#[test]
fn the_voice_effects_sound_the_same_in_any_block_size() {
    let x = voice(4.0, 0.3, 0.05);
    let measured = lufs(&x);
    for (fx, params) in [
        ("pitch_shift", vec![("semitones", 5.0)]),
        ("declick", vec![]),
        ("noise_reduction", vec![]),
        ("deesser", vec![]),
        ("normalize", vec![("target_lufs", -14.0), ("measured_lufs", measured)]),
    ] {
        let by = |block: usize| {
            let mut e = create_effect(fx, SR).unwrap();
            for (k, v) in &params {
                e.set_param(k, *v);
            }
            let mut buf = stereo(&x);
            for b in buf.chunks_mut(block * 2) {
                e.process(b, &ctx());
            }
            buf
        };
        let (small, whole) = (by(256), by(x.len()));
        let off = small.iter().zip(&whole).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(off < 1e-5, "{fx}: played in blocks and exported whole differ by {off}");
    }
}
