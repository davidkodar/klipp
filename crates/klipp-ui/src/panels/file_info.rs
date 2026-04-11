use crate::theme::KlippTheme;
use std::path::Path;

/// Show the File Info window. The path is shown to the user along with
/// filesystem metadata (size, mtime, type) and — for video/image files —
/// media metadata read from ffmpeg's format header (codec, dimensions,
/// duration, frame rate). The header read is fast (single-digit ms for
/// most files) so we do it inline rather than offloading to a worker.
///
/// `thumb` is the optional preview thumbnail (loaded lazily from the shared
/// media-pool disk cache by the caller). When present it's drawn at the top
/// next to the filename header.
///
/// Sets `*open` to false when the user closes the window.
pub fn show_file_info(
    ctx: &egui::Context,
    path: &Path,
    thumb: Option<&egui::TextureHandle>,
    theme: &KlippTheme,
    open: &mut bool,
) {
    egui::Window::new("File Info")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .default_pos(ctx.screen_rect().center() - egui::vec2(230.0, 220.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 5.0);

            let filename = path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "(no name)".into());

            // ── Header row: thumbnail (left) + filename (right) ──
            // The thumbnail is aspect-fit into a 160×90 box (16:9). If no
            // thumbnail is available, we just show the filename centered.
            ui.horizontal(|ui| {
                if let Some(tex) = thumb {
                    let box_w = 160.0_f32;
                    let box_h = 90.0_f32;
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(box_w, box_h),
                        egui::Sense::hover(),
                    );
                    // Dark backdrop so letterbox bars don't show through.
                    ui.painter().rect_filled(rect, 4.0, egui::Color32::from_rgb(18, 18, 18));
                    // Aspect-fit the texture inside the box.
                    let sz = tex.size();
                    let img_aspect = sz[0] as f32 / sz[1].max(1) as f32;
                    let box_aspect = box_w / box_h;
                    let fitted = if img_aspect > box_aspect {
                        let h = box_w / img_aspect;
                        let y_off = (box_h - h) / 2.0;
                        egui::Rect::from_min_size(
                            egui::pos2(rect.min.x, rect.min.y + y_off),
                            egui::vec2(box_w, h),
                        )
                    } else {
                        let w = box_h * img_aspect;
                        let x_off = (box_w - w) / 2.0;
                        egui::Rect::from_min_size(
                            egui::pos2(rect.min.x + x_off, rect.min.y),
                            egui::vec2(w, box_h),
                        )
                    };
                    ui.painter().image(
                        tex.id(),
                        fitted,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    ui.add_space(4.0);
                }
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(&filename)
                            .strong()
                            .size(14.0)
                            .color(theme.text_primary),
                    );
                });
            });
            ui.add_space(4.0);

            // ── Filesystem ──
            info_row(ui, theme, "Path", &path.to_string_lossy());

            match std::fs::metadata(path) {
                Ok(meta) => {
                    info_row(ui, theme, "Size", &format_size(meta.len()));
                    if let Ok(modified) = meta.modified() {
                        info_row(ui, theme, "Modified", &format_system_time(modified));
                    }
                    if let Ok(created) = meta.created() {
                        info_row(ui, theme, "Created", &format_system_time(created));
                    }
                    let kind = if meta.is_dir() {
                        "Directory"
                    } else if meta.is_symlink() {
                        "Symbolic link"
                    } else {
                        "File"
                    };
                    info_row(ui, theme, "Type", kind);
                }
                Err(e) => {
                    info_row(ui, theme, "Error", &format!("Cannot read metadata: {e}"));
                }
            }

            // ── Media metadata (only attempted for known video/image extensions) ──
            let ext = path.extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "avi", "webm", "m4v", "mts", "ts"];
            const STILL_EXTS: &[&str] = &["png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp", "exr"];
            let is_video = VIDEO_EXTS.contains(&ext.as_str());
            let is_still = STILL_EXTS.contains(&ext.as_str());

            if is_video || is_still {
                ui.separator();
                ui.label(
                    egui::RichText::new("Media")
                        .strong()
                        .size(12.0)
                        .color(theme.text_primary),
                );
                ui.add_space(2.0);

                match klipp_engine::probe_media_header(path) {
                    Ok(info) => {
                        if let Some(codec) = info.codec.as_deref() {
                            info_row(ui, theme, "Codec", codec);
                        }
                        if let (Some(w), Some(h)) = (info.width, info.height) {
                            info_row(ui, theme, "Dimensions", &format!("{w} × {h}"));
                        }
                        if let Some(fps) = info.fps {
                            info_row(ui, theme, "Frame rate", &format!("{fps:.3} fps"));
                        }
                        if is_video {
                            if let Some(dur) = info.duration_secs {
                                info_row(ui, theme, "Duration", &format_duration(dur));
                            }
                        }
                    }
                    Err(e) => {
                        info_row(ui, theme, "Media probe", &format!("failed: {e}"));
                    }
                }
            }
        });
}

fn info_row(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{label}:"))
                .size(12.0)
                .color(theme.text_dim),
        );
        ui.label(
            egui::RichText::new(value)
                .size(12.0)
                .color(theme.text_primary),
        );
    });
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn format_duration(secs: f64) -> String {
    let total = secs as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    let frac = ((secs - total as f64) * 1000.0) as u64;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{frac:03}")
    } else {
        format!("{m}:{s:02}.{frac:03}")
    }
}

fn format_system_time(t: std::time::SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs() as i64;
            // Y-M-D H:M:S in local time (rough — uses chrono if available)
            chrono::DateTime::<chrono::Local>::from(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64)
            )
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
        }
        Err(_) => "(unknown)".to_string(),
    }
}
