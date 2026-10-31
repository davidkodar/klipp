//! frei0r 1.x plugin host.
//!
//! Bridges the open-source video-FX plugin format that Kdenlive,
//! Shotcut, OBS, MLT, and FFmpeg all consume into Klipp's effect
//! chain. v1 supports `F0R_PLUGIN_TYPE_FILTER` only — sources +
//! mixers are queued as `FREI0R-NEXT-SOURCES` / `FREI0R-NEXT-MIXERS`.
//!
//! ## Architecture
//!
//! - [`ffi`]      — raw `repr(C)` bindings to `frei0r.h`.
//! - [`host`]     — discovery + on-disk cache (mirrors LADSPA pattern).
//! - [`instance`] — per-instance lifecycle, parameter dispatch,
//!   color-model adapter, temporal-plugin allowlist.
//! - [`tile`]     — the instances kept across frames: padding, tiling,
//!   declared refusals and fallbacks.
//!
//! ## Safety
//!
//! All FFI entry points go through [`frei0r_guard!`] which wraps the
//! call in `std::panic::catch_unwind`. A Rust panic across `extern "C"`
//! is UB on Rust 2021 and aborts on 2024 — the guard converts it into
//! a graceful skip. Real C-side SIGSEGVs still kill the process; that
//! risk is the same one OFX + LADSPA hosts already accept.
//!
//! ## Linux-only
//!
//! frei0r is Linux/Unix-native (per spec §sec_pluglocations). Klipp
//! is Linux-only, so the path resolution + dlopen contract is a clean
//! match.

pub mod ffi;
pub mod host;
pub mod instance;
/// A test's: the next plug-in frame panics inside its guard, as a plug-in
/// wrapper's might (#3181). Test hooks only.
#[cfg(any(test, feature = "test-hooks"))]
pub static PANIC_NEXT_UPDATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
mod library;
pub mod tile;

pub use host::{
    discover_frei0r_plugins, discover_frei0r_plugins_cached, search_paths, ColorModel,
    Frei0rPluginInfo, ParamInfo, ParamKind, PluginType,
};
pub use instance::{temporal, Frei0rEffect, Frei0rPlugin, Frei0rValue};
pub use tile::{Fallback, Frei0rInstanceSet, Refusal, Rendered};
