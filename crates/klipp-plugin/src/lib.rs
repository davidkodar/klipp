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
