/// The two symbols every OFX plugin must export.
pub type OfxGetNumberOfPluginsFn = unsafe extern "C" fn() -> std::ffi::c_int;
pub type OfxGetPluginFn = unsafe extern "C" fn(nth: std::ffi::c_int) -> *const OfxPluginRaw;

/// The raw C struct as defined by the OFX spec.
#[repr(C)]
pub struct OfxPluginRaw {
    pub plugin_api: *const std::ffi::c_char,
    pub api_version: std::ffi::c_int,
    pub plugin_identifier: *const std::ffi::c_char,
    pub plugin_version_major: u32,
    pub plugin_version_minor: u32,
    pub set_host: Option<unsafe extern "C" fn(host: *const std::ffi::c_void)>,
    pub main_entry: Option<
        unsafe extern "C" fn(
            action: *const std::ffi::c_char,
            handle: *const std::ffi::c_void,
            in_args: *const std::ffi::c_void,
            out_args: *mut std::ffi::c_void,
        ) -> std::ffi::c_int,
    >,
}
