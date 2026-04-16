use crate::state::AppState;
use klipp_core::AppCommand;

/// Cache for LUT preview thumbnails, stored in egui context data.
#[derive(Clone, Default)]
struct LutPreviewCache(std::collections::HashMap<std::path::PathBuf, egui::TextureHandle>);

/// Show the Color tab content: color redirect, global LUT, and LUT presets.
pub fn show_color_and_luts(
    ui: &mut egui::Ui,
    state: &AppState,
    cmds: &mut Vec<AppCommand>,
) {
    egui::ScrollArea::vertical().auto_shrink([false; 2]).id_salt("color_lut_scroll").show(ui, |ui| {
        // Input Color Space selector
        if let Some(clip_id) = state.selected_clip {
            let current_log = state.sequences.get(state.active_sequence_idx)
                .and_then(|seq| seq.tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id))
                .and_then(|c| c.node_graph.nodes.iter()
                    .find_map(|n| n.params.log_format))
                .unwrap_or_default();

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new("Input Color Space").size(10.0)
                    .color(egui::Color32::from_rgb(0x90, 0x90, 0xa0)));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                use klipp_core::LogFormat as LF;
                let formats = [
                    (LF::Srgb, "sRGB / Rec.709"), (LF::Linear, "Linear"),
                    (LF::LogC3, "ARRI LogC3"), (LF::LogC4, "ARRI LogC4"),
                    (LF::SLog2, "Sony S-Log2"), (LF::SLog3, "Sony S-Log3"),
                    (LF::CanonLog, "Canon Log"), (LF::CanonLog3, "Canon Log 3"),
                    (LF::VLog, "Panasonic V-Log"), (LF::NLog, "Nikon N-Log"),
                    (LF::FLog, "Fuji F-Log"), (LF::DJIDLog, "DJI D-Log"),
                    (LF::GoProFlat, "GoPro ProTune Flat"),
                    (LF::AppleLog, "Apple Log"), (LF::LeicaLLog, "Leica L-Log"),
                    (LF::RedLog3G10, "RED Log3G10"), (LF::BmdFilmGen5, "BMD Film Gen 5"),
                    (LF::HLG, "HLG (BT.2100)"), (LF::PQ, "PQ (ST 2084)"),
                ];
                let current_label = formats.iter()
                    .find(|(f, _)| *f == current_log)
                    .map(|(_, l)| *l)
                    .unwrap_or("sRGB / Rec.709");
                egui::ComboBox::from_id_salt("color_tab_log_format")
                    .selected_text(egui::RichText::new(current_label).size(9.0))
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for &(fmt, label) in &formats {
                            if ui.selectable_label(current_log == fmt,
                                egui::RichText::new(label).size(9.0),
                            ).clicked() {
                                cmds.push(AppCommand::SetClipLogFormat { clip_id, log_format: fmt });
                            }
                        }
                    });
            });
            ui.add_space(8.0);
        }

        // Open Color Panel button
        ui.vertical_centered(|ui| {
            if ui.button(
                egui::RichText::new("Open Color Panel  (C)")
                    .size(11.0)
                    .color(egui::Color32::from_rgb(0xe8, 0xa0, 0x20)),
            ).clicked() {
                cmds.push(AppCommand::ToggleColorPanel);
            }
        });

        // Global LUT
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Global LUT").strong().size(12.0));
        ui.separator();
        ui.add_space(4.0);

        if let Some(path) = &state.global_lut_path {
            let filename = path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Unknown".into());
            // Enable/disable + filename + clear on one row
            ui.horizontal(|ui| {
                let mut enabled = state.global_lut_enabled;
                if ui.checkbox(&mut enabled, "").changed() {
                    cmds.push(AppCommand::SetGlobalLutEnabled(enabled));
                }
                let label_color = if state.global_lut_enabled {
                    egui::Color32::from_rgb(100, 200, 120)
                } else {
                    egui::Color32::from_rgb(120, 120, 130)
                };
                ui.colored_label(label_color, &filename);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Clear").clicked() {
                        cmds.push(AppCommand::ClearGlobalLut);
                    }
                });
            });
            ui.add_space(4.0);
            let mut opacity = state.global_lut_opacity;
            ui.label("Opacity");
            if ui.add(egui::Slider::new(&mut opacity, 0.0..=1.0)
                .step_by(0.01).min_decimals(2)
            ).changed() {
                cmds.push(AppCommand::SetGlobalLutOpacity(opacity));
            }
            ui.add_space(4.0);
            if ui.button("Load .cube LUT...").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Load .cube LUT")
                    .add_filter("LUT files", &["cube", "3dl", "csp"])
                    .pick_file()
                {
                    cmds.push(AppCommand::LoadGlobalLut { path });
                }
            }
        } else {
            ui.label("No LUT loaded");
            ui.add_space(4.0);
            if ui.button("Load .cube LUT...").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Load .cube LUT")
                    .add_filter("LUT files", &["cube", "3dl", "csp"])
                    .pick_file()
                {
                    cmds.push(AppCommand::LoadGlobalLut { path });
                }
            }
        }

        // Bundled LUT Presets
        ui.add_space(8.0);
        ui.label(egui::RichText::new("LUT Presets").strong().size(12.0));
        ui.separator();
        ui.add_space(4.0);

        let luts_dir = find_bundled_luts_dir();
        if let Some(ref base) = luts_dir {
            let categories = [
                ("cinematic", "Cinematic"),
                ("film-bw", "Film B&W"),
                ("film-slide", "Film Slide"),
                ("film-negative", "Film Negative"),
                ("film-print", "Film Print"),
                ("film-instant", "Film Instant"),
                ("fuji-xtrans", "Fuji X-Trans"),
            ];
            for (dir_name, label) in &categories {
                let cat_dir = base.join(dir_name);
                if !cat_dir.exists() { continue; }
                let id = egui::Id::new(format!("lut_preset_{dir_name}"));
                egui::CollapsingHeader::new(*label)
                    .id_salt(id)
                    .show(ui, |ui| {
                        let mut entries: Vec<_> = std::fs::read_dir(&cat_dir)
                            .into_iter().flatten()
                            .filter_map(|e| e.ok())
                            .filter(|e| e.path().extension()
                                .is_some_and(|ext| ext == "cube" || ext == "3dl" || ext == "csp"))
                            .collect();
                        entries.sort_by_key(|e| e.file_name());
                        let ctx = ui.ctx().clone();
                        for entry in entries {
                            let path = entry.path();
                            let name = path.file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let is_active = state.global_lut_path.as_ref() == Some(&path);
                            let thumb_size = 32.0;
                            let row_h = thumb_size + 4.0;
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), row_h),
                                egui::Sense::click(),
                            );
                            // Background: subtle highlight for active, hover for others
                            if is_active {
                                ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgba_premultiplied(232, 160, 32, 30));
                            } else if resp.hovered() {
                                ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 12));
                            }
                            // LUT preview thumbnail (lazy-loaded only when visible)
                            let thumb_rect = egui::Rect::from_min_size(
                                egui::pos2(rect.min.x + 4.0, rect.min.y + 2.0),
                                egui::vec2(thumb_size, thumb_size),
                            );
                            if ui.is_rect_visible(thumb_rect) {
                                if let Some(tex) = get_lut_preview(&ctx, &path) {
                                    ui.painter().image(
                                        tex.id(),
                                        thumb_rect,
                                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                        egui::Color32::WHITE,
                                    );
                                    // Subtle border around thumbnail
                                    ui.painter().rect_stroke(thumb_rect, 1.0, egui::Stroke::new(0.5, egui::Color32::from_rgb(60, 60, 65)));
                                }
                            }
                            // Text: white for active, dim for others
                            let text_color = if is_active {
                                egui::Color32::from_rgb(255, 255, 255)
                            } else {
                                egui::Color32::from_rgb(180, 180, 190)
                            };
                            let text_x = rect.min.x + thumb_size + 12.0;
                            // Active indicator dot
                            if is_active {
                                ui.painter().circle_filled(
                                    egui::pos2(text_x, rect.center().y),
                                    3.0,
                                    egui::Color32::from_rgb(232, 160, 32),
                                );
                            }
                            ui.painter().text(
                                egui::pos2(text_x + if is_active { 8.0 } else { 0.0 }, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                &name,
                                egui::FontId::proportional(11.0),
                                text_color,
                            );
                            if resp.clicked() {
                                if is_active {
                                    cmds.push(AppCommand::ClearGlobalLut);
                                } else {
                                    cmds.push(AppCommand::LoadGlobalLut { path });
                                }
                            }
                        }
                    });
            }
        }
    });
}

/// Get or create a LUT preview thumbnail texture for the given LUT file path.
/// Returns None if the LUT cannot be loaded. Uses lazy loading (only generates
/// preview when first requested). Cache is stored in egui context data so it
/// is shared across all inspector instances.
fn get_lut_preview(
    ctx: &egui::Context,
    path: &std::path::Path,
) -> Option<egui::TextureHandle> {
    let cache_id = egui::Id::new("lut_preview_cache");
    // Check if already cached
    let existing: Option<egui::TextureHandle> = ctx.data_mut(|d| {
        d.get_temp::<LutPreviewCache>(cache_id)
            .and_then(|c| c.0.get(path).cloned())
    });
    if let Some(handle) = existing {
        return Some(handle);
    }
    // Load LUT and generate preview
    let lut = klipp_color::load_lut(path).ok()?;
    let rgba = klipp_color::generate_lut_preview(&lut);
    let image = egui::ColorImage::from_rgba_unmultiplied([64, 64], &rgba);
    let name = path.file_stem()
        .map(|s| format!("lut_preview_{}", s.to_string_lossy()))
        .unwrap_or_else(|| "lut_preview".into());
    let handle = ctx.load_texture(&name, image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| {
        let mut cache = d.get_temp::<LutPreviewCache>(cache_id)
            .unwrap_or_default();
        cache.0.insert(path.to_path_buf(), handle.clone());
        d.insert_temp(cache_id, cache);
    });
    Some(handle)
}

/// Find the bundled LUTs directory. Checks:
/// 1. Next to the executable (installed/packaged)
/// 2. In the source tree (development)
fn find_bundled_luts_dir() -> Option<std::path::PathBuf> {
    // Next to executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("assets").join("luts");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    // Source tree (CARGO_MANIFEST_DIR at compile time)
    let source_tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .and_then(|p| p.parent()) // workspace root
        .map(|p| p.join("assets").join("luts"));
    if let Some(ref p) = source_tree {
        if p.exists() {
            return source_tree;
        }
    }
    None
}

