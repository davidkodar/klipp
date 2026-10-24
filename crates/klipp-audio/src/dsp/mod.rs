#[cfg(test)]
mod tests;

pub mod biquad;
pub mod chorus;
pub mod compressor;
pub mod multiband_compressor;
pub mod declick;
pub mod deesser;
pub mod delay;
pub mod eq;
pub mod gate;
pub mod high_pass;
pub mod denoiser;
pub mod resample;
pub mod voice_isolation;
pub mod hum_removal;
pub mod limiter;
pub mod noise_reduction;
pub mod normalize;
pub mod pitch_shift;
pub mod reverb;
pub mod varispeed;
pub mod stretch;

pub use klipp_core::AudioEffectInstance;

/// Describes a parameter unit for UI display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamUnit {
    Db,
    Hz,
    Ms,
    /// Stored as 0..100 and shown as is.
    Percent,
    /// Stored as 0..1 and shown as a percentage (0.75 reads "75%").
    Fraction,
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

impl AudioParamDescriptor {
    /// Whether this parameter can be safely automated mid-stream.
    ///
    /// **Performance** params (gain, threshold, frequency, mix, drive) are
    /// keyframable: changing them per-buffer produces a smooth musical
    /// effect (with sub-chunk smoothing, no audible step).
    ///
    /// **Structural** params (compressor attack/release in ms, FFT window
    /// size, filter type, look-ahead) are NOT keyframable: changing them
    /// mid-stream resets internal state, glitches the output, or is
    /// physically meaningless across a sample-boundary. The inspector
    /// hides the diamond toggle for these.
    ///
    /// Default behaviour: pattern-match on common structural param names.
    /// An effect that needs different semantics for one of its params can
    /// override `AudioEffect::param_keyframable` (rare).
    pub fn is_keyframable(&self) -> bool {
        // The names below are the union of every structural param across
        // built-in effects (compressor/gate/limiter attack+release,
        // pitch_shift FFT internals, deesser/eq filter rebuild boundaries,
        // multiband crossovers). Performance params — threshold, ratio,
        // gain, freq used as a tone control, mix, wet, dry, depth, rate,
        // makeup, knee, sensitivity — fall through to true.
        !matches!(
            self.name,
            "attack"
                | "release"
                | "lookahead"
                | "window"
                | "window_size"
                | "fft_size"
                | "filter_type"
                | "filter_mode"
                | "crossover_low"
                | "crossover_mid"
                | "crossover_high"
                | "bands"
                | "shape"
        )
    }
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
    /// Get ready to process `channels`-channel audio before the first
    /// block, for effects with a costly start (Voice Isolation builds its
    /// model engine). Real-time feeds call this when they build a chain,
    /// so the start does not stall the first block after Play.
    fn prepare(&mut self, _channels: usize) {}
    fn latency_samples(&self) -> u32 {
        0
    }
    fn serialize_params(&self) -> Vec<(String, f32)>;
    fn deserialize_params(&mut self, params: &[(String, f32)]);

    /// Whether `param_name` is safe to automate via keyframes mid-stream.
    /// Default delegates to `AudioParamDescriptor::is_keyframable` so the
    /// common name-based heuristic covers most effects. Override only when
    /// an effect's param semantics diverge from the defaults — e.g. a
    /// "tone" knob whose name happens to be "freq" but is a smooth tone
    /// control, not a filter-pole rebuild.
    fn param_keyframable(&self, param_name: &str) -> bool {
        self.param_descriptors()
            .iter()
            .find(|d| d.name == param_name)
            .map(|d| d.is_keyframable())
            .unwrap_or(false)
    }
}

/// Sub-chunk size for keyframed processing. 64 stereo frames at 48kHz =
/// ~1.33ms — fine enough for smooth automation, coarse enough that
/// per-effect coefficient rebuilds (e.g. EQ biquads, LADSPA set_param)
/// don't dominate CPU.
const SUB_CHUNK_FRAMES: usize = 64;

/// Runtime effect chain processor.
pub struct AudioEffectChain {
    slots: Vec<ChainSlot>,
    dry_buffer: Vec<f32>,
    /// Output frames still to drop for [`Self::process_aligned`]; `None`
    /// until the first aligned call after construction or `reset`.
    align_trim_frames: Option<usize>,
    /// Effect types of the instances the chain was built from, in order
    /// (an instance whose effect could not load has no slot).
    built_from: Vec<String>,
}

struct ChainSlot {
    effect: Box<dyn AudioEffect>,
    /// Index of the instance this slot was built from.
    instance_index: usize,
    enabled: bool,
    wet_dry: f32,
    /// Keyframe tracks for individual params (name → kfs). Time domain is
    /// clip-relative seconds (matches the video keyframe convention).
    param_kfs: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    /// Reserved bypass-keyframe track. Value 0 = bypassed, 1 = active.
    /// Step interpolation is the natural default but Linear/Bezier work
    /// too — anything > 0.5 reads as "active" at evaluation time.
    bypass_kfs: Vec<klipp_core::Keyframe>,
    /// Reserved wet/dry-keyframe track. Value in [0, 1].
    wet_dry_kfs: Vec<klipp_core::Keyframe>,
}

impl ChainSlot {
    fn has_kfs(&self) -> bool {
        !self.param_kfs.is_empty()
            || !self.bypass_kfs.is_empty()
            || !self.wet_dry_kfs.is_empty()
    }
}

impl AudioEffectChain {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            dry_buffer: Vec::new(),
            align_trim_frames: None,
            built_from: Vec::new(),
        }
    }

    pub fn from_instances(instances: &[AudioEffectInstance], sample_rate: u32) -> Self {
        Self::from_instances_with_kfs(instances, &std::collections::HashMap::new(), sample_rate)
    }

    /// Build a chain with optional per-effect keyframe tracks.
    ///
    /// `keyframes_by_key` is the same shape as `clip.audio_effect_keyframes`:
    /// keyed `"{effect_index}:{param_name}"`, where `effect_index` matches
    /// the position in `instances`. Reserved param names:
    ///
    /// - `"__bypass"` → bypass-keyframe track (0 = off, 1 = on)
    /// - `"__wetdry"` → wet/dry-keyframe track (0..1)
    ///
    /// Any other name routes to the slot's `param_kfs[name]` track and is
    /// applied via `effect.set_param` at evaluation time.
    pub fn from_instances_with_kfs(
        instances: &[AudioEffectInstance],
        keyframes_by_key: &std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
        sample_rate: u32,
    ) -> Self {
        let mut chain = Self::new();
        chain.built_from = instances.iter().map(|i| i.effect_type.clone()).collect();
        for (idx, inst) in instances.iter().enumerate() {
            if let Some(mut effect) = create_effect(&inst.effect_type, sample_rate) {
                effect.deserialize_params(
                    &inst
                        .params
                        .iter()
                        .map(|(k, v)| (k.clone(), *v))
                        .collect::<Vec<_>>(),
                );
                let mut param_kfs = std::collections::HashMap::new();
                let mut bypass_kfs = Vec::new();
                let mut wet_dry_kfs = Vec::new();
                let prefix = format!("{idx}:");
                for (key, kfs) in keyframes_by_key {
                    if let Some(rest) = key.strip_prefix(&prefix) {
                        match rest {
                            "__bypass" => bypass_kfs = kfs.clone(),
                            "__wetdry" => wet_dry_kfs = kfs.clone(),
                            param_name => {
                                param_kfs.insert(param_name.to_string(), kfs.clone());
                            }
                        }
                    }
                }
                chain.slots.push(ChainSlot {
                    effect,
                    instance_index: idx,
                    enabled: inst.enabled,
                    wet_dry: inst.wet_dry,
                    param_kfs,
                    bypass_kfs,
                    wet_dry_kfs,
                });
            }
        }
        chain
    }

    pub fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        if self.dry_buffer.len() < buffer.len() {
            self.dry_buffer.resize(buffer.len(), 0.0);
        }

        // Static fast path — no automation anywhere in the chain. Skip
        // sub-chunking entirely so unkfd chains pay zero overhead.
        if !self.slots.iter().any(|s| s.has_kfs()) {
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
            return;
        }

        // Keyframed path: divide the buffer into ~64-frame slices, re-
        // evaluate every kf'd value at each slice's start time, and
        // process slot-by-slot. Slots without kfs in the chain still
        // run on the same slice so chain ordering is preserved.
        let channels = ctx.channels.max(1) as usize;
        let sub_chunk_floats = SUB_CHUNK_FRAMES * channels;
        let total = buffer.len();
        let mut offset = 0;
        while offset < total {
            let end = (offset + sub_chunk_floats).min(total);
            let frames_in = (offset / channels) as f64;
            let slice_time = ctx.clip_time_secs + frames_in / ctx.sample_rate as f64;
            let slice_ctx = AudioProcessContext {
                sample_rate: ctx.sample_rate,
                channels: ctx.channels,
                clip_time_secs: slice_time,
            };
            let slice_len = end - offset;

            for slot_idx in 0..self.slots.len() {
                // Bypass: kf can override the static `enabled` flag.
                let base_enabled = self.slots[slot_idx].enabled;
                let effective_enabled = if self.slots[slot_idx].bypass_kfs.is_empty() {
                    base_enabled
                } else {
                    let default = if base_enabled { 1.0 } else { 0.0 };
                    klipp_core::keyframe::evaluate(
                        &self.slots[slot_idx].bypass_kfs,
                        slice_time,
                        default,
                    ) > 0.5
                };
                if !effective_enabled {
                    continue;
                }

                // Param kfs: evaluate then set_param. Collect first so the
                // immutable iter on param_kfs is dropped before we touch
                // effect mutably.
                if !self.slots[slot_idx].param_kfs.is_empty() {
                    let updates: Vec<(String, f32)> = self.slots[slot_idx]
                        .param_kfs
                        .iter()
                        .map(|(name, kfs)| {
                            let current = self.slots[slot_idx]
                                .effect
                                .get_param(name)
                                .unwrap_or(0.0);
                            (
                                name.clone(),
                                klipp_core::keyframe::evaluate(kfs, slice_time, current),
                            )
                        })
                        .collect();
                    for (name, value) in updates {
                        self.slots[slot_idx].effect.set_param(&name, value);
                    }
                }

                // Wet/dry: kf can override the static value.
                let base_wet_dry = self.slots[slot_idx].wet_dry;
                let wet_dry = if self.slots[slot_idx].wet_dry_kfs.is_empty() {
                    base_wet_dry
                } else {
                    klipp_core::keyframe::evaluate(
                        &self.slots[slot_idx].wet_dry_kfs,
                        slice_time,
                        base_wet_dry,
                    )
                    .clamp(0.0, 1.0)
                };

                let slice = &mut buffer[offset..end];
                if (wet_dry - 1.0).abs() < 0.001 {
                    self.slots[slot_idx].effect.process(slice, &slice_ctx);
                } else {
                    self.dry_buffer[..slice_len].copy_from_slice(slice);
                    self.slots[slot_idx].effect.process(slice, &slice_ctx);
                    let wet = wet_dry;
                    let dry = 1.0 - wet;
                    for (i, sample) in slice.iter_mut().enumerate() {
                        *sample = *sample * wet + self.dry_buffer[i] * dry;
                    }
                }
            }
            offset = end;
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
        self.align_trim_frames = None;
    }

    /// Change the settings (parameters and wet/dry) of a running chain in
    /// place, without rebuilding or resetting it: a slider moved during
    /// playback is heard at once, with no gap. `instances` must describe
    /// this chain's effects in order; a chain of another shape (an effect
    /// added, removed or toggled) is left alone and false returned, since
    /// that needs a rebuild.
    pub fn apply_settings(&mut self, instances: &[AudioEffectInstance]) -> bool {
        let same_shape = instances.len() == self.built_from.len()
            && instances.iter().zip(&self.built_from).all(|(i, t)| &i.effect_type == t);
        if !same_shape {
            return false;
        }
        for slot in &mut self.slots {
            let inst = &instances[slot.instance_index];
            slot.wet_dry = inst.wet_dry;
            for (name, value) in &inst.params {
                if slot.effect.get_param(name) != Some(*value) {
                    slot.effect.set_param(name, *value);
                }
            }
        }
        true
    }

    /// Let every effect do its costly start now rather than on the first
    /// block. See [`AudioEffect::prepare`].
    pub fn prepare(&mut self, channels: usize) {
        for slot in &mut self.slots {
            slot.effect.prepare(channels);
        }
    }

    /// Process a streamed block and keep the output aligned with the input
    /// position. Effects that look ahead (Voice Isolation, the Broadband
    /// Denoiser, Pitch Shift) delay their output by their latency; after
    /// construction or `reset` (a seek), the first `total_latency_samples`
    /// frames of output are dropped once, so what follows lines up with the
    /// picture. The caller has consumed the whole input either way.
    pub fn process_aligned(&mut self, buffer: &mut Vec<f32>, ctx: &AudioProcessContext) {
        let pending = match self.align_trim_frames {
            Some(n) => n,
            None => self.total_latency_samples(ctx.sample_rate) as usize,
        };
        self.process(buffer, ctx);
        let channels = ctx.channels.max(1) as usize;
        let drop_frames = pending.min(buffer.len() / channels);
        buffer.drain(..drop_frames * channels);
        self.align_trim_frames = Some(pending - drop_frames);
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Sum of latency reported by every enabled slot in the chain.
    ///
    /// Used by `klipp_audio::AudioMixer`'s plugin-delay-compensation
    /// machinery: tracks with shorter chain latency get delay lines
    /// at their bus input so all tracks reach the mixer aligned.
    /// Disabled slots contribute 0 — bypassing a heavy plugin
    /// effectively removes its latency from the chain.
    ///
    /// Defensively clamped against absurdly-large values that would
    /// crater a transport: any single effect reporting more than
    /// `sample_rate` samples (1 second) is capped — legitimate
    /// convolution reverbs with long IRs fit comfortably under that
    /// bound, but a buggy plugin returning u32::MAX cannot mis-size
    /// a delay line by gigabytes.
    pub fn total_latency_samples(&self, sample_rate: u32) -> u32 {
        let per_effect_cap = sample_rate.max(1);
        self.slots
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.effect.latency_samples().min(per_effect_cap))
            .sum::<u32>()
            // Sum-of-effects also clamps at sample_rate so even a long
            // chain can't overshoot 1 second of accumulated delay.
            .min(per_effect_cap)
    }
}

impl Default for AudioEffectChain {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl AudioEffectChain {
    /// Test-only: push a pre-built effect into the chain. Production code
    /// goes through `from_instances` / the factory.
    pub(crate) fn push_test_slot(&mut self, effect: Box<dyn AudioEffect>, enabled: bool, wet_dry: f32) {
        let instance_index = self.slots.len();
        self.slots.push(ChainSlot {
            effect,
            instance_index,
            enabled,
            wet_dry,
            param_kfs: std::collections::HashMap::new(),
            bypass_kfs: Vec::new(),
            wet_dry_kfs: Vec::new(),
        });
    }

    /// Test-only: push a slot with attached keyframe tracks. Routing of
    /// reserved keys (`__bypass`, `__wetdry`) matches `from_instances_with_kfs`.
    pub(crate) fn push_test_slot_with_kfs(
        &mut self,
        effect: Box<dyn AudioEffect>,
        enabled: bool,
        wet_dry: f32,
        param_kfs: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
        bypass_kfs: Vec<klipp_core::Keyframe>,
        wet_dry_kfs: Vec<klipp_core::Keyframe>,
    ) {
        let instance_index = self.slots.len();
        self.slots.push(ChainSlot {
            effect,
            instance_index,
            enabled,
            wet_dry,
            param_kfs,
            bypass_kfs,
            wet_dry_kfs,
        });
    }
}

/// Plug-ins whose host refused to run them, by type id, with the
/// host's reason. Their sound goes through untouched, which nobody hears
/// as a fault: a run refused for an unconnected port went unnoticed for
/// every x42 and LSP plug-in. Kept so the plug-in can be named.
static REFUSED_RUNS: std::sync::Mutex<std::collections::BTreeMap<String, String>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// A plug-in's host refused to run it: warned about once per plug-in,
/// and kept for [`refused_run`].
pub fn note_refused_run(type_id: &str, why: &str) {
    let mut refused = REFUSED_RUNS.lock().unwrap_or_else(|e| e.into_inner());
    if !refused.contains_key(type_id) {
        tracing::warn!("{type_id} cannot run ({why}): its sound goes through untouched");
        refused.insert(type_id.to_string(), why.to_string());
    }
}

/// Why `type_id`'s host refused to run it, if it has.
pub fn refused_run(type_id: &str) -> Option<String> {
    REFUSED_RUNS.lock().unwrap_or_else(|e| e.into_inner()).get(type_id).cloned()
}

/// Whether `type_id` loads and runs here: made and given one block of
/// silence. The reason when it does not.
pub fn probe_run(type_id: &str, sample_rate: u32) -> Result<(), String> {
    if let Some(why) = refused_run(type_id) {
        return Err(why);
    }
    let mut effect = create_effect(type_id, sample_rate).ok_or_else(|| "it does not load".to_string())?;
    let mut block = vec![0.0f32; 256 * 2];
    effect.process(&mut block, &AudioProcessContext { sample_rate, channels: 2, clip_time_secs: 0.0 });
    refused_run(type_id).map_or(Ok(()), Err)
}

/// Factory: create effect by type ID.
///
/// LADSPA plugins use a composite type_id: `"ladspa:{path}:{label}"`. The
/// path component may itself contain a `:` (e.g. Windows drive letters), so
/// we split on the LAST `:` to separate the label.
pub fn create_effect(type_id: &str, sample_rate: u32) -> Option<Box<dyn AudioEffect>> {
    // LV2 plugins resolve via URI (`"lv2:{plugin_uri}"`). Stable
    // across distro layouts; control values are addressed by
    // `lv2:symbol` so port reorders in plugin updates don't break
    // saved projects.
    if type_id.starts_with("lv2:") {
        return crate::lv2::instantiate_from_type_id(type_id, sample_rate, &[]);
    }
    // CLAP plugins resolve via reverse-DNS plugin ID
    // (`"clap:{plugin_id}"`). Param values are addressed by stable
    // u32 ParamID (hex-encoded in the params Vec) so plugin updates
    // that reorder ports don't corrupt saves.
    if type_id.starts_with("clap:") {
        return crate::clap::instantiate_from_type_id(type_id, sample_rate, &[]);
    }
    if let Some(rest) = type_id.strip_prefix("ladspa:") {
        if let Some(last_colon) = rest.rfind(':') {
            let path_str = &rest[..last_colon];
            let label = &rest[last_colon + 1..];
            return match crate::ladspa::LadspaEffect::new(
                std::path::Path::new(path_str),
                label,
                sample_rate,
            ) {
                Ok(e) => Some(Box::new(e)),
                Err(err) => {
                    tracing::warn!("LADSPA load failed for {type_id}: {err}");
                    None
                }
            };
        }
        tracing::warn!("LADSPA type_id missing label separator: {type_id}");
        return None;
    }

    match type_id {
        "eq" => Some(Box::new(eq::ParametricEq::new(sample_rate))),
        "compressor" => Some(Box::new(compressor::Compressor::new(sample_rate))),
        "limiter" => Some(Box::new(limiter::Limiter::new(sample_rate))),
        "gate" => Some(Box::new(gate::NoiseGate::new(sample_rate))),
        "high_pass" => Some(Box::new(high_pass::HighPass::new(sample_rate))),
        "denoiser" => Some(Box::new(denoiser::Denoiser::new(sample_rate))),
        "voice_isolation" => Some(Box::new(voice_isolation::VoiceIsolation::new(sample_rate))),
        "reverb" => Some(Box::new(reverb::Reverb::new(sample_rate))),
        "delay" => Some(Box::new(delay::Delay::new(sample_rate))),
        "deesser" => Some(Box::new(deesser::DeEsser::new(sample_rate))),
        "hum_removal" => Some(Box::new(hum_removal::HumRemoval::new(sample_rate))),
        "noise_reduction" => Some(Box::new(noise_reduction::NoiseReduction::new(sample_rate))),
        "declick" => Some(Box::new(declick::DeClick::new(sample_rate))),
        "normalize" => Some(Box::new(normalize::Normalize::new(sample_rate))),
        "pitch_shift" => Some(Box::new(pitch_shift::PitchShift::new(sample_rate))),
        "chorus" => Some(Box::new(chorus::Chorus::new(sample_rate))),
        "multiband_compressor" => Some(Box::new(multiband_compressor::MultibandCompressor::new(sample_rate))),
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
        ("high_pass", "High-pass Filter"),
        ("denoiser", "Broadband Denoiser"),
        ("voice_isolation", "Voice Isolation"),
        ("reverb", "Reverb"),
        ("delay", "Delay"),
        ("deesser", "De-Esser"),
        ("hum_removal", "Hum Removal"),
        ("noise_reduction", "Multi-band Gate"),
        ("declick", "De-click"),
        ("normalize", "Normalize"),
        ("pitch_shift", "Pitch Shift"),
        ("chorus", "Chorus"),
        ("multiband_compressor", "Multiband Compressor"),
    ]
}
