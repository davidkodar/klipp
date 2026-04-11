pub mod media_tab;
pub mod effect_registry;
pub mod effect_thumbnails;
pub mod effects_tab;
pub mod audio_fx_tab;
pub mod titles_tab;
pub mod transitions_tab;
pub mod generators_tab;

use crate::icons::Icon;
use crate::layout_state::{BrowserTab, LibraryCategory};
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// One pending file-browser selection action, captured during the row loop
/// and applied after the ScrollArea closure releases its borrow on
/// `self.files_entries`.
enum FileSelectAction {
    /// Replace selection with a single path. Sets the anchor to that path.
    Set(std::path::PathBuf),
    /// Toggle a single path in/out of the selection. Updates the anchor.
    Toggle(std::path::PathBuf),
    /// Replace selection with a pre-computed range of paths (anchor is left
    /// untouched so subsequent shift+clicks still extend from the same anchor).
    Range(Vec<std::path::PathBuf>),
}

/// Result from a file-browser thumbnail worker.
struct FileThumbResult {
    path: std::path::PathBuf,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

/// Max concurrent file-thumbnail decode workers. Software decode only,
/// so this is CPU-bounded — keep modest to avoid starving the UI.
const FILE_THUMB_MAX_INFLIGHT: usize = 4;
/// Max width to decode thumbs at. Matches the media-pool cache so the
/// disk file is reusable across both views.
const FILE_THUMB_MAX_WIDTH: u32 = 480;

pub(crate) const STILL_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp", "exr",
];
pub(crate) const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "mov", "avi", "webm", "m4v", "mts", "ts",
];

fn ext_lower(path: &std::path::Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Whether this file is a candidate for visual thumbnail generation
/// (video or still image — audio is excluded; it gets a glyph instead).
fn is_visual_media(path: &std::path::Path) -> bool {
    let ext = ext_lower(path);
    STILL_EXTENSIONS.contains(&ext.as_str()) || VIDEO_EXTENSIONS.contains(&ext.as_str())
}

/// Decode-or-load a thumbnail for a single file. Tries the shared disk cache
/// first (the same one the media pool uses), then falls back to ffmpeg software
/// decode at FILE_THUMB_MAX_WIDTH on the long edge. Writes the result back to
/// the disk cache on a successful generation.
///
/// Pure function — no UI state — so it can be called from a worker thread or
/// directly from a test.
fn generate_file_thumb(path: &std::path::Path) -> Result<(Vec<u8>, u32, u32), String> {
    let cache_key = klipp_media::disk_cache::compute_cache_key(path);

    // Try disk cache first (shared with media-pool thumbnails).
    if let Some(ref key) = cache_key {
        if klipp_media::disk_cache::thumbnail_cache_exists(key) {
            if let Ok((rgba, w, h)) = klipp_media::disk_cache::load_thumbnail_cache(key) {
                return Ok((rgba, w, h));
            }
        }
    }

    // Generate from source. Stills decode at seek=0, duration=0.
    // Videos use the non-black variant which probes 0/5/10/20/30%.
    let (rgba, w, h) = klipp_engine::generate_thumbnail_at_size_non_black(
        path, 0.0, 0.0, FILE_THUMB_MAX_WIDTH,
    )?;

    if let Some(ref key) = cache_key {
        let _ = klipp_media::disk_cache::write_thumbnail_cache(key, &rgba, w, h);
    }

    Ok((rgba, w, h))
}

/// Smart bin filter criteria for auto-filtering media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartBinFilter {
    None,
    Unused,
    VideoOnly,
    AudioOnly,
    StillsOnly,
    HighRating, // 4+ stars
}

/// Thumbnail size for the Files tab. Controls row height, thumb box size,
/// and where the filename is positioned. Persisted in EditingPrefs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum FileThumbSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl FileThumbSize {
    /// Row height in logical pixels.
    pub fn row_height(self) -> f32 {
        match self {
            Self::Small => 32.0,
            Self::Medium => 56.0,
            Self::Large => 88.0,
        }
    }
    /// Thumbnail box width × height (16:9 with a couple px of padding).
    pub fn thumb_box(self) -> egui::Vec2 {
        match self {
            Self::Small => egui::vec2(44.0, 24.0),
            Self::Medium => egui::vec2(88.0, 48.0),
            Self::Large => egui::vec2(144.0, 80.0),
        }
    }
}

pub struct BrowserPanel {
    pub search_query: String,
    // Media tab state (migrated from MediaPanel)
    pub media_sort_by: media_tab::SortBy,
    pub media_sort_ascending: bool,
    pub media_selected_bin: Option<String>,
    pub smart_bin_filter: SmartBinFilter,
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
    /// Currently selected files in the files tab. Supports multi-select via
    /// Ctrl/Cmd+click (toggle) and Shift+click (range). Cleared on directory
    /// navigation. Visual-only — does not affect import flow.
    pub files_selected: HashSet<std::path::PathBuf>,
    /// Anchor for range selection (the last item that was plain- or
    /// ctrl-clicked). Shift+click selects every visible row between the
    /// anchor and the clicked row, inclusive.
    pub files_selection_anchor: Option<std::path::PathBuf>,
    // File-thumbnail state (in-memory; disk cache is shared with media pool)
    file_thumb_textures: HashMap<std::path::PathBuf, egui::TextureHandle>,
    file_thumb_pending: HashSet<std::path::PathBuf>,
    file_thumb_failed: HashSet<std::path::PathBuf>,
    file_thumb_inflight: Arc<AtomicUsize>,
    file_thumb_tx: crossbeam_channel::Sender<FileThumbResult>,
    file_thumb_rx: crossbeam_channel::Receiver<FileThumbResult>,
    pub file_thumb_size: FileThumbSize,
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
        let (file_thumb_tx, file_thumb_rx) = crossbeam_channel::unbounded();
        Self {
            search_query: String::new(),
            media_sort_by: media_tab::SortBy::Name,
            media_sort_ascending: true,
            media_selected_bin: None,
            smart_bin_filter: SmartBinFilter::None,
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
            files_selected: HashSet::new(),
            files_selection_anchor: None,
            file_thumb_textures: HashMap::new(),
            file_thumb_pending: HashSet::new(),
            file_thumb_failed: HashSet::new(),
            file_thumb_inflight: Arc::new(AtomicUsize::new(0)),
            file_thumb_tx,
            file_thumb_rx,
            file_thumb_size: FileThumbSize::default(),
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
        files_pinned: &mut bool,
        files_split_ratio: &mut f32,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // Transfer reveal_in_browser request from AppState to browser scroll
        if let Some(mid) = state.reveal_in_browser.take() {
            self.scroll_to_media_id = Some(mid);
            *active_tab = BrowserTab::Media; // Switch to media tab
        }

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

                        let display_label = label.to_string();

                        // Allocate tab rect manually to avoid egui button hover styling
                        let text_galley = ui.painter().layout_no_wrap(
                            display_label.clone(),
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
                            &display_label,
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
                            if *tab == BrowserTab::Files && *files_pinned {
                                // Clicking FILES while pinned unpins and shows files full width
                                *files_pinned = false;
                            }
                            *active_tab = *tab;
                        }
                    }

                    // Show pin indicator on FILES tab when pinned
                    if *files_pinned {
                        let pin_color = theme.accent;
                        let pin_icon = Icon::PushPin.image(10.0).tint(pin_color);
                        ui.add_space(2.0);
                        ui.add(pin_icon.max_size(egui::vec2(10.0, 10.0)));
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
                LibraryCategory::Generators => "Search generators\u{2026}",
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
                )
                .on_hover_text("List view");
                if list_resp.clicked() {
                    self.browser_view_mode = media_tab::BrowserViewMode::List;
                }

                // Grid button
                let grid_color = if grid_active { theme.text_accent } else { theme.text_dim };
                let grid_resp = ui.add(
                    egui::Button::image(Icon::GridFour.image(14.0).tint(grid_color))
                        .min_size(egui::vec2(24.0, 24.0))
                        .frame(false),
                )
                .on_hover_text("Grid view");
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
                let sort_dir_resp = ui.add(
                    egui::Button::image(sort_icon.image(12.0).tint(theme.text_dim))
                        .min_size(egui::vec2(16.0, 24.0))
                        .frame(false),
                )
                .on_hover_text(if self.media_sort_ascending {
                    "Sort ascending — click to flip"
                } else {
                    "Sort descending — click to flip"
                });
                if sort_dir_resp.clicked() {
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
        let show_split = *files_pinned && *active_tab != BrowserTab::Files;

        if show_split {
            // Split view: left pane (MEDIA/LIBRARY) + divider + right pane (FILES)
            let content_rect = ui.available_rect_before_wrap();
            let total_w = content_rect.width();
            let divider_w = 4.0;
            let left_w = (total_w - divider_w) * files_split_ratio.clamp(0.25, 0.75);
            let right_w = total_w - left_w - divider_w;

            let left_rect = egui::Rect::from_min_size(
                content_rect.min,
                egui::vec2(left_w, content_rect.height()),
            );
            let divider_rect = egui::Rect::from_min_size(
                egui::pos2(content_rect.min.x + left_w, content_rect.min.y),
                egui::vec2(divider_w, content_rect.height()),
            );
            let right_rect = egui::Rect::from_min_size(
                egui::pos2(content_rect.min.x + left_w + divider_w, content_rect.min.y),
                egui::vec2(right_w, content_rect.height()),
            );

            // Left pane: FILES browser (source → import flow is left-to-right)
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(left_rect), |ui| {
                ui.set_clip_rect(left_rect);
                let file_cmds = self.show_files_tab(ui, state, theme, Some((files_pinned, active_tab)));
                cmds.extend(file_cmds);
            });

            // Divider (draggable)
            let divider_resp = ui.allocate_new_ui(egui::UiBuilder::new().max_rect(divider_rect), |ui| {
                let (rect, resp) = ui.allocate_exact_size(
                    divider_rect.size(),
                    egui::Sense::click_and_drag(),
                );
                let hovered = resp.hovered() || resp.dragged();
                let color = if hovered { theme.accent } else { theme.border };
                ui.painter().rect_filled(rect, 0.0, color);
                if hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if resp.dragged() {
                    let delta = resp.drag_delta().x;
                    let new_left_w = left_w + delta;
                    *files_split_ratio = (new_left_w / (total_w - divider_w)).clamp(0.25, 0.75);
                }
            });
            // Consume the space so egui knows it's used
            let _ = divider_resp;

            // Right pane: current tab content (Media or Library)
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(right_rect), |ui| {
                ui.set_clip_rect(right_rect);
                match active_tab {
                    BrowserTab::Media => {
                        // Smart bin filter selector
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Filter:").size(10.0).color(theme.text_dim));
                            let filter_label = match self.smart_bin_filter {
                                SmartBinFilter::None => "All Media",
                                SmartBinFilter::Unused => "Unused",
                                SmartBinFilter::VideoOnly => "Video Only",
                                SmartBinFilter::AudioOnly => "Audio Only",
                                SmartBinFilter::StillsOnly => "Stills Only",
                                SmartBinFilter::HighRating => "4+ Stars",
                            };
                            egui::ComboBox::from_id_salt("smart_bin")
                                .selected_text(filter_label)
                                .width(100.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::None, "All Media");
                                    ui.separator();
                                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::Unused, "Unused");
                                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::VideoOnly, "Video Only");
                                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::AudioOnly, "Audio Only");
                                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::StillsOnly, "Stills Only");
                                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::HighRating, "4+ Stars");
                                });
                        });
                        let tab_cmds = media_tab::show(
                            ui,
                            state,
                            theme,
                            &self.search_query,
                            &mut self.media_sort_by,
                            &mut self.media_sort_ascending,
                            &mut self.media_selected_bin,
                            &mut self.browser_view_mode,
                            &mut self.renaming_bin,
                            &mut self.creating_bin,
                            &mut self.scroll_to_media_id,
                            self.smart_bin_filter,
                        );
                        cmds.extend(tab_cmds);
                    }
                    BrowserTab::Library => {
                        let tab_cmds = self.show_library_tab(ui, state, theme);
                        cmds.extend(tab_cmds);
                    }
                    BrowserTab::Files => unreachable!(),
                }
            });
        } else {
            // Normal single-tab view
            match active_tab {
                BrowserTab::Files => {
                    let file_cmds = self.show_files_tab(ui, state, theme, Some((files_pinned, active_tab)));
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
                        &mut self.browser_view_mode,
                        &mut self.renaming_bin,
                        &mut self.creating_bin,
                        &mut self.scroll_to_media_id,
                        self.smart_bin_filter,
                    );
                    cmds.extend(tab_cmds);
                }
                BrowserTab::Library => {
                    let tab_cmds = self.show_library_tab(ui, state, theme);
                    cmds.extend(tab_cmds);
                }
            }
        }

        cmds
    }

    /// Navigate the files browser to a new directory. Clears in-memory thumbnail
    /// state so RAM stays bounded. Disk cache is preserved.
    fn navigate_files_to(&mut self, path: std::path::PathBuf) {
        self.files_current_path = path;
        self.files_entries_dirty = true;
        self.file_thumb_textures.clear();
        self.file_thumb_pending.clear();
        self.file_thumb_failed.clear();
        // Selection doesn't carry across folders.
        self.files_selected.clear();
        self.files_selection_anchor = None;
        // Note: in-flight workers will still complete and send results;
        // drain_file_thumbs filters out paths no longer in the current entries.
    }

    /// Re-read the current directory and drop in-memory thumbnail caches so
    /// any externally-modified files are picked up. Selection IS preserved
    /// (paths that no longer exist will simply disappear from the new
    /// `files_entries` list). The disk thumbnail cache stays intact — its
    /// keys (path+size+mtime) auto-invalidate per file.
    fn refresh_files(&mut self) {
        self.files_entries_dirty = true;
        self.file_thumb_textures.clear();
        self.file_thumb_pending.clear();
        self.file_thumb_failed.clear();
    }

    /// Drain pending file-thumbnail results and upload textures to egui.
    /// Results for files no longer in the current directory are dropped silently.
    fn drain_file_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.file_thumb_rx.try_recv() {
            self.file_thumb_pending.remove(&result.path);
            // Drop results from a previous directory.
            if !self.files_entries.iter().any(|e| e.path == result.path) {
                continue;
            }
            let pixels: Vec<egui::Color32> = result.rgba
                .chunks_exact(4)
                .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                .collect();
            let image = egui::ColorImage {
                size: [result.width as usize, result.height as usize],
                pixels,
            };
            let name = format!("file_thumb_{}", result.path.display());
            let tex = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
            self.file_thumb_textures.insert(result.path, tex);
        }
    }

    /// Spawn a background worker to generate (or load from disk) a thumbnail
    /// for a single file. No-op if already in flight, failed, or cached.
    fn spawn_file_thumb(&mut self, path: std::path::PathBuf, ctx: &egui::Context) {
        if self.file_thumb_textures.contains_key(&path) { return; }
        if self.file_thumb_pending.contains(&path) { return; }
        if self.file_thumb_failed.contains(&path) { return; }
        if self.file_thumb_inflight.load(Ordering::Relaxed) >= FILE_THUMB_MAX_INFLIGHT {
            return;
        }

        self.file_thumb_pending.insert(path.clone());
        self.file_thumb_inflight.fetch_add(1, Ordering::Relaxed);

        let tx = self.file_thumb_tx.clone();
        let inflight = self.file_thumb_inflight.clone();
        let ctx = ctx.clone();

        std::thread::Builder::new()
            .name("file-thumb-worker".into())
            .spawn(move || {
                // Drop guard: always decrement inflight on thread exit.
                struct InflightGuard(Arc<AtomicUsize>);
                impl Drop for InflightGuard {
                    fn drop(&mut self) { self.0.fetch_sub(1, Ordering::Relaxed); }
                }
                let _guard = InflightGuard(inflight);

                match generate_file_thumb(&path) {
                    Ok((rgba, w, h)) => {
                        let _ = tx.send(FileThumbResult { path, rgba, width: w, height: h });
                        ctx.request_repaint();
                    }
                    Err(e) => {
                        tracing::debug!("file thumb gen failed for {:?}: {}", path, e);
                        // No result sent — path stays in `file_thumb_pending` forever
                        // (until directory change clears it), preventing retry storms.
                        // The row falls back to the default file icon.
                    }
                }
            })
            .ok();
    }

    fn show_files_tab(&mut self, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, mut files_pinned: Option<(&mut bool, &mut BrowserTab)>) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let pad = 10.0;

        // Drain any completed thumbnail results into textures.
        self.drain_file_thumbs(ui.ctx());

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
            )
            .on_hover_text("Go up one directory");
            if back_resp.clicked() {
                if let Some(parent) = self.files_current_path.parent() {
                    self.navigate_files_to(parent.to_path_buf());
                }
            }

            // Home button
            let home_resp = ui.add(
                egui::Button::new(
                    egui::RichText::new("~").size(12.0).color(theme.text_primary)
                ).fill(theme.bg_raised).rounding(3.0).min_size(egui::vec2(24.0, 20.0)),
            )
            .on_hover_text("Go to home directory");
            if home_resp.clicked() {
                self.navigate_files_to(dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")));
            }

            // S/M/L thumbnail-size pickers, immediately to the right of the
            // home (~) button. Three small filled-rectangle previews of the
            // actual thumb size (no SVG icons needed). Left-to-right flow so
            // they're guaranteed visible at any pane width.
            ui.add_space(4.0);
            let btn_size = egui::vec2(22.0, 20.0);
            let glyph_w = |s: FileThumbSize| match s {
                FileThumbSize::Small => 6.0,
                FileThumbSize::Medium => 10.0,
                FileThumbSize::Large => 14.0,
            };
            for size in [FileThumbSize::Small, FileThumbSize::Medium, FileThumbSize::Large] {
                let is_active = self.file_thumb_size == size;
                let (rect, resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                let (bg, border) = if is_active {
                    (theme.accent, theme.accent)
                } else if resp.hovered() {
                    (theme.bg_hover, theme.border)
                } else {
                    (theme.bg_raised, theme.border)
                };
                ui.painter().rect(rect, 3.0, bg, egui::Stroke::new(0.5, border));
                let gw = glyph_w(size);
                let gh = gw * 9.0 / 16.0;
                let glyph_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(gw, gh));
                let glyph_color = if is_active {
                    egui::Color32::from_rgb(30, 30, 34) // dark on amber
                } else {
                    theme.text_secondary
                };
                ui.painter().rect_filled(glyph_rect, 1.0, glyph_color);
                let label = match size {
                    FileThumbSize::Small => "Small thumbnails",
                    FileThumbSize::Medium => "Medium thumbnails",
                    FileThumbSize::Large => "Large thumbnails",
                };
                let resp = resp.on_hover_text(label);
                if resp.clicked() {
                    self.file_thumb_size = size;
                }
            }

            // Pin button — push to right side of toolbar
            if let Some((ref pinned, _)) = files_pinned {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let is_pinned = **pinned;
                    let pin_color = if is_pinned {
                        egui::Color32::from_rgb(30, 30, 34) // dark icon on amber fill
                    } else {
                        theme.text_dim
                    };
                    let pin_resp = ui.add(
                        egui::Button::image(Icon::PushPin.image(12.0).tint(pin_color))
                            .min_size(egui::vec2(22.0, 20.0))
                            .fill(if is_pinned { theme.accent } else { theme.bg_raised })
                            .rounding(3.0),
                    )
                    .on_hover_text(if is_pinned {
                        "Unpin Files panel (return to single tab)"
                    } else {
                        "Pin Files panel alongside Media/Library"
                    });
                    if pin_resp.clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("_pin_clicked"), true));
                    }
                });
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

        // Handle pin click (deferred to avoid borrow conflict inside horizontal)
        if let Some((ref mut pinned, ref mut active_tab)) = files_pinned {
            let pin_clicked: bool = ui.ctx().data_mut(|d| d.get_temp(egui::Id::new("_pin_clicked")).unwrap_or(false));
            if pin_clicked {
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("_pin_clicked"), false));
                **pinned = !**pinned;
                if **pinned {
                    **active_tab = BrowserTab::Media;
                }
            }
        }

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
                                .color(if is_current { egui::Color32::from_rgb(30, 30, 34) } else { theme.text_secondary })
                        )
                        .fill(if is_current { theme.accent } else { egui::Color32::TRANSPARENT })
                        .stroke(egui::Stroke::new(0.5, if is_current { theme.accent } else { theme.border }))
                        .rounding(3.0),
                    )
                    .on_hover_text(path.to_string_lossy().to_string());
                    if btn.clicked() {
                        self.navigate_files_to(path.clone());
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

        // Pull state out before we hand `ui` to the ScrollArea closure so we can
        // mutate `self` (e.g. navigate on Go Home click, spawn thumb workers)
        // without borrow conflicts with `filtered`/`self.files_entries`.
        let mut go_home_clicked = false;
        let mut pending_navigate: Option<std::path::PathBuf> = None;
        let mut thumb_spawn_requests: Vec<std::path::PathBuf> = Vec::new();
        let mut pending_select: Option<FileSelectAction> = None;
        // Right-click → "Add to media pool" / "Add to bin" request.
        // Tuple: (paths to import, optional bin name).
        let mut import_request: Option<(Vec<std::path::PathBuf>, Option<String>)> = None;
        // Right-click → "File info..." request (path of file to show info for).
        let mut pending_show_file_info: Option<std::path::PathBuf> = None;
        // Right-click → "Open in Source Viewer" — list of files to import.
        // The FIRST file becomes the source viewer's active media.
        let mut pending_open_in_source: Option<Vec<std::path::PathBuf>> = None;
        // Right-click → "Refresh folder" — re-read current dir + drop thumbs.
        let mut pending_refresh = false;
        // Capture before the closure — `self` is borrowed inside via `filtered`.
        let thumb_size = self.file_thumb_size;
        let row_h = thumb_size.row_height();
        let thumb_box_size = thumb_size.thumb_box();
        // Font scales modestly with size — the larger thumbs deserve a slightly
        // larger filename, but not so much that Small becomes cramped.
        let name_font_size = match thumb_size {
            FileThumbSize::Small => 11.0,
            FileThumbSize::Medium => 12.0,
            FileThumbSize::Large => 13.0,
        };
        egui::ScrollArea::vertical().id_salt("files_scroll").show(ui, |ui| {
            // Error state
            if let Some(ref err) = self.files_error {
                let err_text = err.clone();
                ui.add_space(16.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("Cannot access folder").size(12.0).color(egui::Color32::from_rgb(210, 60, 60)));
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(err_text.as_str()).size(10.0).color(theme.text_dim));
                    ui.add_space(8.0);
                    if ui.button("Go Home").clicked() {
                        go_home_clicked = true;
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

            for (row_idx, entry) in filtered.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_h),
                    egui::Sense::click(), // egui::Sense::click() captures both primary and secondary
                );

                // Background priority: selected > hovered > none.
                let is_selected = self.files_selected.contains(&entry.path);
                if is_selected {
                    ui.painter().rect_filled(rect, 2.0, theme.accent_dim);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 2.0, theme.bg_hover);
                }

                // Thumbnail slot is centered vertically and left-padded by `pad`.
                // The icon slot is sized for non-media (folder/file glyph): a fixed
                // 14×14 square that doesn't grow with the thumb size — keeps icons
                // tidy at Large while still giving thumbnails room to breathe.
                let thumb_box = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + pad, rect.center().y - thumb_box_size.y * 0.5),
                    thumb_box_size,
                );

                let icon_color = if entry.is_dir {
                    theme.accent
                } else if entry.is_media {
                    theme.text_primary
                } else {
                    theme.text_dim
                };

                let mut painted_thumb = false;
                if !entry.is_dir && is_visual_media(&entry.path) {
                    if let Some(tex) = self.file_thumb_textures.get(&entry.path) {
                        let sz = tex.size();
                        let img_aspect = sz[0] as f32 / sz[1].max(1) as f32;
                        let box_aspect = thumb_box.width() / thumb_box.height();
                        let fitted = if img_aspect > box_aspect {
                            let h = thumb_box.width() / img_aspect;
                            let y_off = (thumb_box.height() - h) / 2.0;
                            egui::Rect::from_min_size(
                                egui::pos2(thumb_box.min.x, thumb_box.min.y + y_off),
                                egui::vec2(thumb_box.width(), h),
                            )
                        } else {
                            let w = thumb_box.height() * img_aspect;
                            let x_off = (thumb_box.width() - w) / 2.0;
                            egui::Rect::from_min_size(
                                egui::pos2(thumb_box.min.x + x_off, thumb_box.min.y),
                                egui::vec2(w, thumb_box.height()),
                            )
                        };
                        // Dark backdrop so letterbox bars don't show panel bg through them.
                        ui.painter().rect_filled(thumb_box, 2.0, egui::Color32::from_rgb(18, 18, 18));
                        ui.painter().image(
                            tex.id(),
                            fitted,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                        painted_thumb = true;
                    } else if ui.is_rect_visible(rect) {
                        // Visible row that wants a thumb but doesn't have one — request it.
                        thumb_spawn_requests.push(entry.path.clone());
                    }
                }

                if !painted_thumb {
                    // Folders and non-thumb-eligible files: draw the glyph centered
                    // within the thumb_box so the layout is consistent regardless
                    // of whether a thumb is showing.
                    let glyph_sz = (thumb_box.height() * 0.6).clamp(14.0, 28.0);
                    let icon_rect = egui::Rect::from_center_size(
                        thumb_box.center(),
                        egui::vec2(glyph_sz, glyph_sz),
                    );
                    let icon = if entry.is_dir { Icon::Folder } else { Icon::FilmStrip };
                    icon.image(glyph_sz).tint(icon_color).paint_at(ui, icon_rect);
                }

                // Name + size are positioned to the right of the thumb_box.
                // Selected rows use the accent text color for clear contrast on
                // the accent_dim background.
                let name_color = if is_selected {
                    theme.text_accent
                } else if entry.is_dir {
                    theme.text_primary
                } else if entry.is_media {
                    theme.text_primary
                } else {
                    theme.text_dim
                };
                let name_x = thumb_box.max.x + 8.0;
                ui.painter().text(
                    egui::pos2(name_x, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &entry.name,
                    egui::FontId::proportional(name_font_size),
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

                // ── Interactions ──
                // All `self` mutations are deferred to after the closure to
                // avoid borrowing `self` while `filtered` still aliases
                // `self.files_entries`.

                // Double-click: navigate (dir) or import (media file).
                if resp.double_clicked() {
                    if entry.is_dir {
                        pending_navigate = Some(entry.path.clone());
                    } else if entry.is_media {
                        cmds.push(AppCommand::LoadMediaFile {
                            path: entry.path.clone(),
                            media_id: klipp_core::MediaId::new(),
                        });
                    }
                }

                // Single primary click: navigate into a directory, OR update
                // the file selection (with modifier-aware behavior).
                if resp.clicked() {
                    if entry.is_dir {
                        pending_navigate = Some(entry.path.clone());
                    } else {
                        let mods = ui.input(|i| i.modifiers);
                        if mods.shift {
                            // Shift+click: select range from anchor (or current
                            // row if no anchor) up to and including this row.
                            let anchor_idx = self.files_selection_anchor.as_ref()
                                .and_then(|a| filtered.iter().position(|e| e.path == *a));
                            let range_paths: Vec<std::path::PathBuf> = match anchor_idx {
                                Some(a) => {
                                    let (lo, hi) = if a <= row_idx { (a, row_idx) } else { (row_idx, a) };
                                    filtered[lo..=hi].iter().map(|e| e.path.clone()).collect()
                                }
                                None => vec![entry.path.clone()],
                            };
                            pending_select = Some(FileSelectAction::Range(range_paths));
                        } else if mods.ctrl || mods.command {
                            // Ctrl/Cmd+click: toggle this row in the selection.
                            pending_select = Some(FileSelectAction::Toggle(entry.path.clone()));
                        } else {
                            // Plain click: replace selection with this row.
                            pending_select = Some(FileSelectAction::Set(entry.path.clone()));
                        }
                    }
                }

                // Right-click context menu — available on EVERY entry (file or
                // directory). Import actions are gated to media files; the
                // Tier 1 actions (reveal/open/copy/info) work for everything.
                //
                // Operates on the existing selection if the right-clicked file
                // is part of it; otherwise operates on just this single row.
                let target_paths: Vec<std::path::PathBuf> = if self.files_selected.contains(&entry.path)
                    && self.files_selected.len() > 1
                {
                    // Multi-selection — keep only visible media files.
                    filtered.iter()
                        .filter(|e| !e.is_dir && e.is_media && self.files_selected.contains(&e.path))
                        .map(|e| e.path.clone())
                        .collect()
                } else if !entry.is_dir && entry.is_media {
                    vec![entry.path.clone()]
                } else {
                    Vec::new()
                };
                let menu_count = target_paths.len();
                let bins_snapshot: Vec<String> = state.media_pool.bins.iter()
                    .map(|b| b.name.clone())
                    .collect();
                let menu_path = entry.path.clone();
                let menu_is_dir = entry.is_dir;
                resp.context_menu(|ui| {
                    // ── Import actions (only for media files) ──
                    if menu_count > 0 {
                        let label = if menu_count == 1 {
                            "Add to Media Pool".to_string()
                        } else {
                            format!("Add {} files to Media Pool", menu_count)
                        };
                        if ui.button(label).clicked() {
                            import_request = Some((target_paths.clone(), None));
                            ui.close_menu();
                        }
                        if !bins_snapshot.is_empty() {
                            ui.menu_button("Add to Bin", |ui| {
                                for bin_name in &bins_snapshot {
                                    if ui.button(bin_name).clicked() {
                                        import_request = Some((target_paths.clone(), Some(bin_name.clone())));
                                        ui.close_menu();
                                    }
                                }
                            });
                        }

                        // ── Open in Source Viewer ──
                        // Imports the file(s) to the media pool AND switches the
                        // viewer to Source mode showing the FIRST imported file.
                        // (Klipp's source viewer requires a MediaId, so we can't
                        // do true preview-without-import yet — see Tier 2 audit.)
                        let src_label = if menu_count == 1 {
                            "Open in Source Viewer".to_string()
                        } else {
                            format!("Import {} & Open First in Source Viewer", menu_count)
                        };
                        if ui.button(src_label).clicked() {
                            pending_open_in_source = Some(target_paths.clone());
                            ui.close_menu();
                        }
                        ui.separator();
                    }

                    // ── Reveal in File Manager (parent directory) ──
                    let reveal_label = if menu_is_dir {
                        "Open in File Manager"
                    } else {
                        "Reveal in File Manager"
                    };
                    if ui.button(reveal_label).clicked() {
                        let target = if menu_is_dir {
                            menu_path.clone()
                        } else {
                            menu_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| menu_path.clone())
                        };
                        let _ = std::process::Command::new("xdg-open")
                            .arg(&target)
                            .spawn();
                        ui.close_menu();
                    }

                    // ── Open with Default Application (only for files) ──
                    if !menu_is_dir {
                        if ui.button("Open with Default Application").clicked() {
                            let _ = std::process::Command::new("xdg-open")
                                .arg(&menu_path)
                                .spawn();
                            ui.close_menu();
                        }
                    }

                    // ── Copy file path ──
                    if ui.button("Copy File Path").clicked() {
                        ui.output_mut(|o| o.copied_text = menu_path.to_string_lossy().to_string());
                        ui.close_menu();
                    }

                    ui.separator();

                    // ── File Info... ──
                    if ui.button("File Info\u{2026}").clicked() {
                        pending_show_file_info = Some(menu_path.clone());
                        ui.close_menu();
                    }

                    // ── Refresh folder ──
                    if ui.button("Refresh Folder").clicked() {
                        pending_refresh = true;
                        ui.close_menu();
                    }
                });

                // Tooltip on hover (path)
                resp.on_hover_text(entry.path.to_string_lossy().as_ref());
            }
        });

        // Apply deferred mutations now that the ScrollArea closure has released
        // its borrow on `self.files_entries`.
        if go_home_clicked {
            self.navigate_files_to(dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")));
        }
        if let Some(target) = pending_navigate {
            self.navigate_files_to(target);
        }
        if !thumb_spawn_requests.is_empty() {
            let ctx = ui.ctx().clone();
            for path in thumb_spawn_requests {
                self.spawn_file_thumb(path, &ctx);
            }
        }

        // Apply selection changes captured during the row loop.
        match pending_select {
            Some(FileSelectAction::Set(p)) => {
                self.files_selected.clear();
                self.files_selected.insert(p.clone());
                self.files_selection_anchor = Some(p);
            }
            Some(FileSelectAction::Toggle(p)) => {
                if self.files_selected.contains(&p) {
                    self.files_selected.remove(&p);
                } else {
                    self.files_selected.insert(p.clone());
                }
                self.files_selection_anchor = Some(p);
            }
            Some(FileSelectAction::Range(paths)) => {
                self.files_selected.clear();
                for p in paths {
                    self.files_selected.insert(p);
                }
                // Anchor stays put — subsequent shift+clicks extend from the
                // SAME anchor (Finder/FCP behavior).
            }
            None => {}
        }

        // Right-click → import to media pool / bin.
        if let Some((paths, bin)) = import_request {
            for path in paths {
                let media_id = klipp_core::MediaId::new();
                cmds.push(AppCommand::LoadMediaFile {
                    path,
                    media_id,
                });
                if let Some(ref bin_name) = bin {
                    cmds.push(AppCommand::MoveItemToBin {
                        media_id,
                        bin: Some(bin_name.clone()),
                    });
                }
            }
        }

        // Right-click → show File Info modal.
        if let Some(path) = pending_show_file_info {
            state.file_info = Some(path);
        }

        // Right-click → import + open in Source viewer.
        // For multi-select, all files are imported and the FIRST one becomes
        // the active source media. There's a 1-frame flicker because the
        // engine processes LoadMediaFile asynchronously, so the source viewer
        // briefly renders the placeholder until the MediaItem appears in the
        // pool — imperceptible in practice.
        if let Some(paths) = pending_open_in_source {
            let mut first_id: Option<klipp_core::MediaId> = None;
            for path in paths {
                let media_id = klipp_core::MediaId::new();
                if first_id.is_none() {
                    first_id = Some(media_id);
                }
                cmds.push(AppCommand::LoadMediaFile { path, media_id });
            }
            if let Some(mid) = first_id {
                state.source_media_id = Some(mid);
                state.viewer_mode = klipp_core::ViewerMode::Source;
            }
        }

        // Right-click → refresh current folder.
        if pending_refresh {
            self.refresh_files();
        }

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
                (LibraryCategory::Generators, "Generators"),
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
            LibraryCategory::Generators => {
                generators_tab::show(ui, state, theme, &self.search_query)
            }
        }
    }
}

pub(super) const MEDIA_EXTENSIONS: &[&str] = &[
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

#[cfg(test)]
mod tests {
    use super::{MEDIA_EXTENSIONS, FILE_THUMB_MAX_WIDTH};

    /// Helper: walk a directory and collect media file paths, mirroring the
    /// ImportMediaFolder handler logic in app.rs.
    fn collect_media_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                    if MEDIA_EXTENSIONS.iter().any(|me| me.eq_ignore_ascii_case(ext)) {
                        files.push(entry.path().to_path_buf());
                    }
                }
            }
        }
        files
    }

    #[test]
    fn import_folder_finds_media_files() {
        let dir = std::env::temp_dir().join("klipp_test_import_folder");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Create media files
        std::fs::write(dir.join("clip.mp4"), b"fake").unwrap();
        std::fs::write(dir.join("photo.jpg"), b"fake").unwrap();
        std::fs::write(dir.join("audio.wav"), b"fake").unwrap();

        // Create non-media files
        std::fs::write(dir.join("readme.txt"), b"text").unwrap();
        std::fs::write(dir.join("project.json"), b"{}").unwrap();

        let files = collect_media_files(&dir);
        assert_eq!(files.len(), 3, "should find exactly 3 media files");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_folder_recurses_subdirectories() {
        let dir = std::env::temp_dir().join("klipp_test_import_recurse");
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("sub");
        let subsub = sub.join("deep");
        std::fs::create_dir_all(&subsub).unwrap();

        std::fs::write(dir.join("root.mov"), b"fake").unwrap();
        std::fs::write(sub.join("mid.mkv"), b"fake").unwrap();
        std::fs::write(subsub.join("deep.png"), b"fake").unwrap();

        let files = collect_media_files(&dir);
        assert_eq!(files.len(), 3, "should recurse into subdirectories");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_folder_case_insensitive_extensions() {
        let dir = std::env::temp_dir().join("klipp_test_import_case");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("video.MP4"), b"fake").unwrap();
        std::fs::write(dir.join("image.JPG"), b"fake").unwrap();
        std::fs::write(dir.join("audio.Wav"), b"fake").unwrap();

        let files = collect_media_files(&dir);
        assert_eq!(files.len(), 3, "should match extensions case-insensitively");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_folder_empty_directory() {
        let dir = std::env::temp_dir().join("klipp_test_import_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let files = collect_media_files(&dir);
        assert_eq!(files.len(), 0, "empty directory should yield no files");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Copy a real JPEG fixture from the repo's assets into a temp file.
    ///
    /// We use JPEG (not PNG) because the existing `generate_thumbnail_at_size`
    /// decoder helper doesn't flush the decoder after the single image packet,
    /// which causes single-image PNG inputs to return "No frame decoded".
    /// JPEG works because libjpeg outputs the frame on receive_frame
    /// without needing a flush. This is a separate pre-existing decoder bug
    /// (see test note below) — for the file-thumbnail wiring test, JPEG
    /// is sufficient because that's the primary user-visible case anyway
    /// (e.g. phone photos like `IMG_xxxx.jpg`).
    fn copy_fixture_jpg_to(target: &std::path::Path) {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src = manifest.join("../../assets/effect_preview_reference.jpg");
        std::fs::copy(&src, target)
            .unwrap_or_else(|e| panic!("copy fixture from {}: {}", src.display(), e));
    }

    /// End-to-end check that the file-browser thumbnail pipeline can:
    ///   1) decode a real image off disk via ffmpeg,
    ///   2) write the result to the shared media-pool disk cache,
    ///   3) load it back from the disk cache on a second call.
    ///
    /// This is the core wiring that the file-browser "Phase A" feature relies on.
    #[test]
    fn file_thumb_decodes_and_caches() {
        // ffmpeg must be initialized before any decoder calls. Cheap and idempotent.
        klipp_engine::init_ffmpeg().expect("ffmpeg init");

        // Use a unique temp dir + filename so the disk cache key is fresh on
        // every test run (cache key is path+size+mtime; new path = guaranteed miss).
        let dir = std::env::temp_dir().join(format!(
            "klipp_test_file_thumb_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jpg_path = dir.join("test_thumb.jpg");
        copy_fixture_jpg_to(&jpg_path);

        // First call: cache miss → ffmpeg decode → write cache → return.
        let (rgba1, w1, h1) = super::generate_file_thumb(&jpg_path)
            .expect("first thumb generation should succeed");
        assert!(w1 > 0 && h1 > 0, "decoded dims must be positive: {w1}x{h1}");
        assert!(
            w1 <= FILE_THUMB_MAX_WIDTH,
            "decoded width {w1} should respect FILE_THUMB_MAX_WIDTH {}",
            FILE_THUMB_MAX_WIDTH,
        );
        assert_eq!(
            rgba1.len() as u32,
            w1 * h1 * 4,
            "rgba length must equal w*h*4",
        );

        // Verify the cache file was written.
        let cache_key = klipp_media::disk_cache::compute_cache_key(&jpg_path)
            .expect("cache key for real file");
        assert!(
            klipp_media::disk_cache::thumbnail_cache_exists(&cache_key),
            "first call should have populated the disk cache",
        );

        // Second call: should hit the disk cache and return identical bytes.
        let (rgba2, w2, h2) = super::generate_file_thumb(&jpg_path)
            .expect("second thumb load should succeed");
        assert_eq!((w1, h1), (w2, h2), "cached dims must match");
        assert_eq!(rgba1, rgba2, "cached rgba must match");

        // Cleanup test file (cache file is shared with media pool — leave it).
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_thumb_size_dimensions_monotonic() {
        // Row height and thumb box must grow strictly with size — otherwise
        // the toolbar picker is meaningless.
        let small = super::FileThumbSize::Small;
        let medium = super::FileThumbSize::Medium;
        let large = super::FileThumbSize::Large;
        assert!(small.row_height() < medium.row_height());
        assert!(medium.row_height() < large.row_height());
        assert!(small.thumb_box().x < medium.thumb_box().x);
        assert!(medium.thumb_box().x < large.thumb_box().x);
        assert!(small.thumb_box().y < medium.thumb_box().y);
        assert!(medium.thumb_box().y < large.thumb_box().y);
        // Thumb box must always fit inside the row vertically.
        for s in [small, medium, large] {
            assert!(
                s.thumb_box().y <= s.row_height(),
                "thumb box y={} must fit in row_h={} for {:?}",
                s.thumb_box().y, s.row_height(), s,
            );
        }
        // Default must be Medium (chosen for balance — Small was the original
        // post-Phase-A look and the user found it too small to be useful).
        assert_eq!(super::FileThumbSize::default(), medium);
    }

    #[test]
    fn is_visual_media_classification() {
        use std::path::PathBuf;
        assert!(super::is_visual_media(&PathBuf::from("foo.mp4")));
        assert!(super::is_visual_media(&PathBuf::from("foo.MOV")));
        assert!(super::is_visual_media(&PathBuf::from("foo.jpg")));
        assert!(super::is_visual_media(&PathBuf::from("foo.PNG")));
        assert!(!super::is_visual_media(&PathBuf::from("foo.wav")));
        assert!(!super::is_visual_media(&PathBuf::from("foo.mp3")));
        assert!(!super::is_visual_media(&PathBuf::from("foo.txt")));
        assert!(!super::is_visual_media(&PathBuf::from("foo")));
    }
}
