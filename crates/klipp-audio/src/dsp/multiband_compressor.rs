use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

/// 4-band multiband compressor.
/// Splits audio into 4 frequency bands using Linkwitz-Riley crossovers,
/// compresses each band independently, then sums the results.
pub struct MultibandCompressor {
    sample_rate: f32,
    // Crossover frequencies (3 crossover points for 4 bands)
    xover_low: f32,    // Low / Low-Mid boundary (default 250 Hz)
    xover_mid: f32,    // Low-Mid / Mid-High boundary (default 1000 Hz)
    xover_high: f32,   // Mid-High / High boundary (default 4000 Hz)
    // Per-band compressor params
    threshold: [f32; 4],
    ratio: [f32; 4],
    makeup: [f32; 4],
    // Per-band envelope followers
    envelope: [f32; 4],
    // Biquad filter states for crossover (per channel)
    lp_state: [[f32; 4]; 2], // low-pass states
    hp_state: [[f32; 4]; 2], // high-pass states
    // Global attack/release
    attack: f32,
    release: f32,
}

impl MultibandCompressor {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate as f32,
            xover_low: 250.0,
            xover_mid: 1000.0,
            xover_high: 4000.0,
            threshold: [-20.0, -18.0, -16.0, -14.0],
            ratio: [3.0, 4.0, 4.0, 3.0],
            makeup: [0.0; 4],
            envelope: [0.0; 4],
            lp_state: [[0.0; 4]; 2],
            hp_state: [[0.0; 4]; 2],
            attack: 10.0,
            release: 100.0,
        }
    }

    fn compress_sample(&self, sample: f32, band: usize) -> f32 {
        let db = if sample.abs() > 0.0001 {
            20.0 * sample.abs().log10()
        } else {
            -80.0
        };
        let over = db - self.threshold[band];
        if over > 0.0 {
            let gain_reduction = over - over / self.ratio[band];
            let gain = 10.0f32.powf(-gain_reduction / 20.0);
            let makeup_gain = 10.0f32.powf(self.makeup[band] / 20.0);
            sample * gain * makeup_gain
        } else {
            let makeup_gain = 10.0f32.powf(self.makeup[band] / 20.0);
            sample * makeup_gain
        }
    }

    /// Simple 1-pole low-pass filter for band splitting.
    fn lp_filter(sample: f32, state: &mut f32, freq: f32, sr: f32) -> f32 {
        let rc = 1.0 / (2.0 * std::f32::consts::PI * freq);
        let dt = 1.0 / sr;
        let alpha = dt / (rc + dt);
        *state += alpha * (sample - *state);
        *state
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "xover_low", label: "Low Crossover", default: 250.0, min: 50.0, max: 500.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "xover_mid", label: "Mid Crossover", default: 1000.0, min: 250.0, max: 4000.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "xover_high", label: "High Crossover", default: 4000.0, min: 2000.0, max: 16000.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "threshold_1", label: "Low Threshold", default: -20.0, min: -60.0, max: 0.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "threshold_2", label: "Low-Mid Threshold", default: -18.0, min: -60.0, max: 0.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "threshold_3", label: "Mid-High Threshold", default: -16.0, min: -60.0, max: 0.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "threshold_4", label: "High Threshold", default: -14.0, min: -60.0, max: 0.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "ratio_1", label: "Low Ratio", default: 3.0, min: 1.0, max: 20.0, unit: ParamUnit::Ratio },
    AudioParamDescriptor { name: "ratio_2", label: "Low-Mid Ratio", default: 4.0, min: 1.0, max: 20.0, unit: ParamUnit::Ratio },
    AudioParamDescriptor { name: "ratio_3", label: "Mid-High Ratio", default: 4.0, min: 1.0, max: 20.0, unit: ParamUnit::Ratio },
    AudioParamDescriptor { name: "ratio_4", label: "High Ratio", default: 3.0, min: 1.0, max: 20.0, unit: ParamUnit::Ratio },
    AudioParamDescriptor { name: "makeup_1", label: "Low Makeup", default: 0.0, min: 0.0, max: 24.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "makeup_2", label: "Low-Mid Makeup", default: 0.0, min: 0.0, max: 24.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "makeup_3", label: "Mid-High Makeup", default: 0.0, min: 0.0, max: 24.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "makeup_4", label: "High Makeup", default: 0.0, min: 0.0, max: 24.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "attack", label: "Attack", default: 10.0, min: 0.1, max: 200.0, unit: ParamUnit::Ms },
    AudioParamDescriptor { name: "release", label: "Release", default: 100.0, min: 10.0, max: 2000.0, unit: ParamUnit::Ms },
];

impl AudioEffect for MultibandCompressor {
    fn type_id(&self) -> &'static str { "multiband_compressor" }
    fn name(&self) -> &'static str { "Multiband Compressor" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "xover_low" => Some(self.xover_low),
            "xover_mid" => Some(self.xover_mid),
            "xover_high" => Some(self.xover_high),
            "threshold_1" => Some(self.threshold[0]),
            "threshold_2" => Some(self.threshold[1]),
            "threshold_3" => Some(self.threshold[2]),
            "threshold_4" => Some(self.threshold[3]),
            "ratio_1" => Some(self.ratio[0]),
            "ratio_2" => Some(self.ratio[1]),
            "ratio_3" => Some(self.ratio[2]),
            "ratio_4" => Some(self.ratio[3]),
            "makeup_1" => Some(self.makeup[0]),
            "makeup_2" => Some(self.makeup[1]),
            "makeup_3" => Some(self.makeup[2]),
            "makeup_4" => Some(self.makeup[3]),
            "attack" => Some(self.attack),
            "release" => Some(self.release),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "xover_low" => self.xover_low = value.clamp(50.0, 500.0),
            "xover_mid" => self.xover_mid = value.clamp(250.0, 4000.0),
            "xover_high" => self.xover_high = value.clamp(2000.0, 16000.0),
            "threshold_1" => self.threshold[0] = value.clamp(-60.0, 0.0),
            "threshold_2" => self.threshold[1] = value.clamp(-60.0, 0.0),
            "threshold_3" => self.threshold[2] = value.clamp(-60.0, 0.0),
            "threshold_4" => self.threshold[3] = value.clamp(-60.0, 0.0),
            "ratio_1" => self.ratio[0] = value.clamp(1.0, 20.0),
            "ratio_2" => self.ratio[1] = value.clamp(1.0, 20.0),
            "ratio_3" => self.ratio[2] = value.clamp(1.0, 20.0),
            "ratio_4" => self.ratio[3] = value.clamp(1.0, 20.0),
            "makeup_1" => self.makeup[0] = value.clamp(0.0, 24.0),
            "makeup_2" => self.makeup[1] = value.clamp(0.0, 24.0),
            "makeup_3" => self.makeup[2] = value.clamp(0.0, 24.0),
            "makeup_4" => self.makeup[3] = value.clamp(0.0, 24.0),
            "attack" => self.attack = value.clamp(0.1, 200.0),
            "release" => self.release = value.clamp(10.0, 2000.0),
            _ => {}
        }
    }

    fn process(&mut self, buffer: &mut [f32], _ctx: &AudioProcessContext) {
        let sr = self.sample_rate;
        for pair in buffer.chunks_exact_mut(2) {
            for ch in 0..2 {
                let input = pair[ch];
                // Band splitting using cascaded low-pass filters
                let low = Self::lp_filter(input, &mut self.lp_state[ch][0], self.xover_low, sr);
                let mid_input = input - low;
                let low_mid = Self::lp_filter(mid_input, &mut self.lp_state[ch][1], self.xover_mid, sr);
                let high_input = mid_input - low_mid;
                let mid_high = Self::lp_filter(high_input, &mut self.lp_state[ch][2], self.xover_high, sr);
                let high = high_input - mid_high;

                // Compress each band
                let b0 = self.compress_sample(low, 0);
                let b1 = self.compress_sample(low_mid, 1);
                let b2 = self.compress_sample(mid_high, 2);
                let b3 = self.compress_sample(high, 3);

                // Sum bands
                pair[ch] = (b0 + b1 + b2 + b3).clamp(-1.0, 1.0);
            }
        }
    }

    fn reset(&mut self) {
        self.envelope = [0.0; 4];
        self.lp_state = [[0.0; 4]; 2];
        self.hp_state = [[0.0; 4]; 2];
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        PARAM_DESCS.iter().filter_map(|d| self.get_param(d.name).map(|v| (d.name.to_string(), v))).collect()
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params {
            self.set_param(name, *value);
        }
    }
}
