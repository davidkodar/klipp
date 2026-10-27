//! LADSPA plugin host — discovery, loading, and `AudioEffect` adapter.
//!
//! Mirrors the OFX host pattern from `klipp-plugin/src/host.rs`: uses
//! `libloading::Library::new()` to dlopen each `.so`, looks up the single
//! exported `ladspa_descriptor` function, and iterates descriptors until
//! one returns null.
//!
//! Threading: LADSPA plugins are not inherently thread-safe, but `AudioEffect`
//! only requires `Send`. The mix thread owns the effect exclusively after
//! construction, so `unsafe impl Send` is valid as long as the raw pointers
//! never escape. We do NOT impl `Sync`.
//!
//! `effect_type` serialisation format: `"ladspa:{path}:{label}"`. The path
//! may contain colons on unusual filesystems, so parsing splits on the LAST
//! `:` to separate the label.

use std::ffi::CStr;
use std::os::raw::{c_char, c_float, c_int, c_ulong, c_void};
use std::path::{Path, PathBuf};

use crate::dsp::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

// ─── FFI types ─────────────────────────────────────────────────────────────

pub type LadspaHandle = *mut c_void;
pub type LadspaSampleRate = c_ulong;
pub type LadspaData = c_float;

// Port descriptor flags
pub const LADSPA_PORT_INPUT: c_int = 0x1;
pub const LADSPA_PORT_OUTPUT: c_int = 0x2;
pub const LADSPA_PORT_CONTROL: c_int = 0x4;
pub const LADSPA_PORT_AUDIO: c_int = 0x8;

// Port range hint descriptor flags
pub const LADSPA_HINT_BOUNDED_BELOW: c_int = 0x1;
pub const LADSPA_HINT_BOUNDED_ABOVE: c_int = 0x2;
pub const LADSPA_HINT_TOGGLED: c_int = 0x4;
pub const LADSPA_HINT_SAMPLE_RATE: c_int = 0x8;
pub const LADSPA_HINT_LOGARITHMIC: c_int = 0x10;
pub const LADSPA_HINT_INTEGER: c_int = 0x20;
pub const LADSPA_HINT_DEFAULT_MASK: c_int = 0x3C0;
pub const LADSPA_HINT_DEFAULT_NONE: c_int = 0x0;
pub const LADSPA_HINT_DEFAULT_MINIMUM: c_int = 0x40;
pub const LADSPA_HINT_DEFAULT_LOW: c_int = 0x80;
pub const LADSPA_HINT_DEFAULT_MIDDLE: c_int = 0xC0;
pub const LADSPA_HINT_DEFAULT_HIGH: c_int = 0x100;
pub const LADSPA_HINT_DEFAULT_MAXIMUM: c_int = 0x140;
pub const LADSPA_HINT_DEFAULT_0: c_int = 0x200;
pub const LADSPA_HINT_DEFAULT_1: c_int = 0x240;
pub const LADSPA_HINT_DEFAULT_100: c_int = 0x280;
pub const LADSPA_HINT_DEFAULT_440: c_int = 0x2C0;

#[repr(C)]
pub struct LadspaPortRangeHint {
    pub hint_descriptor: c_int,
    pub lower_bound: LadspaData,
    pub upper_bound: LadspaData,
}

#[repr(C)]
pub struct LadspaDescriptor {
    pub unique_id: c_ulong,
    pub label: *const c_char,
    pub properties: c_int,
    pub name: *const c_char,
    pub maker: *const c_char,
    pub copyright: *const c_char,
    pub port_count: c_ulong,
    pub port_descriptors: *const c_int,
    pub port_names: *const *const c_char,
    pub port_range_hints: *const LadspaPortRangeHint,
    pub implementation_data: *mut c_void,
    pub instantiate:
        unsafe extern "C" fn(*const LadspaDescriptor, LadspaSampleRate) -> LadspaHandle,
    pub connect_port: unsafe extern "C" fn(LadspaHandle, c_ulong, *mut LadspaData),
    pub activate: Option<unsafe extern "C" fn(LadspaHandle)>,
    pub run: unsafe extern "C" fn(LadspaHandle, c_ulong),
    pub run_adding: Option<unsafe extern "C" fn(LadspaHandle, c_ulong)>,
    pub set_run_adding_gain: Option<unsafe extern "C" fn(LadspaHandle, LadspaData)>,
    pub deactivate: Option<unsafe extern "C" fn(LadspaHandle)>,
    pub cleanup: unsafe extern "C" fn(LadspaHandle),
}

pub type LadspaDescriptorFunction =
    unsafe extern "C" fn(c_ulong) -> *const LadspaDescriptor;

// ─── Descriptor iteration cap ──────────────────────────────────────────────
//
// LADSPA bundles in practice expose 1–50 descriptors. The plugin's
// `ladspa_descriptor(i)` is supposed to return null when the iterator
// runs out, but a buggy or malicious plugin can return non-null
// forever. The host must bound the walk so it can't hang.
//
// Used by both the discovery walk (`scan_path`) and the find-by-label
// walk in `LadspaEffect::new`.
pub(crate) const LADSPA_DESCRIPTOR_CAP: usize = 4096;

/// Outcome of a bounded descriptor walk that's looking for a specific label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DescriptorLookup {
    /// Plugin returned a descriptor whose label matched at this index.
    FoundAt(usize),
    /// Plugin returned null before the cap (normal termination).
    NotFound,
    /// Cap was reached without a null OR a match — the plugin is misbehaving.
    CapHit,
}

/// Walk a plugin's descriptor list looking for one whose label matches.
///
/// `label_at(i)` returns `Some(label)` for a descriptor at index `i`, or
/// `None` if the plugin's `ladspa_descriptor(i)` returned null at that
/// index (the documented end-of-iteration sentinel).
///
/// Bounded by `cap` so a non-terminating plugin can't hang the host.
pub(crate) fn find_descriptor_index_by_label<F>(
    cap: usize,
    label: &str,
    mut label_at: F,
) -> DescriptorLookup
where
    F: FnMut(usize) -> Option<String>,
{
    for i in 0..cap {
        match label_at(i) {
            None => return DescriptorLookup::NotFound,
            Some(this_label) => {
                if this_label == label {
                    return DescriptorLookup::FoundAt(i);
                }
            }
        }
    }
    DescriptorLookup::CapHit
}

// ─── LADSPA latency-port readback policy ───────────────────────────────────
//
// LADSPA v1 has no dedicated latency field. The de-facto convention is a
// control-OUTPUT port literally named "latency" that the plugin writes on
// every `run()` call. The host-side policy below converts the most recent
// f32 sample-count the plugin wrote into a u32 the host can use for
// alignment, with three guards:
//
//   * No latency port at all (`slot == None`) → 0.
//   * Slot index out of bounds (defensive — should never happen in
//     practice since `control_out_sinks` is sized once at construction
//     and indexed by an enum-checked slot) → 0.
//   * Plugin wrote NaN, +∞, or a negative value (some misbehaving
//     plugins emit garbage when the calc isn't ready) → 0.
//   * Otherwise: round half-up to u32 (some plugins report fractional
//     samples for look-ahead windows).
//
// Extracted as a free function so the policy is
// pinned by unit tests without needing to construct a real LadspaEffect
// (which requires a .so + libloading + audio thread).
pub(crate) fn read_latency_samples(slot: Option<usize>, control_out_sinks: &[f32]) -> u32 {
    let s = match slot {
        Some(s) if s < control_out_sinks.len() => s,
        _ => return 0,
    };
    let samples = control_out_sinks[s];
    if samples.is_finite() && samples > 0.0 {
        (samples + 0.5) as u32
    } else {
        0
    }
}

// ─── Discovery types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LadspaControlPort {
    pub index: usize,
    pub name: String,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub is_integer: bool,
    pub is_logarithmic: bool,
    pub is_toggle: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LadspaPluginInfo {
    pub path: PathBuf,
    pub unique_id: u64,
    pub label: String,
    pub name: String,
    pub maker: String,
    /// LADSPA `copyright` field — typically a short licence phrase like
    /// "GPL" or "MIT/X11". Empty when the plugin author left it blank.
    /// `#[serde(default)]` so old cache files (pre-v2 schema) load
    /// cleanly with this field empty; a rescan fills it in.
    #[serde(default)]
    pub copyright: String,
    pub port_count: usize,
    pub control_inputs: Vec<LadspaControlPort>,
    pub audio_inputs: usize,
    pub audio_outputs: usize,
}

// ─── Discovery ─────────────────────────────────────────────────────────────

/// Safe C string extraction — returns empty string on null pointer.
unsafe fn cstr_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

/// Infer a reasonable default value from a LADSPA port range hint.
///
/// Handles DEFAULT_0 / DEFAULT_1 / DEFAULT_100 / DEFAULT_440 constants
/// and the MINIMUM / LOW / MIDDLE / HIGH / MAXIMUM interpolation flags.
/// Logarithmic hints interpolate in log-space. Sample-rate hints scale the
/// bounds by `sample_rate`. Falls back to the midpoint (or 0.0) if no hint
/// is set.
pub fn infer_default(hint: &LadspaPortRangeHint, sample_rate: u32) -> f32 {
    let h = hint.hint_descriptor;
    let is_sr = (h & LADSPA_HINT_SAMPLE_RATE) != 0;
    let sr = sample_rate as f32;
    let lower = if is_sr { hint.lower_bound * sr } else { hint.lower_bound };
    let upper = if is_sr { hint.upper_bound * sr } else { hint.upper_bound };
    let is_log = (h & LADSPA_HINT_LOGARITHMIC) != 0;

    let mix = |frac: f32| -> f32 {
        if is_log && lower > 0.0 && upper > 0.0 {
            (lower.ln() * (1.0 - frac) + upper.ln() * frac).exp()
        } else {
            lower * (1.0 - frac) + upper * frac
        }
    };

    match h & LADSPA_HINT_DEFAULT_MASK {
        x if x == LADSPA_HINT_DEFAULT_MINIMUM => lower,
        x if x == LADSPA_HINT_DEFAULT_LOW => mix(0.25),
        x if x == LADSPA_HINT_DEFAULT_MIDDLE => mix(0.5),
        x if x == LADSPA_HINT_DEFAULT_HIGH => mix(0.75),
        x if x == LADSPA_HINT_DEFAULT_MAXIMUM => upper,
        x if x == LADSPA_HINT_DEFAULT_0 => 0.0,
        x if x == LADSPA_HINT_DEFAULT_1 => 1.0,
        x if x == LADSPA_HINT_DEFAULT_100 => 100.0,
        x if x == LADSPA_HINT_DEFAULT_440 => 440.0,
        _ => {
            // DEFAULT_NONE — fall back to midpoint of bounded range or 0.
            let bounded_below = (h & LADSPA_HINT_BOUNDED_BELOW) != 0;
            let bounded_above = (h & LADSPA_HINT_BOUNDED_ABOVE) != 0;
            if bounded_below && bounded_above {
                mix(0.5)
            } else if bounded_below {
                lower
            } else if bounded_above {
                upper
            } else {
                0.0
            }
        }
    }
}

/// Return the list of directories to scan for LADSPA `.so` files.
///
/// Reads `$LADSPA_PATH` (colon-separated on Unix) first, then adds the
/// well-known system locations, then the user's `~/.ladspa`.
fn ladspa_search_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Ok(env_path) = std::env::var("LADSPA_PATH") {
        for p in env_path.split(':') {
            if !p.is_empty() {
                paths.push(PathBuf::from(p));
            }
        }
    }
    paths.push(PathBuf::from("/usr/lib/ladspa"));
    paths.push(PathBuf::from("/usr/local/lib/ladspa"));
    paths.push(PathBuf::from("/usr/lib64/ladspa"));
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".ladspa"));
    } else if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(home).join(".ladspa"));
    }
    paths
}

/// Discover every LADSPA plugin on the system.
///
/// Walks the search paths returned by `ladspa_search_paths()`, loads each
/// `.so` file, and extracts descriptors. Broken libraries are silently
/// skipped (logged at debug level only — no panics, no toasts). Plugins
/// with 0 audio inputs OR 0 audio outputs (pure generators / analysers)
/// are filtered out since they don't fit the current AudioEffectChain
/// in-place processing model.
pub fn discover_ladspa_plugins() -> Vec<LadspaPluginInfo> {
    let out = scan_search_paths();
    publish_scanned(&out);
    out
}

/// The plug-ins the last scan found, by library and label: the only
/// LADSPA libraries Klipp loads. `None` until a scan has run.
static SCANNED: std::sync::Mutex<Option<Vec<(PathBuf, String)>>> = std::sync::Mutex::new(None);

fn publish_scanned(plugins: &[LadspaPluginInfo]) {
    *SCANNED.lock().unwrap_or_else(|e| e.into_inner()) = Some(plugins.iter().map(|p| (p.path.clone(), p.label.clone())).collect());
}

/// Whether `path` and `label` name a plug-in the scan of the LADSPA
/// folders found. An effect id is resolved against this list and never
/// loaded from a path it names on its own; before any scan has run, the
/// folders are scanned once.
pub fn is_scanned(path: &Path, label: &str) -> bool {
    if SCANNED.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
        publish_scanned(&scan_search_paths());
    }
    SCANNED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|list| list.iter().any(|(p, l)| p == path && l == label))
}

fn scan_search_paths() -> Vec<LadspaPluginInfo> {
    let mut out = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    for dir in ladspa_search_paths() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            tracing::debug!("LADSPA: skipping unreadable dir {}", dir.display());
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("so") {
                continue;
            }
            for info in scan_ladspa_library(&path) {
                // Dedupe by unique_id across the whole search path.
                if seen_ids.insert(info.unique_id) {
                    out.push(info);
                }
            }
        }
    }
    out
}

/// Cached-on-disk variant of [`discover_ladspa_plugins`].
///
/// Caches the result of a successful scan in a JSON file so subsequent
/// launches skip the dlopen/iterate cost (50–500 ms depending on plugin
/// count) and return in 1–5 ms. Cache validity is checked by snapshotting
/// the mtime of every search directory at scan time and comparing on
/// load — adding / removing / renaming a `.so` inside one of these dirs
/// bumps the parent dir's mtime, so the cache invalidates automatically.
///
/// Mirrors the disk-cache pattern used by the title crate's font registry.
///
/// `cache_dir` is the base under which the cache file lives (the actual
/// path is `<cache_dir>/ladspa_registry.json`). Pass `None` to disable
/// caching and always do a fresh scan; otherwise pass something like
/// `dirs::cache_dir().map(|p| p.join("klipp"))`.
///
/// Failures during cache read or write are non-fatal — they fall through
/// to a fresh scan. The cache is purely an optimisation; correctness
/// always depends on the live scan path.
pub fn discover_ladspa_plugins_cached(cache_dir: Option<&Path>) -> Vec<LadspaPluginInfo> {
    let dirs = ladspa_search_paths();

    if let Some(base) = cache_dir {
        if let Some(plugins) = ladspa_cache::load(base, &dirs) {
            tracing::info!(
                "LADSPA: loaded {} plugin(s) from disk cache",
                plugins.len()
            );
            publish_scanned(&plugins);
            return plugins;
        }
    }

    let plugins = discover_ladspa_plugins();

    if let Some(base) = cache_dir {
        if let Err(e) = ladspa_cache::save(base, &dirs, &plugins) {
            tracing::warn!("LADSPA: failed to write cache: {e}");
        }
    }

    plugins
}

// ─── Disk cache ────────────────────────────────────────────────────────────
//
// Persisted at `<cache_dir>/ladspa_registry.json`. The cache is a JSON
// document containing a schema-version number, a per-search-directory
// mtime snapshot, and the discovered plugins. On load we recompute the
// mtime snapshot and compare; any difference invalidates the cache.

mod ladspa_cache {
    use super::LadspaPluginInfo;
    use serde::{Deserialize, Serialize};
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    /// Bump when [`LadspaPluginInfo`] or [`super::LadspaControlPort`]
    /// gain or lose fields, so old caches don't deserialise into the new
    /// shape with default-zero fields and confuse downstream code.
    ///
    /// History:
    ///   1 — initial schema
    ///   2 — added LadspaPluginInfo::copyright (safe to load v1 with
    ///       serde default = "", but a rescan fills the new field
    ///       with real values, which the dialog needs to surface)
    const SCHEMA_VERSION: u32 = 2;

    const FILE_NAME: &str = "ladspa_registry.json";

    #[derive(Serialize, Deserialize)]
    struct DiskCache {
        version: u32,
        dir_signatures: Vec<DirSig>,
        plugins: Vec<LadspaPluginInfo>,
    }

    #[derive(Serialize, Deserialize, PartialEq, Eq)]
    struct DirSig {
        path: String,
        /// Mtime in seconds since UNIX epoch. `None` for directories that
        /// don't exist; that absence is part of the signature so a future
        /// install of plugins into a previously-empty path forces a
        /// rescan.
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

    /// Try to read the on-disk cache. Returns `Some(plugins)` only if
    /// (a) the file exists and parses, (b) the schema version matches,
    /// and (c) the recorded directory mtimes match the current ones.
    pub fn load(base: &Path, dirs: &[PathBuf]) -> Option<Vec<LadspaPluginInfo>> {
        let path = cache_file(base);
        let data = std::fs::read_to_string(&path).ok()?;
        let cache: DiskCache = serde_json::from_str(&data).ok()?;
        if cache.version != SCHEMA_VERSION {
            tracing::debug!(
                "LADSPA cache: schema mismatch ({} vs {}), rescanning",
                cache.version,
                SCHEMA_VERSION
            );
            return None;
        }
        if cache.dir_signatures != collect_signatures(dirs) {
            tracing::debug!("LADSPA cache: directory mtime changed, rescanning");
            return None;
        }
        Some(cache.plugins)
    }

    /// Persist the scan result to disk. Creates the cache directory if
    /// it doesn't exist. Failures bubble up so callers can log them but
    /// they never alter the behaviour of a fresh scan.
    pub fn save(
        base: &Path,
        dirs: &[PathBuf],
        plugins: &[LadspaPluginInfo],
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

/// Open a single `.so`, iterate its descriptors, and return one info record
/// per plugin that has at least one audio input AND one audio output.
pub fn scan_ladspa_library(path: &Path) -> Vec<LadspaPluginInfo> {
    // SAFETY: loading an arbitrary .so is inherently unsafe — if the library
    // runs side effects in its constructor we have no defence. This mirrors
    // how the OFX host loads bundles.
    let lib = match klipp_load_lock::while_loading(|| unsafe { libloading::Library::new(path) }) {
        Ok(l) => l,
        Err(e) => {
            tracing::debug!("LADSPA: dlopen failed for {}: {}", path.display(), e);
            return Vec::new();
        }
    };

    let descriptor_fn: libloading::Symbol<LadspaDescriptorFunction> =
        match unsafe { lib.get(b"ladspa_descriptor\0") } {
            Ok(s) => s,
            Err(_) => return Vec::new(), // not a LADSPA plugin — ignore silently
        };

    // Guard descriptor iteration with catch_unwind so a single broken plugin
    // inside a bundle-style .so doesn't abort the whole discovery walk.
    // The iteration is bounded by LADSPA_DESCRIPTOR_CAP — see the
    // module-scope const for the rationale.
    let path_buf = path.to_path_buf();
    let df = *descriptor_fn;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut plugins = Vec::new();
        for i in 0..LADSPA_DESCRIPTOR_CAP {
            let raw = unsafe { df(i as c_ulong) };
            if raw.is_null() {
                break;
            }
            let desc = unsafe { &*raw };
            if let Some(info) = unsafe { extract_plugin_info(&path_buf, desc) } {
                plugins.push(info);
            }
        }
        if plugins.len() == LADSPA_DESCRIPTOR_CAP {
            tracing::warn!(
                "LADSPA: {} hit {}-descriptor cap — plugin may be non-terminating",
                path_buf.display(), LADSPA_DESCRIPTOR_CAP,
            );
        }
        plugins
    }));

    match result {
        Ok(plugins) => plugins,
        Err(_) => {
            tracing::debug!("LADSPA: descriptor iteration panicked in {}", path.display());
            Vec::new()
        }
    }
}

/// Pull all human-readable metadata + control-port info out of a descriptor.
/// Returns None if the plugin has no audio in OR no audio out (generators
/// / analysers are out of scope).
unsafe fn extract_plugin_info(
    path: &Path,
    desc: &LadspaDescriptor,
) -> Option<LadspaPluginInfo> {
    let port_count = desc.port_count as usize;
    let mut audio_in = 0usize;
    let mut audio_out = 0usize;
    let mut control_inputs = Vec::new();

    for i in 0..port_count {
        let port_desc = *desc.port_descriptors.add(i);
        let is_input = (port_desc & LADSPA_PORT_INPUT) != 0;
        let is_output = (port_desc & LADSPA_PORT_OUTPUT) != 0;
        let is_control = (port_desc & LADSPA_PORT_CONTROL) != 0;
        let is_audio = (port_desc & LADSPA_PORT_AUDIO) != 0;

        if is_audio && is_input {
            audio_in += 1;
        } else if is_audio && is_output {
            audio_out += 1;
        } else if is_control && is_input {
            let hint = &*desc.port_range_hints.add(i);
            let name_ptr = *desc.port_names.add(i);
            let name = cstr_to_string(name_ptr);

            let bounded_below = (hint.hint_descriptor & LADSPA_HINT_BOUNDED_BELOW) != 0;
            let bounded_above = (hint.hint_descriptor & LADSPA_HINT_BOUNDED_ABOVE) != 0;
            let is_sr = (hint.hint_descriptor & LADSPA_HINT_SAMPLE_RATE) != 0;
            let sr_mult = if is_sr { 48_000.0 } else { 1.0 };

            // Fallback bounds when the plugin doesn't declare them.
            let lower = if bounded_below { hint.lower_bound * sr_mult } else { 0.0 };
            let upper = if bounded_above {
                hint.upper_bound * sr_mult
            } else if bounded_below {
                (hint.lower_bound * sr_mult).max(1.0) * 10.0
            } else {
                1.0
            };
            let default = infer_default(hint, 48_000);

            control_inputs.push(LadspaControlPort {
                index: i,
                name,
                default,
                min: lower,
                max: upper.max(lower + f32::EPSILON),
                is_integer: (hint.hint_descriptor & LADSPA_HINT_INTEGER) != 0,
                is_logarithmic: (hint.hint_descriptor & LADSPA_HINT_LOGARITHMIC) != 0,
                is_toggle: (hint.hint_descriptor & LADSPA_HINT_TOGGLED) != 0,
            });
        }
    }

    if audio_in == 0 || audio_out == 0 {
        return None;
    }

    Some(LadspaPluginInfo {
        path: path.to_path_buf(),
        #[allow(clippy::unnecessary_cast)]
        unique_id: desc.unique_id as u64,
        label: cstr_to_string(desc.label),
        name: cstr_to_string(desc.name),
        maker: cstr_to_string(desc.maker),
        copyright: cstr_to_string(desc.copyright),
        port_count,
        control_inputs,
        audio_inputs: audio_in,
        audio_outputs: audio_out,
    })
}

// ─── LadspaEffect (AudioEffect impl) ───────────────────────────────────────

/// An instantiated LADSPA plugin wrapped as an `AudioEffect`.
///
/// Holds the `libloading::Library` alive for the lifetime of the handle —
/// dropping the lib while the handle is live would unmap the function
/// pointers inside the descriptor.
pub struct LadspaEffect {
    _lib: libloading::Library,
    descriptor: *const LadspaDescriptor,
    handle: LadspaHandle,
    #[allow(dead_code)]
    sample_rate: u32,

    /// Storage backing every control INPUT port. Index parallel with
    /// `info.control_inputs`.
    control_values: Vec<f32>,

    /// Per-port audio input buffers (one Vec per audio IN port, in port order).
    audio_in_bufs: Vec<Vec<f32>>,
    /// Per-port audio output buffers (one Vec per audio OUT port, in port order).
    audio_out_bufs: Vec<Vec<f32>>,
    audio_in_indices: Vec<usize>,
    audio_out_indices: Vec<usize>,
    /// Scratch for control OUTPUT ports we don't surface (discarded).
    control_out_sinks: Vec<f32>,
    control_out_indices: Vec<usize>,
    /// Index into `control_out_sinks` for a control-output port named
    /// "latency" (LADSPA v1's de-facto convention for plugins that
    /// introduce delay). `None` when the plugin doesn't expose one.
    latency_slot: Option<usize>,

    info: LadspaPluginInfo,

    /// Cached pointers we last passed to connect_port, used to skip reconnection
    /// when the buffer storage is unchanged between `process()` calls.
    last_in_ptrs: Vec<*mut f32>,
    last_out_ptrs: Vec<*mut f32>,

    /// Stable empty descriptor slice — satisfies `param_descriptors() -> &[..]`
    /// without fabricating `&'static str` port names (LADSPA names are runtime
    /// strings; we surface them through the UI's LADSPA-specific branch).
    empty_descs: Vec<AudioParamDescriptor>,
}

// SAFETY: `AudioEffect` requires `Send`. The mix thread owns the effect
// exclusively after construction — the raw descriptor pointer and plugin
// handle never cross threads concurrently. No `Sync` impl (trait doesn't
// need it, and LADSPA plugins are not thread-safe for concurrent access).
unsafe impl Send for LadspaEffect {}

impl LadspaEffect {
    /// Load the plugin at `path` whose label matches `label` and instantiate
    /// it at the given sample rate. Connects all ports and calls `activate`.
    pub fn new(path: &Path, label: &str, sample_rate: u32) -> Result<Self, String> {
        // SAFETY: same caveat as discovery — loading arbitrary libraries.
        let lib = klipp_load_lock::while_loading(|| unsafe { libloading::Library::new(path) })
            .map_err(|e| format!("dlopen failed: {e}"))?;

        let descriptor_fn: libloading::Symbol<LadspaDescriptorFunction> =
            unsafe { lib.get(b"ladspa_descriptor\0") }
                .map_err(|_| "ladspa_descriptor symbol missing".to_string())?;

        // Find descriptor with matching label. Bounded by
        // LADSPA_DESCRIPTOR_CAP — a buggy plugin that returns non-null
        // for every index would otherwise loop ~4 billion times before
        // the host realised the label wasn't there.
        let mut descriptor_ptr: *const LadspaDescriptor = std::ptr::null();
        let lookup = find_descriptor_index_by_label(
            LADSPA_DESCRIPTOR_CAP,
            label,
            |i| {
                let raw = unsafe { descriptor_fn(i as c_ulong) };
                if raw.is_null() {
                    None
                } else {
                    let d = unsafe { &*raw };
                    let dl = unsafe { cstr_to_string(d.label) };
                    if dl == label {
                        // Capture the matching pointer so the caller can
                        // instantiate from it after the lookup returns.
                        descriptor_ptr = raw;
                    }
                    Some(dl)
                }
            },
        );
        match lookup {
            DescriptorLookup::FoundAt(_) => { /* descriptor_ptr is set */ }
            DescriptorLookup::NotFound => {
                return Err(format!("label '{label}' not found in {}", path.display()));
            }
            DescriptorLookup::CapHit => {
                tracing::warn!(
                    "LADSPA: hit {}-descriptor cap looking for '{}' in {} — plugin may be non-terminating",
                    LADSPA_DESCRIPTOR_CAP, label, path.display(),
                );
                return Err(format!(
                    "label '{label}' not found within descriptor cap in {}",
                    path.display()
                ));
            }
        }

        // Rebuild an info record from the selected descriptor so we own our
        // copy of the metadata for the lifetime of the effect.
        let info = unsafe { extract_plugin_info(path, &*descriptor_ptr) }
            .ok_or_else(|| "plugin has no audio IO".to_string())?;

        // Instantiate.
        let handle = unsafe {
            ((*descriptor_ptr).instantiate)(descriptor_ptr, sample_rate as LadspaSampleRate)
        };
        if handle.is_null() {
            return Err("instantiate returned null".to_string());
        }

        // Discover audio/control port indices so we can size our buffers.
        // Also remember the slot (within control_out_sinks) of a
        // control-output port named "latency" — LADSPA v1's de-facto
        // convention for reporting plugin delay to the host. Matched
        // case-insensitively; trimmed of whitespace.
        let desc = unsafe { &*descriptor_ptr };
        let mut audio_in_indices = Vec::new();
        let mut audio_out_indices = Vec::new();
        let mut control_out_indices = Vec::new();
        let mut latency_slot: Option<usize> = None;
        for i in 0..desc.port_count as usize {
            let pd = unsafe { *desc.port_descriptors.add(i) };
            let is_input = (pd & LADSPA_PORT_INPUT) != 0;
            let is_output = (pd & LADSPA_PORT_OUTPUT) != 0;
            let is_audio = (pd & LADSPA_PORT_AUDIO) != 0;
            let is_control = (pd & LADSPA_PORT_CONTROL) != 0;
            if is_audio && is_input {
                audio_in_indices.push(i);
            } else if is_audio && is_output {
                audio_out_indices.push(i);
            } else if is_control && is_output {
                let slot = control_out_indices.len();
                control_out_indices.push(i);
                if latency_slot.is_none() {
                    let name_ptr = unsafe { *desc.port_names.add(i) };
                    let name = unsafe { cstr_to_string(name_ptr) };
                    if name.trim().eq_ignore_ascii_case("latency") {
                        latency_slot = Some(slot);
                    }
                }
            }
        }

        let control_values: Vec<f32> =
            info.control_inputs.iter().map(|p| p.default).collect();
        let control_out_sinks = vec![0.0f32; control_out_indices.len()];

        let mut effect = Self {
            _lib: lib,
            descriptor: descriptor_ptr,
            handle,
            sample_rate,
            control_values,
            audio_in_bufs: vec![Vec::new(); audio_in_indices.len()],
            audio_out_bufs: vec![Vec::new(); audio_out_indices.len()],
            audio_in_indices,
            audio_out_indices,
            control_out_sinks,
            control_out_indices,
            latency_slot,
            info,
            last_in_ptrs: Vec::new(),
            last_out_ptrs: Vec::new(),
            empty_descs: Vec::new(),
        };

        effect.connect_control_ports();
        effect.connect_control_outputs();

        // activate() is optional per the spec.
        unsafe {
            if let Some(act) = (*descriptor_ptr).activate {
                act(handle);
            }
        }

        Ok(effect)
    }

    /// Connect each control-input port to its slot in `self.control_values`.
    fn connect_control_ports(&mut self) {
        let desc = unsafe { &*self.descriptor };
        for (slot, port) in self.info.control_inputs.iter().enumerate() {
            // SAFETY: control_values is stable as long as we don't reallocate
            // it (we never push/pop after construction).
            let ptr = self.control_values.as_mut_ptr().wrapping_add(slot);
            unsafe {
                (desc.connect_port)(self.handle, port.index as c_ulong, ptr);
            }
        }
    }

    /// Connect each control-output port to a sink slot (we discard values).
    fn connect_control_outputs(&mut self) {
        let desc = unsafe { &*self.descriptor };
        for (slot, port_idx) in self.control_out_indices.iter().enumerate() {
            let ptr = self.control_out_sinks.as_mut_ptr().wrapping_add(slot);
            unsafe {
                (desc.connect_port)(self.handle, *port_idx as c_ulong, ptr);
            }
        }
    }

    /// Ensure audio buffers hold `frames` samples and reconnect ports if the
    /// backing pointers changed (Vec reallocation).
    fn ensure_audio_buffers(&mut self, frames: usize) {
        let desc = unsafe { &*self.descriptor };
        let mut in_ptrs = Vec::with_capacity(self.audio_in_bufs.len());
        for b in &mut self.audio_in_bufs {
            if b.len() < frames {
                b.resize(frames, 0.0);
            }
            in_ptrs.push(b.as_mut_ptr());
        }
        let mut out_ptrs = Vec::with_capacity(self.audio_out_bufs.len());
        for b in &mut self.audio_out_bufs {
            if b.len() < frames {
                b.resize(frames, 0.0);
            }
            out_ptrs.push(b.as_mut_ptr());
        }

        if in_ptrs != self.last_in_ptrs {
            for (i, port_idx) in self.audio_in_indices.iter().enumerate() {
                unsafe {
                    (desc.connect_port)(self.handle, *port_idx as c_ulong, in_ptrs[i]);
                }
            }
            self.last_in_ptrs = in_ptrs;
        }
        if out_ptrs != self.last_out_ptrs {
            for (i, port_idx) in self.audio_out_indices.iter().enumerate() {
                unsafe {
                    (desc.connect_port)(self.handle, *port_idx as c_ulong, out_ptrs[i]);
                }
            }
            self.last_out_ptrs = out_ptrs;
        }
    }
}

impl AudioEffect for LadspaEffect {
    fn type_id(&self) -> &'static str {
        "ladspa"
    }

    fn name(&self) -> &'static str {
        // AudioEffect requires &'static str — LADSPA names are runtime strings,
        // so we return a generic label. UI code that needs the real plugin
        // name reads it from `state.ladspa_plugins` via the effect_type path.
        "LADSPA Plugin"
    }

    fn param_descriptors(&self) -> &[AudioParamDescriptor] {
        // Static descriptors would require leaking per-instance strings.
        // The UI handles LADSPA via `state.ladspa_plugins` directly, so we
        // return an empty slice here. `AudioEffectChain::from_instances`
        // still drives params through `deserialize_params` / `set_param`.
        &self.empty_descs
    }

    fn get_param(&self, name: &str) -> Option<f32> {
        let idx = self.info.control_inputs.iter().position(|p| p.name == name)?;
        self.control_values.get(idx).copied()
    }

    fn set_param(&mut self, name: &str, value: f32) {
        if let Some(idx) = self.info.control_inputs.iter().position(|p| p.name == name) {
            let port = &self.info.control_inputs[idx];
            let clamped = value.clamp(port.min, port.max);
            self.control_values[idx] = clamped;
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        // We run the pipeline for interleaved 1- or 2-channel audio.
        // ctx.channels tells us the true host channel count; fall back to
        // 2 (stereo) if the caller didn't set it.
        let host_channels = ctx.channels.max(1) as usize;
        if host_channels == 0 || buffer.is_empty() {
            return;
        }
        let frames = buffer.len() / host_channels;
        if frames == 0 {
            return;
        }

        self.ensure_audio_buffers(frames);

        let plugin_in = self.audio_in_bufs.len();
        let plugin_out = self.audio_out_bufs.len();

        // ── Deinterleave into plugin input buffers ─────────────────────
        if plugin_in == 1 {
            // Mono plugin — downmix host channels.
            let inv = 1.0 / host_channels as f32;
            let buf = &mut self.audio_in_bufs[0];
            for f in 0..frames {
                let mut sum = 0.0;
                for c in 0..host_channels {
                    sum += buffer[f * host_channels + c];
                }
                buf[f] = sum * inv;
            }
        } else {
            // Plugin has multiple audio inputs — fan out host channels,
            // repeating the last host channel for extra plugin inputs.
            for (pi, buf) in self.audio_in_bufs.iter_mut().enumerate() {
                let src_c = pi.min(host_channels - 1);
                for f in 0..frames {
                    buf[f] = buffer[f * host_channels + src_c];
                }
            }
        }

        // ── Run ────────────────────────────────────────────────────────
        unsafe {
            ((*self.descriptor).run)(self.handle, frames as c_ulong);
        }

        // ── Interleave plugin outputs back into host buffer ────────────
        if plugin_out == 1 {
            // Mono output — duplicate to every host channel.
            let buf = &self.audio_out_bufs[0];
            for f in 0..frames {
                let v = buf[f];
                for c in 0..host_channels {
                    buffer[f * host_channels + c] = v;
                }
            }
        } else {
            for c in 0..host_channels {
                let po = c.min(plugin_out - 1);
                let buf = &self.audio_out_bufs[po];
                for f in 0..frames {
                    buffer[f * host_channels + c] = buf[f];
                }
            }
        }
    }

    fn reset(&mut self) {
        // LADSPA offers deactivate/activate for state reset. Some plugins
        // require the sequence; if either is absent just skip silently.
        unsafe {
            if let Some(deact) = (*self.descriptor).deactivate {
                deact(self.handle);
            }
            if let Some(act) = (*self.descriptor).activate {
                act(self.handle);
            }
        }
    }

    fn latency_samples(&self) -> u32 {
        // LADSPA v1 has no dedicated latency field. The de-facto
        // convention is a control-OUTPUT port literally named "latency"
        // that the plugin writes on every `run()`. At construction we
        // scan port names and remember the slot (if any); here we read
        // the most recent value the plugin wrote there.
        //
        // See `read_latency_samples` (free function below) for the
        // readback policy — NaN/infinity/negative guards, half-up
        // rounding to u32, slot-out-of-bounds guard. Extracted so the
        // policy is unit-testable without constructing a real
        // LadspaEffect.
        read_latency_samples(self.latency_slot, &self.control_out_sinks)
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        self.info
            .control_inputs
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.clone(), self.control_values[i]))
            .collect()
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params {
            self.set_param(name, *value);
        }
    }
}

impl Drop for LadspaEffect {
    fn drop(&mut self) {
        // SAFETY: descriptor outlives self because `_lib` keeps the .so
        // mapped until Self is dropped, and descriptor pointers live inside
        // the .so's static data.
        unsafe {
            if let Some(deact) = (*self.descriptor).deactivate {
                deact(self.handle);
            }
            ((*self.descriptor).cleanup)(self.handle);
        }
    }
}

// ─── Param-unit hint for UI sliders ────────────────────────────────────────

/// Best-effort mapping from a control-port name to a `ParamUnit` for UI
/// display. LADSPA has no formal units system so we scan for common
/// substrings ("Hz", "dB", "ms", "%"). Fallback is `ParamUnit::None`.
pub fn unit_hint_for_port_name(name: &str) -> ParamUnit {
    let lower = name.to_lowercase();
    if lower.contains("hz") || lower.contains("freq") {
        ParamUnit::Hz
    } else if lower.contains("db") || lower.contains("gain") || lower.contains("level") {
        ParamUnit::Db
    } else if lower.contains("ms") || lower.contains("time") || lower.contains("delay") {
        ParamUnit::Ms
    } else if lower.contains("ratio") {
        ParamUnit::Ratio
    } else {
        ParamUnit::None
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {

    /// An effect id from a project naming a library outside the LADSPA
    /// scan is never loaded: a real shared library copied to a folder of
    /// its own, named by a `ladspa:` id, gives no effect and is not
    /// mapped into the process. Every plug-in the scan found still is
    /// known.
    #[test]
    fn an_effect_id_loads_only_a_scanned_plug_in() {
        let lib = ["/usr/lib/libz.so.1", "/usr/lib64/libz.so.1", "/lib/x86_64-linux-gnu/libz.so.1", "/usr/lib/x86_64-linux-gnu/libz.so.1"]
            .into_iter().map(Path::new).find(|p| p.is_file());
        let dir = std::env::temp_dir().join(format!("klipp-ladspa-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let evil = dir.join("plugin.so");
        match lib {
            Some(l) => { std::fs::copy(l, &evil).unwrap(); }
            None => std::fs::write(&evil, b"not a library").unwrap(),
        }
        let id = format!("ladspa:{}:anything", evil.display());
        assert!(!is_scanned(&evil, "anything"));
        assert!(crate::dsp::create_effect(&id, 48_000).is_none(), "left offline");
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
        assert!(!maps.contains(&dir.display().to_string()), "the library was never mapped");
        for p in discover_ladspa_plugins() {
            assert!(is_scanned(&p.path, &p.label), "{} {}", p.path.display(), p.label);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    #[test]
    fn ladspa_type_id_round_trip() {
        let path = "/usr/lib/ladspa/amp.so";
        let label = "amp_stereo";
        let type_id = format!("ladspa:{path}:{label}");
        assert!(type_id.starts_with("ladspa:"));
        let rest = type_id.strip_prefix("ladspa:").unwrap();
        let last = rest.rfind(':').unwrap();
        assert_eq!(&rest[..last], path);
        assert_eq!(&rest[last + 1..], label);
    }

    #[test]
    fn infer_default_middle() {
        let hint = LadspaPortRangeHint {
            hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
                | LADSPA_HINT_BOUNDED_ABOVE
                | LADSPA_HINT_DEFAULT_MIDDLE,
            lower_bound: 0.0,
            upper_bound: 1.0,
        };
        let d = infer_default(&hint, 48000);
        assert!((d - 0.5).abs() < 0.01, "got {d}");
    }

    #[test]
    fn infer_default_constants() {
        let hint_0 = LadspaPortRangeHint {
            hint_descriptor: LADSPA_HINT_DEFAULT_0,
            lower_bound: -10.0,
            upper_bound: 10.0,
        };
        assert_eq!(infer_default(&hint_0, 48000), 0.0);

        let hint_1 = LadspaPortRangeHint {
            hint_descriptor: LADSPA_HINT_DEFAULT_1,
            lower_bound: 0.0,
            upper_bound: 100.0,
        };
        assert_eq!(infer_default(&hint_1, 48000), 1.0);

        let hint_100 = LadspaPortRangeHint {
            hint_descriptor: LADSPA_HINT_DEFAULT_100,
            lower_bound: 0.0,
            upper_bound: 200.0,
        };
        assert_eq!(infer_default(&hint_100, 48000), 100.0);

        let hint_440 = LadspaPortRangeHint {
            hint_descriptor: LADSPA_HINT_DEFAULT_440,
            lower_bound: 20.0,
            upper_bound: 20000.0,
        };
        assert_eq!(infer_default(&hint_440, 48000), 440.0);
    }

    #[test]
    fn infer_default_minimum_maximum() {
        let hint_min = LadspaPortRangeHint {
            hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
                | LADSPA_HINT_BOUNDED_ABOVE
                | LADSPA_HINT_DEFAULT_MINIMUM,
            lower_bound: -6.0,
            upper_bound: 6.0,
        };
        assert_eq!(infer_default(&hint_min, 48000), -6.0);

        let hint_max = LadspaPortRangeHint {
            hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
                | LADSPA_HINT_BOUNDED_ABOVE
                | LADSPA_HINT_DEFAULT_MAXIMUM,
            lower_bound: -6.0,
            upper_bound: 6.0,
        };
        assert_eq!(infer_default(&hint_max, 48000), 6.0);
    }

    #[test]
    fn infer_default_logarithmic_middle() {
        // Log-space midpoint of [1, 100] = 10.
        let hint = LadspaPortRangeHint {
            hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
                | LADSPA_HINT_BOUNDED_ABOVE
                | LADSPA_HINT_LOGARITHMIC
                | LADSPA_HINT_DEFAULT_MIDDLE,
            lower_bound: 1.0,
            upper_bound: 100.0,
        };
        let d = infer_default(&hint, 48000);
        assert!((d - 10.0).abs() < 0.01, "got {d}");
    }

    #[test]
    fn infer_default_sample_rate_scaling() {
        // Upper bound of 0.5 with SAMPLE_RATE hint and 48000 sr → 24000 (Nyquist).
        let hint = LadspaPortRangeHint {
            hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
                | LADSPA_HINT_BOUNDED_ABOVE
                | LADSPA_HINT_SAMPLE_RATE
                | LADSPA_HINT_DEFAULT_MAXIMUM,
            lower_bound: 0.0,
            upper_bound: 0.5,
        };
        let d = infer_default(&hint, 48000);
        assert!((d - 24000.0).abs() < 0.1);
    }

    #[test]
    fn discover_returns_vec_without_panic() {
        // Count may be 0 in CI (no LADSPA installed) — we only assert no panic.
        let plugins = discover_ladspa_plugins();
        let _ = plugins;
    }

    #[test]
    fn unit_hint_maps_common_names() {
        assert_eq!(unit_hint_for_port_name("Frequency (Hz)"), ParamUnit::Hz);
        assert_eq!(unit_hint_for_port_name("Gain (dB)"), ParamUnit::Db);
        assert_eq!(unit_hint_for_port_name("Delay Time"), ParamUnit::Ms);
        assert_eq!(unit_hint_for_port_name("Ratio"), ParamUnit::Ratio);
        assert_eq!(unit_hint_for_port_name("Dry/Wet"), ParamUnit::None);
    }

    // ═══════════════════════════════════════════════════════════════
    // find_descriptor_index_by_label.
    //
    // The find-by-label walk in LadspaEffect::new previously did
    //   for i in 0..u32::MAX as usize { … }
    // which would loop ~4 billion times on a buggy plugin that returns
    // non-null for every index without a matching label. The fix
    // bounds the walk by LADSPA_DESCRIPTOR_CAP (the same constant
    // already used by the discovery loop). These tests exercise the
    // four reachable outcomes via stub closures so the loop's
    // termination logic is testable without loading a real .so.
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn descriptor_cap_is_bounded_and_reasonable() {
        // Future "let me bump this back to u32::MAX" surfaces in CI.
        assert!(LADSPA_DESCRIPTOR_CAP <= 8192,
            "cap must stay bounded so a buggy plugin can't hang the host");
        assert!(LADSPA_DESCRIPTOR_CAP >= 256,
            "cap must allow legitimately large LADSPA bundles (some expose 100+)");
    }

    #[test]
    fn find_descriptor_returns_first_match_and_short_circuits() {
        // Plugin returns labels "a", "b", "c", "target", "more" then null.
        let labels = ["a", "b", "c", "target", "more"];
        let mut visited = 0usize;
        let result = find_descriptor_index_by_label(
            LADSPA_DESCRIPTOR_CAP,
            "target",
            |i| {
                visited = visited.max(i + 1);
                labels.get(i).map(|s| s.to_string())
            },
        );
        assert_eq!(result, DescriptorLookup::FoundAt(3));
        assert_eq!(visited, 4, "must short-circuit on match — index 4 should not be probed");
    }

    #[test]
    fn find_descriptor_returns_not_found_when_plugin_terminates() {
        // Plugin returns labels "a", "b", "c" then null at index 3.
        // Cap is huge but null arrives first — we should NOT report cap-hit.
        let labels = ["a", "b", "c"];
        let result = find_descriptor_index_by_label(
            LADSPA_DESCRIPTOR_CAP,
            "missing",
            |i| labels.get(i).map(|s| s.to_string()),
        );
        assert_eq!(result, DescriptorLookup::NotFound);
    }

    #[test]
    fn find_descriptor_returns_cap_hit_on_non_terminating_plugin() {
        // The exact bug C-7 calls out: plugin returns non-null for every
        // index AND the label never matches. Without the cap this would
        // loop ~4 billion times. With the cap it must report CapHit and
        // call count must equal cap.
        let mut call_count = 0usize;
        let cap = 16; // small cap to keep the test fast
        let result = find_descriptor_index_by_label(
            cap,
            "needle",
            |_i| {
                call_count += 1;
                Some("haystack".to_string())
            },
        );
        assert_eq!(result, DescriptorLookup::CapHit);
        assert_eq!(call_count, cap,
            "non-terminating plugin must be probed exactly `cap` times — neither more (loop bound bug) nor fewer (early exit bug)");
    }

    // ═══════════════════════════════════════════════════════════════
    // read_latency_samples.
    //
    // The 2026-04-20 array entry claimed the latency port-name scan
    // shipped, and today's audit confirmed the production code (port
    // scan at construction, control_out_sinks readback at query) is
    // present and correct. These tests pin the readback policy so a
    // future "let me drop the NaN/negative guards" or "let me change
    // the rounding" surfaces in CI rather than as wrong A/V alignment
    // when an audio effect with a latency port is engaged.
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn latency_returns_zero_when_plugin_has_no_latency_port() {
        // Slot=None is the common case: most LADSPA plugins don't
        // expose a "latency" control output. The host must report 0
        // delay so A/V sync isn't perturbed by a phantom shift.
        assert_eq!(read_latency_samples(None, &[]), 0);
        assert_eq!(read_latency_samples(None, &[42.0, 7.0]), 0);
    }

    #[test]
    fn latency_reads_the_correct_slot() {
        // Slot points into control_out_sinks; the value at that slot
        // is the plugin's last-written latency in samples.
        let sinks = vec![0.0, 64.0, 0.0]; // slot 1 holds the latency
        assert_eq!(read_latency_samples(Some(1), &sinks), 64);
    }

    #[test]
    fn latency_rounds_fractional_samples_half_up() {
        // Look-ahead limiters / FFT plugins often report fractional
        // sample latency. Half-up matches the comment in the production
        // body and keeps the host's alignment within ±0.5 sample.
        assert_eq!(read_latency_samples(Some(0), &[5.5]), 6,
            "5.5 must round to 6 (half-up)");
        assert_eq!(read_latency_samples(Some(0), &[5.4]), 5,
            "5.4 must round to 5");
        assert_eq!(read_latency_samples(Some(0), &[5.0]), 5,
            "exact 5.0 stays 5");
    }

    #[test]
    fn latency_clamps_garbage_values_to_zero() {
        // Some plugins emit NaN or +/-inf during the first few run()
        // calls before their internal state stabilises. The host
        // refuses to propagate that into A/V alignment.
        assert_eq!(read_latency_samples(Some(0), &[f32::NAN]), 0,
            "NaN latency must clamp to 0, not panic");
        assert_eq!(read_latency_samples(Some(0), &[f32::INFINITY]), 0,
            "+inf latency must clamp to 0");
        assert_eq!(read_latency_samples(Some(0), &[f32::NEG_INFINITY]), 0,
            "-inf latency must clamp to 0");
        assert_eq!(read_latency_samples(Some(0), &[-3.0]), 0,
            "negative latency is nonsensical → 0");
        assert_eq!(read_latency_samples(Some(0), &[0.0]), 0,
            "exactly 0.0 → 0 (not interesting; just pinning behaviour)");
    }

    #[test]
    fn latency_with_out_of_bounds_slot_returns_zero_no_panic() {
        // Defensive: the latency_slot is set at construction from
        // control_out_indices.len(), which equals control_out_sinks.len()
        // for the lifetime of LadspaEffect, so an out-of-bounds slot is
        // unreachable in normal flow. But pinning the guard means a
        // future refactor that desyncs the two can't crash the audio
        // thread. The original inline match would have indexed
        // control_out_sinks[slot] and panicked.
        assert_eq!(read_latency_samples(Some(99), &[0.0, 0.0]), 0);
        assert_eq!(read_latency_samples(Some(0), &[]), 0);
    }

    #[test]
    fn find_descriptor_handles_empty_plugin() {
        // Plugin returns null at index 0 — empty descriptor list.
        let result = find_descriptor_index_by_label(
            LADSPA_DESCRIPTOR_CAP,
            "anything",
            |_i| None,
        );
        assert_eq!(result, DescriptorLookup::NotFound);
    }

    // ─── Disk cache round-trip ───────────────────────────────────────

    fn fixture_plugin(unique_id: u64, name: &str) -> LadspaPluginInfo {
        LadspaPluginInfo {
            path: PathBuf::from(format!("/tmp/fake/{name}.so")),
            unique_id,
            label: name.to_string(),
            name: name.to_string(),
            maker: "Test Suite".to_string(),
            copyright: "GPL".to_string(),
            port_count: 4,
            control_inputs: vec![LadspaControlPort {
                index: 0,
                name: "Gain".to_string(),
                default: 1.0,
                min: 0.0,
                max: 2.0,
                is_integer: false,
                is_logarithmic: false,
                is_toggle: false,
            }],
            audio_inputs: 1,
            audio_outputs: 1,
        }
    }

    fn unique_tmp(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "klipp-test-ladspa-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn cache_roundtrips_plugins() {
        let tmp = unique_tmp("roundtrip");
        std::fs::create_dir_all(&tmp).unwrap();

        let dirs = vec![tmp.clone()];
        let plugins = vec![fixture_plugin(1, "alpha"), fixture_plugin(2, "beta")];
        ladspa_cache::save(&tmp, &dirs, &plugins).expect("save");

        let loaded = ladspa_cache::load(&tmp, &dirs).expect("load");
        assert_eq!(loaded.len(), plugins.len());
        assert_eq!(loaded[0].unique_id, 1);
        assert_eq!(loaded[0].name, "alpha");
        assert_eq!(loaded[0].control_inputs.len(), 1);
        assert_eq!(loaded[0].control_inputs[0].name, "Gain");
        assert_eq!(loaded[1].unique_id, 2);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cache_invalidates_when_dir_signature_differs() {
        // Save against one dir, load against a different dir → miss.
        // Simulates the "user installed a new plugin pack into a previously-
        // empty path" case (or removed plugins from one of the dirs).
        let base = unique_tmp("mtime");
        let dir_a = base.join("a");
        let dir_b = base.join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        let plugins = vec![fixture_plugin(42, "stable")];
        ladspa_cache::save(&base, std::slice::from_ref(&dir_a), &plugins).expect("save");

        let same = ladspa_cache::load(&base, std::slice::from_ref(&dir_a));
        assert!(same.is_some(), "same-dir load should hit");

        let different = ladspa_cache::load(&base, std::slice::from_ref(&dir_b));
        assert!(
            different.is_none(),
            "different-dir signature should miss the cache"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cache_load_missing_file_returns_none() {
        let tmp = unique_tmp("missing");
        // Don't create the dir — cache file definitely doesn't exist.
        let result = ladspa_cache::load(&tmp, std::slice::from_ref(&tmp));
        assert!(result.is_none(), "missing cache file → None");
    }

    #[test]
    fn cache_load_corrupt_json_returns_none() {
        let tmp = unique_tmp("corrupt");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("ladspa_registry.json"), "{not valid json").unwrap();

        let result = ladspa_cache::load(&tmp, std::slice::from_ref(&tmp));
        assert!(result.is_none(), "corrupt cache → None, not panic");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
