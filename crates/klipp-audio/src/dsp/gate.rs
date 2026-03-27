use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

/// Noise gate.
pub struct NoiseGate {
    sample_rate: f32,
    threshold: f32,
    attack: f32,
    release: f32,
    range: f32,
    envelope: f32,
    gate_gain: f32,
}

impl NoiseGate {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate as f32,
            threshold: -40.0,
            attack: 1.0,
            release: 50.0,
            range: -80.0,
            envelope: 0.0,
            gate_gain: 0.0,
        }
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "threshold", label: "Threshold", default: -40.0, min: -80.0, max: 0.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "attack", label: "Attack", default: 1.0, min: 0.01, max: 50.0, unit: ParamUnit::Ms },
    AudioParamDescriptor { name: "release", label: "Release", default: 50.0, min: 5.0, max: 1000.0, unit: ParamUnit::Ms },
    AudioParamDescriptor { name: "range", label: "Range", default: -80.0, min: -80.0, max: 0.0, unit: ParamUnit::Db },
];

impl AudioEffect for NoiseGate {
    fn type_id(&self) -> &'static str { "gate" }
    fn name(&self) -> &'static str { "Noise Gate" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "threshold" => Some(self.threshold),
            "attack" => Some(self.attack),
            "release" => Some(self.release),
            "range" => Some(self.range),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "threshold" => self.threshold = value.clamp(-80.0, 0.0),
            "attack" => self.attack = value.clamp(0.01, 50.0),
            "release" => self.release = value.clamp(5.0, 1000.0),
            "range" => self.range = value.clamp(-80.0, 0.0),
            _ => {}
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;

        let attack_coeff = (-1.0 / (self.attack * 0.001 * self.sample_rate)).exp();
        let release_coeff = (-1.0 / (self.release * 0.001 * self.sample_rate)).exp();
        let threshold_linear = 10.0_f32.powf(self.threshold / 20.0);
        let range_linear = 10.0_f32.powf(self.range / 20.0);

        for frame in 0..num_frames {
            let mut peak = 0.0_f32;
            for ch in 0..channels {
                peak = peak.max(buffer[frame * channels + ch].abs());
            }

            let coeff = if peak > self.envelope { attack_coeff } else { release_coeff };
            self.envelope = coeff * self.envelope + (1.0 - coeff) * peak;

            let target = if self.envelope > threshold_linear { 1.0 } else { range_linear };
            let gate_coeff = if target > self.gate_gain { 1.0 - attack_coeff } else { 1.0 - release_coeff };
            self.gate_gain += gate_coeff * (target - self.gate_gain);

            for ch in 0..channels {
                buffer[frame * channels + ch] *= self.gate_gain;
            }
        }
    }

    fn reset(&mut self) { self.envelope = 0.0; self.gate_gain = 0.0; }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("threshold".into(), self.threshold),
            ("attack".into(), self.attack),
            ("release".into(), self.release),
            ("range".into(), self.range),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params { self.set_param(name, *value); }
    }
}
