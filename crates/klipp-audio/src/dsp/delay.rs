use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use std::f32::consts::PI;

const MAX_DELAY_MS: f32 = 2000.0;
const MAX_CHANNELS: usize = 2;

/// Stereo delay with ping-pong mode and low-pass filter in feedback path.
pub struct Delay {
    sample_rate: f32,
    delay_time: f32,   // ms
    feedback: f32,     // 0..0.95
    mix: f32,          // 0..1
    ping_pong: bool,
    // Delay lines: one per channel
    buffers: [Vec<f32>; MAX_CHANNELS],
    write_pos: [usize; MAX_CHANNELS],
    delay_samples: usize,
    // Low-pass filter state in feedback path (one per channel)
    lp_state: [f32; MAX_CHANNELS],
    lp_coeff: f32, // single-pole low-pass coefficient
    dirty: bool,
}

impl Delay {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let max_samples = (MAX_DELAY_MS * 0.001 * sr) as usize + 1;

        let mut d = Self {
            sample_rate: sr,
            delay_time: 250.0,
            feedback: 0.4,
            mix: 0.3,
            ping_pong: false,
            buffers: [vec![0.0; max_samples], vec![0.0; max_samples]],
            write_pos: [0; MAX_CHANNELS],
            delay_samples: 0,
            lp_state: [0.0; MAX_CHANNELS],
            lp_coeff: 0.0,
            dirty: true,
        };
        d.recompute();
        d
    }

    fn recompute(&mut self) {
        self.delay_samples = ((self.delay_time * 0.001 * self.sample_rate) as usize)
            .min(self.buffers[0].len() - 1)
            .max(1);
        // Low-pass at ~4 kHz in feedback path for natural decay
        let cutoff = 4000.0_f32.min(self.sample_rate * 0.49);
        let rc = 1.0 / (2.0 * PI * cutoff);
        let dt = 1.0 / self.sample_rate;
        self.lp_coeff = dt / (rc + dt);
        self.dirty = false;
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "delay_time", label: "Time", default: 250.0, min: 1.0, max: 2000.0, unit: ParamUnit::Ms },
    AudioParamDescriptor { name: "feedback", label: "Feedback", default: 0.4, min: 0.0, max: 0.95, unit: ParamUnit::None },
    AudioParamDescriptor { name: "mix", label: "Mix", default: 0.3, min: 0.0, max: 1.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "ping_pong", label: "Ping-Pong", default: 0.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
];

impl AudioEffect for Delay {
    fn type_id(&self) -> &'static str { "delay" }
    fn name(&self) -> &'static str { "Delay" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "delay_time" => Some(self.delay_time),
            "feedback" => Some(self.feedback),
            "mix" => Some(self.mix),
            "ping_pong" => Some(if self.ping_pong { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// Echo after echo, each `feedback` of the one before, until the
    /// wet sound is below −80 dB: one echo with no feedback.
    fn tail_secs(&self) -> f32 {
        if self.mix <= 0.0 {
            return 0.0;
        }
        let echoes = if self.feedback > 0.0 { 1.0 + ((super::TAIL_FLOOR / self.mix).ln() / self.feedback.ln()).max(0.0).ceil() } else { 1.0 };
        echoes * self.delay_time * 0.001
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "delay_time" => self.delay_time = value.clamp(1.0, MAX_DELAY_MS),
            "feedback" => self.feedback = value.clamp(0.0, 0.95),
            "mix" => self.mix = value.clamp(0.0, 1.0),
            "ping_pong" => self.ping_pong = value > 0.5,
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
        let buf_len = self.buffers[0].len();
        let delay = self.delay_samples;
        let feedback = self.feedback;
        let mix = self.mix;
        let dry = 1.0 - mix;
        let lp_coeff = self.lp_coeff;

        for frame in 0..num_frames {
            for ch in 0..channels.min(MAX_CHANNELS) {
                let idx = frame * channels + ch;
                let input = buffer[idx];

                // Read from delay line
                let read_pos = (self.write_pos[ch] + buf_len - delay) % buf_len;
                let delayed = self.buffers[ch][read_pos];

                // Low-pass filter the delayed signal for feedback
                self.lp_state[ch] += lp_coeff * (delayed - self.lp_state[ch]);
                let filtered = self.lp_state[ch];

                // Write: input + filtered feedback
                let fb_signal = if self.ping_pong && channels >= 2 {
                    // Ping-pong: feed back from opposite channel
                    let other_ch = 1 - ch;
                    let other_read = (self.write_pos[other_ch] + buf_len - delay) % buf_len;
                    self.buffers[other_ch][other_read]
                } else {
                    filtered
                };

                self.buffers[ch][self.write_pos[ch]] = input + fb_signal * feedback;

                // Output: dry/wet mix
                buffer[idx] = input * dry + delayed * mix;
            }

            // Advance write positions
            for ch in 0..channels.min(MAX_CHANNELS) {
                self.write_pos[ch] = (self.write_pos[ch] + 1) % buf_len;
            }
        }
    }

    fn reset(&mut self) {
        for buf in &mut self.buffers {
            buf.iter_mut().for_each(|s| *s = 0.0);
        }
        self.write_pos = [0; MAX_CHANNELS];
        self.lp_state = [0.0; MAX_CHANNELS];
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("delay_time".into(), self.delay_time),
            ("feedback".into(), self.feedback),
            ("mix".into(), self.mix),
            ("ping_pong".into(), if self.ping_pong { 1.0 } else { 0.0 }),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params {
            self.set_param(name, *value);
        }
    }
}
