use std::path::{Path, PathBuf};

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
