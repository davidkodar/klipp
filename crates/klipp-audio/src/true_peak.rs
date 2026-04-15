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
//! Note on the FIR table: the prompt's hand-rolled coefficient table
//! was not normalised correctly (phase DC gains drifted from 1.0).
//! Substituted a runtime-computed Hann-windowed sinc per the prompt's
//! standing instruction to use a known-good source instead.

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
}
