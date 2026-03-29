//! OFX C ABI type definitions.
//!
//! These `#[repr(C)]` structs match the OpenFX 1.5 specification exactly.
//! They are used for FFI with dynamically loaded `.ofx` plugin binaries.

use std::ffi::{c_char, c_double, c_int, c_uint, c_void};

// ── Entry point symbols ──────────────────────────────────────────────

/// The two symbols every OFX plugin must export.
pub type OfxGetNumberOfPluginsFn = unsafe extern "C" fn() -> c_int;
pub type OfxGetPluginFn = unsafe extern "C" fn(nth: c_int) -> *const OfxPluginRaw;

// ── Opaque handle types ──────────────────────────────────────────────

pub type OfxPropertySetHandle = *mut c_void;
pub type OfxParamSetHandle = *mut c_void;
pub type OfxParamHandle = *mut c_void;
pub type OfxImageClipHandle = *mut c_void;
pub type OfxImageHandle = *mut c_void;
pub type OfxImageEffectHandle = *mut c_void;
pub type OfxImageMemoryHandle = *mut c_void;
pub type OfxMutexHandle = *mut c_void;
pub type OfxStatus = c_int;

// ── Status codes ─────────────────────────────────────────────────────

pub const K_OFX_STAT_OK: OfxStatus = 0;
pub const K_OFX_STAT_FAILED: OfxStatus = 1;
pub const K_OFX_STAT_ERR_FATAL: OfxStatus = 2;
pub const K_OFX_STAT_ERR_UNKNOWN: OfxStatus = 3;
pub const K_OFX_STAT_ERR_MISSING_HOST_FEATURE: OfxStatus = 4;
pub const K_OFX_STAT_ERR_UNSUPPORTED: OfxStatus = 5;
pub const K_OFX_STAT_ERR_EXISTS: OfxStatus = 6;
pub const K_OFX_STAT_ERR_FORMAT: OfxStatus = 7;
pub const K_OFX_STAT_ERR_MEMORY: OfxStatus = 8;
pub const K_OFX_STAT_ERR_BAD_HANDLE: OfxStatus = 9;
pub const K_OFX_STAT_ERR_BAD_INDEX: OfxStatus = 10;
pub const K_OFX_STAT_ERR_VALUE: OfxStatus = 11;
pub const K_OFX_STAT_REPLY_YES: OfxStatus = 12;
pub const K_OFX_STAT_REPLY_NO: OfxStatus = 13;
pub const K_OFX_STAT_REPLY_DEFAULT: OfxStatus = 14;

// GPU render status codes (OFX 1.5)
pub const K_OFX_STAT_GPU_OUT_OF_MEMORY: OfxStatus = 1001;
pub const K_OFX_STAT_GPU_RENDER_FAILED: OfxStatus = 1002;

// ── Geometry types ───────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct OfxRectI {
    pub x1: c_int,
    pub y1: c_int,
    pub x2: c_int,
    pub y2: c_int,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct OfxRectD {
    pub x1: c_double,
    pub y1: c_double,
    pub x2: c_double,
    pub y2: c_double,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct OfxRangeD {
    pub min: c_double,
    pub max: c_double,
}

// ── Core OFX structs ─────────────────────────────────────────────────

/// The raw C struct as defined by the OFX spec.
#[repr(C)]
pub struct OfxPluginRaw {
    pub plugin_api: *const c_char,
    pub api_version: c_int,
    pub plugin_identifier: *const c_char,
    pub plugin_version_major: u32,
    pub plugin_version_minor: u32,
    pub set_host: Option<unsafe extern "C" fn(host: *const c_void)>,
    pub main_entry: Option<
        unsafe extern "C" fn(
            action: *const c_char,
            handle: *const c_void,
            in_args: *const c_void,
            out_args: *mut c_void,
        ) -> c_int,
    >,
}

/// The host struct passed to every plugin via setHost().
/// Plugin stores this pointer and uses it to fetch suites.
/// Field order matches the OFX spec: host property set first, fetchSuite second.
#[repr(C)]
pub struct OfxHostC {
    pub host: OfxPropertySetHandle,
    pub fetch_suite: unsafe extern "C" fn(
        host: OfxPropertySetHandle,
        suite_name: *const c_char,
        suite_version: c_int,
    ) -> *const c_void,
}

// ── Suite function tables ────────────────────────────────────────────

/// OfxPropertySuite v1 — 19 functions.
#[repr(C)]
pub struct OfxPropertySuiteV1 {
    pub prop_set_pointer: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *mut c_void) -> OfxStatus,
    pub prop_set_string: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *const c_char) -> OfxStatus,
    pub prop_set_double: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, c_double) -> OfxStatus,
    pub prop_set_int: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, c_int) -> OfxStatus,
    pub prop_set_pointer_n: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *const *mut c_void) -> OfxStatus,
    pub prop_set_string_n: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *const *const c_char) -> OfxStatus,
    pub prop_set_double_n: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *const c_double) -> OfxStatus,
    pub prop_set_int_n: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *const c_int) -> OfxStatus,
    pub prop_get_pointer: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *mut *mut c_void) -> OfxStatus,
    pub prop_get_string: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *mut *const c_char) -> OfxStatus,
    pub prop_get_double: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *mut c_double) -> OfxStatus,
    pub prop_get_int: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *mut c_int) -> OfxStatus,
    pub prop_get_pointer_n: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *mut *mut c_void) -> OfxStatus,
    pub prop_get_string_n: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *mut *const c_char) -> OfxStatus,
    pub prop_get_double_n: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *mut c_double) -> OfxStatus,
    pub prop_get_int_n: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, c_int, *mut c_int) -> OfxStatus,
    pub prop_reset: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char) -> OfxStatus,
    pub prop_get_dimension: unsafe extern "C" fn(OfxPropertySetHandle, *const c_char, *mut c_int) -> OfxStatus,
}

// Safety: OfxPropertySuiteV1 contains only function pointers — no mutable state.
unsafe impl Sync for OfxPropertySuiteV1 {}

/// OfxParameterSuite v1 — 18 functions.
#[repr(C)]
pub struct OfxParameterSuiteV1 {
    pub param_define: unsafe extern "C" fn(OfxParamSetHandle, *const c_char, *const c_char, *mut OfxPropertySetHandle) -> OfxStatus,
    pub param_get_handle: unsafe extern "C" fn(OfxParamSetHandle, *const c_char, *mut OfxParamHandle, *mut OfxPropertySetHandle) -> OfxStatus,
    pub param_set_get_property_set: unsafe extern "C" fn(OfxParamSetHandle, *mut OfxPropertySetHandle) -> OfxStatus,
    pub param_get_property_set: unsafe extern "C" fn(OfxParamHandle, *mut OfxPropertySetHandle) -> OfxStatus,
    pub param_get_value: unsafe extern "C" fn(OfxParamHandle, ...) -> OfxStatus,
    pub param_get_value_at_time: unsafe extern "C" fn(OfxParamHandle, c_double, ...) -> OfxStatus,
    pub param_get_derivative: unsafe extern "C" fn(OfxParamHandle, c_double, ...) -> OfxStatus,
    pub param_get_integral: unsafe extern "C" fn(OfxParamHandle, c_double, c_double, ...) -> OfxStatus,
    pub param_set_value: unsafe extern "C" fn(OfxParamHandle, ...) -> OfxStatus,
    pub param_set_value_at_time: unsafe extern "C" fn(OfxParamHandle, c_double, ...) -> OfxStatus,
    pub param_get_num_keys: unsafe extern "C" fn(OfxParamHandle, *mut c_uint) -> OfxStatus,
    pub param_get_key_time: unsafe extern "C" fn(OfxParamHandle, c_uint, *mut c_double) -> OfxStatus,
    pub param_get_key_index: unsafe extern "C" fn(OfxParamHandle, c_double, c_int, *mut c_int) -> OfxStatus,
    pub param_delete_key: unsafe extern "C" fn(OfxParamHandle, c_double) -> OfxStatus,
    pub param_delete_all_keys: unsafe extern "C" fn(OfxParamHandle) -> OfxStatus,
    pub param_copy: unsafe extern "C" fn(OfxParamHandle, OfxParamHandle, c_double, *const OfxRangeD) -> OfxStatus,
    pub param_edit_begin: unsafe extern "C" fn(OfxParamSetHandle, *const c_char) -> OfxStatus,
    pub param_edit_end: unsafe extern "C" fn(OfxParamSetHandle) -> OfxStatus,
}

unsafe impl Sync for OfxParameterSuiteV1 {}

/// OfxImageEffectSuite v1 — 14 functions.
#[repr(C)]
pub struct OfxImageEffectSuiteV1 {
    pub get_property_set: unsafe extern "C" fn(OfxImageEffectHandle, *mut OfxPropertySetHandle) -> OfxStatus,
    pub get_param_set: unsafe extern "C" fn(OfxImageEffectHandle, *mut OfxParamSetHandle) -> OfxStatus,
    pub clip_define: unsafe extern "C" fn(OfxImageEffectHandle, *const c_char, *mut OfxPropertySetHandle) -> OfxStatus,
    pub clip_get_handle: unsafe extern "C" fn(OfxImageEffectHandle, *const c_char, *mut OfxImageClipHandle, *mut OfxPropertySetHandle) -> OfxStatus,
    pub clip_get_property_set: unsafe extern "C" fn(OfxImageClipHandle, *mut OfxPropertySetHandle) -> OfxStatus,
    pub clip_get_image: unsafe extern "C" fn(OfxImageClipHandle, c_double, *const OfxRectD, *mut OfxPropertySetHandle) -> OfxStatus,
    pub clip_release_image: unsafe extern "C" fn(OfxPropertySetHandle) -> OfxStatus,
    pub clip_get_region_of_definition: unsafe extern "C" fn(OfxImageClipHandle, c_double, *mut OfxRectD) -> OfxStatus,
    pub abort: unsafe extern "C" fn(OfxImageEffectHandle) -> c_int,
    pub image_memory_alloc: unsafe extern "C" fn(OfxImageEffectHandle, usize, *mut OfxImageMemoryHandle) -> OfxStatus,
    pub image_memory_free: unsafe extern "C" fn(OfxImageMemoryHandle) -> OfxStatus,
    pub image_memory_lock: unsafe extern "C" fn(OfxImageMemoryHandle, *mut *mut c_void) -> OfxStatus,
    pub image_memory_unlock: unsafe extern "C" fn(OfxImageMemoryHandle) -> OfxStatus,
}

unsafe impl Sync for OfxImageEffectSuiteV1 {}

/// OfxMemorySuite v1 — 2 functions.
#[repr(C)]
pub struct OfxMemorySuiteV1 {
    pub memory_alloc: unsafe extern "C" fn(*mut c_void, usize, *mut *mut c_void) -> OfxStatus,
    pub memory_free: unsafe extern "C" fn(*mut c_void) -> OfxStatus,
}

unsafe impl Sync for OfxMemorySuiteV1 {}

/// OfxMultiThreadSuite v1 — 9 functions.
#[repr(C)]
pub struct OfxMultiThreadSuiteV1 {
    pub multithread: unsafe extern "C" fn(
        func: unsafe extern "C" fn(c_uint, c_uint, *mut c_void),
        n_threads: c_uint,
        custom_arg: *mut c_void,
    ) -> OfxStatus,
    pub multithread_num_cpus: unsafe extern "C" fn(*mut c_uint) -> OfxStatus,
    pub multithread_index: unsafe extern "C" fn(*mut c_uint) -> OfxStatus,
    pub multithread_is_spawned_thread: unsafe extern "C" fn() -> c_int,
    pub mutex_create: unsafe extern "C" fn(*mut OfxMutexHandle, c_int) -> OfxStatus,
    pub mutex_destroy: unsafe extern "C" fn(OfxMutexHandle) -> OfxStatus,
    pub mutex_lock: unsafe extern "C" fn(OfxMutexHandle) -> OfxStatus,
    pub mutex_unlock: unsafe extern "C" fn(OfxMutexHandle) -> OfxStatus,
    pub mutex_try_lock: unsafe extern "C" fn(OfxMutexHandle) -> OfxStatus,
}

unsafe impl Sync for OfxMultiThreadSuiteV1 {}

/// OfxMessageSuite v1 — 1 function (variadic format args ignored, message logged).
#[repr(C)]
pub struct OfxMessageSuiteV1 {
    pub message: unsafe extern "C" fn(
        handle: *mut c_void,
        msg_type: *const c_char,
        msg_id: *const c_char,
        format: *const c_char,
    ) -> OfxStatus,
}

unsafe impl Sync for OfxMessageSuiteV1 {}

/// OfxProgressSuite v1 — 3 functions.
#[repr(C)]
pub struct OfxProgressSuiteV1 {
    pub progress_start: unsafe extern "C" fn(*mut c_void, *const c_char) -> OfxStatus,
    pub progress_update: unsafe extern "C" fn(*mut c_void, c_double) -> OfxStatus,
    pub progress_end: unsafe extern "C" fn(*mut c_void) -> OfxStatus,
}

unsafe impl Sync for OfxProgressSuiteV1 {}

/// OfxImageEffectOpenGLRenderSuiteV1 — 3 functions (OFX 1.3+).
/// Provides OpenGL texture loading for GPU-accelerated OFX plugins.
#[repr(C)]
pub struct OfxImageEffectOpenGLRenderSuiteV1 {
    /// Load a clip's image as an OpenGL texture.
    /// Returns a property set with kOfxImageEffectPropOpenGLTextureIndex/Target.
    pub clip_load_texture: unsafe extern "C" fn(
        clip: OfxImageClipHandle,
        time: c_double,
        format: *const c_char,
        region: *const OfxRectD,
        texture_handle: *mut OfxPropertySetHandle,
    ) -> OfxStatus,
    /// Release a texture previously loaded via clipLoadTexture.
    pub clip_free_texture: unsafe extern "C" fn(
        texture_handle: OfxPropertySetHandle,
    ) -> OfxStatus,
    /// Ask the host to free any cached GPU resources.
    pub flush_resources: unsafe extern "C" fn() -> OfxStatus,
}

unsafe impl Sync for OfxImageEffectOpenGLRenderSuiteV1 {}
