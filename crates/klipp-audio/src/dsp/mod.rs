pub mod chorus;
pub mod compressor;
pub mod declick;
pub mod deesser;
pub mod delay;
pub mod eq;
pub mod gate;
pub mod limiter;
pub mod noise_reduction;
pub mod normalize;
pub mod pitch_shift;
pub mod reverb;

pub use klipp_core::AudioEffectInstance;

/// Describes a parameter unit for UI display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamUnit {
    Db,
    Hz,
    Ms,
    Percent,
    Ratio,
    None,
}

/// Static descriptor for one effect parameter.
pub struct AudioParamDescriptor {
    pub name: &'static str,
    pub label: &'static str,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub unit: ParamUnit,
}

/// Context for processing.
pub struct AudioProcessContext {
    pub sample_rate: u32,
    pub channels: u32,
    pub clip_time_secs: f64,
}

/// Core trait for all audio effects.
pub trait AudioEffect: Send {
    fn type_id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn param_descriptors(&self) -> &[AudioParamDescriptor];
    fn get_param(&self, name: &str) -> Option<f32>;
    fn set_param(&mut self, name: &str, value: f32);
    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext);
    fn reset(&mut self);
    fn latency_samples(&self) -> u32 {
        0
    }
    fn serialize_params(&self) -> Vec<(String, f32)>;
    fn deserialize_params(&mut self, params: &[(String, f32)]);
}

/// Runtime effect chain processor.
pub struct AudioEffectChain {
    slots: Vec<ChainSlot>,
    dry_buffer: Vec<f32>,
}

struct ChainSlot {
    effect: Box<dyn AudioEffect>,
    enabled: bool,
    wet_dry: f32,
}

impl AudioEffectChain {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            dry_buffer: Vec::new(),
        }
    }

    pub fn from_instances(instances: &[AudioEffectInstance], sample_rate: u32) -> Self {
        let mut chain = Self::new();
        for inst in instances {
            if let Some(mut effect) = create_effect(&inst.effect_type, sample_rate) {
                effect.deserialize_params(
                    &inst
                        .params
                        .iter()
                        .map(|(k, v)| (k.clone(), *v))
                        .collect::<Vec<_>>(),
                );
                chain.slots.push(ChainSlot {
                    effect,
                    enabled: inst.enabled,
                    wet_dry: inst.wet_dry,
                });
            }
        }
        chain
    }

    pub fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        if self.dry_buffer.len() < buffer.len() {
            self.dry_buffer.resize(buffer.len(), 0.0);
        }

        for slot_idx in 0..self.slots.len() {
            if !self.slots[slot_idx].enabled {
                continue;
            }
            let wet_dry = self.slots[slot_idx].wet_dry;
            if (wet_dry - 1.0).abs() < 0.001 {
                self.slots[slot_idx].effect.process(buffer, ctx);
            } else {
                self.dry_buffer[..buffer.len()].copy_from_slice(buffer);
                self.slots[slot_idx].effect.process(buffer, ctx);
                let wet = wet_dry;
                let dry = 1.0 - wet;
                for (i, sample) in buffer.iter_mut().enumerate() {
                    *sample = *sample * wet + self.dry_buffer[i] * dry;
                }
            }
        }
    }

    pub fn set_param(&mut self, effect_index: usize, name: &str, value: f32) {
        if let Some(slot) = self.slots.get_mut(effect_index) {
            slot.effect.set_param(name, value);
        }
    }

    pub fn set_enabled(&mut self, effect_index: usize, enabled: bool) {
        if let Some(slot) = self.slots.get_mut(effect_index) {
            slot.enabled = enabled;
        }
    }

    pub fn set_wet_dry(&mut self, effect_index: usize, wet_dry: f32) {
        if let Some(slot) = self.slots.get_mut(effect_index) {
            slot.wet_dry = wet_dry;
        }
    }

    pub fn reset(&mut self) {
        for slot in &mut self.slots {
            slot.effect.reset();
        }
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }
}

impl Default for AudioEffectChain {
    fn default() -> Self {
        Self::new()
    }
}

/// Factory: create effect by type ID.
pub fn create_effect(type_id: &str, sample_rate: u32) -> Option<Box<dyn AudioEffect>> {
    match type_id {
        "eq" => Some(Box::new(eq::ParametricEq::new(sample_rate))),
        "compressor" => Some(Box::new(compressor::Compressor::new(sample_rate))),
        "limiter" => Some(Box::new(limiter::Limiter::new(sample_rate))),
        "gate" => Some(Box::new(gate::NoiseGate::new(sample_rate))),
        "reverb" => Some(Box::new(reverb::Reverb::new(sample_rate))),
        "delay" => Some(Box::new(delay::Delay::new(sample_rate))),
        "deesser" => Some(Box::new(deesser::DeEsser::new(sample_rate))),
        "noise_reduction" => Some(Box::new(noise_reduction::NoiseReduction::new(sample_rate))),
        "declick" => Some(Box::new(declick::DeClick::new(sample_rate))),
        "normalize" => Some(Box::new(normalize::Normalize::new(sample_rate))),
        "pitch_shift" => Some(Box::new(pitch_shift::PitchShift::new(sample_rate))),
        "chorus" => Some(Box::new(chorus::Chorus::new(sample_rate))),
        _ => None,
    }
}

/// List all available built-in effect types.
pub fn available_effects() -> Vec<(&'static str, &'static str)> {
    vec![
        ("eq", "Parametric EQ"),
        ("compressor", "Compressor"),
        ("limiter", "Limiter"),
        ("gate", "Noise Gate"),
        ("reverb", "Reverb"),
        ("delay", "Delay"),
        ("deesser", "De-Esser"),
        ("noise_reduction", "Noise Reduction"),
        ("declick", "De-click"),
        ("normalize", "Normalize"),
        ("pitch_shift", "Pitch Shift"),
        ("chorus", "Chorus"),
    ]
}
