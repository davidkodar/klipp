//! Analysis assets (M4 step 8): what an analysis node reads, computed once
//! from the media and kept apart from the node's authored settings.
//!
//! An asset is the *raw* analysis of a whole media file: a deflicker's
//! luma per frame, a stabilisation's frame-to-frame motion. It's keyed by
//! the media's identity (its file: path, size and modification time, as
//! the analysis cache keys everything), the size and rate it was analysed
//! at, and the algorithm's version, and by nothing of a clip's. So a trim,
//! a split or a duplicate keeps it, and the node reads it at its frames'
//! source times; another file (a relink, a proxy, the file changed) is
//! another key, and nothing is found until it's analysed. A node's
//! settings that don't change the raw pass (the deflicker window, the
//! stabilisation smoothing) are applied to the asset when it's read, so
//! moving them re-analyses nothing.
//!
//! An asset found under its key at another algorithm version is stale and
//! never read: the node runs as if it had none, and the analysis is owed
//! again.
//!
//! Known limit: an asset covers the whole file, so a five-second clip of a
//! two-hour file analyses two hours. Analysing in chunks can come when it
//! matters.

use serde::{Deserialize, Serialize};

/// The analyses kept as assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssetKind {
    /// Mean linear luma per frame.
    Deflicker,
    /// Frame-to-frame motion and the trajectory it accumulates to.
    Stabilisation,
}

impl AssetKind {
    /// The name its files are stored under.
    pub fn id(self) -> &'static str {
        match self {
            AssetKind::Deflicker => "deflicker",
            AssetKind::Stabilisation => "stabilisation",
        }
    }
}

/// What an asset was computed from, all of it: two keys are the same
/// asset exactly when they're equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetKey {
    pub kind: AssetKind,
    /// The media file's identity (`klipp_media::disk_cache::compute_cache_key`).
    pub media: String,
    /// The size the frames were analysed at.
    pub width: u32,
    pub height: u32,
    /// The rate it was analysed at, as a fraction.
    pub fps: (u32, u32),
}

/// The algorithm's version for `kind`: a change to the raw pass bumps it,
/// and every asset made by the old one is stale.
pub fn algorithm(kind: AssetKind) -> u32 {
    match kind {
        AssetKind::Deflicker => 1,
        AssetKind::Stabilisation => 1,
    }
}

/// A stored asset: its key, the algorithm that made it, and the data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub key: AssetKey,
    pub algorithm: u32,
    pub data: serde_json::Value,
}

/// What a lookup finds.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    /// Nothing under the key: not analysed (or the media changed).
    Absent,
    /// Something under the key, made by another algorithm: never read.
    Stale { algorithm: u32 },
    Ready(serde_json::Value),
}

/// Where assets are kept.
pub trait AssetStore {
    fn get(&self, key: &AssetKey) -> Option<Asset>;
    fn put(&mut self, asset: Asset) -> Result<(), String>;
}

/// The asset for `key`, if one is there at the current algorithm.
pub fn lookup(store: &dyn AssetStore, key: &AssetKey) -> Found {
    match store.get(key) {
        None => Found::Absent,
        Some(a) if a.key != *key => Found::Absent,
        Some(a) if a.algorithm != algorithm(key.kind) => Found::Stale { algorithm: a.algorithm },
        Some(a) => Found::Ready(a.data),
    }
}

/// Keep `data`, computed now for `key`.
pub fn store(store: &mut dyn AssetStore, key: AssetKey, data: serde_json::Value) -> Result<(), String> {
    store.put(Asset { algorithm: algorithm(key.kind), key, data })
}

/// The key of `kind`'s analysis of the media at `path`, at `width ×
/// height` and `fps`: `None` when the file isn't there.
pub fn key_for(kind: AssetKind, path: &std::path::Path, width: u32, height: u32, fps: (u32, u32)) -> Option<AssetKey> {
    Some(AssetKey { kind, media: crate::store::key_for(path)?, width, height, fps })
}

/// Assets in the analysis cache, beside the media's other results: one
/// file an asset, named by its key.
pub struct CacheStore;

fn file_id(key: &AssetKey) -> String {
    format!("{}-{}x{}-{}_{}", key.kind.id(), key.width, key.height, key.fps.0, key.fps.1)
}

impl AssetStore for CacheStore {
    fn get(&self, key: &AssetKey) -> Option<Asset> {
        let value = crate::store::load(&key.media, &file_id(key), 1)?;
        serde_json::from_value(value).ok()
    }

    fn put(&mut self, asset: Asset) -> Result<(), String> {
        let value = serde_json::to_value(&asset).map_err(|e| e.to_string())?;
        crate::store::save(&asset.key.media, &file_id(&asset.key), 1, &value)
    }
}

/// Assets in memory: a test's, or a render's that keeps none.
#[derive(Default)]
pub struct MemoryStore(std::collections::HashMap<AssetKey, Asset>);

impl AssetStore for MemoryStore {
    fn get(&self, key: &AssetKey) -> Option<Asset> {
        self.0.get(key).cloned()
    }

    fn put(&mut self, asset: Asset) -> Result<(), String> {
        self.0.insert(asset.key.clone(), asset);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(media: &str) -> AssetKey {
        AssetKey { kind: AssetKind::Deflicker, media: media.into(), width: 480, height: 270, fps: (24, 1) }
    }

    /// Stored and looked up under its key: ready. Not stored: absent.
    #[test]
    fn an_asset_is_found_under_its_key() {
        let mut s = MemoryStore::default();
        assert_eq!(lookup(&s, &key("a")), Found::Absent);
        store(&mut s, key("a"), serde_json::json!([1.0, 2.0])).unwrap();
        assert_eq!(lookup(&s, &key("a")), Found::Ready(serde_json::json!([1.0, 2.0])));
    }

    /// Another file (a relink, a proxy, the file changed: another media
    /// identity), another size or rate, another kind: not this asset.
    #[test]
    fn another_source_or_analysis_size_is_another_asset() {
        let mut s = MemoryStore::default();
        store(&mut s, key("a"), serde_json::json!([1.0])).unwrap();
        assert_eq!(lookup(&s, &key("b")), Found::Absent, "another file");
        assert_eq!(lookup(&s, &AssetKey { width: 960, ..key("a") }), Found::Absent, "another size");
        assert_eq!(lookup(&s, &AssetKey { fps: (25, 1), ..key("a") }), Found::Absent, "another rate");
        assert_eq!(lookup(&s, &AssetKey { kind: AssetKind::Stabilisation, ..key("a") }), Found::Absent, "another analysis");
    }

    /// One made by another algorithm is stale, and its data never given.
    #[test]
    fn a_stale_asset_is_never_read() {
        let mut s = MemoryStore::default();
        s.put(Asset { key: key("a"), algorithm: algorithm(AssetKind::Deflicker) + 1, data: serde_json::json!([9.0]) }).unwrap();
        assert_eq!(lookup(&s, &key("a")), Found::Stale { algorithm: algorithm(AssetKind::Deflicker) + 1 });
    }

    /// The media's identity is its file's: a file rewritten with another
    /// modification time is another key; the same file, the same key.
    #[test]
    fn a_changed_file_is_another_key() {
        let dir = std::env::temp_dir().join(format!("klipp-asset-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mov");
        std::fs::write(&path, b"one").unwrap();
        let a = key_for(AssetKind::Deflicker, &path, 480, 270, (24, 1)).unwrap();
        assert_eq!(key_for(AssetKind::Deflicker, &path, 480, 270, (24, 1)).unwrap(), a, "the same file");
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        assert_ne!(key_for(AssetKind::Deflicker, &path, 480, 270, (24, 1)).unwrap(), a, "changed since");
        assert_eq!(key_for(AssetKind::Deflicker, &dir.join("gone.mov"), 480, 270, (24, 1)), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
