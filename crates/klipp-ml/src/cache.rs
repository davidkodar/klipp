//! XDG-standard local cache for downloaded ML models.
//!
//! Layout under `~/.local/share/klipp/models/`:
//!
//! ```text
//! models/
//!   {model_id}.bin          fully-verified file
//!   {model_id}.bin.partial  in-progress download (atomic-renamed on success)
//! ```
//!
//! The `{model_id}` path comes from the manifest's stable `id` field
//! — never from `display_name` — so a UI label tweak doesn't make
//! Klipp think the cached file is missing.
//!
//! ## Air-gap workflow
//!
//! A user on an offline machine can `wget` (or sneakernet) the model
//! file on a different host, verify the SHA-256 against what Klipp
//! displays in the consent dialog, and drop the file at
//! `<models_root>/{id}.bin`. Next time the model is asked for,
//! [`ModelCache::is_cached`] returns [`CachedStatus::Verified`] and
//! Klipp skips the network entirely.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::manifest::ModelEntry;

/// Filename suffix for fully-downloaded, hash-verified models.
const FINAL_SUFFIX: &str = ".bin";

/// Filename suffix for in-progress downloads. Appended on `<id>.bin`
/// so a kill mid-download leaves debris that's obvious + cleanable.
pub const PARTIAL_SUFFIX: &str = ".bin.partial";

/// Result of asking the cache whether a model is ready to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedStatus {
    /// File is present and its SHA-256 matches the manifest. Ready
    /// to load; no network needed.
    Verified,
    /// File is not present. Caller should download.
    Missing,
    /// File is present but its SHA-256 does NOT match. Treated as
    /// corruption. Caller should redownload (which overwrites). We
    /// deliberately don't auto-delete — the user might want to keep
    /// the bytes for a forensic look first.
    Mismatch,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("could not resolve XDG data dir for model cache")]
    NoXdgDataDir,
    #[error("io error in model cache: {0}")]
    Io(#[from] std::io::Error),
}

/// Filesystem cache for downloaded models. Cheap to construct; opens
/// no files until [`ModelCache::is_cached`] hashes one or a download
/// writes into it.
#[derive(Debug, Clone)]
pub struct ModelCache {
    root: PathBuf,
}

impl ModelCache {
    /// Open the user's cache at the standard XDG location:
    /// `~/.local/share/klipp/models/`. Creates the directory if it
    /// doesn't exist (idempotent).
    pub fn open() -> Result<Self, CacheError> {
        let data = dirs::data_dir().ok_or(CacheError::NoXdgDataDir)?;
        let root = data.join("klipp").join("models");
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Open a cache rooted at an explicit path. Used by tests; the
    /// directory must already exist.
    pub fn open_at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Path where a verified-and-loadable file for this entry lives.
    pub fn path_for(&self, entry: &ModelEntry) -> PathBuf {
        self.path_for_id(&entry.id)
    }

    /// Same path resolution as [`Self::path_for`] but keyed only on
    /// the stable id string. Used by the custom-URL paste flow,
    /// where there's no `ModelEntry` (the file is described by a
    /// user-side `CustomModelEntry` instead) but the on-disk
    /// naming convention is the same.
    pub fn path_for_id(&self, id: &str) -> PathBuf {
        self.root.join(format!("{}{FINAL_SUFFIX}", id))
    }

    /// Path where an in-progress download writes. Atomically renamed
    /// to [`Self::path_for`] when verification passes.
    pub fn partial_path_for(&self, entry: &ModelEntry) -> PathBuf {
        self.root.join(format!("{}{PARTIAL_SUFFIX}", entry.id))
    }

    /// Root directory of the cache. Useful for the "open in file
    /// browser" affordance + for the air-gap install instructions.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Check whether the model is already cached AND its bytes match
    /// the manifest's SHA-256. Reads the file from disk; on a 900 MB
    /// model the check itself takes ~1-3 s — fine for a one-shot
    /// "is the model usable?" gate, NOT for per-frame use.
    ///
    /// Returns [`CachedStatus::Missing`] when the file isn't there.
    /// Returns [`CachedStatus::Mismatch`] when it is but its hash is
    /// wrong (corrupted / partial / tampered with). [`CachedStatus::Verified`] is
    /// the only "good to load" case.
    pub fn is_cached(&self, entry: &ModelEntry) -> Result<CachedStatus, CacheError> {
        let path = self.path_for(entry);
        if !path.exists() {
            return Ok(CachedStatus::Missing);
        }
        let actual = sha256_of_file(&path)?;
        if actual.eq_ignore_ascii_case(&entry.sha256) {
            Ok(CachedStatus::Verified)
        } else {
            Ok(CachedStatus::Mismatch)
        }
    }

    /// Remove the file a model `id` no longer in the manifest was kept
    /// in, once its replacement is in place: only when it is exactly the
    /// file that id named (`size_bytes` long, hashing to `sha256`), so a
    /// file put there by hand stays. The bytes freed, when it was
    /// removed.
    pub fn remove_replaced(&self, id: &str, size_bytes: u64, sha256: &str) -> Result<Option<u64>, CacheError> {
        let path = self.path_for_id(id);
        match std::fs::metadata(&path) {
            Ok(m) if m.is_file() && m.len() == size_bytes => {}
            _ => return Ok(None),
        }
        if !sha256_of_file(&path)?.eq_ignore_ascii_case(sha256) {
            return Ok(None);
        }
        std::fs::remove_file(&path)?;
        Ok(Some(size_bytes))
    }
}

/// Compute the SHA-256 of a file as a 64-char lowercase hex string.
/// Streams the file in 64 KiB chunks so a 900 MB model uses bounded
/// memory.
pub fn sha256_of_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Source;
    use std::io::Write;

    fn make_entry(id: &str, sha256: &str, size: u64) -> ModelEntry {
        ModelEntry {
            id: id.into(),
            display_name: format!("Test {id}"),
            size_bytes: size,
            sha256: sha256.into(),
            license: "Apache-2.0".into(),
            attribution: "test".into(),
            sources: vec![Source { url: "https://example.com/test.bin".into() }],
            kind: crate::manifest::ModelKind::default(),
            runtime: crate::manifest::ModelRuntime::default(),
            prompts: crate::manifest::PromptKinds::empty(),
            with: Vec::new(),
        }
    }

    #[test]
    fn missing_file_reports_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::open_at(dir.path());
        let entry = make_entry(
            "ghost",
            "0000000000000000000000000000000000000000000000000000000000000000",
            42,
        );
        let status = cache.is_cached(&entry).unwrap();
        assert_eq!(status, CachedStatus::Missing);
    }

    #[test]
    fn matching_file_reports_verified() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::open_at(dir.path());
        let bytes = b"klipp test bytes";
        // sha256 of "klipp test bytes" — computed via sha2::Sha256
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let expected = hex::encode(hasher.finalize());

        let entry = make_entry("good", &expected, bytes.len() as u64);
        let path = cache.path_for(&entry);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        f.sync_all().unwrap();

        let status = cache.is_cached(&entry).unwrap();
        assert_eq!(status, CachedStatus::Verified);
    }

    #[test]
    fn wrong_bytes_report_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::open_at(dir.path());
        let entry = make_entry(
            "tampered",
            // expect a hash that does NOT match the bytes we write
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            10,
        );
        let path = cache.path_for(&entry);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"different!").unwrap();
        f.sync_all().unwrap();

        let status = cache.is_cached(&entry).unwrap();
        assert_eq!(status, CachedStatus::Mismatch);
    }

    #[test]
    fn cache_path_uses_id_not_display_name() {
        // The id field is the stable identifier — renaming
        // display_name shouldn't move the on-disk file.
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::open_at(dir.path());
        let mut entry = make_entry(
            "stable_id",
            "0000000000000000000000000000000000000000000000000000000000000000",
            1,
        );
        let p1 = cache.path_for(&entry);
        entry.display_name = "Completely Different Label".into();
        let p2 = cache.path_for(&entry);
        assert_eq!(p1, p2);
    }

    #[test]
    fn partial_path_distinct_from_final() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::open_at(dir.path());
        let entry = make_entry(
            "x",
            "0000000000000000000000000000000000000000000000000000000000000000",
            1,
        );
        let final_path = cache.path_for(&entry);
        let partial = cache.partial_path_for(&entry);
        assert_ne!(final_path, partial);
        assert!(partial.to_string_lossy().ends_with(PARTIAL_SUFFIX));
    }

    /// A replaced model's file goes only when it is the file its id
    /// named; one of another size or hash stays.
    #[test]
    fn only_the_replaced_file_itself_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::open_at(dir.path());
        let bytes = b"klipp test bytes";
        let sha = "0".repeat(64);
        let real_sha = {
            std::fs::write(cache.path_for_id("old"), bytes).unwrap();
            sha256_of_file(&cache.path_for_id("old")).unwrap()
        };
        assert_eq!(cache.remove_replaced("old", bytes.len() as u64, &sha).unwrap(), None, "another hash stays");
        assert_eq!(cache.remove_replaced("old", 3, &real_sha).unwrap(), None, "another size stays");
        assert!(cache.path_for_id("old").is_file());
        assert_eq!(cache.remove_replaced("old", bytes.len() as u64, &real_sha).unwrap(), Some(bytes.len() as u64));
        assert!(!cache.path_for_id("old").exists());
        assert_eq!(cache.remove_replaced("old", bytes.len() as u64, &real_sha).unwrap(), None, "nothing left to remove");
    }

    #[test]
    fn sha256_of_file_matches_known_vector() {
        // SHA-256("klipp") computed with sha2::Sha256 — pinned so a
        // refactor of streaming chunks can't silently change the
        // result.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"klipp").unwrap();
        f.sync_all().unwrap();
        let got = sha256_of_file(&path).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(b"klipp");
        let expected = hex::encode(hasher.finalize());
        assert_eq!(got, expected);
    }
}
