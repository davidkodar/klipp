use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use std::f32::consts::PI;

const MAX_CHANNELS: usize = 2;
const MAX_VOICES: usize = 4;
/// Delay buffer size: 50ms at 192kHz = 9600 samples per channel.
const DELAY_BUF_SIZE: usize = 9600;
/// Base delay in seconds (center of modulation range).
const BASE_DELAY_SECS: f32 = 0.010; // 10ms

/// Per-channel delay line.
struct DelayLine {
    buf: [f32; DELAY_BUF_SIZE],
    write_pos: usize,
}

impl DelayLine {
    fn new() -> Self {
        Self {
            buf: [0.0; DELAY_BUF_SIZE],
            write_pos: 0,
        }
    }

    fn reset(&mut self) {
        self.buf = [0.0; DELAY_BUF_SIZE];
        self.write_pos = 0;
    }

    /// Write a sample and advance.
    #[inline]
    fn write(&mut self, sample: f32) {
        self.buf[self.write_pos] = sample;
        self.write_pos = (self.write_pos + 1) % DELAY_BUF_SIZE;
    }

    /// Read from the delay line with fractional delay (linear interpolation).
    #[inline]
    fn read_delay(&self, delay_samples: f32) -> f32 {
        let delay = delay_samples.clamp(0.0, (DELAY_BUF_SIZE - 2) as f32);
        let int_delay = delay as usize;
        let frac = delay - int_delay as f32;
        let idx0 = (self.write_pos + DELAY_BUF_SIZE - 1 - int_delay) % DELAY_BUF_SIZE;
        let idx1 = (idx0 + DELAY_BUF_SIZE - 1) % DELAY_BUF_SIZE;
        self.buf[idx0] * (1.0 - frac) + self.buf[idx1] * frac
    }
}

/// Chorus effect: multiple modulated delay lines for width and richness.
pub struct Chorus {
    sample_rate: f32,
    rate: f32,
    depth: f32,
    mix: f32,
    voices: f32,
    delay_lines: [[DelayLine; MAX_VOICES]; MAX_CHANNELS],
    /// LFO phases per voice (in radians).
    lfo_phases: [f32; MAX_VOICES],
}

impl Chorus {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let delay_lines = std::array::from_fn(|_| {
            std::array::from_fn(|_| DelayLine::new())
        });
        // Spread LFO phases evenly across voices
        let lfo_phases = std::array::from_fn(|v| {
            2.0 * PI * v as f32 / MAX_VOICES as f32
        });
        Self {
            sample_rate: sr,
            rate: 1.5,
            depth: 5.0,
            mix: 50.0,
            voices: 2.0,
            delay_lines,
            lfo_phases,
        }
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "rate", label: "Rate", default: 1.5, min: 0.1, max: 10.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "depth", label: "Depth", default: 5.0, min: 0.0, max: 20.0, unit: ParamUnit::Ms },
    AudioParamDescriptor { name: "mix", label: "Mix", default: 50.0, min: 0.0, max: 100.0, unit: ParamUnit::Percent },
    AudioParamDescriptor { name: "voices", label: "Voices", default: 2.0, min: 1.0, max: 4.0, unit: ParamUnit::None },
];

impl AudioEffect for Chorus {
    fn type_id(&self) -> &'static str { "chorus" }
    fn name(&self) -> &'static str { "Chorus" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "rate" => Some(self.rate),
            "depth" => Some(self.depth),
            "mix" => Some(self.mix),
            "voices" => Some(self.voices),
            _ => None,
        }
    }

    /// Its voices' delay: the base and the sweep's depth.
    fn tail_secs(&self) -> f32 {
        if self.mix <= 0.0 {
            return 0.0;
        }
        BASE_DELAY_SECS + self.depth * 0.001
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "rate" => self.rate = value.clamp(0.1, 10.0),
            "depth" => self.depth = value.clamp(0.0, 20.0),
            "mix" => self.mix = value.clamp(0.0, 100.0),
            "voices" => self.voices = value.clamp(1.0, 4.0).round(),
            _ => {}
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;
        if num_frames == 0 { return; }

        let num_voices = (self.voices as usize).clamp(1, MAX_VOICES);
        let wet = self.mix / 100.0;
        let dry = 1.0 - wet;
        let base_delay_samples = BASE_DELAY_SECS * self.sample_rate;
        let depth_samples = self.depth * 0.001 * self.sample_rate;
        let lfo_inc = 2.0 * PI * self.rate / self.sample_rate;

        for frame in 0..num_frames {
            // Advance LFO phases (shared across channels)
            for v in 0..num_voices {
                self.lfo_phases[v] += lfo_inc;
                if self.lfo_phases[v] > 2.0 * PI {
                    self.lfo_phases[v] -= 2.0 * PI;
                }
            }

            for ch in 0..channels.min(MAX_CHANNELS) {
                let idx = frame * channels + ch;
                let dry_sample = buffer[idx];

                // Write dry signal into all voice delay lines
                for v in 0..num_voices {
                    self.delay_lines[ch][v].write(dry_sample);
                }

                // Sum modulated delay outputs from all voices
                let mut wet_sum = 0.0_f32;
                for v in 0..num_voices {
                    let lfo_val = self.lfo_phases[v].sin();
                    let delay = base_delay_samples + depth_samples * lfo_val;
                    wet_sum += self.delay_lines[ch][v].read_delay(delay);
                }
                wet_sum /= num_voices as f32;

                buffer[idx] = dry_sample * dry + wet_sum * wet;
            }
        }
    }

    fn reset(&mut self) {
        for ch_lines in &mut self.delay_lines {
            for dl in ch_lines {
                dl.reset();
            }
        }
        self.lfo_phases = std::array::from_fn(|v| {
            2.0 * PI * v as f32 / MAX_VOICES as f32
        });
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("rate".into(), self.rate),
            ("depth".into(), self.depth),
            ("mix".into(), self.mix),
            ("voices".into(), self.voices),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params { self.set_param(name, *value); }
    }
}
