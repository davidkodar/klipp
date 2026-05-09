//! Raw frei0r 1.x C ABI bindings.
//!
//! Mirrors `/usr/include/frei0r.h` (FREI0R_MAJOR_VERSION 1, MINOR 2).
//! Every type and constant in this module exists for one reason only:
//! to talk to a third-party `.so`. Higher-level safety / lifetime
//! contracts live in `instance.rs` and `host.rs`.
//!
//! # Safety
//!
//! Every `unsafe extern "C"` fn pointer here is invoked from inside
//! [`crate::frei0r_guard!`] which wraps the call in
//! `std::panic::catch_unwind`. A Rust panic unwinding across
//! `extern "C"` is UB on Rust 2021 and aborts on 2024 — either way
//! the host process dies on behalf of a third-party plugin. The
//! guard converts any panic into an `Err(())` that callers turn into
//! a graceful skip + `tracing::warn!`.

use std::os::raw::{c_char, c_int, c_uint, c_void};

// ─── Plugin types (f0r_get_plugin_info → plugin_type) ─────────────────────

pub const F0R_PLUGIN_TYPE_FILTER: c_int = 0;
pub const F0R_PLUGIN_TYPE_SOURCE: c_int = 1;
pub const F0R_PLUGIN_TYPE_MIXER2: c_int = 2;
pub const F0R_PLUGIN_TYPE_MIXER3: c_int = 3;

// ─── Color models ─────────────────────────────────────────────────────────

pub const F0R_COLOR_MODEL_BGRA8888: c_int = 0;
pub const F0R_COLOR_MODEL_RGBA8888: c_int = 1;
/// Endian-dependent RGBA / ARGB packed into one u32. On little-endian
/// Linux this is byte-equivalent to RGBA8888 — Klipp targets Linux only,
/// so we treat PACKED32 as an alias for RGBA8888 in the adapter and
/// document the assumption.
pub const F0R_COLOR_MODEL_PACKED32: c_int = 2;

// ─── Param types ──────────────────────────────────────────────────────────

pub const F0R_PARAM_BOOL: c_int = 0;
pub const F0R_PARAM_DOUBLE: c_int = 1;
pub const F0R_PARAM_COLOR: c_int = 2;
pub const F0R_PARAM_POSITION: c_int = 3;
pub const F0R_PARAM_STRING: c_int = 4;

// ─── Structs ──────────────────────────────────────────────────────────────

#[repr(C)]
pub struct F0rPluginInfo {
    pub name: *const c_char,
    pub author: *const c_char,
    pub plugin_type: c_int,
    pub color_model: c_int,
    pub frei0r_version: c_int,
    pub major_version: c_int,
    pub minor_version: c_int,
    pub num_params: c_int,
    pub explanation: *const c_char,
}

#[repr(C)]
pub struct F0rParamInfo {
    pub name: *const c_char,
    pub param_type: c_int,
    pub explanation: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct F0rParamColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct F0rParamPosition {
    pub x: f64,
    pub y: f64,
}

// ─── Opaque types ─────────────────────────────────────────────────────────

pub type F0rInstance = *mut c_void;
pub type F0rParam = *mut c_void;

// ─── Plugin entry-point function pointers ─────────────────────────────────
//
// frei0r exports these by name as plain C functions. We dlopen and look
// up by symbol; the host stores function pointers (not Symbol<>) so the
// borrow checker doesn't tie the pointer's lifetime to a particular
// borrow of `Library`. The Library handle itself is held alive for
// the same duration in `Frei0rPlugin`.

pub type F0rInitFn = unsafe extern "C" fn() -> c_int;
pub type F0rDeinitFn = unsafe extern "C" fn();
pub type F0rGetPluginInfoFn = unsafe extern "C" fn(*mut F0rPluginInfo);
pub type F0rGetParamInfoFn = unsafe extern "C" fn(*mut F0rParamInfo, c_int);
pub type F0rConstructFn = unsafe extern "C" fn(c_uint, c_uint) -> F0rInstance;
pub type F0rDestructFn = unsafe extern "C" fn(F0rInstance);
pub type F0rSetParamValueFn = unsafe extern "C" fn(F0rInstance, F0rParam, c_int);
pub type F0rGetParamValueFn = unsafe extern "C" fn(F0rInstance, F0rParam, c_int);
pub type F0rUpdateFn = unsafe extern "C" fn(F0rInstance, f64, *const u32, *mut u32);
pub type F0rUpdate2Fn = unsafe extern "C" fn(
    F0rInstance,
    f64,
    *const u32,
    *const u32,
    *const u32,
    *mut u32,
);

// ─── Panic guard for FFI callbacks ────────────────────────────────────────

/// Wrap an FFI call into a frei0r plugin so a plugin panic / segfault
/// turns into `Err(())` instead of unwinding across `extern "C"`
/// (which is UB on Rust 2021 and aborts on 2024).
///
/// This catches Rust panics ONLY — a real C-side SIGSEGV still kills
/// the process. The audit-block deferred sub-process isolation to a
/// follow-up; for v1 we accept that a misbehaving plugin can crash
/// the host, same as the LADSPA + OFX hosts do today.
#[macro_export]
macro_rules! frei0r_guard {
    ($body:block) => {{
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $body))
            .map_err(|_| {
                tracing::warn!("frei0r: FFI callback panicked — skipping");
            })
    }};
}

// ─── Resolution constraints ───────────────────────────────────────────────

/// frei0r 1.x hard upper bound on either dimension. Above this, plugin
/// behaviour is undefined; v1 of our host tiles at <= TILE_MAX_DIM.
pub const F0R_MAX_DIM: u32 = 2048;

/// Required dimension multiple per the spec.
pub const F0R_DIM_ALIGN: u32 = 8;

/// Required frame-data alignment in bytes per the spec.
pub const F0R_FRAME_ALIGN_BYTES: usize = 16;
