use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

const MAX_CHANNELS: usize = 2;
/// RMS window: 400ms at 192kHz = 76800 samples.
const MAX_RMS_WINDOW: usize = 76800;

/// Real-time auto-gain normalizer.
///
/// Measures running RMS (400ms window) and slowly adjusts gain toward the
/// target loudness level. Supports peak-normalize mode (instant) and
/// LUFS-approximate mode (slow attack/release to avoid pumping).
pub struct Normalize {
    sample_rate: f32,
    target_lufs: f32,
    mode: f32,
    /// Current applied gain (linear).
    current_gain: f32,
    /// Running sum-of-squares for RMS calculation.
    rms_sum: f64,
    /// Circular buffer of squared samples for windowed RMS.
    rms_ring: Vec<f32>,
    /// Write position in RMS ring.
    rms_pos: usize,
    /// Number of samples accumulated so far (up to window size).
    rms_count: usize,
    /// RMS window size in samples.
    rms_window: usize,
}

impl Normalize {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        // 400ms RMS window
        let window = ((0.4 * sr) as usize).min(MAX_RMS_WINDOW);
        Self {
            sample_rate: sr,
            target_lufs: -14.0,
            mode: 1.0,
            current_gain: 1.0,
            rms_sum: 0.0,
            rms_ring: vec![0.0; window],
            rms_pos: 0,
            rms_count: 0,
            rms_window: window,
        }
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "target_lufs", label: "Target LUFS", default: -14.0, min: -24.0, max: 0.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "mode", label: "Mode", default: 1.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
];

impl AudioEffect for Normalize {
    fn type_id(&self) -> &'static str { "normalize" }
    fn name(&self) -> &'static str { "Normalize" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "target_lufs" => Some(self.target_lufs),
            "mode" => Some(self.mode),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "target_lufs" => self.target_lufs = value.clamp(-24.0, 0.0),
            "mode" => self.mode = if value > 0.5 { 1.0 } else { 0.0 },
            _ => {}
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;
        if num_frames == 0 { return; }

        let target_linear = 10.0_f32.powf(self.target_lufs / 20.0);
        let is_lufs_mode = self.mode > 0.5;

        // Attack/release coefficients for gain smoothing
        let attack_coeff = (-1.0 / (0.1 * self.sample_rate)).exp(); // 100ms
        let release_coeff = (-1.0 / (1.0 * self.sample_rate)).exp(); // 1000ms

        for frame in 0..num_frames {
            // Compute mono-sum of this frame for RMS measurement
            let mut sum_sq = 0.0_f32;
            for ch in 0..channels.min(MAX_CHANNELS) {
                let s = buffer[frame * channels + ch];
                sum_sq += s * s;
            }
            let avg_sq = sum_sq / channels.min(MAX_CHANNELS) as f32;

            // Update windowed RMS ring buffer
            if self.rms_window > 0 {
                let old_sq = self.rms_ring[self.rms_pos];
                self.rms_ring[self.rms_pos] = avg_sq;
                self.rms_pos = (self.rms_pos + 1) % self.rms_window;
                self.rms_sum += avg_sq as f64 - old_sq as f64;
                if self.rms_count < self.rms_window {
                    self.rms_count += 1;
                }
                // Prevent negative drift from floating point
                if self.rms_sum < 0.0 { self.rms_sum = 0.0; }
            }

            if is_lufs_mode {
                // LUFS-approximate mode: slow auto-gain
                if self.rms_count > 0 {
                    let rms = (self.rms_sum / self.rms_count as f64).sqrt() as f32;
                    if rms > 1e-8 {
                        let desired_gain = target_linear / rms;
                        // Clamp gain to avoid extreme amplification of silence
                        let desired_gain = desired_gain.clamp(0.01, 40.0);
                        // Smooth gain changes
                        let coeff = if desired_gain < self.current_gain {
                            attack_coeff
                        } else {
                            release_coeff
                        };
                        self.current_gain = coeff * self.current_gain + (1.0 - coeff) * desired_gain;
                    }
                }
            } else {
                // Peak normalize mode: track peak and scale
                let mut peak = 0.0_f32;
                for ch in 0..channels.min(MAX_CHANNELS) {
                    peak = peak.max(buffer[frame * channels + ch].abs());
                }
                if peak > 1e-8 {
                    let desired_gain = target_linear / peak;
                    let desired_gain = desired_gain.clamp(0.01, 40.0);
                    let coeff = if desired_gain < self.current_gain {
                        attack_coeff
                    } else {
                        release_coeff
                    };
                    self.current_gain = coeff * self.current_gain + (1.0 - coeff) * desired_gain;
                }
            }

            // Apply gain
            for ch in 0..channels {
                buffer[frame * channels + ch] *= self.current_gain;
            }
        }
    }

    fn reset(&mut self) {
        self.current_gain = 1.0;
        self.rms_sum = 0.0;
        self.rms_pos = 0;
        self.rms_count = 0;
        for s in &mut self.rms_ring {
            *s = 0.0;
        }
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("target_lufs".into(), self.target_lufs),
            ("mode".into(), self.mode),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params { self.set_param(name, *value); }
    }
}
