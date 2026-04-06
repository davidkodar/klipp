use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use crate::lufs::LufsMeter;

const MAX_CHANNELS: usize = 2;

/// Real-time auto-gain normalizer.
///
/// Uses a BS.1770-4 LUFS meter for loudness analysis (when in LUFS mode)
/// or peak tracking (when in peak mode), and slowly adjusts gain toward
/// the target loudness level.
pub struct Normalize {
    sample_rate: f32,
    target_lufs: f32,
    mode: f32,
    /// Current applied gain (linear).
    current_gain: f32,
    /// BS.1770-4 compliant meter for LUFS-mode analysis.
    lufs_meter: LufsMeter,
}

impl Normalize {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        Self {
            sample_rate: sr,
            target_lufs: -14.0,
            mode: 1.0,
            current_gain: 1.0,
            lufs_meter: LufsMeter::new(sr),
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

        let is_lufs_mode = self.mode > 0.5;

        // Attack/release coefficients for gain smoothing
        let attack_coeff = (-1.0 / (0.1 * self.sample_rate)).exp(); // 100ms
        let release_coeff = (-1.0 / (1.0 * self.sample_rate)).exp(); // 1000ms

        if is_lufs_mode {
            // BS.1770-4 LUFS mode: feed buffer through K-weighted meter
            // then derive gain from momentary loudness vs target
            self.lufs_meter.process(buffer);
            let measured_lufs = self.lufs_meter.momentary_lufs;

            if measured_lufs.is_finite() && measured_lufs > -70.0 {
                // desired gain in dB = target - measured, then convert to linear
                let gain_db = self.target_lufs - measured_lufs;
                let desired_gain = 10.0_f32.powf(gain_db / 20.0).clamp(0.01, 40.0);
                let coeff = if desired_gain < self.current_gain {
                    attack_coeff
                } else {
                    release_coeff
                };
                self.current_gain = coeff * self.current_gain + (1.0 - coeff) * desired_gain;
            }
        } else {
            // Peak normalize mode: track peak and scale
            for frame in 0..num_frames {
                let mut peak = 0.0_f32;
                for ch in 0..channels.min(MAX_CHANNELS) {
                    peak = peak.max(buffer[frame * channels + ch].abs());
                }
                if peak > 1e-8 {
                    let target_linear = 10.0_f32.powf(self.target_lufs / 20.0);
                    let desired_gain = (target_linear / peak).clamp(0.01, 40.0);
                    let coeff = if desired_gain < self.current_gain {
                        attack_coeff
                    } else {
                        release_coeff
                    };
                    self.current_gain = coeff * self.current_gain + (1.0 - coeff) * desired_gain;
                }
            }
        }

        // Apply gain to all samples
        for s in buffer.iter_mut() {
            *s *= self.current_gain;
        }
    }

    fn reset(&mut self) {
        self.current_gain = 1.0;
        self.lufs_meter.reset();
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
