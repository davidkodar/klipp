use super::biquad::{BiquadCascade, FilterSlope};
use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

/// Butterworth high-pass filter for removing camera rumble and wind energy.
///
/// Frequency is intentionally automatable; slope is structural and should be
/// set before playback or rendering. The filter is stereo-safe and keeps one
/// stateful cascade per channel.
pub struct HighPass {
    sample_rate: f32,
    frequency: f32,
    slope: f32,
    filters: [BiquadCascade; 2],
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "frequency", label: "Cutoff", default: 100.0, min: 20.0, max: 1000.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "slope", label: "Slope", default: 3.0, min: 0.0, max: 5.0, unit: ParamUnit::None },
];

impl HighPass {
    pub fn new(sample_rate: u32) -> Self {
        let mut effect = Self {
            sample_rate: sample_rate as f32,
            frequency: 100.0,
            slope: 3.0,
            filters: [BiquadCascade::new(2), BiquadCascade::new(2)],
        };
        effect.rebuild();
        effect
    }

    fn rebuild(&mut self) {
        let slope = FilterSlope::from_index(self.slope.round().clamp(0.0, 5.0) as u32);
        for filter in &mut self.filters {
            filter.set_hp(self.frequency, slope, self.sample_rate);
        }
    }
}

impl AudioEffect for HighPass {
    fn type_id(&self) -> &'static str { "high_pass" }
    fn name(&self) -> &'static str { "High-pass Filter" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name { "frequency" => Some(self.frequency), "slope" => Some(self.slope), _ => None }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "frequency" => self.frequency = value.clamp(20.0, 1000.0),
            "slope" => self.slope = value.clamp(0.0, 5.0),
            _ => return,
        }
        self.rebuild();
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = (ctx.channels as usize).clamp(1, 2);
        for frame in buffer.chunks_exact_mut(channels) {
            for (ch, sample) in frame.iter_mut().enumerate() {
                *sample = self.filters[ch].process(*sample);
            }
        }
    }

    fn reset(&mut self) {
        for filter in &mut self.filters { filter.reset(); }
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![("frequency".into(), self.frequency), ("slope".into(), self.slope)]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params { self.set_param(name, *value); }
    }

    fn param_keyframable(&self, param_name: &str) -> bool {
        param_name == "frequency"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> AudioProcessContext {
        AudioProcessContext { sample_rate: 48_000, channels: 1, clip_time_secs: 0.0 }
    }

    #[test]
    fn attenuates_wind_rumble_and_preserves_voice_band() {
        let mut fx = HighPass::new(48_000);
        fx.set_param("frequency", 120.0);
        let mut low = vec![0.0; 48_000];
        let mut voice = vec![0.0; 48_000];
        for i in 0..48_000 {
            let t = i as f32 / 48_000.0;
            low[i] = (2.0 * std::f32::consts::PI * 50.0 * t).sin();
            voice[i] = (2.0 * std::f32::consts::PI * 1000.0 * t).sin();
        }
        fx.process(&mut low, &ctx());
        fx.reset();
        fx.process(&mut voice, &ctx());
        let low_rms = (low.iter().skip(1000).map(|v| v * v).sum::<f32>() / 47000.0).sqrt();
        let voice_rms = (voice.iter().skip(1000).map(|v| v * v).sum::<f32>() / 47000.0).sqrt();
        assert!(low_rms < 0.2, "50 Hz rumble should be strongly attenuated: {low_rms}");
        assert!(voice_rms > 0.5, "1 kHz speech band should remain: {voice_rms}");
    }

    #[test]
    fn params_round_trip() {
        let mut fx = HighPass::new(48_000);
        fx.deserialize_params(&[("frequency".into(), 180.0), ("slope".into(), 4.0)]);
        assert_eq!(fx.serialize_params(), vec![("frequency".into(), 180.0), ("slope".into(), 4.0)]);
    }
}
