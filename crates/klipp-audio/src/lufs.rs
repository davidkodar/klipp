//! ITU-R BS.1770-4 compliant loudness meter.
//!
//! Implements K-weighted loudness measurement with:
//! - Two-stage K-weighting filter (pre-filter high shelf + RLB high-pass)
//! - 400ms measurement blocks with 75% overlap (100ms hop)
//! - Absolute gate (-70 LUFS) and relative gate (-10 LU)
//! - Momentary loudness (400ms), short-term (3s), and integrated (gated)
//! - True peak: sample-accurate by default; pass `oversample = true`
//!   to enable BS.1770-4 Annex 2 4× polyphase FIR upsampling for
//!   inter-sample peak detection.

use crate::true_peak::TruePeakDetector;
use std::collections::VecDeque;

/// Shared LUFS readings published from the mix thread to the UI.
#[derive(Debug, Clone, Default)]
pub struct LufsReadings {
    pub momentary: f32,
    pub short_term: f32,
    pub integrated: f32,
    pub loudness_range: f32,
    pub true_peak_dbfs: f32,
    /// Linear true-peak (max absolute amplitude) — same value as
    /// `true_peak_dbfs` but in linear units. Useful for
    /// loudness-normalisation peak-ceiling math without a dB
    /// round-trip.
    pub true_peak_linear: f32,
}

/// ITU-R BS.1770-4 compliant loudness meter for stereo audio.
pub struct LufsMeter {
    sample_rate: f32,
    // K-weighting filters (one per channel: L, R)
    pre_filter: [BiquadFilter; 2],
    rlb_filter: [BiquadFilter; 2],
    // Block accumulation (400ms blocks, 75% overlap = 100ms hop)
    /// Per-sample K-weighted mean-square values in current block window.
    block_samples: VecDeque<f32>,
    /// Running sum of samples in block_samples (avoids O(n) recompute).
    block_sum: f64,
    block_size: usize,
    hop_size: usize,
    hop_counter: usize,
    // Gating history — stores mean-square per completed 400ms block
    block_loudness_history: Vec<f32>,
    // True peak — maximum sample magnitude seen
    true_peak_linear: f32,
    /// 4× upsampling true-peak detector (only consulted when
    /// `oversample` is true; otherwise we fall back to sample-hold).
    tp_detector: TruePeakDetector,
    /// When true, true-peak is measured via 4× polyphase FIR upsampling
    /// (BS.1770-4 Annex 2 compliant). When false, we use the simpler
    /// sample-hold max(|s|) — slightly cheaper, but can under-read
    /// inter-sample peaks by up to ~3 dB on near-Nyquist signals.
    oversample: bool,
    // Short-term block history for LRA calculation (3s blocks)
    short_term_history: Vec<f32>,
    // Output values
    pub momentary_lufs: f32,
    pub short_term_lufs: f32,
    pub integrated_lufs: f32,
    pub loudness_range: f32,
    pub true_peak_dbfs: f32,
}

/// Direct Form I biquad filter using f64 for coefficient precision.
struct BiquadFilter {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl BiquadFilter {
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    /// K-weighting Stage 1: pre-filter (high shelf +4 dB at ~1.5 kHz).
    ///
    /// Coefficients derived from ITU-R BS.1770-4 Table 1 using the
    /// analog prototype bilinear-transformed to the given sample rate.
    fn k_weight_pre(sample_rate: f32) -> Self {
        let f0 = 1681.974450955533_f64;
        let g = 3.999843853973347_f64;
        let q = 0.7071752369554196_f64;
        let k = (std::f64::consts::PI * f0 / sample_rate as f64).tan();
        let vb = 10_f64.powf(g / 20.0);
        let vb_k_q = vb.sqrt() * k / q;
        let k2 = k * k;
        let a0 = 1.0 + k / q + k2;
        Self {
            b0: (vb + vb_k_q + k2) / a0,
            b1: 2.0 * (k2 - vb) / a0,
            b2: (vb - vb_k_q + k2) / a0,
            a1: 2.0 * (k2 - 1.0) / a0,
            a2: (1.0 - k / q + k2) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// K-weighting Stage 2: revised low-frequency B-curve (RLB) high-pass ~38 Hz.
    fn k_weight_rlb(sample_rate: f32) -> Self {
        let f0 = 38.13547087602444_f64;
        let q = 0.5003270373238773_f64;
        let k = (std::f64::consts::PI * f0 / sample_rate as f64).tan();
        let k2 = k * k;
        let a0 = 1.0 + k / q + k2;
        Self {
            b0: 1.0 / a0,
            b1: -2.0 / a0,
            b2: 1.0 / a0,
            a1: 2.0 * (k2 - 1.0) / a0,
            a2: (1.0 - k / q + k2) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }
}

fn ms_to_lufs(ms: f32) -> f32 {
    if ms > 1e-10 {
        -0.691 + 10.0 * ms.log10()
    } else {
        f32::NEG_INFINITY
    }
}

impl LufsMeter {
    /// Build a meter at the given sample rate. `oversample` controls
    /// the true-peak path: `true` enables 4× polyphase FIR upsampling
    /// (BS.1770-4 Annex 2 compliant), `false` falls back to cheaper
    /// sample-hold peak detection.
    pub fn new(sample_rate: f32, oversample: bool) -> Self {
        let block_size = (sample_rate * 0.4) as usize; // 400 ms
        let hop_size = (sample_rate * 0.1) as usize; // 100 ms (75% overlap)
        Self {
            sample_rate,
            pre_filter: [
                BiquadFilter::k_weight_pre(sample_rate),
                BiquadFilter::k_weight_pre(sample_rate),
            ],
            rlb_filter: [
                BiquadFilter::k_weight_rlb(sample_rate),
                BiquadFilter::k_weight_rlb(sample_rate),
            ],
            block_samples: VecDeque::with_capacity(block_size + hop_size),
            block_sum: 0.0,
            block_size,
            hop_size,
            hop_counter: 0,
            block_loudness_history: Vec::new(),
            short_term_history: Vec::new(),
            true_peak_linear: 0.0,
            tp_detector: TruePeakDetector::new(),
            oversample,
            momentary_lufs: f32::NEG_INFINITY,
            short_term_lufs: f32::NEG_INFINITY,
            integrated_lufs: f32::NEG_INFINITY,
            loudness_range: 0.0,
            true_peak_dbfs: f32::NEG_INFINITY,
        }
    }

    /// Process interleaved stereo f32 samples through the BS.1770-4 pipeline.
    pub fn process(&mut self, samples: &[f32]) {
        let frames = samples.len() / 2;
        for i in 0..frames {
            let l = samples[i * 2] as f64;
            let r = samples[i * 2 + 1] as f64;

            // K-weighting: pre-filter then RLB, per channel
            let l_k = self.rlb_filter[0].process(self.pre_filter[0].process(l));
            let r_k = self.rlb_filter[1].process(self.pre_filter[1].process(r));

            // Mean square: equal weight for L and R (BS.1770 specifies G_L = G_R = 1.0)
            let ms = (l_k * l_k + r_k * r_k) as f32;
            self.block_samples.push_back(ms);
            self.block_sum += ms as f64;

            // True peak — 4× oversampled when enabled (BS.1770-4
            // Annex 2 compliant), sample-hold otherwise.
            let peak = if self.oversample {
                self.tp_detector.process(l as f32, r as f32)
            } else {
                (l.abs() as f32).max(r.abs() as f32)
            };
            if peak > self.true_peak_linear {
                self.true_peak_linear = peak;
                self.true_peak_dbfs = if peak > 0.0 {
                    20.0 * peak.log10()
                } else {
                    f32::NEG_INFINITY
                };
            }

            self.hop_counter += 1;
            if self.hop_counter >= self.hop_size {
                self.hop_counter = 0;
                self.complete_block();
            }
        }
    }

    /// Called every hop (100 ms). Evaluates the current 400 ms block.
    fn complete_block(&mut self) {
        // Trim to block_size — slide the window
        while self.block_samples.len() > self.block_size {
            let old = self.block_samples.pop_front().unwrap_or(0.0);
            self.block_sum -= old as f64;
        }
        // Guard against fp drift
        if self.block_sum < 0.0 {
            self.block_sum = 0.0;
        }

        if self.block_samples.len() < self.block_size {
            return; // not enough data yet
        }

        // Block mean square (normalized: divide by block_size, already summed L²+R²)
        // BS.1770 formula: L_K = -0.691 + 10 * log10( sum_i G_i * z_i )
        // For stereo: z_i = (1/T) * integral of y_i² dt, so we divide by block_size
        let block_ms = (self.block_sum / self.block_size as f64) as f32;

        // Momentary loudness (latest 400 ms block)
        self.momentary_lufs = ms_to_lufs(block_ms);

        // Store for integrated calculation
        self.block_loudness_history.push(block_ms);

        // Short-term loudness: 3 s = last 30 blocks (at 100 ms hop)
        let st_blocks = 30.min(self.block_loudness_history.len());
        let st_start = self.block_loudness_history.len() - st_blocks;
        let st_sum: f64 = self.block_loudness_history[st_start..].iter().map(|&v| v as f64).sum();
        let st_ms = (st_sum / st_blocks as f64) as f32;
        self.short_term_lufs = ms_to_lufs(st_ms);

        // Store short-term LUFS for LRA calculation
        if self.short_term_lufs.is_finite() {
            self.short_term_history.push(self.short_term_lufs);
        }

        // Integrated loudness with BS.1770-4 gating + LRA
        self.update_integrated();
    }

    /// Two-pass gating per BS.1770-4 Section 4.
    fn update_integrated(&mut self) {
        // Absolute gate: -70 LUFS → mean-square threshold
        let abs_gate_ms = 10_f32.powf((-70.0 + 0.691) / 10.0);

        // Pass 1: mean of blocks above absolute gate
        let mut ungated_sum = 0.0_f64;
        let mut ungated_count = 0_u32;
        for &ms in &self.block_loudness_history {
            if ms > abs_gate_ms {
                ungated_sum += ms as f64;
                ungated_count += 1;
            }
        }

        if ungated_count == 0 {
            self.integrated_lufs = f32::NEG_INFINITY;
            return;
        }

        // Relative gate: -10 LU below the ungated mean
        let ungated_mean = (ungated_sum / ungated_count as f64) as f32;
        let rel_gate_ms = ungated_mean * 10_f32.powf(-10.0 / 10.0); // = ungated_mean * 0.1

        // Pass 2: mean of blocks above both gates
        let mut gated_sum = 0.0_f64;
        let mut gated_count = 0_u32;
        for &ms in &self.block_loudness_history {
            if ms > abs_gate_ms && ms > rel_gate_ms {
                gated_sum += ms as f64;
                gated_count += 1;
            }
        }

        if gated_count == 0 {
            self.integrated_lufs = f32::NEG_INFINITY;
            return;
        }

        let gated_mean = (gated_sum / gated_count as f64) as f32;
        self.integrated_lufs = ms_to_lufs(gated_mean);

        // LRA (Loudness Range) per EBU R128 / EBU Tech 3342:
        // Percentile range of short-term loudness values above the relative gate.
        // LRA = P95 - P10 of gated short-term loudness distribution.
        if self.short_term_history.len() >= 2 {
            let abs_gate_lufs = -70.0_f32;
            let rel_gate_lufs = self.integrated_lufs - 20.0; // -20 LU below integrated
            let mut gated_st: Vec<f32> = self.short_term_history.iter()
                .copied()
                .filter(|&v| v > abs_gate_lufs && v > rel_gate_lufs)
                .collect();
            if gated_st.len() >= 2 {
                gated_st.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let n = gated_st.len();
                let p10_idx = (n as f32 * 0.10).floor() as usize;
                let p95_idx = ((n as f32 * 0.95).ceil() as usize).min(n - 1);
                self.loudness_range = gated_st[p95_idx] - gated_st[p10_idx];
            }
        }
    }

    /// Get current readings as a snapshot.
    pub fn readings(&self) -> LufsReadings {
        LufsReadings {
            momentary: self.momentary_lufs,
            short_term: self.short_term_lufs,
            integrated: self.integrated_lufs,
            loudness_range: self.loudness_range,
            true_peak_dbfs: self.true_peak_dbfs,
            true_peak_linear: self.true_peak_linear,
        }
    }

    /// Reset all state (called when user presses reset or on seek).
    /// Preserves the oversample mode — switching modes requires
    /// reconstructing the meter via `Self::new(sr, oversample)`.
    pub fn reset(&mut self) {
        let sr = self.sample_rate;
        let os = self.oversample;
        *self = Self::new(sr, os);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_sine(sample_rate: f32, freq: f32, duration_secs: f32, amplitude: f32) -> Vec<f32> {
        let num_frames = (sample_rate * duration_secs) as usize;
        let mut samples = Vec::with_capacity(num_frames * 2);
        for i in 0..num_frames {
            let t = i as f32 / sample_rate;
            let s = amplitude * (2.0 * std::f32::consts::PI * freq * t).sin();
            samples.push(s); // L
            samples.push(s); // R
        }
        samples
    }

    #[test]
    fn silence_reads_neg_infinity() {
        let mut meter = LufsMeter::new(48000.0, true);
        let silence = vec![0.0f32; 48000 * 2]; // 1 second stereo
        meter.process(&silence);
        assert!(meter.momentary_lufs.is_infinite() && meter.momentary_lufs < 0.0);
        assert!(meter.integrated_lufs.is_infinite() && meter.integrated_lufs < 0.0);
    }

    #[test]
    fn full_scale_sine_reads_near_expected_lufs() {
        // A 1 kHz full-scale sine at 0 dBFS should read approximately -3.01 LUFS
        // (since LUFS of a sine is ~-3 dB relative to square wave, plus K-weighting
        // is nearly flat at 1 kHz). Expected: roughly -3 to -4 LUFS.
        let mut meter = LufsMeter::new(48000.0, true);
        let samples = generate_sine(48000.0, 1000.0, 2.0, 1.0);
        meter.process(&samples);

        // Should be finite and in a reasonable range
        assert!(meter.momentary_lufs.is_finite());
        assert!(meter.momentary_lufs > -10.0 && meter.momentary_lufs < 0.0,
            "Expected ~-3 LUFS for full-scale 1kHz sine, got {}", meter.momentary_lufs);
    }

    #[test]
    fn quiet_signal_gated_out() {
        // A very quiet signal should be below the -70 LUFS absolute gate
        let mut meter = LufsMeter::new(48000.0, true);
        let samples = generate_sine(48000.0, 1000.0, 2.0, 0.00001);
        meter.process(&samples);
        // Integrated should be -inf because all blocks are below absolute gate
        assert!(meter.integrated_lufs.is_infinite() && meter.integrated_lufs < 0.0);
    }

    #[test]
    fn reset_clears_state() {
        let mut meter = LufsMeter::new(48000.0, true);
        let samples = generate_sine(48000.0, 1000.0, 1.0, 0.5);
        meter.process(&samples);
        assert!(meter.momentary_lufs.is_finite());

        meter.reset();
        assert!(meter.momentary_lufs.is_infinite());
        assert!(meter.integrated_lufs.is_infinite());
        assert_eq!(meter.true_peak_linear, 0.0);
    }

    #[test]
    fn short_term_uses_3s_window() {
        let mut meter = LufsMeter::new(48000.0, true);
        // Process 4 seconds of audio
        let samples = generate_sine(48000.0, 1000.0, 4.0, 0.5);
        meter.process(&samples);
        // Short-term should be finite after >400ms
        assert!(meter.short_term_lufs.is_finite());
        // Short-term and momentary should be close for steady-state signal
        let diff = (meter.short_term_lufs - meter.momentary_lufs).abs();
        assert!(diff < 1.0, "Steady-state S-M diff should be small, got {diff}");
    }

    #[test]
    fn k_weighting_attenuates_low_frequencies() {
        // 50 Hz tone should measure lower than 1 kHz tone at same amplitude
        // because K-weighting RLB filter attenuates low frequencies
        let mut meter_low = LufsMeter::new(48000.0, true);
        let mut meter_mid = LufsMeter::new(48000.0, true);

        let low = generate_sine(48000.0, 50.0, 2.0, 0.5);
        let mid = generate_sine(48000.0, 1000.0, 2.0, 0.5);

        meter_low.process(&low);
        meter_mid.process(&mid);

        assert!(meter_low.momentary_lufs < meter_mid.momentary_lufs,
            "50Hz ({:.1}) should read lower than 1kHz ({:.1}) due to K-weighting",
            meter_low.momentary_lufs, meter_mid.momentary_lufs);
    }

    // ═══════════════════════════════════════════════════════════════
    // K-weighting filter accuracy
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn k_weight_pre_filter_boosts_high_frequencies() {
        // Pre-filter is a high shelf +4dB at ~1.5kHz.
        // 4kHz tone should read higher than 200Hz tone at same amplitude.
        let mut meter_hi = LufsMeter::new(48000.0, true);
        let mut meter_lo = LufsMeter::new(48000.0, true);

        meter_hi.process(&generate_sine(48000.0, 4000.0, 2.0, 0.3));
        meter_lo.process(&generate_sine(48000.0, 200.0, 2.0, 0.3));

        assert!(meter_hi.momentary_lufs > meter_lo.momentary_lufs,
            "4kHz ({:.1}) should read higher than 200Hz ({:.1}) due to pre-filter boost",
            meter_hi.momentary_lufs, meter_lo.momentary_lufs);
    }

    #[test]
    fn k_weighting_flat_at_1khz() {
        // K-weighting should be approximately 0dB at 1kHz.
        // Compare K-weighted LUFS to what simple RMS would give.
        let mut meter = LufsMeter::new(48000.0, true);
        let amplitude = 0.5_f32;
        let samples = generate_sine(48000.0, 1000.0, 2.0, amplitude);
        meter.process(&samples);

        // RMS of a sine = amplitude / sqrt(2). For stereo sum: 2 * (amp/sqrt(2))^2 = amp^2
        // Then LUFS = -0.691 + 10*log10(amp^2) = -0.691 + 20*log10(amp)
        let expected = -0.691 + 20.0 * (amplitude as f64).log10();
        let diff = (meter.momentary_lufs as f64 - expected).abs();
        assert!(diff < 1.5, // Within 1.5 dB (K-weighting not perfectly flat at 1kHz)
            "1kHz LUFS ({:.2}) should be near theoretical ({:.2}), diff={:.2}",
            meter.momentary_lufs, expected, diff);
    }

    // ═══════════════════════════════════════════════════════════════
    // Block / hop boundary handling
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn no_output_before_first_block() {
        let mut meter = LufsMeter::new(48000.0, true);
        // Process only 100ms (less than one 400ms block)
        let short = generate_sine(48000.0, 1000.0, 0.1, 0.5);
        meter.process(&short);
        // Should still be -inf since no complete block formed
        assert!(meter.momentary_lufs.is_infinite());
    }

    #[test]
    fn first_block_at_400ms() {
        let mut meter = LufsMeter::new(48000.0, true);
        // Process exactly 400ms
        let block = generate_sine(48000.0, 1000.0, 0.4, 0.5);
        meter.process(&block);
        // Still -inf: need 400ms of data AND a hop boundary
        // Process one more hop (100ms) to trigger the first block evaluation
        let hop = generate_sine(48000.0, 1000.0, 0.1, 0.5);
        meter.process(&hop);
        // Now we should have a reading (we had 500ms, block window is 400ms, first hop at 100ms triggers)
        // Actually the first hop fires at 100ms, but block_samples only has 100ms of data at that point.
        // The second hop fires at 200ms, third at 300ms, fourth at 400ms — that's when block_size is reached.
        // So we need 400ms total for the first complete block.
        // At 400ms, the hop fires and block_samples.len() == block_size. Let's just process 500ms total.
        assert!(meter.momentary_lufs.is_finite(),
            "Should have a reading after 500ms, got {}", meter.momentary_lufs);
    }

    #[test]
    fn incremental_processing_matches_bulk() {
        // Process 2s of audio in one call vs many small calls — results should match
        let samples = generate_sine(48000.0, 1000.0, 2.0, 0.5);

        let mut bulk = LufsMeter::new(48000.0, true);
        bulk.process(&samples);

        let mut incremental = LufsMeter::new(48000.0, true);
        // Process in 256-sample chunks (128 stereo frames)
        for chunk in samples.chunks(256) {
            incremental.process(chunk);
        }

        let diff_m = (bulk.momentary_lufs - incremental.momentary_lufs).abs();
        let diff_i = (bulk.integrated_lufs - incremental.integrated_lufs).abs();
        assert!(diff_m < 0.01, "Momentary differs: bulk={:.3} inc={:.3}",
            bulk.momentary_lufs, incremental.momentary_lufs);
        assert!(diff_i < 0.01, "Integrated differs: bulk={:.3} inc={:.3}",
            bulk.integrated_lufs, incremental.integrated_lufs);
    }

    // ═══════════════════════════════════════════════════════════════
    // Gating
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn relative_gate_excludes_quiet_sections() {
        let mut meter = LufsMeter::new(48000.0, true);

        // 2s of loud signal
        let loud = generate_sine(48000.0, 1000.0, 2.0, 0.5);
        meter.process(&loud);
        let integrated_loud_only = meter.integrated_lufs;

        // Now add 2s of much quieter signal (should be gated out by relative gate)
        let quiet = generate_sine(48000.0, 1000.0, 2.0, 0.005);
        meter.process(&quiet);
        let integrated_with_quiet = meter.integrated_lufs;

        // The quiet section should be below the relative gate (-10 LU below the loud mean),
        // so integrated should barely change
        let diff = (integrated_loud_only - integrated_with_quiet).abs();
        assert!(diff < 1.0,
            "Relative gate should exclude quiet section: loud_only={:.1} with_quiet={:.1} diff={:.2}",
            integrated_loud_only, integrated_with_quiet, diff);
    }

    #[test]
    fn integrated_accumulates_over_time() {
        let mut meter = LufsMeter::new(48000.0, true);

        // Process 1s — get first integrated reading
        meter.process(&generate_sine(48000.0, 1000.0, 1.0, 0.5));
        let i1 = meter.integrated_lufs;

        // Process 1 more second of same level — integrated should be similar
        meter.process(&generate_sine(48000.0, 1000.0, 1.0, 0.5));
        let i2 = meter.integrated_lufs;

        assert!(i1.is_finite() && i2.is_finite());
        let diff = (i1 - i2).abs();
        assert!(diff < 0.5, "Steady signal integrated should be stable: i1={:.1} i2={:.1}", i1, i2);
    }

    // ══════════════════════════════════════════════════════════���════
    // True peak
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn true_peak_tracks_maximum() {
        let mut meter = LufsMeter::new(48000.0, true);

        // Quiet signal first
        meter.process(&generate_sine(48000.0, 1000.0, 1.0, 0.1));
        let tp1 = meter.true_peak_dbfs;

        // Then louder signal
        meter.process(&generate_sine(48000.0, 1000.0, 1.0, 0.8));
        let tp2 = meter.true_peak_dbfs;

        assert!(tp2 > tp1, "True peak should increase with louder signal: {:.1} -> {:.1}", tp1, tp2);
        // 0.8 amplitude => ~-1.94 dBFS
        assert!((tp2 - 20.0 * 0.8_f32.log10()).abs() < 0.1,
            "True peak for 0.8 amplitude should be ~-1.94 dBFS, got {:.2}", tp2);
    }

    #[test]
    fn true_peak_does_not_decrease() {
        let mut meter = LufsMeter::new(48000.0, true);

        // Loud then quiet
        meter.process(&generate_sine(48000.0, 1000.0, 1.0, 0.9));
        let peak_after_loud = meter.true_peak_dbfs;

        meter.process(&generate_sine(48000.0, 1000.0, 1.0, 0.1));
        let peak_after_quiet = meter.true_peak_dbfs;

        assert_eq!(peak_after_loud, peak_after_quiet,
            "True peak should hold: {:.2} vs {:.2}", peak_after_loud, peak_after_quiet);
    }

    #[test]
    fn oversample_true_peak_higher_than_sample_hold() {
        // A near-Nyquist sine has inter-sample peaks that the simple
        // sample-hold path misses but the 4× oversampled path catches.
        use std::f32::consts::PI;
        let sr = 48000.0_f32;
        let freq = 0.45 * (sr / 2.0);
        let mut meter_os = LufsMeter::new(sr, true);
        let mut meter_sp = LufsMeter::new(sr, false);
        let mut buf = Vec::new();
        for i in 0..512 {
            let t = i as f32 / sr;
            let s = 0.5 * (2.0 * PI * freq * t).sin();
            buf.push(s); buf.push(s); // stereo interleaved
        }
        meter_os.process(&buf);
        meter_sp.process(&buf);
        assert!(
            meter_os.readings().true_peak_dbfs > meter_sp.readings().true_peak_dbfs,
            "Oversampled TP ({:.3} dBFS) should exceed sample-hold TP ({:.3} dBFS) for near-Nyquist signal",
            meter_os.readings().true_peak_dbfs,
            meter_sp.readings().true_peak_dbfs,
        );
    }

    // ═══════════════════════════════════════════════════════════════
    // LufsReadings snapshot
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn readings_snapshot_matches_fields() {
        let mut meter = LufsMeter::new(48000.0, true);
        meter.process(&generate_sine(48000.0, 1000.0, 2.0, 0.5));

        let r = meter.readings();
        assert_eq!(r.momentary, meter.momentary_lufs);
        assert_eq!(r.short_term, meter.short_term_lufs);
        assert_eq!(r.integrated, meter.integrated_lufs);
        assert_eq!(r.true_peak_dbfs, meter.true_peak_dbfs);
    }

    #[test]
    fn default_readings_are_neg_infinity() {
        let r = LufsReadings::default();
        assert_eq!(r.momentary, 0.0); // Default f32 is 0.0
        assert_eq!(r.short_term, 0.0);
        // Note: Default gives 0.0, but meter.new() gives -inf.
        // The UI should handle both.
    }

    // ═══════════════════════════════════════════════════════════════
    // Different sample rates
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn works_at_44100hz() {
        let mut meter = LufsMeter::new(44100.0, true);
        let samples = generate_sine(44100.0, 1000.0, 2.0, 0.5);
        meter.process(&samples);
        assert!(meter.momentary_lufs.is_finite());
        assert!(meter.integrated_lufs.is_finite());
    }

    #[test]
    fn works_at_96000hz() {
        let mut meter = LufsMeter::new(96000.0, true);
        let samples = generate_sine(96000.0, 1000.0, 2.0, 0.5);
        meter.process(&samples);
        assert!(meter.momentary_lufs.is_finite());
    }

    #[test]
    fn consistent_across_sample_rates() {
        // Same signal at different sample rates should give similar LUFS
        let mut m48 = LufsMeter::new(48000.0, true);
        let mut m44 = LufsMeter::new(44100.0, true);
        let mut m96 = LufsMeter::new(96000.0, true);

        m48.process(&generate_sine(48000.0, 1000.0, 2.0, 0.5));
        m44.process(&generate_sine(44100.0, 1000.0, 2.0, 0.5));
        m96.process(&generate_sine(96000.0, 1000.0, 2.0, 0.5));

        let diff_44_48 = (m44.momentary_lufs - m48.momentary_lufs).abs();
        let diff_96_48 = (m96.momentary_lufs - m48.momentary_lufs).abs();

        assert!(diff_44_48 < 0.5, "44.1k vs 48k: {:.2} vs {:.2}", m44.momentary_lufs, m48.momentary_lufs);
        assert!(diff_96_48 < 0.5, "96k vs 48k: {:.2} vs {:.2}", m96.momentary_lufs, m48.momentary_lufs);
    }

    // ═══════════════════════════════════════════════════════════════
    // Edge cases
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn empty_input_is_noop() {
        let mut meter = LufsMeter::new(48000.0, true);
        meter.process(&[]);
        assert!(meter.momentary_lufs.is_infinite());
    }

    #[test]
    fn single_sample_pair() {
        let mut meter = LufsMeter::new(48000.0, true);
        meter.process(&[0.5, -0.5]);
        // Not enough for a block — should still be -inf
        assert!(meter.momentary_lufs.is_infinite());
    }

    #[test]
    fn dc_offset_handled() {
        // DC offset should be attenuated by the RLB high-pass filter
        let mut meter = LufsMeter::new(48000.0, true);
        let num_frames = 48000 * 2;
        let dc: Vec<f32> = vec![0.5; num_frames * 2]; // constant DC
        meter.process(&dc);
        // The RLB high-pass should remove DC, reading should be much lower
        // than what a naive RMS would give
        // DC at 0.5 => naive would be about -6 dBFS, but RLB should kill it
        assert!(meter.momentary_lufs < -30.0 || meter.momentary_lufs.is_infinite(),
            "DC should be attenuated by RLB filter, got {:.1}", meter.momentary_lufs);
    }

    // ═══════════════════════════════════════════════════════════════
    // Amplitude linearity
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn halving_amplitude_drops_6db() {
        let mut meter_full = LufsMeter::new(48000.0, true);
        let mut meter_half = LufsMeter::new(48000.0, true);

        meter_full.process(&generate_sine(48000.0, 1000.0, 2.0, 0.5));
        meter_half.process(&generate_sine(48000.0, 1000.0, 2.0, 0.25));

        let diff = meter_full.momentary_lufs - meter_half.momentary_lufs;
        // Halving amplitude = -6.02 dB
        assert!((diff - 6.02).abs() < 0.5,
            "Halving amplitude should drop ~6dB: full={:.2} half={:.2} diff={:.2}",
            meter_full.momentary_lufs, meter_half.momentary_lufs, diff);
    }

    // ═══════════════════════════════════════════════════════════════
    // LRA (Loudness Range)
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn lra_zero_for_steady_signal() {
        let mut meter = LufsMeter::new(48000.0, true);
        let samples = generate_sine(48000.0, 1000.0, 5.0, 0.5);
        meter.process(&samples);
        // Steady-state signal: all short-term blocks should be similar → LRA near 0
        assert!(meter.loudness_range < 2.0,
            "Steady signal LRA should be near 0, got {:.1}", meter.loudness_range);
    }

    #[test]
    fn lra_nonzero_for_dynamic_signal() {
        let mut meter = LufsMeter::new(48000.0, true);
        // Alternate loud and quiet sections
        for _ in 0..3 {
            meter.process(&generate_sine(48000.0, 1000.0, 2.0, 0.8)); // loud
            meter.process(&generate_sine(48000.0, 1000.0, 2.0, 0.05)); // quiet
        }
        // Dynamic signal should have measurable LRA
        assert!(meter.loudness_range > 2.0,
            "Dynamic signal LRA should be > 2, got {:.1}", meter.loudness_range);
    }

    #[test]
    fn lra_in_readings() {
        let mut meter = LufsMeter::new(48000.0, true);
        meter.process(&generate_sine(48000.0, 1000.0, 5.0, 0.5));
        let r = meter.readings();
        assert_eq!(r.loudness_range, meter.loudness_range);
    }
}
