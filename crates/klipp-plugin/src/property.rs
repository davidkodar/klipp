//! OFX Property System — the foundation every suite builds on.
//!
//! Every OFX handle (host, effect, clip, image, param) has a property set:
//! a named map of typed, indexed values. The global `PROP_STORE` maps
//! opaque `OfxPropertySetHandle` values (pointer-as-usize) to Rust
//! `PropertySet` instances.

use crate::ffi::*;
use std::collections::HashMap;
use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};
use std::sync::{LazyLock, Mutex};
use tracing::warn;

// ── Property value types ─────────────────────────────────────────────

/// A typed OFX property value. OFX supports 4 types, each indexed.
#[derive(Debug, Clone)]
pub enum PropValue {
    String(Vec<CString>),
    Double(Vec<f64>),
    Int(Vec<i32>),
    Pointer(Vec<*mut c_void>),
}

// Safety: Pointer values stored here are opaque handles managed by the host.
// The property store is always accessed under a Mutex.
unsafe impl Send for PropValue {}

/// A property set — a named collection of typed, indexed values.
#[derive(Debug, Default)]
pub struct PropertySet {
    props: HashMap<String, PropValue>,
}

impl PropertySet {
    pub fn set_string(&mut self, name: &str, index: usize, value: &str) {
        let entry = self
            .props
            .entry(name.to_string())
            .or_insert_with(|| PropValue::String(Vec::new()));
        if let PropValue::String(v) = entry {
            if v.len() <= index {
                v.resize(index + 1, CString::default());
            }
            v[index] = CString::new(value).unwrap_or_default();
        }
    }

    pub fn get_string(&self, name: &str, index: usize) -> Option<&CStr> {
        if let Some(PropValue::String(v)) = self.props.get(name) {
            v.get(index).map(|s| s.as_c_str())
        } else {
            None
        }
    }

    pub fn get_string_lossy(&self, name: &str, index: usize) -> Option<String> {
        self.get_string(name, index)
            .map(|cs| cs.to_string_lossy().into_owned())
    }

    pub fn set_double(&mut self, name: &str, index: usize, value: f64) {
        let entry = self
            .props
            .entry(name.to_string())
            .or_insert_with(|| PropValue::Double(Vec::new()));
        if let PropValue::Double(v) = entry {
            if v.len() <= index {
                v.resize(index + 1, 0.0);
            }
            v[index] = value;
        }
    }

    pub fn get_double(&self, name: &str, index: usize) -> Option<f64> {
        if let Some(PropValue::Double(v)) = self.props.get(name) {
            v.get(index).copied()
        } else {
            None
        }
    }

    pub fn set_int(&mut self, name: &str, index: usize, value: i32) {
        let entry = self
            .props
            .entry(name.to_string())
            .or_insert_with(|| PropValue::Int(Vec::new()));
        if let PropValue::Int(v) = entry {
            if v.len() <= index {
                v.resize(index + 1, 0);
            }
            v[index] = value;
        }
    }

    pub fn get_int(&self, name: &str, index: usize) -> Option<i32> {
        if let Some(PropValue::Int(v)) = self.props.get(name) {
            v.get(index).copied()
        } else {
            None
        }
    }

    pub fn set_pointer(&mut self, name: &str, index: usize, value: *mut c_void) {
        let entry = self
            .props
            .entry(name.to_string())
            .or_insert_with(|| PropValue::Pointer(Vec::new()));
        if let PropValue::Pointer(v) = entry {
            if v.len() <= index {
                v.resize(index + 1, std::ptr::null_mut());
            }
            v[index] = value;
        }
    }

    pub fn get_pointer(&self, name: &str, index: usize) -> Option<*mut c_void> {
        if let Some(PropValue::Pointer(v)) = self.props.get(name) {
            v.get(index).copied()
        } else {
            None
        }
    }

    pub fn dimension(&self, name: &str) -> i32 {
        match self.props.get(name) {
            Some(PropValue::String(v)) => v.len() as i32,
            Some(PropValue::Double(v)) => v.len() as i32,
            Some(PropValue::Int(v)) => v.len() as i32,
            Some(PropValue::Pointer(v)) => v.len() as i32,
            None => 0,
        }
    }

    pub fn has_property(&self, name: &str) -> bool {
        self.props.contains_key(name)
    }

    pub fn reset(&mut self, name: &str) {
        self.props.remove(name);
    }
}

// ── Global property store ────────────────────────────────────────────

static PROP_STORE: LazyLock<Mutex<HashMap<usize, Box<PropertySet>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Register a property set and return its opaque handle.
/// The handle is the stable address of the boxed PropertySet.
pub fn register_propset(ps: PropertySet) -> OfxPropertySetHandle {
    let boxed = Box::new(ps);
    let key = &*boxed as *const PropertySet as usize;
    PROP_STORE.lock().unwrap().insert(key, boxed);
    key as OfxPropertySetHandle
}

/// Access a property set by handle for reading/writing.
/// The callback receives a mutable reference to the PropertySet.
/// Returns the callback's return value, or None if the handle is invalid.
pub fn with_propset<R>(handle: OfxPropertySetHandle, f: impl FnOnce(&mut PropertySet) -> R) -> Option<R> {
    let key = handle as usize;
    let mut store = PROP_STORE.lock().unwrap();
    store.get_mut(&key).map(|ps| f(ps))
}

/// Remove a property set from the store.
pub fn remove_propset(handle: OfxPropertySetHandle) {
    let key = handle as usize;
    PROP_STORE.lock().unwrap().remove(&key);
}

// ── OfxPropertySuiteV1 implementation ────────────────────────────────

unsafe extern "C" fn prop_set_pointer_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    value: *mut c_void,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| ps.set_pointer(&name, index as usize, value)) {
        Some(()) => K_OFX_STAT_OK,
        None => {
            warn!("[OFX] propSetPointer: BAD_HANDLE handle={:?} prop={}", handle, name);
            K_OFX_STAT_ERR_BAD_HANDLE
        }
    }
}

unsafe extern "C" fn prop_set_string_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    value: *const c_char,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    let value = CStr::from_ptr(value).to_string_lossy();
    match with_propset(handle, |ps| ps.set_string(&name, index as usize, &value)) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_set_double_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    value: c_double,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| ps.set_double(&name, index as usize, value)) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_set_int_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    value: c_int,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| ps.set_int(&name, index as usize, value)) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_set_pointer_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    values: *const *mut c_void,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            ps.set_pointer(&name, i, *values.add(i));
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_set_string_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    values: *const *const c_char,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            let s = CStr::from_ptr(*values.add(i)).to_string_lossy();
            ps.set_string(&name, i, &s);
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_set_double_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    values: *const c_double,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            ps.set_double(&name, i, *values.add(i));
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_set_int_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    values: *const c_int,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            ps.set_int(&name, i, *values.add(i));
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_get_pointer_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    out: *mut *mut c_void,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| ps.get_pointer(&name, index as usize)) {
        Some(Some(ptr)) => {
            *out = ptr;
            K_OFX_STAT_OK
        }
        Some(None) => {
            // Return null for missing pointer properties — more permissive than ERR_BAD_INDEX
            *out = std::ptr::null_mut();
            K_OFX_STAT_OK
        }
        None => {
            warn!("[OFX] propGetPointer: BAD_HANDLE handle={:?} prop={}", handle, name);
            K_OFX_STAT_ERR_BAD_HANDLE
        }
    }
}

/// Static empty C string for default string property return.
static EMPTY_CSTR: &[u8] = b"\0";

unsafe extern "C" fn prop_get_string_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    out: *mut *const c_char,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        let has = ps.has_property(&name);
        let val = ps.get_string(&name, index as usize).map(|cs| cs.as_ptr());
        (has, val)
    }) {
        Some((_, Some(ptr))) => {
            *out = ptr;
            K_OFX_STAT_OK
        }
        Some((has, None)) => {
            if !has {
                tracing::debug!("[OFX] prop miss: '{}' idx {} → default \"\"", name, index);
            }
            *out = EMPTY_CSTR.as_ptr() as *const c_char;
            K_OFX_STAT_OK
        }
        None => {
            warn!("[OFX] propGetString: BAD_HANDLE handle={:?} prop={}", handle, name);
            K_OFX_STAT_ERR_BAD_HANDLE
        }
    }
}

unsafe extern "C" fn prop_get_double_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    out: *mut c_double,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        let has = ps.has_property(&name);
        let val = ps.get_double(&name, index as usize);
        (has, val)
    }) {
        Some((_, Some(val))) => {
            *out = val;
            K_OFX_STAT_OK
        }
        Some((has, None)) => {
            if !has {
                tracing::debug!("[OFX] prop miss: '{}' idx {} → default 0.0", name, index);
            }
            *out = 0.0;
            K_OFX_STAT_OK
        }
        None => {
            warn!("[OFX] propGetDouble: BAD_HANDLE handle={:?} prop={}", handle, name);
            K_OFX_STAT_ERR_BAD_HANDLE
        }
    }
}

unsafe extern "C" fn prop_get_int_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    out: *mut c_int,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        let has = ps.has_property(&name);
        let val = ps.get_int(&name, index as usize);
        (has, val)
    }) {
        Some((_, Some(val))) => {
            *out = val;
            K_OFX_STAT_OK
        }
        Some((has, None)) => {
            if !has {
                tracing::debug!("[OFX] prop miss: '{}' idx {} → default 0", name, index);
            }
            *out = 0;
            K_OFX_STAT_OK
        }
        None => {
            warn!("[OFX] propGetInt: BAD_HANDLE handle={:?} prop={}", handle, name);
            K_OFX_STAT_ERR_BAD_HANDLE
        }
    }
}

unsafe extern "C" fn prop_get_pointer_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    out: *mut *mut c_void,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            *out.add(i) = ps.get_pointer(&name, i).unwrap_or(std::ptr::null_mut());
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_get_string_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    out: *mut *const c_char,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            *out.add(i) = ps.get_string(&name, i)
                .map(|cs| cs.as_ptr())
                .unwrap_or(EMPTY_CSTR.as_ptr() as *const c_char);
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_get_double_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    out: *mut c_double,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            *out.add(i) = ps.get_double(&name, i).unwrap_or(0.0);
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_get_int_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    out: *mut c_int,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            *out.add(i) = ps.get_int(&name, i).unwrap_or(0);
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_reset_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| ps.reset(&name)) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn prop_get_dimension_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    out: *mut c_int,
) -> OfxStatus {
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| ps.dimension(&name)) {
        Some(dim) => {
            *out = dim;
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

/// Static OfxPropertySuiteV1 table — handed to plugins via fetchSuite.
pub static PROPERTY_SUITE_V1: OfxPropertySuiteV1 = OfxPropertySuiteV1 {
    prop_set_pointer: prop_set_pointer_fn,
    prop_set_string: prop_set_string_fn,
    prop_set_double: prop_set_double_fn,
    prop_set_int: prop_set_int_fn,
    prop_set_pointer_n: prop_set_pointer_n_fn,
    prop_set_string_n: prop_set_string_n_fn,
    prop_set_double_n: prop_set_double_n_fn,
    prop_set_int_n: prop_set_int_n_fn,
    prop_get_pointer: prop_get_pointer_fn,
    prop_get_string: prop_get_string_fn,
    prop_get_double: prop_get_double_fn,
    prop_get_int: prop_get_int_fn,
    prop_get_pointer_n: prop_get_pointer_n_fn,
    prop_get_string_n: prop_get_string_n_fn,
    prop_get_double_n: prop_get_double_n_fn,
    prop_get_int_n: prop_get_int_n_fn,
    prop_reset: prop_reset_fn,
    prop_get_dimension: prop_get_dimension_fn,
};

// ── Host property population ─────────────────────────────────────────

/// Populate a PropertySet with the host's capabilities and identity.
pub fn populate_host_properties(props: &mut PropertySet) {
    // Host identity
    props.set_string("OfxPropName", 0, "Klipp");
    props.set_string("OfxPropLabel", 0, "Klipp NLE");
    props.set_int("OfxPropVersion", 0, 0);
    props.set_int("OfxPropVersion", 1, 1);
    props.set_int("OfxPropVersion", 2, 0);
    props.set_string("OfxPropVersionLabel", 0, "0.1.0");
    props.set_int("OfxPropAPIVersion", 0, 1);
    props.set_int("OfxPropAPIVersion", 1, 5);

    // Image effect capabilities
    props.set_int("OfxImageEffectHostPropIsBackground", 0, 0);
    props.set_int("OfxImageEffectPropSupportsOverlays", 0, 0);
    props.set_int("OfxImageEffectPropSupportsMultiResolution", 0, 0);
    props.set_int("OfxImageEffectPropSupportsTiles", 0, 0);
    props.set_int("OfxImageEffectPropTemporalClipAccess", 0, 0);
    props.set_int("OfxImageEffectPropSupportsMultipleClipDepths", 0, 0);
    props.set_int("OfxImageEffectPropSupportsMultipleClipPARs", 0, 0);
    props.set_int("OfxImageEffectPropSetableFrameRate", 0, 0);
    props.set_int("OfxImageEffectPropSetableFielding", 0, 0);
    props.set_int("OfxParamHostPropSupportsCustomInteract", 0, 0);
    props.set_int("OfxParamHostPropSupportsStringAnimation", 0, 0);
    props.set_int("OfxParamHostPropSupportsChoiceAnimation", 0, 0);
    props.set_int("OfxParamHostPropSupportsBooleanAnimation", 0, 0);
    props.set_int("OfxParamHostPropSupportsCustomAnimation", 0, 0);
    props.set_int("OfxParamHostPropSupportsParametricAnimation", 0, 0);
    props.set_int("OfxParamHostPropMaxParameters", 0, -1); // unlimited
    props.set_int("OfxParamHostPropMaxPages", 0, 0);

    // Supported pixel depths: 8-bit only
    props.set_string("OfxImageEffectPropSupportedPixelDepths", 0, crate::constants::kOfxBitDepthByte);

    // Supported contexts: filter only
    props.set_string("OfxImageEffectPropSupportedContexts", 0, crate::constants::kOfxImageEffectContextFilter);

    // Supported pixel components: RGBA only
    props.set_string("OfxImageEffectPropSupportedComponents", 0, crate::constants::kOfxImageComponentRGBA);

    // Sequential render
    props.set_int("OfxImageEffectInstancePropSequentialRender", 0, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_property_set_round_trip() {
        let mut ps = PropertySet::default();

        ps.set_string("name", 0, "hello");
        assert_eq!(ps.get_string_lossy("name", 0).as_deref(), Some("hello"));

        ps.set_double("radius", 0, 3.14);
        assert_eq!(ps.get_double("radius", 0), Some(3.14));

        ps.set_int("count", 0, 42);
        assert_eq!(ps.get_int("count", 0), Some(42));

        ps.set_pointer("data", 0, 0xDEAD as *mut c_void);
        assert_eq!(ps.get_pointer("data", 0), Some(0xDEAD as *mut c_void));

        assert_eq!(ps.dimension("name"), 1);
        assert_eq!(ps.dimension("missing"), 0);

        ps.reset("name");
        assert_eq!(ps.dimension("name"), 0);
    }

    #[test]
    fn test_indexed_properties() {
        let mut ps = PropertySet::default();
        ps.set_int("OfxPropVersion", 0, 1);
        ps.set_int("OfxPropVersion", 1, 5);
        ps.set_int("OfxPropVersion", 2, 0);

        assert_eq!(ps.get_int("OfxPropVersion", 0), Some(1));
        assert_eq!(ps.get_int("OfxPropVersion", 1), Some(5));
        assert_eq!(ps.get_int("OfxPropVersion", 2), Some(0));
        assert_eq!(ps.dimension("OfxPropVersion"), 3);
    }

    #[test]
    fn test_global_store_register_and_access() {
        let mut ps = PropertySet::default();
        ps.set_string("test", 0, "value");
        let handle = register_propset(ps);

        assert!(handle != std::ptr::null_mut());

        let val = with_propset(handle, |ps| ps.get_string_lossy("test", 0));
        assert_eq!(val.unwrap().as_deref(), Some("value"));

        remove_propset(handle);
        assert!(with_propset(handle, |_| ()).is_none());
    }

    #[test]
    fn test_host_properties() {
        let mut ps = PropertySet::default();
        populate_host_properties(&mut ps);

        assert_eq!(ps.get_string_lossy("OfxPropName", 0).as_deref(), Some("Klipp"));
        assert_eq!(ps.get_int("OfxPropAPIVersion", 0), Some(1));
        assert_eq!(ps.get_int("OfxPropAPIVersion", 1), Some(5));
        assert_eq!(
            ps.get_string_lossy("OfxImageEffectPropSupportedPixelDepths", 0).as_deref(),
            Some("OfxBitDepthByte")
        );
    }
}
