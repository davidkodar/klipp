use crate::inspector::rows::Switch;
use crate::prefs::Preferences;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, MarkerColor, ProxyResolution};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrefsTab {
    General,
    Ai,
    System,
    Appearance,
    Playback,
    Editing,
    FileBrowser,
    Audio,
    AudioHardware,
    Export,
    MediaCache,
    Color,
    Title,
    Timeline,
    Viewer,
    Proxy,
    ExternalTools,
    Accounts,
    Advanced,
    Keyboard,
    About,
}

/// What the Keyboard section shows for an action the preset gives no key
/// (#3021).
const NO_KEY: &str = "No key in this preset";

pub struct PreferencesPanel {
    /// "Check for updates…" was clicked: About opens and checks.
    pub(crate) show_updates: bool,
    active_tab: PrefsTab,
    /// Action currently being remapped (waiting for key press). (action_name, combo_index)
    remapping: Option<(String, usize)>,
    /// Search filter for keyboard shortcuts
    kb_search: String,
    /// Global preferences search query
    search_query: String,
    /// "Clear all caches" — set when user clicks the button, drives the
    /// confirmation modal. Carries the snapshot of dirs to delete so the
    /// click result is committed even if cache sizes change between the
    /// click and the user pressing Confirm.
    clear_all_confirm: Option<ClearAllConfirm>,
    /// Preferences → AI → Connect an assistant (#625): what was found,
    /// filled and changed on worker threads, since an assistant's own
    /// command takes a second or two.
    assistant: std::sync::Arc<std::sync::Mutex<AssistantConnect>>,
    /// The Connect an assistant section's button asked for the window.
    open_assistant_setup: bool,
    /// Preferences → Accounts (#1894): a Freesound sign-in under way on a
    /// worker thread, and the code a person pastes when the redirect
    /// can't come back.
    accounts: std::sync::Arc<std::sync::Mutex<AccountsState>>,
    freesound_code: String,
}

/// The Accounts page's sign-in, as its worker leaves it (#1894).
#[derive(Default)]
struct AccountsState {
    /// Waiting on the browser's approval.
    waiting: bool,
    /// The last sign-in's or sign-out's failure, said until the next try.
    error: Option<String>,
}

/// `path` with the home directory written `~`, as a person reads it.
fn tilde(path: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(std::path::Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// What the Connect an assistant section says of an assistant that has
/// Klipp only in some projects, named as `projects` (#625): where, and
/// that Connect makes it every project's.
fn projects_only_text(projects: &[String]) -> String {
    let places = match projects {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    format!("Connected only in {places}. Connect adds it for every project.")
}

/// What the Connect an assistant section shows (#625), and the Set Up AI
/// Assistants window, which shows the same (#2649).
#[derive(Default)]
pub(crate) struct AssistantConnect {
    /// Looked for once the section first shows, and on Refresh.
    looked: bool,
    klipp_mcp: Option<std::path::PathBuf>,
    clients: Vec<(crate::assistant_clients::Client, crate::assistant_clients::Registration)>,
    /// What is under way, said while it runs.
    busy: Option<String>,
    /// The last connection check's answer.
    check: Option<Result<String, String>>,
    /// The last Connect or Disconnect that failed, and why.
    error: Option<String>,
    /// Each assistant's own test (#2649): Klipp's status asked through
    /// the klipp-mcp the assistant has registered.
    tests: Vec<(crate::assistant_clients::Client, Result<String, String>)>,
    /// When each assistant last talked to Klipp, by the name it gave
    /// (`klipp_core::assistants_seen`).
    seen: std::collections::BTreeMap<String, u64>,
}

/// How long ago `secs` (Unix seconds) was, in a few words.
fn ago(secs: u64, now: u64) -> String {
    let d = now.saturating_sub(secs);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86_400),
    }
}

struct ClearAllConfirm {
    dirs: Vec<std::path::PathBuf>,
    total_bytes: u64,
}

impl Default for PreferencesPanel {
    fn default() -> Self {
        Self {
            active_tab: PrefsTab::General,
            show_updates: false,
            remapping: None,
            kb_search: String::new(),
            search_query: String::new(),
            clear_all_confirm: None,
            assistant: Default::default(),
            open_assistant_setup: false,
            accounts: Default::default(),
            freesound_code: String::new(),
        }
    }
}

impl PreferencesPanel {
    // ── Design constants ──
    const SIDEBAR_W: f32 = 270.0;

    /// Show the preferences window. Returns (still_open, commands).
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        prefs: &mut Preferences,
        prefs_dirty: &mut bool,
        key_bindings: &mut crate::keybindings::KeyBindings,
        theme_registry: &crate::theme::ThemeRegistry,
        theme: &KlippTheme,
        snap_config: klipp_core::SnapConfig,
    ) -> (bool, Vec<AppCommand>) {
        let mut open = true;
        let mut cmds = Vec::new();

        // Modal backdrop. Paint inside an Area so egui registers it
        // in `area_order` — layers that go through `ctx.layer_painter`
        // directly bypass `area_order` and end up in the *straggler*
        // queue after all registered areas within the same Order,
        // which drew the backdrop ON TOP OF the Preferences window
        // (and dimmed the window itself).
        //
        // With the Area approach: backdrop is registered first, then
        // the Preferences Window is registered. Within Foreground,
        // `wants_to_be_on_top` (set by Window interaction) sorts
        // windows above non-interacted areas — so the Window paints
        // on top of the backdrop. Tooltips (Order::Tooltip) then
        // render above everything.
        //
        // Alpha: the user's `modal_backdrop_alpha` pref defaults to
        // 120 (~47% dim) which is too light to feel modal when the
        // app behind has 4K-bright content. Floor at 200 (~78%) for
        // Preferences specifically.
        let backdrop_alpha = prefs.advanced.modal_backdrop_alpha.max(200);
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new("prefs_modal_backdrop"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                let resp = ui.allocate_rect(screen, egui::Sense::click());
                // `painter_at` bypasses the Ui's incidental clip_rect
                // (which is 0×0 before `allocate_rect` commits) so the
                // fill definitely covers the full screen.
                ui.painter_at(screen).rect_filled(
                    screen,
                    0.0,
                    egui::Color32::from_black_alpha(backdrop_alpha),
                );
                if resp.clicked() { open = false; }
            });

        let win_w = 900.0_f32.min(screen.width() - 40.0);
        let win_h = 650.0_f32.min(screen.height() - 40.0);

        // Fixed-size, movable modal. `.resizable(true)` with
        // `.title_bar(false)` gave broken UX — top edge doubled as
        // a resize handle that collided with the custom title strip
        // ("grab and shrink upward" bug), and side-edge resize
        // flickered because the window couldn't reconcile user drag
        // with auto-fit content. Fixed sizing eliminates all three
        // resize bugs. `movable(true)` keeps drag-to-move active
        // anywhere on the frame so the user can still reposition.
        egui::Window::new("Preferences")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .movable(true)
            .order(egui::Order::Foreground)
            .default_width(win_w)
            .default_height(win_h)
            .default_pos(egui::pos2(screen.center().x - win_w / 2.0, screen.center().y - win_h / 2.0))
            .frame(egui::Frame::window(&ctx.style()).fill(theme.bg_void).rounding(4.0).inner_margin(0.0))
            .show(ctx, |ui| {
                let win_rect = ui.max_rect();

                // ── Title bar ──
                let title_h = 40.0;
                let title_rect = egui::Rect::from_min_size(win_rect.min, egui::vec2(win_rect.width(), title_h));
                ui.painter().rect_filled(title_rect, egui::Rounding { nw: 10.0, ne: 10.0, sw: 0.0, se: 0.0 }, theme.bg_deep);
                // Close button (red circle)
                let close_center = egui::pos2(title_rect.left() + 20.0, title_rect.center().y);
                let close_resp = ui.allocate_rect(egui::Rect::from_center_size(close_center, egui::vec2(16.0, 16.0)), egui::Sense::click());
                ui.painter().circle_filled(close_center, 7.0, theme.marker_error);
                if close_resp.hovered() {
                    // Draw X with two crossing lines instead of a font glyph
                    let s = 3.0; // half-size of the X
                    let stroke = egui::Stroke::new(1.5_f32, egui::Color32::BLACK);
                    ui.painter().line_segment([egui::pos2(close_center.x - s, close_center.y - s), egui::pos2(close_center.x + s, close_center.y + s)], stroke);
                    ui.painter().line_segment([egui::pos2(close_center.x + s, close_center.y - s), egui::pos2(close_center.x - s, close_center.y + s)], stroke);
                }
                if close_resp.clicked() { open = false; }
                // Title text
                ui.painter().text(title_rect.center(), egui::Align2::CENTER_CENTER, "Preferences", egui::FontId::proportional(13.0), theme.text_primary);

                // ── Search bar ──
                let search_h = 36.0;
                let search_rect = egui::Rect::from_min_size(
                    egui::pos2(win_rect.left() + 12.0, title_rect.bottom() + 4.0),
                    egui::vec2(win_rect.width() - 24.0, search_h),
                );
                ui.painter().rect_filled(search_rect, 4.0, theme.bg_panel_alt);
                ui.painter().rect_stroke(search_rect, 4.0, egui::Stroke::new(1.0_f32, theme.bg_hover));
                // Interactive search input — vertically centered in the bar
                let text_h = 18.0;
                let text_rect = egui::Rect::from_min_max(
                    egui::pos2(search_rect.left() + 32.0, search_rect.center().y - text_h / 2.0),
                    egui::pos2(search_rect.right() - 8.0, search_rect.center().y + text_h / 2.0),
                );
                let mut search_ui = ui.new_child(egui::UiBuilder::new().max_rect(text_rect));
                search_ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search_query)
                            .frame(false)
                            .hint_text("Search preferences...")
                            .desired_width(text_rect.width())
                            .text_color(theme.text_primary)
                    );
                });
                // Search icon
                ui.painter().text(
                    egui::pos2(search_rect.left() + 16.0, search_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Q",
                    egui::FontId::proportional(11.0),
                    theme.text_dim,
                );

                // ── Main content area ──
                let content_top = search_rect.bottom() + 8.0;
                let bottom_bar_h = 48.0;
                let content_bottom = win_rect.bottom() - bottom_bar_h;

                // Left sidebar
                let sidebar_rect = egui::Rect::from_min_max(
                    egui::pos2(win_rect.left(), content_top),
                    egui::pos2(win_rect.left() + Self::SIDEBAR_W, content_bottom),
                );

                // Right content
                let content_rect = egui::Rect::from_min_max(
                    egui::pos2(sidebar_rect.right() + 1.0, content_top),
                    egui::pos2(win_rect.right(), content_bottom),
                );

                // Draw sidebar divider
                ui.painter().line_segment(
                    [egui::pos2(sidebar_rect.right(), sidebar_rect.top()), egui::pos2(sidebar_rect.right(), sidebar_rect.bottom())],
                    egui::Stroke::new(1.0_f32, theme.border),
                );

                // ── Sidebar navigation ──
                // Use a child UI whose max_rect is exactly the sidebar area.
                // This constrains the scroll area AND its scrollbar to this region.
                let sidebar_child_rect = sidebar_rect.shrink2(egui::vec2(0.0, 0.0));
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_child_rect), |ui| {
                    ui.set_clip_rect(sidebar_child_rect);
                    ui.set_min_height(sidebar_child_rect.height());
                    egui::ScrollArea::vertical().id_salt("prefs_sidebar").auto_shrink(false).show(ui, |ui| {
                        let searching = !self.search_query.is_empty();

                        // Define sidebar structure: (category, [(tab, icon, label)])
                        let categories: &[(&str, &[(PrefsTab, &str)])] = &[
                            ("PROJECT", &[(PrefsTab::General, "General"), (PrefsTab::Appearance, "Appearance")]),
                            ("PERFORMANCE", &[(PrefsTab::System, "System"), (PrefsTab::Playback, "Playback"), (PrefsTab::MediaCache, "Cache")]),
                            ("EDITING", &[(PrefsTab::Editing, "Editing"), (PrefsTab::Timeline, "Timeline"), (PrefsTab::Viewer, "Viewer")]),
                            ("MEDIA & AUDIO", &[(PrefsTab::FileBrowser, "File Browser"), (PrefsTab::Audio, "Audio"), (PrefsTab::AudioHardware, "Audio HW"), (PrefsTab::Proxy, "Proxy"), (PrefsTab::ExternalTools, "External Tools"), (PrefsTab::Accounts, "Accounts")]),
                            ("COLOUR & EXPORT", &[(PrefsTab::Color, "Colour"), (PrefsTab::Export, "Export")]),
                            ("AI", &[(PrefsTab::Ai, "AI")]),
                            ("INTERFACE", &[(PrefsTab::Title, "Titles"), (PrefsTab::Keyboard, "Keyboard"), (PrefsTab::Advanced, "Advanced")]),
                            ("ABOUT", &[(PrefsTab::About, "About Klipp")]),
                        ];

                        // Auto-switch to first matching tab when search changes
                        if searching {
                            let current_matches = self.tab_matches_search(self.active_tab);
                            if !current_matches {
                                for (_, items) in categories {
                                    for (tab, _) in *items {
                                        if self.tab_matches_search(*tab) {
                                            self.active_tab = *tab;
                                            break;
                                        }
                                    }
                                    if self.tab_matches_search(self.active_tab) { break; }
                                }
                            }
                        }

                        ui.add_space(4.0);
                        for (cat_name, items) in categories {
                            // Filter items for this category
                            let visible: Vec<_> = items.iter().filter(|(tab, _)| self.tab_matches_search(*tab)).collect();
                            if visible.is_empty() { continue; }

                            if !searching { ui.add_space(8.0); }
                            self.sidebar_category(ui, cat_name, theme);
                            for (tab, label) in visible {
                                self.sidebar_item(ui, *tab, label, theme);
                            }
                        }
                    });
                });

                // ── Right content panel ──
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content_rect), |ui| {
                    ui.set_clip_rect(content_rect);
                    ui.set_min_height(content_rect.height());
                    egui::ScrollArea::vertical()
                        .id_salt("prefs_content")
                        .auto_shrink(false)
                        .show(ui, |ui| {
                        ui.add_space(8.0);
                        match self.active_tab {
                            PrefsTab::General => self.show_general(ui, prefs, prefs_dirty, theme),
                            PrefsTab::Playback => self.show_playback(ui, prefs, prefs_dirty, &mut cmds, theme),
                            PrefsTab::Editing => self.show_editing(ui, prefs, prefs_dirty, &mut cmds, theme, snap_config),
                            PrefsTab::FileBrowser => self.show_file_browser(ui, prefs, prefs_dirty, theme),
                            PrefsTab::Audio => self.show_audio(ui, prefs, prefs_dirty, theme),
                            PrefsTab::Export => self.show_export(ui, prefs, prefs_dirty, &mut cmds, theme),
                            PrefsTab::Proxy => self.show_proxy(ui, prefs, prefs_dirty, &mut cmds, theme),
                            PrefsTab::ExternalTools => self.show_external_tools(ui, prefs, prefs_dirty, theme),
                            PrefsTab::Accounts => self.show_accounts(ui, theme),
                            PrefsTab::Ai => self.show_ai(ui, prefs, prefs_dirty, &mut cmds, theme),
                            PrefsTab::System => self.show_system(ui, prefs, prefs_dirty, theme),
                            PrefsTab::Appearance => self.show_appearance(ui, prefs, prefs_dirty, theme_registry, theme),
                            PrefsTab::AudioHardware => self.show_audio_hardware(ui, prefs, prefs_dirty, theme),
                            PrefsTab::MediaCache => self.show_media_cache(ui, prefs, prefs_dirty, &mut cmds, theme),
                            PrefsTab::Color => self.show_color(ui, prefs, prefs_dirty, theme),
                            PrefsTab::Title => self.show_title(ui, prefs, prefs_dirty, theme),
                            PrefsTab::Timeline => self.show_timeline(ui, prefs, prefs_dirty, theme),
                            PrefsTab::Viewer => self.show_viewer(ui, prefs, prefs_dirty, theme),
                            PrefsTab::Advanced => self.show_advanced(ui, prefs, prefs_dirty, theme),
                            PrefsTab::Keyboard => self.show_keyboard(ui, key_bindings, theme),
                            PrefsTab::About => self.show_about(ui, theme),
                        }
                    });
                });

                // ── Bottom bar ──
                // Painted on the Preferences window's own layer
                // (Foreground) and LAST within this show closure —
                // draw order guarantees it covers scrollbar bleed
                // from the content area above without needing a
                // higher layer. The previous Order::Tooltip trick
                // put this bar ABOVE real tooltips (the Skim quality
                // ⓘ tooltip appeared behind the Reset / Done buttons)
                // — always keep UI chrome below the Tooltip layer.
                let bottom_rect = egui::Rect::from_min_max(
                    egui::pos2(win_rect.left(), content_bottom),
                    win_rect.max,
                );
                let bottom_painter = ui.painter().clone();
                bottom_painter.rect_filled(bottom_rect, egui::Rounding { nw: 0.0, ne: 0.0, sw: 10.0, se: 10.0 }, theme.bg_deep);
                // Separator line
                bottom_painter.line_segment(
                    [egui::pos2(bottom_rect.left(), bottom_rect.top()), egui::pos2(bottom_rect.right(), bottom_rect.top())],
                    egui::Stroke::new(1.0_f32, theme.border),
                );
                // "Changes saved automatically" text
                bottom_painter.text(
                    egui::pos2(bottom_rect.left() + 16.0, bottom_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Changes saved automatically",
                    egui::FontId::proportional(11.0),
                    theme.text_dim,
                );
                // Done button (amber)
                let done_rect = egui::Rect::from_min_size(
                    egui::pos2(bottom_rect.right() - 80.0, bottom_rect.top() + 8.0),
                    egui::vec2(66.0, 28.0),
                );
                let done_resp = ui.allocate_rect(done_rect, egui::Sense::click());
                // Both arms were `theme.accent`, so the primary action in
                // this dialog was the one button with no hover feedback.
                // `accent_dim` -> `accent` is how `launch_screen.rs` does an
                // accent button; the neighbouring Reset button uses
                // bg_raised -> bg_hover because it is not the primary action.
                let done_bg = if done_resp.hovered() { theme.accent } else { theme.accent_dim };
                bottom_painter.rect_filled(done_rect, 4.0, done_bg);
                bottom_painter.text(done_rect.center(), egui::Align2::CENTER_CENTER, "Done", egui::FontId::proportional(12.0).clone(), theme.text_on_accent);
                if done_resp.clicked() { open = false; }
                // Reset defaults button
                let reset_rect = egui::Rect::from_min_size(
                    egui::pos2(done_rect.left() - 120.0, bottom_rect.top() + 8.0),
                    egui::vec2(106.0, 28.0),
                );
                let reset_resp = ui.allocate_rect(reset_rect, egui::Sense::click());
                let reset_bg = if reset_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
                bottom_painter.rect_filled(reset_rect, 4.0, reset_bg);
                bottom_painter.rect_stroke(reset_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
                bottom_painter.text(reset_rect.center(), egui::Align2::CENTER_CENTER, "Reset to defaults", egui::FontId::proportional(11.0), theme.text_primary);
                if reset_resp.clicked() {
                    *prefs = Preferences::default();
                    *prefs_dirty = true;
                }
            });

        // Render any pending confirmation modals on top of the prefs window.
        self.show_clear_all_confirm(ctx, &mut cmds, theme);

        (open, cmds)
    }

    /// Searchable keywords for each preferences tab.
    /// Turn to the page named: `cache` or `keyboard`. Another name leaves
    /// the page as it was.
    pub fn open_page(&mut self, page: &str) {
        match page {
            "cache" => self.active_tab = PrefsTab::MediaCache,
            "keyboard" => self.active_tab = PrefsTab::Keyboard,
            "colour" => self.active_tab = PrefsTab::Color,
            _ => return,
        }
        self.search_query.clear();
    }

    fn tab_search_keywords(tab: PrefsTab) -> &'static [&'static str] {
        match tab {
            PrefsTab::General => &["general", "updates", "preview", "auto-save", "backup", "recent projects", "project", "default folder", "inspector", "startup", "launch", "open last"],
            // Both spellings on purpose: these are what a user types
            // into the preferences search, and half the world spells it
            // the other way. The labels are British; the search is not
            // fussy.
            PrefsTab::Appearance => &["appearance", "theme", "ui scale", "accent colour", "accent color", "role colours", "role colors", "colour", "color", "video", "dialogue", "music", "sfx"],
            PrefsTab::Ai => &["ai", "artificial intelligence", "transcription", "transcribe", "whisper", "speech", "subtitles", "captions", "model", "models", "download", "gpu", "cpu", "mcp", "assistant", "opt out", "disable", "enable", "vision", "onnx", "ocr", "screen text", "text on screen", "redact"],
            PrefsTab::System => &["system", "gpu", "hardware", "decode", "threads", "frame queue", "playback resolution", "nvdec", "vaapi", "cuda"],
            PrefsTab::Playback => &["playback", "quality", "background render", "pre-roll", "jkl", "speed", "loop"],
            PrefsTab::MediaCache => &["cache", "thumbnail", "waveform", "render cache", "cache size", "transcode", "clear cache"],
            PrefsTab::Editing => &["editing", "snap", "zoom", "playhead", "magnetic", "timeline", "transition", "duration", "multicam", "angle", "sync", "tracking", "tracker", "search radius"],
            PrefsTab::FileBrowser => &["file browser", "browser", "files", "thumbnail", "thumb", "skim", "skimming", "hover", "sort", "filter", "favorites", "pinned", "folders"],
            PrefsTab::Audio => &["audio", "pan law", "fade", "curve", "loudness", "normalize", "peak hold", "anti-click", "mixing"],
            PrefsTab::AudioHardware => &["audio hardware", "output device", "sample rate", "buffer size", "monitoring", "pipewire", "alsa"],
            PrefsTab::Proxy => &["proxy", "auto-generate", "resolution", "proxy cache", "transcode"],
            PrefsTab::Accounts => &["accounts", "account", "freesound", "sign in", "sign-in", "login", "log in", "sign out", "original", "originals", "sound effects", "oauth"],
            PrefsTab::ExternalTools => &["external tools", "external app", "handoff", "round-trip", "round trip", "gimp", "krita", "audacity", "kdenlive", "editor", "image editor", "audio editor", "video editor"],
            PrefsTab::Color => &["color", "working space", "rec.709", "lut", "calibration", "monitor", "interpolation", "scopes", "vectorscope", "skin angle"],
            PrefsTab::Export => &["export", "encoder", "h.264", "h.265", "prores", "colour space", "color space", "audio sample rate", "overwrite", "parallel render"],
            PrefsTab::Title => &["title", "font", "size", "alignment", "duration", "safe area", "text"],
            PrefsTab::Timeline => &["timeline", "clip colours", "clip colors", "clip gap", "trim handle", "track height", "waveform"],
            PrefsTab::Viewer => &["viewer", "overlay", "scope quality", "false colour", "zebra"],
            PrefsTab::Keyboard => &["keyboard", "shortcuts", "keybindings", "hotkeys", "remap"],
            PrefsTab::Advanced => &["advanced", "toast", "notification", "performance", "debug", "logging", "reset", "clear"],
            PrefsTab::About => &["about", "version", "license", "gpu", "cpu", "memory", "hardware", "technology"],
        }
    }

    /// Check if a tab matches the current search query.
    fn tab_matches_search(&self, tab: PrefsTab) -> bool {
        if self.search_query.is_empty() { return true; }
        let q = self.search_query.to_lowercase();
        Self::tab_search_keywords(tab).iter().any(|kw| kw.contains(&q))
    }

    /// Draw a category header in the sidebar (e.g., "PROJECT", "PERFORMANCE").
    fn sidebar_category(&self, ui: &mut egui::Ui, label: &str, theme: &KlippTheme) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(egui::RichText::new(label).size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
        });
        ui.add_space(2.0);
    }

    /// A section in the list on the left: its name, the selected one on
    /// a raised fill.
    fn sidebar_item(&mut self, ui: &mut egui::Ui, tab: PrefsTab, label: &str, theme: &KlippTheme) {
        let is_active = self.active_tab == tab;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(Self::SIDEBAR_W - 16.0, 26.0), egui::Sense::click());
        let rect = rect.translate(egui::vec2(8.0, 0.0));
        if is_active {
            ui.painter().rect_filled(rect, 4.0, theme.bg_raised);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 4.0, theme.bg_hover);
        }
        let text_color = if is_active { theme.text_primary } else { theme.text_secondary };
        ui.painter().text(
            egui::pos2(rect.left() + 12.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(crate::inspector::rows::TEXT),
            text_color,
        );

        if resp.clicked() {
            // On Audio HW tab activation, re-poll cpal so hot-plugged
            // devices appear without requiring a restart. cpal has no
            // device-change push API on any platform — re-enumerating
            // when the user explicitly opens this tab is the closest
            // thing to "automatic" we can offer without pulling in
            // libudev / CoreAudio listeners / MMNotificationClient.
            if tab == PrefsTab::AudioHardware && self.active_tab != PrefsTab::AudioHardware {
                klipp_engine::audio_output::refresh_audio_devices_cache();
            }
            self.active_tab = tab;
        }
    }

    /// A section's header: its name, with a hairline below, as the
    /// inspector's sections have.
    fn section_header(ui: &mut egui::Ui, label: &str, theme: &KlippTheme) {
        ui.add_space(12.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), crate::widgets::inspector_group::SECTION_HEADER_H), egui::Sense::hover());
        ui.painter().text(
            egui::pos2(rect.left() + crate::inspector::rows::ROW_PAD, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(crate::theme::type_scale::CONTROL),
            theme.text_primary,
        );
        ui.painter().hline(rect.x_range(), rect.max.y - 0.5, egui::Stroke::new(1.0_f32, theme.border));
        ui.add_space(4.0);
    }

    /// Draw a setting row with label on left and a widget on right (returned via closure).
    fn setting_row(ui: &mut egui::Ui, label: &str, theme: &KlippTheme, add_right: impl FnOnce(&mut egui::Ui)) {
        Self::setting_row_inner(ui, label, None, theme, add_right);
    }

    fn setting_row_with_subtitle(ui: &mut egui::Ui, label: &str, subtitle: &str, theme: &KlippTheme, add_right: impl FnOnce(&mut egui::Ui)) {
        Self::setting_row_inner(ui, label, Some(subtitle), theme, add_right);
    }

    fn setting_row_inner(ui: &mut egui::Ui, label: &str, subtitle: Option<&str>, theme: &KlippTheme, add_right: impl FnOnce(&mut egui::Ui)) {
        // The label on the left in the secondary colour, the control on
        // the right, no rule between rows; a description, when there is
        // one, wraps under the label across the row, up to three lines.
        use crate::inspector::rows::{ROW_H, ROW_PAD, TEXT};
        const SUBTITLE_LINE_H: f32 = 14.0;
        const MAX_SUBTITLE_ROWS: usize = 3;
        const WIDGET_AREA_W: f32 = 250.0;

        let subtitle_galley = subtitle.map(|sub| {
            let mut job = egui::text::LayoutJob::default();
            job.append(sub, 0.0, egui::text::TextFormat { font_id: egui::FontId::proportional(11.0), color: theme.text_dim, ..Default::default() });
            job.wrap = egui::text::TextWrapping {
                max_width: (ui.available_width() - ROW_PAD * 2.0).max(0.0),
                max_rows: MAX_SUBTITLE_ROWS,
                break_anywhere: false,
                overflow_character: Some('\u{2026}'),
            };
            ui.ctx().fonts(|f| f.layout_job(job))
        });
        let sub_h = subtitle_galley.as_ref().map_or(0.0, |g| g.rows.len().max(1) as f32 * SUBTITLE_LINE_H + 4.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H + sub_h), egui::Sense::hover());
        let top = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), ROW_H));
        ui.painter().text(
            egui::pos2(rect.left() + ROW_PAD, top.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(TEXT),
            theme.text_secondary,
        );
        if let Some(g) = subtitle_galley {
            ui.painter().galley(egui::pos2(rect.left() + ROW_PAD, top.bottom()), g, theme.text_dim);
        }
        let widget_rect = egui::Rect::from_min_max(
            egui::pos2(rect.right() - WIDGET_AREA_W, top.top() + 1.0),
            egui::pos2(rect.right() - ROW_PAD, top.bottom() - 1.0),
        );
        let mut widget_ui = ui.new_child(egui::UiBuilder::new().max_rect(widget_rect));
        widget_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            add_right(ui);
        });
    }

    fn show_general(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
        theme: &KlippTheme,
    ) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("General").strong().size(16.0).color(theme.text_primary));
        });

        Self::section_header(ui, "UPDATES", theme);
        Self::setting_row_with_subtitle(ui, "Check automatically", "Checks GitHub for new releases once a day", theme, |ui| {
            if ui.add(Switch::new(&mut prefs.general.check_for_updates, theme)).changed() {
                prefs.general.update_checks_answered = true;
                *dirty = true;
            }
        });
        Self::setting_row(ui, "Include preview releases", theme, |ui| {
            if crate::updates::preview_switch(&mut prefs.general, ui, |ui, on| ui.add(Switch::new(on, theme))) { *dirty = true; }
        });
        Self::setting_row(ui, "Installed version", theme, |ui| {
            ui.label(klipp_core::build_info::get().version);
            if ui.button("Check for updates…").clicked() { self.show_updates = true; }
        });

        // ── STARTUP section ──
        Self::section_header(ui, "STARTUP", theme);
        // The Welcome window's own "Show at startup" (#2933), the same
        // setting, and the small-window note armed again (#2944).
        Self::setting_row_with_subtitle(ui, "Show welcome at startup", "The keyboard layout, a short tour, editing with an AI assistant", theme, |ui| {
            if ui.add(Switch::new(&mut prefs.general.show_welcome, theme)).changed() {
                *dirty = true;
            }
        });
        Self::setting_row_with_subtitle(ui, "Note a smaller interface", "Say so when Auto makes the interface smaller for a small window", theme, |ui| {
            let mut on = !prefs.appearance.small_screen_note_shown;
            if ui.add(Switch::new(&mut on, theme)).changed() {
                prefs.appearance.small_screen_note_shown = !on;
                *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "On launch", "What to show when Klipp starts", theme, |ui| {
            use crate::prefs::StartupBehavior;
            egui::ComboBox::from_id_salt("startup_behavior")
                .selected_text(match prefs.general.startup_behavior {
                    StartupBehavior::ProjectManager => "Project Manager",
                    StartupBehavior::EmptyProject => "Empty project",
                    StartupBehavior::LastProject => "Reopen last project",
                })
                .width(150.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.general.startup_behavior, StartupBehavior::ProjectManager, "Project Manager").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.general.startup_behavior, StartupBehavior::EmptyProject, "Empty project").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.general.startup_behavior, StartupBehavior::LastProject, "Reopen last project").changed() { *dirty = true; }
                });
        });

        // ── PROJECT section ──
        Self::section_header(ui, "PROJECT", theme);

        Self::setting_row(ui, "Auto-save interval", theme, |ui| {
            let intervals = [
                (0u32, "Off"),
                (60, "Every 1 minute"),
                (300, "Every 5 minutes"),
                (600, "Every 10 minutes"),
                (1800, "Every 30 minutes"),
            ];
            let current_label = intervals.iter()
                .find(|(v, _)| *v == prefs.general.auto_save_interval_secs)
                .map(|(_, l)| *l).unwrap_or("Custom");
            egui::ComboBox::from_id_salt("autosave_interval")
                .selected_text(current_label)
                .show_ui(ui, |ui| {
                    for (val, label) in &intervals {
                        if ui.selectable_value(&mut prefs.general.auto_save_interval_secs, *val, *label).changed() {
                            *dirty = true;
                        }
                    }
                });
        });

        Self::setting_row_with_subtitle(ui, "Also auto-save every N operations", "0 = off", theme, |ui| {
            let mut v = prefs.general.auto_save_op_threshold;
            if ui.add(egui::DragValue::new(&mut v).range(0..=1000).speed(0.5)).changed() {
                prefs.general.auto_save_op_threshold = v;
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Backup versions to keep", theme, |ui| {
            let mut v = prefs.editing.backup_count;
            if ui.add(egui::DragValue::new(&mut v).range(0..=20).speed(0.5)).changed() {
                prefs.editing.backup_count = v;
                *dirty = true;
            }
        });

        // Where new projects go (#1864): ~/Videos until one is chosen.
        Self::setting_row(ui, "Projects root", theme, |ui| {
            let picker = egui::Id::new("prefs-default-project-dir");
            if ui.button("Browse...").clicked() {
                crate::file_dialog::pick_path(picker, || {
                    rfd::FileDialog::new().set_title("Projects Root").pick_folder()
                });
            }
            if let Some(crate::file_dialog::Picked::Path(path)) = crate::file_dialog::take_path(picker) {
                prefs.general.default_project_dir = Some(path);
                *dirty = true;
            }
            let dir_text = prefs.general.default_project_dir.as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "~/Videos".into());
            ui.label(egui::RichText::new(&dir_text).size(11.0).color(theme.text_dim));
        });

        // ── INSPECTOR section ──
        Self::section_header(ui, "INSPECTOR", theme);

        Self::setting_row(ui, "Remember collapsed sections", theme, |ui| {
            if ui.add(Switch::new(&mut prefs.general.remember_inspector_layout, theme)).changed() {
                *dirty = true;
            }
        });

    }

    fn show_playback(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Playback").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── QUALITY section ──
        Self::section_header(ui, "QUALITY", theme);

        Self::setting_row_with_subtitle(
            ui,
            "Playback quality",
            "Render the viewer at a half or a quarter of its size while playing, for heavy timelines. Paused frames and exports stay full size",
            theme,
            |ui| {
                use klipp_core::PlaybackQuality;
                let label = |q: PlaybackQuality| match q {
                    PlaybackQuality::Full => "Full",
                    PlaybackQuality::Half => "Half",
                    PlaybackQuality::Quarter => "Quarter",
                };
                egui::ComboBox::from_id_salt("playback_quality")
                    .selected_text(label(prefs.playback.playback_quality))
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for q in [PlaybackQuality::Full, PlaybackQuality::Half, PlaybackQuality::Quarter] {
                            if ui.selectable_value(&mut prefs.playback.playback_quality, q, label(q)).changed() {
                                *dirty = true;
                            }
                        }
                    });
            },
        );

        Self::setting_row_with_subtitle(ui, "Background render", "Pre-render complex sections in background", theme, |ui| {
            let mut val = prefs.playback.background_render;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.playback.background_render = val;
                *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(
            ui,
            "Background performance",
            "How background work yields when you play / scrub / interact",
            theme,
            |ui| {
                use crate::prefs::BackgroundPerformance;
                let current = match prefs.playback.background_performance {
                    BackgroundPerformance::Defer => "Defer",
                    BackgroundPerformance::Balanced => "Balanced",
                    BackgroundPerformance::MaxThroughput => "Max throughput",
                };
                egui::ComboBox::from_id_salt("background_perf")
                    .selected_text(current)
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for (val, label) in [
                            (BackgroundPerformance::Defer, "Defer (recommended)"),
                            (BackgroundPerformance::Balanced, "Balanced"),
                            (BackgroundPerformance::MaxThroughput, "Max throughput"),
                        ] {
                            if ui.selectable_value(
                                &mut prefs.playback.background_performance,
                                val,
                                label,
                            ).changed() {
                                *dirty = true;
                            }
                        }
                    });
            },
        );

        Self::setting_row_with_subtitle(
            ui,
            "Optimised playback",
            "Use the lighter skim file for playback when source media is too heavy",
            theme,
            |ui| {
                use crate::prefs::OptimisedPlayback;
                let current = match prefs.playback.optimised_playback {
                    OptimisedPlayback::Auto => "Auto",
                    OptimisedPlayback::AlwaysOriginal => "Always original",
                    OptimisedPlayback::AlwaysOptimized => "Always optimised",
                };
                egui::ComboBox::from_id_salt("optimised_playback")
                    .selected_text(current)
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for (val, label) in [
                            (OptimisedPlayback::Auto, "Auto (recommended)"),
                            (OptimisedPlayback::AlwaysOriginal, "Always original"),
                            (OptimisedPlayback::AlwaysOptimized, "Always optimised"),
                        ] {
                            if ui.selectable_value(
                                &mut prefs.playback.optimised_playback,
                                val,
                                label,
                            ).changed() {
                                *dirty = true;
                            }
                        }
                    });
            },
        );

        // ── TRANSPORT section ──
        Self::section_header(ui, "TRANSPORT", theme);

        Self::setting_row(ui, "Pre-roll duration", theme, |ui| {
            let mut v = prefs.playback.pre_roll_duration;
            if ui.add(egui::DragValue::new(&mut v).range(0.0..=10.0).speed(0.1).suffix(" s").fixed_decimals(1)).changed() {
                prefs.playback.pre_roll_duration = v;
                *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "JKL speed steps", "Each further J or L press multiplies the speed by this, three times at most", theme, |ui| {
            let steps = [(2.0_f32, "2\u{00d7}"), (3.0, "3\u{00d7}"), (4.0, "4\u{00d7}")];
            let current = steps.iter()
                .find(|(v, _)| (*v - prefs.playback.jkl_speed_step).abs() < 0.01)
                .map(|(_, l)| *l).unwrap_or("2\u{00d7}");
            egui::ComboBox::from_id_salt("jkl_step")
                .selected_text(current)
                .width(50.0)
                .show_ui(ui, |ui| {
                    for (val, label) in &steps {
                        if ui.selectable_value(&mut prefs.playback.jkl_speed_step, *val, *label).changed() {
                            *dirty = true;
                        }
                    }
                });
        });

        Self::setting_row(ui, "Loop playback by default", theme, |ui| {
            let mut val = prefs.playback.loop_playback;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.playback.loop_playback = val;
                *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(
            ui,
            "Dim UI during playback",
            "Darkens the panels around the viewer for cinematic focus",
            theme,
            |ui| {
                let mut val = prefs.playback.dim_ui_on_play;
                if ui.add(Switch::new(&mut val, theme)).changed() {
                    prefs.playback.dim_ui_on_play = val;
                    *dirty = true;
                }
            },
        );

        Self::setting_row_with_subtitle(
            ui,
            "Audio scrubbing",
            "Play audio while dragging the playhead",
            theme,
            |ui| {
                let mut val = prefs.playback.scrub_audio;
                if ui.add(Switch::new(&mut val, theme)).changed() {
                    prefs.playback.scrub_audio = val;
                    *dirty = true;
                    cmds.push(AppCommand::SetScrubAudioEnabled(val));
                }
            },
        );
    }

    fn show_editing(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
        snap_config: klipp_core::SnapConfig,
    ) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Editing").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── TIMELINE BEHAVIOR section ──
        Self::section_header(ui, "TIMELINE BEHAVIOR", theme);

        Self::setting_row_with_subtitle(ui, "Snap threshold", "Distance in pixels before snapping", theme, |ui| {
            let mut v = prefs.editing.snap_threshold_px;
            if ui.add(egui::DragValue::new(&mut v).range(2.0..=30.0).speed(0.5).suffix(" px").fixed_decimals(0)).changed() {
                // Dispatch through AppCommand so the handler also
                // mirrors to state.snap_threshold_px (consulted live
                // by the snap-resolution layer via the per-paint
                // sync_editing_prefs_to_timeline at panels/timeline.rs:1060).
                prefs.editing.snap_threshold_px = v;
                *dirty = true;
                cmds.push(AppCommand::SetSnapThresholdPx(v));
            }
        });

        Self::setting_row_with_subtitle(ui, "Grid snap", "Snap dragged clip edges to a time grid (Off keeps current behaviour)", theme, |ui| {
            use klipp_core::GridInterval;
            let cur = prefs.editing.grid_snap_interval;
            let cur_label = match cur {
                GridInterval::Custom(_) => "Custom",
                _ => cur.label(),
            };
            // Custom variant is keyed by tag; the seconds value lives
            // in a separate DragValue rendered next to the combo.
            const PRESETS: &[GridInterval] = &[
                GridInterval::Off,
                GridInterval::Adaptive,
                GridInterval::Frame,
                GridInterval::HalfSecond,
                GridInterval::Second,
                GridInterval::TwoSeconds,
                GridInterval::FiveSeconds,
                GridInterval::Custom(0.25),
            ];
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("grid_snap_interval")
                    .selected_text(cur_label)
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for &preset in PRESETS {
                            let label = preset.label();
                            // Selection compares by tag for Custom so picking
                            // "Custom" doesn't reset the user's seconds value.
                            let is_selected = matches!(
                                (preset, cur),
                                (GridInterval::Off, GridInterval::Off)
                                    | (GridInterval::Adaptive, GridInterval::Adaptive)
                                    | (GridInterval::Frame, GridInterval::Frame)
                                    | (GridInterval::HalfSecond, GridInterval::HalfSecond)
                                    | (GridInterval::Second, GridInterval::Second)
                                    | (GridInterval::TwoSeconds, GridInterval::TwoSeconds)
                                    | (GridInterval::FiveSeconds, GridInterval::FiveSeconds)
                                    | (GridInterval::Custom(_), GridInterval::Custom(_))
                            );
                            if ui.selectable_label(is_selected, label).clicked() {
                                let new_val = match preset {
                                    GridInterval::Custom(_) => match cur {
                                        // Preserve existing custom value.
                                        GridInterval::Custom(secs) => GridInterval::Custom(secs),
                                        _ => GridInterval::Custom(0.25),
                                    },
                                    other => other,
                                };
                                if new_val != cur {
                                    // Dispatch through AppCommand so the
                                    // handler also mirrors to state.grid_
                                    // snap_interval (consulted live by
                                    // the snap-resolution layer). Direct
                                    // mutation here only would require a
                                    // restart for the change to take
                                    // effect, since the prefs→state mirror
                                    // at app.rs:570 only fires at startup.
                                    prefs.editing.grid_snap_interval = new_val;
                                    *dirty = true;
                                    cmds.push(AppCommand::SetGridInterval(new_val));
                                }
                            }
                        }
                    });
                if let GridInterval::Custom(secs) = prefs.editing.grid_snap_interval {
                    let mut v = secs;
                    if ui.add(egui::DragValue::new(&mut v).range(0.001..=60.0).speed(0.05).suffix(" s").fixed_decimals(3)).changed() {
                        let new_val = GridInterval::Custom(v.max(0.001));
                        prefs.editing.grid_snap_interval = new_val;
                        *dirty = true;
                        cmds.push(AppCommand::SetGridInterval(new_val));
                    }
                }
            });
        });

        Self::setting_row_with_subtitle(
            ui,
            "Adapt timeline to window",
            "Keep the timeline fitted when the window or layout size changes",
            theme,
            |ui| {
                let mut val = prefs.editing.adaptive_timeline_fit;
                if ui.add(Switch::new(&mut val, theme)).changed() {
                    prefs.editing.adaptive_timeline_fit = val;
                    *dirty = true;
                }
            },
        );

        // Per-target snap toggles. Mirror the right-click context
        // menu on the timeline-toolbar Snap icon and the ruler context
        // menu — both surfaces share the same SnapConfig, so this
        // pane is just a third view of the same state. Each row
        // dispatches AppCommand::SetSnapMaster / SetSnapTarget; the
        // central handler is the single mutation path (no direct
        // writes from this pane). SnapConfig itself lives on
        // LayoutState (not prefs.editing), so the rows are read-only
        // mirrors of `state.snap` until a row is clicked.
        Self::setting_row_with_subtitle(ui, "Snap (master)", "Disables every per-target snap below (also bound to N)", theme, |ui| {
            let mut val = snap_config.master;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                cmds.push(AppCommand::SetSnapMaster(val));
            }
        });
        // Per-target rows: greyed when master is off, since the
        // booleans only have effect with master on.
        ui.add_enabled_ui(snap_config.master, |ui| {
            use klipp_core::SnapTargetKind;
            let rows: &[(SnapTargetKind, &str, &str, bool)] = &[
                (SnapTargetKind::ClipEdge, "Snap to clip edges", "Across all tracks", snap_config.clips),
                (SnapTargetKind::Playhead, "Snap to playhead", "Match the current playhead position", snap_config.playhead),
                (SnapTargetKind::Marker, "Snap to markers", "User-placed and range markers", snap_config.markers),
                (SnapTargetKind::InPoint, "Snap to in/out", "Sequence in / out / start (t=0)", snap_config.in_out),
                (SnapTargetKind::Grid, "Snap to grid", "Uses the Grid snap interval above", snap_config.grid),
            ];
            for &(target, label, subtitle, enabled) in rows {
                Self::setting_row_with_subtitle(ui, label, subtitle, theme, |ui| {
                    let mut val = enabled;
                    if ui.add(Switch::new(&mut val, theme)).changed() {
                        cmds.push(AppCommand::SetSnapTarget { target, enabled: val });
                    }
                });
            }
        });

        Self::setting_row(ui, "Zoom step multiplier", theme, |ui| {
            let steps = [(1.25_f32, "1.25\u{00d7}"), (1.5, "1.5\u{00d7}"), (2.0, "2\u{00d7}")];
            let cur = prefs.editing.zoom_step;
            let current = steps.iter()
                .find(|(v, _)| (*v - cur).abs() < 0.01)
                .map(|(_, l)| *l).unwrap_or("1.5\u{00d7}");
            egui::ComboBox::from_id_salt("zoom_step")
                .selected_text(current)
                .width(55.0)
                .show_ui(ui, |ui| {
                    for (val, label) in &steps {
                        if ui.selectable_label((*val - cur).abs() < 0.01, *label).clicked()
                            && (*val - cur).abs() >= 0.01
                        {
                            // Dispatch through AppCommand for live-sync to state.zoom_step.
                            prefs.editing.zoom_step = *val;
                            *dirty = true;
                            cmds.push(AppCommand::SetZoomStep(*val));
                        }
                    }
                });
        });

        Self::setting_row(ui, "Playhead follows playback", theme, |ui| {
            let mut val = prefs.editing.playhead_follows_playback;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.editing.playhead_follows_playback = val;
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Magnetic timeline", theme, |ui| {
            let mut val = prefs.editing.magnetic_timeline;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                // Dispatch through AppCommand: bridges the
                // magnetic_timeline (pref) ↔ magnetic_timeline_enabled
                // (state) naming asymmetry. Without this, mid-session
                // changes only propagated via the debounced 1s
                // re-sync block in app.rs.
                prefs.editing.magnetic_timeline = val;
                *dirty = true;
                cmds.push(AppCommand::SetMagneticTimeline(val));
            }
        });

        // ── DEFAULTS section ──
        Self::section_header(ui, "DEFAULTS", theme);

        Self::setting_row_with_subtitle(ui, "New sequence resolution", "Used when no clip is available to inherit from", theme, |ui| {
            use crate::panels::new_project_dialog::RESOLUTION_PRESETS;
            let idx = prefs.editing.default_sequence_resolution_idx.min(RESOLUTION_PRESETS.len() - 1);
            let label = RESOLUTION_PRESETS[idx].0;
            egui::ComboBox::from_id_salt("default_seq_res")
                .selected_text(label)
                .width(220.0)
                .show_ui(ui, |ui| {
                    for (i, (name, _, _)) in RESOLUTION_PRESETS.iter().enumerate() {
                        // Hide "Custom..." — preset must be a concrete size.
                        if *name == "Custom..." { continue; }
                        if ui.selectable_label(prefs.editing.default_sequence_resolution_idx == i, *name).clicked() {
                            // Dispatch through AppCommand for live-sync.
                            // Read at "+ new sequence" time (timeline/
                            // sequence_tabs.rs:182) — mid-session
                            // changes need to propagate so the very
                            // next "+" click picks up the new default.
                            prefs.editing.default_sequence_resolution_idx = i;
                            *dirty = true;
                            cmds.push(AppCommand::SetDefaultSequenceResolutionIdx(i));
                        }
                    }
                });
        });

        Self::setting_row(ui, "Default transition", theme, |ui| {
            // Currently only Cross Dissolve is implemented
            let transitions = ["Cross Dissolve", "Dip to Black", "Wipe"];
            let mut selected = 0_usize; // Cross Dissolve
            egui::ComboBox::from_id_salt("default_trans")
                .selected_text(transitions[selected])
                .width(120.0)
                .show_ui(ui, |ui| {
                    for (i, label) in transitions.iter().enumerate() {
                        if ui.selectable_value(&mut selected, i, *label).changed() {
                            *dirty = true;
                        }
                    }
                });
        });

        Self::setting_row(ui, "Default transition duration", theme, |ui| {
            let mut v = prefs.editing.default_transition_frames;
            if ui.add(egui::DragValue::new(&mut v).range(1..=120).speed(1.0).suffix(" fr")).changed() {
                // Dispatch through AppCommand for live-sync to
                // state.default_transition_frames (read by transition
                // application sites in timeline.rs and transitions_tab.rs).
                prefs.editing.default_transition_frames = v;
                *dirty = true;
                cmds.push(AppCommand::SetDefaultTransitionFrames(v));
            }
        });

        // How far Shift+Left and Shift+Right move the playhead (#921):
        // one second, or a number of frames.
        Self::setting_row(ui, "Shift+arrow step", theme, |ui| {
            let mut v = prefs.editing.shift_step_frames;
            let drag = egui::DragValue::new(&mut v)
                .range(0..=600)
                .speed(1.0)
                .custom_formatter(|n, _| if n < 0.5 { "1 second".to_string() } else { format!("{n:.0} fr") });
            if ui.add(drag).on_hover_text("0 moves one second, whatever the frame rate").changed() {
                prefs.editing.shift_step_frames = v;
                *dirty = true;
                cmds.push(AppCommand::SetShiftStepFrames(v));
            }
        });

        // ── MULTICAM section ──
        Self::section_header(ui, "MULTICAM", theme);

        Self::setting_row(ui, "Show angle badges on timeline", theme, |ui| {
            let mut val = prefs.editing.multicam_show_angles;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                // Dispatch through AppCommand for live-sync. Was
                // covered by the debounced 1s re-sync block before
                // T-PREFS-LIVE-SYNC; now applies immediately.
                prefs.editing.multicam_show_angles = val;
                *dirty = true;
                cmds.push(AppCommand::SetMulticamShowAngles(val));
            }
        });

        Self::setting_row_with_subtitle(ui, "Default sync method", "How Create Synced Multicam puts the angles on one clock", theme, |ui| {
            use crate::prefs::MulticamSyncMethod;
            let label = |m: MulticamSyncMethod| match m {
                MulticamSyncMethod::Manual => "Timecode, then sound",
                MulticamSyncMethod::Timecode => "Timecode",
                MulticamSyncMethod::AudioWaveform => "Sound",
            };
            egui::ComboBox::from_id_salt("multicam_sync")
                .selected_text(label(prefs.editing.multicam_sync_method))
                .width(150.0)
                .show_ui(ui, |ui| {
                    for m in [MulticamSyncMethod::Manual, MulticamSyncMethod::Timecode, MulticamSyncMethod::AudioWaveform] {
                        if ui.selectable_value(&mut prefs.editing.multicam_sync_method, m, label(m)).changed() { *dirty = true; }
                    }
                });
        });

        // ── TRACKING section ──
        Self::section_header(ui, "TRACKING", theme);

        Self::setting_row_with_subtitle(ui, "Search radius", "How far a new track point looks for its match, in pixels", theme, |ui| {
            let mut v = prefs.editing.track_search_radius;
            if ui.add(egui::DragValue::new(&mut v).range(8..=128).speed(1.0).suffix(" px")).changed() {
                prefs.editing.track_search_radius = v; *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "Patch size", "Half-width of matching patch", theme, |ui| {
            let mut v = prefs.editing.track_patch_size;
            if ui.add(egui::DragValue::new(&mut v).range(2..=16).speed(1.0).suffix(" px")).changed() {
                prefs.editing.track_patch_size = v; *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "Loss threshold", "SAD threshold for tracking loss", theme, |ui| {
            let mut v = prefs.editing.track_loss_threshold;
            if ui.add(egui::DragValue::new(&mut v).range(5..=100).speed(1.0)).changed() {
                prefs.editing.track_loss_threshold = v; *dirty = true;
            }
        });

        // ── SCENE DETECTION section ──
        Self::section_header(ui, "SCENE DETECTION", theme);

        Self::setting_row_with_subtitle(ui, "Sensitivity",
            "Higher = more sensitive, more false positives", theme, |ui| {
            let mut v = prefs.editing.scene_detect_sensitivity;
            if ui.add(egui::Slider::new(&mut v, 0.1..=1.0).step_by(0.05).fixed_decimals(2)).changed() {
                prefs.editing.scene_detect_sensitivity = v; *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "Minimum gap",
            "Minimum seconds between detected cuts", theme, |ui| {
            let mut v = prefs.editing.scene_detect_min_gap_secs;
            if ui.add(egui::Slider::new(&mut v, 0.1..=5.0).step_by(0.1).suffix(" s").fixed_decimals(1)).changed() {
                prefs.editing.scene_detect_min_gap_secs = v; *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "Marker colour",
            "Colour applied to markers from scene detection", theme, |ui| {
            let current = prefs.editing.scene_detect_marker_color;
            egui::ComboBox::from_id_salt("scene_detect_marker_color")
                .selected_text(current.label())
                .width(130.0)
                .show_ui(ui, |ui| {
                    for &c in MarkerColor::all() {
                        if ui.selectable_value(&mut prefs.editing.scene_detect_marker_color, c, c.label()).changed() {
                            *dirty = true;
                        }
                    }
                });
        });

        let _ = cmds; // suppress unused warning
    }

    fn show_file_browser(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
        theme: &KlippTheme,
    ) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(
                egui::RichText::new("File Browser")
                    .strong()
                    .size(16.0)
                    .color(theme.text_primary),
            );
        });
        ui.add_space(4.0);

        // ── HOVER SKIM section ──
        // Hover-skim previews are great for fast file triage but cost
        // CPU/IO and (for audio) can be distracting. Both default on but
        // can be turned off independently for users who prefer a quieter
        // browser or who work on slower hardware.
        Self::section_header(ui, "HOVER SKIM", theme);

        Self::setting_row_with_subtitle(
            ui,
            "Video hover skim",
            "Decode video frames as you hover over file rows",
            theme,
            |ui| {
                let mut val = prefs.file_browser.file_browser_video_skim;
                if ui.add(Switch::new(&mut val, theme)).changed() {
                    prefs.file_browser.file_browser_video_skim = val;
                    *dirty = true;
                }
            },
        );

        Self::setting_row_with_subtitle(
            ui,
            "Audio hover skim",
            "Play audio as you hover over audio file rows",
            theme,
            |ui| {
                let mut val = prefs.file_browser.file_browser_audio_skim;
                if ui.add(Switch::new(&mut val, theme)).changed() {
                    prefs.file_browser.file_browser_audio_skim = val;
                    *dirty = true;
                }
            },
        );

        Self::setting_row_with_subtitle(
            ui,
            "Filmstrip hover preview",
            "Show multi-frame filmstrip with large popup on video hover",
            theme,
            |ui| {
                let mut val = prefs.file_browser.filmstrip_hover;
                if ui.add(Switch::new(&mut val, theme)).changed() {
                    prefs.file_browser.filmstrip_hover = val;
                    *dirty = true;
                }
            },
        );

        Self::setting_row_with_subtitle(
            ui,
            "Filmstrip quality",
            "Higher = sharper preview, slower first-hover decode",
            theme,
            |ui| {
                use crate::prefs::FilmstripQuality;
                egui::ComboBox::from_id_salt("filmstrip_q")
                    .selected_text(prefs.file_browser.filmstrip_quality.label())
                    .width(80.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_value(&mut prefs.file_browser.filmstrip_quality, FilmstripQuality::Low, "Low").changed() { *dirty = true; }
                        if ui.selectable_value(&mut prefs.file_browser.filmstrip_quality, FilmstripQuality::Medium, "Medium").changed() { *dirty = true; }
                        if ui.selectable_value(&mut prefs.file_browser.filmstrip_quality, FilmstripQuality::High, "High").changed() { *dirty = true; }
                    });
            },
        );

        Self::setting_row_with_subtitle(
            ui,
            "Keep filmstrip visible",
            "Show filmstrip tiles even after cursor leaves (popup still requires hover)",
            theme,
            |ui| {
                let mut val = prefs.file_browser.filmstrip_persist;
                if ui.add(Switch::new(&mut val, theme)).changed() {
                    prefs.file_browser.filmstrip_persist = val;
                    *dirty = true;
                }
            },
        );

        Self::setting_row_with_subtitle(
            ui,
            "Group files by date",
            "Show Today / Yesterday / This Week / This Month / Older headers",
            theme,
            |ui| {
                let mut val = prefs.file_browser.group_by_date;
                if ui.add(Switch::new(&mut val, theme)).changed() {
                    prefs.file_browser.group_by_date = val;
                    *dirty = true;
                }
            },
        );

        Self::setting_row_with_subtitle(
            ui,
            "Auto-refresh folder",
            "Refresh file list when files change externally (inotify)",
            theme,
            |ui| {
                let mut val = prefs.file_browser.auto_refresh;
                if ui.add(Switch::new(&mut val, theme)).changed() {
                    prefs.file_browser.auto_refresh = val;
                    *dirty = true;
                }
            },
        );

        Self::setting_row_with_subtitle(
            ui,
            "Show recent files",
            "Show the Recents button in the file browser sidebar",
            theme,
            |ui| {
                let mut val = prefs.file_browser.show_recent_files;
                if ui.add(Switch::new(&mut val, theme)).changed() {
                    prefs.file_browser.show_recent_files = val;
                    *dirty = true;
                }
            },
        );

        // ── COLUMNS section ──
        Self::section_header(ui, "COLUMNS", theme);

        Self::setting_row_with_subtitle(
            ui,
            "Visible columns",
            "Right-click the column header in the file browser to toggle",
            theme,
            |ui| {
                use crate::browser::ColumnId;
                for col in ColumnId::all() {
                    let mut vis = prefs.file_browser.visible_columns.contains(col);
                    if ui.checkbox(&mut vis, col.label()).changed() {
                        if vis {
                            if !prefs.file_browser.visible_columns.contains(col) {
                                prefs.file_browser.visible_columns.push(*col);
                            }
                        } else {
                            prefs.file_browser.visible_columns.retain(|c| c != col);
                        }
                        *dirty = true;
                    }
                }
            },
        );

        // ── DISPLAY section ──
        Self::section_header(ui, "DISPLAY", theme);

        Self::setting_row(ui, "Thumbnail size", theme, |ui| {
            use crate::browser::FileThumbSize;
            egui::ComboBox::from_id_salt("fb_thumb_size")
                .selected_text(match prefs.file_browser.file_thumb_size {
                    FileThumbSize::Small => "Small",
                    FileThumbSize::Medium => "Medium",
                    FileThumbSize::Large => "Large",
                })
                .width(100.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(
                            &mut prefs.file_browser.file_thumb_size,
                            FileThumbSize::Small,
                            "Small",
                        )
                        .changed()
                    {
                        *dirty = true;
                    }
                    if ui
                        .selectable_value(
                            &mut prefs.file_browser.file_thumb_size,
                            FileThumbSize::Medium,
                            "Medium",
                        )
                        .changed()
                    {
                        *dirty = true;
                    }
                    if ui
                        .selectable_value(
                            &mut prefs.file_browser.file_thumb_size,
                            FileThumbSize::Large,
                            "Large",
                        )
                        .changed()
                    {
                        *dirty = true;
                    }
                });
        });

        Self::setting_row(ui, "Default sort", theme, |ui| {
            use crate::browser::FileSortBy;
            egui::ComboBox::from_id_salt("fb_sort_by")
                .selected_text(match prefs.file_browser.file_sort_by {
                    FileSortBy::Name => "Name",
                    FileSortBy::DateModified => "Date modified",
                    FileSortBy::Size => "Size",
                    FileSortBy::Type => "Type",
                    FileSortBy::Rating => "Rating",
                })
                .width(140.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(&mut prefs.file_browser.file_sort_by, FileSortBy::Name, "Name")
                        .changed()
                    { *dirty = true; }
                    if ui
                        .selectable_value(&mut prefs.file_browser.file_sort_by, FileSortBy::DateModified, "Date modified")
                        .changed()
                    { *dirty = true; }
                    if ui
                        .selectable_value(&mut prefs.file_browser.file_sort_by, FileSortBy::Size, "Size")
                        .changed()
                    { *dirty = true; }
                    if ui
                        .selectable_value(&mut prefs.file_browser.file_sort_by, FileSortBy::Type, "Type")
                        .changed()
                    { *dirty = true; }
                    if ui
                        .selectable_value(&mut prefs.file_browser.file_sort_by, FileSortBy::Rating, "Rating")
                        .changed()
                    { *dirty = true; }
                });
        });

        Self::setting_row_with_subtitle(
            ui,
            "Sort ascending",
            "Off = newest/largest first",
            theme,
            |ui| {
                let mut val = prefs.file_browser.file_sort_ascending;
                if ui.add(Switch::new(&mut val, theme)).changed() {
                    prefs.file_browser.file_sort_ascending = val;
                    *dirty = true;
                }
            },
        );

        Self::setting_row(ui, "Default type filter", theme, |ui| {
            use crate::browser::FileTypeFilter;
            egui::ComboBox::from_id_salt("fb_type_filter")
                .selected_text(match prefs.file_browser.file_type_filter {
                    FileTypeFilter::All => "All",
                    FileTypeFilter::Video => "Video only",
                    FileTypeFilter::Audio => "Audio only",
                    FileTypeFilter::Images => "Images only",
                })
                .width(140.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(&mut prefs.file_browser.file_type_filter, FileTypeFilter::All, "All")
                        .changed()
                    { *dirty = true; }
                    if ui
                        .selectable_value(&mut prefs.file_browser.file_type_filter, FileTypeFilter::Video, "Video only")
                        .changed()
                    { *dirty = true; }
                    if ui
                        .selectable_value(&mut prefs.file_browser.file_type_filter, FileTypeFilter::Audio, "Audio only")
                        .changed()
                    { *dirty = true; }
                    if ui
                        .selectable_value(&mut prefs.file_browser.file_type_filter, FileTypeFilter::Images, "Images only")
                        .changed()
                    { *dirty = true; }
                });
        });

        // ── FAVORITES section ──
        Self::section_header(ui, "FAVORITES", theme);

        if prefs.file_browser.pinned_folders.is_empty() {
            Self::setting_row_with_subtitle(
                ui,
                "Pinned folders",
                "Right-click any folder in the browser and choose \"Pin to Favorites\"",
                theme,
                |ui| {
                    ui.label(
                        egui::RichText::new("none")
                            .size(11.0)
                            .color(theme.text_dim),
                    );
                },
            );
        } else {
            Self::setting_row(ui, "Pinned folders", theme, |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} folder{}",
                        prefs.file_browser.pinned_folders.len(),
                        if prefs.file_browser.pinned_folders.len() == 1 { "" } else { "s" }
                    ))
                    .size(11.0)
                    .color(theme.text_dim),
                );
            });
            // Each pinned folder gets its own row showing the path and an
            // unpin button. Right-clicking in the file browser is the usual
            // way to manage favorites; this is the explicit fallback.
            let mut to_remove: Option<usize> = None;
            for (i, path) in prefs.file_browser.pinned_folders.iter().enumerate() {
                let display = path.display().to_string();
                Self::setting_row_with_subtitle(ui, "  \u{1F4CC}", &display, theme, |ui| {
                    if ui.button("Unpin").clicked() {
                        to_remove = Some(i);
                    }
                });
            }
            if let Some(i) = to_remove {
                prefs.file_browser.pinned_folders.remove(i);
                *dirty = true;
            }
        }
    }

    fn show_audio(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
        theme: &KlippTheme,
    ) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Audio").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── MIXING section ──
        Self::section_header(ui, "MIXING", theme);

        Self::setting_row_with_subtitle(ui, "Pan law", "Gain change when panning center to side", theme, |ui| {
            egui::ComboBox::from_id_salt("pan_law")
                .selected_text(match prefs.audio.pan_law {
                    klipp_core::PanLaw::Linear => "Linear (balance)",
                    klipp_core::PanLaw::ConstantPower => "\u{2013}3 dB (equal power)",
                    klipp_core::PanLaw::Minus3dB => "\u{2013}3 dB",
                    klipp_core::PanLaw::Minus4_5dB => "\u{2013}4.5 dB",
                    klipp_core::PanLaw::Minus6dB => "\u{2013}6 dB",
                })
                .width(150.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.audio.pan_law, klipp_core::PanLaw::ConstantPower, "\u{2013}3 dB (equal power)").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.audio.pan_law, klipp_core::PanLaw::Linear, "Linear (balance)").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.audio.pan_law, klipp_core::PanLaw::Minus3dB, "\u{2013}3 dB").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.audio.pan_law, klipp_core::PanLaw::Minus4_5dB, "\u{2013}4.5 dB").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.audio.pan_law, klipp_core::PanLaw::Minus6dB, "\u{2013}6 dB").changed() { *dirty = true; }
                });
        });

        Self::setting_row_with_subtitle(ui, "Anti-click fade", "Duration of micro-fade at clip edges", theme, |ui| {
            let mut v = prefs.advanced.anti_click_fade_ms;
            if ui.add(egui::DragValue::new(&mut v).range(0.0..=20.0).speed(0.5).suffix(" ms").fixed_decimals(0)).changed() {
                prefs.advanced.anti_click_fade_ms = v; *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "Default fade duration", "A clip's Add Fade In and Add Fade Out", theme, |ui| {
            let mut v_secs = prefs.audio.default_fade_duration_ms as f32 / 1000.0;
            if ui.add(egui::DragValue::new(&mut v_secs).range(0.05..=5.0).speed(0.05).suffix(" s").fixed_decimals(2)).changed() {
                prefs.audio.default_fade_duration_ms = (v_secs * 1000.0).round() as u32;
                *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "Default fade curve", "A fade added, or drawn out from none on the timeline", theme, |ui| {
            egui::ComboBox::from_id_salt("fade_curve")
                .selected_text(crate::inspector::audio_tab::fade_curve_label(
                    prefs.audio.default_fade_curve,
                ))
                .width(100.0)
                .show_ui(ui, |ui| {
                    // One list, from the same source as the clip
                    // inspector's, so a shape cannot appear in one
                    // picker and not the other.
                    for curve in crate::inspector::audio_tab::FADE_CURVES {
                        let label = crate::inspector::audio_tab::fade_curve_label(*curve);
                        if ui.selectable_value(&mut prefs.audio.default_fade_curve, *curve, label).changed() {
                            *dirty = true;
                        }
                    }
                });
        });

        // ── LOUDNESS section ──
        Self::section_header(ui, "LOUDNESS", theme);

        Self::setting_row(ui, "Loudness standard", theme, |ui| {
            egui::ComboBox::from_id_salt("loudness_std")
                .selected_text(match prefs.audio.loudness_standard {
                    klipp_core::LoudnessStandard::EbuR128 => "EBU R128 (\u{2013}23 LUFS)",
                    klipp_core::LoudnessStandard::AtscA85 => "ATSC A/85 (\u{2013}24 LKFS)",
                    klipp_core::LoudnessStandard::Off => "Off",
                })
                .width(160.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.audio.loudness_standard, klipp_core::LoudnessStandard::EbuR128, "EBU R128 (\u{2013}23 LUFS)").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.audio.loudness_standard, klipp_core::LoudnessStandard::AtscA85, "ATSC A/85 (\u{2013}24 LKFS)").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.audio.loudness_standard, klipp_core::LoudnessStandard::Off, "Off").changed() { *dirty = true; }
                });
        });

        Self::setting_row(ui, "Peak hold time", theme, |ui| {
            let mut v = prefs.audio.peak_hold_time_secs;
            if ui.add(egui::DragValue::new(&mut v).range(0.5..=10.0).speed(0.1).suffix(" s").fixed_decimals(1)).changed() {
                prefs.audio.peak_hold_time_secs = v; *dirty = true;
            }
        });

        // ── AUTO-DUCK DEFAULTS section ──
        Self::section_header(ui, "AUTO-DUCK DEFAULTS", theme);

        Self::setting_row_with_subtitle(ui, "Duck level", "Target volume while ducking (0 = silence, 1 = unchanged)", theme, |ui| {
            let mut v = prefs.audio.duck_level;
            if ui.add(egui::DragValue::new(&mut v).range(0.0..=1.0).speed(0.01).fixed_decimals(2)).changed() {
                prefs.audio.duck_level = v; *dirty = true;
            }
        });

        Self::setting_row(ui, "Attack", theme, |ui| {
            let mut v = prefs.audio.duck_attack_secs;
            if ui.add(egui::DragValue::new(&mut v).range(0.01..=2.0).speed(0.01).suffix(" s").fixed_decimals(2)).changed() {
                prefs.audio.duck_attack_secs = v; *dirty = true;
            }
        });

        Self::setting_row(ui, "Release", theme, |ui| {
            let mut v = prefs.audio.duck_release_secs;
            if ui.add(egui::DragValue::new(&mut v).range(0.01..=2.0).speed(0.01).suffix(" s").fixed_decimals(2)).changed() {
                prefs.audio.duck_release_secs = v; *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "Threshold", "Sidechain level above which ducking activates", theme, |ui| {
            let mut v = prefs.audio.duck_threshold;
            if ui.add(egui::DragValue::new(&mut v).range(0.0..=1.0).speed(0.01).fixed_decimals(2)).changed() {
                prefs.audio.duck_threshold = v; *dirty = true;
            }
        });
    }

    fn show_export(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
        _cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Export").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── DEFAULTS section ──
        Self::section_header(ui, "DEFAULTS", theme);

        Self::setting_row(ui, "Audio sample rate", theme, |ui| {
            let rates = [(44100_u32, "44.1 kHz"), (48000, "48 kHz"), (96000, "96 kHz")];
            let current = rates.iter()
                .find(|(v, _)| *v == prefs.export.audio_sample_rate)
                .map(|(_, l)| *l).unwrap_or("48 kHz");
            egui::ComboBox::from_id_salt("export_sr")
                .selected_text(current)
                .width(80.0)
                .show_ui(ui, |ui| {
                    for (val, label) in &rates {
                        if ui.selectable_value(&mut prefs.export.audio_sample_rate, *val, *label).changed() { *dirty = true; }
                    }
                });
        });

        // ── BEHAVIOR section ──
        Self::setting_row_with_subtitle(ui, "Default export folder", "Where the Export dialog puts a new export", theme, |ui| {
            let picker = egui::Id::new("prefs-default-export-dir");
            if ui.button("Browse\u{2026}").clicked() {
                crate::file_dialog::pick_path(picker, || {
                    rfd::FileDialog::new().set_title("Default Export Folder").pick_folder()
                });
            }
            if let Some(crate::file_dialog::Picked::Path(path)) = crate::file_dialog::take_path(picker) {
                prefs.export.default_output_dir = Some(path);
                *dirty = true;
            }
            if prefs.export.default_output_dir.is_some() && ui.small_button("Reset").on_hover_text("Back to the Videos folder").clicked() {
                prefs.export.default_output_dir = None;
                *dirty = true;
            }
            let shown = prefs.export.default_output_dir.clone().unwrap_or_else(|| dirs::video_dir().or_else(dirs::home_dir).unwrap_or_default());
            ui.add(egui::Label::new(egui::RichText::new(tilde(&shown)).size(11.0).color(theme.text_dim)).truncate());
        });

        Self::section_header(ui, "BEHAVIOR", theme);

        Self::setting_row_with_subtitle(ui, "Overwrite existing files", "When the export's file is already there", theme, |ui| {
            use crate::prefs::OverwriteMode;
            let label = |m: OverwriteMode| match m {
                OverwriteMode::Ask => "Ask",
                OverwriteMode::Always => "Overwrite",
                OverwriteMode::Never => "Pick a new name",
            };
            egui::ComboBox::from_id_salt("overwrite_mode")
                .selected_text(label(prefs.export.overwrite_mode))
                .width(130.0)
                .show_ui(ui, |ui| {
                    for m in [OverwriteMode::Ask, OverwriteMode::Always, OverwriteMode::Never] {
                        if ui.selectable_value(&mut prefs.export.overwrite_mode, m, label(m)).changed() { *dirty = true; }
                    }
                });
        });

        Self::setting_row(ui, "Parallel render jobs", theme, |ui| {
            let mut val = prefs.export.parallel_render_jobs;
            let resp = ui.add(
                egui::Slider::new(&mut val, 1..=8).text("jobs"),
            ).on_hover_text(
                "How many export jobs run simultaneously.\n\
                 Higher = faster multi-segment exports, more CPU/GPU load.\n\
                 Takes effect on the next export.",
            );
            if resp.changed() {
                prefs.export.parallel_render_jobs = val;
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Open folder after export", theme, |ui| {
            let mut val = prefs.export.open_folder_after_export;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.export.open_folder_after_export = val;
                *dirty = true;
            }
        });

        // Post-export shell command (#63 Tier-1).
        // Fire-and-forget via bash -c, with {file}/{dir} placeholders.
        Self::setting_row(ui, "Post-export command", theme, |ui| {
            let resp = ui
                .add(
                    egui::TextEdit::singleline(&mut prefs.export.post_export_command)
                        .desired_width(360.0)
                        .hint_text("e.g. rsync -av {file} user@host:/media/"),
                )
                .on_hover_text(
                    "Shell command run after an export finishes.\n\
                     {file} → output file path\n\
                     {dir}  → output directory\n\
                     Runs via `bash -c` in the background. Leave empty to skip.",
                );
            if resp.changed() {
                *dirty = true;
            }
        });
    }

    fn show_proxy(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Proxy").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── GENERATION section ──
        Self::section_header(ui, "GENERATION", theme);

        Self::setting_row(ui, "Auto-generate on import", theme, |ui| {
            let mut val = prefs.proxy.auto_generate_on_import;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.proxy.auto_generate_on_import = val;
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Auto-switch during scrub", theme, |ui| {
            let mut val = prefs.proxy.auto_switch_during_scrub;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.proxy.auto_switch_during_scrub = val;
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Proxy resolution", theme, |ui| {
            egui::ComboBox::from_id_salt("proxy_resolution")
                .selected_text(prefs.proxy.proxy_resolution.label())
                .width(100.0)
                .show_ui(ui, |ui| {
                    for res in [
                        ProxyResolution::Quarter,
                        ProxyResolution::Half,
                        ProxyResolution::W960,
                        ProxyResolution::W1280,
                    ] {
                        if ui.selectable_value(&mut prefs.proxy.proxy_resolution, res, res.label()).changed() {
                            *dirty = true;
                        }
                    }
                });
        });

        // ── STORAGE section ──
        Self::section_header(ui, "STORAGE", theme);

        // Proxy cache directory with path + Browse
        {
            let row_h = crate::inspector::rows::ROW_H + 4.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "Proxy cache",
                egui::FontId::proportional(crate::inspector::rows::TEXT),
                theme.text_secondary,
            );
            // Browse button
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 80.0, rect.top() + 6.0),
                egui::vec2(66.0, 24.0),
            );
            let btn_resp = ui.allocate_rect(btn_rect, egui::Sense::click());
            let btn_bg = if btn_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(btn_rect, 4.0, btn_bg);
            ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
            ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, "Browse...", egui::FontId::proportional(11.0), theme.text_primary);
            // Path display
            let display = prefs.proxy.proxy_cache_dir.as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "~/.cache/klipp/proxies".into());
            ui.painter().text(
                egui::pos2(btn_rect.left() - 8.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                &display,
                egui::FontId::proportional(11.0),
                theme.text_dim,
            );
            let picker = egui::Id::new("prefs-proxy-cache-dir");
            if btn_resp.clicked() {
                crate::file_dialog::pick_path(picker, || {
                    rfd::FileDialog::new().set_title("Proxy Cache Folder").pick_folder()
                });
            }
            if let Some(crate::file_dialog::Picked::Path(path)) = crate::file_dialog::take_path(picker) {
                prefs.proxy.proxy_cache_dir = Some(path);
                *dirty = true;
            }
        }

        // ── MANAGEMENT section ──
        Self::section_header(ui, "MANAGEMENT", theme);

        // Aggregate proxy cache size for the subtitle. We compute this
        // cheaply inline rather than snapshotting cache_categories so the
        // number stays accurate even if the preferences window is opened
        // before the cache manager has been opened once.
        let cache_dir = prefs.proxy.proxy_cache_dir.clone()
            .unwrap_or_else(|| dirs::cache_dir().unwrap_or_default().join("klipp/proxies"));
        let (total_bytes, file_count) = crate::cache_manager::scan_dir_size(&cache_dir);
        let size_label = crate::cache_manager::format_cache_size(total_bytes);

        // Single "Open Cache Manager…" button replaces the old inline clear
        // button — the new dialog shows all four cache categories (proxy,
        // filmstrips, audio peaks, render) with a pie chart and per-
        // category clear. Subtitle still shows proxy totals for quick
        // context without opening the modal.
        Self::setting_row_with_subtitle(
            ui,
            "Proxy cache",
            &format!("{} files \u{2014} {}", file_count, size_label),
            theme,
            |ui| {
                let btn_rect = egui::Rect::from_min_size(
                    ui.cursor().min,
                    egui::vec2(150.0, 24.0),
                );
                let resp = ui.allocate_rect(btn_rect, egui::Sense::click());
                let bg = if resp.hovered() {
                    theme.bg_hover
                } else {
                    theme.bg_raised
                };
                ui.painter().rect_filled(btn_rect, 4.0, bg);
                ui.painter().rect_stroke(
                    btn_rect,
                    4.0,
                    egui::Stroke::new(1.0_f32, theme.border_light),
                );
                ui.painter().text(
                    btn_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Open Cache Manager\u{2026}",
                    egui::FontId::proportional(11.0),
                    theme.text_primary,
                );
                if resp.clicked() {
                    cmds.push(AppCommand::OpenCacheManager);
                }
            },
        );
    }

    /// Accounts (#1894): the person's Freesound sign-in, for sound effects
    /// as their original recordings. The same sign-in an assistant does
    /// over the socket, here without one.
    fn show_accounts(&mut self, ui: &mut egui::Ui, theme: &KlippTheme) {
        use klipp_audio_search::freesound_auth as auth;
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Accounts").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);
        Self::section_header(ui, "FREESOUND", theme);
        let client = auth::Client::from_keys();
        let signed_in = client.as_ref().and_then(|_| auth::signed_in());
        let (waiting, error) = {
            let st = self.accounts.lock().unwrap_or_else(|e| e.into_inner());
            (st.waiting, st.error.clone())
        };
        match (&client, &signed_in) {
            (None, _) => {
                Self::setting_row_with_subtitle(
                    ui,
                    "Freesound",
                    "Signing in brings sound effects as their original recordings, not previews. It needs your own Freesound API credential: make one at freesound.org/apiv2/apply with the callback URL below, and put freesound_client_id and freesound_client_secret in ~/.config/klipp/keys.toml.",
                    theme,
                    |ui| {
                        ui.label(egui::RichText::new("No credential").color(theme.text_dim));
                    },
                );
                Self::setting_row(ui, "Callback URL", theme, |ui| {
                    if ui.button("Copy").on_hover_text(auth::REDIRECT_URI).clicked() {
                        ui.ctx().copy_text(auth::REDIRECT_URI.to_string());
                    }
                    ui.label(egui::RichText::new(auth::REDIRECT_URI).monospace().size(11.0));
                });
            }
            (Some(_), Some(tokens)) => {
                let who = tokens.username.clone().map_or("Signed in".to_string(), |n| format!("Signed in as {n}"));
                Self::setting_row_with_subtitle(ui, "Freesound", "Sound effects come as their original recordings.", theme, |ui| {
                    if ui.button("Sign out").clicked() {
                        let err = auth::sign_out().err();
                        self.accounts.lock().unwrap_or_else(|e| e.into_inner()).error = err;
                    }
                    ui.label(egui::RichText::new(who).color(theme.text_primary));
                });
            }
            (Some(client), None) => {
                Self::setting_row_with_subtitle(ui, "Freesound", "Not signed in: sound effects come as previews. Sign in opens Freesound in your browser to approve.", theme, |ui| {
                    if waiting {
                        ui.add(egui::Spinner::new());
                        ui.label(egui::RichText::new("Waiting for the browser\u{2026}").color(theme.text_dim));
                    } else if ui.button("Sign in\u{2026}").clicked() {
                        self.start_freesound_sign_in(client.clone(), ui.ctx().clone());
                    }
                });
                if waiting {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                }
                // Where Freesound shows a code instead of coming back.
                Self::setting_row_with_subtitle(ui, "Code from Freesound", "Only if Freesound showed you a code instead of returning to Klipp.", theme, |ui| {
                    let ready = !self.freesound_code.trim().is_empty();
                    if ui.add_enabled(ready, egui::Button::new("Finish")).clicked() {
                        let code = std::mem::take(&mut self.freesound_code);
                        let err = auth::finish_sign_in(client, &code).err();
                        self.accounts.lock().unwrap_or_else(|e| e.into_inner()).error = err;
                    }
                    ui.add(egui::TextEdit::singleline(&mut self.freesound_code).desired_width(150.0).hint_text("paste the code"));
                });
            }
        }
        if let Some(e) = error {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(egui::RichText::new(e).color(theme.marker_warning).size(11.0));
            });
        }
    }

    /// Start a sign-in on a worker: the listener bound first, then the
    /// browser opened at the approval page; the page shows how it went.
    fn start_freesound_sign_in(&self, client: klipp_audio_search::freesound_auth::Client, ctx: egui::Context) {
        use klipp_audio_search::freesound_auth as auth;
        {
            let mut st = self.accounts.lock().unwrap_or_else(|e| e.into_inner());
            st.waiting = true;
            st.error = None;
        }
        let state_out = std::sync::Arc::clone(&self.accounts);
        std::thread::spawn(move || {
            let state = auth::new_state();
            let url = client.authorize_url(&state);
            let outcome = auth::await_redirect_signalling(&state, std::time::Duration::from_secs(300), || {
                if let Err(e) = auth::open_browser(&url) {
                    tracing::warn!("Freesound sign-in: {e}");
                }
            })
            .and_then(|code| auth::finish_sign_in(&client, &code));
            let mut st = state_out.lock().unwrap_or_else(|e| e.into_inner());
            st.waiting = false;
            st.error = outcome.err().map(|e| format!("{e}. If Freesound showed a code, paste it below."));
            ctx.request_repaint();
        });
    }

    fn show_external_tools(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
        theme: &KlippTheme,
    ) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("External Tools").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── APPLICATIONS section ──
        Self::section_header(ui, "APPLICATIONS", theme);

        Self::setting_row_with_subtitle(ui, "Image editor", "Used for still images (PNG/JPG/etc). Empty = OS default.", theme, |ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut prefs.external_tools.image_editor)
                    .desired_width(230.0)
                    .hint_text("e.g. gimp"),
            );
            if resp.changed() { *dirty = true; }
        });

        Self::setting_row_with_subtitle(ui, "Audio editor", "Used for audio-only clips. Empty = OS default.", theme, |ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut prefs.external_tools.audio_editor)
                    .desired_width(230.0)
                    .hint_text("e.g. audacity"),
            );
            if resp.changed() { *dirty = true; }
        });

        Self::setting_row_with_subtitle(ui, "Video editor", "Used for video clips. Empty = OS default.", theme, |ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut prefs.external_tools.video_editor)
                    .desired_width(230.0)
                    .hint_text("e.g. kdenlive"),
            );
            if resp.changed() { *dirty = true; }
        });

        // ── ROUND-TRIP section ──
        Self::section_header(ui, "ROUND-TRIP", theme);

        Self::setting_row_with_subtitle(
            ui,
            "Reload on external edit",
            "When the external app saves the file, refresh metadata and thumbnails automatically.",
            theme,
            |ui| {
                let mut val = prefs.external_tools.round_trip_reload;
                let resp = ui.add(Switch::new(&mut val, theme))
                    .on_hover_text("Uses the existing media watcher — no extra file watches are created.");
                if resp.changed() {
                    prefs.external_tools.round_trip_reload = val;
                    *dirty = true;
                }
            },
        );

        // ── HINT ──
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            ui.add_space(14.0);
            ui.label(
                egui::RichText::new(
                    "Enter an executable name (looked up on $PATH) or a full path. \
                     Examples: gimp, krita, audacity, kdenlive, /usr/bin/gimp. \
                     Leave empty to fall back to xdg-open (OS default). Linux only \u{2014} \
                     macOS/Windows support is deferred.",
                )
                .size(11.0)
                .color(theme.text_dim),
            );
        });
    }

    // ══════════════════════════════════════════════
    // NEW PREFERENCE TABS (Phase 2)
    // ══════════════════════════════════════════════

    fn show_ai(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, cmds: &mut Vec<AppCommand>, theme: &KlippTheme) {
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("AI").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── Features (each its own opt-out) ──
        Self::section_header(ui, "FEATURES", theme);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("All on-device — nothing here ever leaves your machine. The non-AI analysis (shots, silence, loudness) always runs.").color(theme.text_secondary));
        });
        ui.add_space(6.0);
        Self::setting_row_with_subtitle(ui, "Speech transcription", "For captions and subtitles. Runs on import, and on demand from a clip's right-click menu.", theme, |ui| {
            let mut val = prefs.ai.transcription;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.ai.transcription = val;
                klipp_analysis::set_transcription_enabled(val);
                *dirty = true;
            }
        });
        Self::setting_row_with_subtitle(ui, "Subject detection", "Finds people and objects in the picture for framing and masking.", theme, |ui| {
            let mut val = prefs.ai.subject_detection;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.ai.subject_detection = val;
                klipp_analysis::set_subject_detection_enabled(val);
                *dirty = true;
            }
        });
        Self::setting_row_with_subtitle(ui, "Screen text (OCR)", "Reads what is on screen in a screen recording, so footage can be searched by its text and secrets left on screen are flagged. Needs two small models (about 10 MB), downloaded only when you ask.", theme, |ui| {
            let mut val = prefs.ai.screen_text;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.ai.screen_text = val;
                klipp_analysis::set_screen_text_enabled(val);
                *dirty = true;
            }
        });
        Self::setting_row_with_subtitle(ui, "AI assistant (MCP)  \u{2022}  restart required", "Runs the control socket and MCP server so an agent can drive Klipp through the klipp-mcp tools. Off means no agent can connect.", theme, |ui| {
            let mut val = prefs.ai.assistant;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.ai.assistant = val;
                *dirty = true;
            }
        });

        // ── Status ──
        Self::section_header(ui, "STATUS", theme);
        let speech_status = if !prefs.ai.transcription {
            "Off".to_string()
        } else {
            match klipp_analysis::transcript::installed_model() {
                Some((id, _)) => format!("Ready — {}", model_display_name(id)),
                None => "No speech model downloaded — get one below".to_string(),
            }
        };
        Self::setting_row(ui, "Speech (transcription)", theme, |ui| {
            ui.label(egui::RichText::new(speech_status).color(theme.text_secondary));
        });
        let vision_status = if prefs.ai.subject_detection { ai_vision_status() } else { "Off".to_string() };
        Self::setting_row(ui, "Subject detection", theme, |ui| {
            ui.label(egui::RichText::new(vision_status).color(theme.text_secondary));
        });
        let text_status = if !prefs.ai.screen_text {
            "Off".to_string()
        } else if klipp_analysis::screentext::installed() {
            "Ready".to_string()
        } else {
            "No OCR models downloaded — get them below".to_string()
        };
        Self::setting_row(ui, "Screen text (OCR)", theme, |ui| {
            ui.label(egui::RichText::new(text_status).color(theme.text_secondary));
        });
        Self::setting_row(ui, "Assistant (MCP)", theme, |ui| {
            let s = if prefs.ai.assistant { "On — see Connect an assistant below" } else { "Off" };
            ui.label(egui::RichText::new(s).color(theme.text_secondary));
        });

        self.show_connect_assistant(ui, prefs, theme);

        // ── Transcription (speech) ──
        Self::section_header(ui, "TRANSCRIPTION (SPEECH)", theme);
        if !prefs.ai.transcription {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(egui::RichText::new("Turn on Speech transcription above to use these.").color(theme.text_secondary));
            });
        }
        Self::setting_row_with_subtitle(ui, "Transcribe on CPU", "Slower, but works without a Vulkan GPU.", theme, |ui| {
            let mut val = prefs.ai.transcribe_on_cpu;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.ai.transcribe_on_cpu = val;
                klipp_analysis::transcript::set_transcribe_on_cpu(val);
                *dirty = true;
            }
        });
        let active = klipp_analysis::transcript::installed_model().map(|(id, _)| id);
        for id in klipp_analysis::transcript::MODELS {
            let (name, size, installed) = model_row_info(id);
            let sub = if active == Some(id) {
                format!("{}  •  in use", whisper_blurb(id))
            } else {
                whisper_blurb(id).to_string()
            };
            Self::setting_row_with_subtitle(ui, &format!("{name}  ({size})"), &sub, theme, |ui| {
                if installed {
                    ui.label(egui::RichText::new("Installed").color(theme.text_secondary));
                } else if ui.button("Download").clicked() {
                    cmds.push(AppCommand::OpenModelDownloadDialog { model_id: id.to_string() });
                }
            });
        }

        // ── Screen text (OCR) ──
        Self::section_header(ui, "SCREEN TEXT (OCR)", theme);
        if !prefs.ai.screen_text {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(egui::RichText::new("Turn on Screen text above to use these.").color(theme.text_secondary));
            });
        }
        for id in klipp_analysis::screentext::MODEL_IDS {
            let (name, size, installed) = model_row_info(id);
            let sub = if id == klipp_analysis::screentext::DET_MODEL_ID { "Finds where text is on the screen (PaddleOCR, Apache-2.0)." } else { "Reads each line of English text (PaddleOCR, Apache-2.0)." };
            Self::setting_row_with_subtitle(ui, &format!("{name}  ({size})"), sub, theme, |ui| {
                if installed {
                    ui.label(egui::RichText::new("Installed").color(theme.text_secondary));
                } else if ui.button("Download").clicked() {
                    cmds.push(AppCommand::OpenModelDownloadDialog { model_id: id.to_string() });
                }
            });
        }

        // ── All models ──
        Self::section_header(ui, "MODELS", theme);
        Self::setting_row_with_subtitle(ui, "All AI models", "Browse, download, switch and remove every model — speech and vision — with sizes and details.", theme, |ui| {
            if ui.button("Manage models\u{2026}").clicked() {
                cmds.push(AppCommand::OpenAiModelsDialog);
            }
        });
    }

    /// Preferences → AI → Connect an assistant (#625): the installed
    /// `klipp-mcp`, each assistant on this computer that can register it by
    /// its own command with Connect and Disconnect, a read-only check that
    /// Klipp answers through it, and the settings for any other assistant.
    fn show_connect_assistant(&mut self, ui: &mut egui::Ui, prefs: &Preferences, theme: &KlippTheme) {
        Self::section_header(ui, "CONNECT AN ASSISTANT", theme);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            if ui.button("Set Up AI Assistants\u{2026}").on_hover_text("The same, in a window of its own").clicked() {
                self.open_assistant_setup = true;
            }
        });
        Self::assistant_setup_body(ui, &self.assistant, prefs.ai.assistant, theme);
    }

    /// The Set Up AI Assistants window (#2649): open while `open` is.
    pub fn show_assistant_setup_window(&mut self, ctx: &egui::Context, open: &mut bool, prefs: &Preferences, theme: &KlippTheme) {
        if !*open {
            return;
        }
        let shared = std::sync::Arc::clone(&self.assistant);
        egui::Window::new("Set Up AI Assistants")
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .max_height(ctx.screen_rect().height() * 0.85)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| Self::assistant_setup_body(ui, &shared, prefs.ai.assistant, theme));
            });
    }

    /// Whether the section asked for the window, once.
    pub fn take_open_assistant_setup(&mut self) -> bool {
        std::mem::take(&mut self.open_assistant_setup)
    }

    /// What both show: the installed `klipp-mcp`, each assistant with its
    /// state and Connect, Disconnect and Test, when assistants last talked
    /// to Klipp, and the settings for any other. `ai_on` is the AI
    /// assistant switch: off, nothing connects.
    pub(crate) fn assistant_setup_body(ui: &mut egui::Ui, shared: &std::sync::Arc<std::sync::Mutex<AssistantConnect>>, ai_on: bool, theme: &KlippTheme) {
        use crate::assistant_clients::{self as clients, Registration};
        ui.horizontal_wrapped(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("An AI assistant on this computer can work in Klipp with you: edit, grade, mix, title and export in the open project. It runs Klipp's klipp-mcp program, which only talks to the Klipp on this computer.").color(theme.text_secondary));
        });
        ui.add_space(4.0);
        let shared = std::sync::Arc::clone(shared);
        let ctx = ui.ctx().clone();
        // Look, on a worker: which assistants there are and what they have.
        let refresh = {
            let shared = std::sync::Arc::clone(&shared);
            let ctx = ctx.clone();
            move || {
                let found = clients::klipp_mcp_path();
                let regs: Vec<_> = clients::Client::ALL.iter().map(|c| (*c, clients::registration(*c))).collect();
                let seen = klipp_core::assistants_seen::read();
                if let Ok(mut st) = shared.lock() {
                    st.klipp_mcp = found;
                    st.clients = regs;
                    st.seen = seen;
                    st.busy = None;
                }
                ctx.request_repaint();
            }
        };
        {
            let mut st = shared.lock().unwrap_or_else(|e| e.into_inner());
            if !st.looked {
                st.looked = true;
                st.busy = Some("Looking for assistants\u{2026}".into());
                let r = refresh.clone();
                std::thread::spawn(r);
            }
        }
        let st = shared.lock().unwrap_or_else(|e| e.into_inner());
        let klipp_mcp = st.klipp_mcp.clone();
        let clients_now = st.clients.clone();
        let busy = st.busy.clone();
        let check = st.check.clone();
        let error = st.error.clone();
        let tests = st.tests.clone();
        let seen = st.seen.clone();
        drop(st);

        if !ai_on {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(14.0);
                ui.label(egui::RichText::new("The AI assistant is off (above), so no assistant can connect. Turn it on and restart Klipp first.").color(theme.marker_warning));
            });
        }
        Self::setting_row_with_subtitle(ui, "Klipp's MCP program", "What an assistant runs to reach Klipp.", theme, |ui| {
            let text = klipp_mcp.as_ref().map_or("Not found next to Klipp".to_string(), |p| p.display().to_string());
            ui.label(egui::RichText::new(text).monospace().color(theme.text_secondary));
        });
        Self::setting_row_with_subtitle(ui, "Check the connection", "Asks Klipp for its status through that program, reading only.", theme, |ui| {
            if ui.add_enabled(klipp_mcp.is_some() && busy.is_none(), egui::Button::new("Check")).clicked() {
                if let Some(path) = klipp_mcp.clone() {
                    let shared = std::sync::Arc::clone(&shared);
                    let ctx = ctx.clone();
                    if let Ok(mut st) = shared.lock() {
                        st.busy = Some("Checking\u{2026}".into());
                    }
                    std::thread::spawn(move || {
                        let got = clients::check(&path);
                        if let Ok(mut st) = shared.lock() {
                            st.check = Some(got);
                            st.busy = None;
                        }
                        ctx.request_repaint();
                    });
                }
            }
        });
        if let Some(result) = &check {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(14.0);
                match result {
                    Ok(summary) => {
                        ui.label(egui::RichText::new(format!("Connected. {summary}")).color(theme.text_secondary));
                    }
                    Err(why) => {
                        ui.label(egui::RichText::new(why).color(theme.marker_warning));
                    }
                }
            });
        }
        for (client, reg) in &clients_now {
            let (state, can_connect, can_disconnect) = match reg {
                Registration::NotInstalled => ("Not installed on this computer".to_string(), false, false),
                Registration::NotConnected => ("Not connected".to_string(), true, false),
                Registration::Connected(cmd) if klipp_mcp.as_ref().is_some_and(|p| p.to_string_lossy() == cmd.as_str()) => ("Connected".to_string(), false, true),
                // An update that moved klipp-mcp leaves the entry pointing
                // at nothing (#2649): Update points it at this Klipp's.
                Registration::Connected(cmd) if !std::path::Path::new(cmd).exists() => (format!("Needs updating: its klipp-mcp ({cmd}) is no longer there"), true, true),
                Registration::Connected(cmd) => (format!("Connected to another klipp-mcp ({cmd})"), true, true),
                Registration::Unknown(why) => (format!("Could not read its settings: {why}"), true, false),
                // Only where a project registers it (#625): say which, and
                // that Connect makes it every project's.
                Registration::ProjectsOnly(projects) => {
                    let named: Vec<String> = projects.iter().map(|p| tilde(p)).collect();
                    (projects_only_text(&named), true, false)
                }
            };
            let client = *client;
            let tested = tests.iter().find(|(c, _)| *c == client).map(|(_, r)| r.clone());
            let state = match &tested {
                Some(Ok(_)) => format!("{state}. Test: Klipp answered."),
                Some(Err(why)) => format!("{state}. Test: {why}"),
                None => state,
            };
            Self::setting_row_with_subtitle(ui, client.name(), &state, theme, |ui| {
                let idle = busy.is_none() && klipp_mcp.is_some();
                // Ask Klipp through the klipp-mcp this assistant runs.
                if let Registration::Connected(cmd) = reg {
                    if ui.add_enabled(busy.is_none(), egui::Button::new("Test")).on_hover_text(format!("Ask Klipp for its status through the klipp-mcp {} runs. Reads only.", client.name())).clicked() {
                        let (shared, ctx, cmd) = (std::sync::Arc::clone(&shared), ctx.clone(), cmd.clone());
                        std::thread::spawn(move || {
                            let got = clients::check(std::path::Path::new(&cmd));
                            if let Ok(mut st) = shared.lock() {
                                st.tests.retain(|(c, _)| *c != client);
                                st.tests.push((client, got));
                            }
                            ctx.request_repaint();
                        });
                    }
                }
                if can_disconnect && ui.add_enabled(idle, egui::Button::new("Disconnect")).on_hover_text(format!("Remove Klipp from {}'s servers. Nothing else of its settings changes.", client.name())).clicked() {
                    let (shared, refresh) = (std::sync::Arc::clone(&shared), refresh.clone());
                    if let Ok(mut st) = shared.lock() {
                        st.busy = Some(format!("Disconnecting {}\u{2026}", client.name()));
                    }
                    std::thread::spawn(move || {
                        let r = clients::disconnect(client);
                        if let Ok(mut st) = shared.lock() {
                            st.error = r.err().map(|e| format!("{}: {e}", client.name()));
                        }
                        refresh();
                    });
                }
                let label = if matches!(reg, Registration::Connected(_)) { "Update" } else { "Connect" };
                let how = if client.by_file() { "in its settings file, backed up first" } else { "with its own command" };
                if can_connect && ui.add_enabled(idle && ai_on, egui::Button::new(label)).on_hover_text(format!("Add Klipp to {}'s servers {how}. Nothing else of its settings changes.", client.name())).clicked() {
                    if let Some(path) = klipp_mcp.clone() {
                        let (shared, refresh) = (std::sync::Arc::clone(&shared), refresh.clone());
                        if let Ok(mut st) = shared.lock() {
                            st.busy = Some(format!("Connecting {}\u{2026}", client.name()));
                        }
                        std::thread::spawn(move || {
                            let r = clients::connect(client, &path);
                            if let Ok(mut st) = shared.lock() {
                                st.error = r.err().map(|e| format!("{}: {e}", client.name()));
                            }
                            refresh();
                        });
                    }
                }
            });
        }
        if let Some(b) = &busy {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.spinner();
                ui.label(egui::RichText::new(b).color(theme.text_secondary));
            });
        }
        if let Some(e) = &error {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(14.0);
                ui.label(egui::RichText::new(e).color(theme.marker_warning));
            });
        }
        // When each assistant last talked to Klipp, by the name it gave.
        ui.horizontal_wrapped(|ui| {
            ui.add_space(14.0);
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let mut by_time: Vec<_> = seen.iter().collect();
            by_time.sort_by_key(|(_, t)| std::cmp::Reverse(**t));
            let text = if by_time.is_empty() {
                "No assistant has talked to Klipp yet.".to_string()
            } else {
                format!("Last talked to Klipp: {}", by_time.iter().map(|(n, t)| format!("{n}, {}", ago(**t, now))).collect::<Vec<_>>().join("; "))
            };
            ui.label(egui::RichText::new(text).color(theme.text_secondary));
        });
        ui.horizontal_wrapped(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("After connecting, start a new session in the assistant (or restart it) so it loads Klipp's tools. Then ask it \u{201c}Is Klipp connected?\u{201d} or \u{201c}What can I do in Klipp?\u{201d}").color(theme.text_secondary));
            if ui.small_button("Refresh").clicked() && busy.is_none() {
                if let Ok(mut st) = shared.lock() {
                    st.busy = Some("Looking for assistants\u{2026}".into());
                }
                std::thread::spawn(refresh.clone());
            }
        });
        if let Some(path) = &klipp_mcp {
            ui.add_space(4.0);
            egui::CollapsingHeader::new(egui::RichText::new("Another assistant").color(theme.text_primary))
                .id_salt("connect_other_assistant")
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Any assistant that runs local MCP servers can use Klipp. Add this to its MCP settings:").color(theme.text_secondary));
                    for (what, text) in [
                        ("Most assistants (JSON, mcpServers)", clients::json_snippet(path)),
                        ("Editors with a servers list (JSON, servers, e.g. VS Code's mcp.json)", clients::servers_snippet(path)),
                        ("Assistants with YAML extensions (e.g. Goose's config.yaml)", clients::yaml_snippet(path)),
                        ("Codex (~/.codex/config.toml)", clients::toml_snippet(path)),
                        ("Claude Code, in a terminal", clients::Client::ClaudeCode.connect_command(path)),
                        ("Codex, in a terminal", clients::Client::Codex.connect_command(path)),
                    ] {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(what).color(theme.text_primary));
                            if ui.small_button("Copy settings").clicked() {
                                ui.ctx().copy_text(text.clone());
                            }
                        });
                        ui.label(egui::RichText::new(&text).monospace().color(theme.text_secondary));
                    }
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("ChatGPT in the browser, and other assistants that run online, cannot reach a program on this computer, and Klipp does not offer a way in from the internet yet.").color(theme.text_secondary));
                });
        }
    }

    fn show_system(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, theme: &KlippTheme) {
        // Page heading with "Restart required" badge
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("System").strong().size(16.0).color(theme.text_primary));
            ui.add_space(8.0);
            // Restart required badge
            let badge_text = "Restart required";
            let badge_font = egui::FontId::proportional(10.0);
            let text_galley = ui.painter().layout_no_wrap(badge_text.to_string(), badge_font.clone(), theme.marker_warning);
            let badge_size = egui::vec2(text_galley.size().x + 12.0, 20.0);
            let (badge_rect, _) = ui.allocate_exact_size(badge_size, egui::Sense::hover());
            ui.painter().rect_stroke(badge_rect, 4.0, egui::Stroke::new(1.0_f32, theme.marker_warning));
            ui.painter().text(badge_rect.center(), egui::Align2::CENTER_CENTER, badge_text, badge_font, theme.marker_warning);
        });
        ui.add_space(4.0);

        // ── GPU section ──
        Self::section_header(ui, "GPU", theme);

        Self::setting_row_with_subtitle(ui, "GPU preference", "Affects effects, colour grading, and decode", theme, |ui| {
            egui::ComboBox::from_id_salt("gpu_pref")
                .selected_text(match prefs.system.gpu_preference {
                    klipp_core::GpuPreference::Auto => "Auto (recommended)",
                    klipp_core::GpuPreference::HighPerformance => "Prefer discrete",
                    klipp_core::GpuPreference::LowPower => "Prefer integrated",
                    klipp_core::GpuPreference::CpuOnly => "CPU only",
                })
                .width(140.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.system.gpu_preference, klipp_core::GpuPreference::Auto, "Auto (recommended)").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.system.gpu_preference, klipp_core::GpuPreference::HighPerformance, "Prefer discrete").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.system.gpu_preference, klipp_core::GpuPreference::LowPower, "Prefer integrated").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.system.gpu_preference, klipp_core::GpuPreference::CpuOnly, "CPU only").changed() { *dirty = true; }
                });
        });

        Self::setting_row_with_subtitle(ui, "Hardware video decode", "NVDEC / VAAPI", theme, |ui| {
            let mut val = prefs.system.hardware_decode;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.system.hardware_decode = val;
                *dirty = true;
            }
        });

        // ── THREADING section ──
        Self::section_header(ui, "THREADING", theme);

        Self::setting_row_with_subtitle(ui, "Decode threads", "Per clip decoder thread count", theme, |ui| {
            let threads = [
                (0_u32, "Auto"),
                (1, "1"),
                (2, "2"),
                (4, "4"),
                (8, "8"),
                (16, "16"),
            ];
            let current = threads.iter()
                .find(|(v, _)| *v == prefs.system.decode_threads)
                .map(|(_, l)| *l).unwrap_or("Auto");
            egui::ComboBox::from_id_salt("decode_threads")
                .selected_text(current)
                .width(60.0)
                .show_ui(ui, |ui| {
                    for (val, label) in &threads {
                        if ui.selectable_value(&mut prefs.system.decode_threads, *val, *label).changed() {
                            *dirty = true;
                        }
                    }
                });
        });

        Self::setting_row_with_subtitle(ui, "Frame queue size", "Larger = smoother, more RAM used", theme, |ui| {
            let sizes: &[u32] = &[4, 8, 16, 24, 32, 48, 64];
            let current = format!("{}", prefs.system.frame_queue_size);
            egui::ComboBox::from_id_salt("frame_queue")
                .selected_text(&current)
                .width(60.0)
                .show_ui(ui, |ui| {
                    for &s in sizes {
                        if ui.selectable_value(&mut prefs.system.frame_queue_size, s, format!("{s}")).changed() {
                            *dirty = true;
                        }
                    }
                });
        });

        Self::setting_row(ui, "Max playback resolution", theme, |ui| {
            let resolutions = [(720_u32, "720p"), (1080, "1080p"), (1440, "1440p"), (2160, "4K")];
            let current = resolutions.iter()
                .find(|(v, _)| *v == prefs.system.max_playback_height)
                .map(|(_, l)| *l).unwrap_or("1080p");
            egui::ComboBox::from_id_salt("max_pb_h")
                .selected_text(current)
                .width(80.0)
                .show_ui(ui, |ui| {
                    for (val, label) in &resolutions {
                        if ui.selectable_value(&mut prefs.system.max_playback_height, *val, *label).changed() {
                            *dirty = true;
                        }
                    }
                });
        });
    }

    fn show_appearance(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
        theme_registry: &crate::theme::ThemeRegistry,
        theme: &KlippTheme,
    ) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Appearance").strong().size(16.0).color(theme.text_primary));
        });

        // ── THEME section ──
        Self::section_header(ui, "THEME", theme);

        // Follow the desktop theme: a full system palette where there
        // is one, else the desktop portal's dark or light preference
        // and accent (#2625). The subtitle says which, or that nothing
        // was found. While it is on and something is followed, the
        // picker below stays visible but inactive so the user sees
        // what they get back.
        let subtitle = theme_registry.system_theme_summary();
        Self::setting_row_with_subtitle(ui, "Follow system theme", &subtitle, theme, |ui| {
            let mut val = prefs.appearance.follow_system_theme;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.appearance.follow_system_theme = val;
                prefs.appearance.follow_system_theme_pending = false;
                *dirty = true;
            }
        });
        let following = theme_registry.has_system_theme() && prefs.appearance.follow_system_theme;

        // Theme picker — lists every entry in the theme registry
        // (built-ins + user TOMLs from ~/.config/klipp/themes/).
        // Drop a TOML file in that directory and restart the app to
        // see it appear here. The selected name is stored as
        // `prefs.appearance.theme_name`; the registry on the
        // appended live-reload path resolves it back to a KlippTheme.
        // The live system entry is not offered: picking it by hand
        // would freeze one snapshot of the desktop palette.
        Self::setting_row_with_subtitle(
            ui,
            "Theme",
            "Built-in + user themes from ~/.config/klipp/themes/*.toml",
            theme,
            |ui| {
                let current = prefs.appearance.theme_name.clone();
                let names: Vec<(String, crate::theme::ThemeSource)> = theme_registry
                    .iter()
                    .filter(|(_, s)| *s != crate::theme::ThemeSource::Omarchy)
                    .map(|(n, s)| (n.to_string(), s))
                    .collect();
                ui.add_enabled_ui(!following, |ui| {
                    egui::ComboBox::from_id_salt("theme_picker")
                        .selected_text(&current)
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for (name, source) in names {
                                let label = match source {
                                    crate::theme::ThemeSource::Builtin => name.clone(),
                                    crate::theme::ThemeSource::User => format!("{name}  •  user"),
                                    crate::theme::ThemeSource::Omarchy => continue,
                                };
                                if ui
                                    .selectable_value(&mut prefs.appearance.theme_name, name, label)
                                    .changed()
                                {
                                    *dirty = true;
                                }
                            }
                        });
                });
            },
        );

        // UI scale
        Self::setting_row_with_subtitle(ui, "UI scale", "Auto follows the system scale, and steps down for a small window", theme, |ui| {
            let scales: &[(u32, &str)] = &[
                (0, "Auto"),
                (75, "75%"),
                (80, "80%"),
                (85, "85%"),
                (90, "90%"),
                (100, "100%"),
                (110, "110%"),
                (125, "125%"),
                (150, "150%"),
                (175, "175%"),
                (200, "200%"),
            ];
            let current = scales.iter()
                .find(|(v, _)| *v == prefs.appearance.ui_scale_percent)
                .map(|(_, l)| *l).unwrap_or("Auto");
            egui::ComboBox::from_id_salt("ui_scale")
                .selected_text(current)
                .width(70.0)
                .show_ui(ui, |ui| {
                    for (val, label) in scales {
                        if ui.selectable_value(&mut prefs.appearance.ui_scale_percent, *val, *label).changed() {
                            // Apply a set size immediately; Auto depends on
                            // the screen and is worked out by the app
                            // (`apply_ui_scale`) once the preference lands.
                            if *val != 0 {
                                ui.ctx().set_zoom_factor(*val as f32 / 100.0);
                            }
                            *dirty = true;
                        }
                    }
                });
        });

        // ── ROLE COLORS section ──
        Self::section_header(ui, "ROLE COLOURS", theme);

        // Helper for role color row with swatch + Edit button
        let role_color_row = |ui: &mut egui::Ui, label: &str, rgb: &mut [u8; 3], dirty: &mut bool, _id_salt: &str| {
            let row_h = crate::inspector::rows::ROW_H + 4.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            // Label
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(crate::inspector::rows::TEXT),
                theme.text_secondary,
            );
            // Edit button (right side)
            let edit_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 70.0, rect.top() + 6.0),
                egui::vec2(56.0, 24.0),
            );
            let edit_resp = ui.allocate_rect(edit_rect, egui::Sense::click());
            let edit_bg = if edit_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(edit_rect, 4.0, edit_bg);
            ui.painter().rect_stroke(edit_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
            ui.painter().text(edit_rect.center(), egui::Align2::CENTER_CENTER, "Edit...", egui::FontId::proportional(11.0), theme.text_primary);
            // Color swatch (before Edit button)
            let swatch_center = egui::pos2(edit_rect.left() - 18.0, rect.center().y);
            let c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            ui.painter().rect_filled(
                egui::Rect::from_center_size(swatch_center, egui::vec2(22.0, 22.0)),
                4.0, c,
            );
            // On Edit click, open color picker
            if edit_resp.clicked() {
                // Use egui's built-in color picker via a small widget area
                let widget_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.right() - 140.0, rect.top() + 2.0),
                    egui::vec2(60.0, 28.0),
                );
                let mut widget_ui = ui.new_child(egui::UiBuilder::new().max_rect(widget_rect));
                let mut col = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                if widget_ui.color_edit_button_srgba(&mut col).changed() {
                    *rgb = [col.r(), col.g(), col.b()];
                    *dirty = true;
                }
            }
        };

        role_color_row(ui, "Video", &mut prefs.appearance.role_video, dirty, "rv");
        role_color_row(ui, "Dialogue", &mut prefs.appearance.role_dialogue, dirty, "rd");
        role_color_row(ui, "Music", &mut prefs.appearance.role_music, dirty, "rm");
        role_color_row(ui, "SFX", &mut prefs.appearance.role_sfx, dirty, "rs");
        role_color_row(ui, "Title", &mut prefs.appearance.role_title, dirty, "rt");
        role_color_row(ui, "B-Roll", &mut prefs.appearance.role_broll, dirty, "rb");
        role_color_row(ui, "Ambience", &mut prefs.appearance.role_ambience, dirty, "ra");
        role_color_row(ui, "Foley", &mut prefs.appearance.role_foley, dirty, "rf");
        role_color_row(ui, "Voiceover", &mut prefs.appearance.role_voiceover, dirty, "ro");
        role_color_row(ui, "Graphics", &mut prefs.appearance.role_graphics, dirty, "rg");
    }

    fn show_audio_hardware(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, theme: &KlippTheme) {
        // Read the process-wide cached device list. The cache is primed at
        // app startup (background thread) and re-populated when the user
        // enters this tab via `enter_tab()`, so this call is essentially
        // free on every frame. Hot-plugged devices show up next time the
        // user re-enters the tab or clicks "Refresh" below.
        let devices = klipp_engine::audio_output::cached_output_devices();
        let host_name = klipp_engine::audio_output::host_name();

        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Audio hardware").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── OUTPUT DEVICE section ──
        Self::section_header(ui, "OUTPUT DEVICE", theme);

        // Build the dropdown label for the currently-selected device.
        // For "Default" (None), pick a label that includes the actual
        // device name cpal will use, so users see what's really happening.
        let default_device_label: String = devices
            .iter()
            .find(|d| d.is_default)
            .map(|d| format!("Default ({})", d.name))
            .unwrap_or_else(|| "Default (auto)".to_string());

        Self::setting_row_with_subtitle(ui, "Output device  \u{2022}  restart required", "Playback opens this device at launch; a device that has gone plays through the default.", theme, |ui| {
            let current_text = prefs
                .audio_hardware
                .output_device
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| default_device_label.clone());
            egui::ComboBox::from_id_salt("audio_device")
                .selected_text(current_text)
                .width(220.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            prefs.audio_hardware.output_device.is_none(),
                            &default_device_label,
                        )
                        .clicked()
                    {
                        prefs.audio_hardware.output_device = None;
                        *dirty = true;
                    }
                    if !devices.is_empty() {
                        ui.separator();
                    }
                    for d in &devices {
                        let selected = prefs
                            .audio_hardware
                            .output_device
                            .as_deref()
                            == Some(d.name.as_str());
                        if ui.selectable_label(selected, &d.name).clicked() {
                            prefs.audio_hardware.output_device = Some(d.name.clone());
                            *dirty = true;
                        }
                    }
                });
        });

        Self::setting_row(ui, "Sample rate", theme, |ui| {
            let rates = [(44100_u32, "44.1 kHz"), (48000, "48 kHz"), (96000, "96 kHz"), (192000, "192 kHz")];
            let current = rates.iter()
                .find(|(v, _)| *v == prefs.audio_hardware.sample_rate)
                .map(|(_, l)| *l).unwrap_or("48 kHz");
            egui::ComboBox::from_id_salt("sample_rate")
                .selected_text(current)
                .width(80.0)
                .show_ui(ui, |ui| {
                    for (val, label) in &rates {
                        if ui.selectable_value(&mut prefs.audio_hardware.sample_rate, *val, *label).changed() { *dirty = true; }
                    }
                });
        });

        Self::setting_row_with_subtitle(ui, "Buffer size", "Larger = more stable, higher latency", theme, |ui| {
            let sizes: &[u32] = &[64, 128, 256, 512, 1024, 2048];
            let current = format!("{}", prefs.audio_hardware.buffer_size);
            egui::ComboBox::from_id_salt("buffer_sz")
                .selected_text(&current)
                .width(60.0)
                .show_ui(ui, |ui| {
                    for &s in sizes {
                        if ui.selectable_value(&mut prefs.audio_hardware.buffer_size, s, format!("{s}")).changed() {
                            *dirty = true;
                        }
                    }
                });
        });

        // ── INPUT DEVICE section (for audio recording, #60) ──
        Self::section_header(ui, "RECORDING INPUT", theme);
        Self::setting_row_with_subtitle(
            ui,
            "Input device",
            "Used when arming a track and pressing Record.",
            theme,
            |ui| {
                let input_devices = klipp_engine::audio_input::enumerate_input_devices();
                let current_text = if prefs.audio_hardware.input_device.is_empty() {
                    // Pair "Default" with the actual OS default name so users
                    // can see what cpal will pick — mirrors the output picker.
                    input_devices
                        .iter()
                        .find(|(_, is_default)| *is_default)
                        .map(|(n, _)| format!("Default ({n})"))
                        .unwrap_or_else(|| "Default (auto)".to_string())
                } else {
                    prefs.audio_hardware.input_device.clone()
                };
                egui::ComboBox::from_id_salt("input_device")
                    .selected_text(current_text)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(
                                prefs.audio_hardware.input_device.is_empty(),
                                "Default",
                            )
                            .clicked()
                        {
                            prefs.audio_hardware.input_device.clear();
                            *dirty = true;
                        }
                        if !input_devices.is_empty() {
                            ui.separator();
                        }
                        for (name, _is_default) in &input_devices {
                            let selected = prefs.audio_hardware.input_device == *name;
                            if ui.selectable_label(selected, name).clicked() {
                                prefs.audio_hardware.input_device = name.clone();
                                *dirty = true;
                            }
                        }
                    });
            },
        );

        Self::setting_row_with_subtitle(
            ui,
            "System mute reminder",
            "Repeat notification when app audio is muted by the OS",
            theme,
            |ui| {
                let intervals: &[(u32, &str)] = &[
                    (0, "Once"),
                    (10, "10s"),
                    (15, "15s"),
                    (30, "30s"),
                    (60, "60s"),
                ];
                let current_label = intervals.iter()
                    .find(|(v, _)| *v == prefs.audio_hardware.mute_reminder_interval_secs)
                    .map(|(_, l)| *l)
                    .unwrap_or("15s");
                egui::ComboBox::from_id_salt("mute_reminder")
                    .selected_text(current_label)
                    .width(60.0)
                    .show_ui(ui, |ui| {
                        for (val, label) in intervals {
                            if ui.selectable_value(
                                &mut prefs.audio_hardware.mute_reminder_interval_secs,
                                *val,
                                *label,
                            ).changed() {
                                *dirty = true;
                            }
                        }
                    });
            },
        );

        // ── DETECTED HARDWARE section ──
        // Read-only diagnostics — what cpal actually sees on this system.
        // Useful for debugging device-not-found issues and for users who
        // want to know what their machine is offering.
        Self::section_header(ui, "DETECTED HARDWARE", theme);

        // `refreshed_devices` is filled when the user clicks the Refresh
        // button below; we use it as the override list for the rest of
        // the section so the new state appears immediately, not on the
        // next frame.
        let mut refreshed_devices: Option<Vec<klipp_engine::audio_output::AudioDeviceInfo>> = None;
        Self::setting_row_with_subtitle(
            ui,
            "Audio backend",
            // cpal has no push notifications for hot-plug, so the user
            // needs an explicit "I just plugged something in" button.
            "Click Refresh after plugging in a new device",
            theme,
            |ui| {
                if ui.button("Refresh").clicked() {
                    refreshed_devices =
                        Some(klipp_engine::audio_output::refresh_audio_devices_cache());
                }
                ui.label(
                    egui::RichText::new(host_name)
                        .size(12.0)
                        .color(theme.text_primary),
                );
            },
        );
        // Use refreshed list if available, else the cached one captured above.
        let devices: Vec<klipp_engine::audio_output::AudioDeviceInfo> =
            refreshed_devices.unwrap_or(devices);

        if devices.is_empty() {
            Self::setting_row_with_subtitle(
                ui,
                "Output devices",
                "No audio output devices detected",
                theme,
                |ui| {
                    ui.label(
                        egui::RichText::new("\u{2014}")
                            .size(12.0)
                            .color(theme.text_dim),
                    );
                },
            );
        } else {
            // Summary row: how many devices and which is the system default.
            let default_name = devices
                .iter()
                .find(|d| d.is_default)
                .map(|d| d.name.as_str())
                .unwrap_or("(none)");
            let count_label = format!(
                "{} device{} \u{00b7} default: {}",
                devices.len(),
                if devices.len() == 1 { "" } else { "s" },
                default_name,
            );
            Self::setting_row(ui, "Output devices", theme, |ui| {
                ui.label(
                    egui::RichText::new(count_label)
                        .size(11.0)
                        .color(theme.text_dim),
                );
            });

            // One row per device showing rate range, channel count, format.
            for d in devices {
                let label = if d.is_default {
                    format!("\u{25cf} {}", d.name) // bullet marks default
                } else {
                    format!("  {}", d.name)
                };
                // No reported range, or a range of exactly one rate:
                // either way there is a single number to show.
                let rate_text = if d.min_sample_rate == 0
                    || d.max_sample_rate == 0
                    || d.min_sample_rate == d.max_sample_rate
                {
                    format!("{} Hz", d.default_sample_rate)
                } else {
                    format!(
                        "{} Hz (range {}\u{2013}{})",
                        d.default_sample_rate, d.min_sample_rate, d.max_sample_rate
                    )
                };
                let subtitle = format!(
                    "{} \u{00b7} {} ch \u{00b7} {}",
                    rate_text, d.default_channels, d.default_format
                );
                Self::setting_row_with_subtitle(ui, &label, &subtitle, theme, |ui| {
                    if d.is_default {
                        ui.label(
                            egui::RichText::new("Default")
                                .size(crate::inspector::rows::TEXT)
                                .color(theme.text_secondary),
                        );
                    } else {
                        ui.label("");
                    }
                });
            }
        }
    }

    fn show_media_cache(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, cmds: &mut Vec<AppCommand>, theme: &KlippTheme) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Cache").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── LOCATIONS section ──
        Self::section_header(ui, "LOCATIONS", theme);

        // Helper: cache directory row with path display + Browse button
        let cache_dir_row = |ui: &mut egui::Ui, label: &str, path: &mut Option<std::path::PathBuf>, default_display: &str, dirty: &mut bool, _id: &str| {
            let row_h = crate::inspector::rows::ROW_H + 4.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            // Label
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(crate::inspector::rows::TEXT),
                theme.text_secondary,
            );
            // Browse button
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 80.0, rect.top() + 6.0),
                egui::vec2(66.0, 24.0),
            );
            let btn_resp = ui.allocate_rect(btn_rect, egui::Sense::click());
            let btn_bg = if btn_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(btn_rect, 4.0, btn_bg);
            ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
            ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, "Browse...", egui::FontId::proportional(11.0), theme.text_primary);
            // Path display (dim, before button)
            let display = path.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| default_display.to_string());
            ui.painter().text(
                egui::pos2(btn_rect.left() - 8.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                &display,
                egui::FontId::proportional(11.0),
                theme.text_dim,
            );
            let picker = egui::Id::new("prefs-cache-dir").with(label);
            if btn_resp.clicked() {
                let title = label.to_string();
                crate::file_dialog::pick_path(picker, move || {
                    rfd::FileDialog::new().set_title(title).pick_folder()
                });
            }
            if let Some(crate::file_dialog::Picked::Path(p)) = crate::file_dialog::take_path(picker) {
                *path = Some(p); *dirty = true;
            }
        };

        cache_dir_row(ui, "Thumbnail cache", &mut prefs.media_cache.filmstrip_cache_dir, "~/.cache/klipp/thumbnails", dirty, "thumb_dir");
        cache_dir_row(ui, "Audio cache", &mut prefs.media_cache.audio_cache_dir, "~/.cache/klipp/audio-cache", dirty, "wave_dir");

        // ── RENDER CACHE section ──
        Self::section_header(ui, "RENDER CACHE", theme);

        Self::setting_row(ui, "Smart render cache", theme, |ui| {
            let mut val = prefs.media_cache.enable_render_cache;
            if ui.add(Switch::new(&mut val, theme)).changed() {
                prefs.media_cache.enable_render_cache = val;
                cmds.push(AppCommand::SetRenderCacheEnabled(val));
                *dirty = true;
            }
        });

        // ── LIMITS section ──
        Self::section_header(ui, "LIMITS", theme);

        Self::setting_row_with_subtitle(ui, "Max cache size", "The render cache's cap: past it the least recently used frames go", theme, |ui| {
            let mut v = prefs.media_cache.max_cache_size_gb;
            if ui.add(egui::DragValue::new(&mut v).range(1..=500).speed(1.0).suffix(" GB")).changed() {
                prefs.media_cache.max_cache_size_gb = v; *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "Thumbnail resolution", "How wide a media item's thumbnail is made; wider is sharper on large tiles", theme, |ui| {
            let sizes = [(320_u32, "320 px"), (480, "480 px"), (640, "640 px"), (960, "960 px")];
            let current = sizes.iter()
                .find(|(v, _)| *v == prefs.media_cache.thumbnail_height)
                .map(|(_, l)| (*l).to_string())
                .unwrap_or_else(|| format!("{} px", prefs.media_cache.thumbnail_height));
            egui::ComboBox::from_id_salt("thumb_res")
                .selected_text(current)
                .width(80.0)
                .show_ui(ui, |ui| {
                    for (val, label) in &sizes {
                        if ui.selectable_value(&mut prefs.media_cache.thumbnail_height, *val, *label).changed() {
                            *dirty = true;
                        }
                    }
                });
        });

        Self::setting_row_with_subtitle(
            ui,
            "Filmstrip tile resolution",
            "Height of each filmstrip tile on the timeline. Higher = sharper at enlarged track heights, larger cache on disk and memory. 256 is enough for ~250px tracks without upscale blur; 384/512 is for very tall tracks or 4K displays.",
            theme,
            |ui| {
                // Dropdown covers the common presets; the DragValue below
                // accepts any custom value from 32..=1024 so "configurable"
                // means any integer, not just five dropdown choices.
                let sizes = [
                    (48_u32,  "48px (Draft)"),
                    (72,      "72px"),
                    (96,      "96px (Default)"),
                    (128,     "128px"),
                    (192,     "192px"),
                    (256,     "256px (HD)"),
                    (384,     "384px"),
                    (512,     "512px (Max preset)"),
                ];
                let current_label = sizes.iter()
                    .find(|(v, _)| *v == prefs.media_cache.filmstrip_height)
                    .map(|(_, l)| *l)
                    .unwrap_or("Custom");
                egui::ComboBox::from_id_salt("filmstrip_res")
                    .selected_text(current_label)
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for (val, label) in &sizes {
                            if ui.selectable_value(&mut prefs.media_cache.filmstrip_height, *val, *label).changed() {
                                *dirty = true;
                            }
                        }
                    });
                ui.add_space(6.0);
                let mut v = prefs.media_cache.filmstrip_height;
                if ui
                    .add(
                        egui::DragValue::new(&mut v)
                            .range(32..=1024)
                            .speed(1.0)
                            .suffix(" px"),
                    )
                    .changed()
                {
                    prefs.media_cache.filmstrip_height = v;
                    *dirty = true;
                }
            },
        );

        Self::setting_row(ui, "Skim transcode quality", theme, |ui| {
            // Quality maps to H.264 CRF / NVENC QP: lower number =
            // higher quality + larger file. 30 is the FCP ProRes
            // Proxy / DaVinci DNxHR LB equivalent — industry
            // standard for a hover-preview proxy.
            let qualities = [
                (24_u32, "High"),
                (30, "Standard"),
                (34, "Low"),
                (38, "Draft"),
            ];
            let current = qualities.iter()
                .find(|(v, _)| *v == prefs.media_cache.skim_transcode_quality)
                .map(|(_, l)| *l).unwrap_or("Standard");
            // Layout is right-to-left inside the widget area — add
            // the ComboBox FIRST so it lands on the right, then the
            // ⓘ icon lands to its left.
            egui::ComboBox::from_id_salt("skim_q")
                .selected_text(current)
                .width(90.0)
                .show_ui(ui, |ui| {
                    for (val, label) in &qualities {
                        if ui.selectable_value(&mut prefs.media_cache.skim_transcode_quality, *val, *label).changed() {
                            *dirty = true;
                        }
                    }
                });
            ui.add_space(6.0);
            // Small info badge — shows a size-expectations cheat
            // sheet on hover. Users can gauge disk cost before
            // changing the preset, which the dropdown's single
            // word label can't convey.
            ui.label(
                egui::RichText::new("\u{24D8}")  // ⓘ circled small letter i
                    .size(13.0)
                    .color(theme.text_dim),
            ).on_hover_text(
                "Skim is a hover-preview proxy: 1080p H.264 with a \
                 short GOP for fast per-frame seek. Source resolution \
                 barely affects size — skim always downscales to 1080p. \
                 Frame rate and duration are what matter.\n\n\
                 Approximate size per hour of source (Standard):\n\
                 \u{2022}  1080p 30 fps   ~ 4 GB\n\
                 \u{2022}  1080p 60 fps   ~ 8 GB\n\
                 \u{2022}  4K   30 fps   ~ 6 GB  (downscaled)\n\
                 \u{2022}  4K   60 fps   ~12 GB  (downscaled)\n\
                 \u{2022}  4K HDR 60 fps ~14 GB  (tonemap adds ~20%)\n\n\
                 Presets:\n\
                 \u{2022}  High     — ~2x Standard, near-transparent quality\n\
                 \u{2022}  Standard — typical proxy quality\n\
                 \u{2022}  Low      — ~0.5x Standard, visible compression\n\
                 \u{2022}  Draft    — ~0.25x Standard, blocky but viable\n\n\
                 Cache is regenerated on demand — delete anytime in the \
                 Cache panel to reclaim space.",
            );
        });

        // ── CACHE SIZES section ──
        Self::section_header(ui, "CACHE SIZES", theme);

        let klipp_cache = dirs::cache_dir().unwrap_or_default().join("klipp");
        // The engine's render cache is always here; it does not read
        // `render_cache_dir`, so measuring or clearing that would miss it.
        let render_dir = klipp_cache.join("render");
        let thumb_dir = prefs.media_cache.filmstrip_cache_dir.clone()
            .unwrap_or_else(|| klipp_cache.join("thumbnails"));
        // The audio cache pref controls the engine's audio_cache_dir
        // (`klipp-media/src/disk_cache.rs::audio_cache_dir`) which defaults
        // to `audio-cache`, NOT `waveforms`. Previous default of "waveforms"
        // was a stale path that always sized to 0 and never cleared the
        // real ~tens-of-GB PCM data.
        let wave_dir = prefs.media_cache.audio_cache_dir.clone()
            .unwrap_or_else(|| klipp_cache.join("audio-cache"));
        // Filmstrip .strip binaries — separate from `thumbnails`. Engine
        // path: `klipp-media/src/disk_cache.rs::filmstrip_dir`.
        let filmstrip_dir = klipp_cache.join("filmstrips");
        // Pre-computed audio peak data — `klipp-media/src/disk_cache.rs:285`.
        let audio_peaks_dir = klipp_cache.join("audio-peaks");
        let skim_dir = klipp_cache.join("skim");
        let proxy_dir = prefs.proxy.proxy_cache_dir.clone()
            .unwrap_or_else(|| klipp_cache.join("proxies"));

        let dir_size = |path: &std::path::Path| -> u64 {
            walkdir::WalkDir::new(path).into_iter()
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        };

        let render_size = dir_size(&render_dir);
        let thumb_size = dir_size(&thumb_dir);
        let wave_size = dir_size(&wave_dir);
        let filmstrip_size = dir_size(&filmstrip_dir);
        let audio_peaks_size = dir_size(&audio_peaks_dir);
        let skim_size = dir_size(&skim_dir);
        let proxy_size = dir_size(&proxy_dir);
        let total_size = render_size + thumb_size + wave_size + filmstrip_size
            + audio_peaks_size + skim_size + proxy_size;

        let format_size = |bytes: u64| -> String {
            if bytes >= 1_073_741_824 { format!("{:.1} GB", bytes as f64 / 1_073_741_824.0) }
            else if bytes >= 1_048_576 { format!("{:.1} MB", bytes as f64 / 1_048_576.0) }
            else if bytes >= 1024 { format!("{} KB", bytes / 1024) }
            else { format!("{} B", bytes) }
        };

        // Renders a "Clear" button at the layout-managed position and
        // returns whether it was clicked. Disk wipe + regeneration dispatch
        // is handled per-row below so each row can push its own regen
        // command (audio, filmstrip, skim) — without that, wiping caches
        // leaves the in-memory MediaPool pointing at deleted files and
        // playback either uses the slow ffmpeg fallback or goes silent.
        let clear_btn = |ui: &mut egui::Ui, label: &str| -> bool {
            let resp = ui.allocate_response(egui::vec2(56.0, 24.0), egui::Sense::click());
            let btn_rect = resp.rect;
            let bg = if resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(btn_rect, 4.0, bg);
            ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0_f32, theme.accent));
            ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(11.0), theme.accent);
            resp.clicked()
        };
        let wipe_dir = |dir: &std::path::Path| {
            let _ = std::fs::remove_dir_all(dir);
            let _ = std::fs::create_dir_all(dir);
        };

        Self::setting_row(ui, "Skim preview height", theme, |ui| {
            // How tall a skim frame is decoded, for the hover and scrub
            // preview and for the scope render fed from it. Capped by the
            // source and by the skim file, which is never taller than
            // 1080, so this only ever lowers.
            // The reducer halves rather than scales, so only powers of two
            // below the source are reachable: asking a 1080 skim for 720
            // gives 540. Offer what it can actually deliver, labelled by
            // the picture rather than by a number that would be a lie.
            let heights = [(1080_u32, "Full"), (540, "Half"), (270, "Quarter")];
            let current = heights
                .iter()
                .find(|(v, _)| *v == prefs.playback.skim_resolution)
                .map(|(_, l)| *l)
                .unwrap_or("Full");
            // Right-to-left inside the widget area: the ComboBox first so
            // it lands on the right, then the badge to its left.
            egui::ComboBox::from_id_salt("skim_h")
                .selected_text(current)
                .width(90.0)
                .show_ui(ui, |ui| {
                    for (val, label) in &heights {
                        if ui.selectable_value(&mut prefs.playback.skim_resolution, *val, *label).changed() {
                            *dirty = true;
                        }
                    }
                });
            ui.add_space(6.0);
            ui.label(egui::RichText::new("\u{24D8}").size(13.0).color(theme.text_dim)).on_hover_text(
                "The height a hover or scrub preview frame is decoded at, \
                 and the height the scopes read while you skim.\n\n\
                 Lower is smoother: half the height is roughly a quarter \
                 of the pixels to decode and read back, so more preview \
                 frames land while you drag.\n\n\
                 It does not affect the frame you stop on, playback, or \
                 export — those are always full size. It does affect how \
                 dense the waveform and parade look while skimming.\n\n\
                 \u{2022}  Full    — what skimming has always shown, \
                 the default\n\
                 \u{2022}  Half    — a cheaper decode, sparser scopes\n\
                 \u{2022}  Quarter — cheapest, for slow media",
            );
        });

        Self::setting_row_with_subtitle(ui, "Render cache", &format_size(render_size), theme, |ui| {
            if clear_btn(ui, "Clear") { wipe_dir(&render_dir); }
        });
        Self::setting_row_with_subtitle(ui, "Poster thumbnails", &format_size(thumb_size), theme, |ui| {
            if clear_btn(ui, "Clear") {
                wipe_dir(&thumb_dir);
                cmds.push(AppCommand::RegenerateFilmstripsForAll);
            }
        });
        Self::setting_row_with_subtitle(ui, "Filmstrip strips", &format_size(filmstrip_size), theme, |ui| {
            if clear_btn(ui, "Clear") {
                wipe_dir(&filmstrip_dir);
                cmds.push(AppCommand::RegenerateFilmstripsForAll);
            }
        });
        Self::setting_row_with_subtitle(ui, "Audio cache (PCM)", &format_size(wave_size), theme, |ui| {
            if clear_btn(ui, "Clear") {
                wipe_dir(&wave_dir);
                cmds.push(AppCommand::RegenerateAudioCacheForAll);
            }
        });
        Self::setting_row_with_subtitle(ui, "Audio peaks", &format_size(audio_peaks_size), theme, |ui| {
            if clear_btn(ui, "Clear") {
                wipe_dir(&audio_peaks_dir);
                // Peaks are written by the same conform pass that writes PCM.
                cmds.push(AppCommand::RegenerateAudioCacheForAll);
            }
        });
        Self::setting_row_with_subtitle(ui, "Skim transcodes", &format_size(skim_size), theme, |ui| {
            if clear_btn(ui, "Clear") {
                wipe_dir(&skim_dir);
                cmds.push(AppCommand::RegenerateSkimTranscodesForAll);
            }
        });
        Self::setting_row_with_subtitle(ui, "Proxy files", &format_size(proxy_size), theme, |ui| {
            // Proxies are large and slow to generate — auto-regen would be
            // surprising. User can re-trigger via media browser context menu.
            if clear_btn(ui, "Clear") { wipe_dir(&proxy_dir); }
        });

        ui.add_space(4.0);

        // Orphan cleanup
        Self::section_header(ui, "CLEANUP", theme);

        Self::setting_row_with_subtitle(ui, "Orphaned cache files", "Files not used by any project", theme, |ui| {
            let resp = ui.allocate_response(egui::vec2(100.0, 24.0), egui::Sense::click());
            let btn_rect = resp.rect;
            let bg = if resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(btn_rect, 4.0, bg);
            ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0_f32, theme.accent));
            ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, "Scan & Clean", egui::FontId::proportional(11.0), theme.accent);
            if resp.clicked() {
                cmds.push(AppCommand::CleanOrphanedCaches);
            }
        });

        ui.add_space(4.0);

        // Clear all caches
        Self::setting_row_with_subtitle(ui, "All caches", &format_size(total_size), theme, |ui| {
            let resp = ui.allocate_response(egui::vec2(70.0, 24.0), egui::Sense::click());
            let btn_rect = resp.rect;
            let bg = if resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(btn_rect, 4.0, bg);
            ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0_f32, theme.marker_error));
            ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, "Clear all", egui::FontId::proportional(11.0), theme.marker_error);
            if resp.clicked() {
                // Stage the destructive action behind a confirmation modal
                // so a stray click doesn't nuke tens of GB of regenerable
                // cache. Per-row "Clear" buttons stay one-click — single
                // categories are deliberate and recover quickly.
                self.clear_all_confirm = Some(ClearAllConfirm {
                    dirs: vec![
                        render_dir.clone(), thumb_dir.clone(), filmstrip_dir.clone(),
                        wave_dir.clone(), audio_peaks_dir.clone(), skim_dir.clone(),
                        proxy_dir.clone(),
                    ],
                    total_bytes: total_size,
                });
            }
        });
    }

    /// Commands to push when the "Clear all caches" modal's Confirm button
    /// fires. These re-trigger generation of audio / filmstrips / skim for
    /// every existing media item in the pool, so wiping caches doesn't
    /// leave the project pointing at deleted files. Proxies are not
    /// auto-regenerated — they're large and slow, user opts in via the
    /// media browser context menu.
    pub fn clear_all_regen_commands() -> Vec<AppCommand> {
        vec![
            AppCommand::RegenerateAudioCacheForAll,
            AppCommand::RegenerateFilmstripsForAll,
            AppCommand::RegenerateSkimTranscodesForAll,
        ]
    }

    /// "Clear all caches" confirmation modal. Renders only when
    /// `clear_all_confirm` is Some. Confirm performs the destructive
    /// action; Cancel dismisses without deleting anything.
    fn show_clear_all_confirm(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>, theme: &KlippTheme) {
        let Some(confirm) = self.clear_all_confirm.as_ref() else { return };
        let total_str = if confirm.total_bytes >= 1_073_741_824 {
            format!("{:.1} GB", confirm.total_bytes as f64 / 1_073_741_824.0)
        } else if confirm.total_bytes >= 1_048_576 {
            format!("{:.1} MB", confirm.total_bytes as f64 / 1_048_576.0)
        } else {
            format!("{} KB", confirm.total_bytes / 1024)
        };
        let mut close = false;
        let mut do_clear = false;
        egui::Window::new("Delete all caches?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.add_space(4.0);
                ui.label(format!(
                    "This will permanently delete {total_str} of cached data \
                    (render frames, thumbnails, filmstrips, audio cache, audio peaks, \
                    skim transcodes, proxy files).",
                ));
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Caches are regenerated automatically as needed, \
                        but rebuilding them takes time on next playback / export.",
                    )
                    .color(theme.text_dim)
                    .size(11.0),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() { close = true; }
                    ui.add_space(8.0);
                    let delete_btn = egui::Button::new(
                        egui::RichText::new("Delete all").color(egui::Color32::WHITE),
                    )
                    .fill(theme.marker_error);
                    if ui.add(delete_btn).clicked() {
                        do_clear = true;
                        close = true;
                    }
                });
            });
        if do_clear {
            if let Some(c) = self.clear_all_confirm.as_ref() {
                for d in &c.dirs {
                    let _ = std::fs::remove_dir_all(d);
                    let _ = std::fs::create_dir_all(d);
                }
            }
            // Re-trigger generation for every existing media item so the
            // pool stops referencing deleted files. Audio is the most
            // user-visible — without this, timeline playback loses sound
            // until the user re-imports each clip.
            cmds.extend(Self::clear_all_regen_commands());
        }
        if close {
            self.clear_all_confirm = None;
        }
    }

    fn show_color(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, theme: &KlippTheme) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Colour").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── LUTs section ──
        // Working colorspace lives on the project (Project.working_colorspace),
        // not on user prefs — it's a creative decision that travels with the
        // project, the way every professional NLE handles it.
        Self::section_header(ui, "LUTs", theme);

        // This monitor's calibration, applied last; a project's look to
        // view through is its viewing LUT, in Colour Management (#466).
        Self::setting_row_with_subtitle(ui, "Calibration LUT", "This monitor's calibration, applied to the viewer last (SDR): never in exports, scopes or measurements", theme, |ui| {
            let display = prefs.color.viewer_lut_path.as_ref()
                .map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string())
                .unwrap_or_else(|| "None".into());
            // Collected outside the dropdown: it has closed by the time
            // the picker answers.
            let picker = egui::Id::new("prefs-viewer-lut");
            if let Some(crate::file_dialog::Picked::Path(p)) = crate::file_dialog::take_path(picker) {
                prefs.color.viewer_lut_path = Some(p); *dirty = true;
            }
            egui::ComboBox::from_id_salt("viewer_lut")
                .selected_text(&display)
                .width(90.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(prefs.color.viewer_lut_path.is_none(), "None").clicked() {
                        prefs.color.viewer_lut_path = None; *dirty = true;
                    }
                    if ui.selectable_label(false, "Browse...").clicked() {
                        crate::file_dialog::pick_path(picker, || {
                            rfd::FileDialog::new().set_title("Calibration LUT").add_filter("LUT", &["cube", "3dl"]).pick_file()
                        });
                    }
                });
        });

        Self::setting_row(ui, "SDR viewer transfer", theme, |ui| {
            use klipp_core::SdrTransfer;
            egui::ComboBox::from_id_salt("sdr_transfer")
                .selected_text(match prefs.color.sdr_display_transfer {
                    SdrTransfer::Srgb => "sRGB",
                    SdrTransfer::Bt1886 => "BT.1886 (gamma 2.4)",
                })
                .width(160.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(&mut prefs.color.sdr_display_transfer, SdrTransfer::Srgb, "sRGB")
                        .on_hover_text("A desktop monitor. The viewer encodes the sRGB piecewise curve.")
                        .changed()
                    {
                        *dirty = true;
                    }
                    if ui
                        .selectable_value(&mut prefs.color.sdr_display_transfer, SdrTransfer::Bt1886, "BT.1886 (gamma 2.4)")
                        .on_hover_text("A calibrated grading monitor. Rec.709 deliveries are mastered on gamma 2.4; the shadows match what the delivery will show there.")
                        .changed()
                    {
                        *dirty = true;
                    }
                });
        });
        Self::setting_row(ui, "LUT interpolation", theme, |ui| {
            use crate::prefs::LutInterpolation;
            egui::ComboBox::from_id_salt("lut_interp")
                .selected_text(match prefs.color.lut_interpolation {
                    LutInterpolation::Trilinear => "Trilinear",
                    LutInterpolation::Tetrahedral => "Tetrahedral",
                })
                .width(100.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(&mut prefs.color.lut_interpolation, LutInterpolation::Trilinear, "Trilinear")
                        .on_hover_text("Faster in the viewer. Bends the straight lines a LUT draws, which shows as a shifted hue or a banded gradient.")
                        .changed()
                    {
                        *dirty = true;
                    }
                    if ui
                        .selectable_value(&mut prefs.color.lut_interpolation, LutInterpolation::Tetrahedral, "Tetrahedral")
                        .on_hover_text("Follows the LUT. What exports always use, whatever this is set to.")
                        .changed()
                    {
                        *dirty = true;
                    }
                });
        });

        // ── SCOPES section ──
        Self::section_header(ui, "SCOPES", theme);

        Self::setting_row(ui, "Scope refresh rate", theme, |ui| {
            let rates = [(16_u32, "60 fps"), (24, "42 fps"), (32, "30 fps"), (42, "24 fps"), (66, "15 fps")];
            let current = rates.iter()
                .find(|(v, _)| *v == prefs.color.scope_update_interval_ms)
                .map(|(_, l)| *l).unwrap_or("24 fps");
            egui::ComboBox::from_id_salt("scope_fps")
                .selected_text(current)
                .width(70.0)
                .show_ui(ui, |ui| {
                    for (val, label) in &rates {
                        if ui.selectable_value(&mut prefs.color.scope_update_interval_ms, *val, *label).changed() { *dirty = true; }
                    }
                });
        });

        Self::setting_row(ui, "Sampling quality", theme, |ui| {
            use klipp_core::ScopeQuality;
            egui::ComboBox::from_id_salt("scope_q")
                .selected_text(match prefs.color.scope_quality {
                    ScopeQuality::Low => "Fast (every 4px)",
                    ScopeQuality::Medium => "Standard (every 2px)",
                    ScopeQuality::High => "Full (every pixel)",
                })
                .width(150.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.color.scope_quality, ScopeQuality::Low, "Fast (every 4px)").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.color.scope_quality, ScopeQuality::Medium, "Standard (every 2px)").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.color.scope_quality, ScopeQuality::High, "Full (every pixel)").changed() { *dirty = true; }
                });
        });

        Self::setting_row(ui, "Printer point", theme, |ui| {
            let mut v = prefs.color.printer_point;
            if ui.add(egui::DragValue::new(&mut v).range(0.001..=0.1).speed(0.0005).fixed_decimals(4))
                .on_hover_text("How far one printer-light key moves a channel's offset, as a share of the signal. The keys step red, green, blue or all three by it.")
                .changed()
            {
                prefs.color.printer_point = v; *dirty = true;
            }
        });

        Self::setting_row(ui, "Vectorscope skin angle", theme, |ui| {
            let mut v = prefs.color.vectorscope_skin_angle;
            if ui.add(egui::DragValue::new(&mut v).range(0.0..=360.0).speed(0.5).suffix("\u{00b0}").fixed_decimals(0)).changed() {
                prefs.color.vectorscope_skin_angle = v; *dirty = true;
            }
        });

        // ── NODE GRAPH section ──
        // The node graph is the editor surface for the colour pipeline,
        // so its prefs live in this page. Snap-on-connect makes wiring
        // forgiving on a trackpad / high-DPI display where a 9 px port
        // hit-zone can feel small; can be disabled for users who want
        // click-precise drops.
        Self::section_header(ui, "NODE GRAPH", theme);

        Self::setting_row_with_subtitle(
            ui,
            "Snap to ports while connecting",
            "Highlight + lock the drag wire to the nearest input port",
            theme,
            |ui| {
                if ui.add(Switch::new(&mut prefs.color.port_snap_enabled, theme)).changed() {
                    *dirty = true;
                }
            },
        );

        Self::setting_row_with_subtitle(
            ui,
            "Snap radius",
            "How close the cursor must get before the wire snaps (screen pixels)",
            theme,
            |ui| {
                let enabled = prefs.color.port_snap_enabled;
                ui.add_enabled_ui(enabled, |ui| {
                    let mut v = prefs.color.port_snap_radius_px;
                    if ui.add(
                        egui::DragValue::new(&mut v)
                            .range(4.0..=32.0)
                            .speed(0.5)
                            .suffix(" px")
                            .fixed_decimals(0),
                    ).changed() {
                        prefs.color.port_snap_radius_px = v;
                        *dirty = true;
                    }
                });
            },
        );
    }

    fn show_title(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, theme: &KlippTheme) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Titles").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── TEXT DEFAULTS section: a new title's text ──
        Self::section_header(ui, "TEXT DEFAULTS", theme);

        Self::setting_row_with_subtitle(ui, "Default font", "A font family installed here; empty keeps the title's own", theme, |ui| {
            let default_font = klipp_timeline::TextBox::default().font_name;
            if ui.add(egui::TextEdit::singleline(&mut prefs.title.default_font).hint_text(default_font).desired_width(160.0)).changed() {
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Default size", theme, |ui| {
            let mut v = prefs.title.default_size_pt;
            if ui.add(egui::DragValue::new(&mut v).range(8.0..=400.0).speed(0.5).suffix(" pt").fixed_decimals(0)).changed() {
                prefs.title.default_size_pt = v; *dirty = true;
            }
        });

        Self::setting_row(ui, "Default colour", theme, |ui| {
            let mut c = egui::Color32::from_rgb(prefs.title.default_color[0], prefs.title.default_color[1], prefs.title.default_color[2]);
            if ui.color_edit_button_srgba(&mut c).changed() {
                prefs.title.default_color = [c.r(), c.g(), c.b()];
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Default alignment", theme, |ui| {
            use crate::prefs::TextAlignment;
            egui::ComboBox::from_id_salt("title_align")
                .selected_text(match prefs.title.default_alignment {
                    TextAlignment::Left => "Left",
                    TextAlignment::Center => "Center",
                    TextAlignment::Right => "Right",
                })
                .width(80.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.title.default_alignment, TextAlignment::Left, "Left").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.title.default_alignment, TextAlignment::Center, "Center").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.title.default_alignment, TextAlignment::Right, "Right").changed() { *dirty = true; }
                });
        });

        // ── BEHAVIOR section ──
        Self::section_header(ui, "BEHAVIOR", theme);

        Self::setting_row(ui, "Default duration", theme, |ui| {
            let mut v = prefs.title.default_duration_secs;
            if ui.add(egui::DragValue::new(&mut v).range(0.5..=30.0).speed(0.1).suffix(" s").fixed_decimals(1)).changed() {
                prefs.title.default_duration_secs = v; *dirty = true;
            }
        });

    }

    /// The page's name at its top.
    fn page_title(ui: &mut egui::Ui, title: &str, theme: &KlippTheme) {
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new(title).strong().size(16.0).color(theme.text_primary));
        });
    }

    fn show_timeline(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, theme: &KlippTheme) {
        Self::page_title(ui, "Timeline", theme);
        Self::section_header(ui, "CLIP COLOURS", theme);
        let mut colors_changed = false;
        for (i, color) in prefs.timeline.clip_colors.iter_mut().enumerate() {
            Self::setting_row(ui, &format!("Colour {}", i + 1), theme, |ui| {
                let mut c = egui::Color32::from_rgb(color[0], color[1], color[2]);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    *color = [c.r(), c.g(), c.b()];
                    colors_changed = true;
                }
            });
        }
        if colors_changed { *dirty = true; }

        let dim_row = |ui: &mut egui::Ui, label: &str, val: &mut f32, min: f32, max: f32, suffix: &str, dirty: &mut bool| {
            Self::setting_row(ui, label, theme, |ui| {
                let mut v = *val;
                if ui.add(egui::DragValue::new(&mut v).range(min..=max).speed(0.5).suffix(suffix)).changed() {
                    *val = v; *dirty = true;
                }
            });
        };

        Self::section_header(ui, "DIMENSIONS", theme);
        dim_row(ui, "Clip gap", &mut prefs.timeline.clip_gap_px, 0.0, 20.0, " px", dirty);
        dim_row(ui, "Trim handle", &mut prefs.timeline.trim_handle_width, 6.0, 30.0, " px", dirty);
        dim_row(ui, "Overview bar", &mut prefs.timeline.overview_bar_height, 12.0, 48.0, " px", dirty);
        dim_row(ui, "Min track height", &mut prefs.timeline.min_track_height, 16.0, 64.0, " px", dirty);
        dim_row(ui, "Max track height", &mut prefs.timeline.max_track_height, 100.0, 600.0, " px", dirty);
        dim_row(ui, "Text track height", &mut prefs.timeline.text_track_height, 20.0, 80.0, " px", dirty);

        Self::section_header(ui, "SELECTION", theme);
        dim_row(ui, "Selection border width", &mut prefs.timeline.clip_selection_border_width, 1.0, 8.0, " px", dirty);
    }

    fn show_viewer(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, theme: &KlippTheme) {
        Self::page_title(ui, "Viewer", theme);
        Self::section_header(ui, "SAFE AREAS", theme);
        Self::setting_row(ui, "Action safe", theme, |ui| {
            let mut v = prefs.viewer.action_safe_pct * 100.0;
            if ui.add(egui::Slider::new(&mut v, 70.0..=100.0).suffix("%")).changed() {
                prefs.viewer.action_safe_pct = v / 100.0; *dirty = true;
            }
        });
        Self::setting_row(ui, "Title safe", theme, |ui| {
            let mut v = prefs.viewer.title_safe_pct * 100.0;
            if ui.add(egui::Slider::new(&mut v, 50.0..=100.0).suffix("%")).changed() {
                prefs.viewer.title_safe_pct = v / 100.0; *dirty = true;
            }
        });

        // A level's IRE in BT.709 legal range: 16 is 0 IRE, 235 is 100.
        let ire = |v: u8| (v as f32 - 16.0) / 2.19;
        Self::section_header(ui, "ZEBRAS", theme);
        Self::setting_row(ui, "High threshold (clipping)", theme, |ui| {
            let mut v = prefs.viewer.zebra_high_threshold;
            ui.label(egui::RichText::new(format!("{:.0}% IRE", ire(v))).color(theme.text_dim).size(11.0));
            if ui.add(egui::DragValue::new(&mut v).range(128..=255).speed(1.0)).changed() {
                prefs.viewer.zebra_high_threshold = v; *dirty = true;
            }
        });
        Self::setting_row(ui, "Low threshold (underexposure)", theme, |ui| {
            let mut v = prefs.viewer.zebra_low_threshold;
            ui.label(egui::RichText::new(format!("{:.0}% IRE", ire(v))).color(theme.text_dim).size(11.0));
            if ui.add(egui::DragValue::new(&mut v).range(0..=128).speed(1.0)).changed() {
                prefs.viewer.zebra_low_threshold = v; *dirty = true;
            }
        });

        Self::section_header(ui, "FOCUS PEAKING", theme);
        Self::setting_row(ui, "Sensitivity", theme, |ui| {
            let mut v = prefs.viewer.focus_peaking_sensitivity;
            if ui.add(egui::Slider::new(&mut v, 0.0..=1.0)).changed() {
                prefs.viewer.focus_peaking_sensitivity = v; *dirty = true;
            }
        });
        Self::setting_row(ui, "Colour", theme, |ui| {
            let pc = &mut prefs.viewer.focus_peaking_color;
            let mut c = egui::Color32::from_rgb(pc[0], pc[1], pc[2]);
            if ui.color_edit_button_srgba(&mut c).changed() {
                *pc = [c.r(), c.g(), c.b()];
                *dirty = true;
            }
        });

        Self::section_header(ui, "FALSE COLOUR", theme);
        ui.horizontal(|ui| {
            ui.add_space(crate::inspector::rows::ROW_PAD);
            ui.vertical(|ui| Self::show_false_color_editor(ui, prefs, dirty, theme));
        });

        Self::section_header(ui, "OVERLAYS", theme);
        Self::setting_row(ui, "Grid colour", theme, |ui| {
            let gc = &mut prefs.viewer.grid_color;
            let mut c = egui::Color32::from_rgba_unmultiplied(gc[0], gc[1], gc[2], gc[3]);
            if ui.color_edit_button_srgba(&mut c).changed() {
                *gc = [c.r(), c.g(), c.b(), c.a()];
                *dirty = true;
            }
        });
        Self::setting_row(ui, "Checkerboard tile", theme, |ui| {
            let mut v = prefs.viewer.checkerboard_tile_size;
            if ui.add(egui::DragValue::new(&mut v).range(4.0..=32.0).speed(0.5).suffix(" px")).changed() {
                prefs.viewer.checkerboard_tile_size = v; *dirty = true;
            }
        });
        Self::setting_row(ui, "Transform handle size", theme, |ui| {
            let mut v = prefs.viewer.transform_handle_size;
            if ui.add(egui::DragValue::new(&mut v).range(4.0..=20.0).speed(0.5).suffix(" px")).changed() {
                prefs.viewer.transform_handle_size = v; *dirty = true;
            }
        });
        Self::setting_row(ui, "Fullscreen overlay timeout", theme, |ui| {
            let mut v = prefs.viewer.fullscreen_overlay_timeout_secs;
            if ui.add(egui::DragValue::new(&mut v).range(1.0..=10.0).speed(0.1).suffix(" sec")).changed() {
                prefs.viewer.fullscreen_overlay_timeout_secs = v; *dirty = true;
            }
        });
    }

    fn show_false_color_editor(ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, theme: &KlippTheme) {
        use crate::prefs::{FalseColorPalette, FalseColorStop};

        let built_ins = FalseColorPalette::built_ins();
        let current_name = prefs.viewer.false_color_palette.name.clone();

        // Determine if current palette matches a built-in
        let is_built_in = built_ins.contains(&prefs.viewer.false_color_palette);
        let display_name = if is_built_in { current_name.as_str() } else { "Custom" };

        // Preset dropdown
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Preset").size(crate::inspector::rows::TEXT).color(theme.text_secondary));
            egui::ComboBox::from_id_salt("fc_preset")
                .selected_text(display_name)
                .width(180.0)
                .show_ui(ui, |ui| {
                    for preset in &built_ins {
                        let selected = prefs.viewer.false_color_palette == *preset;
                        if ui.selectable_label(selected, &preset.name).clicked() {
                            prefs.viewer.false_color_palette = preset.clone();
                            *dirty = true;
                        }
                    }
                });

            // Reset to preset button
            if let Some(matching) = built_ins.iter().find(|p| p.name == current_name) {
                let differs = *matching != prefs.viewer.false_color_palette;
                if ui.add_enabled(differs, egui::Button::new("Reset to preset")).clicked() {
                    prefs.viewer.false_color_palette = matching.clone();
                    *dirty = true;
                }
            }
        });

        ui.add_space(4.0);

        // Validation warnings
        let overlaps = prefs.viewer.false_color_palette.overlapping_stops();
        let unmapped = prefs.viewer.false_color_palette.unmapped_count();
        if !overlaps.is_empty() {
            ui.colored_label(theme.marker_error,
                format!("{} overlapping stop pair(s) — resolve manually", overlaps.len()));
        }
        if unmapped > 0 {
            ui.colored_label(theme.marker_warning,
                format!("{unmapped} luma value(s) unmapped (will show source colour)"));
        }

        ui.add_space(4.0);

        // Column headers
        let dim_text = theme.text_dim;
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("#").color(dim_text).size(10.0));
            ui.add_space(12.0);
            ui.label(egui::RichText::new("Min").color(dim_text).size(10.0));
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Max").color(dim_text).size(10.0));
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Colour").color(dim_text).size(10.0));
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Label").color(dim_text).size(10.0));
        });

        // Determine which stops overlap (for red highlighting)
        let overlap_set: std::collections::HashSet<usize> = overlaps.iter()
            .flat_map(|(a, b)| [*a, *b])
            .collect();

        // Stop rows
        let mut remove_idx = None;
        let stop_count = prefs.viewer.false_color_palette.stops.len();
        let mut any_changed = false;
        for i in 0..stop_count {
            let stop = &mut prefs.viewer.false_color_palette.stops[i];
            let is_overlap = overlap_set.contains(&i);
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                let num_text = format!("{}", i + 1);
                if is_overlap {
                    ui.label(egui::RichText::new(&num_text).color(theme.marker_error).size(11.0));
                } else {
                    ui.label(egui::RichText::new(&num_text).color(dim_text).size(11.0));
                }
                ui.add_space(4.0);

                // Luma min
                let mut min_val = stop.luma_min;
                if ui.add(egui::DragValue::new(&mut min_val).range(0..=254).speed(1.0)).changed() {
                    stop.luma_min = min_val;
                    any_changed = true;
                }

                // Luma max
                let mut max_val = stop.luma_max;
                let max_lower = stop.luma_min.saturating_add(1);
                if ui.add(egui::DragValue::new(&mut max_val).range(max_lower..=255).speed(1.0)).changed() {
                    stop.luma_max = max_val;
                    any_changed = true;
                }

                // Color swatch
                let mut c = egui::Color32::from_rgb(stop.color[0], stop.color[1], stop.color[2]);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    stop.color = [c.r(), c.g(), c.b()];
                    any_changed = true;
                }

                // Label
                let mut label = stop.label.clone();
                let resp = ui.add(egui::TextEdit::singleline(&mut label).desired_width(120.0).char_limit(24));
                if resp.changed() {
                    stop.label = label;
                    any_changed = true;
                }

                // Delete button (disabled if only 1 stop)
                if ui.add_enabled(stop_count > 1, egui::Button::new("\u{2212}").min_size(egui::vec2(20.0, 18.0))).clicked() {
                    remove_idx = Some(i);
                    any_changed = true;
                }
            });
        }

        // Handle removal
        if let Some(idx) = remove_idx {
            prefs.viewer.false_color_palette.stops.remove(idx);
        }

        // Add stop button
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            if ui.button("+ Add stop").clicked() {
                // Find the first uncovered luma value, or append at 254–255
                let mut covered = [false; 256];
                for s in &prefs.viewer.false_color_palette.stops {
                    for l in s.luma_min..=s.luma_max {
                        covered[l as usize] = true;
                    }
                }
                let gap_start = covered.iter().position(|&c| !c).unwrap_or(254);
                let gap_end = covered[gap_start..].iter().position(|&c| c)
                    .map(|p| gap_start + p - 1)
                    .unwrap_or(255);
                prefs.viewer.false_color_palette.stops.push(FalseColorStop {
                    luma_min: gap_start as u8,
                    luma_max: gap_end as u8,
                    color: [0x80, 0x80, 0x80],
                    label: "New stop".into(),
                });
                any_changed = true;
            }
        });

        if any_changed {
            // Sort stops and mark as custom if no longer matching a preset
            prefs.viewer.false_color_palette.sort_stops();
            if !FalseColorPalette::built_ins().contains(&prefs.viewer.false_color_palette) {
                prefs.viewer.false_color_palette.name = "Custom".into();
            }
            *dirty = true;
        }
    }

    fn show_advanced(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, theme: &KlippTheme) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Advanced").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── UI section ──
        Self::section_header(ui, "UI", theme);

        Self::setting_row(ui, "Toast notification duration", theme, |ui| {
            let mut v = prefs.advanced.toast_duration_secs;
            if ui.add(egui::DragValue::new(&mut v).range(0.5..=10.0).speed(0.1).suffix(" s").fixed_decimals(1)).changed() {
                prefs.advanced.toast_duration_secs = v; *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "Performance overlay", "Show FPS and frame timing in viewer", theme, |ui| {
            // This is a toggle for showing perf overlay by default (controlled by P key at runtime)
            // Use perf_overlay_interval_ms > 0 as proxy for "enabled"
            let mut enabled = prefs.advanced.perf_overlay_interval_ms > 0;
            if ui.add(Switch::new(&mut enabled, theme)).changed() {
                prefs.advanced.perf_overlay_interval_ms = if enabled { 500 } else { 0 };
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Debug logging", theme, |ui| {
            let levels = ["Off", "Error", "Warn", "Info", "Debug"];
            // Map current log level — for now just a preference, not wired to tracing
            let current_idx = ui.ctx().data(|d| d.get_temp::<usize>(egui::Id::new("debug_log_level"))).unwrap_or(0);
            let current = levels[current_idx];
            egui::ComboBox::from_id_salt("debug_log")
                .selected_text(current)
                .width(80.0)
                .show_ui(ui, |ui| {
                    for (i, label) in levels.iter().enumerate() {
                        if ui.selectable_label(current_idx == i, *label).clicked() {
                            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("debug_log_level"), i));
                            *dirty = true;
                        }
                    }
                });
        });

        // ── RESET section ──
        Self::section_header(ui, "RESET", theme);

        Self::setting_row(ui, "Reset window layout", theme, |ui| {
            let btn_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(90.0, 24.0));
            let resp = ui.allocate_rect(btn_rect, egui::Sense::click());
            let bg = if resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(btn_rect, 4.0, bg);
            ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
            ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, "Reset layout", egui::FontId::proportional(11.0), theme.text_primary);
            if resp.clicked() {
                // Reset egui memory (window positions, sizes)
                ui.ctx().memory_mut(|mem| *mem = Default::default());
            }
        });

        Self::setting_row(ui, "Clear recent projects", theme, |ui| {
            let btn_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(56.0, 24.0));
            let resp = ui.allocate_rect(btn_rect, egui::Sense::click());
            let bg = if resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(btn_rect, 4.0, bg);
            ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0_f32, theme.accent));
            // CONTRAST RULE: accent-bordered button uses accent text on dark bg
            ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, "Clear", egui::FontId::proportional(11.0), theme.accent);
            if resp.clicked() {
                prefs.general.recent_projects.clear();
                *dirty = true;
            }
        });

        Self::setting_row_with_subtitle(ui, "Reset all preferences", "Restores factory defaults", theme, |ui| {
            let btn_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(110.0, 24.0));
            let resp = ui.allocate_rect(btn_rect, egui::Sense::click());
            let bg = if resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(btn_rect, 4.0, bg);
            ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0_f32, theme.accent));
            ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, "Reset everything", egui::FontId::proportional(11.0), theme.accent);
            if resp.clicked() {
                *prefs = crate::prefs::Preferences::default();
                *dirty = true;
            }
        });
    }

    fn show_keyboard(&mut self, ui: &mut egui::Ui, key_bindings: &mut crate::keybindings::KeyBindings, theme: &KlippTheme) {
        use crate::keybindings::{KeyBindings, key_event_to_combo};

        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Keyboard shortcuts").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── Layout: start from another NLE's keys ──
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Layout").size(12.0).color(theme.text_primary));
            ui.add_space(8.0);
            let mut preset = key_bindings.preset;
            egui::ComboBox::from_id_salt("keymap_preset")
                .selected_text(preset.label())
                .show_ui(ui, |ui| {
                    for p in crate::keybindings::KeymapPreset::ALL {
                        ui.selectable_value(&mut preset, p, p.label());
                    }
                });
            if preset != key_bindings.preset {
                key_bindings.set_preset(preset);
                let _ = key_bindings.save();
            }
        });
        {
            let changed = key_bindings.overrides().len();
            let mut note = if key_bindings.preset == crate::keybindings::KeymapPreset::Klipp {
                "Klipp's own keys.".to_string()
            } else {
                format!(
                    "{}'s defaults for the {} actions Klipp shares with it. Everything else keeps Klipp's key, \
                     and a Klipp key that {} uses for something else is taken off.",
                    key_bindings.preset.label(),
                    key_bindings.preset.mapped_count(),
                    key_bindings.preset.label(),
                )
            };
            let displaced = key_bindings.preset.displaced();
            if !displaced.is_empty() {
                let names: Vec<&str> = displaced.iter().map(|a| KeyBindings::action_label(a)).collect();
                note.push_str(&format!(" Left without a key: {}.", names.join(", ")));
            }
            if changed > 0 {
                note.push_str(&format!(" Your {changed} change(s) stay on top when you switch."));
            }
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.add(egui::Label::new(egui::RichText::new(note).size(10.0).color(theme.text_dim)).wrap());
            });
        }
        ui.add_space(6.0);

        // ── SHORTCUTS section header with Reset all button ──
        {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), crate::widgets::inspector_group::SECTION_HEADER_H), egui::Sense::hover());
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "SHORTCUTS",
                egui::FontId::proportional(crate::theme::type_scale::CONTROL),
                theme.text_primary,
            );
            ui.painter().hline(rect.x_range(), rect.max.y - 0.5, egui::Stroke::new(1.0_f32, theme.border));
            // Reset all to defaults button (right-aligned)
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 130.0, rect.top() + 3.0),
                egui::vec2(116.0, 22.0),
            );
            let btn_resp = ui.allocate_rect(btn_rect, egui::Sense::click());
            let btn_bg = if btn_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(btn_rect, 4.0, btn_bg);
            ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
            ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, "Reset all to layout", egui::FontId::proportional(10.0), theme.text_primary);
            if btn_resp.clicked() {
                key_bindings.reset_to_preset();
                let _ = key_bindings.save();
            }
        }

        // Show conflicts as warning rows
        let conflicts = key_bindings.find_conflicts();
        if !conflicts.is_empty() {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0 + conflicts.len() as f32 * 18.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, theme.bg_raised);
            let warn_color = theme.marker_warning;
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.top() + 12.0),
                egui::Align2::LEFT_CENTER,
                format!("\u{26a0} {} shortcut conflict(s)", conflicts.len()),
                egui::FontId::proportional(11.0),
                warn_color,
            );
            for (i, crate::keybindings::Conflict { a, b, combo, place }) in conflicts.iter().enumerate() {
                ui.painter().text(
                    egui::pos2(rect.left() + 28.0, rect.top() + 28.0 + i as f32 * 18.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{} and {} both use {} {}", KeyBindings::action_label(a), KeyBindings::action_label(b), combo.display(), place.label()),
                    egui::FontId::proportional(10.0),
                    theme.marker_warning,
                );
            }
        }

        // If remapping, show capture banner
        if let Some((ref action_name, combo_idx)) = self.remapping.clone() {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, theme.bg_raised);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("Press a key combination for \"{}\" (Escape to cancel)", KeyBindings::action_label(action_name)),
                egui::FontId::proportional(11.0),
                theme.accent,
            );

            let input = ui.input(|i| i.clone());
            if input.key_pressed(egui::Key::Escape) {
                self.remapping = None;
            } else if let Some(combo) = key_event_to_combo(&input) {
                if let Some(combos) = key_bindings.bindings.get_mut(action_name.as_str()) {
                    if combo_idx < combos.len() {
                        combos[combo_idx] = combo;
                    } else {
                        combos.push(combo);
                    }
                }
                let _ = key_bindings.save();
                self.remapping = None;
            }
        }

        let search = self.kb_search.to_lowercase();

        // Draw each category with sub-header + action rows
        for (category, actions) in KeyBindings::categories() {
            let filtered: Vec<_> = actions.iter().filter(|a| {
                if search.is_empty() { return true; }
                let label = KeyBindings::action_label(a).to_lowercase();
                let binding_str = key_bindings.display_for(a).to_lowercase();
                label.contains(&search) || binding_str.contains(&search) || a.to_lowercase().contains(&search)
            }).collect();
            if filtered.is_empty() { continue; }

            // Category sub-header (dim, smaller than section header)
            let (cat_rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::hover());
            ui.painter().rect_filled(cat_rect, 0.0, theme.bg_panel_alt);
            ui.painter().text(
                egui::pos2(cat_rect.left() + 14.0, cat_rect.center().y),
                egui::Align2::LEFT_CENTER,
                category.to_uppercase(),
                egui::FontId::proportional(10.0),
                theme.text_dim,
            );

            // Action rows
            for action in &filtered {
                let label = KeyBindings::action_label(action);
                let combos = key_bindings.bindings.get(**action).cloned().unwrap_or_default();
                let is_remapping = self.remapping.as_ref().is_some_and(|(a, _)| a == **action);

                let row_h = crate::inspector::rows::ROW_H + 4.0;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());

                // Action label
                ui.painter().text(
                    egui::pos2(rect.left() + 14.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    label,
                    egui::FontId::proportional(crate::inspector::rows::TEXT),
                    theme.text_secondary,
                );

                // "Set" button (rightmost)
                let set_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.right() - 50.0, rect.top() + 6.0),
                    egui::vec2(36.0, 24.0),
                );
                let set_resp = ui.allocate_rect(set_rect, egui::Sense::click());
                let set_bg = if is_remapping { theme.bg_raised }
                    else if set_resp.hovered() { theme.bg_hover }
                    else { theme.bg_raised };
                ui.painter().rect_filled(set_rect, 4.0, set_bg);
                ui.painter().rect_stroke(set_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
                let set_label = if is_remapping { "..." } else { "Set" };
                ui.painter().text(set_rect.center(), egui::Align2::CENTER_CENTER, set_label, egui::FontId::proportional(10.0), theme.text_primary);
                if set_resp.clicked() && !is_remapping {
                    self.remapping = Some((action.to_string(), 0));
                }

                // Key badge pills (before Set button)
                if !combos.is_empty() {
                    let badge_right = set_rect.left() - 8.0;
                    let mut x = badge_right;
                    // Draw combo parts as individual key pills (right-to-left)
                    for combo in combos.iter().rev() {
                        let parts: Vec<&str> = {
                            let mut p = Vec::new();
                            if combo.ctrl { p.push("Ctrl"); }
                            if combo.shift { p.push("\u{21e7}"); }
                            if combo.alt { p.push("Alt"); }
                            p.push(&combo.key);
                            p
                        };
                        // Draw each key as a separate pill, right-to-left
                        for part in parts.iter().rev() {
                            let font = egui::FontId::proportional(10.0);
                            let text_galley = ui.painter().layout_no_wrap(part.to_string(), font.clone(), theme.text_primary);
                            let pill_w = text_galley.size().x + 12.0;
                            let pill_h = 20.0;
                            let pill_rect = egui::Rect::from_min_size(
                                egui::pos2(x - pill_w, rect.center().y - pill_h / 2.0),
                                egui::vec2(pill_w, pill_h),
                            );
                            ui.painter().rect_filled(pill_rect, 4.0, theme.border);
                            ui.painter().rect_stroke(pill_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
                            ui.painter().text(pill_rect.center(), egui::Align2::CENTER_CENTER, *part, font, theme.text_primary);
                            x = pill_rect.left() - 3.0;
                        }
                        x -= 6.0; // gap between multiple combos
                    }
                } else {
                    // No binding: said in words, beside the Set that gives
                    // it one (#3021).
                    ui.painter().text(
                        egui::pos2(set_rect.left() - 10.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        NO_KEY,
                        egui::FontId::proportional(crate::inspector::rows::TEXT),
                        theme.text_dim,
                    );
                }
            }
        }
    }

    fn show_about(&mut self, ui: &mut egui::Ui, theme: &KlippTheme) {
        if ui.button("Check for updates…").clicked() { self.show_updates = true; }
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("About Klipp").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            crate::widgets::wordmark::wordmark(ui, 36.0);
        });
        ui.add_space(8.0);

        // ── APPLICATION section ──
        Self::section_header(ui, "APPLICATION", theme);

        let build = klipp_core::build_info::get();
        Self::setting_row(ui, "Version", theme, |ui| {
            ui.label(egui::RichText::new(build.label()).color(theme.text_dim));
        });
        if let Some(at) = build.built_at {
            Self::setting_row(ui, "Built", theme, |ui| {
                ui.label(egui::RichText::new(klipp_core::build_info::format_utc(at)).color(theme.text_dim));
            });
        }

        Self::setting_row(ui, "Author", theme, |ui| {
            ui.label(egui::RichText::new("David Bolin").color(theme.text_dim));
        });

        Self::setting_row(ui, "License", theme, |ui| {
            ui.label(egui::RichText::new("GPL v3.0").color(theme.text_dim));
        });

        // ── DETECTED HARDWARE section ──
        Self::section_header(ui, "DETECTED HARDWARE", theme);

        // CPU
        Self::setting_row(ui, "CPU", theme, |ui| {
            let cpu_info = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
            let model = cpu_info.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "Unknown".into());
            let display = crate::ui_util::elide(&model, 40);
            ui.label(egui::RichText::new(display).color(theme.text_dim).size(11.0));
        });

        // GPU — extract just the model name from lspci (e.g. "GeForce RTX 4080")
        // GPU — model name + driver/CUDA info
        {
            let gpu_name = std::process::Command::new("sh")
                .args(["-c", "lspci 2>/dev/null | grep -i 'vga\\|3d' | head -1 | sed 's/.*: //'"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| {
                    let s = s.trim().to_string();
                    if let Some(start) = s.find('[') {
                        if let Some(end) = s.find(']') {
                            return s[start + 1..end].to_string();
                        }
                    }
                    s
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Unknown".into());

            // Detect driver + API info based on vendor
            let driver_info = if gpu_name.to_lowercase().contains("nvidia") || gpu_name.to_lowercase().contains("geforce") || gpu_name.to_lowercase().contains("rtx") || gpu_name.to_lowercase().contains("gtx") {
                // NVIDIA: get driver version + CUDA version
                let driver = std::process::Command::new("sh")
                    .args(["-c", "nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1"])
                    .output().ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let cuda = std::process::Command::new("sh")
                    .args(["-c", "nvidia-smi 2>/dev/null | grep -oP 'CUDA Version: \\K[0-9.]+'"])
                    .output().ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                match (driver, cuda) {
                    (Some(d), Some(c)) => format!("Driver {d}, CUDA {c}"),
                    (Some(d), None) => format!("Driver {d}"),
                    _ => String::new(),
                }
            } else if gpu_name.to_lowercase().contains("amd") || gpu_name.to_lowercase().contains("radeon") {
                // AMD: check mesa/amdgpu driver
                std::process::Command::new("sh")
                    .args(["-c", "glxinfo 2>/dev/null | grep 'OpenGL version' | sed 's/.*: //' | head -1"])
                    .output().ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| format!("Mesa {}", s.trim()))
                    .filter(|s| s.len() > 5)
                    .unwrap_or_default()
            } else if gpu_name.to_lowercase().contains("intel") {
                // Intel: check mesa/i915 driver
                std::process::Command::new("sh")
                    .args(["-c", "glxinfo 2>/dev/null | grep 'OpenGL version' | sed 's/.*: //' | head -1"])
                    .output().ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| format!("Mesa {}", s.trim()))
                    .filter(|s| s.len() > 5)
                    .unwrap_or_default()
            } else {
                String::new()
            };

            if driver_info.is_empty() {
                Self::setting_row(ui, "GPU", theme, |ui| {
                    ui.label(egui::RichText::new(&gpu_name).color(theme.text_dim).size(11.0));
                });
            } else {
                Self::setting_row_with_subtitle(ui, "GPU", &driver_info, theme, |ui| {
                    ui.label(egui::RichText::new(&gpu_name).color(theme.text_dim).size(11.0));
                });
            }
        }

        // Memory — show installed (rounded up to nearest power-of-2 stick size) + usable
        Self::setting_row(ui, "Memory", theme, |ui| {
            let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
            let total_kb = meminfo.lines()
                .find(|l| l.starts_with("MemTotal"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let usable_gb = total_kb as f64 / 1_048_576.0;
            // Round up to nearest common DIMM size (8, 16, 32, 64, 128)
            let installed_gb = [8u64, 16, 32, 64, 128, 256].iter()
                .find(|&&s| (s as f64) >= usable_gb)
                .copied()
                .unwrap_or((usable_gb.ceil()) as u64);
            let display = format!("{} GB ({:.1} GB usable)", installed_gb, usable_gb);
            ui.label(egui::RichText::new(display).color(theme.text_dim));
        });

        // Audio — show primary sound card (skip GPU HDMI audio) + backend
        Self::setting_row_with_subtitle(ui, "Audio", "Output via PipeWire (cpal)", theme, |ui| {
            let cards = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
            // Parse card names, skip GPU HDMI/DP audio entries (NVidia, AMD HDMI, etc.)
            let gpu_keywords = ["nvidia", "hdmi", "displayport", "radeon"];
            let primary = cards.lines()
                .filter(|l| l.contains(" - "))
                .filter_map(|l| l.split(" - ").nth(1).map(|s| s.trim().to_string()))
                .find(|name| {
                    let lower = name.to_lowercase();
                    !gpu_keywords.iter().any(|kw| lower.contains(kw))
                })
                .unwrap_or_else(|| "Default device".into());
            ui.label(egui::RichText::new(primary).color(theme.text_dim).size(11.0));
        });

        // ── TECHNOLOGY section ──
        Self::section_header(ui, "TECHNOLOGY", theme);

        let tech_row = |ui: &mut egui::Ui, name: &str, desc: &str| {
            let row_h = crate::inspector::rows::ROW_H + 4.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                name,
                egui::FontId::proportional(crate::inspector::rows::TEXT),
                theme.text_secondary,
            );
            ui.painter().text(
                egui::pos2(rect.right() - 14.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                desc,
                egui::FontId::proportional(11.0),
                theme.text_dim,
            );
        };

        tech_row(ui, "Rust", "Systems programming language");
        tech_row(ui, "egui / eframe", "Immediate-mode GUI framework");
        tech_row(ui, "ffmpeg-next", "Video/audio decode & encode");
        tech_row(ui, "wgpu", "GPU compute shaders (Vulkan)");
        tech_row(ui, "cpal", "Cross-platform audio output");
        tech_row(ui, "rfd", "Native file dialogs");
        tech_row(ui, "serde", "Serialization framework");
        tech_row(ui, "image", "Image loading/saving");

        // ── ACKNOWLEDGMENTS section ──
        Self::section_header(ui, "ACKNOWLEDGMENTS", theme);

        let ack_row = |ui: &mut egui::Ui, text: &str| {
            let row_h = 28.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                text,
                egui::FontId::proportional(11.0),
                theme.text_dim,
            );
        };

        ack_row(ui, "FFmpeg project \u{2014} multimedia framework (LGPL)");
        ack_row(ui, "egui by Emil Ernerfeldt \u{2014} MIT License");
        ack_row(ui, "wgpu contributors \u{2014} Apache 2.0 / MIT");
        ack_row(ui, "Phosphor Icons \u{2014} MIT License");
        ack_row(ui, "OpenColourIO / ACES \u{2014} colour science references");
        ack_row(ui, "The Rust community and crate ecosystem");

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("\u{00a9} 2025\u{2013}2026 David Bolin. All rights reserved.").size(10.0).color(theme.text_dim));
        });
    }
}

/// The human name of an AI model from the manifest, or its id.
fn model_display_name(id: &str) -> String {
    klipp_ml::manifest::load_effective().ok()
        .and_then(|m| m.get(id).map(|e| e.display_name.clone()))
        .unwrap_or_else(|| id.to_string())
}

/// A model row's `(display name, human size, installed)` for the AI tab.
fn model_row_info(id: &str) -> (String, String, bool) {
    let Ok(manifest) = klipp_ml::manifest::load_effective() else {
        return (id.to_string(), "?".to_string(), false);
    };
    let Some(entry) = manifest.get(id) else {
        return (id.to_string(), "?".to_string(), false);
    };
    let installed = klipp_ml::cache::ModelCache::open().ok()
        .map(|c| c.path_for(entry).is_file())
        .unwrap_or(false);
    (entry.display_name.clone(), fmt_bytes(entry.size_bytes), installed)
}

/// A one-line capability note for a bundled Whisper model.
fn whisper_blurb(id: &str) -> &'static str {
    match id {
        "whisper_large_v3_turbo_q5" => "The most accurate, and still fast on a GPU. The default choice.",
        "whisper_base_ggml" => "Small and quick, less accurate. A good fallback on a slow machine.",
        _ => "Speech-to-text model.",
    }
}

/// The ONNX vision runtime's status line for the AI tab.
fn ai_vision_status() -> String {
    #[cfg(feature = "ai-ort")]
    {
        klipp_ml_ort::resolve_runtime_status().label()
    }
    #[cfg(not(feature = "ai-ort"))]
    {
        "CPU (built without the ONNX GPU runtime)".to_string()
    }
}

/// Bytes as a short human size (MB/GB) for model listings.
fn fmt_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", (bytes as f64 / 1e6).round() as u64)
    }
}

#[cfg(test)]
mod connect_assistant_tests {
    use super::*;

    /// Every piece of text drawn in a frame.
    fn texts(out: &egui::FullOutput) -> Vec<String> {
        out.shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    /// Where an assistant has Klipp only for some projects, the section
    /// names them.
    #[test]
    fn a_project_only_connection_names_where() {
        assert_eq!(projects_only_text(&["~/Klipp".into()]), "Connected only in ~/Klipp. Connect adds it for every project.");
        assert_eq!(projects_only_text(&["~/a".into(), "~/b".into()]), "Connected only in ~/a and ~/b. Connect adds it for every project.");
        assert_eq!(projects_only_text(&["~/a".into(), "~/b".into(), "~/c".into()]), "Connected only in ~/a, ~/b and ~/c. Connect adds it for every project.");
    }

    /// #625: the Connect an assistant section draws its rows on this
    /// computer: the program, the check, each assistant with its state,
    /// and the settings for another one — with the assistant on, and a
    /// warning first with it off.
    #[test]
    fn the_connect_section_draws_what_it_found() {
        let ctx = egui::Context::default();
        let theme = KlippTheme::default();
        let mut panel = PreferencesPanel::default();
        let mut prefs = Preferences::default();
        prefs.ai.assistant = true;
        let frame = |panel: &mut PreferencesPanel, prefs: &Preferences| {
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| panel.show_connect_assistant(ui, prefs, &theme));
            });
            texts(&out)
        };
        let first = frame(&mut panel, &prefs);
        assert!(first.iter().any(|t| t.contains("CONNECT AN ASSISTANT")), "{first:?}");
        // The look runs on a worker: give it its moment.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while panel.assistant.lock().unwrap().busy.is_some() {
            assert!(std::time::Instant::now() < deadline, "the look for assistants never finished");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let shown = frame(&mut panel, &prefs);
        for want in ["Klipp's MCP program", "Check the connection", "Claude Code", "Codex", "Another assistant"] {
            assert!(shown.iter().any(|t| t.contains(want)), "no {want:?} in {shown:?}");
        }
        eprintln!("drawn:\n  {}", shown.join("\n  "));
        prefs.ai.assistant = false;
        let off = frame(&mut panel, &prefs);
        assert!(off.iter().any(|t| t.contains("The AI assistant is off")), "{off:?}");
    }
    /// #2649: the Set Up AI Assistants window shows the same, the new
    /// assistants among them, the settings for other ones in each shape,
    /// and the last-seen line; the section's button asks for it, and with
    /// the assistant off it says so.
    #[test]
    fn the_setup_window_shows_every_assistant() {
        let ctx = egui::Context::default();
        let theme = KlippTheme::default();
        let mut panel = PreferencesPanel::default();
        let mut prefs = Preferences::default();
        prefs.ai.assistant = true;
        let frame = |panel: &mut PreferencesPanel, prefs: &Preferences, events: Vec<egui::Event>| {
            let mut open = true;
            let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 1600.0))), events, ..Default::default() }, |ctx| {
                panel.show_assistant_setup_window(ctx, &mut open, prefs, &theme);
            });
            texts(&out)
        };
        frame(&mut panel, &prefs, Vec::new());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while panel.assistant.lock().unwrap().busy.is_some() {
            assert!(std::time::Instant::now() < deadline, "the look for assistants never finished");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let shown = frame(&mut panel, &prefs, Vec::new());
        for want in ["Set Up AI Assistants", "Klipp's MCP program", "Claude Code", "Codex", "Gemini CLI", "Zed", "Cursor", "talked to Klipp"] {
            assert!(shown.iter().any(|t| t.contains(want)), "no {want:?} in {shown:?}");
        }
        prefs.ai.assistant = false;
        let off = frame(&mut panel, &prefs, Vec::new());
        assert!(off.iter().any(|t| t.contains("The AI assistant is off")), "{off:?}");
        // The section's button asks for the window, once.
        panel.open_assistant_setup = true;
        assert!(panel.take_open_assistant_setup());
        assert!(!panel.take_open_assistant_setup());
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    const TABS: [PrefsTab; 21] = [
        PrefsTab::General, PrefsTab::Ai, PrefsTab::System, PrefsTab::Appearance, PrefsTab::Playback, PrefsTab::Editing,
        PrefsTab::FileBrowser, PrefsTab::Audio, PrefsTab::AudioHardware, PrefsTab::Export, PrefsTab::MediaCache, PrefsTab::Color,
        PrefsTab::Title, PrefsTab::Timeline, PrefsTab::Viewer, PrefsTab::Proxy, PrefsTab::ExternalTools, PrefsTab::Accounts, PrefsTab::Advanced,
        PrefsTab::Keyboard, PrefsTab::About,
    ];

    /// Every row and section label the dialog drew before its restyle,
    /// read from its source then (a "Label:" written as "Label" now).
    const OLD_LABELS: &[&str] = &[
        "ACKNOWLEDGMENTS",
        "AI assistant (MCP)  \u{2022}  restart required",
        "APPLICATION",
        "APPLICATIONS",
        "AUTO-DUCK DEFAULTS",
        "Action safe",
        "Adapt timeline to window",
        "All AI models",
        "All caches",
        "Also auto-save every N operations",
        "Ambience",
        "Anti-click fade",
        "Assistant (MCP)",
        "Attack",
        "Audio",
        "Audio backend",
        "Audio cache",
        "Audio cache (PCM)",
        "Audio editor",
        "Audio hover skim",
        "Audio peaks",
        "Audio sample rate",
        "Audio scrubbing",
        "Author",
        "Auto-generate on import",
        "Auto-refresh folder",
        "Auto-save interval",
        "Auto-show angle viewer",
        "Auto-switch during scrub",
        "B-Roll",
        "BEHAVIOR",
        "Background performance",
        "Background render",
        "Backup versions to keep",
        "Buffer size",
        "Built",
        "CACHE SIZES",
        "CLEANUP",
        "COLUMNS",
        "CONNECT AN ASSISTANT",
        "CPU",
        "Calibration LUT",
        "Check automatically",
        "Check the connection",
        "Checkerboard tile",
        "Clear recent projects",
        "Clip corner radius",
        "Clip gap",
        "Colour space tagging",
        "Colour",
        "DEFAULTS",
        "DETECTED HARDWARE",
        "DISPLAY",
        "Debug logging",
        "Decode threads",
        "Default alignment",
        "Default colour",
        "Default duration",
        "Default encoder preset",
        "Default fade curve",
        "Default fade duration",
        "Default font",
        "Default size",
        "Default sort",
        "Default sync method",
        "Default transition",
        "Default transition duration",
        "Default type filter",
        "Dialogue",
        "Dim UI during playback",
        "Duck level",
        "FAVORITES",
        "FEATURES",
        "Filmstrip hover preview",
        "Filmstrip quality",
        "Filmstrip strips",
        "Filmstrip tile resolution",
        "Foley",
        "Follow system theme",
        "Frame queue size",
        "Fullscreen overlay timeout",
        "GENERATION",
        "GPU",
        "GPU preference",
        "Graphics",
        "Grid colour",
        "Grid snap",
        "Group files by date",
        "HOVER SKIM",
        "Hardware video decode",
        "High threshold (clipping)",
        "INSPECTOR",
        "Image editor",
        "Include preview releases",
        "Input device",
        "Installed version",
        "JKL speed steps",
        "Keep filmstrip visible",
        "Klipp's MCP program",
        "LIMITS",
        "LOCATIONS",
        "LOUDNESS",
        "LUT interpolation",
        "LUTs",
        "License",
        "Loop playback by default",
        "Loss threshold",
        "Loudness standard",
        "Low threshold (underexposure)",
        "MANAGEMENT",
        "MIXING",
        "MODELS",
        "MULTICAM",
        "Magnetic timeline",
        "Marker colour",
        "Max cache size",
        "Max playback resolution",
        "Max track height",
        "Memory",
        "Min track height",
        "Minimum gap",
        "Monitoring mode",
        "Music",
        "NODE GRAPH",
        "New sequence resolution",
        "Normalize target",
        "OUTPUT DEVICE",
        "On launch",
        "Open folder after export",
        "Optimised playback",
        "Orphaned cache files",
        "Output device  \u{2022}  restart required",
        "Output devices",
        "Overview bar",
        "Overwrite existing files",
        "PROJECT",
        "Pan law",
        "Parallel render jobs",
        "Patch size",
        "Peak hold time",
        "Performance overlay",
        "Pinned folders",
        "Playback quality",
        "Playhead follows playback",
        "Post-export command",
        "Poster thumbnails",
        "Pre-roll duration",
        "Preset",
        "Printer point",
        "Projects root",
        "Proxy cache",
        "Proxy files",
        "Proxy resolution",
        "QUALITY",
        "RECORDING INPUT",
        "RENDER CACHE",
        "RESET",
        "ROLE COLOURS",
        "ROUND-TRIP",
        "Recent projects to show",
        "Release",
        "Reload on external edit",
        "Remember collapsed sections",
        "Render cache",
        "Reset all preferences",
        "Reset window layout",
        "Ruler height",
        "Rust",
        "SCENE DETECTION",
        "SCOPES",
        "SCREEN TEXT (OCR)",
        "SDR viewer transfer",
        "SFX",
        "STARTUP",
        "STATUS",
        "STORAGE",
        "Sample rate",
        "Sampling quality",
        "Scope refresh rate",
        "Screen text (OCR)",
        "Search radius",
        "Selection border width",
        "Sensitivity",
        "Sensitivity",
        "Shift+arrow step",
        "Show angle badges on timeline",
        "Show keyboard shortcut hints",
        "Show recent files",
        "Skim preview height",
        "Skim transcode quality",
        "Skim transcodes",
        "Smart render cache",
        "Snap (master)",
        "Snap radius",
        "Snap threshold",
        "Snap to ports while connecting",
        "Sort ascending",
        "Speech (transcription)",
        "Speech transcription",
        "Subject detection",
        "System mute reminder",
        "TECHNOLOGY",
        "TEXT DEFAULTS",
        "THEME",
        "THREADING",
        "TIMELINE BEHAVIOR",
        "TRACKING",
        "TRANSCRIPTION (SPEECH)",
        "TRANSPORT",
        "Text track height",
        "Theme",
        "Threshold",
        "Thumbnail cache",
        "Thumbnail resolution",
        "Thumbnail size",
        "Title",
        "Title safe",
        "Title-safe area overlay",
        "Toast notification duration",
        "Transcribe on CPU",
        "Transform handle size",
        "Trim handle",
        "UI",
        "UI scale",
        "UPDATES",
        "Vectorscope skin angle",
        "Version",
        "Video",
        "Video editor",
        "Video hover skim",
        "Visible columns",
        "Voiceover",
        "Zoom step multiplier",
        "cpal",
        "egui / eframe",
        "ffmpeg-next",
        "image",
        "rfd",
        "serde",
        "wgpu",
    ];

    /// Settings the dialog showed that nothing read: changing one did
    /// nothing, so they were removed (#1816). "Render cache" here is the
    /// folder row; the cache's size row keeps the name.
    const HIDDEN: &[&str] = &[
        "Monitoring mode", "Normalize target",
        "Colour space tagging", "Default encoder preset",
        "Clip corner radius", "Ruler height",
        "Title-safe area overlay",
        "Show keyboard shortcut hints", "Recent projects to show",
        // Removed with multicam mode (#1844), which the playhead never
        // switches on.
        "Auto-show angle viewer",
    ];

    /// One frame of the dialog on `tab`, with `events`: its texts by their
    /// centres, its hairlines by height and left end, and the prefs.
    struct Drawn {
        texts: Vec<(String, egui::Pos2)>,
        rules: Vec<(f32, f32, f32)>,
        /// The heights of the texts drawn in the primary colour.
        primary: Vec<f32>,
    }

    fn draw(ctx: &egui::Context, panel: &mut PreferencesPanel, prefs: &mut Preferences, dirty: &mut bool, events: Vec<egui::Event>) -> Drawn {
        let theme = KlippTheme::default();
        let reg = crate::theme::ThemeRegistry::load();
        let mut kb = crate::keybindings::KeyBindings::default();
        let out = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 4000.0))), events, ..Default::default() },
            |ctx| {
                panel.show(ctx, prefs, dirty, &mut kb, &reg, &theme, klipp_core::SnapConfig::default());
            },
        );
        fn walk(s: &egui::Shape, out: &mut Vec<egui::Shape>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                s => out.push(s.clone()),
            }
        }
        let mut flat = Vec::new();
        for c in &out.shapes {
            walk(&c.shape, &mut flat);
        }
        let mut texts = Vec::new();
        let mut rules = Vec::new();
        let mut primary = Vec::new();
        for s in flat {
            match s {
                egui::Shape::Text(t) => {
                    let c = t.galley.job.sections.first().map(|s| s.format.color);
                    if c == Some(theme.text_primary) || t.override_text_color == Some(theme.text_primary) {
                        primary.push(t.pos.y + t.galley.size().y / 2.0);
                    }
                    texts.push((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0));
                }
                egui::Shape::LineSegment { points, .. } if points[0].y == points[1].y && (points[1].x - points[0].x).abs() > 100.0 => rules.push((points[0].y, points[0].x, points[1].x)),
                _ => {}
            }
        }
        Drawn { texts, rules, primary }
    }

    /// Each tab drawn twice (the second frame with its layout settled).
    fn tab(tab: PrefsTab, prefs: &mut Preferences) -> Drawn {
        let ctx = egui::Context::default();
        let mut panel = PreferencesPanel { active_tab: tab, ..Default::default() };
        let mut dirty = false;
        draw(&ctx, &mut panel, prefs, &mut dirty, Vec::new());
        draw(&ctx, &mut panel, prefs, &mut dirty, Vec::new())
    }

    /// A press and release at `at`.
    fn click(ctx: &egui::Context, panel: &mut PreferencesPanel, prefs: &mut Preferences, dirty: &mut bool, at: egui::Pos2) {
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        draw(ctx, panel, prefs, dirty, vec![egui::Event::PointerMoved(at), press(true)]);
        draw(ctx, panel, prefs, dirty, vec![press(false)]);
    }

    /// A boolean is a switch at its row's right edge that flips the
    /// setting and marks the preferences to be saved; a section in the
    /// list on the left opens its page.
    #[test]
    fn the_switches_and_the_list_do_what_they_say() {
        let ctx = egui::Context::default();
        let mut panel = PreferencesPanel::default();
        let mut prefs = Preferences::default();
        let mut dirty = false;
        let before = prefs.general.check_for_updates;
        draw(&ctx, &mut panel, &mut prefs, &mut dirty, Vec::new());
        let d = draw(&ctx, &mut panel, &mut prefs, &mut dirty, Vec::new());
        let at = |l: &str| d.texts.iter().find(|(s, _)| s == l).unwrap_or_else(|| panic!("no {l}")).1;
        let row = at("Include preview releases");
        let right = d.rules.iter().filter(|(_, x0, _)| *x0 > 280.0).map(|(_, _, x1)| *x1).fold(0.0, f32::max);
        click(&ctx, &mut panel, &mut prefs, &mut dirty, egui::pos2(right - crate::inspector::rows::ROW_PAD - 16.0, row.y));
        assert_ne!(prefs.general.preview_updates, Preferences::default().general.preview_updates, "the switch flips its setting");
        assert!(dirty, "and the preferences are saved");
        assert_eq!(prefs.general.check_for_updates, before, "only its own");

        let timeline = at("Timeline");
        click(&ctx, &mut panel, &mut prefs, &mut dirty, timeline);
        assert_eq!(panel.active_tab, PrefsTab::Timeline);
    }

    /// #2933, #2944: General's "Show welcome at startup" and the Welcome
    /// window's "Show at startup" are one setting, each way: the switch
    /// sets what the window's box shows, and the box, applied as the app
    /// applies it, sets what the switch flips from. "Note a smaller
    /// interface" arms the small-screen note again.
    #[test]
    fn show_welcome_at_startup_is_one_setting_both_ways() {
        let ctx = egui::Context::default();
        let mut panel = PreferencesPanel::default();
        let mut prefs = Preferences::default();
        let mut dirty = false;
        assert!(prefs.general.show_welcome, "a fresh profile");
        let switch_at = |ctx: &egui::Context, panel: &mut PreferencesPanel, prefs: &mut Preferences, dirty: &mut bool, label: &str| {
            draw(ctx, panel, prefs, dirty, Vec::new());
            let d = draw(ctx, panel, prefs, dirty, Vec::new());
            let row = d.texts.iter().find(|(s, _)| s == label).unwrap_or_else(|| panic!("no {label}")).1;
            let right = d.rules.iter().filter(|(_, x0, _)| *x0 > 280.0).map(|(_, _, x1)| *x1).fold(0.0, f32::max);
            egui::pos2(right - crate::inspector::rows::ROW_PAD - 16.0, row.y)
        };
        // Preferences → the window.
        let at = switch_at(&ctx, &mut panel, &mut prefs, &mut dirty, "Show welcome at startup");
        click(&ctx, &mut panel, &mut prefs, &mut dirty, at);
        assert!(!prefs.general.show_welcome && dirty, "the switch turns it off");
        let wctx = egui::Context::default();
        let mut state = crate::state::AppState { show_welcome: true, ..Default::default() };
        let mut kb = crate::keybindings::KeyBindings::defaults();
        let theme = KlippTheme::default();
        let mut boxed = None;
        let mut got = Vec::new();
        for i in 0..2 {
            let out = wctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))), time: Some(i as f64 * 0.1), ..Default::default() }, |ctx| {
                got = crate::welcome::show_welcome(ctx, &mut state, &kb, prefs.general.show_welcome, &theme);
            });
            // The box's label is part of the box.
            boxed = out.shapes.iter().find_map(|s| match &s.shape { egui::Shape::Text(t) if t.galley.text() == "Show at startup" => Some(t.pos + t.galley.size() / 2.0), _ => None });
        }
        let boxed = boxed.expect("the window's box");
        // The window → Preferences: its box, ticked, turns it back on.
        let press = |pressed| egui::Event::PointerButton { pos: boxed, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut actions = Vec::new();
        for (i, events) in [vec![egui::Event::PointerMoved(boxed), press(true)], vec![press(false)]].into_iter().enumerate() {
            let _ = wctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))), time: Some(0.2 + i as f64 * 0.1), events, ..Default::default() }, |ctx| {
                actions.extend(crate::welcome::show_welcome(ctx, &mut state, &kb, prefs.general.show_welcome, &theme));
            });
        }
        assert_eq!(actions, [crate::welcome::WelcomeAction::ShowAtStartup(true)], "the box shows it off, and ticks it on");
        for a in actions {
            crate::welcome::apply(a, &mut state, &mut prefs, &mut kb);
        }
        assert!(prefs.general.show_welcome, "Preferences' setting is on again");
        let at = switch_at(&ctx, &mut panel, &mut prefs, &mut dirty, "Show welcome at startup");
        click(&ctx, &mut panel, &mut prefs, &mut dirty, at);
        assert!(!prefs.general.show_welcome, "and the switch flips it from there");
        // The small-screen note, shown once, armed again by its switch.
        prefs.appearance.small_screen_note_shown = true;
        let at = switch_at(&ctx, &mut panel, &mut prefs, &mut dirty, "Note a smaller interface");
        click(&ctx, &mut panel, &mut prefs, &mut dirty, at);
        assert!(!prefs.appearance.small_screen_note_shown, "armed again");
    }

    /// The Updates section's "Check for updates…" asks for the check in
    /// About; the section keeps the settings.
    #[test]
    fn check_for_updates_asks_for_about() {
        let ctx = egui::Context::default();
        let mut panel = PreferencesPanel::default();
        let mut prefs = Preferences::default();
        let mut dirty = false;
        draw(&ctx, &mut panel, &mut prefs, &mut dirty, Vec::new());
        let d = draw(&ctx, &mut panel, &mut prefs, &mut dirty, Vec::new());
        let at = |l: &str| d.texts.iter().find(|(s, _)| s == l).unwrap_or_else(|| panic!("no {l}")).1;
        assert!(d.texts.iter().any(|(s, _)| s == "Check automatically"));
        let button = at("Check for updates…");
        click(&ctx, &mut panel, &mut prefs, &mut dirty, button);
        assert!(panel.show_updates);
    }

    /// Across its pages the dialog still shows every setting and section
    /// it showed before, and within a page the only rules are the
    /// section headers' (and the window's own edges): no rule between
    /// rows.
    /// #1894: the Accounts page has the Freesound sign-in, whichever state
    /// the machine's keys put it in.
    #[test]
    fn the_accounts_page_has_the_freesound_sign_in() {
        let mut prefs = Preferences::default();
        let d = tab(PrefsTab::Accounts, &mut prefs);
        let texts: Vec<&str> = d.texts.iter().map(|(s, _)| s.as_str()).collect();
        assert!(texts.contains(&"FREESOUND") && texts.contains(&"Freesound"), "{texts:?}");
    }

    #[test]
    fn every_setting_is_kept_and_rows_have_no_rules() {
        let mut prefs = Preferences::default();
        prefs.ai.assistant = true;
        let mut all: Vec<String> = Vec::new();
        for t in TABS {
            let d = tab(t, &mut prefs);
            // The content's rules (right of the list): each one under a
            // header, whose name is in the primary colour; a row's
            // label is in the secondary.
            for (y, _, _) in d.rules.iter().filter(|(_, x0, _)| *x0 > 280.0) {
                assert!(d.primary.iter().any(|h| (y - h) > 4.0 && (y - h) < 20.0), "{t:?}: a rule at {y} is not under a section header: {:?}", d.rules);
            }
            all.extend(d.texts.into_iter().map(|(s, _)| s));
        }
        // "Built" shows only in a build that knows when it was built.
        let missing: Vec<&&str> = OLD_LABELS.iter().filter(|l| **l != "Built" && !HIDDEN.contains(l) && !all.iter().any(|s| s == **l)).collect();
        assert!(missing.is_empty(), "no longer shown: {missing:?}");
        let shown: Vec<&&str> = HIDDEN.iter().filter(|l| all.iter().any(|s| s == **l)).collect();
        assert!(shown.is_empty(), "settings that change nothing are shown: {shown:?}");
    }
}

#[cfg(test)]
mod open_page_tests {
    use super::*;

    /// The welcome screen's Manage… and Keyboard Shortcuts turn to their
    /// pages; a name it does not know leaves the page as it was.
    #[test]
    fn open_page_turns_to_the_named_page() {
        let mut panel = PreferencesPanel::default();
        panel.open_page("cache");
        assert_eq!(panel.active_tab, PrefsTab::MediaCache);
        panel.open_page("keyboard");
        assert_eq!(panel.active_tab, PrefsTab::Keyboard);
        panel.open_page("colour");
        assert_eq!(panel.active_tab, PrefsTab::Color, "the calibration LUT's page (#466)");
        panel.open_page("nonsense");
        assert_eq!(panel.active_tab, PrefsTab::Color);
    }
}

#[cfg(test)]
mod keyboard_section_tests {
    use super::*;

    /// #3021: an action the preset gives no key says so in words, beside
    /// its Set; no bare dash is drawn.
    #[test]
    fn an_action_with_no_key_says_so() {
        let ctx = egui::Context::default();
        let theme = KlippTheme::default();
        let mut panel = PreferencesPanel::default();
        let mut kb = crate::keybindings::KeyBindings::from_preset(crate::keybindings::KeymapPreset::Premiere);
        let unbound = crate::keybindings::KeyBindings::categories().iter().flat_map(|(_, a)| a.iter()).filter(|a| kb.bindings.get(**a).is_none_or(|c| c.is_empty())).count();
        assert!(unbound > 0, "the preset leaves some actions without a key");
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 20000.0))), ..Default::default() };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| panel.show_keyboard(ui, &mut kb, &theme));
        });
        let texts: Vec<String> = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::epaint::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        }).collect();
        assert_eq!(texts.iter().filter(|t| *t == NO_KEY).count(), unbound, "every unbound row says so");
        assert!(!texts.iter().any(|t| t == "\u{2014}"), "no bare dash");
    }
}
