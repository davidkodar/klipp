/// Guard an OFX callback against plugin-supplied null pointers.
///
/// Returns `K_OFX_STAT_ERR_VALUE` immediately if any listed pointer is null.
/// The OFX spec requires plugins to pass valid pointers, but a buggy or
/// malicious plugin passing null would otherwise cause an unchecked
/// dereference in `CStr::from_ptr` / `*out = …` and take the host down with
/// a SIGSEGV. Use this as the first line of every `extern "C" fn` callback
/// that accepts pointer parameters.
#[macro_export]
macro_rules! check_non_null {
    ($($p:ident),+ $(,)?) => {
        $(
            if $p.is_null() {
                return $crate::ffi::K_OFX_STAT_ERR_VALUE;
            }
        )+
    };
}

/// Wrap an OFX callback body so that a panic turns into
/// `K_OFX_STAT_ERR_FATAL` instead of unwinding across the FFI boundary
/// (which is UB on Rust 2021 and aborts the whole host process on 2024).
///
/// Usage: `ofx_guard!({ body })` inside every `unsafe extern "C" fn … ->
/// OfxStatus`. The body executes inside `catch_unwind`; any panic from
/// inside — allocation failure, index out of bounds, a residual
/// `.unwrap()` on unexpected state — is logged and converted to a clean
/// error status the plugin can observe. `AssertUnwindSafe` is used
/// because the callback body typically touches shared Mutex-protected
/// global stores; lock poisoning from a panic mid-critical-section is
/// handled by the workspace-wide unwrap_or_else(|e| e.into_inner())
/// treatment applied in a previous pass.
#[macro_export]
macro_rules! ofx_guard {
    ($body:block) => {{
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> $crate::ffi::OfxStatus { $body },
        )) {
            Ok(status) => status,
            Err(_) => {
                tracing::error!(
                    "OFX callback panicked — returning K_OFX_STAT_ERR_FATAL",
                );
                $crate::ffi::K_OFX_STAT_ERR_FATAL
            }
        }
    }};
}

pub mod constants;
pub mod ffi;
pub mod gpu_cuda;
pub mod gpu_opencl;
pub mod gpu_opengl;
pub mod host;
pub mod image_effect;
pub mod instance;
pub mod memory;
pub mod message;
pub mod param;
pub mod property;
#[allow(clippy::manual_c_str_literals)]
pub mod test_plugin;
pub mod thread;

pub use host::{
    AppliedEffect, OfxCategory, OfxHost, OfxHostRuntime, OfxPluginInfo, OfxValue, OfxValueExt,
    PluginError,
};
pub use param::{ParamDescriptor, ParamType, ParamValue};
pub use image_effect::FrameProviderFn;
