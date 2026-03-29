//! OFX Plugin Instance — per-clip state for each applied effect.
//!
//! Each clip that has an OFX effect applied gets a PluginInstance.
//! Instances are created via kOfxActionCreateInstance and destroyed
//! via kOfxActionDestroyInstance.

use crate::ffi::*;
use crate::param::ParamValue;
use crate::property::{self, PropertySet};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex};

/// Per-clip OFX plugin instance.
pub struct PluginInstance {
    /// Stable handle for this instance (given to plugins as OfxImageEffectHandle).
    pub handle: OfxImageEffectHandle,
    /// Per-instance property set.
    pub props_handle: OfxPropertySetHandle,
    /// Per-instance parameter set handle.
    pub param_set_handle: OfxParamSetHandle,
    /// Per-instance parameter values (synced from AppliedEffect).
    pub param_values: HashMap<String, ParamValue>,
    /// Clip handles: "Source", "Output", etc.
    pub clips: HashMap<String, ClipInstance>,
    /// Whether kOfxActionCreateInstance has been called.
    pub created: bool,
    /// Whether kOfxImageEffectActionGetClipPreferences has been called.
    pub clip_prefs_fetched: bool,
    /// The premultiplication string this plugin expects.
    pub premult_string: String,
    /// Plugin index in OfxHostRuntime.plugins.
    pub plugin_idx: usize,
    /// Abort flag — set to true by the host to signal the plugin should stop rendering.
    pub abort_flag: Arc<AtomicBool>,
}

// Safety: All handle fields are opaque keys into global Mutex-protected stores.
unsafe impl Send for PluginInstance {}

pub struct ClipInstance {
    pub name: String,
    pub handle: OfxImageClipHandle,
    pub props_handle: OfxPropertySetHandle,
    /// Current image handle set by the host before calling Render.
    pub current_image: Option<OfxPropertySetHandle>,
}

// Safety: handles are opaque keys.
unsafe impl Send for ClipInstance {}

impl PluginInstance {
    pub fn new(plugin_idx: usize) -> Self {
        let mut props = PropertySet::default();
        props.set_int("OfxImageEffectPropFrameRate", 0, 24);
        props.set_int("OfxImageEffectPropFrameRange", 0, 0);
        props.set_int("OfxImageEffectPropFrameRange", 1, 0);
        let props_handle = property::register_propset(props);

        // Create param set for this instance
        let param_set_handle = crate::param::register_param_set();

        // The handle is the address of the property set — unique and stable
        let handle = props_handle as OfxImageEffectHandle;

        Self {
            handle,
            props_handle,
            param_set_handle,
            param_values: HashMap::new(),
            clips: HashMap::new(),
            created: false,
            clip_prefs_fetched: false,
            premult_string: crate::constants::kOfxImageAlphaUnPremultiplied.to_string(),
            plugin_idx,
            abort_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Add a clip to this instance (called during Describe or CreateInstance).
    pub fn add_clip(&mut self, name: &str) {
        let clip_props = PropertySet::default();
        let clip_props_handle = property::register_propset(clip_props);
        let clip_handle = clip_props_handle as OfxImageClipHandle;

        self.clips.insert(
            name.to_string(),
            ClipInstance {
                name: name.to_string(),
                handle: clip_handle,
                props_handle: clip_props_handle,
                current_image: None,
            },
        );
    }
}

// ── Global instance store ────────────────────────────────────────────

/// Maps instance handle (as usize) → PluginInstance.
static INSTANCE_STORE: LazyLock<Mutex<HashMap<usize, PluginInstance>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn register_instance(instance: PluginInstance) -> OfxImageEffectHandle {
    let handle = instance.handle;
    let key = handle as usize;
    INSTANCE_STORE.lock().unwrap().insert(key, instance);
    handle
}

pub fn with_instance<R>(
    handle: OfxImageEffectHandle,
    f: impl FnOnce(&mut PluginInstance) -> R,
) -> Option<R> {
    let key = handle as usize;
    INSTANCE_STORE.lock().unwrap().get_mut(&key).map(f)
}

pub fn remove_instance(handle: OfxImageEffectHandle) {
    let key = handle as usize;
    INSTANCE_STORE.lock().unwrap().remove(&key);
}

/// Find an instance by plugin name. Returns the handle if found.
pub fn find_instance_by_plugin(plugin_name: &str, instances: &HashMap<String, usize>) -> Option<OfxImageEffectHandle> {
    instances.get(plugin_name).map(|&key| key as OfxImageEffectHandle)
}

// ── Clip handle lookup ───────────────────────────────────────────────

/// Maps clip handle (as usize) → (instance handle, clip name).
/// This allows the image effect suite to find which instance a clip belongs to.
static CLIP_TO_INSTANCE: LazyLock<Mutex<HashMap<usize, (usize, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn register_clip_mapping(clip_handle: OfxImageClipHandle, instance_handle: OfxImageEffectHandle, clip_name: &str) {
    CLIP_TO_INSTANCE.lock().unwrap().insert(
        clip_handle as usize,
        (instance_handle as usize, clip_name.to_string()),
    );
}

pub fn lookup_clip(clip_handle: OfxImageClipHandle) -> Option<(OfxImageEffectHandle, String)> {
    CLIP_TO_INSTANCE.lock().unwrap().get(&(clip_handle as usize)).map(|(h, n)| {
        (*h as OfxImageEffectHandle, n.clone())
    })
}
