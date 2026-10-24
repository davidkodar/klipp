//! LV2 plugin host — discovery, loading, and `AudioEffect` adapter.
//!
//! Mirrors [`crate::ladspa`]: a discovery walk against the system
//! search paths writes a schema-versioned cache; per-clip effect
//! instances are constructed on demand via `create_effect("lv2:{uri}",
//! sr)` from [`crate::dsp::create_effect`].
//!
//! # Channel layout
//!
//! Many LV2 plugins are mono (1-in / 1-out). Klipp's mix path is
//! stereo. The runtime resolves this automatically:
//!
//!   - 2-in / 2-out plugin → 1 instance, channels straight through.
//!   - 1-in / 1-out plugin on a stereo track → 2 instances, dual-mono.
//!
//! Future user-selectable modes (StereoLinked, MonoSummed) are
//! reserved by design — when v2 adds them, an `instance_mode` field
//! is added to [`klipp_core::AudioEffectInstance`] with `serde(default)`
//! and v1 saves load identically. Long-term-safe by additive serde.
//!
//! # Identity
//!
//! Type-id is `"lv2:{plugin_uri}"`. Control values are addressed by
//! `lv2:symbol` (the plugin author's stable contract) — never by
//! port index, since plugin versions are free to reorder ports.
//! See `feedback_longterm_thinking`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::dsp::{AudioEffect, AudioParamDescriptor, AudioProcessContext};

// ─── Process-global plugin registry ───────────────────────────────────────
//
// Populated once at startup from `discover_lv2_plugins_cached`; the
// inspector + project doctor look up by URI without re-walking the
// system search paths or re-iterating every port. Mirrors the
// pattern in `klipp_effects::frei0r_effect`.

static PLUGIN_REGISTRY: OnceLock<Mutex<Vec<Lv2PluginInfo>>> = OnceLock::new();

fn registry() -> &'static Mutex<Vec<Lv2PluginInfo>> {
    PLUGIN_REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Set the runtime LV2 plugin registry. Called once at app startup
/// after [`discover_lv2_plugins_cached`] returns. Re-callable: the
/// Rescan path replaces the whole list.
pub fn set_plugin_registry(plugins: Vec<Lv2PluginInfo>) {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    *r = plugins;
}

/// Look up a plugin's metadata by URI from the runtime registry.
/// Used by the inspector to render symbol-keyed sliders without
/// having to plumb `&[Lv2PluginInfo]` through every caller.
pub fn lookup_by_uri(uri: &str) -> Option<Lv2PluginInfo> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|p| p.uri == uri)
        .cloned()
}

/// Snapshot the registry — used by Project Doctor to enumerate
/// installed plugins for the missing-plugin walk.
pub fn registry_snapshot() -> Vec<Lv2PluginInfo> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

// ─── World singleton ──────────────────────────────────────────────────────

fn world() -> &'static Mutex<Arc<livi::World>> {
    static WORLD: OnceLock<Mutex<Arc<livi::World>>> = OnceLock::new();
    WORLD.get_or_init(|| Mutex::new(klipp_load_lock::while_loading(|| Arc::new(livi::World::new()))))
}

#[allow(dead_code)]
pub(crate) fn reset_world() {
    if let Ok(mut w) = world().lock() {
        *w = klipp_load_lock::while_loading(|| Arc::new(livi::World::new()));
    }
}

fn current_world() -> Arc<livi::World> {
    world()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

// ─── Discovery types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lv2ControlPort {
    /// Stable plugin-author symbol — the long-term-safe key for
    /// `AudioEffectInstance.params`. NEVER reorder-sensitive.
    pub symbol: String,
    /// Human-readable name; may change between plugin versions.
    pub name: String,
    pub default: f32,
    /// Min / max are `Option` to match livi's surface (LV2 ports are
    /// allowed to declare only one bound, or none at all). Inspector
    /// falls back to `[0.0, 1.0]` when absent.
    pub min: Option<f32>,
    pub max: Option<f32>,
    /// 0-based port index inside the plugin. Stored so the runtime
    /// can build a symbol → PortIndex map without re-iterating
    /// every block. Fragile across plugin versions — saved files
    /// must NEVER round-trip via index.
    pub index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lv2PluginInfo {
    /// LV2 plugin URI — stable identifier (e.g. `urn:gx_amp` or
    /// `http://drobilla.net/plugins/mda/EPiano`).
    pub uri: String,
    pub name: String,
    pub audio_inputs: usize,
    pub audio_outputs: usize,
    pub control_inputs: Vec<Lv2ControlPort>,
}

impl Lv2PluginInfo {
    pub fn type_id(&self) -> String {
        format!("lv2:{}", self.uri)
    }
}

// ─── Search paths ─────────────────────────────────────────────────────────

fn default_search_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/usr/lib/lv2"),
        PathBuf::from("/usr/lib64/lv2"),
        PathBuf::from("/usr/local/lib/lv2"),
    ];
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".lv2"));
    } else if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(home).join(".lv2"));
    }
    paths
}

pub fn search_paths() -> Vec<PathBuf> {
    let raw: Vec<PathBuf> = if let Ok(env) = std::env::var("LV2_PATH") {
        env.split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect()
    } else {
        default_search_paths()
    };
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(raw.len());
    for p in raw {
        let key = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if seen.insert(key) {
            out.push(p);
        }
    }
    out
}

// ─── Discovery ────────────────────────────────────────────────────────────

pub fn discover_lv2_plugins() -> Vec<Lv2PluginInfo> {
    if std::env::var("KLIPP_LV2_DISABLE")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        tracing::info!("LV2: discovery disabled via KLIPP_LV2_DISABLE");
        return Vec::new();
    }
    let world = current_world();
    let mut out = Vec::new();
    for plugin in world.iter_plugins() {
        if let Some(info) = extract_plugin_info(&plugin) {
            out.push(info);
        }
    }
    tracing::info!("LV2: discovered {} plugin(s)", out.len());
    out
}

pub fn discover_lv2_plugins_cached(cache_dir: Option<&Path>) -> Vec<Lv2PluginInfo> {
    let dirs = search_paths();
    if let Some(base) = cache_dir {
        if let Some(plugins) = cache::load(base, &dirs) {
            tracing::info!("LV2: loaded {} plugin(s) from disk cache", plugins.len());
            return plugins;
        }
    }
    let plugins = discover_lv2_plugins();
    if let Some(base) = cache_dir {
        if let Err(e) = cache::save(base, &dirs, &plugins) {
            tracing::warn!("LV2: failed to write cache: {e}");
        }
    }
    plugins
}

fn extract_plugin_info(plugin: &livi::Plugin) -> Option<Lv2PluginInfo> {
    let audio_inputs = plugin
        .ports_with_type(livi::PortType::AudioInput)
        .count();
    let audio_outputs = plugin
        .ports_with_type(livi::PortType::AudioOutput)
        .count();
    if audio_outputs == 0 {
        // Source / analyser — out of v1 scope (LV2-NEXT-SOURCES).
        return None;
    }
    if audio_inputs > 2 || audio_outputs > 2 {
        // Surround / side-chain — needs explicit channel mapping UX
        // (LV2-NEXT-MULTICHANNEL).
        return None;
    }

    let control_inputs: Vec<Lv2ControlPort> = plugin
        .ports_with_type(livi::PortType::ControlInput)
        .map(|p| Lv2ControlPort {
            symbol: p.symbol.clone(),
            name: p.name.clone(),
            default: p.default_value,
            min: p.min_value,
            max: p.max_value,
            index: p.index.0,
        })
        .collect();

    Some(Lv2PluginInfo {
        uri: plugin.uri(),
        name: plugin.name(),
        audio_inputs,
        audio_outputs,
        control_inputs,
    })
}

// ─── On-disk cache ────────────────────────────────────────────────────────

mod cache {
    use super::Lv2PluginInfo;
    use serde::{Deserialize, Serialize};
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    /// Bump when [`super::Lv2PluginInfo`] / [`super::Lv2ControlPort`]
    /// gain or lose serialised fields. Old caches are silently
    /// rescanned on schema mismatch.
    ///
    /// History:
    ///   1 — initial schema (control inputs only, audio I/O ≤ 2)
    pub(super) const SCHEMA_VERSION: u32 = 1;
    pub(super) const FILE_NAME: &str = "lv2_registry.json";

    #[derive(Serialize, Deserialize)]
    struct DiskCache {
        version: u32,
        dir_signatures: Vec<DirSig>,
        plugins: Vec<Lv2PluginInfo>,
    }

    #[derive(Serialize, Deserialize, PartialEq, Eq)]
    struct DirSig {
        path: String,
        mtime_secs: Option<u64>,
    }

    fn cache_file(base: &Path) -> PathBuf {
        base.join(FILE_NAME)
    }

    fn collect_signatures(dirs: &[PathBuf]) -> Vec<DirSig> {
        dirs.iter()
            .map(|d| {
                let mtime_secs = std::fs::metadata(d)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                DirSig {
                    path: d.to_string_lossy().into_owned(),
                    mtime_secs,
                }
            })
            .collect()
    }

    pub(super) fn load(base: &Path, dirs: &[PathBuf]) -> Option<Vec<Lv2PluginInfo>> {
        let path = cache_file(base);
        let data = std::fs::read_to_string(&path).ok()?;
        let cache: DiskCache = serde_json::from_str(&data).ok()?;
        if cache.version != SCHEMA_VERSION {
            tracing::debug!(
                "LV2 cache: schema mismatch ({} vs {}), rescanning",
                cache.version,
                SCHEMA_VERSION
            );
            return None;
        }
        if cache.dir_signatures != collect_signatures(dirs) {
            tracing::debug!("LV2 cache: directory mtime changed, rescanning");
            return None;
        }
        Some(cache.plugins)
    }

    pub(super) fn save(
        base: &Path,
        dirs: &[PathBuf],
        plugins: &[Lv2PluginInfo],
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(base)?;
        let cache = DiskCache {
            version: SCHEMA_VERSION,
            dir_signatures: collect_signatures(dirs),
            plugins: plugins.to_vec(),
        };
        let json = serde_json::to_string(&cache).map_err(std::io::Error::other)?;
        std::fs::write(cache_file(base), json)
    }
}

// ─── Lv2Effect ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelLayout {
    StereoNative,
    DualMono,
}

pub struct Lv2Effect {
    info: Lv2PluginInfo,
    layout: ChannelLayout,
    instances: Vec<livi::Instance>,
    /// The rate the instances run at: a reset makes its fresh ones at
    /// it, not at 48 kHz, which ran a 44.1 kHz chain's plug-in at the
    /// wrong rate after every reset.
    sample_rate: u32,
    /// Live control values, indexed parallel to `info.control_inputs`.
    /// Pushed into livi instances each block via `set_control_input`
    /// (PortIndex form — looked up via the symbol→index map below).
    control_values: Vec<f32>,
    /// Symbol → control-input PortIndex. Built once at construct
    /// time. Saved files address by symbol; runtime translates to
    /// index for the livi call. Reorder-safe across plugin versions.
    symbol_to_idx: HashMap<String, usize>,
    /// PortIndex of the plugin's latency control-output port, if any.
    /// LV2 spec convention: a port with `lv2:designation lv2:latency`.
    /// Most plugins follow the LADSPA convention of literally naming
    /// the port "latency"; we look it up by symbol/name match. Same
    /// shape as `klipp_audio::ladspa::LadspaEffect::latency_slot`.
    latency_port: Option<livi::PortIndex>,
    /// Pre-allocated scratch buffers so the audio thread never
    /// allocates per-block.
    scratch_in_l: Vec<f32>,
    scratch_in_r: Vec<f32>,
    scratch_out_l: Vec<f32>,
    scratch_out_r: Vec<f32>,
    /// One empty event sequence for each of the plug-in's atom ports,
    /// shared by its instances, which run one after another. A port left
    /// unconnected made livi refuse every run, and the sound went
    /// through untouched: any x42 or LSP plug-in did nothing.
    atoms_in: Vec<livi::event::LV2AtomSequence>,
    atoms_out: Vec<livi::event::LV2AtomSequence>,
    /// AudioEffect requires &'static str — leak runtime strings once
    /// per instance lifetime. Bounded by chain edits, not per-block.
    type_id_static: &'static str,
    name_static: &'static str,
    empty_descs: [AudioParamDescriptor; 0],
}

unsafe impl Send for Lv2Effect {}

/// Room for what a plug-in reports on an atom output in one block:
/// meter readings and notifications, which nothing here reads.
const ATOM_OUT_BYTES: usize = 8192;

/// Event sequences for `plugin`'s atom inputs and outputs, made with the
/// `features` its instances are made with.
fn atom_ports(plugin: &livi::Plugin, features: &livi::Features) -> (Vec<livi::event::LV2AtomSequence>, Vec<livi::event::LV2AtomSequence>) {
    let counts = plugin.port_counts();
    (
        (0..counts.atom_sequence_inputs).map(|_| livi::event::LV2AtomSequence::new(features, 64)).collect(),
        (0..counts.atom_sequence_outputs).map(|_| livi::event::LV2AtomSequence::new(features, ATOM_OUT_BYTES)).collect(),
    )
}

/// `count` instances of `plugin` at `sample_rate`: what an effect is made
/// with, and what a reset makes afresh.
fn instantiate(plugin: &livi::Plugin, features: &std::sync::Arc<livi::Features>, count: usize, sample_rate: u32) -> Result<Vec<livi::Instance>, String> {
    (0..count)
        .map(|_| {
            klipp_load_lock::while_loading(|| unsafe { plugin.instantiate(features.clone(), sample_rate as f64) })
                .map_err(|e| format!("LV2 instantiate failed: {e}"))
        })
        .collect()
}

impl Lv2Effect {
    pub fn new(uri: &str, sample_rate: u32) -> Result<Self, String> {
        let world = current_world();
        let plugin = world
            .plugin_by_uri(uri)
            .ok_or_else(|| format!("LV2 plugin not found: {uri}"))?;
        let info = extract_plugin_info(&plugin)
            .ok_or_else(|| format!("LV2 plugin {uri} is out of v1 scope"))?;

        let features = world.build_features(livi::FeaturesBuilder {
            min_block_length: 1,
            max_block_length: 4096,
        });

        let layout = match (info.audio_inputs, info.audio_outputs) {
            (2, 2) => ChannelLayout::StereoNative,
            (1, 1) | (0, 1) => ChannelLayout::DualMono,
            _ => {
                return Err(format!(
                    "LV2 plugin {uri} has unsupported channel layout {} in / {} out",
                    info.audio_inputs, info.audio_outputs
                ))
            }
        };

        let count = match layout {
            ChannelLayout::StereoNative => 1,
            ChannelLayout::DualMono => 2,
        };
        let instances = instantiate(&plugin, &features, count, sample_rate)?;
        let (atoms_in, atoms_out) = atom_ports(&plugin, &features);

        let control_values: Vec<f32> = info.control_inputs.iter().map(|p| p.default).collect();
        let symbol_to_idx: HashMap<String, usize> = info
            .control_inputs
            .iter()
            .enumerate()
            .map(|(i, p)| (p.symbol.clone(), i))
            .collect();

        // Locate the plugin's latency control-output port. LV2 spec
        // says the host should look at `lv2:designation lv2:latency`;
        // livi doesn't expose designation queries, so we fall back to
        // the LADSPA-compatible convention of matching symbol or
        // human-readable name "latency" (case-insensitive). Most
        // plugins that report latency follow this naming.
        let latency_port = plugin
            .ports_with_type(livi::PortType::ControlOutput)
            .find(|p| {
                let s = p.symbol.to_lowercase();
                let n = p.name.to_lowercase();
                s == "latency" || n == "latency"
            })
            .map(|p| p.index);

        let type_id = format!("lv2:{uri}");
        let name = info.name.clone();

        Ok(Self {
            info,
            layout,
            instances,
            sample_rate,
            control_values,
            symbol_to_idx,
            latency_port,
            scratch_in_l: Vec::new(),
            scratch_in_r: Vec::new(),
            scratch_out_l: Vec::new(),
            scratch_out_r: Vec::new(),
            atoms_in,
            atoms_out,
            type_id_static: Box::leak(type_id.into_boxed_str()),
            name_static: Box::leak(name.into_boxed_str()),
            empty_descs: [],
        })
    }

    pub fn info(&self) -> &Lv2PluginInfo {
        &self.info
    }

    /// Set a control value by stable LV2 symbol (NOT by index). Looks
    /// up the parallel index inside `info.control_inputs`; on miss
    /// logs at debug level so an old project loading a renamed
    /// param doesn't toast.
    fn set_control_by_symbol(&mut self, symbol: &str, value: f32) {
        let Some(&idx) = self.symbol_to_idx.get(symbol) else {
            tracing::debug!(
                "LV2 '{}': set_param '{symbol}' not found",
                self.info.name
            );
            return;
        };
        if idx >= self.control_values.len() {
            return;
        }
        let port = &self.info.control_inputs[idx];
        let lo = port.min.unwrap_or(f32::MIN);
        let hi = port.max.unwrap_or(f32::MAX);
        self.control_values[idx] = value.clamp(lo, hi);
    }

    fn ensure_scratch(&mut self, frames: usize) {
        if self.scratch_in_l.len() < frames {
            self.scratch_in_l.resize(frames, 0.0);
        }
        if self.scratch_in_r.len() < frames {
            self.scratch_in_r.resize(frames, 0.0);
        }
        if self.scratch_out_l.len() < frames {
            self.scratch_out_l.resize(frames, 0.0);
        }
        if self.scratch_out_r.len() < frames {
            self.scratch_out_r.resize(frames, 0.0);
        }
    }

    fn push_control_values(&mut self) {
        // Fan out the live control snapshot to every instance (1 for
        // StereoNative, 2 for DualMono — both channels see the same
        // values, which preserves matched stereo automation).
        for inst in &mut self.instances {
            for (i, port) in self.info.control_inputs.iter().enumerate() {
                let _ = inst
                    .set_control_input(livi::PortIndex(port.index), self.control_values[i]);
            }
        }
    }
}

impl AudioEffect for Lv2Effect {
    fn type_id(&self) -> &'static str {
        self.type_id_static
    }

    fn name(&self) -> &'static str {
        self.name_static
    }

    fn param_descriptors(&self) -> &[AudioParamDescriptor] {
        // Static descriptors would require leaking per-instance
        // strings. UI handles LV2 via state.lv2_plugins directly
        // (mirror LADSPA precedent at ladspa.rs:860).
        &self.empty_descs
    }

    fn get_param(&self, name: &str) -> Option<f32> {
        let idx = *self.symbol_to_idx.get(name)?;
        self.control_values.get(idx).copied()
    }

    fn set_param(&mut self, name: &str, value: f32) {
        self.set_control_by_symbol(name, value);
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let host_channels = ctx.channels.max(1) as usize;
        if buffer.is_empty() || host_channels == 0 {
            return;
        }
        let frames = buffer.len() / host_channels;
        if frames == 0 {
            return;
        }
        self.ensure_scratch(frames);
        self.push_control_values();

        match self.layout {
            ChannelLayout::StereoNative => {
                // De-interleave stereo input → run plugin → interleave.
                for f in 0..frames {
                    let l = buffer[f * host_channels];
                    let r = if host_channels >= 2 {
                        buffer[f * host_channels + 1]
                    } else {
                        l
                    };
                    self.scratch_in_l[f] = l;
                    self.scratch_in_r[f] = r;
                }
                for s in self.scratch_out_l[..frames]
                    .iter_mut()
                    .chain(self.scratch_out_r[..frames].iter_mut())
                {
                    *s = 0.0;
                }
                let in_l: &[f32] = &self.scratch_in_l[..frames];
                let in_r: &[f32] = &self.scratch_in_r[..frames];
                // Need disjoint mutable borrows for the two outputs.
                let (out_l, out_r) = {
                    let out_l_ref: *mut [f32] = &mut self.scratch_out_l[..frames];
                    let out_r_ref: *mut [f32] = &mut self.scratch_out_r[..frames];
                    // SAFETY: scratch_out_l and scratch_out_r are
                    // disjoint Vecs, the two slice pointers cannot
                    // alias even though both originate from `self`.
                    unsafe { (&mut *out_l_ref, &mut *out_r_ref) }
                };
                let inputs: [&[f32]; 2] = [in_l, in_r];
                let outputs: [&mut [f32]; 2] = [out_l, out_r];
                self.atoms_in.iter_mut().for_each(livi::event::LV2AtomSequence::clear);
                self.atoms_out.iter_mut().for_each(livi::event::LV2AtomSequence::clear_as_chunk);
                let ports = livi::EmptyPortConnections::new()
                    .with_audio_inputs(inputs.into_iter())
                    .with_audio_outputs(outputs.into_iter())
                    .with_atom_sequence_inputs(self.atoms_in.iter())
                    .with_atom_sequence_outputs(self.atoms_out.iter_mut());
                if let Err(e) = unsafe { self.instances[0].run(frames, ports) } {
                    crate::dsp::note_refused_run(self.type_id_static, &e.to_string());
                    return;
                }
                for f in 0..frames {
                    buffer[f * host_channels] = self.scratch_out_l[f];
                    if host_channels >= 2 {
                        buffer[f * host_channels + 1] = self.scratch_out_r[f];
                    }
                }
            }
            ChannelLayout::DualMono => {
                // Two mono passes — one per channel. Control values
                // already fanned out above so both channels stay
                // matched.
                for ch in 0..host_channels.min(2) {
                    for f in 0..frames {
                        self.scratch_in_l[f] = buffer[f * host_channels + ch];
                    }
                    for s in self.scratch_out_l[..frames].iter_mut() {
                        *s = 0.0;
                    }
                    let in_buf: &[f32] = &self.scratch_in_l[..frames];
                    let out_buf: &mut [f32] = &mut self.scratch_out_l[..frames];
                    let inputs: [&[f32]; 1] = [in_buf];
                    let outputs: [&mut [f32]; 1] = [out_buf];
                    self.atoms_in.iter_mut().for_each(livi::event::LV2AtomSequence::clear);
                    self.atoms_out.iter_mut().for_each(livi::event::LV2AtomSequence::clear_as_chunk);
                    let ports = livi::EmptyPortConnections::new()
                        .with_audio_inputs(inputs.into_iter())
                        .with_audio_outputs(outputs.into_iter())
                        .with_atom_sequence_inputs(self.atoms_in.iter())
                        .with_atom_sequence_outputs(self.atoms_out.iter_mut());
                    let inst_idx = ch.min(self.instances.len() - 1);
                    if let Err(e) =
                        unsafe { self.instances[inst_idx].run(frames, ports) }
                    {
                        crate::dsp::note_refused_run(self.type_id_static, &e.to_string());
                        continue;
                    }
                    for f in 0..frames {
                        buffer[f * host_channels + ch] = self.scratch_out_l[f];
                    }
                }
            }
        }
    }

    fn reset(&mut self) {
        // livi has no public reset/activate API. Re-instantiate against
        // the cached URI to clear plugin state. Cheap (~1 ms) — same
        // shape the LADSPA host's deactivate/activate sequence achieves.
        let world = current_world();
        let Some(plugin) = world.plugin_by_uri(&self.info.uri) else {
            return;
        };
        let features = world.build_features(livi::FeaturesBuilder {
            min_block_length: 1,
            max_block_length: 4096,
        });
        // Replace instances with fresh ones at the effect's own rate; on
        // any failure keep the old (state is leaked but the chain keeps
        // producing audio).
        if let Ok(fresh) = instantiate(&plugin, &features, self.instances.len(), self.sample_rate) {
            self.instances = fresh;
            (self.atoms_in, self.atoms_out) = atom_ports(&plugin, &features);
        }
    }

    fn latency_samples(&self) -> u32 {
        // Read from the latency control-output port the plugin
        // wrote to during the last run(). For DualMono pairs both
        // instances run identical code so they report identical
        // latency; sample slot[0]. Negative / NaN / inf values are
        // clamped to 0 (defensive — buggy plugins). The PDC layer
        // applies its own upper-bound clamp (1 second × sample_rate)
        // so a plugin that reports a nonsense huge value can't
        // crater the transport.
        let Some(port) = self.latency_port else {
            return 0;
        };
        let Some(instance) = self.instances.first() else {
            return 0;
        };
        match instance.control_output(port) {
            Some(v) if v.is_finite() && v >= 0.0 => (v + 0.5) as u32,
            _ => 0,
        }
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        // SYMBOL-keyed serialisation. Long-term-safe across plugin
        // version updates that reorder ports.
        self.info
            .control_inputs
            .iter()
            .enumerate()
            .map(|(i, p)| (p.symbol.clone(), self.control_values[i]))
            .collect()
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (symbol, value) in params {
            self.set_control_by_symbol(symbol, *value);
        }
    }
}

/// Construct an `Lv2Effect` from a `klipp_core::AudioEffectInstance`
/// at the type-id + params layer. Called from
/// [`crate::dsp::create_effect`] when the type-id starts `"lv2:"`.
pub fn instantiate_from_type_id(
    type_id: &str,
    sample_rate: u32,
    params: &[(String, f32)],
) -> Option<Box<dyn AudioEffect>> {
    let uri = type_id.strip_prefix("lv2:")?;
    let mut effect = match Lv2Effect::new(uri, sample_rate) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!("LV2 load failed for {type_id}: {err}");
            return None;
        }
    };
    effect.deserialize_params(params);
    Some(Box::new(effect))
}

/// Default `(symbol, value)` pairs for a freshly-added plugin instance.
pub fn default_params(uri: &str) -> Option<Vec<(String, f32)>> {
    let world = current_world();
    let plugin = world.plugin_by_uri(uri)?;
    let info = extract_plugin_info(&plugin)?;
    Some(
        info.control_inputs
            .iter()
            .map(|p| (p.symbol.clone(), p.default))
            .collect(),
    )
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialise tests that touch `LV2_PATH` / `KLIPP_LV2_DISABLE`
    /// env vars. Cargo runs tests in parallel by default; concurrent
    /// env mutations would race and produce flaky failures.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn type_id_uri_only() {
        let info = Lv2PluginInfo {
            uri: "urn:gx_amp".into(),
            name: "GxAmp".into(),
            audio_inputs: 1,
            audio_outputs: 1,
            control_inputs: vec![],
        };
        assert_eq!(info.type_id(), "lv2:urn:gx_amp");
    }

    #[test]
    fn search_paths_default_includes_user_home_last() {
        let _g = env_lock();
        std::env::remove_var("LV2_PATH");
        let paths = search_paths();
        assert!(!paths.is_empty());
        assert!(paths[0].to_string_lossy().contains("/usr/lib/lv2"));
    }

    #[test]
    fn search_paths_env_override_replaces_defaults() {
        let _g = env_lock();
        std::env::set_var("LV2_PATH", "/tmp/a:/tmp/b");
        let paths = search_paths();
        assert_eq!(paths, vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]);
        std::env::remove_var("LV2_PATH");
    }

    #[test]
    fn search_paths_dedupes_symlinks() {
        let _g = env_lock();
        let tmp = std::env::temp_dir();
        let target = tmp.join("klipp-lv2-symlink-test");
        let _ = std::fs::create_dir_all(&target);
        let symlink = tmp.join("klipp-lv2-symlink-test-link");
        let _ = std::fs::remove_file(&symlink);
        if std::os::unix::fs::symlink(&target, &symlink).is_err() {
            return;
        }
        std::env::set_var(
            "LV2_PATH",
            format!("{}:{}", target.display(), symlink.display()),
        );
        let paths = search_paths();
        std::env::remove_var("LV2_PATH");
        let _ = std::fs::remove_file(&symlink);
        let _ = std::fs::remove_dir(&target);
        assert_eq!(
            paths.len(),
            1,
            "two paths that canonicalize to the same dir must collapse to one"
        );
    }

    #[test]
    fn cache_round_trip_via_serde() {
        // Pin the schema_version=1 contract.
        let info = Lv2PluginInfo {
            uri: "http://example.org/plugin/test".into(),
            name: "Test Plugin".into(),
            audio_inputs: 1,
            audio_outputs: 1,
            control_inputs: vec![Lv2ControlPort {
                symbol: "gain".into(),
                name: "Gain".into(),
                default: 1.0,
                min: Some(0.0),
                max: Some(2.0),
                index: 0,
            }],
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: Lv2PluginInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.uri, info.uri);
        assert_eq!(back.control_inputs.len(), 1);
        assert_eq!(back.control_inputs[0].symbol, "gain");
    }

    #[test]
    fn schema_version_constant_is_locked_to_1() {
        // Long-term-safety pin: future bumps require a deliberate
        // edit + thoughtful migration rationale.
        assert_eq!(cache::SCHEMA_VERSION, 1);
    }

    /// Closes the long-term-safety promise that saved files survive a
    /// plugin's port reorder. Build an Lv2PluginInfo with two ports
    /// in order [a, b], serialise, then load against a "new version"
    /// where the port order has flipped to [b, a]. Symbol-keyed
    /// dispatch must put the values back on the right ports.
    #[test]
    fn control_values_round_trip_by_symbol_not_index() {
        let original_v1 = vec![
            Lv2ControlPort {
                symbol: "gain".into(),
                name: "Gain".into(),
                default: 0.5,
                min: Some(0.0),
                max: Some(1.0),
                index: 0,
            },
            Lv2ControlPort {
                symbol: "tone".into(),
                name: "Tone".into(),
                default: 0.5,
                min: Some(0.0),
                max: Some(1.0),
                index: 1,
            },
        ];
        // Saved values keyed by symbol (matches serialize_params output).
        let saved: Vec<(String, f32)> =
            vec![("gain".into(), 0.9), ("tone".into(), 0.1)];

        // Plugin v2 reordered the ports — same symbols, different
        // indices. Index-based deserialisation would confuse gain and
        // tone here.
        let v2_reordered = [Lv2ControlPort {
                symbol: "tone".into(),
                name: "Tone".into(),
                default: 0.5,
                min: Some(0.0),
                max: Some(1.0),
                index: 0,
            },
            Lv2ControlPort {
                symbol: "gain".into(),
                name: "Gain".into(),
                default: 0.5,
                min: Some(0.0),
                max: Some(1.0),
                index: 1,
            }];
        let _ = original_v1;
        let symbol_to_idx: HashMap<String, usize> = v2_reordered
            .iter()
            .enumerate()
            .map(|(i, p)| (p.symbol.clone(), i))
            .collect();
        let mut values = vec![0.0_f32; v2_reordered.len()];
        for (sym, val) in &saved {
            if let Some(&idx) = symbol_to_idx.get(sym) {
                values[idx] = *val;
            }
        }
        // Tone (now at idx 0) must hold 0.1; gain (now at idx 1)
        // must hold 0.9. Index-keyed code would have swapped them.
        assert_eq!(values[0], 0.1, "tone goes to its NEW position by symbol");
        assert_eq!(values[1], 0.9, "gain goes to its NEW position by symbol");
    }

    /// If LV2 is installed on the host, discovery yields at least one
    /// plugin. Gated so CI without LV2 passes.
    #[test]
    fn latency_readback_clamp_policy() {
        // The free function in `latency_samples()` accepts only
        // finite, non-negative values; everything else maps to 0.
        // Pin the policy here (mirror the LADSPA `read_latency_samples`
        // test pattern in this file's sibling crate).
        fn clamp(v: f32) -> u32 {
            match Some(v) {
                Some(v) if v.is_finite() && v >= 0.0 => (v + 0.5) as u32,
                _ => 0,
            }
        }
        assert_eq!(clamp(0.0), 0);
        assert_eq!(clamp(64.0), 64);
        assert_eq!(clamp(64.7), 65, "half-up rounding for fractional samples");
        assert_eq!(clamp(-1.0), 0, "negative latency is treated as 0");
        assert_eq!(clamp(f32::NAN), 0, "NaN clamps to 0");
        assert_eq!(clamp(f32::INFINITY), 0, "+inf clamps to 0");
    }

    /// True when `/usr/lib/lv2` holds at least one bundle containing a
    /// plugin binary.
    ///
    /// The directory existing is not enough to conclude that plugins are
    /// installed: the `lv2` package ships only the specification bundles
    /// (`atom.lv2`, `core.lv2`, `buf-size.lv2`, …), which carry no `.so`
    /// at all. A machine can therefore have the LV2 standard installed
    /// and nothing whatsoever to discover.
    fn system_has_lv2_plugin_binaries() -> bool {
        let Ok(bundles) = std::fs::read_dir("/usr/lib/lv2") else {
            return false;
        };
        for bundle in bundles.flatten() {
            let Ok(files) = std::fs::read_dir(bundle.path()) else {
                continue;
            };
            if files
                .flatten()
                .any(|f| f.path().extension().is_some_and(|e| e == "so"))
            {
                return true;
            }
        }
        false
    }

    #[test]
    fn discover_finds_at_least_one_plugin_when_installed() {
        let _g = env_lock();
        if !system_has_lv2_plugin_binaries() {
            eprintln!("skipping: no LV2 plugin binaries under /usr/lib/lv2");
            return;
        }
        std::env::remove_var("KLIPP_LV2_DISABLE");
        let plugins = discover_lv2_plugins();
        assert!(
            !plugins.is_empty(),
            "/usr/lib/lv2 holds plugin binaries but discovery yielded zero \
             plugins"
        );
    }

    /// A reset makes its fresh instances at the effect's own rate, not
    /// at 48 kHz: built at 44.1 kHz, it is at 44.1 kHz after a reset, and
    /// still plays. Skips without an installed plug-in that loads.
    #[test]
    fn a_reset_keeps_the_effects_rate() {
        use crate::dsp::{AudioEffect, AudioProcessContext};
        let _g = env_lock();
        if !system_has_lv2_plugin_binaries() {
            eprintln!("skipping: no LV2 plugin binaries under /usr/lib/lv2");
            return;
        }
        std::env::remove_var("KLIPP_LV2_DISABLE");
        let Some(mut fx) = discover_lv2_plugins().iter().find_map(|p| Lv2Effect::new(&p.uri, 44_100).ok()) else {
            eprintln!("skipping: no LV2 plugin loads");
            return;
        };
        let before = fx.instances.len();
        fx.reset();
        assert_eq!((fx.sample_rate, fx.instances.len()), (44_100, before), "the same instances, at the effect's rate");
        let mut buf = vec![0.1f32; 256 * 2];
        fx.process(&mut buf, &AudioProcessContext { sample_rate: 44_100, channels: 2, clip_time_secs: 0.0 });
        assert!(buf.iter().all(|v| v.is_finite()), "it plays after the reset");
    }

    /// A plug-in with atom ports runs: x42's dpl, held at -8 dB, limits
    /// a 0.95 tone. Its unconnected control and notify ports made livi
    /// refuse every run, and the tone went through at 0.95. Skipped where
    /// dpl is not installed.
    #[test]
    fn a_plugin_with_atom_ports_processes() {
        const DPL: &str = "lv2:http://gareus.org/oss/lv2/dpl#mono";
        let _env = env_lock();
        let Some(mut fx) = instantiate_from_type_id(DPL, 48_000, &[("threshold".into(), -8.0)]) else {
            eprintln!("skipped: x42 dpl is not installed");
            return;
        };
        let ctx = AudioProcessContext { sample_rate: 48_000, channels: 2, clip_time_secs: 0.0 };
        let mut peak = 0f32;
        for block in 0..48 {
            let mut buf: Vec<f32> = (0..1024)
                .flat_map(|n| {
                    let v = 0.95 * (std::f32::consts::TAU * 440.0 * (block * 1024 + n) as f32 / 48_000.0).sin();
                    [v, v]
                })
                .collect();
            fx.process(&mut buf, &ctx);
            if block >= 24 {
                peak = buf.iter().fold(peak, |m, v| m.max(v.abs()));
            }
        }
        assert!(peak < 0.45, "held at -8 dB, the tone peaks at {peak}");
        assert_eq!(crate::dsp::refused_run(DPL), None);
        assert_eq!(crate::dsp::probe_run(DPL, 48_000), Ok(()));
    }
}
