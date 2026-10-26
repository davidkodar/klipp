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

/// A file's analysis carried over to it at `new_path` (#2790): a relink to
/// the same content keeps what was analysed under the key it had before
/// (`old_key`), copied to the key it has now, so nothing is analysed
/// again. Copied, not moved: another project may still use the old path.
/// False when there was nothing to carry, or it could not be copied.
pub fn carry_over(old_key: &str, new_path: &Path) -> bool {
    carry_over_in(&analysis_dir(), old_key, new_path)
}

/// [`carry_over`] in the analysis store at `root`.
pub fn carry_over_in(root: &Path, old_key: &str, new_path: &Path) -> bool {
    let Some(new_key) = key_for(new_path) else { return false };
    let (from, to) = (root.join(old_key), root.join(&new_key));
    if new_key == old_key || !from.is_dir() {
        return false;
    }
    fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let target = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy(&entry.path(), &target)?;
            } else if !target.exists() {
                std::fs::copy(entry.path(), target)?;
            }
        }
        Ok(())
    }
    copy(&from, &to).is_ok()
}

#[cfg(test)]
mod carry_over_tests {
    use super::*;

    /// #2790: a file moved and relinked is the same file to its analysis.
    /// Measured: the analysis under the old key is found under the new
    /// file's key, so nothing is queued to analyse it again; a different
    /// file, or the same key, carries nothing.
    #[test]
    fn a_relinked_file_keeps_its_analysis() {
        let dir = std::env::temp_dir().join(format!("klipp-carry-over-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (root, media) = (dir.join("analysis"), dir.join("media"));
        std::fs::create_dir_all(&media).unwrap();
        let (old, new) = (media.join("old.wav"), media.join("moved/new.wav"));
        std::fs::write(&old, b"the same sound").unwrap();
        let old_key = key_for(&old).unwrap();
        std::fs::create_dir_all(root.join(&old_key)).unwrap();
        std::fs::write(root.join(&old_key).join("transcript.v3.json"), b"{\"words\":[]}").unwrap();
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::rename(&old, &new).unwrap();

        assert!(carry_over_in(&root, &old_key, &new), "carried over");
        let new_key = key_for(&new).unwrap();
        assert_ne!(new_key, old_key, "a new path is a new key");
        assert_eq!(std::fs::read(root.join(&new_key).join("transcript.v3.json")).unwrap(), b"{\"words\":[]}");
        assert!(root.join(&old_key).is_dir(), "copied: another project may still use the old path");
        assert!(!carry_over_in(&root, &new_key, &new), "the same key: nothing to carry");
        assert!(!carry_over_in(&root, "no-such-key", &new), "nothing analysed: nothing to carry");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
