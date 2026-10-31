//! Caches: their categories measured and orphans cleaned, and the AI mask
//! cache kept under its cap (#890).

use super::*;

impl KlippApp {
    /// Scan all cache directories and remove files not referenced by current media pool.
    /// Cache Manager (feature #78): refresh the sized snapshot of on-disk
    /// cache categories. Called when the dialog opens and after every
    /// `ClearCacheCategory` so the pie chart / size list always reflect
    /// current disk state.
    pub(super) fn refresh_cache_categories(&mut self) {
        let cache_root = dirs::cache_dir().unwrap_or_default().join("klipp");
        let proxy_override = self.prefs.proxy.proxy_cache_dir.as_deref();
        self.state.cache_categories =
            crate::cache_manager::scan_cache_categories(&cache_root, proxy_override);
        if let Some(project_cache) = self.freeze_cache_folder() {
            self.state.cache_categories.extend(crate::cache_manager::frozen_categories(&project_cache));
        }
    }

    pub(super) fn clean_orphaned_caches(&self) -> String {
        let klipp_cache = dirs::cache_dir().unwrap_or_default().join("klipp");

        // Collect all valid cache keys from the current media pool
        let mut valid_media_ids = std::collections::HashSet::new();
        let mut valid_cache_keys = std::collections::HashSet::new();

        for item in self.state.media_pool.all() {
            // Media ID (used by proxy files)
            valid_media_ids.insert(format!("{}", item.id.0));

            // Content cache key (used by filmstrips, skim, thumbnails, audio)
            if let Some(key) = klipp_media::disk_cache::compute_cache_key(&item.path) {
                valid_cache_keys.insert(key);
            }
        }

        let mut total_removed = 0u64;
        let mut files_removed = 0usize;

        // Scan each cache directory
        // Every directory a cache lives in, resolved the way the cache
        // code resolves it. This used to scan `thumbnails` for
        // filmstrips and `waveforms` for audio — neither is where those
        // caches are written — so the real directories were never
        // cleaned; and files named by media id no longer count as
        // valid, since no cache is read by that name any more.
        let dirs_and_matchers: Vec<(std::path::PathBuf, Box<dyn Fn(&str) -> bool>)> =
            crate::cache_manager::orphan_scan_dirs(
                &klipp_cache,
                self.prefs.proxy.proxy_cache_dir.as_deref(),
                self.prefs.media_cache.filmstrip_cache_dir.as_deref(),
                self.prefs.media_cache.audio_cache_dir.as_deref(),
            )
            .into_iter()
            .map(|(dir, by)| {
                let ids = valid_media_ids.clone();
                let keys = valid_cache_keys.clone();
                let f: Box<dyn Fn(&str) -> bool> = match by {
                    crate::cache_manager::OrphanKey::MediaId => Box::new(move |stem: &str| ids.contains(stem)),
                    crate::cache_manager::OrphanKey::CacheKey => Box::new(move |stem: &str| keys.contains(stem)),
                };
                (dir, f)
            })
            .collect();
        for (dir, is_valid) in &dirs_and_matchers {
            if !dir.exists() { continue; }
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.is_file() { continue; }
                    // The content key is everything before the first dot:
                    // picture caches append `.{height}`, a log format and a
                    // pipeline version. Comparing the whole stem treated
                    // every filmstrip as an orphan and deleted it.
                    let stem = path.file_name()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.split('.').next())
                        .unwrap_or("");
                    if !is_valid(stem) {
                        if let Ok(meta) = path.metadata() {
                            total_removed += meta.len();
                            files_removed += 1;
                        }
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }

        if files_removed == 0 {
            "No orphaned cache files found".to_string()
        } else {
            let size_str = if total_removed >= 1_073_741_824 {
                format!("{:.1} GB", total_removed as f64 / 1_073_741_824.0)
            } else if total_removed >= 1_048_576 {
                format!("{:.1} MB", total_removed as f64 / 1_048_576.0)
            } else {
                format!("{} KB", total_removed / 1024)
            };
            format!("Cleaned {files_removed} orphaned files ({size_str})")
        }
    }
}

/// 2 GB cap on the AI mask cache directory. Bigger than any
/// realistic short-term workflow (a 1080p mask PNG is ~80–200 KB
/// — that's 10k+ masks at the high end) but enforces a ceiling
/// so months of heavy use can't quietly fill the disk.
pub(super) const AI_MASK_CACHE_BYTES_CAP: u64 = 2 * 1024 * 1024 * 1024;

/// LRU prune of `~/.cache/klipp/ai_masks/`. Walks the directory,
/// stats each file's size + mtime, and if the total is over the
/// cap deletes oldest-first until under cap. Errors at every
/// step are logged + swallowed — failure to prune is never a
/// reason to refuse to start the app.
///
/// This is the disk-pressure safety net. Proper orphan-detection
/// (parse filenames, cross-reference with current project's clip
/// IDs) is filed as a follow-up — the schema documented in
/// execute_apply_mask_to_clip puts enough metadata in the
/// filename to support it.
pub(super) fn prune_ai_mask_cache() {
    let Some(cache_root) = dirs::cache_dir() else {
        return;
    };
    let dir = cache_root.join("klipp").join("ai_masks");
    if !dir.exists() {
        return;
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(it) => it,
        Err(e) => {
            tracing::debug!(
                "ai mask cache prune: read_dir failed: {} ({e})",
                dir.display(),
            );
            return;
        }
    };
    let mut files: Vec<(std::path::PathBuf, u64, std::time::SystemTime)> =
        Vec::new();
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let size = meta.len();
        let mtime = meta
            .modified()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        total += size;
        files.push((path, size, mtime));
    }
    if total <= AI_MASK_CACHE_BYTES_CAP {
        return;
    }
    // Oldest-first delete order so the freshest masks (most
    // likely to be referenced by the current project) survive.
    files.sort_by_key(|(_, _, mtime)| *mtime);
    let mut deleted = 0u32;
    let mut freed = 0u64;
    for (path, size, _) in files {
        if total.saturating_sub(freed) <= AI_MASK_CACHE_BYTES_CAP {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            freed += size;
            deleted += 1;
        }
    }
    if deleted > 0 {
        tracing::info!(
            "ai mask cache prune: deleted {} file(s), freed {} bytes (cap {})",
            deleted,
            freed,
            AI_MASK_CACHE_BYTES_CAP,
        );
    }
}
