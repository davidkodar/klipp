//! # Safety conventions for host.rs
//!
//! The `unsafe { }` blocks in this file are divided between plugin
//! lifecycle (dlopen + typed symbol lookup for the plugin's
//! `OfxGetPlugin` / `OfxGetNumberOfPlugins` exports) and the render
//! dispatch (calling the plugin's `main_entry` with freshly-built
//! property sets). Both share a common rationale:
//!
//! * **Plugin library liveness** — every [`OfxPlugin`] holds an
//!   `Option<Arc<libloading::Library>>`. The raw `*const OfxPluginRaw`
//!   pointer stays valid for as long as the Arc does. Host code never
//!   uses the raw pointer after dropping the Arc.
//! * **Property-set handles** — opaque `*mut c_void` values that are
//!   really addresses of `Box<PropertySet>` registered in the global
//!   `PROP_STORE`. Derefing one through `with_propset` / `with_param`
//!   is sound while the handle is registered; the host is the sole
//!   writer to that store so it controls the lifetime.
//! * **`set_host(&OfxHostC)`** hands the plugin a `*const OfxHostC`
//!   to the process's one host struct (`PROCESS_HOST`), leaked once
//!   and never freed, so the plugin can store the pointer whichever
//!   host gave it and however long that host lives.
//! * **`main_entry` invocation** re-enters plugin code. Any panic
//!   inside the plugin's suite callbacks is converted to
//!   `K_OFX_STAT_ERR_FATAL` by the `ofx_guard!` wrapper on each
//!   callback body, so an unwind cannot cross back through the
//!   `extern "C"` boundary here.

use crate::ffi::{self, OfxStatus};
use crate::image_effect::{self, OfxImage};
use crate::instance::{self, PluginInstance};
use crate::param::{self, ParamDescriptor, ParamValue};
use crate::property::{self, PropertySet};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, info, warn};

// ── Error type ───────────────────────────────────────────────────────

#[derive(Debug)]
pub enum PluginError {
    LoadFailed(String),
    MissingSymbol(&'static str),
    NotFound,
    ActionFailed(String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoadFailed(e) => write!(f, "failed to load: {e}"),
            Self::MissingSymbol(s) => write!(f, "missing symbol: {s}"),
            Self::NotFound => write!(f, "plugin not found"),
            Self::ActionFailed(e) => write!(f, "action failed: {e}"),
        }
    }
}

// ── Enums ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OfxCategory {
    #[default]
    Filter,
    Color,
    Transition,
    Generator,
    Other,
}

// OfxValue is defined in klipp-core and re-exported here for backward compatibility.
pub use klipp_core::OfxValue;

/// Extension trait to convert OfxValue to ParamValue.
pub trait OfxValueExt {
    fn to_param_value(&self) -> ParamValue;
}

impl OfxValueExt for OfxValue {
    fn to_param_value(&self) -> ParamValue {
        match self {
            Self::Double(v) => ParamValue::Double(*v),
            Self::Double2(a, b) => ParamValue::Double2(*a, *b),
            Self::Double3(a, b, c) => ParamValue::Double3(*a, *b, *c),
            Self::Int(v) => ParamValue::Int(*v),
            Self::Int2(a, b) => ParamValue::Int2(*a, *b),
            Self::Int3(a, b, c) => ParamValue::Int3(*a, *b, *c),
            Self::Bool(v) => ParamValue::Bool(*v),
            Self::Choice(v) => ParamValue::Choice(*v),
            Self::String(s) => ParamValue::String(s.clone()),
            Self::Rgba(r, g, b, a) => ParamValue::Rgba(*r, *g, *b, *a),
            Self::Rgb(r, g, b) => ParamValue::Rgb(*r, *g, *b),
        }
    }
}

// ── Plugin info (safe for UI) ────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct OfxPluginInfo {
    pub name: String,
    pub version: (u32, u32),
    pub category: OfxCategory,
    pub parameters: Vec<ParamDescriptor>,
    /// OFX grouping string (e.g. "Filter/Blur", "Color/Adjust")
    pub grouping: String,
    /// Contexts this plugin supports (Filter, Transition, Generator).
    pub supported_contexts: Vec<OfxCategory>,
    /// Human-readable description from the plugin's Describe action.
    pub description: String,
    /// Plugin advertised support for `kOfxImageEffectPropSupportsOpenGLRender`
    /// during Describe. Drives the on-thumbnail GPU/CPU badge in the
    /// effects browser. Mirrors `OfxPlugin::supports_opengl`.
    pub supports_opengl: bool,
    /// It asks for frames at other times (`OfxImageEffectPropTemporalClipAccess`),
    /// as it declared: mirrors `OfxPlugin::temporal`, which the render
    /// path reads.
    pub temporal: bool,
}

// ── Loaded plugin ────────────────────────────────────────────────────

pub struct OfxPlugin {
    pub name: String,
    pub version: (u32, u32),
    pub param_descriptors: Vec<ParamDescriptor>,
    pub category: OfxCategory,
    pub description: String,
    pub grouping: String,
    /// The premultiplication string this plugin uses (detected during Describe).
    /// Standard OFX uses "OfxImageUnPreMultiplied", Natron builds use "OfxImageAlphaUnPremultiplied".
    pub premult_string: String,
    /// Contexts this plugin supports.
    pub supported_contexts: Vec<OfxCategory>,
    /// Whether this plugin requires float pixel depth (e.g. CImg plugins).
    pub needs_float: bool,
    /// Whether this plugin supports CUDA rendering.
    pub supports_cuda: bool,
    /// Whether this plugin supports OpenCL rendering.
    pub supports_opencl: bool,
    /// Whether this plugin supports OpenGL rendering.
    pub supports_opengl: bool,
    /// It asks for frames at other times (`OfxImageEffectPropTemporalClipAccess`).
    pub temporal: bool,
    /// Its render thread safety is "unsafe": no two of its renders at once,
    /// in any host.
    pub unsafe_threads: bool,
    /// It needs its frames in order (`OfxImageEffectInstancePropSequentialRender`):
    /// it keeps history, so nothing may keep its output by frame.
    pub sequential_render: bool,
    /// Raw plugin pointer — valid for the lifetime of `_lib` when the plugin
    /// comes from a dynamically-loaded `.ofx` bundle. `None` for plugins
    /// registered directly via `register_raw_plugin` (test injection), where
    /// the raw pointer owns itself via `Box::into_raw`.
    raw: *const ffi::OfxPluginRaw,
    _lib: Option<Arc<libloading::Library>>,
    /// Whether Load+Describe have been called.
    initialized: bool,
    /// Whether this host holds one of the plug-in's loads ([`LOADS`]): its
    /// drop gives it back, and the last one given back unloads it.
    loaded: bool,
    /// Descriptor-phase effect handle (used during Describe).
    descriptor_handle: ffi::OfxImageEffectHandle,
    /// Descriptor-phase param set handle.
    descriptor_param_set: ffi::OfxParamSetHandle,
}

// Safety: raw pointer is kept alive by _lib Arc. Not dereferenced across threads.
unsafe impl Send for OfxPlugin {}

impl std::fmt::Debug for OfxPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfxPlugin")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("category", &self.category)
            .field("initialized", &self.initialized)
            .finish()
    }
}

impl OfxPlugin {
    pub fn info(&self) -> OfxPluginInfo {
        OfxPluginInfo {
            name: self.name.clone(),
            version: self.version,
            category: self.category,
            parameters: self.param_descriptors.clone(),
            grouping: self.grouping.clone(),
            supported_contexts: self.supported_contexts.clone(),
            description: self.description.clone(),
            supports_opengl: self.supports_opengl,
            temporal: self.temporal,
        }
    }
}

impl Drop for OfxPlugin {
    fn drop(&mut self) {
        // Mirror the OFX plugin lifecycle: kOfxActionLoad ran during
        // initialise_plugin, so the matching kOfxActionUnload runs
        // here. Plugins use Unload to release persistent resources
        // (CUDA contexts, license checks, GL caches); skipping it
        // leaks across host tear-down.
        //
        // raw is only null if construction failed before the field
        // was set; in normal flow it's either a Box::into_raw'd
        // pointer (test injection) or the dylib's plugin entry.
        if !self.raw.is_null() {
            let raw = unsafe { &*self.raw };
            // Unloaded only by the last host holding it: the library is the
            // process's, and another host's instances may still be running.
            if self.loaded && give_back_load(self.raw) {
                if let Some(main_entry) = raw.main_entry {
                    let _ = unsafe {
                        main_entry(
                            c"OfxActionUnload".as_ptr(),
                            std::ptr::null(),
                            std::ptr::null(),
                            std::ptr::null_mut(),
                        )
                    };
                }
            }
            // Test-injected plugins took ownership of the raw struct
            // via Box::into_raw at registration time; reclaim the
            // Box so it deallocates rather than leaking. Real plugins
            // (Some(_lib)) own their raw pointer through the dylib's
            // static storage — Drop on the Arc<Library> below
            // releases everything.
            if self._lib.is_none() {
                let _ = unsafe { Box::from_raw(self.raw as *mut ffi::OfxPluginRaw) };
            }
        }
    }
}

// ── The process's host ───────────────────────────────────────────────

/// The C host struct every plug-in is given by setHost, and its property
/// set, as addresses: made once and never freed. A plug-in library is the
/// process's and keeps one host pointer, from the last setHost, so a host
/// struct of each [`OfxHostRuntime`]'s own dangled when that host dropped
/// while another's instances still called through it.
static PROCESS_HOST: std::sync::LazyLock<(usize, usize)> = std::sync::LazyLock::new(|| {
    let mut host_props = PropertySet::default();
    property::populate_host_properties(&mut host_props);
    // GPU rendering: all contexts are lazily initialized at first render
    // to avoid corrupting driver state or conflicting with eframe's contexts.
    // We advertise capabilities upfront so plugins can declare support.
    host_props.set_string(crate::constants::kOfxImageEffectPropCudaRenderSupported, 0, "true");
    host_props.set_string(crate::constants::kOfxImageEffectPropCudaStreamSupported, 0, "true");
    host_props.set_string(crate::constants::kOfxImageEffectPropOpenCLRenderSupported, 0, "true");
    host_props.set_string(crate::constants::kOfxImageEffectPropOpenGLRenderSupported, 0, "true");
    let props = property::register_propset(host_props);
    let host: &'static ffi::OfxHostC = Box::leak(Box::new(ffi::OfxHostC { host: props, fetch_suite: fetch_suite_fn }));
    (host as *const ffi::OfxHostC as usize, props as usize)
});

fn process_host() -> *const ffi::OfxHostC {
    PROCESS_HOST.0 as *const ffi::OfxHostC
}

/// The hosts holding each plug-in loaded, by its entry point's address:
/// the first to load it sends Load, and the last to give it back Unload.
static LOADS: std::sync::LazyLock<std::sync::Mutex<HashMap<usize, usize>>> = std::sync::LazyLock::new(Default::default);

/// One more host holds `raw` loaded; whether it is the first.
fn take_load(raw: *const ffi::OfxPluginRaw) -> bool {
    let mut loads = LOADS.lock().unwrap_or_else(|e| e.into_inner());
    let n = loads.entry(raw as usize).or_insert(0);
    *n += 1;
    *n == 1
}

/// A host gives `raw` back; whether it was the last.
fn give_back_load(raw: *const ffi::OfxPluginRaw) -> bool {
    let mut loads = LOADS.lock().unwrap_or_else(|e| e.into_inner());
    match loads.get_mut(&(raw as usize)) {
        Some(n) if *n > 1 => {
            *n -= 1;
            false
        }
        _ => {
            loads.remove(&(raw as usize));
            true
        }
    }
}

// ── fetchSuite dispatcher ────────────────────────────────────────────

unsafe extern "C" fn fetch_suite_fn(
    _host_props: ffi::OfxPropertySetHandle,
    suite_name: *const c_char,
    suite_version: c_int,
) -> *const c_void {
    let name = CStr::from_ptr(suite_name).to_string_lossy();

    match name.as_ref() {
        "OfxPropertySuite" if suite_version == 1 => {
            &property::PROPERTY_SUITE_V1 as *const _ as *const c_void
        }
        "OfxParameterSuite" if suite_version == 1 => {
            &param::PARAM_SUITE_V1 as *const _ as *const c_void
        }
        "OfxImageEffectSuite" if suite_version == 1 => {
            &image_effect::IMAGE_EFFECT_SUITE_V1 as *const _ as *const c_void
        }
        "OfxMemorySuite" if suite_version == 1 => {
            &crate::memory::MEMORY_SUITE_V1 as *const _ as *const c_void
        }
        "OfxMultiThreadSuite" if suite_version == 1 => {
            &crate::thread::MULTITHREAD_SUITE_V1 as *const _ as *const c_void
        }
        "OfxMessageSuite" if suite_version >= 1 => {
            &crate::message::MESSAGE_SUITE_V1 as *const _ as *const c_void
        }
        "OfxProgressSuite" if suite_version >= 1 => {
            &crate::message::PROGRESS_SUITE_V1 as *const _ as *const c_void
        }
        "OfxImageEffectOpenGLRenderSuite" if suite_version == 1 => {
            &crate::gpu_opengl::OPENGL_RENDER_SUITE_V1 as *const _ as *const c_void
        }
        _ => {
            debug!("[OFX] Unknown suite: {} v{}", name, suite_version);
            std::ptr::null()
        }
    }
}

// ── Host Runtime ─────────────────────────────────────────────────────

/// OFX plugin host runtime — discovers, loads, initializes, and renders .ofx.bundle plugins.
pub struct OfxHostRuntime {
    pub(crate) plugins: Vec<OfxPlugin>,
    search_paths: Vec<PathBuf>,
    /// Registered host property set handle.
    host_propset_handle: ffi::OfxPropertySetHandle,
    /// Maps plugin_name → instance handle (key in INSTANCE_STORE).
    instances: HashMap<String, usize>,
    /// CUDA runtime (if available).
    cuda: Option<crate::gpu_cuda::CudaRuntime>,
    /// OpenCL runtime (if available — AMD/Intel/NVIDIA).
    opencl: Option<crate::gpu_opencl::OpenClRuntime>,
    /// OpenGL runtime (if available — headless EGL context).
    ///
    /// Wrapped in `Arc` so the runtime can be shared with the global
    /// `GL_RUNTIME_STORE` used by the OFX GL suite callbacks without any
    /// lifetime transmutation. A panic during render that drops this
    /// host field can no longer leave the suite store pointing at freed
    /// memory — the Arc keeps the runtime alive until cleared.
    opengl: Option<std::sync::Arc<crate::gpu_opengl::OpenGlRuntime>>,
    /// Current render context — frame dimensions for temporal access.
    render_width: u32,
    render_height: u32,
}

// Safety: All handle fields are opaque keys into global Mutex-protected stores.
unsafe impl Send for OfxHostRuntime {}

impl Default for OfxHostRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl OfxHostRuntime {
    pub fn new() -> Self {
        let cuda: Option<crate::gpu_cuda::CudaRuntime> = None;
        let opencl: Option<crate::gpu_opencl::OpenClRuntime> = None;
        let opengl: Option<std::sync::Arc<crate::gpu_opengl::OpenGlRuntime>> = None;

        let host_propset_handle = PROCESS_HOST.1 as ffi::OfxPropertySetHandle;

        let mut search_paths = vec![
            PathBuf::from("/usr/OFX/Plugins"),
            PathBuf::from("/usr/local/OFX/Plugins"),
        ];
        if let Some(home) = dirs::home_dir() {
            search_paths.push(home.join(".OFX/Plugins"));
        }

        Self {
            plugins: Vec::new(),
            search_paths,
            host_propset_handle,
            instances: HashMap::new(),
            cuda,
            opencl,
            opengl,
            render_width: 0,
            render_height: 0,
        }
    }

    // ── Discovery ────────────────────────────────────────────────────

    pub fn discover_plugins(&mut self) -> usize {
        let mut paths = self.search_paths.clone();
        if let Ok(env_path) = std::env::var("OFX_PLUGIN_PATH") {
            for p in env_path.split(';') {
                paths.insert(0, PathBuf::from(p));
            }
        }
        for p in extra_plugin_dirs().lock().map(|d| d.clone()).unwrap_or_default() {
            paths.insert(0, p);
        }

        self.discover_plugins_in(&paths)
    }

    /// Folders every later [`Self::discover_plugins`] in this process
    /// searches first, as `OFX_PLUGIN_PATH` would: how a test reaches its
    /// fixture plug-ins through the engine's and export's own discovery,
    /// without changing the process's environment.
    pub fn add_plugin_dirs(dirs: &[PathBuf]) {
        if let Ok(mut d) = extra_plugin_dirs().lock() {
            for dir in dirs {
                if !d.contains(dir) {
                    d.push(dir.clone());
                }
            }
        }
    }

    /// Discover using only these directories, without consulting environment or
    /// system paths. Uses the same loader and initialization as normal discovery.
    pub fn discover_plugins_in(&mut self, paths: &[PathBuf]) -> usize {
        let mut count = 0;
        for search_path in paths {
            let Ok(entries) = std::fs::read_dir(search_path) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if path.is_dir() && name.ends_with(".ofx.bundle") {
                    let stem = name.trim_end_matches(".ofx.bundle");
                    let binary = path
                        .join("Contents")
                        .join("Linux-x86-64")
                        .join(format!("{stem}.ofx"));
                    if binary.exists() {
                        match self.load_plugin(&binary) {
                            Ok(loaded) => {
                                let start_idx = self.plugins.len();
                                for p in loaded {
                                    info!(
                                        "OFX: loaded '{}' v{}.{}",
                                        p.name, p.version.0, p.version.1
                                    );
                                    self.plugins.push(p);
                                    count += 1;
                                }
                                // Initialize each newly loaded plugin
                                for idx in start_idx..self.plugins.len() {
                                    if let Err(e) = self.initialise_plugin(idx) {
                                        warn!("OFX: init failed for {}: {e}", self.plugins[idx].name);
                                    }
                                }
                            }
                            Err(e) => warn!("OFX: failed to load {}: {e}", binary.display()),
                        }
                    }
                }
            }
        }
        info!("OFX: discovered {count} plugin(s) total");
        count
    }

    fn load_plugin(&self, binary: &Path) -> Result<Vec<OfxPlugin>, PluginError> {
        let lib = Arc::new(
            klipp_load_lock::while_loading(|| unsafe { libloading::Library::new(binary) })
                .map_err(|e| PluginError::LoadFailed(e.to_string()))?,
        );

        let get_n: libloading::Symbol<ffi::OfxGetNumberOfPluginsFn> = unsafe {
            lib.get(b"OfxGetNumberOfPlugins\0")
        }
        .map_err(|_| PluginError::MissingSymbol("OfxGetNumberOfPlugins"))?;

        let get_plugin: libloading::Symbol<ffi::OfxGetPluginFn> =
            unsafe { lib.get(b"OfxGetPlugin\0") }
                .map_err(|_| PluginError::MissingSymbol("OfxGetPlugin"))?;

        let n = unsafe { get_n() };
        let mut result = Vec::new();

        for i in 0..n {
            let raw = unsafe { get_plugin(i) };
            if raw.is_null() {
                continue;
            }
            let raw_ref = unsafe { &*raw };

            let api = unsafe { CStr::from_ptr(raw_ref.plugin_api) }.to_string_lossy();
            if api != "OfxImageEffectPluginAPI" {
                continue;
            }

            let name = unsafe { CStr::from_ptr(raw_ref.plugin_identifier) }
                .to_string_lossy()
                .to_string();

            result.push(OfxPlugin {
                name,
                version: (raw_ref.plugin_version_major, raw_ref.plugin_version_minor),
                param_descriptors: Vec::new(),
                category: OfxCategory::Filter,
                description: String::new(),
                grouping: String::new(),
                premult_string: crate::constants::kOfxImageAlphaUnPremultiplied.to_string(),
                supported_contexts: Vec::new(),
                needs_float: false,
                supports_cuda: false,
                supports_opencl: false,
                supports_opengl: false,
                temporal: false,
                unsafe_threads: false,
                sequential_render: false,
                raw,
                _lib: Some(lib.clone()),
                initialized: false,
                loaded: false,
                descriptor_handle: std::ptr::null_mut(),
                descriptor_param_set: std::ptr::null_mut(),
            });
        }

        Ok(result)
    }

    /// Register a raw plugin directly (for test injection, no dlopen).
    pub fn register_raw_plugin(&mut self, raw: ffi::OfxPluginRaw) -> usize {
        // Leak the raw struct so the pointer remains valid
        let raw_ptr = Box::into_raw(Box::new(raw));
        let raw_ref = unsafe { &*raw_ptr };

        let name = unsafe { CStr::from_ptr(raw_ref.plugin_identifier) }
            .to_string_lossy()
            .to_string();

        // Test-injected plugins own their raw pointer through `Box::into_raw`
        // above, so no backing library is required. Real plugins coming from
        // `load_plugin` carry a `Some(Arc<Library>)`; this path stays `None`.
        let idx = self.plugins.len();
        self.plugins.push(OfxPlugin {
            name,
            version: (raw_ref.plugin_version_major, raw_ref.plugin_version_minor),
            param_descriptors: Vec::new(),
            category: OfxCategory::Filter,
            description: String::new(),
            grouping: String::new(),
            premult_string: crate::constants::kOfxImageAlphaUnPremultiplied.to_string(),
            supported_contexts: Vec::new(),
            needs_float: false,
            supports_cuda: false,
            supports_opencl: false,
            supports_opengl: false,
            temporal: false,
            unsafe_threads: false,
            sequential_render: false,
            raw: raw_ptr,
            _lib: None,
            initialized: false,
            loaded: false,
            descriptor_handle: std::ptr::null_mut(),
            descriptor_param_set: std::ptr::null_mut(),
        });
        idx
    }

    // ── Plugin lifecycle ─────────────────────────────────────────────

    /// Initialize a plugin: setHost → Load → Describe → DescribeInContext.
    pub fn initialise_plugin(&mut self, plugin_idx: usize) -> Result<(), PluginError> {
        let plugin = &self.plugins[plugin_idx];
        let raw = unsafe { &*plugin.raw };

        // 1. setHost — the process's one OfxHostC: a plug-in keeps the
        // pointer from its last setHost, whichever host gave it, so it
        // must outlive every host (a host's own struct dangled once that
        // host dropped and another's instances called through it).
        if let Some(set_host) = raw.set_host {
            unsafe {
                set_host(process_host() as *const c_void);
            }
        }

        // 2. kOfxActionLoad, by the first host to load it only: Load and
        // Unload pair once for the library, not once a host.
        if !plugin.loaded && take_load(plugin.raw) {
            let status = self.call_action_raw(
                plugin_idx,
                b"OfxActionLoad\0",
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if status != ffi::K_OFX_STAT_OK && status != ffi::K_OFX_STAT_REPLY_DEFAULT {
                give_back_load(self.plugins[plugin_idx].raw);
                return Err(PluginError::ActionFailed(format!(
                    "kOfxActionLoad returned {status}"
                )));
            }
        }
        self.plugins[plugin_idx].loaded = true;

        // 3. Create descriptor effect handle + param set for Describe
        let mut desc_inst = PluginInstance::new(plugin_idx);
        desc_inst.add_clip("Source");
        desc_inst.add_clip("Output");
        for clip in desc_inst.clips.values() {
            instance::register_clip_mapping(clip.handle, desc_inst.handle, &clip.name);
        }
        let desc_handle = desc_inst.handle;
        let desc_param_set = desc_inst.param_set_handle;

        // Set context on the descriptor so plugins can query it
        property::with_propset(desc_inst.props_handle, |ps| {
            ps.set_string("OfxImageEffectPropContext", 0, crate::constants::kOfxImageEffectContextFilter);
        });

        instance::register_instance(desc_inst);

        // 4. kOfxActionDescribe
        let status = self.call_action_raw(
            plugin_idx,
            b"OfxActionDescribe\0",
            desc_handle,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if status != ffi::K_OFX_STAT_OK && status != ffi::K_OFX_STAT_REPLY_DEFAULT {
            warn!(
                "kOfxActionDescribe returned {status} for {}",
                self.plugins[plugin_idx].name
            );
        }

        // 5. Read which contexts the plugin supports from its descriptor
        let plugin_contexts = instance::with_instance(desc_handle, |inst| {
            property::with_propset(inst.props_handle, |ps| {
                let dim = ps.dimension("OfxImageEffectPropSupportedContexts");
                let mut contexts = Vec::new();
                for i in 0..dim as usize {
                    if let Some(ctx) = ps.get_string_lossy("OfxImageEffectPropSupportedContexts", i) {
                        contexts.push(ctx);
                    }
                }
                contexts
            })
        }).flatten().unwrap_or_default();

        // Parse context strings into OfxCategory
        let supported_contexts: Vec<OfxCategory> = if plugin_contexts.is_empty() {
            vec![OfxCategory::Filter] // default to Filter if plugin didn't declare
        } else {
            plugin_contexts.iter().filter_map(|ctx| match ctx.as_str() {
                "OfxImageEffectContextFilter" => Some(OfxCategory::Filter),
                "OfxImageEffectContextTransition" => Some(OfxCategory::Transition),
                "OfxImageEffectContextGenerator" => Some(OfxCategory::Generator),
                "OfxImageEffectContextGeneral" => Some(OfxCategory::Other),
                _ => None,
            }).collect()
        };

        // 6. kOfxImageEffectActionDescribeInContext — call for each supported context
        let context_strings: Vec<&str> = supported_contexts.iter().map(|c| match c {
            OfxCategory::Filter => crate::constants::kOfxImageEffectContextFilter,
            OfxCategory::Transition => crate::constants::kOfxImageEffectContextTransition,
            OfxCategory::Generator => crate::constants::kOfxImageEffectContextGenerator,
            _ => crate::constants::kOfxImageEffectContextFilter,
        }).collect();

        for ctx_str in &context_strings {
            let mut dic_args = PropertySet::default();
            dic_args.set_string("OfxImageEffectPropContext", 0, ctx_str);
            let dic_args_handle = property::register_propset(dic_args);

            let status = self.call_action_raw(
                plugin_idx,
                b"OfxImageEffectActionDescribeInContext\0",
                desc_handle,
                dic_args_handle,
                std::ptr::null_mut(),
            );
            if status != ffi::K_OFX_STAT_OK && status != ffi::K_OFX_STAT_REPLY_DEFAULT {
                debug!(
                    "DescribeInContext({}) returned {status} for {}",
                    ctx_str, self.plugins[plugin_idx].name
                );
            }
            property::remove_propset(dic_args_handle);
        }

        // 6. Read back param descriptors (after both Describe and DescribeInContext)
        // Deduplicate by name — multi-context DescribeInContext can define the same params twice
        let mut descriptors = param::read_param_descriptors(desc_param_set);
        {
            let mut seen = std::collections::HashSet::new();
            descriptors.retain(|d| seen.insert(d.name.clone()));
        }
        info!(
            "OFX: '{}' declared {} parameter(s)",
            self.plugins[plugin_idx].name,
            descriptors.len()
        );

        // Read grouping from descriptor properties
        let grouping = instance::with_instance(desc_handle, |inst| {
            property::with_propset(inst.props_handle, |ps| {
                ps.get_string_lossy("OfxImageEffectPluginPropGrouping", 0)
            })
        })
        .flatten()
        .flatten()
        .unwrap_or_default();

        // Detect premultiplication string variant by reading what the plugin wrote
        // on its Output clip. Natron-built plugins use "OfxImageAlphaUnPremultiplied",
        // standard OFX uses "OfxImageUnPreMultiplied".
        let premult_str = instance::with_instance(desc_handle, |inst| {
            inst.clips.get("Output").and_then(|clip| {
                property::with_propset(clip.props_handle, |ps| {
                    ps.get_string_lossy("OfxImageEffectPropPreMultiplication", 0)
                })
            }).flatten()
        }).flatten().unwrap_or_else(|| crate::constants::kOfxImageAlphaUnPremultiplied.to_string());

        // Detect GPU rendering capabilities from plugin descriptor
        let (supports_cuda, supports_opencl, supports_opengl) = instance::with_instance(desc_handle, |inst| {
            property::with_propset(inst.props_handle, |ps| {
                let cuda = ps.get_string_lossy(crate::constants::kOfxImageEffectPropCudaRenderSupported, 0)
                    .map(|s| s == "true" || s == "needed")
                    .unwrap_or(false);
                let opencl = ps.get_string_lossy(crate::constants::kOfxImageEffectPropOpenCLRenderSupported, 0)
                    .map(|s| s == "true" || s == "needed")
                    .unwrap_or(false);
                let gl = ps.get_string_lossy(crate::constants::kOfxImageEffectPropOpenGLRenderSupported, 0)
                    .map(|s| s == "true" || s == "needed")
                    .unwrap_or(false);
                (cuda, opencl, gl)
            })
        }).flatten().unwrap_or((false, false, false));

        if supports_cuda {
            info!("OFX: '{}' supports CUDA rendering", self.plugins[plugin_idx].name);
        }
        if supports_opencl {
            info!("OFX: '{}' supports OpenCL rendering", self.plugins[plugin_idx].name);
        }
        if supports_opengl {
            info!("OFX: '{}' supports OpenGL rendering", self.plugins[plugin_idx].name);
        }

        // Detect if plugin requires float pixel depth (e.g. CImg plugins)
        let needs_float = instance::with_instance(desc_handle, |inst| {
            property::with_propset(inst.props_handle, |ps| {
                let dim = ps.dimension("OfxImageEffectPropSupportedPixelDepths");
                if dim == 0 { return false; }
                let mut has_byte = false;
                let mut has_float = false;
                for i in 0..dim as usize {
                    if let Some(d) = ps.get_string_lossy("OfxImageEffectPropSupportedPixelDepths", i) {
                        if d == crate::constants::kOfxBitDepthByte { has_byte = true; }
                        if d == crate::constants::kOfxBitDepthFloat { has_float = true; }
                    }
                }
                has_float && !has_byte
            })
        }).flatten().unwrap_or(false);
        if needs_float {
            info!("OFX: '{}' requires float pixel depth", self.plugins[plugin_idx].name);
        }

        // What it says of time and threads: a temporal plug-in may ask for
        // other frames, an unsafe one never renders twice at once, and a
        // sequential one keeps history between frames.
        let (temporal, unsafe_threads, sequential_render) = instance::with_instance(desc_handle, |inst| {
            property::with_propset(inst.props_handle, |ps| {
                let temporal = ps.get_int("OfxImageEffectPropTemporalClipAccess", 0).unwrap_or(0) != 0;
                let unsafe_threads = ps.get_string_lossy("OfxImageEffectPluginRenderThreadSafety", 0).is_some_and(|s| s == "OfxImageEffectRenderUnsafe");
                let sequential = ps.get_int("OfxImageEffectInstancePropSequentialRender", 0).unwrap_or(0) == 1;
                (temporal, unsafe_threads, sequential)
            })
        }).flatten().unwrap_or((false, false, false));

        let plugin = &mut self.plugins[plugin_idx];
        plugin.temporal = temporal;
        plugin.unsafe_threads = unsafe_threads;
        plugin.sequential_render = sequential_render;
        plugin.param_descriptors = descriptors;
        plugin.grouping = grouping;
        plugin.supported_contexts = supported_contexts;
        plugin.needs_float = needs_float;
        plugin.supports_cuda = supports_cuda;
        plugin.supports_opencl = supports_opencl;
        plugin.supports_opengl = supports_opengl;
        if !premult_str.is_empty() {
            plugin.premult_string = premult_str;
        }
        plugin.initialized = true;
        plugin.descriptor_handle = desc_handle;
        plugin.descriptor_param_set = desc_param_set;

        Ok(())
    }

    /// Create a render instance for a plugin effect.
    fn create_instance(
        &mut self,
        plugin_name: &str,
        plugin_idx: usize,
        context: OfxCategory,
        shared: bool,
    ) -> Result<ffi::OfxImageEffectHandle, PluginError> {
        let mut inst = PluginInstance::new(plugin_idx);
        inst.premult_string = self.plugins[plugin_idx].premult_string.clone();
        if self.plugins[plugin_idx].needs_float {
            inst.bit_depth = crate::constants::kOfxBitDepthFloat.to_string();
        }

        // Set up clips based on context
        match context {
            OfxCategory::Filter | OfxCategory::Color | OfxCategory::Other => {
                inst.add_clip("Source");
                inst.add_clip("Output");
            }
            OfxCategory::Transition => {
                inst.add_clip("SourceFrom");
                inst.add_clip("SourceTo");
                inst.add_clip("Output");
            }
            OfxCategory::Generator => {
                inst.add_clip("Output");
            }
        }

        let context_str = match context {
            OfxCategory::Filter | OfxCategory::Color | OfxCategory::Other => crate::constants::kOfxImageEffectContextFilter,
            OfxCategory::Transition => crate::constants::kOfxImageEffectContextTransition,
            OfxCategory::Generator => crate::constants::kOfxImageEffectContextGenerator,
        };

        // Set up instance properties — must include project properties because
        // the OFX C++ support library reads them in the ImageEffect constructor.
        property::with_propset(inst.props_handle, |ps| {
            ps.set_string("OfxImageEffectPropContext", 0, context_str);
            let depth = if self.plugins[plugin_idx].needs_float { crate::constants::kOfxBitDepthFloat } else { crate::constants::kOfxBitDepthByte };
            ps.set_string("OfxImageEffectPropPixelDepth", 0, depth);
            ps.set_string("OfxImageEffectPropComponents", 0, crate::constants::kOfxImageComponentRGBA);
            // Project properties — needed by ImageEffect constructor
            ps.set_double("OfxImageEffectPropProjectSize", 0, 1920.0);
            ps.set_double("OfxImageEffectPropProjectSize", 1, 1080.0);
            ps.set_double("OfxImageEffectPropProjectOffset", 0, 0.0);
            ps.set_double("OfxImageEffectPropProjectOffset", 1, 0.0);
            ps.set_double("OfxImageEffectPropProjectExtent", 0, 1920.0);
            ps.set_double("OfxImageEffectPropProjectExtent", 1, 1080.0);
            ps.set_double("OfxImageEffectPropProjectPixelAspectRatio", 0, 1.0);
            ps.set_double("OfxImageEffectPropFrameRate", 0, 24.0);
            ps.set_double("OfxImageEffectPropFrameRange", 0, 0.0);
            ps.set_double("OfxImageEffectPropFrameRange", 1, 1000.0);
            ps.set_int("OfxImageEffectInstancePropSequentialRender", 0, 0);
            ps.set_int("OfxImageEffectPropSupportsMultiResolution", 0, 0);
            ps.set_int("OfxImageEffectPropSupportsTiles", 0, 0);
            ps.set_double("OfxImageEffectPropRenderScale", 0, 1.0);
            ps.set_double("OfxImageEffectPropRenderScale", 1, 1.0);
        });

        // Determine which clips are "connected" based on context
        let connected_clips: Vec<String> = match context {
            OfxCategory::Filter | OfxCategory::Color | OfxCategory::Other => {
                vec!["Source".to_string(), "Output".to_string()]
            }
            OfxCategory::Transition => {
                vec!["SourceFrom".to_string(), "SourceTo".to_string(), "Output".to_string()]
            }
            OfxCategory::Generator => {
                vec!["Output".to_string()]
            }
        };

        // Set up clip properties — constructor reads these via fetchClip
        let premult = self.plugins[plugin_idx].premult_string.clone();
        for (clip_name, clip) in &inst.clips {
            instance::register_clip_mapping(clip.handle, inst.handle, clip_name);
            let is_connected = connected_clips.contains(clip_name);
            let is_required = connected_clips.contains(clip_name);
            let depth = if self.plugins[plugin_idx].needs_float { crate::constants::kOfxBitDepthFloat } else { crate::constants::kOfxBitDepthByte };
            property::with_propset(clip.props_handle, |ps| {
                ps.set_string("OfxImageEffectPropPixelDepth", 0, depth);
                ps.set_string("OfxImageEffectPropComponents", 0, crate::constants::kOfxImageComponentRGBA);
                ps.set_string("OfxImageEffectPropSupportedComponents", 0, crate::constants::kOfxImageComponentRGBA);
                ps.set_string("OfxImageClipPropSupportedComponents", 0, crate::constants::kOfxImageComponentRGBA);
                ps.set_string("OfxImageEffectPropSupportedPixelDepths", 0, depth);
                ps.set_int("OfxImageClipPropConnected", 0, if is_connected { 1 } else { 0 });
                ps.set_double("OfxImageEffectPropFrameRate", 0, 24.0);
                ps.set_double("OfxImageEffectPropPixelAspectRatio", 0, 1.0);
                ps.set_double("OfxImageEffectPropFrameRange", 0, 0.0);
                ps.set_double("OfxImageEffectPropFrameRange", 1, 1000.0);
                ps.set_double("OfxImageEffectPropUnmappedFrameRange", 0, 0.0);
                ps.set_double("OfxImageEffectPropUnmappedFrameRange", 1, 1000.0);
                ps.set_string("OfxImageClipPropFieldOrder", 0, crate::constants::kOfxFieldNone);
                ps.set_string("OfxImageEffectPropPreMultiplication", 0, &premult);
                ps.set_int("OfxImageClipPropContinuousSamples", 0, 0);
                ps.set_int("OfxImageClipPropIsMask", 0, if clip_name == "Mask" { 1 } else { 0 });
                ps.set_int("OfxImageClipPropOptional", 0, if is_required { 0 } else { 1 });
                ps.set_string("OfxImageClipPropUnmappedComponents", 0, crate::constants::kOfxImageComponentRGBA);
                ps.set_string("OfxImageClipPropUnmappedPixelDepth", 0, depth);
            });
        }

        // Copy param descriptors from the plugin's descriptor phase into the instance param set.
        // Each param needs a property set with type/name/label properties so the
        // OFX C++ support library's validateParameterProperties doesn't throw.
        let plugin = &self.plugins[plugin_idx];
        for desc in &plugin.param_descriptors {
            param::with_param_set(inst.param_set_handle, |pset| {
                let mut props = PropertySet::default();
                props.set_string("OfxParamPropType", 0, param_type_to_ofx(&desc.param_type));
                props.set_string("OfxPropName", 0, &desc.name);
                props.set_string("OfxPropLabel", 0, &desc.label);
                if !desc.hint.is_empty() {
                    props.set_string("OfxParamPropHint", 0, &desc.hint);
                }
                props.set_int("OfxParamPropSecret", 0, if desc.secret { 1 } else { 0 });
                props.set_int("OfxParamPropEnabled", 0, if desc.enabled { 1 } else { 0 });
                // Set default values
                set_param_default_props(&mut props, &desc.param_type, &desc.default);
                let props_handle = property::register_propset(props);
                let handle_key = props_handle as usize;
                let pi = param::ParamInstance {
                    descriptor: desc.clone(),
                    value: desc.default.clone(),
                    props_handle,
                    keyframes: Vec::new(),
                };
                {
                    let mut store = crate::param::PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
                    store.insert(handle_key, pi);
                }
                pset.param_names.push(desc.name.clone());
                pset.param_handles.insert(desc.name.clone(), handle_key);
            });
        }

        let handle = inst.handle;
        instance::register_instance(inst);

        // Verify pre-CreateInstance state
        let (ctx_ok, param_count) = instance::with_instance(handle, |inst| {
            let ctx = property::with_propset(inst.props_handle, |ps| {
                ps.get_string_lossy("OfxImageEffectPropContext", 0)
            }).flatten();
            let count = param::with_param_set(inst.param_set_handle, |pset| {
                pset.param_names.len()
            }).unwrap_or(0);
            (ctx, count)
        }).unwrap_or((None, 0));
        debug!(
            "OFX pre-CreateInstance '{}': handle={:?} context={:?} params={}",
            plugin_name, handle, ctx_ok, param_count
        );

        // Call kOfxActionCreateInstance
        let status = self.call_action_raw(
            plugin_idx,
            b"OfxActionCreateInstance\0",
            handle,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if status != ffi::K_OFX_STAT_OK && status != ffi::K_OFX_STAT_REPLY_DEFAULT {
            warn!("kOfxActionCreateInstance returned {status} for {plugin_name}");
            // Check if plugin managed to set InstanceData despite error status
            let has_data = instance::with_instance(handle, |inst| {
                property::with_propset(inst.props_handle, |ps| {
                    ps.get_pointer("OfxPropInstanceData", 0)
                        .is_some_and(|p| !p.is_null())
                })
            }).flatten().unwrap_or(false);
            if !has_data {
                // Instance creation truly failed — plugin has no data
                instance::remove_instance(handle);
                return Err(PluginError::ActionFailed(format!(
                    "kOfxActionCreateInstance returned {status}"
                )));
            }
        }

        instance::with_instance(handle, |inst| inst.created = true);

        // The one instance every clip's entry shares, by name; a node's own
        // (`OfxPluginInstance`) is its slot's alone.
        if shared {
            self.instances.insert(plugin_name.to_string(), handle as usize);
        }
        info!("OFX: created instance for '{plugin_name}'");

        Ok(handle)
    }

    /// Get or create an instance for a plugin effect.
    fn get_or_create_instance(
        &mut self,
        plugin_name: &str,
        context: OfxCategory,
    ) -> Result<ffi::OfxImageEffectHandle, PluginError> {
        if let Some(&key) = self.instances.get(plugin_name) {
            return Ok(key as ffi::OfxImageEffectHandle);
        }

        let plugin_idx = self.plugin_index(plugin_name).ok_or(PluginError::NotFound)?;

        self.create_instance(plugin_name, plugin_idx, context, true)
    }

    /// The plug-in that renders `name`: of the initialised ones with that
    /// identifier, the highest version, the first found on a tie. Bundles
    /// and search paths often carry several versions of one plug-in, and
    /// the order they are found in says nothing about which is newer
    /// (#1018).
    pub(crate) fn plugin_index(&self, name: &str) -> Option<usize> {
        best_version(self.plugins.iter().enumerate().filter(|(_, p)| p.name == name && p.initialized).map(|(i, p)| (i, p.version)))
    }

    /// Set the instance's parameters to a clip's: its saved values, and
    /// the plug-in's defaults for everything it never changed. One
    /// instance serves every clip that uses the plug-in, and a clip saves
    /// only what was changed, so applying the saved values alone left each
    /// parameter as the previous clip set it, and a picture depended on
    /// the order clips were drawn in (#1018).
    fn sync_params_to_instance(
        &self,
        inst_handle: ffi::OfxImageEffectHandle,
        param_values: &HashMap<String, OfxValue>,
    ) {
        let Some(param_set_handle) =
            instance::with_instance(inst_handle, |inst| inst.param_set_handle)
        else {
            return;
        };

        let Some(handles) = param::with_param_set(param_set_handle, |pset| pset.param_handles.clone()) else {
            return;
        };
        for (name, handle_key) in handles {
            param::with_param(handle_key as ffi::OfxParamHandle, |pi| {
                pi.value = match param_values.get(&name) {
                    Some(value) => declared_value(&value.to_param_value(), &pi.descriptor),
                    None => pi.descriptor.default.clone(),
                };
            });
        }
    }

    // ── Action dispatch ──────────────────────────────────────────────

    fn call_action_raw(
        &self,
        plugin_idx: usize,
        action: &[u8],
        handle: ffi::OfxImageEffectHandle,
        in_args: ffi::OfxPropertySetHandle,
        out_args: ffi::OfxPropertySetHandle,
    ) -> OfxStatus {
        let raw = unsafe { &*self.plugins[plugin_idx].raw };
        let Some(main_entry) = raw.main_entry else {
            return ffi::K_OFX_STAT_FAILED;
        };
        unsafe {
            main_entry(
                action.as_ptr() as *const c_char,
                handle as *const c_void,
                in_args as *const c_void,
                out_args,
            )
        }
    }

    // ── Render ───────────────────────────────────────────────────────

    pub fn apply_effects_chain(
        &mut self,
        effects: &[AppliedEffect],
        rgba: &[u8],
        width: u32,
        height: u32,
        frame_time: f64,
    ) -> Vec<u8> {
        self.apply_effects_chain_full(effects, rgba, width, height, frame_time, None)
    }

    /// Apply a chain of effects with an optional abort. The chain has no
    /// other frames to give: a plug-in that asks for one gets none (a
    /// temporal plug-in runs as a node, which hands it its frames).
    pub fn apply_effects_chain_with_abort(
        &mut self,
        effects: &[AppliedEffect],
        rgba: &[u8],
        width: u32,
        height: u32,
        frame_time: f64,
        abort: Option<Arc<AtomicBool>>,
    ) -> Vec<u8> {
        self.apply_effects_chain_full(effects, rgba, width, height, frame_time, abort)
    }

    fn apply_effects_chain_full(
        &mut self,
        effects: &[AppliedEffect],
        rgba: &[u8],
        width: u32,
        height: u32,
        frame_time: f64,
        abort: Option<Arc<AtomicBool>>,
    ) -> Vec<u8> {
        if effects.is_empty() || effects.iter().all(|e| !e.enabled) {
            return rgba.to_vec();
        }

        // Store render dimensions for temporal access
        self.render_width = width;
        self.render_height = height;

        let premult = self.plugins.first()
            .map(|p| p.premult_string.clone())
            .unwrap_or_else(|| crate::constants::kOfxImageAlphaUnPremultiplied.to_string());
        let _render = image_effect::set_render_context(None, frame_time, width, height, &premult, false);

        let mut current = rgba.to_vec();
        for effect in effects {
            if !effect.enabled {
                continue;
            }
            // Check abort between effects in the chain
            if let Some(ref flag) = abort {
                if flag.load(Ordering::Relaxed) {
                    debug!("OFX: effects chain aborted before '{}'", effect.plugin_name);
                    return current;
                }
            }
            // Update render context with needs_float for this specific plugin
            if let Some(idx) = self.plugin_index(&effect.plugin_name) {
                let needs_float = self.plugins[idx].needs_float;
                let premult = self.plugins[idx].premult_string.clone();
                // Update context for this effect's float requirements
                let mut ctx = image_effect::RENDER_CONTEXT.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut c) = *ctx {
                    c.needs_float = needs_float;
                    c.premult = premult;
                }
                drop(ctx);
            }

            match self.apply_effect(effect, &current, width, height, frame_time, &abort) {
                Ok(result) => current = result,
                Err(e) => warn!("OFX: apply_effect '{}' failed: {e}", effect.plugin_name),
            }
        }

        // The temporal frame provider is cleared as `_render` drops.
        current
    }

    fn apply_effect(
        &mut self,
        effect: &AppliedEffect,
        input: &[u8],
        width: u32,
        height: u32,
        frame_time: f64,
        abort: &Option<Arc<AtomicBool>>,
    ) -> Result<Vec<u8>, PluginError> {

        // Get or create instance
        let inst_handle = self.get_or_create_instance(&effect.plugin_name, effect.context)?;
        self.render_on(inst_handle, effect, input, width, height, frame_time, abort, 24.0, 1.0, (0.0, 1000.0))
    }

    /// `effect` rendered on the instance `inst_handle`, at `frame_time` in
    /// the time unit of `fps` (declared as the instance's and its clips'
    /// frame rate), at `render_scale`, over `frame_range` (its clips'
    /// frame range, the frames it may ask for).
    #[allow(clippy::too_many_arguments)]
    fn render_on(
        &mut self,
        inst_handle: ffi::OfxImageEffectHandle,
        effect: &AppliedEffect,
        input: &[u8],
        width: u32,
        height: u32,
        frame_time: f64,
        abort: &Option<Arc<AtomicBool>>,
        fps: f64,
        render_scale: f64,
        frame_range: (f64, f64),
    ) -> Result<Vec<u8>, PluginError> {
        let plugin_idx = instance::with_instance(inst_handle, |inst| inst.plugin_idx)
            .ok_or(PluginError::NotFound)?;

        // Determine which GPU path to use (priority: CUDA > OpenCL > OpenGL > CPU)
        let use_cuda = self.cuda.is_some() && self.plugins[plugin_idx].supports_cuda;
        let use_opencl = !use_cuda && self.opencl.is_some() && self.plugins[plugin_idx].supports_opencl;
        let use_opengl = !use_cuda && !use_opencl && self.opengl.is_some() && self.plugins[plugin_idx].supports_opengl;

        // Set abort flag on the instance so the plugin's abort() callback can check it
        instance::with_instance(inst_handle, |inst| {
            if let Some(ref flag) = abort {
                inst.abort_flag = flag.clone();
            } else {
                inst.abort_flag.store(false, Ordering::Relaxed);
            }
        });

        // Sync param values from the effect
        self.sync_params_to_instance(inst_handle, &effect.param_values);

        // Update instance and clip properties for this render
        instance::with_instance(inst_handle, |inst| {
            // Instance properties
            property::with_propset(inst.props_handle, |ps| {
                ps.set_double("OfxImageEffectPropProjectSize", 0, width as f64);
                ps.set_double("OfxImageEffectPropProjectSize", 1, height as f64);
                ps.set_double("OfxImageEffectPropProjectOffset", 0, 0.0);
                ps.set_double("OfxImageEffectPropProjectOffset", 1, 0.0);
                ps.set_double("OfxImageEffectPropProjectExtent", 0, width as f64);
                ps.set_double("OfxImageEffectPropProjectExtent", 1, height as f64);
                ps.set_double("OfxImageEffectPropProjectPixelAspectRatio", 0, 1.0);
                ps.set_double("OfxImageEffectPropFrameRate", 0, fps);
                ps.set_double("OfxImageEffectPropFrameRange", 0, frame_range.0);
                ps.set_double("OfxImageEffectPropFrameRange", 1, frame_range.1);
                ps.set_double("OfxImageEffectPropRenderScale", 0, render_scale);
                ps.set_double("OfxImageEffectPropRenderScale", 1, render_scale);
            });

            // Clip properties — determine connected clips based on context
            let connected = match effect.context {
                OfxCategory::Filter | OfxCategory::Color | OfxCategory::Other => {
                    vec!["Source", "Output"]
                }
                OfxCategory::Transition => vec!["SourceFrom", "SourceTo", "Output"],
                OfxCategory::Generator => vec!["Output"],
            };
            let depth = if self.plugins[plugin_idx].needs_float { crate::constants::kOfxBitDepthFloat } else { crate::constants::kOfxBitDepthByte };
            for (clip_name, clip) in &inst.clips {
                let is_connected = connected.contains(&clip_name.as_str());
                property::with_propset(clip.props_handle, |ps| {
                    ps.set_string("OfxImageEffectPropPixelDepth", 0, depth);
                    ps.set_string("OfxImageEffectPropComponents", 0, crate::constants::kOfxImageComponentRGBA);
                    ps.set_int("OfxImageClipPropConnected", 0, if is_connected { 1 } else { 0 });
                    ps.set_double("OfxImageEffectPropFrameRate", 0, fps);
                    ps.set_double("OfxImageEffectPropPixelAspectRatio", 0, 1.0);
                    ps.set_double("OfxImageEffectPropFrameRange", 0, frame_range.0);
                    ps.set_double("OfxImageEffectPropFrameRange", 1, frame_range.1);
                    ps.set_double("OfxImageEffectPropUnmappedFrameRange", 0, frame_range.0);
                    ps.set_double("OfxImageEffectPropUnmappedFrameRange", 1, frame_range.1);
                    ps.set_string("OfxImageClipPropFieldOrder", 0, crate::constants::kOfxFieldNone);
                });
            }
        });

        // Call GetClipPreferences once per instance — plugin negotiates output format
        let needs_clip_prefs = instance::with_instance(inst_handle, |inst| !inst.clip_prefs_fetched)
            .unwrap_or(false);
        if needs_clip_prefs {
            let clip_pref_out = PropertySet::default();
            let clip_pref_out_handle = property::register_propset(clip_pref_out);

            let cp_status = self.call_action_raw(
                plugin_idx,
                b"OfxImageEffectActionGetClipPreferences\0",
                inst_handle,
                std::ptr::null_mut(),
                clip_pref_out_handle,
            );

            if cp_status == ffi::K_OFX_STAT_OK {
                // Read back negotiated preferences and update clip properties
                instance::with_instance(inst_handle, |inst| {
                    let clip_names: Vec<String> = inst.clips.keys().cloned().collect();
                    for clip_name in clip_names {
                        let clip_name = clip_name.as_str();
                        if let Some(clip) = inst.clips.get(clip_name) {
                            let components_key = format!("OfxImageClipPropComponents_{clip_name}");
                            let depth_key = format!("OfxImageEffectPropPixelDepth_{clip_name}");

                            let components = property::with_propset(clip_pref_out_handle, |ps| {
                                ps.get_string_lossy(&components_key, 0)
                            }).flatten();
                            let depth = property::with_propset(clip_pref_out_handle, |ps| {
                                ps.get_string_lossy(&depth_key, 0)
                            }).flatten();

                            property::with_propset(clip.props_handle, |ps| {
                                if let Some(ref c) = components {
                                    if !c.is_empty() {
                                        ps.set_string("OfxImageEffectPropComponents", 0, c);
                                    }
                                }
                                if let Some(ref d) = depth {
                                    // Don't let ClipPreferences downgrade Float to Byte
                                    // for plugins that need Float
                                    if !d.is_empty() {
                                        ps.set_string("OfxImageEffectPropPixelDepth", 0, d);
                                    }
                                }
                            });
                        }
                    }
                });
            }

            if cp_status != ffi::K_OFX_STAT_OK && cp_status != ffi::K_OFX_STAT_REPLY_DEFAULT {
                debug!(
                    "OFX GetClipPreferences returned status={} for '{}'",
                    cp_status, effect.plugin_name
                );
            }

            property::remove_propset(clip_pref_out_handle);
            instance::with_instance(inst_handle, |inst| inst.clip_prefs_fetched = true);
        }

        // Create input and output images based on context
        let premult = instance::with_instance(inst_handle, |inst| inst.premult_string.clone())
            .unwrap_or_else(|| crate::constants::kOfxImageAlphaUnPremultiplied.to_string());

        // Track all registered image handles for cleanup
        let mut registered_img_handles: Vec<ffi::OfxPropertySetHandle> = Vec::new();

        match effect.context {
            OfxCategory::Transition => {
                // Transition: SourceFrom = input, SourceTo = transition_input, Output = blank
                let from_image = OfxImage::from_rgba(input.to_vec(), width, height, frame_time, &premult);
                let from_handle = image_effect::register_image(from_image);
                registered_img_handles.push(from_handle);

                let to_data = effect.transition_input.as_deref().unwrap_or(input);
                let to_image = OfxImage::from_rgba(to_data.to_vec(), width, height, frame_time, &premult);
                let to_handle = image_effect::register_image(to_image);
                registered_img_handles.push(to_handle);

                let output_image = OfxImage::from_rgba(input.to_vec(), width, height, frame_time, &premult);
                let output_img_handle = image_effect::register_image(output_image);
                registered_img_handles.push(output_img_handle);

                instance::with_instance(inst_handle, |inst| {
                    if let Some(clip) = inst.clips.get_mut("SourceFrom") {
                        clip.current_image = Some(from_handle);
                    }
                    if let Some(clip) = inst.clips.get_mut("SourceTo") {
                        clip.current_image = Some(to_handle);
                    }
                    if let Some(clip) = inst.clips.get_mut("Output") {
                        clip.current_image = Some(output_img_handle);
                    }
                });
            }
            OfxCategory::Generator => {
                // Generator: no input, Output only
                let output_image = OfxImage::from_rgba(vec![0u8; (width * height * 4) as usize], width, height, frame_time, &premult);
                let output_img_handle = image_effect::register_image(output_image);
                registered_img_handles.push(output_img_handle);

                instance::with_instance(inst_handle, |inst| {
                    if let Some(clip) = inst.clips.get_mut("Output") {
                        clip.current_image = Some(output_img_handle);
                    }
                });
            }
            _ => {
                // Filter: Source + Output
                let needs_float = self.plugins[plugin_idx].needs_float;
                let (input_image, output_image) = if needs_float {
                    (OfxImage::from_rgba_as_float(input, width, height, frame_time, &premult),
                     OfxImage::from_rgba_as_float(input, width, height, frame_time, &premult))
                } else {
                    (OfxImage::from_rgba(input.to_vec(), width, height, frame_time, &premult),
                     // Initialize output with input data — plugins only write to unmasked pixels
                     OfxImage::from_rgba(input.to_vec(), width, height, frame_time, &premult))
                };

                let input_img_handle = image_effect::register_image(input_image);
                let output_img_handle = image_effect::register_image(output_image);
                registered_img_handles.push(input_img_handle);
                registered_img_handles.push(output_img_handle);

                instance::with_instance(inst_handle, |inst| {
                    if let Some(clip) = inst.clips.get_mut("Source") {
                        clip.current_image = Some(input_img_handle);
                    }
                    if let Some(clip) = inst.clips.get_mut("Output") {
                        clip.current_image = Some(output_img_handle);
                    }
                });
            }
        }

        // ── GPU: allocate device memory and override data pointers ──
        // When GPU is active, kOfxImagePropData must point to device memory.
        let mut cuda_device_ptrs: Vec<u64> = Vec::new();
        let mut opencl_buffers: Vec<*mut std::ffi::c_void> = Vec::new();
        let mut cuda_active = false;
        let mut opencl_active = false;
        let mut opengl_active = false;

        if use_cuda {
            // Lazy init CUDA on first use
            if self.cuda.is_none() {
                self.cuda = crate::gpu_cuda::CudaRuntime::new();
                if let Some(ref c) = self.cuda {
                    info!("OFX: CUDA initialized lazily on '{}'", c.device_name());
                }
            }
            if let Some(ref cuda) = self.cuda {
                if cuda.push_context() {
                    cuda_active = true;
                    let frame_bytes = (width * height * 4) as usize;

                    for &img_handle in &registered_img_handles {
                        if let Some(dptr) = cuda.alloc(frame_bytes) {
                            cuda_device_ptrs.push(dptr);

                            let uploaded = image_effect::with_image(img_handle, |img| {
                                cuda.copy_htod(dptr, &img.data)
                            }).unwrap_or(false);

                            if !uploaded {
                                warn!("CUDA: failed to upload image to device, falling back to CPU");
                                for &dp in &cuda_device_ptrs {
                                    cuda.free(dp);
                                }
                                cuda_device_ptrs.clear();
                                cuda_active = false;
                                cuda.pop_context();
                                break;
                            }

                            property::with_propset(img_handle, |ps| {
                                ps.set_pointer("OfxImagePropData", 0, dptr as *mut std::ffi::c_void);
                            });
                        } else {
                            warn!("CUDA: device alloc failed, falling back to CPU");
                            for &dp in &cuda_device_ptrs {
                                cuda.free(dp);
                            }
                            cuda_device_ptrs.clear();
                            cuda_active = false;
                            cuda.pop_context();
                            break;
                        }
                    }
                }
            }
        } else if use_opencl {
            // Lazy init OpenCL on first use
            if self.opencl.is_none() {
                self.opencl = crate::gpu_opencl::OpenClRuntime::new();
                if let Some(ref c) = self.opencl {
                    info!("OFX: OpenCL initialized lazily on '{}' ({})", c.device_name(), c.platform_name());
                }
            }
            if let Some(ref cl) = self.opencl {
                opencl_active = true;

                for &img_handle in &registered_img_handles {
                    // Allocate cl_mem with a copy of the CPU data
                    let mem = image_effect::with_image(img_handle, |img| {
                        cl.alloc_with_data(&img.data)
                    }).flatten();

                    if let Some(mem) = mem {
                        opencl_buffers.push(mem);

                        // Override kOfxImagePropData to point to cl_mem
                        // OFX spec: kOfxImagePropData is a cl_mem when OpenCL enabled
                        property::with_propset(img_handle, |ps| {
                            ps.set_pointer("OfxImagePropData", 0, mem);
                        });
                    } else {
                        warn!("OpenCL: buffer alloc failed, falling back to CPU");
                        for &m in &opencl_buffers {
                            unsafe { cl.free(m); }
                        }
                        opencl_buffers.clear();
                        opencl_active = false;
                        break;
                    }
                }
            }
        }

        // ── OpenGL: lazily create context and set up runtime ────────
        if use_opengl && !cuda_active && !opencl_active {
            // Lazy init: create OpenGL context on first use
            if self.opengl.is_none() {
                self.opengl = crate::gpu_opengl::OpenGlRuntime::new()
                    .map(std::sync::Arc::new);
                if self.opengl.is_none() {
                    warn!("OFX: OpenGL context creation failed at render time");
                }
            }
            if let Some(ref gl) = self.opengl {
                if gl.make_current() {
                    opengl_active = true;
                    // Share ownership of the runtime with the suite-level
                    // GL_RUNTIME_STORE via Arc. The store keeps the runtime
                    // alive even if self.opengl is dropped between set and
                    // clear (e.g. on a panic unwind).
                    crate::gpu_opengl::set_gl_runtime(std::sync::Arc::clone(gl));

                    // Call kOfxActionOpenGLContextAttached (once per instance)
                    let needs_attach = instance::with_instance(inst_handle, |inst| {
                        property::with_propset(inst.props_handle, |ps| {
                            ps.get_int("_klipp_gl_attached", 0).unwrap_or(0) == 0
                        })
                    }).flatten().unwrap_or(true);

                    if needs_attach {
                        self.call_action_raw(
                            plugin_idx,
                            b"OfxActionOpenGLContextAttached\0",
                            inst_handle,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        );
                        instance::with_instance(inst_handle, |inst| {
                            property::with_propset(inst.props_handle, |ps| {
                                ps.set_int("_klipp_gl_attached", 0, 1);
                            });
                        });
                    }
                }
            }
        }

        // The output image handle is always the last one registered
        let output_img_handle = *registered_img_handles.last().unwrap();

        // For transitions, set the Transition param value
        if effect.context == OfxCategory::Transition {
            let transition_value = OfxValue::Double(effect.transition_amount);
            let mut tv = HashMap::new();
            tv.insert("Transition".to_string(), transition_value);
            self.sync_params_to_instance(inst_handle, &tv);
        }

        // Full-frame render window (tiling disabled for stability with real plugins)
        let render_x1 = 0i32;
        let render_y1 = 0i32;
        let render_x2 = width as i32;
        let render_y2 = height as i32;

        // Call kOfxImageEffectActionBeginSequenceRender (required by OFX spec before Render)
        let mut bsr_args = PropertySet::default();
        bsr_args.set_double("OfxImageEffectPropFrameRange", 0, frame_time);
        bsr_args.set_double("OfxImageEffectPropFrameRange", 1, frame_time);
        bsr_args.set_double("OfxImageEffectPropFrameStep", 0, 1.0);
        bsr_args.set_double("OfxImageEffectPropRenderScale", 0, render_scale);
        bsr_args.set_double("OfxImageEffectPropRenderScale", 1, render_scale);
        bsr_args.set_int("OfxPropIsInteractive", 0, 0);
        bsr_args.set_int("OfxImageEffectPropSequentialRenderStatus", 0, 0);
        bsr_args.set_int("OfxImageEffectPropInteractiveRenderStatus", 0, 0);
        bsr_args.set_int("OfxImageEffectPropRenderQualityDraft", 0, 0);
        // GPU enable flags
        bsr_args.set_int(crate::constants::kOfxImageEffectPropCudaEnabled, 0, if use_cuda { 1 } else { 0 });
        bsr_args.set_int(crate::constants::kOfxImageEffectPropOpenCLEnabled, 0, if use_opencl { 1 } else { 0 });
        bsr_args.set_int(crate::constants::kOfxImageEffectPropOpenGLEnabled, 0, if use_opengl { 1 } else { 0 });
        if use_cuda {
            if let Some(ref cuda) = self.cuda {
                bsr_args.set_pointer(crate::constants::kOfxImageEffectPropCudaStream, 0, cuda.stream_ptr());
            }
        }
        if use_opencl {
            if let Some(ref cl) = self.opencl {
                bsr_args.set_pointer(crate::constants::kOfxImageEffectPropOpenCLCommandQueue, 0, cl.queue_ptr());
            }
        }
        let bsr_handle = property::register_propset(bsr_args);

        let bsr_status = self.call_action_raw(
            plugin_idx,
            b"OfxImageEffectActionBeginSequenceRender\0",
            inst_handle,
            bsr_handle,
            std::ptr::null_mut(),
        );
        if bsr_status == ffi::K_OFX_STAT_FAILED {
            property::remove_propset(bsr_handle);
            // Clean up images
            for &h in &registered_img_handles {
                image_effect::release_image(h);
            }
            return Err(PluginError::ActionFailed(format!(
                "BeginSequenceRender returned {bsr_status}"
            )));
        }
        property::remove_propset(bsr_handle);

        // Build inArgs for Render — use RoD-derived render window
        let mut in_args = PropertySet::default();
        in_args.set_double("OfxPropTime", 0, frame_time);
        in_args.set_int("OfxImageEffectPropRenderWindow", 0, render_x1);
        in_args.set_int("OfxImageEffectPropRenderWindow", 1, render_y1);
        in_args.set_int("OfxImageEffectPropRenderWindow", 2, render_x2);
        in_args.set_int("OfxImageEffectPropRenderWindow", 3, render_y2);
        in_args.set_double("OfxImageEffectPropRenderScale", 0, render_scale);
        in_args.set_double("OfxImageEffectPropRenderScale", 1, render_scale);
        in_args.set_string("OfxImageEffectPropFieldToRender", 0, crate::constants::kOfxFieldNone);
        in_args.set_int("OfxPropIsInteractive", 0, 0);
        in_args.set_int("OfxImageEffectPropSequentialRenderStatus", 0, 0);
        in_args.set_int("OfxImageEffectPropInteractiveRenderStatus", 0, 0);
        in_args.set_int("OfxImageEffectPropRenderQualityDraft", 0, 0);
        // GPU enable flags on render inArgs
        in_args.set_int(crate::constants::kOfxImageEffectPropCudaEnabled, 0, if use_cuda { 1 } else { 0 });
        in_args.set_int(crate::constants::kOfxImageEffectPropOpenCLEnabled, 0, if use_opencl { 1 } else { 0 });
        in_args.set_int(crate::constants::kOfxImageEffectPropOpenGLEnabled, 0, if use_opengl { 1 } else { 0 });
        if use_cuda {
            if let Some(ref cuda) = self.cuda {
                in_args.set_pointer(crate::constants::kOfxImageEffectPropCudaStream, 0, cuda.stream_ptr());
            }
        }
        if use_opencl {
            if let Some(ref cl) = self.opencl {
                in_args.set_pointer(crate::constants::kOfxImageEffectPropOpenCLCommandQueue, 0, cl.queue_ptr());
            }
        }
        let in_args_handle = property::register_propset(in_args);

        // Call kOfxImageEffectActionRender
        let render_t0 = std::time::Instant::now();
        let status = self.call_action_raw(
            plugin_idx,
            b"OfxImageEffectActionRender\0",
            inst_handle,
            in_args_handle,
            std::ptr::null_mut(),
        );
        let render_ms = render_t0.elapsed().as_secs_f64() * 1000.0;

        // Debug: read output via raw pointer AND via Vec to compare
        if status != ffi::K_OFX_STAT_OK && status != ffi::K_OFX_STAT_REPLY_DEFAULT {
            debug!(
                "OFX render action returned status={} for '{}' ({:.1}ms)",
                status, effect.plugin_name, render_ms
            );
        }

        // Clean up inArgs
        property::remove_propset(in_args_handle);

        // Call kOfxImageEffectActionEndSequenceRender
        let mut esr_args = PropertySet::default();
        esr_args.set_double("OfxImageEffectPropRenderScale", 0, render_scale);
        esr_args.set_double("OfxImageEffectPropRenderScale", 1, render_scale);
        esr_args.set_int("OfxPropIsInteractive", 0, 0);
        esr_args.set_int("OfxImageEffectPropSequentialRenderStatus", 0, 0);
        esr_args.set_int("OfxImageEffectPropInteractiveRenderStatus", 0, 0);
        esr_args.set_int("OfxImageEffectPropRenderQualityDraft", 0, 0);
        esr_args.set_int(crate::constants::kOfxImageEffectPropCudaEnabled, 0, if cuda_active { 1 } else { 0 });
        esr_args.set_int(crate::constants::kOfxImageEffectPropOpenCLEnabled, 0, if opencl_active { 1 } else { 0 });
        esr_args.set_int(crate::constants::kOfxImageEffectPropOpenGLEnabled, 0, if opengl_active { 1 } else { 0 });
        let esr_handle = property::register_propset(esr_args);

        let _esr_status = self.call_action_raw(
            plugin_idx,
            b"OfxImageEffectActionEndSequenceRender\0",
            inst_handle,
            esr_handle,
            std::ptr::null_mut(),
        );
        property::remove_propset(esr_handle);

        // Clear clip image references
        instance::with_instance(inst_handle, |inst| {
            for clip in inst.clips.values_mut() {
                clip.current_image = None;
            }
        });

        // ── Handle GPU render failure → CPU fallback ────────────────
        let gpu_failed = (cuda_active || opencl_active || opengl_active)
            && (status == ffi::K_OFX_STAT_GPU_RENDER_FAILED
                || status == ffi::K_OFX_STAT_GPU_OUT_OF_MEMORY);

        if gpu_failed {
            debug!("OFX: GPU render failed (status={status}) for '{}', retrying with CPU", effect.plugin_name);
            // Clean up GPU resources
            if cuda_active {
                if let Some(ref cuda) = self.cuda {
                    for &dp in &cuda_device_ptrs {
                        cuda.free(dp);
                    }
                    cuda.pop_context();
                }
            }
            if opencl_active {
                if let Some(ref cl) = self.opencl {
                    for &m in &opencl_buffers {
                        unsafe { cl.free(m); }
                    }
                }
            }
            if opengl_active {
                crate::gpu_opengl::clear_gl_runtime();
                if let Some(ref gl) = self.opengl {
                    gl.release_current();
                }
            }
            // Clean up images
            for &h in &registered_img_handles {
                image_effect::take_image(h);
                image_effect::release_image(h);
            }
            // Clear clip references
            instance::with_instance(inst_handle, |inst| {
                for clip in inst.clips.values_mut() {
                    clip.current_image = None;
                }
            });
            // Retry without CUDA — recursive call but with a CPU-only effect
            let mut cpu_effect = effect.clone();
            cpu_effect.context = effect.context; // preserve context
            // Mark instance for re-creation without CUDA
            self.instances.remove(&effect.plugin_name);
            instance::remove_instance(inst_handle);
            // Temporarily disable GPU for this plugin and retry
            let was_cuda = self.plugins[plugin_idx].supports_cuda;
            let was_opencl = self.plugins[plugin_idx].supports_opencl;
            let was_opengl = self.plugins[plugin_idx].supports_opengl;
            self.plugins[plugin_idx].supports_cuda = false;
            self.plugins[plugin_idx].supports_opencl = false;
            self.plugins[plugin_idx].supports_opengl = false;
            let result = self.apply_effect(&cpu_effect, input, width, height, frame_time, abort);
            self.plugins[plugin_idx].supports_cuda = was_cuda;
            self.plugins[plugin_idx].supports_opencl = was_opencl;
            self.plugins[plugin_idx].supports_opengl = was_opengl;
            return result;
        }

        // ── GPU: download output from device ────────────────────────
        let gpu_active = cuda_active || opencl_active || opengl_active;
        let result = if cuda_active {
            if let Some(ref cuda) = self.cuda {
                // Sync the stream to ensure plugin's kernel has completed
                cuda.sync_stream();

                let frame_bytes = (width * height * 4) as usize;
                let mut output_cpu = vec![0u8; frame_bytes];

                // The output device pointer is the last one allocated
                let output_dptr = *cuda_device_ptrs.last().unwrap();

                if status == ffi::K_OFX_STAT_OK || status == ffi::K_OFX_STAT_REPLY_DEFAULT {
                    cuda.copy_dtoh(&mut output_cpu, output_dptr);
                } else {
                    output_cpu = input.to_vec();
                }

                // Free all device memory
                for &dp in &cuda_device_ptrs {
                    cuda.free(dp);
                }
                cuda.pop_context();

                // Clean up CPU image objects
                for &h in &registered_img_handles {
                    image_effect::take_image(h);
                    image_effect::release_image(h);
                }

                output_cpu
            } else {
                input.to_vec()
            }
        } else if opencl_active {
            if let Some(ref cl) = self.opencl {
                cl.finish(); // ensure plugin's kernels have completed

                let frame_bytes = (width * height * 4) as usize;
                let mut output_cpu = vec![0u8; frame_bytes];

                // The output cl_mem is the last one allocated
                let output_mem = *opencl_buffers.last().unwrap();

                if status == ffi::K_OFX_STAT_OK || status == ffi::K_OFX_STAT_REPLY_DEFAULT {
                    unsafe { cl.read_buffer(output_mem, &mut output_cpu); }
                } else {
                    output_cpu = input.to_vec();
                }

                // Free all cl_mem buffers
                for &m in &opencl_buffers {
                    unsafe { cl.free(m); }
                }

                // Clean up CPU image objects
                for &h in &registered_img_handles {
                    image_effect::take_image(h);
                    image_effect::release_image(h);
                }

                output_cpu
            } else {
                input.to_vec()
            }
        } else if opengl_active {
            // OpenGL path: plugin rendered into FBO via clipLoadTexture.
            // Read back pixels from the bound FBO, then clean up.
            if let Some(ref gl) = self.opengl {
                gl.flush_and_finish();

                let output_cpu = if status == ffi::K_OFX_STAT_OK || status == ffi::K_OFX_STAT_REPLY_DEFAULT {
                    gl.read_pixels(width, height)
                } else {
                    input.to_vec()
                };

                crate::gpu_opengl::clear_gl_runtime();
                gl.release_current();

                // Clean up CPU image objects
                for &h in &registered_img_handles {
                    image_effect::take_image(h);
                    image_effect::release_image(h);
                }

                output_cpu
            } else {
                input.to_vec()
            }
        } else {
            // CPU path — read output from image data as before
            if status == ffi::K_OFX_STAT_OK || status == ffi::K_OFX_STAT_REPLY_DEFAULT {
                let output_data = image_effect::take_image(output_img_handle);
                for &h in &registered_img_handles {
                    if h != output_img_handle {
                        image_effect::take_image(h);
                    }
                }
                output_data.map(|o| {
                    if self.plugins[plugin_idx].needs_float {
                        OfxImage::float_data_to_bytes(&o.data, o.width, o.height)
                    } else {
                        o.data
                    }
                }).unwrap_or_else(|| input.to_vec())
            } else {
                debug!(
                    "OFX render returned {status} for '{}'",
                    effect.plugin_name
                );
                for &h in &registered_img_handles {
                    image_effect::take_image(h);
                }
                input.to_vec()
            }
        };

        // Release image propsets (if not already cleaned up by GPU path)
        if !gpu_active {
            for &h in &registered_img_handles {
                image_effect::release_image(h);
            }
        }

        // A render that failed drew nothing: an error, never the input
        // passed off as the plug-in's picture. The chain keeps the frame
        // as it was on an error; a node declines it (M4 step 10).
        if status != ffi::K_OFX_STAT_OK && status != ffi::K_OFX_STAT_REPLY_DEFAULT {
            return Err(PluginError::ActionFailed(format!("its render returned {status}")));
        }
        Ok(result)
    }

    // ── Accessors ────────────────────────────────────────────────────

    pub fn plugins(&self) -> &[OfxPlugin] {
        &self.plugins
    }

    /// One entry per plug-in identifier, in the order they were found:
    /// the version that renders it (the highest initialised one, else the
    /// highest found), so the browser and inspector show what renders.
    pub fn plugin_infos(&self) -> Vec<OfxPluginInfo> {
        let mut seen = std::collections::HashSet::new();
        self.plugins
            .iter()
            .filter(|p| seen.insert(p.name.as_str()))
            .filter_map(|p| {
                let idx = self.plugin_index(&p.name).or_else(|| {
                    best_version(self.plugins.iter().enumerate().filter(|(_, q)| q.name == p.name).map(|(i, q)| (i, q.version)))
                })?;
                Some(self.plugins[idx].info())
            })
            .collect()
    }
}

/// Return the list of OFX plugin metadata discovered on the default
/// search paths, cached for the process lifetime.
///
/// U-5: consumers that only need the metadata (inspector menus,
/// effect registry merge) can call this instead of constructing
/// their own `OfxHost` — discovery is expensive (directory walks +
/// dlopen of every bundle) and the engine already runs it once.
/// The engine still creates its own host for live invocation;
/// this cache is for read-only consumers.
pub fn plugin_infos_cached() -> Vec<OfxPluginInfo> {
    static CACHED: std::sync::OnceLock<Vec<OfxPluginInfo>> = std::sync::OnceLock::new();
    CACHED
        .get_or_init(|| {
            let mut host = OfxHost::new();
            host.discover_plugins();
            host.plugin_infos()
        })
        .clone()
}

// Re-open the impl block closed above to preserve the rest of the methods.
impl OfxHost {

    pub fn host_propset_handle(&self) -> ffi::OfxPropertySetHandle {
        self.host_propset_handle
    }

    /// Whether CUDA GPU rendering is available.
    pub fn cuda_available(&self) -> bool {
        self.cuda.is_some()
    }

    /// Whether OpenCL GPU rendering is available.
    pub fn opencl_available(&self) -> bool {
        self.opencl.is_some()
    }

    /// Whether OpenGL rendering is available.
    pub fn opengl_available(&self) -> bool {
        self.opengl.is_some()
    }
}

// ── A node's own instance (M4 step 3) ────────────────────────────────

/// What a plug-in says of itself, for a node to decide how to run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfxDescribed {
    /// It offers the Filter context.
    pub filter: bool,
    /// It asks for frames at other times.
    pub temporal: bool,
    /// It keeps history between frames: nothing may keep its output by frame.
    pub sequential_render: bool,
    /// Its render thread safety is "unsafe".
    pub unsafe_threads: bool,
    /// A stamp of its version and parameters (name and type): the same
    /// plug-in loaded under another schema has another, so an instance
    /// made under one is never run under the other.
    pub schema: u64,
}

/// The most other frames [`OfxPluginInstance::frames_needed`] names: the
/// nearest to the frame rendered. Twice what any node may read in a frame
/// (`klipp_nodes`' 32), so a plug-in asking past that is still seen to.
pub const MAX_FRAMES_NEEDED: usize = 64;

/// The renders of each "unsafe" plug-in, by name, held one at a time
/// across every host: the viewer and each export hold their own. Always
/// taken inside the host's lock, never before it.
static UNSAFE_PLUGINS: std::sync::LazyLock<std::sync::Mutex<HashMap<String, Arc<std::sync::Mutex<()>>>>> =
    std::sync::LazyLock::new(Default::default);

fn unsafe_lock(name: &str) -> Arc<std::sync::Mutex<()>> {
    Arc::clone(UNSAFE_PLUGINS.lock().unwrap_or_else(|e| e.into_inner()).entry(name.to_string()).or_default())
}

/// One plug-in instance of its own, for one node on one clip: made at the
/// clip's frame rate (OFX's time unit is the clip's frames at that rate),
/// rendered through the host it came from, destroyed when dropped.
///
/// It keeps its host alive (the `Arc`), so the host never goes before its
/// instances. **Never drop one while holding the host's lock**: dropping
/// takes it, and std's `Mutex` isn't re-entrant.
pub struct OfxPluginInstance {
    host: Arc<std::sync::Mutex<OfxHostRuntime>>,
    /// Its key in the instance store (`OfxImageEffectHandle` as `usize`).
    handle: usize,
    plugin: String,
    fps: f64,
    /// Held around each render when the plug-in says it is unsafe.
    unsafe_lock: Option<Arc<std::sync::Mutex<()>>>,
    /// The bytes of the last frame it rendered.
    frame_bytes: usize,
}

/// The host's lock, or the render failing: a plug-in that panicked inside a
/// call poisoned it, and the runtime's state from before the call is all
/// it touched, so it is taken back rather than failing every later frame.
fn host_of(host: &std::sync::Mutex<OfxHostRuntime>) -> std::sync::MutexGuard<'_, OfxHostRuntime> {
    host.lock().unwrap_or_else(|e| e.into_inner())
}

impl OfxHostRuntime {
    /// What `name` says of itself, or `None` when no such plug-in loaded.
    pub fn describe(&self, name: &str) -> Option<OfxDescribed> {
        let p = &self.plugins[self.plugin_index(name)?];
        Some(OfxDescribed {
            filter: p.supported_contexts.contains(&OfxCategory::Filter),
            temporal: p.temporal,
            sequential_render: p.sequential_render,
            unsafe_threads: p.unsafe_threads,
            schema: {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                p.version.hash(&mut h);
                for d in &p.param_descriptors {
                    d.name.hash(&mut h);
                    format!("{:?}", d.param_type).hash(&mut h);
                }
                h.finish()
            },
        })
    }

    /// A new instance of `name` of its own, in the Filter context, at the
    /// clip's `fps`.
    pub fn new_instance(this: &Arc<std::sync::Mutex<Self>>, name: &str, fps: f64) -> Result<OfxPluginInstance, PluginError> {
        let mut host = host_of(this);
        let idx = host.plugin_index(name).ok_or(PluginError::NotFound)?;
        let unsafe_threads = host.plugins[idx].unsafe_threads;
        let handle = host.create_instance(name, idx, OfxCategory::Filter, false)?;
        drop(host);
        Ok(OfxPluginInstance {
            host: Arc::clone(this),
            handle: handle as usize,
            plugin: name.to_string(),
            fps,
            unsafe_lock: unsafe_threads.then(|| unsafe_lock(name)),
            frame_bytes: 0,
        })
    }
}

impl OfxPluginInstance {
    fn handle(&self) -> ffi::OfxImageEffectHandle {
        self.handle as ffi::OfxImageEffectHandle
    }

    /// Its GetFramesNeeded for the frame at `time` (the clip's frames): the
    /// other times it reads, each frame of the range it gives but `time`,
    /// the [`MAX_FRAMES_NEEDED`] nearest at most.
    /// A plug-in that answers by default reads only its own frame.
    pub fn frames_needed(&mut self, params: &HashMap<String, OfxValue>, time: f64) -> Vec<f64> {
        let host = host_of(&self.host);
        let Some(idx) = instance::with_instance(self.handle(), |inst| inst.plugin_idx) else { return Vec::new() };
        host.sync_params_to_instance(self.handle(), params);
        let mut in_args = PropertySet::default();
        in_args.set_double("OfxPropTime", 0, time);
        let in_handle = property::register_propset(in_args);
        let out_handle = property::register_propset(PropertySet::default());
        let status = host.call_action_raw(idx, b"OfxImageEffectActionGetFramesNeeded\0", self.handle(), in_handle, out_handle);
        let range = property::with_propset(out_handle, |ps| {
            Some((ps.get_double("OfxImageClipPropFrameRange_Source", 0)?, ps.get_double("OfxImageClipPropFrameRange_Source", 1)?))
        })
        .flatten();
        property::remove_propset(in_handle);
        property::remove_propset(out_handle);
        match (status, range) {
            (ffi::K_OFX_STAT_OK, Some((lo, hi))) if lo <= hi => {
                // The frames nearest `time` only, at most MAX_FRAMES_NEEDED:
                // a plug-in naming its whole clip (or a million frames)
                // isn't walked frame by frame. More than any node may read,
                // so one asking past a node's budget still shows it.
                let reach = MAX_FRAMES_NEEDED as f64;
                let (from, to) = (lo.floor().max((time - reach).floor()), hi.min(time + reach));
                let mut times: Vec<f64> = (0..)
                    .map(|k| from + k as f64)
                    .take_while(|t| *t <= to)
                    .filter(|t| (t - time).abs() > 1e-9)
                    .collect();
                times.sort_by(|a, b| (a - time).abs().total_cmp(&(b - time).abs()));
                times.truncate(MAX_FRAMES_NEEDED);
                times.sort_by(f64::total_cmp);
                times
            }
            _ => Vec::new(),
        }
    }

    /// The frame at `time` (the clip's frames) from `rgba8` (sRGB RGBA8,
    /// `width × height`), at `render_scale`. `frames` are the other times it
    /// asked for, each as the source gave it (`rgba8`, width, height); its
    /// clipGetImage at a time not there gets no image, never this frame.
    #[allow(clippy::too_many_arguments)]
    pub fn render(&mut self, params: &HashMap<String, OfxValue>, rgba8: &[u8], width: u32, height: u32, time: f64, render_scale: f64, frames: Vec<(f64, (Vec<u8>, u32, u32))>) -> Result<Vec<u8>, PluginError> {
        // Always the host first, then the plug-in's own: viewer and export
        // each lock their own host, and one order keeps them from deadlocking
        // on an unsafe plug-in both use.
        let mut host = host_of(&self.host);
        let _one_at_a_time = self.unsafe_lock.as_ref().map(|l| l.lock().unwrap_or_else(|e| e.into_inner()));
        let premult = instance::with_instance(self.handle(), |inst| inst.premult_string.clone())
            .unwrap_or_else(|| crate::constants::kOfxImageAlphaUnPremultiplied.to_string());
        // Its other frames, by the times it named: a time not asked for has
        // no image.
        let provider: image_effect::FrameProviderFn = Box::new(move |t| frames.iter().find(|(at, _)| (at - t).abs() < 1e-6).map(|(_, f)| f.clone()));
        // Its other frames in the depth it takes them in.
        let needs_float = instance::with_instance(self.handle(), |inst| inst.plugin_idx).is_some_and(|i| host.plugins[i].needs_float);
        let render = image_effect::set_render_context(Some(provider), time, width, height, &premult, needs_float);
        let effect = AppliedEffect { plugin_name: self.plugin.clone(), enabled: true, param_values: params.clone(), ..Default::default() };
        self.frame_bytes = rgba8.len();
        // Any frame it may ask for: the node hands in only the source's, held
        // to the media's range and its budget.
        let out = host.render_on(self.handle(), &effect, rgba8, width, height, time, &None, self.fps, render_scale, (-1.0e6, 1.0e6));
        drop(render);
        out
    }

    /// The state it keeps, as an estimate (OFX doesn't say): two frames of
    /// the size it last rendered. A plug-in keeping more is bounded by the
    /// owner's slot count.
    pub fn bytes(&self) -> usize {
        self.frame_bytes * 2
    }
}

impl Drop for OfxPluginInstance {
    /// Destroys the instance through its host. The host's lock is taken
    /// here: never drop one while holding it.
    fn drop(&mut self) {
        let host = host_of(&self.host);
        let _one_at_a_time = self.unsafe_lock.as_ref().map(|l| l.lock().unwrap_or_else(|e| e.into_inner()));
        if let Some(idx) = instance::with_instance(self.handle(), |inst| inst.plugin_idx) {
            let status = host.call_action_raw(idx, b"OfxActionDestroyInstance\0", self.handle(), std::ptr::null_mut(), std::ptr::null_mut());
            if status != ffi::K_OFX_STAT_OK && status != ffi::K_OFX_STAT_REPLY_DEFAULT {
                warn!("OFX: '{}' DestroyInstance returned {status}", self.plugin);
            }
        }
        instance::remove_instance(self.handle());
    }
}

// ── Applied effect ───────────────────────────────────────────────────

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AppliedEffect {
    pub plugin_name: String,
    pub enabled: bool,
    pub param_values: HashMap<String, OfxValue>,
    /// The context this effect operates in (Filter, Transition, Generator).
    #[serde(default = "default_filter_context")]
    pub context: OfxCategory,
    /// Secondary input for Transition effects (the "to" frame).
    #[serde(skip)]
    pub transition_input: Option<Vec<u8>>,
    /// Transition amount (0.0 = fully "from", 1.0 = fully "to").
    #[serde(default)]
    pub transition_amount: f64,
    /// The entry's own id (a UUID's text), given when it is added to a
    /// clip and kept through every edit: the plug-in node it runs as, so
    /// its instance, follows it through any reorder, even past a copy of
    /// its own plug-in (#1500). `None` for an entry from before, which
    /// runs under an id derived from its place among its plug-in's copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

fn default_filter_context() -> OfxCategory {
    OfxCategory::Filter
}

/// The index of the highest version among `(index, version)`, the
/// earliest on a tie.
fn best_version(found: impl Iterator<Item = (usize, (u32, u32))>) -> Option<usize> {
    found.fold(None, |best: Option<(usize, (u32, u32))>, (i, v)| match best {
        Some((_, bv)) if bv >= v => best,
        _ => Some((i, v)),
    })
    .map(|(i, _)| i)
}

/// A saved value as the parameter the plug-in declares now. The host
/// writes the declared type's bytes through the plug-in's pointer, so a
/// value saved as another type (the plug-in changed it in an update) is
/// converted first, and one that can't be converted gives way to the
/// plug-in's default rather than reaching the instance (#1017).
fn declared_value(saved: &ParamValue, desc: &ParamDescriptor) -> ParamValue {
    if let Some(v) = saved.as_declared(&desc.param_type) {
        return v;
    }
    warn!(param = %desc.name, ?saved, declared = ?desc.param_type, "[OFX] saved value doesn't fit the parameter's type; using the plug-in's default");
    desc.default.clone()
}

/// Convert ParamType to OFX type string.
fn param_type_to_ofx(pt: &param::ParamType) -> &'static str {
    match pt {
        param::ParamType::Double => "OfxParamTypeDouble",
        param::ParamType::Double2 => "OfxParamTypeDouble2D",
        param::ParamType::Double3 => "OfxParamTypeDouble3D",
        param::ParamType::Int => "OfxParamTypeInteger",
        param::ParamType::Int2 => "OfxParamTypeInteger2D",
        param::ParamType::Int3 => "OfxParamTypeInteger3D",
        param::ParamType::Bool => "OfxParamTypeBoolean",
        param::ParamType::Choice => "OfxParamTypeChoice",
        param::ParamType::String => "OfxParamTypeString",
        param::ParamType::Rgba => "OfxParamTypeRGBA",
        param::ParamType::Rgb => "OfxParamTypeRGB",
        param::ParamType::Group => "OfxParamTypeGroup",
        param::ParamType::Page => "OfxParamTypePage",
        param::ParamType::Parametric => "OfxParamTypeParametric",
        param::ParamType::PushButton => "OfxParamTypePushButton",
        param::ParamType::Custom => "OfxParamTypeCustom",
    }
}

/// Set default value properties on a param property set.
fn set_param_default_props(
    props: &mut PropertySet,
    param_type: &param::ParamType,
    default: &param::ParamValue,
) {
    match (param_type, default) {
        (param::ParamType::Double, param::ParamValue::Double(v)) => {
            props.set_double("OfxParamPropDefault", 0, *v);
        }
        (param::ParamType::Double2, param::ParamValue::Double2(a, b)) => {
            props.set_double("OfxParamPropDefault", 0, *a);
            props.set_double("OfxParamPropDefault", 1, *b);
        }
        (param::ParamType::Double3, param::ParamValue::Double3(a, b, c)) => {
            props.set_double("OfxParamPropDefault", 0, *a);
            props.set_double("OfxParamPropDefault", 1, *b);
            props.set_double("OfxParamPropDefault", 2, *c);
        }
        (param::ParamType::Int, param::ParamValue::Int(v)) => {
            props.set_int("OfxParamPropDefault", 0, *v);
        }
        (param::ParamType::Bool, param::ParamValue::Bool(v)) => {
            props.set_int("OfxParamPropDefault", 0, if *v { 1 } else { 0 });
        }
        (param::ParamType::Choice, param::ParamValue::Choice(v)) => {
            props.set_int("OfxParamPropDefault", 0, *v);
        }
        (param::ParamType::Rgba, param::ParamValue::Rgba(r, g, b, a)) => {
            props.set_double("OfxParamPropDefault", 0, *r);
            props.set_double("OfxParamPropDefault", 1, *g);
            props.set_double("OfxParamPropDefault", 2, *b);
            props.set_double("OfxParamPropDefault", 3, *a);
        }
        (param::ParamType::Rgb, param::ParamValue::Rgb(r, g, b)) => {
            props.set_double("OfxParamPropDefault", 0, *r);
            props.set_double("OfxParamPropDefault", 1, *g);
            props.set_double("OfxParamPropDefault", 2, *b);
        }
        _ => {}
    }
}

// Type alias for backward compatibility.
pub type OfxHost = OfxHostRuntime;

/// [`OfxHost::add_plugin_dirs`]' folders.
fn extra_plugin_dirs() -> &'static std::sync::Mutex<Vec<PathBuf>> {
    static DIRS: std::sync::OnceLock<std::sync::Mutex<Vec<PathBuf>>> = std::sync::OnceLock::new();
    DIRS.get_or_init(Default::default)
}

#[cfg(test)]
mod declared_type_tests {
    use super::*;

    /// Every OFX value type with the bytes a plug-in reserves for it.
    const DECLARED: [(&str, usize); 11] = [
        ("OfxParamTypeDouble", 8),
        ("OfxParamTypeDouble2D", 16),
        ("OfxParamTypeDouble3D", 24),
        ("OfxParamTypeInteger", 4),
        ("OfxParamTypeInteger2D", 8),
        ("OfxParamTypeInteger3D", 12),
        ("OfxParamTypeBoolean", 4),
        ("OfxParamTypeChoice", 4),
        ("OfxParamTypeString", std::mem::size_of::<*const c_char>()),
        ("OfxParamTypeRGBA", 32),
        ("OfxParamTypeRGB", 24),
    ];

    fn stored() -> Vec<OfxValue> {
        vec![
            OfxValue::Double(2.6),
            OfxValue::Double2(1.0, 2.0),
            OfxValue::Double3(1.0, 2.0, 3.0),
            OfxValue::Int(7),
            OfxValue::Int2(1, 2),
            OfxValue::Int3(1, 2, 3),
            OfxValue::Bool(true),
            OfxValue::Choice(2),
            OfxValue::String("saved".into()),
            OfxValue::Rgba(0.1, 0.2, 0.3, 0.4),
            OfxValue::Rgb(0.1, 0.2, 0.3),
        ]
    }

    const GUARD: u8 = 0xA5;

    /// A parameter declared as `ofx_type`, given `value` the way a saved
    /// project gives it, then read back into a slot followed by guard
    /// bytes. Returns the slot and its guard.
    fn read_back(ofx_type: &str, value: &OfxValue) -> [u8; 64] {
        let inst = PluginInstance::new(0);
        let pset = inst.param_set_handle;
        let handle = instance::register_instance(inst);
        let ty = std::ffi::CString::new(ofx_type).unwrap();
        let mut props: ffi::OfxPropertySetHandle = std::ptr::null_mut();
        assert_eq!(unsafe { param::param_define_fn(pset, ty.as_ptr(), c"P".as_ptr(), &mut props) }, ffi::K_OFX_STAT_OK);

        let host = OfxHostRuntime::new();
        host.sync_params_to_instance(handle, &HashMap::from([("P".to_string(), value.clone())]));

        let param = param::with_param_set(pset, |s| s.param_handles["P"]).unwrap() as ffi::OfxParamHandle;
        // u64s, so the slot is aligned for any value a plug-in reads.
        let mut buf = [u64::from_ne_bytes([GUARD; 8]); 8];
        assert_eq!(unsafe { param::param_get_value_fn(param, buf.as_mut_ptr() as *mut c_void) }, ffi::K_OFX_STAT_OK);
        instance::remove_instance(handle);
        let mut bytes = [0u8; 64];
        for (i, w) in buf.iter().enumerate() {
            bytes[i * 8..i * 8 + 8].copy_from_slice(&w.to_ne_bytes());
        }
        bytes
    }

    /// #1017: whatever a value was saved as, the host writes only the
    /// declared type's bytes through the plug-in's pointer.
    #[test]
    fn a_saved_value_of_any_type_stays_inside_the_declared_slot() {
        for (ofx_type, size) in DECLARED {
            for value in stored() {
                let bytes = read_back(ofx_type, &value);
                assert!(
                    bytes[size..].iter().all(|&b| b == GUARD),
                    "{ofx_type} given {value:?} wrote past its {size} bytes"
                );
            }
        }
    }

    #[test]
    fn a_saved_value_is_converted_to_the_declared_type() {
        let int = |b: [u8; 64]| i32::from_ne_bytes(b[..4].try_into().unwrap());
        let double = |b: [u8; 64], i: usize| f64::from_ne_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
        assert_eq!(int(read_back("OfxParamTypeInteger", &OfxValue::Double(2.6))), 3);
        assert_eq!(double(read_back("OfxParamTypeDouble", &OfxValue::Int(7)), 0), 7.0);
        assert_eq!(int(read_back("OfxParamTypeBoolean", &OfxValue::Double(0.0))), 0);
        let rgba = read_back("OfxParamTypeRGBA", &OfxValue::Rgb(0.1, 0.2, 0.3));
        assert_eq!((double(rgba, 2), double(rgba, 3)), (0.3, 1.0));
        // A string can't be a number: the plug-in's default (none here) wins.
        assert_eq!(double(read_back("OfxParamTypeDouble", &OfxValue::String("x".into())), 0), 0.0);
    }
}
