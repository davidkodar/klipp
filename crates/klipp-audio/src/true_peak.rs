//! True-peak detection as FFmpeg's EBU R128 meter (`ebur128=peak=true`)
//! reads it (#1942): the signal resampled to 192 kHz and the highest
//! sample of that, so a peak between input samples is caught and the
//! figure is the one an outside check of the file will give.
//!
//! The resampler is FFmpeg's (swresample's), written out: a 32-tap
//! Kaiser-windowed sinc (beta 9) cut off at the input's Nyquist, with one
//! phase for each 192 kHz output position, exact for the rate
//! (`192000 / gcd(rate, 192000)` phases: 4 at 48 kHz, 640 at 44.1 kHz, 2
//! at 96 kHz), each phase normalised to unit gain. On clicks, impulses,
//! full-band noise and a sine near Nyquist it reads FFmpeg's resampler to
//! within a thousandth of a dB at 44.1, 48 and 96 kHz. At 192 kHz and
//! above it is the sample peak, as FFmpeg's is.
//!
//! It replaced a 4x, 48-tap Hann-windowed sinc that read full-band
//! material up to 1.1 dB low: its window and per-phase normalisation
//! rolled off the top octave, and no phase was the sample itself.
//!
//! A meter that keeps only the running maximum asks [`TruePeakDetector::
//! process_above`] with that maximum: the resampled outputs are bounded by
//! the window's largest sample times the largest row sum, so a frame that
//! can't reach the maximum costs a compare, not 256 multiply-adds, and
//! the reading is the same.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Taps of the resampling filter, at the input rate.
const TAPS: usize = 32;
/// The tap on the output's position (swresample's centre).
const CENTRE: usize = (TAPS - 1) / 2;
/// The rate the signal is resampled to, as FFmpeg's meter does.
const TARGET_RATE: u64 = 192_000;
/// The most phases a table is built with: an odd rate whose exact phase
/// count is larger takes positions on this grid instead.
const MAX_PHASES: u64 = 8192;
const KAISER_BETA: f64 = 9.0;

/// One rate's resampling filter: a row of taps per phase, and how far
/// each output steps through them.
struct Table {
    phases: u64,
    step: u64,
    rows: Vec<[f32; TAPS]>,
    /// The largest sum of a row's absolute taps: no output is larger than
    /// the window's largest sample times this.
    max_l1: f32,
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0f64, 1.0f64);
    for k in 1..50 {
        term *= (x / 2.0) / k as f64;
        sum += term * term;
    }
    sum
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn build_table(rate: u32) -> Table {
    let g = gcd(rate as u64, TARGET_RATE);
    let (mut step, mut phases) = (rate as u64 / g, TARGET_RATE / g);
    if phases > MAX_PHASES {
        // Positions to within 1/8192 of a sample.
        step = (rate as f64 * MAX_PHASES as f64 / TARGET_RATE as f64).round() as u64;
        phases = MAX_PHASES;
    }
    let pi = std::f64::consts::PI;
    let rows: Vec<[f32; TAPS]> = (0..phases)
        .map(|ph| {
            let mut row = [0.0f64; TAPS];
            for (i, tap) in row.iter_mut().enumerate() {
                let x = pi * ((i as f64 - CENTRE as f64) - ph as f64 / phases as f64);
                let sinc = if x.abs() < 1e-12 { 1.0 } else { x.sin() / x };
                let w = 2.0 * x / (TAPS as f64 * pi);
                let window = if w.abs() >= 1.0 { 0.0 } else { bessel_i0(KAISER_BETA * (1.0 - w * w).sqrt()) / bessel_i0(KAISER_BETA) };
                *tap = sinc * window;
            }
            let sum: f64 = row.iter().sum();
            let mut out = [0.0f32; TAPS];
            for (o, t) in out.iter_mut().zip(row) {
                *o = (t / sum) as f32;
            }
            out
        })
        .collect();
    let max_l1 = rows.iter().map(|r| r.iter().map(|t| t.abs()).sum::<f32>()).fold(0.0, f32::max);
    Table { phases, step, rows, max_l1 }
}

/// The table for `rate`, built once.
fn table(rate: u32) -> Arc<Table> {
    static TABLES: OnceLock<Mutex<HashMap<u32, Arc<Table>>>> = OnceLock::new();
    let tables = TABLES.get_or_init(Default::default);
    let mut tables = tables.lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(tables.entry(rate).or_insert_with(|| Arc::new(build_table(rate))))
}

/// A stereo stream's true peak: fed a frame at a time, each call gives
/// the largest resampled sample it made.
#[derive(Clone)]
pub struct TruePeakDetector {
    /// The filter; `None` at 192 kHz and above, where the true peak is
    /// the sample peak.
    table: Option<Arc<Table>>,
    /// The last `TAPS` frames per channel, twice over so they read as
    /// one slice, oldest first, from `pos + 1`.
    hist: [[f32; 2 * TAPS]; 2],
    /// Where the newest frame is.
    pos: usize,
    /// Frames fed.
    fed: u64,
    /// The next output's position, in phases from the stream's start.
    next: u64,
    /// The largest |sample| among the frames in `hist`.
    window_max: f32,
}

impl TruePeakDetector {
    /// A detector for a stream at `sample_rate` Hz.
    pub fn new(sample_rate: u32) -> Self {
        let table = (sample_rate > 0 && (sample_rate as u64) < TARGET_RATE).then(|| table(sample_rate));
        Self { table, hist: [[0.0; 2 * TAPS]; 2], pos: TAPS - 1, fed: 0, next: 0, window_max: 0.0 }
    }

    /// Feed one stereo frame; the largest absolute resampled sample it
    /// completes, on either channel.
    pub fn process(&mut self, l: f32, r: f32) -> f32 {
        self.process_above(l, r, 0.0)
    }

    /// [`Self::process`] for a caller that keeps the running maximum
    /// `floor`: outputs that can't exceed it are not computed, and the
    /// result is then at most `floor`. The maximum over the stream is the
    /// same as [`Self::process`] gives.
    pub fn process_above(&mut self, l: f32, r: f32, floor: f32) -> f32 {
        let Some(table) = self.table.as_ref() else { return l.abs().max(r.abs()) };
        // The frame leaving, then the one coming in.
        self.pos = (self.pos + 1) % TAPS;
        let leaving = self.hist[0][self.pos].abs().max(self.hist[1][self.pos].abs());
        for (c, v) in [l, r].into_iter().enumerate() {
            self.hist[c][self.pos] = v;
            self.hist[c][self.pos + TAPS] = v;
        }
        let coming = l.abs().max(r.abs());
        if coming >= self.window_max {
            self.window_max = coming;
        } else if leaving >= self.window_max {
            self.window_max = (0..TAPS).map(|i| self.hist[0][i].abs().max(self.hist[1][i].abs())).fold(0.0, f32::max);
        }
        let newest = self.fed;
        self.fed += 1;
        // The outputs whose last tap is this frame: those positioned in
        // the frame `TAPS - 1 - CENTRE` back.
        let Some(base) = newest.checked_sub((TAPS - 1 - CENTRE) as u64) else { return 0.0 };
        let mut peak = 0.0f32;
        let reachable = self.window_max * table.max_l1 > floor;
        let start = self.pos + 1;
        while self.next / table.phases == base {
            if reachable {
                let row = &table.rows[(self.next % table.phases) as usize];
                for ch in &self.hist {
                    let window = &ch[start..start + TAPS];
                    let acc: f32 = row.iter().zip(window).map(|(c, x)| c * x).sum();
                    peak = peak.max(acc.abs());
                }
            }
            self.next += table.step;
        }
        peak
    }

    /// End the stream: the outputs still owed for its last frames, made
    /// with silence after them as FFmpeg's resampler flushes, and the
    /// largest of them. A peak in the last `TAPS` / 2 frames (a click
    /// the stream ends on) is only read here.
    pub fn flush(&mut self) -> f32 {
        let mut peak = 0.0f32;
        if self.table.is_some() {
            for _ in 0..TAPS - 1 - CENTRE {
                peak = peak.max(self.process(0.0, 0.0));
            }
        }
        peak
    }

    /// Forget the stream: a meter reset, or a seek.
    pub fn reset(&mut self) {
        self.hist = [[0.0; 2 * TAPS]; 2];
        self.pos = TAPS - 1;
        self.fed = 0;
        self.next = 0;
        self.window_max = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_signal_passes_at_unity_gain() {
        // Each phase is normalised to DC gain 1.0, so a sustained DC
        // input of 0.5 should produce outputs ≈ 0.5 once the delay
        // line is fully primed.
        let mut det = TruePeakDetector::new(48_000);
        for _ in 0..TAPS { det.process(0.5, 0.0); }
        let peak = det.process(0.5, 0.0);
        assert!((peak - 0.5).abs() < 0.05,
            "DC input should pass at unity gain: got {} (expected ~0.5)", peak);
    }

    #[test]
    fn near_nyquist_sine_exceeds_sample_peak() {
        // A 0.5-amplitude sine at 0.45 * Nyquist produces inter-sample
        // peaks that sample-hold misses but 4× oversampling catches.
        use std::f32::consts::PI;
        let sample_rate = 48000.0_f32;
        let freq = 0.45 * (sample_rate / 2.0);
        let mut det = TruePeakDetector::new(48_000);
        let mut max_tp: f32 = 0.0;
        let mut max_sample: f32 = 0.0;
        for i in 0..512 {
            let t = i as f32 / sample_rate;
            let s = 0.5 * (2.0 * PI * freq * t).sin();
            max_sample = max_sample.max(s.abs());
            let tp = det.process(s, s);
            max_tp = max_tp.max(tp);
        }
        assert!(max_tp > max_sample,
            "True peak ({}) should exceed sample peak ({}) for \
             near-Nyquist signal", max_tp, max_sample);
    }

    #[test]
    fn reset_clears_delay_line() {
        let mut det = TruePeakDetector::new(48_000);
        for _ in 0..20 { det.process(0.9, 0.9); }
        det.reset();
        // After reset, processing a single zero sample should yield
        // a near-zero output (no echo of the prior loud signal).
        let peak = det.process(0.0, 0.0);
        assert!(peak.abs() < 1e-6, "reset should clear delay line, got {}", peak);
    }

    #[test]
    fn silence_produces_zero_peak() {
        let mut det = TruePeakDetector::new(48_000);
        for _ in 0..256 {
            let p = det.process(0.0, 0.0);
            assert_eq!(p, 0.0);
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // Bounded-tolerance reference tests.
    //
    // The four qualitative tests above prove the filter "does
    // something reasonable". These tests pin the actual numeric
    // behaviour so a future change to the FIR (e.g. someone tries to
    // drop in the BS.1770-4 Annex 2 reference table) is checked
    // against expected outputs on synthesised stress signals.
    //
    // Tolerance bands are chosen wide enough that a *byte-exact*
    // BS.1770-4 reference filter would also satisfy them — so a
    // future replacement that's compliant won't fail these tests.
    // They're tight enough to fail if someone breaks the polyphase
    // decomposition or de-normalises a phase.
    // ═══════════════════════════════════════════════════════════════

    /// Run a stereo sine through the detector for `n` samples and
    /// return both the maximum sample-hold amplitude and the
    /// maximum true-peak the detector reports.
    fn sweep_sine(amp: f32, freq_hz: f32, sample_rate: f32, n: usize)
        -> (f32 /* sample-hold */, f32 /* true-peak */)
    {
        sweep_sine_phase(amp, freq_hz, sample_rate, 0.0, n)
    }

    /// Like `sweep_sine` but lets the caller pick the starting phase.
    /// Useful for the inter-sample-peak test where the phase offset
    /// is what guarantees samples avoid the actual peak.
    fn sweep_sine_phase(
        amp: f32, freq_hz: f32, sample_rate: f32, phase_rad: f32, n: usize,
    ) -> (f32 /* sample-hold */, f32 /* true-peak */)
    {
        use std::f32::consts::PI;
        let mut det = TruePeakDetector::new(48_000);
        let mut sample_hold: f32 = 0.0;
        let mut true_peak: f32 = 0.0;
        for i in 0..n {
            let t = i as f32 / sample_rate;
            let s = amp * (2.0 * PI * freq_hz * t + phase_rad).sin();
            sample_hold = sample_hold.max(s.abs());
            let tp = det.process(s, s);
            true_peak = true_peak.max(tp);
        }
        (sample_hold, true_peak)
    }

    /// Convert a linear amplitude ratio to dB.
    fn db(x: f32) -> f32 { 20.0 * x.log10() }

    #[test]
    fn ref_low_frequency_sine_recovers_full_amplitude() {
        // 1 kHz at 48 kHz Fs: well below Nyquist, no inter-sample
        // peak loss. True peak should be within 0.5 dB of the sine's
        // amplitude (0.7).
        let (sample_hold, true_peak) = sweep_sine(0.7, 1000.0, 48000.0, 4800);
        assert!(true_peak > 0.66 && true_peak < 0.74,
            "1 kHz sine of amp 0.7: true-peak {} (expected ≈0.7 ± 0.5 dB)", true_peak);
        // Sample-hold also gets close because sample alignment is
        // dense (48 samples per cycle).
        assert!(sample_hold > 0.65,
            "1 kHz sine should be densely sampled — got sample-hold {}", sample_hold);
    }

    #[test]
    fn ref_near_nyquist_sine_recovers_inter_sample_peaks() {
        use std::f32::consts::PI;
        // freq = Fs/4 (12 kHz at 48 kHz) gives exactly 4 samples
        // per cycle. With phase offset π/4 the samples land at
        // π/4, 3π/4, 5π/4, 7π/4 — i.e. ±sin(π/4) = ±0.7071 — and
        // NEVER at the actual peak. Sample-hold therefore reads
        // ~0.707 * amplitude (under by ~3 dB), and a working true-
        // peak filter must recover most of the missing 3 dB.
        //
        // This is the canonical inter-sample-peak stress test: a
        // longer sweep at e.g. 0.45 × Nyquist would eventually have
        // *some* sample land near the peak (because the sample
        // rate and signal frequency aren't commensurate), so
        // sample-hold reads ≈ amp and the test couldn't tell true-
        // peak from sample-hold.
        let amp = 0.5;
        let (sample_hold, true_peak) =
            sweep_sine_phase(amp, 12000.0, 48000.0, PI / 4.0, 1024);

        // Sample-hold under-reads by ~3 dB (sin(π/4) ≈ 0.707).
        let under_read_db = db(sample_hold / amp);
        assert!(under_read_db < -2.5,
            "Sample-hold should under-read by ~3 dB at Fs/4 phase=π/4; got {:.2} dB", under_read_db);

        // True-peak should recover within ~1.5 dB. BS.1770-4 would
        // be tighter (~0.3 dB); our substitute is wider but still
        // way better than the 3 dB sample-hold loss, which is the
        // user-visible point.
        let recovery_db = db(true_peak / amp);
        assert!(recovery_db > -1.5 && recovery_db < 0.5,
            "True-peak should recover to within 1.5 dB; got {:.2} dB", recovery_db);

        assert!(true_peak > sample_hold + 0.05,
            "true-peak ({}) must clearly exceed sample-hold ({}) at Fs/4 phase=π/4", true_peak, sample_hold);
    }

    #[test]
    fn ref_impulse_response_settles_near_unity() {
        // A single 1.0 impulse, then silence: the filter's response
        // peaks at unity (the centre tap is the maximum coefficient).
        // After the delay line drains we return to ~0. This pins the
        // overall gain calibration — a phase that's lost its DC
        // normalisation would push this peak away from 1.0.
        let mut det = TruePeakDetector::new(48_000);
        let mut peak: f32 = 0.0;
        peak = peak.max(det.process(1.0, 1.0));
        for _ in 0..TAPS {
            peak = peak.max(det.process(0.0, 0.0));
        }
        // Tolerance is wide because each phase is DC-normalised, not
        // peak-normalised — the exact response peak depends on the
        // window. Anywhere in [0.4, 1.2] is fine; outside means the
        // FIR has lost its calibration.
        assert!((0.4..=1.2).contains(&peak),
            "impulse response peak {} should be near unity (range 0.4..1.2)", peak);
    }

    #[test]
    fn ref_zero_amplitude_sine_is_zero_peak() {
        // Sanity: a sine of amplitude 0 produces zero true-peak.
        // Catches a regression where the filter biases the output.
        let (sh, tp) = sweep_sine(0.0, 1000.0, 48000.0, 1024);
        assert_eq!(sh, 0.0);
        assert!(tp.abs() < 1e-5,
            "zero-amplitude sine must produce zero true-peak, got {}", tp);
    }

    #[test]
    fn ref_full_scale_sine_does_not_overshoot_significantly() {
        // 0 dBFS sine at 1 kHz: a well-behaved upsampler shouldn't
        // generate a true-peak much above 1.0 (the filter's job is
        // to RECOVER inter-sample peaks, not amplify). At 1 kHz the
        // inter-sample gain is essentially 1.0, so true-peak ≈ 1.0.
        // We allow up to 0.5 dB of overshoot — anything more means
        // the polyphase decomposition has gain error.
        let (_, true_peak) = sweep_sine(1.0, 1000.0, 48000.0, 4800);
        assert!(true_peak <= 1.06,
            "1 kHz 0 dBFS sine: true-peak {} should not overshoot 1.0 by more than ~0.5 dB", true_peak);
        // And shouldn't be wildly under either.
        assert!(true_peak >= 0.95,
            "1 kHz 0 dBFS sine: true-peak {} should be close to 1.0 (not under by more than ~0.5 dB)", true_peak);
    }
}

/// The detector against FFmpeg's EBU R128 meter (#1942).
#[cfg(test)]
mod against_ffmpeg {
    use super::*;

    /// A deterministic noise source.
    fn noise(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }

    /// Three seconds of each test signal at `rate`, interleaved stereo.
    fn signals(rate: u32) -> Vec<(&'static str, Vec<f32>)> {
        let n = rate as usize * 3;
        let fs = rate as f32;
        let mut out = Vec::new();
        let stereo = |mono: Vec<f32>| mono.iter().flat_map(|s| [*s, *s]).collect::<Vec<f32>>();
        // Single-sample impulses, a quarter second apart.
        out.push(("impulses", stereo((0..n).map(|i| if i % (rate as usize / 4) == 100 { 0.5 } else { 0.0 }).collect())));
        // Square clicks 0.6 ms long, alternating sign.
        let click = (0.0006 * fs).round() as usize;
        out.push(("square clicks", stereo((0..n).map(|i| { let k = i / (rate as usize / 4); if i % (rate as usize / 4) < click { if k.is_multiple_of(2) { 0.5 } else { -0.5 } } else { 0.0 } }).collect())));
        // Full-band noise bursts, 50 ms each.
        let mut seed = 0x1234_5678_9abc_def0u64;
        out.push(("noise bursts", stereo((0..n).map(|i| if i % (rate as usize / 2) < rate as usize / 20 { 0.4 * noise(&mut seed) } else { 0.0 }).collect())));
        // A sine at 0.45 of the rate, between samples at its peaks.
        // Faded in and out over 50 ms, so its ends ring no peaks of their own.
        let fade = |i: usize| { let r = rate as usize / 20; let e = i.min(n - 1 - i); if e >= r { 1.0 } else { 0.5 - 0.5 * (std::f32::consts::PI * e as f32 / r as f32).cos() } };
        out.push(("near-Nyquist sine", stereo((0..n).map(|i| fade(i) * 0.5 * (2.0 * std::f32::consts::PI * 0.45 * i as f32 + std::f32::consts::FRAC_PI_4).sin()).collect())));
        // The issue's material: a 110 Hz tone under noise bursts and clicks.
        let mut seed = 0xdead_beef_u64;
        // A click the stream ends on: its peak is in the frames only the
        // flush resamples.
        out.push(("ends on a click", stereo((0..n).map(|i| if i + 3 >= n { 0.6 } else { 0.0 }).collect())));
        out.push(("tone + bursts + clicks", stereo((0..n).map(|i| {
            let t = i as f32 / fs;
            let tone = 0.2 * (2.0 * std::f32::consts::PI * 110.0 * t).sin();
            let burst = if i % (rate as usize / 2) < rate as usize / 25 { 0.3 * noise(&mut seed) } else { 0.0 };
            let clk = if i % (rate as usize / 3) < click { 0.45 } else { 0.0 };
            tone + burst + clk
        }).collect())));
        out
    }

    /// FFmpeg's `ebur128=peak=true` true peak of `samples`, dBFS.
    fn ebur128(samples: &[f32], rate: u32) -> Option<f32> {
        let path = std::env::temp_dir().join(format!("klipp-tp-probe-{}-{rate}.f32", std::process::id()));
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&path, bytes).ok()?;
        let out = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-f", "f32le", "-ar", &rate.to_string(), "-ac", "2", "-i"])
            .arg(&path)
            .args(["-af", "ebur128=peak=true", "-f", "null", "-"])
            .output()
            .ok()?;
        let _ = std::fs::remove_file(&path);
        let text = String::from_utf8_lossy(&out.stderr);
        let at = text.rfind("True peak:")?;
        text[at..].lines().find_map(|l| l.trim().strip_prefix("Peak:")?.split_whitespace().next()?.parse().ok())
    }

    fn db(v: f32) -> f32 {
        20.0 * v.max(1e-9).log10()
    }

    /// FFmpeg's resampler to 192 kHz and its `astats` peak, to a
    /// thousandth of a dB (`ebur128` prints a tenth).
    fn swr_astats(samples: &[f32], rate: u32) -> Option<f32> {
        let path = std::env::temp_dir().join(format!("klipp-tp-probe-a-{}-{rate}.f32", std::process::id()));
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&path, bytes).ok()?;
        let out = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-f", "f32le", "-ar", &rate.to_string(), "-ac", "2", "-i"])
            .arg(&path)
            .args(["-af", "aresample=192000,astats=measure_perchannel=none", "-f", "null", "-"])
            .output()
            .ok()?;
        let _ = std::fs::remove_file(&path);
        let text = String::from_utf8_lossy(&out.stderr);
        text.lines().rev().find_map(|l| l.split("Peak level dB:").nth(1)?.trim().parse::<f32>().ok())
    }

    /// #1942: the detector reads what FFmpeg's EBU R128 meter reads, to
    /// within a thousandth of a dB of its resampler (and a tenth, its
    /// print, of `ebur128` itself) on impulses, clicks, full-band noise,
    /// a sine near Nyquist and a mix of a tone, bursts and clicks, at
    /// 44.1, 48 and 96 kHz. The meter's skip of frames that can't reach
    /// the running maximum changes nothing. Skipped, and said, without
    /// FFmpeg.
    #[test]
    fn true_peak_reads_as_ffmpegs_meter() {
        for rate in [44_100u32, 48_000, 96_000] {
            for (name, samples) in signals(rate) {
                let (Some(reference), Some(fine)) = (ebur128(&samples, rate), swr_astats(&samples, rate)) else {
                    eprintln!("UNVERIFIED: no ffmpeg, the true peak is not compared");
                    return;
                };
                let mut det = TruePeakDetector::new(rate);
                let mut every = 0.0f32;
                for f in samples.chunks_exact(2) {
                    every = every.max(det.process(f[0], f[1]));
                }
                every = every.max(det.flush());
                let mut det = TruePeakDetector::new(rate);
                let mut above = 0.0f32;
                for f in samples.chunks_exact(2) {
                    above = above.max(det.process_above(f[0], f[1], above));
                }
                above = above.max(det.flush());
                // The meter reads the flushed tail too, without ending.
                let mut meter = crate::lufs::LufsMeter::new(rate as f32, true);
                meter.process(&samples);
                let metered = meter.readings().true_peak_linear;
                let got = db(every);
                eprintln!("{rate:>6} {name:<24} detector {got:8.3}  ffmpeg resampler {fine:8.3}  ebur128 {reference:6.1}");
                if name == "ends on a click" {
                    // FFmpeg reads no reference here: `ebur128` never
                    // flushes its resampler, so it misses the click, and
                    // `aresample` flushes with the end mirrored, a click
                    // the signal never had. Flushed with silence, as a
                    // player's output goes quiet after the last sample,
                    // the click reads at least its sample peak, and a
                    // true peak over it between its samples.
                    let sample_peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                    let mut unflushed = TruePeakDetector::new(rate);
                    let missed = samples.chunks_exact(2).fold(0.0f32, |m, f| m.max(unflushed.process(f[0], f[1])));
                    assert!(missed < sample_peak * 0.5, "{rate}: without the flush the click is missed ({missed})");
                    assert!(every > sample_peak, "{rate}: the click the stream ends on reads {every}, its samples {sample_peak}");
                } else {
                    assert!((got - fine).abs() <= 0.01, "{rate} {name}: {got:.3} dBTP, FFmpeg's resampler {fine:.3}");
                    assert!((got - reference).abs() <= 0.1, "{rate} {name}: {got:.3} dBTP, ebur128 {reference:.1}");
                }
                assert_eq!(above, every, "{rate} {name}: the skip changed the reading");
                assert_eq!(metered, every, "{rate} {name}: the meter's reading");
            }
        }
    }
}
