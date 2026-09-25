use crate::prefs::RecentProject;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use std::collections::HashMap;
use std::path::PathBuf;

/// The narrowest a card may get before the grid drops a column, and
/// the widest it may grow before one more column is better than more
/// size.
///
/// The card used to be a fixed 220 square, and the column count came
/// from dividing the available width by it — so a wider display got
/// *more small cards*, not a better look at any of them. At 3072 px
/// that meant ten cards across, their metadata truncated, and two
/// thirds of the window empty below.
/// Chosen so a wide window gives *fewer, larger* cards rather than
/// more small ones. At 200 the grid produced twelve columns of 211 px
/// on a 2715 px page — narrower than the fixed 220 it replaced, which
/// is the opposite of the point. At 280 the same page gives nine
/// columns of 287, and a half-width window four of 285: near enough
/// the same card either way, and big enough that the metadata line
/// stops truncating.
/// What a card should be, near enough, at any window width.
const CARD_TARGET_W: f32 = 300.0;
/// The range it may be pushed to when the target does not divide the
/// width evenly.
const CARD_MIN_W: f32 = 240.0;
const CARD_MAX_W: f32 = 340.0;
/// A card is its thumbnail plus two lines of text under it. The
/// thumbnail is 16:9 and takes 58% of the height (`draw_card`), so the
/// card's own aspect follows from that rather than being squared off.
const CARD_ASPECT: f32 = 1.0 / 1.18;
const CARD_GAP: f32 = 16.0;

/// How wide each card is, and how many fit, for a given grid width.
///
/// Columns are chosen to land the card nearest [`CARD_TARGET_W`],
/// rounding rather than flooring, so the card stays about the same
/// size at every window width and the grid simply holds more of them.
///
/// Flooring against a *minimum* is what the first version did, and it
/// only sets a floor: a 1169 px grid took three columns and then
/// stretched each card to 379x447 to fill the row — cards at nearly
/// the maximum size, seven rows deep, which is what "the cards look
/// stretched" was. The same code gave 287 px cards at 2715 px, so it
/// looked correct wherever it was measured and wrong at the size the
/// window actually spends its time.
///
/// Rounding to a target gives 280 at 1169 and 287 at 2715 — the same
/// card either way.
fn card_metrics(grid_width: f32) -> (usize, f32, f32) {
    let cols = (grid_width / CARD_TARGET_W).round().max(1.0) as usize;
    let gaps = CARD_GAP * (cols.saturating_sub(1)) as f32;
    let w = ((grid_width - gaps) / cols as f32).clamp(CARD_MIN_W, CARD_MAX_W);
    (cols, w, w / CARD_ASPECT)
}
const SIDEBAR_W: f32 = 300.0;

#[derive(Clone, Copy, PartialEq, Default)]
enum SortMode {
    #[default]
    Modified,
    Name,
    Size,
    /// How long the cut is. The obvious fourth key for video work, and
    /// `duration_secs` was already on every recent project — it is
    /// printed on the cards, it just could not be sorted by.
    Duration,
}

impl SortMode {
    fn label(self) -> &'static str {
        match self {
            Self::Modified => "Modified",
            Self::Name => "Name",
            Self::Size => "Size",
            Self::Duration => "Duration",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Default)]
enum ViewMode {
    #[default]
    Grid,
    List,
}

#[derive(Default)]
pub struct LaunchScreen {
    search_query: String,
    thumbnail_textures: HashMap<PathBuf, egui::TextureHandle>,
    renaming_project: Option<(PathBuf, String)>,
    logo_texture: Option<egui::TextureHandle>,
    sort_mode: SortMode,
    view_mode: ViewMode,
    /// Projects with an auto-save newer than their last save, re-read
    /// from disk every few seconds rather than every repaint.
    newer_autosave: HashMap<PathBuf, bool>,
    autosave_checked_at: Option<std::time::Instant>,
}

impl LaunchScreen {
    /// Remove a cached thumbnail texture so it gets reloaded on next draw.
    pub fn invalidate_thumbnail(&mut self, path: &std::path::Path) {
        self.thumbnail_textures.remove(path);
    }

    fn ensure_logo_texture(&mut self, ctx: &egui::Context) -> &egui::TextureHandle {
        self.logo_texture.get_or_insert_with(|| {
            let png_bytes = include_bytes!("../../../../assets/branding/klipp-refined/klipp-icon-128.png");
            let image = crate::ui_util::decode_png_or_fallback(png_bytes);
            crate::app::alloc_texture_logged(ctx, "klipp_launch_logo", image, egui::TextureOptions::LINEAR)
        })
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        recent: &[RecentProject],
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        self.ensure_logo_texture(ctx);
        if self.autosave_checked_at.is_none_or(|t| t.elapsed().as_secs() >= 3) {
            self.newer_autosave = recent.iter()
                .map(|rp| (rp.path.clone(), crate::app::has_newer_autosave(&rp.path)))
                .collect();
            self.autosave_checked_at = Some(std::time::Instant::now());
        }
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
                egui::Stroke::new(1.0_f32, theme.border),
            );

            self.draw_sidebar(ui, recent, theme, &mut cmds);
        });

        // Draw main area with project grid
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
        let pad_x = 24.0;
        let button_w = SIDEBAR_W - pad_x * 2.0;

        // ── Logo ──
        let logo_size = 80.0;
        let logo_center = egui::pos2(area.center().x, area.min.y + 55.0);
        let logo_rect = egui::Rect::from_center_size(
            logo_center,
            egui::vec2(logo_size, logo_size),
        );
        let logo_tex = self.logo_texture.as_ref().unwrap();
        ui.painter().image(
            logo_tex.id(),
            logo_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        // "Klipp" title
        let klipp_y = logo_center.y + logo_size * 0.5 + 14.0;
        ui.painter().text(
            egui::pos2(logo_center.x, klipp_y),
            egui::Align2::CENTER_CENTER,
            "Klipp",
            egui::FontId::proportional(28.0),
            theme.accent,
        );

        // Version + platform
        ui.painter().text(
            egui::pos2(logo_center.x, klipp_y + 22.0),
            egui::Align2::CENTER_CENTER,
            // Use the running app's package version, as About does.
            format!("Version {} · Linux", klipp_core::build_info::get().version),
            egui::FontId::proportional(11.0),
            theme.text_dim,
        );

        // ── Action Buttons ──
        let buttons_y = klipp_y + 54.0;

        // + New Project (orange filled)
        let new_rect = egui::Rect::from_min_size(
            egui::pos2(area.min.x + pad_x, buttons_y),
            egui::vec2(button_w, 38.0),
        );
        let new_resp = ui.allocate_rect(new_rect, egui::Sense::click());
        let new_bg = if new_resp.hovered() { theme.accent } else { theme.accent_dim };
        ui.painter().rect_filled(new_rect, 4.0, new_bg);
        ui.painter().text(
            new_rect.center(),
            egui::Align2::CENTER_CENTER,
            "+ New Project",
            egui::FontId::proportional(13.0),
            theme.bg_void,
        );
        if new_resp.clicked() {
            cmds.push(AppCommand::NewProject);
        }

        // Open Project... (outlined)
        let open_rect = egui::Rect::from_min_size(
            egui::pos2(area.min.x + pad_x, buttons_y + 48.0),
            egui::vec2(button_w, 36.0),
        );
        let open_resp = ui.allocate_rect(open_rect, egui::Sense::click());
        let open_bg = if open_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
        ui.painter().rect_filled(open_rect, 4.0, open_bg);
        ui.painter().rect_stroke(open_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
        ui.painter().text(
            open_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Open Project\u{2026}",
            egui::FontId::proportional(13.0),
            theme.text_primary,
        );
        if open_resp.clicked() {
            crate::file_dialog::spawn(move || {
                rfd::FileDialog::new()
                    .set_title("Open Project")
                    .pick_folder()
                    .map(|p| vec![AppCommand::OpenProject { path: p }])
                    .unwrap_or_default()
            });
        }

        // No "Import Media" here.
        //
        // It dispatched `ImportMedia`, which loads the file into the
        // untitled project's media pool — and the launch screen is
        // shown precisely while there is no project open, which
        // importing does not change. So the file went into a pool the
        // user could not see, on a screen that did not move.
        //
        // Importing is a thing you do *to* a project. The two entry
        // points above are the ones that produce one.

        // ── Recent Projects List ──
        let recent_y = buttons_y + 148.0;
        if !recent.is_empty() {
            ui.painter().text(
                egui::pos2(area.min.x + pad_x, recent_y),
                egui::Align2::LEFT_TOP,
                "RECENT",
                egui::FontId::proportional(10.0),
                theme.text_dim,
            );

            // A few, not all of them.
            //
            // This filled the sidebar with as many as would fit — all
            // twenty, on a tall window — beside a grid showing the same
            // twenty. Two complete lists of the same projects, side by
            // side.
            //
            // It is not redundant in principle: the grid follows the
            // chosen sort, this always follows recency, so with sort
            // set to Name or Size it is still the way back to what you
            // had open. That is a shortcut, and a shortcut is short.
            const MAX_RECENT: usize = 5;
            let item_h = 44.0;
            let fits = ((area.max.y - recent_y - 60.0) / item_h).floor().max(1.0) as usize;
            let items_to_show = recent.len().min(fits).min(MAX_RECENT);

            for (i, rp) in recent.iter().take(items_to_show).enumerate() {
                let y = recent_y + 20.0 + i as f32 * item_h;
                let item_rect = egui::Rect::from_min_size(
                    egui::pos2(area.min.x + pad_x - 8.0, y),
                    egui::vec2(button_w + 16.0, item_h - 4.0),
                );
                let resp = ui.allocate_rect(item_rect, egui::Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(item_rect, 4.0, theme.bg_hover);
                }

                // Color bar (small rect matching project color)
                let color = project_color(&rp.name);
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(area.min.x + pad_x, y + 6.0),
                    egui::vec2(28.0, 6.0),
                );
                ui.painter().rect_filled(bar_rect, 2.0, color);

                // Second bar (simulating timeline)
                let bar2_color = darken_color(color, 0.6);
                let bar2_rect = egui::Rect::from_min_size(
                    egui::pos2(area.min.x + pad_x, y + 14.0),
                    egui::vec2(20.0, 3.0),
                );
                ui.painter().rect_filled(bar2_rect, 1.0, bar2_color);

                // Project name
                let name_x = area.min.x + pad_x + 38.0;
                ui.painter().text(
                    egui::pos2(name_x, y + 8.0),
                    egui::Align2::LEFT_TOP,
                    &rp.name,
                    egui::FontId::proportional(12.0),
                    if resp.hovered() { theme.text_primary } else { theme.text_secondary },
                );

                // Relative timestamp
                let time_str = rp.last_saved.as_deref()
                    .map(relative_time)
                    .unwrap_or_default();
                if !time_str.is_empty() {
                    ui.painter().text(
                        egui::pos2(name_x, y + 23.0),
                        egui::Align2::LEFT_TOP,
                        &time_str,
                        egui::FontId::proportional(10.0),
                        theme.text_dim,
                    );
                }

                // Active indicator (orange dot on most recent)
                if i == 0 {
                    let dot_center = egui::pos2(
                        item_rect.max.x - 8.0,
                        item_rect.center().y,
                    );
                    ui.painter().circle_filled(dot_center, 3.5, theme.accent);
                }

                if resp.clicked() {
                    cmds.push(AppCommand::OpenProject { path: rp.path.clone() });
                }
            }
        }

        // ── Footer ──
        let footer_y = area.max.y - 32.0;
        // One item, and it does something.
        //
        // There were three — Account, Settings, Help — and all three
        // allocated a click rect, highlighted on hover, and dropped the
        // response. Three buttons that could not be pressed.
        //
        // "Account" also implied a service: Klipp is a local
        // application with nothing to sign in to. "Settings" asked the
        // user to configure an application they had not opened yet, and
        // could not have worked anyway — the launch screen takes the
        // whole frame and returns before any window is drawn, so a
        // preferences window opened from here would never appear.
        let footer_rect = egui::Rect::from_min_size(
            egui::pos2(area.center().x - 39.0, footer_y),
            egui::vec2(78.0, 20.0),
        );
        let resp = ui
            .allocate_rect(footer_rect, egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            let _ = std::process::Command::new("xdg-open")
                .arg("https://github.com/davidkodar/klipp")
                .spawn();
        }
        ui.painter().text(
            footer_rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{2139} Help",
            egui::FontId::proportional(11.0),
            if resp.hovered() { theme.text_primary } else { theme.text_dim },
        );
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
        let pad = 28.0;

        // ── Header Row ──

        // "Projects" title (large)
        ui.painter().text(
            egui::pos2(area.min.x + pad, area.min.y + pad),
            egui::Align2::LEFT_TOP,
            "Projects",
            egui::FontId::proportional(26.0),
            theme.text_primary,
        );

        // Project count + last modified summary
        let most_recent_time = recent.first()
            .and_then(|rp| rp.last_saved.as_deref())
            .map(relative_time)
            .unwrap_or_default();
        let summary = if recent.is_empty() {
            "No projects".to_string()
        } else {
            let count_str = format!("{} project{}", recent.len(), if recent.len() == 1 { "" } else { "s" });
            if most_recent_time.is_empty() {
                count_str
            } else {
                format!("{count_str} \u{00B7} Last modified {most_recent_time}")
            }
        };
        ui.painter().text(
            egui::pos2(area.min.x + pad, area.min.y + pad + 32.0),
            egui::Align2::LEFT_TOP,
            &summary,
            egui::FontId::proportional(12.0),
            theme.text_dim,
        );

        // ── Controls row: Search | Sort | View toggle ──
        let controls_y = area.min.y + pad + 56.0;
        let controls_right = area.max.x - pad;

        // Search bar, on the grid's left edge.
        //
        // It used to start at `pad + 200.0` — a fixed offset that put
        // it neither under the title nor against anything else, so it
        // floated in the middle of the header lining up with nothing.
        // Sharing the grid's left margin makes the row read as a row.
        let search_w = 260.0_f32.min((area.width() - pad * 2.0) * 0.4);
        let search_rect = egui::Rect::from_min_size(
            egui::pos2(area.min.x + pad, controls_y - 2.0),
            egui::vec2(search_w, 26.0),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(search_rect), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .hint_text("Search projects\u{2026}")
                    .desired_width(search_w),
            );
        });

        // Grid/List toggle (right side)
        let toggle_size = 24.0;
        let list_rect = egui::Rect::from_min_size(
            egui::pos2(controls_right - toggle_size, controls_y),
            egui::vec2(toggle_size, toggle_size),
        );
        let list_resp = ui.allocate_rect(list_rect, egui::Sense::click());
        let list_color = if self.view_mode == ViewMode::List { theme.text_primary } else { theme.text_dim };
        // List icon (three horizontal lines)
        for j in 0..3 {
            let ly = list_rect.min.y + 6.0 + j as f32 * 6.0;
            ui.painter().hline(
                list_rect.min.x + 4.0..=list_rect.max.x - 4.0,
                ly,
                egui::Stroke::new(2.0_f32, list_color),
            );
        }
        if list_resp.clicked() {
            self.view_mode = ViewMode::List;
        }

        let grid_rect = egui::Rect::from_min_size(
            egui::pos2(controls_right - toggle_size * 2.0 - 6.0, controls_y),
            egui::vec2(toggle_size, toggle_size),
        );
        let grid_resp = ui.allocate_rect(grid_rect, egui::Sense::click());
        let grid_color = if self.view_mode == ViewMode::Grid { theme.text_primary } else { theme.text_dim };
        // Grid icon (2x2 squares)
        let gs = 8.0;
        let gp = 2.0;
        for row in 0..2 {
            for col in 0..2 {
                let sr = egui::Rect::from_min_size(
                    egui::pos2(
                        grid_rect.min.x + 3.0 + col as f32 * (gs + gp),
                        grid_rect.min.y + 3.0 + row as f32 * (gs + gp),
                    ),
                    egui::vec2(gs, gs),
                );
                ui.painter().rect_filled(sr, 1.5, grid_color);
            }
        }
        if grid_resp.clicked() {
            self.view_mode = ViewMode::Grid;
        }

        // Sort dropdown
        let sort_x = controls_right - toggle_size * 2.0 - 6.0 - 10.0;
        let sort_w = 90.0;
        let sort_rect = egui::Rect::from_min_size(
            egui::pos2(sort_x - sort_w, controls_y),
            egui::vec2(sort_w, 24.0),
        );
        let sort_resp = ui.allocate_rect(sort_rect, egui::Sense::click());
        let sort_bg = if sort_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
        ui.painter().rect_filled(sort_rect, 4.0, sort_bg);
        ui.painter().rect_stroke(sort_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border));
        ui.painter().text(
            egui::pos2(sort_rect.center().x - 4.0, sort_rect.center().y),
            egui::Align2::CENTER_CENTER,
            self.sort_mode.label(),
            egui::FontId::proportional(12.0),
            theme.text_secondary,
        );
        // Dropdown arrow (painter-drawn triangle)
        {
            let cx = sort_rect.max.x - 14.0;
            let cy = sort_rect.center().y;
            let s = 3.5;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(cx - s, cy - s * 0.5),
                    egui::pos2(cx + s, cy - s * 0.5),
                    egui::pos2(cx, cy + s * 0.7),
                ],
                theme.text_dim,
                egui::Stroke::NONE,
            ));
        }
        // Sort dropdown menu
        sort_resp.context_menu(|ui| {
            for mode in [SortMode::Modified, SortMode::Name, SortMode::Size, SortMode::Duration] {
                if ui.button(mode.label()).clicked() {
                    self.sort_mode = mode;
                    ui.close_menu();
                }
            }
        });
        if sort_resp.clicked() {
            // Cycle through sort modes on click
            self.sort_mode = match self.sort_mode {
                SortMode::Modified => SortMode::Name,
                SortMode::Name => SortMode::Size,
                SortMode::Size => SortMode::Duration,
                SortMode::Duration => SortMode::Modified,
            };
        }

        // ── Section header ──
        let section_y = controls_y + 36.0;
        ui.painter().text(
            egui::pos2(area.min.x + pad, section_y),
            egui::Align2::LEFT_TOP,
            "ALL PROJECTS",
            egui::FontId::proportional(10.0),
            theme.text_dim,
        );

        // Filter and sort
        let query = self.search_query.to_lowercase();
        let mut filtered: Vec<&RecentProject> = recent
            .iter()
            .filter(|rp| query.is_empty() || rp.name.to_lowercase().contains(&query))
            .collect();

        // Apply sort
        match self.sort_mode {
            SortMode::Modified => {
                // Already sorted most-recent-first from prefs
            }
            SortMode::Name => {
                filtered.sort_by_key(|a| a.name.to_lowercase());
            }
            SortMode::Size => {
                filtered.sort_by_key(|a| std::cmp::Reverse(a.total_size_bytes.unwrap_or(0)));
            }
            SortMode::Duration => {
                // Longest first, and an unknown duration sorts last
                // rather than ahead of every real one.
                filtered.sort_by(|a, b| {
                    b.duration_secs
                        .unwrap_or(0.0)
                        .total_cmp(&a.duration_secs.unwrap_or(0.0))
                });
            }
        }

        // ── Project Grid / List ──
        let grid_top = section_y + 22.0;
        let grid_width = area.width() - pad * 2.0;

        let scroll_area = egui::Rect::from_min_max(
            egui::pos2(area.min.x, grid_top),
            area.max,
        );

        if self.view_mode == ViewMode::Grid {
            self.draw_grid(ctx, ui, scroll_area, pad, grid_width, &filtered, theme, cmds);
        } else {
            self.draw_list(ctx, ui, scroll_area, pad, grid_width, &filtered, theme, cmds);
        }
    }

    fn draw_grid(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        scroll_area: egui::Rect,
        pad: f32,
        grid_width: f32,
        filtered: &[&RecentProject],
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let (cols, card_w, card_h) = card_metrics(grid_width);

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(scroll_area), |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                // "New Project" card is first
                let new_card_rect = egui::Rect::from_min_size(
                    ui.min_rect().min + egui::vec2(pad, 0.0),
                    egui::vec2(card_w, card_h),
                );
                self.draw_new_project_card(ui, new_card_rect, theme, cmds);

                let mut card_idx = 1usize; // Start at 1 (slot 0 = New Project)

                // Workspace grouping
                let mut workspace_groups: HashMap<PathBuf, (String, Vec<&RecentProject>)> = HashMap::new();
                let mut standalone: Vec<&RecentProject> = Vec::new();

                for rp in filtered {
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

                // Workspace groups
                let mut ws_keys: Vec<_> = workspace_groups.keys().cloned().collect();
                ws_keys.sort();
                for ws_path in &ws_keys {
                    let (ws_name, projects) = &workspace_groups[ws_path];
                    // Start on fresh row
                    card_idx = card_idx.div_ceil(cols) * cols;
                    let row = card_idx / cols;
                    let y_offset = row as f32 * (card_h + CARD_GAP);

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
                        format!("\u{1F4C1} {} \u{2014} {} project(s)", ws_name, projects.len()),
                        egui::FontId::proportional(12.0),
                        theme.text_secondary,
                    );

                    card_idx = (card_idx / cols + 1) * cols;

                    for rp in projects {
                        let col = card_idx % cols;
                        let row = card_idx / cols;
                        let x = pad + col as f32 * (card_w + CARD_GAP);
                        let y = row as f32 * (card_h + CARD_GAP);

                        let card_rect = egui::Rect::from_min_size(
                            ui.min_rect().min + egui::vec2(x, y),
                            egui::vec2(card_w, card_h),
                        );
                        ui.allocate_at_least(
                            egui::vec2(grid_width, (row + 1) as f32 * (card_h + CARD_GAP)),
                            egui::Sense::hover(),
                        );
                        self.draw_card(ctx, ui, card_rect, rp, theme, cmds);
                        card_idx += 1;
                    }

                    card_idx = card_idx.div_ceil(cols) * cols;
                }

                // Standalone projects
                if !standalone.is_empty() && !workspace_groups.is_empty() {
                    card_idx = card_idx.div_ceil(cols) * cols;
                    let row = card_idx / cols;
                    let y_offset = row as f32 * (card_h + CARD_GAP);
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
                    let x = pad + col as f32 * (card_w + CARD_GAP);
                    let y = row as f32 * (card_h + CARD_GAP);

                    let card_rect = egui::Rect::from_min_size(
                        ui.min_rect().min + egui::vec2(x, y),
                        egui::vec2(card_w, card_h),
                    );
                    ui.allocate_at_least(
                        egui::vec2(grid_width, (row + 1) as f32 * (card_h + CARD_GAP)),
                        egui::Sense::hover(),
                    );
                    self.draw_card(ctx, ui, card_rect, rp, theme, cmds);
                    card_idx += 1;
                }

                // Empty state
                if filtered.is_empty() {
                    ui.add_space(24.0);
                    // Left-aligned under the grid, not centred in
                    // whatever width happens to be left over — centred,
                    // it landed adrift in the middle-right of an empty
                    // page, related to nothing on screen. Here it reads
                    // as a caption to the New Project card above it.
                    ui.horizontal(|ui| {
                        ui.add_space(pad);
                        ui.vertical(|ui| {
                        // Two different emptinesses, and they were
                        // being told apart by nobody: a new user with
                        // no projects at all was told their *search*
                        // had matched nothing, on the first screen they
                        // ever saw, without having typed anything.
                        if self.search_query.trim().is_empty() {
                            ui.label(
                                egui::RichText::new("No projects yet")
                                    .color(theme.text_primary)
                                    .size(15.0),
                            );
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(
                                    "Start one with New Project, or open an existing .klipp file.",
                                )
                                .color(theme.text_dim)
                                .size(12.0),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new(format!(
                                    "No projects match \u{201C}{}\u{201D}",
                                    self.search_query.trim()
                                ))
                                .color(theme.text_dim)
                                .size(13.0),
                            );
                        }
                        });
                    });
                }
            });
        });
    }

    fn draw_list(
        &mut self,
        _ctx: &egui::Context,
        ui: &mut egui::Ui,
        scroll_area: egui::Rect,
        pad: f32,
        grid_width: f32,
        filtered: &[&RecentProject],
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(scroll_area), |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let row_h = 52.0;

                for rp in filtered {
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(grid_width, row_h),
                        egui::Sense::click(),
                    );
                    let hovered = resp.hovered();

                    if hovered {
                        ui.painter().rect_filled(rect, 4.0, theme.bg_hover);
                    }

                    // A colour chip keyed to the name, for recognition
                    // down a long list. It claims nothing about the
                    // project's contents — unlike the timeline strip
                    // that used to be drawn over it, which was the same
                    // hash pretending to be an edit.
                    let color = project_color(&rp.name);
                    let bar_rect = egui::Rect::from_min_size(
                        egui::pos2(rect.min.x + pad, rect.min.y + 10.0),
                        egui::vec2(36.0, 32.0),
                    );
                    ui.painter().rect_filled(bar_rect, 4.0, color);

                    // Reserve space for the right-aligned timestamp
                    // FIRST so the name + meta can be truncated to
                    // fit whatever is left. Without this, long names
                    // or meta strings would either overflow into the
                    // timestamp area or paint past the card edge.
                    let text_x = rect.min.x + pad + 48.0;
                    let icon_size_right = 9.0;
                    let time_str: String = rp.last_saved.as_deref()
                        .map(relative_time)
                        .unwrap_or_default();
                    let time_reserved = if time_str.is_empty() {
                        0.0
                    } else {
                        let g = ui.painter().layout_no_wrap(
                            time_str.clone(),
                            egui::FontId::proportional(11.0),
                            theme.text_dim,
                        );
                        g.size().x + icon_size_right + 3.0
                    };
                    // Missing-media badge sits right of the name;
                    // reserve ~24 px so truncation doesn't collide
                    // with it.
                    let badge_reserved = if rp.missing_media == Some(true) { 26.0 } else { 0.0 };
                    let text_right_limit = rect.max.x - pad - time_reserved - 12.0;
                    let text_max_w = (text_right_limit - text_x - badge_reserved).max(0.0);

                    // Project name (truncated to available width)
                    let name_galley = layout_truncated(
                        ui.painter(),
                        &rp.name,
                        egui::FontId::proportional(13.0),
                        theme.text_primary,
                        text_max_w,
                    );
                    ui.painter().galley(
                        egui::pos2(text_x, rect.min.y + 12.0),
                        name_galley.clone(),
                        theme.text_primary,
                    );

                    // Metadata line (fps · res · dur · size + counts)
                    let meta = build_meta_string(rp);
                    let counts = build_counts_string(rp);
                    let combined = if meta.is_empty() {
                        counts
                    } else if counts.is_empty() {
                        meta
                    } else {
                        format!("{meta}  \u{00B7}  {counts}")
                    };
                    if !combined.is_empty() {
                        let g = layout_truncated(
                            ui.painter(),
                            &combined,
                            egui::FontId::monospace(10.0),
                            theme.text_dim,
                            text_max_w,
                        );
                        ui.painter().galley(
                            egui::pos2(text_x, rect.min.y + 30.0),
                            g,
                            theme.text_dim,
                        );
                    }

                    // Missing media warning (right of the rendered
                    // name — use the truncated galley's actual width
                    // so the badge doesn't overshoot).
                    if rp.missing_media == Some(true) {
                        let warn_x = text_x + name_galley.size().x + 8.0;
                        draw_missing_media_badge(ui, egui::pos2(warn_x + 18.0, rect.min.y + 6.0), theme.marker_warning);
                    }

                    // Timestamp (right-aligned) with clock icon
                    if !time_str.is_empty() {
                        let time_galley = ui.painter().layout_no_wrap(
                            time_str.clone(),
                            egui::FontId::proportional(11.0),
                            theme.text_dim,
                        );
                        let text_w = time_galley.size().x;
                        let text_right = rect.max.x - pad;
                        let text_left = text_right - text_w;
                        let icon_x = text_left - icon_size_right - 3.0;
                        let icon_cy = rect.center().y;
                        draw_clock_icon(
                            ui.painter(),
                            egui::pos2(icon_x + icon_size_right / 2.0, icon_cy),
                            icon_size_right,
                            theme.text_dim,
                        );
                        ui.painter().text(
                            egui::pos2(text_right, icon_cy),
                            egui::Align2::RIGHT_CENTER,
                            &time_str,
                            egui::FontId::proportional(11.0),
                            theme.text_dim,
                        );
                    }

                    if resp.clicked() {
                        cmds.push(AppCommand::OpenProject { path: rp.path.clone() });
                    }

                    // Context menu
                    resp.context_menu(|ui| {
                        self.project_context_menu(ui, rp, cmds);
                    });

                    // Separator
                    let sep_y = rect.max.y;
                    ui.painter().hline(
                        rect.min.x + pad..=rect.max.x - pad,
                        sep_y,
                        egui::Stroke::new(0.5_f32, theme.border),
                    );
                }

                if filtered.is_empty() {
                    ui.add_space(24.0);
                    // Left-aligned under the grid, not centred in
                    // whatever width happens to be left over — centred,
                    // it landed adrift in the middle-right of an empty
                    // page, related to nothing on screen. Here it reads
                    // as a caption to the New Project card above it.
                    ui.horizontal(|ui| {
                        ui.add_space(pad);
                        ui.vertical(|ui| {
                        // Two different emptinesses, and they were
                        // being told apart by nobody: a new user with
                        // no projects at all was told their *search*
                        // had matched nothing, on the first screen they
                        // ever saw, without having typed anything.
                        if self.search_query.trim().is_empty() {
                            ui.label(
                                egui::RichText::new("No projects yet")
                                    .color(theme.text_primary)
                                    .size(15.0),
                            );
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(
                                    "Start one with New Project, or open an existing .klipp file.",
                                )
                                .color(theme.text_dim)
                                .size(12.0),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new(format!(
                                    "No projects match \u{201C}{}\u{201D}",
                                    self.search_query.trim()
                                ))
                                .color(theme.text_dim)
                                .size(13.0),
                            );
                        }
                        });
                    });
                }
            });
        });
    }

    fn draw_new_project_card(
        &self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let resp = ui.allocate_rect(rect, egui::Sense::click());
        let hovered = resp.hovered();

        // Dashed border card
        let stroke_color = if hovered { theme.accent } else { theme.border_light };
        let dash_len = 8.0;
        let gap_len = 5.0;
        let rounding = 8.0;

        // Draw dashed rectangle
        draw_dashed_rect(ui.painter(), rect, rounding, stroke_color, 1.5, dash_len, gap_len);

        if hovered {
            ui.painter().rect_filled(
                rect.shrink(1.0),
                rounding,
                egui::Color32::from_rgba_unmultiplied(220, 220, 228, 8),
            );
        }

        // + icon (circle with plus)
        let center = egui::pos2(rect.center().x, rect.center().y - 16.0);
        let icon_r = 22.0;
        let icon_color = if hovered { theme.accent } else { theme.text_dim };
        ui.painter().circle_stroke(center, icon_r, egui::Stroke::new(1.5_f32, icon_color));
        // Horizontal bar
        ui.painter().hline(
            center.x - 8.0..=center.x + 8.0,
            center.y,
            egui::Stroke::new(2.0_f32, icon_color),
        );
        // Vertical bar
        ui.painter().vline(
            center.x,
            center.y - 8.0..=center.y + 8.0,
            egui::Stroke::new(2.0_f32, icon_color),
        );

        // "New Project" text
        ui.painter().text(
            egui::pos2(rect.center().x, rect.center().y + 24.0),
            egui::Align2::CENTER_CENTER,
            "New Project",
            egui::FontId::proportional(13.0),
            if hovered { theme.accent } else { theme.text_dim },
        );

        if resp.clicked() {
            cmds.push(AppCommand::NewProject);
        }
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
            egui::Stroke::new(1.5_f32, theme.accent)
        } else {
            egui::Stroke::new(1.0_f32, theme.border)
        };
        ui.painter().rect(rect, 8.0, bg, stroke);

        // Thumbnail area (top ~60%)
        let thumb_h = rect.height() * 0.58;
        let thumb_rect = egui::Rect::from_min_size(
            rect.min + egui::vec2(1.0, 1.0),
            egui::vec2(rect.width() - 2.0, thumb_h),
        );

        // Try to load/show actual thumbnail
        let has_thumb = if let Some(ref thumb_path) = rp.thumbnail_path {
            if let Some(tex) = self.thumbnail_textures.get(thumb_path) {
                // Fit image within thumb_rect preserving aspect ratio
                let img_aspect = tex.size()[0] as f32 / tex.size()[1] as f32;
                let rect_aspect = thumb_rect.width() / thumb_rect.height();
                let fitted = if img_aspect > rect_aspect {
                    // Image is wider — fit to width, letterbox top/bottom
                    let h = thumb_rect.width() / img_aspect;
                    let y_off = (thumb_rect.height() - h) / 2.0;
                    egui::Rect::from_min_size(
                        egui::pos2(thumb_rect.min.x, thumb_rect.min.y + y_off),
                        egui::vec2(thumb_rect.width(), h),
                    )
                } else {
                    // Image is taller — fit to height, pillarbox left/right
                    let w = thumb_rect.height() * img_aspect;
                    let x_off = (thumb_rect.width() - w) / 2.0;
                    egui::Rect::from_min_size(
                        egui::pos2(thumb_rect.min.x + x_off, thumb_rect.min.y),
                        egui::vec2(w, thumb_rect.height()),
                    )
                };
                // Dark background behind pillarbox/letterbox bars
                ui.painter().rect_filled(
                    thumb_rect,
                    egui::Rounding { nw: 7.0, ne: 7.0, sw: 0.0, se: 0.0 },
                    theme.bg_void,
                );
                ui.painter().image(
                    tex.id(),
                    fitted,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                true
            } else if thumb_path.exists() {
                if let Ok(data) = std::fs::read(thumb_path) {
                    if let Ok(img) = image_from_png(&data) {
                        let name = thumb_path.to_string_lossy();
                        let tex = crate::app::alloc_texture_logged(
                            ctx,
                            &name,
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
            // A poster frame is taken from the first media file in the
            // project's manifest, so no thumbnail means no media: the
            // project is empty. Say that, rather than leaving a hole.
            //
            // It used to fill with `project_color` at full strength —
            // a saturated block brighter than any real frame on the
            // page, so the eye went first to the projects with the
            // least in them.
            ui.painter().rect_filled(
                thumb_rect,
                egui::Rounding { nw: 7.0, ne: 7.0, sw: 0.0, se: 0.0 },
                theme.bg_raised,
            );

            // "Empty project", not the Klipp logo and "Klipp project".
            //
            // Every card in this list is a Klipp project — saying so on
            // each one is a caption that repeats the context it sits
            // in, and with eight empty projects it was eight copies of
            // the logo competing with the real thumbnails beside them.
            //
            // What distinguishes these cards is that they are empty,
            // which is what the words now say. A bare panel would not
            // do: with nothing in it at all it reads as a thumbnail
            // that failed to load rather than a project with nothing
            // in it yet.
            ui.painter().text(
                thumb_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Empty project",
                egui::FontId::proportional(12.0),
                theme.text_dim,
            );
        }

        // No timeline strip.
        //
        // There was one, and it was a hash of the project's *name*:
        // the number of clips, their widths and their colours all came
        // out of `name.bytes()`, with a white playhead drawn at a fixed
        // 35% of the width. It described nothing. Two projects with the
        // same name drew the same edit; renaming a project changed the
        // edit it appeared to contain.
        //
        // On a card whose whole job is "is this the project I want",
        // that is not decoration — it is invented data in the place a
        // user looks for real data. The counts below are measured, and
        // they say what the strip pretended to.

        // Missing media warning badge (top-right of thumbnail)
        if rp.missing_media == Some(true) {
            draw_missing_media_badge(ui, egui::pos2(thumb_rect.max.x, thumb_rect.min.y), theme.marker_warning);
        }

        // Unsaved work: an auto-save newer than the project. Without this
        // the card shows the last save ("Empty project" when there never
        // was one) and nothing says the work is there to restore.
        if self.newer_autosave.get(&rp.path).copied().unwrap_or(false) {
            let g = layout_truncated(
                ui.painter(),
                "Unsaved work recovered \u{2014} open to restore",
                egui::FontId::proportional(10.0),
                theme.bg_void,
                thumb_rect.width() - 20.0,
            );
            let pill = egui::Rect::from_min_size(
                egui::pos2(thumb_rect.min.x + 6.0, thumb_rect.max.y - g.size().y - 12.0),
                g.size() + egui::vec2(8.0, 6.0),
            );
            ui.painter().rect_filled(pill, 4.0, theme.accent);
            ui.painter().galley(pill.min + egui::vec2(4.0, 3.0), g, theme.bg_void);
        }

        // ── Info area (bottom ~40%) ──
        let info_top = rect.min.y + thumb_h + 6.0;
        let left_x = rect.min.x + 10.0;
        let max_w = rect.width() - 20.0;

        // Project name. Truncate with an ellipsis if the title
        // wouldn't fit the card width — hard-clipping via clip_rect
        // chopped letters mid-glyph on long names.
        let name_galley = layout_truncated(
            ui.painter(),
            &rp.name,
            egui::FontId::proportional(13.0),
            theme.text_primary,
            max_w,
        );
        ui.painter().galley(
            egui::pos2(left_x, info_top),
            name_galley,
            theme.text_primary,
        );

        // Metadata line (fps, resolution, duration, size). Truncate
        // when the joined string would overflow — the "7.1 GB" tail
        // on the 4K/long-clip card was rendering past the card edge.
        let meta = build_meta_string(rp);
        let mut y_cursor = info_top + 18.0;
        if !meta.is_empty() {
            let g = layout_truncated(
                ui.painter(),
                &meta,
                egui::FontId::monospace(10.0),
                theme.text_dim,
                max_w,
            );
            ui.painter().galley(egui::pos2(left_x, y_cursor), g, theme.text_dim);
            y_cursor += 13.0;
        }

        // Counts line (clips, tracks)
        let counts = build_counts_string(rp);
        if !counts.is_empty() {
            let g = layout_truncated(
                ui.painter(),
                &counts,
                egui::FontId::proportional(10.0),
                theme.text_dim,
                max_w,
            );
            ui.painter().galley(egui::pos2(left_x, y_cursor), g, theme.text_dim);
            y_cursor += 13.0;
        }

        // Modified timestamp with clock icon
        if let Some(ref saved) = rp.last_saved {
            let time_str = relative_time(saved);
            if !time_str.is_empty() {
                let icon_size = 8.0;
                let icon_center = egui::pos2(left_x + icon_size / 2.0, y_cursor + 5.5);
                draw_clock_icon(ui.painter(), icon_center, icon_size, theme.text_secondary);
                // Remaining width after the icon + its gap.
                let text_max_w = (max_w - icon_size - 4.0).max(0.0);
                let g = layout_truncated(
                    ui.painter(),
                    &time_str,
                    egui::FontId::proportional(10.0),
                    theme.text_secondary,
                    text_max_w,
                );
                ui.painter().galley(
                    egui::pos2(left_x + icon_size + 4.0, y_cursor),
                    g,
                    theme.text_secondary,
                );
            }
        }

        // Click to open
        if resp.clicked() {
            cmds.push(AppCommand::OpenProject { path: rp.path.clone() });
        }

        // Right-click context menu
        resp.context_menu(|ui| {
            self.project_context_menu(ui, rp, cmds);
        });
    }

    fn project_context_menu(
        &mut self,
        ui: &mut egui::Ui,
        rp: &RecentProject,
        cmds: &mut Vec<AppCommand>,
    ) {
        if ui.button("Open").clicked() {
            cmds.push(AppCommand::OpenProject { path: rp.path.clone() });
            ui.close_menu();
        }
        if ui.button("Rename\u{2026}").clicked() {
            self.renaming_project = Some((rp.path.clone(), rp.name.clone()));
            ui.close_menu();
        }
        if ui.button("Remove from Recent").clicked() {
            cmds.push(AppCommand::RemoveRecentProject { path: rp.path.clone() });
            ui.close_menu();
        }
        ui.separator();
        if ui
            .button(egui::RichText::new("Delete Project").color(ui.visuals().error_fg_color))
            .clicked()
        {
            cmds.push(AppCommand::DeleteProject { path: rp.path.clone() });
            ui.close_menu();
        }
    }
}

// ── Helpers ──

/// Lay out a single-line galley that truncates with an ellipsis when
/// the text wouldn't fit `max_w`. Project-card fields (name,
/// fps·res·dur·size, clip/track counts, timestamp) all use this so
/// nothing ever paints past the card's right edge.
fn layout_truncated(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    max_w: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::text::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: max_w.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('\u{2026}'),
    };
    painter.layout_job(job)
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

fn build_meta_string(rp: &RecentProject) -> String {
    let mut parts = Vec::new();
    if let Some(ref fps) = rp.fps {
        parts.push(fps.clone());
    }
    if let Some(ref res) = rp.resolution {
        parts.push(res.clone());
    }
    if let Some(dur) = rp.duration_secs {
        let mins = (dur / 60.0).floor() as u32;
        let secs = (dur % 60.0).floor() as u32;
        parts.push(format!("{mins}:{secs:02}"));
    }
    if let Some(size) = rp.total_size_bytes {
        parts.push(format_size_compact(size));
    }
    parts.join(" \u{00B7} ")
}

/// Build a secondary metadata line with clip/track counts.
fn build_counts_string(rp: &RecentProject) -> String {
    let mut parts = Vec::new();
    if let Some(clips) = rp.clip_count {
        parts.push(format!("{clips} clip{}", if clips == 1 { "" } else { "s" }));
    }
    if let Some(tracks) = rp.track_count {
        parts.push(format!("{tracks} track{}", if tracks == 1 { "" } else { "s" }));
    }
    parts.join(", ")
}

/// Draw a small warning triangle icon for missing media.
fn draw_missing_media_badge(ui: &egui::Ui, top_right: egui::Pos2, fill: egui::Color32) {
    let size = 18.0;
    let margin = 6.0;
    let center = egui::pos2(top_right.x - margin - size / 2.0, top_right.y + margin + size / 2.0);

    // Triangle background
    let tri_top = egui::pos2(center.x, center.y - size / 2.0);
    let tri_bl = egui::pos2(center.x - size / 2.0, center.y + size / 2.0);
    let tri_br = egui::pos2(center.x + size / 2.0, center.y + size / 2.0);

    let painter = ui.painter();
    painter.add(egui::Shape::convex_polygon(
        vec![tri_top, tri_bl, tri_br],
        fill,
        egui::Stroke::NONE,
    ));

    // Exclamation mark
    painter.text(
        egui::pos2(center.x, center.y + 1.5),
        egui::Align2::CENTER_CENTER,
        "!",
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(30, 25, 10),
    );
}

/// Draw a small clock icon using painter calls.
fn draw_clock_icon(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let r = size / 2.0;
    // Circle outline
    painter.circle_stroke(center, r, egui::Stroke::new(1.0_f32, color));
    // Hour hand (pointing up-right, ~2 o'clock)
    painter.line_segment(
        [center, egui::pos2(center.x + r * 0.4, center.y - r * 0.35)],
        egui::Stroke::new(1.0_f32, color),
    );
    // Minute hand (pointing up)
    painter.line_segment(
        [center, egui::pos2(center.x, center.y - r * 0.55)],
        egui::Stroke::new(1.0_f32, color),
    );
}

/// Generate a deterministic color from a project name.
fn project_color(name: &str) -> egui::Color32 {
    let mut hash: u32 = 5381;
    for b in name.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u32);
    }
    // Generate a saturated hue
    let hue = (hash % 360) as f32;
    let (r, g, b) = hsv_to_rgb(hue, 0.55, 0.65);
    egui::Color32::from_rgb(r, g, b)
}

fn darken_color(c: egui::Color32, factor: f32) -> egui::Color32 {
    egui::Color32::from_rgb(
        (c.r() as f32 * factor) as u8,
        (c.g() as f32 * factor) as u8,
        (c.b() as f32 * factor) as u8,
    )
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

/// Draw a dashed rounded rectangle.
fn draw_dashed_rect(
    painter: &egui::Painter,
    rect: egui::Rect,
    rounding: f32,
    color: egui::Color32,
    width: f32,
    dash: f32,
    gap: f32,
) {
    // Approximate with line segments along each side (simplified — straight edges)
    let sides = [
        // Top
        (egui::pos2(rect.min.x + rounding, rect.min.y), egui::pos2(rect.max.x - rounding, rect.min.y)),
        // Right
        (egui::pos2(rect.max.x, rect.min.y + rounding), egui::pos2(rect.max.x, rect.max.y - rounding)),
        // Bottom
        (egui::pos2(rect.max.x - rounding, rect.max.y), egui::pos2(rect.min.x + rounding, rect.max.y)),
        // Left
        (egui::pos2(rect.min.x, rect.max.y - rounding), egui::pos2(rect.min.x, rect.min.y + rounding)),
    ];

    let stroke = egui::Stroke::new(width, color);

    for (start, end) in sides {
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1.0 { continue; }
        let ux = dx / len;
        let uy = dy / len;

        let mut d = 0.0;
        let mut drawing = true;
        while d < len {
            let seg = if drawing { dash } else { gap };
            let seg_end = (d + seg).min(len);
            if drawing {
                let p0 = egui::pos2(start.x + ux * d, start.y + uy * d);
                let p1 = egui::pos2(start.x + ux * seg_end, start.y + uy * seg_end);
                painter.line_segment([p0, p1], stroke);
            }
            d = seg_end;
            drawing = !drawing;
        }
    }

    // Corner arcs (approximate with a few line segments each)
    let corners = [
        (egui::pos2(rect.max.x - rounding, rect.min.y + rounding), 270.0), // top-right
        (egui::pos2(rect.max.x - rounding, rect.max.y - rounding), 0.0),   // bottom-right
        (egui::pos2(rect.min.x + rounding, rect.max.y - rounding), 90.0),  // bottom-left
        (egui::pos2(rect.min.x + rounding, rect.min.y + rounding), 180.0), // top-left
    ];
    let segments = 6;
    for (center, start_angle) in corners {
        for i in 0..segments {
            let a0 = (start_angle + 90.0 * i as f32 / segments as f32).to_radians();
            let a1 = (start_angle + 90.0 * (i + 1) as f32 / segments as f32).to_radians();
            let p0 = egui::pos2(center.x + rounding * a0.cos(), center.y + rounding * a0.sin());
            let p1 = egui::pos2(center.x + rounding * a1.cos(), center.y + rounding * a1.sin());
            // Dash pattern for corners
            if i % 2 == 0 {
                painter.line_segment([p0, p1], stroke);
            }
        }
    }
}

/// Parse ISO 8601 datetime string and return a human-friendly relative time.
fn relative_time(iso: &str) -> String {
    use chrono::NaiveDateTime;

    // Strip trailing 'Z' (legacy UTC marker)
    let cleaned = iso.trim_end_matches('Z');

    let parsed = NaiveDateTime::parse_from_str(cleaned, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(cleaned, "%Y-%m-%dT%H:%M:%S%.f"))
        .or_else(|_| NaiveDateTime::parse_from_str(cleaned, "%Y-%m-%dT%H:%M:%S"))
        .or_else(|_| NaiveDateTime::parse_from_str(&format!("{cleaned} 00:00:00"), "%Y-%m-%d %H:%M:%S"));

    let dt = match parsed {
        Ok(dt) => dt,
        Err(_) => return String::new(),
    };

    // Compare against local time (timestamps are stored in local time)
    let now = chrono::Local::now().naive_local();
    let diff = now.signed_duration_since(dt);
    let time_str = dt.format("%H:%M").to_string();

    let secs = diff.num_seconds();
    if secs < 0 {
        return "just now".into();
    }
    let mins = secs / 60;
    let hours = mins / 60;
    let days = hours / 24;
    let weeks = days / 7;

    if mins < 1 {
        "just now".into()
    } else if mins < 60 {
        format!("{mins} min{} ago", if mins == 1 { "" } else { "s" })
    } else if hours < 24 {
        format!("Today at {time_str}")
    } else if days == 1 {
        format!("Yesterday at {time_str}")
    } else if days < 7 {
        format!("{days} days ago")
    } else if weeks == 1 {
        "Last week".into()
    } else if weeks < 5 {
        format!("{weeks} weeks ago")
    } else {
        dt.format("%b %d, %Y").to_string()
    }
}

/// Decode a PNG file into an egui ColorImage.
fn image_from_png(data: &[u8]) -> Result<egui::ColorImage, String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    // png 0.18: output_buffer_size returns None when the dimensions
    // would overflow usize. Treat that as a decode failure.
    let buf_size = reader.output_buffer_size()
        .ok_or_else(|| "PNG dimensions overflow buffer size".to_string())?;
    let mut buf = vec![0u8; buf_size];
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
        _ => return Err("Unsupported PNG colour type".into()),
    };

    Ok(egui::ColorImage {
        size: [w, h],
        pixels,
    })
}
