//! Bridge between Klipp's `BuiltinEffect::Frei0r` chain entry and
//! the `klipp-frei0r` host crate.
//!
//! Owns the process-global runtime cache that maps a frei0r `type_id`
//! (`"frei0r:{path}:{name}"`) to a single dlopened `Arc<Frei0rPlugin>`.
//! Per-clip rendering constructs a fresh `Frei0rEffect` against the
//! cached plugin each frame at the resolution it needs — frei0r
//! instances are bound to a (width, height) pair, and per-frame
//! reconstruction is cheap (just `f0r_construct`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use klipp_frei0r::ffi::F0R_MAX_DIM;
use klipp_frei0r::{temporal, Frei0rPlugin, Frei0rPluginInfo, Frei0rValue};

// ─── Process-global runtime cache ─────────────────────────────────────────

struct RuntimeCache {
    /// Discovery list — populated lazily on first lookup. We store the
    /// full info list so missing-plugin lookups can surface a clear
    /// error rather than "did the host see this at all?".
    plugins: Vec<Frei0rPluginInfo>,
    /// dlopened plugin handles keyed by type_id. Loaded on demand —
    /// most projects use a tiny subset of the installed plugin set.
    loaded: HashMap<String, Arc<Frei0rPlugin>>,
    /// Plugins we tried to load and failed. Cached so we don't retry
    /// dlopen every frame.
    failed: HashMap<String, String>,
}

fn cache() -> &'static Mutex<RuntimeCache> {
    static CACHE: OnceLock<Mutex<RuntimeCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(RuntimeCache {
            plugins: Vec::new(),
            loaded: HashMap::new(),
            failed: HashMap::new(),
        })
    })
}

/// Push a freshly-discovered plugin list into the runtime cache.
/// Called once at app startup from the UI layer after
/// `discover_frei0r_plugins_cached` returns.
///
/// Re-callable: if the user hits Rescan in the plugins panel, the
/// new list replaces the old one and previously-loaded handles are
/// dropped (so a removed `.so` no longer shows up).
pub fn set_plugin_registry(plugins: Vec<Frei0rPluginInfo>) {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.plugins = plugins;
    c.loaded.clear();
    c.failed.clear();
}

fn lookup_info(type_id: &str) -> Option<Frei0rPluginInfo> {
    let c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.plugins.iter().find(|p| p.type_id() == type_id).cloned()
}

fn get_or_load(type_id: &str) -> Option<Arc<Frei0rPlugin>> {
    {
        let c = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = c.loaded.get(type_id) {
            return Some(p.clone());
        }
        if c.failed.contains_key(type_id) {
            return None;
        }
    }

    let info = lookup_info(type_id)?;
    let path = info.path.clone();
    match Frei0rPlugin::open(&path, info) {
        Ok(plugin) => {
            let arc = Arc::new(plugin);
            let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
            c.loaded.insert(type_id.to_string(), arc.clone());
            Some(arc)
        }
        Err(e) => {
            tracing::warn!("frei0r: open failed for {type_id}: {e}");
            let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
            c.failed.insert(type_id.to_string(), e);
            None
        }
    }
}

// ─── Public API ───────────────────────────────────────────────────────────

/// Apply one frei0r effect to a frame. Honours the spec's resolution
/// constraints and the temporal-plugin allowlist:
///
///   - Width / height ≤ 2048 and 8-aligned → straight render.
///   - Width / height > 2048 with NON-temporal plugin → tiled render
///     via `klipp_frei0r::tile::render`.
///   - Width / height > 2048 with TEMPORAL plugin (delay0r etc.)  →
///     passthrough + `tracing::warn!`. The inspector blocks this case
///     at apply time so the warn here is purely defensive.
///   - Plugin not in registry → passthrough + `tracing::warn!`.
///   - Plugin failed to dlopen → passthrough (cached in `failed`).
///
/// Never panics. Callers always get back a frame of the same size.
pub fn apply(
    type_id: &str,
    params: &[Frei0rValue],
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    time_secs: f64,
) -> Vec<u8> {
    let Some(plugin) = get_or_load(type_id) else {
        tracing::warn!("frei0r: plugin '{type_id}' not available — passthrough");
        return rgba;
    };

    // Reject temporal plugins above the spec cap. Tiling fragments
    // their per-instance frame history.
    if (width > F0R_MAX_DIM || height > F0R_MAX_DIM)
        && temporal::is_temporal(&plugin.info.name)
    {
        tracing::warn!(
            "frei0r: '{}' is temporal and cannot tile above {F0R_MAX_DIM}px — passthrough",
            plugin.info.name
        );
        return rgba;
    }

    // Snapshot params for the apply_params closure. The closure is
    // invoked once per (sub-)instance the renderer constructs (one
    // for the no-tile path, N for the tile path).
    let params_snapshot: Vec<Frei0rValue> = params.to_vec();
    let apply_params = move |effect: &klipp_frei0r::Frei0rEffect| {
        for (i, value) in params_snapshot.iter().enumerate() {
            effect.set_param(i, value);
        }
    };

    klipp_frei0r::render(plugin, width, height, time_secs, &rgba, &apply_params)
}

/// Look up a plugin's metadata for inspector / project-doctor use.
/// Returns `None` if the plugin isn't installed (or `set_plugin_registry`
/// hasn't been called yet).
pub fn lookup(type_id: &str) -> Option<Frei0rPluginInfo> {
    lookup_info(type_id)
}

/// Snapshot the registry — used by tests and by the project-doctor
/// "missing plugin" check that needs to enumerate currently-known
/// installs without going through the on-disk cache.
pub fn registry_snapshot() -> Vec<Frei0rPluginInfo> {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .plugins
        .clone()
}

/// Returns true when the project's frame size cleanly fits into
/// frei0r's resolution constraints — used by the inspector to decide
/// whether to surface a "this plugin can't run at this size" toast
/// before dispatching the apply.
pub fn supports_resolution(plugin_name: &str, width: u32, height: u32) -> bool {
    if width % 8 != 0 || height % 8 != 0 {
        return false;
    }
    if width > F0R_MAX_DIM || height > F0R_MAX_DIM {
        // Tiled path covers non-temporal plugins; temporals can't tile.
        return !temporal::is_temporal(plugin_name);
    }
    true
}

/// Test-only: clear the runtime cache. Used by integration tests to
/// guarantee a clean state between runs.
#[doc(hidden)]
pub fn _reset_for_test() {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.plugins.clear();
    c.loaded.clear();
    c.failed.clear();
}

/// Test-only: register a single plugin so apply() can resolve a
/// type_id without going through the disk discovery walk.
#[doc(hidden)]
pub fn _register_for_test(info: Frei0rPluginInfo) {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.plugins.push(info);
}

// ─── Helper used by inspector when a frei0r effect is added ───────────────

/// Build a parameter list with default values for the named plugin.
/// Returns `None` when the plugin isn't installed.
pub fn default_params(type_id: &str) -> Option<Vec<Frei0rValue>> {
    let info = lookup_info(type_id)?;
    Some(
        info.params
            .iter()
            .map(|p| Frei0rValue::default_for(p.kind))
            .collect(),
    )
}

/// Convenience: list every plugin path we currently know about.
/// Useful for project-doctor "is this clip referencing a removed
/// plugin?" checks without exposing the full struct.
pub fn known_paths() -> Vec<PathBuf> {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .plugins
        .iter()
        .map(|p| p.path.clone())
        .collect()
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_frei0r::{ColorModel, ParamInfo, ParamKind, PluginType};

    fn fake_info() -> Frei0rPluginInfo {
        Frei0rPluginInfo {
            path: PathBuf::from("/tmp/nonexistent.so"),
            name: "FakeFx".to_string(),
            author: "test".to_string(),
            plugin_type: PluginType::Filter,
            color_model: ColorModel::Rgba8888,
            frei0r_version: 1,
            major_version: 0,
            minor_version: 0,
            explanation: String::new(),
            params: vec![
                ParamInfo {
                    name: "Amount".to_string(),
                    kind: ParamKind::Double,
                    explanation: String::new(),
                },
                ParamInfo {
                    name: "Active".to_string(),
                    kind: ParamKind::Bool,
                    explanation: String::new(),
                },
            ],
        }
    }

    #[test]
    fn apply_returns_passthrough_for_unknown_plugin() {
        _reset_for_test();
        let frame = vec![42u8; 16 * 16 * 4];
        let out = apply("frei0r:/nope/x.so:Bogus", &[], frame.clone(), 16, 16, 0.0);
        assert_eq!(out, frame, "unknown plugin must pass through unchanged");
    }

    #[test]
    fn default_params_match_plugin_kinds() {
        _reset_for_test();
        _register_for_test(fake_info());
        let params = default_params("frei0r:/tmp/nonexistent.so:FakeFx").unwrap();
        assert_eq!(params.len(), 2);
        assert!(matches!(params[0], Frei0rValue::Double(_)));
        assert!(matches!(params[1], Frei0rValue::Bool(_)));
    }

    #[test]
    fn supports_resolution_enforces_8_alignment() {
        assert!(supports_resolution("Cartoon", 1920, 1080));
        assert!(!supports_resolution("Cartoon", 1920, 1081));
        assert!(!supports_resolution("Cartoon", 1921, 1080));
    }

    #[test]
    fn supports_resolution_allows_4k_for_non_temporal() {
        assert!(supports_resolution("Cartoon", 3840, 2160));
    }

    #[test]
    fn supports_resolution_blocks_4k_for_temporal() {
        assert!(!supports_resolution("Delay0r", 3840, 2160));
        assert!(!supports_resolution("Baltan", 3840, 2160));
    }

    #[test]
    fn lookup_returns_none_for_missing() {
        _reset_for_test();
        assert!(lookup("frei0r:/nonexistent.so:Whatever").is_none());
    }

    #[test]
    fn registry_snapshot_round_trip() {
        _reset_for_test();
        _register_for_test(fake_info());
        let snap = registry_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].name, "FakeFx");
    }

    #[test]
    fn known_paths_lists_registered() {
        _reset_for_test();
        _register_for_test(fake_info());
        assert_eq!(
            known_paths(),
            vec![PathBuf::from("/tmp/nonexistent.so")]
        );
    }

    #[test]
    fn set_plugin_registry_replaces_loaded_state() {
        _reset_for_test();
        _register_for_test(fake_info());
        set_plugin_registry(vec![]);
        assert!(registry_snapshot().is_empty(), "set_plugin_registry must replace");
    }
}
