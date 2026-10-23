//! OFX 1.5 plugin host: dlopen-and-bridge for third-party `.ofx.bundle`
//! effects plugins.
//!
//! This is the workspace's only C-ABI FFI crate and therefore the most
//! `unsafe`-dense. It dlopens an OFX plugin binary, exposes the OFX C
//! API through a fixed set of suite function tables ([`property`],
//! [`param`], [`image_effect`], [`memory`], [`message`], [`thread`],
//! [`gpu_opengl`], [`gpu_cuda`], [`gpu_opencl`]), and bridges the
//! plugin's GPU rendering context to our wgpu / libc / libEGL / CUDA
//! runtimes.
//!
//! # Boundary rules
//!
//! Every `unsafe extern "C" fn` that a plugin can call goes through
//! two guards declared in this module:
//!
//! * [`check_non_null!`] — early-returns `K_OFX_STAT_ERR_VALUE` for
//!   any null pointer the plugin handed us, so a buggy plugin can't
//!   turn us into a SIGSEGV with `*out = value`.
//! * [`ofx_guard!`] — wraps the callback body in
//!   `std::panic::catch_unwind`. A Rust panic unwinding across
//!   `extern "C"` is UB on Rust 2021 and aborts on 2024 — either way
//!   it would take the whole host down on behalf of a third-party
//!   plugin. The guard converts any panic into
//!   `K_OFX_STAT_ERR_FATAL` plus a `tracing::error!`.
//!
//! # Known outstanding issues
//!
//! * `param.rs`'s variadic-transmute — `paramGetValue` / `paramSetValue`
//!   are variadic in the OFX spec but implemented here as non-variadic
//!   and `mem::transmute`'d into the suite table. Works by coincidence
//!   on x86-64 SysV ABI with the OpenFX C++ support lib's calling
//!   convention, but is formally UB. Needs a per-type entry-point
//!   redesign.
//!
//! It is tracked for a dedicated plugin-host refactor pass. (A string
//! `prop_get_string_fn` returns now lives until its property set is
//! destroyed, #2449.)

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

pub use host::{OfxDescribed, OfxPluginInstance, MAX_FRAMES_NEEDED, 
    plugin_infos_cached, AppliedEffect, OfxCategory, OfxHost, OfxHostRuntime, OfxPluginInfo,
    OfxValue, OfxValueExt, PluginError,
};
pub use param::{ParamDescriptor, ParamType, ParamValue};
pub use image_effect::FrameProviderFn;

#[cfg(test)]
mod check_non_null_tests {
    // ═══════════════════════════════════════════════════════════════
    // CRIT-4 regression.
    //
    // The original tracker entry called out ~70 OFX callbacks where
    // `*out = value` ran without a null-check, so a buggy plugin
    // passing a null pointer would write to address 0 and SIGSEGV
    // the host. The 2026-04-16 fix introduced check_non_null! and
    // applied it at every entry point. These tests pin the macro's
    // contract — null short-circuits with K_OFX_STAT_ERR_VALUE,
    // non-null falls through — so a future "let me simplify this
    // macro" cleanup can't silently re-open the SIGSEGV vector.
    // ═══════════════════════════════════════════════════════════════

    use crate::ffi::{OfxStatus, K_OFX_STAT_ERR_VALUE, K_OFX_STAT_OK};

    /// Mirrors the structure of every OFX callback that takes one
    /// pointer arg: check_non_null! at the top, then proceed.
    fn one_arg_callback(p: *const u8) -> OfxStatus {
        check_non_null!(p);
        K_OFX_STAT_OK
    }

    /// Multi-arg form — matches OFX callbacks that receive several
    /// pointer args at once (e.g. property suite get/set fns).
    fn multi_arg_callback(a: *const u8, b: *mut u8, c: *const i32) -> OfxStatus {
        check_non_null!(a, b, c);
        K_OFX_STAT_OK
    }

    #[test]
    fn check_non_null_returns_err_value_on_null_single_arg() {
        let result = one_arg_callback(std::ptr::null());
        assert_eq!(result, K_OFX_STAT_ERR_VALUE,
            "null pointer must short-circuit with K_OFX_STAT_ERR_VALUE");
    }

    #[test]
    fn check_non_null_proceeds_when_pointer_is_non_null() {
        let dummy: u8 = 42;
        let result = one_arg_callback(&dummy as *const u8);
        assert_eq!(result, K_OFX_STAT_OK,
            "non-null pointer must fall through to the body");
    }

    #[test]
    fn check_non_null_short_circuits_on_first_null_in_multi_arg_form() {
        // Mixed: middle arg is null. The macro must return as soon
        // as it sees the first null, not "any null".
        let dummy_a: u8 = 1;
        let dummy_c: i32 = 3;
        let result = multi_arg_callback(
            &dummy_a as *const u8,
            std::ptr::null_mut(),
            &dummy_c as *const i32,
        );
        assert_eq!(result, K_OFX_STAT_ERR_VALUE,
            "any null arg in the multi-arg form must trigger ERR_VALUE");
    }

    #[test]
    fn check_non_null_proceeds_when_all_args_non_null() {
        let a: u8 = 1;
        let mut b: u8 = 2;
        let c: i32 = 3;
        let result = multi_arg_callback(&a, &mut b, &c);
        assert_eq!(result, K_OFX_STAT_OK,
            "all-non-null multi-arg call must fall through");
    }
}
