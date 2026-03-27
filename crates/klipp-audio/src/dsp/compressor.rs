use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

/// Dynamic range compressor.
pub struct Compressor {
    sample_rate: f32,
    threshold: f32,
    ratio: f32,
    attack: f32,
    release: f32,
    makeup: f32,
    knee: f32,
    envelope: f32,
}

impl Compressor {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate as f32,
            threshold: -18.0,
            ratio: 4.0,
            attack: 10.0,
            release: 100.0,
            makeup: 0.0,
            knee: 6.0,
            envelope: 0.0,
        }
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "threshold", label: "Threshold", default: -18.0, min: -60.0, max: 0.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "ratio", label: "Ratio", default: 4.0, min: 1.0, max: 20.0, unit: ParamUnit::Ratio },
    AudioParamDescriptor { name: "attack", label: "Attack", default: 10.0, min: 0.1, max: 200.0, unit: ParamUnit::Ms },
    AudioParamDescriptor { name: "release", label: "Release", default: 100.0, min: 10.0, max: 2000.0, unit: ParamUnit::Ms },
    AudioParamDescriptor { name: "makeup", label: "Makeup", default: 0.0, min: 0.0, max: 30.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "knee", label: "Knee", default: 6.0, min: 0.0, max: 24.0, unit: ParamUnit::Db },
];

impl AudioEffect for Compressor {
    fn type_id(&self) -> &'static str { "compressor" }
    fn name(&self) -> &'static str { "Compressor" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "threshold" => Some(self.threshold),
            "ratio" => Some(self.ratio),
            "attack" => Some(self.attack),
            "release" => Some(self.release),
            "makeup" => Some(self.makeup),
            "knee" => Some(self.knee),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "threshold" => self.threshold = value.clamp(-60.0, 0.0),
            "ratio" => self.ratio = value.clamp(1.0, 20.0),
            "attack" => self.attack = value.clamp(0.1, 200.0),
            "release" => self.release = value.clamp(10.0, 2000.0),
            "makeup" => self.makeup = value.clamp(0.0, 30.0),
            "knee" => self.knee = value.clamp(0.0, 24.0),
            _ => {}
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;

        let attack_coeff = (-1.0 / (self.attack * 0.001 * self.sample_rate)).exp();
        let release_coeff = (-1.0 / (self.release * 0.001 * self.sample_rate)).exp();
        let makeup_linear = 10.0_f32.powf(self.makeup / 20.0);
        let threshold_db = self.threshold;
        let ratio = self.ratio;
        let half_knee = self.knee * 0.5;

        for frame in 0..num_frames {
            let mut peak = 0.0_f32;
            for ch in 0..channels {
                peak = peak.max(buffer[frame * channels + ch].abs());
            }

            let coeff = if peak > self.envelope { attack_coeff } else { release_coeff };
            self.envelope = coeff * self.envelope + (1.0 - coeff) * peak;

            let env_db = if self.envelope > 1e-10 {
                20.0 * self.envelope.log10()
            } else {
                -200.0
            };

            let over_db = env_db - threshold_db;
            let gain_reduction_db = if over_db <= -half_knee {
                0.0
            } else if over_db >= half_knee {
                over_db * (1.0 - 1.0 / ratio)
            } else {
                let x = over_db + half_knee;
                x * x / (4.0 * half_knee) * (1.0 - 1.0 / ratio)
            };

            let gain = 10.0_f32.powf(-gain_reduction_db / 20.0) * makeup_linear;

            for ch in 0..channels {
                buffer[frame * channels + ch] *= gain;
            }
        }
    }

    fn reset(&mut self) { self.envelope = 0.0; }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("threshold".into(), self.threshold),
            ("ratio".into(), self.ratio),
            ("attack".into(), self.attack),
            ("release".into(), self.release),
            ("makeup".into(), self.makeup),
            ("knee".into(), self.knee),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params { self.set_param(name, *value); }
    }
}
