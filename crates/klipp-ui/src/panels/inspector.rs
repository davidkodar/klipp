use crate::state::AppState;
use klipp_core::AppCommand;

pub struct InspectorPanel;

impl Default for InspectorPanel {
    fn default() -> Self {
        Self
    }
}

impl InspectorPanel {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &AppState) -> Vec<AppCommand> {
        ui.painter().rect_filled(
            ui.available_rect_before_wrap(),
            0.0,
            egui::Color32::from_rgb(28, 28, 32),
        );

        if let Some(media) = &state.loaded_media {
            ui.add_space(12.0);
            ui.heading("Clip Info");
            ui.separator();
            ui.add_space(4.0);

            egui::Grid::new("inspector_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("File:");
                    ui.label(&media.filename);
                    ui.end_row();

                    ui.label("Duration:");
                    ui.label(media.duration.format_hmsf(&media.fps));
                    ui.end_row();

                    ui.label("FPS:");
                    ui.label(format!("{:.3}", media.fps.as_f64()));
                    ui.end_row();

                    ui.label("Size:");
                    ui.label(format!(
                        "{} \u{00d7} {}",
                        media.resolution.width, media.resolution.height
                    ));
                    ui.end_row();
                });
        } else {
            ui.centered_and_justified(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(80, 80, 90),
                    "No clip selected",
                );
            });
        }

        Vec::new()
    }
}
