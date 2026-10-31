//! Cache manager helpers — scans the on-disk cache categories that Klipp
//! populates during editing so the user can inspect / clear them.
//!
//! Categories (feature #78):
//!   - Proxy clips     — `<proxy_cache_dir>` or `~/.cache/klipp/proxies`
//!   - Filmstrips      — `~/.cache/klipp/filmstrips`
//!   - Audio peaks     — `~/.cache/klipp/audio-peaks` + `~/.cache/klipp/audio-cache`
//!   - Render cache    — `~/.cache/klipp/render`
//!
//! `scan_dir_size` is intentionally **non-recursive** for MVP — these four
//! directories are flat (each category writes files directly, no nested
//! subdirs). Deferred: recursive walk if any category ever starts nesting.

use std::path::{Path, PathBuf};

/// A single tracked cache directory.
#[derive(Debug, Clone)]
pub struct CacheCategory {
    pub name: &'static str,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub file_count: u64,
    pub color: egui::Color32,
}

/// Scan a single directory (non-recursive) and return `(total_bytes, file_count)`.
/// Returns `(0, 0)` for missing directories.
pub fn scan_dir_size(path: &Path) -> (u64, u64) {
    if !path.exists() {
        return (0, 0);
    }
    let mut total = 0u64;
    let mut count = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total += meta.len();
                    count += 1;
                }
            }
        }
    }
    (total, count)
}

/// Every file under `path`, at any depth: `(total_bytes, file_count)`.
pub fn scan_tree_size(path: &Path) -> (u64, u64) {
    let mut total = 0u64;
    let mut count = 0u64;
    for e in walkdir::WalkDir::new(path).into_iter().flatten().filter(|e| e.file_type().is_file()) {
        total += e.metadata().map(|m| m.len()).unwrap_or(0);
        count += 1;
    }
    (total, count)
}

/// The open project's frozen tracks (#3207), under `project_cache`
/// (`<project>/cache`): their frames, and their sound's stems.
pub fn frozen_categories(project_cache: &Path) -> Vec<CacheCategory> {
    let frames = project_cache.join("freeze");
    let sound = project_cache.join("stems");
    let rendered = project_cache.join("frames");
    let (frame_bytes, frame_count) = scan_tree_size(&frames);
    let (sound_bytes, sound_count) = scan_tree_size(&sound);
    let (rendered_bytes, rendered_count) = scan_tree_size(&rendered);
    vec![
        CacheCategory {
            name: "Rendered frames (this project)",
            path: rendered,
            size_bytes: rendered_bytes,
            file_count: rendered_count,
            // Accent green
            color: egui::Color32::from_rgb(0x5f, 0xb8, 0x5f),
        },
        CacheCategory {
            name: "Frozen frames (this project)",
            path: frames,
            size_bytes: frame_bytes,
            file_count: frame_count,
            // Ice blue
            color: egui::Color32::from_rgb(0x7f, 0xc8, 0xe8),
        },
        CacheCategory {
            name: "Frozen sound (this project)",
            path: sound,
            size_bytes: sound_bytes,
            file_count: sound_count,
            // Teal
            color: egui::Color32::from_rgb(0x3f, 0xa8, 0x9c),
        },
    ]
}

/// Sum sizes for a set of directories into a single `(total_bytes, file_count)`.
/// Used by categories whose storage spans more than one on-disk folder
/// (e.g. audio has both `audio-peaks` and `audio-cache`).
fn scan_dirs_combined(paths: &[PathBuf]) -> (u64, u64) {
    let mut total = 0u64;
    let mut count = 0u64;
    for p in paths {
        let (b, c) = scan_dir_size(p);
        total += b;
        count += c;
    }
    (total, count)
}

/// Returns the four tracked cache categories, sized against the current
/// on-disk state. `cache_root` is the base `~/.cache/klipp/` directory; the
/// optional `proxy_override` lets the user redirect proxy storage via prefs.
pub fn scan_cache_categories(
    cache_root: &Path,
    proxy_override: Option<&Path>,
) -> Vec<CacheCategory> {
    let proxy_path = proxy_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| cache_root.join("proxies"));
    let filmstrip_path = cache_root.join("filmstrips");
    let audio_peaks_path = cache_root.join("audio-peaks");
    let audio_cache_path = cache_root.join("audio-cache");
    let render_path = cache_root.join("render");

    let (proxy_bytes, proxy_count) = scan_dir_size(&proxy_path);
    let (strip_bytes, strip_count) = scan_dir_size(&filmstrip_path);
    let (audio_bytes, audio_count) =
        scan_dirs_combined(&[audio_peaks_path.clone(), audio_cache_path.clone()]);
    let (render_bytes, render_count) = scan_dir_size(&render_path);

    vec![
        CacheCategory {
            name: "Proxy clips",
            path: proxy_path,
            size_bytes: proxy_bytes,
            file_count: proxy_count,
            // Blue
            color: egui::Color32::from_rgb(0x4a, 0x90, 0xe2),
        },
        CacheCategory {
            name: "Filmstrips",
            path: filmstrip_path,
            size_bytes: strip_bytes,
            file_count: strip_count,
            // Amber
            color: egui::Color32::from_rgb(0xe0, 0xa8, 0x30),
        },
        CacheCategory {
            name: "Audio peaks",
            // Primary path — "Clear" will also wipe audio-cache alongside.
            path: audio_peaks_path,
            size_bytes: audio_bytes,
            file_count: audio_count,
            // Green
            color: egui::Color32::from_rgb(0x50, 0xb0, 0x60),
        },
        CacheCategory {
            name: "Render cache",
            path: render_path,
            size_bytes: render_bytes,
            file_count: render_count,
            // Red
            color: egui::Color32::from_rgb(0xd0, 0x50, 0x50),
        },
    ]
}

/// What a cache file's name (its first dot-separated component) is
/// matched against when looking for orphans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanKey {
    /// Named by media id: proxies.
    MediaId,
    /// Named by the file's content key: everything else.
    CacheKey,
}

/// Every directory the orphan cleaner scans, resolved as the cache code
/// resolves it (the same preference overrides), with how each file's
/// name is matched. Skim keys carry `.hdrtm` / `.r90` suffixes and
/// filmstrips `.{height}.c2` — the first component is still the key.
pub fn orphan_scan_dirs(
    cache_root: &Path,
    proxy_override: Option<&Path>,
    filmstrip_override: Option<&Path>,
    audio_override: Option<&Path>,
) -> Vec<(PathBuf, OrphanKey)> {
    vec![
        (proxy_override.map(Path::to_path_buf).unwrap_or_else(|| cache_root.join("proxies")), OrphanKey::MediaId),
        (cache_root.join("skim"), OrphanKey::CacheKey),
        (filmstrip_override.map(Path::to_path_buf).unwrap_or_else(|| cache_root.join("filmstrips")), OrphanKey::CacheKey),
        (cache_root.join("thumbnails"), OrphanKey::CacheKey),
        (audio_override.map(Path::to_path_buf).unwrap_or_else(|| cache_root.join("audio-cache")), OrphanKey::CacheKey),
        (cache_root.join("audio-peaks"), OrphanKey::CacheKey),
    ]
}

/// Sibling path for the "Audio peaks" category — the heavy PCM conform cache
/// lives in a second directory and should be cleared alongside peaks so the
/// UI category stays consistent with what the user sees on disk.
pub fn audio_cache_sibling(cache_root: &Path) -> PathBuf {
    cache_root.join("audio-cache")
}

/// Format a byte count into a human string (B / KB / MB / GB).
pub fn format_cache_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    /// #3207: the frozen categories count every file under the project's
    /// cache, in the folders a track each keeps.
    #[test]
    fn frozen_categories_count_every_file_below() {
        let root = std::env::temp_dir().join(format!("klipp-frozen-cats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (dir, bytes) in [("freeze/a", 10), ("freeze/b", 20), ("stems/c", 7)] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("x.bin"), vec![0u8; bytes]).unwrap();
        }
        let cats = super::frozen_categories(&root);
        assert_eq!(cats[0].path, root.join("frames"), "rendered frames first (#3212)");
        assert_eq!((cats[1].size_bytes, cats[1].file_count), (30, 2), "frames: {:?}", cats[1].path);
        assert_eq!((cats[2].size_bytes, cats[2].file_count), (7, 1));
        assert_eq!(cats[1].path, root.join("freeze"));
        assert_eq!(cats[2].path, root.join("stems"));
        let _ = std::fs::remove_dir_all(&root);
    }

    use super::*;

    #[test]
    fn format_cache_size_bytes() {
        assert_eq!(format_cache_size(512), "512 B");
        assert_eq!(format_cache_size(1024), "1.0 KB");
        assert_eq!(format_cache_size(1536), "1.5 KB");
    }

    #[test]
    fn format_cache_size_megabytes() {
        assert_eq!(format_cache_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_cache_size((1.5 * 1024.0 * 1024.0) as u64), "1.5 MB");
    }

    #[test]
    fn format_cache_size_gigabytes() {
        assert_eq!(format_cache_size(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn scan_nonexistent_dir_returns_zero() {
        let result = scan_dir_size(std::path::Path::new("/nonexistent/path/xyz"));
        assert_eq!(result, (0, 0));
    }

    #[test]
    fn scan_categories_handles_missing_root() {
        let cats =
            scan_cache_categories(std::path::Path::new("/nonexistent/path/xyz"), None);
        assert_eq!(cats.len(), 4);
        for c in &cats {
            assert_eq!(c.size_bytes, 0);
            assert_eq!(c.file_count, 0);
        }
        assert_eq!(cats[0].name, "Proxy clips");
        assert_eq!(cats[1].name, "Filmstrips");
        assert_eq!(cats[2].name, "Audio peaks");
        assert_eq!(cats[3].name, "Render cache");
    }

    #[test]
    fn scan_categories_respects_proxy_override() {
        let override_path = std::path::Path::new("/tmp/klipp-override-xyz");
        let cats = scan_cache_categories(
            std::path::Path::new("/nonexistent/root"),
            Some(override_path),
        );
        assert_eq!(cats[0].path, override_path);
    }
}

#[cfg(test)]
mod orphan_dir_tests {
    use super::*;

    /// The cleaner scans where the caches are written. It scanned
    /// `thumbnails` for filmstrips and `waveforms` for audio, and so
    /// never cleaned either real directory.
    #[test]
    fn the_cleaner_scans_the_directories_the_caches_are_written_to() {
        let root = Path::new("/c");
        let dirs: Vec<String> = orphan_scan_dirs(root, None, None, None)
            .into_iter()
            .map(|(d, _)| d.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        for expected in ["proxies", "skim", "filmstrips", "thumbnails", "audio-cache", "audio-peaks"] {
            assert!(dirs.contains(&expected.to_string()), "{expected} missing from {dirs:?}");
        }
        assert!(!dirs.contains(&"waveforms".to_string()));
        let with = orphan_scan_dirs(root, None, Some(Path::new("/f")), Some(Path::new("/a")));
        assert!(with.iter().any(|(d, k)| d == Path::new("/f") && *k == OrphanKey::CacheKey));
        assert!(with.iter().any(|(d, k)| d == Path::new("/a") && *k == OrphanKey::CacheKey));
        let proxies = with.iter().find(|(_, k)| *k == OrphanKey::MediaId).unwrap();
        assert_eq!(proxies.0, Path::new("/c/proxies"), "only proxies are named by media id");
    }
}
