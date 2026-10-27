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

/// How long a sheet's hardware decode may take before it is given up
/// for the CPU: ffmpeg's VAAPI decode can deadlock on a seek into some
/// files and wait forever, which held a review past any deadline (#519).
const GPU_SHEET_LIMIT: std::time::Duration = std::time::Duration::from_secs(45);

/// Run a sheet's ffmpeg: within [`GPU_SHEET_LIMIT`] when it decodes on
/// the GPU, to the end on the CPU. Through [`crate::hold`], so a sheet
/// waits while the editor plays, as every other analysis decode does.
fn run_sheet(ffmpeg: &Path, cmd: &mut std::process::Command, gpu: bool, out: &Path) -> Result<(), String> {
    let limit = gpu.then_some(GPU_SHEET_LIMIT);
    let done = crate::hold::output_within(cmd.stderr(std::process::Stdio::piped()), limit).map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))?;
    let Some(done) = done else {
        return Err(format!("the GPU decode did not finish within {} s", GPU_SHEET_LIMIT.as_secs()));
    };
    if !done.status.success() || !out.is_file() {
        return Err(format!("ffmpeg could not make the sheet: {}", String::from_utf8_lossy(&done.stderr).trim()));
    }
    Ok(())
}

/// A sheet whose tiles are far apart: the file opened once per tile and
/// sought to its time, one frame taken from each, laid out together.
#[allow(clippy::too_many_arguments)]
fn sheet_by_seeking(ffmpeg: &Path, media: &Path, tiles_secs: &[f64], cols: u32, rows: u32, tile_width: u32, out: &Path, gpu: bool) -> Result<(), String> {
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "error", "-y"]);
    let hw = if gpu { crate::hwaccel_args(ffmpeg, "") } else { Vec::new() };
    let input = klipp_media::input_args(media)?;
    for t in tiles_secs {
        cmd.args(&hw).arg("-ss").arg(format!("{t:.3}")).args(&input);
    }
    let w = tile_width.clamp(64, 1024);
    let font = any_font();
    let size = (tile_width / 14).clamp(10, 28);
    let mut graph = String::new();
    for (k, t) in tiles_secs.iter().enumerate() {
        graph.push_str(&format!("[{k}:v]trim=end_frame=1,scale={w}:-2,setsar=1"));
        if let Some(font) = &font {
            let (h, m, sec) = ((*t / 3600.0) as u64, ((*t / 60.0) as u64) % 60, *t % 60.0);
            graph.push_str(&format!(
                ",drawtext=fontfile={}:text='{h:02}\\:{m:02}\\:{sec:06.3}':x=6:y=6:fontsize={size}:fontcolor=white:box=1:boxcolor=black@0.5",
                font.display()
            ));
        }
        graph.push_str(&format!("[t{k}];"));
    }
    for k in 0..tiles_secs.len() {
        graph.push_str(&format!("[t{k}]"));
    }
    graph.push_str(&format!("concat=n={}:v=1:a=0,tile={cols}x{rows}", tiles_secs.len()));
    cmd.args(["-an", "-sn", "-filter_complex", &graph, "-frames:v", "1", "-q:v", "4"]).arg(out);
    run_sheet(ffmpeg, &mut cmd, gpu, out)
}

/// A sheet whose tiles are close together: one pass over the span,
/// keeping a frame every `span / frames` seconds.
#[allow(clippy::too_many_arguments)]
fn sheet_by_filter(ffmpeg: &Path, media: &Path, from_secs: f64, span: f64, frames: u32, cols: u32, rows: u32, tile_width: u32, out: &Path, gpu: bool) -> Result<(), String> {
    let mut vf = format!("fps={}/{:.4}:round=up,scale={}:-2", frames, span, tile_width.clamp(64, 1024));
    if let Some(font) = any_font() {
        vf.push_str(&format!(
            ",drawtext=fontfile={}:text='%{{pts\\:hms\\:{:.3}}}':x=6:y=6:fontsize={}:fontcolor=white:box=1:boxcolor=black@0.5",
            font.display(),
            from_secs,
            (tile_width / 14).clamp(10, 28)
        ));
    }
    vf.push_str(&format!(",tile={cols}x{rows}"));
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "error", "-y"]);
    if gpu {
        cmd.args(crate::hwaccel_args(ffmpeg, ""));
    }
    cmd.arg("-ss")
        .arg(format!("{from_secs:.3}"))
        .arg("-t")
        .arg(format!("{span:.3}"))
        .args(klipp_media::input_args(media)?)
        .args(["-an", "-sn", "-vf", &vf, "-frames:v", "1", "-q:v", "4"])
        .arg(out);
    run_sheet(ffmpeg, &mut cmd, gpu, out)
}

/// `frames` frames of `media` between `from_secs` and `to_secs`,
/// evenly spaced, `cols` to a row, each `tile_width` wide, as a JPEG
/// at `out`. Decoded on the GPU where it can be, on the CPU when that
/// fails or does not finish.
#[allow(clippy::too_many_arguments)]
pub fn contact_sheet(ffmpeg: &Path, media: &Path, from_secs: f64, to_secs: f64, frames: u32, cols: u32, tile_width: u32, out: &Path) -> Result<ContactSheet, String> {
    sheet(ffmpeg, media, from_secs, to_secs, frames, cols, tile_width, out, true)
}

/// [`contact_sheet`] decoded on the CPU only: for a small file, such as
/// a review's draft, where the GPU saves nothing and its decoder can
/// hang on a seek (#519).
#[allow(clippy::too_many_arguments)]
pub fn contact_sheet_on_cpu(ffmpeg: &Path, media: &Path, from_secs: f64, to_secs: f64, frames: u32, cols: u32, tile_width: u32, out: &Path) -> Result<ContactSheet, String> {
    sheet(ffmpeg, media, from_secs, to_secs, frames, cols, tile_width, out, false)
}

#[allow(clippy::too_many_arguments)]
fn sheet(ffmpeg: &Path, media: &Path, from_secs: f64, to_secs: f64, frames: u32, cols: u32, tile_width: u32, out: &Path, gpu: bool) -> Result<ContactSheet, String> {
    let frames = frames.clamp(1, 64);
    let cols = cols.clamp(1, frames);
    let rows = frames.div_ceil(cols);
    let span = (to_secs - from_secs).max(0.1);
    // One frame every span/frames seconds, the first at the start.
    let step = span / frames as f64;
    let tiles_secs: Vec<f64> = (0..frames).map(|i| from_secs + i as f64 * step).collect();
    // Rounded up, a tile is the frame at its time. The filter's default
    // rounds to the nearest tick, and each tile then shows the picture
    // half a step after the time burnt into it: half a minute on a
    // sheet of a half-hour file.
    // Tiles more than a couple of seconds apart are fetched one seek
    // each: the filter decodes every frame of the span to keep one in
    // hundreds, which on a half-hour 1440p file was over two minutes.
    let make = |gpu: bool| {
        if step >= 2.0 {
            sheet_by_seeking(ffmpeg, media, &tiles_secs, cols, rows, tile_width, out, gpu)
        } else {
            sheet_by_filter(ffmpeg, media, from_secs, span, frames, cols, rows, tile_width, out, gpu)
        }
    };
    if let Err(e) = make(gpu) {
        if !gpu {
            return Err(e);
        }
        tracing::warn!("contact sheet of {}: {e}; decoding on the CPU", media.display());
        make(false)?;
    }
    let (width, height) = image_size(out).unwrap_or((0, 0));
    Ok(ContactSheet { path: out.to_path_buf(), width, height, cols, rows, tile_width, tiles_secs, from_secs, to_secs })
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
        .args(klipp_media::input_args(media)?)
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
