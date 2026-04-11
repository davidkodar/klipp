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

/// Result from a file-browser skim-decode worker. Same shape as FileThumbResult
/// but on a separate channel so the renderer can distinguish "poster frame
/// arrived" from "live skim frame arrived" without ambiguity.
struct FileSkimResult {
    path: std::path::PathBuf,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

/// Result from a file-browser audio peaks worker.
struct FileAudioPeaksResult {
    path: std::path::PathBuf,
    /// Empty Vec means decode failed — used to mark the path as failed
    /// so we don't retry every frame.
    peaks: Vec<(f32, f32)>,
}

/// Number of waveform buckets to compute for audio thumbnails. Matches
/// roughly the pixel width of the Large thumb so even at the biggest size
/// we have one bucket per pixel column.
const FILE_AUDIO_PEAKS_BUCKETS: usize = 144;

/// Max concurrent file-thumbnail decode workers. Software decode only,
/// so this is CPU-bounded — keep modest to avoid starving the UI.
const FILE_THUMB_MAX_INFLIGHT: usize = 4;
/// Max width to decode thumbs at. Matches the media-pool cache so the
/// disk file is reusable across both views.
const FILE_THUMB_MAX_WIDTH: u32 = 480;

// Re-exports from the canonical module so the rest of this file (and other
// files in this crate that import via `super::`) can keep using the old
// names without churn. The actual definitions live in
// `klipp_core::media_extensions`.
pub(crate) use klipp_core::media_extensions::IMAGE_EXTENSIONS as STILL_EXTENSIONS;
pub(crate) use klipp_core::media_extensions::VIDEO_EXTENSIONS;
pub(crate) use klipp_core::media_extensions::AUDIO_EXTENSIONS;

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

/// Read system file-manager bookmarks (KDE Places + GTK plain-text formats)
/// from the standard XDG locations. Returns a deduped list of `(label, path)`
/// tuples for valid local directories.
///
/// Sources searched, in order:
/// - `~/.local/share/user-places.xbel` (KDE Plasma)
/// - `~/.config/gtk-4.0/bookmarks` (modern GTK)
/// - `~/.config/gtk-3.0/bookmarks` (legacy GTK)
/// - `~/.gtk-bookmarks` (very old GTK 2)
///
/// KDE entries marked `<isSystemItem>true</isSystemItem>` are skipped because
/// they duplicate the hardcoded XDG bookmarks bar (Home/Desktop/Videos/etc.).
/// Non-`file://` entries (`remote:/`, `trash:/`, etc.) are skipped.
/// Paths are percent-decoded before being returned.
fn read_system_bookmarks() -> Vec<(String, std::path::PathBuf)> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };
    let mut out: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();

    // ── KDE XBEL ──
    let xbel = home.join(".local/share/user-places.xbel");
    if let Ok(contents) = std::fs::read_to_string(&xbel) {
        parse_kde_xbel(&contents, &mut out, &mut seen);
    }

    // ── GTK plain-text bookmarks ──
    for rel in &[
        ".config/gtk-4.0/bookmarks",
        ".config/gtk-3.0/bookmarks",
        ".gtk-bookmarks",
    ] {
        let p = home.join(rel);
        if let Ok(contents) = std::fs::read_to_string(&p) {
            parse_gtk_bookmarks(&contents, &mut out, &mut seen);
        }
    }

    out
}

/// Parse KDE Places XBEL with light line-based scanning. Avoids pulling in a
/// full XML parser for one feature. The format we need is regular enough that
/// this approach works reliably for the bookmarks subset.
fn parse_kde_xbel(
    contents: &str,
    out: &mut Vec<(String, std::path::PathBuf)>,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
) {
    // Tracks the current bookmark being parsed across multiple lines.
    let mut current_href: Option<String> = None;
    let mut current_title: Option<String> = None;
    let mut current_is_system = false;

    let flush = |href: &mut Option<String>,
                 title: &mut Option<String>,
                 is_system: &mut bool,
                 out: &mut Vec<(String, std::path::PathBuf)>,
                 seen: &mut std::collections::HashSet<std::path::PathBuf>| {
        if let (Some(h), Some(t)) = (href.take(), title.take()) {
            if !*is_system {
                if let Some(path) = file_uri_to_path(&h) {
                    if path.is_dir() && seen.insert(path.clone()) {
                        out.push((t, path));
                    }
                }
            }
        }
        *is_system = false;
    };

    for line in contents.lines() {
        let trimmed = line.trim();

        // New bookmark — flush the previous one first.
        if trimmed.starts_with("<bookmark ") {
            flush(&mut current_href, &mut current_title, &mut current_is_system, out, seen);
            // Extract href="..."
            if let Some(start) = trimmed.find("href=\"") {
                let after = &trimmed[start + 6..];
                if let Some(end) = after.find('"') {
                    current_href = Some(after[..end].to_string());
                }
            }
        } else if trimmed.starts_with("<title>") && trimmed.ends_with("</title>") {
            let inner = &trimmed[7..trimmed.len() - 8];
            current_title = Some(inner.to_string());
        } else if trimmed.contains("<isSystemItem>true</isSystemItem>") {
            current_is_system = true;
        } else if trimmed == "</bookmark>" {
            flush(&mut current_href, &mut current_title, &mut current_is_system, out, seen);
        }
    }
    // Flush any final dangling bookmark (in case of malformed XML).
    flush(&mut current_href, &mut current_title, &mut current_is_system, out, seen);
}

/// Parse GTK plain-text bookmarks. Format: one URI per line, optional space
/// + display name override.
fn parse_gtk_bookmarks(
    contents: &str,
    out: &mut Vec<(String, std::path::PathBuf)>,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
) {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Split URI from optional display name (separated by single space).
        let (uri, label_override) = match line.split_once(' ') {
            Some((uri, label)) => (uri, Some(label.to_string())),
            None => (line, None),
        };
        if let Some(path) = file_uri_to_path(uri) {
            if !path.is_dir() {
                continue;
            }
            if !seen.insert(path.clone()) {
                continue;
            }
            let label = label_override.unwrap_or_else(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string())
            });
            out.push((label, path));
        }
    }
}

/// Decode a `file:///...` URI to a PathBuf. Returns None for non-file URIs
/// (`remote:/`, `trash:/`, etc.) or invalid input. Performs percent-decoding
/// (`%20` → space, etc.) on the path component.
fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let path_part = uri.strip_prefix("file://")?;
    // file:///foo → /foo (the leading slash stays)
    let decoded = percent_decode(path_part);
    if decoded.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(decoded))
}

/// Percent-decode an ASCII percent-encoded string. Treats invalid escapes as
/// literal characters (lenient — better than dropping the bookmark entirely).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Quick type filter for the Files tab. Persisted in EditingPrefs.
/// Directories are always shown regardless of filter so the user can navigate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum FileTypeFilter {
    #[default]
    All,
    Video,
    Audio,
    Images,
}

impl FileTypeFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Images => "Images",
        }
    }

    /// Check whether a single file entry matches this filter. Directories
    /// always match (so navigation always works). For files, the extension
    /// is checked against the corresponding extension constant.
    pub fn matches(self, entry: &FileEntry) -> bool {
        if entry.is_dir {
            return true;
        }
        let ext = entry.path.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        match self {
            Self::All => true,
            Self::Video => VIDEO_EXTENSIONS.contains(&ext.as_str()),
            Self::Audio => AUDIO_EXTENSIONS.contains(&ext.as_str()),
            Self::Images => STILL_EXTENSIONS.contains(&ext.as_str()),
        }
    }
}

/// Sort key for the Files tab. Persisted in EditingPrefs.
/// Directories are always grouped above files regardless of the chosen key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum FileSortBy {
    #[default]
    Name,
    DateModified,
    Size,
    Type,
}

impl FileSortBy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::DateModified => "Date",
            Self::Size => "Size",
            Self::Type => "Type",
        }
    }
}

/// Sort a slice of FileEntry in place by the chosen key. Directories are
/// always grouped above files; the sort key + direction only affect ordering
/// within the file group (and within the directory group).
fn sort_file_entries(entries: &mut [FileEntry], by: FileSortBy, ascending: bool) {
    entries.sort_by(|a, b| {
        // Dirs always first.
        match (a.is_dir, b.is_dir) {
            (true, false) => return std::cmp::Ordering::Less,
            (false, true) => return std::cmp::Ordering::Greater,
            _ => {}
        }
        let ord = match by {
            FileSortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            FileSortBy::Size => a.size.cmp(&b.size),
            FileSortBy::DateModified => a.mtime.cmp(&b.mtime),
            FileSortBy::Type => {
                let ext_a = a.path.extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                let ext_b = b.path.extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                // Tiebreaker: name within the same extension.
                ext_a.cmp(&ext_b)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            }
        };
        if ascending { ord } else { ord.reverse() }
    });
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
    /// Files-tab sort key.
    pub files_sort_by: FileSortBy,
    /// Files-tab sort direction (true = ascending).
    pub files_sort_ascending: bool,
    /// Files-tab quick type filter (All / Video / Audio / Images).
    pub files_type_filter: FileTypeFilter,
    /// User-pinned favorite folders, rendered after the XDG bookmarks bar.
    /// Persisted via `EditingPrefs::pinned_folders`.
    pub pinned_folders: Vec<std::path::PathBuf>,
    /// System file-manager bookmarks read once at startup from KDE Places
    /// (`~/.local/share/user-places.xbel`) and GTK plain-text bookmarks
    /// (`~/.config/gtk-{3,4}.0/bookmarks`). Read-only at runtime.
    pub system_bookmarks: Vec<(String, std::path::PathBuf)>,
    /// Whether the Files panel currently "owns" keyboard focus. Set to true
    /// when the user clicks a row, cleared when any text-edit widget takes
    /// focus (search bar, dialogs, etc.). Controls whether arrow keys / Enter
    /// / Backspace navigate the file list.
    pub files_panel_focused: bool,
    /// Type-to-navigate prefix buffer. Accumulates typed characters within
    /// a 750ms window; matches the first visible entry whose name starts
    /// with the buffer (case-insensitive). Cleared on timeout or when the
    /// files panel loses focus.
    pub type_ahead_buffer: String,
    /// Timestamp of the last type-ahead keystroke. Used to expire the
    /// buffer after 750ms of inactivity.
    pub type_ahead_last_input: Option<std::time::Instant>,
    /// Live skim frame for the row currently under the cursor (FCP-style
    /// hover skimming). Single slot: only one row can be skimmed at a time.
    /// Cleared when the cursor leaves a video row, which makes the row
    /// fall back to its cached poster thumbnail.
    file_skim_texture: Option<(std::path::PathBuf, egui::TextureHandle)>,
    /// Cached file durations (seconds), populated lazily on first hover.
    /// Used to map cursor X position to a skim timestamp.
    file_durations: std::collections::HashMap<std::path::PathBuf, f64>,
    /// Whether a skim-decode worker thread is currently running. Prevents
    /// spawning more than one decode at a time (back-pressure).
    file_skim_in_flight: bool,
    /// Latest skim request received while a decode was in flight. Picked up
    /// by `drain_file_skims` when the current decode finishes.
    file_skim_queued: Option<(std::path::PathBuf, f64)>,
    file_skim_tx: crossbeam_channel::Sender<FileSkimResult>,
    file_skim_rx: crossbeam_channel::Receiver<FileSkimResult>,
    /// Lightweight waveform peaks for audio files in the current directory.
    /// Loaded lazily on first viewport visibility, cached on disk via
    /// `klipp_media::disk_cache::write_audio_peaks_cache` so subsequent
    /// visits are instant.
    file_audio_peaks: HashMap<std::path::PathBuf, Vec<(f32, f32)>>,
    file_audio_pending: HashSet<std::path::PathBuf>,
    file_audio_failed: HashSet<std::path::PathBuf>,
    file_audio_tx: crossbeam_channel::Sender<FileAudioPeaksResult>,
    file_audio_rx: crossbeam_channel::Receiver<FileAudioPeaksResult>,
}

#[derive(Clone)]
pub struct FileEntry {
    pub name: String,
    pub path: std::path::PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub is_media: bool,
    /// Last-modified time, populated from `metadata().modified()`. May be
    /// `None` on filesystems that don't track mtime (rare).
    pub mtime: Option<std::time::SystemTime>,
}

impl Default for BrowserPanel {
    fn default() -> Self {
        let (file_thumb_tx, file_thumb_rx) = crossbeam_channel::unbounded();
        let (file_skim_tx, file_skim_rx) = crossbeam_channel::unbounded();
        let (file_audio_tx, file_audio_rx) = crossbeam_channel::unbounded();
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
            files_sort_by: FileSortBy::default(),
            files_sort_ascending: true,
            files_type_filter: FileTypeFilter::default(),
            pinned_folders: Vec::new(),
            system_bookmarks: read_system_bookmarks(),
            files_panel_focused: false,
            type_ahead_buffer: String::new(),
            type_ahead_last_input: None,
            file_skim_texture: None,
            file_durations: HashMap::new(),
            file_skim_in_flight: false,
            file_skim_queued: None,
            file_skim_tx,
            file_skim_rx,
            file_audio_peaks: HashMap::new(),
            file_audio_pending: HashSet::new(),
            file_audio_failed: HashSet::new(),
            file_audio_tx,
            file_audio_rx,
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

            // Type filter + sort dropdown + direction toggle for Files tab.
            // Mirrors the Media-tab pattern above. Changing the sort key
            // re-sorts `files_entries` immediately (no disk read). The type
            // filter is applied at render time on the existing entries.
            if *active_tab == BrowserTab::Files {
                // Type filter dropdown — All / Video / Audio / Images.
                // Directories are always shown regardless of selection.
                egui::ComboBox::from_id_salt("files_type_filter")
                    .selected_text(
                        egui::RichText::new(self.files_type_filter.label())
                            .size(10.0)
                            .color(theme.text_secondary),
                    )
                    .width(60.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.files_type_filter, FileTypeFilter::All, "All");
                        ui.selectable_value(&mut self.files_type_filter, FileTypeFilter::Video, "Video only");
                        ui.selectable_value(&mut self.files_type_filter, FileTypeFilter::Audio, "Audio only");
                        ui.selectable_value(&mut self.files_type_filter, FileTypeFilter::Images, "Images only");
                    });

                let prev_sort = self.files_sort_by;
                let prev_asc = self.files_sort_ascending;

                egui::ComboBox::from_id_salt("files_sort")
                    .selected_text(
                        egui::RichText::new(self.files_sort_by.label())
                            .size(10.0)
                            .color(theme.text_secondary),
                    )
                    .width(60.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.files_sort_by, FileSortBy::Name, "Name");
                        ui.selectable_value(&mut self.files_sort_by, FileSortBy::DateModified, "Date modified");
                        ui.selectable_value(&mut self.files_sort_by, FileSortBy::Size, "Size");
                        ui.selectable_value(&mut self.files_sort_by, FileSortBy::Type, "Type");
                    });

                let sort_icon = if self.files_sort_ascending {
                    Icon::SortAscending
                } else {
                    Icon::SortDescending
                };
                let sort_dir_resp = ui.add(
                    egui::Button::image(sort_icon.image(12.0).tint(theme.text_dim))
                        .min_size(egui::vec2(16.0, 24.0))
                        .frame(false),
                )
                .on_hover_text(if self.files_sort_ascending {
                    "Sort ascending — click to flip"
                } else {
                    "Sort descending — click to flip"
                });
                if sort_dir_resp.clicked() {
                    self.files_sort_ascending = !self.files_sort_ascending;
                }

                // Re-sort the existing entries in place if either changed.
                if self.files_sort_by != prev_sort || self.files_sort_ascending != prev_asc {
                    sort_file_entries(
                        &mut self.files_entries,
                        self.files_sort_by,
                        self.files_sort_ascending,
                    );
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
        self.file_audio_peaks.clear();
        self.file_audio_pending.clear();
        self.file_audio_failed.clear();
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
        self.file_audio_peaks.clear();
        self.file_audio_pending.clear();
        self.file_audio_failed.clear();
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

    /// Drain skim-decode results from the worker channel into `file_skim_texture`.
    /// Called at the top of `show_files_tab` each frame. Only the LATEST result
    /// for the most recently requested path is kept; stale results from old
    /// hover positions are dropped.
    fn drain_file_skims(&mut self, ctx: &egui::Context) {
        let mut last: Option<FileSkimResult> = None;
        while let Ok(result) = self.file_skim_rx.try_recv() {
            last = Some(result);
        }
        if let Some(result) = last {
            self.file_skim_in_flight = false;
            // Only upload if the result is non-empty (decode failures send empty rgba).
            if !result.rgba.is_empty() && result.width > 0 && result.height > 0 {
                let pixels: Vec<egui::Color32> = result.rgba
                    .chunks_exact(4)
                    .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                    .collect();
                let image = egui::ColorImage {
                    size: [result.width as usize, result.height as usize],
                    pixels,
                };
                let tex = ctx.load_texture(
                    "file_skim_frame",
                    image,
                    egui::TextureOptions::LINEAR,
                );
                self.file_skim_texture = Some((result.path.clone(), tex));
            }

            // If a more recent hover request came in while we were decoding,
            // start the queued one immediately.
            if let Some((qpath, qtime)) = self.file_skim_queued.take() {
                self.spawn_file_skim_decode(qpath, qtime, ctx);
            }
        }
    }

    /// Request a skim frame for `path` at `time_secs`. If a decode is already
    /// in flight, queue the latest request to fire when the current one finishes
    /// (back-pressure — at most one decode at a time per panel).
    fn request_file_skim(&mut self, path: std::path::PathBuf, time_secs: f64, ctx: &egui::Context) {
        if self.file_skim_in_flight {
            // Replace any older queued request — we only care about the latest.
            self.file_skim_queued = Some((path, time_secs));
            return;
        }
        self.spawn_file_skim_decode(path, time_secs, ctx);
    }

    fn spawn_file_skim_decode(&mut self, path: std::path::PathBuf, time_secs: f64, ctx: &egui::Context) {
        self.file_skim_in_flight = true;
        let tx = self.file_skim_tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("file-skim-worker".into())
            .spawn(move || {
                // Decode at FILE_THUMB_MAX_WIDTH for visual quality at the
                // largest thumb size (Large = 144px). Software decode is
                // sufficient — frames are tiny.
                match klipp_engine::generate_thumbnail_at_size(&path, time_secs, FILE_THUMB_MAX_WIDTH) {
                    Ok((rgba, w, h)) => {
                        let _ = tx.send(FileSkimResult { path, rgba, width: w, height: h });
                    }
                    Err(e) => {
                        tracing::debug!("file skim decode failed for {:?} @ {:.2}s: {}", path, time_secs, e);
                        // Send an empty result so drain_file_skims clears in_flight.
                        let _ = tx.send(FileSkimResult { path, rgba: Vec::new(), width: 0, height: 0 });
                    }
                }
                ctx.request_repaint();
            })
            .ok();
    }

    /// Drain audio-peaks results from the worker channel into the in-memory
    /// cache. Called at the top of `show_files_tab` each frame.
    fn drain_file_audio_peaks(&mut self) {
        while let Ok(result) = self.file_audio_rx.try_recv() {
            self.file_audio_pending.remove(&result.path);
            if result.peaks.is_empty() {
                // Decode failed — mark as failed so we don't retry every frame.
                self.file_audio_failed.insert(result.path);
            } else if self.files_entries.iter().any(|e| e.path == result.path) {
                // Drop results from a previous directory.
                self.file_audio_peaks.insert(result.path, result.peaks);
            }
        }
    }

    /// Spawn a background worker to generate (or load from disk) the audio
    /// peaks summary for a single audio file. No-op if already cached, in
    /// flight, failed, or if the worker pool is full.
    fn spawn_file_audio_peaks(&mut self, path: std::path::PathBuf) {
        if self.file_audio_peaks.contains_key(&path) { return; }
        if self.file_audio_pending.contains(&path) { return; }
        if self.file_audio_failed.contains(&path) { return; }
        // Reuse the existing thumb worker pool counter so audio + thumbs
        // share the same 4-worker budget (no CPU starvation).
        if self.file_thumb_inflight.load(Ordering::Relaxed) >= FILE_THUMB_MAX_INFLIGHT {
            return;
        }

        self.file_audio_pending.insert(path.clone());
        self.file_thumb_inflight.fetch_add(1, Ordering::Relaxed);

        let tx = self.file_audio_tx.clone();
        let inflight = self.file_thumb_inflight.clone();

        std::thread::Builder::new()
            .name("file-audio-peaks-worker".into())
            .spawn(move || {
                struct InflightGuard(Arc<AtomicUsize>);
                impl Drop for InflightGuard {
                    fn drop(&mut self) { self.0.fetch_sub(1, Ordering::Relaxed); }
                }
                let _guard = InflightGuard(inflight);

                // Try the disk cache first (shared key with the rest of the
                // klipp file caches: path + size + mtime).
                let cache_key = klipp_media::disk_cache::compute_cache_key(&path);
                if let Some(ref key) = cache_key {
                    if klipp_media::disk_cache::audio_peaks_cache_exists(key) {
                        if let Ok(peaks) = klipp_media::disk_cache::load_audio_peaks_cache(key) {
                            let _ = tx.send(FileAudioPeaksResult { path, peaks });
                            return;
                        }
                    }
                }

                // Generate from source (decodes the entire file to a 4 kHz
                // mono f32 stream then buckets it into 144 (min, max) pairs).
                match klipp_engine::generate_audio_peaks(&path, FILE_AUDIO_PEAKS_BUCKETS) {
                    Ok(peaks) => {
                        if let Some(ref key) = cache_key {
                            let _ = klipp_media::disk_cache::write_audio_peaks_cache(key, &peaks);
                        }
                        let _ = tx.send(FileAudioPeaksResult { path, peaks });
                    }
                    Err(e) => {
                        tracing::debug!("audio peaks gen failed for {:?}: {}", path, e);
                        let _ = tx.send(FileAudioPeaksResult { path, peaks: Vec::new() });
                    }
                }
            })
            .ok();
    }

    /// Lazily probe a video file's duration via ffmpeg's format header.
    /// Cached in `file_durations` so subsequent hovers are free.
    /// Returns None if the file isn't probable (audio, broken, etc.).
    fn ensure_file_duration(&mut self, path: &std::path::Path) -> Option<f64> {
        if let Some(d) = self.file_durations.get(path) {
            return Some(*d);
        }
        // probe_media_header is fast (~10–50ms), acceptable as a one-time
        // hitch on first hover per file.
        match klipp_engine::probe_media_header(path) {
            Ok(info) => {
                if let Some(d) = info.duration_secs {
                    if d > 0.0 {
                        self.file_durations.insert(path.to_path_buf(), d);
                        return Some(d);
                    }
                }
                None
            }
            Err(_) => None,
        }
    }

    fn show_files_tab(&mut self, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, mut files_pinned: Option<(&mut bool, &mut BrowserTab)>) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let pad = 10.0;

        // Drain any completed thumbnail results into textures.
        self.drain_file_thumbs(ui.ctx());
        // Drain any completed skim-frame results into the live skim texture.
        self.drain_file_skims(ui.ctx());
        // Drain any completed audio peaks results into the in-memory cache.
        self.drain_file_audio_peaks();

        // Check for async directory read result
        if let Some(ref rx) = self.files_receiver {
            match rx.try_recv() {
                Ok(Ok(mut entries)) => {
                    sort_file_entries(&mut entries, self.files_sort_by, self.files_sort_ascending);
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

            // ── User-pinned favorites ──
            // Rendered inline after the XDG bookmarks. The Star prefix
            // distinguishes them visually. Right-click any pinned button to
            // remove it from the favorites list.
            //
            // We snapshot `self.pinned_folders` into a local Vec so the
            // iteration doesn't borrow `self`, freeing us to call
            // `self.navigate_files_to(...)` and mutate `self.pinned_folders`
            // inside the loop body.
            let pinned_snapshot: Vec<std::path::PathBuf> = self.pinned_folders.clone();
            let mut pending_unpin: Option<std::path::PathBuf> = None;
            for path in &pinned_snapshot {
                let is_current = *path == self.files_current_path;
                let display_name = path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                let btn = ui.add(
                    egui::Button::new(
                        egui::RichText::new(format!("\u{2605} {}", display_name)).size(9.0)
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
                btn.context_menu(|ui| {
                    if ui.button("Remove from Favorites").clicked() {
                        pending_unpin = Some(path.clone());
                        ui.close_menu();
                    }
                });
            }
            // Apply unpin after the loop so we don't mutate `pinned_snapshot`'s
            // backing store (`self.pinned_folders`) mid-iteration. Persistence
            // is handled by the diff check in `render_browser_panel`.
            if let Some(target) = pending_unpin {
                self.pinned_folders.retain(|p| p != &target);
            }

            // ── System bookmarks (KDE Places + GTK) ──
            // Rendered after the user-pinned favorites. Skipped if the path
            // is already shown elsewhere in the bar (XDG default or pinned).
            // The visual style matches the other bookmark buttons exactly so
            // they feel native — no special icon prefix.
            //
            // Snapshot into locals so the iteration doesn't borrow `self`,
            // freeing us to call `self.navigate_files_to(...)` inside the loop.
            let xdg_paths: Vec<std::path::PathBuf> = bookmarks
                .iter()
                .filter_map(|(_, dir)| dir.clone())
                .collect();
            let system_snapshot: Vec<(String, std::path::PathBuf)> = self.system_bookmarks.clone();
            let pinned_paths: Vec<std::path::PathBuf> = self.pinned_folders.clone();
            for (label, path) in &system_snapshot {
                // Dedupe against XDG defaults and user pins.
                if xdg_paths.iter().any(|p| p == path) {
                    continue;
                }
                if pinned_paths.iter().any(|p| p == path) {
                    continue;
                }
                let is_current = *path == self.files_current_path;
                let btn = ui.add(
                    egui::Button::new(
                        egui::RichText::new(label).size(9.0)
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
        let type_filter = self.files_type_filter;
        let filtered: Vec<&FileEntry> = self.files_entries.iter()
            .filter(|e| type_filter.matches(e))
            .filter(|e| search.is_empty() || e.name.to_lowercase().contains(&search))
            .collect();

        // Pull state out before we hand `ui` to the ScrollArea closure so we can
        // mutate `self` (e.g. navigate on Go Home click, spawn thumb workers)
        // without borrow conflicts with `filtered`/`self.files_entries`.
        let mut go_home_clicked = false;
        let mut pending_navigate: Option<std::path::PathBuf> = None;
        let mut thumb_spawn_requests: Vec<std::path::PathBuf> = Vec::new();
        let mut audio_peaks_spawn_requests: Vec<std::path::PathBuf> = Vec::new();
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
        // Right-click → "Pin / Unpin Favorites" — directory path to toggle.
        let mut pending_pin_toggle: Option<std::path::PathBuf> = None;
        // Hover skim — latest skim decode request captured during the row loop.
        // Tuple: (path, time_secs). Applied after the loop to avoid borrow conflicts.
        let mut pending_skim_request: Option<(std::path::PathBuf, f64)> = None;
        // Hover skim — duration to probe (lazy, on first hover per file).
        // Set inside the loop, applied after the loop. Sync probe is OK because
        // it's a one-time hitch per file (~10–50ms).
        let mut pending_duration_probe: Option<std::path::PathBuf> = None;
        // True if any video row is currently being hovered with skim active.
        // Used to clear `file_skim_texture` when the cursor leaves all video
        // rows, so the rows revert to their cached poster thumbnails.
        let mut any_video_row_hovered = false;
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

        // ── Keyboard navigation ──
        // Only fires when the files panel has focus (set on row click below)
        // AND no text-edit widget anywhere has focus (search bar, dialogs, etc.).
        // Mutates `self.files_selected` + `self.files_selection_anchor`
        // immediately, and stashes `scroll_to_index` so the row loop can call
        // `scroll_to_rect` for the new selection on the same frame.
        let text_edit_active = ui.ctx().wants_keyboard_input();
        if text_edit_active {
            self.files_panel_focused = false;
            // Reset type-ahead so the buffer doesn't carry over from a
            // search-bar interaction back to the file list.
            self.type_ahead_buffer.clear();
            self.type_ahead_last_input = None;
        }
        let mut scroll_to_index: Option<usize> = None;
        if self.files_panel_focused && !text_edit_active && !filtered.is_empty() {
            let (k_down, k_up, k_home, k_end, k_pgdn, k_pgup, k_enter, k_back) = ui.input(|i| (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::Home),
                i.key_pressed(egui::Key::End),
                i.key_pressed(egui::Key::PageDown),
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Backspace),
            ));
            // Find current anchor's index in `filtered`. If the anchor isn't
            // in the visible list (e.g. user just enabled a filter that hides
            // it), fall back to None so the next move starts from the top.
            let cur_idx = self.files_selection_anchor.as_ref()
                .and_then(|a| filtered.iter().position(|e| &e.path == a));

            let last = filtered.len() - 1;
            let page = 10usize;
            let mut new_idx: Option<usize> = None;

            if k_down {
                new_idx = Some(match cur_idx { Some(i) if i < last => i + 1, _ => 0 });
            } else if k_up {
                new_idx = Some(match cur_idx { Some(i) if i > 0 => i - 1, Some(_) => 0, None => last });
            } else if k_home {
                new_idx = Some(0);
            } else if k_end {
                new_idx = Some(last);
            } else if k_pgdn {
                new_idx = Some(match cur_idx { Some(i) => (i + page).min(last), None => 0 });
            } else if k_pgup {
                new_idx = Some(match cur_idx { Some(i) => i.saturating_sub(page), None => 0 });
            }

            if let Some(idx) = new_idx {
                let path = filtered[idx].path.clone();
                self.files_selected.clear();
                self.files_selected.insert(path.clone());
                self.files_selection_anchor = Some(path);
                scroll_to_index = Some(idx);
            }

            // ── Type-to-navigate ──
            // Read any Text events this frame (typed characters from the
            // keyboard or IME). Append to the buffer if the user is still
            // typing within the 750ms window; otherwise reset and start fresh.
            // Then jump to the first visible entry whose name starts with the
            // buffer (case-insensitive). Finder/Files-style behavior.
            let typed: String = ui.input(|i| {
                i.events.iter()
                    .filter_map(|e| if let egui::Event::Text(s) = e { Some(s.clone()) } else { None })
                    .collect()
            });
            if !typed.is_empty() {
                // Expire the buffer if too long has passed since the last keystroke.
                let now = std::time::Instant::now();
                let expired = self.type_ahead_last_input
                    .map(|t| now.duration_since(t) > std::time::Duration::from_millis(750))
                    .unwrap_or(true);
                if expired {
                    self.type_ahead_buffer.clear();
                }
                self.type_ahead_buffer.push_str(&typed);
                self.type_ahead_last_input = Some(now);

                // Find first visible entry whose name starts with the buffer.
                let needle = self.type_ahead_buffer.to_lowercase();
                if let Some((idx, entry)) = filtered.iter().enumerate()
                    .find(|(_, e)| e.name.to_lowercase().starts_with(&needle))
                {
                    let path = entry.path.clone();
                    self.files_selected.clear();
                    self.files_selected.insert(path.clone());
                    self.files_selection_anchor = Some(path);
                    scroll_to_index = Some(idx);
                }
                // No match → leave selection alone (no flash, no error).
                // Buffer still updated so a subsequent keystroke can extend it.
            }

            // Enter: activate the current anchor (or first row if none).
            if k_enter {
                let activate_idx = cur_idx.or(Some(0));
                if let Some(idx) = activate_idx {
                    let entry = filtered[idx];
                    if entry.is_dir {
                        pending_navigate = Some(entry.path.clone());
                    } else if entry.is_media {
                        cmds.push(AppCommand::LoadMediaFile {
                            path: entry.path.clone(),
                            media_id: klipp_core::MediaId::new(),
                        });
                    }
                }
            }

            // Backspace: go up one directory (same as the back button).
            if k_back {
                if let Some(parent) = self.files_current_path.parent() {
                    pending_navigate = Some(parent.to_path_buf());
                }
            }
        }

        // Reserve vertical space for the status bar at the bottom of the panel.
        // The scroll area is capped to (available - status_h) and pinned to its
        // max size via auto_shrink([false, false]), so the status bar always
        // sits at the bottom regardless of how few/many file rows there are.
        let status_h = 22.0_f32;
        let scroll_h = (ui.available_height() - status_h).max(0.0);

        egui::ScrollArea::vertical()
            .id_salt("files_scroll")
            .max_height(scroll_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
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
                    // click_and_drag sense captures click (single + double),
                    // secondary click (right-click → context menu), AND the
                    // drag-started event used to begin a cross-panel drag.
                    egui::Sense::click_and_drag(),
                );

                // Keyboard navigation just moved selection to this row →
                // scroll it into view (centered) on this frame.
                if Some(row_idx) == scroll_to_index {
                    ui.scroll_to_rect(rect, Some(egui::Align::Center));
                }

                // Drag-and-drop start: when the user begins dragging a row,
                // populate the global drag_drop state with the file path(s)
                // so the timeline / media tab can detect the drop on hover.
                //
                // Multi-file logic mirrors the right-click context menu:
                // if the dragged row is part of a multi-selection, drag all
                // selected media files; otherwise drag just this row.
                // Directories are NOT draggable (no semantic for them).
                if resp.drag_started() && !entry.is_dir {
                    let drag_paths: Vec<std::path::PathBuf> = if self.files_selected.contains(&entry.path)
                        && self.files_selected.len() > 1
                    {
                        filtered.iter()
                            .filter(|e| !e.is_dir && e.is_media && self.files_selected.contains(&e.path))
                            .map(|e| e.path.clone())
                            .collect()
                    } else if entry.is_media {
                        vec![entry.path.clone()]
                    } else {
                        Vec::new()
                    };
                    if !drag_paths.is_empty() {
                        state.drag_drop.begin(crate::drag_drop::DragPayload::FilePaths {
                            paths: drag_paths,
                        });
                    }
                }
                // Update cursor position while a drag is in progress so the
                // floating ghost (rendered globally in app.rs) follows the
                // pointer.
                if state.drag_drop.is_active() {
                    if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                        state.drag_drop.update_pos(p);
                    }
                }

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
                    // Texture priority: live skim frame for THIS path > cached
                    // poster thumbnail. The skim texture is single-slot, so at
                    // most one row at a time can show a live frame.
                    let tex_to_use: Option<&egui::TextureHandle> = self.file_skim_texture
                        .as_ref()
                        .filter(|(p, _)| *p == entry.path)
                        .map(|(_, t)| t)
                        .or_else(|| self.file_thumb_textures.get(&entry.path));

                    if let Some(tex) = tex_to_use {
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

                    // ── Hover skimming ──
                    // Only for VIDEO files (audio + still images don't have a
                    // duration to skim through). Gated on the global toolbar
                    // toggle `state.skimming_enabled`.
                    let ext = entry.path.extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let is_video = VIDEO_EXTENSIONS.contains(&ext.as_str());
                    if is_video && state.skimming_enabled && !state.has_modal_open() {
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            if thumb_box.contains(pos) {
                                any_video_row_hovered = true;
                                // Need duration to map cursor X → time. Cached.
                                if let Some(dur) = self.file_durations.get(&entry.path).copied() {
                                    let frac = ((pos.x - thumb_box.min.x) / thumb_box.width())
                                        .clamp(0.0, 1.0);
                                    let t = dur * frac as f64;
                                    pending_skim_request = Some((entry.path.clone(), t));
                                } else if pending_duration_probe.is_none() {
                                    // First hover — defer the probe to after the
                                    // loop so we can call self.ensure_file_duration.
                                    pending_duration_probe = Some(entry.path.clone());
                                }
                            }
                        }
                    }
                }

                // ── Audio waveform thumbnails ──
                // For audio files, draw a tiny waveform inside the same
                // thumb_box that videos use for posters. Lazy-loaded from
                // the shared disk cache (~/.cache/klipp/audio-peaks/).
                if !painted_thumb && !entry.is_dir {
                    let ext = entry.path.extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let is_audio = AUDIO_EXTENSIONS.contains(&ext.as_str());
                    if is_audio {
                        if let Some(peaks) = self.file_audio_peaks.get(&entry.path) {
                            // Draw the waveform: vertical line per bucket from
                            // (x, center - max*half_h) to (x, center - min*half_h).
                            // The peaks are normalized to roughly [-1, 1] f32.
                            ui.painter().rect_filled(
                                thumb_box,
                                2.0,
                                egui::Color32::from_rgb(18, 18, 18),
                            );
                            let center_y = thumb_box.center().y;
                            let half_h = thumb_box.height() * 0.45; // 90% range, 5% margin top/bottom
                            let bucket_count = peaks.len();
                            if bucket_count > 0 {
                                let bucket_w = thumb_box.width() / bucket_count as f32;
                                let stroke = egui::Stroke::new(
                                    bucket_w.max(1.0),
                                    theme.text_secondary,
                                );
                                for (i, (min, max)) in peaks.iter().enumerate() {
                                    // Clamp to keep insanely loud peaks in-bounds.
                                    let lo = min.clamp(-1.0, 1.0);
                                    let hi = max.clamp(-1.0, 1.0);
                                    let x = thumb_box.min.x + (i as f32 + 0.5) * bucket_w;
                                    let y_top = center_y - hi * half_h;
                                    let y_bot = center_y - lo * half_h;
                                    // Always draw at least 1px so silent passages are visible.
                                    let y_top = y_top.min(center_y - 0.5);
                                    let y_bot = y_bot.max(center_y + 0.5);
                                    ui.painter().line_segment(
                                        [egui::pos2(x, y_top), egui::pos2(x, y_bot)],
                                        stroke,
                                    );
                                }
                            }
                            painted_thumb = true;
                        } else if !self.file_audio_failed.contains(&entry.path)
                            && ui.is_rect_visible(rect)
                        {
                            // Visible row that wants audio peaks but doesn't
                            // have them — queue a generation request.
                            audio_peaks_spawn_requests.push(entry.path.clone());
                        }
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
                    let icon = if entry.is_dir {
                        Icon::Folder
                    } else {
                        let ext = entry.path.extension()
                            .map(|e| e.to_string_lossy().to_lowercase())
                            .unwrap_or_default();
                        if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
                            // Audio file with peaks not yet loaded → music note
                            // glyph as the placeholder, replaced by the waveform
                            // once the worker finishes.
                            Icon::MusicNote
                        } else {
                            Icon::FilmStrip
                        }
                    };
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

                // Any primary click on a row gives the files panel keyboard
                // focus, so subsequent arrow keys / Enter / Backspace navigate
                // the file list. Cleared automatically when any text-edit
                // widget takes focus elsewhere.
                if resp.clicked() || resp.double_clicked() {
                    self.files_panel_focused = true;
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
                // Snapshot of "is this directory currently pinned" so the menu
                // closure can read it without borrowing self.pinned_folders.
                let menu_is_pinned = entry.is_dir && self.pinned_folders.iter().any(|p| p == &entry.path);
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

                    // ── Pin / Unpin Favorites (directories only) ──
                    if menu_is_dir {
                        let pin_label = if menu_is_pinned {
                            "Unpin from Favorites"
                        } else {
                            "Pin to Favorites"
                        };
                        if ui.button(pin_label).clicked() {
                            pending_pin_toggle = Some(menu_path.clone());
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

        // ── Status bar (bottom of files panel) ──
        // MUST come BEFORE the deferred-mutation block below, because the
        // status bar reads `filtered` which borrows `self.files_entries`.
        // Capture all the counts/sizes into locals first so the borrow on
        // `filtered` is dropped before any `self.navigate_files_to(...)` call.
        let total_count = self.files_entries.len();
        let visible_count = filtered.len();
        let selected_count = self.files_selected.len();
        let selected_size: u64 = filtered.iter()
            .filter(|e| !e.is_dir && self.files_selected.contains(&e.path))
            .map(|e| e.size)
            .sum();
        let is_loading = self.files_loading;
        let has_error = self.files_error.is_some();

        let (status_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), status_h),
            egui::Sense::hover(),
        );
        ui.painter().rect_filled(status_rect, 0.0, theme.bg_deep);
        ui.painter().hline(
            status_rect.x_range(),
            status_rect.min.y,
            egui::Stroke::new(0.5, theme.border),
        );

        // Build status text. Format follows Finder/Files conventions:
        //   "247 items"                                  (default)
        //   "187 of 247 items"                           (filter or search active)
        //   "247 items  ·  5 selected  ·  312.4 MB"      (with selection)
        let status_text = if is_loading {
            "Loading\u{2026}".to_string()
        } else if has_error {
            // Error state already shown inside the scroll area; keep status bar empty.
            String::new()
        } else {
            let mut parts: Vec<String> = Vec::new();
            if visible_count != total_count {
                parts.push(format!("{} of {} items", visible_count, total_count));
            } else {
                parts.push(format!("{} items", total_count));
            }
            if selected_count > 0 {
                parts.push(format!("{} selected", selected_count));
                if selected_size > 0 {
                    parts.push(format_file_size(selected_size));
                }
            }
            parts.join("  \u{00B7}  ")
        };
        if !status_text.is_empty() {
            ui.painter().text(
                egui::pos2(status_rect.min.x + 8.0, status_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &status_text,
                egui::FontId::proportional(10.0),
                theme.text_dim,
            );
        }

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
        if !audio_peaks_spawn_requests.is_empty() {
            for path in audio_peaks_spawn_requests {
                self.spawn_file_audio_peaks(path);
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

        // Right-click → toggle pin/unpin a directory in the favorites list.
        // Persistence is handled by the diff check in `render_browser_panel`.
        if let Some(target) = pending_pin_toggle {
            if let Some(idx) = self.pinned_folders.iter().position(|p| p == &target) {
                self.pinned_folders.remove(idx);
            } else {
                self.pinned_folders.push(target);
            }
        }

        // Hover skim — apply requests captured during the row loop.
        // 1. If a duration probe is needed (first hover), do it synchronously
        //    here. The next frame will pick up the cached duration and the
        //    actual skim request will fire.
        if let Some(probe_path) = pending_duration_probe {
            let _ = self.ensure_file_duration(&probe_path);
        }
        // 2. If the cursor is on a video row with a known duration, request
        //    a skim decode at the computed timestamp.
        if let Some((skim_path, skim_time)) = pending_skim_request {
            let ctx = ui.ctx().clone();
            self.request_file_skim(skim_path, skim_time, &ctx);
        }
        // 3. If no video row was hovered this frame, drop the live skim
        //    texture so the previously-skimmed row reverts to its poster.
        if !any_video_row_hovered && self.file_skim_texture.is_some() {
            self.file_skim_texture = None;
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

// Computed once at startup from the canonical klipp_core constants. The old
// `pub(super) const MEDIA_EXTENSIONS: &[&str] = &[...]` was a hardcoded
// duplicate of the union of the three category constants — it's now a
// `OnceLock<Vec<&'static str>>` so the lists stay in sync automatically.
//
// Callers that previously did `MEDIA_EXTENSIONS.contains(&ext.as_str())`
// continue to work because we expose it as a function returning `&[&str]`.
pub(super) fn media_extensions() -> &'static [&'static str] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<&'static str>> = OnceLock::new();
    CACHE.get_or_init(klipp_core::media_extensions::all_media_extensions).as_slice()
}

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
        let is_media = !is_dir && klipp_core::media_extensions::is_media(&ext);

        entries.push(FileEntry {
            name,
            path: entry.path(),
            is_dir,
            size: if is_dir { 0 } else { meta.len() },
            is_media,
            mtime: meta.modified().ok(),
        });
    }

    // Note: sorting is applied by the caller after the worker returns,
    // because the sort key is part of UI state and the worker doesn't see it.

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
    use super::FILE_THUMB_MAX_WIDTH;

    /// Helper: walk a directory and collect media file paths, mirroring the
    /// ImportMediaFolder handler logic in app.rs.
    fn collect_media_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                if klipp_core::media_extensions::path_is_media(entry.path()) {
                    files.push(entry.path().to_path_buf());
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
    fn percent_decode_basic_and_escapes() {
        assert_eq!(super::percent_decode("/home/david"), "/home/david");
        assert_eq!(super::percent_decode("/home/david/My%20Documents"), "/home/david/My Documents");
        // Multiple escapes
        assert_eq!(super::percent_decode("a%20b%20c"), "a b c");
        // Lone percent treated as literal (lenient parsing)
        assert_eq!(super::percent_decode("100%"), "100%");
        // Invalid hex digits → literal
        assert_eq!(super::percent_decode("%ZZ"), "%ZZ");
    }

    #[test]
    fn file_uri_to_path_handles_file_scheme_only() {
        use std::path::PathBuf;
        assert_eq!(
            super::file_uri_to_path("file:///home/david/Pictures"),
            Some(PathBuf::from("/home/david/Pictures"))
        );
        // Percent-decoded
        assert_eq!(
            super::file_uri_to_path("file:///home/david/My%20Folder"),
            Some(PathBuf::from("/home/david/My Folder"))
        );
        // Non-file schemes are rejected
        assert_eq!(super::file_uri_to_path("remote:/"), None);
        assert_eq!(super::file_uri_to_path("trash:/"), None);
        assert_eq!(super::file_uri_to_path("recentlyused:/files"), None);
        assert_eq!(super::file_uri_to_path(""), None);
        assert_eq!(super::file_uri_to_path("file://"), None);
    }

    #[test]
    fn parse_kde_xbel_skips_system_items_and_non_file_uris() {
        use std::collections::HashSet;
        // Use a real existing directory so the `path.is_dir()` check passes
        // (the parser only emits real, accessible directories).
        let tmp = std::env::temp_dir().join(format!("klipp_test_xbel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let project_dir = tmp.join("Project Folder");
        std::fs::create_dir_all(&project_dir).unwrap();

        let xbel = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel>
 <bookmark href="file:///home/david">
  <title>Home</title>
  <info>
   <metadata owner="http://www.kde.org">
    <isSystemItem>true</isSystemItem>
   </metadata>
  </info>
 </bookmark>
 <bookmark href="remote:/">
  <title>Network</title>
 </bookmark>
 <bookmark href="file://{}">
  <title>Project</title>
 </bookmark>
</xbel>"#,
            project_dir.display()
        );

        let mut out = Vec::new();
        let mut seen = HashSet::new();
        super::parse_kde_xbel(&xbel, &mut out, &mut seen);

        // Home is system → skipped. Network is non-file → skipped.
        // Project is the only real entry.
        assert_eq!(out.len(), 1, "expected exactly one entry, got {:?}", out);
        assert_eq!(out[0].0, "Project");
        assert_eq!(out[0].1, project_dir);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_gtk_bookmarks_handles_format_variants() {
        use std::collections::HashSet;
        let tmp = std::env::temp_dir().join(format!("klipp_test_gtk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let dir_a = tmp.join("Folder A");
        let dir_b = tmp.join("FolderB");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        // dir_a uses an explicit display name override; dir_b uses the
        // default (last path component). Also includes a comment, a blank
        // line, and a non-existent path that should be filtered out.
        let contents = format!(
            "# a comment line\n\
             \n\
             file://{}/Folder%20A My Custom Name\n\
             file://{}/FolderB\n\
             file:///nonexistent/should/be/skipped\n",
            tmp.display(),
            tmp.display()
        );

        let mut out = Vec::new();
        let mut seen = HashSet::new();
        super::parse_gtk_bookmarks(&contents, &mut out, &mut seen);

        assert_eq!(out.len(), 2, "expected 2 entries, got {:?}", out);
        // First entry: explicit name + percent-decoded path
        assert_eq!(out[0].0, "My Custom Name");
        assert_eq!(out[0].1, dir_a);
        // Second entry: default name from path
        assert_eq!(out[1].0, "FolderB");
        assert_eq!(out[1].1, dir_b);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn file_type_filter_matches_correct_extensions_and_keeps_dirs() {
        use std::path::PathBuf;
        let mk = |name: &str, is_dir: bool| super::FileEntry {
            name: name.to_string(),
            path: PathBuf::from(name),
            is_dir,
            size: 0,
            is_media: false,
            mtime: None,
        };

        let dir = mk("subfolder", true);
        let video = mk("clip.mp4", false);
        let audio = mk("song.mp3", false);
        let image = mk("photo.jpg", false);
        let text = mk("readme.txt", false);

        // All filter — everything passes (including non-media .txt).
        assert!(super::FileTypeFilter::All.matches(&dir));
        assert!(super::FileTypeFilter::All.matches(&video));
        assert!(super::FileTypeFilter::All.matches(&audio));
        assert!(super::FileTypeFilter::All.matches(&image));
        assert!(super::FileTypeFilter::All.matches(&text));

        // Video filter — directories always pass; only video extensions match.
        assert!(super::FileTypeFilter::Video.matches(&dir), "dirs always visible");
        assert!(super::FileTypeFilter::Video.matches(&video));
        assert!(!super::FileTypeFilter::Video.matches(&audio));
        assert!(!super::FileTypeFilter::Video.matches(&image));
        assert!(!super::FileTypeFilter::Video.matches(&text));

        // Audio filter
        assert!(super::FileTypeFilter::Audio.matches(&dir));
        assert!(!super::FileTypeFilter::Audio.matches(&video));
        assert!(super::FileTypeFilter::Audio.matches(&audio));
        assert!(!super::FileTypeFilter::Audio.matches(&image));
        assert!(!super::FileTypeFilter::Audio.matches(&text));

        // Images filter
        assert!(super::FileTypeFilter::Images.matches(&dir));
        assert!(!super::FileTypeFilter::Images.matches(&video));
        assert!(!super::FileTypeFilter::Images.matches(&audio));
        assert!(super::FileTypeFilter::Images.matches(&image));
        assert!(!super::FileTypeFilter::Images.matches(&text));

        // Default filter is All.
        assert_eq!(super::FileTypeFilter::default(), super::FileTypeFilter::All);
    }

    #[test]
    fn sort_file_entries_keeps_dirs_first_and_respects_key() {
        use std::path::PathBuf;
        use std::time::{Duration, UNIX_EPOCH};

        let mk = |name: &str, is_dir: bool, size: u64, mtime_secs: u64| super::FileEntry {
            name: name.to_string(),
            path: PathBuf::from(name),
            is_dir,
            size,
            is_media: false,
            mtime: Some(UNIX_EPOCH + Duration::from_secs(mtime_secs)),
        };

        // Mixed dirs + files in random order.
        let mut entries = vec![
            mk("zebra.mp4", false, 5_000, 100),
            mk("alpha.txt", false, 1_000, 300),
            mk("bravo_dir", true, 0, 200),
            mk("alpha_dir", true, 0, 50),
            mk("middle.png", false, 2_500, 200),
        ];

        // Sort by name ascending. Dirs first (alpha order), then files (alpha order).
        super::sort_file_entries(&mut entries, super::FileSortBy::Name, true);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha_dir", "bravo_dir", "alpha.txt", "middle.png", "zebra.mp4"]);

        // Sort by size descending. Dirs still first.
        super::sort_file_entries(&mut entries, super::FileSortBy::Size, false);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names[0], "alpha_dir");
        assert_eq!(names[1], "bravo_dir");
        // Files: 5000, 2500, 1000
        assert_eq!(&names[2..], &["zebra.mp4", "middle.png", "alpha.txt"]);

        // Sort by date modified ascending. Dirs first by their own mtime.
        super::sort_file_entries(&mut entries, super::FileSortBy::DateModified, true);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        // dirs sorted by mtime: alpha_dir(50), bravo_dir(200)
        // files sorted by mtime: zebra(100), middle(200), alpha(300)
        assert_eq!(names, vec!["alpha_dir", "bravo_dir", "zebra.mp4", "middle.png", "alpha.txt"]);

        // Sort by type. Files grouped by extension; tiebreak on name.
        super::sort_file_entries(&mut entries, super::FileSortBy::Type, true);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        // dirs first, then files in extension order (mp4, png, txt)
        assert_eq!(names[0], "alpha_dir");
        assert_eq!(names[1], "bravo_dir");
        assert_eq!(&names[2..], &["zebra.mp4", "middle.png", "alpha.txt"]);
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
