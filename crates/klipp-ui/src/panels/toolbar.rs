use crate::prefs::RecentProject;
use crate::state::{AppState, Workspace};
use crate::theme::KlippTheme;
use crate::widgets::tooltip::styled_tooltip;
use klipp_core::AppCommand;

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
        kb: &crate::keybindings::KeyBindings,
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
        rx -= 28.0;
        let resp = alloc_btn(ui, egui::pos2(rx, cy - 14.0), 28.0);
        let ic = icon_color(&resp, false, theme);
        draw_export_icon(&painter, resp.rect, ic);
        styled_tooltip(&resp, theme, "Export / Share", Some(&kb.display_for("export")));
        if resp.clicked() { cmds.push(AppCommand::ShowExportDialog); }

        rx -= 2.0;

        // Render Queue (icon 5)
        rx -= 28.0;
        let resp = alloc_btn(ui, egui::pos2(rx, cy - 14.0), 28.0);
        let ic = icon_color(&resp, false, theme);
        draw_render_queue_icon(&painter, resp.rect, ic);
        styled_tooltip(&resp, theme, "Render Queue", None);
        if resp.clicked() { cmds.push(AppCommand::ShowRenderQueue); }

        if matches!(state.active_workspace, Workspace::Edit | Workspace::Color) {
            rx -= 96.0;
            let rect = egui::Rect::from_min_size(egui::pos2(rx, cy - 14.0), egui::vec2(92.0, 28.0));
            // "Nodes", the name the Colour page uses for the same graphs, lit while
            // the dock is open (#1726).
            if ui.put(rect, egui::Button::new("Nodes").selected(state.graph_dock_open)).on_hover_text("The clip's effects and grade as connected nodes").clicked() {
                cmds.push(AppCommand::ToggleGraphDock);
            }
        }

        let right_edge = rx - 10.0;

        // ═══════════════════════════════════════════
        // LEFT GROUP — brand + menus + tool buttons
        // ═══════════════════════════════════════════
        // Drawn first, up to the right group: where it ends is where the
        // workspace tabs may start (#1355).
        let left_rect = egui::Rect::from_min_max(toolbar_rect.min, egui::pos2(right_edge.max(toolbar_rect.min.x), toolbar_rect.max.y));
        let left_end = ui.allocate_new_ui(egui::UiBuilder::new().max_rect(left_rect), |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.add_space(10.0);

                // Brand: logo icon + "Klipp"
                let logo_tex = self.logo_texture.get_or_insert_with(|| {
                    let png_bytes = include_bytes!("../../../../assets/branding/klipp-refined/klipp-icon-64.png");
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
                ui.label(egui::RichText::new("Klipp").size(crate::theme::type_scale::TITLE).strong().color(theme.text_primary));
                ui.add_space(12.0);

                // Separator
                sep(ui, theme);

                // The menu bar: plain titles on the bar, a hover tone,
                // no button frames.
                {
                    let v = ui.visuals_mut();
                    v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
                    v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
                    v.widgets.hovered.weak_bg_fill = theme.bg_hover;
                    v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
                    v.widgets.active.weak_bg_fill = theme.bg_hover;
                    v.widgets.active.bg_stroke = egui::Stroke::NONE;
                    v.widgets.open.weak_bg_fill = theme.bg_hover;
                    v.widgets.open.bg_stroke = egui::Stroke::NONE;
                }
                Self::file_menu(ui, &mut cmds, state, recent_projects, kb);
                Self::edit_menu(ui, &mut cmds, state, kb);
                Self::clip_menu(ui, &mut cmds, state, kb);
                Self::timeline_menu(ui, &mut cmds, state, kb);
                Self::mark_menu(ui, &mut cmds, state, kb);
                Self::view_menu(ui, &mut cmds, state, kb);
                Self::window_menu(ui, &mut cmds, state, kb);
                Self::tools_menu(ui, &mut cmds, state);
                Self::help_menu(ui, state);
                ui.min_rect().right()
            })
            .inner
        })
        .inner;


        // ═══════════════════════════════════════════
        // WORKSPACE TABS — centred in toolbar, where the menus leave room
        // ═══════════════════════════════════════════
        // A segmented control: three labels in one field, the active one
        // lit, sized to its titles so none is ever clipped.
        let workspaces = [(Workspace::Edit, "Edit"), (Workspace::Color, "Colour"), (Workspace::Export, "Export")];
        let ws_font = egui::FontId::proportional(crate::theme::type_scale::CONTROL);
        let labels: Vec<&str> = workspaces.iter().map(|w| w.1).collect();
        let ws_size = crate::widgets::segmented::segmented_size(ui, &labels, &ws_font, crate::theme::control::REGULAR);
        let active = workspaces.iter().position(|w| w.0 == state.active_workspace).unwrap_or(0);
        let picked = match switcher_place(toolbar_rect, left_end + 10.0, right_edge, ws_size) {
            SwitcherPlace::Tabs(ws_rect) => crate::widgets::segmented::segmented_in(ui, ws_rect, &labels, active, theme, ws_font),
            // Too narrow for the three: the one shown, the others a click away.
            SwitcherPlace::Menu(rect) => {
                let mut chosen = active;
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ui.centered_and_justified(|ui| {
                        egui::ComboBox::from_id_salt("workspace_switcher").selected_text(labels[active]).width(rect.width()).show_ui(ui, |ui| {
                            for (i, label) in labels.iter().enumerate() {
                                ui.selectable_value(&mut chosen, i, *label);
                            }
                        });
                    });
                });
                (chosen != active).then_some(chosen)
            }
            SwitcherPlace::None => None,
        };
        if let Some(i) = picked {
            // Just the workspace. What it implies — the colour panel,
            // the browser borrow — is settled in one place by the app's
            // `reconcile_workspace`, so this tab and the `C` shortcut
            // can no longer give two different layouts for one switch.
            //
            // Scopes are deliberately not forced on: that decision was
            // already taken for the shortcut route, and this one had
            // quietly kept doing it.
            state.active_workspace = workspaces[i].0;
        }

        cmds
    }

    // ══════════════════════════════════════════════
    // Menu bar: File | Edit | Clip | Timeline | Mark | View | Window | Help
    // ══════════════════════════════════════════════

    fn file_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState, recent: &[RecentProject], kb: &crate::keybindings::KeyBindings) {
        ui.menu_button(egui::RichText::new("File").size(crate::theme::type_scale::CONTROL), |ui| {
            if ui.add(egui::Button::new("New Project...").shortcut_text(kb.display_for("new_project"))).clicked() { cmds.push(AppCommand::NewProject); ui.close_menu(); }
            if ui.add(egui::Button::new("Open Project...").shortcut_text(kb.display_for("open_project"))).clicked() {
                ui.close_menu();
                crate::file_dialog::spawn(crate::project_paths::pick_project_commands);
            }
            if ui.button("Open Project from Another Editor...").on_hover_text(klipp_project::import_report::READS).clicked() {
                ui.close_menu();
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .set_title("Open Project from Another Editor")
                        .add_filter("Timelines", &["fcpxml", "xml", "otio", "kdenlive", "mlt", "edl", "aaf"])
                        .add_filter("Any file", &["*"])
                        .pick_file()
                        .map(|path| vec![AppCommand::OpenFromAnotherEditor { path, request_id: uuid::Uuid::nil() }])
                        .unwrap_or_default()
                });
            }
            if !recent.is_empty() {
                ui.menu_button("Open Recent", |ui| {
                    for rp in recent {
                        let label = rp.name.clone();
                        if ui.button(&label).clicked() { cmds.push(AppCommand::OpenProject { path: rp.path.clone() }); ui.close_menu(); }
                    }
                });
            }
            ui.separator();
            if ui.add(egui::Button::new("Save").shortcut_text(kb.display_for("save_project"))).clicked() { cmds.push(AppCommand::SaveProject); ui.close_menu(); }
            if ui.add(egui::Button::new("Save As...").shortcut_text(kb.display_for("save_project_as"))).clicked() {
                ui.close_menu();
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .set_title("Save As")
                        .add_filter("Klipp Project", &["klipp"])
                        .set_file_name("project.klipp")
                        .save_file()
                        .map(|p| vec![AppCommand::SaveProjectAs { path: p }])
                        .unwrap_or_default()
                });
            }
            // The project's structure kept to start new ones from (#618).
            if ui.add_enabled(state.project_path.is_some(), egui::Button::new("Save Project as Template...")).clicked() {
                state.save_template_dialog = Some(Default::default());
                ui.close_menu();
            }
            if ui.button("Revert to Saved").clicked() {
                if let Some(path) = state.project_path.clone() { cmds.push(AppCommand::OpenProject { path }); }
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Import Media...").shortcut_text(kb.display_for("import_media"))).clicked() {
                ui.close_menu();
                crate::file_dialog::spawn(crate::command_palette::import_media_commands);
            }
            ui.separator();
            if ui.button("New Bin").clicked() {
                cmds.push(AppCommand::NewBin { media_ids: Vec::new() });
                ui.close_menu();
            }
            let selected: Vec<klipp_core::MediaId> = state.selected_media_ids.iter().copied().collect();
            if ui.add_enabled(!selected.is_empty(), egui::Button::new("New Bin from Selection")).on_disabled_hover_text("Select clips in the media pool first.").clicked() {
                cmds.push(AppCommand::NewBin { media_ids: selected });
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Import Subtitles...").clicked() {
                ui.close_menu();
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .set_title("Import Subtitles")
                        .add_filter("Subtitles", &["srt","vtt"])
                        .pick_file()
                        .map(|path| vec![AppCommand::ImportSubtitles { path }])
                        .unwrap_or_default()
                });
            }
            if ui.button("Import Metadata Sheet...").on_hover_text("A CSV or ALE: scene, shot, take, description, keywords, rating and the rest, matched to clips by file name.").clicked() {
                ui.close_menu();
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .set_title("Import Metadata Sheet")
                        .add_filter("Metadata sheet", &["csv", "ale", "txt"])
                        .pick_file()
                        .map(|path| vec![AppCommand::ImportMetadataSheet { path }])
                        .unwrap_or_default()
                });
            }
            // Timeline-format imports (foreign NLE projects → a new Sequence).
            // Grouped under a submenu so adding formats (OTIO, FCPXML, EDL, AAF)
            // doesn't clutter the top level.
            ui.menu_button("Import Timeline", |ui| {
                if ui.button("Kdenlive Project...").clicked() {
                    ui.close_menu();
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Import Kdenlive Project")
                            .add_filter("Kdenlive Project", &["kdenlive", "mlt"])
                            .pick_file()
                            .map(|path| vec![AppCommand::ImportKdenlive { path }])
                            .unwrap_or_default()
                    });
                }
                if ui.button("EDL (.edl)...").clicked() {
                    ui.close_menu();
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Import EDL")
                            .add_filter("CMX 3600 EDL", &["edl"])
                            .pick_file()
                            .map(|path| vec![AppCommand::ImportEdl { path }])
                            .unwrap_or_default()
                    });
                }
                if ui.button("OpenTimelineIO (.otio)...").clicked() {
                    ui.close_menu();
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Import OpenTimelineIO")
                            .add_filter("OpenTimelineIO", &["otio"])
                            .pick_file()
                            .map(|path| vec![AppCommand::ImportOtio { path }])
                            .unwrap_or_default()
                    });
                }
                if ui.button("Final Cut Pro XML (.fcpxml)...").clicked() {
                    ui.close_menu();
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Import FCPXML Project")
                            .add_filter("Final Cut Pro XML", &["fcpxml", "xml"])
                            .pick_file()
                            .map(|path| vec![AppCommand::ImportFcpxml { path }])
                            .unwrap_or_default()
                    });
                }
            });
            ui.separator();
            if ui.add(egui::Button::new("Export...").shortcut_text(kb.display_for("export"))).clicked() { cmds.push(AppCommand::ShowExportDialog); ui.close_menu(); }
            if ui.button("Export Subtitles...").clicked() {
                ui.close_menu();
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .set_title("Export Subtitles")
                        .add_filter("SRT", &["srt"])
                        .add_filter("WebVTT", &["vtt"])
                        .add_filter("ASS", &["ass"])
                        .add_filter("SSA", &["ssa"])
                        .add_filter("TTML (IMSC)", &["ttml"])
                        .add_filter("SCC (CEA-608)", &["scc"])
                        .add_filter("EBU-STL", &["stl"])
                        .set_file_name("subtitles.srt")
                        .save_file()
                        .map(|path| vec![AppCommand::ExportSubtitles { path }])
                        .unwrap_or_default()
                });
            }
            if ui.button("YouTube Chapters\u{2026}").clicked() {
                ui.close_menu();
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .set_title("Export YouTube Chapters")
                        .add_filter("Text file", &["txt"])
                        .set_file_name("chapters.txt")
                        .save_file()
                        .map(|path| vec![AppCommand::ExportYouTubeChapters { path }])
                        .unwrap_or_default()
                });
            }
            if ui.button("Generate Render Script\u{2026}").clicked() {
                ui.close_menu();
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .set_title("Save Render Script")
                        .add_filter("Shell script", &["sh"])
                        .set_file_name("render.sh")
                        .save_file()
                        .map(|path| vec![AppCommand::GenerateRenderScript { path }])
                        .unwrap_or_default()
                });
            }
            // Timeline-format exports (current Sequence → interchange format).
            ui.menu_button("Export Timeline", |ui| {
                if ui.button("FCPXML...").clicked() {
                    ui.close_menu();
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Export FCPXML")
                            .add_filter("FCPXML", &["fcpxml"])
                            .set_file_name("export.fcpxml")
                            .save_file()
                            .map(|path| vec![AppCommand::ExportFcpxml { path }])
                            .unwrap_or_default()
                    });
                }
                if ui.button("Kdenlive Project...").clicked() {
                    ui.close_menu();
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Export as Kdenlive Project")
                            .add_filter("Kdenlive project", &["kdenlive"])
                            .set_file_name("project.kdenlive")
                            .save_file()
                            .map(|path| vec![AppCommand::ExportKdenlive { path }])
                            .unwrap_or_default()
                    });
                }
                if ui.button("EDL...").clicked() {
                    ui.close_menu();
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Export EDL")
                            .add_filter("EDL", &["edl"])
                            .set_file_name("export.edl")
                            .save_file()
                            .map(|path| vec![AppCommand::ExportEdl { path }])
                            .unwrap_or_default()
                    });
                }
                // OpenTimelineIO, here with the other formats (#925); it was
                // only in the command palette.
                if ui.button("OpenTimelineIO...").clicked() {
                    ui.close_menu();
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Export OpenTimelineIO")
                            .add_filter("OpenTimelineIO", &["otio"])
                            .set_file_name("export.otio")
                            .save_file()
                            .map(|path| vec![AppCommand::ExportOtio { path }])
                            .unwrap_or_default()
                    });
                }
                // AAF through OpenTimelineIO's converter, where installed (#89);
                // greyed, saying what to install, where it is not.
                let missing = match klipp_project::otio_export::aaf_support() {
                    klipp_project::otio_export::AafSupport::Missing(what) => Some(what),
                    _ => None,
                };
                let aaf = ui.add_enabled(missing.is_none(), egui::Button::new("AAF (via OpenTimelineIO)..."));
                let aaf = match missing {
                    Some(what) => aaf.on_disabled_hover_text(format!("Needs {what}: {}", klipp_project::otio_export::AAF_INSTALL)),
                    None => aaf,
                };
                if aaf.clicked() {
                    ui.close_menu();
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Export AAF")
                            .add_filter("AAF", &["aaf"])
                            .set_file_name("export.aaf")
                            .save_file()
                            .map(|path| vec![AppCommand::ExportAaf { path }])
                            .unwrap_or_default()
                    });
                }
            });
            ui.menu_button("Export Metadata Sheet", |ui| {
                if ui.button("CSV...").clicked() {
                    ui.close_menu();
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Export Metadata as CSV")
                            .add_filter("CSV", &["csv"])
                            .set_file_name("metadata.csv")
                            .save_file()
                            .map(|path| vec![AppCommand::ExportMetadataSheet { path, ale: false }])
                            .unwrap_or_default()
                    });
                }
                if ui.button("ALE (Avid Log Exchange)...").clicked() {
                    ui.close_menu();
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Export Metadata as ALE")
                            .add_filter("ALE", &["ale"])
                            .set_file_name("metadata.ale")
                            .save_file()
                            .map(|path| vec![AppCommand::ExportMetadataSheet { path, ale: true }])
                            .unwrap_or_default()
                    });
                }
            });
            ui.separator();
            if ui.add(egui::Button::new("Close Project").shortcut_text(kb.display_for("close_project"))).clicked() { cmds.push(AppCommand::CloseProject); ui.close_menu(); }
            if ui.button("Project Properties...").clicked() { state.show_project_info = true; ui.close_menu(); }
            if ui.button("Colour Management\u{2026}").clicked() { state.show_colour_management = true; ui.close_menu(); }
            if ui.button("Sequence Settings...").clicked() {
                cmds.push(AppCommand::OpenSequenceSettings { sequence_idx: state.active_sequence_idx });
                ui.close_menu();
            }
            if ui.add_enabled(state.last_import_report.is_some(), egui::Button::new("Last Import Report…")).clicked() { state.show_import_report = true; ui.close_menu(); }
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
            if ui.add(egui::Button::new("Quit").shortcut_text(kb.display_for("quit"))).clicked() { cmds.push(AppCommand::Quit); ui.close_menu(); }
        });
    }

    fn edit_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState, kb: &crate::keybindings::KeyBindings) {
        ui.menu_button(egui::RichText::new("Edit").size(crate::theme::type_scale::CONTROL), |ui| {
            if ui.add_enabled(state.next_undo().is_some(), egui::Button::new(state.undo_label().map(|d| format!("Undo {d}")).unwrap_or("Undo".into())).shortcut_text(kb.display_for("undo"))).clicked() { cmds.push(AppCommand::Undo); ui.close_menu(); }
            if ui.add_enabled(state.next_redo().is_some(), egui::Button::new(state.redo_label().map(|d| format!("Redo {d}")).unwrap_or("Redo".into())).shortcut_text(kb.display_for("redo"))).clicked() { cmds.push(AppCommand::Redo); ui.close_menu(); }
            if ui.button("History...").clicked() { state.show_history_panel = true; ui.close_menu(); }
            if ui.button("Find and Replace in Titles\u{2026}").on_hover_text("Find words across every title, and replace them in one step").clicked() { state.show_title_find = true; ui.close_menu(); }
            ui.separator();
            if ui.add_enabled(state.selected_clip.is_some(), egui::Button::new("Cut").shortcut_text(kb.display_for("cut_clip"))).clicked() {
                if let Some(clip_id) = state.selected_clip {
                    cmds.push(AppCommand::CopyClip { clip_id });
                    cmds.push(AppCommand::RemoveClip { clip_id });
                }
                ui.close_menu();
            }
            if ui.add_enabled(state.selected_clip.is_some(), egui::Button::new("Copy").shortcut_text(kb.display_for("copy_clip"))).clicked() {
                if let Some(clip_id) = state.selected_clip { cmds.push(AppCommand::CopyClip { clip_id }); }
                ui.close_menu();
            }
            if ui.add_enabled(state.clipboard.is_some(), egui::Button::new("Paste").shortcut_text(kb.display_for("paste_clip"))).clicked() {
                cmds.push(AppCommand::PasteClip);
                ui.close_menu();
            }
            if ui.add_enabled(state.clipboard.is_some(), egui::Button::new("Paste Insert")).on_hover_text("Cut every unlocked track at the playhead and move what follows along by the paste's length").clicked() {
                cmds.push(AppCommand::PasteClipInsert);
                ui.close_menu();
            }
            // A title's look (#1718).
            let title_selected = state.selected_clip.is_some_and(|id| state.sequence().all_clips().any(|c| c.id == id && c.title_data().is_some()));
            if ui.add_enabled(title_selected, egui::Button::new("Copy Title Style")).clicked() {
                if let Some(clip_id) = state.selected_clip { cmds.push(AppCommand::CopyTitleStyle { clip_id }); }
                ui.close_menu();
            }
            if ui.add_enabled(title_selected && state.title_style_clipboard.is_some(), egui::Button::new("Paste Title Style")).on_hover_text("Give the selected titles the copied look, each keeping its words").clicked() {
                let to = if state.selected_clips.is_empty() { state.selected_clip.into_iter().collect() } else { state.selected_clips.clone() };
                cmds.push(AppCommand::PasteTitleStyle { from: None, to });
                ui.close_menu();
            }
            if ui.add_enabled(state.selected_clip.is_some(), egui::Button::new("Copy Attributes").shortcut_text(kb.display_for("copy_attributes"))).clicked() {
                if let Some(cid) = state.selected_clip { cmds.push(AppCommand::CopyAttributes { clip_id: cid }); }
                ui.close_menu();
            }
            if ui.add_enabled(state.attributes_clipboard.is_some() && state.selected_clip.is_some(), egui::Button::new("Paste Attributes...").shortcut_text(kb.display_for("paste_attributes")))
                .on_disabled_hover_text(if state.attributes_clipboard.is_none() { "Copy a clip or its attributes first" } else { "Select the clips to paste onto" })
                .clicked()
            {
                state.show_paste_attributes_dialog = true; ui.close_menu();
            }
            if ui.add_enabled(state.selected_clip.is_some(), egui::Button::new("Duplicate").shortcut_text(kb.display_for("duplicate_clip"))).clicked() {
                if let Some(clip_id) = state.selected_clip { cmds.push(AppCommand::DuplicateClip { clip_id }); }
                ui.close_menu();
            }
            if ui.add_enabled(state.selected_clip.is_some(), egui::Button::new("Delete").shortcut_text(kb.display_for("delete_clip"))).clicked() {
                if let Some(clip_id) = state.selected_clip { cmds.push(AppCommand::RemoveClip { clip_id }); }
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Select All").shortcut_text(kb.display_for("select_all"))).clicked() { cmds.push(AppCommand::SelectAllClips); ui.close_menu(); }
            // No default key (each would take one the other actions use);
            // bindable in Preferences, so no shortcut text here.
            if ui.button("Select Forward").on_hover_text("Every clip starting at or after the playhead, on every unlocked track").clicked() { cmds.push(AppCommand::SelectClipsForward); ui.close_menu(); }
            if ui.button("Select Backward").on_hover_text("Every clip ending at or before the playhead, on every unlocked track").clicked() { cmds.push(AppCommand::SelectClipsBackward); ui.close_menu(); }
            // By what the clips are (#925).
            let any = state.selected_clip.is_some() || !state.selected_clips.is_empty();
            if ui.button("Invert Selection").on_hover_text("Every clip not selected now").clicked() { cmds.push(AppCommand::SelectInverse); ui.close_menu(); }
            if ui.add_enabled(any, egui::Button::new("Select Same Source")).on_hover_text("Every clip from the selected clips' media files").clicked() { cmds.push(AppCommand::SelectSameSource); ui.close_menu(); }
            ui.menu_button("Select by Colour", |ui| {
                for label in klipp_media::ColorLabel::ALL.into_iter().filter(|l| l.to_u8() != 0) {
                    let swatch = label.rgb().map(|(r, g, b)| egui::Color32::from_rgb(r, g, b)).unwrap_or(ui.visuals().weak_text_color());
                    if ui.button(egui::RichText::new(format!("\u{25CF} {}", label.label())).color(swatch)).clicked() {
                        cmds.push(AppCommand::SelectByColour { label: label.to_u8() });
                        ui.close_menu();
                    }
                }
            });
            if ui.button("Go to Next Gap").on_hover_text("The playhead to the next empty stretch on an unlocked track").clicked() { cmds.push(AppCommand::GoToNextGap); ui.close_menu(); }
            // The selected clip's track (#844); a track's own header menu
            // has the same three for any track.
            let on_track = state.selected_clip.is_some();
            for (label, action, range) in [
                ("Select All on Track", "select_track_clips", klipp_core::SelectRange::All),
                ("Select Forward on Track", "select_track_forward", klipp_core::SelectRange::Forward),
                ("Select Backward on Track", "select_track_backward", klipp_core::SelectRange::Backward),
            ] {
                if ui.add_enabled(on_track, egui::Button::new(label).shortcut_text(kb.display_for(action))).on_disabled_hover_text("Select a clip on the track first, or use the track header's menu").clicked() {
                    cmds.push(AppCommand::SelectClipsOnSelectedTrack { range });
                    ui.close_menu();
                }
            }
            if ui.add(egui::Button::new("Deselect All").shortcut_text(kb.display_for("deselect_all"))).clicked() { { state.selected_clip = None; state.selected_clips.clear(); }; ui.close_menu(); }
            ui.separator();
            if ui.add(egui::Button::new("Preferences...").shortcut_text(kb.display_for("preferences"))).clicked() { cmds.push(AppCommand::ShowPreferences); ui.close_menu(); }
        });
    }

    fn clip_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState, kb: &crate::keybindings::KeyBindings) {
        ui.menu_button(egui::RichText::new("Clip").size(crate::theme::type_scale::CONTROL), |ui| {
            let has_clip = state.selected_clip.is_some();
            if ui.add_enabled(has_clip, egui::Button::new("Enable / Disable")).clicked() {
                if let Some(cid) = state.selected_clip {
                    let enabled = state.sequence().all_clips().find(|c| c.id == cid).is_none_or(|c| c.enabled);
                    cmds.push(AppCommand::SetClipEnabled { clip_id: cid, enabled: !enabled });
                }
                ui.close_menu();
            }
            if ui.add_enabled(has_clip, egui::Button::new("Solo")).clicked() {
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
            if ui.add(egui::Button::new("Match Frame").shortcut_text(kb.display_for("match_frame_program_to_source"))).clicked() {
                cmds.push(AppCommand::MatchFrameProgramToSource);
                ui.close_menu();
            }
            let has_source = state.source_media_id.is_some();
            if ui.add_enabled(has_source,
                egui::Button::new("Match Frame Reverse").shortcut_text(kb.display_for("match_frame_source_to_program")),
            ).clicked() {
                cmds.push(AppCommand::MatchFrameSourceToProgram);
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Split at Playhead").shortcut_text(kb.display_for("split_at_playhead"))).clicked() {
                cmds.extend(state.split_at_playhead_commands());
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
            // Delete's key, which is what this runs; it showed the lift key.
            if ui.add_enabled(has_clip, egui::Button::new("Ripple Delete").shortcut_text(kb.display_for("delete_clip"))).clicked() {
                if let Some(clip_id) = state.selected_clip { cmds.push(AppCommand::RemoveClip { clip_id }); }
                ui.close_menu();
            }
            ui.separator();
            if ui.add_enabled(has_clip, egui::Button::new("Speed...")).clicked() {
                if let Some(clip_id) = state.selected_clip {
                    cmds.push(AppCommand::OpenSpeedDialog { clip_id });
                }
                ui.close_menu();
            }
            ui.menu_button("Motion Presets", |ui| {
                for (label, preset) in [
                    ("Ken Burns: Zoom In", "ken_burns_zoom_in"),
                    ("Ken Burns: Zoom Out", "ken_burns_zoom_out"),
                    ("Ken Burns: Pan Left", "ken_burns_pan_left"),
                ] {
                    if let Some(cid) = state.selected_clip {
                        if ui.button(label).clicked() { cmds.push(AppCommand::ApplyMotionPreset { clip_id: cid, preset: preset.into() }); ui.close_menu(); }
                    }
                }
                ui.separator();
                for (label, preset) in [
                    ("PiP: Top Left", "pip_top_left"),
                    ("PiP: Top Right", "pip_top_right"),
                    ("PiP: Bottom Left", "pip_bottom_left"),
                    ("PiP: Bottom Right", "pip_bottom_right"),
                ] {
                    if let Some(cid) = state.selected_clip {
                        if ui.button(label).clicked() { cmds.push(AppCommand::ApplyMotionPreset { clip_id: cid, preset: preset.into() }); ui.close_menu(); }
                    }
                }
            });
            ui.separator();
            if ui.add_enabled(has_clip, egui::Button::new("Copy Attributes").shortcut_text(kb.display_for("copy_attributes"))).clicked() {
                if let Some(cid) = state.selected_clip { cmds.push(AppCommand::CopyAttributes { clip_id: cid }); }
                ui.close_menu();
            }
            if ui.add_enabled(state.attributes_clipboard.is_some() && has_clip, egui::Button::new("Paste Attributes...").shortcut_text(kb.display_for("paste_attributes"))).clicked() {
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
                            if ui.add(egui::Button::new(&label).shortcut_text(kb.display_for(&format!("multicam_angle_{angle}")))).clicked() {
                                cmds.push(AppCommand::SwitchMulticamAngle { angle });
                                ui.close_menu();
                            }
                        }
                    });
                    // Whose sound the group plays: the angles' sounds
                    // together are one moment heard several times.
                    if let Some((angles, current)) = state.sequence().multicam_at(None, state.playhead).map(|m| (m.angles, m.primary.multicam_sound)) {
                        ui.menu_button("Multicam Sound", |ui| {
                            let choices = angles
                                .iter()
                                .map(|a| klipp_core::MulticamSound::Angle(*a))
                                .chain([klipp_core::MulticamSound::FollowPicture, klipp_core::MulticamSound::Off]);
                            for sound in choices {
                                if ui.radio(sound == current, sound.label()).clicked() {
                                    cmds.push(AppCommand::SetMulticamSound { sound, at: None });
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                }
            }
            if has_clip && ui.button("Normalize Audio\u{2026}").on_hover_text("Bring the selected clips to a loudness or peak target by their gain").clicked() {
                cmds.push(AppCommand::OpenNormalizeDialog { clip_id: None });
                ui.close_menu();
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
                if has_tracks
                    && ui.button("Analyze Track Points").clicked() {
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
            if ui.add(egui::Button::new("Open in Colour Panel").shortcut_text(kb.display_for("toggle_color_panel"))).clicked() {
                state.show_color_panel = !state.show_color_panel;
                ui.close_menu();
            }
        });
    }

    fn timeline_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState, kb: &crate::keybindings::KeyBindings) {
        ui.menu_button(egui::RichText::new("Timeline").size(crate::theme::type_scale::CONTROL), |ui| {
            if ui.button("Add Video Track").clicked() { cmds.push(AppCommand::AddVideoTrack); ui.close_menu(); }
            if ui.button("Add Audio Track").clicked() { cmds.push(AppCommand::AddAudioTrack); ui.close_menu(); }
            // The selected clip's track; without one there is nothing to
            // say which, so the item waits for a selection (#911).
            let delete_track = ui.add_enabled(state.selected_clip.is_some(), egui::Button::new("Delete Track"));
            let delete_track = delete_track.on_disabled_hover_text("Select a clip on the track to delete it, or right-click the track's header");
            if delete_track.clicked() {
                if let Some(clip_id) = state.selected_clip {
                    if let Some(track) = state.sequence().tracks.iter().find(|t| t.clips.iter().any(|c| c.id == clip_id)) {
                        cmds.push(AppCommand::RemoveTrack { track_id: track.id });
                    }
                }
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Insert Edit").shortcut_text(kb.display_for("insert_edit"))).clicked() { cmds.push(AppCommand::InsertEdit); ui.close_menu(); }
            if ui.add(egui::Button::new("Overwrite Edit").shortcut_text(kb.display_for("overwrite_edit"))).clicked() { cmds.push(AppCommand::OverwriteEdit); ui.close_menu(); }
            if ui.add(egui::Button::new("Fit to Fill").shortcut_text(kb.display_for("fit_to_fill_edit"))).clicked() { cmds.push(AppCommand::FitToFillEdit); ui.close_menu(); }
            if ui.add(egui::Button::new("Place on Top").shortcut_text(kb.display_for("place_on_top_edit"))).clicked() { cmds.push(AppCommand::PlaceOnTopEdit); ui.close_menu(); }
            if ui.add(egui::Button::new("Replace Edit").shortcut_text(kb.display_for("replace_edit"))).clicked() { cmds.push(AppCommand::ReplaceEdit); ui.close_menu(); }
            if ui.button("Ripple Overwrite").on_hover_text("Replace the clip under the playhead with the source's marked range; what follows moves by the change in length").clicked() { cmds.push(AppCommand::RippleOverwriteEdit); ui.close_menu(); }
            if ui.button("Lift Edit").clicked() { cmds.push(AppCommand::LiftEdit); ui.close_menu(); }
            if ui.button("Extract Edit").clicked() { cmds.push(AppCommand::ExtractEdit); ui.close_menu(); }
            if ui.add(egui::Button::new("Freeze Frame").shortcut_text(kb.display_for("freeze_frame"))).clicked() {
                cmds.push(AppCommand::InsertFreezeFrame { duration_secs: 2.0 });
                ui.close_menu();
            }
            let joinable = state.through_edit_at_playhead();
            let item = ui.add_enabled(joinable.is_some(), egui::Button::new("Join Through Edit at Playhead"));
            let item = if joinable.is_some() {
                item.on_hover_text("Close the cut under the playhead: the two pieces of the same shot either side of it made one clip again, their sound with them")
            } else {
                item.on_disabled_hover_text("Put the playhead on a cut between two pieces of the same shot (same file and speed, its source running on), on the selected clip's track")
            };
            if item.clicked() {
                if let Some(clip_id) = joinable {
                    cmds.push(AppCommand::JoinThroughEdit { clip_id, next: true });
                }
                ui.close_menu();
            }
            ui.separator();
            // Render In to Out, or the whole timeline without them, and
            // Render Selection (#924): graded clips rendered into the cache
            // in the background while paused; its progress and Cancel here.
            let (seq_in, seq_out) = (state.sequence().in_point, state.sequence().out_point);
            if let Some((done, total)) = state.render_range {
                ui.horizontal(|ui| {
                    ui.label(format!("Rendering\u{2026} {done} of {total} frames"));
                    if ui.small_button("Cancel").clicked() {
                        cmds.push(AppCommand::CancelRenderRange);
                        ui.close_menu();
                    }
                });
            } else {
                let label = if seq_in.is_some() || seq_out.is_some() { "Render In to Out" } else { "Render Timeline" };
                if ui.button(label).on_hover_text("Render graded clips into the render cache, so they play from it").clicked() {
                    let from = seq_in.unwrap_or(klipp_core::Timecode { value: 0, rate: state.fps.rate() });
                    let to = seq_out.unwrap_or_else(|| state.sequence_duration());
                    cmds.push(AppCommand::RenderRange { from, to });
                    ui.close_menu();
                }
                let span = selection_span(state);
                if ui.add_enabled(span.is_some(), egui::Button::new("Render Selection")).on_disabled_hover_text("Select the clips to render").clicked() {
                    if let Some((from, to)) = span {
                        cmds.push(AppCommand::RenderRange { from, to });
                    }
                    ui.close_menu();
                }
            }
            // The render cache for In to Out, or the whole timeline without
            // them (#924): rendered frames dropped, to render again.
            let label = if seq_in.is_some() || seq_out.is_some() { "Clear Render Cache In to Out" } else { "Clear Render Cache" };
            if ui.button(label).on_hover_text("Drop the rendered frames there; graded clips render again as they play").clicked() {
                let from = seq_in.unwrap_or(klipp_core::Timecode { value: 0, rate: state.fps.rate() });
                let to = seq_out.unwrap_or_else(|| state.sequence_duration());
                cmds.push(AppCommand::ClearRenderCache { from, to });
                ui.close_menu();
            }
            ui.separator();
            // Detect Scene Cuts (#616): the selected clip split at its
            // picture changes in one step; its progress and Cancel here
            // while it runs.
            let selected = state.selected_clip;
            match selected {
                Some(clip_id) if state.scene_detect_in_progress.contains(&clip_id) => {
                    let pct = (state.scene_detect_progress.get(&clip_id).copied().unwrap_or(0.0) * 100.0) as u32;
                    ui.horizontal(|ui| {
                        ui.label(format!("Finding scene cuts\u{2026} {pct}%"));
                        if ui.small_button("Cancel").clicked() {
                            cmds.push(AppCommand::CancelSceneDetect { clip_id });
                            ui.close_menu();
                        }
                    });
                }
                _ => {
                    let refusal = match selected {
                        Some(id) => state.scene_cut_refusal(id),
                        None => Some("select a video clip first"),
                    };
                    let item = ui.add_enabled(refusal.is_none(), egui::Button::new("Detect Scene Cuts"));
                    let item = match refusal {
                        Some(why) => item.on_disabled_hover_text(format!("Splits the selected clip where its picture cuts: {why}")),
                        None => item.on_hover_text("Find where the picture cuts in the selected clip and split it there, its sound with it, in one undoable step"),
                    };
                    if item.clicked() {
                        if let Some(clip_id) = selected {
                            cmds.push(AppCommand::DetectSceneCuts { clip_id });
                        }
                        ui.close_menu();
                    }
                }
            }
            ui.separator();
            if ui.button("Add Adjustment Layer").clicked() { cmds.push(AppCommand::CreateAdjustmentLayer); ui.close_menu(); }
            ui.menu_button("Add Generator", |ui| {
                if ui.button("Solid Colour").clicked() {
                    cmds.push(AppCommand::CreateGeneratorClip { track_index: None, data: klipp_core::GeneratorData::SolidColor { color: [128, 128, 128, 255] } });
                    ui.close_menu();
                }
                if ui.button("Colour Bars").clicked() {
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
            let snap_check = if state.snap.master { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{snap_check}Snapping")).shortcut_text(kb.display_for("toggle_snap"))).clicked() { cmds.push(AppCommand::SetSnapMaster(!state.snap.master)); ui.close_menu(); }
            let linked_check = if state.linked_selection { "\u{2714} " } else { "  " };
            if ui.button(format!("{linked_check}Linked Selection")).on_hover_text("Picture and sound trim and move together").clicked() { state.linked_selection = !state.linked_selection; ui.close_menu(); }
            if ui.add(egui::Button::new("Fit to Window").shortcut_text(kb.display_for("fit_to_window"))).clicked() { cmds.push(AppCommand::FitTimelineToWindow); ui.close_menu(); }
            if ui.add_enabled(state.selected_clip.is_some() || !state.selected_clips.is_empty(), egui::Button::new("Zoom to Selection").shortcut_text(kb.display_for("zoom_to_selection"))).clicked() { cmds.push(AppCommand::ZoomToSelection); ui.close_menu(); }
            ui.separator();
            if ui.add_enabled(!state.compound_clip_stack.is_empty(), egui::Button::new("Leave Compound Clip").shortcut_text(kb.display_for("leave_compound"))).clicked() {
                cmds.push(AppCommand::CloseCompoundClip);
                ui.close_menu();
            }
        });
    }

    fn mark_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState, kb: &crate::keybindings::KeyBindings) {
        ui.menu_button(egui::RichText::new("Mark").size(crate::theme::type_scale::CONTROL), |ui| {
            if ui.add(egui::Button::new("Set In Point").shortcut_text(kb.display_for("set_in_point"))).clicked() { cmds.push(AppCommand::SetSequenceInPoint(state.playhead)); ui.close_menu(); }
            if ui.add(egui::Button::new("Set Out Point").shortcut_text(kb.display_for("set_out_point"))).clicked() { cmds.push(AppCommand::SetSequenceOutPoint(state.playhead)); ui.close_menu(); }
            if ui.add(egui::Button::new("Clear In Point").shortcut_text(kb.display_for("clear_in_point"))).clicked() { cmds.push(AppCommand::ClearSequenceInPoint); ui.close_menu(); }
            if ui.add(egui::Button::new("Clear Out Point").shortcut_text(kb.display_for("clear_out_point"))).clicked() { cmds.push(AppCommand::ClearSequenceOutPoint); ui.close_menu(); }
            if ui.add(egui::Button::new("Clear In and Out").shortcut_text(kb.display_for("clear_in_out"))).clicked() { cmds.push(AppCommand::ClearSequenceInPoint); cmds.push(AppCommand::ClearSequenceOutPoint); ui.close_menu(); }
            ui.separator();
            if ui.add(egui::Button::new("Go to In Point")).clicked() { if let Some(inp) = state.sequence().in_point { cmds.push(AppCommand::SeekTo(inp)); } ui.close_menu(); }
            if ui.add(egui::Button::new("Go to Out Point")).clicked() { if let Some(outp) = state.sequence().out_point { cmds.push(AppCommand::SeekTo(outp)); } ui.close_menu(); }
            if ui.add(egui::Button::new("Play In to Out").shortcut_text(kb.display_for("play_in_to_out"))).clicked() { cmds.push(AppCommand::PlayInToOut); ui.close_menu(); }
            ui.separator();
            if ui.add(egui::Button::new("Add Marker").shortcut_text(kb.display_for("add_marker"))).clicked() {
                cmds.push(AppCommand::AddMarker { position: state.playhead, label: String::new(), color: klipp_core::MarkerColor::Blue });
                ui.close_menu();
            }
            // No shortcut of its own: Shift+M, shown here before, adds a
            // clip marker (#909).
            if ui.add_enabled(!state.sequence().markers.is_empty(), egui::Button::new("Edit Marker...")).clicked() {
                // The marker nearest the playhead, opened for editing.
                let at = state.playhead.to_secs_f64();
                state.edit_marker_request = state
                    .sequence()
                    .markers
                    .iter()
                    .min_by(|a, b| (a.position.to_secs_f64() - at).abs().total_cmp(&(b.position.to_secs_f64() - at).abs()))
                    .map(|m| m.id);
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
            let source_media = state.source_media_id.and_then(|id| state.media_pool.get(&id));
            let in_source = state.viewer_mode == klipp_core::ViewerMode::Source && source_media.is_some();
            if ui.add_enabled(in_source, egui::Button::new("Add Source Marker").shortcut_text(kb.display_for("add_marker"))).on_hover_text("A note on the pool clip at the source playhead; the source viewer must be active.").clicked() {
                if let Some(id) = state.source_media_id {
                    cmds.push(AppCommand::AddMediaMarker { media_id: id, marker: klipp_core::MediaMarker::note(state.source_playhead.to_secs_f64()) });
                }
                ui.close_menu();
            }
            for (label, key, kind) in [("Favourite Source Range", "Shift+F", klipp_core::MediaMarkerKind::Favourite), ("Reject Source Range", "Shift+R", klipp_core::MediaMarkerKind::Reject)] {
                if ui.add_enabled(in_source, egui::Button::new(label).shortcut_text(key)).on_hover_text("The stretch between the source in and out points, or the whole clip when none are set.").clicked() {
                    if let (Some(id), Some(media)) = (state.source_media_id, source_media) {
                        let from = media.default_source_in.map(|t| t.to_secs_f64()).unwrap_or(0.0);
                        let to = media.default_source_out.map(|t| t.to_secs_f64()).unwrap_or_else(|| media.duration.to_secs_f64());
                        if to > from {
                            cmds.push(AppCommand::AddMediaMarker { media_id: id, marker: klipp_core::MediaMarker::range(kind, from, to) });
                        }
                    }
                    ui.close_menu();
                }
            }
            ui.separator();
            if ui.add(egui::Button::new("Next Marker").shortcut_text(kb.display_for("next_marker"))).clicked() {
                if let Some(next) = crate::navigation::marker_from(state.sequence(), state.playhead, true) {
                    cmds.push(AppCommand::SeekTo(next));
                }
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Previous Marker").shortcut_text(kb.display_for("prev_marker"))).clicked() {
                if let Some(prev) = crate::navigation::marker_from(state.sequence(), state.playhead, false) {
                    cmds.push(AppCommand::SeekTo(prev));
                }
                ui.close_menu();
            }
        });
    }

    fn view_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState, kb: &crate::keybindings::KeyBindings) {
        ui.menu_button(egui::RichText::new("View").size(crate::theme::type_scale::CONTROL), |ui| {
            // Zoom targets the currently-focused viewer (state.viewer_mode).
            // Source and Program have independent zoom + pan so clicking on
            // one viewer and Ctrl-+/-ing zooms only that one.
            let focused = state.viewer_mode;
            if ui.add(egui::Button::new("Zoom In").shortcut_text(kb.display_for("zoom_in"))).clicked() {
                let v = state.view_for_mut(focused);
                let current = if v.zoom <= 0.0 { 1.0 } else { v.zoom };
                v.zoom = (current * 1.25).clamp(0.05, 16.0);
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Zoom Out").shortcut_text(kb.display_for("zoom_out"))).clicked() {
                let v = state.view_for_mut(focused);
                let current = if v.zoom <= 0.0 { 1.0 } else { v.zoom };
                let next = current / 1.25;
                if next < 0.1 {
                    v.zoom = 0.0;
                    v.pan = egui::Vec2::ZERO;
                } else {
                    v.zoom = next;
                }
                ui.close_menu();
            }
            // The whole interface, not the picture: on a 4K screen the
            // default could be too small to read, and the setting was
            // only in Preferences.
            // Multicam mode (#1844): the angle grid in the Source
            // viewer's place, as the Angles button and its key switch it.
            let mut multicam = state.multicam_mode;
            if ui.checkbox(&mut multicam, "Multicam Angles").clicked() {
                cmds.push(AppCommand::SetMulticamMode { on: multicam, open: None });
            }
            ui.menu_button("Interface Size", |ui| {
                // Wide enough that the check mark stays on its entry's line.
                ui.set_min_width(96.0);
                for (label, pct) in [("Auto", 0u32), ("100%", 100), ("125%", 125), ("150%", 150), ("175%", 175), ("200%", 200)] {
                    let check = if state.ui_scale_percent == pct { "\u{2714} " } else { "  " };
                    if ui.button(format!("{check}{label}")).clicked() {
                        state.ui_scale_request = Some(pct);
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Viewer Zoom", |ui| {
                let current_zoom = state.view_for(focused).zoom;
                for (label, zoom) in [("Fit", 0.0f32), ("25%", 0.25), ("50%", 0.5), ("100%", 1.0), ("200%", 2.0)] {
                    let check = if (current_zoom - zoom).abs() < 0.01 { "\u{2714} " } else { "  " };
                    if ui.button(format!("{check}{label}")).clicked() {
                        let v = state.view_for_mut(focused);
                        v.zoom = zoom;
                        if zoom == 0.0 { v.pan = egui::Vec2::ZERO; }
                        ui.close_menu();
                    }
                }
            });
            ui.separator();
            // Fullscreen is on F11, which a Mac keyboard does not
            // always have, so it needs somewhere to be clicked.
            let full_check = if state.fullscreen_viewer { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{full_check}Fullscreen Viewer"))
                .shortcut_text(kb.display_for("toggle_fullscreen"))).clicked() {
                cmds.push(AppCommand::ToggleFullscreen);
                ui.close_menu();
            }
            ui.separator();
            let zebra_check = if state.viewer_overlays.zebras { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{zebra_check}Zebras")).shortcut_text(kb.display_for("toggle_zebra"))).clicked() {
                state.viewer_overlays.zebras = !state.viewer_overlays.zebras;
                ui.close_menu();
            }
            let fc_check = if state.viewer_overlays.false_color { "\u{2714} " } else { "  " };
            // Main button toggles false color on/off
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(format!("{fc_check}False Colour")).shortcut_text(kb.display_for("toggle_false_color"))).clicked() {
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
            if ui.add(egui::Button::new(format!("{peak_check}Focus Peaking"))).clicked() {
                state.viewer_overlays.focus_peaking = !state.viewer_overlays.focus_peaking;
                ui.close_menu();
            }
            ui.separator();
            let safe_check = if state.viewer_overlays.safe_areas { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{safe_check}Safe Areas")).shortcut_text(kb.display_for("toggle_safe_areas"))).clicked() {
                state.viewer_overlays.safe_areas = !state.viewer_overlays.safe_areas;
                ui.close_menu();
            }
            let grid_check = if state.viewer_overlays.grid { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{grid_check}Grid Overlay")).shortcut_text(kb.display_for("toggle_grid_overlay"))).clicked() {
                state.viewer_overlays.grid = !state.viewer_overlays.grid;
                ui.close_menu();
            }
            let checker_check = if state.viewer_overlays.checkerboard { "\u{2714} " } else { "  " };
            if ui.button(format!("{checker_check}Checkerboard")).clicked() {
                state.viewer_overlays.checkerboard = !state.viewer_overlays.checkerboard;
                ui.close_menu();
            }
            ui.separator();
            let scopes_check = if state.scopes_shown() { "\u{2714} " } else { "  " };
            ui.menu_button(format!("{scopes_check}Scopes"), |ui| {
                // Pick a scope: shows it (dock, overlay or floating window)
                // and makes it the one the dock and overlay draw.
                use crate::state::ScopeMode;
                for (label, mode) in [
                    ("Waveform", ScopeMode::Waveform),
                    ("Vectorscope", ScopeMode::Vectorscope),
                    ("Histogram", ScopeMode::Histogram),
                    ("Parade", ScopeMode::Parade),
                ] {
                    if ui.button(label).clicked() {
                        state.scope_mode = mode;
                        if !state.scopes_shown() {
                            state.toggle_scopes();
                        }
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button("Hide Scopes").clicked() {
                    state.show_scopes = false;
                    state.scope_window.open = false;
                    ui.close_menu();
                }
                ui.separator();
                let overlay_check = if state.scope_overlay_mode { "\u{2714} " } else { "  " };
                if ui.button(format!("{overlay_check}Overlay on Viewer")).clicked() { state.scope_overlay_mode = !state.scope_overlay_mode; ui.close_menu(); }
            });
            let color_check = if state.show_color_panel { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{color_check}Colour Panel")).shortcut_text(kb.display_for("toggle_color_panel"))).clicked() {
                state.show_color_panel = !state.show_color_panel;
                ui.close_menu();
            }
            ui.separator();
            if ui.add(egui::Button::new("Extract Frame to File\u{2026}").shortcut_text(kb.display_for("extract_frame_to_file"))).clicked() {
                cmds.push(AppCommand::FrameGrab);
                ui.close_menu();
            }
            if ui.add(egui::Button::new("Extract Frame to Bin").shortcut_text(kb.display_for("extract_frame_to_bin"))).clicked() {
                cmds.push(AppCommand::ExtractFrameToBin);
                ui.close_menu();
            }
            ui.separator();
            let skim_check = if state.skimming_enabled { "\u{2714} " } else { "  " };
            if ui.add(egui::Button::new(format!("{skim_check}Skimming")).shortcut_text(kb.display_for("toggle_skimming"))).clicked() {
                state.skimming_enabled = !state.skimming_enabled;
                ui.close_menu();
            }
            let dual_check = if state.viewers_side_by_side() { "\u{2714} " } else { "  " };
            if ui.button(format!("{dual_check}Dual Viewers")).on_hover_text("Remembered per workspace").clicked() {
                state.toggle_viewer_layout();
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
            if ui.add(egui::Button::new(rec_label).shortcut_text(kb.display_for("toggle_perf_recording"))).clicked() {
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

    fn window_menu(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, state: &mut AppState, kb: &crate::keybindings::KeyBindings) {
        ui.menu_button(egui::RichText::new("Window").size(crate::theme::type_scale::CONTROL), |ui| {
            // Active-workspace mode flags (controls the composite
            // inspector layout — not the same as a full workspace
            // preset below; kept as-is for backwards compatibility).
            ui.label(egui::RichText::new("ACTIVE MODE").size(10.0).color(egui::Color32::from_rgb(0x70, 0x70, 0x78)));
            // The same three names the workspace tabs use. These read
            // "Default / Colour Grading / Audio Mixing" once, which made
            // the third one plainly wrong — clicking "Audio Mixing" put
            // you in Export — and the other two disagree with the tabs
            // for no reason a user can see.
            for (ws, label) in &[(Workspace::Edit, "Edit"), (Workspace::Color, "Colour"), (Workspace::Export, "Export")] {
                let c = if state.active_workspace == *ws { "\u{2714} " } else { "  " };
                if ui.button(format!("{c}{label}")).clicked() { state.active_workspace = *ws; ui.close_menu(); }
            }
            ui.separator();

            // Built-in workspace layout presets (#39). Dispatched via
            // AppCommand so the apply path — write visibility + sizes +
            // layout.save() — stays in one place.
            ui.label(egui::RichText::new("BUILT-IN LAYOUTS").size(10.0).color(egui::Color32::from_rgb(0x70, 0x70, 0x78)));
            for (i, layout) in crate::state::builtin_workspace_layouts().iter().enumerate() {
                if ui.button(layout.display_name()).clicked() {
                    cmds.push(AppCommand::ApplyBuiltinWorkspaceLayout { index: i });
                    ui.close_menu();
                }
            }

            // User-saved workspaces.
            if !state.custom_workspaces.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new("SAVED").size(10.0).color(egui::Color32::from_rgb(0x70, 0x70, 0x78)));
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
            ui.label(egui::RichText::new("PANELS").size(10.0).color(egui::Color32::from_rgb(0x70, 0x70, 0x78)));
            let toggle_label = |active: bool, name: &str| if active { format!("\u{2714} {name}") } else { format!("  {name}") };
            if ui.button(toggle_label(state.graph_dock_open, "Nodes")).clicked() { cmds.push(AppCommand::ToggleGraphDock); ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.show_inspector_panel, "Inspector")).shortcut_text(kb.display_for("toggle_inspector_panel"))).clicked() { cmds.push(AppCommand::ToggleInspectorPanel); ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.show_browser_panel, "Browser")).shortcut_text(kb.display_for("toggle_browser_panel"))).clicked() { cmds.push(AppCommand::ToggleBrowserPanel); ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.show_curve_editor, "Curve Editor")).shortcut_text(kb.display_for("toggle_keyframe_editor"))).clicked() { state.show_curve_editor = !state.show_curve_editor; ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.show_audio_mixer, "Audio Mixer")).shortcut_text(kb.display_for("toggle_audio_mixer"))).clicked() { state.show_audio_mixer = !state.show_audio_mixer; ui.close_menu(); }
            if ui.button(toggle_label(state.show_history_panel, "History")).clicked() { state.show_history_panel = !state.show_history_panel; ui.close_menu(); }
            if ui.button(toggle_label(state.show_markers_panel, "Markers")).clicked() { state.show_markers_panel = !state.show_markers_panel; ui.close_menu(); }
            if ui.button(toggle_label(state.show_edit_list, "Edit List")).on_hover_text("Every clip, transition and marker in a table: sort it, filter it by name, click a row to go there.").clicked() { state.show_edit_list = !state.show_edit_list; ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.show_subtitle_editor, "Subtitle Editor")).shortcut_text(kb.display_for("toggle_subtitle_editor"))).clicked() { state.show_subtitle_editor = !state.show_subtitle_editor; ui.close_menu(); }
            if ui.button(toggle_label(state.show_transcript_panel, "Transcript")).clicked() { state.show_transcript_panel = !state.show_transcript_panel; ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.show_project_notes, "Project Notes")).shortcut_text(kb.display_for("toggle_project_notes"))).clicked() { state.show_project_notes = !state.show_project_notes; ui.close_menu(); }
            if ui.button(toggle_label(state.show_edit_plan, "Edit Plan")).on_hover_text("The paper edit an assistant proposed: read it, approve it, build it.").clicked() { state.show_edit_plan = !state.show_edit_plan; ui.close_menu(); }
            if ui.button(toggle_label(state.show_colour_check, "Colour Check")).on_hover_text("The frame on screen read the way a colourist reads it: skin against its band, neutrals, exposure, noise, banding, gamut — each fault named with the knob to turn.").clicked() { state.show_colour_check = !state.show_colour_check; ui.close_menu(); }
            if ui.button(toggle_label(state.show_delivery_check, "Delivery Check")).on_hover_text("The check before a piece goes out: the cut against the craft's rules and the brief, colour and loudness on request. Accept a finding on purpose with a reason, or mark it to fix.").clicked() { state.show_delivery_check = !state.show_delivery_check; ui.close_menu(); }
            if ui.button(toggle_label(state.show_sound_over_time, "Sound Over Time")).on_hover_text("A whole file's sound as frequency against time: where a hum starts, when a hiss creeps in. The scrolling analyser shows what is playing; this shows the file.").clicked() { state.show_sound_over_time = !state.show_sound_over_time; ui.close_menu(); }
            if ui.button(toggle_label(state.show_narrate, "Narrate")).on_hover_text("A scratch voice from text, made on this machine: choose a voice, a speed and the pauses, hear it, and place it on the Voice track.").clicked() { state.show_narrate = !state.show_narrate; ui.close_menu(); }
            if ui.add(egui::Button::new(toggle_label(state.ai_tools_state.panel_visible, "AI Tools")).shortcut_text(kb.display_for("toggle_ai_tools_panel"))).clicked() { state.ai_tools_state.panel_visible = !state.ai_tools_state.panel_visible; ui.close_menu(); }
            ui.separator();
            // Modal dialogs — distinct from panels because they
            // overlay the editor instead of docking.
            //
            // The plugin and model managers (Audio Plugins, AI Models)
            // live under Tools, not here: the noun mismatch between a
            // modal dialog and a dockable panel got worse as more were
            // added. This note used to send the reader to a top-level
            // "Plugins" menu, which has never existed.
            if ui.add(egui::Button::new("Command Palette").shortcut_text(kb.display_for("command_palette"))).clicked() { state.show_command_palette = true; ui.close_menu(); }
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
        ui.menu_button(egui::RichText::new("Tools").size(crate::theme::type_scale::CONTROL), |ui| {
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
            ui.separator();
            if ui.button("Project Doctor\u{2026}").clicked() {
                cmds.push(AppCommand::OpenProjectDoctor);
                ui.close_menu();
            }
            if ui.button("System Probe\u{2026}").clicked() {
                cmds.push(AppCommand::OpenSystemProbe);
                ui.close_menu();
            }
        });
    }

    fn help_menu(ui: &mut egui::Ui, state: &mut AppState) {
        ui.menu_button(egui::RichText::new("Help").size(crate::theme::type_scale::CONTROL), |ui| {
            if ui.add(egui::Button::new("Klipp Documentation")).clicked() {
                // Open local mdbook docs if available, fall back to GitHub
                let local_docs = std::path::Path::new("docs/book/index.html");
                if local_docs.exists() {
                    let abs = std::fs::canonicalize(local_docs).unwrap_or_default();
                    let _ = crate::ui_util::open_external(format!("file://{}", abs.display()));
                } else {
                    let _ = crate::ui_util::open_external("https://github.com/davidkodar/klipp");
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
                let _ = crate::ui_util::open_external("https://github.com/davidkodar/klipp/issues");
                ui.close_menu();
            }
            if ui.button("Release Notes").clicked() {
                let _ = crate::ui_util::open_external("https://github.com/davidkodar/klipp/releases");
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

/// An icon button's glyph: the accent when active, brighter under the
/// pointer, secondary otherwise.
fn icon_color(resp: &egui::Response, active: bool, theme: &KlippTheme) -> egui::Color32 {
    if active { theme.accent }
    else if resp.hovered() { theme.text_primary }
    else { theme.text_secondary }
}

/// An icon button's field: a tint of the accent when active, the hover
/// tone under the pointer, nothing otherwise.
pub(crate) fn icon_rect(btn: egui::Rect) -> egui::Rect {
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

pub(crate) fn draw_scopes_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
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


/// The span of the selected clips, from the first one's start to the
/// last one's end, for Render Selection (#924).
fn selection_span(state: &AppState) -> Option<(klipp_core::Timecode, klipp_core::Timecode)> {
    let selected = |id: &klipp_core::ClipId| state.selected_clip == Some(*id) || state.selected_clips.contains(id);
    let clips: Vec<&klipp_timeline::Clip> = state.sequence().tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| selected(&c.id)).collect();
    let from = clips.iter().map(|c| c.timeline_in).min()?;
    let to = clips.iter().map(|c| c.timeline_out).max()?;
    (to > from).then_some((from, to))
}

/// Where the workspace switcher goes on the bar.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SwitcherPlace {
    /// The three tabs, in this rect.
    Tabs(egui::Rect),
    /// A drop-down naming the workspace shown, in this rect.
    Menu(egui::Rect),
    /// No room at all.
    None,
}

/// The narrowest drop-down the switcher shrinks to.
const SWITCHER_MENU_WIDTH: f32 = 96.0;

/// Where the workspace tabs of `size` go between the menus' end (`from`)
/// and the right group's start (`to`) on `bar` (#1355): centred on the
/// bar when they fit there, else centred in the room between, else a
/// drop-down in that room. They never draw over a menu or a button.
fn switcher_place(bar: egui::Rect, from: f32, to: f32, size: egui::Vec2) -> SwitcherPlace {
    let at = |x: f32, w: f32| egui::Rect::from_center_size(egui::pos2(x, bar.center().y), egui::vec2(w, size.y));
    let centred = at(bar.center().x, size.x);
    if centred.left() >= from && centred.right() <= to {
        return SwitcherPlace::Tabs(centred);
    }
    let room = to - from;
    if room >= size.x {
        return SwitcherPlace::Tabs(at((from + to) / 2.0, size.x));
    }
    if room >= SWITCHER_MENU_WIDTH {
        return SwitcherPlace::Menu(at((from + to) / 2.0, SWITCHER_MENU_WIDTH));
    }
    SwitcherPlace::None
}

#[cfg(test)]
mod graph_menu_tests {
    use super::*;

    #[test]
    fn open_graph_is_reachable_from_the_toolbar_and_window_menu() {
        for workspace in [Workspace::Edit, Workspace::Color] {
            for menu in [false, true] {
                let ctx = egui::Context::default();
                let mut panel = ToolbarPanel { logo_texture: None };
                let mut state = AppState { active_workspace: workspace, ..Default::default() };
                let mut time = 0.0;
                let mut frame = |state: &mut AppState, events| {
                    time += 0.1;
                    let mut cmds = Vec::new();
                    let output = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0))), time: Some(time), events, ..Default::default() }, |ctx| {
                        egui::TopBottomPanel::top("toolbar").exact_height(40.0).show(ctx, |ui| {
                            cmds = panel.show(ui, state, &[], &KlippTheme::default(), &crate::keybindings::KeyBindings::default());
                        });
                    });
                    let texts: Vec<_> = output.shapes.iter().filter_map(|s| match &s.shape {
                        egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None,
                    }).collect();
                    (cmds, texts)
                };
                let button = |at, pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
                for open in [true, false] {
                    let mut texts = Vec::new();
                    for _ in 0..3 { texts = frame(&mut state, Vec::new()).1; }
                    if menu {
                        let at = texts.iter().find(|(s, _)| s == "Window").unwrap().1;
                        frame(&mut state, vec![egui::Event::PointerMoved(at), button(at, true)]);
                        frame(&mut state, vec![button(at, false)]);
                        for _ in 0..3 { texts = frame(&mut state, Vec::new()).1; }
                    }
                    let label = "Nodes";
                    let at = texts.iter().rev().find(|(s, _)| s.trim_start_matches([' ', '✔']) == label).unwrap_or_else(|| panic!("{label}: {texts:?}")).1;
                    frame(&mut state, vec![egui::Event::PointerMoved(at), button(at, true)]);
                    let cmds = frame(&mut state, vec![button(at, false)]).0;
                    assert!(matches!(cmds.as_slice(), [AppCommand::ToggleGraphDock]), "{cmds:?}");
                    state.path_command(&cmds[0]);
                    assert_eq!(state.graph_dock_open, open);
                    assert_eq!(state.active_workspace, workspace);
                    assert!(!state.dirty);
                }
            }
        }
    }
}

#[cfg(test)]
mod shortcut_label_tests {
    //! The menus must not write a key combo down themselves.
    //!
    //! egui's `shortcut_text` only *draws* a label — it binds nothing.
    //! So a literal in a menu is a claim with nothing checking it, and
    //! they drifted: the File menu advertised Ctrl+N and Ctrl+Q with no
    //! handler behind either, Split at Playhead said `S` while the
    //! binding was Ctrl+B, and Go to In Point said Shift+I, which is
    //! bound to *clear* the in point. Every label now comes from
    //! `KeyBindings::display_for`, so it cannot disagree with the
    //! binding and it follows a user's remapping for free.

    /// This file, minus this test module — which talks *about*
    /// `shortcut_text` and would otherwise match itself.
    fn menu_source() -> &'static str {
        const SRC: &str = include_str!("toolbar.rs");
        match SRC.find("mod shortcut_label_tests") {
            Some(i) => &SRC[..i],
            None => SRC,
        }
    }

    /// Every file that can print a key combo at the user. A tooltip is
    /// the same promise a menu label is, and the tooltips drifted the
    /// same way: the timeline's edit-mode button said "W" after the
    /// binding moved to Shift+W, and its Fit button said "Shift+Z"
    /// while fit_to_window has always been Z.
    fn shortcut_printing_sources() -> Vec<(&'static str, &'static str)> {
        const TB: &str = include_str!("toolbar.rs");
        let tb = match TB.find("mod shortcut_label_tests") {
            Some(i) => &TB[..i],
            None => TB,
        };
        vec![
            ("panels/toolbar.rs", tb),
            ("timeline/toolbar.rs", include_str!("../timeline/toolbar.rs")),
        ]
    }

    #[test]
    fn no_tooltip_writes_its_own_key_combo() {
        let mut offenders = Vec::new();
        for (name, src) in shortcut_printing_sources() {
            for (n, line) in src.lines().enumerate() {
                // `styled_tooltip(.., Some("Ctrl+E"))` — a literal combo
                // rather than `Some(&kb.display_for("export"))`.
                if line.contains("styled_tooltip(") && line.contains("Some(\"") {
                    offenders.push(format!("{name}:{}: {}", n + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "tooltips must take their combo from kb.display_for so it cannot disagree \
             with the binding:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn no_menu_writes_its_own_key_combo() {
        let literals: Vec<&str> = menu_source()
            .lines()
            .filter(|l| {
                // `shortcut_text("Ctrl+S")` and `shortcut_text(format!("{n}"))`
                // are the same mistake wearing different clothes; the
                // multicam angles were the second kind.
                l.contains("shortcut_text(\"") || l.contains("shortcut_text(format!")
            })
            .collect();
        assert!(
            literals.is_empty(),
            "menu items must call shortcut_text(kb.display_for(\"action\")) so the label \
             cannot disagree with the binding. Found {} hardcoded:\n{}",
            literals.len(),
            literals.join("\n")
        );
    }

    #[test]
    fn every_action_a_menu_names_is_actually_bound() {
        let kb = crate::keybindings::KeyBindings::defaults();
        let mut unbound = Vec::new();
        for line in menu_source().lines() {
            let mut rest = line;
            while let Some(i) = rest.find("display_for(\"") {
                rest = &rest[i + "display_for(\"".len()..];
                let Some(end) = rest.find('"') else { break };
                let action = &rest[..end];
                // display_for returns "" for an unknown action, which
                // renders as a menu item with no shortcut — the silent
                // failure this test exists to make loud.
                if kb.display_for(action).is_empty() {
                    unbound.push(action.to_string());
                }
                rest = &rest[end..];
            }
        }
        unbound.sort();
        unbound.dedup();
        assert!(
            unbound.is_empty(),
            "menus name actions that have no default binding, so they render blank: {unbound:?}"
        );
    }
}

#[cfg(test)]
pub(crate) mod narrow_bar_tests {
    use super::*;

    /// The texts the toolbar paints at `width`, with their rects, after a
    /// few frames to settle.
    fn texts_at(width: f32, workspace: Workspace) -> Vec<(String, egui::Rect)> {
        let ctx = egui::Context::default();
        let mut panel = ToolbarPanel { logo_texture: None };
        let mut state = AppState { active_workspace: workspace, ..Default::default() };
        let mut texts = Vec::new();
        for i in 0..3 {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))), time: Some(f64::from(i) * 0.1), ..Default::default() };
            let output = ctx.run(input, |ctx| {
                egui::TopBottomPanel::top("toolbar").exact_height(40.0).show(ctx, |ui| {
                    panel.show(ui, &mut state, &[], &KlippTheme::default(), &crate::keybindings::KeyBindings::default());
                });
            });
            texts = output.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) if !t.galley.text().trim().is_empty() => Some((t.galley.text().to_owned(), t.visual_bounding_rect())),
                _ => None,
            }).collect();
        }
        texts
    }

    /// Pairs of texts drawn over each other.
    pub(crate) fn overlaps(texts: &[(String, egui::Rect)]) -> Vec<String> {
        let mut out = Vec::new();
        for (i, (a, ra)) in texts.iter().enumerate() {
            for (b, rb) in &texts[i + 1..] {
                if ra.shrink(0.5).intersects(rb.shrink(0.5)) {
                    out.push(format!("{a:?} {ra:?} over {b:?} {rb:?}"));
                }
            }
        }
        out
    }

    /// At 1018 px, and narrower and wider, in each workspace: the menus,
    /// the workspace switcher and the right group never draw over each
    /// other, and every menu title is still there (#1355).
    #[test]
    fn the_bar_draws_nothing_over_anything_at_any_width() {
        for width in [1920.0, 1280.0, 1018.0, 900.0, 760.0] {
            for workspace in [Workspace::Edit, Workspace::Color, Workspace::Export] {
                let texts = texts_at(width, workspace);
                let clash = overlaps(&texts);
                let off: Vec<_> = texts.iter().filter(|(_, r)| r.left() < 0.0 || r.right() > width).collect();
                assert!(off.is_empty(), "{width} px, {workspace:?}: off the bar: {off:?}");
                assert!(clash.is_empty(), "{width} px, {workspace:?}:\n{}", clash.join("\n"));
                for menu in ["File", "Edit", "View", "Window", "Help"] {
                    assert!(texts.iter().any(|(t, _)| t == menu), "{width} px, {workspace:?}: no {menu} menu among {texts:?}");
                }
            }
        }
    }

    /// #466: the File menu opens Colour Management, after Project
    /// Properties.
    #[test]
    fn the_file_menu_opens_colour_management() {
        let ctx = egui::Context::default();
        let mut panel = ToolbarPanel { logo_texture: None };
        let mut state = AppState::default();
        let mut time = 0.0;
        let mut frame = |state: &mut AppState, events| {
            time += 0.1;
            let output = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0))), time: Some(time), events, ..Default::default() }, |ctx| {
                egui::TopBottomPanel::top("toolbar").exact_height(40.0).show(ctx, |ui| {
                    panel.show(ui, state, &[], &KlippTheme::default(), &crate::keybindings::KeyBindings::default());
                });
            });
            output.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None,
            }).collect::<Vec<_>>()
        };
        let button = |at, pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut texts = Vec::new();
        for _ in 0..3 { texts = frame(&mut state, Vec::new()); }
        let at = texts.iter().find(|(s, _)| s == "File").unwrap().1;
        frame(&mut state, vec![egui::Event::PointerMoved(at), button(at, true)]);
        frame(&mut state, vec![button(at, false)]);
        for _ in 0..3 { texts = frame(&mut state, Vec::new()); }
        let props = texts.iter().find(|(s, _)| s == "Project Properties...").expect("Project Properties").1;
        let at = texts.iter().find(|(s, _)| s == "Colour Management\u{2026}").unwrap_or_else(|| panic!("{texts:?}")).1;
        assert!(at.y > props.y, "after Project Properties");
        frame(&mut state, vec![egui::Event::PointerMoved(at), button(at, true)]);
        frame(&mut state, vec![button(at, false)]);
        assert!(state.show_colour_management);
    }

    /// The switcher sits centred when the menus leave room, moves into the
    /// room they leave when they don't, becomes a drop-down when the three
    /// tabs no longer fit, and is left out with no room at all.
    #[test]
    fn the_switcher_goes_where_there_is_room() {
        let bar = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 40.0));
        let size = egui::vec2(200.0, 28.0);
        let SwitcherPlace::Tabs(r) = switcher_place(bar, 300.0, 850.0, size) else { panic!("centred") };
        assert_eq!(r.center().x, 500.0);
        let SwitcherPlace::Tabs(r) = switcher_place(bar, 560.0, 840.0, size) else { panic!("moved over") };
        assert!(r.left() >= 560.0 && r.right() <= 840.0, "{r:?}");
        let SwitcherPlace::Menu(r) = switcher_place(bar, 700.0, 840.0, size) else { panic!("a drop-down") };
        assert!(r.left() >= 700.0 && r.right() <= 840.0, "{r:?}");
        assert_eq!(switcher_place(bar, 800.0, 840.0, size), SwitcherPlace::None);
    }
}
