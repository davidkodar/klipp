//! Frames kept on disk by content key (#3207, #3212): a frozen track's
//! layers now, the background render's frames later, in namespaces under
//! one root (the project's cache folder) and one size cap.
//!
//! A frame is a file `<namespace>/<tick>_<key>_<w>x<h>.klf`: a header
//! (magic, format version, size) and its pixels, RGBA half floats with
//! straight alpha, LZ4-compressed. It is written to a `.tmp` beside it and
//! renamed, so a cancelled or crashed write leaves nothing a reader takes.
//! A frame is found by its tick, its key and its size; one stored under
//! another key is stale and is never returned for this one, but is kept
//! until evicted, so undo back to a state that was stored finds it again.
//!
//! Over the cap, the oldest frames of namespaces that aren't pinned go
//! first. A pinned namespace (a frozen track's) is never evicted: when only
//! pinned frames are left over the cap, [`CacheStore::put`] says so and the
//! caller stops rather than evicting its own frames.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"KLF1";
/// The format of a stored frame: RGBA half floats, straight alpha, linear
/// in the working space.
const FORMAT_RGBA_F16: u32 = 1;
const HEADER: usize = 16;
/// The largest frame a header may claim, so a hostile or corrupt file
/// can't ask for an allocation the size of memory: 16K × 16K.
const MAX_SIDE: u32 = 16_384;

/// Where a frame lives in the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameAt {
    pub tick: i64,
    pub key: u64,
    pub size: (u32, u32),
}

impl FrameAt {
    fn file_name(&self) -> String {
        format!("{:010}_{:016x}_{}x{}.klf", self.tick, self.key, self.size.0, self.size.1)
    }

    fn parse(name: &str) -> Option<FrameAt> {
        let stem = name.strip_suffix(".klf")?;
        let mut parts = stem.splitn(3, '_');
        let tick = parts.next()?.parse().ok()?;
        let key = u64::from_str_radix(parts.next()?, 16).ok()?;
        let (w, h) = parts.next()?.split_once('x')?;
        Some(FrameAt { tick, key, size: (w.parse().ok()?, h.parse().ok()?) })
    }
}

#[derive(Clone, Debug)]
struct Entry {
    namespace: String,
    at: FrameAt,
    bytes: u64,
}

/// Why a frame wasn't stored.
#[derive(Debug, PartialEq, Eq)]
pub enum PutRefused {
    /// The cap is full of pinned frames: nothing may be evicted for it.
    Full,
    /// The file couldn't be written.
    Io(String),
}

impl std::fmt::Display for PutRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PutRefused::Full => write!(f, "the cache is full of frozen frames: raise its size or unfreeze a track"),
            PutRefused::Io(why) => write!(f, "the frame couldn't be written: {why}"),
        }
    }
}

/// See the module.
pub struct CacheStore {
    root: PathBuf,
    cap_bytes: u64,
    /// Oldest first: (write order, file) → entry.
    entries: BTreeMap<(u64, PathBuf), Entry>,
    /// Where each file is in `entries`.
    order_of: HashMap<PathBuf, u64>,
    next_order: u64,
    bytes: HashMap<String, u64>,
    pinned: HashSet<String>,
}

impl CacheStore {
    /// The store under `root`, its frames on disk counted (oldest by
    /// modification time first), within `cap_bytes`.
    pub fn open(root: impl Into<PathBuf>, cap_bytes: u64) -> Self {
        let root = root.into();
        let mut found: Vec<(std::time::SystemTime, Entry, PathBuf)> = Vec::new();
        // A namespace may be a path (`freeze/<track>`): frames at any depth
        // below it, their folder their namespace.
        for e in walkdir::WalkDir::new(&root).min_depth(2).max_depth(4).into_iter().flatten().filter(|e| e.file_type().is_file()) {
            let path = e.path().to_path_buf();
            let name = e.file_name().to_string_lossy();
            if name.ends_with(".tmp") {
                // A write that never finished.
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let Some(at) = FrameAt::parse(&name) else { continue };
            let Some(namespace) = path.parent().and_then(|p| p.strip_prefix(&root).ok()).map(|p| p.to_string_lossy().into_owned()) else { continue };
            let Ok(meta) = e.metadata() else { continue };
            found.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), Entry { namespace, at, bytes: meta.len() }, path));
        }
        found.sort_by_key(|(t, _, _)| *t);
        let mut store = CacheStore { root, cap_bytes, entries: BTreeMap::new(), order_of: HashMap::new(), next_order: 0, bytes: HashMap::new(), pinned: HashSet::new() };
        for (_, entry, path) in found {
            store.account(path, entry);
        }
        store
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The cap, in bytes; frames over it are evicted at once, oldest of the
    /// unpinned first.
    pub fn set_cap(&mut self, cap_bytes: u64) {
        self.cap_bytes = cap_bytes;
        self.evict();
    }

    pub fn cap(&self) -> u64 {
        self.cap_bytes
    }

    /// Keep `namespace`'s frames whatever the cap (a frozen track's).
    pub fn pin(&mut self, namespace: &str) {
        self.pinned.insert(namespace.to_owned());
    }

    /// Let `namespace`'s frames be evicted again (an unfrozen track's
    /// render, kept for a re-freeze while it lasts).
    pub fn unpin(&mut self, namespace: &str) {
        self.pinned.remove(namespace);
        self.evict();
    }

    /// The bytes `namespace` holds on disk.
    pub fn bytes(&self, namespace: &str) -> u64 {
        self.bytes.get(namespace).copied().unwrap_or(0)
    }

    pub fn total_bytes(&self) -> u64 {
        self.bytes.values().sum()
    }

    /// Whether the frame is stored.
    pub fn contains(&self, namespace: &str, at: FrameAt) -> bool {
        self.order_of.contains_key(&self.path(namespace, at))
    }

    /// Store `rgba` (straight alpha, linear, `at.size`) as `namespace`'s frame
    /// `at`, evicting the oldest unpinned frames over the cap.
    pub fn put(&mut self, namespace: &str, at: FrameAt, rgba: &[f32]) -> Result<(), PutRefused> {
        let (w, h) = at.size;
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || rgba.len() != (w as usize) * (h as usize) * 4 {
            return Err(PutRefused::Io(format!("{} values for a {w}x{h} frame", rgba.len())));
        }
        let mut halves = Vec::with_capacity(rgba.len() * 2);
        for v in rgba {
            halves.extend_from_slice(&half::f16::from_f32(*v).to_bits().to_le_bytes());
        }
        let packed = lz4_flex::compress_prepend_size(&halves);
        let needed = (HEADER + packed.len()) as u64;
        if !self.room_for(namespace, needed) {
            return Err(PutRefused::Full);
        }
        let path = self.path(namespace, at);
        let dir = path.parent().expect("a frame is in its namespace's folder");
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            let tmp = path.with_extension("klf.tmp");
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(MAGIC)?;
            f.write_all(&FORMAT_RGBA_F16.to_le_bytes())?;
            f.write_all(&w.to_le_bytes())?;
            f.write_all(&h.to_le_bytes())?;
            f.write_all(&packed)?;
            f.sync_data()?;
            std::fs::rename(&tmp, &path)
        };
        write().map_err(|e| PutRefused::Io(e.to_string()))?;
        self.forget(&path);
        self.account(path, Entry { namespace: namespace.to_owned(), at, bytes: needed });
        self.evict();
        Ok(())
    }

    /// `namespace`'s frame `at`, as stored (straight alpha, linear), or
    /// `None` when it isn't stored or its file is unreadable (then removed).
    pub fn get(&mut self, namespace: &str, at: FrameAt) -> Option<Vec<f32>> {
        let path = self.path(namespace, at);
        if !self.order_of.contains_key(&path) {
            return None;
        }
        match read_frame(&path, at.size) {
            Some(pixels) => Some(pixels),
            None => {
                tracing::warn!("frame cache: {} is unreadable; it is rendered again", path.display());
                let _ = std::fs::remove_file(&path);
                self.forget(&path);
                None
            }
        }
    }

    /// Every frame `namespace` holds.
    pub fn frames(&self, namespace: &str) -> Vec<FrameAt> {
        self.entries.values().filter(|e| e.namespace == namespace).map(|e| e.at).collect()
    }

    /// Remove `namespace` and its frames (a cleared freeze).
    pub fn remove_namespace(&mut self, namespace: &str) {
        let paths: Vec<PathBuf> = self.entries.iter().filter(|(_, e)| e.namespace == namespace).map(|((_, p), _)| p.clone()).collect();
        for p in paths {
            let _ = std::fs::remove_file(&p);
            self.forget(&p);
        }
        let _ = std::fs::remove_dir_all(self.root.join(namespace));
        self.bytes.remove(namespace);
        self.pinned.remove(namespace);
    }

    fn path(&self, namespace: &str, at: FrameAt) -> PathBuf {
        self.root.join(namespace).join(at.file_name())
    }

    fn account(&mut self, path: PathBuf, entry: Entry) {
        *self.bytes.entry(entry.namespace.clone()).or_default() += entry.bytes;
        let order = self.next_order;
        self.next_order += 1;
        self.order_of.insert(path.clone(), order);
        self.entries.insert((order, path), entry);
    }

    fn forget(&mut self, path: &Path) {
        if let Some(order) = self.order_of.remove(path) {
            if let Some(e) = self.entries.remove(&(order, path.to_path_buf())) {
                if let Some(b) = self.bytes.get_mut(&e.namespace) {
                    *b = b.saturating_sub(e.bytes);
                }
            }
        }
    }

    /// Whether `needed` more bytes for `namespace` fit once every frame
    /// that may be evicted is gone: what is pinned, with this frame when
    /// its namespace is pinned, must fit the cap.
    fn room_for(&self, namespace: &str, needed: u64) -> bool {
        let pinned: u64 = self.bytes.iter().filter(|(n, _)| self.pinned.contains(*n)).map(|(_, b)| b).sum();
        let kept = if self.pinned.contains(namespace) { pinned + needed } else { pinned };
        kept <= self.cap_bytes && needed <= self.cap_bytes
    }

    fn evict(&mut self) {
        while self.total_bytes() > self.cap_bytes {
            let Some((_, path)) = self.entries.iter().find(|(_, e)| !self.pinned.contains(&e.namespace)).map(|(k, _)| k.clone()) else { break };
            let _ = std::fs::remove_file(&path);
            self.forget(&path);
        }
    }
}

fn read_frame(path: &Path, size: (u32, u32)) -> Option<Vec<f32>> {
    let data = std::fs::read(path).ok()?;
    if data.len() < HEADER || &data[..4] != MAGIC {
        return None;
    }
    let word = |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
    let (format, w, h) = (word(4), word(8), word(12));
    if format != FORMAT_RGBA_F16 || (w, h) != size || w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return None;
    }
    let expected = (w as usize) * (h as usize) * 4 * 2;
    // The prepended size must match the frame, before anything is
    // allocated for it.
    let claimed = u32::from_le_bytes(data.get(HEADER..HEADER + 4)?.try_into().ok()?) as usize;
    if claimed != expected {
        return None;
    }
    let raw = lz4_flex::decompress_size_prepended(&data[HEADER..]).ok()?;
    if raw.len() != expected {
        return None;
    }
    Some(raw.chunks_exact(2).map(|b| half::f16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("klipp-cache-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// A 4x2 frame whose values vary, with alpha that does too.
    fn frame(seed: f32) -> Vec<f32> {
        (0..4 * 2).flat_map(|i| [seed + i as f32 * 0.05, 0.5 - seed, 0.25, (i as f32 / 7.0).min(1.0)]).collect()
    }

    fn at(tick: i64, key: u64) -> FrameAt {
        FrameAt { tick, key, size: (4, 2) }
    }

    /// A frame comes back as it went in, to half-float precision, by its
    /// tick, key and size only; under another key it is not found but is
    /// kept, for undo back to it; the store counts its bytes.
    #[test]
    fn a_frame_is_found_by_its_tick_key_and_size_only() {
        let d = dir("roundtrip");
        let mut s = CacheStore::open(&d, 1 << 30);
        s.put("freeze/t1", at(5, 0xa), &frame(0.1)).unwrap();
        let got = s.get("freeze/t1", at(5, 0xa)).expect("stored");
        assert!(got.iter().zip(frame(0.1)).all(|(a, b)| (a - b).abs() < 1e-3), "half-float precision");
        assert!(s.get("freeze/t1", at(5, 0xb)).is_none(), "another key: stale, not returned");
        assert!(s.get("freeze/t1", FrameAt { size: (8, 4), ..at(5, 0xa) }).is_none(), "another size");
        assert!(s.get("freeze/t2", at(5, 0xa)).is_none(), "another namespace");
        s.put("freeze/t1", at(5, 0xb), &frame(0.2)).unwrap();
        assert!(s.contains("freeze/t1", at(5, 0xa)) && s.contains("freeze/t1", at(5, 0xb)), "the older key kept");
        assert!(s.bytes("freeze/t1") > 0 && s.bytes("freeze/t1") == s.total_bytes());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Opened again, the store finds what it wrote; a write that never
    /// finished (a `.tmp`) is removed, never read.
    #[test]
    fn opened_again_it_finds_its_frames_and_drops_unfinished_writes() {
        let d = dir("reopen");
        {
            let mut s = CacheStore::open(&d, 1 << 30);
            s.put("freeze/t1", at(1, 1), &frame(0.3)).unwrap();
            s.put("freeze/t1", at(2, 1), &frame(0.4)).unwrap();
        }
        let half_written = d.join("freeze/t1").join(format!("{}.tmp", at(3, 1).file_name()));
        std::fs::write(&half_written, b"KLF1 not done").unwrap();
        let mut s = CacheStore::open(&d, 1 << 30);
        assert_eq!(s.frames("freeze/t1").len(), 2);
        assert!(s.get("freeze/t1", at(2, 1)).is_some());
        assert!(!half_written.exists(), "the unfinished write removed");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Over the cap, the oldest unpinned frames go; a pinned namespace's
    /// never do, and when they fill the cap a new frame is refused rather
    /// than evicting them.
    #[test]
    fn the_cap_evicts_the_oldest_unpinned_and_never_a_pinned_frame() {
        let d = dir("cap");
        let mut s = CacheStore::open(&d, 1 << 30);
        s.put("frames", at(1, 1), &frame(0.1)).unwrap();
        s.pin("freeze/t1");
        s.put("freeze/t1", at(1, 1), &frame(0.2)).unwrap();
        s.put("frames", at(2, 1), &frame(0.3)).unwrap();
        // A byte short of all three: the oldest unpinned one goes.
        s.set_cap(s.total_bytes() - 1);
        assert!(!s.contains("frames", at(1, 1)), "the oldest unpinned evicted");
        assert!(s.contains("frames", at(2, 1)) && s.contains("freeze/t1", at(1, 1)));
        // Room for the pinned frame alone: a second pinned one is refused,
        // the pinned one kept, every unpinned one gone.
        s.set_cap(s.bytes("freeze/t1") + 1);
        assert!(s.contains("freeze/t1", at(1, 1)) && !s.contains("frames", at(2, 1)));
        assert_eq!(s.put("freeze/t1", at(2, 1), &frame(0.4)), Err(PutRefused::Full));
        assert!(s.contains("freeze/t1", at(1, 1)), "the pinned frame kept");
        // Unpinned, it may go.
        s.unpin("freeze/t1");
        s.set_cap(0);
        assert_eq!(s.total_bytes(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A cleared namespace leaves nothing on disk or in the counts.
    #[test]
    fn a_removed_namespace_leaves_nothing() {
        let d = dir("remove");
        let mut s = CacheStore::open(&d, 1 << 30);
        s.pin("freeze/t1");
        s.put("freeze/t1", at(1, 1), &frame(0.1)).unwrap();
        s.put("frames", at(1, 1), &frame(0.1)).unwrap();
        s.remove_namespace("freeze/t1");
        assert_eq!(s.bytes("freeze/t1"), 0);
        assert!(!d.join("freeze/t1").exists());
        assert!(s.contains("frames", at(1, 1)), "another namespace kept");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A frame file from elsewhere is read as hostile: a header that isn't
    /// ours, a size other than its name's, a compressed length that claims
    /// more than the frame (checked before anything is allocated for it),
    /// or a truncated body is refused and the file removed, never shown.
    #[test]
    fn a_hostile_or_broken_frame_file_is_refused_and_removed() {
        let d = dir("hostile");
        let mut s = CacheStore::open(&d, 1 << 30);
        s.put("freeze/t1", at(1, 1), &frame(0.1)).unwrap();
        let good = std::fs::read(d.join("freeze/t1").join(at(1, 1).file_name())).unwrap();
        let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
        let mut magic = good.clone();
        magic[..4].copy_from_slice(b"EVIL");
        cases.push(("another magic", magic));
        let mut sized = good.clone();
        sized[8..12].copy_from_slice(&16_384u32.to_le_bytes());
        cases.push(("a size other than its name's", sized));
        let mut claims = good.clone();
        claims[HEADER..HEADER + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        cases.push(("a length that claims 4 GB", claims));
        cases.push(("truncated", good[..good.len() / 2].to_vec()));
        cases.push(("only a header", good[..HEADER].to_vec()));
        for (i, (name, bytes)) in cases.into_iter().enumerate() {
            let a = at(10 + i as i64, 1);
            let path = d.join("freeze/t1").join(a.file_name());
            std::fs::write(&path, bytes).unwrap();
            let mut s = CacheStore::open(&d, 1 << 30);
            assert!(s.get("freeze/t1", a).is_none(), "{name}: refused");
            assert!(!path.exists(), "{name}: removed");
        }
        assert!(s.get("freeze/t1", at(1, 1)).is_some(), "the good frame still read");
        let _ = std::fs::remove_dir_all(&d);
    }
}
