//! Brick-wall limiter with lookahead, attack/release smoothing, input/output
//! gain, ceiling clip, wet/dry mix, and per-block input + output peak meters.
//!
//! Forked from Tona Studio's `tona_dsp::Limiter` and re-skinned under Klipp's
//! [`AudioEffect`] trait. Same `type_id: "limiter"` and constructor signature
//! so the existing inspector / effect-registry / saved-project entries
//! continue to refer to it without a migration.
//!
//! # What changed vs. the pre-port limiter
//!
//! * **Lookahead.** A configurable 0–20 ms delay buffer; the gain reduction
//!   is computed from the *current* input but applied to the *delayed*
//!   output, so the envelope is already pulled down by the time a fast
//!   transient reaches the output. Closes the long-standing limitation in
//!   the previous implementation's doc comment ("very fast transients can
//!   still clip the ceiling on the first sample of an attack").
//! * **Threshold ≠ ceiling.** Threshold drives the gain-reduction envelope;
//!   ceiling is a separate hard clamp on the post-gain output. This matches
//!   modern brick-wall limiter conventions and lets a user run e.g. 6 dB of
//!   gain reduction below threshold while still hard-stopping at -0.3 dBFS.
//! * **Input + output gain stages** so the limiter can be used as a loudness
//!   maximiser without a separate gain plugin in front / behind.
//! * **Attack** time in addition to release.
//! * **Wet/dry mix** for parallel-style limiting.
//! * **True peak** (#2560). The envelope follows the peaks between samples
//!   too, as a 4× oversampled true-peak meter reads them, and the ceiling
//!   is the last stage, after the output gain and the wet/dry mix: it
//!   holds on what the limiter puts out. The output gain and the mix used
//!   to come after it, and a peak between two samples passed it.
//! * **Per-block peak meters** (input + output, L + R) for inspector
//!   metering. Read with [`Limiter::input_peak_l`] etc. and reset with
//!   [`Limiter::reset_peaks`] before processing each block.
//!
//! # Channel handling
//!
//! Lookahead buffers exist for the first two channels (the realistic case
//! for an NLE — output is stereo end-to-end). For mono, only the L buffer
//! is used. For 3+ channels, gain is applied uniformly across every
//! channel based on the L+R peak detector, but the >2 channels are not
//! delayed — acceptable for the rare case and matches the pre-port
//! arbitrary-channel-count behaviour.

use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

const MAX_LOOKAHEAD_MS: f32 = 20.0;
/// How many frames the true peak is held for.
const PEAK_HOLD: usize = 8;

#[inline]
fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

pub struct Limiter {
    sample_rate: f32,
    // Stored param values (dB / ms / 0..1) for serialise + UI display.
    threshold_db: f32,
    ceiling_db: f32,
    input_gain_db: f32,
    output_gain_db: f32,
    attack_ms: f32,
    release_ms: f32,
    lookahead_ms: f32,
    mix: f32,
    // Derived runtime state.
    threshold_lin: f32,
    ceiling_lin: f32,
    input_gain_lin: f32,
    output_gain_lin: f32,
    attack_coeff: f32,
    release_coeff: f32,
    gain_reduction: f32,
    lookahead_buf_l: Vec<f32>,
    lookahead_buf_r: Vec<f32>,
    lookahead_pos: usize,
    lookahead_samples: usize,
    /// The input's true peak, as a 4× oversampled meter reads it: the
    /// peaks between samples, which the envelope follows too.
    true_peak: crate::TruePeakDetector,
    /// Its last few readings: each gives only the points between samples
    /// its frame completes, so the envelope follows their maximum and does
    /// not release between a waveform's peaks.
    recent_peaks: [f32; PEAK_HOLD],
    recent_pos: usize,
    /// Whether the envelope follows the true peak (the default) or the
    /// samples alone: see [`Limiter::follow_sample_peaks`].
    sample_peaks_only: bool,
    // Per-block peak meters (linear, post the corresponding gain stage).
    input_peak_l: f32,
    input_peak_r: f32,
    output_peak_l: f32,
    output_peak_r: f32,
    dirty: bool,
}

impl Limiter {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut l = Self {
            sample_rate: sr,
            threshold_db: -1.0,
            ceiling_db: -0.3,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            lookahead_ms: 2.0,
            mix: 1.0,
            threshold_lin: 1.0,
            ceiling_lin: 1.0,
            input_gain_lin: 1.0,
            output_gain_lin: 1.0,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            gain_reduction: 1.0,
            lookahead_buf_l: Vec::new(),
            lookahead_buf_r: Vec::new(),
            lookahead_pos: 0,
            lookahead_samples: 0,
            true_peak: crate::TruePeakDetector::new(sample_rate),
            recent_peaks: [0.0; PEAK_HOLD],
            recent_pos: 0,
            sample_peaks_only: false,
            input_peak_l: 0.0,
            input_peak_r: 0.0,
            output_peak_l: 0.0,
            output_peak_r: 0.0,
            dirty: true,
        };
        l.recompute();
        l
    }

    fn recompute(&mut self) {
        self.threshold_lin = db_to_linear(self.threshold_db);
        self.ceiling_lin = db_to_linear(self.ceiling_db);
        self.input_gain_lin = db_to_linear(self.input_gain_db);
        self.output_gain_lin = db_to_linear(self.output_gain_db);
        // exp(-1 / (tau_seconds * sample_rate)) one-pole time constant.
        let attack_s = self.attack_ms.max(0.01) * 0.001;
        let release_s = self.release_ms.max(1.0) * 0.001;
        self.attack_coeff = (-1.0 / (attack_s * self.sample_rate)).exp();
        self.release_coeff = (-1.0 / (release_s * self.sample_rate)).exp();

        // Lookahead buffer resize. Only reallocate when the sample count
        // actually changes, so a no-op recompute (e.g. after toggling
        // ceiling) doesn't wipe the in-flight delay line.
        let new_samples =
            (self.lookahead_ms.clamp(0.0, MAX_LOOKAHEAD_MS) * 0.001 * self.sample_rate) as usize;
        if new_samples != self.lookahead_samples {
            self.lookahead_samples = new_samples;
            if new_samples > 0 {
                self.lookahead_buf_l.resize(new_samples, 0.0);
                self.lookahead_buf_r.resize(new_samples, 0.0);
                self.lookahead_buf_l.fill(0.0);
                self.lookahead_buf_r.fill(0.0);
            } else {
                self.lookahead_buf_l.clear();
                self.lookahead_buf_r.clear();
            }
            self.lookahead_pos = 0;
        }
        self.dirty = false;
    }

    /// Follow the samples' peaks alone, as before #2560: for the delivery
    /// chain, which measures the true peak of each trial and fits its own
    /// margin under the ceiling for the peaks between samples.
    pub fn follow_sample_peaks(&mut self) {
        self.sample_peaks_only = true;
    }

    /// Current gain reduction applied to the signal, expressed in dB
    /// (negative = reducing). 0 dB = passthrough. Read from the inspector
    /// to drive the gain-reduction meter.
    pub fn gain_reduction_db(&self) -> f32 {
        20.0 * self.gain_reduction.max(1e-12).log10()
    }

    /// Reset the per-block peak accumulators. Call once at the top of each
    /// inspector-paint block so the meter shows the most recent block's
    /// peak rather than accumulating forever.
    pub fn reset_peaks(&mut self) {
        self.input_peak_l = 0.0;
        self.input_peak_r = 0.0;
        self.output_peak_l = 0.0;
        self.output_peak_r = 0.0;
    }

    pub fn input_peak_l(&self) -> f32 {
        self.input_peak_l
    }
    pub fn input_peak_r(&self) -> f32 {
        self.input_peak_r
    }
    pub fn output_peak_l(&self) -> f32 {
        self.output_peak_l
    }
    pub fn output_peak_r(&self) -> f32 {
        self.output_peak_r
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "threshold",   label: "Threshold",   default: -1.0, min: -24.0, max: 0.0,             unit: ParamUnit::Db },
    AudioParamDescriptor { name: "ceiling",     label: "Ceiling",     default: -0.3, min: -12.0, max: 0.0,             unit: ParamUnit::Db },
    AudioParamDescriptor { name: "input_gain",  label: "Input Gain",  default: 0.0,  min: -24.0, max: 24.0,            unit: ParamUnit::Db },
    AudioParamDescriptor { name: "output_gain", label: "Output Gain", default: 0.0,  min: -24.0, max: 24.0,            unit: ParamUnit::Db },
    AudioParamDescriptor { name: "attack",      label: "Attack",      default: 1.0,  min: 0.1,   max: 100.0,           unit: ParamUnit::Ms },
    AudioParamDescriptor { name: "release",     label: "Release",     default: 50.0, min: 5.0,   max: 500.0,           unit: ParamUnit::Ms },
    AudioParamDescriptor { name: "lookahead",   label: "Lookahead",   default: 2.0,  min: 0.0,   max: MAX_LOOKAHEAD_MS, unit: ParamUnit::Ms },
    AudioParamDescriptor { name: "mix",         label: "Mix",         default: 1.0,  min: 0.0,   max: 1.0,             unit: ParamUnit::None },
];

impl AudioEffect for Limiter {
    fn type_id(&self) -> &'static str { "limiter" }
    fn name(&self) -> &'static str { "Limiter" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "threshold"   => Some(self.threshold_db),
            "ceiling"     => Some(self.ceiling_db),
            "input_gain"  => Some(self.input_gain_db),
            "output_gain" => Some(self.output_gain_db),
            "attack"      => Some(self.attack_ms),
            "release"     => Some(self.release_ms),
            "lookahead"   => Some(self.lookahead_ms),
            "mix"         => Some(self.mix),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "threshold"   => self.threshold_db   = value.clamp(-24.0, 0.0),
            "ceiling"     => self.ceiling_db     = value.clamp(-12.0, 0.0),
            "input_gain"  => self.input_gain_db  = value.clamp(-24.0, 24.0),
            "output_gain" => self.output_gain_db = value.clamp(-24.0, 24.0),
            "attack"      => self.attack_ms     = value.clamp(0.1, 100.0),
            "release"     => self.release_ms    = value.clamp(5.0, 500.0),
            "lookahead"   => self.lookahead_ms  = value.clamp(0.0, MAX_LOOKAHEAD_MS),
            "mix"         => self.mix           = value.clamp(0.0, 1.0),
            _ => return,
        }
        self.dirty = true;
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        if self.dirty {
            self.recompute();
        }
        let channels = ctx.channels as usize;
        if channels == 0 {
            return;
        }
        let num_frames = buffer.len() / channels;

        for frame in 0..num_frames {
            let base = frame * channels;
            // Read L (and R if present) from the buffer, apply input gain.
            let raw_l = buffer[base];
            let raw_r = if channels >= 2 { buffer[base + 1] } else { raw_l };
            let in_l = raw_l * self.input_gain_lin;
            let in_r = raw_r * self.input_gain_lin;
            self.input_peak_l = self.input_peak_l.max(in_l.abs());
            self.input_peak_r = self.input_peak_r.max(in_r.abs());

            // Lookahead split. Peak detection is on the *current* sample;
            // the audio path emits the *delayed* sample, so the gain is
            // already pulled down by the time the transient lands.
            let (delayed_l, delayed_r) = if self.lookahead_samples > 0 {
                let dl = self.lookahead_buf_l[self.lookahead_pos];
                let dr = self.lookahead_buf_r[self.lookahead_pos];
                self.lookahead_buf_l[self.lookahead_pos] = in_l;
                self.lookahead_buf_r[self.lookahead_pos] = in_r;
                self.lookahead_pos = (self.lookahead_pos + 1) % self.lookahead_samples;
                (dl, dr)
            } else {
                (in_l, in_r)
            };

            // Peak detector + envelope smoothing. Asymmetric: snap on
            // attack (gain falling), one-pole release (gain returning).
            // The peak is the true one, the oversampled meter's, which
            // comes a few samples after the input: the lookahead covers it.
            let peak = if self.sample_peaks_only {
                in_l.abs().max(in_r.abs())
            } else {
                self.recent_peaks[self.recent_pos] = in_l.abs().max(in_r.abs()).max(self.true_peak.process(in_l, in_r));
                self.recent_pos = (self.recent_pos + 1) % PEAK_HOLD;
                self.recent_peaks.iter().copied().fold(0.0f32, f32::max)
            };
            // Down to the threshold, and far enough that the true peak,
            // after the output gain, stays under the ceiling: a clamp of
            // the samples alone lets the peaks between them over it.
            let to_threshold = if peak > self.threshold_lin { self.threshold_lin / peak } else { 1.0 };
            let out_peak = peak * self.output_gain_lin;
            let to_ceiling = if out_peak > self.ceiling_lin { self.ceiling_lin / out_peak } else { 1.0 };
            let target = to_threshold.min(to_ceiling);
            self.gain_reduction = if target < self.gain_reduction {
                target + self.attack_coeff * (self.gain_reduction - target)
            } else {
                target + self.release_coeff * (self.gain_reduction - target)
            };

            // Apply gain reduction → output gain → wet/dry → ceiling: the
            // ceiling last, so it holds on what goes out.
            let mut out_l = delayed_l * self.gain_reduction * self.output_gain_lin;
            let mut out_r = delayed_r * self.gain_reduction * self.output_gain_lin;

            // Wet/dry blend against the *original* input (pre input-gain)
            // so mix=0 is a true bypass.
            if self.mix < 1.0 {
                let dry = 1.0 - self.mix;
                out_l = raw_l * dry + out_l * self.mix;
                out_r = raw_r * dry + out_r * self.mix;
            }
            out_l = out_l.clamp(-self.ceiling_lin, self.ceiling_lin);
            out_r = out_r.clamp(-self.ceiling_lin, self.ceiling_lin);

            self.output_peak_l = self.output_peak_l.max(out_l.abs());
            self.output_peak_r = self.output_peak_r.max(out_r.abs());

            // Write back. Channels 0/1 take the limited L/R; channels 2+
            // take the same gain reduction (no per-channel lookahead
            // buffer, but they ride the L+R envelope).
            buffer[base] = out_l;
            if channels >= 2 {
                buffer[base + 1] = out_r;
            }
            for ch in 2..channels {
                let s = buffer[base + ch] * self.input_gain_lin;
                let limited = (s * self.gain_reduction)
                    .clamp(-self.ceiling_lin, self.ceiling_lin)
                    * self.output_gain_lin;
                buffer[base + ch] = if self.mix < 1.0 {
                    buffer[base + ch] * (1.0 - self.mix) + limited * self.mix
                } else {
                    limited
                };
            }
        }
    }

    fn reset(&mut self) {
        self.gain_reduction = 1.0;
        self.lookahead_buf_l.fill(0.0);
        self.lookahead_buf_r.fill(0.0);
        self.lookahead_pos = 0;
        self.true_peak = crate::TruePeakDetector::new(self.sample_rate as u32);
        self.recent_peaks = [0.0; PEAK_HOLD];
        self.reset_peaks();
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("threshold".into(),   self.threshold_db),
            ("ceiling".into(),     self.ceiling_db),
            ("input_gain".into(),  self.input_gain_db),
            ("output_gain".into(), self.output_gain_db),
            ("attack".into(),      self.attack_ms),
            ("release".into(),     self.release_ms),
            ("lookahead".into(),   self.lookahead_ms),
            ("mix".into(),         self.mix),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params {
            self.set_param(name, *value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(sr: u32, ch: u32) -> AudioProcessContext {
        AudioProcessContext { sample_rate: sr, channels: ch, clip_time_secs: 0.0 }
    }

    /// type_id stays "limiter" so the existing UI callers (timeline.rs:69,
    /// audio_fx_tab.rs:33/68, effects_tab.rs:1743, effect_registry.rs:481)
    /// resolve without a migration.
    #[test]
    fn type_id_and_name_are_stable() {
        let l = Limiter::new(48_000);
        assert_eq!(l.type_id(), "limiter");
        assert_eq!(l.name(), "Limiter");
    }

    #[test]
    fn defaults_match_param_descriptors() {
        let l = Limiter::new(48_000);
        for d in PARAM_DESCS {
            let actual = l.get_param(d.name).unwrap_or_else(|| panic!("param {} missing", d.name));
            assert!(
                (actual - d.default).abs() < 1e-6,
                "param {} default {} != descriptor {}", d.name, actual, d.default,
            );
        }
    }

    /// Old-format saved projects only carried `ceiling` + `release` from the
    /// pre-port limiter. Round-trip those two and assert: (a) they
    /// deserialise into the new fields, (b) the eight-param serialise still
    /// emits all eight (so the project file gets upgraded transparently on
    /// next save).
    #[test]
    fn old_two_param_state_deserialises_into_new_fields() {
        let mut l = Limiter::new(48_000);
        l.deserialize_params(&[
            ("ceiling".into(), -1.5),
            ("release".into(), 75.0),
        ]);
        assert!((l.get_param("ceiling").unwrap() - (-1.5)).abs() < 1e-6);
        assert!((l.get_param("release").unwrap() - 75.0).abs() < 1e-6);
        // New params keep their defaults.
        assert!((l.get_param("threshold").unwrap() - (-1.0)).abs() < 1e-6);
        assert!((l.get_param("lookahead").unwrap() - 2.0).abs() < 1e-6);
        // Resave emits the full eight-param set.
        let saved = l.serialize_params();
        assert_eq!(saved.len(), PARAM_DESCS.len());
    }

    /// The whole reason for the port. Push a sub-millisecond click 3 dB
    /// above the ceiling; with lookahead the *output* must already be
    /// clamped to the ceiling on that very sample. Without lookahead the
    /// pre-port limiter let the first sample of an attack through (the
    /// "very fast transients can still clip" note in the original doc).
    #[test]
    fn lookahead_catches_transient_on_first_sample() {
        let mut l = Limiter::new(48_000);
        l.set_param("threshold", -3.0);
        l.set_param("ceiling", -0.3);
        l.set_param("attack", 1.0);
        l.set_param("lookahead", 2.0);
        l.set_param("mix", 1.0);
        l.recompute();

        // Prime the lookahead pipeline with silence.
        let mut warmup = vec![0.0_f32; 2 * 48 * 4]; // 4 ms stereo
        l.process(&mut warmup, &ctx(48_000, 2));

        // Single transient sample at 0 dBFS, surrounded by silence.
        let mut block = vec![0.0_f32; 2 * 48 * 4];
        block[0] = 1.0;
        block[1] = 1.0;
        l.process(&mut block, &ctx(48_000, 2));

        let ceiling_lin = db_to_linear(-0.3);
        let output_peak = block.iter().fold(0.0_f32, |a, &b| a.max(b.abs()));
        assert!(
            output_peak <= ceiling_lin + 1e-4,
            "lookahead failed: output peak {} exceeds ceiling {}", output_peak, ceiling_lin,
        );
    }

    /// Below threshold, the limiter must be transparent — the dry signal
    /// passes through gain stages and ceiling clamp untouched.
    #[test]
    fn passthrough_below_threshold() {
        let mut l = Limiter::new(48_000);
        l.set_param("threshold", -1.0);
        l.set_param("ceiling", -0.3);
        l.set_param("lookahead", 0.0); // no delay, easier to compare frame-aligned
        l.recompute();

        let half_ceiling = db_to_linear(-6.3);
        let mut block = vec![half_ceiling; 2 * 64];
        l.process(&mut block, &ctx(48_000, 2));
        for s in &block {
            assert!((s - half_ceiling).abs() < 1e-4, "passthrough sample drifted: {s}");
        }
    }

    /// Mix=0 must be byte-stable bypass — the output equals the original
    /// input regardless of any limiter settings. Catches a regression class
    /// where a future change accidentally lifts the dry signal through the
    /// input-gain stage (would have broken the parallel-style use case).
    #[test]
    fn mix_zero_is_bypass() {
        let mut l = Limiter::new(48_000);
        l.set_param("input_gain", 6.0);
        l.set_param("output_gain", -6.0);
        l.set_param("threshold", -12.0);
        l.set_param("mix", 0.0);
        l.recompute();

        let mut block: Vec<f32> = (0..2 * 256).map(|i| (i as f32 / 256.0).sin()).collect();
        let original = block.clone();
        l.process(&mut block, &ctx(48_000, 2));
        // The input untouched by the gains, the gain reduction or the mix:
        // only the ceiling, which is last and holds on what goes out (#2560).
        let ceiling = db_to_linear(-0.3);
        for (a, b) in block.iter().zip(original.iter()) {
            assert!((a - b.clamp(-ceiling, ceiling)).abs() < 1e-4, "mix=0 not bypass: {a} vs {b}");
        }
    }

    /// #2560: the ceiling holds on what goes out, after the output gain
    /// and the mix, and on the peaks between samples: a tone near half
    /// the rate, its samples well under the ceiling and its peaks between
    /// them over it, comes out under it as a true-peak meter reads it.
    #[test]
    fn the_ceiling_holds_after_the_output_gain_and_between_samples() {
        let mut l = Limiter::new(48_000);
        l.set_param("threshold", -3.0);
        l.set_param("ceiling", -2.0);
        l.set_param("output_gain", 6.0);
        l.recompute();
        // A tone at a quarter of the rate, a quarter-cycle off the samples:
        // each sample at 0.707 of the peak.
        let mut block: Vec<f32> = (0..2 * 4800).map(|i| 0.95 * (std::f32::consts::FRAC_PI_2 * (i / 2) as f32 + std::f32::consts::FRAC_PI_4).sin()).collect();
        l.process(&mut block, &ctx(48_000, 2));
        let mut tp = crate::TruePeakDetector::new(48_000);
        let peak = block.chunks(2).skip(480).map(|s| tp.process(s[0], s[1])).fold(0.0f32, f32::max);
        let db = 20.0 * peak.log10();
        assert!(db <= -2.0 + 0.2, "{db:.2} dBTP over a -2 dB ceiling");
    }

    /// reset() must clear the lookahead buffer and the gain-reduction
    /// envelope so a re-armed playback doesn't leak the previous block's
    /// trailing transient into silence.
    #[test]
    fn reset_clears_lookahead_and_envelope() {
        let mut l = Limiter::new(48_000);
        l.set_param("lookahead", 2.0);
        l.set_param("threshold", -3.0);
        l.recompute();

        let mut hot = vec![0.9_f32; 2 * 256];
        l.process(&mut hot, &ctx(48_000, 2));
        assert!(l.gain_reduction < 1.0, "limiter should have engaged");

        l.reset();
        assert!((l.gain_reduction - 1.0).abs() < 1e-6);
        assert!(l.lookahead_buf_l.iter().all(|&s| s == 0.0));
        assert!(l.lookahead_buf_r.iter().all(|&s| s == 0.0));
    }

    /// Mono input must run through the same gain-reduction envelope without
    /// touching uninitialised channels — guards the `if channels >= 2`
    /// branches in process().
    #[test]
    fn mono_processing_does_not_panic_or_drift() {
        let mut l = Limiter::new(48_000);
        l.set_param("threshold", -3.0);
        l.set_param("lookahead", 1.0);
        l.recompute();

        let mut block = vec![0.5_f32; 256];
        l.process(&mut block, &ctx(48_000, 1));
        assert!(block.iter().all(|s| s.is_finite()));
    }

    /// Per-block peak meters track the current block's input/output
    /// extremes and reset cleanly. Inspector relies on this to repaint a
    /// fresh meter every block.
    #[test]
    fn peak_meters_track_and_reset() {
        let mut l = Limiter::new(48_000);
        l.set_param("threshold", -1.0);
        l.set_param("ceiling", -0.3);
        l.set_param("lookahead", 0.0);
        l.recompute();

        let mut block = vec![0.9_f32; 2 * 128];
        l.process(&mut block, &ctx(48_000, 2));
        assert!(l.input_peak_l() > 0.8);
        assert!(l.input_peak_r() > 0.8);
        assert!(l.output_peak_l() > 0.0);

        l.reset_peaks();
        assert_eq!(l.input_peak_l(), 0.0);
        assert_eq!(l.output_peak_l(), 0.0);
    }
}
