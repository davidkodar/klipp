//! Smart render cache — stores post-grade RGBA frames compressed with LZ4.
//!
//! Cache key: clip_id + frame_number + grade_hash.
//! Grade hash: FNV1a of deterministic JSON serialization of the grade state.
//! LRU eviction when total size exceeds max_cache_size_gb.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tracing::{debug, warn};

/// FNV-1a hash (same as klipp_media::disk_cache, but we don't depend on that crate here).
fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// A hash of the render environment a cached frame was made under —
/// everything that changes the pixels without changing the clip: the
/// working colour space the grade runs in, LUT interpolation, whether
/// the proxy or the original was decoded, and the camera log format the
/// decoder converts the media with. None of these was in the key, so a
/// switch of working space or a log-format correction showed the old
/// frame back from the cache.
pub fn render_environment_hash(
    lut_tetrahedral: bool,
    working: klipp_core::WorkingColorspace,
    use_proxies: bool,
    log_format: Option<klipp_core::LogFormat>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    lut_tetrahedral.hash(&mut h);
    (working as u8).hash(&mut h);
    use_proxies.hash(&mut h);
    log_format.map(|f| f as u8).hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod environment_hash_tests {
    use super::render_environment_hash;
    use klipp_core::{LogFormat, WorkingColorspace};

    #[test]
    fn every_environment_change_changes_the_key() {
        let base = render_environment_hash(true, WorkingColorspace::Rec709Linear, false, None);
        assert_eq!(base, render_environment_hash(true, WorkingColorspace::Rec709Linear, false, None));
        assert_ne!(base, render_environment_hash(false, WorkingColorspace::Rec709Linear, false, None), "LUT interpolation");
        assert_ne!(base, render_environment_hash(true, WorkingColorspace::AcesCG, false, None), "working space");
        assert_ne!(base, render_environment_hash(true, WorkingColorspace::Rec709Linear, true, None), "proxy decode");
        assert_ne!(base, render_environment_hash(true, WorkingColorspace::Rec709Linear, false, Some(LogFormat::SLog3)), "log format");
    }
}

/// Compute a deterministic hash of a clip's grade state.
/// Uses sorted JSON serialization to ensure HashMap order doesn't affect the hash.
pub fn compute_grade_hash(
    node_graph: &klipp_nodes::NodeGraph,
    builtin_effects: &[klipp_effects::BuiltinEffect],
    builtin_effects_enabled: &[bool],
) -> u64 {
    // Serialize node graph deterministically
    // NodeGraph uses Vec (ordered) for nodes/edges, but node_positions uses HashMap.
    // We serialize nodes + edges + params only (positions don't affect rendering).
    let mut hasher_input = Vec::with_capacity(4096);

    for node in &node_graph.nodes {
        hasher_input.extend_from_slice(node.id.as_bytes());
        hasher_input.push(if node.enabled { 1 } else { 0 });
        // Serialize kind to JSON for deterministic hashing
        if let Ok(json) = serde_json::to_vec(&node.kind) {
            hasher_input.extend_from_slice(&json);
        }
        // Serialize params to JSON for deterministic hashing
        if let Ok(json) = serde_json::to_vec(&node.params) {
            hasher_input.extend_from_slice(&json);
        }
        // Include keyframes
        if let Ok(json) = serde_json::to_vec(&node.keyframes) {
            hasher_input.extend_from_slice(&json);
        }
    }

    // Include edges
    for edge in &node_graph.edges {
        hasher_input.extend_from_slice(edge.from_node.as_bytes());
        hasher_input.extend_from_slice(edge.to_node.as_bytes());
        hasher_input.push(edge.from_port);
        hasher_input.push(edge.to_port);
    }

    // Include builtin effects
    if let Ok(json) = serde_json::to_vec(builtin_effects) {
        hasher_input.extend_from_slice(&json);
    }
    for &enabled in builtin_effects_enabled {
        hasher_input.push(if enabled { 1 } else { 0 });
    }

    fnv1a(&hasher_input)
}

/// Cache key for a single rendered frame.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RenderCacheKey {
    pub clip_id: klipp_core::ClipId,
    pub frame_number: i64,
    pub grade_hash: u64,
}

impl RenderCacheKey {
    pub fn filename(&self) -> String {
        format!("{}_{:08}_{:016x}.lz4", self.clip_id.0.as_simple(), self.frame_number, self.grade_hash)
    }
}

/// In-memory hot cache (last N post-grade frames, zero decompress cost).
///
/// Keyed by the full `RenderCacheKey` (clip_id + frame_number + grade_hash)
/// so two clips that happen to share a frame_number cannot evict each other.
/// Eviction is FIFO on insert — the oldest entry is dropped when the cache
/// hits `capacity`, which is sufficient for a "last N frames" window and
/// matches the semantic intent of the previous modulo-slot design.
pub struct HotCache {
    entries: HashMap<RenderCacheKey, HotCacheEntry>,
    /// Insertion order, oldest at the front. On re-insert of an existing key
    /// the previous entry is moved to the back so the hot set keeps the most
    /// recently touched frames.
    order: VecDeque<RenderCacheKey>,
    capacity: usize,
}

struct HotCacheEntry {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

impl HotCache {
    /// How many entries belong to one clip at one grade. O(capacity) —
    /// the hot cache holds ~30 frames — where probing every timeline
    /// position was O(clip length in ticks): 18.8 million lookups for a
    /// five-minute 59.94 fps clip.
    pub fn count_for(&self, clip_id: klipp_core::ClipId, grade_hash: u64) -> usize {
        self.entries
            .keys()
            .filter(|k| k.clip_id == clip_id && k.grade_hash == grade_hash)
            .count()
    }

    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn get(&self, key: &RenderCacheKey) -> Option<(&[u8], u32, u32)> {
        self.entries
            .get(key)
            .map(|e| (e.rgba.as_slice(), e.width, e.height))
    }

    pub fn insert(&mut self, key: RenderCacheKey, rgba: Vec<u8>, width: u32, height: u32) {
        // Re-inserting an existing key: drop its old position so it ends up
        // at the back of the FIFO.
        if self.entries.contains_key(&key) {
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
            }
        }
        // Evict oldest until there's room. Capacity is typically small
        // (~30), so a linear pop is fine.
        while self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, HotCacheEntry { rgba, width, height });
    }

    /// Move an existing key to the back of the FIFO, leaving its frame
    /// where it is.
    ///
    /// A hit that reads the frame straight out of the cache has no copy of
    /// it to re-insert, and should not have to make one to say it was used:
    /// at 4K that copy is 33 MB.
    pub fn touch(&mut self, key: &RenderCacheKey) {
        if !self.entries.contains_key(key) {
            return;
        }
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            if let Some(k) = self.order.remove(pos) {
                self.order.push_back(k);
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

/// Key used to order on-disk cache entries by age.
///
/// The `path_hash` tiebreaker lets two entries with the same `mtime`
/// coexist in the BTreeMap without clobbering each other — a real risk
/// on filesystems whose mtime granularity is 1s or coarser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EntryKey {
    mtime: SystemTime,
    path_hash: u64,
}

#[derive(Clone, Debug)]
struct EntryMeta {
    path: PathBuf,
    bytes: u64,
}

fn path_hash(path: &Path) -> u64 {
    // Use the filename only — it uniquely identifies an entry within the
    // cache dir and avoids hashing the (stable) parent dir over and over.
    let bytes = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.as_bytes())
        .unwrap_or_default();
    fnv1a(bytes)
}

/// Disk-backed render cache with LZ4 compression and LRU eviction.
///
/// Eviction is driven by an in-memory index (`BTreeMap<EntryKey, EntryMeta>`)
/// that mirrors the on-disk state. The index is built once at construction
/// time via a single `walkdir` pass, and updated incrementally by `put`
/// (inserts) and `evict_if_needed` (removes from the front). This keeps
/// `put` at O(log n) per frame instead of paying an O(n) directory scan.
pub struct DiskRenderCache {
    cache_dir: PathBuf,
    max_size_bytes: u64,
    /// Every .lz4 entry currently on disk, sorted by mtime
    /// (oldest first — the front of the map is the next eviction target).
    index: BTreeMap<EntryKey, EntryMeta>,
    /// Running total of index entries' byte sizes. Avoids a full sum
    /// when deciding whether to evict.
    total_bytes: u64,
    enabled: bool,
}

impl DiskRenderCache {
    pub fn new(cache_dir: PathBuf, max_size_gb: u32) -> Self {
        let _ = std::fs::create_dir_all(&cache_dir);

        // Build the on-disk index once, at construction time.
        // This replaces the old lazy walkdir that ran on every put().
        let start = std::time::Instant::now();
        let mut index: BTreeMap<EntryKey, EntryMeta> = BTreeMap::new();
        let mut total_bytes: u64 = 0;
        for entry in walkdir::WalkDir::new(&cache_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("lz4") {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }
            let bytes = meta.len();
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let key = EntryKey { mtime, path_hash: path_hash(path) };
            index.insert(key, EntryMeta { path: path.to_path_buf(), bytes });
            total_bytes += bytes;
        }
        let elapsed = start.elapsed();
        debug!(
            "Render cache: indexed {} entries ({:.1} MB) in {:.1} ms",
            index.len(),
            total_bytes as f64 / 1_048_576.0,
            elapsed.as_secs_f64() * 1000.0,
        );

        let mut me = Self {
            cache_dir,
            max_size_bytes: max_size_gb as u64 * 1_073_741_824,
            index,
            total_bytes,
            enabled: true,
        };
        // If the existing cache already exceeds the cap (e.g. the cap was
        // lowered), trim it now rather than waiting for the first put().
        me.evict_if_needed();
        me
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Whether a frame is on disk for `key`, without reading it: the file's
    /// existence, where [`Self::get`] reads and decompresses the whole frame.
    pub fn contains(&self, key: &RenderCacheKey) -> bool {
        self.enabled && self.cache_dir.join(key.filename()).exists()
    }

    /// Read a cached frame from disk. Returns (rgba, width, height) or None.
    ///
    /// File layout (see [`Self::put`] for the writer):
    ///   bytes 0..4   : width  (u32 little-endian)
    ///   bytes 4..8   : height (u32 little-endian)
    ///   bytes 8..    : LZ4 payload produced by `lz4_flex::compress_prepend_size`
    ///                  (the payload carries its own 4-byte uncompressed-size
    ///                  header, so we pair it with `decompress_size_prepended`).
    pub fn get(&mut self, key: &RenderCacheKey) -> Option<(Vec<u8>, u32, u32)> {
        if !self.enabled { return None; }
        let filename = key.filename();
        let path = self.cache_dir.join(&filename);
        if !path.exists() { return None; }

        match std::fs::read(&path) {
            Ok(compressed) => {
                if compressed.len() < 8 { return None; }
                // Header: width (u32) + height (u32) + LZ4 compressed RGBA
                let width = u32::from_le_bytes([compressed[0], compressed[1], compressed[2], compressed[3]]);
                let height = u32::from_le_bytes([compressed[4], compressed[5], compressed[6], compressed[7]]);
                match lz4_flex::decompress_size_prepended(&compressed[8..]) {
                    Ok(rgba) => Some((rgba, width, height)),
                    Err(e) => {
                        warn!("Render cache decompress failed: {e}");
                        // Remove the corrupt file from disk AND index.
                        self.remove_entry(&path);
                        None
                    }
                }
            }
            Err(_) => None,
        }
    }

    /// Store a graded frame to disk cache.
    ///
    /// File layout (paired with [`Self::get`]):
    ///   bytes 0..4   : width  (u32 little-endian)
    ///   bytes 4..8   : height (u32 little-endian)
    ///   bytes 8..    : LZ4 payload produced by `lz4_flex::compress_prepend_size`
    ///                  (self-describing — carries the uncompressed size in its
    ///                  own 4-byte prefix).
    pub fn put(&mut self, key: &RenderCacheKey, rgba: &[u8], width: u32, height: u32) {
        if !self.enabled { return; }

        let compressed = lz4_flex::compress_prepend_size(rgba);
        let filename = key.filename();
        let path = self.cache_dir.join(&filename);

        // Write header (width + height) + compressed data.
        // Atomic rename pattern: write to a temp file, then rename into place
        // so a reader never sees a half-written file.
        let mut data = Vec::with_capacity(8 + compressed.len());
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&compressed);

        let tmp_path = self.cache_dir.join(format!("{filename}.tmp"));
        if let Err(e) = std::fs::write(&tmp_path, &data) {
            warn!("Render cache write failed: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp_path, &path) {
            warn!("Render cache rename failed: {e}");
            let _ = std::fs::remove_file(&tmp_path);
            return;
        }

        // Only touch the index AFTER a successful rename — if the write
        // or rename failed we must not lie about the file being present.
        let bytes = data.len() as u64;
        // If we're overwriting an existing entry (same filename → same
        // path_hash), remove its old index entry first so total_bytes
        // stays accurate. We don't know the old mtime without a stat,
        // and the BTreeMap is keyed by (mtime, path_hash), so scan by
        // path_hash. This is O(n) in the worst case but overwrites of
        // identical keys are rare in practice.
        let ph = path_hash(&path);
        let stale_key: Option<EntryKey> = self
            .index
            .iter()
            .find_map(|(k, _)| (k.path_hash == ph).then_some(*k));
        if let Some(k) = stale_key {
            if let Some(meta) = self.index.remove(&k) {
                self.total_bytes = self.total_bytes.saturating_sub(meta.bytes);
            }
        }

        let mtime = SystemTime::now();
        let key = EntryKey { mtime, path_hash: ph };
        self.index.insert(key, EntryMeta { path, bytes });
        self.total_bytes += bytes;

        // LRU eviction if over size limit
        self.evict_if_needed();
    }

    /// Remove a specific path from disk AND from the index. Used when we
    /// discover a corrupt entry mid-read; keeps the index in sync.
    fn remove_entry(&mut self, path: &Path) {
        let ph = path_hash(path);
        let key_to_remove: Option<EntryKey> = self
            .index
            .iter()
            .find_map(|(k, _)| (k.path_hash == ph).then_some(*k));
        if let Some(k) = key_to_remove {
            if let Some(meta) = self.index.remove(&k) {
                self.total_bytes = self.total_bytes.saturating_sub(meta.bytes);
            }
        }
        let _ = std::fs::remove_file(path);
    }

    /// Evict oldest entries until we're back under the size cap.
    ///
    /// Uses the in-memory index instead of a directory walk — O(log n)
    /// per eviction rather than O(n) per frame. External deletions of
    /// cache files are tolerated: a `NotFound` on `remove_file` is
    /// logged once and ignored.
    fn evict_if_needed(&mut self) {
        if self.total_bytes <= self.max_size_bytes {
            return;
        }

        let size_before = self.total_bytes;
        let mut removed_bytes: u64 = 0;

        while self.total_bytes > self.max_size_bytes {
            let Some((_k, meta)) = self.index.pop_first() else {
                // Index is empty but total_bytes > cap; self-heal.
                self.total_bytes = 0;
                break;
            };
            self.total_bytes = self.total_bytes.saturating_sub(meta.bytes);
            removed_bytes += meta.bytes;
            match std::fs::remove_file(&meta.path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // External deletion — index was stale, nothing to do.
                }
                Err(e) => {
                    warn!(
                        "Render cache: failed to delete {}: {e}",
                        meta.path.display()
                    );
                }
            }
        }

        if removed_bytes > 0 {
            debug!(
                "Render cache: evicted {:.1} MB (was {:.1}/{:.1} GB)",
                removed_bytes as f64 / 1_048_576.0,
                size_before as f64 / 1_073_741_824.0,
                self.max_size_bytes as f64 / 1_073_741_824.0,
            );
        }
    }

    /// Get the cache directory path.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}

#[cfg(test)]
mod tests {
    /// The coverage bar's count must come from the entries, not from
    /// probing timeline positions — and only this clip at this grade.
    #[test]
    fn hot_cache_counts_one_clip_at_one_grade() {
        let a = klipp_core::ClipId::new();
        let b = klipp_core::ClipId::new();
        let mut hot = HotCache::new(8);
        for f in 0..3 {
            hot.insert(RenderCacheKey { clip_id: a, frame_number: f * 1001, grade_hash: 7 }, vec![0; 4], 1, 1);
        }
        hot.insert(RenderCacheKey { clip_id: a, frame_number: 5005, grade_hash: 9 }, vec![0; 4], 1, 1);
        hot.insert(RenderCacheKey { clip_id: b, frame_number: 0, grade_hash: 7 }, vec![0; 4], 1, 1);
        assert_eq!(hot.count_for(a, 7), 3);
        assert_eq!(hot.count_for(a, 9), 1, "a stale grade's frames are not this grade's");
        assert_eq!(hot.count_for(b, 7), 1);
    }

    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NONCE: AtomicU64 = AtomicU64::new(0);

    /// Build a fresh temp directory that's unique per test invocation.
    /// Cleaned up via `Drop` on the returned guard.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(label: &str) -> Self {
            let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = std::env::temp_dir()
                .join(format!("klipp-render-cache-test-{label}-{pid}-{nonce}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create tmp");
            TmpDir(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Write a fake .lz4 file (valid header so `get` can still read it,
    /// though these tests only care about byte accounting).
    fn write_fake_entry(dir: &Path, name: &str, payload_bytes: usize) -> PathBuf {
        let path = dir.join(name);
        // 8-byte header (width=1, height=1) + payload. Actual LZ4 content
        // doesn't matter for eviction — just the file size.
        let mut data = Vec::with_capacity(8 + payload_bytes);
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.resize(8 + payload_bytes, 0u8);
        std::fs::write(&path, &data).expect("write fake entry");
        path
    }

    fn make_key(frame: i64) -> RenderCacheKey {
        RenderCacheKey {
            clip_id: klipp_core::ClipId::new(),
            frame_number: frame,
            grade_hash: 0xdead_beef,
        }
    }

    #[test]
    fn eviction_uses_index_not_walkdir() {
        // Capacity: 10 MB. Insert 3 × 4 MB entries (incompressible, so
        // LZ4 can't collapse them to a few KB). After the third insert
        // total would be ~12 MB > 10 MB, so the oldest entry must be
        // evicted and `total_bytes` must match what's actually on disk.
        let tmp = TmpDir::new("eviction");
        // `max_size_gb: u32` — we need ~10 MB. Construct with 0 GB then
        // clobber the internal cap. This stays within the public API for
        // construction; the cap override is test-only.
        let mut cache = DiskRenderCache::new(tmp.path().to_path_buf(), 0);
        cache.max_size_bytes = 10 * 1024 * 1024;

        // Truly incompressible payload: 4 MB filled with an xorshift-like
        // stream so LZ4 can't find repeats. A simple `i * 37 + 13`
        // pattern cycles every 256 bytes and compresses ~16×, which
        // would mask the eviction we're trying to exercise.
        let mut rgba_4mb = vec![0u8; 4 * 1024 * 1024];
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        for b in rgba_4mb.iter_mut() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            *b = s as u8;
        }

        let key_a = make_key(1);
        let key_b = make_key(2);
        let key_c = make_key(3);

        cache.put(&key_a, &rgba_4mb, 1024, 1024);
        // Ensure distinct mtimes so BTreeMap ordering is deterministic on
        // filesystems with coarse mtime granularity (most are ns; some are
        // 1s). 20 ms is more than enough in practice.
        std::thread::sleep(std::time::Duration::from_millis(20));
        cache.put(&key_b, &rgba_4mb, 1024, 1024);
        std::thread::sleep(std::time::Duration::from_millis(20));
        cache.put(&key_c, &rgba_4mb, 1024, 1024);

        // Guard against running under a filled /tmp — CachyOS defaults
        // tmpfs to 50% of RAM and a leftover test artefact (or a
        // debug-session orphan like the 8 GB `test_concat.mkv` this
        // session produced) can push /tmp to 100% and make every put
        // silently fail with ENOSPC. Without this guard the real
        // assertion below reads as a mysterious "index.len == 0",
        // hiding the actual environment problem.
        if cache.total_bytes == 0 {
            eprintln!(
                "SKIP eviction_uses_index_not_walkdir: /tmp write failed (likely ENOSPC — \
                 check `df -h /tmp` and clean up leftovers)"
            );
            return;
        }

        // After the third insert, total should be <= cap (eviction ran).
        assert!(
            cache.total_bytes <= cache.max_size_bytes,
            "total_bytes {} should be <= cap {}",
            cache.total_bytes,
            cache.max_size_bytes
        );

        // The oldest entry (key_a) must have been evicted, so its file
        // should no longer exist on disk.
        let path_a = tmp.path().join(key_a.filename());
        assert!(
            !path_a.exists(),
            "oldest entry {} should have been evicted",
            path_a.display()
        );

        // total_bytes must equal the sum of sizes still on disk.
        let on_disk: u64 = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("lz4"))
            .filter_map(|e| e.metadata().ok().map(|m| m.len()))
            .sum();
        assert_eq!(
            cache.total_bytes, on_disk,
            "in-memory total_bytes must match actual disk total"
        );

        // Index must be in sync with disk too.
        assert_eq!(cache.index.len(), 2, "two entries should remain");
    }

    #[test]
    fn contains_answers_without_reading_the_frame() {
        let tmp = TmpDir::new("contains");
        let mut cache = DiskRenderCache::new(tmp.path().to_path_buf(), 1);
        let key = make_key(7);
        assert!(!cache.contains(&key), "nothing written yet");
        cache.put(&key, &[1u8; 16], 2, 2);
        if cache.total_bytes == 0 {
            eprintln!("SKIP contains_answers_without_reading_the_frame: /tmp write failed");
            return;
        }
        assert!(cache.contains(&key), "a written frame is contained");
        assert!(!cache.contains(&make_key(8)), "another frame is not");
        cache.set_enabled(false);
        assert!(!cache.contains(&key), "a disabled cache holds nothing");
    }

    #[test]
    fn startup_indexes_existing_cache_dir() {
        // Pre-populate a directory with 5 .lz4 files of known sizes, then
        // open a DiskRenderCache over it and assert the index reflects
        // exactly what's on disk.
        let tmp = TmpDir::new("startup");
        let sizes = [1_024usize, 2_048, 4_096, 8_192, 16_384];
        let mut expected_total: u64 = 0;
        for (i, &sz) in sizes.iter().enumerate() {
            let name = format!("fake_{i:08}_{:016x}.lz4", 0xabcd_1234u64);
            let _ = write_fake_entry(tmp.path(), &name, sz);
            // 8-byte header + payload.
            expected_total += 8 + sz as u64;
        }

        // Drop a non-.lz4 sibling to make sure it's ignored.
        std::fs::write(tmp.path().join("not-a-cache-file.txt"), b"hello")
            .expect("write stray");

        // 20 GB cap — way above our 5-file test total, so no eviction.
        let cache = DiskRenderCache::new(tmp.path().to_path_buf(), 20);

        assert_eq!(cache.index.len(), 5, "index must contain all 5 .lz4 files");
        assert_eq!(
            cache.total_bytes, expected_total,
            "total_bytes must equal sum of file sizes"
        );

        // Entries in the BTreeMap are ordered by mtime — touch a file and
        // rebuild to confirm the order reflects on-disk mtime. Non-critical
        // but documents the invariant.
        drop(cache);

        // Pick any one of the files, bump its mtime into the future, and
        // re-open. The bumped file should be *last* in the index.
        let target = tmp.path().join(format!("fake_{:08}_{:016x}.lz4", 2, 0xabcd_1234u64));
        // Simply rewrite it — that updates mtime on most filesystems.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&target, vec![0u8; 4_096 + 8]).expect("rewrite to bump mtime");

        let cache = DiskRenderCache::new(tmp.path().to_path_buf(), 20);
        let last_path = cache
            .index
            .iter()
            .next_back()
            .map(|(_, m)| m.path.clone())
            .expect("at least one entry");
        assert_eq!(
            last_path, target,
            "newest (most-recently-written) file must be at the back of the index"
        );
    }

    #[test]
    fn evict_tolerates_externally_deleted_files() {
        // If someone else deletes a cache file out from under us, the
        // index is stale but eviction must still make progress.
        let tmp = TmpDir::new("external_delete");
        let mut cache = DiskRenderCache::new(tmp.path().to_path_buf(), 0);
        cache.max_size_bytes = 1; // force eviction on any insert

        let mut payload = vec![0u8; 1024];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31);
        }
        let key = make_key(42);
        cache.put(&key, &payload, 16, 16);

        // Delete the file out-of-band (simulating an external tool) and
        // confirm eviction doesn't panic or leave the index wedged.
        let path = tmp.path().join(key.filename());
        let _ = std::fs::remove_file(&path);

        // Force another eviction pass; the index will try to delete the
        // (already-gone) file and should tolerate NotFound.
        let key2 = make_key(43);
        cache.put(&key2, &payload, 16, 16);

        // total_bytes should track remaining-on-disk, which is just key2's
        // file (or nothing, if cap=1 evicts that too).
        assert!(cache.total_bytes <= cache.max_size_bytes + payload.len() as u64 + 8);
    }

    #[test]
    fn round_trip_put_then_get() {
        // Regression: `put` writes `compress_prepend_size` but `get` used
        // `decompress` on `compressed[8..]` — so the 4-byte size prefix that
        // `compress_prepend_size` embeds was fed to the raw decoder as
        // compressed payload, producing "offset to copy is not contained in
        // the decompressed buffer" warnings and silent cache misses.
        //
        // This test closes that gap: put a known buffer, read it straight
        // back, assert the bytes match exactly — and assert the matching
        // width/height come through.
        let tmp = TmpDir::new("round_trip");
        let mut cache = DiskRenderCache::new(tmp.path().to_path_buf(), 20);

        let width: u32 = 64;
        let height: u32 = 48;
        let len = (width * height * 4) as usize;

        // Non-trivial buffer: mix compressible runs with pseudo-random bytes
        // so both LZ4 paths (back-references and literal runs) get exercised.
        let mut rgba = vec![0u8; len];
        let mut s: u64 = 0xC0FF_EE00_1234_5678;
        for (i, b) in rgba.iter_mut().enumerate() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            *b = if i % 8 < 4 { 0xAB } else { s as u8 };
        }

        let key = make_key(99);
        cache.put(&key, &rgba, width, height);

        let (got, got_w, got_h) = cache
            .get(&key)
            .expect("round-trip: entry must be readable after put");
        assert_eq!(got_w, width, "width must round-trip");
        assert_eq!(got_h, height, "height must round-trip");
        assert_eq!(got.len(), rgba.len(), "decompressed length must match");
        assert_eq!(got, rgba, "round-trip bytes must match input");

        // Sanity: the file must still exist on disk after the successful
        // `get` — `get` only deletes on decompress failure.
        let path = tmp.path().join(key.filename());
        assert!(path.exists(), "successful get must not delete the entry");
    }

    #[test]
    fn atomic_rename_leaves_file_on_disk() {
        // Sanity check for the atomic-rename path: after `put`, a file
        // with the computed filename exists at the expected location
        // and no stray `.tmp` siblings remain.
        let tmp = TmpDir::new("atomic_rename");
        let mut cache = DiskRenderCache::new(tmp.path().to_path_buf(), 20);
        let key = make_key(7);
        let rgba: Vec<u8> = (0..(4 * 4 * 4)).map(|i| i as u8).collect(); // 4x4 px
        cache.put(&key, &rgba, 4, 4);

        let final_path = tmp.path().join(key.filename());
        assert!(final_path.exists(), "put must leave file at final path");
        assert_eq!(cache.index.len(), 1);
        assert_eq!(cache.total_bytes, std::fs::metadata(&final_path).unwrap().len());

        // No leftover .tmp files.
        let stray_tmp: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s == "tmp")
                    .unwrap_or(false)
            })
            .collect();
        assert!(stray_tmp.is_empty(), "no leftover .tmp files");
    }
}
