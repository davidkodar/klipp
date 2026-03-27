use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

// Freeverb (Schroeder reverb): 8 parallel comb filters into 4 series allpass filters.
// Stereo: slightly offset delay lengths for L/R.

const NUM_COMBS: usize = 8;
const NUM_ALLPASSES: usize = 4;
const STEREO_SPREAD: usize = 23;

// Comb filter delay lengths (in samples at 44100 Hz), from original Freeverb
const COMB_LENGTHS: [usize; NUM_COMBS] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
// Allpass delay lengths
const ALLPASS_LENGTHS: [usize; NUM_ALLPASSES] = [556, 441, 341, 225];

const MAX_PRE_DELAY_SAMPLES: usize = 4410; // 100ms at 44100

struct CombFilter {
    buffer: Vec<f32>,
    index: usize,
    filter_store: f32,
}

impl CombFilter {
    fn new(size: usize) -> Self {
        Self {
            buffer: vec![0.0; size],
            index: 0,
            filter_store: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32, feedback: f32, damp1: f32, damp2: f32) -> f32 {
        let output = self.buffer[self.index];
        self.filter_store = output * damp2 + self.filter_store * damp1;
        self.buffer[self.index] = input + self.filter_store * feedback;
        self.index += 1;
        if self.index >= self.buffer.len() {
            self.index = 0;
        }
        output
    }

    fn reset(&mut self) {
        self.buffer.iter_mut().for_each(|s| *s = 0.0);
        self.index = 0;
        self.filter_store = 0.0;
    }
}

struct AllpassFilter {
    buffer: Vec<f32>,
    index: usize,
}

impl AllpassFilter {
    fn new(size: usize) -> Self {
        Self {
            buffer: vec![0.0; size],
            index: 0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let buffered = self.buffer[self.index];
        let output = buffered - input;
        self.buffer[self.index] = input + buffered * 0.5;
        self.index += 1;
        if self.index >= self.buffer.len() {
            self.index = 0;
        }
        output
    }

    fn reset(&mut self) {
        self.buffer.iter_mut().for_each(|s| *s = 0.0);
        self.index = 0;
    }
}

pub struct Reverb {
    sample_rate: f32,
    room_size: f32,
    damping: f32,
    wet: f32,
    width: f32,
    pre_delay_ms: f32,
    // Derived
    feedback: f32,
    damp1: f32,
    damp2: f32,
    // L/R comb and allpass filters
    combs_l: Vec<CombFilter>,
    combs_r: Vec<CombFilter>,
    allpasses_l: Vec<AllpassFilter>,
    allpasses_r: Vec<AllpassFilter>,
    // Pre-delay line
    pre_delay_buf: Vec<f32>,
    pre_delay_index: usize,
    pre_delay_samples: usize,
    dirty: bool,
}

impl Reverb {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let scale = sr / 44100.0;

        let combs_l: Vec<CombFilter> = COMB_LENGTHS
            .iter()
            .map(|&len| CombFilter::new(((len as f32) * scale) as usize + 1))
            .collect();
        let combs_r: Vec<CombFilter> = COMB_LENGTHS
            .iter()
            .map(|&len| CombFilter::new(((len as f32) * scale) as usize + STEREO_SPREAD + 1))
            .collect();
        let allpasses_l: Vec<AllpassFilter> = ALLPASS_LENGTHS
            .iter()
            .map(|&len| AllpassFilter::new(((len as f32) * scale) as usize + 1))
            .collect();
        let allpasses_r: Vec<AllpassFilter> = ALLPASS_LENGTHS
            .iter()
            .map(|&len| AllpassFilter::new(((len as f32) * scale) as usize + STEREO_SPREAD + 1))
            .collect();

        let max_pre = ((MAX_PRE_DELAY_SAMPLES as f32) * scale) as usize + 1;

        let mut rev = Self {
            sample_rate: sr,
            room_size: 0.5,
            damping: 0.5,
            wet: 0.3,
            width: 1.0,
            pre_delay_ms: 0.0,
            feedback: 0.0,
            damp1: 0.0,
            damp2: 0.0,
            combs_l,
            combs_r,
            allpasses_l,
            allpasses_r,
            pre_delay_buf: vec![0.0; max_pre],
            pre_delay_index: 0,
            pre_delay_samples: 0,
            dirty: true,
        };
        rev.recompute();
        rev
    }

    fn recompute(&mut self) {
        self.feedback = self.room_size * 0.28 + 0.7; // scale 0..1 -> 0.7..0.98
        self.damp1 = self.damping * 0.4; // scale 0..1 -> 0..0.4
        self.damp2 = 1.0 - self.damp1;
        self.pre_delay_samples = ((self.pre_delay_ms * 0.001 * self.sample_rate) as usize)
            .min(self.pre_delay_buf.len() - 1);
        self.dirty = false;
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "room_size", label: "Room Size", default: 0.5, min: 0.0, max: 1.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "damping", label: "Damping", default: 0.5, min: 0.0, max: 1.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "wet", label: "Wet", default: 0.3, min: 0.0, max: 1.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "width", label: "Width", default: 1.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "pre_delay", label: "Pre-Delay", default: 0.0, min: 0.0, max: 100.0, unit: ParamUnit::Ms },
];

impl AudioEffect for Reverb {
    fn type_id(&self) -> &'static str { "reverb" }
    fn name(&self) -> &'static str { "Reverb" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "room_size" => Some(self.room_size),
            "damping" => Some(self.damping),
            "wet" => Some(self.wet),
            "width" => Some(self.width),
            "pre_delay" => Some(self.pre_delay_ms),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "room_size" => self.room_size = value.clamp(0.0, 1.0),
            "damping" => self.damping = value.clamp(0.0, 1.0),
            "wet" => self.wet = value.clamp(0.0, 1.0),
            "width" => self.width = value.clamp(0.0, 1.0),
            "pre_delay" => self.pre_delay_ms = value.clamp(0.0, 100.0),
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

        let is_stereo = channels >= 2;
        let num_frames = buffer.len() / channels;

        let wet = self.wet;
        let dry = 1.0 - wet;
        let wet1 = wet * (self.width * 0.5 + 0.5);
        let wet2 = wet * ((1.0 - self.width) * 0.5);

        let feedback = self.feedback;
        let damp1 = self.damp1;
        let damp2 = self.damp2;

        for frame in 0..num_frames {
            let in_l = buffer[frame * channels];
            let in_r = if is_stereo { buffer[frame * channels + 1] } else { in_l };
            let mono_in = (in_l + in_r) * 0.5;

            // Pre-delay
            let delayed = if self.pre_delay_samples > 0 {
                let read_idx = (self.pre_delay_index + self.pre_delay_buf.len()
                    - self.pre_delay_samples)
                    % self.pre_delay_buf.len();
                let out = self.pre_delay_buf[read_idx];
                self.pre_delay_buf[self.pre_delay_index] = mono_in;
                self.pre_delay_index = (self.pre_delay_index + 1) % self.pre_delay_buf.len();
                out
            } else {
                mono_in
            };

            // Parallel comb filters
            let mut out_l = 0.0_f32;
            let mut out_r = 0.0_f32;
            for comb in &mut self.combs_l {
                out_l += comb.process(delayed, feedback, damp1, damp2);
            }
            for comb in &mut self.combs_r {
                out_r += comb.process(delayed, feedback, damp1, damp2);
            }

            // Series allpass filters
            for ap in &mut self.allpasses_l {
                out_l = ap.process(out_l);
            }
            for ap in &mut self.allpasses_r {
                out_r = ap.process(out_r);
            }

            // Mix
            buffer[frame * channels] = in_l * dry + out_l * wet1 + out_r * wet2;
            if is_stereo {
                buffer[frame * channels + 1] = in_r * dry + out_r * wet1 + out_l * wet2;
            }
        }
    }

    fn reset(&mut self) {
        for c in &mut self.combs_l { c.reset(); }
        for c in &mut self.combs_r { c.reset(); }
        for a in &mut self.allpasses_l { a.reset(); }
        for a in &mut self.allpasses_r { a.reset(); }
        self.pre_delay_buf.iter_mut().for_each(|s| *s = 0.0);
        self.pre_delay_index = 0;
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("room_size".into(), self.room_size),
            ("damping".into(), self.damping),
            ("wet".into(), self.wet),
            ("width".into(), self.width),
            ("pre_delay".into(), self.pre_delay_ms),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params {
            self.set_param(name, *value);
        }
    }
}
