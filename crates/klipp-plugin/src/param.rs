//! OFX Parameter Suite — plugin parameter definitions and values.
//!
//! During kOfxActionDescribe, plugins call paramDefine() to declare their
//! parameters. Each parameter has a type, label, range, default, etc.
//! The host reads these back to build inspector UI and manage values.
//!
//! Supports keyframe animation: parameters can have time-varying values
//! stored as sorted keyframe lists with linear interpolation.
//!
//! # Safety convention for the suite callbacks
//!
//! Every `unsafe extern "C" fn param_*_fn` in this file is wrapped in
//! `ofx_guard!({ ... })` + opens with `check_non_null!(handle, out,
//! ...)`. Inside that envelope, `*out = value` / `*(out as *mut T) =
//! value` writes are sound: the null check has already fired, and
//! plugins that pass non-owned pointers violate the OFX spec.
//!
//! The suite's variadic members (paramGetValue, paramSetValue and their
//! AtTime, Derivative and Integral forms) are C functions in
//! `ofx_param_shim.c` (#2455): only C reads a `va_list`. Each asks
//! `klipp_ofx_param_shape` (`param_shape_fn` here) what the parameter holds, reads or writes
//! that many arguments, and calls one of the typed functions here with a
//! plain array.

use crate::ffi::*;
use crate::property::PropertySet;
use std::collections::HashMap;
use std::ffi::{c_char, c_double, c_int, c_uint, c_void, CStr};
use std::sync::{LazyLock, Mutex};
use tracing::warn;

// ── Keyframe ────────────────────────────────────────────────────────

/// A single keyframe: a (time, value) pair for parameter animation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Keyframe {
    pub time: f64,
    pub value: ParamValue,
}

const KEYFRAME_TIME_EPSILON: f64 = 1e-9;

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

impl ParamValue {
    /// This value as a parameter of type `ty` holds it, or `None` when it
    /// can't be one. A saved value can disagree with what the plug-in
    /// declares now (an update turned a double into an integer), and the
    /// host writes the declared type's bytes through the plug-in's
    /// pointer, so a value must be converted before it reaches an
    /// instance (#1017). Numbers convert between one another, rounding to
    /// whole numbers; vectors convert between vectors of the same length,
    /// and an RGB gains an opaque alpha or an RGBA loses it.
    pub fn as_declared(&self, ty: &ParamType) -> Option<ParamValue> {
        fn whole(v: f64) -> i32 {
            v.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32
        }
        let scalar = match self {
            Self::Double(v) => Some(*v),
            Self::Int(v) | Self::Choice(v) => Some(*v as f64),
            Self::Bool(v) => Some(if *v { 1.0 } else { 0.0 }),
            _ => None,
        };
        let pair = match self {
            Self::Double2(a, b) => Some((*a, *b)),
            Self::Int2(a, b) => Some((*a as f64, *b as f64)),
            _ => None,
        };
        let triple = match self {
            Self::Double3(a, b, c) | Self::Rgb(a, b, c) => Some((*a, *b, *c)),
            Self::Int3(a, b, c) => Some((*a as f64, *b as f64, *c as f64)),
            Self::Rgba(r, g, b, _) => Some((*r, *g, *b)),
            _ => None,
        };
        match ty {
            ParamType::Double => scalar.map(Self::Double),
            ParamType::Int => scalar.map(|v| Self::Int(whole(v))),
            ParamType::Bool => scalar.map(|v| Self::Bool(v != 0.0)),
            ParamType::Choice => scalar.map(|v| Self::Choice(whole(v))),
            ParamType::Double2 => pair.map(|(a, b)| Self::Double2(a, b)),
            ParamType::Int2 => pair.map(|(a, b)| Self::Int2(whole(a), whole(b))),
            ParamType::Double3 => triple.map(|(a, b, c)| Self::Double3(a, b, c)),
            ParamType::Int3 => triple.map(|(a, b, c)| Self::Int3(whole(a), whole(b), whole(c))),
            ParamType::Rgb => triple.map(|(r, g, b)| Self::Rgb(r, g, b)),
            ParamType::Rgba => match self {
                Self::Rgba(r, g, b, a) => Some(Self::Rgba(*r, *g, *b, *a)),
                _ => triple.map(|(r, g, b)| Self::Rgba(r, g, b, 1.0)),
            },
            ParamType::String => match self {
                Self::String(v) => Some(Self::String(v.clone())),
                _ => None,
            },
            ParamType::Group | ParamType::Page | ParamType::Parametric | ParamType::PushButton | ParamType::Custom => None,
        }
    }
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
    /// Keyframes for animation, sorted by time.
    pub keyframes: Vec<Keyframe>,
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
    PARAM_SET_STORE.lock().unwrap_or_else(|e| e.into_inner()).insert(key, data);
    key as OfxParamSetHandle
}

pub fn with_param_set<R>(handle: OfxParamSetHandle, f: impl FnOnce(&mut ParamSetData) -> R) -> Option<R> {
    let key = handle as usize;
    PARAM_SET_STORE.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&key).map(f)
}

pub fn with_param<R>(handle: OfxParamHandle, f: impl FnOnce(&mut ParamInstance) -> R) -> Option<R> {
    let key = handle as usize;
    PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&key).map(f)
}

/// Read all param descriptors from a param set (after Describe/DescribeInContext).
/// Bridges property suite writes back into ParamDescriptor fields.
pub fn read_param_descriptors(param_set_handle: OfxParamSetHandle) -> Vec<ParamDescriptor> {
    let key = param_set_handle as usize;
    let store = PARAM_SET_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pset) = store.get(&key) else {
        return Vec::new();
    };

    let mut param_store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
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

// ── Keyframe interpolation helpers ──────────────────────────────────

/// Linear interpolation between two ParamValues. `t` is in [0, 1].
/// Non-numeric types use step interpolation (return v1).
fn interpolate_param(v1: &ParamValue, v2: &ParamValue, t: f64) -> ParamValue {
    fn lerp(a: f64, b: f64, t: f64) -> f64 {
        a + (b - a) * t
    }
    fn lerp_i(a: i32, b: i32, t: f64) -> i32 {
        (a as f64 + (b as f64 - a as f64) * t).round() as i32
    }

    match (v1, v2) {
        (ParamValue::Double(a), ParamValue::Double(b)) => ParamValue::Double(lerp(*a, *b, t)),
        (ParamValue::Double2(a0, a1), ParamValue::Double2(b0, b1)) => {
            ParamValue::Double2(lerp(*a0, *b0, t), lerp(*a1, *b1, t))
        }
        (ParamValue::Double3(a0, a1, a2), ParamValue::Double3(b0, b1, b2)) => {
            ParamValue::Double3(lerp(*a0, *b0, t), lerp(*a1, *b1, t), lerp(*a2, *b2, t))
        }
        (ParamValue::Int(a), ParamValue::Int(b)) => ParamValue::Int(lerp_i(*a, *b, t)),
        (ParamValue::Int2(a0, a1), ParamValue::Int2(b0, b1)) => {
            ParamValue::Int2(lerp_i(*a0, *b0, t), lerp_i(*a1, *b1, t))
        }
        (ParamValue::Int3(a0, a1, a2), ParamValue::Int3(b0, b1, b2)) => {
            ParamValue::Int3(lerp_i(*a0, *b0, t), lerp_i(*a1, *b1, t), lerp_i(*a2, *b2, t))
        }
        (ParamValue::Rgba(r1, g1, b1, a1), ParamValue::Rgba(r2, g2, b2, a2)) => {
            ParamValue::Rgba(lerp(*r1, *r2, t), lerp(*g1, *g2, t), lerp(*b1, *b2, t), lerp(*a1, *a2, t))
        }
        (ParamValue::Rgb(r1, g1, b1), ParamValue::Rgb(r2, g2, b2)) => {
            ParamValue::Rgb(lerp(*r1, *r2, t), lerp(*g1, *g2, t), lerp(*b1, *b2, t))
        }
        // Step interpolation for non-numeric types (Bool, Choice, String)
        _ => v1.clone(),
    }
}

/// Get the interpolated value at a given time from a sorted keyframe list.
/// Returns None if no keyframes exist.
fn value_at_time(keyframes: &[Keyframe], time: f64) -> Option<ParamValue> {
    if keyframes.is_empty() {
        return None;
    }
    if keyframes.len() == 1 {
        return Some(keyframes[0].value.clone());
    }

    // Before first keyframe: hold first value
    if time <= keyframes[0].time {
        return Some(keyframes[0].value.clone());
    }
    // After last keyframe: hold last value
    if time >= keyframes[keyframes.len() - 1].time {
        return Some(keyframes[keyframes.len() - 1].value.clone());
    }

    // Find bracketing keyframes
    let idx = keyframes.partition_point(|k| k.time < time);
    if idx == 0 {
        return Some(keyframes[0].value.clone());
    }
    let k0 = &keyframes[idx - 1];
    let k1 = &keyframes[idx];
    let dt = k1.time - k0.time;
    if dt.abs() < KEYFRAME_TIME_EPSILON {
        return Some(k1.value.clone());
    }
    let t = (time - k0.time) / dt;
    Some(interpolate_param(&k0.value, &k1.value, t))
}

/// A value's numbers, each component, for derivative and integral math:
/// a 2D or 3D double, an RGB or RGBA has one per component.
fn components(v: &ParamValue) -> Vec<f64> {
    match v {
        ParamValue::Double(v) => vec![*v],
        ParamValue::Int(v) | ParamValue::Choice(v) => vec![*v as f64],
        ParamValue::Bool(v) => vec![if *v { 1.0 } else { 0.0 }],
        ParamValue::Double2(a, b) => vec![*a, *b],
        ParamValue::Double3(a, b, c) | ParamValue::Rgb(a, b, c) => vec![*a, *b, *c],
        ParamValue::Int2(a, b) => vec![*a as f64, *b as f64],
        ParamValue::Int3(a, b, c) => vec![*a as f64, *b as f64, *c as f64],
        ParamValue::Rgba(r, g, b, a) => vec![*r, *g, *b, *a],
        _ => Vec::new(),
    }
}

/// Write `vals` to `out`, as many as the parameter's type has: each
/// component of a derivative or an integral.
unsafe fn write_components(out: *mut c_void, vals: &[f64]) {
    write_f64s(out, &vals[..vals.len().min(4)]);
}

/// Insert a keyframe at the given time (sorted), replacing if time matches.
fn insert_keyframe(keyframes: &mut Vec<Keyframe>, time: f64, value: ParamValue) {
    let idx = keyframes.partition_point(|k| k.time < time - KEYFRAME_TIME_EPSILON);
    if idx < keyframes.len() && (keyframes[idx].time - time).abs() < KEYFRAME_TIME_EPSILON {
        keyframes[idx].value = value;
    } else {
        keyframes.insert(idx, Keyframe { time, value });
    }
}

/// Write a parameter's value to the plug-in's output pointer(s), as the
/// parameter's declared type and only that type's size, whatever the
/// value was stored as (#1017). A value that can't be the declared type
/// falls back to the declared default, then to zero. Shared by
/// paramGetValue and paramGetValueAtTime.
unsafe fn write_param_value(value: &ParamValue, pi: &ParamInstance, out: *mut c_void) {
    let ty = &pi.descriptor.param_type;
    let v = value.as_declared(ty).or_else(|| pi.descriptor.default.as_declared(ty));
    match ty {
        ParamType::Double => {
            let Some(ParamValue::Double(a)) = v else { return write_f64s(out, &[0.0]) };
            write_f64s(out, &[a]);
        }
        ParamType::Double2 => {
            let Some(ParamValue::Double2(a, b)) = v else { return write_f64s(out, &[0.0; 2]) };
            write_f64s(out, &[a, b]);
        }
        ParamType::Double3 => {
            let Some(ParamValue::Double3(a, b, c)) = v else { return write_f64s(out, &[0.0; 3]) };
            write_f64s(out, &[a, b, c]);
        }
        ParamType::Rgb => {
            let Some(ParamValue::Rgb(r, g, b)) = v else { return write_f64s(out, &[0.0; 3]) };
            write_f64s(out, &[r, g, b]);
        }
        ParamType::Rgba => {
            let Some(ParamValue::Rgba(r, g, b, a)) = v else { return write_f64s(out, &[0.0, 0.0, 0.0, 1.0]) };
            write_f64s(out, &[r, g, b, a]);
        }
        ParamType::Int | ParamType::Choice | ParamType::Bool => {
            *(out as *mut i32) = match v {
                Some(ParamValue::Int(a) | ParamValue::Choice(a)) => a,
                Some(ParamValue::Bool(a)) => a as i32,
                _ => 0,
            };
        }
        ParamType::Int2 => {
            let Some(ParamValue::Int2(a, b)) = v else { return write_i32s(out, &[0; 2]) };
            write_i32s(out, &[a, b]);
        }
        ParamType::Int3 => {
            let Some(ParamValue::Int3(a, b, c)) = v else { return write_i32s(out, &[0; 3]) };
            write_i32s(out, &[a, b, c]);
        }
        ParamType::String => {
            // The plug-in expects a *const c_char that outlives the call,
            // so the string is kept in the parameter's property set.
            let s = match v {
                Some(ParamValue::String(s)) => s,
                _ => String::new(),
            };
            let ptr = crate::property::with_propset(pi.props_handle, |ps| {
                ps.set_string("_string_cache", 0, &s);
                ps.get_string("_string_cache", 0).map(|cs| cs.as_ptr())
            })
            .flatten();
            *(out as *mut *const c_char) = ptr.unwrap_or(c"".as_ptr());
        }
        // A group, page, button or custom parameter has no value to read.
        ParamType::Group | ParamType::Page | ParamType::Parametric | ParamType::PushButton | ParamType::Custom => {}
    }
}

/// Write `vals` to consecutive doubles at `out`.
unsafe fn write_f64s(out: *mut c_void, vals: &[f64]) {
    for (i, v) in vals.iter().enumerate() {
        *(out as *mut f64).add(i) = *v;
    }
}

/// Write `vals` to consecutive 32-bit integers at `out`.
unsafe fn write_i32s(out: *mut c_void, vals: &[i32]) {
    for (i, v) in vals.iter().enumerate() {
        *(out as *mut i32).add(i) = *v;
    }
}

// ── OfxParameterSuiteV1 implementation ──────────────────────────────

pub(crate) unsafe extern "C" fn param_define_fn(
    param_set: OfxParamSetHandle,
    param_type: *const c_char,
    name: *const c_char,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(param_type, name);
    // props_out is optional by OFX convention: if the plugin does not need
    // the handle back it may pass null. The write below is guarded.
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
        keyframes: Vec::new(),
    };
    let handle_key = props_handle as usize;
    PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner()).insert(handle_key, pi);

    // Register in the param set
    let pset_key = param_set as usize;
    if let Some(pset) = PARAM_SET_STORE.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&pset_key) {
        pset.param_names.push(name_str.to_string());
        pset.param_handles.insert(name_str.to_string(), handle_key);
    }

    if !props_out.is_null() {
        *props_out = props_handle;
    }
    K_OFX_STAT_OK
    })
}

unsafe extern "C" fn param_get_handle_fn(
    param_set: OfxParamSetHandle,
    name: *const c_char,
    handle_out: *mut OfxParamHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name);
    // handle_out and props_out writes below are individually guarded so the
    // plugin can opt out of either return value.
    let name_str = CStr::from_ptr(name).to_string_lossy();
    let pset_key = param_set as usize;
    let store = PARAM_SET_STORE.lock().unwrap_or_else(|e| e.into_inner());
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
            keyframes: Vec::new(),
        };
        PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner()).insert(handle_key, pi);
        let mut store = PARAM_SET_STORE.lock().unwrap_or_else(|e| e.into_inner());
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
        let param_store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pi) = param_store.get(&handle_key) {
            *props_out = pi.props_handle;
        }
    }
    K_OFX_STAT_OK
    })
}

unsafe extern "C" fn param_set_get_property_set_fn(
    param_set: OfxParamSetHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(props_out);
    let key = param_set as usize;
    let store = PARAM_SET_STORE.lock().unwrap_or_else(|e| e.into_inner());
    match store.get(&key) {
        Some(pset) => {
            *props_out = pset.props_handle;
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn param_get_property_set_fn(
    handle: OfxParamHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(props_out);
    match with_param(handle, |pi| pi.props_handle) {
        Some(h) => {
            *props_out = h;
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

/// paramGetValue — the current value into `out`: the shim's array of the
/// parameter's components (doubles or 32-bit ints), or a string pointer.
pub(crate) unsafe extern "C" fn param_get_value_fn(handle: OfxParamHandle, out: *mut c_void) -> OfxStatus {
    ofx_guard!({
    check_non_null!(out);
    let key = handle as usize;
    let store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pi) = store.get(&key) else {
        warn!("[OFX] paramGetValue: BAD_HANDLE param={:?}", handle);
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    write_param_value(&pi.value, pi, out);
    K_OFX_STAT_OK
    })
}

/// paramGetValueAtTime — returns interpolated value at the given time.
/// Falls back to the current value if no keyframes are set.
unsafe extern "C" fn param_get_value_at_time_fn(
    handle: OfxParamHandle,
    time: c_double,
    out: *mut c_void,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(out);
    let key = handle as usize;
    let store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pi) = store.get(&key) else {
        warn!("[OFX] paramGetValueAtTime: BAD_HANDLE param={:?}", handle);
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    // If keyframes exist, interpolate; otherwise use current value
    let value = if !pi.keyframes.is_empty() {
        value_at_time(&pi.keyframes, time).unwrap_or_else(|| pi.value.clone())
    } else {
        pi.value.clone()
    };

    write_param_value(&value, pi, out);
    K_OFX_STAT_OK
    })
}

/// paramGetDerivative — returns the rate of change at a given time.
unsafe extern "C" fn param_get_derivative_fn(
    handle: OfxParamHandle,
    time: c_double,
    out: *mut c_void,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(out);
    let key = handle as usize;
    let store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pi) = store.get(&key) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    let n = components(&pi.value).len().max(1);
    if pi.keyframes.len() < 2 {
        // Constant — derivative is 0
        write_components(out, &vec![0.0; n]);
        return K_OFX_STAT_OK;
    }

    // Find bracketing keyframes and compute slope
    let kf = &pi.keyframes;
    let idx = kf.partition_point(|k| k.time < time);
    let (k0, k1) = if idx == 0 {
        (&kf[0], &kf[1])
    } else if idx >= kf.len() {
        (&kf[kf.len() - 2], &kf[kf.len() - 1])
    } else {
        (&kf[idx - 1], &kf[idx])
    };

    let dt = k1.time - k0.time;
    let derivative: Vec<f64> = if dt.abs() < KEYFRAME_TIME_EPSILON {
        vec![0.0; n]
    } else {
        components(&k1.value).iter().zip(components(&k0.value)).map(|(b, a)| (b - a) / dt).collect()
    };

    write_components(out, &derivative);
    K_OFX_STAT_OK
    })
}

/// paramGetIntegral — returns the area under the curve between time1 and time2.
/// Uses trapezoidal integration over linear keyframe segments.
unsafe extern "C" fn param_get_integral_fn(
    handle: OfxParamHandle,
    time1: c_double,
    time2: c_double,
    out: *mut c_void,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(out);
    let key = handle as usize;
    let store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pi) = store.get(&key) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    if pi.keyframes.is_empty() {
        // Constant value — integral = value * duration
        let v: Vec<f64> = components(&pi.value).iter().map(|v| v * (time2 - time1)).collect();
        write_components(out, &v);
        return K_OFX_STAT_OK;
    }

    // Trapezoidal integration, each component: sample at keyframe
    // boundaries within [time1, time2]
    let kf = &pi.keyframes;
    let mut t_prev = time1;
    let mut v_prev = components(&value_at_time(kf, time1).unwrap_or_else(|| pi.value.clone()));
    let mut integral = vec![0.0; v_prev.len()];
    let add = |integral: &mut Vec<f64>, a: &[f64], b: &[f64], dt: f64| {
        for (i, (a, b)) in a.iter().zip(b).enumerate() {
            integral[i] += (a + b) * 0.5 * dt;
        }
    };

    for k in kf {
        if k.time <= time1 {
            continue;
        }
        if k.time >= time2 {
            break;
        }
        let v_curr = components(&k.value);
        add(&mut integral, &v_prev, &v_curr, k.time - t_prev);
        t_prev = k.time;
        v_prev = v_curr;
    }

    // Final segment to time2
    let v_end = components(&value_at_time(kf, time2).unwrap_or_else(|| pi.value.clone()));
    add(&mut integral, &v_prev, &v_end, time2 - t_prev);

    write_components(out, &integral);
    K_OFX_STAT_OK
    })
}

/// paramSetValue — write value from input pointer(s).
unsafe extern "C" fn param_set_value_fn(handle: OfxParamHandle, input: *const c_void) -> OfxStatus {
    ofx_guard!({
    check_non_null!(input);
    let key = handle as usize;
    let mut store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
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
    })
}

/// paramSetValueAtTime — inserts or replaces a keyframe at the given time.
unsafe extern "C" fn param_set_value_at_time_fn(
    handle: OfxParamHandle,
    time: c_double,
    input: *const c_void,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(input);
    let key = handle as usize;
    let mut store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pi) = store.get_mut(&key) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    // Read the value based on param type
    let value = match pi.descriptor.param_type {
        ParamType::Double => ParamValue::Double(*(input as *const f64)),
        ParamType::Double2 => {
            let p = input as *const f64;
            ParamValue::Double2(*p, *p.add(1))
        }
        ParamType::Double3 => {
            let p = input as *const f64;
            ParamValue::Double3(*p, *p.add(1), *p.add(2))
        }
        ParamType::Int => ParamValue::Int(*(input as *const i32)),
        ParamType::Int2 => {
            let p = input as *const i32;
            ParamValue::Int2(*p, *p.add(1))
        }
        ParamType::Int3 => {
            let p = input as *const i32;
            ParamValue::Int3(*p, *p.add(1), *p.add(2))
        }
        ParamType::Bool => ParamValue::Bool(*(input as *const i32) != 0),
        ParamType::Choice => ParamValue::Choice(*(input as *const i32)),
        ParamType::Rgba => {
            let p = input as *const f64;
            ParamValue::Rgba(*p, *p.add(1), *p.add(2), *p.add(3))
        }
        ParamType::Rgb => {
            let p = input as *const f64;
            ParamValue::Rgb(*p, *p.add(1), *p.add(2))
        }
        ParamType::String => {
            let cstr = CStr::from_ptr(*(input as *const *const c_char));
            ParamValue::String(cstr.to_string_lossy().to_string())
        }
        _ => return K_OFX_STAT_OK,
    };

    insert_keyframe(&mut pi.keyframes, time, value);
    K_OFX_STAT_OK
    })
}

unsafe extern "C" fn param_get_num_keys_fn(
    handle: OfxParamHandle,
    n: *mut c_uint,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(n);
    let key = handle as usize;
    let store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
    match store.get(&key) {
        Some(pi) => {
            *n = pi.keyframes.len() as c_uint;
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn param_get_key_time_fn(
    handle: OfxParamHandle,
    nth: c_uint,
    time: *mut c_double,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(time);
    let key = handle as usize;
    let store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pi) = store.get(&key) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };
    match pi.keyframes.get(nth as usize) {
        Some(kf) => {
            *time = kf.time;
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_BAD_INDEX,
    }
    })
}

unsafe extern "C" fn param_get_key_index_fn(
    handle: OfxParamHandle,
    time: c_double,
    direction: c_int,
    index: *mut c_int,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(index);
    let key = handle as usize;
    let store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pi) = store.get(&key) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    if pi.keyframes.is_empty() {
        return K_OFX_STAT_ERR_VALUE;
    }

    let kf = &pi.keyframes;
    let pos = kf.partition_point(|k| k.time < time - KEYFRAME_TIME_EPSILON);

    match direction {
        0 => {
            // Exact match
            if pos < kf.len() && (kf[pos].time - time).abs() < KEYFRAME_TIME_EPSILON {
                *index = pos as c_int;
                K_OFX_STAT_OK
            } else {
                K_OFX_STAT_ERR_VALUE
            }
        }
        -1 => {
            // At or before
            if pos < kf.len() && (kf[pos].time - time).abs() < KEYFRAME_TIME_EPSILON {
                *index = pos as c_int;
                K_OFX_STAT_OK
            } else if pos > 0 {
                *index = (pos - 1) as c_int;
                K_OFX_STAT_OK
            } else {
                K_OFX_STAT_ERR_VALUE
            }
        }
        1 => {
            // At or after
            if pos < kf.len() {
                *index = pos as c_int;
                K_OFX_STAT_OK
            } else {
                K_OFX_STAT_ERR_VALUE
            }
        }
        _ => K_OFX_STAT_ERR_VALUE,
    }
    })
}

unsafe extern "C" fn param_delete_key_fn(
    handle: OfxParamHandle,
    time: c_double,
) -> OfxStatus {
    ofx_guard!({
    let key = handle as usize;
    let mut store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pi) = store.get_mut(&key) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    let pos = pi.keyframes.iter().position(|k| (k.time - time).abs() < KEYFRAME_TIME_EPSILON);
    match pos {
        Some(idx) => {
            pi.keyframes.remove(idx);
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_VALUE,
    }
    })
}

unsafe extern "C" fn param_delete_all_keys_fn(handle: OfxParamHandle) -> OfxStatus {
    ofx_guard!({
    let key = handle as usize;
    let mut store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pi) = store.get_mut(&key) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };
    pi.keyframes.clear();
    K_OFX_STAT_OK
    })
}

unsafe extern "C" fn param_copy_fn(
    dst: OfxParamHandle,
    src: OfxParamHandle,
    offset: c_double,
    range: *const OfxRangeD,
) -> OfxStatus {
    ofx_guard!({
    let src_key = src as usize;
    let dst_key = dst as usize;
    let mut store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());

    // Extract source keyframes
    let Some(src_pi) = store.get(&src_key) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };
    let src_keyframes: Vec<Keyframe> = if range.is_null() {
        src_pi.keyframes.iter().map(|k| Keyframe {
            time: k.time + offset,
            value: k.value.clone(),
        }).collect()
    } else {
        let r = &*range;
        src_pi.keyframes.iter()
            .filter(|k| k.time >= r.min && k.time <= r.max)
            .map(|k| Keyframe {
                time: k.time + offset,
                value: k.value.clone(),
            }).collect()
    };
    let src_value = src_pi.value.clone();

    // Apply to destination
    let Some(dst_pi) = store.get_mut(&dst_key) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };
    // The two parameters may be of different types; only what the
    // destination can hold is copied, as its own type (#1017).
    let ty = dst_pi.descriptor.param_type.clone();
    if src_keyframes.is_empty() {
        // No keyframes — just copy the current value
        if let Some(v) = src_value.as_declared(&ty) {
            dst_pi.value = v;
        }
    } else {
        for kf in src_keyframes {
            if let Some(v) = kf.value.as_declared(&ty) {
                insert_keyframe(&mut dst_pi.keyframes, kf.time, v);
            }
        }
    }
    K_OFX_STAT_OK
    })
}

unsafe extern "C" fn param_edit_begin_fn(
    _param_set: OfxParamSetHandle,
    _name: *const c_char,
) -> OfxStatus {
    ofx_guard!({
    K_OFX_STAT_OK
    })
}

unsafe extern "C" fn param_edit_end_fn(_param_set: OfxParamSetHandle) -> OfxStatus {
    ofx_guard!({
    K_OFX_STAT_OK
    })
}

// ── The variadic members, through the C shim ──────────────────────

/// What a parameter's value is, for `ofx_param_shim.c`.
const PARAM_NONE: c_int = 0;
const PARAM_DOUBLE: c_int = 1;
const PARAM_INT: c_int = 2;
const PARAM_STRING: c_int = 3;

/// What the parameter holds, for the C shim to read or write that many
/// arguments: its kind (double, int or string; none for a group, page,
/// button or custom parameter) and how many components.
#[export_name = "klipp_ofx_param_shape"]
unsafe extern "C" fn param_shape_fn(handle: OfxParamHandle, kind: *mut c_int, count: *mut c_int) -> OfxStatus {
    ofx_guard!({
    check_non_null!(kind, count);
    let store = PARAM_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pi) = store.get(&(handle as usize)) else {
        warn!("[OFX] paramGetValue/SetValue: BAD_HANDLE param={:?}", handle);
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };
    let (k, n) = match pi.descriptor.param_type {
        ParamType::Double => (PARAM_DOUBLE, 1),
        ParamType::Double2 => (PARAM_DOUBLE, 2),
        ParamType::Double3 | ParamType::Rgb => (PARAM_DOUBLE, 3),
        ParamType::Rgba => (PARAM_DOUBLE, 4),
        ParamType::Int | ParamType::Bool | ParamType::Choice => (PARAM_INT, 1),
        ParamType::Int2 => (PARAM_INT, 2),
        ParamType::Int3 => (PARAM_INT, 3),
        ParamType::String => (PARAM_STRING, 1),
        ParamType::Group | ParamType::Page | ParamType::Parametric | ParamType::PushButton | ParamType::Custom => (PARAM_NONE, 0),
    };
    (*kind, *count) = (k, n);
    K_OFX_STAT_OK
    })
}

/// The value now, or at `time` when `at_time`, into the shim's array.
#[export_name = "klipp_ofx_param_get_typed"]
unsafe extern "C" fn param_get_typed_fn(handle: OfxParamHandle, at_time: c_int, time: c_double, out: *mut c_void) -> OfxStatus {
    if at_time != 0 { param_get_value_at_time_fn(handle, time, out) } else { param_get_value_fn(handle, out) }
}

/// The value from the shim's array, set now, or keyed at `time` when `at_time`.
#[export_name = "klipp_ofx_param_set_typed"]
unsafe extern "C" fn param_set_typed_fn(handle: OfxParamHandle, at_time: c_int, time: c_double, input: *const c_void) -> OfxStatus {
    if at_time != 0 { param_set_value_at_time_fn(handle, time, input) } else { param_set_value_fn(handle, input) }
}

#[export_name = "klipp_ofx_param_derivative_typed"]
unsafe extern "C" fn param_derivative_typed_fn(handle: OfxParamHandle, time: c_double, out: *mut c_double) -> OfxStatus {
    param_get_derivative_fn(handle, time, out as *mut c_void)
}

#[export_name = "klipp_ofx_param_integral_typed"]
unsafe extern "C" fn param_integral_typed_fn(handle: OfxParamHandle, time1: c_double, time2: c_double, out: *mut c_double) -> OfxStatus {
    param_get_integral_fn(handle, time1, time2, out as *mut c_void)
}

// In ofx_param_shim.c: the OFX signatures, variadic as C defines them.
extern "C" {
    fn klipp_ofx_param_get_value(handle: OfxParamHandle, ...) -> OfxStatus;
    fn klipp_ofx_param_get_value_at_time(handle: OfxParamHandle, time: c_double, ...) -> OfxStatus;
    fn klipp_ofx_param_get_derivative(handle: OfxParamHandle, time: c_double, ...) -> OfxStatus;
    fn klipp_ofx_param_get_integral(handle: OfxParamHandle, time1: c_double, time2: c_double, ...) -> OfxStatus;
    fn klipp_ofx_param_set_value(handle: OfxParamHandle, ...) -> OfxStatus;
    fn klipp_ofx_param_set_value_at_time(handle: OfxParamHandle, time: c_double, ...) -> OfxStatus;
}

/// Static OfxParameterSuiteV1 table. Its variadic members are the C
/// shim's, which read their arguments as C passes them (#2455).
pub static PARAM_SUITE_V1: OfxParameterSuiteV1 = OfxParameterSuiteV1 {
    param_define: param_define_fn,
    param_get_handle: param_get_handle_fn,
    param_set_get_property_set: param_set_get_property_set_fn,
    param_get_property_set: param_get_property_set_fn,
    param_get_value: klipp_ofx_param_get_value,
    param_get_value_at_time: klipp_ofx_param_get_value_at_time,
    param_get_derivative: klipp_ofx_param_get_derivative,
    param_get_integral: klipp_ofx_param_get_integral,
    param_set_value: klipp_ofx_param_set_value,
    param_set_value_at_time: klipp_ofx_param_set_value_at_time,
    param_get_num_keys: param_get_num_keys_fn,
    param_get_key_time: param_get_key_time_fn,
    param_get_key_index: param_get_key_index_fn,
    param_delete_key: param_delete_key_fn,
    param_delete_all_keys: param_delete_all_keys_fn,
    param_copy: param_copy_fn,
    param_edit_begin: param_edit_begin_fn,
    param_edit_end: param_edit_end_fn,
};

#[cfg(test)]
mod variadic_tests {
    //! The suite's variadic members called as a C plug-in calls them
    //! (#2455): one pointer per component to read into, each its own
    //! variable, and values passed by value, promoted as C promotes them.
    use super::*;

    /// A parameter of `ofx_type` in a set of its own.
    fn param(ofx_type: &std::ffi::CStr) -> OfxParamHandle {
        let pset = register_param_set();
        let mut props: OfxPropertySetHandle = std::ptr::null_mut();
        // SAFETY: the suite's define, given a live set and NUL-terminated names.
        assert_eq!(unsafe { param_define_fn(pset, ofx_type.as_ptr(), c"P".as_ptr(), &mut props) }, K_OFX_STAT_OK);
        with_param_set(pset, |s| s.param_handles["P"]).unwrap() as OfxParamHandle
    }

    #[test]
    fn every_kind_of_value_is_set_and_read_through_the_varargs() {
        let s = &PARAM_SUITE_V1;
        // SAFETY (all blocks): the suite's own entry points, given live
        // handles and as many arguments as each parameter's type has.
        unsafe {
            let h = param(c"OfxParamTypeDouble");
            assert_eq!((s.param_set_value)(h, 0.25f64), K_OFX_STAT_OK);
            let mut a = 0.0f64;
            assert_eq!((s.param_get_value)(h, &mut a as *mut f64), K_OFX_STAT_OK);
            assert_eq!(a, 0.25);

            let h = param(c"OfxParamTypeDouble2D");
            (s.param_set_value)(h, 1.5f64, -2.0f64);
            let (mut x, mut y) = (0.0f64, 0.0f64);
            (s.param_get_value)(h, &mut x as *mut f64, &mut y as *mut f64);
            assert_eq!((x, y), (1.5, -2.0));

            for (ty, n) in [(c"OfxParamTypeDouble3D", 3), (c"OfxParamTypeRGB", 3), (c"OfxParamTypeRGBA", 4)] {
                let h = param(ty);
                match n {
                    3 => (s.param_set_value)(h, 0.1f64, 0.2f64, 0.3f64),
                    _ => (s.param_set_value)(h, 0.1f64, 0.2f64, 0.3f64, 0.4f64),
                };
                let (mut a, mut b, mut c, mut d) = (0.0f64, 0.0f64, 0.0f64, 9.0f64);
                (s.param_get_value)(h, &mut a as *mut f64, &mut b as *mut f64, &mut c as *mut f64, &mut d as *mut f64);
                assert_eq!((a, b, c), (0.1, 0.2, 0.3), "{ty:?}");
                assert_eq!(d, if n == 4 { 0.4 } else { 9.0 }, "{ty:?}: no more than its components are written");
            }

            for (ty, v) in [(c"OfxParamTypeInteger", 7), (c"OfxParamTypeBoolean", 1), (c"OfxParamTypeChoice", 2)] {
                let h = param(ty);
                (s.param_set_value)(h, v as c_int);
                let mut got: c_int = -1;
                (s.param_get_value)(h, &mut got as *mut c_int);
                assert_eq!(got, v, "{ty:?}");
            }

            let h = param(c"OfxParamTypeInteger2D");
            (s.param_set_value)(h, 3 as c_int, -4 as c_int);
            let (mut a, mut b): (c_int, c_int) = (0, 0);
            (s.param_get_value)(h, &mut a as *mut c_int, &mut b as *mut c_int);
            assert_eq!((a, b), (3, -4));

            let h = param(c"OfxParamTypeInteger3D");
            (s.param_set_value)(h, 5 as c_int, 6 as c_int, 7 as c_int);
            let (mut a, mut b, mut c): (c_int, c_int, c_int) = (0, 0, 0);
            (s.param_get_value)(h, &mut a as *mut c_int, &mut b as *mut c_int, &mut c as *mut c_int);
            assert_eq!((a, b, c), (5, 6, 7));

            let h = param(c"OfxParamTypeString");
            (s.param_set_value)(h, c"a title".as_ptr());
            let mut got: *const c_char = std::ptr::null();
            (s.param_get_value)(h, &mut got as *mut *const c_char);
            assert_eq!(CStr::from_ptr(got).to_str().unwrap(), "a title");
        }
    }

    #[test]
    fn values_keyed_at_times_are_read_at_a_time_with_their_rates_and_areas() {
        let s = &PARAM_SUITE_V1;
        // SAFETY: as above.
        unsafe {
            let h = param(c"OfxParamTypeDouble2D");
            assert_eq!((s.param_set_value_at_time)(h, 0.0f64, 1.0f64, 10.0f64), K_OFX_STAT_OK);
            assert_eq!((s.param_set_value_at_time)(h, 10.0f64, 3.0f64, 0.0f64), K_OFX_STAT_OK);
            let (mut x, mut y) = (0.0f64, 0.0f64);
            assert_eq!((s.param_get_value_at_time)(h, 5.0f64, &mut x as *mut f64, &mut y as *mut f64), K_OFX_STAT_OK);
            assert_eq!((x, y), (2.0, 5.0), "halfway between the keys");
            let (mut dx, mut dy) = (0.0f64, 0.0f64);
            assert_eq!((s.param_get_derivative)(h, 5.0f64, &mut dx as *mut f64, &mut dy as *mut f64), K_OFX_STAT_OK);
            assert!((dx - 0.2).abs() < 1e-12 && (dy + 1.0).abs() < 1e-12, "each component's rate: {dx}, {dy}");
            let (mut ix, mut iy) = (0.0f64, 0.0f64);
            assert_eq!((s.param_get_integral)(h, 0.0f64, 10.0f64, &mut ix as *mut f64, &mut iy as *mut f64), K_OFX_STAT_OK);
            assert!((ix - 20.0).abs() < 1e-9 && (iy - 50.0).abs() < 1e-9, "each component's area: {ix}, {iy}");

            let h = param(c"OfxParamTypeInteger");
            assert_eq!((s.param_set_value_at_time)(h, 2.0f64, 4 as c_int), K_OFX_STAT_OK);
            let mut got: c_int = 0;
            (s.param_get_value_at_time)(h, 2.0f64, &mut got as *mut c_int);
            assert_eq!(got, 4);
            let mut d = 0.0f64;
            assert_eq!((s.param_get_derivative)(h, 2.0f64, &mut d as *mut f64), K_OFX_STAT_ERR_VALUE, "an integer has no derivative");

            let bogus = 0x1 as OfxParamHandle;
            assert_eq!((s.param_set_value)(bogus, 1.0f64), K_OFX_STAT_ERR_BAD_HANDLE);
            assert_eq!((s.param_get_value)(bogus, &mut d as *mut f64), K_OFX_STAT_ERR_BAD_HANDLE);
        }
    }
}
