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
