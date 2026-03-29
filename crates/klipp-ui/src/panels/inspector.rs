use crate::icons::Icon;
use crate::state::AppState;
use klipp_core::AppCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InspectorTab {
    Info,
    Color,
    Effects,
    Title,
}

pub struct InspectorPanel {
    active_tab: InspectorTab,
    show_add_effect: bool,
    show_add_builtin: bool,
}

impl Default for InspectorPanel {
    fn default() -> Self {
        Self {
            active_tab: InspectorTab::Info,
            show_add_effect: false,
            show_add_builtin: false,
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
            state.sequence().tracks.iter().enumerate().find_map(|(ti, t)| {
                t.find_clip(cid).map(|c| (ti, c))
            })
        });
        let media = clip_info.and_then(|(_, c)| c.media_id().and_then(|mid| state.media_pool.get(&mid)));

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

            // Only show Title tab when a title clip is selected
            let is_title_clip = clip_info.is_some_and(|(_, c)| {
                matches!(c.kind, klipp_timeline::ClipKind::Title(_))
            });
            if is_title_clip {
                let title_style = if self.active_tab == InspectorTab::Title {
                    egui::RichText::new("Title").strong().size(12.0)
                } else {
                    egui::RichText::new("Title").size(12.0).color(egui::Color32::from_rgb(140, 140, 150))
                };
                if ui.selectable_label(self.active_tab == InspectorTab::Title, title_style).clicked() {
                    self.active_tab = InspectorTab::Title;
                }
            }
        });

        // Auto-switch to Title tab when a title clip is selected
        let is_title_clip = clip_info.is_some_and(|(_, c)| {
            matches!(c.kind, klipp_timeline::ClipKind::Title(_))
        });
        if is_title_clip && self.active_tab == InspectorTab::Info {
            self.active_tab = InspectorTab::Title;
        } else if !is_title_clip && self.active_tab == InspectorTab::Title {
            self.active_tab = InspectorTab::Info;
        }

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
                self.show_color_tab_redirect(ui, state, &mut cmds);
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
            InspectorTab::Title => {
                if let Some((_, clip)) = clip_info {
                    self.show_title_tab(ui, clip, state, &mut cmds);
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(80, 80, 90),
                            "Select a title clip",
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
                if let Some(track) = state.sequence().tracks.get(track_idx) {
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

        let filmstrip_count = clip.media_id()
            .and_then(|mid| state.filmstrip_textures.get(&mid))
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

        let is_generating = state.proxy_generating.contains(&media.id);

        if media.proxy_ready {
            ui.horizontal(|ui| {
                ui.add(Icon::CheckCircle.image(14.0).tint(egui::Color32::from_rgb(100, 200, 120)));
                ui.colored_label(egui::Color32::from_rgb(100, 200, 120), "Ready");
            });
            if let Some(proxy_path) = &media.proxy_path {
                // Show proxy file details
                let file_size = std::fs::metadata(proxy_path).map(|m| m.len()).unwrap_or(0);
                egui::Grid::new("inspector_proxy_info")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Size:");
                        ui.label(format!("{:.1} MB", file_size as f64 / (1024.0 * 1024.0)));
                        ui.end_row();

                        ui.label("Path:");
                        let path_str = proxy_path.to_string_lossy();
                        ui.label(
                            egui::RichText::new(path_str.as_ref())
                                .size(10.0)
                                .color(egui::Color32::from_rgb(100, 100, 110)),
                        );
                        ui.end_row();
                    });

                // Use proxies toggle status
                ui.add_space(4.0);
                if state.use_proxies {
                    ui.colored_label(
                        egui::Color32::from_rgb(80, 160, 220),
                        "Using proxy for playback",
                    );
                } else {
                    ui.colored_label(
                        egui::Color32::from_rgb(100, 100, 110),
                        "Proxy available (Use Proxies is off)",
                    );
                }
            }
        } else if is_generating {
            let progress = state.proxy_progress.get(&media.id).copied().unwrap_or(0.0);
            let eta_str = if progress > 0.01 {
                if let Some(start) = state.proxy_start_times.get(&media.id) {
                    let elapsed = start.elapsed().as_secs_f64();
                    let remaining = (elapsed / progress as f64) - elapsed;
                    let rem_secs = remaining as u64;
                    if rem_secs >= 60 {
                        format!(" — ~{}m {:02}s left", rem_secs / 60, rem_secs % 60)
                    } else {
                        format!(" — ~{}s left", rem_secs)
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 180, 60),
                    format!("Generating proxy — {:.0}%{}", progress * 100.0, eta_str),
                );
            });
            ui.add_space(2.0);
            let bar = egui::ProgressBar::new(progress)
                .desired_width(ui.available_width());
            ui.add(bar);
            ui.add_space(4.0);
            if ui.small_button("Cancel").clicked() {
                cmds.push(AppCommand::CancelProxy { media_id: media.id });
            }
            ui.ctx().request_repaint();
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
            ui.horizontal(|ui| {
                ui.add(Icon::Warning.image(14.0).tint(egui::Color32::from_rgb(255, 160, 80)));
                ui.colored_label(egui::Color32::from_rgb(255, 160, 80), "Media Offline");
            });
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

    fn show_color_tab_redirect(
        &self,
        ui: &mut egui::Ui,
        state: &AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let text_dim = egui::Color32::from_rgb(0x90, 0x90, 0xa0);
        let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);

        // Input Color Space / Log Format selector
        if let Some(clip_id) = state.selected_clip {
            let current_log = state.sequences.get(state.active_sequence_idx)
                .and_then(|seq| seq.tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id))
                .and_then(|c| c.node_graph.nodes.iter()
                    .find_map(|n| n.params.log_format))
                .unwrap_or_default();

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new("Input Color Space").size(10.0).color(text_dim));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                use klipp_core::LogFormat as LF;
                let formats = [
                    (LF::Srgb, "sRGB / Rec.709 (Standard)"),
                    (LF::Linear, "Linear"),
                    (LF::LogC3, "ARRI LogC3"),
                    (LF::LogC4, "ARRI LogC4"),
                    (LF::SLog2, "Sony S-Log2"),
                    (LF::SLog3, "Sony S-Log3"),
                    (LF::CanonLog, "Canon Log"),
                    (LF::CanonLog3, "Canon Log 3"),
                    (LF::VLog, "Panasonic V-Log"),
                    (LF::NLog, "Nikon N-Log"),
                    (LF::FLog, "Fuji F-Log"),
                    (LF::DJIDLog, "DJI D-Log"),
                ];
                let current_label = formats.iter()
                    .find(|(f, _)| *f == current_log)
                    .map(|(_, l)| *l)
                    .unwrap_or("sRGB / Rec.709 (Standard)");

                egui::ComboBox::from_id_salt("log_format_selector")
                    .selected_text(egui::RichText::new(current_label).size(9.0))
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for &(fmt, label) in &formats {
                            if ui.selectable_label(current_log == fmt,
                                egui::RichText::new(label).size(9.0),
                            ).clicked() {
                                cmds.push(AppCommand::SetClipLogFormat {
                                    clip_id, log_format: fmt,
                                });
                            }
                        }
                    });
            });
            ui.add_space(8.0);
        }

        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("Color grading has moved")
                    .size(11.0)
                    .color(text_dim),
            );
            ui.add_space(12.0);
            if ui.button(
                egui::RichText::new("Open Color Panel  (C)")
                    .size(11.0)
                    .color(text_accent),
            ).clicked() {
                cmds.push(AppCommand::ToggleColorPanel);
            }
        });
    }

    fn font_style_to_u8(style: klipp_timeline::FontStyle) -> u8 {
        match style {
            klipp_timeline::FontStyle::Regular => 0,
            klipp_timeline::FontStyle::Bold => 1,
            klipp_timeline::FontStyle::Italic => 2,
            klipp_timeline::FontStyle::BoldItalic => 3,
        }
    }

    fn show_title_tab(
        &self,
        ui: &mut egui::Ui,
        clip: &klipp_timeline::Clip,
        state: &AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let clip_id = clip.id;
        let td = match &clip.kind {
            klipp_timeline::ClipKind::Title(td) => td,
            _ => return,
        };

        ui.add_space(8.0);
        ui.label(egui::RichText::new("Title Properties").strong().size(12.0));
        ui.separator();
        ui.add_space(8.0);

        // Text input
        let mut text = td.text.clone();
        ui.label("Text");
        let response = ui.add(
            egui::TextEdit::multiline(&mut text)
                .desired_width(f32::INFINITY)
                .desired_rows(3)
                .font(egui::TextStyle::Monospace),
        );
        if response.changed() {
            cmds.push(AppCommand::UpdateTitleData {
                clip_id,
                text: text.clone(),
                font_size: td.font_size,
                font_name: td.font_name.clone(),
                font_style: Self::font_style_to_u8(td.font_style),
                color: td.color,
            });
        }
        ui.add_space(8.0);

        // Font family selector
        ui.label("Font");
        let current_font = &td.font_name;
        let display_name = if current_font == "sans-serif" || current_font.is_empty() {
            "Noto Sans (Default)"
        } else {
            current_font
        };
        egui::ComboBox::from_id_salt("title_font_family")
            .selected_text(display_name)
            .width(ui.available_width() - 8.0)
            .show_ui(ui, |ui| {
                // Default option
                if ui.selectable_label(
                    current_font == "sans-serif" || current_font.is_empty(),
                    "Noto Sans (Default)",
                ).clicked() {
                    cmds.push(AppCommand::UpdateTitleData {
                        clip_id,
                        text: td.text.clone(),
                        font_size: td.font_size,
                        font_name: "sans-serif".to_string(),
                        font_style: Self::font_style_to_u8(td.font_style),
                        color: td.color,
                    });
                }
                ui.separator();
                // System fonts (alphabetical from BTreeMap)
                for family in &state.available_fonts {
                    if ui.selectable_label(current_font == family, family).clicked() {
                        cmds.push(AppCommand::UpdateTitleData {
                            clip_id,
                            text: td.text.clone(),
                            font_size: td.font_size,
                            font_name: family.clone(),
                            font_style: Self::font_style_to_u8(td.font_style),
                            color: td.color,
                        });
                    }
                }
            });
        ui.add_space(4.0);

        // Font style toggles (Bold / Italic)
        ui.horizontal(|ui| {
            let is_bold = matches!(td.font_style, klipp_timeline::FontStyle::Bold | klipp_timeline::FontStyle::BoldItalic);
            let is_italic = matches!(td.font_style, klipp_timeline::FontStyle::Italic | klipp_timeline::FontStyle::BoldItalic);

            let bold_text = if is_bold {
                egui::RichText::new("B").strong().size(13.0)
            } else {
                egui::RichText::new("B").size(13.0).color(egui::Color32::from_rgb(140, 140, 150))
            };
            let italic_text = if is_italic {
                egui::RichText::new("I").italics().size(13.0)
            } else {
                egui::RichText::new("I").size(13.0).color(egui::Color32::from_rgb(140, 140, 150))
            };

            if ui.selectable_label(is_bold, bold_text).clicked() {
                let new_bold = !is_bold;
                let new_style = match (new_bold, is_italic) {
                    (true, true) => klipp_timeline::FontStyle::BoldItalic,
                    (true, false) => klipp_timeline::FontStyle::Bold,
                    (false, true) => klipp_timeline::FontStyle::Italic,
                    (false, false) => klipp_timeline::FontStyle::Regular,
                };
                cmds.push(AppCommand::UpdateTitleData {
                    clip_id,
                    text: td.text.clone(),
                    font_size: td.font_size,
                    font_name: td.font_name.clone(),
                    font_style: Self::font_style_to_u8(new_style),
                    color: td.color,
                });
            }
            if ui.selectable_label(is_italic, italic_text).clicked() {
                let new_italic = !is_italic;
                let new_style = match (is_bold, new_italic) {
                    (true, true) => klipp_timeline::FontStyle::BoldItalic,
                    (true, false) => klipp_timeline::FontStyle::Bold,
                    (false, true) => klipp_timeline::FontStyle::Italic,
                    (false, false) => klipp_timeline::FontStyle::Regular,
                };
                cmds.push(AppCommand::UpdateTitleData {
                    clip_id,
                    text: td.text.clone(),
                    font_size: td.font_size,
                    font_name: td.font_name.clone(),
                    font_style: Self::font_style_to_u8(new_style),
                    color: td.color,
                });
            }
        });
        ui.add_space(8.0);

        // Font size — when in text edit mode with a selection, applies to selection only
        let sel_for_size = if state.viewer_text_edit { state.viewer_text_selection.or(state.viewer_text_selection_locked) } else { None };
        let display_font_size = if let Some((sel_start, sel_end)) = sel_for_size {
            selection_font_size(td, sel_start, sel_end)
        } else {
            td.font_size
        };
        let mut font_size = display_font_size;
        ui.label("Font Size");
        if ui.add(egui::Slider::new(&mut font_size, 12.0..=300.0)
            .step_by(1.0)
            .suffix(" pt")
        ).changed() {
            if let Some((sel_start, sel_end)) = sel_for_size {
                cmds.push(AppCommand::ApplyTitleStyle {
                    clip_id,
                    range_start: sel_start,
                    range_end: sel_end,
                    property: "font_size".to_string(),
                    value: font_size.to_string(),
                });
            } else {
                cmds.push(AppCommand::UpdateTitleData {
                    clip_id,
                    text: td.text.clone(),
                    font_size,
                    font_name: td.font_name.clone(),
                    font_style: Self::font_style_to_u8(td.font_style),
                    color: td.color,
                });
            }
        }
        ui.add_space(8.0);

        // Text color — when in text edit mode with a selection, applies to selection only
        ui.label("Text Color");
        let selection = if state.viewer_text_edit { state.viewer_text_selection.or(state.viewer_text_selection_locked) } else { None };
        // Resolve displayed color: if selection exists, check style runs for selection range
        let display_color = if let Some((sel_start, sel_end)) = selection {
            selection_color(td, sel_start, sel_end)
        } else {
            td.color
        };
        let mut color = egui::Color32::from_rgba_premultiplied(display_color[0], display_color[1], display_color[2], display_color[3]);
        if ui.color_edit_button_srgba(&mut color).changed() {
            let new_color = [color.r(), color.g(), color.b(), color.a()];
            if let Some((sel_start, sel_end)) = selection {
                cmds.push(AppCommand::ApplyTitleStyle {
                    clip_id,
                    range_start: sel_start,
                    range_end: sel_end,
                    property: "color".to_string(),
                    value: serde_json::to_string(&new_color).unwrap_or_default(),
                });
            } else {
                cmds.push(AppCommand::UpdateTitleData {
                    clip_id,
                    text: td.text.clone(),
                    font_size: td.font_size,
                    font_name: td.font_name.clone(),
                    font_style: Self::font_style_to_u8(td.font_style),
                    color: new_color,
                });
            }
        }
        ui.add_space(8.0);

        // Style runs info (show count if any exist)
        if !td.style_runs.is_empty() {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} style run(s)", td.style_runs.len()))
                        .small()
                        .color(egui::Color32::from_rgb(140, 140, 160)),
                );
                if ui.small_button("Clear All").clicked() {
                    cmds.push(AppCommand::ClearTitleStyleRuns { clip_id });
                }
            });
            ui.add_space(4.0);
        }

        // Alignment
        ui.label("Alignment");
        ui.horizontal(|ui| {
            let current = match td.alignment {
                klipp_timeline::TextAlignment::Left => 0u8,
                klipp_timeline::TextAlignment::Center => 1,
                klipp_timeline::TextAlignment::Right => 2,
            };
            for (val, label) in [(0, "Left"), (1, "Center"), (2, "Right")] {
                if ui.selectable_label(current == val, label).clicked() && current != val {
                    cmds.push(AppCommand::SetTitleAlignment { clip_id, alignment: val });
                }
            }
        });
        ui.add_space(8.0);

        // Position
        ui.label("Position");
        let mut pos_x = td.position.x;
        let mut pos_y = td.position.y;
        let mut pos_changed = false;
        pos_changed |= ui.add(egui::Slider::new(&mut pos_x, 0.0..=1.0).text("X").step_by(0.01)).changed();
        pos_changed |= ui.add(egui::Slider::new(&mut pos_y, 0.0..=1.0).text("Y").step_by(0.01)).changed();
        if pos_changed {
            cmds.push(AppCommand::SetTitlePosition { clip_id, x: pos_x, y: pos_y });
        }
        ui.add_space(4.0);
        // Position presets
        ui.horizontal(|ui| {
            if ui.small_button("Center").clicked() {
                cmds.push(AppCommand::SetTitlePosition { clip_id, x: 0.5, y: 0.5 });
            }
            if ui.small_button("Lower Third").clicked() {
                cmds.push(AppCommand::SetTitlePosition { clip_id, x: 0.5, y: 0.85 });
            }
            if ui.small_button("Top").clicked() {
                cmds.push(AppCommand::SetTitlePosition { clip_id, x: 0.5, y: 0.15 });
            }
        });
        ui.add_space(8.0);

        // Background
        ui.label("Background");
        let bg_kind: u8 = match &td.background {
            klipp_timeline::TitleBackground::None => 0,
            klipp_timeline::TitleBackground::Color { .. } => 1,
            klipp_timeline::TitleBackground::FullFrame { .. } => 2,
        };
        let bg_color = match &td.background {
            klipp_timeline::TitleBackground::Color { color, .. } => *color,
            klipp_timeline::TitleBackground::FullFrame { color } => *color,
            _ => [0, 0, 0, 180],
        };
        ui.horizontal(|ui| {
            for (val, label) in [(0, "None"), (1, "Box"), (2, "Full")] {
                if ui.selectable_label(bg_kind == val, label).clicked() && bg_kind != val {
                    cmds.push(AppCommand::SetTitleBackground { clip_id, bg_kind: val, color: bg_color });
                }
            }
        });
        if bg_kind > 0 {
            let mut bg_col = egui::Color32::from_rgba_premultiplied(bg_color[0], bg_color[1], bg_color[2], bg_color[3]);
            if ui.color_edit_button_srgba(&mut bg_col).changed() {
                cmds.push(AppCommand::SetTitleBackground {
                    clip_id,
                    bg_kind,
                    color: [bg_col.r(), bg_col.g(), bg_col.b(), bg_col.a()],
                });
            }
        }
        ui.add_space(8.0);

        // Shadow
        ui.label("Shadow");
        let has_shadow = td.shadow.is_some();
        let shadow = td.shadow.unwrap_or(klipp_timeline::TitleShadow {
            color: [0, 0, 0, 180],
            offset_x: 3.0,
            offset_y: 3.0,
            blur: 0.0,
        });
        let mut shadow_enabled = has_shadow;
        if ui.checkbox(&mut shadow_enabled, "Enable Shadow").changed() {
            cmds.push(AppCommand::SetTitleShadow {
                clip_id,
                enabled: shadow_enabled,
                color: shadow.color,
                offset_x: shadow.offset_x,
                offset_y: shadow.offset_y,
                blur: shadow.blur,
            });
        }
        if has_shadow {
            let mut ox = shadow.offset_x;
            let mut oy = shadow.offset_y;
            let mut s_changed = false;
            s_changed |= ui.add(egui::Slider::new(&mut ox, -20.0..=20.0).text("Offset X").step_by(0.5)).changed();
            s_changed |= ui.add(egui::Slider::new(&mut oy, -20.0..=20.0).text("Offset Y").step_by(0.5)).changed();
            let mut s_col = egui::Color32::from_rgba_premultiplied(shadow.color[0], shadow.color[1], shadow.color[2], shadow.color[3]);
            s_changed |= ui.color_edit_button_srgba(&mut s_col).changed();
            if s_changed {
                cmds.push(AppCommand::SetTitleShadow {
                    clip_id,
                    enabled: true,
                    color: [s_col.r(), s_col.g(), s_col.b(), s_col.a()],
                    offset_x: ox,
                    offset_y: oy,
                    blur: shadow.blur,
                });
            }
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
                    .add_filter("LUT files", &["cube", "3dl", "csp"])
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

        // ── Built-in Effects ──────────────────────────────────────
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Built-in Effects").strong().size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("+ Add").clicked() {
                    self.show_add_builtin = !self.show_add_builtin;
                    self.show_add_effect = false;
                }
            });
        });
        ui.separator();
        ui.add_space(4.0);

        if clip.builtin_effects.is_empty() && !self.show_add_builtin {
            ui.colored_label(
                egui::Color32::from_rgb(80, 80, 90),
                "No built-in effects applied",
            );
        }

        let mut builtin_remove = None;
        let mut builtin_move_up = None;
        let mut builtin_move_down = None;

        for (i, effect) in clip.builtin_effects.iter().enumerate() {
            let name = builtin_effect_name(effect);

            // Collapsing header for each effect with controls
            let header_id = ui.make_persistent_id(format!("bfx_{clip_id:?}_{i}"));
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), header_id, true)
                .show_header(ui, |ui| {
                    ui.label(egui::RichText::new(name).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::image(Icon::X.image(12.0).tint(egui::Color32::from_rgb(220, 220, 230)))).on_hover_text("Remove").clicked() {
                            builtin_remove = Some(i);
                        }
                        if i + 1 < clip.builtin_effects.len()
                            && ui.add(egui::Button::image(Icon::CaretDown.image(12.0).tint(egui::Color32::from_rgb(220, 220, 230)))).on_hover_text("Move down").clicked()
                        {
                            builtin_move_down = Some(i);
                        }
                        if i > 0
                            && ui.add(egui::Button::image(Icon::CaretUp.image(12.0).tint(egui::Color32::from_rgb(220, 220, 230)))).on_hover_text("Move up").clicked()
                        {
                            builtin_move_up = Some(i);
                        }
                    });
                })
                .body(|ui| {
                    if let Some(updated) = show_builtin_effect_params(ui, effect, clip_id, i, cmds) {
                        cmds.push(AppCommand::UpdateBuiltinEffect {
                            clip_id,
                            index: i,
                            effect: updated,
                        });
                    }
                });
            ui.add_space(2.0);
        }

        if let Some(idx) = builtin_remove {
            cmds.push(AppCommand::RemoveBuiltinEffect { clip_id, index: idx });
        }
        if let Some(idx) = builtin_move_up {
            cmds.push(AppCommand::ReorderBuiltinEffect { clip_id, from_index: idx, to_index: idx - 1 });
        }
        if let Some(idx) = builtin_move_down {
            cmds.push(AppCommand::ReorderBuiltinEffect { clip_id, from_index: idx, to_index: idx + 1 });
        }

        // Add built-in effect picker
        if self.show_add_builtin {
            ui.add_space(4.0);
            ui.separator();
            ui.label(egui::RichText::new("Add Built-in Effect").strong().size(11.0));
            ui.add_space(4.0);

            let effects = [
                ("Opacity", klipp_effects::BuiltinEffect::Opacity { value: 1.0 }),
                ("Gaussian Blur", klipp_effects::BuiltinEffect::GaussianBlur { radius: 0.0 }),
                ("Sharpen", klipp_effects::BuiltinEffect::Sharpen { amount: 0.0 }),
                ("Crop", klipp_effects::BuiltinEffect::Crop {
                    left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0,
                }),
                ("Transform", klipp_effects::BuiltinEffect::Transform {
                    position_x: 0.0, position_y: 0.0,
                    scale_x: 1.0, scale_y: 1.0,
                    rotation: 0.0,
                    anchor_x: 0.5, anchor_y: 0.5,
                }),
                ("Chroma Key", klipp_effects::BuiltinEffect::ChromaKey {
                    key_color: [0, 255, 0],
                    tolerance: 0.2,
                    spill_suppression: 0.1,
                }),
            ];

            for (label, effect) in &effects {
                if ui.small_button(*label).clicked() {
                    cmds.push(AppCommand::AddBuiltinEffect {
                        clip_id,
                        effect: effect.clone(),
                    });
                    self.show_add_builtin = false;
                }
            }

            ui.add_space(4.0);
            if ui.small_button("Cancel").clicked() {
                self.show_add_builtin = false;
            }
        }

        // ── OFX Plugin Effects ────────────────────────────────────
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("OFX Plugins").strong().size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("+ Add Plugin").clicked() {
                    self.show_add_effect = !self.show_add_effect;
                    self.show_add_builtin = false;
                }
            });
        });
        ui.separator();
        ui.add_space(4.0);

        if clip.effects.is_empty() && !self.show_add_effect {
            ui.colored_label(
                egui::Color32::from_rgb(80, 80, 90),
                "No OFX effects applied",
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

                let display_name = effect.plugin_name
                    .rsplit_once('.')
                    .map(|(_, name)| name)
                    .unwrap_or(&effect.plugin_name);
                ui.label(display_name);
                ui.label(egui::RichText::new("OFX").size(8.0).color(egui::Color32::from_rgb(80, 120, 200)).strong());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::image(Icon::X.image(12.0).tint(egui::Color32::from_rgb(220, 220, 230)))).on_hover_text("Remove").clicked() {
                        remove_idx = Some(i);
                    }
                    if i + 1 < clip.effects.len()
                        && ui.add(egui::Button::image(Icon::CaretDown.image(12.0).tint(egui::Color32::from_rgb(220, 220, 230)))).on_hover_text("Move down").clicked()
                    {
                        move_down_idx = Some(i);
                    }
                    if i > 0
                        && ui.add(egui::Button::image(Icon::CaretUp.image(12.0).tint(egui::Color32::from_rgb(220, 220, 230)))).on_hover_text("Move up").clicked()
                    {
                        move_up_idx = Some(i);
                    }
                });
            });

            // Parameter controls — look up descriptors from the plugin
            if let Some(plugin_info) = state.available_plugins.iter().find(|p| p.name == effect.plugin_name) {
                if !plugin_info.parameters.is_empty() {
                    let id = egui::Id::new(("ofx_params", clip_id, i));
                    egui::CollapsingHeader::new(
                        egui::RichText::new("Parameters").size(10.0).color(egui::Color32::from_rgb(140, 140, 150))
                    )
                        .id_salt(id)
                        .default_open(true)
                        .show(ui, |ui| {
                            for desc in &plugin_info.parameters {
                                if desc.secret {
                                    continue;
                                }
                                match desc.param_type {
                                    klipp_plugin::ParamType::Group | klipp_plugin::ParamType::Page => continue,
                                    _ => {}
                                }

                                let current_value = effect.param_values.get(&desc.name);

                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(&desc.label)
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(160, 160, 170)),
                                    );
                                    ui.add_space(4.0);

                                    match desc.param_type {
                                        klipp_plugin::ParamType::Double => {
                                            let mut v = current_value
                                                .and_then(|cv| cv.as_double())
                                                .or(match &desc.default { klipp_plugin::ParamValue::Double(d) => Some(*d), _ => None })
                                                .unwrap_or(0.0) as f32;
                                            let dmin = desc.display_min.as_ref()
                                                .and_then(|m| match m { klipp_plugin::ParamValue::Double(d) => Some(*d as f32), _ => None })
                                                .unwrap_or(0.0);
                                            let dmax = desc.display_max.as_ref()
                                                .and_then(|m| match m { klipp_plugin::ParamValue::Double(d) => Some(*d as f32), _ => None })
                                                .unwrap_or(1.0);
                                            if ui.add(egui::Slider::new(&mut v, dmin..=dmax).step_by(0.01_f64)).changed() {
                                                cmds.push(AppCommand::SetEffectParam {
                                                    clip_id,
                                                    effect_index: i,
                                                    param_name: desc.name.clone(),
                                                    value: klipp_plugin::OfxValue::Double(v as f64),
                                                });
                                            }
                                            ui.label(egui::RichText::new(format!("{v:.2}")).size(10.0).color(egui::Color32::from_rgb(120, 120, 130)));
                                        }
                                        klipp_plugin::ParamType::Int => {
                                            let mut v = current_value
                                                .and_then(|cv| cv.as_int())
                                                .or(match &desc.default { klipp_plugin::ParamValue::Int(i) => Some(*i), _ => None })
                                                .unwrap_or(0);
                                            let imin = desc.min.as_ref()
                                                .and_then(|m| match m { klipp_plugin::ParamValue::Int(i) => Some(*i), _ => None })
                                                .unwrap_or(0);
                                            let imax = desc.max.as_ref()
                                                .and_then(|m| match m { klipp_plugin::ParamValue::Int(i) => Some(*i), _ => None })
                                                .unwrap_or(100);
                                            if ui.add(egui::DragValue::new(&mut v).range(imin..=imax).speed(1)).changed() {
                                                cmds.push(AppCommand::SetEffectParam {
                                                    clip_id,
                                                    effect_index: i,
                                                    param_name: desc.name.clone(),
                                                    value: klipp_plugin::OfxValue::Int(v),
                                                });
                                            }
                                        }
                                        klipp_plugin::ParamType::Bool => {
                                            let mut v = current_value
                                                .and_then(|cv| cv.as_bool())
                                                .or(match &desc.default { klipp_plugin::ParamValue::Bool(b) => Some(*b), _ => None })
                                                .unwrap_or(false);
                                            if ui.checkbox(&mut v, "").changed() {
                                                cmds.push(AppCommand::SetEffectParam {
                                                    clip_id,
                                                    effect_index: i,
                                                    param_name: desc.name.clone(),
                                                    value: klipp_plugin::OfxValue::Bool(v),
                                                });
                                            }
                                        }
                                        klipp_plugin::ParamType::Choice => {
                                            let mut v = current_value
                                                .and_then(|cv| cv.as_int())
                                                .or(match &desc.default { klipp_plugin::ParamValue::Choice(c) => Some(*c), _ => None })
                                                .unwrap_or(0);
                                            let selected_text = desc.choices.get(v as usize)
                                                .map(|s| s.as_str())
                                                .unwrap_or("");
                                            egui::ComboBox::from_id_salt(egui::Id::new(("ofx_choice", clip_id, i, &desc.name)))
                                                .selected_text(selected_text)
                                                .show_ui(ui, |ui| {
                                                    for (ci, choice) in desc.choices.iter().enumerate() {
                                                        if ui.selectable_value(&mut v, ci as i32, choice).changed() {
                                                            cmds.push(AppCommand::SetEffectParam {
                                                                clip_id,
                                                                effect_index: i,
                                                                param_name: desc.name.clone(),
                                                                value: klipp_plugin::OfxValue::Choice(v),
                                                            });
                                                        }
                                                    }
                                                });
                                        }
                                        klipp_plugin::ParamType::Rgba => {
                                            let (r, g, b, a) = match current_value {
                                                Some(klipp_plugin::OfxValue::Rgba(r, g, b, a)) => (*r, *g, *b, *a),
                                                _ => match &desc.default {
                                                    klipp_plugin::ParamValue::Rgba(r, g, b, a) => (*r, *g, *b, *a),
                                                    _ => (1.0, 1.0, 1.0, 1.0),
                                                },
                                            };
                                            let mut color = egui::Color32::from_rgba_unmultiplied(
                                                (r * 255.0) as u8, (g * 255.0) as u8,
                                                (b * 255.0) as u8, (a * 255.0) as u8,
                                            );
                                            if ui.color_edit_button_srgba(&mut color).changed() {
                                                cmds.push(AppCommand::SetEffectParam {
                                                    clip_id,
                                                    effect_index: i,
                                                    param_name: desc.name.clone(),
                                                    value: klipp_plugin::OfxValue::Rgba(
                                                        color.r() as f64 / 255.0,
                                                        color.g() as f64 / 255.0,
                                                        color.b() as f64 / 255.0,
                                                        color.a() as f64 / 255.0,
                                                    ),
                                                });
                                            }
                                        }
                                        klipp_plugin::ParamType::Rgb => {
                                            let (r, g, b) = match current_value {
                                                Some(klipp_plugin::OfxValue::Rgb(r, g, b)) => (*r, *g, *b),
                                                _ => match &desc.default {
                                                    klipp_plugin::ParamValue::Rgb(r, g, b) => (*r, *g, *b),
                                                    _ => (1.0, 1.0, 1.0),
                                                },
                                            };
                                            let mut color = egui::Color32::from_rgb(
                                                (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8,
                                            );
                                            if ui.color_edit_button_srgba(&mut color).changed() {
                                                cmds.push(AppCommand::SetEffectParam {
                                                    clip_id,
                                                    effect_index: i,
                                                    param_name: desc.name.clone(),
                                                    value: klipp_plugin::OfxValue::Rgb(
                                                        color.r() as f64 / 255.0,
                                                        color.g() as f64 / 255.0,
                                                        color.b() as f64 / 255.0,
                                                    ),
                                                });
                                            }
                                        }
                                        klipp_plugin::ParamType::String => {
                                            let mut v = match current_value {
                                                Some(klipp_plugin::OfxValue::String(s)) => s.clone(),
                                                _ => match &desc.default {
                                                    klipp_plugin::ParamValue::String(s) => s.clone(),
                                                    _ => String::new(),
                                                },
                                            };
                                            if ui.text_edit_singleline(&mut v).changed() {
                                                cmds.push(AppCommand::SetEffectParam {
                                                    clip_id,
                                                    effect_index: i,
                                                    param_name: desc.name.clone(),
                                                    value: klipp_plugin::OfxValue::String(v),
                                                });
                                            }
                                        }
                                        _ => {
                                            ui.label(egui::RichText::new(format!("{:?}", desc.param_type)).size(10.0).color(egui::Color32::from_rgb(100, 100, 110)));
                                        }
                                    }
                                });
                            }
                        });
                }
            }

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

        // Add OFX plugin popup
        if self.show_add_effect {
            ui.add_space(4.0);
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

fn builtin_effect_name(effect: &klipp_effects::BuiltinEffect) -> &'static str {
    match effect {
        klipp_effects::BuiltinEffect::Opacity { .. } => "Opacity",
        klipp_effects::BuiltinEffect::GaussianBlur { .. } => "Gaussian Blur",
        klipp_effects::BuiltinEffect::Sharpen { .. } => "Sharpen",
        klipp_effects::BuiltinEffect::Crop { .. } => "Crop",
        klipp_effects::BuiltinEffect::Transform { .. } => "Transform",
        klipp_effects::BuiltinEffect::ChromaKey { .. } => "Chroma Key",
        klipp_effects::BuiltinEffect::Bloom { .. } => "Bloom",
        klipp_effects::BuiltinEffect::Glow { .. } => "Glow",
        klipp_effects::BuiltinEffect::LightRays { .. } => "Light Rays",
    }
}

/// Show parameter sliders for a built-in effect. Returns Some(updated) if any value changed.
fn show_builtin_effect_params(
    ui: &mut egui::Ui,
    effect: &klipp_effects::BuiltinEffect,
    clip_id: klipp_core::ClipId,
    effect_index: usize,
    cmds: &mut Vec<AppCommand>,
) -> Option<klipp_effects::BuiltinEffect> {
    let mut changed = false;
    let mut updated = effect.clone();
    let mut any_drag_started = false;
    let mut any_drag_stopped = false;
    let drag_key = format!("{:?}:builtin:{}", clip_id, effect_index);

    macro_rules! track_slider {
        ($resp:expr) => {{
            let r = $resp;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
        }};
    }

    match &mut updated {
        klipp_effects::BuiltinEffect::Opacity { value } => {
            track_slider!(ui.add(egui::Slider::new(value, 0.0..=1.0).text("Opacity")));
        }
        klipp_effects::BuiltinEffect::GaussianBlur { radius } => {
            track_slider!(ui.add(egui::Slider::new(radius, 0.0..=50.0).text("Radius")));
        }
        klipp_effects::BuiltinEffect::Sharpen { amount } => {
            track_slider!(ui.add(egui::Slider::new(amount, 0.0..=2.0).text("Amount")));
        }
        klipp_effects::BuiltinEffect::Crop { left, right, top, bottom, feather } => {
            track_slider!(ui.add(egui::Slider::new(left, 0.0..=0.5).text("Left")));
            track_slider!(ui.add(egui::Slider::new(right, 0.0..=0.5).text("Right")));
            track_slider!(ui.add(egui::Slider::new(top, 0.0..=0.5).text("Top")));
            track_slider!(ui.add(egui::Slider::new(bottom, 0.0..=0.5).text("Bottom")));
            track_slider!(ui.add(egui::Slider::new(feather, 0.0..=100.0).text("Feather")));
        }
        klipp_effects::BuiltinEffect::Transform {
            position_x, position_y, scale_x, scale_y,
            rotation, anchor_x, anchor_y,
        } => {
            track_slider!(ui.add(egui::Slider::new(position_x, -1920.0..=1920.0).text("Position X")));
            track_slider!(ui.add(egui::Slider::new(position_y, -1080.0..=1080.0).text("Position Y")));
            track_slider!(ui.add(egui::Slider::new(scale_x, 0.01..=4.0).text("Scale X")));
            track_slider!(ui.add(egui::Slider::new(scale_y, 0.01..=4.0).text("Scale Y")));
            track_slider!(ui.add(egui::Slider::new(rotation, -360.0..=360.0).text("Rotation")));
            track_slider!(ui.add(egui::Slider::new(anchor_x, 0.0..=1.0).text("Anchor X")));
            track_slider!(ui.add(egui::Slider::new(anchor_y, 0.0..=1.0).text("Anchor Y")));
        }
        klipp_effects::BuiltinEffect::ChromaKey { key_color, tolerance, spill_suppression } => {
            let mut color = egui::Color32::from_rgb(key_color[0], key_color[1], key_color[2]);
            if ui.color_edit_button_srgba(&mut color).changed() {
                key_color[0] = color.r();
                key_color[1] = color.g();
                key_color[2] = color.b();
                changed = true;
            }
            track_slider!(ui.add(egui::Slider::new(tolerance, 0.0..=1.0).text("Tolerance")));
            track_slider!(ui.add(egui::Slider::new(spill_suppression, 0.0..=1.0).text("Spill")));
        }
        klipp_effects::BuiltinEffect::Bloom { threshold, radius, intensity } => {
            track_slider!(ui.add(egui::Slider::new(threshold, 0.0..=1.0).text("Threshold")));
            track_slider!(ui.add(egui::Slider::new(radius, 0.0..=100.0).text("Radius")));
            track_slider!(ui.add(egui::Slider::new(intensity, 0.0..=2.0).text("Intensity")));
        }
        klipp_effects::BuiltinEffect::Glow { radius, intensity } => {
            track_slider!(ui.add(egui::Slider::new(radius, 0.0..=100.0).text("Radius")));
            track_slider!(ui.add(egui::Slider::new(intensity, 0.0..=2.0).text("Intensity")));
        }
        klipp_effects::BuiltinEffect::LightRays { center_x, center_y, threshold, samples, decay, intensity } => {
            track_slider!(ui.add(egui::Slider::new(center_x, 0.0..=1.0).text("Center X")));
            track_slider!(ui.add(egui::Slider::new(center_y, 0.0..=1.0).text("Center Y")));
            track_slider!(ui.add(egui::Slider::new(threshold, 0.0..=1.0).text("Threshold")));
            let mut samples_f = *samples as f32;
            let r = ui.add(egui::Slider::new(&mut samples_f, 4.0..=128.0).text("Samples"));
            *samples = samples_f as u32;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
            track_slider!(ui.add(egui::Slider::new(decay, 0.0..=1.0).text("Decay")));
            track_slider!(ui.add(egui::Slider::new(intensity, 0.0..=2.0).text("Intensity")));
        }
    }

    if any_drag_started {
        cmds.push(AppCommand::BeginEffectDrag {
            key: drag_key.clone(),
            clip_id,
            effect_index,
            old_effect: effect.clone(),
        });
    }
    if any_drag_stopped {
        cmds.push(AppCommand::EndEffectDrag {
            key: drag_key,
            clip_id,
            effect_index,
            new_effect: updated.clone(),
        });
    }

    if changed { Some(updated) } else { None }
}

// ─── Rich text selection helpers ────────────────────────────────────────────

/// Get the effective color for a selection range. If all characters in the range
/// share the same color, return that color; otherwise return the base title color.
fn selection_color(td: &klipp_timeline::TitleData, sel_start: usize, sel_end: usize) -> [u8; 4] {
    if td.style_runs.is_empty() {
        return td.color;
    }
    let mut found_color: Option<[u8; 4]> = None;
    for run in &td.style_runs {
        // Check overlap with selection
        if run.end <= sel_start || run.start >= sel_end {
            continue;
        }
        let c = run.color.unwrap_or(td.color);
        match found_color {
            None => found_color = Some(c),
            Some(prev) => {
                if prev != c {
                    return td.color; // Mixed — return base
                }
            }
        }
    }
    found_color.unwrap_or(td.color)
}

/// Get the effective font size for a selection range.
fn selection_font_size(td: &klipp_timeline::TitleData, sel_start: usize, sel_end: usize) -> f32 {
    if td.style_runs.is_empty() {
        return td.font_size;
    }
    let mut found_size: Option<f32> = None;
    for run in &td.style_runs {
        if run.end <= sel_start || run.start >= sel_end {
            continue;
        }
        let s = run.font_size.unwrap_or(td.font_size);
        match found_size {
            None => found_size = Some(s),
            Some(prev) => {
                if (prev - s).abs() > 0.1 {
                    return td.font_size; // Mixed
                }
            }
        }
    }
    found_size.unwrap_or(td.font_size)
}
