use std::path::{Component, Path, PathBuf};

/// Convert an absolute runtime path to a relative path for on-disk storage.
///
/// Relative to the project directory (the `.klipp/` folder).
/// Falls back to absolute if the path is on a different drive/volume
/// (e.g. Windows UNC paths, different mount points).
pub fn to_project_relative(project_dir: &Path, absolute: &Path) -> PathBuf {
    pathdiff::diff_paths(absolute, project_dir).unwrap_or_else(|| absolute.to_path_buf())
}

/// Resolve a stored path (possibly relative) to an absolute runtime path.
///
/// Resolution order:
///   1. If the stored path is already absolute — return as-is
///   2. If relative — join with project_dir (even if the file doesn't exist on disk)
///
/// The caller should run `path.exists()` separately to set the offline flag.
/// Never returns a bare relative path — always joins with project_dir.
pub fn resolve_media_path(project_dir: &Path, stored: &Path) -> PathBuf {
    if stored.is_absolute() {
        stored.to_path_buf()
    } else {
        project_dir.join(stored)
    }
}

/// Resolve a stored path while guarding against path-traversal attacks.
///
/// Returns `None` (and logs `tracing::warn`) for relative paths whose `..`
/// components escape the project directory. Absolute paths are allowed
/// because users legitimately reference media outside the `.klipp` folder
/// (e.g. `~/Videos/clip.mp4`). The caller should treat `None` as "media
/// offline" and never feed the untrusted path to ffmpeg or any other I/O.
///
/// Does NOT touch the filesystem — works on path syntax alone so it's safe
/// to call even when the media or the project drive is disconnected.
pub fn safe_resolve(project_dir: &Path, stored: &Path) -> Option<PathBuf> {
    if stored.is_absolute() {
        Some(normalize_path(stored))
    } else {
        let joined = project_dir.join(stored);
        let normalized = normalize_path(&joined);
        let project_normalized = normalize_path(project_dir);
        if normalized.starts_with(&project_normalized) {
            Some(normalized)
        } else {
            tracing::warn!(
                "safe_resolve: rejecting relative path that escapes project dir: {:?}",
                stored,
            );
            None
        }
    }
}

/// Collapse `.` and `..` components without touching the filesystem.
///
/// Unlike `std::fs::canonicalize`, this never follows symlinks and works
/// for paths that don't exist on disk. A leading `..` that can't be
/// collapsed (no parent to pop) is preserved — the caller's prefix check
/// catches any remaining escape.
fn normalize_path(path: &Path) -> PathBuf {
    let mut parts: Vec<Component<'_>> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                } else {
                    parts.push(comp);
                }
            }
            _ => parts.push(comp),
        }
    }
    parts.iter().collect()
}

/// Maximum input-file size for project / import parsers (64 MiB).
///
/// A 10 GB `.kdenlive` or `.fcpxml` dropped on the importer would OOM the
/// process without a cap. 64 MiB is generous for any realistic project
/// file while keeping the memory ceiling bounded.
pub const MAX_INPUT_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Read a text file into a String, rejecting files larger than `max_bytes`.
///
/// Drop-in replacement for `std::fs::read_to_string` with an OOM guard.
/// The size check uses metadata (no read before the decision) so a 10 GB
/// file is rejected instantly without reading a single byte.
pub fn read_to_string_capped(path: &Path, max_bytes: u64) -> Result<String, std::io::Error> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "file too large ({} bytes, max {} bytes): {}",
                size, max_bytes, path.display()
            ),
        ));
    }
    let mut s = String::with_capacity(size as usize);
    file.take(max_bytes).read_to_string(&mut s)?;
    Ok(s)
}

/// Read a binary file into a Vec<u8>, rejecting files larger than `max_bytes`.
///
/// Same as `read_to_string_capped` but for non-UTF-8 formats (e.g. OTIO).
pub fn read_capped(path: &Path, max_bytes: u64) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "file too large ({} bytes, max {} bytes): {}",
                size, max_bytes, path.display()
            ),
        ));
    }
    let mut buf = Vec::with_capacity(size as usize);
    file.take(max_bytes).read_to_end(&mut buf)?;
    Ok(buf)
}
