use crate::ffi::{self, OfxStatus};
use crate::image_effect::{self, OfxImage};
use crate::instance::{self, PluginInstance};
use crate::param::{self, ParamDescriptor, ParamValue};
use crate::property::{self, PropertySet};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::{Path, PathBuf};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OfxCategory {
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
    /// Raw plugin pointer — valid for the lifetime of _lib.
    raw: *const ffi::OfxPluginRaw,
    _lib: Arc<libloading::Library>,
    /// Whether Load+Describe have been called.
    initialized: bool,
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
        _ => {
            debug!("[OFX] Unknown suite: {} v{}", name, suite_version);
            std::ptr::null()
        }
    }
}

// ── Host Runtime ─────────────────────────────────────────────────────

/// OFX plugin host runtime — discovers, loads, initializes, and renders .ofx.bundle plugins.
pub struct OfxHostRuntime {
    plugins: Vec<OfxPlugin>,
    search_paths: Vec<PathBuf>,
    /// Registered host property set handle.
    host_propset_handle: ffi::OfxPropertySetHandle,
    /// The C host struct given to plugins via setHost. Box for pointer stability.
    c_host: Box<ffi::OfxHostC>,
    /// Maps plugin_name → instance handle (key in INSTANCE_STORE).
    instances: HashMap<String, usize>,
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
        // Create and register host property set
        let mut host_props = PropertySet::default();
        property::populate_host_properties(&mut host_props);
        let host_propset_handle = property::register_propset(host_props);

        let c_host = Box::new(ffi::OfxHostC {
            host: host_propset_handle,
            fetch_suite: fetch_suite_fn,
        });

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
            c_host,
            instances: HashMap::new(),
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

        let mut count = 0;
        for search_path in &paths {
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
            unsafe { libloading::Library::new(binary) }
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
                raw,
                _lib: lib.clone(),
                initialized: false,
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

        // Create a dummy library handle — test plugins don't need one
        // We use a leaked empty library substitute
        let lib = Arc::new(unsafe {
            // Load libc as a harmless library to satisfy the Arc<Library> requirement
            libloading::Library::new("libc.so.6").expect("libc must be loadable")
        });

        let idx = self.plugins.len();
        self.plugins.push(OfxPlugin {
            name,
            version: (raw_ref.plugin_version_major, raw_ref.plugin_version_minor),
            param_descriptors: Vec::new(),
            category: OfxCategory::Filter,
            description: String::new(),
            grouping: String::new(),
            premult_string: crate::constants::kOfxImageAlphaUnPremultiplied.to_string(),
            raw: raw_ptr,
            _lib: lib,
            initialized: false,
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

        // 1. setHost — give plugin our OfxHostC struct
        if let Some(set_host) = raw.set_host {
            unsafe {
                set_host(&*self.c_host as *const ffi::OfxHostC as *const c_void);
            }
        }

        // 2. kOfxActionLoad
        let status = self.call_action_raw(
            plugin_idx,
            b"OfxActionLoad\0",
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if status != ffi::K_OFX_STAT_OK && status != ffi::K_OFX_STAT_REPLY_DEFAULT {
            return Err(PluginError::ActionFailed(format!(
                "kOfxActionLoad returned {status}"
            )));
        }

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

        // 5. kOfxImageEffectActionDescribeInContext — most plugins define params here
        let mut dic_args = PropertySet::default();
        dic_args.set_string("OfxImageEffectPropContext", 0, crate::constants::kOfxImageEffectContextFilter);
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
                "DescribeInContext returned {status} for {}",
                self.plugins[plugin_idx].name
            );
        }
        property::remove_propset(dic_args_handle);

        // 6. Read back param descriptors (after both Describe and DescribeInContext)
        let descriptors = param::read_param_descriptors(desc_param_set);
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

        let plugin = &mut self.plugins[plugin_idx];
        plugin.param_descriptors = descriptors;
        plugin.grouping = grouping;
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
    ) -> Result<ffi::OfxImageEffectHandle, PluginError> {
        let mut inst = PluginInstance::new(plugin_idx);
        inst.premult_string = self.plugins[plugin_idx].premult_string.clone();
        inst.add_clip("Source");
        inst.add_clip("Output");

        // Set up instance properties — must include project properties because
        // the OFX C++ support library reads them in the ImageEffect constructor.
        property::with_propset(inst.props_handle, |ps| {
            ps.set_string("OfxImageEffectPropContext", 0, crate::constants::kOfxImageEffectContextFilter);
            ps.set_string("OfxImageEffectPropPixelDepth", 0, crate::constants::kOfxBitDepthByte);
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

        // Set up clip properties — constructor reads these via fetchClip
        let premult = self.plugins[plugin_idx].premult_string.clone();
        for (clip_name, clip) in &inst.clips {
            instance::register_clip_mapping(clip.handle, inst.handle, clip_name);
            property::with_propset(clip.props_handle, |ps| {
                ps.set_string("OfxImageEffectPropPixelDepth", 0, crate::constants::kOfxBitDepthByte);
                ps.set_string("OfxImageEffectPropComponents", 0, crate::constants::kOfxImageComponentRGBA);
                ps.set_string("OfxImageEffectPropSupportedComponents", 0, crate::constants::kOfxImageComponentRGBA);
                ps.set_string("OfxImageClipPropSupportedComponents", 0, crate::constants::kOfxImageComponentRGBA);
                ps.set_string("OfxImageEffectPropSupportedPixelDepths", 0, crate::constants::kOfxBitDepthByte);
                ps.set_int("OfxImageClipPropConnected", 0, if clip_name == "Source" || clip_name == "Output" { 1 } else { 0 });
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
                ps.set_int("OfxImageClipPropOptional", 0, if clip_name == "Source" || clip_name == "Output" { 0 } else { 1 });
                // Unmapped (pre-negotiation) format — required by OFX C++ support library
                ps.set_string("OfxImageClipPropUnmappedComponents", 0, crate::constants::kOfxImageComponentRGBA);
                ps.set_string("OfxImageClipPropUnmappedPixelDepth", 0, crate::constants::kOfxBitDepthByte);
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
                };
                {
                    let mut store = crate::param::PARAM_STORE.lock().unwrap();
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

        self.instances.insert(plugin_name.to_string(), handle as usize);
        info!("OFX: created instance for '{plugin_name}'");

        Ok(handle)
    }

    /// Get or create an instance for a plugin effect.
    fn get_or_create_instance(
        &mut self,
        plugin_name: &str,
    ) -> Result<ffi::OfxImageEffectHandle, PluginError> {
        if let Some(&key) = self.instances.get(plugin_name) {
            return Ok(key as ffi::OfxImageEffectHandle);
        }

        let plugin_idx = self
            .plugins
            .iter()
            .position(|p| p.name == plugin_name && p.initialized)
            .ok_or(PluginError::NotFound)?;

        self.create_instance(plugin_name, plugin_idx)
    }

    /// Sync AppliedEffect.param_values into the instance's param store.
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

        for (name, value) in param_values {
            let pv = value.to_param_value();
            param::with_param_set(param_set_handle, |pset| {
                if let Some(&handle_key) = pset.param_handles.get(name) {
                    param::with_param(handle_key as ffi::OfxParamHandle, |pi| {
                        pi.value = pv.clone();
                    });
                }
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
        if effects.is_empty() || effects.iter().all(|e| !e.enabled) {
            return rgba.to_vec();
        }
        let mut current = rgba.to_vec();
        for effect in effects {
            if !effect.enabled {
                continue;
            }
            match self.apply_effect(effect, &current, width, height, frame_time) {
                Ok(result) => current = result,
                Err(e) => warn!("OFX: apply_effect '{}' failed: {e}", effect.plugin_name),
            }
        }
        current
    }

    fn apply_effect(
        &mut self,
        effect: &AppliedEffect,
        input: &[u8],
        width: u32,
        height: u32,
        frame_time: f64,
    ) -> Result<Vec<u8>, PluginError> {
        // Get or create instance
        let inst_handle = self.get_or_create_instance(&effect.plugin_name)?;

        let plugin_idx = instance::with_instance(inst_handle, |inst| inst.plugin_idx)
            .ok_or(PluginError::NotFound)?;

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
                ps.set_double("OfxImageEffectPropFrameRate", 0, 24.0);
                ps.set_double("OfxImageEffectPropFrameRange", 0, 0.0);
                ps.set_double("OfxImageEffectPropFrameRange", 1, 1000.0);
                ps.set_double("OfxImageEffectPropRenderScale", 0, 1.0);
                ps.set_double("OfxImageEffectPropRenderScale", 1, 1.0);
            });

            // Clip properties
            for (clip_name, clip) in &inst.clips {
                property::with_propset(clip.props_handle, |ps| {
                    ps.set_string("OfxImageEffectPropPixelDepth", 0, crate::constants::kOfxBitDepthByte);
                    ps.set_string("OfxImageEffectPropComponents", 0, crate::constants::kOfxImageComponentRGBA);
                    // Only Source and Output are connected — optional clips like Mask are not
                    ps.set_int("OfxImageClipPropConnected", 0,
                        if clip_name == "Source" || clip_name == "Output" { 1 } else { 0 });
                    ps.set_double("OfxImageEffectPropFrameRate", 0, 24.0);
                    ps.set_double("OfxImageEffectPropPixelAspectRatio", 0, 1.0);
                    ps.set_double("OfxImageEffectPropFrameRange", 0, 0.0);
                    ps.set_double("OfxImageEffectPropFrameRange", 1, 1000.0);
                    ps.set_double("OfxImageEffectPropUnmappedFrameRange", 0, 0.0);
                    ps.set_double("OfxImageEffectPropUnmappedFrameRange", 1, 1000.0);
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
                    for clip_name in ["Source", "Output"] {
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

        // Create input and output images
        let premult = instance::with_instance(inst_handle, |inst| inst.premult_string.clone())
            .unwrap_or_else(|| crate::constants::kOfxImageAlphaUnPremultiplied.to_string());
        let input_image = OfxImage::from_rgba(input.to_vec(), width, height, frame_time, &premult);
        // Initialize output with input data — plugins only write to unmasked pixels,
        // leaving masked pixels unchanged. A zero-filled buffer causes black output
        // when no mask is connected (all pixels treated as masked).
        let output_image = OfxImage::from_rgba(input.to_vec(), width, height, frame_time, &premult);

        let input_img_handle = image_effect::register_image(input_image);
        let output_img_handle = image_effect::register_image(output_image);

        // Set current images on clips
        instance::with_instance(inst_handle, |inst| {
            if let Some(clip) = inst.clips.get_mut("Source") {
                clip.current_image = Some(input_img_handle);
            }
            if let Some(clip) = inst.clips.get_mut("Output") {
                clip.current_image = Some(output_img_handle);
            }
        });

        // Call kOfxImageEffectActionBeginSequenceRender (required by OFX spec before Render)
        let mut bsr_args = PropertySet::default();
        bsr_args.set_double("OfxImageEffectPropFrameRange", 0, frame_time);
        bsr_args.set_double("OfxImageEffectPropFrameRange", 1, frame_time);
        bsr_args.set_double("OfxImageEffectPropFrameStep", 0, 1.0);
        bsr_args.set_double("OfxImageEffectPropRenderScale", 0, 1.0);
        bsr_args.set_double("OfxImageEffectPropRenderScale", 1, 1.0);
        bsr_args.set_int("OfxPropIsInteractive", 0, 0);
        bsr_args.set_int("OfxImageEffectPropSequentialRenderStatus", 0, 0);
        bsr_args.set_int("OfxImageEffectPropInteractiveRenderStatus", 0, 0);
        bsr_args.set_int("OfxImageEffectPropRenderQualityDraft", 0, 0);
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
            image_effect::release_image(input_img_handle);
            image_effect::release_image(output_img_handle);
            return Err(PluginError::ActionFailed(format!(
                "BeginSequenceRender returned {bsr_status}"
            )));
        }
        property::remove_propset(bsr_handle);

        // Build inArgs for Render
        let mut in_args = PropertySet::default();
        in_args.set_double("OfxPropTime", 0, frame_time);
        in_args.set_int("OfxImageEffectPropRenderWindow", 0, 0);
        in_args.set_int("OfxImageEffectPropRenderWindow", 1, 0);
        in_args.set_int("OfxImageEffectPropRenderWindow", 2, width as i32);
        in_args.set_int("OfxImageEffectPropRenderWindow", 3, height as i32);
        in_args.set_double("OfxImageEffectPropRenderScale", 0, 1.0);
        in_args.set_double("OfxImageEffectPropRenderScale", 1, 1.0);
        in_args.set_string("OfxImageEffectPropFieldToRender", 0, crate::constants::kOfxFieldNone);
        in_args.set_int("OfxPropIsInteractive", 0, 0);
        in_args.set_int("OfxImageEffectPropSequentialRenderStatus", 0, 0);
        in_args.set_int("OfxImageEffectPropInteractiveRenderStatus", 0, 0);
        in_args.set_int("OfxImageEffectPropRenderQualityDraft", 0, 0);
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
        esr_args.set_double("OfxImageEffectPropRenderScale", 0, 1.0);
        esr_args.set_double("OfxImageEffectPropRenderScale", 1, 1.0);
        esr_args.set_int("OfxPropIsInteractive", 0, 0);
        esr_args.set_int("OfxImageEffectPropSequentialRenderStatus", 0, 0);
        esr_args.set_int("OfxImageEffectPropInteractiveRenderStatus", 0, 0);
        esr_args.set_int("OfxImageEffectPropRenderQualityDraft", 0, 0);
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
            if let Some(clip) = inst.clips.get_mut("Source") {
                clip.current_image = None;
            }
            if let Some(clip) = inst.clips.get_mut("Output") {
                clip.current_image = None;
            }
        });

        // Read output image data
        let result = if status == ffi::K_OFX_STAT_OK || status == ffi::K_OFX_STAT_REPLY_DEFAULT {
            let output_data = image_effect::take_image(output_img_handle);
            let _source_data = image_effect::take_image(input_img_handle);
            output_data.map(|o| o.data).unwrap_or_else(|| input.to_vec())
        } else {
            debug!(
                "OFX render returned {status} for '{}'",
                effect.plugin_name
            );
            // Clean up images on failure
            image_effect::take_image(output_img_handle);
            image_effect::take_image(input_img_handle);
            input.to_vec()
        };

        // Release images
        image_effect::release_image(input_img_handle);
        // output already taken via take_image, just clean up propset if it wasn't taken
        image_effect::release_image(output_img_handle);

        Ok(result)
    }

    // ── Accessors ────────────────────────────────────────────────────

    pub fn plugins(&self) -> &[OfxPlugin] {
        &self.plugins
    }

    pub fn plugin_infos(&self) -> Vec<OfxPluginInfo> {
        self.plugins.iter().map(|p| p.info()).collect()
    }

    pub fn host_propset_handle(&self) -> ffi::OfxPropertySetHandle {
        self.host_propset_handle
    }
}

// ── Applied effect ───────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppliedEffect {
    pub plugin_name: String,
    pub enabled: bool,
    pub param_values: HashMap<String, OfxValue>,
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
