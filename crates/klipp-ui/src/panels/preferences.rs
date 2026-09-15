use crate::prefs::Preferences;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, MarkerColor, ProxyResolution};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrefsTab {
    General,
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
    Advanced,
    Keyboard,
    About,
}

pub struct PreferencesPanel {
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
}

struct ClearAllConfirm {
    dirs: Vec<std::path::PathBuf>,
    total_bytes: u64,
}

impl Default for PreferencesPanel {
    fn default() -> Self {
        Self {
            active_tab: PrefsTab::General,
            remapping: None,
            kb_search: String::new(),
            search_query: String::new(),
            clear_all_confirm: None,
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
                ui.painter().rect_filled(search_rect, 6.0, theme.bg_panel_alt);
                ui.painter().rect_stroke(search_rect, 6.0, egui::Stroke::new(1.0_f32, theme.bg_hover));
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
                        let categories: &[(&str, &[(PrefsTab, &str, &str)])] = &[
                            ("PROJECT", &[(PrefsTab::General, "\u{1F4C4}", "General"), (PrefsTab::Appearance, "\u{2699}", "Appearance")]),
                            ("PERFORMANCE", &[(PrefsTab::System, "\u{1F5A5}", "System"), (PrefsTab::Playback, "\u{25B6}", "Playback"), (PrefsTab::MediaCache, "\u{1F4BE}", "Cache")]),
                            ("EDITING", &[(PrefsTab::Editing, "\u{2702}", "Editing"), (PrefsTab::Timeline, "\u{1F39E}", "Timeline"), (PrefsTab::Viewer, "\u{1F5B5}", "Viewer")]),
                            ("MEDIA & AUDIO", &[(PrefsTab::FileBrowser, "\u{1F4C2}", "File Browser"), (PrefsTab::Audio, "\u{1F3B5}", "Audio"), (PrefsTab::AudioHardware, "\u{1F50A}", "Audio HW"), (PrefsTab::Proxy, "\u{1F4F9}", "Proxy"), (PrefsTab::ExternalTools, "\u{1F517}", "External Tools")]),
                            ("COLOR & EXPORT", &[(PrefsTab::Color, "\u{1F3A8}", "Color"), (PrefsTab::Export, "\u{2B07}", "Export")]),
                            ("INTERFACE", &[(PrefsTab::Title, "T", "Titles"), (PrefsTab::Keyboard, "\u{2328}", "Keyboard"), (PrefsTab::Advanced, "\u{2699}", "Advanced")]),
                            ("ABOUT", &[(PrefsTab::About, "\u{2139}", "About Klipp")]),
                        ];

                        // Auto-switch to first matching tab when search changes
                        if searching {
                            let current_matches = self.tab_matches_search(self.active_tab);
                            if !current_matches {
                                for (_, items) in categories {
                                    for (tab, _, _) in *items {
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
                            let visible: Vec<_> = items.iter().filter(|(tab, _, _)| self.tab_matches_search(*tab)).collect();
                            if visible.is_empty() { continue; }

                            if !searching { ui.add_space(8.0); }
                            self.sidebar_category(ui, cat_name, theme);
                            for (tab, icon, label) in visible {
                                self.sidebar_item(ui, *tab, icon, label, theme);
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
                bottom_painter.rect_filled(done_rect, 6.0, done_bg);
                bottom_painter.text(done_rect.center(), egui::Align2::CENTER_CENTER, "Done", egui::FontId::proportional(12.0).clone(), theme.text_on_accent);
                if done_resp.clicked() { open = false; }
                // Reset defaults button
                let reset_rect = egui::Rect::from_min_size(
                    egui::pos2(done_rect.left() - 120.0, bottom_rect.top() + 8.0),
                    egui::vec2(106.0, 28.0),
                );
                let reset_resp = ui.allocate_rect(reset_rect, egui::Sense::click());
                let reset_bg = if reset_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
                bottom_painter.rect_filled(reset_rect, 6.0, reset_bg);
                bottom_painter.rect_stroke(reset_rect, 6.0, egui::Stroke::new(1.0_f32, theme.border_light));
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
    fn tab_search_keywords(tab: PrefsTab) -> &'static [&'static str] {
        match tab {
            PrefsTab::General => &["general", "auto-save", "backup", "recent projects", "project", "default folder", "inspector", "startup", "launch", "open last"],
            PrefsTab::Appearance => &["appearance", "theme", "ui scale", "accent color", "role colors", "video", "dialogue", "music", "sfx"],
            PrefsTab::System => &["system", "gpu", "hardware", "decode", "threads", "frame queue", "playback resolution", "nvdec", "vaapi", "cuda"],
            PrefsTab::Playback => &["playback", "quality", "background render", "pre-roll", "jkl", "speed", "loop"],
            PrefsTab::MediaCache => &["cache", "thumbnail", "waveform", "render cache", "cache size", "transcode", "clear cache"],
            PrefsTab::Editing => &["editing", "snap", "zoom", "playhead", "magnetic", "timeline", "transition", "duration", "multicam", "angle", "sync", "tracking", "tracker", "search radius"],
            PrefsTab::FileBrowser => &["file browser", "browser", "files", "thumbnail", "thumb", "skim", "skimming", "hover", "sort", "filter", "favorites", "pinned", "folders"],
            PrefsTab::Audio => &["audio", "pan law", "fade", "curve", "loudness", "normalize", "peak hold", "anti-click", "mixing"],
            PrefsTab::AudioHardware => &["audio hardware", "output device", "sample rate", "buffer size", "monitoring", "pipewire", "alsa"],
            PrefsTab::Proxy => &["proxy", "auto-generate", "resolution", "proxy cache", "transcode"],
            PrefsTab::ExternalTools => &["external tools", "external app", "handoff", "round-trip", "round trip", "gimp", "krita", "audacity", "kdenlive", "editor", "image editor", "audio editor", "video editor"],
            PrefsTab::Color => &["color", "working space", "rec.709", "lut", "interpolation", "scopes", "vectorscope", "skin angle"],
            PrefsTab::Export => &["export", "encoder", "h.264", "h.265", "prores", "color space", "audio sample rate", "overwrite", "parallel render"],
            PrefsTab::Title => &["title", "font", "size", "alignment", "duration", "safe area", "text"],
            PrefsTab::Timeline => &["timeline", "clip colors", "clip gap", "trim handle", "track height", "waveform"],
            PrefsTab::Viewer => &["viewer", "overlay", "scope quality", "false color", "zebra"],
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
            ui.label(egui::RichText::new(label).size(10.0).color(theme.text_accent).strong());
        });
        ui.add_space(2.0);
    }

    /// Draw a clickable sidebar item with icon.
    fn sidebar_item(&mut self, ui: &mut egui::Ui, tab: PrefsTab, icon: &str, label: &str, theme: &KlippTheme) {
        let is_active = self.active_tab == tab;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(Self::SIDEBAR_W - 16.0, 26.0), egui::Sense::click());
        let rect = rect.translate(egui::vec2(8.0, 0.0));

        if is_active {
            // Solid amber background — use dark text for readability
            ui.painter().rect_filled(rect, 5.0, theme.accent);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 5.0, theme.bg_hover);
        }

        let text_color = if is_active { theme.text_on_accent } else { theme.text_primary };
        ui.painter().text(
            egui::pos2(rect.left() + 10.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            icon,
            egui::FontId::proportional(13.0),
            text_color,
        );
        ui.painter().text(
            egui::pos2(rect.left() + 30.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.0),
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

    /// Draw a section header (e.g., "PROJECT", "INSPECTOR") with subtle background.
    fn section_header(ui: &mut egui::Ui, label: &str, theme: &KlippTheme) {
        ui.add_space(8.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 4.0, theme.bg_panel);
        ui.painter().text(
            egui::pos2(rect.left() + 14.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(10.0),
            theme.text_accent,
        );
    }

    /// Draw a setting row with label on left and a widget on right (returned via closure).
    fn setting_row(ui: &mut egui::Ui, label: &str, theme: &KlippTheme, add_right: impl FnOnce(&mut egui::Ui)) {
        Self::setting_row_inner(ui, label, None, theme, add_right);
    }

    fn setting_row_with_subtitle(ui: &mut egui::Ui, label: &str, subtitle: &str, theme: &KlippTheme, add_right: impl FnOnce(&mut egui::Ui)) {
        Self::setting_row_inner(ui, label, Some(subtitle), theme, add_right);
    }

    fn setting_row_inner(ui: &mut egui::Ui, label: &str, subtitle: Option<&str>, theme: &KlippTheme, add_right: impl FnOnce(&mut egui::Ui)) {
        // Pre-measure the subtitle so the row grows to fit its
        // wrapped height instead of clipping past the right-side
        // widgets. Allow up to 3 lines — beyond that the subtitle
        // belongs in a dialog or tooltip, not inline.
        const LABEL_PAD_X: f32 = 14.0;
        const SUBTITLE_TOP: f32 = 28.0;  // first subtitle line baseline offset from row.top
        const SUBTITLE_LINE_H: f32 = 14.0;
        const MAX_SUBTITLE_ROWS: usize = 3;
        const WIDGET_AREA_W: f32 = 250.0;

        let (row_h, subtitle_galley) = if let Some(sub) = subtitle {
            let mut job = egui::text::LayoutJob::default();
            job.append(
                sub,
                0.0,
                egui::text::TextFormat {
                    font_id: egui::FontId::proportional(10.0),
                    color: theme.text_dim,
                    ..Default::default()
                },
            );
            job.wrap = egui::text::TextWrapping {
                max_width: (ui.available_width() - LABEL_PAD_X * 2.0).max(0.0),
                max_rows: MAX_SUBTITLE_ROWS,
                break_anywhere: false,
                overflow_character: Some('\u{2026}'),
            };
            let g = ui.ctx().fonts(|f| f.layout_job(job));
            let rows = g.rows.len().max(1);
            let h = SUBTITLE_TOP + rows as f32 * SUBTITLE_LINE_H + 4.0;
            (h.max(48.0), Some(g))
        } else {
            (36.0, None)
        };

        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme.bg_panel_alt);
        // Subtle separator
        ui.painter().line_segment(
            [egui::pos2(rect.left() + LABEL_PAD_X, rect.bottom()), egui::pos2(rect.right() - LABEL_PAD_X, rect.bottom())],
            egui::Stroke::new(0.5_f32, theme.bg_raised),
        );
        // Label (+ optional multi-line subtitle)
        if let Some(g) = subtitle_galley {
            ui.painter().text(
                egui::pos2(rect.left() + LABEL_PAD_X, rect.top() + 16.0),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(12.0),
                theme.text_primary,
            );
            ui.painter().galley(
                egui::pos2(rect.left() + LABEL_PAD_X, rect.top() + SUBTITLE_TOP),
                g,
                theme.text_dim,
            );
        } else {
            ui.painter().text(
                egui::pos2(rect.left() + LABEL_PAD_X, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(12.0),
                theme.text_primary,
            );
        }
        // Right-aligned widget area. When a subtitle is present,
        // pin the widget to the TOP band of the row (at the label's
        // vertical level) so the subtitle wrapping below has the
        // full row width to itself.
        let widget_bottom = if subtitle.is_some() {
            rect.top() + 26.0
        } else {
            rect.bottom() - 4.0
        };
        let widget_rect = egui::Rect::from_min_max(
            egui::pos2(rect.right() - WIDGET_AREA_W, rect.top() + 4.0),
            egui::pos2(rect.right() - LABEL_PAD_X, widget_bottom),
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

        // ── STARTUP section ──
        Self::section_header(ui, "STARTUP", theme);

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

        Self::setting_row(ui, "Default project folder", theme, |ui| {
            if ui.button("Browse...").clicked() {
                if let Some(path) = rfd::FileDialog::new().set_title("Default Project Folder").pick_folder() {
                    prefs.general.default_project_dir = Some(path);
                    *dirty = true;
                }
            }
            let dir_text = prefs.general.default_project_dir.as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "(not set)".into());
            ui.label(egui::RichText::new(&dir_text).size(11.0).color(theme.text_dim));
        });

        Self::setting_row(ui, "Recent projects to show", theme, |ui| {
            let count = prefs.general.recent_projects.len();
            let _display_counts = [5u32, 10, 15, 20];
            // For now show current count — this is informational
            egui::ComboBox::from_id_salt("recent_count")
                .selected_text(format!("{}", count.min(20)))
                .width(50.0)
                .show_ui(ui, |_ui| {
                    // Placeholder — recent project count is managed by the max in add_recent_project
                });
        });

        // ── INSPECTOR section ──
        Self::section_header(ui, "INSPECTOR", theme);

        Self::setting_row(ui, "Remember collapsed sections", theme, |ui| {
            if ui.checkbox(&mut prefs.general.remember_inspector_layout, "").changed() {
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Show keyboard shortcut hints", theme, |ui| {
            // Placeholder toggle — add a pref field for this later
            let mut show_hints = true;
            ui.checkbox(&mut show_hints, "");
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

        Self::setting_row(ui, "Playback quality", theme, |ui| {
            use klipp_core::PlaybackQuality;
            egui::ComboBox::from_id_salt("pb_quality")
                .selected_text(match prefs.playback.playback_quality {
                    PlaybackQuality::Full => "Full",
                    PlaybackQuality::Half => "Half",
                    PlaybackQuality::Quarter => "Quarter",
                })
                .width(80.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.playback.playback_quality, PlaybackQuality::Full, "Full").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.playback.playback_quality, PlaybackQuality::Half, "Half").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.playback.playback_quality, PlaybackQuality::Quarter, "Quarter").changed() { *dirty = true; }
                });
        });

        Self::setting_row_with_subtitle(ui, "Background render", "Pre-render complex sections in background", theme, |ui| {
            let mut val = prefs.playback.background_render;
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
            "Use the lighter skim file for playback when source media is too heavy (mirrors DaVinci Optimised Media / FCP Proxy)",
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

        Self::setting_row_with_subtitle(ui, "JKL speed steps", "Multiplier per J/L press", theme, |ui| {
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
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
                if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
                if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
                    if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
                prefs.editing.playhead_follows_playback = val;
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Magnetic timeline", theme, |ui| {
            let mut val = prefs.editing.magnetic_timeline;
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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

        // ── MULTICAM section ──
        Self::section_header(ui, "MULTICAM", theme);

        Self::setting_row(ui, "Auto-show angle viewer", theme, |ui| {
            let mut val = prefs.editing.multicam_auto_viewer;
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
                prefs.editing.multicam_auto_viewer = val;
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Show angle badges on timeline", theme, |ui| {
            let mut val = prefs.editing.multicam_show_angles;
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
                // Dispatch through AppCommand for live-sync. Was
                // covered by the debounced 1s re-sync block before
                // T-PREFS-LIVE-SYNC; now applies immediately.
                prefs.editing.multicam_show_angles = val;
                *dirty = true;
                cmds.push(AppCommand::SetMulticamShowAngles(val));
            }
        });

        Self::setting_row(ui, "Default sync method", theme, |ui| {
            use crate::prefs::MulticamSyncMethod;
            egui::ComboBox::from_id_salt("multicam_sync")
                .selected_text(match prefs.editing.multicam_sync_method {
                    MulticamSyncMethod::Manual => "Manual",
                    MulticamSyncMethod::Timecode => "Timecode",
                    MulticamSyncMethod::AudioWaveform => "Audio Waveform",
                })
                .width(130.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.editing.multicam_sync_method, MulticamSyncMethod::Manual, "Manual").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.editing.multicam_sync_method, MulticamSyncMethod::Timecode, "Timecode").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.editing.multicam_sync_method, MulticamSyncMethod::AudioWaveform, "Audio Waveform").changed() { *dirty = true; }
                });
        });

        // ── TRACKING section ──
        Self::section_header(ui, "TRACKING", theme);

        Self::setting_row_with_subtitle(ui, "Search radius", "Pixel radius for tracker matching", theme, |ui| {
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
                if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
                if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
                if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
                if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
                if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
                if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
                if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
                if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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

        Self::setting_row(ui, "Default fade duration", theme, |ui| {
            let mut v_secs = prefs.audio.default_fade_duration_ms as f32 / 1000.0;
            if ui.add(egui::DragValue::new(&mut v_secs).range(0.0..=5.0).speed(0.05).suffix(" s").fixed_decimals(1)).changed() {
                prefs.audio.default_fade_duration_ms = (v_secs * 1000.0) as u32;
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Default fade curve", theme, |ui| {
            egui::ComboBox::from_id_salt("fade_curve")
                .selected_text(match prefs.audio.default_fade_curve {
                    klipp_core::FadeCurve::Linear => "Linear",
                    klipp_core::FadeCurve::Exponential => "Exponential",
                    klipp_core::FadeCurve::Logarithmic => "Logarithmic",
                    klipp_core::FadeCurve::SCurve => "S-Curve",
                })
                .width(100.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.audio.default_fade_curve, klipp_core::FadeCurve::Linear, "Linear").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.audio.default_fade_curve, klipp_core::FadeCurve::Exponential, "Exponential").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.audio.default_fade_curve, klipp_core::FadeCurve::Logarithmic, "Logarithmic").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.audio.default_fade_curve, klipp_core::FadeCurve::SCurve, "S-Curve").changed() { *dirty = true; }
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

        Self::setting_row(ui, "Normalize target", theme, |ui| {
            let mut v = prefs.audio.normalize_target_lufs;
            if ui.add(egui::DragValue::new(&mut v).range(-30.0..=-6.0).speed(0.5).suffix(" LUFS").fixed_decimals(0)).changed() {
                prefs.audio.normalize_target_lufs = v; *dirty = true;
            }
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

        Self::setting_row(ui, "Default encoder preset", theme, |ui| {
            let presets = ["H.264 High Quality", "H.264 Fast", "H.265 High Quality", "ProRes 422", "ProRes 422 HQ", "DNxHD 145"];
            egui::ComboBox::from_id_salt("enc_preset")
                .selected_text(prefs.export.default_encoder_preset.as_str())
                .width(140.0)
                .show_ui(ui, |ui| {
                    for p in presets {
                        if ui.selectable_label(prefs.export.default_encoder_preset == p, p).clicked() {
                            prefs.export.default_encoder_preset = p.to_string();
                            *dirty = true;
                        }
                    }
                });
        });

        Self::setting_row(ui, "Color space tagging", theme, |ui| {
            use klipp_core::ColorSpace;
            let label = match prefs.export.color_space_tag {
                ColorSpace::Srgb => "sRGB",
                ColorSpace::Rec709 => "Rec.709",
                ColorSpace::Rec2020 => "Rec.2020",
                ColorSpace::P3D65 => "DCI-P3",
                _ => "Other",
            };
            egui::ComboBox::from_id_salt("export_cs")
                .selected_text(label)
                .width(80.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.export.color_space_tag, ColorSpace::Rec709, "Rec.709").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.export.color_space_tag, ColorSpace::Rec2020, "Rec.2020").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.export.color_space_tag, ColorSpace::P3D65, "DCI-P3").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.export.color_space_tag, ColorSpace::Srgb, "sRGB").changed() { *dirty = true; }
                });
        });

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

        // Default export folder with path + Browse
        {
            let row_h = 36.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, theme.bg_panel_alt);
            ui.painter().line_segment(
                [egui::pos2(rect.left() + 14.0, rect.bottom()), egui::pos2(rect.right() - 14.0, rect.bottom())],
                egui::Stroke::new(0.5_f32, theme.bg_raised),
            );
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "Default export folder",
                egui::FontId::proportional(12.0),
                theme.text_primary,
            );
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 80.0, rect.top() + 6.0),
                egui::vec2(66.0, 24.0),
            );
            let btn_resp = ui.allocate_rect(btn_rect, egui::Sense::click());
            let btn_bg = if btn_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(btn_rect, 4.0, btn_bg);
            ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
            ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, "Browse...", egui::FontId::proportional(11.0), theme.text_primary);
            let display = prefs.export.default_output_dir.as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "~/Videos/Exports".into());
            ui.painter().text(
                egui::pos2(btn_rect.left() - 8.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                &display,
                egui::FontId::proportional(11.0),
                theme.text_dim,
            );
            if btn_resp.clicked() {
                if let Some(path) = rfd::FileDialog::new().set_title("Default Export Folder").pick_folder() {
                    prefs.export.default_output_dir = Some(path);
                    *dirty = true;
                }
            }
        }

        // ── BEHAVIOR section ──
        Self::section_header(ui, "BEHAVIOR", theme);

        Self::setting_row(ui, "Overwrite existing files", theme, |ui| {
            use crate::prefs::OverwriteMode;
            egui::ComboBox::from_id_salt("overwrite_mode")
                .selected_text(match prefs.export.overwrite_mode {
                    OverwriteMode::Ask => "Ask",
                    OverwriteMode::Always => "Always",
                    OverwriteMode::Never => "Never",
                })
                .width(100.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.export.overwrite_mode, OverwriteMode::Ask, "Ask").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.export.overwrite_mode, OverwriteMode::Always, "Always").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.export.overwrite_mode, OverwriteMode::Never, "Never").changed() { *dirty = true; }
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
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
                prefs.proxy.auto_generate_on_import = val;
                *dirty = true;
            }
        });

        Self::setting_row(ui, "Auto-switch during scrub", theme, |ui| {
            let mut val = prefs.proxy.auto_switch_during_scrub;
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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
            let row_h = 36.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, theme.bg_panel_alt);
            ui.painter().line_segment(
                [egui::pos2(rect.left() + 14.0, rect.bottom()), egui::pos2(rect.right() - 14.0, rect.bottom())],
                egui::Stroke::new(0.5_f32, theme.bg_raised),
            );
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "Proxy cache",
                egui::FontId::proportional(12.0),
                theme.text_primary,
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
            if btn_resp.clicked() {
                if let Some(path) = rfd::FileDialog::new().set_title("Proxy Cache Folder").pick_folder() {
                    prefs.proxy.proxy_cache_dir = Some(path);
                    *dirty = true;
                }
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
                let resp = ui.add(crate::widgets::toggle::toggle(&mut val))
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

    fn show_system(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, theme: &KlippTheme) {
        // Page heading with "Restart required" badge
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("System").strong().size(16.0).color(theme.text_primary));
            ui.add_space(8.0);
            // Restart required badge
            let badge_text = "Restart required";
            let badge_font = egui::FontId::proportional(10.0);
            let text_galley = ui.painter().layout_no_wrap(badge_text.to_string(), badge_font.clone(), theme.accent);
            let badge_size = egui::vec2(text_galley.size().x + 12.0, 20.0);
            let (badge_rect, _) = ui.allocate_exact_size(badge_size, egui::Sense::hover());
            ui.painter().rect_stroke(badge_rect, 4.0, egui::Stroke::new(1.0_f32, theme.accent));
            ui.painter().text(badge_rect.center(), egui::Align2::CENTER_CENTER, badge_text, badge_font, theme.accent);
        });
        ui.add_space(4.0);

        // ── GPU section ──
        Self::section_header(ui, "GPU", theme);

        Self::setting_row_with_subtitle(ui, "GPU preference", "Affects effects, color grading, and decode", theme, |ui| {
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
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
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

        // Theme picker — lists every entry in the theme registry
        // (built-ins + user TOMLs from ~/.config/klipp/themes/).
        // Drop a TOML file in that directory and restart the app to
        // see it appear here. The selected name is stored as
        // `prefs.appearance.theme_name`; the registry on the
        // appended live-reload path resolves it back to a KlippTheme.
        Self::setting_row_with_subtitle(
            ui,
            "Theme",
            "Built-in + user themes from ~/.config/klipp/themes/*.toml",
            theme,
            |ui| {
                let current = prefs.appearance.theme_name.clone();
                let names: Vec<(String, crate::theme::ThemeSource)> = theme_registry
                    .iter()
                    .map(|(n, s)| (n.to_string(), s))
                    .collect();
                egui::ComboBox::from_id_salt("theme_picker")
                    .selected_text(&current)
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for (name, source) in names {
                            let label = match source {
                                crate::theme::ThemeSource::Builtin => name.clone(),
                                crate::theme::ThemeSource::User => format!("{name}  •  user"),
                            };
                            if ui
                                .selectable_value(&mut prefs.appearance.theme_name, name, label)
                                .changed()
                            {
                                *dirty = true;
                            }
                        }
                    });
            },
        );

        // UI scale
        Self::setting_row_with_subtitle(ui, "UI scale", "Auto matches your system DPI settings", theme, |ui| {
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
                            // Apply zoom factor immediately
                            let zoom = if *val == 0 { 1.0 } else { *val as f32 / 100.0 };
                            ui.ctx().set_zoom_factor(zoom);
                            *dirty = true;
                        }
                    }
                });
        });

        // Accent color — row with subtitle and color preset circles
        {
            let row_h = 48.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, theme.bg_panel_alt);
            ui.painter().line_segment(
                [egui::pos2(rect.left() + 14.0, rect.bottom()), egui::pos2(rect.right() - 14.0, rect.bottom())],
                egui::Stroke::new(0.5_f32, theme.bg_raised),
            );
            // Label + subtitle
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.top() + 14.0),
                egui::Align2::LEFT_CENTER,
                "Accent color",
                egui::FontId::proportional(12.0),
                theme.text_primary,
            );
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.top() + 32.0),
                egui::Align2::LEFT_CENTER,
                "Used for selections and active state",
                egui::FontId::proportional(10.0),
                theme.text_dim,
            );
            // Color preset circles (right-aligned)
            let presets: &[[u8; 3]] = &[
                [0xff, 0x5a, 0x3c], // warm red (default)
                [0x40, 0x80, 0xe0], // blue
                [0x40, 0xb0, 0x60], // green
                [0xe0, 0x70, 0x70], // red/pink
                [0xa0, 0x60, 0xc0], // purple
            ];
            let circle_r = 14.0;
            let spacing = 6.0;
            let total_w = presets.len() as f32 * (circle_r * 2.0 + spacing) - spacing;
            let start_x = rect.right() - 14.0 - total_w;
            for (i, color) in presets.iter().enumerate() {
                let cx = start_x + i as f32 * (circle_r * 2.0 + spacing) + circle_r;
                let cy = rect.center().y;
                let c = egui::Color32::from_rgb(color[0], color[1], color[2]);
                let is_selected = prefs.appearance.accent_color == *color;
                // Clickable area
                let click_rect = egui::Rect::from_center_size(egui::pos2(cx, cy), egui::vec2(circle_r * 2.0 + 2.0, circle_r * 2.0 + 2.0));
                let resp = ui.allocate_rect(click_rect, egui::Sense::click());
                ui.painter().circle_filled(egui::pos2(cx, cy), circle_r, c);
                if is_selected {
                    ui.painter().circle_stroke(egui::pos2(cx, cy), circle_r + 2.0, egui::Stroke::new(2.0_f32, egui::Color32::WHITE));
                }
                if resp.clicked() {
                    prefs.appearance.accent_color = *color;
                    prefs.appearance.playhead_color = *color;
                    *dirty = true;
                }
            }
        }

        // ── ROLE COLORS section ──
        Self::section_header(ui, "ROLE COLORS", theme);

        // Helper for role color row with swatch + Edit button
        let role_color_row = |ui: &mut egui::Ui, label: &str, rgb: &mut [u8; 3], dirty: &mut bool, _id_salt: &str| {
            let row_h = 36.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, theme.bg_panel_alt);
            ui.painter().line_segment(
                [egui::pos2(rect.left() + 14.0, rect.bottom()), egui::pos2(rect.right() - 14.0, rect.bottom())],
                egui::Stroke::new(0.5_f32, theme.bg_raised),
            );
            // Label
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(12.0),
                theme.text_primary,
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

        Self::setting_row(ui, "Output device", theme, |ui| {
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

        Self::setting_row(ui, "Monitoring mode", theme, |ui| {
            use crate::prefs::MonitoringMode;
            egui::ComboBox::from_id_salt("monitor_mode")
                .selected_text(match prefs.audio_hardware.monitoring_mode {
                    MonitoringMode::Software => "Software",
                    MonitoringMode::Hardware => "Hardware (direct)",
                })
                .width(120.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.audio_hardware.monitoring_mode, MonitoringMode::Software, "Software").changed() { *dirty = true; }
                    if ui.selectable_value(&mut prefs.audio_hardware.monitoring_mode, MonitoringMode::Hardware, "Hardware (direct)").changed() { *dirty = true; }
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
                            egui::RichText::new("default")
                                .size(10.0)
                                .color(theme.accent),
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
            let row_h = 36.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, theme.bg_panel_alt);
            ui.painter().line_segment(
                [egui::pos2(rect.left() + 14.0, rect.bottom()), egui::pos2(rect.right() - 14.0, rect.bottom())],
                egui::Stroke::new(0.5_f32, theme.bg_raised),
            );
            // Label
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(12.0),
                theme.text_primary,
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
            if btn_resp.clicked() {
                if let Some(p) = rfd::FileDialog::new().set_title(label).pick_folder() {
                    *path = Some(p); *dirty = true;
                }
            }
        };

        cache_dir_row(ui, "Thumbnail cache", &mut prefs.media_cache.filmstrip_cache_dir, "~/.cache/klipp/thumbnails", dirty, "thumb_dir");
        cache_dir_row(ui, "Audio cache", &mut prefs.media_cache.audio_cache_dir, "~/.cache/klipp/audio-cache", dirty, "wave_dir");
        cache_dir_row(ui, "Render cache", &mut prefs.media_cache.render_cache_dir, "~/.cache/klipp/render", dirty, "render_dir");

        // ── RENDER CACHE section ──
        Self::section_header(ui, "RENDER CACHE", theme);

        Self::setting_row(ui, "Smart render cache", theme, |ui| {
            let mut val = prefs.media_cache.enable_render_cache;
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
                prefs.media_cache.enable_render_cache = val;
                cmds.push(AppCommand::SetRenderCacheEnabled(val));
                *dirty = true;
            }
        });

        // ── LIMITS section ──
        Self::section_header(ui, "LIMITS", theme);

        Self::setting_row(ui, "Max cache size", theme, |ui| {
            let mut v = prefs.media_cache.max_cache_size_gb;
            if ui.add(egui::DragValue::new(&mut v).range(1..=500).speed(1.0).suffix(" GB")).changed() {
                prefs.media_cache.max_cache_size_gb = v; *dirty = true;
            }
        });

        Self::setting_row(ui, "Thumbnail resolution", theme, |ui| {
            let sizes = [(128_u32, "128px"), (192, "192px"), (256, "256px"), (320, "320px")];
            let current = sizes.iter()
                .find(|(v, _)| *v == prefs.media_cache.thumbnail_height)
                .map(|(_, l)| *l).unwrap_or("256px");
            egui::ComboBox::from_id_salt("thumb_res")
                .selected_text(current)
                .width(70.0)
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
                 \u{2022}  Standard — FCP / DaVinci proxy equivalent\n\
                 \u{2022}  Low      — ~0.5x Standard, visible compression\n\
                 \u{2022}  Draft    — ~0.25x Standard, blocky but viable\n\n\
                 Cache is regenerated on demand — delete anytime in the \
                 Cache panel to reclaim space.",
            );
        });

        // ── CACHE SIZES section ──
        Self::section_header(ui, "CACHE SIZES", theme);

        let klipp_cache = dirs::cache_dir().unwrap_or_default().join("klipp");
        let render_dir = prefs.media_cache.render_cache_dir.clone()
            .unwrap_or_else(|| klipp_cache.join("render"));
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
            ui.label(egui::RichText::new("Color").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

        // ── LUTs section ──
        // Working colorspace lives on the project (Project.working_colorspace),
        // not on user prefs — it's a creative decision that travels with the
        // project, the way every professional NLE handles it.
        Self::section_header(ui, "LUTs", theme);

        Self::setting_row_with_subtitle(ui, "Viewer LUT", "Applied after color pipeline for display", theme, |ui| {
            let display = prefs.color.viewer_lut_path.as_ref()
                .map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string())
                .unwrap_or_else(|| "None".into());
            egui::ComboBox::from_id_salt("viewer_lut")
                .selected_text(&display)
                .width(90.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(prefs.color.viewer_lut_path.is_none(), "None").clicked() {
                        prefs.color.viewer_lut_path = None; *dirty = true;
                    }
                    if ui.selectable_label(false, "Browse...").clicked() {
                        if let Some(p) = rfd::FileDialog::new().set_title("Viewer LUT").add_filter("LUT", &["cube", "3dl"]).pick_file() {
                            prefs.color.viewer_lut_path = Some(p); *dirty = true;
                        }
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
                if ui.checkbox(&mut prefs.color.port_snap_enabled, "").changed() {
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

        // ── TEXT DEFAULTS section ──
        Self::section_header(ui, "TEXT DEFAULTS", theme);

        Self::setting_row(ui, "Default font", theme, |ui| {
            let display = if prefs.title.default_font.is_empty() { "System default" } else { &prefs.title.default_font };
            egui::ComboBox::from_id_salt("title_font")
                .selected_text(display)
                .width(130.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(prefs.title.default_font.is_empty(), "System default").clicked() {
                        prefs.title.default_font.clear(); *dirty = true;
                    }
                    for font in ["Arial", "Helvetica", "Inter", "Roboto", "Open Sans", "Montserrat"] {
                        if ui.selectable_label(prefs.title.default_font == font, font).clicked() {
                            prefs.title.default_font = font.to_string(); *dirty = true;
                        }
                    }
                });
        });

        Self::setting_row(ui, "Default size", theme, |ui| {
            let mut v = prefs.title.default_size_pt;
            if ui.add(egui::DragValue::new(&mut v).range(8.0..=200.0).speed(0.5).suffix(" pt").fixed_decimals(0)).changed() {
                prefs.title.default_size_pt = v; *dirty = true;
            }
        });

        Self::setting_row(ui, "Default color", theme, |ui| {
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

        Self::setting_row(ui, "Title-safe area overlay", theme, |ui| {
            let mut val = prefs.title.safe_area_overlay;
            if ui.add(crate::widgets::toggle::toggle(&mut val)).changed() {
                prefs.title.safe_area_overlay = val;
                *dirty = true;
            }
        });
    }

    fn show_timeline(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, _theme: &KlippTheme) {
        ui.label(egui::RichText::new("Clip Colors").strong().size(12.0));
        ui.add_space(4.0);
        let mut colors_changed = false;
        for (i, color) in prefs.timeline.clip_colors.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("Color {}:", i + 1));
                let mut c = egui::Color32::from_rgb(color[0], color[1], color[2]);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    *color = [c.r(), c.g(), c.b()];
                    colors_changed = true;
                }
            });
        }
        if colors_changed { *dirty = true; }

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Dimensions").strong().size(12.0));
        ui.add_space(4.0);

        let dim_row = |ui: &mut egui::Ui, label: &str, val: &mut f32, min: f32, max: f32, suffix: &str, dirty: &mut bool| {
            ui.horizontal(|ui| {
                ui.label(label);
                let mut v = *val;
                if ui.add(egui::DragValue::new(&mut v).range(min..=max).speed(0.5).suffix(suffix)).changed() {
                    *val = v; *dirty = true;
                }
            });
        };

        dim_row(ui, "Clip gap:", &mut prefs.timeline.clip_gap_px, 0.0, 20.0, " px", dirty);
        dim_row(ui, "Trim handle:", &mut prefs.timeline.trim_handle_width, 6.0, 30.0, " px", dirty);
        dim_row(ui, "Ruler height:", &mut prefs.timeline.ruler_height, 16.0, 48.0, " px", dirty);
        dim_row(ui, "Clip corner radius:", &mut prefs.timeline.clip_corner_radius, 0.0, 16.0, " px", dirty);
        dim_row(ui, "Overview bar:", &mut prefs.timeline.overview_bar_height, 12.0, 48.0, " px", dirty);
        dim_row(ui, "Min track height:", &mut prefs.timeline.min_track_height, 16.0, 64.0, " px", dirty);
        dim_row(ui, "Max track height:", &mut prefs.timeline.max_track_height, 100.0, 500.0, " px", dirty);
        dim_row(ui, "Text track height:", &mut prefs.timeline.text_track_height, 20.0, 80.0, " px", dirty);

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Selection").strong().size(12.0));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Selection border colour:");
            let sc = &mut prefs.timeline.clip_selection_color;
            let mut c = egui::Color32::from_rgb(sc[0], sc[1], sc[2]);
            if ui.color_edit_button_srgba(&mut c).changed() {
                *sc = [c.r(), c.g(), c.b()];
                *dirty = true;
            }
        });
        dim_row(
            ui,
            "Selection border width:",
            &mut prefs.timeline.clip_selection_border_width,
            1.0,
            8.0,
            " px",
            dirty,
        );

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Clip Label Strip").strong().size(12.0));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Background colour:");
            let bg = &mut prefs.timeline.clip_label_bg_color;
            let mut c = egui::Color32::from_rgb(bg[0], bg[1], bg[2]);
            if ui.color_edit_button_srgba(&mut c).changed() {
                *bg = [c.r(), c.g(), c.b()];
                *dirty = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Text colour:");
            let tx = &mut prefs.timeline.clip_label_text_color;
            let mut c = egui::Color32::from_rgb(tx[0], tx[1], tx[2]);
            if ui.color_edit_button_srgba(&mut c).changed() {
                *tx = [c.r(), c.g(), c.b()];
                *dirty = true;
            }
        });
    }

    fn show_viewer(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, dirty: &mut bool, theme: &KlippTheme) {
        ui.label(egui::RichText::new("Safe Areas").strong().size(12.0));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Action safe:");
            let mut v = prefs.viewer.action_safe_pct * 100.0;
            if ui.add(egui::Slider::new(&mut v, 70.0..=100.0).suffix("%")).changed() {
                prefs.viewer.action_safe_pct = v / 100.0; *dirty = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Title safe:");
            let mut v = prefs.viewer.title_safe_pct * 100.0;
            if ui.add(egui::Slider::new(&mut v, 50.0..=100.0).suffix("%")).changed() {
                prefs.viewer.title_safe_pct = v / 100.0; *dirty = true;
            }
        });

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Zebras").strong().size(12.0));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("High threshold (clipping):");
            let mut v = prefs.viewer.zebra_high_threshold;
            if ui.add(egui::DragValue::new(&mut v).range(128..=255).speed(1.0)).changed() {
                prefs.viewer.zebra_high_threshold = v; *dirty = true;
            }
            // Show approximate IRE equivalent (BT.709 legal range: 16=0 IRE, 235=100 IRE)
            let ire = (v as f32 - 16.0) / 2.19;
            ui.label(egui::RichText::new(format!("{ire:.0}% IRE")).color(theme.text_dim).size(11.0));
        });
        ui.horizontal(|ui| {
            ui.label("Low threshold (underexposure):");
            let mut v = prefs.viewer.zebra_low_threshold;
            if ui.add(egui::DragValue::new(&mut v).range(0..=128).speed(1.0)).changed() {
                prefs.viewer.zebra_low_threshold = v; *dirty = true;
            }
            let ire = (v as f32 - 16.0) / 2.19;
            ui.label(egui::RichText::new(format!("{ire:.0}% IRE")).color(theme.text_dim).size(11.0));
        });

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Focus Peaking").strong().size(12.0));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Sensitivity:");
            let mut v = prefs.viewer.focus_peaking_sensitivity;
            if ui.add(egui::Slider::new(&mut v, 0.0..=1.0)).changed() {
                prefs.viewer.focus_peaking_sensitivity = v; *dirty = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Color:");
            let pc = &mut prefs.viewer.focus_peaking_color;
            let mut c = egui::Color32::from_rgb(pc[0], pc[1], pc[2]);
            if ui.color_edit_button_srgba(&mut c).changed() {
                *pc = [c.r(), c.g(), c.b()];
                *dirty = true;
            }
        });

        ui.add_space(12.0);
        ui.label(egui::RichText::new("False Color").strong().size(12.0));
        ui.add_space(4.0);
        Self::show_false_color_editor(ui, prefs, dirty, theme);

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Overlays").strong().size(12.0));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Grid color:");
            let gc = &mut prefs.viewer.grid_color;
            let mut c = egui::Color32::from_rgba_unmultiplied(gc[0], gc[1], gc[2], gc[3]);
            if ui.color_edit_button_srgba(&mut c).changed() {
                *gc = [c.r(), c.g(), c.b(), c.a()];
                *dirty = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Checkerboard tile:");
            let mut v = prefs.viewer.checkerboard_tile_size;
            if ui.add(egui::DragValue::new(&mut v).range(4.0..=32.0).speed(0.5).suffix(" px")).changed() {
                prefs.viewer.checkerboard_tile_size = v; *dirty = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Transform handle size:");
            let mut v = prefs.viewer.transform_handle_size;
            if ui.add(egui::DragValue::new(&mut v).range(4.0..=20.0).speed(0.5).suffix(" px")).changed() {
                prefs.viewer.transform_handle_size = v; *dirty = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Fullscreen overlay timeout:");
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
            ui.label("Preset:");
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
                format!("{unmapped} luma value(s) unmapped (will show source color)"));
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
            ui.label(egui::RichText::new("Color").color(dim_text).size(10.0));
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
            if ui.add(crate::widgets::toggle::toggle(&mut enabled)).changed() {
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

        // ── SHORTCUTS section header with Reset all button ──
        {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 4.0, theme.bg_panel);
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "SHORTCUTS",
                egui::FontId::proportional(10.0),
                theme.text_accent,
            );
            // Reset all to defaults button (right-aligned)
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 130.0, rect.top() + 3.0),
                egui::vec2(116.0, 22.0),
            );
            let btn_resp = ui.allocate_rect(btn_rect, egui::Sense::click());
            let btn_bg = if btn_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
            ui.painter().rect_filled(btn_rect, 4.0, btn_bg);
            ui.painter().rect_stroke(btn_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
            ui.painter().text(btn_rect.center(), egui::Align2::CENTER_CENTER, "Reset all to defaults", egui::FontId::proportional(10.0), theme.text_primary);
            if btn_resp.clicked() {
                *key_bindings = KeyBindings::defaults();
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
            for (i, (a, b, combo)) in conflicts.iter().enumerate() {
                ui.painter().text(
                    egui::pos2(rect.left() + 28.0, rect.top() + 28.0 + i as f32 * 18.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{} and {} both use {}", KeyBindings::action_label(a), KeyBindings::action_label(b), combo.display()),
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

                let row_h = 36.0;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
                ui.painter().rect_filled(rect, 0.0, theme.bg_panel_alt);
                ui.painter().line_segment(
                    [egui::pos2(rect.left() + 14.0, rect.bottom()), egui::pos2(rect.right() - 14.0, rect.bottom())],
                    egui::Stroke::new(0.5_f32, theme.bg_raised),
                );

                // Action label
                ui.painter().text(
                    egui::pos2(rect.left() + 14.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    label,
                    egui::FontId::proportional(12.0),
                    theme.text_primary,
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
                    // No binding — show em dash
                    ui.painter().text(
                        egui::pos2(set_rect.left() - 20.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        "\u{2014}",
                        egui::FontId::proportional(12.0),
                        theme.text_dim,
                    );
                }
            }
        }
    }

    fn show_about(&mut self, ui: &mut egui::Ui, theme: &KlippTheme) {
        // Page heading
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("About Klipp").strong().size(16.0).color(theme.text_primary));
        });
        ui.add_space(4.0);

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
            let display = if model.len() > 40 { format!("{}...", &model[..37]) } else { model };
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
            let row_h = 36.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, theme.bg_panel_alt);
            ui.painter().line_segment(
                [egui::pos2(rect.left() + 14.0, rect.bottom()), egui::pos2(rect.right() - 14.0, rect.bottom())],
                egui::Stroke::new(0.5_f32, theme.bg_raised),
            );
            ui.painter().text(
                egui::pos2(rect.left() + 14.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                name,
                egui::FontId::proportional(12.0),
                theme.text_primary,
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
            ui.painter().rect_filled(rect, 0.0, theme.bg_panel_alt);
            ui.painter().line_segment(
                [egui::pos2(rect.left() + 14.0, rect.bottom()), egui::pos2(rect.right() - 14.0, rect.bottom())],
                egui::Stroke::new(0.5_f32, theme.bg_raised),
            );
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
        ack_row(ui, "OpenColorIO / ACES \u{2014} color science references");
        ack_row(ui, "The Rust community and crate ecosystem");

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("\u{00a9} 2025\u{2013}2026 David Bolin. All rights reserved.").size(10.0).color(theme.text_dim));
        });
    }
}
