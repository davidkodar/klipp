use crate::state::AppState;
use klipp_core::AppCommand;

pub struct ToolbarPanel;

impl ToolbarPanel {
    pub fn show(&mut self, ui: &mut egui::Ui, _state: &AppState) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        ui.horizontal(|ui| {
            ui.add_space(8.0);

            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
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
                    if ui.button("Quit").clicked() {
                        cmds.push(AppCommand::Quit);
                        ui.close_menu();
                    }
                });

                ui.menu_button("Edit", |ui| {
                    let r = ui.add_enabled(false, egui::Button::new("Undo"));
                    r.on_disabled_hover_text("(coming soon)");
                    let r = ui.add_enabled(false, egui::Button::new("Redo"));
                    r.on_disabled_hover_text("(coming soon)");
                });

                ui.menu_button("View", |ui| {
                    let r = ui.add_enabled(false, egui::Button::new("Zoom In"));
                    r.on_disabled_hover_text("(coming soon)");
                    let r = ui.add_enabled(false, egui::Button::new("Zoom Out"));
                    r.on_disabled_hover_text("(coming soon)");
                });

                ui.menu_button("Help", |ui| {
                    let r = ui.add_enabled(false, egui::Button::new("About"));
                    r.on_disabled_hover_text("(coming soon)");
                });
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                ui.colored_label(
                    egui::Color32::from_rgb(120, 120, 130),
                    "Klipp v0.1.0-dev",
                );
            });
        });

        cmds
    }
}
