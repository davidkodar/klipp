//! frei0r plugin discovery + on-disk cache.
//!
//! Mirrors the LADSPA host pattern (`klipp-audio/src/ladspa.rs`):
//! search-path walk → per-`.so` dlopen → metadata extraction →
//! schema-versioned JSON cache keyed by directory mtime.
//!
//! ## Search path order (per frei0r spec §sec_pluglocations)
//!
//!   1. `~/.frei0r-1/lib`            (user)
//!   2. `/usr/local/lib/frei0r-1`    (locally installed)
//!   3. `/usr/lib/frei0r-1`          (distro)
//!
//! User overrides are loaded LAST so that a user-installed plugin with
//! the same name wins. The spec also defines a `FREI0R_PATH` colon-sep
//! env var that REPLACES the default list entirely when set.
//!
//! ## Vendor subdir recursion
//!
//! Plugins live either flat (`/usr/lib/frei0r-1/cartoon.so`) or under
//! an optional vendor subdir (`/usr/lib/frei0r-1/mob/cartoon.so`). We
//! walk one level deep — flat AND immediate subdirs — but no deeper.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ffi::*;
use crate::frei0r_guard;

// ─── Public types ─────────────────────────────────────────────────────────

/// frei0r plugin classification — stored as enum in the cache so v2's
/// support for SOURCE / MIXER2 / MIXER3 doesn't need a cache migration
/// when those variants flip from "skip" to "supported".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginType {
    Filter,
    Source,
    Mixer2,
    Mixer3,
    /// Anything we don't recognise. Spec §1.1 says applications must
    /// ignore unknown plugin_type / color_model values rather than
    /// crash; this variant carries that behaviour.
    Unknown,
}

impl PluginType {
    fn from_raw(raw: i32) -> Self {
        match raw {
            F0R_PLUGIN_TYPE_FILTER => Self::Filter,
            F0R_PLUGIN_TYPE_SOURCE => Self::Source,
            F0R_PLUGIN_TYPE_MIXER2 => Self::Mixer2,
            F0R_PLUGIN_TYPE_MIXER3 => Self::Mixer3,
            _ => Self::Unknown,
        }
    }
}

/// Color model the plugin works in. The host adapter in `instance.rs`
/// translates Klipp's RGBA8888 buffer into whichever model the plugin
/// declared, then translates back on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorModel {
    Bgra8888,
    Rgba8888,
    /// Endian-dependent packed 32-bit. On little-endian Linux this is
    /// byte-equivalent to RGBA8888; we treat them as the same in the
    /// adapter. Documented in `ffi::F0R_COLOR_MODEL_PACKED32`.
    Packed32,
    Unknown,
}

impl ColorModel {
    fn from_raw(raw: i32) -> Self {
        match raw {
            F0R_COLOR_MODEL_BGRA8888 => Self::Bgra8888,
            F0R_COLOR_MODEL_RGBA8888 => Self::Rgba8888,
            F0R_COLOR_MODEL_PACKED32 => Self::Packed32,
            _ => Self::Unknown,
        }
    }
}

/// One plugin parameter as discovered at scan time. Default values are
/// not in the frei0r ABI — the plugin sets them inside `f0r_construct`,
/// then the host reads them back via `f0r_get_param_value`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamInfo {
    pub name: String,
    pub kind: ParamKind,
    pub explanation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamKind {
    Bool,
    Double,
    Color,
    Position,
    String,
    Unknown,
}

impl ParamKind {
    fn from_raw(raw: i32) -> Self {
        match raw {
            F0R_PARAM_BOOL => Self::Bool,
            F0R_PARAM_DOUBLE => Self::Double,
            F0R_PARAM_COLOR => Self::Color,
            F0R_PARAM_POSITION => Self::Position,
            F0R_PARAM_STRING => Self::String,
            _ => Self::Unknown,
        }
    }
}

/// Everything the inspector / browser / project-doctor need about a
/// plugin without paying a dlopen each time. Persisted in the cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frei0rPluginInfo {
    pub path: PathBuf,
    /// Short plugin name from `f0r_get_plugin_info` — NOT necessarily
    /// the file stem (a single .so could expose any name in principle,
    /// although the convention is filename = name).
    pub name: String,
    pub author: String,
    pub plugin_type: PluginType,
    pub color_model: ColorModel,
    pub frei0r_version: i32,
    pub major_version: i32,
    pub minor_version: i32,
    pub explanation: String,
    pub params: Vec<ParamInfo>,
}

impl Frei0rPluginInfo {
    /// Project-stable identifier used in `BuiltinEffect::Frei0r.type_id`.
    /// Format: `frei0r:{path}:{name}`. Path-keyed (not name-keyed) so a
    /// vendor subdir collision can't alias two distinct plugins.
    pub fn type_id(&self) -> String {
        format!("frei0r:{}:{}", self.path.display(), self.name)
    }
}

// ─── Search paths ─────────────────────────────────────────────────────────

/// Default search paths in load order — later entries override earlier
/// ones (per spec §sec_order). When `FREI0R_PATH` is set, its
/// colon-separated entries REPLACE this list entirely.
fn default_search_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/usr/lib/frei0r-1"),
        // Some distros (Fedora-style) put 64-bit libs under lib64 even
        // though the frei0r spec only names /usr/lib. Mirror what the
        // LADSPA host does — defensive add.
        PathBuf::from("/usr/lib64/frei0r-1"),
        PathBuf::from("/usr/local/lib/frei0r-1"),
    ];
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".frei0r-1/lib"));
    } else if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(home).join(".frei0r-1/lib"));
    }
    paths
}

/// Resolve the actual search paths, honouring `FREI0R_PATH` override.
///
/// Symlinked directories (e.g. `/usr/lib64 -> lib` on Arch / CachyOS)
/// are deduped via `canonicalize` so we don't walk the same physical
/// directory twice and end up with N×2 plugin entries. Order is
/// preserved — first occurrence of a given canonical path wins.
/// Non-existent directories survive (canonicalize fails) so we keep
/// the spec's defensive listing of all known locations; they're
/// silently skipped at `read_dir` time downstream.
pub fn search_paths() -> Vec<PathBuf> {
    let raw: Vec<PathBuf> = if let Ok(env) = std::env::var("FREI0R_PATH") {
        env.split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect()
    } else {
        default_search_paths()
    };

    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(raw.len());
    for p in raw {
        // Canonical form when the directory exists (resolves symlinks
        // like /usr/lib64 -> lib). Falls back to the original path
        // when canonicalize fails — keeps the entry in the list so
        // the cache's mtime snapshot still records its absence as
        // part of the signature.
        let key = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if seen.insert(key) {
            out.push(p);
        }
    }
    out
}

/// Walk a directory one level deep — collect `.so` files at the top
/// AND `.so` files immediately inside any vendor subdir. Frei0r spec
/// allows both layouts.
fn collect_so_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("so") {
            out.push(path);
        } else if path.is_dir() {
            // One level of vendor recursion only.
            if let Ok(inner) = std::fs::read_dir(&path) {
                for inner_entry in inner.flatten() {
                    let inner_path = inner_entry.path();
                    if inner_path.extension().and_then(|s| s.to_str()) == Some("so") {
                        out.push(inner_path);
                    }
                }
            }
        }
    }
    out
}

// ─── Discovery ────────────────────────────────────────────────────────────

/// Discover every frei0r plugin on the system. Honours the global
/// `KLIPP_FREI0R_DISABLE` env var (any non-empty value disables the
/// host entirely, returning an empty list — useful for bisecting a
/// plugin-induced crash on startup).
pub fn discover_frei0r_plugins() -> Vec<Frei0rPluginInfo> {
    if std::env::var("KLIPP_FREI0R_DISABLE")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        tracing::info!("frei0r: discovery disabled via KLIPP_FREI0R_DISABLE");
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in search_paths() {
        for so_path in collect_so_files(&dir) {
            for info in scan_library(&so_path) {
                // Dedupe by type_id across the whole walk. Later
                // entries (user dirs) override earlier ones implicitly:
                // `seen` records the type_id, but since we walk in the
                // spec's load order, the FIRST occurrence wins. To get
                // the spec's "later wins" semantics we walk in REVERSE.
                let key = info.type_id();
                if seen.insert(key) {
                    out.push(info);
                }
            }
        }
    }
    tracing::info!("frei0r: discovered {} plugin(s)", out.len());
    out
}

/// Cached variant of [`discover_frei0r_plugins`]. JSON cache at
/// `<cache_dir>/frei0r_registry.json`. Schema-versioned + dir-mtime
/// keyed for invalidation.
pub fn discover_frei0r_plugins_cached(cache_dir: Option<&Path>) -> Vec<Frei0rPluginInfo> {
    let dirs = search_paths();

    if let Some(base) = cache_dir {
        if let Some(plugins) = cache::load(base, &dirs) {
            tracing::info!("frei0r: loaded {} plugin(s) from disk cache", plugins.len());
            return plugins;
        }
    }

    let plugins = discover_frei0r_plugins();

    if let Some(base) = cache_dir {
        if let Err(e) = cache::save(base, &dirs, &plugins) {
            tracing::warn!("frei0r: failed to write cache: {e}");
        }
    }

    plugins
}

// ─── Per-`.so` scan ───────────────────────────────────────────────────────

/// Open a single `.so`, look up the frei0r entry points, and extract
/// metadata. Returns at most one plugin (frei0r 1.x has one plugin per
/// `.so` — unlike LADSPA which can pack multiple descriptors).
///
/// Broken libraries are silently skipped at debug level — same policy
/// as the LADSPA + OFX hosts.
pub fn scan_library(path: &Path) -> Vec<Frei0rPluginInfo> {
    // SAFETY: dlopen of an arbitrary .so is inherently unsafe. Same
    // contract OFX + LADSPA hosts already accept.
    let lib = match unsafe { libloading::Library::new(path) } {
        Ok(l) => l,
        Err(e) => {
            tracing::debug!("frei0r: dlopen failed for {}: {e}", path.display());
            return Vec::new();
        }
    };

    // Resolve every required symbol up front — if any are missing,
    // this is not a frei0r plugin.
    let init_fn: F0rInitFn = match unsafe { lib.get::<F0rInitFn>(b"f0r_init\0") } {
        Ok(s) => *s,
        Err(_) => return Vec::new(),
    };
    let deinit_fn: F0rDeinitFn = match unsafe { lib.get::<F0rDeinitFn>(b"f0r_deinit\0") } {
        Ok(s) => *s,
        Err(_) => return Vec::new(),
    };
    let get_plugin_info_fn: F0rGetPluginInfoFn =
        match unsafe { lib.get::<F0rGetPluginInfoFn>(b"f0r_get_plugin_info\0") } {
            Ok(s) => *s,
            Err(_) => return Vec::new(),
        };
    let get_param_info_fn: F0rGetParamInfoFn =
        match unsafe { lib.get::<F0rGetParamInfoFn>(b"f0r_get_param_info\0") } {
            Ok(s) => *s,
            Err(_) => return Vec::new(),
        };

    // Per spec, f0r_init must succeed before any other call. Pair
    // with f0r_deinit on the way out — even on the metadata-only
    // discovery path.
    let init_ok = match frei0r_guard!({ unsafe { init_fn() } }) {
        Ok(rc) => rc != 0,
        Err(_) => false,
    };
    if !init_ok {
        tracing::debug!("frei0r: f0r_init failed for {}", path.display());
        return Vec::new();
    }

    let result = frei0r_guard!({
        let mut plugin_info = F0rPluginInfo {
            name: std::ptr::null(),
            author: std::ptr::null(),
            plugin_type: 0,
            color_model: 0,
            frei0r_version: 0,
            major_version: 0,
            minor_version: 0,
            num_params: 0,
            explanation: std::ptr::null(),
        };
        unsafe { get_plugin_info_fn(&mut plugin_info) };

        let info = Frei0rPluginInfo {
            path: path.to_path_buf(),
            name: cstr_to_string(plugin_info.name),
            author: cstr_to_string(plugin_info.author),
            plugin_type: PluginType::from_raw(plugin_info.plugin_type),
            color_model: ColorModel::from_raw(plugin_info.color_model),
            frei0r_version: plugin_info.frei0r_version,
            major_version: plugin_info.major_version,
            minor_version: plugin_info.minor_version,
            explanation: cstr_to_string(plugin_info.explanation),
            params: collect_params(get_param_info_fn, plugin_info.num_params),
        };

        // v1 supports Filter only — keep Source / Mixer in the cache
        // (so v2 doesn't need to rescan) but log a debug hint.
        if !matches!(info.plugin_type, PluginType::Filter) {
            tracing::debug!(
                "frei0r: '{}' is {:?} — registered but inactive in v1 (Filter-only)",
                info.name,
                info.plugin_type
            );
        }
        if matches!(info.color_model, ColorModel::Unknown) {
            tracing::debug!(
                "frei0r: '{}' has unknown color model {} — skipping",
                info.name,
                plugin_info.color_model
            );
            return None;
        }
        Some(info)
    });

    // Pair the f0r_init that succeeded above.
    let _ = frei0r_guard!({ unsafe { deinit_fn() } });

    match result {
        Ok(Some(info)) => vec![info],
        _ => Vec::new(),
    }
}

fn collect_params(get_param_info_fn: F0rGetParamInfoFn, count: i32) -> Vec<ParamInfo> {
    let mut out = Vec::new();
    if count <= 0 {
        return out;
    }
    // Cap defensively — frei0r doesn't bound num_params and a buggy
    // plugin could return absurd values.
    let count = count.min(256);
    for i in 0..count {
        let mut pi = F0rParamInfo {
            name: std::ptr::null(),
            param_type: 0,
            explanation: std::ptr::null(),
        };
        let res = frei0r_guard!({ unsafe { get_param_info_fn(&mut pi, i) } });
        if res.is_err() {
            break;
        }
        out.push(ParamInfo {
            name: cstr_to_string(pi.name),
            kind: ParamKind::from_raw(pi.param_type),
            explanation: cstr_to_string(pi.explanation),
        });
    }
    out
}

fn cstr_to_string(ptr: *const std::os::raw::c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

// ─── On-disk cache ────────────────────────────────────────────────────────

mod cache {
    use super::Frei0rPluginInfo;
    use serde::{Deserialize, Serialize};
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    /// Bump when [`super::Frei0rPluginInfo`] / [`super::ParamInfo`]
    /// gain or lose serialised fields, so old caches don't deserialise
    /// into the new shape with default-zero data.
    ///
    /// History:
    ///   1 — initial schema (v1 of the host: filter-only, full
    ///       ParamKind enum exhaustive, PluginType enum exhaustive)
    pub(super) const SCHEMA_VERSION: u32 = 1;

    pub(super) const FILE_NAME: &str = "frei0r_registry.json";

    #[derive(Serialize, Deserialize)]
    struct DiskCache {
        version: u32,
        dir_signatures: Vec<DirSig>,
        plugins: Vec<Frei0rPluginInfo>,
    }

    #[derive(Serialize, Deserialize, PartialEq, Eq)]
    struct DirSig {
        path: String,
        mtime_secs: Option<u64>,
    }

    fn cache_file(base: &Path) -> PathBuf {
        base.join(FILE_NAME)
    }

    fn collect_signatures(dirs: &[PathBuf]) -> Vec<DirSig> {
        dirs.iter()
            .map(|d| {
                let mtime_secs = std::fs::metadata(d)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                DirSig {
                    path: d.to_string_lossy().into_owned(),
                    mtime_secs,
                }
            })
            .collect()
    }

    pub(super) fn load(base: &Path, dirs: &[PathBuf]) -> Option<Vec<Frei0rPluginInfo>> {
        let path = cache_file(base);
        let data = std::fs::read_to_string(&path).ok()?;
        let cache: DiskCache = serde_json::from_str(&data).ok()?;
        if cache.version != SCHEMA_VERSION {
            tracing::debug!(
                "frei0r cache: schema mismatch ({} vs {}), rescanning",
                cache.version,
                SCHEMA_VERSION
            );
            return None;
        }
        if cache.dir_signatures != collect_signatures(dirs) {
            tracing::debug!("frei0r cache: directory mtime changed, rescanning");
            return None;
        }
        Some(cache.plugins)
    }

    pub(super) fn save(
        base: &Path,
        dirs: &[PathBuf],
        plugins: &[Frei0rPluginInfo],
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(base)?;
        let cache = DiskCache {
            version: SCHEMA_VERSION,
            dir_signatures: collect_signatures(dirs),
            plugins: plugins.to_vec(),
        };
        let json = serde_json::to_string(&cache).map_err(std::io::Error::other)?;
        std::fs::write(cache_file(base), json)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_id_path_keyed() {
        let info = Frei0rPluginInfo {
            path: PathBuf::from("/usr/lib/frei0r-1/cartoon.so"),
            name: "cartoon".to_string(),
            author: String::new(),
            plugin_type: PluginType::Filter,
            color_model: ColorModel::Rgba8888,
            frei0r_version: 1,
            major_version: 0,
            minor_version: 0,
            explanation: String::new(),
            params: vec![],
        };
        assert_eq!(info.type_id(), "frei0r:/usr/lib/frei0r-1/cartoon.so:cartoon");
    }

    #[test]
    fn search_paths_default_includes_user_home_last() {
        // Keep FREI0R_PATH unset for this test — verify the default ordering.
        std::env::remove_var("FREI0R_PATH");
        let paths = search_paths();
        // System paths come first; user home (if present) comes last.
        assert!(!paths.is_empty());
        assert!(paths[0].to_string_lossy().contains("/usr/lib/frei0r-1"));
    }

    #[test]
    fn search_paths_env_override_replaces_defaults() {
        std::env::set_var("FREI0R_PATH", "/tmp/a:/tmp/b");
        let paths = search_paths();
        assert_eq!(paths, vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]);
        std::env::remove_var("FREI0R_PATH");
    }

    #[test]
    fn search_paths_env_skips_empty_segments() {
        std::env::set_var("FREI0R_PATH", "/tmp/a::/tmp/b:");
        let paths = search_paths();
        assert_eq!(paths, vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]);
        std::env::remove_var("FREI0R_PATH");
    }

    #[test]
    fn search_paths_dedupe_symlinked_dirs() {
        // Closes the 322-vs-162 bug observed at first run on CachyOS:
        // /usr/lib64 -> /usr/lib means default_search_paths() lists
        // both paths, but they resolve to the same physical dir and
        // discovery would otherwise walk it twice. Construct an
        // equivalent override via FREI0R_PATH using two paths that
        // canonicalize to the same temp dir.
        let tmp = std::env::temp_dir();
        let target = tmp.join("klipp-frei0r-symlink-test");
        let _ = std::fs::create_dir_all(&target);
        let symlink = tmp.join("klipp-frei0r-symlink-test-link");
        // Best-effort recreate a fresh symlink each run.
        let _ = std::fs::remove_file(&symlink);
        if std::os::unix::fs::symlink(&target, &symlink).is_err() {
            // fs may not allow symlinks (e.g. CI sandbox) — skip.
            return;
        }
        let env_value = format!("{}:{}", target.display(), symlink.display());
        std::env::set_var("FREI0R_PATH", env_value);
        let paths = search_paths();
        std::env::remove_var("FREI0R_PATH");
        let _ = std::fs::remove_file(&symlink);
        let _ = std::fs::remove_dir(&target);
        assert_eq!(
            paths.len(),
            1,
            "two paths that canonicalize to the same dir must collapse to one"
        );
    }

    #[test]
    fn klipp_frei0r_disable_short_circuits_discovery() {
        std::env::set_var("KLIPP_FREI0R_DISABLE", "1");
        let plugins = discover_frei0r_plugins();
        assert!(plugins.is_empty(), "disable env must short-circuit");
        std::env::remove_var("KLIPP_FREI0R_DISABLE");
    }

    #[test]
    fn cache_round_trip_via_serde() {
        let info = Frei0rPluginInfo {
            path: PathBuf::from("/usr/lib/frei0r-1/cartoon.so"),
            name: "cartoon".to_string(),
            author: "Maksim Golovkin".to_string(),
            plugin_type: PluginType::Filter,
            color_model: ColorModel::Rgba8888,
            frei0r_version: 1,
            major_version: 0,
            minor_version: 1,
            explanation: "Cartoon-style outline".to_string(),
            params: vec![
                ParamInfo {
                    name: "Threshold".to_string(),
                    kind: ParamKind::Double,
                    explanation: "Edge threshold".to_string(),
                },
                ParamInfo {
                    name: "Color".to_string(),
                    kind: ParamKind::Color,
                    explanation: String::new(),
                },
            ],
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: Frei0rPluginInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, info.name);
        assert_eq!(back.params.len(), info.params.len());
        assert_eq!(back.params[0].kind, ParamKind::Double);
        assert_eq!(back.params[1].kind, ParamKind::Color);
    }

    /// Closes audit point §α — if frei0r is installed, scan it and assert
    /// at least one plugin was discovered. Gated on the canonical
    /// install path so CI without frei0r still passes.
    #[test]
    fn discover_finds_at_least_one_plugin_when_installed() {
        if !std::path::Path::new("/usr/lib/frei0r-1").is_dir() {
            eprintln!("skipping: /usr/lib/frei0r-1 not present");
            return;
        }
        std::env::remove_var("KLIPP_FREI0R_DISABLE");
        let plugins = discover_frei0r_plugins();
        assert!(
            !plugins.is_empty(),
            "system frei0r-1 dir is present but discovery yielded zero plugins"
        );
    }
}
