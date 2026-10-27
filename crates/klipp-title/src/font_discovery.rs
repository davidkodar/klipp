//! System font discovery with deduplication, on-disk caching, and background
//! scanning.
//!
//! ## Why this module is more complicated than it looks
//!
//! Discovering ~280 fonts on a typical Linux desktop costs ~5.5 seconds because
//! `parse_font_info` reads every TTF/OTF off disk and runs it through fontdue
//! just to validate. Without this module, three independent callers were doing
//! that work serially on the startup path, eating ~16s of launch time.
//!
//! This module fixes the cost three ways:
//!
//! 1. **Dedupe** — `discover_system_fonts()` returns an `Arc<FontRegistry>`
//!    cached in a process-wide `OnceLock`. The first call does the real scan;
//!    every subsequent call is a near-free `Arc::clone`.
//!
//! 2. **Disk cache** — after a successful scan, the registry is serialized to
//!    `~/.cache/klipp/font_registry.json` together with the mtimes of every
//!    scanned directory. On the next launch we mtime-check those directories
//!    and reload the cache if nothing has changed. Cache hit ≈ 5–20 ms.
//!
//! 3. **Background scan** — `discover_system_fonts_async()` kicks the scan off
//!    on a background thread. Callers that need the registry sooner block on
//!    the in-flight scan instead of starting their own. The launch screen can
//!    therefore start the scan as the very first thing in `main()` and let it
//!    run in parallel with the rest of init.

use crate::variable::{face_traits, FaceTraits, VariationAxis};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::SystemTime;

/// A discovered font family with its file path and style info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontInfo {
    pub family: String,
    pub style: String, // "Regular", "Bold", "Italic", "Bold Italic", etc.
    pub path: PathBuf,
    /// The face's weight, 100 to 900, from its OS/2 table (#1504).
    #[serde(default = "regular_weight")]
    pub weight: u16,
    /// Whether the face is italic or oblique (#1504).
    #[serde(default)]
    pub italic: bool,
    /// A variable face's axes; empty for a static face (#1504).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub axes: Vec<VariationAxis>,
}

fn regular_weight() -> u16 {
    400
}

impl FontInfo {
    /// The face's weight, slope and axes, for
    /// [`nearest_face`](crate::variable::nearest_face).
    pub fn traits(&self) -> FaceTraits {
        FaceTraits { weight: self.weight, italic: self.italic, axes: self.axes.clone() }
    }
}

/// The faces of `family` in `registry`, one per weight and slope, in
/// weight order with the upright before the italic (#1504): what the
/// font picker offers. A variable face's weight axis offers the
/// standard weights in its range, each named by its weight.
pub fn family_faces(registry: &FontRegistry, family: &str) -> Vec<FaceTraits> {
    let mut out: Vec<FaceTraits> = Vec::new();
    for info in registry.get(family).into_iter().flatten() {
        let traits = info.traits();
        let (lo, hi) = traits.weights();
        let weights: Vec<u16> = if lo == hi { vec![traits.weight] } else { (1..=9).map(|k| k * 100).filter(|w| (lo..=hi).contains(w)).collect() };
        for weight in weights {
            if !out.iter().any(|f| f.weight == weight && f.italic == traits.italic) {
                out.push(FaceTraits { weight, ..traits.clone() });
            }
        }
    }
    out.sort_by_key(|f| (f.weight, f.italic));
    out
}

/// Whether a title naming `family` draws in it (#1720): the default
/// face (an empty name or "sans-serif"), a family that ships with the
/// app, or one installed on this computer. Any other name draws in a
/// fallback.
pub fn font_installed(registry: &FontRegistry, family: &str) -> bool {
    family.is_empty() || family == "sans-serif" || crate::bundled::BUNDLED.iter().any(|f| f.family == family) || registry.contains_key(family)
}

/// The name a weight goes by: "Thin" for 100 up to "Black" for 900,
/// the nearest hundred for another.
pub fn weight_name(weight: u16) -> &'static str {
    match (weight + 50) / 100 {
        0 | 1 => "Thin",
        2 => "Extra Light",
        3 => "Light",
        4 => "Regular",
        5 => "Medium",
        6 => "Semi Bold",
        7 => "Bold",
        8 => "Extra Bold",
        _ => "Black",
    }
}

/// The OpenType feature tags the face in `data` has, in its GSUB and
/// GPOS feature lists, sorted and each once (#1710): what the Title
/// tab's feature toggles and stylistic-set picker look up. Empty for a
/// file read-fonts cannot read.
pub fn face_feature_tags(data: &[u8]) -> Vec<String> {
    use read_fonts::{FontRef, TableProvider};
    let Ok(font) = FontRef::from_index(data, 0) else { return Vec::new() };
    let mut tags: Vec<String> = Vec::new();
    if let Ok(list) = font.gsub().and_then(|t| t.feature_list()) {
        tags.extend(list.feature_records().iter().map(|r| r.feature_tag().to_string()));
    }
    if let Ok(list) = font.gpos().and_then(|t| t.feature_list()) {
        tags.extend(list.feature_records().iter().map(|r| r.feature_tag().to_string()));
    }
    tags.sort();
    tags.dedup();
    tags
}

/// The feature tags of `family`'s face in `style` ("Regular", "Bold",
/// "Italic" or "Bold Italic"), found as the renderer finds the face: a
/// bundled family's own, else `registry`'s file (its Regular, then its
/// first, when the style is missing), and the built-in face for
/// "sans-serif" or no family (#1710). Each face is read once and kept;
/// empty for a family not found.
pub fn family_feature_tags(registry: &FontRegistry, family: &str, style: &str) -> Vec<String> {
    static READ: OnceLock<Mutex<BTreeMap<(String, String), Vec<String>>>> = OnceLock::new();
    let key = (family.to_string(), style.to_string());
    let read = READ.get_or_init(Default::default);
    if let Some(tags) = read.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return tags.clone();
    }
    let tags = if family == "sans-serif" || family.is_empty() {
        face_feature_tags(crate::EMBEDDED_FONT)
    } else if let Some(bytes) = crate::bundled::lookup(family, style) {
        face_feature_tags(bytes)
    } else {
        registry
            .get(family)
            .and_then(|infos| infos.iter().find(|i| i.style == style).or_else(|| infos.iter().find(|i| i.style == "Regular")).or_else(|| infos.first()))
            .and_then(|info| std::fs::read(&info.path).ok())
            .map(|data| face_feature_tags(&data))
            .unwrap_or_default()
    };
    read.lock().unwrap_or_else(|e| e.into_inner()).insert(key, tags.clone());
    tags
}

/// A shared, immutable registry of discovered fonts grouped by family.
/// Cheap to clone — internally an `Arc`.
pub type FontRegistry = Arc<BTreeMap<String, Vec<FontInfo>>>;

// ---------------------------------------------------------------------------
// Process-wide deduplication state
// ---------------------------------------------------------------------------

/// Final registry, set exactly once when the first scan completes.
static REGISTRY: OnceLock<FontRegistry> = OnceLock::new();

/// Coordination state for in-flight background scans. Callers that need the
/// registry while a scan is running park on `cvar` instead of starting a
/// duplicate scan.
struct ScanCoordinator {
    /// True if a background scan is currently running.
    in_flight: bool,
}

static SCAN_STATE: OnceLock<(Mutex<ScanCoordinator>, Condvar)> = OnceLock::new();

fn scan_state() -> &'static (Mutex<ScanCoordinator>, Condvar) {
    SCAN_STATE.get_or_init(|| (Mutex::new(ScanCoordinator { in_flight: false }), Condvar::new()))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Return the system font registry, blocking only as long as needed.
///
/// Resolution order:
/// 1. If a previous call already produced a registry, return it instantly.
/// 2. If a background scan is in flight, wait for it to finish.
/// 3. Otherwise, run the scan synchronously on the calling thread.
///
/// In all three cases the result is shared via `Arc` — callers should not
/// expect to mutate the returned map.
pub fn discover_system_fonts() -> FontRegistry {
    if let Some(reg) = REGISTRY.get() {
        return reg.clone();
    }

    // Take the lock and decide whether to wait or run the scan ourselves.
    let (mu, cvar) = scan_state();
    let mut state = mu.lock().unwrap_or_else(|e| e.into_inner());
    if state.in_flight {
        // Someone else (typically the background prefetch) is already running
        // a scan. Park here until they fulfil REGISTRY.
        while REGISTRY.get().is_none() {
            state = cvar.wait(state).unwrap();
        }
        return REGISTRY.get().expect("registry filled before notify").clone();
    }
    // We're the first caller — mark a scan as in-flight and run it on this thread.
    state.in_flight = true;
    drop(state);
    run_scan_and_publish();
    REGISTRY.get().expect("registry filled by run_scan_and_publish").clone()
}

/// Kick off a background scan if one hasn't been started yet. Returns
/// immediately. Subsequent calls to `discover_system_fonts()` will reuse the
/// in-flight scan instead of doing the work themselves.
///
/// This is the canonical "start font discovery as the very first thing in
/// `main()`" entry point.
pub fn discover_system_fonts_async() {
    if REGISTRY.get().is_some() {
        return;
    }
    let (mu, _cvar) = scan_state();
    let mut state = mu.lock().unwrap_or_else(|e| e.into_inner());
    if state.in_flight {
        return;
    }
    state.in_flight = true;
    drop(state);

    // Spawn a background worker. The thread is detached on purpose — its
    // entire job is to fill REGISTRY and notify the condvar, after which
    // there's no more work to do.
    let _ = std::thread::Builder::new()
        .name("klipp-font-scan".into())
        .spawn(run_scan_and_publish);
}

// ---------------------------------------------------------------------------
// Scan + publish + cache
// ---------------------------------------------------------------------------

/// Run the actual scan (or load from cache), publish to `REGISTRY`, and wake
/// any waiters parked on the condvar.
fn run_scan_and_publish() {
    let t0 = std::time::Instant::now();
    let dirs = font_directories();

    // Try the on-disk cache first. It's only valid if every scanned directory's
    // mtime matches the snapshot embedded in the cache file.
    let registry: FontRegistry = match load_disk_cache(&dirs) {
        Some(loaded) => {
            tracing::info!(
                "Font discovery: loaded {} families from disk cache",
                loaded.len()
            );
            Arc::new(loaded)
        }
        None => {
            let mut fonts: BTreeMap<String, Vec<FontInfo>> = BTreeMap::new();
            for dir in &dirs {
                scan_directory(Path::new(dir), &mut fonts);
            }
            tracing::info!("Font discovery: scanned {} families from disk", fonts.len());
            // Persist the result so the next launch is fast. Failures here are
            // non-fatal — the cache is purely an optimization.
            if let Err(e) = save_disk_cache(&dirs, &fonts) {
                tracing::warn!("Font discovery: failed to write cache: {e}");
            }
            Arc::new(fonts)
        }
    };

    // The families that ship with the app join the list so the font
    // picker offers them; the renderer loads them from memory, never
    // from these paths.
    let registry: FontRegistry = {
        let mut fonts: BTreeMap<String, Vec<FontInfo>> = (*registry).clone();
        for (family, styles) in crate::bundled::families() {
            let entry = fonts.entry(family.to_string()).or_default();
            for style in styles {
                if !entry.iter().any(|i| i.style == style) {
                    let traits = crate::bundled::lookup(family, style).and_then(face_traits);
                    entry.push(FontInfo {
                        family: family.to_string(),
                        style: style.to_string(),
                        path: PathBuf::from(format!("bundled://{family}/{style}")),
                        weight: traits.as_ref().map_or(400, |t| t.weight),
                        italic: traits.as_ref().is_some_and(|t| t.italic),
                        axes: traits.map(|t| t.axes).unwrap_or_default(),
                    });
                }
            }
        }
        Arc::new(fonts)
    };

    // Publish + notify waiters under the lock so no one races between
    // `REGISTRY.set` and `cvar.notify_all`.
    let (mu, cvar) = scan_state();
    let mut state = mu.lock().unwrap_or_else(|e| e.into_inner());
    let _ = REGISTRY.set(registry);
    state.in_flight = false;
    cvar.notify_all();
    tracing::info!(
        "STARTUP_PERF font_discovery: scan + publish               {:>9.1} ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
}

pub(crate) fn font_directories() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    vec![
        "/usr/share/fonts".to_string(),
        "/usr/local/share/fonts".to_string(),
        format!("{home}/.local/share/fonts"),
        // The catalogue's fetched families (#2732).
        crate::catalogue::fonts_dir().to_string_lossy().into_owned(),
    ]
}

/// The fonts in one folder and the folders under it, by family: a
/// fetched catalogue family's faces (#2732).
pub fn scan_folder(dir: &Path) -> BTreeMap<String, Vec<FontInfo>> {
    let mut fonts = BTreeMap::new();
    scan_directory(dir, &mut fonts);
    fonts
}

fn scan_directory(dir: &Path, fonts: &mut BTreeMap<String, Vec<FontInfo>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_directory(&path, fonts);
        } else if let Some(ext) = path.extension() {
            let ext = ext.to_string_lossy().to_lowercase();
            if ext == "ttf" || ext == "otf" {
                if let Some(info) = parse_font_info(&path) {
                    fonts.entry(info.family.clone()).or_default().push(info);
                }
            }
        }
    }
}

fn parse_font_info(path: &Path) -> Option<FontInfo> {
    // Validate this is actually a loadable font by trying to parse it.
    // We read the file, but only to validate — we don't keep the Font object.
    let data = std::fs::read(path).ok()?;
    let _font =
        fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings::default()).ok()?;

    let filename = path.file_stem()?.to_string_lossy().to_string();
    let (family, style) = parse_font_name(&filename);
    let traits = face_traits(&data);

    Some(FontInfo {
        family,
        style,
        path: path.to_path_buf(),
        weight: traits.as_ref().map_or(400, |t| t.weight),
        italic: traits.as_ref().is_some_and(|t| t.italic),
        axes: traits.map(|t| t.axes).unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------------
// Disk cache
// ---------------------------------------------------------------------------

/// Persisted form of the font registry plus the mtimes that produced it.
/// We rescan iff any of those mtimes have moved.
#[derive(Serialize, Deserialize)]
struct DiskCache {
    /// Schema version — bump if `FontInfo` ever gains/loses fields.
    version: u32,
    /// Per-directory mtime snapshots taken at scan time. Recursive walks
    /// rely on the fact that adding/removing/renaming a font file inside a
    /// subdirectory bumps the parent directory's mtime.
    dir_signatures: Vec<DirSig>,
    /// The discovered fonts grouped by family — same shape as `FontRegistry`.
    fonts: BTreeMap<String, Vec<FontInfo>>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct DirSig {
    path: String,
    /// Mtime in seconds since UNIX epoch. `None` for directories that don't
    /// exist (so a future install of fonts there forces a rescan).
    mtime_secs: Option<u64>,
}

/// 2: each face's weight, slope and axes (#1504).
const CACHE_SCHEMA_VERSION: u32 = 2;

fn cache_path() -> Option<PathBuf> {
    let base = dirs::cache_dir()?.join("klipp");
    let _ = std::fs::create_dir_all(&base);
    Some(base.join("font_registry.json"))
}

fn collect_signatures(dirs: &[String]) -> Vec<DirSig> {
    dirs.iter()
        .map(|d| {
            let mtime_secs = std::fs::metadata(d)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            DirSig {
                path: d.clone(),
                mtime_secs,
            }
        })
        .collect()
}

fn load_disk_cache(dirs: &[String]) -> Option<BTreeMap<String, Vec<FontInfo>>> {
    let path = cache_path()?;
    let data = std::fs::read_to_string(&path).ok()?;
    let cache: DiskCache = serde_json::from_str(&data).ok()?;
    if cache.version != CACHE_SCHEMA_VERSION {
        tracing::debug!(
            "Font cache: schema mismatch ({} vs {}), rescanning",
            cache.version,
            CACHE_SCHEMA_VERSION
        );
        return None;
    }
    let current = collect_signatures(dirs);
    if cache.dir_signatures != current {
        tracing::debug!("Font cache: directory mtime changed, rescanning");
        return None;
    }
    Some(cache.fonts)
}

fn save_disk_cache(
    dirs: &[String],
    fonts: &BTreeMap<String, Vec<FontInfo>>,
) -> std::io::Result<()> {
    let Some(path) = cache_path() else {
        return Ok(()); // no cache dir → silently skip
    };
    let cache = DiskCache {
        version: CACHE_SCHEMA_VERSION,
        dir_signatures: collect_signatures(dirs),
        fonts: fonts.clone(),
    };
    let json = serde_json::to_string(&cache).map_err(std::io::Error::other)?;
    std::fs::write(&path, json)
}

// ---------------------------------------------------------------------------
// Filename parsing helpers (unchanged from the previous implementation)
// ---------------------------------------------------------------------------

fn parse_font_name(filename: &str) -> (String, String) {
    // Try splitting on '-' first (e.g., "NotoSans-Regular", "Roboto-BoldItalic")
    if let Some(idx) = filename.rfind('-') {
        let family_raw = filename[..idx].replace('_', " ");
        let style = &filename[idx + 1..];
        let family = insert_spaces_before_caps(&family_raw);
        return (family, normalize_style(style));
    }

    // Try common style suffixes (longest first to avoid partial matches)
    for suffix in [
        "BoldItalic",
        "Bold Italic",
        "SemiBoldItalic",
        "ExtraBoldItalic",
        "ExtraBold",
        "SemiBold",
        "Bold",
        "Italic",
        "Light",
        "Medium",
        "Thin",
        "Black",
        "Regular",
    ] {
        if let Some(prefix) = filename.strip_suffix(suffix) {
            let family = prefix
                .trim_end_matches([' ', '_', '-'])
                .to_string();
            let family = insert_spaces_before_caps(&family);
            if !family.is_empty() {
                return (family, normalize_style(suffix));
            }
        }
    }

    (insert_spaces_before_caps(filename), "Regular".to_string())
}

fn insert_spaces_before_caps(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_uppercase() && chars[i - 1].is_lowercase() {
            result.push(' ');
        }
        result.push(c);
    }
    result
}

fn normalize_style(s: &str) -> String {
    match s.to_lowercase().as_str() {
        "regular" | "reg" => "Regular".into(),
        "bold" | "bd" => "Bold".into(),
        "italic" | "it" => "Italic".into(),
        "bolditalic" | "bi" => "Bold Italic".into(),
        "light" | "lt" => "Light".into(),
        "medium" | "med" => "Medium".into(),
        "semibold" | "sb" => "Semi Bold".into(),
        "extrabold" => "Extra Bold".into(),
        "extrabolditalic" => "Extra Bold Italic".into(),
        "semibolditalic" => "Semi Bold Italic".into(),
        "thin" => "Thin".into(),
        "black" | "heavy" => "Black".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hyphenated_name() {
        let (family, style) = parse_font_name("NotoSans-Regular");
        assert_eq!(family, "Noto Sans");
        assert_eq!(style, "Regular");
    }

    #[test]
    fn parse_hyphenated_bold() {
        let (family, style) = parse_font_name("Roboto-BoldItalic");
        assert_eq!(family, "Roboto");
        assert_eq!(style, "Bold Italic");
    }

    #[test]
    fn parse_plain_name() {
        let (family, style) = parse_font_name("DejaVuSans");
        assert_eq!(family, "Deja Vu Sans");
        assert_eq!(style, "Regular");
    }

    #[test]
    fn discover_returns_something() {
        // This test just verifies discovery doesn't panic. On CI without fonts it may be empty.
        let fonts = discover_system_fonts();
        let _ = fonts;
    }

    #[test]
    fn discover_is_deduplicated() {
        // Two consecutive calls must return Arcs that point at the same allocation.
        let a = discover_system_fonts();
        let b = discover_system_fonts();
        assert!(Arc::ptr_eq(&a, &b), "second call must reuse the OnceLock");
    }

    /// A face's feature list is read from its GSUB and GPOS (#1710):
    /// Inter's stylistic sets are ss01 to ss08, JetBrains Mono's four
    /// of them, Bebas Neue's none; the built-in face has small caps and
    /// Inter has not.
    #[test]
    fn a_faces_features_are_its_gsub_and_gpos_tags() {
        let ss = |tags: Vec<String>| tags.into_iter().filter(|t| t.starts_with("ss")).collect::<Vec<_>>();
        let inter = face_feature_tags(crate::bundled::lookup("Inter", "Regular").unwrap());
        assert_eq!(ss(inter.clone()), (1..=8).map(|n| format!("ss{n:02}")).collect::<Vec<_>>());
        assert!(inter.iter().any(|t| t == "kern"), "GPOS's are there too");
        assert!(!inter.iter().any(|t| t == "smcp"));
        assert_eq!(ss(face_feature_tags(crate::bundled::lookup("JetBrains Mono", "Regular").unwrap())), ["ss01", "ss02", "ss19", "ss20"]);
        assert!(ss(face_feature_tags(crate::bundled::lookup("Bebas Neue", "Regular").unwrap())).is_empty());
        let registry: FontRegistry = Arc::new(BTreeMap::new());
        assert!(family_feature_tags(&registry, "sans-serif", "Regular").iter().any(|t| t == "smcp"));
        assert_eq!(family_feature_tags(&registry, "Inter", "Bold Italic").iter().filter(|t| t.starts_with("ss")).count(), 8);
        assert!(family_feature_tags(&registry, "No Such Family", "Regular").is_empty());
        assert!(face_feature_tags(b"not a font").is_empty());
    }
}

#[cfg(test)]
mod installed_tests {
    use super::*;

    /// The default face, a bundled family and an installed one are
    /// here; a name that is none of these is not (#1720).
    #[test]
    fn a_font_is_installed_when_it_draws_in_itself() {
        let mut map = BTreeMap::new();
        map.insert("Installed Sans".to_string(), Vec::new());
        let registry: FontRegistry = Arc::new(map);
        for f in ["", "sans-serif", "Inter", "Bebas Neue", "Installed Sans"] {
            assert!(font_installed(&registry, f), "{f:?}");
        }
        assert!(!font_installed(&registry, "Gone Grotesk"));
    }
}
