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
    *FILMSTRIP_DIR_OVERRIDE.lock().unwrap() = filmstrip;
    *AUDIO_CACHE_DIR_OVERRIDE.lock().unwrap() = audio;
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

fn filmstrip_path(cache_key: &str) -> PathBuf {
    filmstrip_dir().join(format!("{cache_key}.strip"))
}

pub fn filmstrip_cache_exists(cache_key: &str) -> bool {
    filmstrip_path(cache_key).exists()
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

    // Write to .tmp then rename for atomicity
    let final_path = filmstrip_path(cache_key);
    let tmp_path = dir.join(format!("{cache_key}.strip.tmp"));

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

pub fn load_filmstrip_cache(cache_key: &str) -> std::io::Result<Vec<FilmstripFrame>> {
    let path = filmstrip_path(cache_key);
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

// ── Audio conform cache (content-based keys) ──

fn audio_cache_dir() -> PathBuf {
    AUDIO_CACHE_DIR_OVERRIDE.lock().ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| cache_base_dir().join("audio-cache"))
}

/// Check if audio conform cache exists for a content-based key.
/// Falls back to checking the old MediaId-based filename if the new one doesn't exist.
pub fn audio_conform_cache_exists(cache_key: &str, media_id: klipp_core::MediaId) -> (bool, PathBuf, PathBuf) {
    let dir = audio_cache_dir();
    let pcm = dir.join(format!("{cache_key}.pcm"));
    let peaks = dir.join(format!("{cache_key}.peaks"));
    if pcm.exists() && peaks.exists() {
        return (true, pcm, peaks);
    }
    // Fallback: old MediaId-based filenames
    let pcm_old = dir.join(format!("{media_id}.pcm"));
    let peaks_old = dir.join(format!("{media_id}.peaks"));
    if pcm_old.exists() && peaks_old.exists() {
        return (true, pcm_old, peaks_old);
    }
    // Return the new paths for generation
    (false, pcm, peaks)
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
