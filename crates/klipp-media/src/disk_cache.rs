//! Disk-based caching for filmstrip thumbnails and poster frames.
//!
//! Cache key: FNV-1a hash of (canonical_path, file_size, mtime_ns).
//! Invalidated automatically when the source file is modified, moved, or resized.

use crate::FilmstripFrame;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

// ── Reading a cache file back ──
//
// A cache file is trusted for nothing: a damaged one (a crash mid-write,
// a full disk, a stray edit) can say it holds any count of anything. Every
// count and size a header gives is checked against the file's length, with
// checked arithmetic, before anything is allocated for it (#823); a file
// that fails is an error, and the callers then rebuild it as if missing.

/// The largest edge a cached picture may claim. Far above any real
/// thumbnail, filmstrip frame or frame, far below what overflows.
pub const MAX_CACHED_EDGE: u32 = 16_384;

/// The byte length of a `width` × `height` RGBA picture, when both edges
/// are within [`MAX_CACHED_EDGE`].
pub fn cached_rgba_len(width: u32, height: u32) -> Option<usize> {
    if width > MAX_CACHED_EDGE || height > MAX_CACHED_EDGE {
        return None;
    }
    (width as usize).checked_mul(height as usize)?.checked_mul(4)
}

fn invalid(what: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what)
}

/// Fails unless `header + count × each` is exactly the file's `len`.
fn expect_len(len: u64, header: u64, count: u64, each: u64, what: &str) -> std::io::Result<()> {
    let want = count.checked_mul(each).and_then(|b| b.checked_add(header));
    if want == Some(len) {
        Ok(())
    } else {
        Err(invalid(format!("{what}: the header asks for {count} entries of {each} bytes, the file is {len} bytes")))
    }
}

// ── Cache key ──

/// Deterministic FNV-1a hash (stable across Rust versions, unlike DefaultHasher).
fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Compute a cache key from the file's canonical path, size, and modification time.
/// Returns `None` if the file is inaccessible.
pub fn compute_cache_key(path: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(path).ok()?;
    let meta = std::fs::metadata(&canonical).ok()?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();

    let mut buf = Vec::with_capacity(256);
    buf.extend_from_slice(canonical.to_string_lossy().as_bytes());
    buf.push(0);
    buf.extend_from_slice(&size.to_le_bytes());
    buf.push(0);
    buf.extend_from_slice(&mtime.to_le_bytes());

    Some(format!("{:016x}", fnv1a(&buf)))
}

/// Override paths for cache directories (set from preferences).
static FILMSTRIP_DIR_OVERRIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
static AUDIO_CACHE_DIR_OVERRIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Set cache directory overrides from preferences.
pub fn set_cache_dir_overrides(filmstrip: Option<PathBuf>, audio: Option<PathBuf>) {
    *FILMSTRIP_DIR_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = filmstrip;
    *AUDIO_CACHE_DIR_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = audio;
}

/// Where analysis results live, by media cache key: `<cache>/analysis/`.
/// `klipp_analysis::store` writes there; a collected project carries a
/// file's folder and brings it back on open (#3241).
pub fn analysis_dir() -> PathBuf {
    cache_base_dir().join("analysis")
}

/// Base cache directory: `~/.cache/klipp/`.
pub fn cache_base_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("klipp")
}

fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

// ── Filmstrip cache ──
//
// Format: single binary blob per media.
//   magic:       [u8; 4] = b"KLFS"
//   version:     u32
//   frame_count: u32
//   width:       u32
//   height:      u32
//   For each frame:
//     time_secs: f64
//   Raw RGBA data for all frames concatenated (width * height * 4 bytes each)

const FILMSTRIP_MAGIC: [u8; 4] = *b"KLFS";
const FILMSTRIP_VERSION: u32 = 1;

/// Version of the colour pipeline that produced cached thumbnails and
/// filmstrips, part of their filenames. Bump it when a change alters how
/// frames become pictures, or every cached picture keeps the old look.
/// c2: scene-referred sources (PQ, HLG, camera log) are converted and tone
/// mapped as in the viewer; before, they were plain swscale output — log
/// flat, PQ washed out.
const PICTURE_PIPELINE: &str = "c2";

fn filmstrip_dir() -> PathBuf {
    FILMSTRIP_DIR_OVERRIDE.lock().ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| cache_base_dir().join("filmstrips"))
}

fn filmstrip_path(cache_key: &str, height: u32) -> PathBuf {
    // Height is part of the filename so filmstrips at different tile
    // resolutions coexist in the cache. Without this, bumping the
    // filmstrip_height pref would silently reuse the old low-res cache
    // (content hash is identical for the same source file) and changes
    // wouldn't take effect until the user manually cleared the cache.
    filmstrip_dir().join(format!("{cache_key}.{height}.{PICTURE_PIPELINE}.strip"))
}

/// Delete every cached filmstrip for `cache_key`, at any height and
/// pipeline version. Returns how many files went.
pub fn remove_filmstrip_cache(cache_key: &str) -> usize {
    let prefix = format!("{cache_key}.");
    let Ok(entries) = std::fs::read_dir(filmstrip_dir()) else { return 0 };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with(&prefix) && name.ends_with(".strip")
        })
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

pub fn filmstrip_cache_exists(cache_key: &str, height: u32) -> bool {
    filmstrip_path(cache_key, height).exists()
}

pub fn write_filmstrip_cache(
    cache_key: &str,
    frames: &[FilmstripFrame],
) -> std::io::Result<()> {
    if frames.is_empty() {
        return Ok(());
    }
    let dir = filmstrip_dir();
    ensure_dir(&dir)?;

    let height = frames[0].height;
    // Write to .tmp then rename for atomicity
    let final_path = filmstrip_path(cache_key, height);
    let tmp_path = dir.join(format!("{cache_key}.{height}.strip.tmp"));

    let file = std::fs::File::create(&tmp_path)?;
    let mut w = BufWriter::new(file);

    let width = frames[0].width;
    let height = frames[0].height;
    let frame_count = frames.len() as u32;

    w.write_all(&FILMSTRIP_MAGIC)?;
    w.write_all(&FILMSTRIP_VERSION.to_le_bytes())?;
    w.write_all(&frame_count.to_le_bytes())?;
    w.write_all(&width.to_le_bytes())?;
    w.write_all(&height.to_le_bytes())?;

    // Frame time index
    for frame in frames {
        w.write_all(&frame.time_secs.to_le_bytes())?;
    }

    // RGBA data
    let expected_size = cached_rgba_len(width, height).ok_or_else(|| invalid(format!("filmstrip frame {width}x{height} is too large to cache")))?;
    for frame in frames {
        if frame.rgba.len() == expected_size {
            w.write_all(&frame.rgba)?;
        } else {
            // Pad or truncate to expected size
            let mut data = frame.rgba.clone();
            data.resize(expected_size, 0);
            w.write_all(&data)?;
        }
    }

    w.flush()?;
    drop(w);
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

pub fn load_filmstrip_cache(cache_key: &str, height: u32) -> std::io::Result<Vec<FilmstripFrame>> {
    let file = std::fs::File::open(filmstrip_path(cache_key, height))?;
    let len = file.metadata()?.len();
    read_filmstrip(BufReader::new(file), len)
}

/// A filmstrip cache file of `len` bytes, from `r`.
fn read_filmstrip(mut r: impl Read, len: u64) -> std::io::Result<Vec<FilmstripFrame>> {

    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if magic != FILMSTRIP_MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad filmstrip magic",
        ));
    }

    let mut buf4 = [0u8; 4];
    r.read_exact(&mut buf4)?;
    let version = u32::from_le_bytes(buf4);
    if version != FILMSTRIP_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unsupported filmstrip version",
        ));
    }

    r.read_exact(&mut buf4)?;
    let frame_count = u32::from_le_bytes(buf4) as usize;
    r.read_exact(&mut buf4)?;
    let width = u32::from_le_bytes(buf4);
    r.read_exact(&mut buf4)?;
    let height = u32::from_le_bytes(buf4);
    let frame_size = cached_rgba_len(width, height).ok_or_else(|| invalid(format!("filmstrip frames of {width}x{height}")))?;
    // Magic, version, count, width, height; then a time and a picture per frame.
    expect_len(len, 20, frame_count as u64, 8 + frame_size as u64, "filmstrip")?;

    // Read time index
    let mut times = Vec::with_capacity(frame_count);
    let mut buf8 = [0u8; 8];
    for _ in 0..frame_count {
        r.read_exact(&mut buf8)?;
        times.push(f64::from_le_bytes(buf8));
    }

    // Read RGBA data
    let mut frames = Vec::with_capacity(frame_count);
    for time_secs in times {
        let mut rgba = vec![0u8; frame_size];
        r.read_exact(&mut rgba)?;
        frames.push(FilmstripFrame {
            time_secs,
            rgba,
            width,
            height,
        });
    }

    Ok(frames)
}

// ── Thumbnail cache ──
//
// Format: width (u32) + height (u32) + raw RGBA data.
//
// The target max-edge width lives in the filename (mirroring the filmstrip
// cache) so thumbnails at different resolutions coexist. Without that, the
// media-pool and files-browser used to clobber each other's caches and the
// first caller-wins resolution would silently apply to the other view.

fn thumbnail_dir() -> PathBuf {
    cache_base_dir().join("thumbnails")
}

fn thumbnail_path(cache_key: &str, max_width: u32) -> PathBuf {
    thumbnail_dir().join(format!("{cache_key}.{max_width}.{PICTURE_PIPELINE}.thumb"))
}

pub fn thumbnail_cache_exists(cache_key: &str, max_width: u32) -> bool {
    thumbnail_path(cache_key, max_width).exists()
}

pub fn write_thumbnail_cache(
    cache_key: &str,
    max_width: u32,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> std::io::Result<()> {
    let dir = thumbnail_dir();
    ensure_dir(&dir)?;

    let final_path = thumbnail_path(cache_key, max_width);
    let tmp_path = dir.join(format!("{cache_key}.{max_width}.thumb.tmp"));

    let file = std::fs::File::create(&tmp_path)?;
    let mut w = BufWriter::new(file);
    w.write_all(&width.to_le_bytes())?;
    w.write_all(&height.to_le_bytes())?;
    w.write_all(rgba)?;
    w.flush()?;
    drop(w);
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

pub fn load_thumbnail_cache(cache_key: &str, max_width: u32) -> std::io::Result<(Vec<u8>, u32, u32)> {
    let file = std::fs::File::open(thumbnail_path(cache_key, max_width))?;
    let len = file.metadata()?.len();
    read_thumbnail(BufReader::new(file), len)
}

/// A thumbnail cache file of `len` bytes, from `r`.
fn read_thumbnail(mut r: impl Read, len: u64) -> std::io::Result<(Vec<u8>, u32, u32)> {

    let mut buf4 = [0u8; 4];
    r.read_exact(&mut buf4)?;
    let width = u32::from_le_bytes(buf4);
    r.read_exact(&mut buf4)?;
    let height = u32::from_le_bytes(buf4);

    let expected = cached_rgba_len(width, height).ok_or_else(|| invalid(format!("thumbnail of {width}x{height}")))?;
    expect_len(len, 8, 1, expected as u64, "thumbnail")?;
    let mut rgba = vec![0u8; expected];
    r.read_exact(&mut rgba)?;

    Ok((rgba, width, height))
}

// ── Audio peaks cache (lightweight waveform summary for thumbnails) ──
//
// Format:
//   magic:  [u8; 4] = b"KAPK"
//   version: u32
//   count:  u32                    (number of (min, max) buckets)
//   data:   (f32, f32) × count     (little-endian, packed)
//
// Distinct from the heavy `audio_conform_cache` which writes full PCM —
// this is just the visual waveform summary used for file-browser audio
// thumbnails. Typical size: ~1.7 KB for 144 buckets.

const AUDIO_PEAKS_MAGIC: [u8; 4] = *b"KAPK";
const AUDIO_PEAKS_VERSION: u32 = 1;

fn audio_peaks_dir() -> PathBuf {
    cache_base_dir().join("audio-peaks")
}

fn audio_peaks_path(cache_key: &str) -> PathBuf {
    audio_peaks_dir().join(format!("{cache_key}.peaks"))
}

pub fn audio_peaks_cache_exists(cache_key: &str) -> bool {
    audio_peaks_path(cache_key).exists()
}

pub fn write_audio_peaks_cache(
    cache_key: &str,
    peaks: &[(f32, f32)],
) -> std::io::Result<()> {
    let dir = audio_peaks_dir();
    ensure_dir(&dir)?;

    let final_path = audio_peaks_path(cache_key);
    let tmp_path = dir.join(format!("{cache_key}.peaks.tmp"));

    let file = std::fs::File::create(&tmp_path)?;
    let mut w = BufWriter::new(file);
    w.write_all(&AUDIO_PEAKS_MAGIC)?;
    w.write_all(&AUDIO_PEAKS_VERSION.to_le_bytes())?;
    w.write_all(&(peaks.len() as u32).to_le_bytes())?;
    for (min, max) in peaks {
        w.write_all(&min.to_le_bytes())?;
        w.write_all(&max.to_le_bytes())?;
    }
    w.flush()?;
    drop(w);
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

pub fn load_audio_peaks_cache(cache_key: &str) -> std::io::Result<Vec<(f32, f32)>> {
    let file = std::fs::File::open(audio_peaks_path(cache_key))?;
    let len = file.metadata()?.len();
    read_audio_peaks(BufReader::new(file), len)
}

/// An audio peaks cache file of `len` bytes, from `r`.
fn read_audio_peaks(mut r: impl Read, len: u64) -> std::io::Result<Vec<(f32, f32)>> {

    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if magic != AUDIO_PEAKS_MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad audio peaks magic",
        ));
    }

    let mut buf4 = [0u8; 4];
    r.read_exact(&mut buf4)?;
    let version = u32::from_le_bytes(buf4);
    if version != AUDIO_PEAKS_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unsupported audio peaks version",
        ));
    }

    r.read_exact(&mut buf4)?;
    let count = u32::from_le_bytes(buf4) as usize;
    // Magic, version, count; then a (min, max) pair of f32 per bucket.
    expect_len(len, 12, count as u64, 8, "audio peaks")?;

    let mut peaks = Vec::with_capacity(count);
    for _ in 0..count {
        r.read_exact(&mut buf4)?;
        let min = f32::from_le_bytes(buf4);
        r.read_exact(&mut buf4)?;
        let max = f32::from_le_bytes(buf4);
        peaks.push((min, max));
    }
    Ok(peaks)
}

// ── Audio conform cache (content-based keys) ──

fn audio_cache_dir() -> PathBuf {
    AUDIO_CACHE_DIR_OVERRIDE.lock().ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| cache_base_dir().join("audio-cache"))
}

/// Check if audio conform cache exists for a content-based key.
///
/// A cache entry only counts as "existing" when *both* files are non-empty.
/// A prior conform thread crashing after `fs::File::create` (which creates a
/// 0-byte file) but before the write could leave ghost entries on disk —
/// the cache-hit path then loads zero samples and the clip plays silent.
/// Here we treat zero-byte or sub-header-sized files as missing and delete
/// them so the caller regenerates instead of re-using a broken entry.
///
/// Keyed by content only. There used to be a fallback to files named by
/// media id, from before content keys; a media id outlives a relink, so
/// that fallback served the conform of whatever file the id used to
/// point at.
pub fn audio_conform_cache_exists(cache_key: &str) -> (bool, PathBuf, PathBuf) {
    let dir = audio_cache_dir();
    let pcm = dir.join(format!("{cache_key}.pcm"));
    let peaks = dir.join(format!("{cache_key}.peaks"));
    let exists = is_valid_conform_pair(&pcm, &peaks);
    (exists, pcm, peaks)
}

/// Both files must exist AND have a plausible minimum size. A PCM file
/// with fewer than 2 stereo f32 samples (16 bytes) is useless; a peaks
/// file with fewer than one min/max pair (8 bytes) is useless. Files
/// failing the check are unlinked so the caller regenerates cleanly
/// instead of cache-hitting zero-byte ghosts.
fn is_valid_conform_pair(pcm: &Path, peaks: &Path) -> bool {
    const PCM_MIN_BYTES: u64 = 16;   // 2 stereo f32 samples
    const PEAKS_MIN_BYTES: u64 = 8;  // 1 (min,max) f32 pair
    let pcm_ok = std::fs::metadata(pcm)
        .map(|m| m.len() >= PCM_MIN_BYTES)
        .unwrap_or(false);
    let peaks_ok = std::fs::metadata(peaks)
        .map(|m| m.len() >= PEAKS_MIN_BYTES)
        .unwrap_or(false);
    if pcm_ok && peaks_ok {
        return true;
    }
    // At least one file is missing / too small — unlink both so the
    // next conform run starts from a clean slate. Errors are intentionally
    // ignored: if the file doesn't exist, remove_file returns Err; we
    // don't care, the end state we want is "file is gone".
    let _ = std::fs::remove_file(pcm);
    let _ = std::fs::remove_file(peaks);
    false
}

// ── Skim transcode cache (content-based keys) ──

fn skim_dir() -> PathBuf {
    cache_base_dir().join("skim")
}

/// The skim transcode path for a skim cache key (see [`skim_cache_key`]).
/// Content-keyed only: the old media-id fallback outlived relinks and
/// served another file's skim.
pub fn skim_transcode_path(cache_key: &str) -> PathBuf {
    skim_dir().join(format!("{cache_key}.mkv"))
}

/// The cache key a skim transcode is stored under: the file's content
/// key, plus `.hdrtm` when the source is HDR (the skim is tone-mapped to
/// SDR, and older skims that were not must not be reused) and `.r<deg>`
/// when the source is rotated (the skim has upright pixels, and older
/// sideways ones must not be). One function, so the transcode and every
/// place that looks for an existing skim agree; the project loader used
/// to look for the bare content key and pre-loaded exactly those stale
/// HDR and sideways skims the markers exist to skip.
pub fn skim_cache_key(content_key: &str, hdr_source: bool, source_rotation: i32) -> String {
    let mut key = content_key.to_string();
    if hdr_source {
        key.push_str(".hdrtm");
    }
    if source_rotation != 0 {
        key.push_str(&format!(".r{source_rotation}"));
    }
    key
}

#[cfg(test)]
mod skim_key_tests {
    use super::*;

    #[test]
    fn markers_follow_the_source() {
        assert_eq!(skim_cache_key("abc", false, 0), "abc");
        assert_eq!(skim_cache_key("abc", true, 0), "abc.hdrtm");
        assert_eq!(skim_cache_key("abc", false, 90), "abc.r90");
        assert_eq!(skim_cache_key("abc", true, 270), "abc.hdrtm.r270");
    }

    #[test]
    fn a_skim_path_is_the_key_alone() {
        let p = skim_transcode_path("abc.hdrtm");
        assert_eq!(p.file_name().unwrap(), "abc.hdrtm.mkv");
    }
}

#[cfg(test)]
mod damaged_file_tests {
    //! #823: a damaged cache file is refused before anything is allocated
    //! for what its header claims.
    use super::*;

    fn read_all<T>(bytes: &[u8], read: impl Fn(&[u8], u64) -> std::io::Result<T>) -> std::io::Result<T> {
        read(bytes, bytes.len() as u64)
    }

    fn peaks_file(count: u32, pairs: usize) -> Vec<u8> {
        let mut b = AUDIO_PEAKS_MAGIC.to_vec();
        b.extend(AUDIO_PEAKS_VERSION.to_le_bytes());
        b.extend(count.to_le_bytes());
        for i in 0..pairs {
            b.extend((-(i as f32)).to_le_bytes());
            b.extend((i as f32).to_le_bytes());
        }
        b
    }

    #[test]
    fn peaks_read_back_and_a_header_count_past_the_file_is_refused() {
        let peaks = read_all(&peaks_file(3, 3), |b, l| read_audio_peaks(b, l)).unwrap();
        assert_eq!(peaks, vec![(0.0, 0.0), (-1.0, 1.0), (-2.0, 2.0)]);
        // The 12-byte file of the report: a header and nothing else, the
        // count saying four thousand million pairs (about 32 GiB).
        let err = read_all(&peaks_file(u32::MAX, 0), |b, l| read_audio_peaks(b, l)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
        // Truncated mid-pair, and longer than its count says.
        let mut cut = peaks_file(3, 3);
        cut.truncate(cut.len() - 4);
        assert!(read_all(&cut, |b, l| read_audio_peaks(b, l)).is_err());
        assert!(read_all(&peaks_file(2, 3), |b, l| read_audio_peaks(b, l)).is_err());
    }

    fn thumb_file(w: u32, h: u32, bytes: usize) -> Vec<u8> {
        let mut b = w.to_le_bytes().to_vec();
        b.extend(h.to_le_bytes());
        b.extend(std::iter::repeat_n(7u8, bytes));
        b
    }

    #[test]
    fn a_thumbnail_must_be_the_size_its_edges_say() {
        let (rgba, w, h) = read_all(&thumb_file(2, 3, 24), |b, l| read_thumbnail(b, l)).unwrap();
        assert_eq!((rgba.len(), w, h), (24, 2, 3));
        assert!(read_all(&thumb_file(2, 3, 20), |b, l| read_thumbnail(b, l)).is_err());
        // Edges whose product overflows u32 (it panicked in a debug build)
        // and edges past any picture are refused, not multiplied.
        assert!(read_all(&thumb_file(65_536, 65_536, 0), |b, l| read_thumbnail(b, l)).is_err());
        assert!(read_all(&thumb_file(MAX_CACHED_EDGE + 1, 1, 0), |b, l| read_thumbnail(b, l)).is_err());
    }

    fn strip_file(count: u32, w: u32, h: u32, frames: usize) -> Vec<u8> {
        let mut b = FILMSTRIP_MAGIC.to_vec();
        b.extend(FILMSTRIP_VERSION.to_le_bytes());
        b.extend(count.to_le_bytes());
        b.extend(w.to_le_bytes());
        b.extend(h.to_le_bytes());
        for i in 0..frames {
            b.extend((i as f64).to_le_bytes());
        }
        b.extend(std::iter::repeat_n(9u8, frames * w as usize * h as usize * 4));
        b
    }

    #[test]
    fn a_filmstrip_must_hold_the_frames_its_header_counts() {
        let frames = read_all(&strip_file(2, 2, 2, 2), |b, l| read_filmstrip(b, l)).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!((frames[1].time_secs, frames[1].rgba.len()), (1.0, 16));
        // A header claiming millions of frames over a file that holds one.
        assert!(read_all(&strip_file(u32::MAX, 2, 2, 1), |b, l| read_filmstrip(b, l)).is_err());
        // Frames of an edge past any picture.
        assert!(read_all(&strip_file(1, u32::MAX, u32::MAX, 0), |b, l| read_filmstrip(b, l)).is_err());
        let mut cut = strip_file(2, 2, 2, 2);
        cut.pop();
        assert!(read_all(&cut, |b, l| read_filmstrip(b, l)).is_err());
    }
}
