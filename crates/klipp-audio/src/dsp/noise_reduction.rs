use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use std::f32::consts::PI;

const NUM_BANDS: usize = 8;
const MAX_CHANNELS: usize = 2;

/// Biquad filter coefficients (Direct Form II Transposed).
#[derive(Clone, Copy)]
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

#[derive(Clone, Copy, Default)]
struct BiquadState {
    z1: f32,
    z2: f32,
}

/// Per-band state for the multi-band gate.
struct BandState {
    bp_coeffs: BiquadCoeffs,
    bp_state: [BiquadState; MAX_CHANNELS],
    envelope: [f32; MAX_CHANNELS],
}

/// Multi-band gate.
///
/// Splits the signal into 8 frequency bands via biquad bandpass filters,
/// then gates each band independently based on an envelope follower.
/// Allocation-free in `process()`.
///
/// NOTE — this is NOT spectral-subtraction noise reduction. It has no
/// noise-profile capture step, and bandpass filter banks don't sum
/// flat: even at zero "reduction" the output has frequency-response
/// ripple from the bandpass overlap. The Rust struct name and
/// `type_id` remain `NoiseReduction` / `"noise_reduction"` so existing
/// .klipp projects keep loading; the user-facing `name()` advertises
/// the honest behaviour ("Multi-band Gate").
/// Replacing this with STFT spectral subtraction is a future job.
pub struct NoiseReduction {
    sample_rate: f32,
    reduction: f32,
    sensitivity: f32,
    smoothing: f32,
    bands: [BandState; NUM_BANDS],
    /// Pre-allocated scratch buffer to hold a copy of the input signal.
    /// Sized for 2048-frame stereo chunks (matches DSP batch size).
    input_copy: Vec<f32>,
}

/// Band center frequencies (logarithmically spaced across audible range).
const BAND_FREQS: [f32; NUM_BANDS] = [
    100.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 14000.0,
];

fn compute_bandpass(freq: f32, q: f32, sr: f32) -> BiquadCoeffs {
    let w0 = 2.0 * PI * freq / sr;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    let alpha = sin_w0 / (2.0 * q);

    let b0 = alpha;
    let b1 = 0.0;
    let b2 = -alpha;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha;

    let inv_a0 = 1.0 / a0;
    BiquadCoeffs {
        b0: b0 * inv_a0,
        b1: b1 * inv_a0,
        b2: b2 * inv_a0,
        a1: a1 * inv_a0,
        a2: a2 * inv_a0,
    }
}

#[inline]
fn process_biquad(sample: f32, coeffs: &BiquadCoeffs, state: &mut BiquadState) -> f32 {
    let out = coeffs.b0 * sample + state.z1;
    state.z1 = coeffs.b1 * sample - coeffs.a1 * out + state.z2;
    state.z2 = coeffs.b2 * sample - coeffs.a2 * out;
    out
}

impl NoiseReduction {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let bands = std::array::from_fn(|i| BandState {
            bp_coeffs: compute_bandpass(BAND_FREQS[i], 1.4, sr),
            bp_state: [BiquadState::default(); MAX_CHANNELS],
            envelope: [0.0; MAX_CHANNELS],
        });
        Self {
            sample_rate: sr,
            reduction: 12.0,
            sensitivity: 50.0,
            smoothing: 50.0,
            bands,
            input_copy: vec![0.0; 2048 * 2],
        }
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "reduction", label: "Reduction", default: 12.0, min: 0.0, max: 40.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "sensitivity", label: "Sensitivity", default: 50.0, min: 0.0, max: 100.0, unit: ParamUnit::Percent },
    AudioParamDescriptor { name: "smoothing", label: "Smoothing", default: 50.0, min: 0.0, max: 100.0, unit: ParamUnit::Percent },
];

impl AudioEffect for NoiseReduction {
    fn type_id(&self) -> &'static str { "noise_reduction" }
    fn name(&self) -> &'static str { "Multi-band Gate" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "reduction" => Some(self.reduction),
            "sensitivity" => Some(self.sensitivity),
            "smoothing" => Some(self.smoothing),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "reduction" => self.reduction = value.clamp(0.0, 40.0),
            "sensitivity" => self.sensitivity = value.clamp(0.0, 100.0),
            "smoothing" => self.smoothing = value.clamp(0.0, 100.0),
            _ => {}
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;
        if num_frames == 0 { return; }

        // Grow scratch buffer if needed (never allocates in steady state)
        if self.input_copy.len() < buffer.len() {
            self.input_copy.resize(buffer.len(), 0.0);
        }

        // Save original input
        self.input_copy[..buffer.len()].copy_from_slice(buffer);

        // Threshold: sensitivity maps 0..100% to -80..-20 dB as linear amplitude
        let threshold_db = -80.0 + self.sensitivity * 0.6; // 0%=-80dB, 100%=-20dB
        let threshold_linear = 10.0_f32.powf(threshold_db / 20.0);

        // Reduction: how much to attenuate gated bands
        let reduction_linear = 10.0_f32.powf(-self.reduction / 20.0);

        // Envelope coefficients
        let smooth_factor = self.smoothing / 100.0;
        let attack_ms = 1.0 + smooth_factor * 20.0;
        let release_ms = 10.0 + smooth_factor * 200.0;
        let attack_coeff = (-1.0 / (attack_ms * 0.001 * self.sample_rate)).exp();
        let release_coeff = (-1.0 / (release_ms * 0.001 * self.sample_rate)).exp();

        // Zero output buffer — we accumulate gated band signals into it
        for s in buffer.iter_mut() {
            *s = 0.0;
        }

        for band in &mut self.bands {
            for frame in 0..num_frames {
                for ch in 0..channels.min(MAX_CHANNELS) {
                    let idx = frame * channels + ch;
                    let input = self.input_copy[idx];

                    // Bandpass filter this sample
                    let filtered = process_biquad(input, &band.bp_coeffs, &mut band.bp_state[ch]);

                    // Envelope follower on the filtered band
                    let abs_val = filtered.abs();
                    let coeff = if abs_val > band.envelope[ch] {
                        attack_coeff
                    } else {
                        release_coeff
                    };
                    band.envelope[ch] = coeff * band.envelope[ch] + (1.0 - coeff) * abs_val;

                    // Gate: if envelope is below threshold, attenuate this band
                    let gain = if band.envelope[ch] < threshold_linear {
                        reduction_linear
                    } else {
                        1.0
                    };

                    buffer[idx] += filtered * gain;
                }
            }
        }
    }

    fn reset(&mut self) {
        for band in &mut self.bands {
            band.bp_state = [BiquadState::default(); MAX_CHANNELS];
            band.envelope = [0.0; MAX_CHANNELS];
        }
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("reduction".into(), self.reduction),
            ("sensitivity".into(), self.sensitivity),
            ("smoothing".into(), self.smoothing),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params { self.set_param(name, *value); }
    }
}
