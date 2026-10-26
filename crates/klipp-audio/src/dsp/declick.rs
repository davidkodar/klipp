use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

const MAX_CHANNELS: usize = 2;
/// The longest click repaired, at the longest Max Click (10 ms) at
/// 192 kHz, and the lookahead that needs.
const MAX_LOOKAHEAD: usize = 1920 + 2;

/// One channel's lookahead: the samples not yet given back, each marked
/// whether it is part of a click, and what detection knows.
struct ChannelState {
    ring: Vec<f32>,
    marked: Vec<bool>,
    /// The next slot written; the slot `delay` behind it is given back.
    write_pos: usize,
    /// The input's previous sample, for its jumps.
    prev_input: f32,
    /// The mean square of the jumps between samples outside clicks: what
    /// a click stands out from. A click's own jumps never feed it.
    jump_ms: f32,
    /// Samples into the click being marked, when one is, and which way
    /// it jumped in: its edge can take more than a sample (a resampled
    /// click rises over two), and only a jump back ends it.
    in_click: Option<(usize, bool)>,
    /// The last sample given back that was not a click's.
    last_good: f32,
}

impl ChannelState {
    fn new() -> Self {
        Self {
            ring: vec![0.0; MAX_LOOKAHEAD],
            marked: vec![false; MAX_LOOKAHEAD],
            write_pos: 0,
            prev_input: 0.0,
            jump_ms: 0.0,
            in_click: None,
            last_good: 0.0,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

/// De-click (#2778): finds a click as a sudden jump far above how much
/// the signal moves sample to sample, marks it until it jumps back (or
/// Max Click runs out), and bridges the marked samples with a straight
/// line between the good samples either side. It looks ahead by the
/// longest click it repairs, so it reports that as latency.
pub struct DeClick {
    sample_rate: f32,
    threshold: f32,
    max_click_ms: f32,
    channels: [ChannelState; MAX_CHANNELS],
    /// How far behind the input the output is, in samples.
    delay: usize,
}

impl DeClick {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        Self {
            sample_rate: sr,
            threshold: 50.0,
            max_click_ms: 2.0,
            channels: [ChannelState::new(), ChannelState::new()],
            delay: ((0.010 * sr).ceil() as usize + 1).min(MAX_LOOKAHEAD - 1),
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

        // How far a jump must stand out: 2x (threshold 100, eager) to 20x
        // (threshold 0) the usual jump.
        let stand_out = 2.0 + (100.0 - self.threshold) * 0.18;
        let max_click = ((self.max_click_ms * 0.001 * self.sample_rate) as usize).clamp(1, self.delay - 1);
        let delay = self.delay;
        let len = MAX_LOOKAHEAD;

        for frame in 0..num_frames {
            for ch in 0..channels.min(MAX_CHANNELS) {
                let idx = frame * channels + ch;
                let input = buffer[idx];
                let cs = &mut self.channels[ch];

                // Detect, on the way in.
                let step = input - cs.prev_input;
                let jump = step.abs();
                let rising = step > 0.0;
                cs.prev_input = input;
                let usual = cs.jump_ms.sqrt().max(1e-6);
                let sudden = jump > usual * stand_out && jump > 0.01;
                let marked = match cs.in_click {
                    // A jump back, or the longest click, ends it; this
                    // sample is the first good one again.
                    Some((n, up)) if (sudden && rising != up) || n >= max_click => {
                        cs.in_click = None;
                        false
                    }
                    Some((n, up)) => {
                        cs.in_click = Some((n + 1, up));
                        true
                    }
                    None if sudden => {
                        cs.in_click = Some((1, rising));
                        true
                    }
                    None => false,
                };
                if !marked {
                    cs.jump_ms = 0.999 * cs.jump_ms + 0.001 * jump * jump;
                }
                cs.ring[cs.write_pos] = input;
                cs.marked[cs.write_pos] = marked;

                // Repair, on the way out: `delay` samples behind.
                let out_pos = (cs.write_pos + len - delay) % len;
                let out = if cs.marked[out_pos] {
                    // The click's end within the lookahead, and the good
                    // sample after it: a straight line across.
                    let mut k = 1;
                    while k < delay && cs.marked[(out_pos + k) % len] {
                        k += 1;
                    }
                    let next_good = cs.ring[(out_pos + k) % len];
                    let v = cs.last_good + (next_good - cs.last_good) / (k + 1) as f32;
                    // The bridge goes on from here.
                    cs.last_good = v;
                    v
                } else {
                    cs.last_good = cs.ring[out_pos];
                    cs.ring[out_pos]
                };
                cs.write_pos = (cs.write_pos + 1) % len;
                buffer[idx] = out;
            }
        }
    }

    fn reset(&mut self) {
        for ch in &mut self.channels {
            ch.reset();
        }
    }

    fn latency_samples(&self) -> u32 {
        self.delay as u32
    }

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
