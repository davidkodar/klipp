//! frei0r instance lifecycle, parameter dispatch, and the host-side
//! frame adapter (color-model conversion + tiling).
//!
//! ## Threading model
//!
//! Each [`Frei0rEffect`] holds one plugin instance and is `Send` but
//! not `Sync` — the per-clip render path owns it exclusively while
//! evaluating that clip. Concurrent renders use distinct instances
//! (mirror LADSPA / OFX precedent).
//!
//! ## Tiling
//!
//! frei0r 1.x clamps width/height to `[8, 2048]` and requires both
//! to be multiples of 8. Above 2048 the host tiles at 1024×1024 with
//! 64-px overlap (spatial-filter seam mitigation), drops the overlap
//! on composite, and crops back to the original frame size. Some
//! plugins are inherently temporal (delay0r, baltan): they keep
//! per-instance frame history and tiling fragments that history.
//! Those plugins are listed in [`temporal::TEMPORAL_PLUGINS`] and
//! get a clean rejection at >2048 instead of broken output.

use std::path::Path;

use crate::ffi::*;
use crate::frei0r_guard;
use crate::host::{ColorModel, Frei0rPluginInfo, ParamKind};

// ─── Public param value type ──────────────────────────────────────────────

/// 1:1 with frei0r's 5 parameter types. Saved verbatim in project
/// files (`BuiltinEffect::Frei0r.params`) so future plugin variants
/// don't drop fields when v2 lands.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Frei0rValue {
    Bool(bool),
    Double(f64),
    /// RGB triplet in [0, 1]. frei0r colors have NO alpha channel.
    Color([f32; 3]),
    /// Position in [0, 1] × [0, 1].
    Position([f64; 2]),
    String(String),
}

impl Frei0rValue {
    pub fn matches_kind(&self, kind: ParamKind) -> bool {
        matches!(
            (self, kind),
            (Self::Bool(_), ParamKind::Bool)
                | (Self::Double(_), ParamKind::Double)
                | (Self::Color(_), ParamKind::Color)
                | (Self::Position(_), ParamKind::Position)
                | (Self::String(_), ParamKind::String)
        )
    }

    /// Default value for a param of the given kind. Used when the
    /// inspector adds a previously-unknown plugin to a clip.
    pub fn default_for(kind: ParamKind) -> Self {
        match kind {
            ParamKind::Bool => Self::Bool(false),
            ParamKind::Double => Self::Double(0.0),
            ParamKind::Color => Self::Color([1.0, 1.0, 1.0]),
            ParamKind::Position => Self::Position([0.5, 0.5]),
            ParamKind::String | ParamKind::Unknown => Self::String(String::new()),
        }
    }
}

// ─── Plugin (dlopen + f0r_init / f0r_deinit lifecycle) ────────────────────

/// One dlopened frei0r `.so` with its entry points resolved. Holds the
/// `Library` to keep the dlopen alive for the duration of every
/// instance constructed against it.
pub struct Frei0rPlugin {
    // Drop order matters: instances drop first, then the Library. We
    // don't allow instances to outlive the plugin (Frei0rEffect holds
    // an Arc<Frei0rPlugin>), so the explicit Drop on this struct can
    // safely call f0r_deinit and close `_lib` after.
    /// Closes under the load lock when it drops (#1032).
    _lib: crate::library::GuardedLibrary,
    pub info: Frei0rPluginInfo,
    construct: F0rConstructFn,
    destruct: F0rDestructFn,
    update: Option<F0rUpdateFn>,
    set_param_value: F0rSetParamValueFn,
    deinit: F0rDeinitFn,
}

// SAFETY: Plugin entry points are not thread-safe per-instance, but
// the plugin handle itself (a `Library`) is `Send`. Frei0rEffect (one
// instance) is `Send` but not `Sync`; concurrent calls are prevented
// by exclusive ownership at the call site.
unsafe impl Send for Frei0rPlugin {}
unsafe impl Sync for Frei0rPlugin {}

impl Frei0rPlugin {
    /// Open a `.so` and resolve every required symbol. Returns `Err`
    /// if the file isn't a frei0r plugin.
    pub fn open(path: &Path, info: Frei0rPluginInfo) -> Result<Self, String> {
        // Opened, and closed on every way out of here that fails (a
        // missing symbol, a failed `f0r_init`), under the load lock.
        let guarded = crate::library::GuardedLibrary::open(path).map_err(|e| format!("dlopen failed: {e}"))?;
        let lib = guarded.get();

        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let s: libloading::Symbol<$ty> = unsafe { lib.get($name) }
                    .map_err(|e| format!("missing symbol {}: {e}", stringify!($name)))?;
                *s
            }};
        }

        let init = sym!(b"f0r_init\0", F0rInitFn);
        let deinit = sym!(b"f0r_deinit\0", F0rDeinitFn);
        let construct = sym!(b"f0r_construct\0", F0rConstructFn);
        let destruct = sym!(b"f0r_destruct\0", F0rDestructFn);
        let set_param_value = sym!(b"f0r_set_param_value\0", F0rSetParamValueFn);
        // f0r_update is required for FILTER + SOURCE; absent is OK if
        // the plugin only ships f0r_update2 (mixers). v1 host only
        // activates Filter plugins so update must be present here.
        let update: Option<F0rUpdateFn> = unsafe { lib.get(b"f0r_update\0") }
            .ok()
            .map(|s: libloading::Symbol<F0rUpdateFn>| *s);

        // Pair this with f0r_deinit on Drop.
        let init_ok = match frei0r_guard!({ unsafe { init() } }) {
            Ok(rc) => rc != 0,
            Err(_) => false,
        };
        if !init_ok {
            return Err("f0r_init failed".into());
        }

        Ok(Self {
            _lib: guarded,
            info,
            construct,
            destruct,
            update,
            set_param_value,
            deinit,
        })
    }
}

impl Drop for Frei0rPlugin {
    fn drop(&mut self) {
        let deinit = self.deinit;
        let _ = frei0r_guard!({ unsafe { deinit() } });
        // `_lib` drops after this, closing under the load lock.
    }
}

// ─── Per-clip instance ────────────────────────────────────────────────────

/// One frei0r instance bound to a specific resolution. Constructed
/// lazily — the runtime cache (in `klipp-effects`) keys instances by
/// (type_id, width, height, params-hash) so a clip on a 1080p timeline
/// reuses its instance across frames.
pub struct Frei0rEffect {
    plugin: std::sync::Arc<Frei0rPlugin>,
    instance: F0rInstance,
    width: u32,
    height: u32,
}

unsafe impl Send for Frei0rEffect {}

impl Frei0rEffect {
    pub fn new(
        plugin: std::sync::Arc<Frei0rPlugin>,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        if width < F0R_DIM_ALIGN
            || height < F0R_DIM_ALIGN
            || !width.is_multiple_of(F0R_DIM_ALIGN)
            || !height.is_multiple_of(F0R_DIM_ALIGN)
        {
            return Err(format!(
                "frei0r requires 8-aligned dimensions, got {width}×{height}"
            ));
        }
        if width > F0R_MAX_DIM || height > F0R_MAX_DIM {
            return Err(format!(
                "frei0r requires dimensions ≤ {F0R_MAX_DIM}, got {width}×{height}"
            ));
        }

        let construct = plugin.construct;
        let instance = match frei0r_guard!({ unsafe { construct(width, height) } }) {
            Ok(p) if !p.is_null() => p,
            Ok(_) => return Err("f0r_construct returned null".into()),
            Err(_) => return Err("f0r_construct panicked".into()),
        };

        Ok(Self {
            plugin,
            instance,
            width,
            height,
        })
    }

    pub fn info(&self) -> &Frei0rPluginInfo {
        &self.plugin.info
    }

    /// Set a single parameter on the instance. Type must match the
    /// plugin's declared kind; mismatches log a warning and return
    /// without crashing.
    pub fn set_param(&self, index: usize, value: &Frei0rValue) {
        let Some(param) = self.plugin.info.params.get(index) else {
            return;
        };
        if !value.matches_kind(param.kind) {
            tracing::warn!(
                "frei0r '{}' param {} expected {:?}, got mismatched value",
                self.plugin.info.name,
                index,
                param.kind
            );
            return;
        }

        let setter = self.plugin.set_param_value;
        let inst = self.instance;
        let idx = index as i32;

        let _ = frei0r_guard!({
            unsafe {
                match value {
                    Frei0rValue::Bool(b) => {
                        // f0r_param_bool is a `double` per the ABI; values
                        // < 0.5 = false, ≥ 0.5 = true.
                        let mut v: f64 = if *b { 1.0 } else { 0.0 };
                        setter(inst, &mut v as *mut f64 as F0rParam, idx);
                    }
                    Frei0rValue::Double(d) => {
                        let mut v: f64 = *d;
                        setter(inst, &mut v as *mut f64 as F0rParam, idx);
                    }
                    Frei0rValue::Color(rgb) => {
                        let mut v = F0rParamColor {
                            r: rgb[0],
                            g: rgb[1],
                            b: rgb[2],
                        };
                        setter(inst, &mut v as *mut F0rParamColor as F0rParam, idx);
                    }
                    Frei0rValue::Position(xy) => {
                        let mut v = F0rParamPosition {
                            x: xy[0],
                            y: xy[1],
                        };
                        setter(inst, &mut v as *mut F0rParamPosition as F0rParam, idx);
                    }
                    Frei0rValue::String(s) => {
                        // Plugin reads the C string and must copy it
                        // synchronously per spec — we can drop it on
                        // return.
                        let cstr =
                            std::ffi::CString::new(s.as_str()).unwrap_or_default();
                        let mut p: *const std::os::raw::c_char = cstr.as_ptr();
                        setter(inst, &mut p as *mut _ as F0rParam, idx);
                    }
                }
            }
        });
    }

    /// Process one frame — input + output are RGBA8888 buffers in
    /// host (Klipp) byte order. This handles the color-model adapter
    /// transparently if the plugin declared BGRA8888 / PACKED32.
    ///
    /// Both buffers MUST be `width * height * 4` bytes long.
    pub fn run(&self, time_secs: f64, rgba_in: &[u8]) -> Vec<u8> {
        let expect = (self.width as usize) * (self.height as usize) * 4;
        if rgba_in.len() != expect {
            tracing::warn!(
                "frei0r '{}' run: buffer size mismatch (got {}, expected {expect})",
                self.plugin.info.name,
                rgba_in.len()
            );
            return rgba_in.to_vec();
        }

        let Some(update) = self.plugin.update else {
            // No update fn — pure source plugin. v1 host doesn't
            // activate sources; this branch is defensive.
            return rgba_in.to_vec();
        };

        // Convert to plugin-native byte order.
        let plugin_in = match self.plugin.info.color_model {
            ColorModel::Rgba8888 | ColorModel::Packed32 => rgba_in.to_vec(),
            ColorModel::Bgra8888 => swap_rgba_bgra(rgba_in),
            ColorModel::Unknown => return rgba_in.to_vec(),
        };
        let mut plugin_out = vec![0u8; expect];

        let inst = self.instance;
        let width = self.width;
        let height = self.height;
        let _ = frei0r_guard!({
            unsafe {
                update(
                    inst,
                    time_secs,
                    plugin_in.as_ptr() as *const u32,
                    plugin_out.as_mut_ptr() as *mut u32,
                );
            }
            // Bound `width` × `height` access the compiler's optimiser
            // would otherwise be able to elide; spec calls for both
                // dimensions to be ≤ 2048, this just documents the contract.
            let _ = (width, height);
        });

        // Convert plugin-native back to RGBA8888.
        match self.plugin.info.color_model {
            ColorModel::Rgba8888 | ColorModel::Packed32 => plugin_out,
            ColorModel::Bgra8888 => swap_rgba_bgra(&plugin_out),
            ColorModel::Unknown => plugin_out,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
}

impl Drop for Frei0rEffect {
    fn drop(&mut self) {
        let destruct = self.plugin.destruct;
        let inst = self.instance;
        let _ = frei0r_guard!({ unsafe { destruct(inst) } });
    }
}

/// In-place R↔B byte swap for RGBA8888 ↔ BGRA8888 conversion.
/// Alpha is byte-3 in both layouts; only bytes 0 and 2 swap.
fn swap_rgba_bgra(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    for chunk in src.chunks_exact(4) {
        out.push(chunk[2]);
        out.push(chunk[1]);
        out.push(chunk[0]);
        out.push(chunk[3]);
    }
    out
}

// ─── Temporal-plugin allowlist ────────────────────────────────────────────

pub mod temporal {
    //! Plugins whose internal state is per-frame-history-buffer and
    //! therefore CANNOT be safely tiled. At resolutions above
    //! [`super::F0R_MAX_DIM`] the host renders these as passthrough
    //! and the inspector surfaces a "doesn't support 4K" toast.
    //!
    //! Conservative list — when in doubt, NOT in the list (so the
    //! tiling path runs and produces close-to-correct output rather
    //! than silently passing the frame through).

    /// Match by plugin NAME (from `f0r_get_plugin_info`), case-sensitive.
    /// Names shipped by the canonical frei0r distribution.
    pub const TEMPORAL_PLUGINS: &[&str] = &[
        "Delay0r",         // delay0r — multi-frame delay buffer
        "Baltan",          // baltan — temporal feedback delay
        "Nervous",         // nervous — random past-frame jumble
        "FrameBuffer0r",   // framebuffer0r (if present)
        "Lightgraffiti",   // lightgraffiti — accumulates history
        "scanline0r",      // scanline accumulator
    ];

    pub fn is_temporal(plugin_name: &str) -> bool {
        TEMPORAL_PLUGINS.contains(&plugin_name)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frei0r_value_matches_kind() {
        assert!(Frei0rValue::Bool(true).matches_kind(ParamKind::Bool));
        assert!(Frei0rValue::Double(0.5).matches_kind(ParamKind::Double));
        assert!(Frei0rValue::Color([1.0, 0.5, 0.0]).matches_kind(ParamKind::Color));
        assert!(Frei0rValue::Position([0.5, 0.5]).matches_kind(ParamKind::Position));
        assert!(Frei0rValue::String("x".into()).matches_kind(ParamKind::String));
        // Mismatches:
        assert!(!Frei0rValue::Bool(true).matches_kind(ParamKind::Double));
        assert!(!Frei0rValue::Double(0.5).matches_kind(ParamKind::Color));
    }

    #[test]
    fn default_for_each_kind() {
        assert_eq!(
            Frei0rValue::default_for(ParamKind::Bool),
            Frei0rValue::Bool(false)
        );
        assert_eq!(
            Frei0rValue::default_for(ParamKind::Double),
            Frei0rValue::Double(0.0)
        );
        assert_eq!(
            Frei0rValue::default_for(ParamKind::Color),
            Frei0rValue::Color([1.0, 1.0, 1.0])
        );
        assert_eq!(
            Frei0rValue::default_for(ParamKind::Position),
            Frei0rValue::Position([0.5, 0.5])
        );
        assert_eq!(
            Frei0rValue::default_for(ParamKind::String),
            Frei0rValue::String(String::new())
        );
    }

    #[test]
    fn value_round_trip_serde() {
        let cases = [
            Frei0rValue::Bool(true),
            Frei0rValue::Double(0.42),
            Frei0rValue::Color([0.1, 0.2, 0.3]),
            Frei0rValue::Position([0.25, 0.75]),
            Frei0rValue::String("hello".into()),
        ];
        for v in cases {
            let json = serde_json::to_string(&v).unwrap();
            let back: Frei0rValue = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn rgba_bgra_swap_is_involutive() {
        let rgba = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let bgra = swap_rgba_bgra(&rgba);
        assert_eq!(bgra, [30u8, 20, 10, 40, 70, 60, 50, 80]);
        let back = swap_rgba_bgra(&bgra);
        assert_eq!(back, rgba);
    }

    #[test]
    fn temporal_allowlist_recognises_known_names() {
        assert!(temporal::is_temporal("Delay0r"));
        assert!(temporal::is_temporal("Baltan"));
        assert!(!temporal::is_temporal("Cartoon"));
        assert!(!temporal::is_temporal("Invert0r"));
        assert!(!temporal::is_temporal(""));
    }
}
