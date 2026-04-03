//! Smart render cache — stores post-grade RGBA frames compressed with LZ4.
//!
//! Cache key: clip_id + frame_number + grade_hash.
//! Grade hash: FNV1a of deterministic JSON serialization of the grade state.
//! LRU eviction when total size exceeds max_cache_size_gb.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
pub struct HotCache {
    entries: Vec<Option<HotCacheEntry>>,
    capacity: usize,
}

struct HotCacheEntry {
    key: RenderCacheKey,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

impl HotCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: (0..capacity).map(|_| None).collect(),
            capacity,
        }
    }

    pub fn get(&self, key: &RenderCacheKey) -> Option<(&[u8], u32, u32)> {
        let slot = (key.frame_number.unsigned_abs() as usize) % self.capacity;
        self.entries[slot].as_ref()
            .filter(|e| e.key == *key)
            .map(|e| (e.rgba.as_slice(), e.width, e.height))
    }

    pub fn insert(&mut self, key: RenderCacheKey, rgba: Vec<u8>, width: u32, height: u32) {
        let slot = (key.frame_number.unsigned_abs() as usize) % self.capacity;
        self.entries[slot] = Some(HotCacheEntry { key, rgba, width, height });
    }

    pub fn clear(&mut self) {
        for entry in &mut self.entries {
            *entry = None;
        }
    }
}

/// Disk-backed render cache with LZ4 compression and LRU eviction.
pub struct DiskRenderCache {
    cache_dir: PathBuf,
    max_size_bytes: u64,
    /// Tracks file access times for LRU eviction (filename → last_access).
    access_order: BTreeMap<String, std::time::Instant>,
    enabled: bool,
}

impl DiskRenderCache {
    pub fn new(cache_dir: PathBuf, max_size_gb: u32) -> Self {
        let _ = std::fs::create_dir_all(&cache_dir);
        Self {
            cache_dir,
            max_size_bytes: max_size_gb as u64 * 1_073_741_824,
            access_order: BTreeMap::new(),
            enabled: true,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Read a cached frame from disk. Returns (rgba, width, height) or None.
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
                let expected_size = (width * height * 4) as usize;
                match lz4_flex::decompress(&compressed[8..], expected_size) {
                    Ok(rgba) => {
                        self.access_order.insert(filename, std::time::Instant::now());
                        Some((rgba, width, height))
                    }
                    Err(e) => {
                        warn!("Render cache decompress failed: {e}");
                        let _ = std::fs::remove_file(&path);
                        None
                    }
                }
            }
            Err(_) => None,
        }
    }

    /// Store a graded frame to disk cache.
    pub fn put(&mut self, key: &RenderCacheKey, rgba: &[u8], width: u32, height: u32) {
        if !self.enabled { return; }

        let compressed = lz4_flex::compress_prepend_size(rgba);
        let filename = key.filename();
        let path = self.cache_dir.join(&filename);

        // Write header (width + height) + compressed data
        let mut data = Vec::with_capacity(8 + compressed.len());
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&compressed);

        if let Err(e) = std::fs::write(&path, &data) {
            warn!("Render cache write failed: {e}");
            return;
        }

        self.access_order.insert(filename, std::time::Instant::now());

        // LRU eviction if over size limit
        self.evict_if_needed();
    }

    /// Check current cache size and evict oldest entries if over limit.
    fn evict_if_needed(&mut self) {
        let total_size: u64 = walkdir::WalkDir::new(&self.cache_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum();

        if total_size <= self.max_size_bytes { return; }

        // Collect all .lz4 files with modification time
        let mut files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.cache_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("lz4") {
                    if let Ok(meta) = path.metadata() {
                        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        files.push((path, mtime));
                    }
                }
            }
        }

        // Sort by modification time (oldest first)
        files.sort_by_key(|(_, t)| *t);

        // Delete oldest until under limit
        let mut removed = 0u64;
        let excess = total_size - self.max_size_bytes;
        for (path, _) in &files {
            if removed >= excess { break; }
            if let Ok(meta) = path.metadata() {
                removed += meta.len();
                let _ = std::fs::remove_file(path);
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    self.access_order.remove(name);
                }
            }
        }

        if removed > 0 {
            debug!("Render cache: evicted {:.1} MB (was {:.1}/{:.1} GB)",
                removed as f64 / 1_048_576.0,
                total_size as f64 / 1_073_741_824.0,
                self.max_size_bytes as f64 / 1_073_741_824.0);
        }
    }

    /// Get the cache directory path.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}
