use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

const MAX_CHANNELS: usize = 2;
/// Circular buffer size: ~200ms at 192kHz.
const CIRC_BUF_SIZE: usize = 38400;

/// Grain size in seconds — 30ms gives good time-domain pitch shifting.
const GRAIN_SECS: f32 = 0.050;

/// Per-channel pitch shift state using dual-grain overlap-add.
struct ChannelState {
    buf: Vec<f32>,
    write_pos: usize,
    /// Where the first read tap is in its sweep across the grain, 0..1;
    /// the second is half a grain on.
    phase: f64,
}

impl ChannelState {
    fn new() -> Self {
        Self { buf: vec![0.0; CIRC_BUF_SIZE], write_pos: 0, phase: 0.0 }
    }

    fn reset(&mut self) {
        self.buf.fill(0.0);
        self.write_pos = 0;
        self.phase = 0.0;
    }

    /// The sample `delay` samples before the last one written, read
    /// between samples with a 4-point Hermite curve, which keeps the top
    /// of a voice where a straight line between samples dulls it.
    #[inline]
    fn read_delayed(&self, delay: f64) -> f32 {
        let len = CIRC_BUF_SIZE as f64;
        let pos = (self.write_pos as f64 - 1.0 - delay).rem_euclid(len);
        let i = pos.floor();
        let t = (pos - i) as f32;
        let at = |k: f64| self.buf[((i + k).rem_euclid(len)) as usize];
        let (y0, y1, y2, y3) = (at(-1.0), at(0.0), at(1.0), at(2.0));
        let c1 = 0.5 * (y2 - y0);
        let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
        ((c3 * t + c2) * t + c1) * t + y1
    }
}

pub struct PitchShift {
    semitones: f32,
    fine_cents: f32,
    channels: [ChannelState; MAX_CHANNELS],
    /// The grain a tap's delay sweeps, in samples.
    grain_samples: usize,
}

impl PitchShift {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let grain_samples = (GRAIN_SECS * sr).round() as usize;
        Self {
            semitones: 0.0,
            fine_cents: 0.0,
            channels: [ChannelState::new(), ChannelState::new()],
            grain_samples,
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

    /// Two read taps on a delay line (#2778). Each tap's delay changes by
    /// `1 − ratio` samples a sample, so it reads `ratio` samples for every
    /// one written: the pitch times `ratio`. Its delay sweeps one grain
    /// and wraps; the taps are half a grain apart and crossfaded at equal
    /// power, so each wrap happens where its tap is silent.
    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;
        if num_frames == 0 { return; }

        let total_semitones = self.semitones + self.fine_cents / 100.0;
        let ratio = 2.0_f64.powf(total_semitones as f64 / 12.0);
        let shifting = total_semitones.abs() >= 0.001;
        let grain = self.grain_samples as f64;
        // The sweep's step: a delay changing by (1 − ratio) samples a
        // sample, as a share of the grain.
        let step = (1.0 - ratio) / grain;
        // The nearest a tap reads: past the interpolator's reach ahead.
        const NEAREST: f64 = 3.0;

        for frame in 0..num_frames {
            for ch in 0..channels.min(MAX_CHANNELS) {
                let idx = frame * channels + ch;
                let cs = &mut self.channels[ch];
                cs.buf[cs.write_pos] = buffer[idx];
                cs.write_pos = (cs.write_pos + 1) % CIRC_BUF_SIZE;
                if !shifting {
                    continue;
                }
                let pa = cs.phase;
                let pb = (pa + 0.5).fract();
                // Equal power: the taps hold the same sound at unrelated
                // phases, so their powers, not their amplitudes, add.
                let (sa, ca) = (std::f64::consts::PI * pa).sin_cos();
                let (wa, wb) = (sa as f32, ca.abs() as f32);
                let a = cs.read_delayed(NEAREST + pa * grain);
                let b = cs.read_delayed(NEAREST + pb * grain);
                buffer[idx] = a * wa + b * wb;
                cs.phase = (pa + step).rem_euclid(1.0);
            }
        }
    }

    fn reset(&mut self) {
        for ch in &mut self.channels {
            ch.reset();
        }
    }

    fn latency_samples(&self) -> u32 {
        // The taps read half a grain back on average.
        (self.grain_samples / 2) as u32
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
