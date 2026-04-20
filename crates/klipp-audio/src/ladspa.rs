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

// ─── Discovery types ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct LadspaPluginInfo {
    pub path: PathBuf,
    pub unique_id: u64,
    pub label: String,
    pub name: String,
    pub maker: String,
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

/// Open a single `.so`, iterate its descriptors, and return one info record
/// per plugin that has at least one audio input AND one audio output.
pub fn scan_ladspa_library(path: &Path) -> Vec<LadspaPluginInfo> {
    // SAFETY: loading an arbitrary .so is inherently unsafe — if the library
    // runs side effects in its constructor we have no defence. This mirrors
    // how the OFX host loads bundles.
    let lib = match unsafe { libloading::Library::new(path) } {
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
    // Upper bound: LADSPA bundles in practice expose 1-50 descriptors;
    // cap at 4096 so a broken or malicious plugin that returns non-null
    // for every index can't hang discovery indefinitely.
    const LADSPA_DESCRIPTOR_CAP: usize = 4096;
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
        let lib = unsafe { libloading::Library::new(path) }
            .map_err(|e| format!("dlopen failed: {e}"))?;

        let descriptor_fn: libloading::Symbol<LadspaDescriptorFunction> =
            unsafe { lib.get(b"ladspa_descriptor\0") }
                .map_err(|_| "ladspa_descriptor symbol missing".to_string())?;

        // Find descriptor with matching label.
        let mut descriptor_ptr: *const LadspaDescriptor = std::ptr::null();
        for i in 0..u32::MAX as usize {
            let raw = unsafe { descriptor_fn(i as c_ulong) };
            if raw.is_null() {
                break;
            }
            let d = unsafe { &*raw };
            let dl = unsafe { cstr_to_string(d.label) };
            if dl == label {
                descriptor_ptr = raw;
                break;
            }
        }
        if descriptor_ptr.is_null() {
            return Err(format!("label '{label}' not found in {}", path.display()));
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
        let desc = unsafe { &*descriptor_ptr };
        let mut audio_in_indices = Vec::new();
        let mut audio_out_indices = Vec::new();
        let mut control_out_indices = Vec::new();
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
                control_out_indices.push(i);
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
        // LADSPA v1 has no standard latency port. The de-facto convention is
        // a control OUTPUT port named "latency" — we don't surface those
        // (they're routed to the sink). Report 0.
        0
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
}
