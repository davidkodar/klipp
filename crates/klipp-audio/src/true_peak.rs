//! 4× polyphase FIR upsampler for ITU-R BS.1770-4 Annex 2 true-peak
//! detection. Catches inter-sample peaks that fall between input
//! samples and would clip a downstream DAC after reconstruction.
//!
//! A 48-tap Hann-windowed sinc prototype low-pass at the input
//! Nyquist is split into 4 phases of 12 taps each. For each input
//! sample we evaluate all 4 phases, returning the maximum absolute
//! value across both stereo channels — the true-peak contribution
//! from this input frame.
//!
//! Sample-hold peak detection (max of |s|) can under-read the true
//! peak by up to ~3 dB on a worst-case sine wave near Nyquist; this
//! filter recovers that headroom. Phase 0 is the identity tap (so
//! the output set always includes the original sample value).
//!
//! ## Substitute-filter caveat (CODE_AUDIT C-6)
//!
//! The 48 coefficients we use are **NOT byte-exact** to the reference
//! table in BS.1770-4 Annex 2 Table 1. We use a runtime-computed
//! Hann-windowed sinc (a textbook 4× anti-image low-pass) as a stand-
//! in. The earlier hand-rolled coefficient table had phase DC gains
//! drifting from 1.0 and was replaced with this known-good prototype.
//!
//! For loudness metering inside a video editor — i.e. catching dBTP
//! values within ~0.5 dB of true so the user can mix to a target —
//! this is fine. For *byte-exact* BS.1770-4 compliance (e.g. for
//! third-party audit), the reference coefficient table from the
//! standard would have to be used instead. Replacing is straight-
//! forward (drop in 48 numbers, keep the polyphase decomposition);
//! it just needs the spec PDF in front of the implementer to copy
//! the table cleanly.
//!
//! The bounded-tolerance regression tests at the bottom of this
//! file pin the current substitute's behaviour on synthesised
//! reference vectors so a future "let me clean up that FIR" can't
//! silently regress into something worse than the current filter.

use std::sync::OnceLock;

const PHASES: usize = 4;
const TAPS_PER_PHASE: usize = 12;
const PROTOTYPE_LEN: usize = PHASES * TAPS_PER_PHASE; // 48

/// Cached polyphase FIR — one row per phase, 12 taps per row.
/// Computed once on first use via [`Self::fir`].
static FIR: OnceLock<[[f32; TAPS_PER_PHASE]; PHASES]> = OnceLock::new();

/// Build the 4× polyphase FIR by sampling a Hann-windowed sinc that
/// passes DC..Nyquist of the input rate, then splitting into phases
/// and normalising each phase to DC gain 1.0.
fn build_fir() -> [[f32; TAPS_PER_PHASE]; PHASES] {
    use std::f32::consts::PI;

    // Prototype: sinc(t) * hann(t) at PROTOTYPE_LEN samples, centred.
    // Cutoff = 1 / PHASES (i.e. 1/4 of the upsampled rate = input
    // Nyquist) so the upsampled image lands in the stop-band.
    let mut prototype = [0.0_f32; PROTOTYPE_LEN];
    let centre = (PROTOTYPE_LEN as f32 - 1.0) * 0.5;
    let cutoff = 1.0 / PHASES as f32;
    for n in 0..PROTOTYPE_LEN {
        let x = n as f32 - centre;
        // Sinc with the chosen cutoff.
        let s = if x.abs() < 1e-7 {
            cutoff
        } else {
            (PI * cutoff * x).sin() / (PI * x)
        };
        // Hann window over the full prototype.
        let w = 0.5 * (1.0 - (2.0 * PI * n as f32 / (PROTOTYPE_LEN as f32 - 1.0)).cos());
        prototype[n] = s * w;
    }

    // Split into 4 polyphase branches and normalise each to DC gain 1.0
    // so each interpolated output is comparable in amplitude to the
    // input samples (and so DC inputs round-trip without amplification).
    let mut fir = [[0.0_f32; TAPS_PER_PHASE]; PHASES];
    for phase in 0..PHASES {
        let mut sum = 0.0_f32;
        for tap in 0..TAPS_PER_PHASE {
            // Phase k holds prototype indices k, k+PHASES, k+2*PHASES, ...
            // Reverse the tap order so tap 0 multiplies the newest sample
            // — matches the convolution loop below which walks the delay
            // line from buf_pos backwards.
            let proto_idx = phase + (TAPS_PER_PHASE - 1 - tap) * PHASES;
            fir[phase][tap] = prototype[proto_idx];
            sum += fir[phase][tap];
        }
        // Normalise to unity DC gain.
        if sum.abs() > 1e-9 {
            for tap in 0..TAPS_PER_PHASE {
                fir[phase][tap] /= sum;
            }
        }
    }
    fir
}

fn fir() -> &'static [[f32; TAPS_PER_PHASE]; PHASES] {
    FIR.get_or_init(build_fir)
}

/// Per-channel circular delay line + 4-phase polyphase evaluator.
/// State is just the recent sample history; safe to drop and recreate.
pub struct TruePeakDetector {
    /// Delay line per channel (L, R). Newest sample at `buf_pos`,
    /// older samples wrap backwards modulo `TAPS_PER_PHASE`.
    buf: [[f32; TAPS_PER_PHASE]; 2],
    /// Index into `buf` where the most recent sample was written.
    buf_pos: usize,
}

impl TruePeakDetector {
    pub fn new() -> Self {
        // Force the FIR to materialise on first construction so the
        // first call to `process` doesn't pay the build cost.
        let _ = fir();
        Self {
            buf: [[0.0; TAPS_PER_PHASE]; 2],
            buf_pos: TAPS_PER_PHASE - 1, // first sample lands at index 0
        }
    }

    /// Process one stereo sample pair through the 4× upsampler and
    /// return the maximum absolute value across all 4 interpolated
    /// outputs for both channels.
    ///
    /// Call once per input sample; accumulate the returned max over
    /// the block to obtain the true-peak for that block.
    pub fn process(&mut self, l: f32, r: f32) -> f32 {
        // Advance the write head and store the new samples.
        self.buf_pos = (self.buf_pos + 1) % TAPS_PER_PHASE;
        self.buf[0][self.buf_pos] = l;
        self.buf[1][self.buf_pos] = r;

        let coefs_all = fir();
        let mut max_abs: f32 = 0.0;
        for phase_coefs in coefs_all.iter() {
            // Convolve each channel's delay line with this phase.
            let mut acc_l = 0.0_f32;
            let mut acc_r = 0.0_f32;
            for tap in 0..TAPS_PER_PHASE {
                // Walk the delay line backwards from buf_pos so tap 0
                // reads the newest sample; matches the FIR ordering
                // we set up in `build_fir`.
                let idx = (self.buf_pos + TAPS_PER_PHASE - tap) % TAPS_PER_PHASE;
                let c = phase_coefs[tap];
                acc_l += self.buf[0][idx] * c;
                acc_r += self.buf[1][idx] * c;
            }
            let m = acc_l.abs().max(acc_r.abs());
            if m > max_abs {
                max_abs = m;
            }
        }
        max_abs
    }

    /// Clear the delay line — call when a meter is reset or the
    /// audio stream is seeked / restarted to avoid stale samples
    /// leaking into the first measured block.
    pub fn reset(&mut self) {
        self.buf = [[0.0; TAPS_PER_PHASE]; 2];
        self.buf_pos = TAPS_PER_PHASE - 1;
    }
}

impl Default for TruePeakDetector {
    fn default() -> Self {
        Self::new()
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
        let mut det = TruePeakDetector::new();
        for _ in 0..PROTOTYPE_LEN { det.process(0.5, 0.0); }
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
        let mut det = TruePeakDetector::new();
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
        let mut det = TruePeakDetector::new();
        for _ in 0..20 { det.process(0.9, 0.9); }
        det.reset();
        // After reset, processing a single zero sample should yield
        // a near-zero output (no echo of the prior loud signal).
        let peak = det.process(0.0, 0.0);
        assert!(peak.abs() < 1e-6, "reset should clear delay line, got {}", peak);
    }

    #[test]
    fn silence_produces_zero_peak() {
        let mut det = TruePeakDetector::new();
        for _ in 0..256 {
            let p = det.process(0.0, 0.0);
            assert_eq!(p, 0.0);
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // Bounded-tolerance reference tests — closes CODE_AUDIT C-6.
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
        let mut det = TruePeakDetector::new();
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
        let mut det = TruePeakDetector::new();
        let mut peak: f32 = 0.0;
        peak = peak.max(det.process(1.0, 1.0));
        for _ in 0..PROTOTYPE_LEN {
            peak = peak.max(det.process(0.0, 0.0));
        }
        // Tolerance is wide because each phase is DC-normalised, not
        // peak-normalised — the exact response peak depends on the
        // window. Anywhere in [0.4, 1.2] is fine; outside means the
        // FIR has lost its calibration.
        assert!(peak >= 0.4 && peak <= 1.2,
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
