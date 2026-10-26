use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use std::f32::consts::PI;

const MAX_CHANNELS: usize = 2;
/// Below this level (-60 dBFS) the signal is silence, not a voice.
const SILENCE: f32 = 0.001;

/// Biquad coefficients for bandpass detection filter.
#[derive(Debug, Clone, Copy)]
struct BiquadCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Default for BiquadCoeffs {
    fn default() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    z1: f32,
    z2: f32,
}

/// Frequency-selective compressor targeting sibilance (4-10 kHz).
/// Uses a bandpass filter to detect sibilance, then applies split-band
/// gain reduction only to the sibilant frequency range.
pub struct DeEsser {
    sample_rate: f32,
    frequency: f32,    // Hz (2000-12000)
    threshold: f32,    // dB (-40 to 0)
    reduction: f32,    // dB (0-12)
    bandwidth: f32,    // Q (0.5-5)
    // Bandpass filter for sibilance detection
    bp_coeffs: BiquadCoeffs,
    bp_state: [BiquadState; MAX_CHANNELS],
    // Envelope followers
    envelope: [f32; MAX_CHANNELS],
    /// The whole signal's level, which the sibilance is judged against.
    voice_envelope: [f32; MAX_CHANNELS],
    release_coeff: f32,
    voice_release: f32,
    threshold_linear: f32,
    reduction_linear: f32,
    dirty: bool,
}

impl DeEsser {
    pub fn new(sample_rate: u32) -> Self {
        let mut ds = Self {
            sample_rate: sample_rate as f32,
            frequency: 6000.0,
            threshold: -20.0,
            reduction: 6.0,
            bandwidth: 1.0,
            bp_coeffs: BiquadCoeffs::default(),
            bp_state: [BiquadState::default(); MAX_CHANNELS],
            envelope: [0.0; MAX_CHANNELS],
            voice_envelope: [0.0; MAX_CHANNELS],
            release_coeff: 0.0,
            voice_release: 0.0,
            threshold_linear: 0.0,
            reduction_linear: 0.0,
            dirty: true,
        };
        ds.recompute();
        ds
    }

    fn recompute(&mut self) {
        // Bandpass filter (BPF from RBJ cookbook)
        let w0 = 2.0 * PI * self.frequency / self.sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * self.bandwidth);

        let b0 = alpha;
        let b2 = -alpha;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;

        let inv_a0 = 1.0 / a0;
        self.bp_coeffs = BiquadCoeffs {
            b0: b0 * inv_a0,
            b1: 0.0, // b1 is always 0 for bandpass
            b2: b2 * inv_a0,
            a1: a1 * inv_a0,
            a2: a2 * inv_a0,
        };

        // Fast release for de-essing (~5ms)
        self.release_coeff = (-1.0 / (0.005 * self.sample_rate)).exp();
        self.voice_release = (-1.0 / (0.030 * self.sample_rate)).exp();
        self.threshold_linear = db_to_linear(self.threshold);
        self.reduction_linear = db_to_linear(-self.reduction);
        self.dirty = false;
    }
}

#[inline]
fn process_biquad(sample: f32, coeffs: &BiquadCoeffs, state: &mut BiquadState) -> f32 {
    let out = coeffs.b0 * sample + state.z1;
    state.z1 = coeffs.b1 * sample - coeffs.a1 * out + state.z2;
    state.z2 = coeffs.b2 * sample - coeffs.a2 * out;
    out
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "frequency", label: "Frequency", default: 6000.0, min: 2000.0, max: 12000.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "threshold", label: "Threshold", default: -20.0, min: -40.0, max: 0.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "reduction", label: "Reduction", default: 6.0, min: 0.0, max: 12.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "bandwidth", label: "Bandwidth", default: 1.0, min: 0.5, max: 5.0, unit: ParamUnit::None },
];

impl AudioEffect for DeEsser {
    fn type_id(&self) -> &'static str { "deesser" }
    fn name(&self) -> &'static str { "De-Esser" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "frequency" => Some(self.frequency),
            "threshold" => Some(self.threshold),
            "reduction" => Some(self.reduction),
            "bandwidth" => Some(self.bandwidth),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "frequency" => self.frequency = value.clamp(2000.0, 12000.0),
            "threshold" => self.threshold = value.clamp(-40.0, 0.0),
            "reduction" => self.reduction = value.clamp(0.0, 12.0),
            "bandwidth" => self.bandwidth = value.clamp(0.5, 5.0),
            _ => return,
        }
        self.dirty = true;
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        if self.dirty {
            self.recompute();
        }

        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;
        let thresh = self.threshold_linear;
        let max_reduction = self.reduction_linear;

        for frame in 0..num_frames {
            for ch in 0..channels.min(MAX_CHANNELS) {
                let idx = frame * channels + ch;
                let input = buffer[idx];

                // Bandpass filter to isolate sibilance band
                let sibilance = process_biquad(input, &self.bp_coeffs, &mut self.bp_state[ch]);
                let sib_level = sibilance.abs();

                // Envelope follower: instant attack, fast release
                let env = &mut self.envelope[ch];
                if sib_level > *env {
                    *env = sib_level;
                } else {
                    *env = self.release_coeff * *env + (1.0 - self.release_coeff) * sib_level;
                }
                let voice = &mut self.voice_envelope[ch];
                let level = input.abs();
                *voice = if level > *voice { level } else { self.voice_release * *voice + (1.0 - self.voice_release) * level };

                // Split-band and relative (#2778): the band is turned down
                // by how far it stands above Threshold relative to the
                // whole voice, at most Reduction, so a voice's own air
                // stays and only an "s" that stands out from it is calmed,
                // whatever the voice's level. Near silence is left alone.
                if *env > thresh * *voice && *voice > SILENCE {
                    let excess = *env / (thresh * *voice);
                    let gain = (1.0 / excess).max(max_reduction);
                    buffer[idx] = input - sibilance + sibilance * gain;
                }
            }
        }
    }

    fn reset(&mut self) {
        self.bp_state = [BiquadState::default(); MAX_CHANNELS];
        self.envelope = [0.0; MAX_CHANNELS];
        self.voice_envelope = [0.0; MAX_CHANNELS];
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("frequency".into(), self.frequency),
            ("threshold".into(), self.threshold),
            ("reduction".into(), self.reduction),
            ("bandwidth".into(), self.bandwidth),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params {
            self.set_param(name, *value);
        }
    }
}

#[inline]
fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}
