//! Measured sweep of the audio effects: each one, at its defaults or the
//! setting named, on a signal it is for, measured to do what it says in
//! the direction it says. Report: `target/sweep/audio.txt`.

use klipp_audio::dsp::{available_effects, create_effect, AudioProcessContext};
use klipp_audio::LufsMeter;
use klipp_testing::sweep::Sweep;
use std::f32::consts::PI;

const SR: u32 = 48_000;

/// Items known not to do what they say, and the issue that holds each.
/// Issue 0: found by the sweep, not filed yet.
const KNOWN: &[(&str, u32)] = &[("hum_removal 250 Hz", 2779), ("chorus keeps the level", 0)];

/// Interleaved stereo of `f(t)` for `secs`.
fn signal(secs: f32, f: impl Fn(f32) -> f32) -> Vec<f32> {
    (0..(secs * SR as f32) as usize).flat_map(|i| {
        let v = f(i as f32 / SR as f32);
        [v, v]
    }).collect()
}

fn sine(hz: f32, amp: f32) -> impl Fn(f32) -> f32 {
    move |t| amp * (2.0 * PI * hz * t).sin()
}

/// White noise from a fixed seed, at `amp` peak.
fn noise(amp: f32) -> impl Fn(f32) -> f32 {
    move |t| {
        let mut x = (t * SR as f32) as u32 ^ 0x9E37_79B9;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        amp * ((x % 20_001) as f32 / 10_000.0 - 1.0)
    }
}

/// `params` on a fresh `kind`, run over `input`.
fn run(kind: &str, params: &[(&str, f32)], input: &[f32]) -> Vec<f32> {
    let mut fx = create_effect(kind, SR).unwrap_or_else(|| panic!("no effect {kind}"));
    for (k, v) in params {
        fx.set_param(k, *v);
    }
    let mut out = input.to_vec();
    let ctx = AudioProcessContext { sample_rate: SR, channels: 2, clip_time_secs: 0.0 };
    for block in out.chunks_mut(2048 * 2) {
        fx.process(block, &ctx);
    }
    out
}

/// The level of `hz` in the left channel past the first quarter second
/// (effects settle), in dB of a full-scale sine.
fn level_at(buf: &[f32], hz: f32) -> f32 {
    let left: Vec<f32> = buf.chunks(2).skip(SR as usize / 4).map(|p| p[0]).collect();
    let w = 2.0 * PI * hz / SR as f32;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, s) in left.iter().enumerate() {
        re += (*s * (w * i as f32).cos()) as f64;
        im += (*s * (w * i as f32).sin()) as f64;
    }
    let amp = 2.0 * (re * re + im * im).sqrt() / left.len().max(1) as f64;
    20.0 * amp.max(1e-12).log10() as f32
}

fn rms_db(buf: &[f32]) -> f32 {
    let rms = (buf.iter().map(|s| s * s).sum::<f32>() / buf.len().max(1) as f32).sqrt();
    20.0 * rms.max(1e-9).log10()
}

fn peak_db(buf: &[f32]) -> f32 {
    20.0 * buf.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-9).log10()
}

fn lufs(buf: &[f32]) -> f32 {
    let mut m = LufsMeter::new(SR as f32, false);
    m.process(buf);
    m.readings().integrated
}

/// Frames `a..b` seconds of an interleaved buffer.
fn span(buf: &[f32], a: f32, b: f32) -> &[f32] {
    let at = |s: f32| ((s * SR as f32) as usize * 2).min(buf.len());
    &buf[at(a)..at(b)]
}

#[test]
fn sweep_audio() {
    let mut sweep = Sweep::new("audio", KNOWN);
    let mut seen: Vec<&str> = Vec::new();
    let mut check = |sweep: &mut Sweep, kind: &'static str, item: &str, measured: String, pass: bool| {
        seen.push(kind);
        sweep.check(item, measured, pass);
    };

    // EQ: band 2 (250 Hz) +9 dB lifts a 250 Hz tone; switched off, nothing.
    let tone250 = signal(1.0, sine(250.0, 0.1));
    let lift = level_at(&run("eq", &[("band2_gain", 9.0)], &tone250), 250.0) - level_at(&tone250, 250.0);
    check(&mut sweep, "eq", "eq band2 +9 dB at 250 Hz", format!("{lift:+.1} dB"), (6.0..=10.0).contains(&lift));
    let off = level_at(&run("eq", &[("band2_gain", 9.0), ("band2_enabled", 0.0)], &tone250), 250.0) - level_at(&tone250, 250.0);
    check(&mut sweep, "eq", "eq band2 switched off", format!("{off:+.2} dB"), off.abs() < 0.5);

    // Compressor at its defaults (-18 dB, 4:1) turns a -6 dBFS tone down.
    let loud = signal(1.0, sine(440.0, 0.5));
    let comp = rms_db(span(&run("compressor", &[], &loud), 0.5, 1.0)) - rms_db(span(&loud, 0.5, 1.0));
    check(&mut sweep, "compressor", "compressor on a -6 dBFS tone", format!("{comp:+.1} dB"), (-10.0..=-4.0).contains(&comp));

    // Limiter holds a +3 dBFS tone under its ceiling (-0.3 dB).
    let hot = signal(1.0, sine(440.0, 1.41));
    let peak = peak_db(span(&run("limiter", &[], &hot), 0.2, 1.0));
    check(&mut sweep, "limiter", "limiter ceiling -0.3 dB", format!("peak {peak:+.2} dBFS"), peak <= 0.0);

    // Gate: quiet noise to near silence, a loud tone through.
    let hiss = signal(1.0, noise(0.001));
    let gated = rms_db(span(&run("gate", &[], &hiss), 0.3, 1.0)) - rms_db(span(&hiss, 0.3, 1.0));
    check(&mut sweep, "gate", "gate closes on -60 dB noise", format!("{gated:+.1} dB"), gated < -30.0);
    let open = rms_db(span(&run("gate", &[], &loud), 0.3, 1.0)) - rms_db(span(&loud, 0.3, 1.0));
    check(&mut sweep, "gate", "gate passes a -6 dBFS tone", format!("{open:+.2} dB"), open.abs() < 1.0);

    // High-pass at 300 Hz: 100 Hz down, 2 kHz kept.
    let low = signal(1.0, sine(100.0, 0.1));
    let high = signal(1.0, sine(2000.0, 0.1));
    let cut = level_at(&run("high_pass", &[("frequency", 300.0)], &low), 100.0) - level_at(&low, 100.0);
    let kept = level_at(&run("high_pass", &[("frequency", 300.0)], &high), 2000.0) - level_at(&high, 2000.0);
    check(&mut sweep, "high_pass", "high_pass 300 Hz cuts 100 Hz", format!("{cut:+.1} dB"), cut < -6.0);
    check(&mut sweep, "high_pass", "high_pass 300 Hz keeps 2 kHz", format!("{kept:+.2} dB"), kept.abs() < 1.0);

    // Denoiser: a steady hiss floor comes down.
    let floor = signal(2.0, noise(0.03));
    let den = rms_db(span(&run("denoiser", &[], &floor), 1.0, 2.0)) - rms_db(span(&floor, 1.0, 2.0));
    check(&mut sweep, "denoiser", "denoiser on steady hiss", format!("{den:+.1} dB"), den < -4.0);

    // De-esser: a loud 6.5 kHz "s" over a 300 Hz voice comes down, the voice stays.
    let ess = signal(1.0, |t| sine(300.0, 0.1)(t) + sine(6500.0, 0.2)(t));
    let deessed = run("deesser", &[], &ess);
    let s_down = level_at(&deessed, 6500.0) - level_at(&ess, 6500.0);
    let v_kept = level_at(&deessed, 300.0) - level_at(&ess, 300.0);
    check(&mut sweep, "deesser", "deesser takes the s down", format!("{s_down:+.1} dB"), s_down < -3.0);
    check(&mut sweep, "deesser", "deesser keeps the voice", format!("{v_kept:+.2} dB"), v_kept.abs() < 1.0);

    // Hum removal at 50 Hz: the hum and its harmonics come down, a voice tone stays.
    let hum = signal(1.0, |t| sine(50.0, 0.1)(t) + sine(150.0, 0.1)(t) + sine(250.0, 0.1)(t) + sine(1000.0, 0.1)(t));
    let dehummed = run("hum_removal", &[("fundamental_hz", 50.0)], &hum);
    for hz in [50.0, 150.0, 250.0] {
        let d = level_at(&dehummed, hz) - level_at(&hum, hz);
        check(&mut sweep, "hum_removal", &format!("hum_removal {hz} Hz"), format!("{d:+.1} dB"), d < -10.0);
    }
    let voice = level_at(&dehummed, 1000.0) - level_at(&hum, 1000.0);
    check(&mut sweep, "hum_removal", "hum_removal keeps 1 kHz", format!("{voice:+.2} dB"), voice.abs() < 1.0);

    // Noise reduction: the floor between words down, the whole not louder.
    let words = signal(3.0, |t| noise(0.01)(t) + if (t * 2.0).fract() < 0.5 { sine(300.0, 0.2)(t) } else { 0.0 });
    let reduced = run("noise_reduction", &[], &words);
    let gap = |b: &[f32]| rms_db(span(b, 2.78, 2.98));
    let floor_down = gap(&reduced) - gap(&words);
    let louder = lufs(&reduced) - lufs(&words);
    check(&mut sweep, "noise_reduction", "noise_reduction floor between words", format!("{floor_down:+.1} dB"), floor_down < -4.0);
    check(&mut sweep, "noise_reduction", "noise_reduction not louder", format!("{louder:+.2} LU"), louder < 0.5);

    // Declick: clicks on a tone are bridged.
    let clicky = signal(2.0, |t| sine(300.0, 0.2)(t) + if (t * SR as f32) as u32 % 17_760 < 20 { 0.7 } else { 0.0 });
    let jumps = |b: &[f32]| b.chunks(2).collect::<Vec<_>>().windows(2).filter(|w| (w[1][0] - w[0][0]).abs() > 0.3).count();
    let (before, after) = (jumps(&clicky), jumps(&run("declick", &[], &clicky)));
    check(&mut sweep, "declick", "declick bridges clicks", format!("{before} jumps to {after}"), after * 5 <= before);

    // Normalize: ride (mode 0) moves a quiet tone toward -14 LUFS; clip
    // mode with its measurement sets one gain onto it.
    let quiet = signal(4.0, sine(1000.0, 0.03));
    let at = lufs(&quiet);
    let ride = lufs(span(&run("normalize", &[("mode", 0.0)], &quiet), 2.0, 4.0)) - at;
    check(&mut sweep, "normalize", "normalize ride toward -14 LUFS", format!("{at:.1} LUFS {ride:+.1} LU"), ride > 6.0);
    let clip = lufs(&run("normalize", &[("mode", 1.0), ("measured_lufs", at)], &quiet));
    check(&mut sweep, "normalize", "normalize clip to -14 LUFS", format!("{clip:.2} LUFS"), (clip + 14.0).abs() < 0.5);

    // Pitch shift +5 semitones takes 440 Hz to 587 Hz.
    let a4 = signal(1.0, sine(440.0, 0.3));
    let shifted = run("pitch_shift", &[("semitones", 5.0)], &a4);
    let (up, left) = (level_at(&shifted, 587.3), level_at(&shifted, 440.0));
    check(&mut sweep, "pitch_shift", "pitch_shift +5 st to 587 Hz", format!("587 Hz {up:.1} dB, 440 Hz {left:.1} dB"), up > left + 15.0);

    // Reverb and delay ring after a burst; chorus changes the sound at its level.
    let burst = signal(2.0, |t| if t < 0.5 { sine(440.0, 0.3)(t) } else { 0.0 });
    let rings = |b: &[f32]| rms_db(span(b, 0.55, 0.9));
    let rev = rings(&run("reverb", &[], &burst));
    check(&mut sweep, "reverb", "reverb rings after a burst", format!("{rev:.1} dB"), rev > -50.0);
    let echo = rms_db(span(&run("delay", &[], &burst), 0.55, 0.8));
    check(&mut sweep, "delay", "delay echoes after a burst", format!("{echo:.1} dB"), echo > -40.0);
    // On noise, as on a voice: a sine's copies partly cancel.
    let wide = signal(1.0, noise(0.2));
    let chorus = run("chorus", &[], &wide);
    let diff: Vec<f32> = chorus.iter().zip(&wide).map(|(a, b)| a - b).collect();
    let (changed, level) = (rms_db(&diff) - rms_db(&wide), rms_db(&chorus) - rms_db(&wide));
    check(&mut sweep, "chorus", "chorus changes the sound", format!("diff {changed:+.1} dB"), changed > -30.0);
    check(&mut sweep, "chorus", "chorus keeps the level", format!("{level:+.1} dB"), level.abs() < 3.0);

    // Multiband compressor turns loud broadband sound down.
    let band = signal(1.0, noise(0.7));
    let mb = rms_db(span(&run("multiband_compressor", &[], &band), 0.5, 1.0)) - rms_db(span(&band, 0.5, 1.0));
    check(&mut sweep, "multiband_compressor", "multiband_compressor on loud noise", format!("{mb:+.1} dB"), mb < -2.0);

    // Every built-in effect is measured, but the model-backed one.
    for (id, _) in available_effects() {
        if id == "voice_isolation" {
            sweep.skip(id, "its model downloads on request");
        } else if !seen.contains(&id) {
            sweep.check(&format!("{id} measured"), "no measurement in the sweep", false);
        }
    }
    sweep.finish();
}
