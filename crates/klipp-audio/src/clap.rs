//! CLAP (CLever Audio Plugin) plugin host.
//!
//! Mirrors the LV2 host shape ([`crate::lv2`]): discovery walk,
//! schema-versioned on-disk cache, process-global plugin registry
//! for inspector lookup-by-id, [`ClapEffect`] adapter implementing
//! [`crate::dsp::AudioEffect`] for per-clip chains.
//!
//! # Identity
//!
//! Type-id is `"clap:{plugin_id}"`. CLAP plugin IDs are reverse-DNS
//! strings (e.g. `"org.surge-synth-team.surge-xt-fx"`) defined by
//! the plugin author and stable across versions per spec.
//! Parameter values are addressed by stable u32 ParamID,
//! hex-formatted in [`klipp_core::AudioEffectInstance::params`] so
//! plugin updates that reorder the param list don't corrupt saves.
//!
//! # Channel layout
//!
//! Same dual-mono auto-pair as LV2:
//!
//! - 2-in / 2-out plugin → 1 stereo-native instance, channels through.
//! - 1-in / 1-out plugin on a stereo track → 2-instance dual-mono pair,
//!   parameter changes fan out to both for matched-stereo behaviour.
//!
//! # Threading
//!
//! CLAP enforces strict thread-class separation; clack-host
//! enforces it through the type system (only the
//! `StartedPluginAudioProcessor` exposes `process()`, only the main
//! thread sees the controller). Klipp's mix thread owns the
//! [`ClapEffect`] exclusively after construction. `Send` is safe;
//! `Sync` is not.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::latency::PluginLatency;
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::params::{ParamInfoBuffer, PluginParams};
use clack_host::events::event_types::ParamValueEvent;
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::events::{Match, Pckn};
use clack_host::host::{HostExtensions, HostInfo};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_host::utils::{ClapId, Cookie};

use crate::dsp::{AudioEffect, AudioParamDescriptor, AudioProcessContext};

// ─── Public types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClapParamInfo {
    pub id: u32,
    pub name: String,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    #[serde(default)]
    pub flags: u32,
}

impl ClapParamInfo {
    /// Hex-formatted ParamID — the long-term-safe key in
    /// [`klipp_core::AudioEffectInstance::params`].
    pub fn key(&self) -> String {
        format!("{:#x}", self.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClapPluginInfo {
    pub bundle_path: PathBuf,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub version: String,
    pub audio_inputs: usize,
    pub audio_outputs: usize,
    pub params: Vec<ClapParamInfo>,
}

impl ClapPluginInfo {
    pub fn type_id(&self) -> String {
        format!("clap:{}", self.id)
    }
}

// ─── Process-global registry ──────────────────────────────────────────────

static REGISTRY: OnceLock<Mutex<Vec<ClapPluginInfo>>> = OnceLock::new();

fn registry() -> &'static Mutex<Vec<ClapPluginInfo>> {
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn set_plugin_registry(plugins: Vec<ClapPluginInfo>) {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    *r = plugins;
}

pub fn lookup_by_id(plugin_id: &str) -> Option<ClapPluginInfo> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|p| p.id == plugin_id)
        .cloned()
}

pub fn registry_snapshot() -> Vec<ClapPluginInfo> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

// ─── Search paths ─────────────────────────────────────────────────────────

fn default_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".clap"));
    } else if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(home).join(".clap"));
    }
    paths.extend([
        PathBuf::from("/usr/local/lib/clap"),
        PathBuf::from("/usr/lib/clap"),
        PathBuf::from("/usr/lib64/clap"),
    ]);
    paths
}

pub fn search_paths() -> Vec<PathBuf> {
    let raw: Vec<PathBuf> = if let Ok(env) = std::env::var("CLAP_PATH") {
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

fn collect_clap_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("clap") {
            out.push(path);
        } else if path.is_dir() {
            if let Ok(inner) = std::fs::read_dir(&path) {
                for inner_entry in inner.flatten() {
                    let inner_path = inner_entry.path();
                    if inner_path.extension().and_then(|s| s.to_str()) == Some("clap") {
                        out.push(inner_path);
                    }
                }
            }
        }
    }
    out
}

// ─── Discovery ────────────────────────────────────────────────────────────

pub fn discover_clap_plugins() -> Vec<ClapPluginInfo> {
    if std::env::var("KLIPP_CLAP_DISABLE")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        tracing::info!("CLAP: discovery disabled via KLIPP_CLAP_DISABLE");
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    for dir in search_paths() {
        for path in collect_clap_files(&dir) {
            for info in scan_bundle(&path) {
                if seen_ids.insert(info.id.clone()) {
                    out.push(info);
                }
            }
        }
    }
    tracing::info!("CLAP: discovered {} plugin(s)", out.len());
    out
}

pub fn discover_clap_plugins_cached(cache_dir: Option<&Path>) -> Vec<ClapPluginInfo> {
    let dirs = search_paths();
    if let Some(base) = cache_dir {
        if let Some(plugins) = cache::load(base, &dirs) {
            tracing::info!("CLAP: loaded {} plugin(s) from disk cache", plugins.len());
            return plugins;
        }
    }
    let plugins = discover_clap_plugins();
    if let Some(base) = cache_dir {
        if let Err(e) = cache::save(base, &dirs, &plugins) {
            tracing::warn!("CLAP: failed to write cache: {e}");
        }
    }
    plugins
}

/// Scan a `.clap` bundle and return every plugin descriptor it declares.
pub fn scan_bundle(path: &Path) -> Vec<ClapPluginInfo> {
    let entry = match klipp_load_lock::while_loading(|| unsafe { PluginEntry::load(path) }) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!("CLAP: load failed for {}: {e}", path.display());
            return Vec::new();
        }
    };
    let Some(factory) = entry.get_plugin_factory() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for desc in factory.plugin_descriptors() {
        let Some(id_cstr) = desc.id() else { continue };
        let Ok(id) = id_cstr.to_str() else { continue };
        let name = desc
            .name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| id.to_string());
        let vendor = desc
            .vendor()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let version = desc
            .version()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        // Probe the plugin once at scan to fill audio port + param
        // metadata. This is expensive (full instantiate + activate
        // would be heavier still — we only need the main-thread
        // handle long enough to read extension data, then drop the
        // instance). Probing failures fall back to (2, 2) stereo +
        // empty params so the plugin still appears in the browser
        // even if introspection fails.
        let id_owned = id.to_string();
        let id_cstr = match CString::new(id_owned.as_str()) {
            Ok(c) => c,
            Err(_) => {
                tracing::debug!("CLAP: skipping plugin with NUL in id at {}", path.display());
                continue;
            }
        };
        let (audio_inputs, audio_outputs, params) =
            probe_plugin_metadata(path, &id_cstr).unwrap_or_else(|e| {
                tracing::debug!(
                    "CLAP: probe failed for {} ({id_owned}): {e} — defaulting to stereo, no params",
                    path.display()
                );
                (2, 2, Vec::new())
            });

        out.push(ClapPluginInfo {
            bundle_path: path.to_path_buf(),
            id: id_owned,
            name,
            vendor,
            version,
            audio_inputs,
            audio_outputs,
            params,
        });
    }
    out
}

/// Probe a single plugin once at scan to read its audio-port channel
/// counts and parameter descriptors. Instantiates a temporary
/// `PluginInstance` (no activate / start_processing — we only need
/// the main-thread handle), reads the audio_ports + params
/// extensions through `instance.plugin_handle()`, then drops the
/// instance. Cost is roughly the same as a single CLAP plugin's
/// `clap_plugin_factory.create_plugin()` call.
fn probe_plugin_metadata(
    bundle_path: &Path,
    plugin_id: &std::ffi::CStr,
) -> Result<(usize, usize, Vec<ClapParamInfo>), String> {
    let entry = klipp_load_lock::while_loading(|| unsafe { PluginEntry::load(bundle_path) })
        .map_err(|e| format!("PluginEntry::load: {e}"))?;
    let host_info = HostInfo::new(
        "Klipp",
        "Klipp",
        "https://github.com/davidkodar/klipp",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|e| format!("HostInfo: {e}"))?;
    let mut instance = PluginInstance::<KlippClapHost>::new(
        |_| KlippShared,
        |_| KlippMainThread,
        &entry,
        plugin_id,
        &host_info,
    )
    .map_err(|e| format!("PluginInstance::new: {e}"))?;

    let mut handle = instance.plugin_handle();

    let mut audio_inputs = 0usize;
    let mut audio_outputs = 0usize;
    if let Some(ports_ext) = handle.get_extension::<PluginAudioPorts>() {
        let mut buf = AudioPortInfoBuffer::new();
        let in_count = ports_ext.count(&mut handle, true);
        for i in 0..in_count {
            if let Some(info) = ports_ext.get(&mut handle, i, true, &mut buf) {
                audio_inputs += info.channel_count as usize;
            }
        }
        let out_count = ports_ext.count(&mut handle, false);
        for i in 0..out_count {
            if let Some(info) = ports_ext.get(&mut handle, i, false, &mut buf) {
                audio_outputs += info.channel_count as usize;
            }
        }
    } else {
        // Plugin doesn't declare audio ports — assume stereo for
        // effect plugins. Pure synths (which have 0 audio inputs)
        // will be filtered out at instantiation time anyway.
        audio_inputs = 2;
        audio_outputs = 2;
    }

    let mut params: Vec<ClapParamInfo> = Vec::new();
    if let Some(params_ext) = handle.get_extension::<PluginParams>() {
        let mut buf = ParamInfoBuffer::new();
        let count = params_ext.count(&mut handle);
        for i in 0..count {
            if let Some(info) = params_ext.get_info(&mut handle, i, &mut buf) {
                let name = String::from_utf8_lossy(info.name).into_owned();
                params.push(ClapParamInfo {
                    id: info.id.get(),
                    name,
                    default: info.default_value as f32,
                    min: info.min_value as f32,
                    max: info.max_value as f32,
                    flags: info.flags.bits(),
                });
            }
        }
    }

    Ok((audio_inputs, audio_outputs, params))
}

// ─── On-disk cache ────────────────────────────────────────────────────────

mod cache {
    use super::ClapPluginInfo;
    use serde::{Deserialize, Serialize};
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    /// Bump when ClapPluginInfo / ClapParamInfo gain or lose
    /// serialised fields, OR when the discovery walk's content
    /// shape changes (e.g. v1 used sentinel audio-port counts +
    /// empty params; v2 introspects ports + params at scan time
    /// so the same fields now carry real values).
    ///
    /// History:
    ///   1 — initial scaffold; audio_inputs/outputs were sentinel
    ///       (2, 2), params Vec was empty.
    ///   2 — discovery probes each plugin via PluginAudioPorts +
    ///       PluginParams extensions; audio_inputs/outputs reflect
    ///       declared channel counts; params Vec has full info.
    pub(super) const SCHEMA_VERSION: u32 = 2;
    pub(super) const FILE_NAME: &str = "clap_registry.json";

    #[derive(Serialize, Deserialize)]
    struct DiskCache {
        version: u32,
        dir_signatures: Vec<DirSig>,
        plugins: Vec<ClapPluginInfo>,
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

    pub(super) fn load(base: &Path, dirs: &[PathBuf]) -> Option<Vec<ClapPluginInfo>> {
        let path = cache_file(base);
        let data = std::fs::read_to_string(&path).ok()?;
        let cache: DiskCache = serde_json::from_str(&data).ok()?;
        if cache.version != SCHEMA_VERSION {
            tracing::debug!(
                "CLAP cache: schema mismatch ({} vs {}), rescanning",
                cache.version,
                SCHEMA_VERSION
            );
            return None;
        }
        if cache.dir_signatures != collect_signatures(dirs) {
            tracing::debug!("CLAP cache: directory mtime changed, rescanning");
            return None;
        }
        Some(cache.plugins)
    }

    pub(super) fn save(
        base: &Path,
        dirs: &[PathBuf],
        plugins: &[ClapPluginInfo],
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

// ─── HostHandlers — minimal, audio-thread friendly ────────────────────────

pub struct KlippClapHost;

pub struct KlippShared;

impl<'a> SharedHandler<'a> for KlippShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
    fn initializing(&self, _instance: InitializingPluginHandle<'a>) {}
}

impl HostLogImpl for KlippShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        match severity {
            LogSeverity::Fatal | LogSeverity::Error => {
                tracing::error!("CLAP plugin: {message}");
            }
            LogSeverity::Warning => tracing::warn!("CLAP plugin: {message}"),
            LogSeverity::Info => tracing::info!("CLAP plugin: {message}"),
            _ => tracing::debug!("CLAP plugin ({severity:?}): {message}"),
        }
    }
}

pub struct KlippMainThread;

impl<'a> MainThreadHandler<'a> for KlippMainThread {}

impl HostHandlers for KlippClapHost {
    type Shared<'a> = KlippShared;
    type MainThread<'a> = KlippMainThread;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder.register::<HostLog>();
    }
}

// ─── ClapEffect ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelLayout {
    StereoNative,
    DualMono,
}

/// One activated CLAP plugin instance plus the per-block scratch
/// space it needs. For [`ChannelLayout::DualMono`] we hold two of
/// these (one per channel); for [`ChannelLayout::StereoNative`] we
/// hold one.
struct InstanceSlot {
    /// The activated processor — owns the started state and the
    /// PluginInstance behind it. Kept as Option so we can drop it
    /// during reset() before constructing the replacement.
    processor: Option<StartedPluginAudioProcessor<KlippClapHost>>,
    /// AudioPorts builder for the input side. CLAP plugins typically
    /// declare a single "main" audio port containing 1 (mono) or 2
    /// (stereo) channels; we model exactly one port.
    in_ports: AudioPorts,
    /// AudioPorts builder for the output side.
    out_ports: AudioPorts,
    /// Per-channel scratch buffers (interleaved as a single Vec for
    /// stereo, sized to `frames * channels`). Reused across blocks.
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
    /// Block-level event buffer — refilled at the start of each
    /// process() call with current parameter values. v1 sends every
    /// non-default param at offset 0 (block-level automation).
    /// CLAP-NEXT-SAMPLE-ACCURATE-AUTOMATION will add per-sample
    /// offsets to the same buffer additively.
    event_in: EventBuffer,
    /// Steady frame counter for transport-aware plugins.
    steady_counter: u64,
}

pub struct ClapEffect {
    info: ClapPluginInfo,
    layout: ChannelLayout,
    slots: Vec<InstanceSlot>,
    param_values: std::collections::HashMap<u32, f32>,
    sample_rate: u32,
    /// Latency in samples reported by `clap.latency` at instance
    /// build time. CLAP `PluginLatency::get` is a main-thread call;
    /// the AudioEffect trait's `latency_samples()` is `&self`, so we
    /// snapshot once during build_slot when we hold the
    /// PluginMainThreadHandle. v1 = static latency; mid-session
    /// changes via `clap_host_latency.changed` callback land with
    /// `PDC-NEXT-VARIABLE-LATENCY`.
    latency_samples_cached: u32,
    type_id_static: &'static str,
    name_static: &'static str,
    empty_descs: [AudioParamDescriptor; 0],
}

unsafe impl Send for ClapEffect {}

impl ClapEffect {
    pub fn new(plugin_id: &str, sample_rate: u32) -> Result<Self, String> {
        let info = lookup_by_id(plugin_id)
            .ok_or_else(|| format!("CLAP plugin not found in registry: {plugin_id}"))?;

        let layout = match (info.audio_inputs, info.audio_outputs) {
            (2, 2) => ChannelLayout::StereoNative,
            (1, 1) | (0, 1) => ChannelLayout::DualMono,
            // Discovery currently records the v1 default of (2, 2) for
            // every plugin until per-port introspection lands; mismatched
            // shapes show up at instantiation time when activate() reports
            // an audio-config error. For v1 we accept the (2, 2) default
            // and let the plugin reject it if it's wrong.
            _ => ChannelLayout::StereoNative,
        };

        let slot_count = match layout {
            ChannelLayout::StereoNative => 1,
            ChannelLayout::DualMono => 2,
        };
        let mut slots = Vec::with_capacity(slot_count);
        let mut latency_samples_cached = 0u32;
        for _ in 0..slot_count {
            let (slot, latency) = build_slot(&info, layout, sample_rate)?;
            slots.push(slot);
            // Matched slots run identical plugin code so they should
            // report identical latency; take the max defensively in
            // case a plugin ever returns inconsistent values.
            latency_samples_cached = latency_samples_cached.max(latency);
        }

        let param_values: std::collections::HashMap<u32, f32> =
            info.params.iter().map(|p| (p.id, p.default)).collect();

        let type_id = format!("clap:{plugin_id}");
        let name = info.name.clone();

        Ok(Self {
            info,
            layout,
            slots,
            param_values,
            sample_rate,
            latency_samples_cached,
            type_id_static: Box::leak(type_id.into_boxed_str()),
            name_static: Box::leak(name.into_boxed_str()),
            empty_descs: [],
        })
    }

    pub fn info(&self) -> &ClapPluginInfo {
        &self.info
    }

    fn set_control_by_key(&mut self, key: &str, value: f32) {
        let Some(id) = parse_param_id(key) else {
            tracing::debug!(
                "CLAP '{}': set_param key '{key}' not a ParamID",
                self.info.name
            );
            return;
        };
        let clamped = self
            .info
            .params
            .iter()
            .find(|p| p.id == id)
            .map(|p| value.clamp(p.min, p.max))
            .unwrap_or(value);
        self.param_values.insert(id, clamped);
    }
}

fn parse_param_id(key: &str) -> Option<u32> {
    if let Some(hex) = key.strip_prefix("0x").or_else(|| key.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        key.parse::<u32>().ok()
    }
}

/// Build a single InstanceSlot — dlopen the bundle, instantiate the
/// plugin, activate it at the given sample rate, and start
/// processing. Audio port channel count is derived from the layout
/// argument (StereoNative = 2, DualMono = 1).
fn build_slot(
    info: &ClapPluginInfo,
    layout: ChannelLayout,
    sample_rate: u32,
) -> Result<(InstanceSlot, u32), String> {
    let plugin_id_cstr =
        CString::new(info.id.as_str()).map_err(|_| "CLAP plugin id contains NUL".to_string())?;
    let entry = klipp_load_lock::while_loading(|| unsafe { PluginEntry::load(&info.bundle_path) })
        .map_err(|e| format!("CLAP load failed: {e}"))?;
    let host_info = HostInfo::new(
        "Klipp",
        "Klipp",
        "https://github.com/davidkodar/klipp",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|e| format!("HostInfo: {e}"))?;
    let mut instance = PluginInstance::<KlippClapHost>::new(
        |_| KlippShared,
        |_| KlippMainThread,
        &entry,
        &plugin_id_cstr,
        &host_info,
    )
    .map_err(|e| format!("PluginInstance::new: {e}"))?;

    let configuration = PluginAudioConfiguration {
        sample_rate: sample_rate as f64,
        min_frames_count: 1,
        max_frames_count: crate::dsp::PLUGIN_MAX_BLOCK as u32,
    };
    let stopped = instance
        .activate(|_, _| (), configuration)
        .map_err(|e| format!("activate: {e}"))?;
    let processor = stopped
        .start_processing()
        .map_err(|e| format!("start_processing: {e}"))?;

    // Read clap.latency once at instance build time. PluginLatency::get
    // is a main-thread call; the AudioEffect trait's latency_samples()
    // is &self with no main-thread access, so we cache the value here.
    // Mid-session latency changes via clap_host_latency.changed are
    // ignored in v1 — handled by PDC-NEXT-VARIABLE-LATENCY.
    let latency = {
        let mut handle = instance.plugin_handle();
        match handle.get_extension::<PluginLatency>() {
            Some(ext) => ext.get(&mut handle),
            None => 0,
        }
    };

    // CLAP plugins typically declare ONE main audio port containing
    // 1 or 2 channels — we model exactly one port whose channel
    // count tracks the layout.
    let channels_per_port = match layout {
        ChannelLayout::StereoNative => 2,
        ChannelLayout::DualMono => 1,
    };
    let in_ports = AudioPorts::with_capacity(channels_per_port, 1);
    let out_ports = AudioPorts::with_capacity(channels_per_port, 1);

    Ok((
        InstanceSlot {
            processor: Some(processor),
            in_ports,
            out_ports,
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            event_in: EventBuffer::new(),
            steady_counter: 0,
        },
        latency,
    ))
}

/// Refill `slot.event_in` with the current parameter snapshot. Each
/// parameter that has a known ParamID generates a `ParamValueEvent`
/// at block offset 0. v1 = block-level automation; sample-accurate
/// dispatch (events with non-zero offsets) is the additive
/// CLAP-NEXT-SAMPLE-ACCURATE-AUTOMATION follow-up.
fn refill_event_buffer(slot: &mut InstanceSlot, param_values: &std::collections::HashMap<u32, f32>) {
    slot.event_in.clear();
    for (&id, &value) in param_values {
        let Some(clap_id) = ClapId::from_raw(id) else {
            continue;
        };
        let event = ParamValueEvent::new(
            0,                                   // sample offset within block
            clap_id,
            Pckn::new(Match::All, Match::All, Match::All, Match::All),
            value as f64,
            Cookie::empty(),
        );
        slot.event_in.push(&event);
    }
}

impl ClapEffect {
    /// One run of at most [`crate::dsp::PLUGIN_MAX_BLOCK`] frames.
    fn process_block(&mut self, buffer: &mut [f32], host_channels: usize) {
        let frames = buffer.len() / host_channels;
        if frames == 0 {
            return;
        }

        // Refill each slot's event buffer with the current param
        // snapshot. Done outside the layout match so DualMono's two
        // slots both see identical events (matched-stereo behaviour).
        for slot in &mut self.slots {
            refill_event_buffer(slot, &self.param_values);
        }

        match self.layout {
            ChannelLayout::StereoNative => {
                if let Err(e) = process_stereo_native(&mut self.slots[0], buffer, host_channels, frames) {
                    crate::dsp::note_refused_run(self.type_id_static, &e);
                }
            }
            ChannelLayout::DualMono => {
                let slots_len = self.slots.len();
                for ch in 0..host_channels.min(2) {
                    let slot = &mut self.slots[ch.min(slots_len - 1)];
                    if let Err(e) = process_dual_mono_channel(slot, buffer, host_channels, frames, ch) {
                        crate::dsp::note_refused_run(self.type_id_static, &e);
                    }
                }
            }
        }
    }
}

impl AudioEffect for ClapEffect {
    fn type_id(&self) -> &'static str {
        self.type_id_static
    }
    fn name(&self) -> &'static str {
        self.name_static
    }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] {
        &self.empty_descs
    }
    fn get_param(&self, name: &str) -> Option<f32> {
        let id = parse_param_id(name)?;
        self.param_values.get(&id).copied()
    }
    fn set_param(&mut self, name: &str, value: f32) {
        self.set_control_by_key(name, value);
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let host_channels = ctx.channels.max(1) as usize;
        crate::dsp::in_plugin_blocks(buffer, host_channels, |block| self.process_block(block, host_channels));
    }

    fn reset(&mut self) {
        // Drop and rebuild slots — same shape as LV2 reset.
        // Re-reads latency from the fresh slots since some plugins
        // recompute latency at instantiation time (e.g. when a buffer
        // size constraint shifts).
        let info = self.info.clone();
        let layout = self.layout;
        let sr = self.sample_rate;
        let n = self.slots.len();
        let mut fresh = Vec::with_capacity(n);
        let mut max_latency = 0u32;
        for _ in 0..n {
            match build_slot(&info, layout, sr) {
                Ok((slot, latency)) => {
                    fresh.push(slot);
                    max_latency = max_latency.max(latency);
                }
                Err(e) => {
                    tracing::warn!("CLAP '{}' reset failed: {e}", info.name);
                    return;
                }
            }
        }
        self.slots = fresh;
        self.latency_samples_cached = max_latency;
    }

    fn latency_samples(&self) -> u32 {
        // Snapshotted at instance build time via clap.latency.
        // Variable latency mid-session is filed as
        // PDC-NEXT-VARIABLE-LATENCY.
        self.latency_samples_cached
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        self.info
            .params
            .iter()
            .map(|p| {
                let v = self.param_values.get(&p.id).copied().unwrap_or(p.default);
                (p.key(), v)
            })
            .collect()
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (key, value) in params {
            self.set_control_by_key(key, *value);
        }
    }
}

/// Stereo-native process — one instance, one port with 2 channels.
fn process_stereo_native(
    slot: &mut InstanceSlot,
    buffer: &mut [f32],
    host_channels: usize,
    frames: usize,
) -> Result<(), String> {
    let needed = frames * 2;
    if slot.in_buf.len() < needed {
        slot.in_buf.resize(needed, 0.0);
    }
    if slot.out_buf.len() < needed {
        slot.out_buf.resize(needed, 0.0);
    }

    // De-interleave host buffer into per-channel scratch.
    {
        let (in_l, in_r) = slot.in_buf[..needed].split_at_mut(frames);
        for f in 0..frames {
            in_l[f] = buffer[f * host_channels];
            in_r[f] = if host_channels >= 2 {
                buffer[f * host_channels + 1]
            } else {
                in_l[f]
            };
        }
    }

    // Zero the output region before the plugin writes to it.
    for s in &mut slot.out_buf[..needed] {
        *s = 0.0;
    }

    let Some(processor) = slot.processor.as_mut() else {
        return Ok(());
    };

    // Build input port — one port carrying both channels via
    // chunks_exact_mut(frames). InputChannel takes &mut [f32]
    // (CLAP host API is uniform on mut for both directions).
    let in_buffers: Vec<InputChannel<'_, f32>> = slot.in_buf[..needed]
        .chunks_exact_mut(frames)
        .map(|chunk| InputChannel {
            buffer: chunk,
            is_constant: false,
        })
        .collect();
    let port_in = AudioPortBuffer {
        latency: 0,
        channels: AudioPortBufferType::f32_input_only(in_buffers.into_iter()),
    };
    let inputs = slot.in_ports.with_input_buffers([port_in]);

    let out_buffers: Vec<&mut [f32]> = slot.out_buf[..needed]
        .chunks_exact_mut(frames)
        .collect();
    let port_out = AudioPortBuffer {
        latency: 0,
        channels: AudioPortBufferType::f32_output_only(out_buffers.into_iter()),
    };
    let mut outputs = slot.out_ports.with_output_buffers([port_out]);

    let in_events = InputEvents::from_buffer(&slot.event_in);
    let res = processor.process(
        &inputs,
        &mut outputs,
        &in_events,
        &mut OutputEvents::void(),
        Some(slot.steady_counter),
        None,
    );
    if let Err(e) = res {
        return Err(e.to_string());
    }

    // Drop the audio-buffer wrappers so the borrows on slot.in_buf /
    // slot.out_buf end before we re-interleave.
    drop(inputs);
    drop(outputs);

    // Re-interleave. Layout in slot.out_buf is [L_frames..., R_frames...].
    for f in 0..frames {
        buffer[f * host_channels] = slot.out_buf[f];
        if host_channels >= 2 {
            buffer[f * host_channels + 1] = slot.out_buf[frames + f];
        }
    }

    slot.steady_counter += frames as u64;
    Ok(())
}

/// Dual-mono per-channel process. Called twice per host block (once
/// for L, once for R) with the corresponding mono slot.
fn process_dual_mono_channel(
    slot: &mut InstanceSlot,
    buffer: &mut [f32],
    host_channels: usize,
    frames: usize,
    ch: usize,
) -> Result<(), String> {
    if slot.in_buf.len() < frames {
        slot.in_buf.resize(frames, 0.0);
    }
    if slot.out_buf.len() < frames {
        slot.out_buf.resize(frames, 0.0);
    }

    for f in 0..frames {
        slot.in_buf[f] = buffer[f * host_channels + ch];
    }
    for s in &mut slot.out_buf[..frames] {
        *s = 0.0;
    }

    let Some(processor) = slot.processor.as_mut() else {
        return Ok(());
    };

    let in_slice: &mut [f32] = &mut slot.in_buf[..frames];
    let port_in = AudioPortBuffer {
        latency: 0,
        channels: AudioPortBufferType::f32_input_only(std::iter::once(InputChannel {
            buffer: in_slice,
            is_constant: false,
        })),
    };
    let inputs = slot.in_ports.with_input_buffers([port_in]);

    let out_slice: &mut [f32] = &mut slot.out_buf[..frames];
    let port_out = AudioPortBuffer {
        latency: 0,
        channels: AudioPortBufferType::f32_output_only(std::iter::once(out_slice)),
    };
    let mut outputs = slot.out_ports.with_output_buffers([port_out]);

    let in_events = InputEvents::from_buffer(&slot.event_in);
    let res = processor.process(
        &inputs,
        &mut outputs,
        &in_events,
        &mut OutputEvents::void(),
        Some(slot.steady_counter),
        None,
    );
    if let Err(e) = res {
        return Err(e.to_string());
    }
    drop(inputs);
    drop(outputs);

    for f in 0..frames {
        buffer[f * host_channels + ch] = slot.out_buf[f];
    }

    slot.steady_counter += frames as u64;
    Ok(())
}

/// Public factory used by [`crate::dsp::create_effect`] when the
/// type-id starts `"clap:"`.
pub fn instantiate_from_type_id(
    type_id: &str,
    sample_rate: u32,
    params: &[(String, f32)],
) -> Option<Box<dyn AudioEffect>> {
    let plugin_id = type_id.strip_prefix("clap:")?;
    let mut effect = match ClapEffect::new(plugin_id, sample_rate) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!("CLAP load failed for {type_id}: {err}");
            return None;
        }
    };
    effect.deserialize_params(params);
    Some(Box::new(effect))
}

/// Default `(key, value)` pairs for a freshly-added plugin instance.
pub fn default_params(plugin_id: &str) -> Option<Vec<(String, f32)>> {
    let info = lookup_by_id(plugin_id)?;
    Some(info.params.iter().map(|p| (p.key(), p.default)).collect())
}


// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn type_id_uses_plugin_id() {
        let info = ClapPluginInfo {
            bundle_path: PathBuf::from("/usr/lib/clap/Surge XT Effects.clap"),
            id: "org.surge-synth-team.surge-xt-fx".into(),
            name: "Surge XT Effects".into(),
            vendor: "Surge Synth Team".into(),
            version: "1.0".into(),
            audio_inputs: 2,
            audio_outputs: 2,
            params: vec![],
        };
        assert_eq!(info.type_id(), "clap:org.surge-synth-team.surge-xt-fx");
    }

    #[test]
    fn param_info_key_is_hex_formatted() {
        let p = ClapParamInfo {
            id: 0x12ab,
            name: "Cutoff".into(),
            default: 0.5,
            min: 0.0,
            max: 1.0,
            flags: 0,
        };
        assert_eq!(p.key(), "0x12ab");
    }

    #[test]
    fn search_paths_default_starts_with_user_home() {
        let _g = env_lock();
        std::env::remove_var("CLAP_PATH");
        let paths = search_paths();
        assert!(!paths.is_empty());
        let first = paths[0].to_string_lossy().into_owned();
        assert!(first.contains(".clap"), "user home first; got {first}");
    }

    #[test]
    fn search_paths_env_override_replaces_defaults() {
        let _g = env_lock();
        std::env::set_var("CLAP_PATH", "/tmp/a:/tmp/b");
        let paths = search_paths();
        std::env::remove_var("CLAP_PATH");
        assert_eq!(paths, vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]);
    }

    #[test]
    fn search_paths_dedupes_symlinks() {
        let _g = env_lock();
        let tmp = std::env::temp_dir();
        let target = tmp.join("klipp-clap-symlink-test");
        let _ = std::fs::create_dir_all(&target);
        let symlink = tmp.join("klipp-clap-symlink-test-link");
        let _ = std::fs::remove_file(&symlink);
        if std::os::unix::fs::symlink(&target, &symlink).is_err() {
            return;
        }
        std::env::set_var(
            "CLAP_PATH",
            format!("{}:{}", target.display(), symlink.display()),
        );
        let paths = search_paths();
        std::env::remove_var("CLAP_PATH");
        let _ = std::fs::remove_file(&symlink);
        let _ = std::fs::remove_dir(&target);
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn klipp_clap_disable_short_circuits_discovery() {
        let _g = env_lock();
        std::env::set_var("KLIPP_CLAP_DISABLE", "1");
        let plugins = discover_clap_plugins();
        std::env::remove_var("KLIPP_CLAP_DISABLE");
        assert!(plugins.is_empty());
    }

    #[test]
    fn cache_round_trip_via_serde() {
        let info = ClapPluginInfo {
            bundle_path: PathBuf::from("/tmp/test.clap"),
            id: "com.example.test".into(),
            name: "Test".into(),
            vendor: "v".into(),
            version: "1.0".into(),
            audio_inputs: 2,
            audio_outputs: 2,
            params: vec![ClapParamInfo {
                id: 42,
                name: "Gain".into(),
                default: 1.0,
                min: 0.0,
                max: 2.0,
                flags: 0,
            }],
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: ClapPluginInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, info.id);
        assert_eq!(back.params[0].id, 42);
    }

    #[test]
    fn schema_version_constant_is_locked() {
        // Long-term-safety pin — bumping the schema must come with a
        // deliberate edit + thoughtful migration thinking. Bumped
        // 1 → 2 when discovery started probing plugins for real
        // audio-port + param data instead of using sentinels.
        assert_eq!(cache::SCHEMA_VERSION, 2);
    }

    #[test]
    fn parse_param_id_round_trip() {
        assert_eq!(parse_param_id("0x12ab"), Some(0x12ab));
        assert_eq!(parse_param_id("0X1F"), Some(0x1f));
        assert_eq!(parse_param_id("42"), Some(42));
        assert_eq!(parse_param_id("not-a-number"), None);
    }

    /// Long-term-safety pin: saved values keyed by ParamID survive
    /// a future plugin version reordering its param list.
    #[test]
    fn control_values_round_trip_by_param_id_not_index() {
        let v1 = [ClapParamInfo {
                id: 0x10,
                name: "Gain".into(),
                default: 0.5,
                min: 0.0,
                max: 1.0,
                flags: 0,
            },
            ClapParamInfo {
                id: 0x20,
                name: "Tone".into(),
                default: 0.5,
                min: 0.0,
                max: 1.0,
                flags: 0,
            }];
        let saved: Vec<(String, f32)> = vec![
            (v1[0].key(), 0.9),
            (v1[1].key(), 0.1),
        ];
        let v2_reordered = [v1[1].clone(), v1[0].clone()];
        let id_to_idx: std::collections::HashMap<String, usize> = v2_reordered
            .iter()
            .enumerate()
            .map(|(i, p)| (p.key(), i))
            .collect();
        let mut values = vec![0.0_f32; v2_reordered.len()];
        for (key, val) in &saved {
            if let Some(&idx) = id_to_idx.get(key) {
                values[idx] = *val;
            }
        }
        assert_eq!(values[0], 0.1, "tone goes to its NEW position by ParamID");
        assert_eq!(values[1], 0.9, "gain goes to its NEW position by ParamID");
    }

    #[test]
    fn discover_finds_at_least_one_plugin_when_installed() {
        let _g = env_lock();
        if !std::path::Path::new("/usr/lib/clap").is_dir() {
            eprintln!("skipping: /usr/lib/clap not present");
            return;
        }
        std::env::remove_var("KLIPP_CLAP_DISABLE");
        let plugins = discover_clap_plugins();
        assert!(
            !plugins.is_empty(),
            "system /usr/lib/clap is present but discovery yielded zero plugins"
        );
    }
}
