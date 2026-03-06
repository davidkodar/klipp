use crate::state::AppState;
use klipp_core::AppCommand;

#[derive(Default)]
pub struct ToolbarPanel {
    pub show_open_modal: bool,
    pub open_path: String,
    pub open_error: Option<String>,
}

impl ToolbarPanel {
    pub fn show(&mut self, ui: &mut egui::Ui, _state: &AppState) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        ui.horizontal(|ui| {
            ui.add_space(8.0);

            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open File...").clicked() {
                        self.show_open_modal = true;
                        self.open_error = None;
                        ui.close_menu();
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

    pub fn show_open_modal(
        &mut self,
        ctx: &egui::Context,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        if !self.show_open_modal {
            return cmds;
        }

        let mut open = self.show_open_modal;

        egui::Window::new("Open File")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .fixed_size([480.0, 0.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Path:");
                    let response =
                        ui.add(egui::TextEdit::singleline(&mut self.open_path).desired_width(400.0));
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let path = std::path::PathBuf::from(&self.open_path);
                        cmds.push(AppCommand::LoadMediaFile {
                            path,
                            media_id: klipp_core::MediaId::new(),
                        });
                    }
                });

                if let Some(err) = &self.open_error {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 96, 96),
                        format!("Could not open file: {err}"),
                    );
                }

                ui.add_space(8.0);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui.button("Open").clicked() {
                        let path = std::path::PathBuf::from(&self.open_path);
                        cmds.push(AppCommand::LoadMediaFile {
                            path,
                            media_id: klipp_core::MediaId::new(),
                        });
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_open_modal = false;
                        self.open_error = None;
                    }
                });
            });

        self.show_open_modal = open;
        cmds
    }
}
