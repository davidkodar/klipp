use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, MediaId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Duration,
    Rating,
    Codec,
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
    sort_by: &mut SortBy,
    sort_ascending: &mut bool,
    selected_bin: &mut Option<String>,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();

    // Collect and filter items
    let search_lower = search_query.to_lowercase();
    let mut items: Vec<_> = state
        .media_pool
        .all()
        .filter(|m| {
            if let Some(ref bin) = selected_bin {
                if m.bin.as_ref() != Some(bin) {
                    return false;
                }
            }
            if !search_lower.is_empty() {
                return m.filename.to_lowercase().contains(&search_lower)
                    || m.tags.iter().any(|t| t.to_lowercase().contains(&search_lower));
            }
            true
        })
        .map(|m| MediaRow {
            id: m.id,
            filename: m.filename.clone(),
            duration: m.duration,
            codec: m.video_codec.clone(),
            resolution: m.resolution,
            rating: m.rating,
            _proxy_ready: m.proxy_ready,
            path: m.path.clone(),
            bin: m.bin.clone(),
        })
        .collect();

    // Sort
    match sort_by {
        SortBy::Name => items.sort_by(|a, b| a.filename.to_lowercase().cmp(&b.filename.to_lowercase())),
        SortBy::Duration => items.sort_by(|a, b| a.duration.value.cmp(&b.duration.value)),
        SortBy::Rating => items.sort_by(|a, b| b.rating.cmp(&a.rating)),
        SortBy::Codec => items.sort_by(|a, b| a.codec.cmp(&b.codec)),
    }
    if !*sort_ascending {
        items.reverse();
    }

    // Group by bin
    let mut bins_seen: Vec<Option<String>> = Vec::new();
    for item in &items {
        if !bins_seen.contains(&item.bin) {
            bins_seen.push(item.bin.clone());
        }
    }

    // Render
    egui::ScrollArea::vertical().id_salt("browser_media_scroll").show(ui, |ui| {
        for bin in &bins_seen {
            let bin_label = bin.as_deref().unwrap_or("Ungrouped");
            let bin_items: Vec<_> = items.iter().filter(|i| &i.bin == bin).collect();

            // Folder header
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("\u{1F4C1}")
                        .size(10.0)
                        .color(theme.text_dim),
                );
                ui.label(
                    egui::RichText::new(bin_label.to_uppercase())
                        .size(10.0)
                        .strong()
                        .color(theme.text_secondary),
                );
            });

            // Clip rows
            for item in &bin_items {
                let is_selected = state.source_media_id == Some(item.id);
                let bg = if is_selected {
                    theme.accent_subtle
                } else {
                    egui::Color32::TRANSPARENT
                };

                let resp = ui.horizontal(|ui| {
                    let row_rect = ui.available_rect_before_wrap();
                    let row_rect = egui::Rect::from_min_size(
                        row_rect.min,
                        egui::vec2(row_rect.width(), 38.0),
                    );
                    ui.painter().rect_filled(row_rect, 0.0, bg);

                    ui.add_space(22.0); // indent

                    // Thumbnail (42x26)
                    let (thumb_rect, _) =
                        ui.allocate_exact_size(egui::vec2(42.0, 26.0), egui::Sense::hover());
                    if let Some(tex) = state.thumbnail_textures.get(&item.id) {
                        ui.painter().image(
                            tex.id(),
                            thumb_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    } else {
                        ui.painter().rect_filled(thumb_rect, 3.0, theme.bg_raised);
                    }

                    ui.add_space(8.0);

                    // Info column
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(&item.filename)
                                .size(11.0)
                                .color(theme.text_primary),
                        );
                        let dur_secs = if item.duration.rate > 0 {
                            item.duration.value as f64 / item.duration.rate as f64
                        } else {
                            0.0
                        };
                        let dur_str = format!(
                            "{:02}:{:02}",
                            (dur_secs / 60.0) as u32,
                            (dur_secs % 60.0) as u32
                        );
                        let res_label = match item.resolution.height {
                            2160 => "4K",
                            1440 => "QHD",
                            1080 => "1080p",
                            720 => "720p",
                            480 => "480p",
                            _ => "",
                        };
                        let meta = if res_label.is_empty() {
                            format!(
                                "{} \u{00b7} {}x{} \u{00b7} {}",
                                dur_str, item.resolution.width, item.resolution.height, item.codec
                            )
                        } else {
                            format!(
                                "{} \u{00b7} {} \u{00b7} {}",
                                dur_str, res_label, item.codec
                            )
                        };
                        ui.label(
                            egui::RichText::new(meta)
                                .size(9.0)
                                .monospace()
                                .color(theme.text_dim),
                        );
                    });

                    // Role dot (placeholder — use video role for now)
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(10.0);
                        let (dot_rect, _) =
                            ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                        ui.painter()
                            .circle_filled(dot_rect.center(), 3.0, theme.role_video);

                        // Used indicator
                        let is_used = state
                            .sequence
                            .tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .any(|c| c.media_id() == Some(item.id));
                        if is_used {
                            let (used_rect, _) =
                                ui.allocate_exact_size(egui::vec2(5.0, 5.0), egui::Sense::hover());
                            ui.painter().circle_filled(
                                used_rect.center(),
                                2.5,
                                egui::Color32::from_rgba_premultiplied(232, 160, 32, 153),
                            );
                        }
                    });
                });

                // Interactions
                let row_resp = resp.response.interact(egui::Sense::click_and_drag());
                if row_resp.clicked() {
                    cmds.push(AppCommand::LoadSourceMedia { media_id: item.id });
                }
                if row_resp.double_clicked() {
                    cmds.push(AppCommand::ImportMediaLinked { media_id: item.id });
                }
                if row_resp.dragged() {
                    state.dragging_media = Some(item.id);
                }

                row_resp.context_menu(|ui| {
                    show_context_menu(ui, item.id, &item.path, &mut cmds);
                });
            }
        }

        // Item count
        if items.is_empty() {
            ui.add_space(40.0);
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Drop media here or use File \u{2192} Import")
                        .size(11.0)
                        .color(theme.text_dim),
                );
            });
        }
    });

    cmds
}

struct MediaRow {
    id: MediaId,
    filename: String,
    duration: klipp_core::RationalTime,
    codec: String,
    resolution: klipp_core::Resolution,
    rating: u8,
    _proxy_ready: bool,
    path: std::path::PathBuf,
    bin: Option<String>,
}

fn show_context_menu(
    ui: &mut egui::Ui,
    media_id: MediaId,
    path: &std::path::Path,
    cmds: &mut Vec<AppCommand>,
) {
    if ui.button("Load in Source").clicked() {
        cmds.push(AppCommand::LoadSourceMedia { media_id });
        ui.close_menu();
    }
    if ui.button("Add to Timeline").clicked() {
        cmds.push(AppCommand::ImportMediaLinked { media_id });
        ui.close_menu();
    }
    if ui.button("Generate Proxy").clicked() {
        cmds.push(AppCommand::GenerateProxy { media_id });
        ui.close_menu();
    }
    ui.separator();
    ui.menu_button("Set Rating", |ui| {
        for r in 0..=5u8 {
            let label = if r == 0 {
                "None".to_string()
            } else {
                "\u{2605}".repeat(r as usize)
            };
            if ui.button(&label).clicked() {
                cmds.push(AppCommand::SetMediaRating { media_id, rating: r });
                ui.close_menu();
            }
        }
    });
    ui.separator();
    if ui.button("Remove from Project").clicked() {
        cmds.push(AppCommand::RemoveFromPool { media_id });
        ui.close_menu();
    }
    if ui.button("Reveal in File Manager").clicked() {
        cmds.push(AppCommand::RevealInFileManager {
            path: path.to_path_buf(),
        });
        ui.close_menu();
    }
}
