use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use crate::lufs::LufsMeter;

/// Normalize (#2778), two ways by Mode.
///
/// **Clip** (mode 1, the default): the whole clip at Target LUFS
/// integrated, by one gain held over it, so its dynamics stay as they
/// are. The gain comes from the clip's integrated loudness through the
/// chain up to this effect, measured once over its source range: the app
/// measures it in the background and export before it renders, and they
/// write it here as `measured_lufs`, with the `measured_key` it was
/// measured for ([`normalize_key`]). Until then the gain is unity.
///
/// **Ride** (mode 0): a leveler, riding the short-term (3 s) loudness
/// toward Target as the clip plays, slowly, for material whose level
/// wanders. A different tool, kept here as the effect's second mode.
pub struct Normalize {
    sample_rate: f32,
    target_lufs: f32,
    mode: f32,
    /// The clip's integrated loudness into this effect, when measured.
    measured_lufs: f32,
    /// What it was measured for, as two exact halves of a 48-bit key.
    measured_key: [f32; 2],
    /// The leveler's gain now (linear).
    current_gain: f32,
    lufs_meter: LufsMeter,
}

impl Normalize {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        Self {
            sample_rate: sr,
            target_lufs: -14.0,
            mode: 1.0,
            measured_lufs: f32::NAN,
            measured_key: [f32::NAN; 2],
            current_gain: 1.0,
            lufs_meter: LufsMeter::new(sr, false),
        }
    }

    /// The gain Clip mode holds (linear): unity until measured.
    pub fn clip_gain(target_lufs: f32, measured_lufs: f32) -> f32 {
        if !measured_lufs.is_finite() || measured_lufs < -70.0 {
            return 1.0;
        }
        10.0_f32.powf((target_lufs - measured_lufs) / 20.0).clamp(0.01, 40.0)
    }
}

/// A clip's normalize measurement key as the two exact `f32` halves its
/// params hold: 24 bits each of `key`'s low 48.
pub fn normalize_key(key: u64) -> [f32; 2] {
    [(key & 0xFF_FFFF) as f32, ((key >> 24) & 0xFF_FFFF) as f32]
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "target_lufs", label: "Target LUFS", default: -14.0, min: -24.0, max: 0.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "mode", label: "Mode", default: 1.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
];

impl AudioEffect for Normalize {
    fn type_id(&self) -> &'static str { "normalize" }
    fn name(&self) -> &'static str { "Normalize" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "target_lufs" => Some(self.target_lufs),
            "mode" => Some(self.mode),
            "measured_lufs" => Some(self.measured_lufs),
            "measured_key_lo" => Some(self.measured_key[0]),
            "measured_key_hi" => Some(self.measured_key[1]),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "target_lufs" => self.target_lufs = value.clamp(-24.0, 0.0),
            "mode" => self.mode = if value > 0.5 { 1.0 } else { 0.0 },
            "measured_lufs" => self.measured_lufs = value,
            "measured_key_lo" => self.measured_key[0] = value,
            "measured_key_hi" => self.measured_key[1] = value,
            _ => {}
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        let num_frames = buffer.len() / channels;
        if num_frames == 0 { return; }

        let gain = if self.mode > 0.5 {
            Self::clip_gain(self.target_lufs, self.measured_lufs)
        } else {
            // Ride: toward the short-term loudness's gain, per block by
            // its whole length, down in about 2 s and up in about 4 s, so
            // a word does not pump it.
            self.lufs_meter.process(buffer);
            let measured = self.lufs_meter.short_term_lufs;
            if measured.is_finite() && measured > -70.0 {
                let desired = 10.0_f32.powf((self.target_lufs - measured) / 20.0).clamp(0.01, 40.0);
                let secs = if desired < self.current_gain { 2.0 } else { 4.0 };
                let k = (-(num_frames as f32) / (secs * self.sample_rate)).exp();
                self.current_gain = k * self.current_gain + (1.0 - k) * desired;
            }
            self.current_gain
        };
        if (gain - 1.0).abs() > f32::EPSILON {
            for s in buffer.iter_mut() {
                *s *= gain;
            }
        }
    }

    fn reset(&mut self) {
        self.current_gain = 1.0;
        self.lufs_meter.reset();
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        let mut out = vec![
            ("target_lufs".into(), self.target_lufs),
            ("mode".into(), self.mode),
        ];
        if self.measured_lufs.is_finite() {
            out.push(("measured_lufs".into(), self.measured_lufs));
            out.push(("measured_key_lo".into(), self.measured_key[0]));
            out.push(("measured_key_hi".into(), self.measured_key[1]));
        }
        out
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params { self.set_param(name, *value); }
    }
}
