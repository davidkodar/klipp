use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use std::f32::consts::PI;

const MAX_CHANNELS: usize = 2;
/// Circular buffer size: ~100ms at 192kHz = 19200 samples per channel.
const CIRC_BUF_SIZE: usize = 19200;

/// Grain size in seconds.
const GRAIN_SECS: f32 = 0.020; // 20ms
/// Per-channel pitch shift state.
struct ChannelState {
    /// Circular input buffer.
    buf: [f32; CIRC_BUF_SIZE],
    /// Write pointer into circular buffer.
    write_pos: usize,
    /// Read pointer (fractional) for grain playback.
    read_pos: f64,
    /// Second read pointer offset by half a grain for overlap-add.
    read_pos2: f64,
}

impl ChannelState {
    fn new() -> Self {
        Self {
            buf: [0.0; CIRC_BUF_SIZE],
            write_pos: 0,
            read_pos: 0.0,
            read_pos2: 0.0,
        }
    }

    fn reset(&mut self) {
        self.buf = [0.0; CIRC_BUF_SIZE];
        self.write_pos = 0;
        self.read_pos = 0.0;
        self.read_pos2 = 0.0;
    }

    /// Read from circular buffer with linear interpolation.
    #[inline]
    fn read_interp(&self, pos: f64) -> f32 {
        let len = CIRC_BUF_SIZE as f64;
        let pos = ((pos % len) + len) % len;
        let idx0 = pos as usize % CIRC_BUF_SIZE;
        let idx1 = (idx0 + 1) % CIRC_BUF_SIZE;
        let frac = (pos - pos.floor()) as f32;
        self.buf[idx0] * (1.0 - frac) + self.buf[idx1] * frac
    }
}

/// Granular pitch shifter using overlap-add with Hann-windowed grains.
pub struct PitchShift {
    #[allow(dead_code)]
    sample_rate: f32,
    semitones: f32,
    fine_cents: f32,
    channels: [ChannelState; MAX_CHANNELS],
    /// Grain size in samples.
    grain_samples: f32,
    /// Pre-computed Hann window for one grain.
    hann_window: Vec<f32>,
}

impl PitchShift {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let grain_samples = (GRAIN_SECS * sr).round();
        let grain_len = grain_samples as usize;

        // Pre-compute Hann window
        let hann_window: Vec<f32> = (0..grain_len)
            .map(|i| {
                let t = i as f32 / (grain_len - 1).max(1) as f32;
                0.5 * (1.0 - (2.0 * PI * t).cos())
            })
            .collect();

        let mut ps = Self {
            sample_rate: sr,
            semitones: 0.0,
            fine_cents: 0.0,
            channels: [ChannelState::new(), ChannelState::new()],
            grain_samples,
            hann_window,
        };

        // Initialize read pointers offset from write position
        for ch in &mut ps.channels {
            ch.read_pos = 0.0;
            ch.read_pos2 = (grain_samples * 0.5) as f64;
        }

        ps
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "semitones", label: "Semitones", default: 0.0, min: -12.0, max: 12.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "fine_cents", label: "Fine (cents)", default: 0.0, min: -100.0, max: 100.0, unit: ParamUnit::None },
];

impl AudioEffect for PitchShift {
    fn type_id(&self) -> &'static str { "pitch_shift" }
    fn name(&self) -> &'static str { "Pitch Shift" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "semitones" => Some(self.semitones),
            "fine_cents" => Some(self.fine_cents),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "semitones" => self.semitones = value.clamp(-12.0, 12.0).round(),
            "fine_cents" => self.fine_cents = value.clamp(-100.0, 100.0),
            _ => {}
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;
        if num_frames == 0 { return; }

        // Compute pitch ratio
        let total_semitones = self.semitones + self.fine_cents / 100.0;
        if total_semitones.abs() < 0.001 {
            // No shift — pass through (still write to buffer for state continuity)
            for frame in 0..num_frames {
                for ch in 0..channels.min(MAX_CHANNELS) {
                    let idx = frame * channels + ch;
                    let cs = &mut self.channels[ch];
                    cs.buf[cs.write_pos] = buffer[idx];
                    cs.write_pos = (cs.write_pos + 1) % CIRC_BUF_SIZE;
                    cs.read_pos += 1.0;
                    cs.read_pos2 += 1.0;
                }
            }
            return;
        }

        let ratio = 2.0_f64.powf(total_semitones as f64 / 12.0);
        let grain_len = self.grain_samples as usize;

        for frame in 0..num_frames {
            for ch in 0..channels.min(MAX_CHANNELS) {
                let idx = frame * channels + ch;
                let cs = &mut self.channels[ch];

                // Write input to circular buffer
                cs.buf[cs.write_pos] = buffer[idx];

                // Read two overlapping grains at modified rate
                let pos1_in_grain = cs.read_pos % self.grain_samples as f64;
                let pos2_in_grain = cs.read_pos2 % self.grain_samples as f64;

                let win1_idx = pos1_in_grain as usize;
                let win2_idx = pos2_in_grain as usize;
                let w1 = if win1_idx < grain_len { self.hann_window[win1_idx] } else { 0.0 };
                let w2 = if win2_idx < grain_len { self.hann_window[win2_idx] } else { 0.0 };

                let s1 = cs.read_interp(cs.read_pos);
                let s2 = cs.read_interp(cs.read_pos2);

                // Overlap-add
                let total_weight = w1 + w2;
                buffer[idx] = if total_weight > 1e-6 {
                    (s1 * w1 + s2 * w2) / total_weight
                } else {
                    0.0
                };

                // Advance pointers
                cs.write_pos = (cs.write_pos + 1) % CIRC_BUF_SIZE;
                cs.read_pos += ratio;
                cs.read_pos2 += ratio;

                // Wrap read pointers to stay near write pointer
                let wp = cs.write_pos as f64;
                let buf_size = CIRC_BUF_SIZE as f64;
                let max_lag = self.grain_samples as f64 * 4.0;
                let diff1 = wp - (cs.read_pos % buf_size);
                if diff1.abs() > max_lag {
                    cs.read_pos = wp - self.grain_samples as f64 * 2.0;
                }
                let diff2 = wp - (cs.read_pos2 % buf_size);
                if diff2.abs() > max_lag {
                    cs.read_pos2 = cs.read_pos + self.grain_samples as f64 * 0.5;
                }
            }
        }
    }

    fn reset(&mut self) {
        for ch in &mut self.channels {
            ch.reset();
        }
    }

    fn latency_samples(&self) -> u32 {
        self.grain_samples as u32
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("semitones".into(), self.semitones),
            ("fine_cents".into(), self.fine_cents),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params { self.set_param(name, *value); }
    }
}
