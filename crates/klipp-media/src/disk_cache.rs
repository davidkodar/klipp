//! Disk-based caching for filmstrip thumbnails and poster frames.
//!
//! Cache key: FNV-1a hash of (canonical_path, file_size, mtime_ns).
//! Invalidated automatically when the source file is modified, moved, or resized.

use crate::FilmstripFrame;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

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
    filmstrip_dir().join(format!("{cache_key}.{height}.strip"))
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
    let expected_size = (width * height * 4) as usize;
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
    let path = filmstrip_path(cache_key, height);
    let file = std::fs::File::open(&path)?;
    let mut r = BufReader::new(file);

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

    // Read time index
    let mut times = Vec::with_capacity(frame_count);
    let mut buf8 = [0u8; 8];
    for _ in 0..frame_count {
        r.read_exact(&mut buf8)?;
        times.push(f64::from_le_bytes(buf8));
    }

    // Read RGBA data
    let frame_size = (width * height * 4) as usize;
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

fn thumbnail_dir() -> PathBuf {
    cache_base_dir().join("thumbnails")
}

fn thumbnail_path(cache_key: &str) -> PathBuf {
    thumbnail_dir().join(format!("{cache_key}.thumb"))
}

pub fn thumbnail_cache_exists(cache_key: &str) -> bool {
    thumbnail_path(cache_key).exists()
}

pub fn write_thumbnail_cache(
    cache_key: &str,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> std::io::Result<()> {
    let dir = thumbnail_dir();
    ensure_dir(&dir)?;

    let final_path = thumbnail_path(cache_key);
    let tmp_path = dir.join(format!("{cache_key}.thumb.tmp"));

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

pub fn load_thumbnail_cache(cache_key: &str) -> std::io::Result<(Vec<u8>, u32, u32)> {
    let path = thumbnail_path(cache_key);
    let file = std::fs::File::open(&path)?;
    let mut r = BufReader::new(file);

    let mut buf4 = [0u8; 4];
    r.read_exact(&mut buf4)?;
    let width = u32::from_le_bytes(buf4);
    r.read_exact(&mut buf4)?;
    let height = u32::from_le_bytes(buf4);

    let mut rgba = Vec::new();
    r.read_to_end(&mut rgba)?;

    let expected = (width * height * 4) as usize;
    if rgba.len() != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("thumbnail size mismatch: got {} expected {}", rgba.len(), expected),
        ));
    }

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
    let path = audio_peaks_path(cache_key);
    let file = std::fs::File::open(&path)?;
    let mut r = BufReader::new(file);

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
/// Falls back to the old MediaId-based filenames if the new ones don't exist.
pub fn audio_conform_cache_exists(cache_key: &str, media_id: klipp_core::MediaId) -> (bool, PathBuf, PathBuf) {
    let dir = audio_cache_dir();
    let pcm = dir.join(format!("{cache_key}.pcm"));
    let peaks = dir.join(format!("{cache_key}.peaks"));
    if is_valid_conform_pair(&pcm, &peaks) {
        return (true, pcm, peaks);
    }
    // Fallback: old MediaId-based filenames
    let pcm_old = dir.join(format!("{media_id}.pcm"));
    let peaks_old = dir.join(format!("{media_id}.peaks"));
    if is_valid_conform_pair(&pcm_old, &peaks_old) {
        return (true, pcm_old, peaks_old);
    }
    // Return the new paths for generation
    (false, pcm, peaks)
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

/// Get the skim transcode path for a content-based key.
/// Also checks the old MediaId-based path as fallback.
pub fn skim_transcode_path(cache_key: &str, media_id: klipp_core::MediaId) -> PathBuf {
    let dir = skim_dir();
    let new_path = dir.join(format!("{cache_key}.mkv"));
    if new_path.exists() {
        return new_path;
    }
    // Fallback: old MediaId-based path
    let old_path = dir.join(format!("{media_id}.mkv"));
    if old_path.exists() {
        return old_path;
    }
    // Return new path for generation
    new_path
}
