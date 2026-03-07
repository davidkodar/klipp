use crate::state::{AppState, PlayheadFollow};
use klipp_core::{AppCommand, Tool};

pub struct ToolbarPanel;

impl ToolbarPanel {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        ui.horizontal(|ui| {
            ui.add_space(8.0);

            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("New Project").clicked() {
                        cmds.push(AppCommand::NewProject);
                        ui.close_menu();
                    }
                    if ui.button("Open Project...").clicked() {
                        ui.close_menu();
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Open Project")
                            .pick_folder()
                        {
                            cmds.push(AppCommand::OpenProject { path });
                        }
                    }
                    if ui.button("Import Media...").clicked() {
                        ui.close_menu();
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter(
                                "Video",
                                &["mp4", "mkv", "mov", "avi", "webm", "m4v", "mts"],
                            )
                            .pick_file()
                        {
                            let media_id = klipp_core::MediaId::new();
                            cmds.push(AppCommand::LoadMediaFile {
                                path,
                                media_id,
                            });
                        }
                    }
                    if ui.button("Open File...").clicked() {
                        ui.close_menu();
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter(
                                "Video",
                                &["mp4", "mkv", "mov", "avi", "webm", "m4v", "mts"],
                            )
                            .pick_file()
                        {
                            cmds.push(AppCommand::LoadMediaFile {
                                path,
                                media_id: klipp_core::MediaId::new(),
                            });
                        }
                    }
                    ui.separator();
                    if ui
                        .add(egui::Button::new("Save").shortcut_text("Ctrl+S"))
                        .clicked()
                    {
                        cmds.push(AppCommand::SaveProject);
                        ui.close_menu();
                    }
                    if ui
                        .add(egui::Button::new("Save As...").shortcut_text("Ctrl+Shift+S"))
                        .clicked()
                    {
                        ui.close_menu();
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Save Project As")
                            .pick_folder()
                        {
                            cmds.push(AppCommand::SaveProjectAs { path });
                        }
                    }
                    ui.separator();
                    if ui
                        .add(egui::Button::new("Export...").shortcut_text("Ctrl+E"))
                        .clicked()
                    {
                        cmds.push(AppCommand::ShowExportDialog);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        cmds.push(AppCommand::Quit);
                        ui.close_menu();
                    }
                });

                ui.menu_button("Edit", |ui| {
                    let can_undo = state.history.can_undo();
                    let undo_label = if let Some(desc) = state.history.undo_label() {
                        format!("Undo {}", desc)
                    } else {
                        "Undo".to_string()
                    };
                    if ui
                        .add_enabled(can_undo, egui::Button::new(undo_label).shortcut_text("Ctrl+Z"))
                        .clicked()
                    {
                        cmds.push(AppCommand::Undo);
                        ui.close_menu();
                    }

                    let can_redo = state.history.can_redo();
                    let redo_label = if let Some(desc) = state.history.redo_label() {
                        format!("Redo {}", desc)
                    } else {
                        "Redo".to_string()
                    };
                    if ui
                        .add_enabled(can_redo, egui::Button::new(redo_label).shortcut_text("Ctrl+Y"))
                        .clicked()
                    {
                        cmds.push(AppCommand::Redo);
                        ui.close_menu();
                    }

                    ui.separator();

                    let has_sel = state.selected_clip.is_some();
                    if ui.add_enabled(has_sel, egui::Button::new("Cut").shortcut_text("Ctrl+X")).clicked() {
                        if let Some(clip_id) = state.selected_clip {
                            cmds.push(AppCommand::CutClip { clip_id });
                        }
                        ui.close_menu();
                    }
                    if ui.add_enabled(has_sel, egui::Button::new("Copy").shortcut_text("Ctrl+C")).clicked() {
                        if let Some(clip_id) = state.selected_clip {
                            cmds.push(AppCommand::CopyClip { clip_id });
                        }
                        ui.close_menu();
                    }
                    let has_clip = state.clipboard.is_some();
                    if ui.add_enabled(has_clip, egui::Button::new("Paste").shortcut_text("Ctrl+V")).clicked() {
                        cmds.push(AppCommand::PasteClip);
                        ui.close_menu();
                    }

                    ui.separator();

                    if ui.add(egui::Button::new("Split at Playhead").shortcut_text("Ctrl+B")).clicked() {
                        let track_index = state.sequence.tracks.iter()
                            .position(|t| t.name == "Video 1")
                            .unwrap_or(0);
                        if let Some(track) = state.sequence.tracks.get(track_index) {
                            if let Some((_, clip)) = track.clip_at_timecode(state.playhead) {
                                cmds.push(AppCommand::BladeClip {
                                    track_index,
                                    clip_id: clip.id,
                                    cut_at: state.playhead,
                                });
                            }
                        }
                        ui.close_menu();
                    }
                });

                ui.menu_button("View", |ui| {
                    if ui.add(egui::Button::new("Full Screen Viewer").shortcut_text("F")).clicked() {
                        cmds.push(AppCommand::ToggleFullscreen);
                        ui.close_menu();
                    }
                    ui.separator();
                    let proxy_label = if state.use_proxies { "\u{2713} Use Proxies" } else { "  Use Proxies" };
                    if ui.button(proxy_label).clicked() {
                        cmds.push(AppCommand::SetUseProxies(!state.use_proxies));
                        ui.close_menu();
                    }
                });

                ui.menu_button("Settings", |ui| {
                    // --- Timeline section ---
                    ui.label(egui::RichText::new("Timeline").strong().size(11.0));

                    let snap_label = if state.snap_enabled { "\u{2713} Snap to Clips" } else { "  Snap to Clips" };
                    if ui.button(snap_label).clicked() {
                        cmds.push(AppCommand::SetSnapEnabled(!state.snap_enabled));
                        ui.close_menu();
                    }

                    let ripple_label = if state.ripple_enabled { "\u{2713} Ripple Edit" } else { "  Ripple Edit" };
                    if ui.button(ripple_label).clicked() {
                        cmds.push(AppCommand::SetRippleEnabled(!state.ripple_enabled));
                        ui.close_menu();
                    }

                    ui.separator();

                    // --- Playhead follow ---
                    ui.label(egui::RichText::new("Playhead Follow").strong().size(11.0));
                    for mode in &[PlayheadFollow::Off, PlayheadFollow::Page, PlayheadFollow::Smooth] {
                        let check = if state.playhead_follow == *mode { "\u{2713} " } else { "  " };
                        if ui.button(format!("{}{}", check, mode.label())).clicked() {
                            state.playhead_follow = *mode;
                            ui.close_menu();
                        }
                    }

                    ui.separator();

                    // --- Performance ---
                    ui.label(egui::RichText::new("Debug").strong().size(11.0));
                    let perf_label = if state.show_perf_overlay { "\u{2713} Performance Overlay" } else { "  Performance Overlay" };
                    if ui.button(perf_label).clicked() {
                        state.show_perf_overlay = !state.show_perf_overlay;
                        ui.close_menu();
                    }
                });

                ui.menu_button("Help", |ui| {
                    if ui.button("About Klipp").clicked() {
                        state.show_about_dialog = true;
                        ui.close_menu();
                    }
                });
            });

            // Tool selector buttons
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(4.0);

            let select_color = if state.active_tool == Tool::Select {
                egui::Color32::from_rgb(80, 130, 200)
            } else {
                egui::Color32::from_rgb(120, 120, 130)
            };
            if ui.add(egui::Button::new(
                egui::RichText::new("\u{2196} Select").color(select_color).size(11.0)
            ).min_size(egui::vec2(60.0, 24.0))).on_hover_text("Select tool (V)").clicked() {
                cmds.push(AppCommand::SetActiveTool(Tool::Select));
            }

            let blade_color = if state.active_tool == Tool::Blade {
                egui::Color32::from_rgb(80, 130, 200)
            } else {
                egui::Color32::from_rgb(120, 120, 130)
            };
            if ui.add(egui::Button::new(
                egui::RichText::new("\u{2702} Blade").color(blade_color).size(11.0)
            ).min_size(egui::vec2(60.0, 24.0))).on_hover_text("Blade tool (B)").clicked() {
                cmds.push(AppCommand::SetActiveTool(Tool::Blade));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                ui.colored_label(
                    egui::Color32::from_rgb(120, 120, 130),
                    format!("Klipp v{}-dev", env!("CARGO_PKG_VERSION")),
                );
            });
        });

        cmds
    }
}
