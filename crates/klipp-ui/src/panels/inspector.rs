use crate::state::AppState;
use klipp_core::AppCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InspectorTab {
    Info,
    Color,
    Effects,
}

pub struct InspectorPanel {
    active_tab: InspectorTab,
    show_add_effect: bool,
}

impl Default for InspectorPanel {
    fn default() -> Self {
        Self {
            active_tab: InspectorTab::Info,
            show_add_effect: false,
        }
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

        let clip_id = state.selected_clip;
        let clip_info = clip_id.and_then(|cid| {
            state.sequence.tracks.iter().enumerate().find_map(|(ti, t)| {
                t.find_clip(cid).map(|c| (ti, c))
            })
        });
        let media = clip_info.and_then(|(_, c)| state.media_pool.get(&c.media_id));

        // Tab bar
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let info_style = if self.active_tab == InspectorTab::Info {
                egui::RichText::new("Info").strong().size(12.0)
            } else {
                egui::RichText::new("Info").size(12.0).color(egui::Color32::from_rgb(140, 140, 150))
            };
            if ui.selectable_label(self.active_tab == InspectorTab::Info, info_style).clicked() {
                self.active_tab = InspectorTab::Info;
            }

            let color_style = if self.active_tab == InspectorTab::Color {
                egui::RichText::new("Color").strong().size(12.0)
            } else {
                egui::RichText::new("Color").size(12.0).color(egui::Color32::from_rgb(140, 140, 150))
            };
            if ui.selectable_label(self.active_tab == InspectorTab::Color, color_style).clicked() {
                self.active_tab = InspectorTab::Color;
            }

            let fx_style = if self.active_tab == InspectorTab::Effects {
                egui::RichText::new("Effects").strong().size(12.0)
            } else {
                egui::RichText::new("Effects").size(12.0).color(egui::Color32::from_rgb(140, 140, 150))
            };
            if ui.selectable_label(self.active_tab == InspectorTab::Effects, fx_style).clicked() {
                self.active_tab = InspectorTab::Effects;
            }
        });
        ui.separator();

        match self.active_tab {
            InspectorTab::Info => {
                if let (Some((track_idx, clip)), Some(media)) = (clip_info, media) {
                    self.show_info_tab(ui, state, track_idx, clip, media, &mut cmds);
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(80, 80, 90),
                            "No clip selected",
                        );
                    });
                }
            }
            InspectorTab::Color => {
                if let Some((_, clip)) = clip_info {
                    self.show_color_tab(ui, clip, &mut cmds);
                }
                self.show_global_lut_section(ui, state, &mut cmds);
            }
            InspectorTab::Effects => {
                if let Some((_, clip)) = clip_info {
                    self.show_effects_tab(ui, state, clip, &mut cmds);
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(80, 80, 90),
                            "Select a clip to add effects",
                        );
                    });
                }
            }
        }

        cmds
    }

    fn show_info_tab(
        &self,
        ui: &mut egui::Ui,
        state: &AppState,
        track_idx: usize,
        clip: &klipp_timeline::Clip,
        media: &klipp_media::MediaItem,
        cmds: &mut Vec<AppCommand>,
    ) {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Clip Info").strong().size(12.0));
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
        ui.label(egui::RichText::new("Timeline").strong().size(12.0));
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
        ui.label(egui::RichText::new("Source").strong().size(12.0));
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
        ui.label(egui::RichText::new("Filmstrip").strong().size(12.0));
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
        ui.label(egui::RichText::new("Proxy").strong().size(12.0));
        ui.separator();
        ui.add_space(4.0);

        if media.proxy_ready {
            ui.colored_label(egui::Color32::from_rgb(100, 200, 120), "Ready");
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
    }

    fn show_color_tab(
        &self,
        ui: &mut egui::Ui,
        clip: &klipp_timeline::Clip,
        cmds: &mut Vec<AppCommand>,
    ) {
        let clip_id = clip.id;
        let grade = clip.color_grade;

        ui.add_space(8.0);
        ui.label(egui::RichText::new("Color Grade").strong().size(12.0));
        ui.separator();
        ui.add_space(8.0);

        let mut exposure = grade.exposure;
        ui.label("Exposure");
        if ui.add(egui::Slider::new(&mut exposure, -5.0..=5.0)
            .step_by(0.05).suffix(" stops").min_decimals(1)
            .custom_formatter(|v, _| format!("{v:+.1}"))
        ).changed() {
            cmds.push(AppCommand::SetClipExposure { clip_id, exposure });
        }
        ui.add_space(6.0);

        let mut contrast = grade.contrast;
        ui.label("Contrast");
        if ui.add(egui::Slider::new(&mut contrast, -1.0..=1.0)
            .step_by(0.01).min_decimals(2)
            .custom_formatter(|v, _| format!("{v:+.2}"))
        ).changed() {
            cmds.push(AppCommand::SetClipContrast { clip_id, contrast });
        }
        ui.add_space(6.0);

        let mut saturation = grade.saturation;
        ui.label("Saturation");
        if ui.add(egui::Slider::new(&mut saturation, 0.0..=2.0)
            .step_by(0.01).min_decimals(2)
        ).changed() {
            cmds.push(AppCommand::SetClipSaturation { clip_id, saturation });
        }
        ui.add_space(6.0);

        let mut temperature = grade.temperature;
        ui.label("Temperature");
        if ui.add(egui::Slider::new(&mut temperature, -100.0..=100.0)
            .step_by(1.0).min_decimals(0)
        ).changed() {
            cmds.push(AppCommand::SetClipTemperature { clip_id, temperature });
        }
        ui.add_space(6.0);

        let mut tint = grade.tint;
        ui.label("Tint");
        if ui.add(egui::Slider::new(&mut tint, -100.0..=100.0)
            .step_by(1.0).min_decimals(0)
        ).changed() {
            cmds.push(AppCommand::SetClipTint { clip_id, tint });
        }

        ui.add_space(12.0);
        if ui.button("Reset All").clicked() {
            cmds.push(AppCommand::ResetClipColorGrade { clip_id });
        }
    }

    fn show_global_lut_section(
        &self,
        ui: &mut egui::Ui,
        state: &AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Global LUT").strong().size(12.0));
        ui.separator();
        ui.add_space(4.0);

        if let Some(path) = &state.global_lut_path {
            let filename = path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Unknown".into());
            ui.colored_label(egui::Color32::from_rgb(100, 200, 120), &filename);
            ui.add_space(4.0);

            let mut opacity = state.global_lut_opacity;
            ui.label("Opacity");
            if ui.add(egui::Slider::new(&mut opacity, 0.0..=1.0)
                .step_by(0.01).min_decimals(2)
            ).changed() {
                cmds.push(AppCommand::SetGlobalLutOpacity(opacity));
            }
            ui.add_space(4.0);

            if ui.small_button("Clear LUT").clicked() {
                cmds.push(AppCommand::ClearGlobalLut);
            }
        } else {
            ui.label("No LUT loaded");
            ui.add_space(4.0);
            if ui.button("Load .cube LUT...").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Load .cube LUT")
                    .add_filter("Cube LUT", &["cube"])
                    .pick_file()
                {
                    cmds.push(AppCommand::LoadGlobalLut { path });
                }
            }
        }
    }

    fn show_effects_tab(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        clip: &klipp_timeline::Clip,
        cmds: &mut Vec<AppCommand>,
    ) {
        let clip_id = clip.id;

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Effects").strong().size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("+ Add Effect").clicked() {
                    self.show_add_effect = true;
                }
            });
        });
        ui.separator();
        ui.add_space(4.0);

        if clip.effects.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(80, 80, 90),
                "No effects applied",
            );
        }

        let mut remove_idx = None;
        let mut move_up_idx = None;
        let mut move_down_idx = None;

        for (i, effect) in clip.effects.iter().enumerate() {
            ui.horizontal(|ui| {
                let mut enabled = effect.enabled;
                if ui.checkbox(&mut enabled, "").changed() {
                    cmds.push(AppCommand::SetEffectEnabled {
                        clip_id,
                        effect_index: i,
                        enabled,
                    });
                }

                // Short display name (strip common prefixes)
                let display_name = effect.plugin_name
                    .rsplit_once('.')
                    .map(|(_, name)| name)
                    .unwrap_or(&effect.plugin_name);
                ui.label(display_name);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("\u{2715}").on_hover_text("Remove").clicked() {
                        remove_idx = Some(i);
                    }
                    if i + 1 < clip.effects.len()
                        && ui.small_button("\u{25BC}").on_hover_text("Move down").clicked()
                    {
                        move_down_idx = Some(i);
                    }
                    if i > 0
                        && ui.small_button("\u{25B2}").on_hover_text("Move up").clicked()
                    {
                        move_up_idx = Some(i);
                    }
                });
            });
            ui.add_space(2.0);
        }

        if let Some(idx) = remove_idx {
            cmds.push(AppCommand::RemoveEffect { clip_id, effect_index: idx });
        }
        if let Some(idx) = move_up_idx {
            cmds.push(AppCommand::ReorderEffect { clip_id, from_index: idx, to_index: idx - 1 });
        }
        if let Some(idx) = move_down_idx {
            cmds.push(AppCommand::ReorderEffect { clip_id, from_index: idx, to_index: idx + 1 });
        }

        // Add effect popup
        if self.show_add_effect {
            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("Available Plugins").strong().size(11.0));
            ui.add_space(4.0);

            if state.available_plugins.is_empty() {
                ui.colored_label(
                    egui::Color32::from_rgb(80, 80, 90),
                    "No OFX plugins found",
                );
            }

            for plugin in &state.available_plugins {
                let display_name = plugin.name
                    .rsplit_once('.')
                    .map(|(_, name)| name)
                    .unwrap_or(&plugin.name);
                let label = format!("{} v{}.{}", display_name, plugin.version.0, plugin.version.1);
                if ui.small_button(&label).clicked() {
                    cmds.push(AppCommand::AddEffect {
                        clip_id,
                        plugin_name: plugin.name.clone(),
                    });
                    self.show_add_effect = false;
                }
            }

            ui.add_space(4.0);
            if ui.small_button("Cancel").clicked() {
                self.show_add_effect = false;
            }
        }
    }
}
