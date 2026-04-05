use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use std::f32::consts::PI;

const MAX_CHANNELS: usize = 2;
/// Circular buffer size: ~200ms at 192kHz.
const CIRC_BUF_SIZE: usize = 38400;

/// Grain size in seconds — 30ms gives good time-domain pitch shifting.
const GRAIN_SECS: f32 = 0.030;

/// Per-channel pitch shift state using dual-grain overlap-add.
struct ChannelState {
    /// Circular input buffer.
    buf: Vec<f32>,
    /// Write pointer into circular buffer.
    write_pos: usize,
    /// Total samples written (monotonic, never wraps).
    total_written: u64,
    /// Grain A: fractional read position (monotonic, offset from total_written).
    grain_a_phase: f64,
    /// Grain B: fractional read position, offset by half a grain from A.
    grain_b_phase: f64,
}

impl ChannelState {
    fn new() -> Self {
        Self {
            buf: vec![0.0; CIRC_BUF_SIZE],
            write_pos: 0,
            total_written: 0,
            grain_a_phase: 0.0,
            grain_b_phase: 0.0,
        }
    }

    fn reset(&mut self) {
        self.buf.fill(0.0);
        self.write_pos = 0;
        self.total_written = 0;
        self.grain_a_phase = 0.0;
        self.grain_b_phase = 0.0;
    }

    /// Read from circular buffer with linear interpolation.
    #[inline]
    fn read_interp(&self, pos: f64) -> f32 {
        let len = CIRC_BUF_SIZE;
        // Convert monotonic position to circular index
        let wrapped = ((pos as i64).rem_euclid(len as i64)) as usize;
        let idx0 = wrapped;
        let idx1 = (wrapped + 1) % len;
        let frac = (pos - pos.floor()) as f32;
        self.buf[idx0] * (1.0 - frac) + self.buf[idx1] * frac
    }
}

/// Granular pitch shifter using overlap-add with Hann-windowed grains.
///
/// Two grains run in parallel, offset by half a grain length. Each grain reads
/// from the circular buffer at a rate determined by the pitch ratio. When a
/// grain completes its window, it resets to a position trailing behind the
/// write pointer, ensuring it always reads valid data.
pub struct PitchShift {
    sample_rate: f32,
    semitones: f32,
    fine_cents: f32,
    channels: [ChannelState; MAX_CHANNELS],
    /// Grain size in samples.
    grain_samples: usize,
    /// Pre-computed Hann window for one grain.
    hann_window: Vec<f32>,
}

impl PitchShift {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let grain_samples = (GRAIN_SECS * sr).round() as usize;

        // Pre-compute Hann window
        let hann_window: Vec<f32> = (0..grain_samples)
            .map(|i| {
                let t = i as f32 / (grain_samples - 1).max(1) as f32;
                0.5 * (1.0 - (2.0 * PI * t).cos())
            })
            .collect();

        Self {
            sample_rate: sr,
            semitones: 0.0,
            fine_cents: 0.0,
            channels: [ChannelState::new(), ChannelState::new()],
            grain_samples,
            hann_window,
        }
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

        let total_semitones = self.semitones + self.fine_cents / 100.0;
        if total_semitones.abs() < 0.001 {
            // No shift — still feed circular buffer for state continuity
            for frame in 0..num_frames {
                for ch in 0..channels.min(MAX_CHANNELS) {
                    let idx = frame * channels + ch;
                    let cs = &mut self.channels[ch];
                    cs.buf[cs.write_pos] = buffer[idx];
                    cs.write_pos = (cs.write_pos + 1) % CIRC_BUF_SIZE;
                    cs.total_written += 1;
                }
            }
            return;
        }

        let ratio = 2.0_f64.powf(total_semitones as f64 / 12.0);
        let grain_len = self.grain_samples;
        let grain_len_f = grain_len as f64;
        let half_grain = grain_len_f * 0.5;

        for frame in 0..num_frames {
            for ch in 0..channels.min(MAX_CHANNELS) {
                let idx = frame * channels + ch;
                let cs = &mut self.channels[ch];

                // Write input to circular buffer
                cs.buf[cs.write_pos] = buffer[idx];
                let wp = cs.write_pos as f64;

                // Initialize grain phases on first sample
                if cs.total_written == 0 {
                    // Grains start trailing behind the write pointer
                    cs.grain_a_phase = 0.0;
                    cs.grain_b_phase = half_grain;
                }

                // Position within each grain's Hann window (0..grain_len)
                let a_in_grain = cs.grain_a_phase % grain_len_f;
                let b_in_grain = cs.grain_b_phase % grain_len_f;

                let a_win_idx = a_in_grain as usize;
                let b_win_idx = b_in_grain as usize;
                let w_a = if a_win_idx < grain_len { self.hann_window[a_win_idx] } else { 0.0 };
                let w_b = if b_win_idx < grain_len { self.hann_window[b_win_idx] } else { 0.0 };

                // Read positions: trail behind write pointer by grain_len,
                // offset by grain phase within the circular buffer
                let read_base = if wp >= grain_len_f { wp - grain_len_f } else { wp + CIRC_BUF_SIZE as f64 - grain_len_f };
                let s_a = cs.read_interp(read_base + a_in_grain * (1.0 - ratio) / 1.0_f64.max(ratio.abs()));
                let s_b = cs.read_interp(read_base + b_in_grain * (1.0 - ratio) / 1.0_f64.max(ratio.abs()));

                // Overlap-add with normalization
                let total_w = w_a + w_b;
                buffer[idx] = if total_w > 1e-6 {
                    (s_a * w_a + s_b * w_b) / total_w
                } else {
                    buffer[idx] // pass through if both windows are zero
                };

                // Advance
                cs.write_pos = (cs.write_pos + 1) % CIRC_BUF_SIZE;
                cs.total_written += 1;
                cs.grain_a_phase += ratio;
                cs.grain_b_phase += ratio;

                // When a grain completes, reset it to maintain overlap
                if cs.grain_a_phase >= grain_len_f * 2.0 {
                    cs.grain_a_phase -= grain_len_f;
                }
                if cs.grain_b_phase >= grain_len_f * 2.0 {
                    cs.grain_b_phase -= grain_len_f;
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
