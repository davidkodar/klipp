//! OFX Parameter Suite — plugin parameter definitions and values.
//!
//! During kOfxActionDescribe, plugins call paramDefine() to declare their
//! parameters. Each parameter has a type, label, range, default, etc.
//! The host reads these back to build inspector UI and manage values.

use crate::ffi::*;
use crate::property::PropertySet;
use std::collections::HashMap;
use std::ffi::{c_char, c_double, c_int, c_uint, c_void, CStr};
use std::sync::{LazyLock, Mutex};
use tracing::warn;

// ── Parameter types ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ParamType {
    Double,
    Double2,
    Double3,
    Int,
    Int2,
    Int3,
    Bool,
    Choice,
    String,
    Rgba,
    Rgb,
    Group,
    Page,
    Parametric,
    PushButton,
    Custom,
}

impl ParamType {
    /// Parse an OFX param type string to our enum.
    pub fn from_ofx_type(s: &str) -> Self {
        match s {
            "OfxParamTypeDouble" => Self::Double,
            "OfxParamTypeDouble2D" => Self::Double2,
            "OfxParamTypeDouble3D" => Self::Double3,
            "OfxParamTypeInteger" => Self::Int,
            "OfxParamTypeInteger2D" => Self::Int2,
            "OfxParamTypeInteger3D" => Self::Int3,
            "OfxParamTypeBoolean" => Self::Bool,
            "OfxParamTypeChoice" => Self::Choice,
            "OfxParamTypeString" | "OfxParamTypeCustom" => Self::String,
            "OfxParamTypeRGBA" => Self::Rgba,
            "OfxParamTypeRGB" => Self::Rgb,
            "OfxParamTypeGroup" => Self::Group,
            "OfxParamTypePage" => Self::Page,
            "OfxParamTypeParametric" => Self::Parametric,
            "OfxParamTypePushButton" => Self::PushButton,
            _ => Self::Custom,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ParamValue {
    Double(f64),
    Double2(f64, f64),
    Double3(f64, f64, f64),
    Int(i32),
    Int2(i32, i32),
    Int3(i32, i32, i32),
    Bool(bool),
    Choice(i32),
    String(String),
    Rgba(f64, f64, f64, f64),
    Rgb(f64, f64, f64),
    None,
}

/// Describes a parameter declared by a plugin during kOfxActionDescribe.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParamDescriptor {
    pub name: String,
    pub label: String,
    pub hint: String,
    pub param_type: ParamType,
    pub default: ParamValue,
    pub min: Option<ParamValue>,
    pub max: Option<ParamValue>,
    pub display_min: Option<ParamValue>,
    pub display_max: Option<ParamValue>,
    pub choices: Vec<String>,
    pub parent: Option<String>,
    pub enabled: bool,
    pub secret: bool,
}

impl ParamDescriptor {
    pub fn new(name: &str, param_type: ParamType) -> Self {
        Self {
            name: name.to_string(),
            label: name.to_string(),
            hint: String::new(),
            param_type,
            default: ParamValue::None,
            min: None,
            max: None,
            display_min: None,
            display_max: None,
            choices: Vec::new(),
            parent: None,
            enabled: true,
            secret: false,
        }
    }
}

// ── Per-param instance in the global store ───────────────────────────

pub struct ParamInstance {
    pub descriptor: ParamDescriptor,
    pub value: ParamValue,
    pub props_handle: OfxPropertySetHandle,
}

// Safety: props_handle is an opaque key into the global PROP_STORE (Mutex-protected).
unsafe impl Send for ParamInstance {}

/// Global param store: maps handle-as-usize to ParamInstance.
pub static PARAM_STORE: LazyLock<Mutex<HashMap<usize, ParamInstance>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Per param-set: maps param set handle → (list of param names, ordered)
static PARAM_SET_STORE: LazyLock<Mutex<HashMap<usize, ParamSetData>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct ParamSetData {
    /// Param names in definition order
    pub param_names: Vec<String>,
    /// Maps param name to its handle (key in PARAM_STORE)
    pub param_handles: HashMap<String, usize>,
    /// The param set's own property set handle
    pub props_handle: OfxPropertySetHandle,
}

// Safety: props_handle is an opaque key into the global PROP_STORE (Mutex-protected).
unsafe impl Send for ParamSetData {}

pub fn register_param_set() -> OfxParamSetHandle {
    let props = PropertySet::default();
    let props_handle = crate::property::register_propset(props);
    let data = ParamSetData {
        param_names: Vec::new(),
        param_handles: HashMap::new(),
        props_handle,
    };
    let key = props_handle as usize;
    PARAM_SET_STORE.lock().unwrap().insert(key, data);
    key as OfxParamSetHandle
}

pub fn with_param_set<R>(handle: OfxParamSetHandle, f: impl FnOnce(&mut ParamSetData) -> R) -> Option<R> {
    let key = handle as usize;
    PARAM_SET_STORE.lock().unwrap().get_mut(&key).map(f)
}

pub fn with_param<R>(handle: OfxParamHandle, f: impl FnOnce(&mut ParamInstance) -> R) -> Option<R> {
    let key = handle as usize;
    PARAM_STORE.lock().unwrap().get_mut(&key).map(f)
}

/// Read all param descriptors from a param set (after Describe/DescribeInContext).
/// Bridges property suite writes back into ParamDescriptor fields.
pub fn read_param_descriptors(param_set_handle: OfxParamSetHandle) -> Vec<ParamDescriptor> {
    let key = param_set_handle as usize;
    let store = PARAM_SET_STORE.lock().unwrap();
    let Some(pset) = store.get(&key) else {
        return Vec::new();
    };

    let mut param_store = PARAM_STORE.lock().unwrap();
    let mut descriptors = Vec::new();

    for name in &pset.param_names {
        let Some(&handle_key) = pset.param_handles.get(name) else {
            continue;
        };
        let Some(pi) = param_store.get_mut(&handle_key) else {
            continue;
        };

        // Read properties from the param's property set into the descriptor
        let ph = pi.props_handle;
        let pt = pi.descriptor.param_type.clone();

        crate::property::with_propset(ph, |ps| {
            if let Some(label) = ps.get_string_lossy("OfxPropLabel", 0) {
                pi.descriptor.label = label;
            }
            if let Some(hint) = ps.get_string_lossy("OfxParamPropHint", 0) {
                pi.descriptor.hint = hint;
            }
            if let Some(v) = read_typed_value(ps, "OfxParamPropDefault", &pt) {
                pi.descriptor.default = v;
            }
            pi.descriptor.min = read_typed_value(ps, "OfxParamPropMin", &pt);
            pi.descriptor.max = read_typed_value(ps, "OfxParamPropMax", &pt);
            pi.descriptor.display_min = read_typed_value(ps, "OfxParamPropDisplayMin", &pt);
            pi.descriptor.display_max = read_typed_value(ps, "OfxParamPropDisplayMax", &pt);

            // Secret
            if let Some(s) = ps.get_int("OfxParamPropSecret", 0) {
                pi.descriptor.secret = s != 0;
            }
            // Enabled
            if let Some(e) = ps.get_int("OfxParamPropEnabled", 0) {
                pi.descriptor.enabled = e != 0;
            }
            // Parent group
            if let Some(p) = ps.get_string_lossy("OfxParamPropParent", 0) {
                if !p.is_empty() {
                    pi.descriptor.parent = Some(p);
                }
            }
            // Choice options
            let n_choices = ps.dimension("OfxParamPropChoiceOption");
            if n_choices > 0 {
                pi.descriptor.choices.clear();
                for i in 0..n_choices as usize {
                    if let Some(c) = ps.get_string_lossy("OfxParamPropChoiceOption", i) {
                        pi.descriptor.choices.push(c);
                    }
                }
            }
        });

        // Set initial value to default if still None
        if matches!(pi.value, ParamValue::None) {
            pi.value = pi.descriptor.default.clone();
        }

        descriptors.push(pi.descriptor.clone());
    }
    descriptors
}

/// Read a typed value from a property set, dispatching on ParamType.
fn read_typed_value(
    ps: &crate::property::PropertySet,
    prop_name: &str,
    param_type: &ParamType,
) -> Option<ParamValue> {
    match param_type {
        ParamType::Double => ps.get_double(prop_name, 0).map(ParamValue::Double),
        ParamType::Double2 => {
            let a = ps.get_double(prop_name, 0)?;
            let b = ps.get_double(prop_name, 1).unwrap_or(0.0);
            Some(ParamValue::Double2(a, b))
        }
        ParamType::Double3 => {
            let a = ps.get_double(prop_name, 0)?;
            let b = ps.get_double(prop_name, 1).unwrap_or(0.0);
            let c = ps.get_double(prop_name, 2).unwrap_or(0.0);
            Some(ParamValue::Double3(a, b, c))
        }
        ParamType::Int => ps.get_int(prop_name, 0).map(ParamValue::Int),
        ParamType::Int2 => {
            let a = ps.get_int(prop_name, 0)?;
            let b = ps.get_int(prop_name, 1).unwrap_or(0);
            Some(ParamValue::Int2(a, b))
        }
        ParamType::Int3 => {
            let a = ps.get_int(prop_name, 0)?;
            let b = ps.get_int(prop_name, 1).unwrap_or(0);
            let c = ps.get_int(prop_name, 2).unwrap_or(0);
            Some(ParamValue::Int3(a, b, c))
        }
        ParamType::Bool => ps.get_int(prop_name, 0).map(|v| ParamValue::Bool(v != 0)),
        ParamType::Choice => ps.get_int(prop_name, 0).map(ParamValue::Choice),
        ParamType::Rgba => {
            let r = ps.get_double(prop_name, 0)?;
            let g = ps.get_double(prop_name, 1).unwrap_or(0.0);
            let b = ps.get_double(prop_name, 2).unwrap_or(0.0);
            let a = ps.get_double(prop_name, 3).unwrap_or(1.0);
            Some(ParamValue::Rgba(r, g, b, a))
        }
        ParamType::Rgb => {
            let r = ps.get_double(prop_name, 0)?;
            let g = ps.get_double(prop_name, 1).unwrap_or(0.0);
            let b = ps.get_double(prop_name, 2).unwrap_or(0.0);
            Some(ParamValue::Rgb(r, g, b))
        }
        ParamType::String => ps.get_string_lossy(prop_name, 0).map(ParamValue::String),
        _ => None,
    }
}

// ── OfxParameterSuiteV1 implementation ──────────────────────────────

unsafe extern "C" fn param_define_fn(
    param_set: OfxParamSetHandle,
    param_type: *const c_char,
    name: *const c_char,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    let type_str = CStr::from_ptr(param_type).to_string_lossy();
    let name_str = CStr::from_ptr(name).to_string_lossy();
    let pt = ParamType::from_ofx_type(&type_str);

    let descriptor = ParamDescriptor::new(&name_str, pt);
    let props = PropertySet::default();
    let props_handle = crate::property::register_propset(props);

    let pi = ParamInstance {
        descriptor,
        value: ParamValue::None,
        props_handle,
    };
    let handle_key = props_handle as usize;
    PARAM_STORE.lock().unwrap().insert(handle_key, pi);

    // Register in the param set
    let pset_key = param_set as usize;
    if let Some(pset) = PARAM_SET_STORE.lock().unwrap().get_mut(&pset_key) {
        pset.param_names.push(name_str.to_string());
        pset.param_handles.insert(name_str.to_string(), handle_key);
    }

    if !props_out.is_null() {
        *props_out = props_handle;
    }
    K_OFX_STAT_OK
}

unsafe extern "C" fn param_get_handle_fn(
    param_set: OfxParamSetHandle,
    name: *const c_char,
    handle_out: *mut OfxParamHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    let name_str = CStr::from_ptr(name).to_string_lossy();
    let pset_key = param_set as usize;
    let store = PARAM_SET_STORE.lock().unwrap();
    let Some(pset) = store.get(&pset_key) else {
        warn!("[OFX] paramGetHandle: BAD_HANDLE param_set={:?}", param_set);
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };
    let handle_key = if let Some(&key) = pset.param_handles.get(name_str.as_ref()) {
        key
    } else {
        // Param not found — create a dummy Bool param so plugins don't crash.
        // Natron-specific params (NatronOfxParam*) and optional params trigger this.
        tracing::debug!("[OFX] paramGetHandle: auto-creating missing param '{}'", name_str);
        drop(store); // release PARAM_SET_STORE lock before acquiring PARAM_STORE
        let mut props = PropertySet::default();
        props.set_string("OfxParamPropType", 0, "OfxParamTypeBoolean");
        props.set_string("OfxPropName", 0, &name_str);
        props.set_string("OfxPropLabel", 0, &name_str);
        props.set_int("OfxParamPropDefault", 0, 0);
        props.set_int("OfxParamPropSecret", 0, 1); // hide from UI
        let props_handle = crate::property::register_propset(props);
        let handle_key = props_handle as usize;
        let pi = ParamInstance {
            descriptor: ParamDescriptor::new(&name_str, ParamType::Bool),
            value: ParamValue::Bool(false),
            props_handle,
        };
        PARAM_STORE.lock().unwrap().insert(handle_key, pi);
        let mut store = PARAM_SET_STORE.lock().unwrap();
        if let Some(pset) = store.get_mut(&pset_key) {
            pset.param_names.push(name_str.to_string());
            pset.param_handles.insert(name_str.to_string(), handle_key);
        }
        handle_key
    };

    if !handle_out.is_null() {
        *handle_out = handle_key as OfxParamHandle;
    }
    if !props_out.is_null() {
        let param_store = PARAM_STORE.lock().unwrap();
        if let Some(pi) = param_store.get(&handle_key) {
            *props_out = pi.props_handle;
        }
    }
    K_OFX_STAT_OK
}

unsafe extern "C" fn param_set_get_property_set_fn(
    param_set: OfxParamSetHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    let key = param_set as usize;
    let store = PARAM_SET_STORE.lock().unwrap();
    match store.get(&key) {
        Some(pset) => {
            *props_out = pset.props_handle;
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn param_get_property_set_fn(
    handle: OfxParamHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    match with_param(handle, |pi| pi.props_handle) {
        Some(h) => {
            *props_out = h;
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

/// paramGetValue — reads the current value into output pointer(s).
/// The C signature is variadic but we receive it as a single *mut c_void
/// pointing to the first output argument. We dispatch based on param type.
unsafe extern "C" fn param_get_value_fn(handle: OfxParamHandle, out: *mut c_void) -> OfxStatus {
    let key = handle as usize;
    let store = PARAM_STORE.lock().unwrap();
    let Some(pi) = store.get(&key) else {
        warn!("[OFX] paramGetValue: BAD_HANDLE param={:?}", handle);
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    match &pi.value {
        ParamValue::Double(v) => {
            *(out as *mut f64) = *v;
        }
        ParamValue::Double2(a, b) => {
            *(out as *mut f64) = *a;
            *(out as *mut f64).add(1) = *b;
        }
        ParamValue::Double3(a, b, c) => {
            *(out as *mut f64) = *a;
            *(out as *mut f64).add(1) = *b;
            *(out as *mut f64).add(2) = *c;
        }
        ParamValue::Int(v) => {
            *(out as *mut i32) = *v;
        }
        ParamValue::Int2(a, b) => {
            *(out as *mut i32) = *a;
            *(out as *mut i32).add(1) = *b;
        }
        ParamValue::Int3(a, b, c) => {
            *(out as *mut i32) = *a;
            *(out as *mut i32).add(1) = *b;
            *(out as *mut i32).add(2) = *c;
        }
        ParamValue::Bool(v) => {
            *(out as *mut i32) = if *v { 1 } else { 0 };
        }
        ParamValue::Choice(v) => {
            *(out as *mut i32) = *v;
        }
        ParamValue::Rgba(r, g, b, a) => {
            *(out as *mut f64) = *r;
            *(out as *mut f64).add(1) = *g;
            *(out as *mut f64).add(2) = *b;
            *(out as *mut f64).add(3) = *a;
        }
        ParamValue::Rgb(r, g, b) => {
            *(out as *mut f64) = *r;
            *(out as *mut f64).add(1) = *g;
            *(out as *mut f64).add(2) = *b;
        }
        ParamValue::String(s) => {
            // Plugin expects a *const c_char pointer
            // We store a CString in the property set and return its pointer
            let cstr = std::ffi::CString::new(s.as_str()).unwrap_or_default();
            crate::property::with_propset(pi.props_handle, |ps| {
                ps.set_string("_string_cache", 0, s);
            });
            // Re-read to get a stable pointer
            if let Some(ptr) = crate::property::with_propset(pi.props_handle, |ps| {
                ps.get_string("_string_cache", 0).map(|cs| cs.as_ptr())
            }).flatten() {
                *(out as *mut *const c_char) = ptr;
            }
            let _ = cstr;
        }
        ParamValue::None => {
            // Value not set — use the descriptor's default
            let default = &pi.descriptor.default;
            match default {
                ParamValue::Double(v) => { *(out as *mut f64) = *v; }
                ParamValue::Int(v) => { *(out as *mut i32) = *v; }
                ParamValue::Bool(v) => { *(out as *mut i32) = if *v { 1 } else { 0 }; }
                ParamValue::Choice(v) => { *(out as *mut i32) = *v; }
                _ => {
                    // Last resort: write 0
                    *(out as *mut i32) = 0;
                }
            }
            return K_OFX_STAT_OK;
        }
    }
    K_OFX_STAT_OK
}

/// paramGetValueAtTime — same as getValue, we don't support animation yet.
unsafe extern "C" fn param_get_value_at_time_fn(
    handle: OfxParamHandle,
    _time: c_double,
    out: *mut c_void,
) -> OfxStatus {
    param_get_value_fn(handle, out)
}

unsafe extern "C" fn param_get_derivative_fn(
    _handle: OfxParamHandle,
    _time: c_double,
    _out: *mut c_void,
) -> OfxStatus {
    K_OFX_STAT_ERR_UNSUPPORTED
}

unsafe extern "C" fn param_get_integral_fn(
    _handle: OfxParamHandle,
    _time1: c_double,
    _time2: c_double,
    _out: *mut c_void,
) -> OfxStatus {
    K_OFX_STAT_ERR_UNSUPPORTED
}

/// paramSetValue — write value from input pointer(s).
unsafe extern "C" fn param_set_value_fn(handle: OfxParamHandle, input: *const c_void) -> OfxStatus {
    let key = handle as usize;
    let mut store = PARAM_STORE.lock().unwrap();
    let Some(pi) = store.get_mut(&key) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    match pi.descriptor.param_type {
        ParamType::Double => {
            pi.value = ParamValue::Double(*(input as *const f64));
        }
        ParamType::Double2 => {
            let p = input as *const f64;
            pi.value = ParamValue::Double2(*p, *p.add(1));
        }
        ParamType::Double3 => {
            let p = input as *const f64;
            pi.value = ParamValue::Double3(*p, *p.add(1), *p.add(2));
        }
        ParamType::Int => {
            pi.value = ParamValue::Int(*(input as *const i32));
        }
        ParamType::Int2 => {
            let p = input as *const i32;
            pi.value = ParamValue::Int2(*p, *p.add(1));
        }
        ParamType::Int3 => {
            let p = input as *const i32;
            pi.value = ParamValue::Int3(*p, *p.add(1), *p.add(2));
        }
        ParamType::Bool => {
            pi.value = ParamValue::Bool(*(input as *const i32) != 0);
        }
        ParamType::Choice => {
            pi.value = ParamValue::Choice(*(input as *const i32));
        }
        ParamType::Rgba => {
            let p = input as *const f64;
            pi.value = ParamValue::Rgba(*p, *p.add(1), *p.add(2), *p.add(3));
        }
        ParamType::Rgb => {
            let p = input as *const f64;
            pi.value = ParamValue::Rgb(*p, *p.add(1), *p.add(2));
        }
        ParamType::String => {
            let cstr = CStr::from_ptr(*(input as *const *const c_char));
            pi.value = ParamValue::String(cstr.to_string_lossy().to_string());
        }
        _ => {}
    }
    K_OFX_STAT_OK
}

unsafe extern "C" fn param_set_value_at_time_fn(
    handle: OfxParamHandle,
    _time: c_double,
    input: *const c_void,
) -> OfxStatus {
    param_set_value_fn(handle, input)
}

unsafe extern "C" fn param_get_num_keys_fn(
    _handle: OfxParamHandle,
    n: *mut c_uint,
) -> OfxStatus {
    *n = 0; // no keyframing yet
    K_OFX_STAT_OK
}

unsafe extern "C" fn param_get_key_time_fn(
    _handle: OfxParamHandle,
    _nth: c_uint,
    _time: *mut c_double,
) -> OfxStatus {
    K_OFX_STAT_ERR_BAD_INDEX
}

unsafe extern "C" fn param_get_key_index_fn(
    _handle: OfxParamHandle,
    _time: c_double,
    _direction: c_int,
    _index: *mut c_int,
) -> OfxStatus {
    K_OFX_STAT_ERR_VALUE
}

unsafe extern "C" fn param_delete_key_fn(
    _handle: OfxParamHandle,
    _time: c_double,
) -> OfxStatus {
    K_OFX_STAT_ERR_UNSUPPORTED
}

unsafe extern "C" fn param_delete_all_keys_fn(_handle: OfxParamHandle) -> OfxStatus {
    K_OFX_STAT_ERR_UNSUPPORTED
}

unsafe extern "C" fn param_copy_fn(
    _dst: OfxParamHandle,
    _src: OfxParamHandle,
    _offset: c_double,
    _range: *const OfxRangeD,
) -> OfxStatus {
    K_OFX_STAT_ERR_UNSUPPORTED
}

unsafe extern "C" fn param_edit_begin_fn(
    _param_set: OfxParamSetHandle,
    _name: *const c_char,
) -> OfxStatus {
    K_OFX_STAT_OK
}

unsafe extern "C" fn param_edit_end_fn(_param_set: OfxParamSetHandle) -> OfxStatus {
    K_OFX_STAT_OK
}

/// Static OfxParameterSuiteV1 table.
///
/// The variadic functions (paramGetValue, paramSetValue, etc.) are stored as
/// non-variadic function pointers. This works because:
/// 1. We transmute the fn pointer slots at suite construction time
/// 2. The C ABI guarantees that extra arguments are simply ignored by the callee
/// 3. Our implementations read the correct number of output args based on param type
#[allow(clippy::missing_transmute_annotations)]
pub static PARAM_SUITE_V1: OfxParameterSuiteV1 = {
    // Safety: transmuting non-variadic fns into variadic fn pointer slots.
    // The OFX spec defines these as variadic for convenience, but the actual
    // number and types of arguments are determined by the parameter type.
    unsafe {
        OfxParameterSuiteV1 {
            param_define: param_define_fn,
            param_get_handle: param_get_handle_fn,
            param_set_get_property_set: param_set_get_property_set_fn,
            param_get_property_set: param_get_property_set_fn,
            param_get_value: std::mem::transmute(param_get_value_fn as unsafe extern "C" fn(OfxParamHandle, *mut c_void) -> OfxStatus),
            param_get_value_at_time: std::mem::transmute(param_get_value_at_time_fn as unsafe extern "C" fn(OfxParamHandle, c_double, *mut c_void) -> OfxStatus),
            param_get_derivative: std::mem::transmute(param_get_derivative_fn as unsafe extern "C" fn(OfxParamHandle, c_double, *mut c_void) -> OfxStatus),
            param_get_integral: std::mem::transmute(param_get_integral_fn as unsafe extern "C" fn(OfxParamHandle, c_double, c_double, *mut c_void) -> OfxStatus),
            param_set_value: std::mem::transmute(param_set_value_fn as unsafe extern "C" fn(OfxParamHandle, *const c_void) -> OfxStatus),
            param_set_value_at_time: std::mem::transmute(param_set_value_at_time_fn as unsafe extern "C" fn(OfxParamHandle, c_double, *const c_void) -> OfxStatus),
            param_get_num_keys: param_get_num_keys_fn,
            param_get_key_time: param_get_key_time_fn,
            param_get_key_index: param_get_key_index_fn,
            param_delete_key: param_delete_key_fn,
            param_delete_all_keys: param_delete_all_keys_fn,
            param_copy: param_copy_fn,
            param_edit_begin: param_edit_begin_fn,
            param_edit_end: param_edit_end_fn,
        }
    }
};
