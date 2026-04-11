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
    let mut state = mu.lock().unwrap();
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
    let mut state = mu.lock().unwrap();
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

    // Publish + notify waiters under the lock so no one races between
    // `REGISTRY.set` and `cvar.notify_all`.
    let (mu, cvar) = scan_state();
    let mut state = mu.lock().unwrap();
    let _ = REGISTRY.set(registry);
    state.in_flight = false;
    cvar.notify_all();
    tracing::info!(
        "STARTUP_PERF font_discovery: scan + publish               {:>9.1} ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
}

fn font_directories() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    vec![
        "/usr/share/fonts".to_string(),
        "/usr/local/share/fonts".to_string(),
        format!("{home}/.local/share/fonts"),
    ]
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

    Some(FontInfo {
        family,
        style,
        path: path.to_path_buf(),
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

const CACHE_SCHEMA_VERSION: u32 = 1;

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
}
