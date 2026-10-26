use super::biquad::{BiquadFilter, FilterType};
use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

const NUM_BANDS: usize = 8;
const MAX_CHANNELS: usize = 2;

/// Where the bands meet, Hz: eight bands from the lows to the air.
const CROSSOVERS: [f32; NUM_BANDS - 1] = [150.0, 350.0, 700.0, 1400.0, 2800.0, 5600.0, 11000.0];

/// The noise floor is the lowest a band's level has been over the last
/// `FLOOR_BLOCKS` blocks of `FLOOR_BLOCK_SECS` (minimum statistics): the
/// pauses between words, which a word does not lift.
const FLOOR_BLOCKS: usize = 6;
const FLOOR_BLOCK_SECS: f32 = 0.25;

/// One channel: its low-passes at each crossover, and per band the
/// level, the lows of its recent blocks, and the gain applied.
struct ChannelState {
    lows: [BiquadFilter; NUM_BANDS - 1],
    envelope: [f32; NUM_BANDS],
    /// The lowest level in each recent block, and in the one under way.
    block_lows: [[f32; FLOOR_BLOCKS]; NUM_BANDS],
    current_low: [f32; NUM_BANDS],
    gain: [f32; NUM_BANDS],
    /// Samples into the block under way, and since the start: the first
    /// few are the follower settling, not the floor.
    in_block: usize,
    seen: usize,
}

impl ChannelState {
    fn new(sr: f32) -> Self {
        Self {
            lows: std::array::from_fn(|i| {
                let mut f = BiquadFilter::new();
                f.set_params(FilterType::LowPass, CROSSOVERS[i], 0.0, std::f32::consts::FRAC_1_SQRT_2, sr);
                f
            }),
            envelope: [0.0; NUM_BANDS],
            block_lows: [[f32::INFINITY; FLOOR_BLOCKS]; NUM_BANDS],
            current_low: [f32::INFINITY; NUM_BANDS],
            gain: [1.0; NUM_BANDS],
            in_block: 0,
            seen: 0,
        }
    }
}

/// Multi-band gate (#2778).
///
/// The signal is split into eight bands that add back up to it exactly
/// (each the difference of the low-passes at its edges), so where nothing
/// is gated the sound is untouched. Each band follows its own noise floor
/// (the low its level keeps returning to between words) and is turned
/// down by Reduction while it stays within Sensitivity of that floor: a
/// floor of hiss, hum or room tone goes down, the voice over it stays.
/// Allocation-free in `process()`.
pub struct NoiseReduction {
    sample_rate: f32,
    reduction: f32,
    sensitivity: f32,
    smoothing: f32,
    channels: [ChannelState; MAX_CHANNELS],
}

impl NoiseReduction {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        Self {
            sample_rate: sr,
            reduction: 12.0,
            sensitivity: 50.0,
            smoothing: 50.0,
            channels: [ChannelState::new(sr), ChannelState::new(sr)],
        }
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "reduction", label: "Reduction", default: 12.0, min: 0.0, max: 40.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "sensitivity", label: "Sensitivity", default: 50.0, min: 0.0, max: 100.0, unit: ParamUnit::Percent },
    AudioParamDescriptor { name: "smoothing", label: "Smoothing", default: 50.0, min: 0.0, max: 100.0, unit: ParamUnit::Percent },
];

impl AudioEffect for NoiseReduction {
    fn type_id(&self) -> &'static str { "noise_reduction" }
    fn name(&self) -> &'static str { "Multi-band Gate" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "reduction" => Some(self.reduction),
            "sensitivity" => Some(self.sensitivity),
            "smoothing" => Some(self.smoothing),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "reduction" => self.reduction = value.clamp(0.0, 40.0),
            "sensitivity" => self.sensitivity = value.clamp(0.0, 100.0),
            "smoothing" => self.smoothing = value.clamp(0.0, 100.0),
            _ => {}
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;
        if num_frames == 0 { return; }

        let sr = self.sample_rate;
        let per = |ms: f32| (-1.0 / (ms * 0.001 * sr)).exp();
        // How far above its floor a band counts as signal: 2 dB at
        // sensitivity 0 (only the quietest is gated) to 12 dB at 100.
        let margin = 10.0_f32.powf((2.0 + self.sensitivity * 0.1) / 20.0);
        let closed = 10.0_f32.powf(-self.reduction / 20.0);
        let smooth = self.smoothing / 100.0;
        // The level follower, and the gain opening fast and closing as
        // smoothly as asked.
        let (env_up, env_down) = (per(2.0), per(20.0));
        let (open, close) = (per(1.0 + smooth * 10.0), per(20.0 + smooth * 200.0));
        let block = (FLOOR_BLOCK_SECS * sr) as usize;
        let settle = (0.01 * sr) as usize;

        for frame in 0..num_frames {
            for ch in 0..channels.min(MAX_CHANNELS) {
                let idx = frame * channels + ch;
                let x = buffer[idx];
                let cs = &mut self.channels[ch];
                cs.seen += 1;
                let mut below = 0.0;
                let mut out = 0.0;
                for b in 0..NUM_BANDS {
                    let low = if b < NUM_BANDS - 1 { cs.lows[b].process(x) } else { x };
                    let band = low - below;
                    below = low;

                    let level = band.abs();
                    let k = if level > cs.envelope[b] { env_up } else { env_down };
                    cs.envelope[b] = k * cs.envelope[b] + (1.0 - k) * level;
                    let env = cs.envelope[b];
                    if cs.seen > settle {
                        cs.current_low[b] = cs.current_low[b].min(env);
                    }
                    let floor = cs.block_lows[b].iter().copied().fold(cs.current_low[b], f32::min);
                    // No floor yet: everything is signal.
                    let target = if !floor.is_finite() || env > floor * margin { 1.0 } else { closed };
                    let g = &mut cs.gain[b];
                    let k = if target > *g { open } else { close };
                    *g = k * *g + (1.0 - k) * target;
                    out += band * *g;
                }
                buffer[idx] = out;
                if ch == 0 {
                    // The block's lows move on together, for every channel.
                    self.channels[0].in_block += 1;
                }
            }
            if self.channels[0].in_block >= block {
                for cs in self.channels.iter_mut().take(channels.min(MAX_CHANNELS)) {
                    for b in 0..NUM_BANDS {
                        cs.block_lows[b].rotate_right(1);
                        cs.block_lows[b][0] = cs.current_low[b];
                        cs.current_low[b] = f32::INFINITY;
                    }
                }
                self.channels[0].in_block = 0;
            }
        }
    }

    fn reset(&mut self) {
        self.channels = [ChannelState::new(self.sample_rate), ChannelState::new(self.sample_rate)];
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("reduction".into(), self.reduction),
            ("sensitivity".into(), self.sensitivity),
            ("smoothing".into(), self.smoothing),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params { self.set_param(name, *value); }
    }
}
