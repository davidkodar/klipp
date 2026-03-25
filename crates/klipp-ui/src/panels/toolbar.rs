use crate::layout_state::EditTool;
use crate::prefs::RecentProject;
use crate::state::{AppState, PlayheadFollow, Workspace};
use crate::theme::KlippTheme;
use crate::widgets::tooltip::styled_tooltip;
use klipp_core::{AppCommand, Tool};

pub struct ToolbarPanel;

impl ToolbarPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        recent_projects: &[RecentProject],
        theme: &KlippTheme,
        layout_tool: &mut EditTool,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let toolbar_rect = ui.max_rect();

        // Paint toolbar background
        ui.painter().rect_filled(toolbar_rect, 0.0, theme.bg_deep);
        ui.painter().hline(
            toolbar_rect.x_range(),
            toolbar_rect.max.y,
            egui::Stroke::new(1.0, theme.border),
        );

        let cy = toolbar_rect.center().y;
        let painter = ui.painter().clone();

        // ═══════════════════════════════════════════
        // RIGHT GROUP — render first to know its left edge
        // ═══════════════════════════════════════════
        let mut rx = toolbar_rect.max.x - 10.0;

        // Export (icon 6)
        rx -= 32.0;
        let resp = alloc_btn(ui, egui::pos2(rx, cy - 16.0), 32.0);
        let ic = icon_color(&resp, false);
        draw_export_icon(&painter, resp.rect, ic);
        styled_tooltip(&resp, theme, "Export / Share", Some("Ctrl+E"));
        if resp.clicked() { cmds.push(AppCommand::ShowExportDialog); }

        rx -= 2.0;

        // Render Queue (icon 5)
        rx -= 32.0;
        let resp = alloc_btn(ui, egui::pos2(rx, cy - 16.0), 32.0);
        let ic = icon_color(&resp, false);
        draw_render_queue_icon(&painter, resp.rect, ic);
        styled_tooltip(&resp, theme, "Render Queue", None);
        if resp.clicked() { cmds.push(AppCommand::ShowRenderQueue); }

        rx -= 4.0;
        // Separator
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(rx, cy - 11.0), egui::vec2(1.0, 22.0)),
            0.0, theme.border_light,
        );
        rx -= 4.0;

        // Skimming (icon 4)
        rx -= 32.0;
        let skim_active = state.skimming_enabled;
        let resp = alloc_btn(ui, egui::pos2(rx, cy - 16.0), 32.0);
        let ic = icon_color(&resp, skim_active);
        let bg = icon_bg(&resp, skim_active);
        painter.rect_filled(resp.rect, 6.0, bg);
        draw_skimming_icon(&painter, resp.rect, ic);
        styled_tooltip(&resp, theme, "Skimming On/Off", Some("S"));
        if resp.clicked() {
            state.skimming_enabled = !state.skimming_enabled;
            let msg = if state.skimming_enabled { "Skimming On" } else { "Skimming Off" };
            state.toast.show(msg);
        }

        rx -= 2.0;

        // Color Grading (icon 3) — switches to Color workspace
        rx -= 32.0;
        let color_active = state.active_workspace == Workspace::Color;
        let resp = alloc_btn(ui, egui::pos2(rx, cy - 16.0), 32.0);
        let ic = icon_color(&resp, color_active);
        let bg = icon_bg(&resp, color_active);
        painter.rect_filled(resp.rect, 6.0, bg);
        draw_color_icon(&painter, resp.rect, ic);
        styled_tooltip(&resp, theme, "Color Workspace", Some("C"));
        if resp.clicked() {
            if color_active {
                state.active_workspace = Workspace::Edit;
                state.show_color_panel = false;
            } else {
                state.active_workspace = Workspace::Color;
                state.show_color_panel = true;
                state.show_scopes = true;
            }
        }

        rx -= 2.0;

        // Scopes (icon 2)
        rx -= 32.0;
        let scopes_active = state.show_scopes;
        let resp = alloc_btn(ui, egui::pos2(rx, cy - 16.0), 32.0);
        let ic = icon_color(&resp, scopes_active);
        let bg = icon_bg(&resp, scopes_active);
        painter.rect_filled(resp.rect, 6.0, bg);
        draw_scopes_icon(&painter, resp.rect, ic);
        styled_tooltip(&resp, theme, "Toggle Scopes", Some("V"));
        if resp.clicked() {
            state.show_scopes = !state.show_scopes;
        }

        rx -= 2.0;

        // Roles (icon 1)
        rx -= 32.0;
        let resp = alloc_btn(ui, egui::pos2(rx, cy - 16.0), 32.0);
        let ic = icon_color(&resp, false);
        draw_roles_icon(&painter, resp.rect, ic);
        styled_tooltip(&resp, theme, "Roles & Lanes", Some("R"));

        let right_edge = rx - 10.0;

        // ═══════════════════════════════════════════
        // WORKSPACE TABS — centred in toolbar
        // ═══════════════════════════════════════════
        let ws_tab_w = 150.0;
        let ws_rect = egui::Rect::from_min_size(
            egui::pos2(toolbar_rect.center().x - ws_tab_w / 2.0, toolbar_rect.min.y),
            egui::vec2(ws_tab_w, toolbar_rect.height()),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(ws_rect), |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 1.0;
                for (ws, label) in [
                    (Workspace::Edit, "Edit"),
                    (Workspace::Color, "Color"),
                    (Workspace::Export, "Export"),
                ] {
                    let active = state.active_workspace == ws;
                    let text = egui::RichText::new(label).size(10.0);
                    let text = if active {
                        text.strong().color(theme.text_accent)
                    } else {
                        text.color(theme.text_dim)
                    };
                    let resp = ui.selectable_label(active, text);
                    if resp.clicked() {
                        state.active_workspace = ws;
                        state.show_color_panel = ws == Workspace::Color;
                        if ws == Workspace::Color {
                            state.show_scopes = true;
                        }
                    }
                }
            });
        });

        // ═══════════════════════════════════════════
        // LEFT GROUP — brand + menus + tool buttons
        // ═══════════════════════════════════════════
        let left_rect = egui::Rect::from_min_max(
            toolbar_rect.min,
            egui::pos2((ws_rect.left() - 10.0).min(right_edge), toolbar_rect.max.y),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(left_rect), |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.add_space(10.0);

                // Brand: shield icon + "Klipp"
                let brand_center = egui::pos2(
                    ui.cursor().min.x + 8.0,
                    cy,
                );
                draw_shield_icon(ui.painter(), brand_center, 8.0, theme.accent);
                ui.add_space(20.0);
                ui.label(egui::RichText::new("Klipp").size(15.0).strong().color(theme.text_accent));
                ui.add_space(12.0);

                // Separator
                sep(ui, theme);

                // Compact menus
                Self::file_menu(ui, &mut cmds, state, recent_projects);
                Self::edit_menu(ui, &mut cmds, state);
                Self::mark_menu(ui, &mut cmds, state);
                Self::view_menu(ui, &mut cmds, state);
                Self::settings_menu(ui, &mut cmds, state);
                Self::help_menu(ui, state);

                sep(ui, theme);

                // Edit tool buttons (only shown when there's enough space)
                let tools = [
                    (EditTool::Select, "Select Tool", "V"),
                    (EditTool::Trim, "Trim Tool", "T"),
                    (EditTool::Blade, "Blade Tool", "B"),
                    (EditTool::Range, "Range Tool", "R"),
                    (EditTool::Zoom, "Zoom Tool", "Z"),
                ];
                for (tool, tip, key) in &tools {
                    let active = *layout_tool == *tool;
                    let (bg, fg) = if active {
                        (theme.accent_subtle, theme.accent)
                    } else {
                        (egui::Color32::TRANSPARENT, theme.text_secondary)
                    };
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(32.0, 32.0),
                        egui::Sense::click(),
                    );
                    let bg = if resp.hovered() && !active { theme.bg_hover } else { bg };
                    let fg = if resp.hovered() && !active { theme.text_primary } else { fg };
                    ui.painter().rect_filled(rect, 6.0, bg);
                    match tool {
                        EditTool::Select => draw_select_icon(ui.painter(), rect, fg),
                        EditTool::Trim => draw_trim_icon(ui.painter(), rect, fg),
                        EditTool::Blade => draw_blade_icon(ui.painter(), rect, fg),
                        EditTool::Range => draw_range_icon(ui.painter(), rect, fg),
                        EditTool::Zoom => draw_zoom_icon(ui.painter(), rect, fg),
                    }
                    styled_tooltip(&resp, theme, tip, Some(key));
                    if resp.clicked() {
                        *layout_tool = *tool;
                        let engine_tool = match tool {
                            EditTool::Select => Tool::Select,
                            EditTool::Blade => Tool::Blade,
                            EditTool::Range => Tool::Range,
                            EditTool::Trim | EditTool::Zoom => Tool::Select,
                        };
                        cmds.push(AppCommand::SetActiveTool(engine_tool));
                    }
                }

                sep(ui, theme);

                // Insert/Overwrite mode toggle
                {
                    let mode_text = match state.edit_mode {
                        klipp_timeline::magnetic::EditMode::Insert => "INS",
                        klipp_timeline::magnetic::EditMode::Overwrite => "OVR",
                    };
                    let mode_color = match state.edit_mode {
                        klipp_timeline::magnetic::EditMode::Insert => egui::Color32::from_rgb(232, 160, 32),
                        klipp_timeline::magnetic::EditMode::Overwrite => egui::Color32::from_rgb(60, 120, 220),
                    };
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(36.0, 20.0),
                        egui::Sense::click(),
                    );
                    let bg = mode_color.linear_multiply(0.25);
                    let border = mode_color.linear_multiply(0.6);
                    ui.painter().rect_filled(rect, 3.0, bg);
                    ui.painter().rect_stroke(rect, 3.0, egui::Stroke::new(1.0, border));
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        mode_text,
                        egui::FontId::monospace(10.0),
                        mode_color,
                    );
                    let tip = if matches!(state.edit_mode, klipp_timeline::magnetic::EditMode::Insert) {
                        "Edit Mode: Insert (W to toggle)"
                    } else {
                        "Edit Mode: Overwrite (W to toggle)"
                    };
                    styled_tooltip(&resp, theme, tip, Some("W"));
                    if resp.clicked() {
                        state.edit_mode = match state.edit_mode {
                            klipp_timeline::magnetic::EditMode::Insert => klipp_timeline::magnetic::EditMode::Overwrite,
                            klipp_timeline::magnetic::EditMode::Overwrite => klipp_timeline::magnetic::EditMode::Insert,
                        };
                    }
                }

                sep(ui, theme);
            });
        });

        cmds
    }

    fn file_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState, recent: &[RecentProject]) {
        ui.menu_button(egui::RichText::new("File").size(11.0), |ui| {
            if ui.button("New Project").clicked() { cmds.push(AppCommand::NewProject); ui.close_menu(); }
            if ui.button("Open Project...").clicked() { ui.close_menu(); if let Some(p) = rfd::FileDialog::new().set_title("Open Project").pick_folder() { cmds.push(AppCommand::OpenProject { path: p }); } }
            if !recent.is_empty() {
                ui.menu_button("Recent Projects", |ui| {
                    for rp in recent {
                        let suffix = rp.last_saved.as_deref().unwrap_or("");
                        let label = if suffix.is_empty() {
                            rp.name.clone()
                        } else {
                            format!("{}  {}", rp.name, suffix)
                        };
                        if ui.button(&label).clicked() {
                            cmds.push(AppCommand::OpenProject { path: rp.path.clone() });
                            ui.close_menu();
                        }
                    }
                });
            }
            if ui.button("Import Media...").clicked() {
                ui.close_menu();
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("All media", &[
                        "mp4", "mkv", "mov", "avi", "webm", "m4v", "mts",
                        "wav", "mp3", "aac", "flac", "ogg", "m4a", "aiff",
                        "png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp",
                    ])
                    .add_filter("Video", &["mp4", "mkv", "mov", "avi", "webm", "m4v", "mts"])
                    .add_filter("Audio", &["wav", "mp3", "aac", "flac", "ogg", "m4a", "aiff"])
                    .add_filter("Image", &["png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp"])
                    .pick_file()
                {
                    cmds.push(AppCommand::LoadMediaFile { path: p, media_id: klipp_core::MediaId::new() });
                }
            }
            if ui.button("Import Image Sequence...").clicked() {
                ui.close_menu();
                if let Some(files) = rfd::FileDialog::new()
                    .set_title("Select image sequence files")
                    .add_filter("Image", &["png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp", "exr"])
                    .pick_files()
                {
                    if let Some((pattern, start, count)) = crate::app::detect_image_sequence(&files) {
                        cmds.push(AppCommand::ImportImageSequence {
                            pattern,
                            start_frame: start,
                            frame_count: count,
                            fps: 25.0, // default — could prompt user
                        });
                    } else if files.len() == 1 {
                        // Single image — import as still
                        cmds.push(AppCommand::LoadMediaFile { path: files[0].clone(), media_id: klipp_core::MediaId::new() });
                    } else {
                        // Multiple files but not a sequence — import each individually
                        for f in files {
                            cmds.push(AppCommand::LoadMediaFile { path: f, media_id: klipp_core::MediaId::new() });
                        }
                    }
                }
            }
            ui.separator();
            if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() { cmds.push(AppCommand::SaveProject); ui.close_menu(); }
            if ui.add(egui::Button::new("Save As...").shortcut_text("Ctrl+Shift+S")).clicked() { ui.close_menu(); if let Some(p) = rfd::FileDialog::new().set_title("Save As").add_filter("Klipp Project", &["klipp"]).set_file_name("project.klipp").save_file() { cmds.push(AppCommand::SaveProjectAs { path: p }); } }
            if ui.button("Project Info...").clicked() { state.show_project_info = true; ui.close_menu(); }
            if ui.button("Consolidate Project...").clicked() { state.show_consolidate_dialog = true; ui.close_menu(); }
            ui.separator();
            if ui.add(egui::Button::new("Export...").shortcut_text("Ctrl+E")).clicked() { cmds.push(AppCommand::ShowExportDialog); ui.close_menu(); }
            if ui.button("Render Queue").clicked() { cmds.push(AppCommand::ShowRenderQueue); ui.close_menu(); }
            ui.separator();
            if ui.button("New Workspace...").clicked() {
                ui.close_menu();
                if let Some(dir) = rfd::FileDialog::new().set_title("Create Workspace").pick_folder() {
                    let name = dir.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Workspace".into());
                    cmds.push(AppCommand::CreateWorkspace { path: dir, name });
                }
            }
            ui.separator();
            if ui.button("Quit").clicked() { cmds.push(AppCommand::Quit); ui.close_menu(); }
        });
    }
    fn edit_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Edit").size(11.0), |ui| {
            if ui.add_enabled(state.history.can_undo(), egui::Button::new(state.history.undo_label().map(|d| format!("Undo {d}")).unwrap_or("Undo".into())).shortcut_text("Ctrl+Z")).clicked() { cmds.push(AppCommand::Undo); ui.close_menu(); }
            if ui.add_enabled(state.history.can_redo(), egui::Button::new(state.history.redo_label().map(|d| format!("Redo {d}")).unwrap_or("Redo".into())).shortcut_text("Ctrl+Y")).clicked() { cmds.push(AppCommand::Redo); ui.close_menu(); }
            ui.separator();
            if ui.add(egui::Button::new("Split at Playhead").shortcut_text("Ctrl+B")).clicked() { let ph = state.playhead; for (ti, t) in state.sequence().tracks.iter().enumerate() { if let Some((_, c)) = t.clip_at_timecode(ph) { cmds.push(AppCommand::BladeClip { track_index: ti, clip_id: c.id, cut_at: ph }); } } ui.close_menu(); }
            ui.separator();
            // Tool selection
            let check = |active: bool| if active { "\u{2713} " } else { "  " };
            if ui.add(egui::Button::new(format!("{}Select Tool", check(state.active_tool == Tool::Select))).shortcut_text("V")).clicked() { cmds.push(AppCommand::SetActiveTool(Tool::Select)); ui.close_menu(); }
            if ui.add(egui::Button::new(format!("{}Blade Tool", check(state.active_tool == Tool::Blade))).shortcut_text("B")).clicked() { cmds.push(AppCommand::SetActiveTool(Tool::Blade)); ui.close_menu(); }
            if ui.add(egui::Button::new(format!("{}Range Tool", check(state.active_tool == Tool::Range))).shortcut_text("R")).clicked() { cmds.push(AppCommand::SetActiveTool(Tool::Range)); ui.close_menu(); }
            ui.separator();
            if ui.button("Delete Unused Media...").clicked() { cmds.push(AppCommand::DeleteUnusedMedia); ui.close_menu(); }
        });
    }
    fn mark_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Mark").size(11.0), |ui| {
            // In/Out points
            if ui.add(egui::Button::new("Set Range Start").shortcut_text("I")).clicked() {
                cmds.push(AppCommand::SetSequenceInPoint(state.playhead));
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Set Range End").shortcut_text("O")).clicked() {
                cmds.push(AppCommand::SetSequenceOutPoint(state.playhead));
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Select Clip Range").shortcut_text("X")).clicked() {
                let ph = state.playhead;
                for track in &state.sequence().tracks {
                    if let Some((_, clip)) = track.clip_at_timecode(ph) {
                        cmds.push(AppCommand::SetSequenceInPoint(clip.timeline_in));
                        cmds.push(AppCommand::SetSequenceOutPoint(clip.timeline_out));
                        break;
                    }
                }
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Clear Range Start").shortcut_text("Shift+I")).clicked() {
                cmds.push(AppCommand::ClearSequenceInPoint);
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Clear Range End").shortcut_text("Shift+O")).clicked() {
                cmds.push(AppCommand::ClearSequenceOutPoint);
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Clear Range").shortcut_text("Alt+X")).clicked() {
                cmds.push(AppCommand::ClearSequenceInPoint);
                cmds.push(AppCommand::ClearSequenceOutPoint);
                ui.close_menu();
            }
            ui.separator();
            // Loop
            let loop_check = if state.loop_playback_enabled { "\u{2713} " } else { "  " };
            if ui.add(egui::Button::new(format!("{loop_check}Loop Playback")).shortcut_text("Ctrl+L")).clicked() {
                cmds.push(AppCommand::ToggleLoopPlayback);
                ui.close_menu();
            }
            ui.separator();
            // Markers
            if ui.add(egui::Button::new("Add Marker").shortcut_text("M")).clicked() {
                cmds.push(AppCommand::AddMarker {
                    position: state.playhead,
                    label: String::new(),
                    color: klipp_core::MarkerColor::Blue,
                });
                ui.close_menu();
            }
            ui.separator();
            // Range operations
            let has_range = state.sequence().in_point.is_some() && state.sequence().out_point.is_some();
            if ui.add_enabled(has_range, egui::Button::new("Lift Range").shortcut_text(";")).clicked() {
                cmds.push(AppCommand::LiftEdit);
                ui.close_menu();
            }
            if ui.add_enabled(has_range, egui::Button::new("Extract Range").shortcut_text("'")).clicked() {
                cmds.push(AppCommand::ExtractEdit);
                ui.close_menu();
            }
        });
    }
    fn view_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("View").size(11.0), |ui| {
            if ui.button("Full Screen Viewer").clicked() { cmds.push(AppCommand::ToggleFullscreen); ui.close_menu(); }
            ui.separator();
            if ui.button(if state.use_proxies { "\u{2713} Proxies" } else { "  Proxies" }).clicked() { cmds.push(AppCommand::SetUseProxies(!state.use_proxies)); ui.close_menu(); }
            if ui.button(if state.show_markers_panel { "\u{2713} Markers" } else { "  Markers" }).clicked() { state.show_markers_panel = !state.show_markers_panel; ui.close_menu(); }
            if ui.add(egui::Button::new("Preferences...").shortcut_text("Ctrl+,")).clicked() { cmds.push(AppCommand::ShowPreferences); ui.close_menu(); }
        });
    }
    fn settings_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Settings").size(11.0), |ui| {
            if ui.button(if state.snap_enabled { "\u{2713} Snap" } else { "  Snap" }).clicked() { cmds.push(AppCommand::SetSnapEnabled(!state.snap_enabled)); ui.close_menu(); }
            if ui.button(if state.ripple_enabled { "\u{2713} Ripple" } else { "  Ripple" }).clicked() { cmds.push(AppCommand::SetRippleEnabled(!state.ripple_enabled)); ui.close_menu(); }
            ui.separator();
            for mode in &[PlayheadFollow::Off, PlayheadFollow::Page, PlayheadFollow::Smooth] {
                let c = if state.playhead_follow == *mode { "\u{2713} " } else { "  " };
                if ui.button(format!("{}{}", c, mode.label())).clicked() { state.playhead_follow = *mode; ui.close_menu(); }
            }
        });
    }
    fn help_menu(ui: &mut egui::Ui, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Help").size(11.0), |ui| {
            if ui.button("About Klipp").clicked() { state.show_about_dialog = true; ui.close_menu(); }
        });
    }
}

// ═══════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════

fn alloc_btn(ui: &mut egui::Ui, pos: egui::Pos2, size: f32) -> egui::Response {
    let rect = egui::Rect::from_min_size(pos, egui::vec2(size, size));
    ui.allocate_rect(rect, egui::Sense::click())
}

fn icon_color(resp: &egui::Response, active: bool) -> egui::Color32 {
    if active { egui::Color32::from_rgb(0xe8, 0xa0, 0x20) }
    else if resp.hovered() { egui::Color32::from_rgb(0xe8, 0xe8, 0xee) }
    else { egui::Color32::from_rgb(0x90, 0x90, 0xa0) }
}

fn icon_bg(resp: &egui::Response, active: bool) -> egui::Color32 {
    if active { egui::Color32::from_rgba_premultiplied(232, 160, 32, 23) }
    else if resp.hovered() { egui::Color32::from_rgb(0x2a, 0x2a, 0x32) }
    else { egui::Color32::TRANSPARENT }
}

fn icon_rect(btn: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(btn.center(), egui::vec2(16.0, 16.0))
}


fn sep(ui: &mut egui::Ui, theme: &KlippTheme) {
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 22.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, theme.border_light);
    ui.add_space(4.0);
}

fn draw_shield_icon(painter: &egui::Painter, center: egui::Pos2, r: f32, color: egui::Color32) {
    let pts = vec![
        egui::pos2(center.x - r, center.y - r * 0.5),
        egui::pos2(center.x, center.y - r),
        egui::pos2(center.x + r, center.y - r * 0.5),
        egui::pos2(center.x + r, center.y + r * 0.2),
        egui::pos2(center.x, center.y + r),
        egui::pos2(center.x - r, center.y + r * 0.2),
    ];
    painter.add(egui::Shape::convex_polygon(pts, egui::Color32::TRANSPARENT, egui::Stroke::new(1.5, color)));
    // Checkmark
    painter.add(egui::Shape::line(vec![
        egui::pos2(center.x - r * 0.3, center.y),
        egui::pos2(center.x - r * 0.05, center.y + r * 0.3),
        egui::pos2(center.x + r * 0.35, center.y - r * 0.25),
    ], egui::Stroke::new(1.5, color)));
}

// ═══════════════════════════════════════════
// LEFT GROUP TOOL ICONS
// ═══════════════════════════════════════════

fn draw_select_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_rect(btn);
    let pts = vec![
        egui::pos2(ir.min.x + 2.0, ir.min.y),
        egui::pos2(ir.min.x + 2.0, ir.max.y - 2.0),
        egui::pos2(ir.min.x + 6.0, ir.max.y - 6.0),
        egui::pos2(ir.min.x + 12.0, ir.center().y + 2.0),
    ];
    painter.add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
}

fn draw_trim_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_rect(btn);
    let s = egui::Stroke::new(1.5, c);
    painter.line_segment([egui::pos2(ir.min.x + 3.0, ir.min.y + 2.0), egui::pos2(ir.min.x + 3.0, ir.max.y - 2.0)], s);
    painter.line_segment([egui::pos2(ir.max.x - 3.0, ir.min.y + 2.0), egui::pos2(ir.max.x - 3.0, ir.max.y - 2.0)], s);
    painter.line_segment([egui::pos2(ir.min.x + 3.0, ir.center().y), egui::pos2(ir.min.x + 6.0, ir.center().y)], s);
    painter.line_segment([egui::pos2(ir.max.x - 6.0, ir.center().y), egui::pos2(ir.max.x - 3.0, ir.center().y)], s);
}

fn draw_blade_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_rect(btn);
    let s = egui::Stroke::new(1.5, c);
    painter.line_segment([egui::pos2(ir.min.x + 2.0, ir.max.y - 2.0), egui::pos2(ir.max.x - 2.0, ir.min.y + 2.0)], s);
    let pts = vec![
        egui::pos2(ir.max.x - 2.0, ir.min.y + 2.0),
        egui::pos2(ir.max.x - 5.0, ir.min.y + 3.0),
        egui::pos2(ir.max.x - 3.0, ir.min.y + 5.0),
    ];
    painter.add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
}

fn draw_zoom_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_rect(btn);
    let s = egui::Stroke::new(1.5, c);
    let center = egui::pos2(ir.min.x + 6.0, ir.min.y + 6.0);
    painter.circle_stroke(center, 5.0, s);
    painter.line_segment([egui::pos2(center.x + 3.5, center.y + 3.5), egui::pos2(ir.max.x - 2.0, ir.max.y - 2.0)], s);
}

// ═══════════════════════════════════════════
// RIGHT GROUP ICONS (from KLIPP_TOOLBAR_ICONS_FIX.md)
// ═══════════════════════════════════════════

fn draw_roles_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_rect(btn);
    let bh = 2.5;
    let gap = 3.0;
    let top_y = ir.center().y - bh - gap;
    painter.rect_filled(egui::Rect::from_min_size(egui::pos2(ir.min.x, top_y), egui::vec2(16.0, bh)), 1.0, c);
    painter.rect_filled(egui::Rect::from_min_size(egui::pos2(ir.min.x, top_y + bh + gap), egui::vec2(12.8, bh)), 1.0, c);
    painter.rect_filled(egui::Rect::from_min_size(egui::pos2(ir.min.x, top_y + (bh + gap) * 2.0), egui::vec2(9.6, bh)), 1.0, c);
}

fn draw_scopes_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_rect(btn);
    painter.rect_stroke(ir, 2.0, egui::Stroke::new(1.2, c));
    let pts = vec![
        egui::pos2(ir.min.x + 2.0, ir.center().y + 3.0),
        egui::pos2(ir.min.x + 5.0, ir.center().y - 4.0),
        egui::pos2(ir.min.x + 8.0, ir.center().y + 2.0),
        egui::pos2(ir.min.x + 11.0, ir.center().y - 5.0),
        egui::pos2(ir.min.x + 14.0, ir.center().y + 1.0),
    ];
    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.3, c)));
}

fn draw_color_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let center = btn.center();
    let radius = 6.7;
    painter.circle_stroke(center, radius, egui::Stroke::new(1.3, c));
    let mut pts = vec![center];
    for i in 0..=20 {
        let angle = std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * (i as f32 / 20.0);
        pts.push(egui::pos2(center.x + radius * angle.cos(), center.y + radius * angle.sin()));
    }
    pts.push(center);
    painter.add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
}

fn draw_skimming_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_rect(btn);
    let cx = ir.center().x;
    let top = ir.min.y + 1.0;
    let bot = ir.max.y - 1.0;
    let mid = ir.center().y;
    painter.line_segment([egui::pos2(cx, top + 5.0), egui::pos2(cx, bot)], egui::Stroke::new(1.5, c));
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(cx, top), egui::pos2(cx - 4.0, top + 6.0), egui::pos2(cx + 4.0, top + 6.0),
    ], c, egui::Stroke::NONE));
    painter.line_segment([egui::pos2(cx - 5.0, mid + 3.0), egui::pos2(cx + 5.0, mid + 3.0)], egui::Stroke::new(1.5, c));
}

fn draw_render_queue_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_rect(btn);
    let bw = 14.0;
    let bh = 3.0;
    let gap = 3.0;
    let x = ir.min.x + 1.0;
    let top_y = ir.center().y - bh - gap;
    for i in 0..3 {
        let y = top_y + i as f32 * (bh + gap);
        painter.rect_filled(egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(bw, bh)), 1.0, c);
    }
    let mid_y = top_y + bh + gap + bh * 0.5;
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(x + bw + 3.0, mid_y), egui::pos2(x + bw, mid_y - 3.0), egui::pos2(x + bw, mid_y + 3.0),
    ], c, egui::Stroke::NONE));
}

fn draw_range_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_rect(btn);
    let s = egui::Stroke::new(1.5, c);
    // Left bracket bar
    painter.line_segment([egui::pos2(ir.min.x + 3.0, ir.min.y + 1.0), egui::pos2(ir.min.x + 3.0, ir.max.y - 1.0)], s);
    // Right bracket bar
    painter.line_segment([egui::pos2(ir.max.x - 3.0, ir.min.y + 1.0), egui::pos2(ir.max.x - 3.0, ir.max.y - 1.0)], s);
    // Horizontal connecting line
    painter.line_segment([egui::pos2(ir.min.x + 4.0, ir.center().y), egui::pos2(ir.max.x - 4.0, ir.center().y)], s);
    // Subtle fill between bars
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(ir.min.x + 3.0, ir.min.y + 3.0),
            egui::pos2(ir.max.x - 3.0, ir.max.y - 3.0),
        ),
        0.0,
        egui::Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), 30),
    );
}

fn draw_export_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_rect(btn);
    let cx = ir.center().x;
    let top = ir.min.y + 1.0;
    let bot = ir.max.y - 2.0;
    let baseline_y = bot - 2.0;
    let arrow_base = baseline_y - 3.0;
    painter.line_segment([egui::pos2(ir.min.x + 1.0, baseline_y), egui::pos2(ir.max.x - 1.0, baseline_y)], egui::Stroke::new(1.5, c));
    painter.line_segment([egui::pos2(cx, arrow_base), egui::pos2(cx, top + 5.0)], egui::Stroke::new(1.5, c));
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(cx, top), egui::pos2(cx - 4.0, top + 5.5), egui::pos2(cx + 4.0, top + 5.5),
    ], c, egui::Stroke::NONE));
}

