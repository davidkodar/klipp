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
/// The format of a whole opaque frame (#3212): red, green and blue in ten
/// bits each through [`encode10`]'s curve, a word a pixel. 23 % smaller
/// than half floats after LZ4 on grainy footage, and no display level off.
const FORMAT_RGB10: u32 = 2;
/// Set in the format word when the pixels are compressed as chunks of
/// their own, decompressed side by side (#3212): one LZ4 block over a 4K
/// frame took 16 ms on one core, more than a frame's share of 24 fps once
/// the rest of a read is added.
const CHUNKED: u32 = 0x100;
/// Chunks a frame is compressed in, and the most a file may claim.
const CHUNKS: usize = 16;
const MAX_CHUNKS: usize = 64;

/// The curve a whole frame's linear values take into ten bits (#3212):
/// sRGB's to 1, then a log segment to 16, so highlights past 1 are kept.
/// A fixed curve, not the display's: a frame kept so is never stale under
/// another display transform.
pub(crate) fn encode10(c: f32) -> u32 {
    let e = if c <= 1.0 {
        if c <= 0.003_130_8 { c.max(0.0) * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
    } else {
        1.0 + c.min(16.0).log2() / 4.0
    };
    ((e / 2.0).clamp(0.0, 1.0) * 1023.0).round() as u32
}

pub(crate) fn decode10(v: u32) -> f32 {
    let e = (v & 1023) as f32 / 1023.0 * 2.0;
    if e <= 1.0 {
        if e <= 0.04045 { e / 12.92 } else { ((e + 0.055) / 1.055).powf(2.4) }
    } else {
        2f32.powf((e - 1.0) * 4.0)
    }
}

/// Whether a frame keeps in ten bits: opaque, and every value within the
/// curve.
fn fits_ten_bits(rgba: &[f32]) -> bool {
    rgba.chunks_exact(4).all(|p| p[3] >= 1.0 && p[..3].iter().all(|c| (0.0..=16.0).contains(c)))
}
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
    pub fn file_name(&self) -> String {
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

/// Whether `namespace` names a folder under the root and nothing else:
/// one or more components joined by `/`, each a plain name of letters,
/// digits, `-` and `_`. Not empty, not absolute, no `.` or `..`: a
/// namespace is joined onto the root and, cleared, removed with all it
/// holds, so anything else could reach the root itself or outside it.
/// Whether `root` or a folder from it down to `dir` is a link.
fn linked(root: &Path, dir: &Path) -> bool {
    let is_link = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
    let mut at = dir;
    loop {
        if is_link(at) {
            return true;
        }
        if at == root {
            return false;
        }
        match at.parent() {
            Some(up) if up.starts_with(root) => at = up,
            _ => return false,
        }
    }
}

pub fn valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace.split('/').all(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
}

/// Why a frame wasn't stored.
#[derive(Debug, PartialEq, Eq)]
pub enum PutRefused {
    /// The cap is full of pinned frames: nothing may be evicted for it.
    Full,
    /// The file couldn't be written.
    Io(String),
    /// The namespace isn't a plain name under the root ([`valid_namespace`]).
    Namespace(String),
}

impl std::fmt::Display for PutRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PutRefused::Full => write!(f, "the cache is full of frozen frames: raise its size or unfreeze a track"),
            PutRefused::Io(why) => write!(f, "the frame couldn't be written: {why}"),
            PutRefused::Namespace(ns) => write!(f, "{ns:?} isn't a cache namespace"),
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
        valid_namespace(namespace) && self.order_of.contains_key(&self.path(namespace, at))
    }

    /// Store `rgba` (straight alpha, linear, `at.size`) as `namespace`'s frame
    /// `at`, evicting the oldest unpinned frames over the cap.
    pub fn put(&mut self, namespace: &str, at: FrameAt, rgba: &[f32]) -> Result<(), PutRefused> {
        self.put_as(namespace, at, rgba, false, true)
    }

    /// [`Self::put`] a whole frame (#3212): in ten bits a channel where it
    /// is opaque and within the curve, as half floats otherwise.
    pub fn put_frame(&mut self, namespace: &str, at: FrameAt, rgba: &[f32]) -> Result<(), PutRefused> {
        self.put_as(namespace, at, rgba, true, true)
    }

    /// [`Self::put_frame`] only where it fits without evicting anything:
    /// `Full` otherwise. Drawing ahead while idle never makes room by
    /// letting go of frames it drew a moment before (#3212).
    pub fn put_frame_within_cap(&mut self, namespace: &str, at: FrameAt, rgba: &[f32]) -> Result<(), PutRefused> {
        self.put_as(namespace, at, rgba, true, false)
    }

    fn put_as(&mut self, namespace: &str, at: FrameAt, rgba: &[f32], ten_bits: bool, evict: bool) -> Result<(), PutRefused> {
        if !valid_namespace(namespace) {
            return Err(PutRefused::Namespace(namespace.to_owned()));
        }
        let (w, h) = at.size;
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || rgba.len() != (w as usize) * (h as usize) * 4 {
            return Err(PutRefused::Io(format!("{} values for a {w}x{h} frame", rgba.len())));
        }
        let (format, payload) = if ten_bits && fits_ten_bits(rgba) {
            let mut words = Vec::with_capacity(rgba.len());
            for p in rgba.chunks_exact(4) {
                words.extend_from_slice(&(encode10(p[0]) | encode10(p[1]) << 10 | encode10(p[2]) << 20).to_le_bytes());
            }
            (FORMAT_RGB10, words)
        } else {
            let mut halves = Vec::with_capacity(rgba.len() * 2);
            for v in rgba {
                halves.extend_from_slice(&half::f16::from_f32(*v).to_bits().to_le_bytes());
            }
            (FORMAT_RGBA_F16, halves)
        };
        let packed = compress_chunks(&payload);
        let format = format | CHUNKED;
        let needed = (HEADER + packed.len()) as u64;
        if !self.room_for(namespace, needed) || (!evict && self.total_bytes() + needed > self.cap_bytes) {
            return Err(PutRefused::Full);
        }
        let path = self.path(namespace, at);
        let dir = path.parent().expect("a frame is in its namespace's folder");
        // Never written through a link: a folder of a shared project's
        // cache that leads elsewhere would put the frame there.
        if linked(&self.root, dir) {
            return Err(PutRefused::Io(format!("{} leads out of the cache through a link", dir.display())));
        }
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            let tmp = path.with_extension("klf.tmp");
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(MAGIC)?;
            f.write_all(&format.to_le_bytes())?;
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
        if !valid_namespace(namespace) {
            return None;
        }
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

    /// Remove `namespace`'s frames whose tick is in `ticks`, at every key
    /// and size (a cleared range): how many went.
    pub fn remove_ticks(&mut self, namespace: &str, ticks: std::ops::Range<i64>) -> usize {
        let paths: Vec<PathBuf> = self.entries.iter().filter(|(_, e)| e.namespace == namespace && ticks.contains(&e.at.tick)).map(|((_, p), _)| p.clone()).collect();
        for p in &paths {
            let _ = std::fs::remove_file(p);
            self.forget(p);
        }
        paths.len()
    }

    /// Remove every namespace under `prefix` (`freeze/`: every frozen
    /// track's frames) and its frames.
    pub fn remove_prefix(&mut self, prefix: &str) {
        let names: std::collections::BTreeSet<String> = self.entries.values().filter(|e| e.namespace.starts_with(prefix)).map(|e| e.namespace.clone()).collect();
        for n in names {
            self.remove_namespace(&n);
        }
    }

    /// Remove `namespace` and its frames (a cleared freeze).
    pub fn remove_namespace(&mut self, namespace: &str) {
        if !valid_namespace(namespace) {
            tracing::warn!("frame cache: {namespace:?} isn't a namespace; nothing removed");
            return;
        }
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

/// `namespace`'s frame `at` under `root`, read without a store's index:
/// what playback reads while the app's store writes (#3207). `None` when
/// it isn't there, the namespace isn't one, or the file doesn't read as a
/// frame of that size: drawn live then.
pub fn read_stored(root: &Path, namespace: &str, at: FrameAt) -> Option<Vec<f32>> {
    read_stored_timed(root, namespace, at).map(|(pixels, _)| pixels)
}

/// A frame as read for the screen: a ten-bit frame's words as stored, for
/// [`ten_bit_display`]; any other unpacked to linear light.
pub enum Stored {
    TenBit(Vec<u8>),
    Linear(Vec<f32>),
}

/// [`read_stored_timed`], a ten-bit frame left as its words: what the
/// frame reader encodes for the screen through a table (#3212).
pub fn read_stored_for_screen(root: &Path, namespace: &str, at: FrameAt) -> Option<(Stored, ReadTimes)> {
    if !valid_namespace(namespace) {
        return None;
    }
    let ms = |t: std::time::Instant| t.elapsed().as_secs_f32() * 1000.0;
    let t = std::time::Instant::now();
    let data = std::fs::read(root.join(namespace).join(at.file_name())).ok()?;
    let read_ms = ms(t);
    let t = std::time::Instant::now();
    let (format, raw) = parse_raw(&data, at.size)?;
    let lz4_ms = ms(t);
    if format == FORMAT_RGB10 {
        return Some((Stored::TenBit(raw), ReadTimes { read_ms, lz4_ms, unpack_ms: 0.0 }));
    }
    let t = std::time::Instant::now();
    let pixels = unpack(format, &raw);
    Some((Stored::Linear(pixels), ReadTimes { read_ms, lz4_ms, unpack_ms: ms(t) }))
}

/// A ten-bit frame's words straight to display bytes: each code's display
/// value (`display`, of its linear light) from a table of the 1024, then
/// dithered and quantised as the display encode does it, so the bytes are
/// the encode's own ([`klipp_color::display::per_channel_display`]).
pub fn ten_bit_display(words: &[u8], display: impl Fn(f32) -> f32) -> Vec<u8> {
    use klipp_color::display::{dither_at, quantise};
    use rayon::prelude::*;
    let table: Vec<f32> = (0..1024).map(|c| display(decode10(c))).collect();
    let mut out = vec![0u8; words.len()];
    out.par_chunks_exact_mut(4).zip(words.par_chunks_exact(4)).enumerate().for_each(|(i, (o, b))| {
        let w = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let n = dither_at(i);
        o[0] = quantise(table[(w & 1023) as usize], n, 255.0) as u8;
        o[1] = quantise(table[((w >> 10) & 1023) as usize], n, 255.0) as u8;
        o[2] = quantise(table[((w >> 20) & 1023) as usize], n, 255.0) as u8;
        o[3] = 255;
    });
    out
}

/// [`read_stored`], with what each part of the read took.
pub fn read_stored_timed(root: &Path, namespace: &str, at: FrameAt) -> Option<(Vec<f32>, ReadTimes)> {
    if !valid_namespace(namespace) {
        return None;
    }
    read_frame_timed(&root.join(namespace).join(at.file_name()), at.size)
}

/// Whether `namespace`'s frame `at` is stored under `root`, by its file
/// alone, unread: for deciding what playback need not decode.
pub fn is_stored(root: &Path, namespace: &str, at: FrameAt) -> bool {
    valid_namespace(namespace) && root.join(namespace).join(at.file_name()).is_file()
}

/// [`read_stored`] as the frame's half floats, two to a word as the GPU
/// stack takes them (red and green, then blue and alpha, the low half
/// first): read and decompressed, nothing widened.
pub fn read_stored_packed(root: &Path, namespace: &str, at: FrameAt) -> Option<Vec<u32>> {
    if !valid_namespace(namespace) {
        return None;
    }
    let (format, raw) = read_raw(&root.join(namespace).join(at.file_name()), at.size)?;
    (format == FORMAT_RGBA_F16).then(|| raw.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

fn read_frame(path: &Path, size: (u32, u32)) -> Option<Vec<f32>> {
    read_frame_timed(path, size).map(|(pixels, _)| pixels)
}

/// What reading a frame took, ms: the file, LZ4, and the unpack to
/// linear light (#3212).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReadTimes {
    pub read_ms: f32,
    pub lz4_ms: f32,
    pub unpack_ms: f32,
}

fn read_frame_timed(path: &Path, size: (u32, u32)) -> Option<(Vec<f32>, ReadTimes)> {
    let ms = |t: std::time::Instant| t.elapsed().as_secs_f32() * 1000.0;
    let t = std::time::Instant::now();
    let data = std::fs::read(path).ok()?;
    let read_ms = ms(t);
    let t = std::time::Instant::now();
    let (format, raw) = parse_raw(&data, size)?;
    let lz4_ms = ms(t);
    let t = std::time::Instant::now();
    let pixels = unpack(format, &raw);
    Some((pixels, ReadTimes { read_ms, lz4_ms, unpack_ms: ms(t) }))
}

/// A ten-bit code's linear light, for each of the 1024: the unpack reads
/// a table rather than raising a power three times a pixel, which at 4K
/// took 160 ms a frame.
static DECODE10: std::sync::LazyLock<Vec<f32>> = std::sync::LazyLock::new(|| (0..1024).map(decode10).collect());

/// A ten-bit frame's words, to linear RGBA.
pub fn unpack_ten_bit(words: &[u8]) -> Vec<f32> {
    unpack(FORMAT_RGB10, words)
}

/// Pixels as stored, to straight-alpha linear RGBA, in parallel.
fn unpack(format: u32, raw: &[u8]) -> Vec<f32> {
    use rayon::prelude::*;
    match format {
        FORMAT_RGB10 => {
            let lut = &*DECODE10;
            let mut out = vec![0.0f32; raw.len()];
            out.par_chunks_exact_mut(4).zip(raw.par_chunks_exact(4)).for_each(|(o, b)| {
                let w = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                o[0] = lut[(w & 1023) as usize];
                o[1] = lut[((w >> 10) & 1023) as usize];
                o[2] = lut[((w >> 20) & 1023) as usize];
                o[3] = 1.0;
            });
            out
        }
        _ => {
            let mut out = vec![0.0f32; raw.len() / 2];
            out.par_iter_mut().zip(raw.par_chunks_exact(2)).for_each(|(o, b)| *o = half::f16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32());
            out
        }
    }
}

/// A frame file's format and pixels, decompressed: little-endian half
/// floats, or ten-bit words. `None` when the file isn't a frame of `size`
/// in a format known (checked before anything is allocated for it).
fn read_raw(path: &Path, size: (u32, u32)) -> Option<(u32, Vec<u8>)> {
    parse_raw(&std::fs::read(path).ok()?, size)
}

/// The byte ranges of `len` bytes cut into `n` chunks, a pixel's eight
/// bytes never split.
fn chunk_ranges(len: usize, n: usize) -> Vec<std::ops::Range<usize>> {
    let per = len.div_ceil(n.max(1)).div_ceil(8) * 8;
    (0..n).map(|i| (i * per).min(len)..((i + 1) * per).min(len)).collect()
}

/// `payload` as [`CHUNKS`] LZ4 blocks side by side: their count, each
/// one's compressed length, then the blocks.
fn compress_chunks(payload: &[u8]) -> Vec<u8> {
    use rayon::prelude::*;
    let blocks: Vec<Vec<u8>> = chunk_ranges(payload.len(), CHUNKS).into_par_iter().map(|r| lz4_flex::block::compress(&payload[r])).collect();
    let mut out = Vec::with_capacity(4 + 4 * blocks.len() + blocks.iter().map(Vec::len).sum::<usize>());
    out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
    for b in &blocks {
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
    }
    for b in &blocks {
        out.extend_from_slice(b);
    }
    out
}

/// [`compress_chunks`]'s blocks back into `expected` bytes, side by side;
/// `None` for a count, a length or a block that doesn't add up, checked
/// before anything is decompressed.
fn decompress_chunks(data: &[u8], expected: usize) -> Option<Vec<u8>> {
    use rayon::prelude::*;
    let n = u32::from_le_bytes(data.get(..4)?.try_into().ok()?) as usize;
    if n == 0 || n > MAX_CHUNKS {
        return None;
    }
    let lengths: Vec<usize> = (0..n).map(|i| data.get(4 + 4 * i..8 + 4 * i).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)).collect::<Option<_>>()?;
    let body = data.get(4 + 4 * n..)?;
    if lengths.iter().try_fold(0usize, |a, l| a.checked_add(*l))? != body.len() {
        return None;
    }
    let mut out = vec![0u8; expected];
    let ranges = chunk_ranges(expected, n);
    let mut blocks = Vec::with_capacity(n);
    let mut at = 0;
    for l in &lengths {
        blocks.push(&body[at..at + l]);
        at += l;
    }
    let mut slices: Vec<&mut [u8]> = Vec::with_capacity(n);
    let mut rest = out.as_mut_slice();
    for r in &ranges {
        let (head, tail) = rest.split_at_mut(r.len());
        slices.push(head);
        rest = tail;
    }
    let whole = slices.into_par_iter().zip(blocks.into_par_iter()).all(|(dst, block)| lz4_flex::block::decompress_into(block, dst).is_ok_and(|got| got == dst.len()));
    whole.then_some(out)
}

fn parse_raw(data: &[u8], size: (u32, u32)) -> Option<(u32, Vec<u8>)> {
    if data.len() < HEADER || &data[..4] != MAGIC {
        return None;
    }
    let word = |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
    let (format, w, h) = (word(4), word(8), word(12));
    let chunked = format & CHUNKED != 0;
    let format = format & !CHUNKED;
    let bytes_a_pixel = match format {
        FORMAT_RGBA_F16 => 8,
        FORMAT_RGB10 => 4,
        _ => return None,
    };
    if (w, h) != size || w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return None;
    }
    let expected = (w as usize) * (h as usize) * bytes_a_pixel;
    if chunked {
        return decompress_chunks(&data[HEADER..], expected).map(|raw| (format, raw));
    }
    // The prepended size must match the frame, before anything is
    // allocated for it.
    let claimed = u32::from_le_bytes(data.get(HEADER..HEADER + 4)?.try_into().ok()?) as usize;
    if claimed != expected {
        return None;
    }
    let raw = lz4_flex::decompress_size_prepended(&data[HEADER..]).ok()?;
    (raw.len() == expected).then_some((format, raw))
}

static SHARED: std::sync::LazyLock<std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<std::sync::Mutex<CacheStore>>>>> = std::sync::LazyLock::new(Default::default);

/// The one store at `root` in this process (#3212): the app's frozen
/// tracks and the engine's frame cache write into the same store, so one
/// index counts both against one cap. Opened on first asking, with
/// `cap_bytes` as its cap then; [`CacheStore::set_cap`] changes it after.
pub fn shared_at(root: &Path, cap_bytes: u64) -> std::sync::Arc<std::sync::Mutex<CacheStore>> {
    let mut all = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    all.entry(root.to_path_buf()).or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(CacheStore::open(root, cap_bytes)))).clone()
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

    /// #3212: drawing ahead while idle stops at the cap rather than make
    /// room: the frames it drew first stay. A range asked for lets the
    /// oldest go, as before.
    #[test]
    fn a_frame_within_the_cap_never_evicts_and_a_range_frame_does() {
        let d = dir("within-cap");
        let mut s = CacheStore::open(&d, 1 << 30);
        s.put_frame("frames", at(0, 1), &frame(0.1)).unwrap();
        let one = s.total_bytes();
        s.set_cap(one * 2);
        s.put_frame_within_cap("frames", at(1, 1), &frame(0.2)).unwrap();
        assert_eq!(s.put_frame_within_cap("frames", at(2, 1), &frame(0.3)), Err(PutRefused::Full), "full: refused");
        assert!(s.contains("frames", at(0, 1)) && s.contains("frames", at(1, 1)), "nothing let go for it");
        s.put_frame("frames", at(2, 1), &frame(0.3)).unwrap();
        assert!(!s.contains("frames", at(0, 1)) && s.contains("frames", at(2, 1)), "a range frame lets the oldest go");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #3212: a whole opaque frame keeps in ten bits a channel, smaller
    /// than half floats and within a display level; one with alpha, or a
    /// value past the curve, keeps as half floats, exactly.
    #[test]
    fn a_whole_opaque_frame_keeps_in_ten_bits_within_a_level() {
        let d = dir("ten-bits");
        let mut s = CacheStore::open(&d, 1 << 30);
        let (w, h) = (64u32, 32u32);
        let opaque: Vec<f32> = (0..w * h).flat_map(|i| [(i % 97) as f32 / 96.0, (i % 13) as f32 / 12.0 * 0.2, 1.5 * (i % 5) as f32 / 4.0, 1.0]).collect();
        let at = FrameAt { tick: 1, key: 9, size: (w, h) };
        s.put_frame("frames", at, &opaque).unwrap();
        let ten = s.bytes("frames");
        let back = s.get("frames", at).expect("stored");
        let display = |c: f32| (if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }.clamp(0.0, 1.0) * 255.0).round() as i32;
        let worst = opaque.iter().zip(&back).map(|(a, b)| (display(*a) - display(*b)).abs()).max().unwrap();
        assert!(worst <= 1, "{worst} levels");
        assert!(back.chunks_exact(4).all(|p| p[3] == 1.0), "opaque back");
        // The same frame as half floats is larger.
        s.put("frames", FrameAt { key: 10, ..at }, &opaque).unwrap();
        assert!(s.bytes("frames") - ten > ten, "ten bits {ten} vs half floats {}", s.bytes("frames") - ten);
        // Alpha, or past 16: half floats, exactly as half floats keep them.
        let mut with_alpha = opaque.clone();
        with_alpha[3] = 0.5;
        let mut hot = opaque.clone();
        hot[0] = 40.0;
        for (k, pixels) in [(11u64, with_alpha), (12, hot)] {
            let at = FrameAt { key: k, ..at };
            s.put_frame("frames", at, &pixels).unwrap();
            let back = s.get("frames", at).unwrap();
            assert!(pixels.iter().zip(&back).all(|(a, b)| (half::f16::from_f32(*a).to_f32() - b).abs() < 1e-6), "key {k}");
        }
        // A frozen layer's packed reader takes half floats only.
        assert!(read_stored_packed(&d, "frames", at).is_none());
        assert!(read_stored_packed(&d, "frames", FrameAt { key: 11, ..at }).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A ten-bit file whose size it claims isn't its frame's is refused
    /// before anything is allocated for it.
    #[test]
    fn a_ten_bit_file_claiming_another_size_is_refused() {
        let d = dir("ten-bits-hostile");
        let at = FrameAt { tick: 1, key: 1, size: (4, 2) };
        let path = d.join("frames").join(at.file_name());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = MAGIC.to_vec();
        for word in [FORMAT_RGB10, 4, 2] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&lz4_flex::compress_prepend_size(&vec![0u8; 1 << 20]));
        std::fs::write(&path, bytes).unwrap();
        assert!(read_stored(&d, "frames", at).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// One store a folder in a process.
    #[test]
    fn a_folder_has_one_store() {
        let d = dir("shared");
        let a = shared_at(&d, 1 << 20);
        let b = shared_at(&d, 1 << 30);
        assert!(std::sync::Arc::ptr_eq(&a, &b));
        assert_eq!(a.lock().unwrap().cap(), 1 << 20, "the cap it was opened with, until set");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A namespace folder that is a link to somewhere else is never
    /// written through: refused, and nothing lands there.
    #[test]
    fn a_frame_is_never_written_through_a_link() {
        let d = dir("link");
        let outside = dir("link-outside");
        std::fs::create_dir_all(d.join("freeze")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, d.join("freeze").join("t1")).unwrap();
        let mut s = CacheStore::open(&d, 1 << 30);
        assert!(matches!(s.put("freeze/t1", at(5, 0xa), &frame(0.1)), Err(PutRefused::Io(_))));
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing landed outside");
        s.put("freeze/t2", at(5, 0xa), &frame(0.1)).expect("a real folder is written");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&outside);
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

    /// A namespace is a plain name under the root, or refused: an empty
    /// one, `..`, an absolute path or one that climbs out stores, finds
    /// and removes nothing, and deletes nothing outside its own folder.
    #[test]
    fn a_namespace_that_could_reach_outside_the_root_is_refused() {
        let d = dir("namespace");
        let outside = d.with_file_name(format!("klipp-cache-store-outside-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"kept").unwrap();
        let mut s = CacheStore::open(&d, 1 << 30);
        s.put("freeze/t1", at(1, 1), &frame(0.1)).unwrap();
        let outside_name = outside.file_name().unwrap().to_string_lossy().into_owned();
        let climbs = format!("../{outside_name}");
        for ns in ["", "..", ".", "/tmp", "freeze/../..", "freeze/", "/", "a//b", "freeze/./t1", climbs.as_str(), outside.to_str().unwrap()] {
            assert!(!valid_namespace(ns), "{ns:?} refused");
            assert_eq!(s.put(ns, at(2, 1), &frame(0.2)), Err(PutRefused::Namespace(ns.to_owned())), "{ns:?}: not stored");
            assert!(s.get(ns, at(1, 1)).is_none() && !s.contains(ns, at(1, 1)), "{ns:?}: nothing found");
            s.remove_namespace(ns);
            assert!(d.exists() && s.contains("freeze/t1", at(1, 1)), "{ns:?}: the root and its frames kept");
            assert!(outside.join("keep").exists(), "{ns:?}: nothing outside the root deleted");
        }
        assert!(valid_namespace("freeze/0a1b-c2_d3") && valid_namespace("frames"));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&outside);
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

    /// #3212: a ten-bit frame encoded for the screen through the table is
    /// byte for byte what the display encode makes of its linear light.
    #[test]
    fn a_ten_bit_frame_through_the_table_is_the_display_encodes_bytes() {
        use klipp_core::{SdrTransfer, ToneMapOp, WorkingColorspace};
        let d = dir("ten-bit-screen");
        let mut s = CacheStore::open(&d, 1 << 30);
        let pixels: Vec<f32> = (0..64 * 32).flat_map(|i| [(i % 97) as f32 / 60.0, (i / 64) as f32 / 31.0, 0.003 * (i % 7) as f32, 1.0]).collect();
        let a = FrameAt { tick: 1, key: 1, size: (64, 32) };
        s.put_frame("frames", a, &pixels).unwrap();
        let Some((Stored::TenBit(words), _)) = read_stored_for_screen(&d, "frames", a) else { panic!("kept in ten bits") };
        for tone in [ToneMapOp::None, ToneMapOp::Hable] {
            let display = klipp_color::display::per_channel_display(tone, WorkingColorspace::Rec709Linear, SdrTransfer::Srgb).unwrap();
            let want = klipp_color::display::display_transform_ws_dithered_to(&unpack_ten_bit(&words), tone, WorkingColorspace::Rec709Linear, SdrTransfer::Srgb);
            assert_eq!(ten_bit_display(&words, display), want, "{tone:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #3212: a frame compressed in chunks reads back whole; one written
    /// as a single block before them still reads; and a chunked file whose
    /// count, lengths or blocks don't add up is refused and removed.
    #[test]
    fn chunked_frames_read_back_and_a_bad_chunk_table_is_refused() {
        let d = dir("chunks");
        let mut s = CacheStore::open(&d, 1 << 30);
        // Big enough for every chunk to hold pixels.
        let big = |seed: f32| -> Vec<f32> { (0..64 * 32).flat_map(|i| [seed + (i % 97) as f32 / 200.0, 0.5, (i / 64) as f32 / 40.0, 1.0]).collect() };
        let a = FrameAt { tick: 1, key: 1, size: (64, 32) };
        s.put_frame("frames", a, &big(0.1)).unwrap();
        let path = d.join("frames").join(a.file_name());
        let good = std::fs::read(&path).unwrap();
        assert_eq!(u32::from_le_bytes(good[4..8].try_into().unwrap()) & CHUNKED, CHUNKED, "written in chunks");
        let back = s.get("frames", a).unwrap();
        assert!(back.iter().zip(big(0.1)).all(|(x, y)| (x - y).abs() < 2e-3), "read back whole");

        // A single block, as files were written before chunks.
        let (format, raw) = parse_raw(&good, a.size).unwrap();
        let mut old = good[..HEADER].to_vec();
        old[4..8].copy_from_slice(&format.to_le_bytes());
        old.extend_from_slice(&lz4_flex::compress_prepend_size(&raw));
        let o = FrameAt { tick: 2, ..a };
        std::fs::write(d.join("frames").join(o.file_name()), &old).unwrap();
        let mut s = CacheStore::open(&d, 1 << 30);
        assert_eq!(s.get("frames", o), Some(back.clone()), "a single block still reads");

        let n = u32::from_le_bytes(good[HEADER..HEADER + 4].try_into().unwrap()) as usize;
        let table = HEADER + 4;
        let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
        let mut none = good.clone();
        none[HEADER..HEADER + 4].copy_from_slice(&0u32.to_le_bytes());
        cases.push(("no chunks", none));
        let mut many = good.clone();
        many[HEADER..HEADER + 4].copy_from_slice(&(MAX_CHUNKS as u32 + 1).to_le_bytes());
        cases.push(("more chunks than allowed", many));
        let mut long = good.clone();
        let first = u32::from_le_bytes(long[table..table + 4].try_into().unwrap());
        long[table..table + 4].copy_from_slice(&(first + 1).to_le_bytes());
        cases.push(("lengths that don't add up to the body", long));
        let mut swapped = good.clone();
        let second = u32::from_le_bytes(swapped[table + 4..table + 8].try_into().unwrap());
        if n > 1 && first != second {
            swapped[table..table + 4].copy_from_slice(&second.to_le_bytes());
            swapped[table + 4..table + 8].copy_from_slice(&first.to_le_bytes());
            cases.push(("blocks cut in the wrong places", swapped));
        }
        let mut huge = good.clone();
        huge[table..table + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        cases.push(("a block that claims 4 GB", huge));
        // Whole, well-made chunk tables only the checks themselves refuse:
        // more chunks than allowed, and bytes after the last block.
        let chunks_of = |n: usize, trailing: &[u8]| {
            let blocks: Vec<Vec<u8>> = chunk_ranges(raw.len(), n).into_iter().map(|r| lz4_flex::block::compress(&raw[r])).collect();
            let mut f = good[..HEADER].to_vec();
            f.extend_from_slice(&(n as u32).to_le_bytes());
            for b in &blocks {
                f.extend_from_slice(&(b.len() as u32).to_le_bytes());
            }
            for b in &blocks {
                f.extend_from_slice(b);
            }
            f.extend_from_slice(trailing);
            f
        };
        assert!(parse_raw(&chunks_of(MAX_CHUNKS, &[]), a.size).is_some(), "as many chunks as allowed: read");
        cases.push(("one chunk more than allowed, each whole", chunks_of(MAX_CHUNKS + 1, &[])));
        cases.push(("bytes after the last block", chunks_of(CHUNKS, b"extra")));
        for (i, (name, bytes)) in cases.into_iter().enumerate() {
            let b = FrameAt { tick: 10 + i as i64, ..a };
            let path = d.join("frames").join(b.file_name());
            std::fs::write(&path, bytes).unwrap();
            let mut s = CacheStore::open(&d, 1 << 30);
            assert!(s.get("frames", b).is_none(), "{name}: refused");
            assert!(!path.exists(), "{name}: removed");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
