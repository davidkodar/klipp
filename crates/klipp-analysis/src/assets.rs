//! Analysis assets (M4 step 8): what an analysis node reads, computed once
//! from the media and kept apart from the node's authored settings.
//!
//! An asset is the *raw* analysis of a whole media file: a deflicker's
//! luma per frame, a stabilisation's frame-to-frame motion. It's keyed by
//! the media's identity (its file: path, size and modification time, as
//! the analysis cache keys everything), the size and rate it was analysed
//! at, and the algorithm's version, and by nothing of a clip's. So a trim,
//! a split or a duplicate keeps it, and the node reads it at its frames'
//! source times; another file (a relink, the file changed) is another
//! key, and nothing is found until it's analysed. A proxy isn't another
//! file: the key is always the original's, so the viewer decoding a proxy
//! and export decoding the original read one asset. A node's
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
    /// Each frame's motion to the frames either side, for rolling-shutter
    /// correction.
    RollingShutter,
}

impl AssetKind {
    /// The name its files are stored under.
    pub fn id(self) -> &'static str {
        match self {
            AssetKind::Deflicker => "deflicker",
            AssetKind::Stabilisation => "stabilisation",
            AssetKind::RollingShutter => "rolling_shutter",
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
        // 2: in the frame's units, centred, its height 1 (#2771).
        // 3: each frame aligned by the planar tracker (#2754).
        AssetKind::Stabilisation => 3,
        AssetKind::RollingShutter => 1,
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
    /// What is kept for `key` at the current algorithm, cheaply, without
    /// reading it: equal stamps are the same asset, so a reader keeps what
    /// it made of one until the stamp changes. `None` when there's none at
    /// the current algorithm (absent, or stale).
    fn stamp(&self, key: &AssetKey) -> Option<u64>;
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

/// The width the analyser decodes a file at.
pub const ANALYSIS_WIDTH: u32 = 480;

/// The key of `kind`'s analysis of the file at `path`, whose frames are
/// `size` and whose rate is `fps`: at the analysis width, the height to
/// its aspect (even). The one formula the viewer and export both key by,
/// so they find each other's assets. `None` when the file isn't there.
pub fn key_at(kind: AssetKind, path: &std::path::Path, size: (u32, u32), fps: (u32, u32)) -> Option<AssetKey> {
    let (w, h) = (size.0.max(1), size.1.max(1));
    let height = ((ANALYSIS_WIDTH as u64 * h as u64 / w as u64) as u32 + 1) & !1;
    key_for(kind, path, ANALYSIS_WIDTH, height, fps)
}

/// The key of `kind`'s analysis of `media`: its file ([`key_at`]).
/// Nothing of a clip's: every clip of the file, trimmed, split or
/// duplicated, reads the one asset. `None` when the file isn't there.
pub fn asset_key(kind: AssetKind, media: &klipp_media::MediaItem) -> Option<AssetKey> {
    key_at(kind, &media.path, (media.resolution.width, media.resolution.height), (media.fps.num, media.fps.den))
}

/// How many frames an analysis of `duration_secs` at `fps` reads: the
/// one count the viewer and export both ask for, so they make the same
/// asset.
pub fn frames_of(duration_secs: f64, fps: f64) -> usize {
    (duration_secs * fps).round().max(0.0) as usize
}

/// Assets on disk, beside the media's other analysis results by default
/// (`<cache>/klipp/analysis/<media identity>/`): one file an asset, named
/// by its key. Outside the project file: a project reopened finds them; a
/// project opened where they aren't analyses again.
pub struct CacheStore {
    root: std::path::PathBuf,
}

impl Default for CacheStore {
    fn default() -> Self {
        Self { root: crate::store::analysis_dir() }
    }
}

impl CacheStore {
    /// A store under `root` instead of the analysis cache.
    pub fn at(root: std::path::PathBuf) -> Self {
        Self { root }
    }

    /// The file's name before its algorithm: one per key.
    fn stem(key: &AssetKey) -> String {
        format!("{}-{}x{}-{}_{}", key.kind.id(), key.width, key.height, key.fps.0, key.fps.1)
    }

    /// The file `key`'s asset by `algorithm` is kept in. The algorithm is
    /// in the name, so whether the current one is there needs no read.
    fn file(&self, key: &AssetKey, algorithm: u32) -> std::path::PathBuf {
        self.root.join(&key.media).join(format!("{}-a{algorithm}.json", Self::stem(key)))
    }
}

impl AssetStore for CacheStore {
    /// The current algorithm's asset, else one by another (stale, which
    /// [`lookup`] says so), read whole.
    fn get(&self, key: &AssetKey) -> Option<Asset> {
        let read = |path: &std::path::Path| std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Asset>(&t).ok());
        if let Some(a) = read(&self.file(key, algorithm(key.kind))) {
            return Some(a);
        }
        let prefix = format!("{}-a", Self::stem(key));
        std::fs::read_dir(self.root.join(&key.media)).ok()?.flatten().find_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name.starts_with(&prefix) && name.ends_with(".json")).then(|| read(&e.path())).flatten()
        })
    }

    fn put(&mut self, asset: Asset) -> Result<(), String> {
        let path = self.file(&asset.key, asset.algorithm);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string(&asset).map_err(|e| e.to_string())?;
        // Whole or not at all: a reader never sees half a file. The temp
        // file is this writer's own, so the viewer and an export writing
        // one asset at once don't write into each other's.
        static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("json.{}-{n}.tmp", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })
    }

    /// The current algorithm's file's length and modification time: a
    /// rewrite (a new analysis, by this process or another) is another
    /// stamp.
    fn stamp(&self, key: &AssetKey) -> Option<u64> {
        let meta = std::fs::metadata(self.file(key, algorithm(key.kind))).ok()?;
        let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some((modified.as_nanos() as u64).rotate_left(17) ^ meta.len())
    }
}

/// Assets in memory: a test's, or a render's that keeps none. Each put
/// is a new stamp.
#[derive(Default)]
pub struct MemoryStore(std::collections::HashMap<AssetKey, (u64, Asset)>, u64);

impl AssetStore for MemoryStore {
    fn get(&self, key: &AssetKey) -> Option<Asset> {
        self.0.get(key).map(|(_, a)| a.clone())
    }

    fn put(&mut self, asset: Asset) -> Result<(), String> {
        self.1 += 1;
        self.0.insert(asset.key.clone(), (self.1, asset));
        Ok(())
    }

    fn stamp(&self, key: &AssetKey) -> Option<u64> {
        self.0.get(key).filter(|(_, a)| a.algorithm == algorithm(key.kind)).map(|(n, _)| *n)
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

    /// Every clip of a file reads one asset: the key is the media's, at
    /// the analysis width and the file's rate, whatever a clip's trim.
    #[test]
    fn every_clip_of_a_file_reads_one_asset() {
        let dir = std::env::temp_dir().join(format!("klipp-asset-media-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mov");
        std::fs::write(&path, b"frames").unwrap();
        let mut item = klipp_media::MediaItem::synthetic_for_test(path.clone(), None);
        item.resolution = klipp_core::Resolution { width: 1920, height: 1080 };
        item.fps = klipp_core::Fps::new(25, 1);
        let k = asset_key(AssetKind::Deflicker, &item).unwrap();
        assert_eq!((k.width, k.height, k.fps), (480, 270, (25, 1)));
        assert_eq!(asset_key(AssetKind::Deflicker, &item.clone()).unwrap(), k, "a duplicate's media");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Kept on disk: a store opened again (a project reopened) finds it,
    /// at the algorithm that made it.
    #[test]
    fn an_asset_on_disk_is_found_again() {
        let root = std::env::temp_dir().join(format!("klipp-asset-store-{}", std::process::id()));
        let mut s = CacheStore::at(root.clone());
        store(&mut s, key("m"), serde_json::json!({ "luma": [0.5, 0.6] })).unwrap();
        let again = CacheStore::at(root.clone());
        assert_eq!(lookup(&again, &key("m")), Found::Ready(serde_json::json!({ "luma": [0.5, 0.6] })));
        assert_eq!(lookup(&again, &key("other")), Found::Absent);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A media item with a proxy keys its analysis by the original file:
    /// the viewer on the proxy and export on the original share it.
    #[test]
    fn a_proxy_shares_its_originals_asset() {
        let dir = std::env::temp_dir().join(format!("klipp-asset-proxy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mov");
        std::fs::write(&path, b"frames").unwrap();
        let plain = klipp_media::MediaItem::synthetic_for_test(path.clone(), None);
        let proxied = klipp_media::MediaItem::synthetic_for_test(path.clone(), Some("proxy.mov"));
        assert_eq!(asset_key(AssetKind::Stabilisation, &proxied), asset_key(AssetKind::Stabilisation, &plain));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file at another path (moved, or a project opened on another
    /// machine) is another key: its asset is absent, to analyse again,
    /// never read as the old one.
    #[test]
    fn a_moved_file_finds_no_asset() {
        let dir = std::env::temp_dir().join(format!("klipp-asset-moved-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("there")).unwrap();
        let (here, there) = (dir.join("clip.mov"), dir.join("there/clip.mov"));
        std::fs::write(&here, b"frames").unwrap();
        let mut s = MemoryStore::default();
        let k = key_for(AssetKind::Stabilisation, &here, 480, 270, (24, 1)).unwrap();
        store(&mut s, k, serde_json::json!([1.0])).unwrap();
        std::fs::rename(&here, &there).unwrap();
        let moved = key_for(AssetKind::Stabilisation, &there, 480, 270, (24, 1)).unwrap();
        assert_eq!(lookup(&s, &moved), Found::Absent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stamp says whether what's kept changed, without reading it: the
    /// same file, the same stamp; rewritten, another; none at the current
    /// algorithm, none (#1321 review: an asset read once, not each frame).
    #[test]
    fn a_stamp_follows_what_is_kept() {
        let root = std::env::temp_dir().join(format!("klipp-asset-stamp-{}", std::process::id()));
        let mut s = CacheStore::at(root.clone());
        assert_eq!(s.stamp(&key("m")), None, "nothing kept");
        store(&mut s, key("m"), serde_json::json!([1.0])).unwrap();
        let first = s.stamp(&key("m")).expect("kept");
        assert_eq!(s.stamp(&key("m")), Some(first), "unchanged");
        std::thread::sleep(std::time::Duration::from_millis(5));
        store(&mut s, key("m"), serde_json::json!([1.0, 2.0])).unwrap();
        assert_ne!(s.stamp(&key("m")), Some(first), "rewritten");
        let mut m = MemoryStore::default();
        store(&mut m, key("m"), serde_json::json!([1.0])).unwrap();
        let one = m.stamp(&key("m"));
        store(&mut m, key("m"), serde_json::json!([1.0])).unwrap();
        assert!(one.is_some() && m.stamp(&key("m")) != one, "each put a new stamp");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// On disk, an asset by another algorithm is found, as stale, and has
    /// no stamp; no writer leaves a temp file behind.
    #[test]
    fn on_disk_another_algorithm_is_stale_and_writes_leave_no_temp() {
        let root = std::env::temp_dir().join(format!("klipp-asset-alg-{}", std::process::id()));
        let mut s = CacheStore::at(root.clone());
        let old = algorithm(AssetKind::Deflicker) + 7;
        s.put(Asset { key: key("m"), algorithm: old, data: serde_json::json!([9.0]) }).unwrap();
        assert_eq!(lookup(&s, &key("m")), Found::Stale { algorithm: old });
        assert_eq!(s.stamp(&key("m")), None);
        store(&mut s, key("m"), serde_json::json!([1.0])).unwrap();
        assert_eq!(lookup(&s, &key("m")), Found::Ready(serde_json::json!([1.0])), "the current one is read first");
        let temps = std::fs::read_dir(root.join("m")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count();
        assert_eq!(temps, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The key's height is the source's aspect at the analysis width, even:
    /// one formula for the viewer's media and export's file.
    #[test]
    fn the_key_is_one_formula() {
        let dir = std::env::temp_dir().join(format!("klipp-asset-keyat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mov");
        std::fs::write(&path, b"frames").unwrap();
        let k = key_at(AssetKind::Deflicker, &path, (1920, 1080), (25, 1)).unwrap();
        assert_eq!((k.width, k.height), (480, 270));
        assert_eq!(key_at(AssetKind::Deflicker, &path, (1080, 1920), (25, 1)).unwrap().height, 854, "portrait, rounded to even");
        assert_eq!(frames_of(2.0, 25.0), 50);
        assert_eq!(frames_of(1.001, 29.97), 30);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
