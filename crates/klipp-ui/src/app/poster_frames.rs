//! Poster frames and thumbnails: taken from the frame buffer, the media or an
//! image file, scaled down and saved as PNG (#890).

use super::*;

/// Build a small thumbnail of the most recent inference's mask
/// for the panel's Recent list. Reuses the overlay crate's
/// `build_mask_texture` and tints with the panel's default
/// overlay colour so the row thumbnails read as "this was a
/// mask" at a glance. Returns `None` when the mask is empty /
/// malformed.
pub(super) fn build_recent_thumbnail(
    ctx: &egui::Context,
    inference: &crate::ai_tools::InferenceState,
) -> Option<egui::TextureHandle> {
    let response = match inference {
        crate::ai_tools::InferenceState::Done { response, .. } => response,
        _ => return None,
    };
    let first = response.candidates.first()?;
    crate::ai_tools::build_mask_texture(
        ctx,
        &first.mask,
        crate::ai_tools::DEFAULT_MASK_OVERLAY_COLOR,
        "ai_recent_thumb",
    )
}

/// Write the skim strip of `seq` beside `poster`, on a thread of its own:
/// [`crate::panels::launch_screen::SKIM_FRAMES`] frames evenly through the
/// cut, side by side, each the timeline's picture there as the export
/// draws it (#2945): its titles, effects, grade and transitions, nested
/// scenes resolved through `all`. A frame that can't be drawn stays black.
pub(super) fn spawn_skim_strip(seq: &klipp_timeline::Sequence, all: &[klipp_timeline::Sequence], media_pool: &klipp_media::MediaPool, poster: &std::path::Path) {
    let n = crate::panels::launch_screen::SKIM_FRAMES;
    let duration = seq.total_duration().to_secs_f64();
    if duration <= 0.0 {
        return;
    }
    let snapshot = match klipp_render::SequenceSnapshot::from_sequence_with_all(seq, all, media_pool, &klipp_core::ExportSettings::default(), None, false, klipp_engine::video_decoder::working_colorspace()) {
        Ok(mut s) => {
            s.grade_encoding = klipp_core::grade_encoding();
            s
        }
        Err(e) => {
            tracing::warn!("skim strip: the timeline could not be read: {e}");
            return;
        }
    };
    let cell_w = 192u32;
    let (rw, rh) = (seq.resolution.width.max(1), seq.resolution.height.max(1));
    // Even, as the render's frame sizes are.
    let cell_h = (((cell_w as u64 * rh as u64 / rw as u64) as u32).clamp(16, 400)) & !1;
    let out = crate::panels::launch_screen::skim_strip_path(poster);
    let _ = std::thread::Builder::new().name("skim-strip".into()).spawn(move || {
        let strip = skim_strip_pixels(&snapshot, duration, n, cell_w, cell_h);
        // Written whole under another name, then put in place, so the
        // welcome screen never reads half a strip.
        let tmp = out.with_extension("strip.tmp");
        if save_rgba_as_png(&strip, cell_w * n as u32, cell_h, &tmp) {
            let _ = std::fs::rename(&tmp, &out);
        }
    });
}

/// The skim strip's pixels: `n` cells of `cell_w` × `cell_h`, side by side,
/// the timeline's picture at the middle of each nth of its `duration`,
/// drawn by the export's renderer from the media's own files at the
/// cell's size (#2945). A cell that can't be drawn is black.
pub(super) fn skim_strip_pixels(snapshot: &klipp_render::SequenceSnapshot, duration: f64, n: usize, cell_w: u32, cell_h: u32) -> Vec<u8> {
    let strip_w = cell_w * n as u32;
    let mut strip = vec![0u8; (strip_w * cell_h * 4) as usize];
    for px in strip.chunks_exact_mut(4) {
        px[3] = 255;
    }
    for i in 0..n {
        let secs = duration * (i as f64 + 0.5) / n as f64;
        let still = match klipp_render::still::timeline_still(snapshot, secs, cell_w, cell_h) {
            Ok(still) => still,
            Err(e) => {
                tracing::debug!("skim strip: no frame at {secs:.2} s: {e}");
                continue;
            }
        };
        for y in 0..cell_h {
            let row = (y * cell_w * 4) as usize;
            let di = ((y * strip_w + i as u32 * cell_w) * 4) as usize;
            strip[di..di + (cell_w * 4) as usize].copy_from_slice(&still.rgba[row..row + (cell_w * 4) as usize]);
        }
    }
    strip
}

/// Generate a poster frame thumbnail and save as PNG.
/// Returns the path to the saved thumbnail, or None if no frame available.
/// Save RGBA pixel data as a PNG file.
pub(super) fn save_rgba_as_png(rgba: &[u8], width: u32, height: u32, path: &std::path::Path) -> bool {
    let Ok(file) = std::fs::File::create(path) else { return false };
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let Ok(mut writer) = encoder.write_header() else { return false };
    writer.write_image_data(rgba).is_ok()
}

pub(super) fn generate_poster_frame(
    frame_buffer: &klipp_engine::SharedFrameBuffer,
    project_path: &std::path::Path,
    media_pool: &klipp_media::MediaPool,
    duration_secs: f64,
) -> Option<PathBuf> {
    let cache_dir = dirs::cache_dir()?.join("klipp").join("thumbnails");
    std::fs::create_dir_all(&cache_dir).ok()?;
    let stem = project_path.file_stem()?.to_string_lossy();
    let out_path = cache_dir.join(format!("{stem}.png"));

    // Strategy 1: Use current viewer frame if non-black
    if let Some(path) = try_poster_from_frame_buffer(frame_buffer, &out_path) {
        return Some(path);
    }

    // Strategy 2: Extract first non-black frame from the first media file
    if let Some(path) = try_poster_from_media(media_pool, duration_secs, &out_path) {
        return Some(path);
    }

    None
}

/// Try to generate a poster frame from the current viewer frame buffer.
/// Returns None if the frame is empty or all-black.
pub(super) fn try_poster_from_frame_buffer(
    frame_buffer: &klipp_engine::SharedFrameBuffer,
    out_path: &std::path::Path,
) -> Option<PathBuf> {
    let fb = frame_buffer.lock().ok()?;
    if fb.data.is_empty() || fb.width == 0 || fb.height == 0 {
        return None;
    }
    if klipp_engine::is_frame_black(&fb.data, fb.width, fb.height) {
        return None;
    }

    let rgb = downsample_rgba_to_rgb(&fb.data, fb.width, fb.height, 320);
    save_rgb_as_png(&rgb.0, rgb.1, rgb.2, out_path).then(|| out_path.to_path_buf())
}

/// Try to generate a poster frame by decoding the first media file in the pool.
pub(super) fn try_poster_from_media(
    media_pool: &klipp_media::MediaPool,
    duration_secs: f64,
    out_path: &std::path::Path,
) -> Option<PathBuf> {
    // Find first video media file
    let first_media = media_pool.all()
        .find(|m| m.path.exists() && is_video_ext(&m.path))?;

    let est_dur = if duration_secs > 0.0 { duration_secs } else { 60.0 };
    let (rgba, w, h) = klipp_engine::generate_thumbnail_at_size_non_black(
        &first_media.path, 1.0, est_dur, 320,
    ).ok()?;

    save_rgba_as_png(&rgba, w, h, out_path).then(|| out_path.to_path_buf())
}

/// Force-capture the current viewer frame as project thumbnail (for custom thumbnail).
pub(super) fn capture_viewer_frame_as_thumbnail(
    frame_buffer: &klipp_engine::SharedFrameBuffer,
    project_path: &std::path::Path,
) -> Option<PathBuf> {
    let fb = frame_buffer.lock().ok()?;
    if fb.data.is_empty() || fb.width == 0 || fb.height == 0 {
        return None;
    }

    let cache_dir = dirs::cache_dir()?.join("klipp").join("thumbnails");
    std::fs::create_dir_all(&cache_dir).ok()?;
    let stem = project_path.file_stem()?.to_string_lossy();
    let out_path = cache_dir.join(format!("{stem}.png"));

    let rgb = downsample_rgba_to_rgb(&fb.data, fb.width, fb.height, 320);
    save_rgb_as_png(&rgb.0, rgb.1, rgb.2, &out_path).then_some(out_path)
}

/// Set project thumbnail from an external image file (PNG, JPEG, etc.).
pub(super) fn set_thumbnail_from_image_file(
    image_path: &std::path::Path,
    project_path: &std::path::Path,
) -> Option<PathBuf> {
    let cache_dir = dirs::cache_dir()?.join("klipp").join("thumbnails");
    std::fs::create_dir_all(&cache_dir).ok()?;
    let stem = project_path.file_stem()?.to_string_lossy();
    let out_path = cache_dir.join(format!("{stem}.png"));

    // Read and decode with the image crate-style approach using ffmpeg
    // Use generate_thumbnail_at_size for images (seeks to 0.0s, which works for stills)
    if klipp_core::media_extensions::path_is_image(image_path) {
        // For images, use ffmpeg to decode and resize. Now correctly handles
        // HEIC/HEIF/AVIF/JXL — the previous hardcoded chain only matched 7
        // legacy extensions and silently fell through to the video branch
        // for HEIC, which then errored on seek.
        if let Ok((rgba, w, h)) = klipp_engine::generate_thumbnail_at_size(image_path, 0.0, 320) {
            return save_rgba_as_png(&rgba, w, h, &out_path).then_some(out_path);
        }
    }

    // For video files, extract a non-black frame
    if let Ok((rgba, w, h)) = klipp_engine::generate_thumbnail_at_size_non_black(image_path, 1.0, 60.0, 320) {
        return save_rgba_as_png(&rgba, w, h, &out_path).then_some(out_path);
    }

    None
}

/// Downsample RGBA frame data to RGB at a target width (maintaining aspect ratio).
pub(super) fn downsample_rgba_to_rgb(rgba: &[u8], src_w: u32, src_h: u32, target_w: u32) -> (Vec<u8>, u32, u32) {
    let dst_w = target_w.min(src_w);
    let dst_h = (src_h as u64 * dst_w as u64 / src_w as u64) as u32;
    let mut rgb = vec![0u8; (dst_w * dst_h * 3) as usize];

    for dy in 0..dst_h {
        for dx in 0..dst_w {
            let sx = (dx as u64 * src_w as u64 / dst_w as u64) as u32;
            let sy = (dy as u64 * src_h as u64 / dst_h as u64) as u32;
            let src_idx = ((sy * src_w + sx) * 4) as usize;
            let dst_idx = ((dy * dst_w + dx) * 3) as usize;
            if src_idx + 2 < rgba.len() && dst_idx + 2 < rgb.len() {
                rgb[dst_idx] = rgba[src_idx];
                rgb[dst_idx + 1] = rgba[src_idx + 1];
                rgb[dst_idx + 2] = rgba[src_idx + 2];
            }
        }
    }
    (rgb, dst_w, dst_h)
}

/// Write RGB pixel data as a PNG file.
pub(super) fn save_rgb_as_png(rgb: &[u8], width: u32, height: u32, path: &std::path::Path) -> bool {
    let Ok(file) = std::fs::File::create(path) else { return false };
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let Ok(mut writer) = encoder.write_header() else { return false };
    writer.write_image_data(rgb).is_ok()
}

#[cfg(test)]
mod skim_strip_tests {
    /// #2945: the project screen's strip shows the picture, a title
    /// included: a project whose picture is a title over nothing showed a
    /// black strip, made from the top video track's raw media.
    #[test]
    fn the_skim_strip_shows_a_title() {
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 25);
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "STRIP".into(), font_size: 160.0, color: [255, 255, 255, 255], ..Default::default() });
        let mut t1 = klipp_timeline::Track::new("T1", klipp_timeline::TrackKind::Text);
        t1.clips = vec![klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(td), ..klipp_timeline::Clip::gap(tc(0.0), tc(4.0)) }];
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        seq.resolution = klipp_core::Resolution { width: 640, height: 360 };
        seq.tracks = vec![t1];
        let snapshot = klipp_render::SequenceSnapshot::from_sequence_with_all(&seq, std::slice::from_ref(&seq), &klipp_media::MediaPool::default(), &klipp_core::ExportSettings::default(), None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let (n, w, h) = (4, 192, 108);
        let strip = super::skim_strip_pixels(&snapshot, 4.0, n, w, h);
        assert_eq!(strip.len(), (w * n as u32 * h * 4) as usize);
        for cell in 0..n as u32 {
            let white = (0..h).flat_map(|y| (0..w).map(move |x| ((y * w * n as u32 + cell * w + x) * 4) as usize)).filter(|&i| strip[i] > 200 && strip[i + 1] > 200 && strip[i + 2] > 200).count();
            assert!(white > 20, "cell {cell} shows the title: {white} white pixels");
        }
    }
}
