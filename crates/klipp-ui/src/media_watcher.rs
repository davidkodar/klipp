use klipp_core::AppEvent;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

/// Start watching parent directories of all media files.
/// Sends `AppEvent::MediaFileChanged` when a known media file is modified or deleted.
/// Returns the watcher handle (must be kept alive — drop stops watching).
pub fn start_media_watcher(
    media_paths: &[PathBuf],
    event_tx: crossbeam_channel::Sender<AppEvent>,
) -> Option<notify::RecommendedWatcher> {
    use notify::{EventKind, RecursiveMode, Watcher};

    if media_paths.is_empty() {
        return None;
    }

    // Collect unique parent directories to watch (conserves inotify watches)
    let watch_dirs: HashSet<PathBuf> = media_paths
        .iter()
        .filter_map(|p| p.parent().map(|d| d.to_path_buf()))
        .collect();

    // Build set of known filenames for filtering events
    let known_files: Arc<HashSet<PathBuf>> = Arc::new(
        media_paths.iter().cloned().collect(),
    );

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            let relevant = matches!(
                event.kind,
                EventKind::Modify(_) | EventKind::Remove(_)
            );
            if relevant {
                for path in &event.paths {
                    if known_files.contains(path) {
                        let deleted = matches!(event.kind, EventKind::Remove(_));
                        let _ = event_tx.send(AppEvent::MediaFileChanged {
                            path: path.clone(),
                            deleted,
                        });
                    }
                }
            }
        }
    })
    .ok()?;

    for dir in &watch_dirs {
        if dir.exists() {
            let _ = watcher.watch(dir, RecursiveMode::NonRecursive);
        }
    }

    tracing::info!(
        "Media watcher started: watching {} directories for {} files",
        watch_dirs.len(),
        media_paths.len()
    );

    Some(watcher)
}
