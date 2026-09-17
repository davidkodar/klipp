//! What a model that can see or hear is shown: a contact sheet of a
//! range of a file, frames evenly spaced with their time burnt in, and
//! an excerpt of its sound. Both made by ffmpeg on demand, small enough
//! for a context window, and neither stored.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContactSheet {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub cols: u32,
    pub rows: u32,
    pub tile_width: u32,
    /// Where in the file each tile is, reading order, seconds.
    pub tiles_secs: Vec<f64>,
    pub from_secs: f64,
    pub to_secs: f64,
}

/// A font for the burnt-in times, through fontconfig; `None` leaves
/// the tiles unlabelled rather than failing.
fn any_font() -> Option<PathBuf> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}", "sans"]).output().ok()?;
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    p.is_file().then_some(p)
}

/// `frames` frames of `media` between `from_secs` and `to_secs`,
/// evenly spaced, `cols` to a row, each `tile_width` wide, as a JPEG
/// at `out`.
#[allow(clippy::too_many_arguments)]
pub fn contact_sheet(
    ffmpeg: &Path,
    media: &Path,
    from_secs: f64,
    to_secs: f64,
    frames: u32,
    cols: u32,
    tile_width: u32,
    out: &Path,
) -> Result<ContactSheet, String> {
    let frames = frames.clamp(1, 64);
    let cols = cols.clamp(1, frames);
    let rows = frames.div_ceil(cols);
    let span = (to_secs - from_secs).max(0.1);
    // One frame every span/frames seconds, the first at the start.
    let step = span / frames as f64;
    let mut vf = format!("fps={}/{:.4},scale={}:-2", frames, span, tile_width.clamp(64, 1024));
    if let Some(font) = any_font() {
        vf.push_str(&format!(
            ",drawtext=fontfile={}:text='%{{pts\\:hms\\:{:.3}}}':x=6:y=6:fontsize={}:fontcolor=white:box=1:boxcolor=black@0.5",
            font.display(),
            from_secs,
            (tile_width / 14).clamp(10, 28)
        ));
    }
    vf.push_str(&format!(",tile={cols}x{rows}"));
    let status = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-nostats", "-loglevel", "error", "-y", "-ss"])
        .arg(format!("{from_secs:.3}"))
        .arg("-t")
        .arg(format!("{span:.3}"))
        .arg("-i")
        .arg(media)
        .args(["-an", "-sn", "-vf", &vf, "-frames:v", "1", "-q:v", "4"])
        .arg(out)
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))?;
    if !status.status.success() || !out.is_file() {
        return Err(format!("ffmpeg could not make the sheet: {}", String::from_utf8_lossy(&status.stderr).trim()));
    }
    let (width, height) = image_size(out).unwrap_or((0, 0));
    Ok(ContactSheet {
        path: out.to_path_buf(),
        width,
        height,
        cols,
        rows,
        tile_width,
        tiles_secs: (0..frames).map(|i| from_secs + i as f64 * step).collect(),
        from_secs,
        to_secs,
    })
}

/// The sound of `media` between `from_secs` and `to_secs` as 16 kHz
/// mono 16-bit WAV at `out`: what a model that can hear is given, and
/// what a person opens to check a take.
pub fn audio_excerpt(ffmpeg: &Path, media: &Path, from_secs: f64, to_secs: f64, out: &Path) -> Result<PathBuf, String> {
    let span = (to_secs - from_secs).clamp(0.1, 60.0);
    let status = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-nostats", "-loglevel", "error", "-y", "-ss"])
        .arg(format!("{from_secs:.3}"))
        .arg("-t")
        .arg(format!("{span:.3}"))
        .arg("-i")
        .arg(media)
        .args(["-vn", "-sn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(out)
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))?;
    if !status.status.success() || !out.is_file() {
        return Err(format!("ffmpeg could not make the excerpt: {}", String::from_utf8_lossy(&status.stderr).trim()));
    }
    Ok(out.to_path_buf())
}

/// A JPEG's size from its SOF marker, without an image crate.
fn image_size(path: &Path) -> Option<(u32, u32)> {
    let bytes = std::fs::read(path).ok()?;
    let mut i = 2;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}
