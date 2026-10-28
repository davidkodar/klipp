//! OFX Property System — the foundation every suite builds on.
//!
//! Every OFX handle (host, effect, clip, image, param) has a property set:
//! a named map of typed, indexed values. The global `PROP_STORE` maps
//! opaque `OfxPropertySetHandle` values (pointer-as-usize) to Rust
//! `PropertySet` instances.
//!
//! # Safety convention for the suite callbacks
//!
//! Every `unsafe extern "C" fn prop_*_fn` in this file follows the
//! same four-line pattern:
//!
//! 1. `ofx_guard!({ ... })` — the body runs inside `catch_unwind` so
//!    a Rust panic never crosses the FFI boundary (see the crate-
//!    level docs).
//! 2. `check_non_null!(name, out, ...)` — validates pointer args per
//!    OFX spec; returns `K_OFX_STAT_ERR_VALUE` on null instead of
//!    segfaulting.
//! 3. `CStr::from_ptr(name).to_string_lossy()` — sound because the
//!    preceding null-check rules out dereferencing a null pointer,
//!    and the OFX spec guarantees the caller passes a properly
//!    nul-terminated C string. Non-UTF-8 names map to the Unicode
//!    replacement character rather than failing.
//! 4. `*out = value` / `*out.add(i) = value` — sound because the
//!    null-check already rejected the null case; the caller supplied
//!    a pointer they own. Individual blocks do not need separate
//!    `// SAFETY:` comments because they all fit this pattern.
//!
//! `prop_get_string_fn` and `prop_get_string_n_fn` return a
//! `*const c_char` into a `CString` the property set owns, read after
//! the `PROP_STORE` mutex drops. A string replaced or reset is kept by
//! its set (`PropertySet::retired`) rather than freed, so a pointer a
//! plugin was given reads the same bytes until the set is destroyed,
//! as the OFX spec expects of a string property's value (#2449).

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
    /// Strings replaced or reset, kept until the set is destroyed: a
    /// plugin may still hold a pointer to one it was given (#2449).
    retired: Vec<CString>,
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
            // A CString's bytes are its own allocation, kept where they
            // are as the vector grows or the string moves to `retired`.
            let old = std::mem::replace(&mut v[index], CString::new(value).unwrap_or_default());
            self.retired.push(old);
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
        if let Some(PropValue::String(v)) = self.props.remove(name) {
            self.retired.extend(v);
        }
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
    PROP_STORE.lock().unwrap_or_else(|e| e.into_inner()).insert(key, boxed);
    key as OfxPropertySetHandle
}

/// Access a property set by handle for reading/writing.
/// The callback receives a mutable reference to the PropertySet.
/// Returns the callback's return value, or None if the handle is invalid.
pub fn with_propset<R>(handle: OfxPropertySetHandle, f: impl FnOnce(&mut PropertySet) -> R) -> Option<R> {
    let key = handle as usize;
    let mut store = PROP_STORE.lock().unwrap_or_else(|e| e.into_inner());
    store.get_mut(&key).map(|ps| f(ps))
}

/// Remove a property set from the store.
pub fn remove_propset(handle: OfxPropertySetHandle) {
    let key = handle as usize;
    PROP_STORE.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
}

// ── OfxPropertySuiteV1 implementation ────────────────────────────────

unsafe extern "C" fn prop_set_pointer_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    value: *mut c_void,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name);
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| ps.set_pointer(&name, index as usize, value)) {
        Some(()) => K_OFX_STAT_OK,
        None => {
            warn!("[OFX] propSetPointer: BAD_HANDLE handle={:?} prop={}", handle, name);
            K_OFX_STAT_ERR_BAD_HANDLE
        }
    }
    })
}

unsafe extern "C" fn prop_set_string_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    value: *const c_char,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, value);
    let name = CStr::from_ptr(name).to_string_lossy();
    let value = CStr::from_ptr(value).to_string_lossy();
    match with_propset(handle, |ps| ps.set_string(&name, index as usize, &value)) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn prop_set_double_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    value: c_double,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name);
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| ps.set_double(&name, index as usize, value)) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn prop_set_int_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    value: c_int,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name);
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| ps.set_int(&name, index as usize, value)) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn prop_set_pointer_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    values: *const *mut c_void,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, values);
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            ps.set_pointer(&name, i, *values.add(i));
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn prop_set_string_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    values: *const *const c_char,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, values);
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
    })
}

unsafe extern "C" fn prop_set_double_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    values: *const c_double,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, values);
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            ps.set_double(&name, i, *values.add(i));
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn prop_set_int_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    values: *const c_int,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, values);
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            ps.set_int(&name, i, *values.add(i));
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn prop_get_pointer_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    out: *mut *mut c_void,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, out);
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
    })
}

/// Static empty C string for default string property return.
static EMPTY_CSTR: &[u8] = b"\0";

unsafe extern "C" fn prop_get_string_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    out: *mut *const c_char,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, out);
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
    })
}

unsafe extern "C" fn prop_get_double_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    out: *mut c_double,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, out);
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
    })
}

unsafe extern "C" fn prop_get_int_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    index: c_int,
    out: *mut c_int,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, out);
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
    })
}

unsafe extern "C" fn prop_get_pointer_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    out: *mut *mut c_void,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, out);
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            *out.add(i) = ps.get_pointer(&name, i).unwrap_or(std::ptr::null_mut());
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn prop_get_string_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    out: *mut *const c_char,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, out);
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
    })
}

unsafe extern "C" fn prop_get_double_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    out: *mut c_double,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, out);
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            *out.add(i) = ps.get_double(&name, i).unwrap_or(0.0);
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn prop_get_int_n_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    count: c_int,
    out: *mut c_int,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, out);
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| {
        for i in 0..count as usize {
            *out.add(i) = ps.get_int(&name, i).unwrap_or(0);
        }
    }) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn prop_reset_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name);
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| ps.reset(&name)) {
        Some(()) => K_OFX_STAT_OK,
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn prop_get_dimension_fn(
    handle: OfxPropertySetHandle,
    name: *const c_char,
    out: *mut c_int,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name, out);
    let name = CStr::from_ptr(name).to_string_lossy();
    match with_propset(handle, |ps| ps.dimension(&name)) {
        Some(dim) => {
            *out = dim;
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
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
    // The host's version is Klipp's own, so it never falls behind a release.
    let part = |s: &str| s.parse::<i32>().unwrap_or(0);
    props.set_int("OfxPropVersion", 0, part(env!("CARGO_PKG_VERSION_MAJOR")));
    props.set_int("OfxPropVersion", 1, part(env!("CARGO_PKG_VERSION_MINOR")));
    props.set_int("OfxPropVersion", 2, part(env!("CARGO_PKG_VERSION_PATCH")));
    props.set_string("OfxPropVersionLabel", 0, env!("CARGO_PKG_VERSION"));
    props.set_int("OfxPropAPIVersion", 0, 1);
    props.set_int("OfxPropAPIVersion", 1, 5);

    // Image effect capabilities
    props.set_int("OfxImageEffectHostPropIsBackground", 0, 0);
    props.set_int("OfxImageEffectPropSupportsOverlays", 0, 0);
    props.set_int("OfxImageEffectPropSupportsMultiResolution", 0, 0);
    props.set_int("OfxImageEffectPropSupportsTiles", 0, 0);
    props.set_int("OfxImageEffectPropTemporalClipAccess", 0, 1);
    props.set_int("OfxImageEffectPropSupportsMultipleClipDepths", 0, 0);
    props.set_int("OfxImageEffectPropSupportsMultipleClipPARs", 0, 0);
    props.set_int("OfxImageEffectPropSetableFrameRate", 0, 0);
    props.set_int("OfxImageEffectPropSetableFielding", 0, 0);
    props.set_int("OfxParamHostPropSupportsCustomInteract", 0, 0);
    props.set_int("OfxParamHostPropSupportsStringAnimation", 0, 0); // string animation not practical
    props.set_int("OfxParamHostPropSupportsChoiceAnimation", 0, 1);
    props.set_int("OfxParamHostPropSupportsBooleanAnimation", 0, 1);
    props.set_int("OfxParamHostPropSupportsCustomAnimation", 0, 0);
    props.set_int("OfxParamHostPropSupportsParametricAnimation", 0, 0);
    props.set_int("OfxParamHostPropMaxParameters", 0, -1); // unlimited
    props.set_int("OfxParamHostPropMaxPages", 0, 0);

    // Supported pixel depths: byte and float
    props.set_string("OfxImageEffectPropSupportedPixelDepths", 0, crate::constants::kOfxBitDepthByte);
    props.set_string("OfxImageEffectPropSupportedPixelDepths", 1, crate::constants::kOfxBitDepthFloat);

    // Supported contexts: filter, transition, generator
    props.set_string("OfxImageEffectPropSupportedContexts", 0, crate::constants::kOfxImageEffectContextFilter);
    props.set_string("OfxImageEffectPropSupportedContexts", 1, crate::constants::kOfxImageEffectContextTransition);
    props.set_string("OfxImageEffectPropSupportedContexts", 2, crate::constants::kOfxImageEffectContextGenerator);

    // Supported pixel components: RGBA only
    props.set_string("OfxImageEffectPropSupportedComponents", 0, crate::constants::kOfxImageComponentRGBA);

    // Sequential render
    props.set_int("OfxImageEffectInstancePropSequentialRender", 0, 0);

    // GPU rendering capabilities — set dynamically by OfxHostRuntime
    // based on available hardware. Defaults to "false" here.
    props.set_string(crate::constants::kOfxImageEffectPropOpenGLRenderSupported, 0, "false");
    props.set_string(crate::constants::kOfxImageEffectPropCudaRenderSupported, 0, "false");
    props.set_string(crate::constants::kOfxImageEffectPropCudaStreamSupported, 0, "false");
    props.set_string(crate::constants::kOfxImageEffectPropMetalRenderSupported, 0, "false");
    props.set_string(crate::constants::kOfxImageEffectPropOpenCLRenderSupported, 0, "false");
    props.set_string(crate::constants::kOfxImageEffectPropOpenCLSupported, 0, "false");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_property_set_round_trip() {
        let mut ps = PropertySet::default();

        ps.set_string("name", 0, "hello");
        assert_eq!(ps.get_string_lossy("name", 0).as_deref(), Some("hello"));

        ps.set_double("radius", 0, 2.5);
        assert_eq!(ps.get_double("radius", 0), Some(2.5));

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

        assert!(!handle.is_null());

        let val = with_propset(handle, |ps| ps.get_string_lossy("test", 0));
        assert_eq!(val.unwrap().as_deref(), Some("value"));

        remove_propset(handle);
        assert!(with_propset(handle, |_| ()).is_none());
    }

    /// #2449: a string handed to a plugin stays readable after the
    /// property is set again or reset, until its set is destroyed.
    #[test]
    fn a_string_given_out_outlives_a_new_value() {
        let mut ps = PropertySet::default();
        ps.set_string("OfxPropLabel", 0, "first");
        let handle = register_propset(ps);
        let name = CString::new("OfxPropLabel").unwrap();
        let get = |index: c_int| {
            let mut out: *const c_char = std::ptr::null();
            // SAFETY: the suite's own entry point, given a live handle,
            // a NUL-terminated name and somewhere to write the pointer.
            let status = unsafe { (PROPERTY_SUITE_V1.prop_get_string)(handle, name.as_ptr(), index, &mut out) };
            assert_eq!(status, K_OFX_STAT_OK);
            out
        };
        let first = get(0);
        with_propset(handle, |ps| {
            ps.set_string("OfxPropLabel", 0, "second, and longer than the first");
            ps.set_string("OfxPropLabel", 3, "grown");
        });
        // SAFETY: the pointer the suite gave out; the set still exists.
        let read = |p: *const c_char| unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_owned();
        assert_eq!(read(first), "first", "the first pointer still reads the old bytes");
        // Kept by the set, the very allocation given out: reading freed
        // memory could pass the line above by chance.
        assert!(with_propset(handle, |ps| ps.retired.iter().any(|s| s.as_ptr() == first)).unwrap(), "the set keeps the string given out");
        assert_eq!(read(get(0)), "second, and longer than the first");
        let second = get(0);
        with_propset(handle, |ps| ps.reset("OfxPropLabel"));
        assert_eq!(read(second), "second, and longer than the first", "a reset keeps it too");
        remove_propset(handle);
    }

    #[test]
    fn test_host_properties() {
        let mut ps = PropertySet::default();
        populate_host_properties(&mut ps);

        assert_eq!(ps.get_string_lossy("OfxPropName", 0).as_deref(), Some("Klipp"));
        // Plug-ins are told Klipp's own version, as released.
        assert_eq!(ps.get_string_lossy("OfxPropVersionLabel", 0).as_deref(), Some(env!("CARGO_PKG_VERSION")));
        let parts: Vec<i32> = (0..3).map(|i| ps.get_int("OfxPropVersion", i).unwrap()).collect();
        assert_eq!(parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join("."), env!("CARGO_PKG_VERSION"));
        assert_eq!(ps.get_int("OfxPropAPIVersion", 0), Some(1));
        assert_eq!(ps.get_int("OfxPropAPIVersion", 1), Some(5));
        assert_eq!(
            ps.get_string_lossy("OfxImageEffectPropSupportedPixelDepths", 0).as_deref(),
            Some("OfxBitDepthByte")
        );
    }
}
