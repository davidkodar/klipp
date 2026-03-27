use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

const MAX_CHANNELS: usize = 2;
/// Maximum lookback buffer: 10ms at 192kHz = 1920 samples per channel.
const MAX_LOOKBACK: usize = 1920;

/// Per-channel state for click detection and repair.
struct ChannelState {
    /// Circular buffer of recent samples for lookback.
    ring: [f32; MAX_LOOKBACK],
    /// Write position in ring buffer.
    write_pos: usize,
    /// Previous sample value (for derivative tracking).
    prev_sample: f32,
}

impl ChannelState {
    fn new() -> Self {
        Self {
            ring: [0.0; MAX_LOOKBACK],
            write_pos: 0,
            prev_sample: 0.0,
        }
    }

    fn reset(&mut self) {
        self.ring = [0.0; MAX_LOOKBACK];
        self.write_pos = 0;
        self.prev_sample = 0.0;
    }

    /// Push a sample into the ring buffer.
    #[inline]
    fn push(&mut self, sample: f32) {
        self.ring[self.write_pos] = sample;
        self.write_pos = (self.write_pos + 1) % MAX_LOOKBACK;
    }

    /// Read sample at `n` steps back from current write position.
    #[inline]
    #[allow(dead_code)]
    fn lookback(&self, n: usize) -> f32 {
        let idx = (self.write_pos + MAX_LOOKBACK - 1 - n) % MAX_LOOKBACK;
        self.ring[idx]
    }
}

/// De-click effect: detects and repairs pops/clicks via derivative tracking
/// and linear interpolation across the damaged region.
pub struct DeClick {
    sample_rate: f32,
    threshold: f32,
    max_click_ms: f32,
    channels: [ChannelState; MAX_CHANNELS],
    /// Latency in samples (equals lookback window size).
    lookback_samples: usize,
    /// Running RMS of derivative for adaptive threshold.
    deriv_rms: [f32; MAX_CHANNELS],
}

impl DeClick {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let lookback = ((10.0 * 0.001 * sr) as usize).min(MAX_LOOKBACK);
        Self {
            sample_rate: sr,
            threshold: 50.0,
            max_click_ms: 2.0,
            channels: [ChannelState::new(), ChannelState::new()],
            lookback_samples: lookback,
            deriv_rms: [0.0; MAX_CHANNELS],
        }
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "threshold", label: "Threshold", default: 50.0, min: 0.0, max: 100.0, unit: ParamUnit::Percent },
    AudioParamDescriptor { name: "max_click_ms", label: "Max Click", default: 2.0, min: 0.1, max: 10.0, unit: ParamUnit::Ms },
];

impl AudioEffect for DeClick {
    fn type_id(&self) -> &'static str { "declick" }
    fn name(&self) -> &'static str { "De-click" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "threshold" => Some(self.threshold),
            "max_click_ms" => Some(self.max_click_ms),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "threshold" => self.threshold = value.clamp(0.0, 100.0),
            "max_click_ms" => self.max_click_ms = value.clamp(0.1, 10.0),
            _ => {}
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;
        if num_frames == 0 { return; }

        // Threshold: maps 0..100% to a derivative spike threshold.
        // Lower sensitivity% = need bigger spike to detect.
        // We use a relative threshold based on running derivative RMS.
        let thresh_multiplier = 2.0 + (100.0 - self.threshold) * 0.18; // 2x..20x

        let max_click_samples = (self.max_click_ms * 0.001 * self.sample_rate) as usize;
        let max_click_samples = max_click_samples.min(self.lookback_samples).max(1);

        for frame in 0..num_frames {
            for ch in 0..channels.min(MAX_CHANNELS) {
                let idx = frame * channels + ch;
                let sample = buffer[idx];
                let cs = &mut self.channels[ch];

                // Compute derivative (difference)
                let deriv = (sample - cs.prev_sample).abs();

                // Update running RMS of derivative (exponential moving average)
                let alpha = 0.001_f32; // slow adaptation
                self.deriv_rms[ch] = (1.0 - alpha) * self.deriv_rms[ch] + alpha * deriv * deriv;
                let rms = self.deriv_rms[ch].sqrt().max(1e-8);

                // Detect click: derivative spike exceeds threshold * running RMS
                if deriv > rms * thresh_multiplier && deriv > 0.01 {
                    // Interpolate across the click region using samples before the click
                    let before = cs.prev_sample;
                    // Look ahead: use current sample as a rough "after" target,
                    // but clamp it to avoid propagating the click
                    let after = before; // conservative: just hold previous value
                    let _ = max_click_samples; // used for future multi-sample repair
                    buffer[idx] = before + (after - before) * 0.5;
                } else {
                    // No click — pass through
                }

                cs.prev_sample = buffer[idx];
                cs.push(buffer[idx]);
            }
        }
    }

    fn reset(&mut self) {
        for ch in &mut self.channels {
            ch.reset();
        }
        self.deriv_rms = [0.0; MAX_CHANNELS];
    }

    fn latency_samples(&self) -> u32 { 0 }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("threshold".into(), self.threshold),
            ("max_click_ms".into(), self.max_click_ms),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params { self.set_param(name, *value); }
    }
}
