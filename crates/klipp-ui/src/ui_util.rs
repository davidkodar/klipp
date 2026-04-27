//! Shared egui layout helpers.

/// Allocate a child UI region whose `max_rect` AND `clip_rect` are both set
/// to the requested rect (intersected with the parent clip rect).
///
/// Use this instead of `ui.allocate_new_ui(UiBuilder::new().max_rect(r), ...)`
/// followed by a manual `ui.set_clip_rect(...)`. It prevents child widgets
/// from painting outside their allocated rect — the root cause of the
/// historical status-bar bleed bugs.
pub fn allocate_clipped_ui<R>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let clipped_rect = rect.intersect(ui.clip_rect());
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.set_clip_rect(clipped_rect);
        add_contents(ui)
    })
    .inner
}

/// Decode a PNG into an `egui::ColorImage`, returning a 1×1 transparent
/// placeholder on any decode failure or unsupported pixel format.
///
/// Used for logo / icon assets where a broken or stripped PNG should not
/// crash the UI on open. The asset is baked in via `include_bytes!` today,
/// but a corrupted file in the tree (or a future move to runtime asset
/// loading) would otherwise take the whole app down at startup.
pub fn decode_png_or_fallback(bytes: &[u8]) -> egui::ColorImage {
    fn fallback() -> egui::ColorImage {
        egui::ColorImage {
            size: [1, 1],
            pixels: vec![egui::Color32::TRANSPARENT],
        }
    }

    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let Ok(mut reader) = decoder.read_info() else {
        tracing::warn!("decode_png_or_fallback: PNG header read failed");
        return fallback();
    };
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let Ok(info) = reader.next_frame(&mut buf) else {
        tracing::warn!("decode_png_or_fallback: PNG frame decode failed");
        return fallback();
    };
    let bytes = &buf[..info.buffer_size()];
    let pixels: Vec<egui::Color32> = match info.color_type {
        png::ColorType::Rgba => bytes
            .chunks_exact(4)
            .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
            .collect(),
        png::ColorType::Rgb => bytes
            .chunks_exact(3)
            .map(|c| egui::Color32::from_rgb(c[0], c[1], c[2]))
            .collect(),
        other => {
            tracing::warn!(
                "decode_png_or_fallback: unsupported PNG color type {:?}",
                other
            );
            return fallback();
        }
    };
    egui::ColorImage {
        size: [info.width as usize, info.height as usize],
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════
    // PANIC-VIEWER regression — closes CODE_AUDIT PANIC-VIEWER.
    //
    // The original tracker entry pointed at three sites
    // (viewer.rs:276-289, toolbar.rs:193, launch_screen.rs:70) that
    // used `decoder.read_info().expect("...")` on baked-in PNG
    // assets. If the asset pipeline ever stripped a PNG (or a
    // corrupt file landed in a future runtime asset path), startup
    // would crash. All three sites now route through
    // decode_png_or_fallback, which returns a 1×1 transparent
    // placeholder on every PNG-error path. These tests pin that
    // contract so a future tightening can't silently re-introduce
    // the panic.
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn decode_png_recovers_from_garbage_bytes() {
        // Random non-PNG bytes — must NOT panic, must return the
        // 1×1 transparent placeholder.
        let img = decode_png_or_fallback(b"this is not a PNG file at all");
        assert_eq!(img.size, [1, 1], "garbage must yield 1×1 placeholder");
        assert_eq!(img.pixels.len(), 1);
        assert_eq!(img.pixels[0], egui::Color32::TRANSPARENT);
    }

    #[test]
    fn decode_png_recovers_from_empty_bytes() {
        // Empty buffer — header read fails immediately.
        let img = decode_png_or_fallback(&[]);
        assert_eq!(img.size, [1, 1]);
        assert_eq!(img.pixels[0], egui::Color32::TRANSPARENT);
    }

    #[test]
    fn decode_png_recovers_from_truncated_png() {
        // Valid PNG signature but the rest is missing — read_info
        // succeeds far enough to advance, then next_frame hits the
        // truncated body.
        let mut bytes = vec![137, 80, 78, 71, 13, 10, 26, 10]; // PNG magic
        bytes.extend_from_slice(b"truncated junk after the magic");
        let img = decode_png_or_fallback(&bytes);
        assert_eq!(img.size, [1, 1],
            "truncated PNG must fall through to placeholder, not panic");
    }

    #[test]
    fn decode_png_loads_a_valid_2x2_rgba_png() {
        // Build a minimal 2×2 RGBA PNG inline so the test doesn't
        // depend on any on-disk asset. Sanity: the happy path
        // returns the right dimensions, not the placeholder.
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("write header");
            // 4 RGBA pixels: red, green, blue, white.
            writer.write_image_data(&[
                255, 0, 0, 255,
                0, 255, 0, 255,
                0, 0, 255, 255,
                255, 255, 255, 255,
            ]).expect("write image data");
        }
        let img = decode_png_or_fallback(&buf);
        assert_eq!(img.size, [2, 2], "valid PNG must decode at full size");
        assert_eq!(img.pixels.len(), 4);
        // Spot-check the red pixel.
        assert_eq!(img.pixels[0].r(), 255);
        assert_eq!(img.pixels[0].g(), 0);
    }
}
