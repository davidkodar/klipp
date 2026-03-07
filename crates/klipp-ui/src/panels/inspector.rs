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

        if let Some(clip_id) = state.selected_clip {
            // Find the clip and its media item
            let clip = state
                .sequence
                .video_track()
                .and_then(|t| t.find_clip(clip_id));
            let media = clip.and_then(|c| state.media_pool.get(&c.media_id));

            if let (Some(clip), Some(media)) = (clip, media) {
                let fps = state.fps;

                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("Clip Info")
                        .strong()
                        .size(12.0),
                );
                ui.separator();
                ui.add_space(4.0);

                egui::Grid::new("inspector_clip_info")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("File:");
                        ui.label(&media.filename);
                        ui.end_row();

                        ui.label("Duration:");
                        ui.label(media.duration.format_hmsf(&fps));
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

                        ui.label("Codec:");
                        let codec_str = if media.audio_codec.is_empty() {
                            media.video_codec.clone()
                        } else {
                            format!("{} / {}", media.video_codec, media.audio_codec)
                        };
                        ui.label(codec_str);
                        ui.end_row();
                    });

                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Timeline")
                        .strong()
                        .size(12.0),
                );
                ui.separator();
                ui.add_space(4.0);

                egui::Grid::new("inspector_timeline")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("In:");
                        ui.label(clip.timeline_in.format_hmsf(&fps));
                        ui.end_row();

                        ui.label("Out:");
                        ui.label(clip.timeline_out.format_hmsf(&fps));
                        ui.end_row();

                        ui.label("Duration:");
                        ui.label(clip.duration().format_hmsf(&fps));
                        ui.end_row();
                    });

                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Source")
                        .strong()
                        .size(12.0),
                );
                ui.separator();
                ui.add_space(4.0);

                egui::Grid::new("inspector_source")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("In:");
                        ui.label(clip.source_in.format_hmsf(&fps));
                        ui.end_row();

                        ui.label("Out:");
                        ui.label(clip.source_out.format_hmsf(&fps));
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
