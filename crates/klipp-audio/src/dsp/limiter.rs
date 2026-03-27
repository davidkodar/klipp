use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

/// Brick-wall limiter.
pub struct Limiter {
    sample_rate: f32,
    ceiling: f32,
    release: f32,
    envelope: f32,
}

impl Limiter {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate as f32,
            ceiling: -0.3,
            release: 50.0,
            envelope: 1.0,
        }
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "ceiling", label: "Ceiling", default: -0.3, min: -12.0, max: 0.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "release", label: "Release", default: 50.0, min: 5.0, max: 500.0, unit: ParamUnit::Ms },
];

impl AudioEffect for Limiter {
    fn type_id(&self) -> &'static str { "limiter" }
    fn name(&self) -> &'static str { "Limiter" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "ceiling" => Some(self.ceiling),
            "release" => Some(self.release),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "ceiling" => self.ceiling = value.clamp(-12.0, 0.0),
            "release" => self.release = value.clamp(5.0, 500.0),
            _ => {}
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;

        let ceiling_linear = 10.0_f32.powf(self.ceiling / 20.0);
        let release_coeff = (-1.0 / (self.release * 0.001 * self.sample_rate)).exp();

        for frame in 0..num_frames {
            let mut peak = 0.0_f32;
            for ch in 0..channels {
                peak = peak.max(buffer[frame * channels + ch].abs());
            }

            let target = if peak > ceiling_linear { ceiling_linear / peak } else { 1.0 };

            if target < self.envelope {
                self.envelope = target;
            } else {
                self.envelope = release_coeff * self.envelope + (1.0 - release_coeff) * target;
            }

            for ch in 0..channels {
                buffer[frame * channels + ch] *= self.envelope;
            }
        }
    }

    fn reset(&mut self) { self.envelope = 1.0; }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![("ceiling".into(), self.ceiling), ("release".into(), self.release)]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params { self.set_param(name, *value); }
    }
}
