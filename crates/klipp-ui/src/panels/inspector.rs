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
        let mut cmds = Vec::new();

        ui.painter().rect_filled(
            ui.available_rect_before_wrap(),
            0.0,
            egui::Color32::from_rgb(28, 28, 32),
        );

        if let Some(clip_id) = state.selected_clip {
            // Find the clip and its media item
            let clip_info = state.sequence.tracks.iter().enumerate().find_map(|(ti, t)| {
                t.find_clip(clip_id).map(|c| (ti, c))
            });
            let media = clip_info.and_then(|(_, c)| state.media_pool.get(&c.media_id));

            if let (Some((track_idx, clip)), Some(media)) = (clip_info, media) {
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
                        ui.label(media.duration.to_timecode_string());
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
                        ui.label(clip.timeline_in.to_timecode_string());
                        ui.end_row();

                        ui.label("Out:");
                        ui.label(clip.timeline_out.to_timecode_string());
                        ui.end_row();

                        ui.label("Duration:");
                        ui.label(clip.duration().to_timecode_string());
                        ui.end_row();

                        ui.label("Track:");
                        if let Some(track) = state.sequence.tracks.get(track_idx) {
                            ui.label(&track.name);
                        }
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
                        ui.label(clip.source_in.to_timecode_string());
                        ui.end_row();

                        ui.label("Out:");
                        ui.label(clip.source_out.to_timecode_string());
                        ui.end_row();
                    });

                // Filmstrip
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Filmstrip")
                        .strong()
                        .size(12.0),
                );
                ui.separator();
                ui.add_space(4.0);

                let filmstrip_count = state
                    .filmstrip_textures
                    .get(&clip.media_id)
                    .map(|f| f.len())
                    .unwrap_or(0);
                if filmstrip_count > 0 {
                    ui.colored_label(
                        egui::Color32::from_rgb(100, 200, 120),
                        format!("{} frames", filmstrip_count),
                    );
                } else {
                    ui.label("Generating...");
                }

                // Proxy status
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Proxy")
                        .strong()
                        .size(12.0),
                );
                ui.separator();
                ui.add_space(4.0);

                if media.proxy_ready {
                    ui.horizontal(|ui| {
                        ui.colored_label(egui::Color32::from_rgb(100, 200, 120), "Ready");
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.label("Not generated");
                        if ui.small_button("Generate Now").clicked() {
                            cmds.push(AppCommand::GenerateProxy { media_id: media.id });
                        }
                    });
                }

                // Offline warning
                if media.offline {
                    ui.add_space(8.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 160, 80),
                        "\u{26A0} Media Offline",
                    );
                    ui.label(format!("Path: {}", media.path.display()));
                    if ui.small_button("Relink...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Relink media file")
                            .pick_file()
                        {
                            cmds.push(AppCommand::RelinkMedia {
                                media_id: media.id,
                                new_path: path,
                            });
                        }
                    }
                }
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

        cmds
    }
}
