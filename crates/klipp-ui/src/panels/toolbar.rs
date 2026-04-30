use crate::layout_state::EditTool;
use crate::prefs::RecentProject;
use crate::state::{AppState, Workspace};
use crate::theme::KlippTheme;
use crate::widgets::tooltip::styled_tooltip;
use klipp_core::{AppCommand, Tool};

pub struct ToolbarPanel {
    pub logo_texture: Option<egui::TextureHandle>,
}

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
            egui::Stroke::new(1.0_f32, theme.border),
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
            }
        }

        rx -= 2.0;

        // Scopes (icon 2). Renders in any workspace (FCP/Premiere
        // convention). Pure show/hide toggle — does not switch workspace.
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

        // Roles & Lanes button — hidden until the underlying feature exists
        // (no clip role assignment, no lane filtering, no role-coloured bars
        // implemented yet). Re-enable here once the feature ships. Tracked
        // in code_audit.html.

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

                // Brand: logo icon + "Klipp"
                let logo_tex = self.logo_texture.get_or_insert_with(|| {
                    let png_bytes = include_bytes!("../../../../assets/icons/klipp_app_icons/hicolor/64x64/apps/klipp.png");
                    let image = crate::ui_util::decode_png_or_fallback(png_bytes);
                    crate::app::alloc_texture_logged(ui.ctx(), "klipp_logo", image, egui::TextureOptions::LINEAR)
                });
                let logo_size = 18.0;
                let logo_rect = egui::Rect::from_center_size(
                    egui::pos2(ui.cursor().min.x + logo_size * 0.5, cy),
                    egui::vec2(logo_size, logo_size),
                );
                ui.painter().image(
                    logo_tex.id(),
                    logo_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                ui.add_space(logo_size + 4.0);
                ui.label(egui::RichText::new("Klipp").size(15.0).strong().color(theme.text_accent));
                ui.add_space(12.0);

                // Separator
                sep(ui, theme);

                // Compact menus
                Self::file_menu(ui, &mut cmds, state, recent_projects);
                Self::edit_menu(ui, &mut cmds, state);
                Self::clip_menu(ui, &mut cmds, state);
                Self::timeline_menu(ui, &mut cmds, state);
                Self::mark_menu(ui, &mut cmds, state);
                Self::view_menu(ui, &mut cmds, state);
                Self::window_menu(ui, &mut cmds, state);
                Self::tools_menu(ui, &mut cmds, state);
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
                        (theme.accent, theme.bg_void)
                    } else {
                        (theme.bg_raised, theme.text_secondary)
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
                            EditTool::Trim => Tool::Trim,
                            EditTool::Zoom => Tool::Select,
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
                    let bg = if resp.hovered() {
                        mode_color.linear_multiply(0.35)
                    } else {
                        mode_color.linear_multiply(0.25)
                    };
                    let border = mode_color.linear_multiply(0.6);
                    ui.painter().rect_filled(rect, 3.0, bg);
                    ui.painter().rect_stroke(rect, 3.0, egui::Stroke::new(1.0_f32, border));
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

    // ══════════════════════════════════════════════
    // Menu bar: File | Edit | Clip | Timeline | Mark | View | Window | Help
    // ══════════════════════════════════════════════

    fn file_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState, recent: &[RecentProject]) {
        ui.menu_button(egui::RichText::new("File").size(11.0), |ui| {
            if ui.add(egui::Button::new("New Project...").shortcut_text("Ctrl+N")).clicked() { cmds.push(AppCommand::NewProject); ui.close_menu(); }
            if ui.add(egui::Button::new("Open Project...").shortcut_text("Ctrl+O")).clicked() { ui.close_menu(); if let Some(p) = rfd::FileDialog::new().set_title("Open Project").pick_folder() { cmds.push(AppCommand::OpenProject { path: p }); } }
            if !recent.is_empty() {
                ui.menu_button("Open Recent", |ui| {
                    for rp in recent {
                        let label = rp.name.clone();
                        if ui.button(&label).clicked() { cmds.push(AppCommand::OpenProject { path: rp.path.clone() }); ui.close_menu(); }
                    }
                });
            }
            ui.separator();
            if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() { cmds.push(AppCommand::SaveProject); ui.close_menu(); }
            if ui.add(egui::Button::new("Save As...").shortcut_text("Ctrl+Shift+S")).clicked() { ui.close_menu(); if let Some(p) = rfd::FileDialog::new().set_title("Save As").add_filter("Klipp Project", &["klipp"]).set_file_name("project.klipp").save_file() { cmds.push(AppCommand::SaveProjectAs { path: p }); } }
            if ui.button("Revert to Saved").clicked() {
                if let Some(path) = state.project_path.clone() { cmds.push(AppCommand::OpenProject { path }); }
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Import Media...").shortcut_text("Ctrl+I")).clicked() {
                ui.close_menu();
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("All media", &klipp_core::media_extensions::all_media_extensions())
                    .add_filter("Video", klipp_core::media_extensions::VIDEO_EXTENSIONS)
                    .add_filter("Audio", klipp_core::media_extensions::AUDIO_EXTENSIONS)
                    .add_filter("Image", klipp_core::media_extensions::IMAGE_EXTENSIONS)
                    .pick_file()
                { cmds.push(AppCommand::LoadMediaFile { path: p, media_id: klipp_core::MediaId::new() }); }
            }
            if ui.button("Import Subtitles...").clicked() {
                ui.close_menu();
                if let Some(path) = rfd::FileDialog::new().set_title("Import Subtitles").add_filter("Subtitles", &["srt","vtt"]).pick_file() {
                    cmds.push(AppCommand::ImportSubtitles { path });
                }
            }
            // Timeline-format imports (foreign NLE projects → a new Sequence).
            // Grouped under a submenu so adding formats (OTIO, FCPXML, EDL, AAF)
            // doesn't clutter the top level.
            ui.menu_button("Import Timeline", |ui| {
                if ui.button("Kdenlive Project...").clicked() {
                    ui.close_menu();
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Import Kdenlive Project")
                        .add_filter("Kdenlive Project", &["kdenlive", "mlt"])
                        .pick_file()
                    {
                        cmds.push(AppCommand::ImportKdenlive { path });
                    }
                }
                if ui.button("OpenTimelineIO (.otio)...").clicked() {
                    ui.close_menu();
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Import OpenTimelineIO")
                        .add_filter("OpenTimelineIO", &["otio"])
                        .pick_file()
                    {
                        cmds.push(AppCommand::ImportOtio { path });
                    }
                }
                if ui.button("Final Cut Pro XML (.fcpxml)...").clicked() {
                    ui.close_menu();
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Import FCPXML Project")
                        .add_filter("Final Cut Pro XML", &["fcpxml", "xml"])
                        .pick_file()
                    {
                        cmds.push(AppCommand::ImportFcpxml { path });
                    }
                }
            });
            ui.separator();
            if ui.add(egui::Button::new("Export...").shortcut_text("Ctrl+E")).clicked() { cmds.push(AppCommand::ShowExportDialog); ui.close_menu(); }
            if ui.button("Export Subtitles...").clicked() { ui.close_menu(); if let Some(path) = rfd::FileDialog::new().set_title("Export Subtitles").add_filter("SRT", &["srt"]).add_filter("WebVTT", &["vtt"]).add_filter("ASS", &["ass"]).add_filter("SSA", &["ssa"]).set_file_name("subtitles.srt").save_file() { cmds.push(AppCommand::ExportSubtitles { path }); } }
            if ui.button("YouTube Chapters\u{2026}").clicked() {
                ui.close_menu();
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Export YouTube Chapters")
                    .add_filter("Text file", &["txt"])
                    .set_file_name("chapters.txt")
                    .save_file()
                {
                    cmds.push(AppCommand::ExportYouTubeChapters { path });
                }
            }
            if ui.button("DVD Chapters\u{2026}").clicked() {
                ui.close_menu();
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Export DVD Chapters")
                    .add_filter("Text file", &["txt"])
                    .set_file_name("chapters.txt")
                    .save_file()
                {
                    cmds.push(AppCommand::ExportDvdChapters { path });
                }
            }
            if ui.button("Generate Render Script\u{2026}").clicked() {
                ui.close_menu();
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Save Render Script")
                    .add_filter("Shell script", &["sh"])
                    .set_file_name("render.sh")
                    .save_file()
                {
                    cmds.push(AppCommand::GenerateRenderScript { path });
                }
            }
            // Timeline-format exports (current Sequence → interchange format).
            ui.menu_button("Export Timeline", |ui| {
                if ui.button("FCPXML...").clicked() {
                    ui.close_menu();
                    if let Some(path) = rfd::FileDialog::new().set_title("Export FCPXML").add_filter("FCPXML", &["fcpxml"]).set_file_name("export.fcpxml").save_file() {
                        cmds.push(AppCommand::ExportFcpxml { path });
                    }
                }
                if ui.button("Kdenlive Project...").clicked() {
                    ui.close_menu();
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Export as Kdenlive Project")
                        .add_filter("Kdenlive project", &["kdenlive"])
                        .set_file_name("project.kdenlive")
                        .save_file()
                    {
                        cmds.push(AppCommand::ExportKdenlive { path });
                    }
                }
                if ui.button("EDL...").clicked() {
                    ui.close_menu();
                    if let Some(path) = rfd::FileDialog::new().set_title("Export EDL").add_filter("EDL", &["edl"]).set_file_name("export.edl").save_file() {
                        cmds.push(AppCommand::ExportEdl { path });
                    }
                }
            });
            ui.separator();
            if ui.add(egui::Button::new("Close Project").shortcut_text("Ctrl+W")).clicked() { cmds.push(AppCommand::CloseProject); ui.close_menu(); }
            if ui.button("Project Properties...").clicked() { state.show_project_info = true; ui.close_menu(); }
            if ui.button("Sequence Settings...").clicked() {
                cmds.push(AppCommand::OpenSequenceSettings { sequence_idx: state.active_sequence_idx });
                ui.close_menu();
            }
            let has_offline = !state.offline_media_ids.is_empty();
            if ui.add_enabled(has_offline, egui::Button::new("Relink Media...")).clicked() { cmds.push(AppCommand::ShowRelinkDialog); ui.close_menu(); }
            // Consolidate Project: copy the project + all media into a new
            // self-contained directory. Requires the project to already be saved.
            let has_project = state.project_path.is_some();
            if ui.add_enabled(has_project, egui::Button::new("Consolidate Project...")).clicked() {
                // Pre-fill destination field with `<project>_consolidated` as a suggestion
                if state.consolidate_dest_path.is_empty() {
                    if let Some(src) = &state.project_path {
                        let suggestion = {
                            let parent = src.parent().map(|p| p.to_path_buf())
                                .unwrap_or_else(|| std::path::PathBuf::from("."));
                            let name = src.file_name().map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "project".to_string());
                            parent.join(format!("{name}_consolidated"))
                        };
                        state.consolidate_dest_path = suggestion.to_string_lossy().to_string();
                    }
                }
                state.show_consolidate_dialog = true;
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Quit").shortcut_text("Ctrl+Q")).clicked() { cmds.push(AppCommand::Quit); ui.close_menu(); }
        });
    }

    fn edit_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Edit").size(11.0), |ui| {
            if ui.add_enabled(state.history.can_undo(), egui::Button::new(state.history.undo_label().map(|d| format!("Undo {d}")).unwrap_or("Undo".into())).shortcut_text("Ctrl+Z")).clicked() { cmds.push(AppCommand::Undo); ui.close_menu(); }
            if ui.add_enabled(state.history.can_redo(), egui::Button::new(state.history.redo_label().map(|d| format!("Redo {d}")).unwrap_or("Redo".into())).shortcut_text("Ctrl+Shift+Z")).clicked() { cmds.push(AppCommand::Redo); ui.close_menu(); }
            if ui.button("History...").clicked() { state.show_history_panel = true; ui.close_menu(); }
            ui.separator();
            if ui.add_enabled(state.selected_clip.is_some(), egui::Button::new("Cut").shortcut_text("Ctrl+X")).clicked() {
                if let Some(clip_id) = state.selected_clip {
                    cmds.push(AppCommand::CopyClip { clip_id });
                    cmds.push(AppCommand::RemoveClip { clip_id });
                }
                ui.close_menu();
            }
            if ui.add_enabled(state.selected_clip.is_some(), egui::Button::new("Copy").shortcut_text("Ctrl+C")).clicked() {
                if let Some(clip_id) = state.selected_clip { cmds.push(AppCommand::CopyClip { clip_id }); }
                ui.close_menu();
            }
            if ui.add_enabled(state.clipboard.is_some(), egui::Button::new("Paste").shortcut_text("Ctrl+V")).clicked() {
                cmds.push(AppCommand::PasteClip);
                ui.close_menu();
            }
            if ui.add_enabled(state.attributes_clipboard.is_some() && state.selected_clip.is_some(), egui::Button::new("Paste Attributes...").shortcut_text("Ctrl+Shift+V")).clicked() {
                state.show_paste_attributes_dialog = true; ui.close_menu();
            }
            if ui.add_enabled(state.selected_clip.is_some(), egui::Button::new("Duplicate").shortcut_text("Ctrl+D")).clicked() {
                if let Some(clip_id) = state.selected_clip { cmds.push(AppCommand::DuplicateClip { clip_id }); }
                ui.close_menu();
            }
            if ui.add_enabled(state.selected_clip.is_some(), egui::Button::new("Delete").shortcut_text("Del")).clicked() {
                if let Some(clip_id) = state.selected_clip { cmds.push(AppCommand::RemoveClip { clip_id }); }
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Select All").shortcut_text("Ctrl+A")).clicked() { cmds.push(AppCommand::SelectAllClips); ui.close_menu(); }
            if ui.button("Select Forward").clicked() { cmds.push(AppCommand::SelectClipsForward); ui.close_menu(); }
            if ui.button("Select Backward").clicked() { cmds.push(AppCommand::SelectClipsBackward); ui.close_menu(); }
            if ui.add(egui::Button::new("Deselect All").shortcut_text("Escape")).clicked() { { state.selected_clip = None; state.selected_clips.clear(); }; ui.close_menu(); }
            ui.separator();
            if ui.add(egui::Button::new("Preferences...").shortcut_text("Ctrl+,")).clicked() { cmds.push(AppCommand::ShowPreferences); ui.close_menu(); }
        });
    }

    fn clip_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Clip").size(11.0), |ui| {
            let has_clip = state.selected_clip.is_some();
            if ui.add_enabled(has_clip, egui::Button::new("Enable / Disable").shortcut_text("V")).clicked() {
                if let Some(cid) = state.selected_clip {
                    let enabled = state.sequence().all_clips().find(|c| c.id == cid).is_none_or(|c| c.enabled);
                    cmds.push(AppCommand::SetClipEnabled { clip_id: cid, enabled: !enabled });
                }
                ui.close_menu();
            }
            if ui.add_enabled(has_clip, egui::Button::new("Solo").shortcut_text("Ctrl+L")).clicked() {
                if let Some(cid) = state.selected_clip {
                    let soloed = state.sequence().all_clips().find(|c| c.id == cid).is_some_and(|c| c.solo);
                    cmds.push(AppCommand::SetClipSolo { clip_id: cid, solo: !soloed });
                }
                ui.close_menu();
            }
            ui.separator();
            // Match Frame — finds the source frame for the program
            // playhead and loads it in the source viewer; the reverse
            // walks the timeline forward to the next occurrence of
            // the source viewer's current frame. Both honour speed
            // ramps + clip.source_in.
            if ui.add(egui::Button::new("Match Frame").shortcut_text("F")).clicked() {
                cmds.push(AppCommand::MatchFrameProgramToSource);
                ui.close_menu();
            }
            let has_source = state.source_media_id.is_some();
            if ui.add_enabled(has_source,
                egui::Button::new("Match Frame Reverse").shortcut_text("Shift+F"),
            ).clicked() {
                cmds.push(AppCommand::MatchFrameSourceToProgram);
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Split at Playhead").shortcut_text("S")).clicked() {
                let ph = state.playhead;
                for (ti, t) in state.sequence().tracks.iter().enumerate() {
                    if let Some((_, c)) = t.clip_at_timecode(ph) { cmds.push(AppCommand::BladeClip { track_index: ti, clip_id: c.id, cut_at: ph }); }
                }
                ui.close_menu();
            }
            if ui.add_enabled(has_clip, egui::Button::new("Close Gap")).clicked() {
                if let Some(cid) = state.selected_clip {
                    // Find the track and clip, then look for a gap before it
                    for (ti, track) in state.sequence().tracks.iter().enumerate() {
                        if let Some((ci, clip)) = track.clips.iter().enumerate().find(|(_, c)| c.id == cid) {
                            if ci > 0 {
                                let prev = &track.clips[ci - 1];
                                let gap_start = prev.timeline_out;
                                let gap_end = clip.timeline_in;
                                if gap_end > gap_start {
                                    cmds.push(AppCommand::CloseGap { track_index: ti, gap_start, gap_end });
                                }
                            }
                            break;
                        }
                    }
                }
                ui.close_menu();
            }
            if ui.add_enabled(has_clip, egui::Button::new("Ripple Delete").shortcut_text("Shift+Del")).clicked() {
                if let Some(clip_id) = state.selected_clip { cmds.push(AppCommand::RemoveClip { clip_id }); }
                ui.close_menu();
            }
            ui.separator();
            if ui.add_enabled(has_clip, egui::Button::new("Speed...")).clicked() {
                // Read current clip speed to pre-fill dialog
                if let Some(cid) = state.selected_clip {
                    let spd = state.sequence().all_clips().find(|c| c.id == cid).map(|c| c.speed).unwrap_or(1.0);
                    state.speed_dialog_value = spd.abs() * 100.0;
                    state.speed_dialog_reverse = spd < 0.0;
                }
                state.show_speed_dialog = true;
                ui.close_menu();
            }
            ui.menu_button("Motion Presets", |ui| {
                for (label, preset) in [
                    ("Ken Burns: Zoom In", "ken_burns_zoom_in"),
                    ("Ken Burns: Zoom Out", "ken_burns_zoom_out"),
                    ("Ken Burns: Pan Left", "ken_burns_pan_left"),
                    ("PiP: Top Right", "pip_top_right"),
                    ("PiP: Bottom Left", "pip_bottom_left"),
                ] {
                    if let Some(cid) = state.selected_clip {
                        if ui.button(label).clicked() { cmds.push(AppCommand::ApplyMotionPreset { clip_id: cid, preset: preset.into() }); ui.close_menu(); }
                    }
                }
            });
            ui.separator();
            if ui.add_enabled(has_clip, egui::Button::new("Copy Attributes").shortcut_text("Ctrl+Shift+C")).clicked() {
                if let Some(cid) = state.selected_clip { cmds.push(AppCommand::CopyAttributes { clip_id: cid }); }
                ui.close_menu();
            }
            if ui.add_enabled(state.attributes_clipboard.is_some() && has_clip, egui::Button::new("Paste Attributes...").shortcut_text("Ctrl+Shift+V")).clicked() {
                state.show_paste_attributes_dialog = true; ui.close_menu();
            }
            ui.separator();
            // Multicam
            if ui.button("Create Multicam Clip").clicked() {
                cmds.push(AppCommand::CreateMulticamClip);
                ui.close_menu();
            }
            if has_clip {
                let is_multicam = state.sequence().all_clips()
                    .find(|c| c.id == state.selected_clip.unwrap())
                    .and_then(|c| c.multicam_group)
                    .is_some();
                if is_multicam {
                    ui.menu_button("Switch Angle", |ui| {
                        for angle in 1..=9_usize {
                            let label = format!("Angle {angle}");
                            if ui.add(egui::Button::new(&label).shortcut_text(format!("{angle}"))).clicked() {
                                cmds.push(AppCommand::SwitchMulticamAngle { angle });
                                ui.close_menu();
                            }
                        }
                    });
                }
            }
            ui.separator();
            if ui.button("Add Track Point").clicked() {
                state.tracking_mode = !state.tracking_mode;
                ui.close_menu();
            }
            if has_clip {
                let has_tracks = state.sequence().all_clips()
                    .find(|c| c.id == state.selected_clip.unwrap())
                    .map(|c| !c.track_points.is_empty())
                    .unwrap_or(false);
                if has_tracks {
                    if ui.button("Analyze Track Points").clicked() {
                        let cid = state.selected_clip.unwrap();
                        let count = state.sequence().all_clips()
                            .find(|c| c.id == cid)
                            .map(|c| c.track_points.len())
                            .unwrap_or(0);
                        for i in 0..count {
                            cmds.push(AppCommand::AnalyzeTrackPoint { clip_id: cid, point_index: i });
                        }
                        ui.close_menu();
                    }
                }
            }
            ui.separator();
            if ui.add_enabled(has_clip, egui::Button::new("Reveal in Browser")).clicked() {
                if let Some(cid) = state.selected_clip {
                    let mid = state.sequence().all_clips().find(|c| c.id == cid).and_then(|c| c.media_id());
                    if let Some(mid) = mid {
                        state.reveal_in_browser = Some(mid);
                        state.show_browser_panel = true;
                    }
                }
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Open in Color Panel").shortcut_text("C")).clicked() {
                state.show_color_panel = !state.show_color_panel;
                ui.close_menu();
            }
        });
    }

    fn timeline_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Timeline").size(11.0), |ui| {
            if ui.button("Add Video Track").clicked() { cmds.push(AppCommand::AddVideoTrack); ui.close_menu(); }
            if ui.button("Add Audio Track").clicked() { cmds.push(AppCommand::AddAudioTrack); ui.close_menu(); }
            if ui.button("Delete Track").clicked() {
                // Delete the track containing the selected clip, or the last track
                if let Some(clip_id) = state.selected_clip {
                    if let Some(track) = state.sequence().tracks.iter().find(|t| t.clips.iter().any(|c| c.id == clip_id)) {
                        cmds.push(AppCommand::RemoveTrack { track_id: track.id });
                    }
                }
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Insert Edit").shortcut_text("W")).clicked() { cmds.push(AppCommand::InsertEdit); ui.close_menu(); }
            if ui.add(egui::Button::new("Overwrite Edit").shortcut_text("D")).clicked() { cmds.push(AppCommand::OverwriteEdit); ui.close_menu(); }
            if ui.button("Lift Edit").clicked() { cmds.push(AppCommand::LiftEdit); ui.close_menu(); }
            if ui.button("Extract Edit").clicked() { cmds.push(AppCommand::ExtractEdit); ui.close_menu(); }
            if ui.add(egui::Button::new("Freeze Frame").shortcut_text("Shift+F")).clicked() {
                cmds.push(AppCommand::InsertFreezeFrame { duration_secs: 2.0 });
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Add Adjustment Layer").clicked() { cmds.push(AppCommand::CreateAdjustmentLayer); ui.close_menu(); }
            ui.menu_button("Add Generator", |ui| {
                if ui.button("Solid Color").clicked() {
                    cmds.push(AppCommand::CreateGeneratorClip { track_index: None, data: klipp_core::GeneratorData::SolidColor { color: [128, 128, 128, 255] } });
                    ui.close_menu();
                }
                if ui.button("Color Bars").clicked() {
                    cmds.push(AppCommand::CreateGeneratorClip { track_index: None, data: klipp_core::GeneratorData::ColorBars });
                    ui.close_menu();
                }
                if ui.button("Black").clicked() {
                    cmds.push(AppCommand::CreateGeneratorClip { track_index: None, data: klipp_core::GeneratorData::SolidColor { color: [0, 0, 0, 255] } });
                    ui.close_menu();
                }
            });
            if ui.button("Create Compound Clip").clicked() {
                cmds.push(AppCommand::CreateCompoundClip { name: "Compound Clip".into() });
                ui.close_menu();
            }
            if ui.button("Create Multicam Clip").clicked() {
                cmds.push(AppCommand::CreateMulticamClip);
                ui.close_menu();
            }
            ui.separator();
            let snap_check = if state.snap_enabled { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{snap_check}Snapping")).shortcut_text("N")).clicked() { cmds.push(AppCommand::SetSnapEnabled(!state.snap_enabled)); ui.close_menu(); }
            if ui.add(egui::Button::new("Fit to Window").shortcut_text("Shift+Z")).clicked() { cmds.push(AppCommand::FitTimelineToWindow); ui.close_menu(); }
            if ui.button("Zoom to Selection").clicked() { cmds.push(AppCommand::FitTimelineToWindow); ui.close_menu(); }
        });
    }

    fn mark_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Mark").size(11.0), |ui| {
            if ui.add(egui::Button::new("Set In Point").shortcut_text("I")).clicked() { cmds.push(AppCommand::SetSequenceInPoint(state.playhead)); ui.close_menu(); }
            if ui.add(egui::Button::new("Set Out Point").shortcut_text("O")).clicked() { cmds.push(AppCommand::SetSequenceOutPoint(state.playhead)); ui.close_menu(); }
            if ui.add(egui::Button::new("Clear In Point").shortcut_text("Alt+I")).clicked() { cmds.push(AppCommand::ClearSequenceInPoint); ui.close_menu(); }
            if ui.add(egui::Button::new("Clear Out Point").shortcut_text("Alt+O")).clicked() { cmds.push(AppCommand::ClearSequenceOutPoint); ui.close_menu(); }
            if ui.add(egui::Button::new("Clear In and Out").shortcut_text("Alt+X")).clicked() { cmds.push(AppCommand::ClearSequenceInPoint); cmds.push(AppCommand::ClearSequenceOutPoint); ui.close_menu(); }
            ui.separator();
            if ui.add(egui::Button::new("Go to In Point").shortcut_text("Shift+I")).clicked() { if let Some(inp) = state.sequence().in_point { cmds.push(AppCommand::SeekTo(inp)); } ui.close_menu(); }
            if ui.add(egui::Button::new("Go to Out Point").shortcut_text("Shift+O")).clicked() { if let Some(outp) = state.sequence().out_point { cmds.push(AppCommand::SeekTo(outp)); } ui.close_menu(); }
            ui.separator();
            if ui.add(egui::Button::new("Add Marker").shortcut_text("M")).clicked() {
                cmds.push(AppCommand::AddMarker { position: state.playhead, label: String::new(), color: klipp_core::MarkerColor::Blue });
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Edit Marker...").shortcut_text("Shift+M")).clicked() {
                // Open marker editor for the marker at or nearest playhead
                state.show_markers_panel = true;
                ui.close_menu();
            }
            if ui.button("Delete Marker").clicked() {
                // Delete the marker at/nearest playhead
                let ph = state.playhead;
                if let Some(marker) = state.sequence().markers.iter().find(|m| m.position == ph) {
                    cmds.push(AppCommand::RemoveMarker { marker_id: marker.id });
                }
                ui.close_menu();
            }
            if ui.button("Delete All Markers").clicked() {
                let ids: Vec<_> = state.sequence().markers.iter().map(|m| m.id).collect();
                for id in ids { cmds.push(AppCommand::RemoveMarker { marker_id: id }); }
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Next Marker").clicked() {
                let ph = state.playhead;
                if let Some(next) = state.sequence().markers.iter().filter(|m| m.position > ph).min_by_key(|m| m.position) {
                    cmds.push(AppCommand::SeekTo(next.position));
                }
                ui.close_menu();
            }
            if ui.button("Previous Marker").clicked() {
                let ph = state.playhead;
                if let Some(prev) = state.sequence().markers.iter().filter(|m| m.position < ph).max_by_key(|m| m.position) {
                    cmds.push(AppCommand::SeekTo(prev.position));
                }
                ui.close_menu();
            }
        });
    }

    fn view_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("View").size(11.0), |ui| {
            if ui.add(egui::Button::new("Zoom In").shortcut_text("Ctrl++")).clicked() {
                state.viewer_zoom = (state.viewer_zoom * 1.25).min(4.0).max(0.25);
                if state.viewer_zoom < 0.2 { state.viewer_zoom = 0.25; }
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Zoom Out").shortcut_text("Ctrl+-")).clicked() {
                state.viewer_zoom = if state.viewer_zoom <= 0.25 { 0.0 } else { (state.viewer_zoom / 1.25).max(0.0) };
                ui.close_menu();
            }
            ui.menu_button("Viewer Zoom", |ui| {
                for (label, zoom) in [("Fit", 0.0f32), ("25%", 0.25), ("50%", 0.5), ("100%", 1.0), ("200%", 2.0)] {
                    let check = if (state.viewer_zoom - zoom).abs() < 0.01 { "\u{2714} " } else { "  " };
                    if ui.button(format!("{check}{label}")).clicked() { state.viewer_zoom = zoom; ui.close_menu(); }
                }
            });
            ui.separator();
            let zebra_check = if state.viewer_overlays.zebras { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{zebra_check}Zebras")).shortcut_text("Z")).clicked() {
                state.viewer_overlays.zebras = !state.viewer_overlays.zebras;
                ui.close_menu();
            }
            let fc_check = if state.viewer_overlays.false_color { "\u{2714} " } else { "  " };
            // Main button toggles false color on/off
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(format!("{fc_check}False Color")).shortcut_text("Alt+F")).clicked() {
                    state.viewer_overlays.false_color = !state.viewer_overlays.false_color;
                    ui.close_menu();
                }
                // Submenu for the palette quick-switcher. egui auto-adds
                // its own submenu arrow to menu_button labels, so we give
                // this a descriptive word rather than another arrow glyph
                // (which produced double-arrow visuals and, at smaller
                // sizes, unrenderable placeholder squares).
                ui.menu_button("Palette", |ui| {
                    let built_ins = crate::prefs::FalseColorPalette::built_ins();
                    for preset in &built_ins {
                        let is_current = state.pending_false_color_palette.is_none()
                            && state.false_color_lut == preset.build_lut();
                        let check = if is_current { "\u{2714} " } else { "  " };
                        if ui.button(format!("{check}{}", preset.name)).clicked() {
                            state.pending_false_color_palette = Some(preset.clone());
                            state.viewer_overlays.false_color = true;
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    if ui.button("Edit palette\u{2026}").clicked() {
                        state.show_preferences = true;
                        ui.close_menu();
                    }
                });
            });
            let peak_check = if state.viewer_overlays.focus_peaking { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{peak_check}Focus Peaking")).shortcut_text("Ctrl+P")).clicked() {
                state.viewer_overlays.focus_peaking = !state.viewer_overlays.focus_peaking;
                ui.close_menu();
            }
            ui.separator();
            let safe_check = if state.viewer_overlays.safe_areas { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{safe_check}Safe Areas")).shortcut_text("Alt+S")).clicked() {
                state.viewer_overlays.safe_areas = !state.viewer_overlays.safe_areas;
                ui.close_menu();
            }
            let grid_check = if state.viewer_overlays.grid { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{grid_check}Grid Overlay")).shortcut_text("Alt+G")).clicked() {
                state.viewer_overlays.grid = !state.viewer_overlays.grid;
                ui.close_menu();
            }
            let checker_check = if state.viewer_overlays.checkerboard { "\u{2714} " } else { "  " };
            if ui.button(format!("{checker_check}Checkerboard")).clicked() {
                state.viewer_overlays.checkerboard = !state.viewer_overlays.checkerboard;
                ui.close_menu();
            }
            ui.separator();
            let scopes_check = if state.show_scopes { "\u{2714} " } else { "  " };
            ui.menu_button(format!("{scopes_check}Scopes"), |ui| {
                // Toggle scopes panel + select scope type
                if ui.button("Waveform").clicked() { state.show_scopes = true; ui.close_menu(); }
                if ui.button("Vectorscope").clicked() { state.show_scopes = true; ui.close_menu(); }
                if ui.button("Histogram").clicked() { state.show_scopes = true; ui.close_menu(); }
                if ui.button("Parade").clicked() { state.show_scopes = true; ui.close_menu(); }
                ui.separator();
                if ui.button("Hide Scopes").clicked() { state.show_scopes = false; ui.close_menu(); }
                ui.separator();
                let overlay_check = if state.scope_overlay_mode { "\u{2714} " } else { "  " };
                if ui.button(format!("{overlay_check}Overlay on Viewer")).clicked() { state.scope_overlay_mode = !state.scope_overlay_mode; ui.close_menu(); }
            });
            let color_check = if state.show_color_panel { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{color_check}Color Panel")).shortcut_text("C")).clicked() {
                state.show_color_panel = !state.show_color_panel;
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Extract Frame to File\u{2026}").shortcut_text("Ctrl+Alt+S")).clicked() {
                cmds.push(AppCommand::FrameGrab);
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Extract Frame to Bin").shortcut_text("Ctrl+Alt+B")).clicked() {
                cmds.push(AppCommand::ExtractFrameToBin);
                ui.close_menu();
            }
            ui.separator();
            let perf_check = if state.show_perf_overlay { "\u{2714} " } else { "  " };
            if ui.button(format!("{perf_check}Performance Overlay").as_str()).clicked() {
                state.show_perf_overlay = !state.show_perf_overlay;
                ui.close_menu();
            }
            let rec_label = if state.perf_recording_active {
                "  Stop Performance Recording"
            } else {
                "  Record Performance Session\u{2026}"
            };
            if ui.add(egui::Button::new(rec_label).shortcut_text("Ctrl+Shift+P")).clicked() {
                state.pending_perf_recording_toggle = true;
                ui.close_menu();
            }
            if let Some(ref p) = state.perf_last_session_path {
                let short = p.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.to_string_lossy().into_owned());
                if ui.button(format!("  Last Session: {short}").as_str()).clicked() {
                    // Toast the full path so the user can copy it —
                    // revealing in the file manager would need an
                    // extra dependency we don't currently ship.
                    state.toast.show(format!("Perf session: {}", p.display()));
                    ui.close_menu();
                }
            }
        });
    }

    fn window_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Window").size(11.0), |ui| {
            // Active-workspace mode flags (controls the composite
            // inspector layout — not the same as a full workspace
            // preset below; kept as-is for backwards compatibility).
            ui.label(egui::RichText::new("ACTIVE MODE").size(9.0).color(egui::Color32::from_rgb(0x70, 0x70, 0x78)));
            for (ws, label) in &[(Workspace::Edit, "Default"), (Workspace::Color, "Color Grading"), (Workspace::Export, "Audio Mixing")] {
                let c = if state.active_workspace == *ws { "\u{2714} " } else { "  " };
                if ui.button(format!("{c}{label}")).clicked() { state.active_workspace = *ws; ui.close_menu(); }
            }
            ui.separator();

            // Built-in workspace layout presets (#39). Dispatched via
            // AppCommand so the apply path — write visibility + sizes +
            // layout.save() — stays in one place.
            ui.label(egui::RichText::new("BUILT-IN LAYOUTS").size(9.0).color(egui::Color32::from_rgb(0x70, 0x70, 0x78)));
            for (i, layout) in crate::state::builtin_workspace_layouts().iter().enumerate() {
                if ui.button(&layout.name).clicked() {
                    cmds.push(AppCommand::ApplyBuiltinWorkspaceLayout { index: i });
                    ui.close_menu();
                }
            }

            // User-saved workspaces.
            if !state.custom_workspaces.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new("SAVED").size(9.0).color(egui::Color32::from_rgb(0x70, 0x70, 0x78)));
                for ws in state.custom_workspaces.clone().iter() {
                    if ui.button(&ws.name).clicked() {
                        cmds.push(AppCommand::ApplyCustomWorkspaceLayout { name: ws.name.clone() });
                        ui.close_menu();
                    }
                }
            }

            ui.separator();
            if ui.button("Save Workspace...").clicked() {
                state.save_workspace_name = "My Workspace".into();
                state.show_save_workspace_dialog = true;
                ui.close_menu();
            }
            if ui.button("\u{2699} Manage Workspaces\u{2026}").clicked() {
                state.show_workspace_manager = true;
                ui.close_menu();
            }
            ui.separator();
            // Panels
            ui.label(egui::RichText::new("PANELS").size(9.0).color(egui::Color32::from_rgb(0x70, 0x70, 0x78)));
            let toggle_label = |active: bool, name: &str| if active { format!("\u{2714} {name}") } else { format!("  {name}") };
            if ui.add(egui::Button::new(toggle_label(state.show_inspector_panel, "Inspector")).shortcut_text("Ctrl+Shift+I")).clicked() { state.show_inspector_panel = !state.show_inspector_panel; ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.show_browser_panel, "Effects Browser")).shortcut_text("Ctrl+Shift+E")).clicked() { state.show_browser_panel = !state.show_browser_panel; ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.show_curve_editor, "Curve Editor")).shortcut_text("Shift+K")).clicked() { state.show_curve_editor = !state.show_curve_editor; ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.show_audio_mixer, "Audio Mixer")).shortcut_text("Ctrl+Shift+M")).clicked() { state.show_audio_mixer = !state.show_audio_mixer; ui.close_menu(); }
            if ui.button(toggle_label(state.show_history_panel, "History")).clicked() { state.show_history_panel = !state.show_history_panel; ui.close_menu(); }
            if ui.button(toggle_label(state.show_markers_panel, "Markers")).clicked() { state.show_markers_panel = !state.show_markers_panel; ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.show_subtitle_editor, "Subtitle Editor")).shortcut_text("Ctrl+Shift+T")).clicked() { state.show_subtitle_editor = !state.show_subtitle_editor; ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.show_project_notes, "Project Notes")).shortcut_text("Ctrl+Shift+N")).clicked() { state.show_project_notes = !state.show_project_notes; ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.ai_tools_state.panel_visible, "AI Tools")).shortcut_text("Alt+A")).clicked() { state.ai_tools_state.panel_visible = !state.ai_tools_state.panel_visible; ui.close_menu(); }
            ui.separator();
            // Modal dialogs — distinct from panels because they
            // overlay the editor instead of docking. Plugin-management
            // dialogs (AI Models, Audio Plugins) live under their own
            // top-level "Plugins" menu so future LV2 / VST3 sections
            // have a natural home; they used to live here under
            // Window but the noun mismatch (modal dialog vs panel)
            // got worse as more were added.
            if ui.add(egui::Button::new("Command Palette").shortcut_text("Ctrl+K")).clicked() { state.show_command_palette = true; ui.close_menu(); }
        });
    }

    /// Tools menu — utilities and managers that don't belong under
    /// File / Edit / Clip / Timeline / Mark / View / Window.
    /// Convention follows Avid Media Composer and DaVinci Resolve
    /// where Tools is the natural home for utility-style dialogs and
    /// managers.
    ///
    /// Two groups separated by a divider:
    ///   1. Plugin / model managers — Audio Plugins, AI Models
    ///   2. Project-state utilities — Cache Manager, Render Queue
    ///
    /// LV2 / VST3 plugin managers will join group 1; project-doctor /
    /// consolidate / asset-relink etc. will join group 2.
    fn tools_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Tools").size(11.0), |ui| {
            if ui.button("Audio Plugins\u{2026}").clicked() {
                state.show_audio_plugins_dialog = true;
                state.audio_plugins_dialog.last_scanned_secs = None;
                ui.close_menu();
            }
            if ui.button("AI Models\u{2026}").clicked() {
                state.show_ai_models_dialog = true;
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Cache Manager\u{2026}").clicked() {
                cmds.push(AppCommand::OpenCacheManager);
                ui.close_menu();
            }
            if ui.button("Render Queue").clicked() {
                cmds.push(AppCommand::ShowRenderQueue);
                ui.close_menu();
            }
        });
    }

    fn help_menu(ui: &mut egui::Ui, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Help").size(11.0), |ui| {
            if ui.add(egui::Button::new("Klipp Documentation").shortcut_text("F1")).clicked() {
                // Open local mdbook docs if available, fall back to GitHub
                let local_docs = std::path::Path::new("docs/book/index.html");
                if local_docs.exists() {
                    let abs = std::fs::canonicalize(local_docs).unwrap_or_default();
                    let _ = std::process::Command::new("xdg-open").arg(format!("file://{}", abs.display())).spawn();
                } else {
                    let _ = std::process::Command::new("xdg-open").arg("https://github.com/davidkodar/klipp").spawn();
                }
                ui.close_menu();
            }
            if ui.button("Keyboard Shortcuts").clicked() {
                // Open preferences on the Keyboard tab
                state.show_preferences = true;
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Report a Bug...").clicked() {
                // Open GitHub issues page
                let _ = std::process::Command::new("xdg-open").arg("https://github.com/davidkodar/klipp/issues").spawn();
                ui.close_menu();
            }
            if ui.button("Release Notes").clicked() {
                let _ = std::process::Command::new("xdg-open").arg("https://github.com/davidkodar/klipp/releases").spawn();
                ui.close_menu();
            }
            ui.separator();
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
    if active { egui::Color32::from_rgb(0x0a, 0x0a, 0x0c) } // dark on bright bg
    else if resp.hovered() { egui::Color32::from_rgb(0xe8, 0xe8, 0xee) }
    else { egui::Color32::from_rgb(0x90, 0x90, 0xa0) }
}

fn icon_bg(resp: &egui::Response, active: bool) -> egui::Color32 {
    if active { egui::Color32::from_rgb(0xe8, 0xa0, 0x20) } // solid accent fill
    else if resp.hovered() { egui::Color32::from_rgb(0x2a, 0x2a, 0x32) }
    else { egui::Color32::TRANSPARENT }
}

fn icon_rect(btn: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(btn.center(), egui::vec2(16.0, 16.0))
}


fn sep(ui: &mut egui::Ui, theme: &KlippTheme) {
    ui.add_space(12.0);
    let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 16.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, theme.border_light);
    ui.add_space(12.0);
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
    let s = egui::Stroke::new(1.5_f32, c);
    painter.line_segment([egui::pos2(ir.min.x + 3.0, ir.min.y + 2.0), egui::pos2(ir.min.x + 3.0, ir.max.y - 2.0)], s);
    painter.line_segment([egui::pos2(ir.max.x - 3.0, ir.min.y + 2.0), egui::pos2(ir.max.x - 3.0, ir.max.y - 2.0)], s);
    painter.line_segment([egui::pos2(ir.min.x + 3.0, ir.center().y), egui::pos2(ir.min.x + 6.0, ir.center().y)], s);
    painter.line_segment([egui::pos2(ir.max.x - 6.0, ir.center().y), egui::pos2(ir.max.x - 3.0, ir.center().y)], s);
}

fn draw_blade_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_rect(btn);
    let s = egui::Stroke::new(1.5_f32, c);
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
    let s = egui::Stroke::new(1.5_f32, c);
    let center = egui::pos2(ir.min.x + 6.0, ir.min.y + 6.0);
    painter.circle_stroke(center, 5.0, s);
    painter.line_segment([egui::pos2(center.x + 3.5, center.y + 3.5), egui::pos2(ir.max.x - 2.0, ir.max.y - 2.0)], s);
}

// ═══════════════════════════════════════════
// RIGHT GROUP ICONS (from KLIPP_TOOLBAR_ICONS_FIX.md)
// ═══════════════════════════════════════════

// Kept for the future Roles & Lanes feature — see code_audit.html.
#[allow(dead_code)]
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
    painter.rect_stroke(ir, 2.0, egui::Stroke::new(1.2_f32, c));
    let pts = vec![
        egui::pos2(ir.min.x + 2.0, ir.center().y + 3.0),
        egui::pos2(ir.min.x + 5.0, ir.center().y - 4.0),
        egui::pos2(ir.min.x + 8.0, ir.center().y + 2.0),
        egui::pos2(ir.min.x + 11.0, ir.center().y - 5.0),
        egui::pos2(ir.min.x + 14.0, ir.center().y + 1.0),
    ];
    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.3_f32, c)));
}

fn draw_color_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let center = btn.center();
    let radius = 6.7;
    painter.circle_stroke(center, radius, egui::Stroke::new(1.3_f32, c));
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
    painter.line_segment([egui::pos2(cx, top + 5.0), egui::pos2(cx, bot)], egui::Stroke::new(1.5_f32, c));
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(cx, top), egui::pos2(cx - 4.0, top + 6.0), egui::pos2(cx + 4.0, top + 6.0),
    ], c, egui::Stroke::NONE));
    painter.line_segment([egui::pos2(cx - 5.0, mid + 3.0), egui::pos2(cx + 5.0, mid + 3.0)], egui::Stroke::new(1.5_f32, c));
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
    let s = egui::Stroke::new(1.5_f32, c);
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
    painter.line_segment([egui::pos2(ir.min.x + 1.0, baseline_y), egui::pos2(ir.max.x - 1.0, baseline_y)], egui::Stroke::new(1.5_f32, c));
    painter.line_segment([egui::pos2(cx, arrow_base), egui::pos2(cx, top + 5.0)], egui::Stroke::new(1.5_f32, c));
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(cx, top), egui::pos2(cx - 4.0, top + 5.5), egui::pos2(cx + 4.0, top + 5.5),
    ], c, egui::Stroke::NONE));
}

