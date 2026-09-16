//! Where results live: `<cache>/klipp/analysis/<cache key>/<id>.v<n>.json`.
//! The cache key is the media file's, so a file that changed gets fresh
//! results and a moved project does not lose them.

use std::path::{Path, PathBuf};

pub fn analysis_dir() -> PathBuf {
    klipp_media::disk_cache::cache_base_dir().join("analysis")
}

/// The media file's cache key, or `None` when the file is not there.
pub fn key_for(path: &Path) -> Option<String> {
    klipp_media::disk_cache::compute_cache_key(path)
}

fn file_for(key: &str, id: &str, version: u32) -> PathBuf {
    analysis_dir().join(key).join(format!("{id}.v{version}.json"))
}

/// A stored result at exactly this schema version.
pub fn load(key: &str, id: &str, version: u32) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(file_for(key, id, version)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn exists(key: &str, id: &str, version: u32) -> bool {
    file_for(key, id, version).is_file()
}

pub fn save(key: &str, id: &str, version: u32, value: &serde_json::Value) -> Result<(), String> {
    let path = file_for(key, id, version);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    // Whole or not at all: a reader never sees half a file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}
