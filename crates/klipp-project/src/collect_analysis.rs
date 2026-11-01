//! A collected project's analysis results (#3241): shots, transcripts,
//! framing, mattes and the like, kept in the cache by each file's cache
//! key ([`klipp_media::disk_cache::analysis_dir`]). The key is the file's
//! path and time, so a copy elsewhere would lose them: collecting copies a
//! file's results into the folder, under `analysis/<file name>/`, and
//! opening the folder brings them back under the key the copy has there.
//!
//! What a folder brings back is read as untrusted: only plain files and
//! folders of plain names, no links, each file capped, inside the folder.

use std::path::{Component, Path};

/// The folder in a collected project that holds its files' results.
pub const FOLDER: &str = "analysis";

/// The largest result file brought back.
const MAX_FILE_BYTES: u64 = 256 << 20;

/// `from`'s plain files and folders copied into `to`, links passed over,
/// files `to` has already kept; at most `cap` bytes each. Whether any
/// was copied.
fn copy_tree(from: &Path, to: &Path, cap: u64) -> std::io::Result<bool> {
    let mut any = false;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if !matches!(Path::new(&name).components().next(), Some(Component::Normal(_))) || Path::new(&name).components().count() != 1 {
            continue;
        }
        let kind = entry.file_type()?;
        let target = to.join(&name);
        if kind.is_dir() {
            any |= copy_tree(&entry.path(), &target, cap)?;
        } else if kind.is_file() && !target.exists() && entry.metadata()?.len() <= cap {
            std::fs::create_dir_all(to)?;
            std::fs::copy(entry.path(), &target)?;
            any = true;
        }
    }
    Ok(any)
}

/// `source`'s results, from `cache` (the analysis folder), copied into
/// the collected project at `dest` for its copy named `name` there.
/// Whether there were any.
pub fn carry_out(cache: &Path, source: &Path, dest: &Path, name: &str) -> bool {
    let Some(key) = klipp_media::disk_cache::compute_cache_key(source) else { return false };
    let from = cache.join(key);
    from.is_dir() && copy_tree(&from, &dest.join(FOLDER).join(name), u64::MAX).unwrap_or(false)
}

/// The results a collected project at `dir` carries, brought into
/// `cache` (the analysis folder) for each of `files` inside it, under the
/// key each has here; what the cache already holds is kept. How many
/// files' results were brought back.
pub fn bring_in(dir: &Path, cache: &Path, files: &[&Path]) -> usize {
    let carried = dir.join(FOLDER);
    let Ok(meta) = std::fs::symlink_metadata(&carried) else { return 0 };
    if !meta.is_dir() {
        return 0;
    }
    let Ok(root) = dir.canonicalize() else { return 0 };
    let mut brought = 0;
    for file in files {
        let Some(name) = file.file_name() else { continue };
        let Ok(real) = file.canonicalize() else { continue };
        if !real.starts_with(&root) {
            continue;
        }
        let from = carried.join(name);
        if !std::fs::symlink_metadata(&from).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let Some(key) = klipp_media::disk_cache::compute_cache_key(file) else { continue };
        if copy_tree(&from, &cache.join(key), MAX_FILE_BYTES).unwrap_or(false) {
            brought += 1;
        }
    }
    brought
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #3241: a file's results go out with its copy and come back on the
    /// other side under the key the copy has there; what the cache holds
    /// is kept; a link in the carried folder, a name with `..` and a copy
    /// outside the project are passed over.
    #[test]
    fn results_go_out_with_the_copy_and_come_back_under_its_key() {
        let base = std::env::temp_dir().join(format!("klipp-collect-analysis-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (cache, here, there, project) = (base.join("cache"), base.join("cache2"), base.join("elsewhere"), base.join("proj"));
        for d in [&cache, &here, &there, &project.join("media")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let source = there.join("take.mov");
        std::fs::write(&source, b"picture").unwrap();
        let key = klipp_media::disk_cache::compute_cache_key(&source).unwrap();
        std::fs::create_dir_all(cache.join(&key).join("mattes")).unwrap();
        std::fs::write(cache.join(&key).join("shots.v1.json"), b"{\"cuts\":[1.0]}").unwrap();
        std::fs::write(cache.join(&key).join("mattes").join("0001.png"), b"m").unwrap();
        assert!(carry_out(&cache, &source, &project, "take.mov"));
        assert!(project.join("analysis/take.mov/shots.v1.json").is_file() && project.join("analysis/take.mov/mattes/0001.png").is_file());
        // The copy, opened: its results under its own key, in a fresh cache.
        let copy = project.join("media/take.mov");
        std::fs::copy(&source, &copy).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, project.join("analysis/take.mov/stolen.json")).unwrap();
        assert_eq!(bring_in(&project, &here, &[&copy, &source]), 1, "the one inside the project");
        let new_key = klipp_media::disk_cache::compute_cache_key(&copy).unwrap();
        assert_ne!(new_key, key);
        assert_eq!(std::fs::read(here.join(&new_key).join("shots.v1.json")).unwrap(), b"{\"cuts\":[1.0]}");
        assert!(here.join(&new_key).join("mattes/0001.png").is_file());
        assert!(!here.join(&new_key).join("stolen.json").exists(), "a link is passed over");
        std::fs::write(here.join(&new_key).join("shots.v1.json"), b"newer").unwrap();
        bring_in(&project, &here, &[&copy]);
        assert_eq!(std::fs::read(here.join(&new_key).join("shots.v1.json")).unwrap(), b"newer", "what the cache holds is kept");
        let _ = std::fs::remove_dir_all(&base);
    }
}
