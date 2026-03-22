use crate::prefs::RecentProject;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use std::collections::HashMap;
use std::path::PathBuf;

const CARD_W: f32 = 220.0;
const CARD_H: f32 = 160.0;
const CARD_GAP: f32 = 16.0;
const SIDEBAR_W: f32 = 220.0;

#[derive(Default)]
pub struct LaunchScreen {
    search_query: String,
    thumbnail_textures: HashMap<PathBuf, egui::TextureHandle>,
    /// Project being renamed: (path, new name buffer)
    renaming_project: Option<(PathBuf, String)>,
}

impl LaunchScreen {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        recent: &[RecentProject],
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let full_rect = ui.max_rect();

        // Fill background
        ui.painter()
            .rect_filled(full_rect, 0.0, theme.bg_void);

        // Sidebar (left)
        let sidebar_rect = egui::Rect::from_min_size(
            full_rect.min,
            egui::vec2(SIDEBAR_W, full_rect.height()),
        );

        // Main area (right of sidebar)
        let main_rect = egui::Rect::from_min_max(
            egui::pos2(full_rect.min.x + SIDEBAR_W + 1.0, full_rect.min.y),
            full_rect.max,
        );

        // Draw sidebar
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_rect), |ui| {
            ui.painter()
                .rect_filled(sidebar_rect, 0.0, theme.bg_deep);
            ui.painter().vline(
                sidebar_rect.max.x,
                sidebar_rect.y_range(),
                egui::Stroke::new(1.0, theme.border),
            );

            self.draw_sidebar(ui, recent, theme, &mut cmds);
        });

        // Draw main area with recent project grid
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(main_rect), |ui| {
            self.draw_main_area(ctx, ui, recent, theme, &mut cmds);
        });

        // Rename project popup
        if let Some((ref rename_path, _)) = self.renaming_project.clone() {
            let mut open = true;
            let mut done = false;
            egui::Window::new("Rename Project")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        let buf = &mut self.renaming_project.as_mut().unwrap().1;
                        let resp = ui.text_edit_singleline(buf);
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            done = true;
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("Rename").clicked() {
                            done = true;
                        }
                        if ui.button("Cancel").clicked() {
                            self.renaming_project = None;
                        }
                    });
                });
            if !open {
                self.renaming_project = None;
            }
            if done {
                let new_name = self.renaming_project.as_ref()
                    .map(|(_, buf)| buf.trim().to_string())
                    .unwrap_or_default();
                if !new_name.is_empty() {
                    cmds.push(AppCommand::RenameProjectOnDisk {
                        old_path: rename_path.clone(),
                        new_name,
                    });
                }
                self.renaming_project = None;
            }
        }

        cmds
    }

    fn draw_sidebar(
        &self,
        ui: &mut egui::Ui,
        recent: &[RecentProject],
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let area = ui.max_rect();

        // Logo — larger version for launch screen
        let logo_color = theme.accent;
        let logo_center = egui::pos2(area.center().x, area.min.y + 60.0);
        let scale = 2.5;
        let shield_pts = vec![
            egui::pos2(logo_center.x - 12.0 * scale, logo_center.y - 24.0 * scale),
            egui::pos2(logo_center.x, logo_center.y - 30.0 * scale),
            egui::pos2(logo_center.x + 12.0 * scale, logo_center.y - 24.0 * scale),
            egui::pos2(logo_center.x + 12.0 * scale, logo_center.y - 8.0 * scale),
            egui::pos2(logo_center.x, logo_center.y),
            egui::pos2(logo_center.x - 12.0 * scale, logo_center.y - 8.0 * scale),
        ];
        ui.painter().add(egui::Shape::convex_polygon(
            shield_pts,
            egui::Color32::TRANSPARENT,
            egui::Stroke::new(2.0, logo_color),
        ));
        ui.painter().text(
            egui::pos2(logo_center.x, logo_center.y + 20.0),
            egui::Align2::CENTER_CENTER,
            "Klipp",
            egui::FontId::proportional(28.0),
            logo_color,
        );

        // Buttons
        let buttons_y = logo_center.y + 60.0;
        let button_w = SIDEBAR_W - 40.0;

        // New Project button
        let new_rect = egui::Rect::from_min_size(
            egui::pos2(area.min.x + 20.0, buttons_y),
            egui::vec2(button_w, 36.0),
        );
        let new_resp = ui.allocate_rect(new_rect, egui::Sense::click());
        let new_bg = if new_resp.hovered() {
            theme.accent
        } else {
            theme.accent_dim
        };
        ui.painter()
            .rect_filled(new_rect, 4.0, new_bg);
        ui.painter().text(
            new_rect.center(),
            egui::Align2::CENTER_CENTER,
            "New Project",
            egui::FontId::proportional(13.0),
            theme.bg_void,
        );
        if new_resp.clicked() {
            cmds.push(AppCommand::NewProject);
        }

        // Open Project button
        let open_rect = egui::Rect::from_min_size(
            egui::pos2(area.min.x + 20.0, buttons_y + 44.0),
            egui::vec2(button_w, 36.0),
        );
        let open_resp = ui.allocate_rect(open_rect, egui::Sense::click());
        let open_bg = if open_resp.hovered() {
            theme.bg_hover
        } else {
            theme.bg_raised
        };
        ui.painter()
            .rect_filled(open_rect, 4.0, open_bg);
        ui.painter().rect_stroke(
            open_rect,
            4.0,
            egui::Stroke::new(1.0, theme.border_light),
        );
        ui.painter().text(
            open_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Open Project...",
            egui::FontId::proportional(13.0),
            theme.text_primary,
        );
        if open_resp.clicked() {
            if let Some(p) = rfd::FileDialog::new()
                .set_title("Open Project")
                .pick_folder()
            {
                cmds.push(AppCommand::OpenProject { path: p });
            }
        }

        // Recent projects quick list (last 5)
        let list_y = buttons_y + 100.0;
        if !recent.is_empty() {
            ui.painter().text(
                egui::pos2(area.min.x + 20.0, list_y),
                egui::Align2::LEFT_TOP,
                "Recent",
                egui::FontId::proportional(11.0),
                theme.text_dim,
            );

            for (i, rp) in recent.iter().take(5).enumerate() {
                let y = list_y + 22.0 + i as f32 * 26.0;
                let item_rect = egui::Rect::from_min_size(
                    egui::pos2(area.min.x + 16.0, y),
                    egui::vec2(button_w + 8.0, 24.0),
                );
                let resp = ui.allocate_rect(item_rect, egui::Sense::click());
                if resp.hovered() {
                    ui.painter()
                        .rect_filled(item_rect, 3.0, theme.bg_hover);
                }
                ui.painter().text(
                    egui::pos2(area.min.x + 24.0, y + 12.0),
                    egui::Align2::LEFT_CENTER,
                    &rp.name,
                    egui::FontId::proportional(12.0),
                    if resp.hovered() {
                        theme.text_primary
                    } else {
                        theme.text_secondary
                    },
                );
                if resp.clicked() {
                    cmds.push(AppCommand::OpenProject {
                        path: rp.path.clone(),
                    });
                }
            }
        }
    }

    fn draw_main_area(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        recent: &[RecentProject],
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let area = ui.max_rect();
        let pad = 24.0;

        // Title
        ui.painter().text(
            egui::pos2(area.min.x + pad, area.min.y + pad),
            egui::Align2::LEFT_TOP,
            "Recent Projects",
            egui::FontId::proportional(18.0),
            theme.text_primary,
        );

        // Search bar
        let search_rect = egui::Rect::from_min_size(
            egui::pos2(area.min.x + pad, area.min.y + pad + 30.0),
            egui::vec2((area.width() - pad * 2.0).min(400.0), 24.0),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(search_rect), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .hint_text("Search recent projects...")
                    .desired_width(search_rect.width()),
            );
        });

        // Filter recent projects by search query
        let query = self.search_query.to_lowercase();
        let filtered: Vec<&RecentProject> = recent
            .iter()
            .filter(|rp| query.is_empty() || rp.name.to_lowercase().contains(&query))
            .collect();

        if filtered.is_empty() {
            let msg = if recent.is_empty() {
                "No recent projects"
            } else {
                "No matching projects"
            };
            ui.painter().text(
                egui::pos2(area.center().x, area.center().y),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(14.0),
                theme.text_dim,
            );
            return;
        }

        // Group projects by workspace
        let mut workspace_groups: HashMap<PathBuf, (String, Vec<&RecentProject>)> = HashMap::new();
        let mut standalone: Vec<&RecentProject> = Vec::new();

        for rp in &filtered {
            if let Some(parent) = rp.path.parent() {
                if klipp_project::workspace::is_workspace(parent) {
                    let ws_name = klipp_project::workspace::load_workspace(parent)
                        .map(|w| w.name)
                        .unwrap_or_else(|| parent.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "Workspace".into()));
                    workspace_groups
                        .entry(parent.to_path_buf())
                        .or_insert_with(|| (ws_name, Vec::new()))
                        .1
                        .push(rp);
                } else {
                    standalone.push(rp);
                }
            } else {
                standalone.push(rp);
            }
        }

        // Card grid
        let grid_top = area.min.y + pad + 66.0;
        let grid_width = area.width() - pad * 2.0;
        let cols = ((grid_width + CARD_GAP) / (CARD_W + CARD_GAP)).floor().max(1.0) as usize;

        let scroll_area = egui::Rect::from_min_max(
            egui::pos2(area.min.x, grid_top),
            area.max,
        );

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(scroll_area), |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let mut card_idx = 0usize;

                // Workspace groups first
                let mut ws_keys: Vec<_> = workspace_groups.keys().cloned().collect();
                ws_keys.sort();
                for ws_path in &ws_keys {
                    let (ws_name, projects) = &workspace_groups[ws_path];
                    let row = card_idx / cols;
                    let y_offset = row as f32 * (CARD_H + CARD_GAP);

                    // Workspace header
                    let header_rect = egui::Rect::from_min_size(
                        ui.min_rect().min + egui::vec2(pad, y_offset),
                        egui::vec2(grid_width, 22.0),
                    );
                    ui.allocate_at_least(
                        egui::vec2(grid_width, y_offset + 22.0),
                        egui::Sense::hover(),
                    );
                    // Folder icon + workspace name
                    ui.painter().text(
                        header_rect.left_center() + egui::vec2(0.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        format!("\u{1F4C1} {}  \u{2014} {} project(s)", ws_name, projects.len()),
                        egui::FontId::proportional(12.0),
                        theme.text_secondary,
                    );

                    // Bump card_idx to next row for the cards
                    card_idx = (card_idx / cols + 1) * cols;

                    for rp in projects {
                        let col = card_idx % cols;
                        let row = card_idx / cols;
                        let x = pad + col as f32 * (CARD_W + CARD_GAP);
                        let y = row as f32 * (CARD_H + CARD_GAP);

                        let card_rect = egui::Rect::from_min_size(
                            ui.min_rect().min + egui::vec2(x, y),
                            egui::vec2(CARD_W, CARD_H),
                        );
                        ui.allocate_at_least(
                            egui::vec2(grid_width, (row + 1) as f32 * (CARD_H + CARD_GAP)),
                            egui::Sense::hover(),
                        );
                        self.draw_card(ctx, ui, card_rect, rp, theme, cmds);
                        card_idx += 1;
                    }

                    // Start next group on a fresh row
                    card_idx = card_idx.div_ceil(cols) * cols;
                }

                // Standalone projects
                if !standalone.is_empty() && !workspace_groups.is_empty() {
                    // "Other Projects" header if there are workspace groups
                    let row = card_idx / cols;
                    let y_offset = row as f32 * (CARD_H + CARD_GAP);
                    let header_rect = egui::Rect::from_min_size(
                        ui.min_rect().min + egui::vec2(pad, y_offset),
                        egui::vec2(grid_width, 22.0),
                    );
                    ui.allocate_at_least(
                        egui::vec2(grid_width, y_offset + 22.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().text(
                        header_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        "Other Projects",
                        egui::FontId::proportional(12.0),
                        theme.text_secondary,
                    );
                    card_idx = (card_idx / cols + 1) * cols;
                }

                for rp in &standalone {
                    let col = card_idx % cols;
                    let row = card_idx / cols;
                    let x = pad + col as f32 * (CARD_W + CARD_GAP);
                    let y = row as f32 * (CARD_H + CARD_GAP);

                    let card_rect = egui::Rect::from_min_size(
                        ui.min_rect().min + egui::vec2(x, y),
                        egui::vec2(CARD_W, CARD_H),
                    );
                    ui.allocate_at_least(
                        egui::vec2(grid_width, (row + 1) as f32 * (CARD_H + CARD_GAP)),
                        egui::Sense::hover(),
                    );
                    self.draw_card(ctx, ui, card_rect, rp, theme, cmds);
                    card_idx += 1;
                }
            });
        });
    }

    fn draw_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        rp: &RecentProject,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let resp = ui.allocate_rect(rect, egui::Sense::click());
        let hovered = resp.hovered();

        // Card background
        let bg = if hovered { theme.bg_hover } else { theme.bg_raised };
        let stroke = if hovered {
            egui::Stroke::new(1.5, theme.accent)
        } else {
            egui::Stroke::new(1.0, theme.border)
        };
        ui.painter().rect(rect, 6.0, bg, stroke);

        // Thumbnail area (top 2/3)
        let thumb_rect = egui::Rect::from_min_size(
            rect.min + egui::vec2(1.0, 1.0),
            egui::vec2(rect.width() - 2.0, rect.height() * 0.65),
        );

        // Try to load/show thumbnail
        let has_thumb = if let Some(ref thumb_path) = rp.thumbnail_path {
            if let Some(tex) = self.thumbnail_textures.get(thumb_path) {
                ui.painter().image(
                    tex.id(),
                    thumb_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                true
            } else if thumb_path.exists() {
                // Load the texture
                if let Ok(data) = std::fs::read(thumb_path) {
                    if let Ok(img) = image_from_png(&data) {
                        let tex = ctx.load_texture(
                            thumb_path.to_string_lossy(),
                            img,
                            egui::TextureOptions::LINEAR,
                        );
                        self.thumbnail_textures.insert(thumb_path.clone(), tex);
                    }
                }
                false
            } else {
                false
            }
        } else {
            false
        };

        if !has_thumb {
            // Draw small Klipp shield as placeholder
            let center = thumb_rect.center();
            let logo_color = egui::Color32::from_rgba_premultiplied(232, 160, 32, 60);
            let s = 1.2;
            let shield_pts = vec![
                egui::pos2(center.x - 12.0 * s, center.y - 24.0 * s),
                egui::pos2(center.x, center.y - 30.0 * s),
                egui::pos2(center.x + 12.0 * s, center.y - 24.0 * s),
                egui::pos2(center.x + 12.0 * s, center.y - 8.0 * s),
                egui::pos2(center.x, center.y),
                egui::pos2(center.x - 12.0 * s, center.y - 8.0 * s),
            ];
            ui.painter().add(egui::Shape::convex_polygon(
                shield_pts,
                egui::Color32::TRANSPARENT,
                egui::Stroke::new(1.5, logo_color),
            ));
        }

        // Info area (bottom 1/3)
        let info_top = rect.min.y + rect.height() * 0.65 + 6.0;

        // Project name
        ui.painter().text(
            egui::pos2(rect.min.x + 10.0, info_top),
            egui::Align2::LEFT_TOP,
            &rp.name,
            egui::FontId::proportional(13.0),
            theme.text_primary,
        );

        // Metadata line
        let mut meta_parts = Vec::new();
        if let Some(ref fps) = rp.fps {
            meta_parts.push(fps.clone());
        }
        if let Some(ref res) = rp.resolution {
            meta_parts.push(res.clone());
        }
        if let Some(dur) = rp.duration_secs {
            let mins = (dur / 60.0).floor() as u32;
            let secs = (dur % 60.0).floor() as u32;
            meta_parts.push(format!("{mins}:{secs:02}"));
        }
        if let Some(size) = rp.total_size_bytes {
            meta_parts.push(format_size_compact(size));
        }
        if !meta_parts.is_empty() {
            ui.painter().text(
                egui::pos2(rect.min.x + 10.0, info_top + 18.0),
                egui::Align2::LEFT_TOP,
                meta_parts.join(" \u{00B7} "),
                egui::FontId::monospace(10.0),
                theme.text_dim,
            );
        }

        // Last saved date
        if let Some(ref saved) = rp.last_saved {
            // Show just the date part (first 10 chars of ISO 8601)
            let date_str = if saved.len() >= 10 { &saved[..10] } else { saved };
            ui.painter().text(
                egui::pos2(rect.min.x + 10.0, info_top + 34.0),
                egui::Align2::LEFT_TOP,
                date_str,
                egui::FontId::proportional(10.0),
                theme.text_dim,
            );
        }

        // Click to open
        if resp.clicked() {
            cmds.push(AppCommand::OpenProject {
                path: rp.path.clone(),
            });
        }

        // Right-click context menu
        resp.context_menu(|ui| {
            if ui.button("Open").clicked() {
                cmds.push(AppCommand::OpenProject {
                    path: rp.path.clone(),
                });
                ui.close_menu();
            }
            if ui.button("Rename...").clicked() {
                self.renaming_project = Some((rp.path.clone(), rp.name.clone()));
                ui.close_menu();
            }
            if ui.button("Remove from Recent").clicked() {
                cmds.push(AppCommand::RemoveRecentProject {
                    path: rp.path.clone(),
                });
                ui.close_menu();
            }
            ui.separator();
            if ui
                .button(egui::RichText::new("Delete Project").color(egui::Color32::from_rgb(200, 60, 60)))
                .clicked()
            {
                cmds.push(AppCommand::DeleteProject {
                    path: rp.path.clone(),
                });
                ui.close_menu();
            }
        });
    }
}

fn format_size_compact(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Decode a PNG file into an egui ColorImage.
fn image_from_png(data: &[u8]) -> Result<egui::ColorImage, String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let bytes = &buf[..info.buffer_size()];

    let w = info.width as usize;
    let h = info.height as usize;

    let pixels: Vec<egui::Color32> = match info.color_type {
        png::ColorType::Rgb => bytes
            .chunks_exact(3)
            .map(|c| egui::Color32::from_rgb(c[0], c[1], c[2]))
            .collect(),
        png::ColorType::Rgba => bytes
            .chunks_exact(4)
            .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
            .collect(),
        _ => return Err("Unsupported PNG color type".into()),
    };

    Ok(egui::ColorImage {
        size: [w, h],
        pixels,
    })
}
