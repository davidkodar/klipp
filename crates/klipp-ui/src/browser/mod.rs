use crate::widgets::menu::{SubmenuRows, MenuRows};
pub mod media_tab;
pub mod effect_registry;
pub mod effect_thumbnails;
pub mod effects_tab;
pub mod library_card;
pub mod audio_fx_tab;
pub mod titles_tab;
pub mod title_previews;
pub mod transition_previews;
pub mod generator_previews;
pub mod transitions_tab;
pub mod generators_tab;
pub mod shared_tab;

use crate::widgets::menu::{menu_body, menu_item};
use crate::icons::Icon;
use crate::layout_state::{BrowserTab, LibraryCategory};
use crate::templates_tab;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Unified search input used by every browser tab + library sub-tab so
/// every search bar in the dock looks and behaves identically: the
/// raised-bg rounded frame, the slim frameless TextEdit, the accent
/// glow ring on focus, and the same Ctrl+F focus consumer (matched
/// against `PanelKind::Browser`). Library sub-tabs previously rendered
/// a 32px-tall bespoke version with a magnifier icon; that diverged
/// from the Files/Media bar and is now consolidated here.
pub fn browser_search_field(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    query: &mut String,
    placeholder: &str,
    state: &mut AppState,
) {
    let framed = egui::Frame::none()
        .fill(theme.bg_raised)
        .stroke(egui::Stroke::new(1.0_f32, theme.border))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(9.0, 5.0))
        .show(ui, |ui| {
            let search = egui::TextEdit::singleline(query)
                .hint_text(placeholder)
                .desired_width(ui.available_width() - 4.0)
                .font(egui::FontId::proportional(12.0))
                .text_color(theme.text_primary)
                .frame(false);
            let resp = ui.add(search);
            // Ctrl+F focus consumer — single shared check so every
            // search field grabs focus the same way.
            if state.focus_search_request == Some(crate::state::PanelKind::Browser) {
                resp.request_focus();
                state.focus_search_request = None;
            }
            // Accent border + soft glow ring on focus. Matches the
            // Files/Media bar's emphasis state — gives users a single
            // visual cue across all search fields.
            if resp.has_focus() {
                let frame_rect = resp.rect.expand2(egui::vec2(9.0, 5.0));
                ui.painter().rect_stroke(
                    frame_rect.expand(2.0),
                    7.0,
                    egui::Stroke::new(3.0_f32, theme.accent_subtle),
                );
                ui.painter().rect_stroke(
                    frame_rect,
                    5.0,
                    egui::Stroke::new(1.0_f32, theme.accent),
                );
            }
            (resp.id, resp.rect)
        });
    // The frame around the text is the field too: a click on its padding
    // (most of a narrow field's height) used to focus nothing, and the
    // typing that followed ran single-key shortcuts (#2551).
    let (text_id, text_rect) = framed.inner;
    let on_padding = ui.input(|i| i.pointer.primary_clicked() && i.pointer.interact_pos().is_some_and(|p| !text_rect.contains(p)));
    if on_padding && ui.rect_contains_pointer(framed.response.rect) {
        ui.memory_mut(|m| m.request_focus(text_id));
    }
}

/// The rows a keyboard move leaves selected in the Files list (#3019), as
/// indices into the rows shown, `lo..=hi`: the row moved to alone, or with
/// `extend` (Shift held) every row from the anchor to it.
fn keyboard_selection(anchor: Option<usize>, to: usize, extend: bool) -> (usize, usize) {
    match anchor.filter(|_| extend) {
        Some(a) => (a.min(to), a.max(to)),
        None => (to, to),
    }
}

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

/// Result from a filmstrip generation worker — one complete set of frames
/// for a single video file.
struct FileFilmstripResult {
    path: std::path::PathBuf,
    /// Frames in time order. Each tuple is (rgba, width, height).
    frames: Vec<(Vec<u8>, u32, u32)>,
}

/// Number of frames in a file-browser filmstrip hover strip.
const FILE_FILMSTRIP_FRAME_COUNT: usize = 8;

/// Number of waveform buckets to compute for audio thumbnails. Matches
/// roughly the pixel width of the Large thumb so even at the biggest size
/// we have one bucket per pixel column.
const FILE_AUDIO_PEAKS_BUCKETS: usize = 144;

/// Max concurrent file-thumbnail decode workers. Software decode only,
/// so this is CPU-bounded — keep modest to avoid starving the UI.
const FILE_THUMB_MAX_INFLIGHT: usize = 4;
/// Max width to decode thumbs at. Matches the media-pool cache so the
/// disk file is reusable across both views.
pub(crate) const FILE_THUMB_MAX_WIDTH: u32 = 480;

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
/// Start watching a directory for filesystem changes (create/delete/rename).
/// Returns the watcher handle + a receiver that gets a `()` signal whenever
/// a debounced batch of changes is detected. The debounce window is 400ms
/// — rapid file writes (large copies, renders) are collapsed into a single
/// notification so the browser doesn't flicker.
///
/// Returns `(None, None)` if the directory doesn't exist or the watcher
/// can't be initialized (e.g. too many inotify watches). Callers should
/// fall back gracefully to manual "Refresh Folder".
fn start_folder_watcher(
    dir: &std::path::Path,
) -> (
    Option<notify::RecommendedWatcher>,
    Option<crossbeam_channel::Receiver<()>>,
) {
    use notify::{EventKind, RecursiveMode, Watcher};

    if !dir.exists() || !dir.is_dir() {
        return (None, None);
    }

    let (tx, rx) = crossbeam_channel::bounded::<()>(4);

    // Debounce: the watcher callback fires on every individual OS event
    // (Create, Remove, Rename, Modify). We don't need per-file detail —
    // just "something changed in this directory". A bounded channel with
    // try_send means bursts of events collapse naturally: if the channel
    // is already full the extra sends are silently dropped, and the
    // consumer drains once per frame (~16ms at 60fps). The 400ms wall-
    // clock debounce happens implicitly because we only act on the drain
    // in show_files_tab, which runs at vsync rate.
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            let relevant = matches!(
                event.kind,
                EventKind::Create(_)
                    | EventKind::Remove(_)
                    | EventKind::Modify(notify::event::ModifyKind::Name(_))
            );
            if relevant {
                let _ = tx.try_send(());
            }
        }
    });

    match watcher {
        Ok(mut w) => {
            if w.watch(dir, RecursiveMode::NonRecursive).is_ok() {
                tracing::debug!("folder_watcher: watching {:?}", dir);
                (Some(w), Some(rx))
            } else {
                (None, None)
            }
        }
        Err(e) => {
            tracing::debug!("folder_watcher: failed to create watcher: {}", e);
            (None, None)
        }
    }
}

/// Sentinel path that represents the virtual "Recent files" view. Not a
/// real filesystem path — used to detect "we're in recents mode" so the
/// file browser can populate `files_entries` from the prefs list instead
/// of reading a directory. Uses a URI-style scheme to avoid any possible
/// collision with a real filesystem path.
static RECENTS_SENTINEL: std::sync::LazyLock<std::path::PathBuf> =
    std::sync::LazyLock::new(|| std::path::PathBuf::from("klipp://recents"));

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
    let cache_key = klipp_engine::video_decoder::picture_cache_key(path);

    // Try disk cache first (shared with media-pool thumbnails). RAW files
    // use the same cache because the embedded JPEG preview is what we
    // ultimately store, not the bayer data.
    if let Some(ref key) = cache_key {
        if klipp_media::disk_cache::thumbnail_cache_exists(key, FILE_THUMB_MAX_WIDTH) {
            if let Ok((rgba, w, h)) = klipp_media::disk_cache::load_thumbnail_cache(key, FILE_THUMB_MAX_WIDTH) {
                return Ok((rgba, w, h));
            }
        }
    }

    // ── Decode path selection ──
    // RAW camera files (ARW, CR2, NEF, RAF, ORF, DNG, etc.) can't be decoded
    // by stock ffmpeg — they need their embedded JPEG preview extracted.
    // Every other still / video goes through the regular ffmpeg path.
    let (rgba, w, h) = if klipp_core::media_extensions::path_is_raw(path) {
        klipp_engine::extract_raw_preview(path, FILE_THUMB_MAX_WIDTH)?
    } else {
        // Stills decode at seek=0, duration=0. Videos use the non-black
        // variant which probes 0/5/10/20/30%.
        klipp_engine::generate_thumbnail_at_size_non_black(
            path, 0.0, 0.0, FILE_THUMB_MAX_WIDTH,
        )?
    };

    if let Some(ref key) = cache_key {
        let _ = klipp_media::disk_cache::write_thumbnail_cache(key, FILE_THUMB_MAX_WIDTH, &rgba, w, h);
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

/// Columns that can be shown in the file browser list view.
/// Name is always visible and not toggleable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ColumnId {
    Size,
    Date,
    Type,
    Duration,
    Rating,
}

impl ColumnId {
    pub fn label(self) -> &'static str {
        match self {
            Self::Size => "Size",
            Self::Date => "Date",
            Self::Type => "Type",
            Self::Duration => "Duration",
            Self::Rating => "Rating",
        }
    }

    /// Fixed width in pixels for each column.
    pub fn width(self) -> f32 {
        match self {
            Self::Size => 60.0,
            Self::Date => 80.0,
            Self::Type => 50.0,
            Self::Duration => 55.0,
            Self::Rating => 50.0,
        }
    }

    /// All available column IDs in display order (left to right after Name).
    pub fn all() -> &'static [ColumnId] {
        &[Self::Size, Self::Date, Self::Type, Self::Duration, Self::Rating]
    }
}

/// Quick type filter for the Files tab. Persisted in FileBrowserPrefs.
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

/// Sort key for the Files tab. Persisted in FileBrowserPrefs.
/// Directories are always grouped above files regardless of the chosen key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum FileSortBy {
    #[default]
    Name,
    DateModified,
    Size,
    Type,
    Rating,
}

impl FileSortBy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::DateModified => "Date",
            Self::Size => "Size",
            Self::Type => "Type",
            Self::Rating => "Rating",
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
            FileSortBy::Rating => a.rating.cmp(&b.rating),
        };
        if ascending { ord } else { ord.reverse() }
    });
}

/// Smart bin filter criteria for auto-filtering media. The rating filter
/// lives separately on `BrowserPanel.min_rating_filter` so the two
/// dropdowns combine with AND semantics (e.g. "Video Only" + "3+ stars").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmartBinFilter {
    None,
    Unused,
    VideoOnly,
    AudioOnly,
    StillsOnly,
    /// Show media items whose source file is currently offline /
    /// missing on disk. Reads `MediaItem.offline`, which the project
    /// loader and media-pool refresh paths set when stat fails on
    /// the recorded path. Useful for triaging broken-link projects.
    Offline,
    /// Clips with a favourite stretch.
    Favourites,
    /// Clips with a rejected stretch.
    Rejected,
    /// A saved smart bin, by id: the clips its rules gather.
    Saved(String),
}

/// Parametric metadata filter for the bin — narrows by codec, source
/// fps, or camera identifier, with the available values pulled
/// dynamically from the current bin contents (see
/// `enumerate_metadata_filter_values`). Combines with `SmartBinFilter`
/// + the rating + colour-label filters using AND semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
#[derive(Default)]
pub enum MetadataFilter {
    /// Disable the filter — every item passes.
    #[default]
    Any,
    /// Only show items whose `MediaItem.video_codec` (case-insensitive
    /// equal) matches the chosen value (e.g. "h264", "hevc").
    Codec(String),
    /// Only show items whose `MediaItem.fps` matches exactly. Encodes
    /// num + den as plain ints so the dropdown round-trips between
    /// renders without floating-point drift.
    Fps { num: u32, den: u32 },
    /// Only show items whose `MediaItem.camera_make` matches the
    /// chosen value (case-insensitive equal). Items with no recorded
    /// camera-make are excluded.
    Camera(String),
    /// The lens the shot was made with, as the camera named it.
    Lens(String),
    /// A resolution, width by height.
    Resolution(u32, u32),
    /// The reel the clip goes under, the editor's or the file's.
    Reel(String),
    /// The scene the editor logged.
    Scene(String),
    /// A keyword the editor logged.
    Keyword(String),
    /// Only good takes.
    GoodTake,
}


/// Thumbnail size for the Files tab. Controls row height, thumb box size,
/// and where the filename is positioned. Persisted in FileBrowserPrefs.
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
    /// Search query for the Media and Library tabs (the "right pane" in
    /// split view). Deliberately NOT shared with Files — typing effects
    /// queries here must not filter the file list rendered alongside.
    pub search_query: String,
    /// Search query for the Files tab only. Kept separate from
    /// `search_query` so effects searches don't leak into the files
    /// panel when it's pinned as the split-view left pane.
    pub files_search_query: String,
    // Media tab state (migrated from MediaPanel)
    pub media_sort_by: media_tab::SortBy,
    /// The list view's columns, in order; kept in the preferences.
    pub list_columns: Vec<klipp_media::Field>,
    pub media_sort_ascending: bool,
    pub media_selected_bin: Option<String>,
    pub smart_bin_filter: SmartBinFilter,
    /// The smart bin being written or edited, while its window is open.
    pub smart_bin_editor: Option<klipp_core::SmartBin>,
    /// Commands raised by the toolbar and its windows, drained into
    /// `show`'s return.
    pending_cmds: Vec<AppCommand>,
    /// Minimum star rating the bin filters to. `None` = show all ratings.
    /// Session state only — deliberately not persisted to prefs for MVP.
    pub min_rating_filter: Option<u8>,
    /// Bitmask of active colour-label filters. Bit N corresponds to
    /// `ColorLabel::ALL[N]` (i.e. `ColorLabel::from_u8(N)`). OR semantics:
    /// a clip passes when ANY of its label bits matches an active bit.
    /// `0` disables the filter (all labels pass). Session state only.
    pub color_label_filter: u8,
    /// Metadata filter — narrows by codec / fps / camera using a single
    /// parametric enum. Available values are computed from current bin
    /// contents at render time (see `enumerate_metadata_filter_values`).
    pub metadata_filter: MetadataFilter,
    pub browser_view_mode: media_tab::BrowserViewMode,
    // Bin management state
    pub renaming_bin: Option<(String, String)>,  // (old_name, current_edit_text)
    // Effects tab state
    pub effects_category: effects_tab::EffectCategory,
    // Audio FX tab state — kept distinct from effects_category so the
    // user's video category stays intact when they toggle to Audio FX
    // and back.
    pub audio_fx_category: effects_tab::EffectCategory,
    pub audio_fx_source_filter: audio_fx_tab::AudioSourceFilter,
    pub audio_fx_sort: effects_tab::EffectSort,
    /// Visual play/pause state for the Audition transport at the top of
    /// the Audio FX tab. Wiring to the audio engine lands later — this
    /// just toggles the icon for now.
    pub audio_fx_audition_playing: bool,
    /// User-picked audition source clip from the timeline. When `None`
    /// the audition transport falls back to the most recent timeline
    /// selection (which also feeds into the source dropdown). Persisted
    /// only as long as the clip exists; cleared automatically by the
    /// renderer when the clip is gone.
    pub audio_fx_audition_source: Option<klipp_core::ClipId>,
    /// Audition transport volume (0.0–1.5, 1.0 = unity). Surfaced as
    /// the slider in the audition header; pushed to the active
    /// `SkimAudioSession` via `AppCommand::AuditionSetVolume` when the
    /// user drags it.
    pub audio_fx_audition_volume: f32,
    /// Audition playhead position in seconds at the last anchor point
    /// (play start or seek). Live position = `anchor + (now - anchor_at)`
    /// while playing.
    pub audio_fx_audition_anchor_secs: f64,
    /// Wall-clock instant at which the audition position anchor was
    /// last set. `None` when paused so the displayed position freezes.
    pub audio_fx_audition_anchor_at: Option<std::time::Instant>,
    /// Name of the currently-selected effect (highlight only). Set
    /// when the user clicks the row body; does not start playback.
    pub audio_fx_selected: Option<String>,
    /// Name of the effect currently being auditioned (= the play
    /// button on that row is filled, with a stop glyph). Set only by
    /// clicking a row's play button — separate from selection so the
    /// user can pick a row to inspect without starting audio.
    /// Visual-only for now; audition routing lands later.
    pub audio_fx_auditioning: Option<String>,
    /// Mirror of the eye-toggle on the filter row. Hides plugins flagged
    /// as not yet wired up. No effect today; lays the groundwork for the
    /// missing-binding sweep.
    pub audio_fx_hide_unsupported: bool,
    // ── Titles & Graphics tab state ──
    /// Show reusable project compositions instead of title presets.
    pub show_project_graphics: bool,
    /// Selected sub-category in the Titles sidebar. `None` = All.
    pub titles_category: Option<&'static str>,
    /// Preview background swatch shown behind every title card.
    pub titles_preview_bg: titles_tab::TitlesPreviewBg,
    /// Live preview text — typed into the small input next to the
    /// preview swatches. Empty falls back to each template's hand-
    /// crafted preview_text.
    pub titles_preview_text: String,
    /// Built-in vs user templates filter.
    pub titles_source_filter: titles_tab::TitlesSourceFilter,
    /// Animated-only filter chip.
    pub titles_animated_only: bool,
    /// Grid (default) vs list view.
    pub titles_view_mode: effects_tab::EffectViewMode,
    /// Sort key for the title cards.
    pub titles_sort: effects_tab::EffectSort,
    /// Selected card name — drives the accent border highlight.
    pub titles_selected: Option<String>,
    /// Title cards rendered by the real title engine.
    title_previews: title_previews::TitlePreviews,
    /// Transition cards rendered between two frames of the project.
    transition_previews: transition_previews::TransitionPreviews,
    /// Generator cards rendered by the generator renderer.
    generator_previews: generator_previews::GeneratorPreviews,
    // ── Transitions tab state ──
    pub transitions_category: Option<transitions_tab::TransitionsCategory>,
    /// Favourites or Recent, shown in place of a category.
    pub transitions_list: Option<library_card::LibraryList>,
    /// Starred transitions by `string_key`. Persisted via
    /// `prefs.effects_browser.favorite_transitions`, mirrored as
    /// `favorite_effects` is.
    pub favorite_transitions: Vec<String>,
    /// Favourites or Recent in the Titles tab, in place of a category.
    pub titles_list: Option<library_card::LibraryList>,
    /// Starred title templates by name, persisted as the transitions are.
    pub favorite_titles: Vec<String>,
    /// Favourites or Recent in the Generators tab.
    pub generators_list: Option<library_card::LibraryList>,
    /// Starred generators by name, persisted as the transitions are.
    pub favorite_generators: Vec<String>,
    pub transitions_view_mode: effects_tab::EffectViewMode,
    /// The transition cards play the selected cut's own frames instead of
    /// the samples (#547). The browser's alone: the project is untouched.
    pub transitions_cut_preview: bool,
    /// Why the selected cut cannot be previewed, while it is asked for.
    pub transitions_cut_note: Option<String>,
    /// How large the transition cards are, so how many fit across (#548).
    pub transitions_card_size: transitions_tab::CardSize,
    pub transitions_sort: effects_tab::EffectSort,
    pub transitions_selected: Option<String>,
    // ── Generators tab state ──
    pub generators_category: Option<&'static str>,
    pub generators_animated_only: bool,
    pub generators_view_mode: effects_tab::EffectViewMode,
    pub generators_sort: effects_tab::EffectSort,
    pub generators_selected: Option<String>,
    // Library tab category
    pub library_category: LibraryCategory,
    /// The Library's SHARED view (#614).
    pub shared_tab: shared_tab::SharedTabState,
    // Audio Library (royalty-free music / SFX search) state
    pub audio_search_query: String,
    /// false = music (Jamendo), true = sound effects (Freesound).
    pub audio_search_sfx: bool,
    pub audio_search_results: Vec<klipp_audio_search::Track>,
    pub audio_search_receiver: Option<std::sync::mpsc::Receiver<Result<Vec<klipp_audio_search::Track>, String>>>,
    pub audio_search_error: Option<String>,
    /// The query a search in flight was asked, for its "Searching…" line.
    pub audio_search_asked: String,
    /// The query the results on show answer, once a search has finished:
    /// an empty list then says nothing was found, not the panel's welcome.
    pub audio_search_answered: Option<String>,
    /// Downloaded preview bytes on their way from a worker thread to the player.
    pub audio_preview_receiver: Option<std::sync::mpsc::Receiver<Result<Vec<u8>, String>>>,
    /// A track being downloaded to add: `(path, title)` when it lands.
    pub audio_download_receiver: Option<std::sync::mpsc::Receiver<Result<(std::path::PathBuf, String), String>>>,
    /// The provider id of the result currently previewing, so its row shows Pause/Stop.
    pub audio_preview_id: Option<String>,
    /// Where the in-flight Audio Library download should land once it arrives.
    pub audio_add_drop: klipp_core::events::ClipDrop,
    /// The provider id of the result being downloaded to add, so its row
    /// shows the download and the others don't.
    pub audio_add_id: Option<String>,
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
    /// The row a keyboard move last went to (#3019): Shift+arrow extends
    /// the selection from the anchor to it, and moves it on.
    pub files_selection_cursor: Option<std::path::PathBuf>,
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
    /// Group files by modification date. When on, sort is forced to
    /// DateModified descending and section headers (Today / Yesterday /
    /// This Week / This Month / Older) are rendered between rows.
    pub group_by_date: bool,
    /// Which columns are visible (besides Name which is always shown).
    pub visible_columns: Vec<ColumnId>,
    /// Files-tab quick type filter (All / Video / Audio / Images).
    pub files_type_filter: FileTypeFilter,
    /// User-pinned favorite folders, rendered after the XDG bookmarks bar.
    /// Persisted via `EditingPrefs::pinned_folders`.
    pub pinned_folders: Vec<std::path::PathBuf>,
    /// User-starred effects in the Library browser. Keyed by
    /// `EffectEntry.name`. Persisted via
    /// `prefs.effects_browser.favorite_effects` — mirrored into the
    /// browser at render-time and diffed back at the end of
    /// `render_browser_panel` (same pattern as `pinned_folders`).
    pub favorite_effects: Vec<String>,
    /// Recently-applied effects, MRU order (most recent first). Session-only
    /// — not persisted to prefs. Trimmed to a small cap so the Recent
    /// pseudo-category in the Library sidebar stays digestible.
    pub recent_effects: Vec<String>,
    /// Effects grid layout: thumbnail grid (default) or dense list.
    pub effect_view_mode: effects_tab::EffectViewMode,
    /// Effects grid sort key — drives the result-strip sort dropdown.
    pub effect_sort: effects_tab::EffectSort,
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
    /// Cached filmstrip tiles (multiple frames per video row). Populated
    /// on first hover via a background thread that calls
    /// `generate_filmstrip_incremental`. Keyed by file path.
    file_filmstrip_textures: HashMap<std::path::PathBuf, Vec<egui::TextureHandle>>,
    /// Deferred-drop ring for texture handles this panel replaces or
    /// clears. Same contract as `AppState::deferred_texture_drops`
    /// but local to the browser so we don't have to plumb AppState
    /// through every BrowserPanel method. Rotated at the start of
    /// each browser render via `rotate_deferred_drops`.
    deferred_texture_drops: [Vec<egui::TextureHandle>; 3],
    deferred_texture_drops_slot: usize,
    file_filmstrip_pending: HashSet<std::path::PathBuf>,
    file_filmstrip_tx: crossbeam_channel::Sender<FileFilmstripResult>,
    file_filmstrip_rx: crossbeam_channel::Receiver<FileFilmstripResult>,
    /// Lightweight waveform peaks for audio files in the current directory.
    /// Loaded lazily on first viewport visibility, cached on disk via
    /// `klipp_media::disk_cache::write_audio_peaks_cache` so subsequent
    /// visits are instant.
    file_audio_peaks: HashMap<std::path::PathBuf, Vec<(f32, f32)>>,
    file_audio_pending: HashSet<std::path::PathBuf>,
    file_audio_failed: HashSet<std::path::PathBuf>,
    file_audio_tx: crossbeam_channel::Sender<FileAudioPeaksResult>,
    file_audio_rx: crossbeam_channel::Receiver<FileAudioPeaksResult>,
    /// Path of the audio file currently being hover-skimmed in the file
    /// browser, or `None` if no audio row is being hovered. Used to
    /// distinguish "first hover of this row" (emit Start) from "still
    /// hovering same row" (emit Update) from "left the row" (emit End).
    /// Single slot — only one audio file can be skimmed at a time.
    file_audio_skim_active: Option<std::path::PathBuf>,
    /// Per-file star ratings cache. Loaded from
    /// `~/.cache/klipp/file-ratings.json` at startup, written on change.
    /// Keyed by absolute path, value 1-5. Entries with rating 0 are
    /// removed (no need to persist "unrated"). Modified flag tracks
    /// whether the cache has been changed since last save.
    pub file_ratings: HashMap<std::path::PathBuf, u8>,
    pub file_ratings_dirty: bool,
    /// Filesystem watcher for the currently-displayed directory. Receives
    /// a notification whenever files are created, deleted, or renamed
    /// inside that directory. The watcher is recreated on each navigation
    /// (dropped + re-created for the new directory) and set to `None`
    /// when the virtual Recents view is active or when auto_refresh is
    /// disabled in preferences.
    ///
    /// We keep the `RecommendedWatcher` handle alive (dropping it stops
    /// watching) and drain events from `folder_watch_rx` at the top of
    /// each `show_files_tab` frame.
    folder_watcher: Option<notify::RecommendedWatcher>,
    folder_watch_rx: Option<crossbeam_channel::Receiver<()>>,
    /// Inline "create new folder" state. `Some(text)` while a temporary
    /// row is showing at the top of the file list with a TextEdit for
    /// the user to type the folder name. `None` when not creating.
    creating_folder: Option<String>,
    /// Inline rename state. `Some((original_path, current_edit_text))`
    /// while a file/folder row is in edit mode. `None` when idle.
    /// The row at `original_path` renders a TextEdit in place of the
    /// normal filename label. Enter commits (via `rename_file_at`),
    /// Escape cancels. Same navigation guard as `creating_folder` —
    /// moving to another directory while renaming is blocked.
    renaming_file: Option<(std::path::PathBuf, String)>,
    /// One-shot flag set by `navigate_files_to` when it had to refuse
    /// a navigation request because an inline editor (create or rename)
    /// was open. Read and reset by the show() deferred block, which
    /// then toasts a message so the click doesn't feel inert.
    navigation_blocked_by_create: bool,
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
    /// User-assigned star rating (0 = unrated, 1-5 = rated). Loaded from
    /// the global ratings cache (`~/.cache/klipp/file-ratings.json`) and
    /// written back on change.
    pub rating: u8,
}

impl Default for BrowserPanel {
    fn default() -> Self {
        let (file_thumb_tx, file_thumb_rx) = crossbeam_channel::unbounded();
        let (file_skim_tx, file_skim_rx) = crossbeam_channel::unbounded();
        let (file_filmstrip_tx, file_filmstrip_rx) = crossbeam_channel::unbounded();
        let (file_audio_tx, file_audio_rx) = crossbeam_channel::unbounded();
        Self {
            search_query: String::new(),
            files_search_query: String::new(),
            media_sort_by: media_tab::SortBy::Name,
            list_columns: klipp_media::Field::DEFAULT_COLUMNS.to_vec(),
            media_sort_ascending: true,
            media_selected_bin: None,
            smart_bin_filter: SmartBinFilter::None,
            smart_bin_editor: None,
            pending_cmds: Vec::new(),
            min_rating_filter: None,
            color_label_filter: 0,
            metadata_filter: MetadataFilter::Any,
            browser_view_mode: media_tab::BrowserViewMode::default(),
            renaming_bin: None,
            effects_category: effects_tab::EffectCategory::All,
            audio_fx_category: effects_tab::EffectCategory::AudioAll,
            audio_fx_source_filter: audio_fx_tab::AudioSourceFilter::All,
            audio_fx_sort: effects_tab::EffectSort::NameAsc,
            titles_category: None,
            show_project_graphics: false,
            titles_preview_bg: titles_tab::TitlesPreviewBg::default(),
            titles_preview_text: String::new(),
            titles_source_filter: titles_tab::TitlesSourceFilter::All,
            titles_animated_only: false,
            titles_view_mode: effects_tab::EffectViewMode::Grid,
            titles_sort: effects_tab::EffectSort::NameAsc,
            titles_selected: None,
            title_previews: title_previews::TitlePreviews::default(),
            transition_previews: transition_previews::TransitionPreviews::default(),
            generator_previews: generator_previews::GeneratorPreviews::default(),
            transitions_category: None,
            transitions_list: None,
            transitions_card_size: Default::default(),
            favorite_transitions: Vec::new(),
            titles_list: None,
            favorite_titles: Vec::new(),
            generators_list: None,
            favorite_generators: Vec::new(),
            transitions_view_mode: effects_tab::EffectViewMode::Grid,
            transitions_cut_preview: false,
            transitions_cut_note: None,
            transitions_sort: effects_tab::EffectSort::NameAsc,
            transitions_selected: None,
            generators_category: None,
            generators_animated_only: false,
            generators_view_mode: effects_tab::EffectViewMode::Grid,
            generators_sort: effects_tab::EffectSort::NameAsc,
            generators_selected: None,
            audio_fx_audition_playing: false,
            audio_fx_audition_source: None,
            audio_fx_audition_volume: 1.0,
            audio_fx_audition_anchor_secs: 0.0,
            audio_fx_audition_anchor_at: None,
            audio_fx_selected: None,
            audio_fx_auditioning: None,
            audio_fx_hide_unsupported: false,
            library_category: LibraryCategory::default(),
            shared_tab: Default::default(),
            audio_search_query: String::new(),
            audio_search_sfx: false,
            audio_search_results: Vec::new(),
            audio_search_receiver: None,
            audio_search_error: None,
            audio_preview_receiver: None,
            audio_download_receiver: None,
            audio_preview_id: None,
            audio_search_asked: String::new(),
            audio_search_answered: None,
            audio_add_id: None,
            audio_add_drop: klipp_core::events::ClipDrop::PoolOnly,
            scroll_to_media_id: None,
            files_current_path: dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")),
            files_entries: Vec::new(),
            files_entries_dirty: true,
            files_loading: false,
            files_error: None,
            files_receiver: None,
            files_selected: HashSet::new(),
            files_selection_anchor: None,
            files_selection_cursor: None,
            file_thumb_textures: HashMap::new(),
            file_thumb_pending: HashSet::new(),
            file_thumb_failed: HashSet::new(),
            file_thumb_inflight: Arc::new(AtomicUsize::new(0)),
            file_thumb_tx,
            file_thumb_rx,
            file_thumb_size: FileThumbSize::default(),
            files_sort_by: FileSortBy::default(),
            files_sort_ascending: true,
            group_by_date: false,
            visible_columns: vec![ColumnId::Size, ColumnId::Date],
            files_type_filter: FileTypeFilter::default(),
            pinned_folders: Vec::new(),
            favorite_effects: Vec::new(),
            recent_effects: Vec::new(),
            effect_view_mode: effects_tab::EffectViewMode::default(),
            effect_sort: effects_tab::EffectSort::default(),
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
            file_filmstrip_textures: HashMap::new(),
            deferred_texture_drops: [Vec::new(), Vec::new(), Vec::new()],
            deferred_texture_drops_slot: 0,
            file_filmstrip_pending: HashSet::new(),
            file_filmstrip_tx,
            file_filmstrip_rx,
            file_audio_peaks: HashMap::new(),
            file_audio_pending: HashSet::new(),
            file_audio_failed: HashSet::new(),
            file_audio_tx,
            file_audio_rx,
            file_audio_skim_active: None,
            file_ratings: load_file_ratings_cache(),
            file_ratings_dirty: false,
            folder_watcher: None,
            folder_watch_rx: None,
            creating_folder: None,
            renaming_file: None,
            navigation_blocked_by_create: false,
        }
    }
}

impl BrowserPanel {
    /// Drop every cached texture this panel owns. Call from
    /// `eframe::App::on_exit` while wgpu is still alive — the
    /// browser holds large thumbnail / filmstrip / deferred-drop
    /// caches that would otherwise drop AFTER wgpu teardown and
    /// race the device shutdown into a SIGSEGV.
    pub fn clear_textures_for_shutdown(&mut self) {
        self.title_previews.drain();
        self.transition_previews.drain();
        self.generator_previews.drain();
        self.file_thumb_textures.clear();
        self.file_filmstrip_textures.clear();
        for bucket in &mut self.deferred_texture_drops {
            bucket.clear();
        }
    }

    /// Stash a `TextureHandle` so its underlying wgpu::Texture is
    /// held across the current frame's submit+present cycle.
    /// Matching helper for `AppState::defer_drop_texture` — egui-wgpu
    /// cannot tolerate dropping handles whose IDs are still in the
    /// current paint list, so every `_texture = None` and every
    /// HashMap-replace on texture fields in this panel goes through
    /// here.
    fn defer_drop_texture(&mut self, h: egui::TextureHandle) {
        let slot = self.deferred_texture_drops_slot;
        self.deferred_texture_drops[slot].push(h);
    }

    /// Extend the deferred-drop bucket from an iterator of handles.
    fn defer_drop_textures(&mut self, it: impl IntoIterator<Item = egui::TextureHandle>) {
        let slot = self.deferred_texture_drops_slot;
        self.deferred_texture_drops[slot].extend(it);
    }

    /// Rotate the deferred-drop ring. Called at the top of `show` so
    /// handles stashed two frames ago are released here — by which
    /// point every submit that could have referenced them has
    /// completed on the GPU.
    fn rotate_deferred_drops(&mut self) {
        let next = (self.deferred_texture_drops_slot + 1) % 3;
        self.deferred_texture_drops[next].clear();
        self.deferred_texture_drops_slot = next;
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        active_tab: &mut BrowserTab,
        files_pinned: &mut bool,
        files_split_ratio: &mut f32,
    ) -> Vec<AppCommand> {
        self.rotate_deferred_drops();
        let mut cmds = Vec::new();

        // Clear files_panel_focused on state every frame. The Files tab's
        // show_files_tab re-sets it if the tab is active and a row was
        // clicked. This prevents the flag from staying stuck true when
        // the user switches away from the Files tab (e.g. to Media),
        // which would permanently block the transport's Spacebar handling.
        state.files_panel_focused = false;

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
                    egui::Stroke::new(1.0_f32, theme.border),
                );
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let tabs = [
                        (BrowserTab::Files, "FILES", Icon::Folder),
                        (BrowserTab::Media, "MEDIA", Icon::FilmStrip),
                        // Library: closest-fit Phosphor — `Sliders` reads as
                        // an FX/effects vibe. A bespoke "books-stack" glyph
                        // would be more on-the-nose; flagged for follow-up.
                        (BrowserTab::Library, "LIBRARY", Icon::Sliders),
                    ];
                    for (tab, label, icon) in &tabs {
                        let is_active = *active_tab == *tab;
                        let display_label = label.to_string();

                        // Pre-measure label so we know how wide the tab
                        // needs to be. Letting egui's horizontal layout
                        // handle positioning means the icon and text
                        // can't collide regardless of cursor edge cases.
                        let text_galley = ui.painter().layout_no_wrap(
                            display_label.clone(),
                            egui::FontId::proportional(crate::theme::type_scale::BODY),
                            theme.text_dim,
                        );
                        let icon_size = 12.0;
                        let icon_gap = 6.0;
                        let h_pad = 12.0;
                        let tab_w = text_galley.size().x + icon_size + icon_gap + h_pad * 2.0;
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

                        // Lay out icon + label horizontally inside the tab
                        // rect using a fresh child UI. Centred vertically;
                        // egui handles the gap so they can't overlap.
                        ui.allocate_new_ui(
                            egui::UiBuilder::new().max_rect(tab_rect),
                            |ui| {
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        ui.spacing_mut().item_spacing.x = icon_gap;
                                        ui.add_space(h_pad);
                                        ui.add(
                                            icon.image(icon_size)
                                                .tint(text_color)
                                                .max_size(egui::vec2(icon_size, icon_size)),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&display_label)
                                                    .size(crate::theme::type_scale::BODY)
                                                    .color(text_color),
                                            )
                                            .selectable(false),
                                        );
                                    },
                                );
                            },
                        );

                        if is_active {
                            // Underline accent stripe — full tab width
                            // minus a small inset, matching the mockup.
                            let stripe_inset = 8.0;
                            ui.painter().hline(
                                (tab_rect.min.x + stripe_inset)
                                    ..=(tab_rect.max.x - stripe_inset),
                                tab_rect.max.y - 1.0,
                                egui::Stroke::new(2.0_f32, theme.accent),
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

                    // Collapse, at the far end of the tab row. The
                    // keyboard and the divider can both do this, but
                    // neither is visible; this is the one that can be
                    // found without being told.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(6.0);
                        let (btn_rect, resp) =
                            ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(btn_rect.shrink(2.0), 4.0, theme.bg_hover);
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        let tint = if resp.hovered() { theme.text_primary } else { theme.text_dim };
                        let icon_rect = egui::Rect::from_center_size(btn_rect.center(), egui::vec2(12.0, 12.0));
                        ui.put(
                            icon_rect,
                            Icon::CaretLeft.image(12.0).tint(tint).max_size(egui::vec2(12.0, 12.0)),
                        );
                        let resp = resp.on_hover_text(crate::keybindings::tip(ui.ctx(), "Collapse browser", "toggle_browser_panel"));
                        if resp.clicked() {
                            cmds.push(AppCommand::ToggleBrowserPanel);
                        }
                    });
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
                LibraryCategory::Titles => "Search titles and graphics\u{2026}",
                LibraryCategory::Transitions => "Search transitions\u{2026}",
                LibraryCategory::Generators => "Search generators\u{2026}",
                LibraryCategory::Audio | LibraryCategory::Shared => "",
            },
        };

        // Compute the split-view flag up front so the toolbar/search row
        // below can avoid rendering when the search would end up spanning
        // both panes (confusing — the search only applies to the active
        // right-pane tab). In split mode we render a per-pane search bar
        // inside the right pane instead.
        let show_split = *files_pinned && *active_tab != BrowserTab::Files;

        // Styled search input + view mode toggle (for Media tab)
        // Nothing in this row belongs to the Library tab: Media
        // contributes the view-mode and sort buttons, Files the type
        // filter and sort, and the search field is skipped because each
        // Library sub-tab renders its own. What was left for Library
        // was an empty horizontal row that still reserved its height —
        // a band of blank panel between the FILES/MEDIA/LIBRARY tabs
        // and the VIDEO FX strip under them, on the tab the browser
        // opens on.
        if *active_tab != BrowserTab::Library {
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
                let sort_label = self.media_sort_by.label();
                egui::ComboBox::from_id_salt("browser_sort")
                    .selected_text(
                        egui::RichText::new(sort_label).size(10.0).color(theme.text_secondary),
                    )
                    .width(55.0)
                    .show_ui(ui, |ui| {
                        for f in klipp_media::Field::SORTABLE {
                            ui.selectable_value(&mut self.media_sort_by, f, f.label());
                        }
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
                        ui.selectable_value(&mut self.files_sort_by, FileSortBy::Rating, "Rating");
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

                // Re-sort the existing entries in place if sort changed.
                if self.files_sort_by != prev_sort || self.files_sort_ascending != prev_asc {
                    sort_file_entries(
                        &mut self.files_entries,
                        self.files_sort_by,
                        self.files_sort_ascending,
                    );
                }

                ui.add_space(4.0);
            }

            // In split view the search bar is rendered inside the right
            // pane so it visually sits above the content it searches. At
            // full-panel widths the top-level bar is still the right
            // place for it.
            //
            // The bar binds to the tab-specific query string: Files uses
            // `files_search_query`, every other tab uses `search_query`.
            // This keeps effects queries from leaking into the Files
            // filter in split view.
            //
            // Library tab owns its own search bar (rendered inside each
            // category sub-tab) so the panel-top placement collapses to
            // nothing — otherwise the Audio FX tab shows two competing
            // search inputs.
            let library_owns_search = *active_tab == BrowserTab::Library;
            if !show_split && !library_owns_search {
                let query_buf: &mut String = if *active_tab == BrowserTab::Files {
                    &mut self.files_search_query
                } else {
                    &mut self.search_query
                };
                browser_search_field(ui, theme, query_buf, search_placeholder, state);
            }
        });
        ui.add_space(4.0);
        }

        // ── Tab content ──
        // `show_split` was computed earlier (before the top-level toolbar
        // row) so the search bar could decide whether to render at top
        // level or defer into the right pane.

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

                // Per-pane search bar (split view only). Visually anchors
                // the search to the content it filters, rather than
                // spanning both panes at the top of the browser where
                // users mistake it for a global search.
                //
                // Library skips this — its sub-tabs render their own
                // search bar so we don't stack two of them.
                let library_owns_search = *active_tab == BrowserTab::Library;
                ui.add_space(2.0);
                if !library_owns_search {
                    browser_search_field(ui, theme, &mut self.search_query, search_placeholder, state);
                }
                ui.add_space(4.0);

                match active_tab {
                    BrowserTab::Media => {
                        // Smart bin + rating + colour-label filters.
                        // AND-combined across categories, OR-combined
                        // within the colour-label bitmask.
                        // Inset like the pool's items below them.
                        egui::Frame::none().inner_margin(egui::Margin::symmetric(8.0, 0.0)).show(ui, |ui| {
                            self.render_media_filter_toolbar(ui, theme, state);
                            media_pool_buttons(ui, &mut cmds);
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
                            &mut self.list_columns,
                            &mut self.renaming_bin,
                            &mut self.scroll_to_media_id,
                            &self.smart_bin_filter,
                            self.min_rating_filter,
                            self.color_label_filter,
                            &self.metadata_filter,
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
                    // Render the filter toolbar above the media list in the
                    // non-split path too — otherwise the filters are only
                    // visible when Files is pinned, which is poor
                    // discoverability.
                    // Inset like the pool's items below them.
                    egui::Frame::none().inner_margin(egui::Margin::symmetric(8.0, 0.0)).show(ui, |ui| {
                        self.render_media_filter_toolbar(ui, theme, state);
                        media_pool_buttons(ui, &mut cmds);
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
                        &mut self.list_columns,
                        &mut self.renaming_bin,
                        &mut self.scroll_to_media_id,
                        &self.smart_bin_filter,
                        self.min_rating_filter,
                        self.color_label_filter,
                        &self.metadata_filter,
                    );
                    cmds.extend(tab_cmds);
                }
                BrowserTab::Library => {
                    let tab_cmds = self.show_library_tab(ui, state, theme);
                    cmds.extend(tab_cmds);
                }
            }
        }

        self.show_smart_bin_editor(ui.ctx(), theme);
        cmds.append(&mut self.pending_cmds);
        cmds
    }

    /// Navigate the files browser to a new directory. Clears in-memory thumbnail
    /// state so RAM stays bounded. Disk cache is preserved.
    ///
    /// Refuses to navigate while an inline "create new folder" editor is
    /// open: the user is in the middle of naming a folder anchored to a
    /// specific parent directory, and silently swapping that parent out
    /// from under them would be confusing. They must commit (Enter) or
    /// cancel (Escape) before leaving the directory. Sets
    /// `navigation_blocked_by_create` so the show() deferred block can
    /// show a toast — without that signal, the click would feel inert.
    fn navigate_files_to(&mut self, path: std::path::PathBuf) {
        if self.creating_folder.is_some() || self.renaming_file.is_some() {
            self.navigation_blocked_by_create = true;
            return;
        }
        self.files_current_path = path.clone();
        self.files_entries_dirty = true;
        // Route cleared TextureHandles through the deferred-drop
        // ring so in-flight submits can't hit destroyed textures.
        let drained_thumbs: Vec<_> = self.file_thumb_textures.drain().map(|(_, h)| h).collect();
        self.defer_drop_textures(drained_thumbs);
        self.file_thumb_pending.clear();
        self.file_thumb_failed.clear();
        self.file_audio_peaks.clear();
        self.file_audio_pending.clear();
        self.file_audio_failed.clear();
        let drained_strips: Vec<_> = self
            .file_filmstrip_textures
            .drain()
            .flat_map(|(_, v)| v.into_iter())
            .collect();
        self.defer_drop_textures(drained_strips);
        self.file_filmstrip_pending.clear();
        // Selection doesn't carry across folders.
        self.files_selected.clear();
        self.files_selection_anchor = None;
        // Note: in-flight workers will still complete and send results;
        // drain_file_thumbs filters out paths no longer in the current entries.

        // Recreate the folder watcher for the new directory. The old
        // watcher (if any) is dropped here, which stops watching the
        // previous directory. If auto_refresh is off, we skip to save
        // inotify watches. The `state` field isn't available here (we
        // only have `&mut self`), so the auto_refresh pref check
        // happens in `show_files_tab` where we drain the channel.
        let (w, rx) = start_folder_watcher(&path);
        self.folder_watcher = w;
        self.folder_watch_rx = rx;
    }

    /// Switch to the virtual "Recent files" view. Populates `files_entries`
    /// from the prefs `recent_files` list, filtering out paths that no longer
    /// exist on disk. This view does NOT read a real directory — it sets
    /// `files_current_path` to the sentinel so the breadcrumb/path bar
    /// renders "Recent Files" instead of a filesystem path, and skips the
    /// async directory-read pass.
    fn navigate_to_recents(&mut self, state: &crate::state::AppState) {
        if self.creating_folder.is_some() || self.renaming_file.is_some() {
            self.navigation_blocked_by_create = true;
            return;
        }
        self.files_current_path = RECENTS_SENTINEL.clone();
        self.files_entries = state
            .recent_files_snapshot
            .iter()
            .filter(|p| p.exists())
            .map(|p| {
                let meta = std::fs::metadata(p).ok();
                let ext = p
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                FileEntry {
                    name: p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    rating: self.file_ratings.get(p).copied().unwrap_or(0),
                    path: p.clone(),
                    is_dir: false,
                    size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    is_media: klipp_core::media_extensions::is_media(&ext),
                    mtime: meta.and_then(|m| m.modified().ok()),
                }
            })
            .collect();
        self.files_entries_dirty = false;
        self.files_loading = false;
        self.files_error = None;
        self.files_receiver = None;
        // Route cleared TextureHandles through the deferred-drop
        // ring so in-flight submits can't hit destroyed textures.
        let drained_thumbs: Vec<_> = self.file_thumb_textures.drain().map(|(_, h)| h).collect();
        self.defer_drop_textures(drained_thumbs);
        self.file_thumb_pending.clear();
        self.file_thumb_failed.clear();
        self.file_audio_peaks.clear();
        self.file_audio_pending.clear();
        self.file_audio_failed.clear();
        let drained_strips: Vec<_> = self
            .file_filmstrip_textures
            .drain()
            .flat_map(|(_, v)| v.into_iter())
            .collect();
        self.defer_drop_textures(drained_strips);
        self.file_filmstrip_pending.clear();
        self.files_selected.clear();
        self.files_selection_anchor = None;
        // No directory to watch in the virtual view.
        self.folder_watcher = None;
        self.folder_watch_rx = None;
    }

    /// Re-read the current directory and drop in-memory thumbnail caches so
    /// any externally-modified files are picked up. Selection IS preserved
    /// (paths that no longer exist will simply disappear from the new
    /// `files_entries` list). The disk thumbnail cache stays intact — its
    /// keys (path+size+mtime) auto-invalidate per file.
    fn refresh_files(&mut self) {
        self.files_entries_dirty = true;
        // Route cleared TextureHandles through the deferred-drop
        // ring so in-flight submits can't hit destroyed textures.
        let drained_thumbs: Vec<_> = self.file_thumb_textures.drain().map(|(_, h)| h).collect();
        self.defer_drop_textures(drained_thumbs);
        self.file_thumb_pending.clear();
        self.file_thumb_failed.clear();
        self.file_audio_peaks.clear();
        self.file_audio_pending.clear();
        self.file_audio_failed.clear();
        let drained_strips: Vec<_> = self
            .file_filmstrip_textures
            .drain()
            .flat_map(|(_, v)| v.into_iter())
            .collect();
        self.defer_drop_textures(drained_strips);
        self.file_filmstrip_pending.clear();
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
            let tex = crate::app::alloc_texture_logged(ctx, &name, image, egui::TextureOptions::LINEAR);
            if let Some(old) = self.file_thumb_textures.insert(result.path, tex) {
                self.defer_drop_texture(old);
            }
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
                let tex = crate::app::alloc_texture_logged(
                    ctx,
                    "file_skim_frame",
                    image,
                    egui::TextureOptions::LINEAR,
                );
                if let Some((_, old)) = self.file_skim_texture.take() {
                    self.defer_drop_texture(old);
                }
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

    /// Drain filmstrip generation results and upload textures to egui.
    fn drain_file_filmstrips(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.file_filmstrip_rx.try_recv() {
            self.file_filmstrip_pending.remove(&result.path);
            if !result.frames.is_empty()
                && self.files_entries.iter().any(|e| e.path == result.path)
            {
                let textures: Vec<egui::TextureHandle> = result
                    .frames
                    .into_iter()
                    .enumerate()
                    .map(|(i, (rgba, w, h))| {
                        let pixels: Vec<egui::Color32> = rgba
                            .chunks_exact(4)
                            .map(|c| {
                                egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
                            })
                            .collect();
                        let image = egui::ColorImage {
                            size: [w as usize, h as usize],
                            pixels,
                        };
                        {
                            let name = format!("filmstrip_{}_{}", result.path.display(), i);
                            crate::app::alloc_texture_logged(ctx, &name, image, egui::TextureOptions::LINEAR)
                        }
                    })
                    .collect();
                if let Some(old_vec) = self.file_filmstrip_textures.insert(result.path, textures) {
                    self.defer_drop_textures(old_vec);
                }
            }
        }
    }

    /// Spawn a background thread to generate a filmstrip for the given
    /// video file. The thread calls `generate_filmstrip_incremental` with
    /// 8 frames at the given target height, then sends the result via the
    /// filmstrip channel.
    fn spawn_file_filmstrip(
        &mut self,
        path: std::path::PathBuf,
        duration: f64,
        target_height: u32,
        ctx: &egui::Context,
    ) {
        if self.file_filmstrip_pending.contains(&path) {
            return; // already in-flight
        }
        self.file_filmstrip_pending.insert(path.clone());
        let tx = self.file_filmstrip_tx.clone();
        let ctx = ctx.clone();
        let n = FILE_FILMSTRIP_FRAME_COUNT;
        std::thread::Builder::new()
            .name("file-filmstrip-worker".into())
            .spawn(move || {
                let interval = duration / n as f64;
                let mut frames: Vec<(Vec<u8>, u32, u32)> = Vec::with_capacity(n);
                // A hole or two in a browser preview is cosmetic; a
                // preview with nothing in it is worth a line in the log.
                let outcome = klipp_engine::generate_filmstrip_incremental(
                    &path,
                    duration,
                    interval,
                    target_height,
                    |_time, rgba, w, h| {
                        frames.push((rgba, w, h));
                    },
                    None, // small browser preview — not worth scheduler overhead
                );
                match &outcome {
                    Err(e) => tracing::warn!("File filmstrip failed for {:?}: {e}", path),
                    Ok(o) if o.tiles == 0 && !o.skipped.is_empty() => {
                        tracing::warn!("File filmstrip made no tiles for {:?}: {}", path, o.skipped[0].1)
                    }
                    Ok(_) => {}
                }
                let _ = tx.send(FileFilmstripResult {
                    path,
                    frames,
                });
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
                let cache_key = klipp_engine::video_decoder::picture_cache_key(&path);
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
        // Right-click Recents button → "Clear Recent Files". Declared
        // early because the Recents button renders before the deferred-
        // action vars block; the closure must capture this by &mut.
        let mut pending_clear_recents = false;

        // Clear files_panel_focused if the user clicked anywhere outside
        // the file browser panel this frame. This is the standard "panel
        // defocus" pattern: clicking the timeline, viewer, or any other
        // panel returns Spacebar ownership to the transport play/pause.
        // Row clicks inside the browser re-set the flag during the row
        // loop below, so keyboard navigation continues to work as long
        // as the user is clicking within the browser.
        if ui.input(|i| i.pointer.primary_pressed()) {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if !ui.max_rect().contains(pos) {
                    self.files_panel_focused = false;
                }
            }
        }

        // Mirror files_panel_focused to AppState so the transport handler
        // (which runs in a different panel, before the browser) knows to
        // yield Spacebar to Quick Look instead of triggering play/pause.
        state.files_panel_focused = self.files_panel_focused;

        // Drain any completed thumbnail results into textures.
        self.drain_file_thumbs(ui.ctx());
        // Drain any completed skim-frame results into the live skim texture.
        self.drain_file_skims(ui.ctx());
        // Drain any completed audio peaks results into the in-memory cache.
        self.drain_file_audio_peaks();
        // Drain any completed filmstrip generation results.
        self.drain_file_filmstrips(ui.ctx());

        // ── Auto-refresh: drain folder watcher events ──
        // If the filesystem watcher detected changes in the current
        // directory (file created, deleted, or renamed externally),
        // trigger a refresh — but only if:
        //   - auto_refresh is enabled in preferences
        //   - no inline editor (create/rename) is open (would be
        //     yanked out from under the user)
        //   - not already loading (prevents duplicate reads)
        if state.file_browser_auto_refresh {
            if let Some(ref rx) = self.folder_watch_rx {
                let mut changed = false;
                while rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed
                    && self.creating_folder.is_none()
                    && self.renaming_file.is_none()
                    && !self.files_loading
                {
                    self.refresh_files();
                    ui.ctx().request_repaint();
                }
            }
        }

        // Sync our local audio-skim tracker with the engine. If something
        // outside the file browser tore down the session (e.g. modal open,
        // viewer mode switch), clear our local "active path" so the next
        // hover starts a fresh session instead of trying to update a dead
        // one.
        if self.file_audio_skim_active.is_some() && state.skim_audio_session.is_none() {
            self.file_audio_skim_active = None;
        }

        // Check for async directory read result
        if let Some(ref rx) = self.files_receiver {
            match rx.try_recv() {
                Ok(Ok(mut entries)) => {
                    // Populate ratings from the cache (background reader
                    // doesn't have access to it).
                    for e in &mut entries {
                        e.rating = self.file_ratings.get(&e.path).copied().unwrap_or(0);
                    }
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

        // Start async directory read if needed.
        // Skip when in the virtual "Recents" view — that view populates
        // files_entries directly from the prefs snapshot, not from disk.
        let is_recents_view = self.files_current_path == RECENTS_SENTINEL.as_path();
        if self.files_entries_dirty && !self.files_loading && !is_recents_view {
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
                ).fill(theme.bg_raised).rounding(2.0).min_size(egui::vec2(24.0, 20.0)),
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
                ).fill(theme.bg_raised).rounding(2.0).min_size(egui::vec2(24.0, 20.0)),
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
                ui.painter().rect(rect, 3.0, bg, egui::Stroke::new(0.5_f32, border));
                let gw = glyph_w(size);
                let gh = gw * 9.0 / 16.0;
                let glyph_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(gw, gh));
                let glyph_color = if is_active {
                    theme.text_on_accent // dark on amber
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

            // Group-by-date toggle — calendar icon, same visual pattern as
            // the S/M/L pickers. When active, forces DateModified descending
            // sort and renders section headers between file rows.
            ui.add_space(4.0);
            {
                let is_active = self.group_by_date;
                let (bg, border_color) = if is_active {
                    (theme.accent, theme.accent)
                } else {
                    (theme.bg_raised, theme.border)
                };
                let text_color = if is_active {
                    theme.text_on_accent // dark on amber
                } else {
                    theme.text_dim
                };
                let group_resp = ui.add(
                    egui::Button::new(
                        egui::RichText::new("G").size(11.0).strong().color(text_color),
                    )
                    .fill(bg)
                    .stroke(egui::Stroke::new(0.5_f32, border_color))
                    .rounding(2.0)
                    .min_size(egui::vec2(22.0, 20.0)),
                )
                .on_hover_text(if is_active {
                    "Grouped by date \u{2014} click to show flat list"
                } else {
                    "Group by date"
                });
                if group_resp.clicked() {
                    self.group_by_date = !self.group_by_date;
                    if self.group_by_date {
                        self.files_sort_by = FileSortBy::DateModified;
                        self.files_sort_ascending = false;
                        sort_file_entries(
                            &mut self.files_entries,
                            self.files_sort_by,
                            self.files_sort_ascending,
                        );
                    }
                }
            }

            // Pin button — push to right side of toolbar
            if let Some((ref pinned, _)) = files_pinned {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let is_pinned = **pinned;
                    let pin_color = if is_pinned {
                        theme.text_on_accent // dark icon on amber fill
                    } else {
                        theme.text_dim
                    };
                    let pin_resp = ui.add(
                        egui::Button::image(Icon::PushPin.image(12.0).tint(pin_color))
                            .min_size(egui::vec2(22.0, 20.0))
                            .fill(if is_pinned { theme.accent } else { theme.bg_raised })
                            .rounding(2.0),
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

            // Current path display — show "Recent Files" for the virtual
            // recents view, otherwise the real filesystem path.
            let path_display = if self.files_current_path == RECENTS_SENTINEL.as_path() {
                "\u{1F552} Recent Files".to_string()
            } else {
                let path_str = self.files_current_path.to_string_lossy();
                if path_str.len() > 35 {
                    format!("...{}", &path_str[path_str.len()-32..])
                } else {
                    path_str.to_string()
                }
            };
            ui.label(
                egui::RichText::new(&path_display)
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
                            egui::RichText::new(*label).size(10.0)
                                .color(if is_current { theme.text_on_accent } else { theme.text_secondary })
                        )
                        .fill(if is_current { theme.accent } else { egui::Color32::TRANSPARENT })
                        .stroke(egui::Stroke::new(0.5_f32, if is_current { theme.accent } else { theme.border }))
                        .rounding(2.0),
                    )
                    .on_hover_text(path.to_string_lossy().to_string());
                    if btn.clicked() {
                        self.navigate_files_to(path.clone());
                    }
                }
            }

            // ── Recents virtual folder ──
            // Gated on `state.show_recent_files` (prefs toggle in the
            // File Browser tab). Files are still tracked even when off,
            // so turning it back on recovers the list.
            if state.show_recent_files {
                let is_current = self.files_current_path == RECENTS_SENTINEL.as_path();
                let btn = ui.add(
                    egui::Button::new(
                        egui::RichText::new("\u{1F552} Recents").size(10.0)
                            .color(if is_current { theme.text_on_accent } else { theme.text_secondary })
                    )
                    .fill(if is_current { theme.accent } else { egui::Color32::TRANSPARENT })
                    .stroke(egui::Stroke::new(0.5_f32, if is_current { theme.accent } else { theme.border }))
                    .rounding(2.0),
                )
                .on_hover_text("Recently imported files");
                if btn.clicked() {
                    self.navigate_to_recents(state);
                }
                btn.context_menu_rows(|ui| menu_body(ui, |ui| {
                    if menu_item(ui, "Clear Recent Files", "").clicked() {
                        state.recent_files_snapshot.clear();
                        self.files_entries.clear();
                        pending_clear_recents = true;
                        ui.close_menu();
                    }
                }));
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
                        egui::RichText::new(format!("\u{2605} {}", display_name)).size(10.0)
                            .color(if is_current { theme.text_on_accent } else { theme.text_secondary })
                    )
                    .fill(if is_current { theme.accent } else { egui::Color32::TRANSPARENT })
                    .stroke(egui::Stroke::new(0.5_f32, if is_current { theme.accent } else { theme.border }))
                    .rounding(2.0),
                )
                .on_hover_text(path.to_string_lossy().to_string());
                if btn.clicked() {
                    self.navigate_files_to(path.clone());
                }
                btn.context_menu_rows(|ui| menu_body(ui, |ui| {
                    if menu_item(ui, "Remove from Favorites", "").clicked() {
                        pending_unpin = Some(path.clone());
                        ui.close_menu();
                    }
                }));
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
                        egui::RichText::new(label).size(10.0)
                            .color(if is_current { theme.text_on_accent } else { theme.text_secondary })
                    )
                    .fill(if is_current { theme.accent } else { egui::Color32::TRANSPARENT })
                    .stroke(egui::Stroke::new(0.5_f32, if is_current { theme.accent } else { theme.border }))
                    .rounding(2.0),
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
            egui::Stroke::new(0.5_f32, theme.border),
        );
        ui.add_space(2.0);

        // ── File listing ──
        // Uses `files_search_query` (Files-only) rather than the shared
        // `search_query` so text typed into the effects/media search bar
        // in the adjacent right pane doesn't filter the file list here.
        let search = self.files_search_query.to_lowercase();
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
        let mut filmstrip_spawn_requests: Vec<(std::path::PathBuf, f64, u32)> = Vec::new();
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
        // Right-click → "New folder" — set after the loop to seed the
        // inline TextEdit with a unique default name. Captured here so
        // the deferred mutation block (which already has `&mut self`)
        // can call `unique_untitled_folder_name` against the current
        // directory without fighting the borrow checker.
        let mut pending_new_folder = false;
        // Inline rename committed → name to actually create on disk.
        // Captured during the inline-create row's `lost_focus()` branch
        // and resolved in the deferred mutation block, where we run the
        // actual `create_folder_in` call, refresh the directory listing,
        // select the new folder, and show a toast.
        let mut pending_commit_folder: Option<String> = None;
        // Right-click → "Rename" or F2 → (path_to_rename).
        // Buffer handles that need deferred-drop on the immutable-self
        // paths (e.g. keyboard shortcut handling while `filtered` is borrowed).
        // Flushed through `defer_drop_textures` after the immutable borrow ends.
        let mut pending_defer_drops: Vec<egui::TextureHandle> = Vec::new();
        let mut pending_start_rename: Option<std::path::PathBuf> = None;
        // Inline rename committed → (old_path, new_name).
        let mut pending_commit_rename: Option<(std::path::PathBuf, String)> = None;
        // Set rating on file(s) → (paths, rating 0-5).
        let mut pending_set_rating: Option<(Vec<std::path::PathBuf>, u8)> = None;
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
        // Hover audio skim — captured during the row loop, applied after.
        // (path, time_secs) — `path` doubles as identity to detect a row
        // change between frames. `any_audio_row_hovered` mirrors the video
        // flag so we can emit `BrowserSkimAudioEnd` exactly once when the
        // cursor leaves all audio rows.
        let mut pending_audio_skim: Option<(std::path::PathBuf, f64)> = None;
        let mut any_audio_row_hovered = false;
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
            let (k_down, k_up, k_home, k_end, k_pgdn, k_pgup, k_enter, k_back, k_f2, k_space) = ui.input(|i| (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::Home),
                i.key_pressed(egui::Key::End),
                i.key_pressed(egui::Key::PageDown),
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Backspace),
                i.key_pressed(egui::Key::F2),
                i.key_pressed(egui::Key::Space),
            ));
            // Find current anchor's index in `filtered`. If the anchor isn't
            // in the visible list (e.g. user just enabled a filter that hides
            // it), fall back to None so the next move starts from the top.
            let cur_idx = self.files_selection_anchor.as_ref()
                .and_then(|a| filtered.iter().position(|e| &e.path == a));
            // Shift moves from where the last keyboard move went, the
            // anchor kept (#3019); Ctrl+A takes every file shown.
            let (extend, select_all) = ui.input(|i| (i.modifiers.shift, i.modifiers.command && i.key_pressed(egui::Key::A)));
            let from_idx = self.files_selection_cursor.as_ref()
                .filter(|_| extend)
                .and_then(|c| filtered.iter().position(|e| &e.path == c))
                .or(cur_idx);

            let last = filtered.len() - 1;
            let page = 10usize;
            let mut new_idx: Option<usize> = None;

            if k_down {
                new_idx = Some(match from_idx { Some(i) if i < last => i + 1, Some(i) if extend => i, _ => 0 });
            } else if k_up {
                new_idx = Some(match from_idx { Some(i) if i > 0 => i - 1, Some(_) => 0, None => last });
            } else if k_home {
                new_idx = Some(0);
            } else if k_end {
                new_idx = Some(last);
            } else if k_pgdn {
                new_idx = Some(match from_idx { Some(i) => (i + page).min(last), None => 0 });
            } else if k_pgup {
                new_idx = Some(match from_idx { Some(i) => i.saturating_sub(page), None => 0 });
            }

            if let Some(idx) = new_idx {
                let path = filtered[idx].path.clone();
                let (lo, hi) = keyboard_selection(cur_idx, idx, extend);
                self.files_selected = filtered[lo..=hi].iter().map(|e| e.path.clone()).collect();
                if lo == hi || cur_idx.is_none() {
                    self.files_selection_anchor = Some(path.clone());
                }
                self.files_selection_cursor = Some(path);
                scroll_to_index = Some(idx);
            } else if select_all {
                self.files_selected = filtered.iter().filter(|e| !e.is_dir).map(|e| e.path.clone()).collect();
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

            // F2: rename the selected file/folder (single selection only).
            // Disabled in the virtual Recents view.
            if k_f2 && self.files_selected.len() == 1 && self.renaming_file.is_none() && !is_recents_view {
                let path = self.files_selected.iter().next().unwrap().clone();
                pending_start_rename = Some(path);
            }

            // 0-5: quick-rate the selected file.
            for (key, rating) in [
                (egui::Key::Num0, 0u8),
                (egui::Key::Num1, 1),
                (egui::Key::Num2, 2),
                (egui::Key::Num3, 3),
                (egui::Key::Num4, 4),
                (egui::Key::Num5, 5),
            ] {
                if ui.input(|i| i.key_pressed(key)) && !self.files_selected.is_empty() {
                    let paths: Vec<_> = self.files_selected.iter().cloned().collect();
                    pending_set_rating = Some((paths, rating));
                    break;
                }
            }

            // Spacebar: Quick Look preview (toggle on/off).
            // Works on the selection anchor (the most recently clicked
            // item). Directories are skipped — nothing to preview.
            if k_space {
                if state.quick_look_path.is_some() {
                    // Already open → close.
                    if let Some(ref session) = state.quick_look_audio {
                        session.stop();
                    }
                    state.quick_look_path = None;
                    if let Some(tex) = state.quick_look_texture.take() {
                        pending_defer_drops.push(tex);
                    }
                    state.quick_look_info = None;
                    state.quick_look_audio = None;
                } else if let Some(ref anchor) = self.files_selection_anchor {
                    let is_dir = filtered.iter()
                        .find(|e| &e.path == anchor)
                        .map(|e| e.is_dir)
                        .unwrap_or(true);
                    if !is_dir {
                        state.quick_look_path = Some(anchor.clone());
                        // Texture + info are loaded lazily in the
                        // Quick Look render pass in app.rs.
                        if let Some(tex) = state.quick_look_texture.take() {
                            pending_defer_drops.push(tex);
                        }
                        state.quick_look_info = None;
                        state.quick_look_audio = None;
                    }
                }
            }
        }

        // Reserve vertical space for the status bar at the bottom of the panel.
        let status_h = 22.0_f32;
        let scroll_h = (ui.available_height() - status_h).max(0.0);

        // When the tree sidebar is active, wrap the file list in a child
        // UI offset to the right so tree and list sit side-by-side.
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
                    ui.label(egui::RichText::new("Cannot access folder").size(12.0).color(theme.marker_error));
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

            // ── Inline "creating new folder" row ──
            // Renders at the top of the list whenever `creating_folder` is
            // Some, before any real entries. The row contains a folder
            // icon and a TextEdit with the typed name. Enter / click-away
            // commit; Escape cancels. The actual filesystem mutation
            // happens in the deferred block below the for loop, where
            // `&mut self` is unambiguously available — that's also where
            // we wire selection of the new folder and the toast.
            //
            // While this row is showing, the empty-folder placeholder
            // below is suppressed so the user isn't confused by an
            // "Empty folder" hint sitting under their inline editor.
            //
            // Implementation note: this MUST use `ui.horizontal()` and
            // let egui auto-size the TextEdit's parent layout, not
            // `ui.new_child()` with a manually-positioned rect. The
            // child-ui-with-fixed-rect approach gives the TextEdit an
            // unstable parent ID that shifts between frames, which makes
            // focus state evaporate and prevents the Enter key from
            // ever reaching the `lost_focus()` check. The bin rename
            // pattern in `media_tab::render_bin_header` uses the same
            // `ui.horizontal()` shape and works correctly — copy it.
            if self.creating_folder.is_some() {
                // Pre-read Enter and Escape from this frame's input
                // BEFORE the TextEdit consumes them. This is a
                // belt-and-braces backup for `lost_focus()`: even if
                // egui's focus tracking ever goes sideways, we'll still
                // commit on Enter and cancel on Escape because we
                // captured the key event explicitly. Both events are
                // also seen by the TextEdit; consuming them via
                // ui.input is non-destructive.
                let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let escape_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));

                let edit_text = self.creating_folder.as_mut().unwrap();
                let text_resp = ui
                    .horizontal(|ui| {
                        ui.add_space(pad);
                        ui.label(
                            egui::RichText::new("\u{1F4C1}")
                                .size(13.0)
                                .color(theme.accent),
                        );
                        ui.add_space(8.0);
                        let resp = ui.add(
                            egui::TextEdit::singleline(edit_text)
                                .desired_width(ui.available_width() - 14.0)
                                .font(egui::FontId::proportional(11.0)),
                        );
                        // Take focus on every frame the editor is
                        // showing. `request_focus` is idempotent and
                        // egui handles the case where the widget is
                        // already focused. Same pattern as the bin
                        // rename in media_tab.
                        resp.request_focus();
                        resp
                    })
                    .inner;

                if escape_pressed {
                    // User cancelled — drop the in-progress name without
                    // touching the filesystem.
                    self.creating_folder = None;
                } else if enter_pressed || text_resp.lost_focus() {
                    // Enter or click-away → commit. The actual
                    // create-and-refresh happens in the deferred block
                    // below; we just stash the typed name in a local so
                    // it survives until then.
                    pending_commit_folder = Some(std::mem::take(edit_text));
                    self.creating_folder = None;
                }
            }

            if filtered.is_empty() && self.creating_folder.is_none() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(if search.is_empty() { "Empty folder" } else { "No matches" })
                            .size(12.0)
                            .color(theme.text_dim),
                    );
                });
            }

            // Pre-compute date group boundaries so we can render section
            // headers inline between file rows without modifying `filtered`.
            // The groups Vec is empty when grouping is off — zero overhead.
            let date_groups = if self.group_by_date && !is_recents_view {
                compute_date_groups(&filtered)
            } else {
                Vec::new()
            };
            let mut next_group_idx = 0;
            let group_header_h = 24.0_f32;

            for (row_idx, entry) in filtered.iter().enumerate() {
                // ── Date group header ──
                // If this row is the first entry in a new date group,
                // render a thin section header before the file row. The
                // header is pure paint — not selectable, not draggable,
                // invisible to keyboard navigation and index arithmetic.
                if next_group_idx < date_groups.len() && date_groups[next_group_idx].0 == row_idx {
                    let label = date_groups[next_group_idx].1.label();
                    next_group_idx += 1;

                    let (hdr_rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), group_header_h),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(hdr_rect, 0.0, theme.bg_deep);
                    ui.painter().text(
                        egui::pos2(hdr_rect.left() + pad, hdr_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::FontId::proportional(10.0),
                        theme.text_secondary,
                    );
                    // Subtle separator at the bottom of the header.
                    ui.painter().hline(
                        hdr_rect.min.x + pad..=hdr_rect.max.x - pad,
                        hdr_rect.max.y,
                        egui::Stroke::new(0.5_f32, theme.border),
                    );
                }

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
                    // ── Filmstrip strip (multi-frame) ──
                    // If a filmstrip is cached for this file, render its
                    // tiles across the thumb_box. The cursor X highlights
                    // the active tile. Falls back to single skim frame or
                    // poster thumbnail while the filmstrip is loading.
                    let cursor_in_box = ui.input(|i| i.pointer.hover_pos())
                        .filter(|p| thumb_box.contains(*p));
                    // Filmstrip shows while hovering, or always if
                    // filmstrip_persist is on. The large preview popup
                    // always requires hover regardless.
                    let filmstrip = if state.filmstrip_hover_enabled
                        && (cursor_in_box.is_some() || state.filmstrip_persist)
                    {
                        self.file_filmstrip_textures.get(&entry.path)
                    } else {
                        None
                    };

                    if let Some(tiles) = filmstrip.filter(|t| !t.is_empty()) {
                        let n = tiles.len() as f32;
                        let tile_w = thumb_box.width() / n;
                        let active_idx = cursor_in_box
                            .map(|p| ((p.x - thumb_box.min.x) / tile_w).floor() as usize)
                            .map(|i| i.min(tiles.len() - 1));

                        ui.painter().rect_filled(thumb_box, 2.0, theme.bg_void);
                        for (i, tex) in tiles.iter().enumerate() {
                            let tile_rect = egui::Rect::from_min_size(
                                egui::pos2(thumb_box.min.x + i as f32 * tile_w, thumb_box.min.y),
                                egui::vec2(tile_w, thumb_box.height()),
                            );
                            let tint = if active_idx == Some(i) {
                                egui::Color32::WHITE
                            } else {
                                egui::Color32::from_rgba_unmultiplied(180, 180, 180, 255)
                            };
                            ui.painter().image(
                                tex.id(),
                                tile_rect,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                tint,
                            );
                            // Thin separator between tiles.
                            if i > 0 {
                                ui.painter().vline(
                                    tile_rect.min.x,
                                    tile_rect.y_range(),
                                    egui::Stroke::new(0.5_f32, theme.bg_void),
                                );
                            }
                        }
                        // Active tile highlight border + large hover preview.
                        if let Some(idx) = active_idx {
                            let active_rect = egui::Rect::from_min_size(
                                egui::pos2(thumb_box.min.x + idx as f32 * tile_w, thumb_box.min.y),
                                egui::vec2(tile_w, thumb_box.height()),
                            );
                            ui.painter().rect_stroke(
                                active_rect,
                                0.0,
                                egui::Stroke::new(1.5_f32, theme.accent),
                            );

                            // Large preview popup — shows the active tile
                            // at a readable size above the row. Uses
                            // egui::Area at Tooltip order so it floats
                            // above everything without stealing focus.
                            let tex = &tiles[idx];
                            let tex_size = tex.size();
                            let preview_w = 320.0_f32;
                            let preview_h = preview_w * tex_size[1] as f32 / tex_size[0].max(1) as f32;
                            let preview_pos = egui::pos2(
                                active_rect.center().x - preview_w * 0.5,
                                rect.min.y - preview_h - 8.0, // above the row
                            );
                            // Clamp to screen bounds so the popup doesn't
                            // go off-screen at the top or sides.
                            let screen = ui.ctx().screen_rect();
                            let clamped_x = preview_pos.x
                                .max(screen.min.x + 4.0)
                                .min(screen.max.x - preview_w - 4.0);
                            let clamped_y = if preview_pos.y < screen.min.y + 4.0 {
                                // Not enough room above — show below.
                                rect.max.y + 8.0
                            } else {
                                preview_pos.y
                            };
                            egui::Area::new(egui::Id::new("filmstrip_preview_popup"))
                                .fixed_pos(egui::pos2(clamped_x, clamped_y))
                                .order(egui::Order::Tooltip)
                                .show(ui.ctx(), |ui| {
                                    let preview_rect = ui.allocate_exact_size(
                                        egui::vec2(preview_w, preview_h),
                                        egui::Sense::hover(),
                                    ).0;
                                    // Dark background with border.
                                    ui.painter().rect(
                                        preview_rect,
                                        4.0,
                                        theme.bg_void,
                                        egui::Stroke::new(1.0_f32, theme.accent),
                                    );
                                    ui.painter().image(
                                        tex.id(),
                                        preview_rect.shrink(2.0),
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        egui::Color32::WHITE,
                                    );
                                    // Frame number label.
                                    ui.painter().text(
                                        egui::pos2(preview_rect.right() - 6.0, preview_rect.bottom() - 6.0),
                                        egui::Align2::RIGHT_BOTTOM,
                                        format!("{}/{}", idx + 1, tiles.len()),
                                        egui::FontId::proportional(10.0),
                                        egui::Color32::from_rgba_unmultiplied(220, 220, 220, 200),
                                    );
                                });
                        }
                        painted_thumb = true;
                    } else {
                        // Fallback: single skim frame or poster thumbnail.
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
                            ui.painter().rect_filled(thumb_box, 2.0, theme.bg_void);
                            ui.painter().image(
                                tex.id(),
                                fitted,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                egui::Color32::WHITE,
                            );
                            painted_thumb = true;
                        } else if ui.is_rect_visible(rect) {
                            thumb_spawn_requests.push(entry.path.clone());
                        }
                    }

                    // ── Hover skimming ──
                    // Only for VIDEO files (audio + still images don't have a
                    // duration to skim through). Gated on the global toolbar
                    // toggle `state.skimming_enabled`.
                    let ext = entry.path.extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let is_video = VIDEO_EXTENSIONS.contains(&ext.as_str());
                    if is_video
                        && state.skimming_enabled
                        && state.file_browser_video_skim_enabled
                        && !state.has_modal_open()
                    {
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            if thumb_box.contains(pos) {
                                any_video_row_hovered = true;
                                if let Some(dur) = self.file_durations.get(&entry.path).copied() {
                                    // Request filmstrip generation if
                                    // enabled, not cached, not pending.
                                    if state.filmstrip_hover_enabled
                                        && !self.file_filmstrip_textures.contains_key(&entry.path)
                                        && !self.file_filmstrip_pending.contains(&entry.path)
                                    {
                                        filmstrip_spawn_requests.push((
                                            entry.path.clone(),
                                            dur,
                                            state.filmstrip_quality.target_height(),
                                        ));
                                    }
                                    let frac = ((pos.x - thumb_box.min.x) / thumb_box.width())
                                        .clamp(0.0, 1.0);
                                    let t = dur * frac as f64;
                                    pending_skim_request = Some((entry.path.clone(), t));
                                } else if pending_duration_probe.is_none() {
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
                                theme.bg_void,
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

                        // ── Hover skim for audio rows ──
                        // Same UX as the video row hover skim above, but
                        // streams audio instead of video. Gated on:
                        //   - the global toolbar skim toggle
                        //   - the per-feature `file_browser_audio_skim`
                        //     pref (so users can keep video skim on but
                        //     turn audio skim off, or vice versa)
                        //   - no modals open
                        if state.skimming_enabled
                            && state.file_browser_audio_skim_enabled
                            && !state.has_modal_open()
                        {
                            if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                                if thumb_box.contains(pos) {
                                    any_audio_row_hovered = true;
                                    if let Some(dur) = self.file_durations.get(&entry.path).copied() {
                                        let frac = ((pos.x - thumb_box.min.x) / thumb_box.width())
                                            .clamp(0.0, 1.0);
                                        let t = dur * frac as f64;
                                        pending_audio_skim = Some((entry.path.clone(), t));
                                    } else if pending_duration_probe.is_none() {
                                        // First hover — defer the probe to
                                        // after the loop. `ensure_file_duration`
                                        // works for audio too.
                                        pending_duration_probe = Some(entry.path.clone());
                                    }
                                }
                            }
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

                // ── Name + metadata rendering ──
                // Name is on the first line; metadata (enabled columns)
                // is on a second line below in a smaller, dimmer font.
                // For Small rows (32px) there's only room for one line,
                // so metadata is skipped. Medium (56px) and Large (88px)
                // have enough vertical space for both lines.
                let is_renaming = self.renaming_file.as_ref()
                    .map(|(p, _)| p == &entry.path)
                    .unwrap_or(false);
                let name_x = thumb_box.max.x + 8.0;
                let name_max_w = (rect.max.x - name_x - pad).max(20.0);
                let has_metadata_line = row_h >= 48.0 && !self.visible_columns.is_empty();

                // Vertical positioning: if two lines, shift name up.
                let name_y = if has_metadata_line && !is_renaming {
                    rect.center().y - name_font_size * 0.55
                } else {
                    rect.center().y
                };

                if is_renaming {
                    let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let escape_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));

                    let (ref rename_path, ref mut edit_text) =
                        self.renaming_file.as_mut().unwrap();
                    let rename_path_clone = rename_path.clone();
                    let text_rect = egui::Rect::from_min_max(
                        egui::pos2(name_x, rect.top() + 2.0),
                        egui::pos2(rect.right() - pad, rect.bottom() - 2.0),
                    );
                    let mut text_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(text_rect)
                            .id_salt("file_browser_rename_edit"),
                    );
                    let text_resp = text_ui.add(
                        egui::TextEdit::singleline(edit_text)
                            .desired_width(text_rect.width())
                            .font(egui::FontId::proportional(name_font_size)),
                    );
                    text_resp.request_focus();

                    if escape_pressed {
                        self.renaming_file = None;
                    } else if enter_pressed || text_resp.lost_focus() {
                        pending_commit_rename =
                            Some((rename_path_clone, std::mem::take(edit_text)));
                        self.renaming_file = None;
                    }
                } else {
                    #[allow(clippy::if_same_then_else)]
                    let name_color = if is_selected {
                        theme.text_accent
                    } else if entry.is_dir {
                        theme.text_primary
                    } else if entry.is_media {
                        theme.text_primary
                    } else {
                        theme.text_dim
                    };
                    // Truncate name to available width (UTF-8 safe).
                    let char_w = name_font_size * 0.55;
                    let max_chars = (name_max_w / char_w) as usize;
                    let truncated = if entry.name.chars().count() > max_chars && max_chars > 3 {
                        let mut s: String = entry.name.chars().take(max_chars - 1).collect();
                        s.push('\u{2026}');
                        s
                    } else {
                        entry.name.clone()
                    };
                    ui.painter().text(
                        egui::pos2(name_x, name_y),
                        egui::Align2::LEFT_CENTER,
                        &truncated,
                        egui::FontId::proportional(name_font_size),
                        name_color,
                    );

                    // Metadata line (second row, below name).
                    // Builds a "1.2 MB · Apr 12 · MP4 · ★★★" string from
                    // enabled columns. Only renders on Medium/Large rows.
                    if has_metadata_line {
                        let mut parts: Vec<String> = Vec::new();
                        for col in &self.visible_columns {
                            let text = match col {
                                ColumnId::Size => {
                                    if !entry.is_dir && entry.size > 0 {
                                        format_file_size(entry.size)
                                    } else {
                                        String::new()
                                    }
                                }
                                ColumnId::Date => {
                                    entry.mtime.map(format_mtime_short).unwrap_or_default()
                                }
                                ColumnId::Type => {
                                    if entry.is_dir {
                                        "Folder".to_string()
                                    } else {
                                        entry.path.extension()
                                            .map(|e| e.to_string_lossy().to_uppercase())
                                            .unwrap_or_default()
                                    }
                                }
                                ColumnId::Duration => {
                                    self.file_durations.get(&entry.path).map(|dur| {
                                        let m = *dur as u32 / 60;
                                        let s = *dur as u32 % 60;
                                        format!("{m}:{s:02}")
                                    }).unwrap_or_default()
                                }
                                ColumnId::Rating => {
                                    if entry.rating > 0 && !entry.is_dir {
                                        "\u{2605}".repeat(entry.rating as usize)
                                    } else {
                                        String::new()
                                    }
                                }
                            };
                            if !text.is_empty() {
                                parts.push(text);
                            }
                        }
                        if !parts.is_empty() {
                            let meta_text = parts.join("  \u{00b7}  ");
                            let meta_y = name_y + name_font_size + 2.0;
                            ui.painter().text(
                                egui::pos2(name_x, meta_y),
                                egui::Align2::LEFT_CENTER,
                                &meta_text,
                                egui::FontId::proportional(10.0),
                                theme.text_dim,
                            );
                        }
                    }
                }


                // ── Interactions ──
                // All `self` mutations are deferred to after the closure to
                // avoid borrowing `self` while `filtered` still aliases
                // `self.files_entries`.

                // Double-click: navigate (dir) or import (media file)
                // AND open the newly-imported file in the Source
                // viewer. Previously fired LoadMediaFile only, which
                // leaked the new file's first frame into the Program
                // viewer via the engine's shared frame buffer. Using
                // the same `pending_open_in_source` flow the right-
                // click "Open in Source Viewer" action uses:
                //   - LoadMediaFile creates the MediaItem in the pool
                //   - source_media_id is pre-set to the new id so the
                //     Source viewer flips to it as soon as the
                //     MediaItem lands
                //   - viewer_mode snaps to Source
                if resp.double_clicked() {
                    if entry.is_dir {
                        pending_navigate = Some(entry.path.clone());
                    } else if entry.is_media {
                        pending_open_in_source = Some(vec![entry.path.clone()]);
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
                resp.context_menu_rows(|ui| menu_body(ui, |ui| {
                    // ── Import actions (only for media files) ──
                    if menu_count > 0 {
                        let label = if menu_count == 1 {
                            "Add to Media Pool".to_string()
                        } else {
                            format!("Add {} files to Media Pool", menu_count)
                        };
                        if menu_item(ui, &label, "").clicked() {
                            import_request = Some((target_paths.clone(), None));
                            ui.close_menu();
                        }
                        if !bins_snapshot.is_empty() {
                            ui.menu_rows("Add to Bin", |ui| menu_body(ui, |ui| {
                                for bin_name in &bins_snapshot {
                                    if menu_item(ui, bin_name, "").clicked() {
                                        import_request = Some((target_paths.clone(), Some(bin_name.clone())));
                                        ui.close_menu();
                                    }
                                }
                            }));
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
                        if menu_item(ui, &src_label, "").clicked() {
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
                    if menu_item(ui, reveal_label, "").clicked() {
                        let target = if menu_is_dir {
                            menu_path.clone()
                        } else {
                            menu_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| menu_path.clone())
                        };
                        let _ = crate::ui_util::open_external(&target);
                        ui.close_menu();
                    }

                    // ── Open with Default Application (only for files) ──
                    if !menu_is_dir
                        && menu_item(ui, "Open with Default Application", "").clicked() {
                            let _ = crate::ui_util::open_external(&menu_path);
                            ui.close_menu();
                        }

                    // ── Pin / Unpin Favorites (directories only) ──
                    if menu_is_dir {
                        let pin_label = if menu_is_pinned {
                            "Unpin from Favorites"
                        } else {
                            "Pin to Favorites"
                        };
                        if menu_item(ui, pin_label, "").clicked() {
                            pending_pin_toggle = Some(menu_path.clone());
                            ui.close_menu();
                        }
                    }

                    // ── Set Rating ──
                    // Available on any file (not directories). Mirrors
                    // the media pool "Set Rating" submenu pattern.
                    if !menu_is_dir {
                        ui.menu_rows("Set Rating", |ui| menu_body(ui, |ui| {
                            for r in (0..=5).rev() {
                                let label = if r == 0 {
                                    "No Rating".to_string()
                                } else {
                                    "\u{2605}".repeat(r as usize)
                                };
                                if menu_item(ui, &label, "").clicked() {
                                    pending_set_rating = Some((vec![menu_path.clone()], r));
                                    ui.close_menu();
                                }
                            }
                        }));
                    }

                    // ── Copy file path ──
                    if menu_item(ui, "Copy File Path", "").clicked() {
                        ui.output_mut(|o| o.copied_text = menu_path.to_string_lossy().to_string());
                        ui.close_menu();
                    }

                    // ── Rename ──
                    // Only available when the right-clicked item is the
                    // sole selection (or when right-clicking a single item
                    // outside the current selection — single-item implicit).
                    // Disabled for multi-selection and for system paths
                    // that shouldn't be renamed.
                    {
                        let single = menu_count <= 1;
                        let renameable = single && !is_system_path(&menu_path) && !is_recents_view;
                        if renameable
                            && menu_item(ui, "Rename", "").clicked() {
                                pending_start_rename = Some(menu_path.clone());
                                ui.close_menu();
                            }
                    }

                    ui.separator();

                    // ── File Info... ──
                    if menu_item(ui, "File Info\u{2026}", "").clicked() {
                        pending_show_file_info = Some(menu_path.clone());
                        ui.close_menu();
                    }

                    ui.separator();

                    // ── New folder ──
                    // Not available in the virtual Recents view.
                    if !is_recents_view
                        && menu_item(ui, "New Folder", "").clicked() {
                            pending_new_folder = true;
                            ui.close_menu();
                        }

                    // ── Refresh folder ──
                    if menu_item(ui, "Refresh Folder", "").clicked() {
                        pending_refresh = true;
                        ui.close_menu();
                    }
                }));

                // Tooltip on hover (path)
                resp.on_hover_text(entry.path.to_string_lossy().as_ref());
            }

            // ── Empty-area right-click menu ──
            // Allocates whatever vertical space is left below the last
            // file row (or the entire scroll area if the directory is
            // empty) and listens for secondary clicks. Lets users create
            // a new folder or refresh without first having to right-click
            // an existing row — important for empty directories where
            // there's nothing to right-click in the first place.
            //
            // The rect uses `Sense::click()` so primary clicks fall
            // through to the scroll area (don't accidentally deselect
            // the current selection on every empty-area click), but
            // egui's `context_menu()` listens for secondary_clicked
            // automatically.
            let remaining_h = ui.available_height();
            if remaining_h > 0.0 {
                let (_empty_rect, empty_resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), remaining_h),
                    egui::Sense::click(),
                );
                empty_resp.context_menu_rows(|ui| menu_body(ui, |ui| {
                    if !is_recents_view
                        && menu_item(ui, "New Folder", "").clicked() {
                            pending_new_folder = true;
                            ui.close_menu();
                        }
                    if menu_item(ui, "Refresh Folder", "").clicked() {
                        pending_refresh = true;
                        ui.close_menu();
                    }
                }));
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
            egui::Stroke::new(0.5_f32, theme.border),
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
        if !filmstrip_spawn_requests.is_empty() {
            let ctx = ui.ctx().clone();
            for (path, dur, target_h) in filmstrip_spawn_requests {
                self.spawn_file_filmstrip(path, dur, target_h, &ctx);
            }
        }

        // Apply selection changes captured during the row loop.
        match pending_select {
            Some(FileSelectAction::Set(p)) => {
                self.files_selected.clear();
                self.files_selected.insert(p.clone());
                self.files_selection_cursor = Some(p.clone());
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
        // For the Recents view, "refresh" repopulates from the snapshot.
        if pending_refresh {
            if is_recents_view {
                self.navigate_to_recents(state);
            } else {
                self.refresh_files();
            }
        }

        // Right-click Recents button → "Clear Recent Files".
        if pending_clear_recents {
            state.recent_files_snapshot.clear();
            // The prefs-side list will be synced via render_browser_panel's
            // usual prefs ↔ browser diff check. For now the snapshot and
            // display are already empty.
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

        // Right-click → "New folder" — seed the inline editor with a
        // collision-free default name. This only kicks the UI into edit
        // mode; the actual filesystem mutation happens later when the
        // user commits via Enter or click-away (see pending_commit_folder
        // immediately below).
        if pending_new_folder {
            let default_name = unique_untitled_folder_name(&self.files_current_path);
            self.creating_folder = Some(default_name);
        }

        // navigate_files_to refused a navigation request because the
        // inline create-folder editor was open. Toast a hint so the
        // click doesn't feel inert. The flag is reset every frame
        // regardless of whether we toast (no risk of repeat toasts —
        // each refused navigation sets the flag exactly once).
        if self.navigation_blocked_by_create {
            self.navigation_blocked_by_create = false;
            state.toast.show(
                "Press Enter to confirm or Escape to cancel the new folder".to_string(),
            );
        }

        // Inline rename committed → actually create the folder, refresh
        // the directory listing, select the new folder, and toast.
        // On error we keep the editor open so the user can fix the name
        // (e.g. typed a duplicate, or hit a permission error and wants
        // to retry under a different name).
        if let Some(name) = pending_commit_folder {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                // Empty name → silently cancel. Matches Finder: typing
                // nothing and pressing Enter just dismisses the editor.
                self.creating_folder = None;
            } else {
                let parent = self.files_current_path.clone();
                match create_folder_in(&parent, trimmed) {
                    Ok(new_path) => {
                        state.toast.show(format!("Created \"{trimmed}\""));
                        self.creating_folder = None;
                        self.refresh_files();
                        // Auto-select the new folder so the user's next
                        // action (Enter to navigate into it, F2 to
                        // rename, etc.) targets it without an extra
                        // click. The selection survives the refresh
                        // because `refresh_files` doesn't touch it.
                        self.files_selected.clear();
                        self.files_selected.insert(new_path.clone());
                        self.files_selection_anchor = Some(new_path);
                    }
                    Err(msg) => {
                        state.toast.show(msg);
                        // Re-open the editor with the rejected name so
                        // the user can edit it instead of starting over.
                        self.creating_folder = Some(name);
                    }
                }
            }
        }

        // ── Flush any textures queued for deferred drop during the
        // keyboard-handler section (where `filtered` held an immutable
        // borrow of `self`).
        if !pending_defer_drops.is_empty() {
            self.defer_drop_textures(pending_defer_drops);
        }

        // ── Set rating ──
        if let Some((paths, rating)) = pending_set_rating {
            for path in &paths {
                // Update the in-memory cache.
                if rating == 0 {
                    self.file_ratings.remove(path);
                } else {
                    self.file_ratings.insert(path.clone(), rating);
                }
                // Update the entry in files_entries so the stars render
                // immediately (no refresh needed).
                for e in &mut self.files_entries {
                    if e.path == *path {
                        e.rating = rating;
                    }
                }
                // Sync to media pool if the file is already imported.
                if let Some(item) = state.media_pool.find_by_path(path) {
                    let mid = item.id;
                    if let Some(item) = state.media_pool.get_mut(&mid) {
                        item.rating = rating;
                    }
                }
            }
            self.file_ratings_dirty = true;
            // Re-sort if currently sorting by rating.
            if self.files_sort_by == FileSortBy::Rating {
                sort_file_entries(
                    &mut self.files_entries,
                    self.files_sort_by,
                    self.files_sort_ascending,
                );
            }
        }

        // ── Rename: start editing ──
        // Seed the inline editor with the entry's current filename.
        // For files, pre-select just the stem (before the extension) so
        // users don't accidentally strip the extension. For directories,
        // select the full name.
        if let Some(path) = pending_start_rename {
            if !is_system_path(&path) {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.renaming_file = Some((path, name));
            } else {
                state.toast.show("Cannot rename system directories".to_string());
            }
        }

        // ── Rename: commit ──
        if let Some((old_path, new_name)) = pending_commit_rename {
            let trimmed = new_name.trim();
            if trimmed.is_empty() {
                // Empty → silently cancel.
                self.renaming_file = None;
            } else {
                match rename_file_at(&old_path, trimmed) {
                    Ok(new_path) => {
                        // Same-name no-op — don't toast, don't refresh.
                        if new_path != old_path {
                            state.toast.show(format!(
                                "Renamed to \"{}\"",
                                new_path.file_name().unwrap_or_default().to_string_lossy()
                            ));

                            // Update pinned folders if the renamed item was pinned.
                            for p in &mut self.pinned_folders {
                                if *p == old_path {
                                    *p = new_path.clone();
                                }
                            }

                            // Update media pool entries pointing to the old path.
                            for item in state.media_pool.all_mut() {
                                if item.path == old_path {
                                    item.path = new_path.clone();
                                }
                            }

                            self.refresh_files();

                            // Select the newly-renamed entry.
                            self.files_selected.clear();
                            self.files_selected.insert(new_path.clone());
                            self.files_selection_anchor = Some(new_path);
                        }
                        self.renaming_file = None;
                    }
                    Err(msg) => {
                        state.toast.show(msg);
                        // Re-open editor with the rejected name so the
                        // user can fix it without starting over.
                        self.renaming_file = Some((old_path, new_name));
                    }
                }
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
            // Route the handle through the deferred-drop ring — the
            // current frame's submit still references its texture id
            // via the paint list built earlier this frame, and a bare
            // drop trips egui-wgpu's "Texture has been destroyed"
            // validation.
            if let Some((_, h)) = self.file_skim_texture.take() {
                self.defer_drop_texture(h);
            }
        }

        // ── Audio hover skim — apply requests captured during the row loop ──
        // 1. Update the speed estimate so `BrowserSkimAudioUpdate` can
        //    silence the feeder when the cursor stops, matching the
        //    behavior used for media-pool clip skimming.
        if let Some((_, _)) = pending_audio_skim.as_ref() {
            if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                let now = std::time::Instant::now();
                let dt = now.duration_since(state.skim_last_update).as_secs_f32();
                let dx = (pos.x - state.skim_last_x).abs();
                let raw_speed = if dt > 0.001 { dx / dt } else { 0.0 };
                state.skim_speed_px_per_sec =
                    state.skim_speed_px_per_sec * 0.7 + raw_speed * 0.3;
                state.skim_last_x = pos.x;
                state.skim_last_update = now;
            }
        }
        // 2. Decide whether this is a Start (different file or no
        //    active session), an Update (same file, just moving), or
        //    an End (no audio row hovered this frame).
        match (pending_audio_skim, self.file_audio_skim_active.clone()) {
            (Some((path, t)), Some(active_path)) if path == active_path => {
                // Same file — just push a position update.
                cmds.push(AppCommand::BrowserSkimAudioUpdate {
                    media_id: klipp_core::MediaId::new(), // ignored by the handler
                    time_secs: t,
                });
            }
            (Some((path, t)), _) => {
                // New file (or first hover) — start a fresh path-based
                // session. The handler will tear down any previous
                // session before opening this one.
                cmds.push(AppCommand::BrowserSkimAudioStartFromPath {
                    source_path: path.clone(),
                    time_secs: t,
                });
                self.file_audio_skim_active = Some(path);
            }
            (None, Some(_)) if !any_audio_row_hovered => {
                // Cursor left every audio row — tear the session down.
                cmds.push(AppCommand::BrowserSkimAudioEnd);
                self.file_audio_skim_active = None;
            }
            _ => {}
        }

        cmds
    }

    /// Render the Media-tab filter row: smart-bin dropdown, minimum-rating
    /// dropdown, and the colour-label dot toggles. All three combine with
    /// AND across categories and OR within each category (e.g. "Video Only"
    /// + "3+ stars" + "Red or Green label").
    ///
    /// Called from both the split-view right pane and the non-split Media
    /// tab so both layouts have the filters above the item list.
    /// The window a smart bin is written in: its name, whether every
    /// rule must match or any, and the rules, each a field, a comparison
    /// and a value. Save sends the bin to the project and selects it.
    fn show_smart_bin_editor(&mut self, ctx: &egui::Context, theme: &KlippTheme) {
        let Some(mut bin) = self.smart_bin_editor.take() else { return };
        let mut open = true;
        let mut done: Option<bool> = None; // Some(true) = save, Some(false) = cancel
        egui::Window::new("Smart Bin")
            .id(egui::Id::new("smart_bin_editor"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Name").color(theme.text_dim));
                    ui.add(egui::TextEdit::singleline(&mut bin.name).desired_width(220.0));
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Gather clips matching").color(theme.text_dim));
                    ui.radio_value(&mut bin.match_all, true, "all of the rules");
                    ui.radio_value(&mut bin.match_all, false, "any rule");
                });
                ui.separator();
                let mut remove: Option<usize> = None;
                for (i, rule) in bin.rules.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt(("rule_field", i))
                            .selected_text(rule.field.label())
                            .width(110.0)
                            .show_ui(ui, |ui| {
                                for f in klipp_core::Field::ALL {
                                    if ui.selectable_value(&mut rule.field, f, f.label()).changed()
                                        && !klipp_core::RuleOp::for_field(rule.field).contains(&rule.op)
                                    {
                                        rule.op = klipp_core::RuleOp::for_field(rule.field)[0];
                                    }
                                }
                            });
                        egui::ComboBox::from_id_salt(("rule_op", i))
                            .selected_text(rule.op.label())
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for op in klipp_core::RuleOp::for_field(rule.field) {
                                    ui.selectable_value(&mut rule.op, op, op.label());
                                }
                            });
                        if rule.op.takes_value() {
                            let hint = match rule.field {
                                f if f.is_date() => "2024-03-15",
                                klipp_core::Field::Duration => "1:30 or seconds",
                                klipp_core::Field::FileSize => "MB",
                                klipp_core::Field::Resolution => "3840x2160 or 4k",
                                klipp_core::Field::Rating => "1 to 5",
                                _ => "",
                            };
                            ui.add(egui::TextEdit::singleline(&mut rule.value).desired_width(120.0).hint_text(hint));
                        }
                        if ui.add(egui::Button::new(egui::RichText::new("\u{00D7}").color(theme.text_dim)).frame(false)).on_hover_text("Remove rule").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    bin.rules.remove(i);
                }
                if ui.button("+ Add rule").clicked() {
                    bin.rules.push(klipp_core::SmartRule::new(klipp_core::Field::Keywords));
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let can_save = !bin.name.trim().is_empty() && !bin.rules.is_empty();
                    if ui.add_enabled(can_save, egui::Button::new("Save")).clicked() {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        done = Some(false);
                    }
                    ui.label(egui::RichText::new(bin.describe()).size(10.0).color(theme.text_dim));
                });
            });
        match done {
            Some(true) => {
                self.smart_bin_filter = SmartBinFilter::Saved(bin.id.clone());
                self.pending_cmds.push(AppCommand::SaveSmartBin { bin });
            }
            Some(false) => {}
            None if open => self.smart_bin_editor = Some(bin),
            None => {}
        }
    }

    fn render_media_filter_toolbar(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState) {
        // One wrapping row of self-labelled pickers: each reads as its
        // own default ("All media", "Any rating") so the row needs no
        // captions and never clips in a narrow browser.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
            let saved_name = |id: &str| state.media_pool.smart_bins.iter().find(|b| b.id == id).map(|b| b.name.clone());
            let filter_label = match &self.smart_bin_filter {
                SmartBinFilter::None => "All media".to_string(),
                SmartBinFilter::Unused => "Unused".to_string(),
                SmartBinFilter::VideoOnly => "Video Only".to_string(),
                SmartBinFilter::AudioOnly => "Audio Only".to_string(),
                SmartBinFilter::StillsOnly => "Stills Only".to_string(),
                SmartBinFilter::Offline => "Offline".to_string(),
                SmartBinFilter::Favourites => "Favourites".to_string(),
                SmartBinFilter::Rejected => "Rejected".to_string(),
                SmartBinFilter::Saved(id) => saved_name(id).unwrap_or_else(|| "Smart bin".to_string()),
            };
            let mut open_editor: Option<klipp_core::SmartBin> = None;
            egui::ComboBox::from_id_salt("smart_bin")
                .selected_text(filter_label)
                .width(0.0)
                .wrap_mode(egui::TextWrapMode::Extend)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::None, "All media");
                    ui.separator();
                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::Unused, "Unused");
                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::VideoOnly, "Video Only");
                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::AudioOnly, "Audio Only");
                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::StillsOnly, "Stills Only");
                    ui.separator();
                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::Offline, "Offline");
                    ui.separator();
                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::Favourites, "Favourites");
                    ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::Rejected, "Rejected");
                    if !state.media_pool.smart_bins.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("Smart bins").size(10.0).color(theme.text_dim));
                        for bin in &state.media_pool.smart_bins {
                            ui.selectable_value(&mut self.smart_bin_filter, SmartBinFilter::Saved(bin.id.clone()), &bin.name)
                                .on_hover_text(bin.describe());
                        }
                    }
                    ui.separator();
                    if ui.button("New smart bin\u{2026}").clicked() {
                        open_editor = Some(klipp_core::SmartBin::new(uuid::Uuid::new_v4().to_string(), "Smart Bin"));
                        ui.close_menu();
                    }
                    if let SmartBinFilter::Saved(id) = &self.smart_bin_filter {
                        if let Some(bin) = state.media_pool.smart_bins.iter().find(|b| &b.id == id) {
                            if ui.button(format!("Edit \"{}\"\u{2026}", bin.name)).clicked() {
                                open_editor = Some(bin.clone());
                                ui.close_menu();
                            }
                            if ui.button(format!("Delete \"{}\"", bin.name)).clicked() {
                                self.pending_cmds.push(AppCommand::DeleteSmartBin { id: id.clone() });
                                self.smart_bin_filter = SmartBinFilter::None;
                                ui.close_menu();
                            }
                        }
                    }
                });
            if let Some(bin) = open_editor {
                self.smart_bin_editor = Some(bin);
            }

            let rating_label = match self.min_rating_filter {
                None => "Any rating".to_string(),
                Some(1) => "1+ \u{2605}".to_string(),
                Some(2) => "2+ \u{2605}\u{2605}".to_string(),
                Some(3) => "3+ \u{2605}\u{2605}\u{2605}".to_string(),
                Some(4) => "4+ \u{2605}\u{2605}\u{2605}\u{2605}".to_string(),
                Some(5) => "5 \u{2605}\u{2605}\u{2605}\u{2605}\u{2605}".to_string(),
                _ => "Any rating".to_string(),
            };
            egui::ComboBox::from_id_salt("rating_filter")
                .selected_text(rating_label)
                .width(0.0)
                .wrap_mode(egui::TextWrapMode::Extend)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.min_rating_filter, None, "Any rating");
                    for stars in 1u8..=5 {
                        let glyphs = "\u{2605}".repeat(stars as usize);
                        let label = if stars == 5 {
                            format!("5 {glyphs}")
                        } else {
                            format!("{stars}+ {glyphs}")
                        };
                        ui.selectable_value(&mut self.min_rating_filter, Some(stars), label);
                    }
                });

            // ── Metadata filter (codec / fps / camera) ──
            // Single dropdown with grouped flat entries. Available
            // values are pulled from current bin contents, so a project
            // with only h264 footage shows only "Codec: h264" — the
            // dropdown stays as small as the actual diversity in the
            // pool.
            let pool_snapshot: Vec<_> = state.media_pool.all().cloned().collect();
            let values = media_tab::enumerate_metadata_filter_values(&pool_snapshot);
            let (codecs, fps_pairs, cameras) = (&values.codecs, &values.fps, &values.cameras);
            let metadata_label = match &self.metadata_filter {
                MetadataFilter::Any => "Any metadata".to_string(),
                MetadataFilter::Codec(c) => format!("Codec: {c}"),
                MetadataFilter::Fps { num, den } => {
                    format!("FPS: {}", media_tab::format_fps_label(*num, *den))
                }
                MetadataFilter::Camera(c) => format!("Camera: {c}"),
                MetadataFilter::Lens(l) => format!("Lens: {l}"),
                MetadataFilter::Resolution(w, h) => format!("{w}x{h}"),
                MetadataFilter::Reel(r) => format!("Reel: {r}"),
                MetadataFilter::Scene(sc) => format!("Scene: {sc}"),
                MetadataFilter::Keyword(k) => format!("Keyword: {k}"),
                MetadataFilter::GoodTake => "Good takes".to_string(),
            };
            egui::ComboBox::from_id_salt("metadata_filter")
                .selected_text(metadata_label)
                .width(0.0)
                .wrap_mode(egui::TextWrapMode::Extend)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.metadata_filter, MetadataFilter::Any, "Any metadata");
                    if !codecs.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("Codec").size(10.0).color(theme.text_dim));
                        for c in codecs {
                            ui.selectable_value(
                                &mut self.metadata_filter,
                                MetadataFilter::Codec(c.clone()),
                                format!("Codec: {c}"),
                            );
                        }
                    }
                    if !fps_pairs.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("FPS").size(10.0).color(theme.text_dim));
                        for (num, den) in fps_pairs {
                            ui.selectable_value(
                                &mut self.metadata_filter,
                                MetadataFilter::Fps { num: *num, den: *den },
                                format!("FPS: {}", media_tab::format_fps_label(*num, *den)),
                            );
                        }
                    }
                    if !cameras.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("Camera").size(10.0).color(theme.text_dim));
                        for c in cameras {
                            ui.selectable_value(
                                &mut self.metadata_filter,
                                MetadataFilter::Camera(c.clone()),
                                format!("Camera: {c}"),
                            );
                        }
                    }
                    let groups: [(&str, &Vec<String>, fn(String) -> MetadataFilter); 4] = [
                        ("Lens", &values.lenses, MetadataFilter::Lens),
                        ("Reel", &values.reels, MetadataFilter::Reel),
                        ("Scene", &values.scenes, MetadataFilter::Scene),
                        ("Keyword", &values.keywords, MetadataFilter::Keyword),
                    ];
                    for (title, list, make) in groups {
                        if list.is_empty() {
                            continue;
                        }
                        ui.separator();
                        ui.label(egui::RichText::new(title).size(10.0).color(theme.text_dim));
                        for v in list {
                            ui.selectable_value(&mut self.metadata_filter, make(v.clone()), format!("{title}: {v}"));
                        }
                    }
                    if !values.resolutions.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("Resolution").size(10.0).color(theme.text_dim));
                        for (w, h) in &values.resolutions {
                            ui.selectable_value(&mut self.metadata_filter, MetadataFilter::Resolution(*w, *h), format!("{w}x{h}"));
                        }
                    }
                    if values.any_good_take {
                        ui.separator();
                        ui.selectable_value(&mut self.metadata_filter, MetadataFilter::GoodTake, "Good takes");
                    }
                });

            // ── Colour-label filter pill ──
            // A single button that displays "Label" (or "Label · N" when
            // filters are active) and opens a popover with the colour dots.
            // Replaces the previous always-visible 7-dot row, which ate
            // horizontal space in the browser toolbar even for projects
            // that don't use colour labels.
            let active_count = self.color_label_filter.count_ones();
            let pill_text = if active_count == 0 {
                "Any label".to_string()
            } else {
                format!("Label \u{00B7} {active_count}")
            };
            let pill_resp = ui.add(
                egui::Button::new(
                    egui::RichText::new(pill_text)
                        .color(if active_count > 0 { theme.text_accent } else { theme.text_primary }),
                )
                .stroke(if active_count > 0 {
                    egui::Stroke::new(1.0_f32, theme.accent)
                } else {
                    egui::Stroke::NONE
                }),
            );

            let popup_id = ui.make_persistent_id("color_label_filter_popup");
            if pill_resp.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }

            egui::popup::popup_below_widget(
                ui,
                popup_id,
                &pill_resp,
                egui::PopupCloseBehavior::CloseOnClickOutside,
                |ui| {
                    ui.set_min_width(200.0);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.label(
                        egui::RichText::new("Filter by label")
                            .size(10.0)
                            .color(theme.text_dim),
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for (i, label) in klipp_media::ColorLabel::ALL.iter().enumerate() {
                            let bit = 1u8 << i;
                            let is_active = (self.color_label_filter & bit) != 0;
                            let (r, g, b) = label.rgb().unwrap_or((120, 120, 130));
                            let dot_color = egui::Color32::from_rgb(r, g, b);
                            let (dot_rect, dot_resp) = ui.allocate_exact_size(
                                egui::vec2(22.0, 22.0),
                                egui::Sense::click(),
                            );
                            let radius = if is_active { 8.0 } else { 7.0 };
                            ui.painter().circle_filled(dot_rect.center(), radius, dot_color);
                            if is_active {
                                ui.painter().circle_stroke(
                                    dot_rect.center(),
                                    9.5,
                                    egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
                                );
                            } else if dot_resp.hovered() {
                                ui.painter().circle_stroke(
                                    dot_rect.center(),
                                    8.5,
                                    egui::Stroke::new(1.0_f32, egui::Color32::from_gray(180)),
                                );
                            }
                            let tooltip = match label {
                                klipp_media::ColorLabel::None => "No label",
                                klipp_media::ColorLabel::Red => "Red",
                                klipp_media::ColorLabel::Orange => "Orange",
                                klipp_media::ColorLabel::Yellow => "Yellow",
                                klipp_media::ColorLabel::Green => "Green",
                                klipp_media::ColorLabel::Blue => "Blue",
                                klipp_media::ColorLabel::Purple => "Purple",
                            };
                            let dot_resp = dot_resp.on_hover_text(tooltip);
                            if dot_resp.clicked() {
                                self.color_label_filter ^= bit;
                            }
                        }
                    });
                    if self.color_label_filter != 0 {
                        ui.separator();
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Clear filter")
                                    .size(10.0)
                                    .color(theme.text_secondary),
                            ))
                            .clicked()
                        {
                            self.color_label_filter = 0;
                        }
                    }
                },
            );
        });
    }

    fn show_library_tab(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        // ── Category tabs: the same strip as every other tab row ──
        let categories: [(LibraryCategory, &str, &str); 7] = [
            (LibraryCategory::VideoFx, "VIDEO FX", "VFX"),
            (LibraryCategory::AudioFx, "AUDIO FX", "AFX"),
            (LibraryCategory::Titles, "TITLES & GRAPHICS", "T&G"),
            (LibraryCategory::Transitions, "TRANSITIONS", "TRN"),
            (LibraryCategory::Generators, "GENERATORS", "GEN"),
            (LibraryCategory::Audio, "MUSIC & SFX", "AUD"),
            (LibraryCategory::Shared, "SHARED", "SHR"),
        ];
        let labels: Vec<&str> = categories.iter().map(|c| c.1).collect();
        let brief: Vec<&str> = categories.iter().map(|c| c.2).collect();
        let active = categories.iter().position(|c| c.0 == self.library_category).unwrap_or(0);
        if let Some(i) = crate::widgets::inspector_group::tab_strip(ui, &labels, &brief, active, theme) {
            self.library_category = categories[i].0;
        }
        ui.add_space(2.0);

        // ── Category content ──
        match self.library_category {
            LibraryCategory::VideoFx => {
                effects_tab::show(
                    ui,
                    state,
                    theme,
                    &mut self.search_query,
                    &mut self.effects_category,
                    &mut self.favorite_effects,
                    &mut self.recent_effects,
                    &mut self.effect_view_mode,
                    &mut self.effect_sort,
                )
            }
            LibraryCategory::AudioFx => {
                // Build the audition-source options from the current
                // timeline selection. Multi-select reflects each clip
                // individually so the dropdown can offer one entry per
                // clip; the "active" entry is whichever the user picked
                // (or the last clip selected when no explicit pick).
                // Single-click sets only `selected_clip`; shift/ctrl
                // multi-click populates `selected_clips`. Build a
                // deduped union so the dropdown sees both — without
                // this the pill said "No clip selected" even when one
                // clip was clearly highlighted on the timeline.
                let mut source_clip_ids: Vec<klipp_core::ClipId> = Vec::new();
                if let Some(primary) = state.selected_clip {
                    source_clip_ids.push(primary);
                }
                for cid in &state.selected_clips {
                    if !source_clip_ids.contains(cid) {
                        source_clip_ids.push(*cid);
                    }
                }
                let source_options: Vec<audio_fx_tab::AuditionSourceOption> = source_clip_ids
                    .iter()
                    .filter_map(|cid| {
                        // Resolve clip → media filename via the active
                        // sequence + media pool. Clips without a media
                        // backing (e.g. generators, when those land)
                        // are skipped — the dropdown is for audio-bearing
                        // clips only.
                        let seq = state.sequence();
                        for track in &seq.tracks {
                            if let Some(clip) = track.find_clip(*cid) {
                                if let Some(mid) = clip.media_id() {
                                    if let Some(item) = state.media_pool.get(&mid) {
                                        return Some(audio_fx_tab::AuditionSourceOption {
                                            clip_id: *cid,
                                            media_id: mid,
                                            label: item.filename.clone(),
                                        });
                                    }
                                }
                            }
                        }
                        None
                    })
                    .collect();
                // If the previously-picked source is no longer in the
                // current selection, reset it so the UI falls back to
                // the default (last-selected) entry.
                if let Some(picked) = self.audio_fx_audition_source {
                    if !source_options.iter().any(|o| o.clip_id == picked) {
                        self.audio_fx_audition_source = None;
                    }
                }

                // Resolve the active source's media path so we can
                // (a) render real waveform peaks and (b) hand off to
                // the skim-audio player when the user hits play. Picks
                // the explicitly-chosen clip when set, otherwise the
                // first selected clip.
                let active_clip_id: Option<klipp_core::ClipId> = self
                    .audio_fx_audition_source
                    .or_else(|| source_options.first().map(|o| o.clip_id));
                // Resolve everything we need from the media pool in one
                // pass: path (for skim playback), duration (for the
                // M:SS total), and the conformed waveform peaks. The
                // peaks are already cached on `MediaItem.waveform_data`
                // — populated by the `conform_audio` worker when the
                // clip is imported, and re-used by the timeline strip
                // — so we can read them directly instead of going
                // through `BrowserPanel::file_audio_peaks` (which has
                // its own worker pool and was getting starved on this
                // path, leaving the audition stuck on "Loading…").
                struct Resolved {
                    path: std::path::PathBuf,
                    duration_secs: f64,
                    has_audio: bool,
                    peaks: Option<Vec<(f32, f32)>>,
                }
                let resolved: Option<Resolved> = active_clip_id.and_then(|cid| {
                    let seq = state.sequence();
                    for track in &seq.tracks {
                        if let Some(clip) = track.find_clip(cid) {
                            if let Some(mid) = clip.media_id() {
                                if let Some(item) = state.media_pool.get(&mid) {
                                    return Some(Resolved {
                                        path: item.path.clone(),
                                        duration_secs: item.duration.to_secs_f64(),
                                        has_audio: item.has_audio,
                                        peaks: item.waveform_data.clone(),
                                    });
                                }
                            }
                        }
                    }
                    None
                });
                let active_path: Option<std::path::PathBuf> =
                    resolved.as_ref().map(|r| r.path.clone());
                let active_media_id: Option<klipp_core::MediaId> =
                    resolved.as_ref().and_then(|_| {
                        active_clip_id.and_then(|cid| {
                            let seq = state.sequence();
                            for track in &seq.tracks {
                                if let Some(clip) = track.find_clip(cid) {
                                    if let Some(mid) = clip.media_id() {
                                        return Some(mid);
                                    }
                                }
                            }
                            None
                        })
                    });
                let active_duration_secs: Option<f64> =
                    resolved.as_ref().map(|r| r.duration_secs);
                // The clip's media_pool peaks are owned by us for the
                // lifetime of this borrow — clone into a stack-local
                // so we can pass a slice without holding a media_pool
                // borrow across the audio_fx_tab call.
                let local_peaks: Option<Vec<(f32, f32)>> =
                    resolved.as_ref().and_then(|r| r.peaks.clone());
                let audition_peaks: Option<&[(f32, f32)]> =
                    local_peaks.as_deref();
                // "Failed" = clip resolved + media item exists + we
                // know it has no audio stream. The conform worker
                // marks this on the MediaItem at import time. If the
                // clip simply hasn't conformed yet we stay in the
                // Loading state and the spinner keeps polling.
                let audition_failed = resolved
                    .as_ref()
                    .map(|r| !r.has_audio)
                    .unwrap_or(false);

                audio_fx_tab::show(
                    ui,
                    state,
                    theme,
                    &mut self.search_query,
                    &mut self.audio_fx_category,
                    &mut self.audio_fx_source_filter,
                    &mut self.audio_fx_sort,
                    &mut self.audio_fx_audition_playing,
                    &mut self.audio_fx_audition_source,
                    &source_options,
                    active_path.as_deref(),
                    active_media_id,
                    audition_peaks,
                    audition_failed,
                    active_duration_secs,
                    &mut self.audio_fx_audition_volume,
                    &mut self.audio_fx_audition_anchor_secs,
                    &mut self.audio_fx_audition_anchor_at,
                    &mut self.audio_fx_selected,
                    &mut self.audio_fx_auditioning,
                    &mut self.audio_fx_hide_unsupported,
                    &mut self.favorite_effects,
                    &self.recent_effects,
                )
            }
            LibraryCategory::Titles => {
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut self.show_project_graphics, false, "Title presets");
                    ui.selectable_value(&mut self.show_project_graphics, true, "Project graphics");
                });
                ui.add_space(4.0);
                if self.show_project_graphics {
                    return templates_tab::show(ui, state, theme, &mut self.search_query);
                }
                {
                    let mut displaced = Vec::new();
                    self.title_previews.poll(ui.ctx(), &mut displaced);
                    self.defer_drop_textures(displaced);
                }
                titles_tab::show(
                    ui,
                    state,
                    theme,
                    &mut self.search_query,
                    &mut self.titles_category,
                    &mut self.titles_list,
                    &mut self.favorite_titles,
                    &mut self.titles_preview_bg,
                    &mut self.titles_preview_text,
                    &mut self.titles_source_filter,
                    &mut self.titles_animated_only,
                    &mut self.titles_view_mode,
                    &mut self.titles_sort,
                    &mut self.titles_selected,
                    &mut self.title_previews,
                )
            }
            LibraryCategory::Transitions => {
                {
                    let mut displaced = Vec::new();
                    self.transition_previews.poll(ui.ctx(), &mut displaced);
                    // The same two sample pictures in every project, made
                    // once (#547): converting the project's thumbnails
                    // again every frame found nothing new most frames (#549).
                    // Or, asked for, the selected cut's own two frames;
                    // with none to show, the samples and the reason.
                    let cut = self.transitions_cut_preview.then(|| transition_previews::cut_frames(state, self.transition_previews.source()));
                    match cut {
                        Some(Ok(Some(ref frames))) => self.transition_previews.set_frames(frames.clone(), &mut displaced),
                        Some(Ok(None)) => {}
                        _ => {
                            if self.transition_previews.source() != Some("sample") {
                                self.transition_previews.set_frames(transition_previews::sample_frames(), &mut displaced);
                            }
                        }
                    }
                    self.transitions_cut_note = match cut {
                        Some(Err(why)) => Some(why),
                        _ => None,
                    };
                    self.defer_drop_textures(displaced);
                }
                transitions_tab::show(
                    ui,
                    state,
                    theme,
                    &mut self.search_query,
                    &mut self.transitions_category,
                    &mut self.transitions_list,
                    &mut self.favorite_transitions,
                    &mut self.transitions_view_mode,
                    &mut self.transitions_sort,
                    &mut self.transitions_card_size,
                    &mut self.transitions_selected,
                    &mut self.transition_previews,
                    &mut self.transitions_cut_preview,
                    self.transitions_cut_note.as_deref(),
                )
            }
            LibraryCategory::Generators => {
                {
                    let mut displaced = Vec::new();
                    self.generator_previews.poll(ui.ctx(), &mut displaced);
                    self.defer_drop_textures(displaced);
                }
                generators_tab::show(
                    ui,
                    state,
                    theme,
                    &mut self.search_query,
                    &mut self.generators_category,
                    &mut self.generators_list,
                    &mut self.favorite_generators,
                    &mut self.generators_animated_only,
                    &mut self.generators_view_mode,
                    &mut self.generators_sort,
                    &mut self.generators_selected,
                    &mut self.generator_previews,
                )
            }
            LibraryCategory::Audio => self.show_audio_library(ui, state, theme),
            LibraryCategory::Shared => shared_tab::show(ui, state, theme, &mut self.shared_tab, &mut self.library_category),
        }
    }

    /// The Audio Library sub-tab: search royalty-free music and sound
    /// effects, preview them on their source page, and add one into the
    /// pool. The search runs on a thread and its result arrives through
    /// `audio_search_receiver`, so the UI never blocks on the network.
    fn show_audio_library(&mut self, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // Collect an async search result if one has arrived.
        if let Some(rx) = &self.audio_search_receiver {
            match rx.try_recv() {
                Ok(Ok(tracks)) => {
                    self.audio_search_results = tracks;
                    self.audio_search_error = None;
                    self.audio_search_receiver = None;
                    self.audio_search_answered = Some(std::mem::take(&mut self.audio_search_asked));
                }
                Ok(Err(e)) => {
                    self.audio_search_error = Some(e);
                    self.audio_search_results.clear();
                    self.audio_search_receiver = None;
                    self.audio_search_answered = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.audio_search_receiver = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        // A downloaded preview arriving: play it in the app.
        if let Some(rx) = &self.audio_preview_receiver {
            match rx.try_recv() {
                Ok(Ok(bytes)) => {
                    if let Err(e) = klipp_audio_search::preview_play(bytes) {
                        state.toast.show(format!("Preview: {e}"));
                    }
                    self.audio_preview_receiver = None;
                }
                Ok(Err(e)) => {
                    state.toast.show(format!("Preview: {e}"));
                    self.audio_preview_receiver = None;
                    self.audio_preview_id = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.audio_preview_receiver = None;
                    self.audio_preview_id = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        // A track finished downloading: import it through the normal path.
        // Also clear the slot if the worker vanished, so a later Add never
        // stays wedged behind a receiver that will never deliver.
        if let Some(rx) = &self.audio_download_receiver {
            match rx.try_recv() {
                Ok(Ok((path, title))) => {
                    cmds.push(AppCommand::ImportMediaTo { path, drop: self.audio_add_drop });
                    let where_to = match self.audio_add_drop {
                        klipp_core::events::ClipDrop::AtPlayhead => " at the playhead",
                        klipp_core::events::ClipDrop::EndOfTimeline
                        | klipp_core::events::ClipDrop::AppendToTrack { .. }
                        | klipp_core::events::ClipDrop::OnTrackAt { .. } => " to the timeline",
                        klipp_core::events::ClipDrop::PoolOnly => "",
                    };
                    state.toast.show(format!("Added \u{201c}{}\u{201d}{}", title, where_to));
                    self.audio_download_receiver = None;
                    self.audio_add_id = None;
                }
                Ok(Err(e)) => {
                    state.toast.show(format!("Couldn't add it: {e}"));
                    self.audio_download_receiver = None;
                    self.audio_add_id = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.audio_download_receiver = None;
                    self.audio_add_id = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let searching = self.audio_search_receiver.is_some();
        let previewing = self.audio_preview_receiver.is_some();
        let adding = self.audio_download_receiver.is_some();

        ui.add_space(6.0);
        // Music / sound-effects toggle.
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.selectable_label(!self.audio_search_sfx, "Music").clicked() {
                self.audio_search_sfx = false;
            }
            if ui.selectable_label(self.audio_search_sfx, "Sound effects").clicked() {
                self.audio_search_sfx = true;
            }
        });

        // Search box + button.
        let mut do_search = false;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let hint = if self.audio_search_sfx {
                "whoosh, impact, rain, door slam…"
            } else {
                "upbeat, cinematic, calm piano…"
            };
            let field_w = (ui.available_width() - 64.0).max(90.0);
            let resp = ui.add(egui::TextEdit::singleline(&mut self.audio_search_query).hint_text(hint).desired_width(field_w));
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                do_search = true;
            }
            if ui.add_enabled(!searching, egui::Button::new("Search")).clicked() {
                do_search = true;
            }
        });

        // Whether this build has a key for the chosen source.
        let configured = if self.audio_search_sfx {
            klipp_audio_search::sfx_available()
        } else {
            klipp_audio_search::music_available()
        };
        if !configured {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Search isn't set up on this build (no API key).").color(theme.text_secondary));
            });
            return cmds;
        }

        if do_search && !self.audio_search_query.trim().is_empty() {
            let (tx, rx) = std::sync::mpsc::channel();
            let query = self.audio_search_query.trim().to_string();
            let sfx = self.audio_search_sfx;
            // The worker wakes the UI when it answers: egui paints on
            // input, so a result otherwise waited for the mouse to move.
            let ctx = ui.ctx().clone();
            let asked = query.clone();
            std::thread::Builder::new().name("klipp-audio-search".into()).spawn(move || {
                let r = if sfx {
                    klipp_audio_search::search_sfx(&asked, 20)
                } else {
                    klipp_audio_search::search_music(&asked, 20)
                };
                let _ = tx.send(r);
                ctx.request_repaint();
            }).ok();
            self.audio_search_receiver = Some(rx);
            self.audio_search_error = None;
            self.audio_search_asked = query;
            ui.ctx().request_repaint();
        }
        let searching = self.audio_search_receiver.is_some();

        if let Some(e) = &self.audio_search_error {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(e).color(theme.text_secondary));
            });
        }

        ui.add_space(6.0);
        if searching {
            // The previous results go while a search runs, so what is on
            // show is never taken for its answer.
            let library = if self.audio_search_sfx { "Freesound" } else { "Jamendo" };
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.spinner();
                ui.label(egui::RichText::new(format!("Searching {library} for \u{201c}{}\u{201d}\u{2026}", self.audio_search_asked)).color(theme.text_secondary));
            });
            return cmds;
        }
        // Results — cloned out so the row buttons can borrow `self`/`state`.
        let results = self.audio_search_results.clone();
        if results.is_empty() && self.audio_search_error.is_none() {
            let text = match &self.audio_search_answered {
                Some(q) => format!("Nothing found for \u{201c}{q}\u{201d}. Try fewer or other words."),
                None => "Search royalty-free music and sound effects, preview, and add.".to_string(),
            };
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(text).color(theme.text_secondary));
            });
        }
        let adding_id = self.audio_add_id.clone();
        let mut preview_req: Option<(String, String)> = None;
        let mut pause_req = false;
        let mut stop_req = false;
        let mut add_req: Option<(klipp_audio_search::Track, klipp_core::events::ClipDrop)> = None;
        let preview_id = self.audio_preview_id.clone();
        let preview_paused = klipp_audio_search::preview_is_paused();
        let preview_active = klipp_audio_search::preview_active();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for t in &results {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&t.title).strong().color(theme.text_primary));
                        let commercial = if t.commercial_ok { String::new() } else { "  ·  non-commercial".to_string() };
                        ui.label(egui::RichText::new(format!("{}  ·  {:.0}s  ·  {}{}", t.artist, t.duration_secs, t.license, commercial)).small().color(theme.text_secondary));
                    });
                });
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    // This row's transport: Preview until it is playing, then
                    // Pause/Resume and Stop right here on the same row.
                    let is_this = preview_active && preview_id.as_deref() == Some(t.id.as_str());
                    if is_this {
                        if ui.button(if preview_paused { "Resume" } else { "Pause" }).clicked() {
                            pause_req = true;
                        }
                        if ui.button("Stop").clicked() {
                            stop_req = true;
                        }
                    } else if previewing && preview_id.as_deref() == Some(t.id.as_str()) {
                        // This row's preview is downloading.
                        ui.spinner();
                        ui.label(egui::RichText::new("Loading preview\u{2026}").small().color(theme.text_secondary));
                    } else if !t.preview_url.is_empty()
                        && ui.add_enabled(!previewing, egui::Button::new("Preview")).on_hover_text("Listen in the app").clicked()
                    {
                        preview_req = Some((t.id.clone(), t.preview_url.clone()));
                    }
                    if adding && adding_id.as_deref() == Some(t.id.as_str()) {
                        // This row's track is downloading to be added.
                        ui.spinner();
                        ui.label(egui::RichText::new("Adding\u{2026}").small().color(theme.text_secondary));
                    } else {
                        ui.add_enabled_ui(!adding, |ui| {
                            ui.menu_rows("Add ▾", |ui| menu_body(ui, |ui| {
                                use klipp_core::events::ClipDrop;
                                if ui.button("Add to media pool").clicked() {
                                    add_req = Some((t.clone(), ClipDrop::PoolOnly));
                                    ui.close_menu();
                                }
                                if ui.button("Add at playhead").clicked() {
                                    add_req = Some((t.clone(), ClipDrop::AtPlayhead));
                                    ui.close_menu();
                                }
                                if ui.button("Add to end of timeline").clicked() {
                                    add_req = Some((t.clone(), ClipDrop::EndOfTimeline));
                                    ui.close_menu();
                                }
                            })).response.on_hover_text("Download into the pool, with its credit kept");
                        });
                    }
                });
                ui.add_space(4.0);
                ui.separator();
            }
        });
        if pause_req {
            klipp_audio_search::preview_toggle_pause();
        }
        if stop_req {
            klipp_audio_search::preview_stop();
            self.audio_preview_id = None;
        }
        // A preview download runs off-thread and plays when it arrives.
        if let Some((id, url)) = preview_req {
            self.audio_preview_id = Some(id);
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = ui.ctx().clone();
            std::thread::Builder::new().name("klipp-audio-preview".into()).spawn(move || {
                let _ = tx.send(klipp_audio_search::download_bytes(&url));
                ctx.request_repaint();
            }).ok();
            self.audio_preview_receiver = Some(rx);
            ui.ctx().request_repaint();
        }
        // Add downloads off-thread (a blocking download here would freeze
        // the UI); it imports when the file lands.
        if let Some((t, drop)) = add_req {
            self.audio_add_drop = drop;
            self.audio_add_id = Some(t.id.clone());
            let (tx, rx) = std::sync::mpsc::channel();
            state.toast.show(format!("Downloading \u{201c}{}\u{201d}\u{2026}", t.title));
            let ctx = ui.ctx().clone();
            std::thread::Builder::new().name("klipp-audio-add".into()).spawn(move || {
                let title = t.title.clone();
                let r = klipp_audio_search::download_track(&t, &klipp_audio_search::download_dir()).map(|p| (p, title));
                let _ = tx.send(r);
                ctx.request_repaint();
            }).ok();
            self.audio_download_receiver = Some(rx);
            ui.ctx().request_repaint();
        }
        cmds
    }
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
            rating: 0, // Populated from the ratings cache after async read completes.
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

/// A file's last change for the Files rows, in local time (see
/// [`format_mtime_at`]).
fn format_mtime_short(mtime: std::time::SystemTime) -> String {
    format_mtime_at(mtime, chrono::Local::now())
}

/// When a file was last changed, as `now` sees it in local time: the
/// clock time today ("14:05"), "Yesterday 14:05", the day and month this
/// year ("3 Oct"), with the year before that ("3 Oct 2024").
fn format_mtime_at(mtime: std::time::SystemTime, now: chrono::DateTime<chrono::Local>) -> String {
    let at: chrono::DateTime<chrono::Local> = mtime.into();
    let (day, today) = (at.date_naive(), now.date_naive());
    if day == today {
        at.format("%H:%M").to_string()
    } else if today.pred_opt() == Some(day) {
        at.format("Yesterday %H:%M").to_string()
    } else if day.format("%Y").to_string() == today.format("%Y").to_string() {
        at.format("%-d %b").to_string()
    } else {
        at.format("%-d %b %Y").to_string()
    }
}

/// Path to the global file-ratings cache.
fn file_ratings_cache_path() -> Option<std::path::PathBuf> {
    dirs::cache_dir().map(|d| d.join("klipp").join("file-ratings.json"))
}

/// Load the file-ratings cache from disk. Returns an empty map on any
/// error (missing file, malformed JSON, permission denied).
fn load_file_ratings_cache() -> HashMap<std::path::PathBuf, u8> {
    let Some(path) = file_ratings_cache_path() else {
        return HashMap::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Save the file-ratings cache to disk. Silently ignores errors (cache
/// is convenience data — losing it is annoying but not destructive).
pub fn save_file_ratings_cache(ratings: &HashMap<std::path::PathBuf, u8>) {
    let Some(path) = file_ratings_cache_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(ratings) {
        let _ = std::fs::write(&path, json);
    }
}

/// Date group labels for the "group by date" view. Each entry maps to
/// a human-readable section header rendered between file rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateGroup {
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    Older,
}

impl DateGroup {
    fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::ThisWeek => "This Week",
            Self::ThisMonth => "This Month",
            Self::Older => "Older",
        }
    }

    /// Classify a file's mtime into a date group relative to `now`.
    fn from_mtime(mtime: Option<std::time::SystemTime>, now: std::time::SystemTime) -> Self {
        let Some(mt) = mtime else { return Self::Older };
        let elapsed = now.duration_since(mt).unwrap_or_default();
        let secs = elapsed.as_secs();
        const DAY: u64 = 86_400;
        if secs < DAY {
            Self::Today
        } else if secs < 2 * DAY {
            Self::Yesterday
        } else if secs < 7 * DAY {
            Self::ThisWeek
        } else if secs < 30 * DAY {
            Self::ThisMonth
        } else {
            Self::Older
        }
    }
}

/// Compute date-group section boundaries for the given filtered list.
/// Returns a list of `(first_row_index, DateGroup)` pairs. Only groups
/// that actually contain files are included (empty groups are hidden).
///
/// The input MUST already be sorted by mtime descending (newest first)
/// for the boundaries to make visual sense. The caller is responsible
/// for enforcing that sort order when group mode is active.
fn compute_date_groups(entries: &[&FileEntry]) -> Vec<(usize, DateGroup)> {
    if entries.is_empty() {
        return Vec::new();
    }
    let now = std::time::SystemTime::now();
    let mut groups: Vec<(usize, DateGroup)> = Vec::new();
    let mut current_group: Option<DateGroup> = None;

    for (idx, entry) in entries.iter().enumerate() {
        let group = DateGroup::from_mtime(entry.mtime, now);
        if current_group != Some(group) {
            groups.push((idx, group));
            current_group = Some(group);
        }
    }
    groups
}

/// Check whether a path is a "system" location that users should never
/// rename from inside an NLE. The check is intentionally conservative:
/// better to err on the side of caution and have the user rename via
/// their OS file manager than to let them brick their system.
///
/// We check the ITEM ITSELF (not its parent) — i.e., we block renaming
/// `/usr` but not renaming a file inside `/usr/local/share/`.
fn is_system_path(path: &std::path::Path) -> bool {
    // Root is never renameable.
    if path == std::path::Path::new("/") {
        return true;
    }
    // User's home directory itself.
    if let Some(home) = dirs::home_dir() {
        if path == home {
            return true;
        }
    }
    // Top-level system directories. This list covers Linux FHS roots;
    // on macOS/Windows the OS permissions already block these so we
    // don't need to enumerate them.
    const SYSTEM_ROOTS: &[&str] = &[
        "/bin", "/boot", "/dev", "/etc", "/lib", "/lib32", "/lib64",
        "/lost+found", "/opt", "/proc", "/root", "/run", "/sbin",
        "/srv", "/sys", "/tmp", "/usr", "/var",
    ];
    let s = path.to_string_lossy();
    SYSTEM_ROOTS.iter().any(|r| s.as_ref() == *r)
}

/// Validate a user-typed new name and perform the rename via
/// `std::fs::rename`. Returns the new full path on success, or a
/// human-readable error message on failure (suitable for a toast).
///
/// Validation mirrors `create_folder_in` — empty, path separators,
/// reserved names, collisions — plus a "same name" no-op short-circuit
/// and a system-path guard.
fn rename_file_at(
    old_path: &std::path::Path,
    new_name: &str,
) -> Result<std::path::PathBuf, String> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err("Name cannot be empty".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err("Name cannot contain '/' or '\\'".to_string());
    }
    if trimmed == "." || trimmed == ".." {
        return Err(format!("'{trimmed}' is a reserved name"));
    }

    let old_name = old_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    // No-op: user committed the same name they started with.
    if trimmed == old_name {
        return Ok(old_path.to_path_buf());
    }

    if is_system_path(old_path) {
        return Err("Cannot rename system directories".to_string());
    }

    let parent = old_path
        .parent()
        .ok_or_else(|| "Cannot determine parent directory".to_string())?;
    let new_path = parent.join(trimmed);

    if new_path.exists() {
        return Err(format!("\"{trimmed}\" already exists here"));
    }

    std::fs::rename(old_path, &new_path)
        .map_err(|e| format!("Cannot rename: {e}"))?;

    Ok(new_path)
}

/// Compute a non-colliding default name for a new folder inside `parent`,
/// macOS Finder style: `untitled folder`, then `untitled folder 2`,
/// `untitled folder 3`, ... up to 999. Returns the first name whose
/// fully-qualified path doesn't already exist on disk.
///
/// Returns `untitled folder` if `parent` doesn't exist (caller will fail
/// at the actual `create_dir` step with a clearer error than "name in use").
fn unique_untitled_folder_name(parent: &std::path::Path) -> String {
    const BASE: &str = "untitled folder";
    if !parent.join(BASE).exists() {
        return BASE.to_string();
    }
    for n in 2..=999 {
        let candidate = format!("{BASE} {n}");
        if !parent.join(&candidate).exists() {
            return candidate;
        }
    }
    // Fallback: 999 untitled folders is well past "user is doing something
    // weird" — ship the base name and let the OS error sort it out.
    BASE.to_string()
}

/// Validate a user-typed folder name and create the directory inside
/// `parent`. Returns the new directory's full path on success, or a
/// human-readable error message on failure (suitable for a toast).
///
/// Validation rules — kept conservative so the same code works on every
/// filesystem Klipp users care about (ext4, btrfs, xfs, fat32 on USB,
/// ntfs on shared drives):
/// - Empty / whitespace-only names are rejected.
/// - Names containing path separators (`/` or `\`) are rejected so users
///   can't accidentally drill into subdirectories during inline rename.
/// - Reserved names `.` and `..` are rejected.
/// - Pre-existing names (file OR directory) are rejected with a clear
///   "already exists here" message instead of relying on the OS error.
/// - Everything else is handed to `std::fs::create_dir`, whose error
///   message (permission denied, read-only filesystem, etc.) is passed
///   through verbatim.
fn create_folder_in(parent: &std::path::Path, name: &str) -> Result<std::path::PathBuf, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Folder name cannot be empty".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err("Folder name cannot contain '/' or '\\'".to_string());
    }
    if trimmed == "." || trimmed == ".." {
        return Err(format!("'{trimmed}' is a reserved name"));
    }
    let target = parent.join(trimmed);
    if target.exists() {
        return Err(format!("\"{trimmed}\" already exists here"));
    }
    std::fs::create_dir(&target).map_err(|e| format!("Cannot create folder: {e}"))?;
    Ok(target)
}

/// Opens Find by Picture (#2212): footage found by what is in its
/// picture. Drawn above the media list in the single and split views.
/// The media pool's buttons under its filters: New Bin and Find by Picture.
fn media_pool_buttons(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>) {
    ui.horizontal(|ui| {
        if ui.small_button("New Bin").on_hover_text("A bin to gather clips in. Its name is ready to type.").clicked() {
            cmds.push(AppCommand::NewBin { media_ids: Vec::new() });
        }
        if ui.small_button("Find by Picture\u{2026}").on_hover_text("Describe a shot (\u{201c}a dog on a beach\u{201d}) to find the footage that looks like it").clicked() {
            cmds.push(AppCommand::OpenVisualSearch);
        }
    });
}

#[cfg(test)]
mod new_folder_tests {
    //! Tests for the file-browser "New folder" feature (#11 in the
    //! file browser feature queue). Covers the two pure helper
    //! functions used by the inline-create flow:
    //!
    //!   - `unique_untitled_folder_name` — collision-free default name
    //!   - `create_folder_in` — validation + actual mkdir
    //!
    //! Each test creates a fresh tempdir under the system temp area so
    //! they can run in parallel without interfering with each other.
    use super::{create_folder_in, unique_untitled_folder_name};
    use std::path::PathBuf;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "klipp_new_folder_test_{}_{}_{}",
            label,
            std::process::id(),
            // Nanoseconds since epoch keeps each test isolated even when
            // pid is reused (e.g. in parallel `cargo test` runs).
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    // ── unique_untitled_folder_name ──────────────────────────────

    #[test]
    fn unique_name_returns_base_when_directory_is_empty() {
        let dir = temp_dir("uniq_empty");
        assert_eq!(unique_untitled_folder_name(&dir), "untitled folder");
        cleanup(&dir);
    }

    #[test]
    fn unique_name_increments_on_first_collision() {
        let dir = temp_dir("uniq_one");
        std::fs::create_dir(dir.join("untitled folder")).unwrap();
        assert_eq!(unique_untitled_folder_name(&dir), "untitled folder 2");
        cleanup(&dir);
    }

    #[test]
    fn unique_name_increments_through_a_run_of_collisions() {
        let dir = temp_dir("uniq_run");
        std::fs::create_dir(dir.join("untitled folder")).unwrap();
        std::fs::create_dir(dir.join("untitled folder 2")).unwrap();
        std::fs::create_dir(dir.join("untitled folder 3")).unwrap();
        std::fs::create_dir(dir.join("untitled folder 4")).unwrap();
        assert_eq!(unique_untitled_folder_name(&dir), "untitled folder 5");
        cleanup(&dir);
    }

    #[test]
    fn unique_name_treats_a_file_collision_the_same_as_a_folder_collision() {
        // The check is `.exists()`, which catches files too. Otherwise we'd
        // suggest a name that immediately fails on create_dir.
        let dir = temp_dir("uniq_file");
        std::fs::write(dir.join("untitled folder"), b"x").unwrap();
        assert_eq!(unique_untitled_folder_name(&dir), "untitled folder 2");
        cleanup(&dir);
    }

    #[test]
    fn unique_name_handles_gaps_in_the_sequence() {
        // If the user manually deleted "untitled folder 2" we should pick
        // 2 again instead of 5 — this isn't critical UX-wise but it
        // documents the "first available" semantic explicitly.
        let dir = temp_dir("uniq_gap");
        std::fs::create_dir(dir.join("untitled folder")).unwrap();
        std::fs::create_dir(dir.join("untitled folder 3")).unwrap();
        std::fs::create_dir(dir.join("untitled folder 4")).unwrap();
        assert_eq!(unique_untitled_folder_name(&dir), "untitled folder 2");
        cleanup(&dir);
    }

    // ── create_folder_in: success path ───────────────────────────

    #[test]
    fn create_folder_creates_the_directory_on_disk() {
        let dir = temp_dir("create_ok");
        let result = create_folder_in(&dir, "Project Files");
        let new_path = result.expect("create should succeed");
        assert!(new_path.exists(), "new path should exist on disk");
        assert!(new_path.is_dir(), "new path should be a directory");
        assert_eq!(new_path, dir.join("Project Files"));
        cleanup(&dir);
    }

    #[test]
    fn create_folder_trims_leading_and_trailing_whitespace() {
        let dir = temp_dir("create_trim");
        let result = create_folder_in(&dir, "  spaced  ");
        let new_path = result.expect("create should succeed");
        // The on-disk name is the trimmed version, not the padded input.
        assert_eq!(new_path, dir.join("spaced"));
        assert!(new_path.exists());
        cleanup(&dir);
    }

    #[test]
    fn create_folder_allows_unicode_names() {
        let dir = temp_dir("create_unicode");
        let result = create_folder_in(&dir, "B-roll \u{1F3AC}");
        let new_path = result.expect("unicode should work");
        assert!(new_path.exists());
        cleanup(&dir);
    }

    // ── create_folder_in: validation rejection ───────────────────

    #[test]
    fn create_folder_rejects_empty_name() {
        let dir = temp_dir("create_empty");
        let err = create_folder_in(&dir, "").unwrap_err();
        assert!(err.to_lowercase().contains("empty"), "got: {err}");
        cleanup(&dir);
    }

    #[test]
    fn create_folder_rejects_whitespace_only_name() {
        let dir = temp_dir("create_ws");
        let err = create_folder_in(&dir, "   ").unwrap_err();
        assert!(err.to_lowercase().contains("empty"), "got: {err}");
        cleanup(&dir);
    }

    #[test]
    fn create_folder_rejects_forward_slash_in_name() {
        let dir = temp_dir("create_slash");
        let err = create_folder_in(&dir, "foo/bar").unwrap_err();
        assert!(err.contains('/') || err.contains("'/'"), "got: {err}");
        // Confirm nothing was actually created.
        assert!(!dir.join("foo").exists());
        cleanup(&dir);
    }

    #[test]
    fn create_folder_rejects_backslash_in_name() {
        let dir = temp_dir("create_bslash");
        let err = create_folder_in(&dir, "foo\\bar").unwrap_err();
        assert!(err.contains('\\') || err.contains("'\\\\'"), "got: {err}");
        cleanup(&dir);
    }

    #[test]
    fn create_folder_rejects_dot() {
        let dir = temp_dir("create_dot");
        let err = create_folder_in(&dir, ".").unwrap_err();
        assert!(err.contains("reserved"), "got: {err}");
        cleanup(&dir);
    }

    #[test]
    fn create_folder_rejects_double_dot() {
        let dir = temp_dir("create_dotdot");
        let err = create_folder_in(&dir, "..").unwrap_err();
        assert!(err.contains("reserved"), "got: {err}");
        cleanup(&dir);
    }

    // ── create_folder_in: collision rejection ────────────────────

    #[test]
    fn create_folder_rejects_existing_directory_name() {
        let dir = temp_dir("create_collide_dir");
        std::fs::create_dir(dir.join("clash")).unwrap();
        let err = create_folder_in(&dir, "clash").unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");
        cleanup(&dir);
    }

    #[test]
    fn create_folder_rejects_existing_file_name() {
        let dir = temp_dir("create_collide_file");
        std::fs::write(dir.join("clash"), b"x").unwrap();
        let err = create_folder_in(&dir, "clash").unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");
        // The file was not clobbered.
        assert!(dir.join("clash").is_file());
        cleanup(&dir);
    }

    #[test]
    fn create_folder_returns_clear_error_for_missing_parent() {
        // Parent doesn't exist → std::fs::create_dir errors out and we
        // pass the OS message through. We just need to confirm we get
        // a "Cannot create folder" prefix, not the helper's own
        // collision/empty messages.
        let nonexistent = std::env::temp_dir().join(format!(
            "klipp_new_folder_test_missing_parent_{}",
            std::process::id()
        ));
        // Make sure it really doesn't exist.
        let _ = std::fs::remove_dir_all(&nonexistent);
        let err = create_folder_in(&nonexistent, "child").unwrap_err();
        assert!(err.starts_with("Cannot create folder"), "got: {err}");
    }
}

#[cfg(test)]
mod file_rating_tests {
    use super::{file_ratings_cache_path, load_file_ratings_cache, save_file_ratings_cache};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn cache_path_is_under_klipp_cache_dir() {
        if let Some(path) = file_ratings_cache_path() {
            let s = path.to_string_lossy();
            assert!(s.contains("klipp"), "cache path should be under klipp dir: {s}");
            assert!(s.ends_with("file-ratings.json"), "should end with file-ratings.json: {s}");
        }
    }

    #[test]
    fn load_returns_empty_for_missing_file() {
        // load_file_ratings_cache reads from the real cache path, which
        // may or may not exist. If it doesn't exist, it returns empty.
        // If it does, it returns whatever's in there. Either way, no panic.
        let _ = load_file_ratings_cache();
    }

    #[test]
    fn save_and_load_round_trip() {
        let Some(path) = file_ratings_cache_path() else { return };
        // Back up existing cache if present.
        let backup = path.with_extension("json.bak.test");
        let had_existing = path.exists();
        if had_existing {
            let _ = std::fs::copy(&path, &backup);
        }

        let mut ratings = HashMap::new();
        ratings.insert(PathBuf::from("/tmp/test_a.mp4"), 5u8);
        ratings.insert(PathBuf::from("/tmp/test_b.wav"), 3u8);
        save_file_ratings_cache(&ratings);

        let loaded = load_file_ratings_cache();
        assert_eq!(loaded.get(&PathBuf::from("/tmp/test_a.mp4")), Some(&5));
        assert_eq!(loaded.get(&PathBuf::from("/tmp/test_b.wav")), Some(&3));

        // Restore original cache.
        if had_existing {
            let _ = std::fs::copy(&backup, &path);
            let _ = std::fs::remove_file(&backup);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn sort_by_rating_puts_highest_first_descending() {
        use super::{sort_file_entries, FileEntry, FileSortBy};
        let mut entries = vec![
            FileEntry {
                name: "low.mp4".into(), path: "low.mp4".into(),
                is_dir: false, size: 100, is_media: true, mtime: None, rating: 1,
            },
            FileEntry {
                name: "high.mp4".into(), path: "high.mp4".into(),
                is_dir: false, size: 100, is_media: true, mtime: None, rating: 5,
            },
            FileEntry {
                name: "mid.mp4".into(), path: "mid.mp4".into(),
                is_dir: false, size: 100, is_media: true, mtime: None, rating: 3,
            },
        ];
        sort_file_entries(&mut entries, FileSortBy::Rating, false); // descending
        assert_eq!(entries[0].name, "high.mp4");
        assert_eq!(entries[1].name, "mid.mp4");
        assert_eq!(entries[2].name, "low.mp4");
    }

    #[test]
    fn sort_by_rating_ascending() {
        use super::{sort_file_entries, FileEntry, FileSortBy};
        let mut entries = vec![
            FileEntry {
                name: "high.mp4".into(), path: "high.mp4".into(),
                is_dir: false, size: 100, is_media: true, mtime: None, rating: 5,
            },
            FileEntry {
                name: "low.mp4".into(), path: "low.mp4".into(),
                is_dir: false, size: 100, is_media: true, mtime: None, rating: 1,
            },
        ];
        sort_file_entries(&mut entries, FileSortBy::Rating, true); // ascending
        assert_eq!(entries[0].name, "low.mp4");
        assert_eq!(entries[1].name, "high.mp4");
    }
}

#[cfg(test)]
mod date_group_tests {
    use super::{compute_date_groups, DateGroup, FileEntry};
    use std::time::{Duration, SystemTime};

    fn entry_with_mtime(name: &str, age_secs: u64) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            path: std::path::PathBuf::from(name),
            is_dir: false,
            size: 100,
            is_media: true,
            mtime: Some(SystemTime::now() - Duration::from_secs(age_secs)),
            rating: 0,
        }
    }

    fn entry_no_mtime(name: &str) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            path: std::path::PathBuf::from(name),
            is_dir: false,
            size: 100,
            is_media: true,
            mtime: None,
            rating: 0,
        }
    }

    #[test]
    fn empty_list_returns_no_groups() {
        let entries: Vec<&FileEntry> = vec![];
        let groups = compute_date_groups(&entries);
        assert!(groups.is_empty());
    }

    #[test]
    fn single_today_file() {
        let e = entry_with_mtime("a.mp4", 3600); // 1 hour ago
        let entries = vec![&e];
        let groups = compute_date_groups(&entries);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], (0, DateGroup::Today));
    }

    #[test]
    fn today_and_yesterday() {
        let a = entry_with_mtime("a.mp4", 3600);     // 1 hour ago = Today
        let b = entry_with_mtime("b.mp4", 100_000);   // ~27 hours ago = Yesterday
        let entries = vec![&a, &b];
        let groups = compute_date_groups(&entries);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], (0, DateGroup::Today));
        assert_eq!(groups[1], (1, DateGroup::Yesterday));
    }

    #[test]
    fn all_five_groups() {
        let today = entry_with_mtime("today.mp4", 100);
        let yesterday = entry_with_mtime("yesterday.mp4", 100_000);
        let this_week = entry_with_mtime("week.mp4", 400_000);   // ~4.6 days
        let this_month = entry_with_mtime("month.mp4", 1_500_000); // ~17 days
        let older = entry_with_mtime("older.mp4", 5_000_000);     // ~58 days
        let entries = vec![&today, &yesterday, &this_week, &this_month, &older];
        let groups = compute_date_groups(&entries);
        assert_eq!(groups.len(), 5);
        assert_eq!(groups[0].1, DateGroup::Today);
        assert_eq!(groups[1].1, DateGroup::Yesterday);
        assert_eq!(groups[2].1, DateGroup::ThisWeek);
        assert_eq!(groups[3].1, DateGroup::ThisMonth);
        assert_eq!(groups[4].1, DateGroup::Older);
    }

    #[test]
    fn multiple_files_in_same_group_produce_one_header() {
        let a = entry_with_mtime("a.mp4", 100);
        let b = entry_with_mtime("b.mp4", 200);
        let c = entry_with_mtime("c.mp4", 300);
        let entries = vec![&a, &b, &c]; // all Today
        let groups = compute_date_groups(&entries);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], (0, DateGroup::Today));
    }

    #[test]
    fn none_mtime_goes_to_older() {
        let e = entry_no_mtime("unknown.mp4");
        let entries = vec![&e];
        let groups = compute_date_groups(&entries);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].1, DateGroup::Older);
    }

    #[test]
    fn group_labels_are_human_readable() {
        assert_eq!(DateGroup::Today.label(), "Today");
        assert_eq!(DateGroup::Yesterday.label(), "Yesterday");
        assert_eq!(DateGroup::ThisWeek.label(), "This Week");
        assert_eq!(DateGroup::ThisMonth.label(), "This Month");
        assert_eq!(DateGroup::Older.label(), "Older");
    }
}

#[cfg(test)]
mod folder_watcher_tests {
    use super::start_folder_watcher;

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "klipp_watcher_test_{}_{}_{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn watcher_returns_none_for_missing_dir() {
        let (w, rx) = start_folder_watcher(std::path::Path::new("/nonexistent_klipp_test_dir"));
        assert!(w.is_none());
        assert!(rx.is_none());
    }

    #[test]
    fn watcher_returns_some_for_existing_dir() {
        let dir = temp_dir("exists");
        let (w, rx) = start_folder_watcher(&dir);
        assert!(w.is_some(), "watcher should be created for existing dir");
        assert!(rx.is_some(), "receiver should be created");
        cleanup(&dir);
    }

    #[test]
    fn watcher_detects_file_creation() {
        let dir = temp_dir("detect_create");
        let (w, rx) = start_folder_watcher(&dir);
        let rx = rx.expect("watcher should be created");

        // Create a file inside the watched directory.
        std::fs::write(dir.join("new_file.txt"), b"hello").unwrap();

        // Give inotify + the OS a moment to deliver the event. The
        // watcher callback runs on a background thread and writes to
        // the channel, which we poll here. 500ms is generous.
        let got = rx.recv_timeout(std::time::Duration::from_millis(500));
        assert!(got.is_ok(), "should receive a change notification");

        drop(w); // stop watching
        cleanup(&dir);
    }

    #[test]
    fn watcher_detects_file_removal() {
        let dir = temp_dir("detect_remove");
        let existing = dir.join("existing.txt");
        std::fs::write(&existing, b"data").unwrap();

        let (w, rx) = start_folder_watcher(&dir);
        let rx = rx.expect("watcher should be created");

        // Remove the file.
        std::fs::remove_file(&existing).unwrap();

        let got = rx.recv_timeout(std::time::Duration::from_millis(500));
        assert!(got.is_ok(), "should receive a removal notification");

        drop(w);
        cleanup(&dir);
    }

    #[test]
    fn watcher_channel_is_bounded_and_does_not_block() {
        // The channel is bounded(4). Sending more than 4 events should
        // not block the watcher thread (try_send drops the excess).
        let dir = temp_dir("bounded");
        let (_w, rx) = start_folder_watcher(&dir);
        let rx = rx.expect("watcher should be created");

        // Create many files rapidly.
        for i in 0..20 {
            std::fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
        }

        // Wait briefly then drain — should get at most 4 messages (bounded).
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert!(count > 0, "should have received at least one event");
        assert!(count <= 4, "bounded channel should cap at 4, got {count}");

        cleanup(&dir);
    }
}

#[cfg(test)]
mod recents_tests {
    use super::RECENTS_SENTINEL;
    use crate::prefs::FileBrowserPrefs;

    #[test]
    fn sentinel_is_not_a_real_path() {
        let p = RECENTS_SENTINEL.as_path();
        assert!(!p.exists(), "sentinel must not match a real filesystem path");
        assert_eq!(p.to_string_lossy(), "klipp://recents");
    }

    #[test]
    fn add_recent_file_inserts_at_front() {
        let mut prefs = FileBrowserPrefs::default();
        prefs.add_recent_file("/a.mp4".into());
        prefs.add_recent_file("/b.mp4".into());
        assert_eq!(prefs.recent_files[0].to_string_lossy(), "/b.mp4");
        assert_eq!(prefs.recent_files[1].to_string_lossy(), "/a.mp4");
    }

    #[test]
    fn add_recent_file_deduplicates() {
        let mut prefs = FileBrowserPrefs::default();
        prefs.add_recent_file("/a.mp4".into());
        prefs.add_recent_file("/b.mp4".into());
        prefs.add_recent_file("/a.mp4".into()); // bump to front
        assert_eq!(prefs.recent_files.len(), 2);
        assert_eq!(prefs.recent_files[0].to_string_lossy(), "/a.mp4");
    }

    #[test]
    fn add_recent_file_truncates_at_max() {
        let mut prefs = FileBrowserPrefs::default();
        for i in 0..60 {
            prefs.add_recent_file(format!("/file_{i}.mp4").into());
        }
        assert_eq!(prefs.recent_files.len(), 50);
        // Most recent (i=59) should be first.
        assert_eq!(prefs.recent_files[0].to_string_lossy(), "/file_59.mp4");
        // Oldest surviving (i=10) should be last.
        assert_eq!(prefs.recent_files[49].to_string_lossy(), "/file_10.mp4");
    }
}

#[cfg(test)]
mod rename_tests {
    use super::{is_system_path, rename_file_at};
    use std::path::PathBuf;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "klipp_rename_test_{}_{}_{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    // ── is_system_path ──────────────────────────────────────────

    #[test]
    fn system_path_blocks_root() {
        assert!(is_system_path(std::path::Path::new("/")));
    }

    #[test]
    fn system_path_blocks_usr() {
        assert!(is_system_path(std::path::Path::new("/usr")));
    }

    #[test]
    fn system_path_blocks_etc() {
        assert!(is_system_path(std::path::Path::new("/etc")));
    }

    #[test]
    fn system_path_blocks_home_dir() {
        if let Some(home) = dirs::home_dir() {
            assert!(is_system_path(&home));
        }
    }

    #[test]
    fn system_path_allows_user_subdirectory() {
        if let Some(home) = dirs::home_dir() {
            assert!(!is_system_path(&home.join("Documents")));
        }
    }

    #[test]
    fn system_path_allows_file_inside_usr() {
        // We block renaming /usr itself, not files inside it.
        assert!(!is_system_path(std::path::Path::new("/usr/local/share/myfile")));
    }

    #[test]
    fn system_path_allows_temp_dir() {
        let dir = temp_dir("syspath");
        assert!(!is_system_path(&dir));
        cleanup(&dir);
    }

    // ── rename_file_at: success ─────────────────────────────────

    #[test]
    fn rename_file_succeeds() {
        let dir = temp_dir("rename_ok");
        let old = dir.join("original.txt");
        std::fs::write(&old, b"data").unwrap();
        let new = rename_file_at(&old, "renamed.txt").expect("rename should succeed");
        assert_eq!(new, dir.join("renamed.txt"));
        assert!(!old.exists());
        assert!(new.exists());
        cleanup(&dir);
    }

    #[test]
    fn rename_directory_succeeds() {
        let dir = temp_dir("rename_dir");
        let old = dir.join("old_dir");
        std::fs::create_dir(&old).unwrap();
        let new = rename_file_at(&old, "new_dir").expect("dir rename should succeed");
        assert_eq!(new, dir.join("new_dir"));
        assert!(!old.exists());
        assert!(new.is_dir());
        cleanup(&dir);
    }

    #[test]
    fn rename_trims_whitespace() {
        let dir = temp_dir("rename_trim");
        let old = dir.join("original.txt");
        std::fs::write(&old, b"data").unwrap();
        let new = rename_file_at(&old, "  trimmed.txt  ").unwrap();
        assert_eq!(new, dir.join("trimmed.txt"));
        cleanup(&dir);
    }

    #[test]
    fn rename_same_name_is_noop() {
        let dir = temp_dir("rename_same");
        let file = dir.join("keep.txt");
        std::fs::write(&file, b"data").unwrap();
        let result = rename_file_at(&file, "keep.txt").unwrap();
        // Same path returned, file untouched.
        assert_eq!(result, file);
        assert!(file.exists());
        cleanup(&dir);
    }

    // ── rename_file_at: validation rejection ────────────────────

    #[test]
    fn rename_rejects_empty_name() {
        let dir = temp_dir("rename_empty");
        let f = dir.join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        let err = rename_file_at(&f, "").unwrap_err();
        assert!(err.to_lowercase().contains("empty"), "got: {err}");
        assert!(f.exists(), "original must not be touched on error");
        cleanup(&dir);
    }

    #[test]
    fn rename_rejects_slash() {
        let dir = temp_dir("rename_slash");
        let f = dir.join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        let err = rename_file_at(&f, "a/b").unwrap_err();
        assert!(err.contains('/'), "got: {err}");
        cleanup(&dir);
    }

    #[test]
    fn rename_rejects_reserved_dot() {
        let dir = temp_dir("rename_dot");
        let f = dir.join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        let err = rename_file_at(&f, ".").unwrap_err();
        assert!(err.contains("reserved"), "got: {err}");
        cleanup(&dir);
    }

    #[test]
    fn rename_rejects_collision_with_existing_file() {
        let dir = temp_dir("rename_collide");
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let err = rename_file_at(&a, "b.txt").unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");
        // Both files still intact.
        assert!(a.exists());
        assert!(b.exists());
        cleanup(&dir);
    }

    #[test]
    fn rename_rejects_system_path() {
        // /usr is in the blocklist — attempting to rename it should fail
        // with a clear message even though the OS would also deny it.
        let err = rename_file_at(std::path::Path::new("/usr"), "usr2").unwrap_err();
        assert!(err.contains("system"), "got: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::FILE_THUMB_MAX_WIDTH;

    /// The media files Import Folder takes from a directory: the walk its
    /// handler uses (#3096).
    fn collect_media_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        klipp_media::folder_walk::media_in_folder(dir, true).unwrap().files.into_iter().map(|f| f.path).collect()
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
        let cache_key = klipp_engine::video_decoder::picture_cache_key(&jpg_path)
            .expect("cache key for real file");
        assert!(
            klipp_media::disk_cache::thumbnail_cache_exists(&cache_key, super::FILE_THUMB_MAX_WIDTH),
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
            rating: 0,
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
            rating: 0,
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

#[cfg(test)]
mod files_row_style_tests {
    use super::*;
    use crate::layout_state::BrowserTab;
    use chrono::TimeZone;

    /// A file's last change reads as a real local time: today's clock
    /// time, yesterday's, a day this year, a day with its year before.
    /// (It read "23 minus hours ago" as a clock time before.)
    #[test]
    fn a_files_time_is_its_real_local_time() {
        let now = chrono::Local.with_ymd_and_hms(2026, 10, 13, 15, 30, 0).unwrap();
        let at = |y, mo, d, h, mi| std::time::SystemTime::from(chrono::Local.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap());
        assert_eq!(format_mtime_at(at(2026, 10, 13, 9, 5), now), "09:05");
        assert_eq!(format_mtime_at(at(2026, 10, 13, 14, 59), now), "14:59", "another time today reads as itself");
        assert_eq!(format_mtime_at(at(2026, 10, 12, 22, 40), now), "Yesterday 22:40");
        assert_eq!(format_mtime_at(at(2026, 3, 3, 8, 0), now), "3 Mar");
        assert_eq!(format_mtime_at(at(2024, 3, 3, 8, 0), now), "3 Mar 2024");
    }

    struct Files {
        ctx: egui::Context,
        panel: BrowserPanel,
        state: AppState,
        dir: std::path::PathBuf,
    }

    impl Files {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("klipp-files-style-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(dir.join("Day 1")).unwrap();
            for f in ["clip.mov", "music.wav", "notes.txt"] {
                std::fs::write(dir.join(f), b"x").unwrap();
            }
            let mut panel = BrowserPanel::default();
            panel.navigate_files_to(dir.clone());
            Self { ctx: egui::Context::default(), panel, state: AppState::default(), dir }
        }

        /// One frame: the texts by their centres, the wide hairlines by
        /// height, and the commands.
        fn frame(&mut self, events: Vec<egui::Event>) -> (Vec<(String, egui::Pos2)>, Vec<f32>, Vec<AppCommand>) {
            let theme = KlippTheme::default();
            let mut tab = BrowserTab::Files;
            let (mut pinned, mut ratio) = (false, 0.5);
            let mut cmds = Vec::new();
            let out = self.ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 900.0))), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| cmds = self.panel.show(ui, &mut self.state, &theme, &mut tab, &mut pinned, &mut ratio));
            });
            let mut texts = Vec::new();
            let mut rules = Vec::new();
            for s in &out.shapes {
                match &s.shape {
                    egui::Shape::Text(t) => texts.push((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                    egui::Shape::LineSegment { points, .. } if points[0].y == points[1].y && points[1].x - points[0].x > 200.0 => rules.push(points[0].y),
                    _ => {}
                }
            }
            (texts, rules, cmds)
        }
    }

    impl Files {
        /// Frames until the folder's entries are listed (the listing may
        /// come from a worker), then that frame.
        fn listed(&mut self) -> (Vec<(String, egui::Pos2)>, Vec<f32>, Vec<AppCommand>) {
            for _ in 0..200 {
                let f = self.frame(Vec::new());
                if f.0.iter().any(|(t, _)| t == "clip.mov") {
                    // One more, so the layout has settled.
                    return self.frame(Vec::new());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("the folder was never listed");
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The Files rows: every entry drawn with its real time, and no rule
    /// between one row and the next.
    #[test]
    fn the_rows_show_real_times_without_rules_between() {
        let mut f = Files::new();
        let before = chrono::Local::now();
        let (texts, rules, _) = f.listed();
        let at = |l: &str| texts.iter().find(|(t, _)| t == l).unwrap_or_else(|| panic!("no {l}: {texts:?}")).1;
        let rows = ["Day 1", "clip.mov", "music.wav", "notes.txt"].map(|r| at(r).y);
        let now = [before.format("%H:%M").to_string(), chrono::Local::now().format("%H:%M").to_string()];
        assert!(texts.iter().any(|(t, _)| now.iter().any(|n| t.ends_with(n.as_str()))), "a file written now shows the time now ({now:?}): {texts:?}");
        let (top, bottom) = (rows[0], rows[3]);
        let between: Vec<&f32> = rules.iter().filter(|r| **r > top && **r < bottom + 20.0).collect();
        assert!(between.is_empty(), "rules between the rows: {between:?}");
    }

    /// A file's menu is the shared menu, and its items do what they did:
    /// Add to Media Pool, clicked for real, loads the file.
    #[test]
    fn a_files_menu_adds_it_to_the_pool() {
        let mut f = Files::new();
        let (texts, _, _) = f.listed();
        let row = texts.iter().find(|(t, _)| t == "clip.mov").unwrap().1;
        let secondary = |pressed| egui::Event::PointerButton { pos: row, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
        f.frame(vec![egui::Event::PointerMoved(row), secondary(true)]);
        f.frame(vec![secondary(false)]);
        let (texts, _, _) = f.frame(Vec::new());
        let item = texts.iter().find(|(t, _)| t == "Add to Media Pool").unwrap_or_else(|| panic!("the menu: {texts:?}")).1;
        let primary = |pressed| egui::Event::PointerButton { pos: item, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = f.frame(vec![egui::Event::PointerMoved(item), primary(true)]).2;
        cmds.extend(f.frame(vec![primary(false)]).2);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::LoadMediaFile { path, .. } if path.ends_with("clip.mov"))), "{cmds:?}");
    }

    /// The media pool opens Find by Picture with Files pinned beside it
    /// and without. (Only the pinned view had the button.)
    #[test]
    fn the_media_pool_opens_find_by_picture_in_both_views() {
        for files_pinned in [false, true] {
            let (ctx, mut panel, mut state, theme) = (egui::Context::default(), BrowserPanel::default(), AppState::default(), KlippTheme::default());
            let mut frame = |events: Vec<egui::Event>| {
                let (mut tab, mut pinned, mut ratio) = (BrowserTab::Media, files_pinned, 0.5);
                let mut cmds = Vec::new();
                let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 900.0))), events, ..Default::default() }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| cmds = panel.show(ui, &mut state, &theme, &mut tab, &mut pinned, &mut ratio));
                });
                let at = out.shapes.iter().find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == "Find by Picture\u{2026}" => Some(t.pos + t.galley.size() / 2.0),
                    _ => None,
                });
                (at, cmds)
            };
            frame(Vec::new());
            let at = frame(Vec::new()).0.unwrap_or_else(|| panic!("no Find by Picture button (Files pinned: {files_pinned})"));
            let primary = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            let mut cmds = frame(vec![egui::Event::PointerMoved(at), primary(true)]).1;
            cmds.extend(frame(vec![primary(false)]).1);
            assert!(cmds.iter().any(|c| matches!(c, AppCommand::OpenVisualSearch)), "Files pinned: {files_pinned}: {cmds:?}");
        }
    }
}

#[cfg(test)]
mod search_field_tests {
    use super::browser_search_field;

    /// #2551: in a narrow panel a click just inside the field's frame,
    /// beside the text, focused nothing, and the typing that followed ran
    /// shortcuts. It focuses the field, and the typing goes into it.
    #[test]
    fn a_click_on_the_fields_padding_types_into_it() {
        let ctx = egui::Context::default();
        let theme = crate::theme::KlippTheme::default();
        let mut state = crate::state::AppState::default();
        let mut query = String::new();
        let mut frame_rect = egui::Rect::NOTHING;
        let mut run = |events: Vec<egui::Event>, query: &mut String, frame_rect: &mut egui::Rect| {
            let input = egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::SidePanel::left("library").exact_width(120.0).resizable(false).show(ctx, |ui| {
                    *frame_rect = ui.vertical(|ui| browser_search_field(ui, &theme, query, "Search", &mut state)).response.rect;
                });
            });
        };
        run(vec![], &mut query, &mut frame_rect);
        run(vec![], &mut query, &mut frame_rect);
        // Just inside the frame's left edge: in its padding, beside the
        // text and farther from it than egui's own reach for a click.
        let at = egui::pos2(frame_rect.left() + 2.0, frame_rect.center().y);
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        run(vec![egui::Event::PointerMoved(at), button(true)], &mut query, &mut frame_rect);
        run(vec![button(false)], &mut query, &mut frame_rect);
        run(vec![], &mut query, &mut frame_rect);
        assert!(ctx.wants_keyboard_input(), "the field has the keys");
        run(vec![egui::Event::Text("v".into())], &mut query, &mut frame_rect);
        assert_eq!(query, "v");
    }
}

#[cfg(test)]
mod keyboard_selection_tests {
    use super::*;

    /// One frame of the Files tab with `keys` pressed (each with its
    /// modifiers).
    fn frame(panel: &mut BrowserPanel, state: &mut AppState, keys: &[(egui::Key, egui::Modifiers)]) {
        let ctx = egui::Context::default();
        let theme = KlippTheme::default();
        // A frame to lay out, then the one the keys arrive in.
        for events in [Vec::new(), keys.iter().map(|&(key, modifiers)| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }).collect()] {
            // The keys' modifiers held, as a keyboard holds them.
            let modifiers = if events.is_empty() { egui::Modifiers::NONE } else { keys.first().map_or(egui::Modifiers::NONE, |k| k.1) };
            let input = egui::RawInput { events, modifiers, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 800.0))), ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (mut tab, mut pinned, mut ratio) = (crate::layout_state::BrowserTab::Files, false, 0.5);
                    let _ = panel.show(ui, state, &theme, &mut tab, &mut pinned, &mut ratio);
                });
            });
        }
    }

    fn names(panel: &BrowserPanel) -> Vec<String> {
        let mut n: Vec<String> = panel.files_selected.iter().filter_map(|p| p.file_name().map(|f| f.to_string_lossy().into_owned())).collect();
        n.sort();
        n
    }

    /// #3019: Shift+arrow extends the selection from its anchor and takes
    /// it back; Ctrl+A selects every file shown, the folder left out; a
    /// plain arrow selects one again.
    #[test]
    fn the_keyboard_selects_several_files() {
        let dir = std::env::temp_dir().join(format!("klipp-files-keys-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for f in ["a.mp4", "b.mp4", "c.mp4", "d.mp4", "e.mp4"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let mut panel = BrowserPanel::default();
        // No watcher re-reading the folder partway through.
        let mut state = AppState { file_browser_auto_refresh: false, ..Default::default() };
        panel.navigate_files_to(dir.clone());
        frame(&mut panel, &mut state, &[]);
        // The folder is read on a worker, which a loaded machine may not
        // have run by now: its answer, while still to come, waited for and
        // taken by a frame.
        if let Some(pending) = panel.files_receiver.take() {
            let read = pending.recv_timeout(std::time::Duration::from_secs(30)).expect("the folder is read");
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(read).unwrap();
            panel.files_receiver = Some(rx);
            frame(&mut panel, &mut state, &[]);
        }
        assert!(panel.files_entries.iter().any(|e| e.name == "b.mp4"), "the folder is listed: {:?}", panel.files_entries.iter().map(|e| &e.name).collect::<Vec<_>>());
        panel.files_panel_focused = true;
        panel.files_selected = [dir.join("b.mp4")].into_iter().collect();
        panel.files_selection_anchor = Some(dir.join("b.mp4"));
        let (shift, ctrl) = (egui::Modifiers::SHIFT, egui::Modifiers::COMMAND);
        frame(&mut panel, &mut state, &[(egui::Key::ArrowDown, shift)]);
        frame(&mut panel, &mut state, &[(egui::Key::ArrowDown, shift)]);
        assert_eq!(names(&panel), ["b.mp4", "c.mp4", "d.mp4"]);
        frame(&mut panel, &mut state, &[(egui::Key::ArrowUp, shift)]);
        assert_eq!(names(&panel), ["b.mp4", "c.mp4"], "taken back a row");
        frame(&mut panel, &mut state, &[(egui::Key::A, ctrl)]);
        assert_eq!(names(&panel), ["a.mp4", "b.mp4", "c.mp4", "d.mp4", "e.mp4"], "every file, not the folder");
        frame(&mut panel, &mut state, &[(egui::Key::ArrowDown, egui::Modifiers::NONE)]);
        assert_eq!(panel.files_selected.len(), 1, "a plain arrow selects one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_keyboard_range_runs_from_the_anchor() {
        assert_eq!(keyboard_selection(Some(2), 5, true), (2, 5));
        assert_eq!(keyboard_selection(Some(4), 1, true), (1, 4));
        assert_eq!(keyboard_selection(Some(2), 5, false), (5, 5));
        assert_eq!(keyboard_selection(None, 3, true), (3, 3));
    }
}
