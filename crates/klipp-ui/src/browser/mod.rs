pub mod media_tab;
pub mod effect_registry;
pub mod effect_thumbnails;
pub mod effects_tab;
pub mod audio_fx_tab;
pub mod titles_tab;
pub mod transitions_tab;

use crate::icons::Icon;
use crate::layout_state::{BrowserTab, LibraryCategory};
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

pub struct BrowserPanel {
    pub search_query: String,
    // Media tab state (migrated from MediaPanel)
    pub media_sort_by: media_tab::SortBy,
    pub media_sort_ascending: bool,
    pub media_selected_bin: Option<String>,
    pub browser_view_mode: media_tab::BrowserViewMode,
    // Bin management state
    pub renaming_bin: Option<(String, String)>,  // (old_name, current_edit_text)
    pub creating_bin: Option<String>,             // new bin name being typed
    // Effects tab state
    pub effects_category: effects_tab::EffectCategory,
    // Library tab category
    pub library_category: LibraryCategory,
    // Scroll-to request from RevealMediaInBrowser
    pub scroll_to_media_id: Option<klipp_core::MediaId>,
    // Files tab state
    pub files_current_path: std::path::PathBuf,
    pub files_entries: Vec<FileEntry>,
    pub files_entries_dirty: bool,
    pub files_loading: bool,
    pub files_error: Option<String>,
    pub files_receiver: Option<std::sync::mpsc::Receiver<Result<Vec<FileEntry>, String>>>,
}

#[derive(Clone)]
pub struct FileEntry {
    pub name: String,
    pub path: std::path::PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub is_media: bool,
}

impl Default for BrowserPanel {
    fn default() -> Self {
        Self {
            search_query: String::new(),
            media_sort_by: media_tab::SortBy::Name,
            media_sort_ascending: true,
            media_selected_bin: None,
            browser_view_mode: media_tab::BrowserViewMode::List,
            renaming_bin: None,
            creating_bin: None,
            effects_category: effects_tab::EffectCategory::All,
            library_category: LibraryCategory::default(),
            scroll_to_media_id: None,
            files_current_path: dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")),
            files_entries: Vec::new(),
            files_entries_dirty: true,
            files_loading: false,
            files_error: None,
            files_receiver: None,
        }
    }
}

impl BrowserPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        active_tab: &mut BrowserTab,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // ── Tab bar ──
        let tab_rect = ui.available_rect_before_wrap();
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                tab_rect.min,
                egui::vec2(tab_rect.width(), 30.0),
            )),
            |ui| {
                ui.painter().rect_filled(
                    ui.max_rect(),
                    0.0,
                    theme.bg_deep,
                );
                ui.painter().hline(
                    ui.max_rect().x_range(),
                    ui.max_rect().max.y,
                    egui::Stroke::new(1.0, theme.border),
                );
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let tabs = [
                        (BrowserTab::Files, "FILES"),
                        (BrowserTab::Media, "MEDIA"),
                        (BrowserTab::Library, "LIBRARY"),
                    ];
                    for (tab, label) in &tabs {
                        let is_active = *active_tab == *tab;

                        // Allocate tab rect manually to avoid egui button hover styling
                        let text_galley = ui.painter().layout_no_wrap(
                            label.to_string(),
                            egui::FontId::proportional(10.0),
                            theme.text_dim,
                        );
                        let tab_w = text_galley.size().x + 24.0;
                        let (tab_rect, resp) = ui.allocate_exact_size(
                            egui::vec2(tab_w, 28.0),
                            egui::Sense::click(),
                        );

                        let text_color = if is_active {
                            theme.text_accent
                        } else if resp.hovered() {
                            theme.text_secondary
                        } else {
                            theme.text_dim
                        };

                        ui.painter().text(
                            tab_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            *label,
                            egui::FontId::proportional(10.0),
                            text_color,
                        );

                        if is_active {
                            ui.painter().hline(
                                tab_rect.x_range(),
                                tab_rect.max.y - 1.0,
                                egui::Stroke::new(2.0, theme.accent),
                            );
                        }

                        if resp.clicked() {
                            *active_tab = *tab;
                        }
                    }
                });
            },
        );

        ui.add_space(30.0); // Skip past tab bar

        // ── Search bar ──
        let search_placeholder = match active_tab {
            BrowserTab::Files => "Search files\u{2026}",
            BrowserTab::Media => "Search media\u{2026}",
            BrowserTab::Library => match self.library_category {
                LibraryCategory::VideoFx => "Search effects\u{2026}",
                LibraryCategory::AudioFx => "Search audio effects\u{2026}",
                LibraryCategory::Titles => "Search titles\u{2026}",
                LibraryCategory::Transitions => "Search transitions\u{2026}",
            },
        };

        // Styled search input + view mode toggle (for Media tab)
        ui.horizontal(|ui| {
            ui.add_space(8.0);

            // View mode toggle (only on Media tab)
            if *active_tab == BrowserTab::Media {
                let list_active = self.browser_view_mode == media_tab::BrowserViewMode::List;
                let grid_active = self.browser_view_mode == media_tab::BrowserViewMode::Grid;

                // List button
                let list_color = if list_active { theme.text_accent } else { theme.text_dim };
                let list_resp = ui.add(
                    egui::Button::image(Icon::List.image(14.0).tint(list_color))
                        .min_size(egui::vec2(24.0, 24.0))
                        .frame(false),
                );
                if list_resp.clicked() {
                    self.browser_view_mode = media_tab::BrowserViewMode::List;
                }

                // Grid button
                let grid_color = if grid_active { theme.text_accent } else { theme.text_dim };
                let grid_resp = ui.add(
                    egui::Button::image(Icon::GridFour.image(14.0).tint(grid_color))
                        .min_size(egui::vec2(24.0, 24.0))
                        .frame(false),
                );
                if grid_resp.clicked() {
                    self.browser_view_mode = media_tab::BrowserViewMode::Grid;
                }

                // Sort dropdown
                let sort_label = match self.media_sort_by {
                    media_tab::SortBy::Name => "Name",
                    media_tab::SortBy::Duration => "Duration",
                    media_tab::SortBy::Rating => "Rating",
                    media_tab::SortBy::Codec => "Codec",
                    media_tab::SortBy::FileSize => "Size",
                    media_tab::SortBy::DateAdded => "Date",
                    media_tab::SortBy::ShootDate => "Shot",
                };
                egui::ComboBox::from_id_salt("browser_sort")
                    .selected_text(
                        egui::RichText::new(sort_label).size(10.0).color(theme.text_secondary),
                    )
                    .width(55.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.media_sort_by, media_tab::SortBy::Name, "Name");
                        ui.selectable_value(&mut self.media_sort_by, media_tab::SortBy::Duration, "Duration");
                        ui.selectable_value(&mut self.media_sort_by, media_tab::SortBy::DateAdded, "Date Added");
                        ui.selectable_value(&mut self.media_sort_by, media_tab::SortBy::FileSize, "File Size");
                        ui.selectable_value(&mut self.media_sort_by, media_tab::SortBy::Rating, "Rating");
                        ui.selectable_value(&mut self.media_sort_by, media_tab::SortBy::ShootDate, "Shoot Date");
                        ui.selectable_value(&mut self.media_sort_by, media_tab::SortBy::Codec, "Codec");
                    });

                // Sort direction toggle
                let sort_icon = if self.media_sort_ascending { Icon::SortAscending } else { Icon::SortDescending };
                if ui.add(
                    egui::Button::image(sort_icon.image(12.0).tint(theme.text_dim))
                        .min_size(egui::vec2(16.0, 24.0))
                        .frame(false),
                ).clicked() {
                    self.media_sort_ascending = !self.media_sort_ascending;
                }

                ui.add_space(4.0);
            }

            egui::Frame::none()
                .fill(theme.bg_raised)
                .stroke(egui::Stroke::new(1.0, theme.border))
                .rounding(5.0)
                .inner_margin(egui::Margin::symmetric(9.0, 5.0))
                .show(ui, |ui| {
                    let search = egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text(search_placeholder)
                        .desired_width(ui.available_width() - 4.0)
                        .font(egui::FontId::proportional(12.0))
                        .text_color(theme.text_primary)
                        .frame(false);
                    let resp = ui.add(search);
                    if resp.has_focus() {
                        let frame_rect = resp.rect.expand2(egui::vec2(9.0, 5.0));
                        ui.painter().rect_stroke(
                            frame_rect,
                            5.0,
                            egui::Stroke::new(1.0, theme.accent_dim),
                        );
                    }
                });
        });
        ui.add_space(4.0);

        // ── Tab content ──
        match active_tab {
            BrowserTab::Files => {
                let file_cmds = self.show_files_tab(ui, state, theme);
                cmds.extend(file_cmds);
            }
            BrowserTab::Media => {
                let tab_cmds = media_tab::show(
                    ui,
                    state,
                    theme,
                    &self.search_query,
                    &mut self.media_sort_by,
                    &mut self.media_sort_ascending,
                    &mut self.media_selected_bin,
                    &self.browser_view_mode,
                    &mut self.renaming_bin,
                    &mut self.creating_bin,
                    &mut self.scroll_to_media_id,
                );
                cmds.extend(tab_cmds);
            }
            BrowserTab::Library => {
                let tab_cmds = self.show_library_tab(ui, state, theme);
                cmds.extend(tab_cmds);
            }
        }

        cmds
    }

    fn show_files_tab(&mut self, ui: &mut egui::Ui, _state: &mut AppState, theme: &KlippTheme) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let pad = 10.0;

        // Check for async directory read result
        if let Some(ref rx) = self.files_receiver {
            match rx.try_recv() {
                Ok(Ok(entries)) => {
                    self.files_entries = entries;
                    self.files_loading = false;
                    self.files_error = None;
                    self.files_receiver = None;
                }
                Ok(Err(err)) => {
                    self.files_entries.clear();
                    self.files_loading = false;
                    self.files_error = Some(err);
                    self.files_receiver = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still loading
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Thread died — treat as error
                    self.files_loading = false;
                    self.files_error = Some("Directory read failed".into());
                    self.files_receiver = None;
                }
            }
        }

        // Start async directory read if needed
        if self.files_entries_dirty && !self.files_loading {
            self.files_entries_dirty = false;
            self.files_loading = true;
            self.files_error = None;
            let path = self.files_current_path.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            self.files_receiver = Some(rx);
            std::thread::Builder::new()
                .name("files-browser-read".into())
                .spawn(move || {
                    let result = read_directory_with_timeout(&path);
                    let _ = tx.send(result);
                })
                .ok();
        }

        // ── Breadcrumb / path bar ──
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(pad);

            // Back button
            let can_go_up = self.files_current_path.parent().is_some()
                && self.files_current_path != std::path::Path::new("/");
            let back_resp = ui.add_enabled(
                can_go_up,
                egui::Button::new(
                    egui::RichText::new("<").size(12.0).color(if can_go_up { theme.text_primary } else { theme.text_dim })
                ).fill(theme.bg_raised).rounding(3.0).min_size(egui::vec2(24.0, 20.0)),
            );
            if back_resp.clicked() {
                if let Some(parent) = self.files_current_path.parent() {
                    self.files_current_path = parent.to_path_buf();
                    self.files_entries_dirty = true;
                }
            }

            // Home button
            let home_resp = ui.add(
                egui::Button::new(
                    egui::RichText::new("~").size(12.0).color(theme.text_primary)
                ).fill(theme.bg_raised).rounding(3.0).min_size(egui::vec2(24.0, 20.0)),
            );
            if home_resp.clicked() {
                self.files_current_path = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
                self.files_entries_dirty = true;
            }

            // Current path display
            let path_str = self.files_current_path.to_string_lossy();
            let short_path = if path_str.len() > 35 {
                format!("...{}", &path_str[path_str.len()-32..])
            } else {
                path_str.to_string()
            };
            ui.label(
                egui::RichText::new(&short_path)
                    .size(10.0)
                    .color(theme.text_dim)
                    .monospace(),
            );
        });

        // ── Quick bookmarks ──
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(pad);
            let bookmarks = [
                ("Home", dirs::home_dir()),
                ("Desktop", dirs::desktop_dir()),
                ("Videos", dirs::video_dir()),
                ("Pictures", dirs::picture_dir()),
                ("Downloads", dirs::download_dir()),
            ];
            for (label, dir) in &bookmarks {
                if let Some(ref path) = dir {
                    let is_current = *path == self.files_current_path;
                    let btn = ui.add(
                        egui::Button::new(
                            egui::RichText::new(*label).size(9.0)
                                .color(if is_current { theme.accent } else { theme.text_secondary })
                        )
                        .fill(if is_current { theme.accent_subtle } else { egui::Color32::TRANSPARENT })
                        .stroke(egui::Stroke::new(0.5, if is_current { theme.accent } else { theme.border }))
                        .rounding(3.0),
                    );
                    if btn.clicked() {
                        self.files_current_path = path.clone();
                        self.files_entries_dirty = true;
                    }
                }
            }
        });

        ui.add_space(4.0);
        ui.painter().hline(
            ui.max_rect().x_range(),
            ui.cursor().min.y,
            egui::Stroke::new(0.5, theme.border),
        );
        ui.add_space(2.0);

        // ── File listing ──
        let search = self.search_query.to_lowercase();
        let filtered: Vec<&FileEntry> = self.files_entries.iter()
            .filter(|e| search.is_empty() || e.name.to_lowercase().contains(&search))
            .collect();

        egui::ScrollArea::vertical().id_salt("files_scroll").show(ui, |ui| {
            // Error state
            if let Some(ref err) = self.files_error {
                ui.add_space(16.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("Cannot access folder").size(12.0).color(egui::Color32::from_rgb(210, 60, 60)));
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(err.as_str()).size(10.0).color(theme.text_dim));
                    ui.add_space(8.0);
                    if ui.button("Go Home").clicked() {
                        self.files_current_path = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
                        self.files_entries_dirty = true;
                    }
                });
                return;
            }

            // Loading state
            if self.files_loading {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Loading...").size(11.0).color(theme.text_dim));
                });
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }

            if filtered.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(if search.is_empty() { "Empty folder" } else { "No matches" })
                            .size(12.0)
                            .color(theme.text_dim),
                    );
                });
            }

            for entry in &filtered {
                let row_h = 28.0;
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_h),
                    egui::Sense::click(),
                );

                // Hover highlight
                if resp.hovered() {
                    ui.painter().rect_filled(rect, 2.0, theme.bg_hover);
                }

                // Icon
                let icon_color = if entry.is_dir {
                    theme.accent
                } else if entry.is_media {
                    theme.text_primary
                } else {
                    theme.text_dim
                };

                let icon = if entry.is_dir { Icon::Folder } else { Icon::FilmStrip };
                let icon_rect = egui::Rect::from_center_size(
                    egui::pos2(rect.min.x + pad + 8.0, rect.center().y),
                    egui::vec2(14.0, 14.0),
                );
                icon.image(14.0).tint(icon_color).paint_at(ui, icon_rect);

                // Name
                let name_color = if entry.is_dir {
                    theme.text_primary
                } else if entry.is_media {
                    theme.text_primary
                } else {
                    theme.text_dim
                };
                ui.painter().text(
                    egui::pos2(rect.min.x + pad + 22.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &entry.name,
                    egui::FontId::proportional(11.0),
                    name_color,
                );

                // Size (for files)
                if !entry.is_dir && entry.size > 0 {
                    let size_str = format_file_size(entry.size);
                    ui.painter().text(
                        egui::pos2(rect.max.x - pad, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        &size_str,
                        egui::FontId::proportional(9.0),
                        theme.text_dim,
                    );
                }

                // Separator
                ui.painter().hline(
                    rect.min.x + pad..=rect.max.x - pad,
                    rect.max.y,
                    egui::Stroke::new(0.3, theme.border),
                );

                // Interactions
                if resp.double_clicked() {
                    if entry.is_dir {
                        self.files_current_path = entry.path.clone();
                        self.files_entries_dirty = true;
                    } else if entry.is_media {
                        // Import into media pool
                        cmds.push(AppCommand::LoadMediaFile {
                            path: entry.path.clone(),
                            media_id: klipp_core::MediaId::new(),
                        });
                    }
                }
                if resp.clicked() && entry.is_dir {
                    // Single click on folder navigates into it
                    self.files_current_path = entry.path.clone();
                    self.files_entries_dirty = true;
                }

                // Tooltip
                resp.on_hover_text(entry.path.to_string_lossy().as_ref());
            }
        });

        cmds
    }

    fn show_library_tab(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        // ── Category pill bar ──
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let categories = [
                (LibraryCategory::VideoFx, "Video FX"),
                (LibraryCategory::AudioFx, "Audio FX"),
                (LibraryCategory::Titles, "Titles"),
                (LibraryCategory::Transitions, "Transitions"),
            ];
            for (cat, label) in &categories {
                let is_active = self.library_category == *cat;
                let bg = if is_active { theme.accent_dim } else { theme.bg_raised };
                let text_color = if is_active { theme.text_accent } else { theme.text_secondary };

                let resp = ui.add(
                    egui::Button::new(
                        egui::RichText::new(*label).size(10.0).color(text_color),
                    )
                    .fill(bg)
                    .rounding(12.0)
                    .min_size(egui::vec2(0.0, 22.0))
                    .stroke(if is_active {
                        egui::Stroke::new(1.0, theme.accent)
                    } else {
                        egui::Stroke::new(1.0, theme.border)
                    }),
                );
                if resp.clicked() {
                    self.library_category = *cat;
                }
            }
        });
        ui.add_space(4.0);

        // ── Category content ──
        match self.library_category {
            LibraryCategory::VideoFx => {
                effects_tab::show(
                    ui,
                    state,
                    theme,
                    &self.search_query,
                    &mut self.effects_category,
                )
            }
            LibraryCategory::AudioFx => {
                audio_fx_tab::show(ui, state, theme, &self.search_query)
            }
            LibraryCategory::Titles => {
                titles_tab::show(ui, state, theme, &self.search_query)
            }
            LibraryCategory::Transitions => {
                transitions_tab::show(ui, state, theme, &self.search_query)
            }
        }
    }
}

const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "mov", "avi", "webm", "m4v", "mts", "ts",
    "wav", "mp3", "aac", "flac", "ogg", "m4a", "aiff",
    "png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp", "exr",
];

/// Read a directory with a timeout to protect against hung network mounts.
fn read_directory_with_timeout(path: &std::path::Path) -> Result<Vec<FileEntry>, String> {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let timeout = Duration::from_secs(5);

    let read_dir = std::fs::read_dir(path)
        .map_err(|e| format!("{}: {}", path.display(), e))?;

    let mut entries = Vec::new();
    for entry in read_dir {
        // Check timeout periodically
        if start.elapsed() > timeout {
            return Err(format!("Timed out reading {} (>5s)", path.display()));
        }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // Skip entries we can't read
        };

        // metadata() can hang on network mounts — skip on error
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') { continue; }

        let is_dir = meta.is_dir();
        let ext = entry.path().extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let is_media = !is_dir && MEDIA_EXTENSIONS.contains(&ext.as_str());

        entries.push(FileEntry {
            name,
            path: entry.path(),
            is_dir,
            size: if is_dir { 0 } else { meta.len() },
            is_media,
        });
    }

    entries.sort_by(|a, b| {
        match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    });

    Ok(entries)
}

fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
