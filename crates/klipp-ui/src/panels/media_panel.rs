use crate::state::AppState;
use klipp_core::{AppCommand, MediaId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    List,
    Grid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Duration,
    Rating,
    Codec,
}

pub struct MediaPanel {
    pub view_mode: ViewMode,
    pub search_query: String,
    pub sort_by: SortBy,
    pub sort_ascending: bool,
    pub selected_bin: Option<String>,
    pub context_menu_media: Option<MediaId>,
}

impl Default for MediaPanel {
    fn default() -> Self {
        Self {
            view_mode: ViewMode::List,
            search_query: String::new(),
            sort_by: SortBy::Name,
            sort_ascending: true,
            selected_bin: None,
            context_menu_media: None,
        }
    }
}

impl MediaPanel {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // Header row: view mode toggle + search
        ui.horizontal(|ui| {
            let list_color = if self.view_mode == ViewMode::List {
                egui::Color32::from_rgb(180, 180, 200)
            } else {
                egui::Color32::from_rgb(80, 80, 90)
            };
            if ui.add(egui::Button::new(
                egui::RichText::new("List").size(10.0).color(list_color),
            ).min_size(egui::vec2(32.0, 18.0))).clicked() {
                self.view_mode = ViewMode::List;
            }
            let grid_color = if self.view_mode == ViewMode::Grid {
                egui::Color32::from_rgb(180, 180, 200)
            } else {
                egui::Color32::from_rgb(80, 80, 90)
            };
            if ui.add(egui::Button::new(
                egui::RichText::new("Grid").size(10.0).color(grid_color),
            ).min_size(egui::vec2(32.0, 18.0))).clicked() {
                self.view_mode = ViewMode::Grid;
            }
            ui.add(egui::TextEdit::singleline(&mut self.search_query)
                .hint_text("Search...")
                .desired_width(ui.available_width() - 4.0));
        });

        // Bin selector + sort
        ui.horizontal(|ui| {
            // Collect bin names
            let mut bins: Vec<String> = state.media_pool.all()
                .filter_map(|m| m.bin.clone())
                .collect();
            bins.sort();
            bins.dedup();

            let selected_label = self.selected_bin.as_deref().unwrap_or("All");
            egui::ComboBox::from_id_salt("media_bin_select")
                .selected_text(selected_label)
                .width(80.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(self.selected_bin.is_none(), "All").clicked() {
                        self.selected_bin = None;
                    }
                    for bin in &bins {
                        let is_sel = self.selected_bin.as_ref() == Some(bin);
                        if ui.selectable_label(is_sel, bin).clicked() {
                            self.selected_bin = Some(bin.clone());
                        }
                    }
                });

            egui::ComboBox::from_id_salt("media_sort")
                .selected_text(match self.sort_by {
                    SortBy::Name => "Name",
                    SortBy::Duration => "Duration",
                    SortBy::Rating => "Rating",
                    SortBy::Codec => "Codec",
                })
                .width(70.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.sort_by, SortBy::Name, "Name");
                    ui.selectable_value(&mut self.sort_by, SortBy::Duration, "Duration");
                    ui.selectable_value(&mut self.sort_by, SortBy::Rating, "Rating");
                    ui.selectable_value(&mut self.sort_by, SortBy::Codec, "Codec");
                });

            let arrow = if self.sort_ascending { "\u{25B2}" } else { "\u{25BC}" };
            if ui.small_button(arrow).clicked() {
                self.sort_ascending = !self.sort_ascending;
            }
        });

        ui.separator();

        // Collect and filter items
        let search_lower = self.search_query.to_lowercase();
        let mut items: Vec<_> = state.media_pool.all()
            .filter(|m| {
                // Bin filter
                if let Some(ref bin) = self.selected_bin {
                    if m.bin.as_ref() != Some(bin) {
                        return false;
                    }
                }
                // Search filter
                if !search_lower.is_empty() {
                    return m.filename.to_lowercase().contains(&search_lower)
                        || m.tags.iter().any(|t| t.to_lowercase().contains(&search_lower));
                }
                true
            })
            .map(|m| (m.id, m.filename.clone(), m.duration, m.video_codec.clone(), m.rating, m.proxy_ready, m.path.clone()))
            .collect();

        // Sort
        match self.sort_by {
            SortBy::Name => items.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase())),
            SortBy::Duration => items.sort_by(|a, b| a.2.value.cmp(&b.2.value)),
            SortBy::Rating => items.sort_by(|a, b| b.4.cmp(&a.4)),
            SortBy::Codec => items.sort_by(|a, b| a.3.cmp(&b.3)),
        }
        if !self.sort_ascending {
            items.reverse();
        }

        // Render items
        egui::ScrollArea::vertical().show(ui, |ui| {
            match self.view_mode {
                ViewMode::List => {
                    for (media_id, filename, duration, codec, rating, proxy_ready, path) in &items {
                        let dur_secs = if duration.rate > 0 { duration.value as f64 / duration.rate as f64 } else { 0.0 };
                        let dur_str = format!("{}:{:02}", (dur_secs / 60.0) as u32, (dur_secs % 60.0) as u32);

                        let resp = ui.horizontal(|ui| {
                            // Thumbnail placeholder
                            let (thumb_rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 28.0), egui::Sense::hover());
                            if let Some(tex) = state.thumbnail_textures.get(media_id) {
                                ui.painter().image(
                                    tex.id(),
                                    thumb_rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    egui::Color32::WHITE,
                                );
                            } else {
                                ui.painter().rect_filled(thumb_rect, 2.0, egui::Color32::from_rgb(40, 40, 48));
                            }

                            // Name
                            ui.label(egui::RichText::new(filename).size(11.0));

                            // Duration
                            ui.label(egui::RichText::new(&dur_str).size(10.0).color(egui::Color32::from_rgb(120, 120, 130)));

                            // Codec
                            ui.label(egui::RichText::new(codec).size(10.0).color(egui::Color32::from_rgb(100, 100, 110)));

                            // Star rating
                            show_star_rating(ui, *media_id, *rating, &mut cmds);

                            // Proxy badge
                            if *proxy_ready && state.use_proxies {
                                ui.label(egui::RichText::new("P").size(9.0).color(egui::Color32::from_rgb(200, 160, 40)));
                            }
                        });

                        // Interactions
                        let row_resp = resp.response.interact(egui::Sense::click_and_drag());
                        if row_resp.clicked() {
                            cmds.push(AppCommand::LoadSourceMedia { media_id: *media_id });
                        }
                        if row_resp.double_clicked() {
                            cmds.push(AppCommand::ImportMediaLinked { media_id: *media_id });
                        }
                        if row_resp.dragged() {
                            state.dragging_media = Some(*media_id);
                        }

                        // Context menu
                        row_resp.context_menu(|ui| {
                            self.show_context_menu(ui, *media_id, path, &mut cmds);
                        });
                    }
                }
                ViewMode::Grid => {
                    let avail_w = ui.available_width();
                    let thumb_size = 80.0;
                    let cols = ((avail_w / (thumb_size + 8.0)) as usize).max(1);

                    egui::Grid::new("media_grid")
                        .num_columns(cols)
                        .spacing([4.0, 4.0])
                        .show(ui, |ui| {
                            for (i, (media_id, filename, _duration, _codec, rating, _proxy_ready, path)) in items.iter().enumerate() {
                                ui.vertical(|ui| {
                                    // Thumbnail
                                    let (thumb_rect, thumb_resp) = ui.allocate_exact_size(
                                        egui::vec2(thumb_size, thumb_size * 0.5625),
                                        egui::Sense::click_and_drag(),
                                    );
                                    if let Some(tex) = state.thumbnail_textures.get(media_id) {
                                        ui.painter().image(
                                            tex.id(),
                                            thumb_rect,
                                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                            egui::Color32::WHITE,
                                        );
                                    } else {
                                        ui.painter().rect_filled(thumb_rect, 2.0, egui::Color32::from_rgb(40, 40, 48));
                                    }

                                    // Filename (truncated)
                                    let short_name = if filename.len() > 12 {
                                        format!("{}...", &filename[..10])
                                    } else {
                                        filename.clone()
                                    };
                                    ui.label(egui::RichText::new(&short_name).size(9.0));

                                    // Star rating
                                    show_star_rating(ui, *media_id, *rating, &mut cmds);

                                    // Interactions
                                    if thumb_resp.clicked() {
                                        cmds.push(AppCommand::LoadSourceMedia { media_id: *media_id });
                                    }
                                    if thumb_resp.double_clicked() {
                                        cmds.push(AppCommand::ImportMediaLinked { media_id: *media_id });
                                    }
                                    if thumb_resp.dragged() {
                                        state.dragging_media = Some(*media_id);
                                    }

                                    thumb_resp.context_menu(|ui| {
                                        self.show_context_menu(ui, *media_id, path, &mut cmds);
                                    });
                                });

                                if (i + 1) % cols == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                }
            }
        });

        // Status bar
        ui.separator();
        ui.label(egui::RichText::new(format!("{} items", items.len())).size(10.0).color(egui::Color32::from_rgb(100, 100, 110)));

        cmds
    }

    fn show_context_menu(&self, ui: &mut egui::Ui, media_id: MediaId, path: &std::path::Path, cmds: &mut Vec<AppCommand>) {
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
                let label = if r == 0 { "None".to_string() } else { "\u{2605}".repeat(r as usize) };
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
            cmds.push(AppCommand::RevealInFileManager { path: path.to_path_buf() });
            ui.close_menu();
        }
    }
}

fn show_star_rating(ui: &mut egui::Ui, media_id: MediaId, current: u8, cmds: &mut Vec<AppCommand>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for i in 1..=5u8 {
            let star = if i <= current { "\u{2605}" } else { "\u{2606}" };
            let color = if i <= current {
                egui::Color32::from_rgb(220, 180, 40)
            } else {
                egui::Color32::from_rgb(80, 80, 90)
            };
            if ui.add(egui::Label::new(egui::RichText::new(star).size(11.0).color(color))
                .sense(egui::Sense::click())).clicked()
            {
                let new_rating = if i == current { 0 } else { i };
                cmds.push(AppCommand::SetMediaRating { media_id, rating: new_rating });
            }
        }
    });
}
