//! Bridge between Klipp's `BuiltinEffect::Frei0r` chain entry and
//! the `klipp-frei0r` host crate.
//!
//! Owns the process-global runtime cache that maps a frei0r `type_id`
//! (`"frei0r:{path}:{name}"`) to a single dlopened `Arc<Frei0rPlugin>`.
//! A clip's frei0r plug-in renders as a node, with its instances kept
//! across frames by the render (`klipp_nodes::frei0r_node`, M4 step 5).
//! [`apply`] renders one frame on instances of its own: for a preview.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use klipp_frei0r::ffi::F0R_MAX_DIM;
use klipp_frei0r::{temporal, Frei0rInstanceSet, Frei0rPlugin, Frei0rPluginInfo, Frei0rValue};

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
    /// Ids a render found no plug-in for, said once each (#3200).
    said_missing: HashSet<String>,
}

fn cache() -> &'static Mutex<RuntimeCache> {
    static CACHE: OnceLock<Mutex<RuntimeCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(RuntimeCache {
            plugins: Vec::new(),
            loaded: HashMap::new(),
            failed: HashMap::new(),
            said_missing: HashSet::new(),
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
    c.said_missing.clear();
}

/// The installed plug-in `type_id` names (#3200): the one with that exact
/// id, else one from a file of the same name whose reported name matches
/// but for case, wherever it is installed. An id written on another
/// machine (frei0r in /usr/lib64, or ~/.frei0r-1), or with its name in
/// another case (`invert0r` for `Invert0r`), finds it; `None` when no
/// installed plug-in matches.
fn resolve(plugins: &[Frei0rPluginInfo], type_id: &str) -> Option<Frei0rPluginInfo> {
    if let Some(p) = plugins.iter().find(|p| p.type_id() == type_id) {
        return Some(p.clone());
    }
    let (path, name) = type_id.strip_prefix("frei0r:")?.rsplit_once(':')?;
    let path = Path::new(path);
    let file = path.file_name()?;
    let same = |p: &&Frei0rPluginInfo| p.path.file_name() == Some(file) && p.name.eq_ignore_ascii_case(name);
    plugins.iter().filter(same).find(|p| p.path == path).or_else(|| plugins.iter().find(same)).cloned()
}

fn lookup_info(type_id: &str) -> Option<Frei0rPluginInfo> {
    let c = cache().lock().unwrap_or_else(|e| e.into_inner());
    resolve(&c.plugins, type_id)
}

/// `type_id` as its installed plug-in's own id, to store (#3200), or why
/// there is none: no installed plug-in matches it, even by file and name
/// but for case, and the error names the installed ones nearest it. A
/// frei0r effect with an id nothing renders used to be taken, and the
/// picture passed through.
pub fn canonical_type_id(type_id: &str) -> Result<String, String> {
    let c = cache().lock().unwrap_or_else(|e| e.into_inner());
    match resolve(&c.plugins, type_id) {
        Some(p) => Ok(p.type_id()),
        None => Err(unknown(&c.plugins, type_id)),
    }
}

/// Why `type_id` isn't installed, naming the installed plug-ins whose
/// names begin most like its own.
fn unknown(plugins: &[Frei0rPluginInfo], type_id: &str) -> String {
    if plugins.is_empty() {
        return format!("no frei0r plug-in {type_id}: none are installed");
    }
    let wanted = type_id.rsplit(':').next().unwrap_or(type_id).to_lowercase();
    let shared = |name: &str| name.to_lowercase().chars().zip(wanted.chars()).take_while(|(a, b)| a == b).count();
    let mut near: Vec<&Frei0rPluginInfo> = plugins.iter().collect();
    near.sort_by(|a, b| shared(&b.name).cmp(&shared(&a.name)).then_with(|| a.name.cmp(&b.name)));
    let named: Vec<String> = near.iter().take(6).map(|p| p.type_id()).collect();
    format!("no frei0r plug-in {type_id} is installed; {} are, among them {}", plugins.len(), named.join(", "))
}

/// Whether a render finding no plug-in for `type_id` is the first since
/// the plug-ins were last listed (#3200): it is said once, not each frame.
pub fn first_missing(type_id: &str) -> bool {
    cache().lock().unwrap_or_else(|e| e.into_inner()).said_missing.insert(type_id.to_owned())
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

/// One frame of a frei0r effect on instances of its own, made and dropped
/// here: a preview's (the effects browser's thumbnails). A clip's renders
/// go through its node, whose instances last. A size frei0r can't take is
/// padded and cropped back; above 2048 the frame is tiled, except for a
/// plug-in that keeps frame history.
///
/// A refusal, or a plug-in that isn't available, returns the frame
/// unchanged with a `warn!`: a preview has nowhere to say so.
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
    let rendered = Frei0rInstanceSet::new(plugin, width, height).and_then(|mut set| {
        set.set_params(params);
        set.render(time_secs, &rgba)
    });
    match rendered {
        Ok(r) => r.rgba,
        Err(refusal) => {
            tracing::warn!("frei0r: {refusal} — passthrough");
            rgba
        }
    }
}

/// The loaded plug-in `type_id` names, opened once and shared: what a
/// frei0r node keeps its instances against. `None` when it isn't
/// installed or won't open.
pub fn plugin(type_id: &str) -> Option<Arc<Frei0rPlugin>> {
    get_or_load(type_id)
}

/// Whether `type_id`'s plug-in keeps frame history (the measured list,
/// `klipp_frei0r::temporal`): its frame depends on the frames rendered
/// before it, so nothing may keep its output by frame. A plug-in that
/// isn't known is taken to.
pub fn keeps_history(type_id: &str) -> bool {
    lookup_info(type_id).is_none_or(|info| klipp_frei0r::temporal::is_temporal(&info))
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

/// Whether `plugin` can render `width × height` frames: every size but
/// one above 2048 for a plug-in that keeps frame history, which can't
/// be tiled. Other sizes frei0r can't take are padded and cropped back.
pub fn supports_resolution(plugin: &Frei0rPluginInfo, width: u32, height: u32) -> bool {
    !(temporal::is_temporal(plugin) && (width > F0R_MAX_DIM || height > F0R_MAX_DIM))
}

/// Test-only: clear the runtime cache. Used by integration tests to
/// guarantee a clean state between runs.
#[doc(hidden)]
pub fn _reset_for_test() {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.plugins.clear();
    c.loaded.clear();
    c.failed.clear();
    c.said_missing.clear();
}

/// Test-only: register a single plugin so apply() can resolve a
/// type_id without going through the disk discovery walk.
#[doc(hidden)]
pub fn _register_for_test(info: Frei0rPluginInfo) {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.plugins.push(info);
}

/// Test-only: register a filter plug-in with no parameters by its file and
/// its reported name, for a crate without `klipp-frei0r`'s types to say
/// which plug-ins are "installed". Its file need not exist.
#[doc(hidden)]
pub fn _register_named_for_test(path: &str, name: &str) {
    use klipp_frei0r::{ColorModel, PluginType};
    _register_for_test(Frei0rPluginInfo {
        path: PathBuf::from(path),
        name: name.to_owned(),
        author: String::new(),
        plugin_type: PluginType::Filter,
        color_model: ColorModel::Rgba8888,
        frei0r_version: 1,
        major_version: 0,
        minor_version: 0,
        explanation: String::new(),
        params: Vec::new(),
    });
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
    /// The registry these tests reset and fill is global, so they cannot
    /// run beside each other: one would clear the registry another had
    /// just written, which failed at random depending on the schedule.
    /// Each takes this lock for its whole body.
    static REGISTRY: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let _guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        _reset_for_test();
        let frame = vec![42u8; 16 * 16 * 4];
        let out = apply("frei0r:/nope/x.so:Bogus", &[], frame.clone(), 16, 16, 0.0);
        assert_eq!(out, frame, "unknown plugin must pass through unchanged");
    }

    #[test]
    fn default_params_match_plugin_kinds() {
        let _guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        _reset_for_test();
        _register_for_test(fake_info());
        let params = default_params("frei0r:/tmp/nonexistent.so:FakeFx").unwrap();
        assert_eq!(params.len(), 2);
        assert!(matches!(params[0], Frei0rValue::Double(_)));
        assert!(matches!(params[1], Frei0rValue::Bool(_)));
    }

    fn at(path: &str) -> Frei0rPluginInfo {
        Frei0rPluginInfo { path: PathBuf::from(path), ..fake_info() }
    }

    #[test]
    fn supports_resolution_takes_unaligned_sizes() {
        // Padded and cropped back, not refused (#1469).
        assert!(supports_resolution(&at("/x/cartoon.so"), 1920, 1080));
        assert!(supports_resolution(&at("/x/cartoon.so"), 1920, 1081));
        assert!(supports_resolution(&at("/x/cartoon.so"), 960, 540));
    }

    #[test]
    fn supports_resolution_allows_4k_for_non_temporal() {
        assert!(supports_resolution(&at("/x/cartoon.so"), 3840, 2160));
    }

    #[test]
    fn supports_resolution_blocks_4k_for_temporal() {
        assert!(!supports_resolution(&at("/usr/lib/frei0r-1/delay0r.so"), 3840, 2160));
        assert!(!supports_resolution(&at("/usr/lib/frei0r-1/baltan.so"), 3840, 2160));
        // By file, whatever the display name (#1472).
        assert!(!supports_resolution(&Frei0rPluginInfo { name: "Light Graffiti".into(), ..at("/usr/lib/frei0r-1/lightgraffiti.so") }, 3840, 2160));
        assert!(supports_resolution(&at("/usr/lib/frei0r-1/delay0r.so"), 1920, 1080));
    }

    #[test]
    fn lookup_returns_none_for_missing() {
        let _guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        _reset_for_test();
        assert!(lookup("frei0r:/nonexistent.so:Whatever").is_none());
    }

    #[test]
    fn registry_snapshot_round_trip() {
        let _guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        _reset_for_test();
        _register_for_test(fake_info());
        let snap = registry_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].name, "FakeFx");
    }

    #[test]
    fn known_paths_lists_registered() {
        let _guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        _reset_for_test();
        _register_for_test(fake_info());
        assert_eq!(
            known_paths(),
            vec![PathBuf::from("/tmp/nonexistent.so")]
        );
    }

    #[test]
    fn set_plugin_registry_replaces_loaded_state() {
        let _guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        _reset_for_test();
        _register_for_test(fake_info());
        set_plugin_registry(vec![]);
        assert!(registry_snapshot().is_empty(), "set_plugin_registry must replace");
    }

    /// #3200: an id finds its plug-in exactly, or by its file and its
    /// name but for case wherever the file is installed (another machine's
    /// folder); one nothing matches finds none, and storing it is refused
    /// with the installed ids nearest it named.
    #[test]
    fn an_id_finds_its_plug_in_by_file_and_name_but_for_case() {
        let at = |path: &str, name: &str| Frei0rPluginInfo { path: PathBuf::from(path), name: name.into(), ..fake_info() };
        let plugins = vec![at("/usr/lib/frei0r-1/invert0r.so", "Invert0r"), at("/usr/lib/frei0r-1/saturat0r.so", "saturat0r"), at("/home/me/.frei0r-1/lib/invert0r.so", "Invert0r")];
        let found = |id: &str| resolve(&plugins, id).map(|p| p.type_id());
        assert_eq!(found("frei0r:/usr/lib/frei0r-1/invert0r.so:Invert0r").as_deref(), Some("frei0r:/usr/lib/frei0r-1/invert0r.so:Invert0r"), "exactly");
        assert_eq!(found("frei0r:/home/me/.frei0r-1/lib/invert0r.so:invert0r").as_deref(), Some("frei0r:/home/me/.frei0r-1/lib/invert0r.so:Invert0r"), "its own path first, the name but for case");
        assert_eq!(found("frei0r:/usr/lib64/frei0r-1/invert0r.so:invert0r").as_deref(), Some("frei0r:/usr/lib/frei0r-1/invert0r.so:Invert0r"), "another machine's folder");
        assert_eq!(found("frei0r:/usr/lib/frei0r-1/invert0r.so:Saturat0r"), None, "another plug-in's name in that file");
        assert_eq!(found("frei0r:/usr/lib/frei0r-1/bogus.so:bogus"), None);
        assert_eq!(found("invert0r"), None, "not an id");
        let why = unknown(&plugins, "frei0r:/x/inverter.so:inverter");
        assert!(why.contains("3 are") && why.find("invert0r.so").unwrap() < why.find("saturat0r.so").unwrap(), "the nearest named first: {why}");
        assert!(unknown(&[], "frei0r:/x.so:x").contains("none are installed"));
    }
}
