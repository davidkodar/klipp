use crate::app::format_size_brief;
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
    // Collapsible section state for Info tab
    info_clip_open: bool,
    info_tags_open: bool,
    info_timeline_open: bool,
    // Tag input buffer
    tag_input: String,
}

impl Default for InspectorPanel {
    fn default() -> Self {
        Self {
            active_tab: InspectorTab::Info,
            show_add_effect: false,
            show_add_builtin: false,
            info_clip_open: true,
            info_tags_open: true,
            info_timeline_open: true,
            tag_input: String::new(),
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
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    self.show_color_tab_redirect(ui, state, &mut cmds);
                    self.show_global_lut_section(ui, state, &mut cmds);
                });
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
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        track_idx: usize,
        clip: &klipp_timeline::Clip,
        media: &klipp_media::MediaItem,
        cmds: &mut Vec<AppCommand>,
    ) {
        let pad = 14.0;
        let row_h = 30.0;
        let value_color = egui::Color32::from_rgb(220, 220, 230);
        let accent = egui::Color32::from_rgb(232, 160, 32);
        let section_bg = egui::Color32::from_rgb(24, 24, 28);
        let card_bg = egui::Color32::from_rgb(30, 30, 36);
        let border = egui::Color32::from_rgb(44, 44, 52);
        let dim = egui::Color32::from_rgb(100, 100, 115);

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(8.0);

            // ════════════════════════════════════════
            // CLIP INFO section
            // ════════════════════════════════════════
            self.info_clip_open = Self::section_card(
                ui, "CLIP INFO", self.info_clip_open, None,
                section_bg, card_bg, border, pad, |ui| {
                    let w = ui.available_width();

                    // Resolution
                    Self::info_row(ui, pad, row_h, w, border, "Resolution", |ui, right_x, cy| {
                        let text = format!("{} \u{00D7} {}", media.resolution.width, media.resolution.height);
                        ui.painter().text(
                            egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                            text, egui::FontId::proportional(13.0), value_color,
                        );
                    });

                    // Frame rate
                    Self::info_row(ui, pad, row_h, w, border, "Frame rate", |ui, right_x, cy| {
                        let fps_val = media.fps.as_f64();
                        let fps_text = if (fps_val - fps_val.round()).abs() < 0.01 {
                            format!("{}  fps", fps_val.round() as u32)
                        } else {
                            format!("{:.3}  fps", fps_val)
                        };
                        ui.painter().text(
                            egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                            fps_text, egui::FontId::proportional(13.0), value_color,
                        );
                    });

                    // Codec (pill badges)
                    Self::info_row(ui, pad, row_h, w, border, "Codec", |ui, right_x, cy| {
                        let mut x = right_x;
                        let pill_h = 20.0;
                        let pill_bg = egui::Color32::from_rgb(40, 40, 48);
                        let pill_border = egui::Color32::from_rgb(60, 60, 70);

                        // Audio codec pill
                        if !media.audio_codec.is_empty() {
                            let codec = media.audio_codec.to_uppercase();
                            let galley = ui.painter().layout_no_wrap(
                                codec.clone(), egui::FontId::monospace(11.0), value_color,
                            );
                            let pw = galley.size().x + 12.0;
                            let pill_rect = egui::Rect::from_min_size(
                                egui::pos2(x - pw, cy - pill_h / 2.0),
                                egui::vec2(pw, pill_h),
                            );
                            ui.painter().rect(pill_rect, 4.0, pill_bg, egui::Stroke::new(1.0, pill_border));
                            ui.painter().text(
                                pill_rect.center(), egui::Align2::CENTER_CENTER,
                                codec, egui::FontId::monospace(11.0), value_color,
                            );
                            x -= pw + 6.0;
                        }

                        // Video codec pill
                        if !media.video_codec.is_empty() {
                            let codec = media.video_codec.to_uppercase();
                            let galley = ui.painter().layout_no_wrap(
                                codec.clone(), egui::FontId::monospace(11.0), value_color,
                            );
                            let pw = galley.size().x + 12.0;
                            let pill_rect = egui::Rect::from_min_size(
                                egui::pos2(x - pw, cy - pill_h / 2.0),
                                egui::vec2(pw, pill_h),
                            );
                            ui.painter().rect(pill_rect, 4.0, pill_bg, egui::Stroke::new(1.0, pill_border));
                            ui.painter().text(
                                pill_rect.center(), egui::Align2::CENTER_CENTER,
                                codec, egui::FontId::monospace(11.0), value_color,
                            );
                        }
                    });

                    // Audio (channel bars + label)
                    if media.has_audio {
                        Self::info_row(ui, pad, row_h, w, border, "Audio", |ui, right_x, cy| {
                            let channels = media.audio_channels.unwrap_or(2);
                            let sample_rate = media.sample_rate.unwrap_or(48000);
                            let ch_label = match channels {
                                1 => "Mono",
                                2 => "Stereo",
                                6 => "5.1",
                                8 => "7.1",
                                _ => "",
                            };
                            let sr_khz = sample_rate as f64 / 1000.0;
                            let sr_text = if sr_khz == sr_khz.floor() {
                                format!("{} kHz", sr_khz as u32)
                            } else {
                                format!("{:.1} kHz", sr_khz)
                            };
                            let label = format!("{ch_label} \u{00B7} {sr_text}");

                            let label_galley = ui.painter().layout_no_wrap(
                                label.clone(), egui::FontId::proportional(12.0), dim,
                            );
                            let label_w = label_galley.size().x;

                            // Draw text
                            ui.painter().text(
                                egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                                label, egui::FontId::proportional(12.0), dim,
                            );

                            // Draw channel bars
                            let bars_x = right_x - label_w - 10.0;
                            let bar_w = 4.0;
                            let bar_gap = 2.0;
                            let max_bar_h = 14.0;
                            let bar_heights: &[f32] = match channels {
                                1 => &[0.8],
                                2 => &[0.9, 0.7],
                                6 => &[0.7, 0.9, 1.0, 0.8, 0.6, 0.5],
                                8 => &[0.7, 0.9, 1.0, 0.8, 0.6, 0.5, 0.7, 0.6],
                                _ => &[0.8, 0.7],
                            };
                            let total_bars_w = bar_heights.len() as f32 * (bar_w + bar_gap) - bar_gap;
                            let bx = bars_x - total_bars_w;
                            for (i, &h_frac) in bar_heights.iter().enumerate() {
                                let bh = max_bar_h * h_frac;
                                let bar_rect = egui::Rect::from_min_size(
                                    egui::pos2(bx + i as f32 * (bar_w + bar_gap), cy - bh / 2.0),
                                    egui::vec2(bar_w, bh),
                                );
                                ui.painter().rect_filled(bar_rect, 1.0, accent);
                            }
                        });
                    }

                    // Duration
                    Self::info_row(ui, pad, row_h, w, border, "Duration", |ui, right_x, cy| {
                        ui.painter().text(
                            egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                            media.duration.to_timecode_string(),
                            egui::FontId::proportional(13.0), value_color,
                        );
                    });

                    // File size
                    Self::info_row(ui, pad, row_h, w, border, "File size", |ui, right_x, cy| {
                        let size_str = format_size_brief(media.file_size_bytes);
                        ui.painter().text(
                            egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                            size_str, egui::FontId::proportional(13.0), value_color,
                        );
                    });

                    // Colour space
                    Self::info_row(ui, pad, row_h, w, border, "Colour", |ui, right_x, cy| {
                        use klipp_media::ColourSpace;
                        let (badge_text, badge_color, sub_text) = match media.colour_space {
                            ColourSpace::Sdr => ("SDR", egui::Color32::from_rgb(60, 120, 200), "BT.709"),
                            ColourSpace::Hdr10 => ("HDR10", egui::Color32::from_rgb(220, 140, 30), "BT.2020"),
                            ColourSpace::Hlg => ("HLG", egui::Color32::from_rgb(200, 100, 40), "BT.2020"),
                            ColourSpace::Unknown => ("SDR", egui::Color32::from_rgb(60, 120, 200), "BT.709"),
                        };

                        // Sub-text
                        let sub_galley = ui.painter().layout_no_wrap(
                            sub_text.to_string(), egui::FontId::proportional(12.0), dim,
                        );
                        let sub_w = sub_galley.size().x;
                        ui.painter().text(
                            egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                            sub_text, egui::FontId::proportional(12.0), dim,
                        );

                        // Badge
                        let badge_galley = ui.painter().layout_no_wrap(
                            badge_text.to_string(), egui::FontId::proportional(11.0), egui::Color32::WHITE,
                        );
                        let bw = badge_galley.size().x + 10.0;
                        let bh = 18.0;
                        let badge_rect = egui::Rect::from_min_size(
                            egui::pos2(right_x - sub_w - 8.0 - bw, cy - bh / 2.0),
                            egui::vec2(bw, bh),
                        );
                        ui.painter().rect_filled(badge_rect, 3.0, badge_color);
                        ui.painter().text(
                            badge_rect.center(), egui::Align2::CENTER_CENTER,
                            badge_text, egui::FontId::proportional(11.0),
                            egui::Color32::WHITE,
                        );
                    });

                    // Shoot date
                    if let Some(created) = media.media_created_at.filter(|&t| t > 0) {
                        Self::info_row(ui, pad, row_h, w, border, "Shoot date", |ui, right_x, cy| {
                            let dt = chrono::DateTime::from_timestamp(created as i64, 0);
                            let text = dt.map(|d| d.format("%d %b %Y, %H:%M").to_string())
                                .unwrap_or_else(|| "Unknown".into());
                            ui.painter().text(
                                egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                                text, egui::FontId::proportional(13.0), value_color,
                            );
                        });
                    }

                    // Proxy status
                    Self::info_row(ui, pad, row_h, w, border, "Proxy", |ui, right_x, cy| {
                        let is_generating = state.proxy_generating.contains(&media.id);
                        if media.proxy_ready {
                            ui.painter().text(
                                egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                                "Ready", egui::FontId::proportional(13.0),
                                egui::Color32::from_rgb(100, 200, 120),
                            );
                        } else if is_generating {
                            let progress = state.proxy_progress.get(&media.id).copied().unwrap_or(0.0);
                            ui.painter().text(
                                egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                                format!("{:.0}%", progress * 100.0),
                                egui::FontId::proportional(13.0),
                                egui::Color32::from_rgb(220, 180, 60),
                            );
                            ui.ctx().request_repaint();
                        } else {
                            ui.painter().text(
                                egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                                "None", egui::FontId::proportional(13.0), dim,
                            );
                        }
                    });
                },
            );

            ui.add_space(8.0);

            // ════════════════════════════════════════
            // TAGS section
            // ════════════════════════════════════════
            let tag_count = media.tags.len();
            self.info_tags_open = Self::section_card(
                ui, "TAGS", self.info_tags_open,
                Some(format!("{tag_count}")),
                section_bg, card_bg, border, pad, |ui| {
                    if media.tags.is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("No tags added")
                                .italics()
                                .size(12.0)
                                .color(dim),
                        );
                        ui.add_space(6.0);
                    } else {
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            let mut remove_tag = None;
                            for (i, tag) in media.tags.iter().enumerate() {
                                let tag_resp = ui.add(
                                    egui::Button::new(
                                        egui::RichText::new(tag).size(11.0)
                                    )
                                    .rounding(10.0)
                                    .fill(egui::Color32::from_rgb(50, 50, 60))
                                );
                                if tag_resp.secondary_clicked() {
                                    remove_tag = Some(i);
                                }
                                tag_resp.on_hover_text("Right-click to remove");
                            }
                            if let Some(idx) = remove_tag {
                                cmds.push(AppCommand::RemoveMediaTag {
                                    media_id: media.id,
                                    tag_index: idx,
                                });
                            }
                        });
                        ui.add_space(6.0);
                    }

                    // Add tag input
                    let w = ui.available_width() - pad * 2.0;
                    ui.horizontal(|ui| {
                        ui.add_space(pad);
                        let add_rect = egui::Rect::from_min_size(
                            ui.cursor().min,
                            egui::vec2(w, 28.0),
                        );
                        let resp = ui.allocate_rect(add_rect, egui::Sense::click());
                        ui.painter().rect(
                            add_rect, 6.0,
                            egui::Color32::from_rgb(34, 34, 40),
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(52, 52, 60)),
                        );
                        ui.painter().text(
                            egui::pos2(add_rect.min.x + 10.0, add_rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            "+  Add tag\u{2026}",
                            egui::FontId::proportional(12.0),
                            dim,
                        );
                        if resp.clicked() {
                            // We'll use a simple popup for tag input
                            ui.memory_mut(|mem| mem.open_popup(egui::Id::new("add_tag_popup")));
                        }
                    });
                    ui.add_space(8.0);
                },
            );

            // Tag input popup
            let popup_id = egui::Id::new("add_tag_popup");
            if ui.memory(|mem| mem.is_popup_open(popup_id)) {
                egui::Area::new(popup_id)
                    .order(egui::Order::Foreground)
                    .fixed_pos(ui.cursor().min + egui::vec2(pad, 0.0))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let resp = ui.text_edit_singleline(&mut self.tag_input);
                                resp.request_focus();
                                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    let tag = self.tag_input.trim().to_string();
                                    if !tag.is_empty() {
                                        cmds.push(AppCommand::AddMediaTag {
                                            media_id: media.id,
                                            tag,
                                        });
                                    }
                                    self.tag_input.clear();
                                    ui.memory_mut(|mem| mem.close_popup());
                                }
                                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                    self.tag_input.clear();
                                    ui.memory_mut(|mem| mem.close_popup());
                                }
                            });
                        });
                    });
            }

            ui.add_space(8.0);

            // ════════════════════════════════════════
            // TIMELINE section
            // ════════════════════════════════════════
            self.info_timeline_open = Self::section_card(
                ui, "TIMELINE", self.info_timeline_open, None,
                section_bg, card_bg, border, pad, |ui| {
                    let w = ui.available_width();

                    // Progress bar — clip position in sequence
                    let seq_dur = state.sequence_duration().to_secs_f64();
                    let clip_in = clip.timeline_in.to_secs_f64();
                    let clip_out = clip.timeline_out.to_secs_f64();
                    if seq_dur > 0.0 {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(w - pad * 2.0, 6.0), egui::Sense::hover(),
                        );
                        // Background bar
                        ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgb(40, 40, 48));
                        // Clip range highlight
                        let start_frac = (clip_in / seq_dur).clamp(0.0, 1.0) as f32;
                        let end_frac = (clip_out / seq_dur).clamp(0.0, 1.0) as f32;
                        let highlight = egui::Rect::from_min_max(
                            egui::pos2(rect.min.x + rect.width() * start_frac, rect.min.y),
                            egui::pos2(rect.min.x + rect.width() * end_frac, rect.max.y),
                        );
                        ui.painter().rect_filled(highlight, 2.0, accent);

                        // Timecode labels under bar
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.add_space(pad);
                            ui.colored_label(dim,
                                egui::RichText::new(clip.timeline_in.to_timecode_string()).monospace().size(10.0));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_space(pad);
                                ui.colored_label(dim,
                                    egui::RichText::new(clip.timeline_out.to_timecode_string()).monospace().size(10.0));
                            });
                        });
                    }
                    ui.add_space(4.0);

                    // In
                    Self::info_row(ui, pad, row_h, w, border, "In", |ui, right_x, cy| {
                        ui.painter().text(
                            egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                            clip.timeline_in.to_timecode_string(),
                            egui::FontId::monospace(13.0), value_color,
                        );
                    });

                    // Out
                    Self::info_row(ui, pad, row_h, w, border, "Out", |ui, right_x, cy| {
                        ui.painter().text(
                            egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                            clip.timeline_out.to_timecode_string(),
                            egui::FontId::monospace(13.0), value_color,
                        );
                    });

                    // Duration (accent color)
                    Self::info_row(ui, pad, row_h, w, border, "Duration", |ui, right_x, cy| {
                        ui.painter().text(
                            egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                            clip.duration().to_timecode_string(),
                            egui::FontId::monospace(13.0), accent,
                        );
                    });

                    // Track
                    Self::info_row(ui, pad, row_h, w, border, "Track", |ui, right_x, cy| {
                        let track_name = state.sequence().tracks.get(track_idx)
                            .map(|t| t.name.clone())
                            .unwrap_or_default();
                        ui.painter().text(
                            egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                            track_name,
                            egui::FontId::proportional(13.0),
                            egui::Color32::from_rgb(80, 160, 220),
                        );
                    });

                    // Speed
                    Self::info_row(ui, pad, row_h, w, border, "Speed", |ui, right_x, cy| {
                        let speed = clip.speed;
                        let color = if (speed - 1.0).abs() < 0.001 {
                            egui::Color32::from_rgb(100, 200, 120) // green = normal
                        } else if speed > 1.0 {
                            egui::Color32::from_rgb(220, 180, 60) // amber = fast
                        } else {
                            egui::Color32::from_rgb(200, 80, 80) // red = slow
                        };
                        ui.painter().text(
                            egui::pos2(right_x, cy), egui::Align2::RIGHT_CENTER,
                            format!("{:.0}%", speed * 100.0),
                            egui::FontId::proportional(13.0), color,
                        );
                    });
                },
            );

            // Offline warning (outside cards)
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

            ui.add_space(16.0);
        });
    }

    /// Draw a collapsible section card. Returns the new open state.
    fn section_card(
        ui: &mut egui::Ui,
        title: &str,
        open: bool,
        badge: Option<String>,
        header_bg: egui::Color32,
        card_bg: egui::Color32,
        border_color: egui::Color32,
        pad: f32,
        add_contents: impl FnOnce(&mut egui::Ui),
    ) -> bool {
        let mut is_open = open;
        let w = ui.available_width();
        let header_h = 30.0;
        let dim = egui::Color32::from_rgb(100, 100, 115);

        // Card outer frame
        let outer_start = ui.cursor().min;

        // Header
        let (header_rect, header_resp) = ui.allocate_exact_size(
            egui::vec2(w, header_h), egui::Sense::click(),
        );
        ui.painter().rect_filled(header_rect, egui::Rounding { nw: 6.0, ne: 6.0, sw: if is_open { 0.0 } else { 6.0 }, se: if is_open { 0.0 } else { 6.0 }}, header_bg);

        // Left border accent
        let accent_rect = egui::Rect::from_min_size(
            header_rect.min,
            egui::vec2(3.0, header_h),
        );
        ui.painter().rect_filled(accent_rect, egui::Rounding { nw: 6.0, ne: 0.0, sw: if is_open { 0.0 } else { 6.0 }, se: 0.0 }, border_color);

        // Title
        ui.painter().text(
            egui::pos2(header_rect.min.x + pad + 4.0, header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            title,
            egui::FontId::proportional(11.0),
            egui::Color32::from_rgb(140, 140, 155),
        );

        // Badge (tag count etc.)
        if let Some(ref badge_text) = badge {
            ui.painter().text(
                egui::pos2(header_rect.max.x - pad - 16.0, header_rect.center().y),
                egui::Align2::RIGHT_CENTER,
                badge_text,
                egui::FontId::proportional(11.0),
                dim,
            );
        }

        // Collapse arrow
        let arrow = if is_open { "\u{25BE}" } else { "\u{25B8}" };
        ui.painter().text(
            egui::pos2(header_rect.max.x - pad, header_rect.center().y),
            egui::Align2::RIGHT_CENTER,
            arrow,
            egui::FontId::proportional(11.0),
            dim,
        );

        if header_resp.clicked() {
            is_open = !is_open;
        }

        // Body
        if is_open {
            let body_start = ui.cursor().min;
            // Draw card body background (will be sized after contents)
            let frame = egui::Frame::default()
                .fill(card_bg)
                .inner_margin(egui::Margin { left: 0.0, right: 0.0, top: 4.0, bottom: 0.0 })
                .stroke(egui::Stroke::NONE)
                .shadow(egui::Shadow::NONE);

            frame.show(ui, |ui| {
                add_contents(ui);
            });

            // Bottom border
            let body_end_y = ui.cursor().min.y;
            let body_rect = egui::Rect::from_min_max(
                body_start,
                egui::pos2(body_start.x + w, body_end_y),
            );
            // Left border on body
            let body_accent = egui::Rect::from_min_size(
                body_rect.min,
                egui::vec2(3.0, body_rect.height()),
            );
            ui.painter().rect_filled(body_accent, 0.0, border_color);

            // Card outline
            let full_card = egui::Rect::from_min_max(
                outer_start,
                egui::pos2(outer_start.x + w, body_end_y),
            );
            ui.painter().rect_stroke(full_card, 6.0, egui::Stroke::new(0.5, border_color));
        } else {
            // Outline for collapsed header
            ui.painter().rect_stroke(header_rect, 6.0, egui::Stroke::new(0.5, border_color));
        }

        is_open
    }

    /// Draw a single key-value row in a section.
    fn info_row(
        ui: &mut egui::Ui,
        pad: f32,
        row_h: f32,
        total_w: f32,
        border: egui::Color32,
        label: &str,
        draw_value: impl FnOnce(&mut egui::Ui, f32, f32),
    ) {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(total_w, row_h), egui::Sense::hover(),
        );

        // Subtle separator line at bottom
        ui.painter().hline(
            rect.min.x + pad..=rect.max.x - pad,
            rect.max.y,
            egui::Stroke::new(0.5, border),
        );

        // Label (left)
        ui.painter().text(
            egui::pos2(rect.min.x + pad, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(13.0),
            egui::Color32::from_rgb(160, 160, 170),
        );

        // Value (right) — callback
        let right_x = rect.max.x - pad;
        let cy = rect.center().y;
        draw_value(ui, right_x, cy);
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
                    (LF::AppleLog, "Apple Log"),
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

    /// Show the Color tab content: color redirect, global LUT, and LUT presets.
    /// Called from both panels/inspector.rs and inspector/mod.rs.
    pub fn show_color_and_luts(
        ui: &mut egui::Ui,
        state: &AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        egui::ScrollArea::vertical().auto_shrink([false; 2]).id_salt("color_lut_scroll").show(ui, |ui| {
            // Input Color Space selector
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
                    ui.label(egui::RichText::new("Input Color Space").size(10.0)
                        .color(egui::Color32::from_rgb(0x90, 0x90, 0xa0)));
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    use klipp_core::LogFormat as LF;
                    let formats = [
                        (LF::Srgb, "sRGB / Rec.709"), (LF::Linear, "Linear"),
                        (LF::LogC3, "ARRI LogC3"), (LF::LogC4, "ARRI LogC4"),
                        (LF::SLog2, "Sony S-Log2"), (LF::SLog3, "Sony S-Log3"),
                        (LF::CanonLog, "Canon Log"), (LF::CanonLog3, "Canon Log 3"),
                        (LF::VLog, "Panasonic V-Log"), (LF::NLog, "Nikon N-Log"),
                        (LF::FLog, "Fuji F-Log"), (LF::DJIDLog, "DJI D-Log"),
                        (LF::AppleLog, "Apple Log"),
                    ];
                    let current_label = formats.iter()
                        .find(|(f, _)| *f == current_log)
                        .map(|(_, l)| *l)
                        .unwrap_or("sRGB / Rec.709");
                    egui::ComboBox::from_id_salt("color_tab_log_format")
                        .selected_text(egui::RichText::new(current_label).size(9.0))
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for &(fmt, label) in &formats {
                                if ui.selectable_label(current_log == fmt,
                                    egui::RichText::new(label).size(9.0),
                                ).clicked() {
                                    cmds.push(AppCommand::SetClipLogFormat { clip_id, log_format: fmt });
                                }
                            }
                        });
                });
                ui.add_space(8.0);
            }

            // Open Color Panel button
            ui.vertical_centered(|ui| {
                if ui.button(
                    egui::RichText::new("Open Color Panel  (C)")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(0xe8, 0xa0, 0x20)),
                ).clicked() {
                    cmds.push(AppCommand::ToggleColorPanel);
                }
            });

            // Global LUT
            ui.add_space(12.0);
            ui.label(egui::RichText::new("Global LUT").strong().size(12.0));
            ui.separator();
            ui.add_space(4.0);

            if let Some(path) = &state.global_lut_path {
                let filename = path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Unknown".into());
                // Enable/disable + filename + clear on one row
                ui.horizontal(|ui| {
                    let mut enabled = state.global_lut_enabled;
                    if ui.checkbox(&mut enabled, "").changed() {
                        cmds.push(AppCommand::SetGlobalLutEnabled(enabled));
                    }
                    let label_color = if state.global_lut_enabled {
                        egui::Color32::from_rgb(100, 200, 120)
                    } else {
                        egui::Color32::from_rgb(120, 120, 130)
                    };
                    ui.colored_label(label_color, &filename);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Clear").clicked() {
                            cmds.push(AppCommand::ClearGlobalLut);
                        }
                    });
                });
                ui.add_space(4.0);
                let mut opacity = state.global_lut_opacity;
                ui.label("Opacity");
                if ui.add(egui::Slider::new(&mut opacity, 0.0..=1.0)
                    .step_by(0.01).min_decimals(2)
                ).changed() {
                    cmds.push(AppCommand::SetGlobalLutOpacity(opacity));
                }
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

            // Bundled LUT Presets
            ui.add_space(8.0);
            ui.label(egui::RichText::new("LUT Presets").strong().size(12.0));
            ui.separator();
            ui.add_space(4.0);

            let luts_dir = Self::find_bundled_luts_dir();
            if let Some(ref base) = luts_dir {
                let categories = [
                    ("cinematic", "Cinematic"),
                    ("film-bw", "Film B&W"),
                    ("film-slide", "Film Slide"),
                    ("film-negative", "Film Negative"),
                    ("film-print", "Film Print"),
                    ("film-instant", "Film Instant"),
                    ("fuji-xtrans", "Fuji X-Trans"),
                ];
                for (dir_name, label) in &categories {
                    let cat_dir = base.join(dir_name);
                    if !cat_dir.exists() { continue; }
                    let id = egui::Id::new(format!("lut_preset_{dir_name}"));
                    egui::CollapsingHeader::new(*label)
                        .id_salt(id)
                        .show(ui, |ui| {
                            let mut entries: Vec<_> = std::fs::read_dir(&cat_dir)
                                .into_iter().flatten()
                                .filter_map(|e| e.ok())
                                .filter(|e| e.path().extension()
                                    .is_some_and(|ext| ext == "cube" || ext == "3dl" || ext == "csp"))
                                .collect();
                            entries.sort_by_key(|e| e.file_name());
                            for entry in entries {
                                let path = entry.path();
                                let name = path.file_stem()
                                    .map(|s| s.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                let is_active = state.global_lut_path.as_ref() == Some(&path);
                                let row_h = 22.0;
                                let (rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h),
                                    egui::Sense::click(),
                                );
                                // Background: subtle highlight for active, hover for others
                                if is_active {
                                    ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgba_premultiplied(232, 160, 32, 30));
                                } else if resp.hovered() {
                                    ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 12));
                                }
                                // Text: white for active, dim for others
                                let text_color = if is_active {
                                    egui::Color32::from_rgb(255, 255, 255)
                                } else {
                                    egui::Color32::from_rgb(180, 180, 190)
                                };
                                // Active indicator dot
                                if is_active {
                                    ui.painter().circle_filled(
                                        egui::pos2(rect.min.x + 8.0, rect.center().y),
                                        3.0,
                                        egui::Color32::from_rgb(232, 160, 32),
                                    );
                                }
                                ui.painter().text(
                                    egui::pos2(rect.min.x + if is_active { 16.0 } else { 8.0 }, rect.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    &name,
                                    egui::FontId::proportional(11.0),
                                    text_color,
                                );
                                if resp.clicked() {
                                    if is_active {
                                        cmds.push(AppCommand::ClearGlobalLut);
                                    } else {
                                        cmds.push(AppCommand::LoadGlobalLut { path });
                                    }
                                }
                            }
                        });
                }
            }
        });
    }

    /// Find the bundled LUTs directory. Checks:
    /// 1. Next to the executable (installed/packaged)
    /// 2. In the source tree (development)
    fn find_bundled_luts_dir() -> Option<std::path::PathBuf> {
        // Next to executable
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join("assets").join("luts");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
        // Source tree (CARGO_MANIFEST_DIR at compile time)
        let source_tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent() // crates/
            .and_then(|p| p.parent()) // workspace root
            .map(|p| p.join("assets").join("luts"));
        if let Some(ref p) = source_tree {
            if p.exists() {
                return source_tree;
            }
        }
        None
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

        // ── Bundled LUT Presets ──
        ui.add_space(8.0);
        ui.label(egui::RichText::new("LUT Presets").strong().size(12.0));
        ui.separator();
        ui.add_space(4.0);

        // Find bundled LUTs directory (next to the executable or in the source tree)
        let luts_dir = Self::find_bundled_luts_dir();
        if let Some(ref base) = luts_dir {
            let categories = [
                ("cinematic", "Cinematic"),
                ("film-bw", "Film B&W"),
                ("film-slide", "Film Slide"),
                ("film-negative", "Film Negative"),
                ("film-print", "Film Print"),
                ("film-instant", "Film Instant"),
                ("fuji-xtrans", "Fuji X-Trans"),
            ];

            for (dir_name, label) in &categories {
                let cat_dir = base.join(dir_name);
                if !cat_dir.exists() {
                    continue;
                }
                let id = egui::Id::new(format!("lut_cat_{dir_name}"));
                egui::CollapsingHeader::new(*label)
                    .id_salt(id)
                    .show(ui, |ui| {
                        let mut entries: Vec<_> = std::fs::read_dir(&cat_dir)
                            .into_iter()
                            .flatten()
                            .filter_map(|e| e.ok())
                            .filter(|e| {
                                e.path().extension()
                                    .is_some_and(|ext| ext == "cube" || ext == "3dl" || ext == "csp")
                            })
                            .collect();
                        entries.sort_by_key(|e| e.file_name());

                        for entry in entries {
                            let path = entry.path();
                            let name = path.file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let is_active = state.global_lut_path.as_ref() == Some(&path);
                            if ui.selectable_label(is_active, &name).clicked() {
                                if is_active {
                                    cmds.push(AppCommand::ClearGlobalLut);
                                } else {
                                    cmds.push(AppCommand::LoadGlobalLut { path });
                                }
                            }
                        }
                    });
            }
        } else {
            ui.colored_label(
                egui::Color32::from_rgb(120, 120, 130),
                "No bundled LUTs found",
            );
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
        klipp_effects::BuiltinEffect::Vignette { .. } => "Vignette",
        klipp_effects::BuiltinEffect::Diffusion { .. } => "Diffusion",
        klipp_effects::BuiltinEffect::LensFlare { .. } => "Lens Flare",
        klipp_effects::BuiltinEffect::LightWrap { .. } => "Light Wrap",
        klipp_effects::BuiltinEffect::AnamorphicStreak { .. } => "Anamorphic Streak",
        klipp_effects::BuiltinEffect::FilmHalation { .. } => "Film Halation",
        klipp_effects::BuiltinEffect::Invert { .. } => "Invert",
        klipp_effects::BuiltinEffect::Posterize { .. } => "Posterize",
        klipp_effects::BuiltinEffect::Mosaic { .. } => "Mosaic",
        klipp_effects::BuiltinEffect::EdgeDetect { .. } => "Edge Detect",
        klipp_effects::BuiltinEffect::ChannelMixer { .. } => "Channel Mixer",
        klipp_effects::BuiltinEffect::DirectionalBlur { .. } => "Directional Blur",
        klipp_effects::BuiltinEffect::Emboss { .. } => "Emboss",
        klipp_effects::BuiltinEffect::Mirror { .. } => "Mirror",
        klipp_effects::BuiltinEffect::LensDistortion { .. } => "Lens Distortion",
        klipp_effects::BuiltinEffect::Dilate { .. } => "Dilate",
        klipp_effects::BuiltinEffect::Erode { .. } => "Erode",
        klipp_effects::BuiltinEffect::Denoise { .. } => "Denoise",
        klipp_effects::BuiltinEffect::Gamma { .. } => "Gamma",
        klipp_effects::BuiltinEffect::HSVAdjust { .. } => "HSV Adjust",
        klipp_effects::BuiltinEffect::NoiseGenerator { .. } => "Noise Generator",
        klipp_effects::BuiltinEffect::MedianFilter { .. } => "Median Filter",
        klipp_effects::BuiltinEffect::RadialBlur { .. } => "Radial Blur",
        klipp_effects::BuiltinEffect::Levels { .. } => "Levels",
        klipp_effects::BuiltinEffect::Equalize { .. } => "Equalize",
        klipp_effects::BuiltinEffect::Letterbox { .. } => "Letterbox",
        klipp_effects::BuiltinEffect::ChromaticAberration { .. } => "Chromatic Aberration",
        klipp_effects::BuiltinEffect::SpeedRamp { .. } => "Speed Ramp",
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
        klipp_effects::BuiltinEffect::Vignette { amount, size, softness, roundness } => {
            track_slider!(ui.add(egui::Slider::new(amount, -1.0..=1.0).text("Amount")));
            track_slider!(ui.add(egui::Slider::new(size, 0.0..=2.0).text("Size")));
            track_slider!(ui.add(egui::Slider::new(softness, 0.0..=2.0).text("Softness")));
            track_slider!(ui.add(egui::Slider::new(roundness, 0.0..=1.0).text("Roundness")));
        }
        klipp_effects::BuiltinEffect::Diffusion { amount, radius, threshold, desaturation } => {
            track_slider!(ui.add(egui::Slider::new(amount, 0.0..=1.0).text("Amount")));
            track_slider!(ui.add(egui::Slider::new(radius, 0.0..=100.0).text("Radius")));
            track_slider!(ui.add(egui::Slider::new(threshold, 0.0..=1.0).text("Threshold")));
            track_slider!(ui.add(egui::Slider::new(desaturation, 0.0..=1.0).text("Desaturation")));
        }
        klipp_effects::BuiltinEffect::LensFlare { position_x, position_y, brightness, flare_size, ghost_count, ghost_intensity, chromatic_aberration } => {
            track_slider!(ui.add(egui::Slider::new(position_x, 0.0..=1.0).text("Position X")));
            track_slider!(ui.add(egui::Slider::new(position_y, 0.0..=1.0).text("Position Y")));
            track_slider!(ui.add(egui::Slider::new(brightness, 0.0..=2.0).text("Brightness")));
            track_slider!(ui.add(egui::Slider::new(flare_size, 0.0..=1.0).text("Flare Size")));
            let mut gc_f = *ghost_count as f32;
            let r = ui.add(egui::Slider::new(&mut gc_f, 0.0..=8.0).text("Ghost Count"));
            *ghost_count = gc_f as u32;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
            track_slider!(ui.add(egui::Slider::new(ghost_intensity, 0.0..=1.0).text("Ghost Intensity")));
            track_slider!(ui.add(egui::Slider::new(chromatic_aberration, 0.0..=1.0).text("Chromatic Aberration")));
        }
        klipp_effects::BuiltinEffect::LightWrap { width: wrap_w, intensity, softness } => {
            track_slider!(ui.add(egui::Slider::new(wrap_w, 0.0..=100.0).text("Width")));
            track_slider!(ui.add(egui::Slider::new(intensity, 0.0..=1.0).text("Intensity")));
            track_slider!(ui.add(egui::Slider::new(softness, 0.0..=2.0).text("Softness")));
        }
        klipp_effects::BuiltinEffect::AnamorphicStreak { threshold, length, intensity, tint } => {
            track_slider!(ui.add(egui::Slider::new(threshold, 0.0..=1.0).text("Threshold")));
            track_slider!(ui.add(egui::Slider::new(length, 0.0..=200.0).text("Length")));
            track_slider!(ui.add(egui::Slider::new(intensity, 0.0..=2.0).text("Intensity")));
            track_slider!(ui.add(egui::Slider::new(tint, 0.0..=1.0).text("Tint")));
        }
        klipp_effects::BuiltinEffect::FilmHalation { threshold, radius, intensity, hue } => {
            track_slider!(ui.add(egui::Slider::new(threshold, 0.0..=1.0).text("Threshold")));
            track_slider!(ui.add(egui::Slider::new(radius, 0.0..=50.0).text("Radius")));
            track_slider!(ui.add(egui::Slider::new(intensity, 0.0..=1.0).text("Intensity")));
            track_slider!(ui.add(egui::Slider::new(hue, 0.0..=1.0).text("Hue")));
        }
        klipp_effects::BuiltinEffect::SpeedRamp { speed, blend_mode, ramp_curve, ramp_start, ramp_end, reverse, freeze_at, freeze } => {
            track_slider!(ui.add(egui::Slider::new(speed, 0.1..=4.0).step_by(0.05_f64).text("Speed")));

            let blend_labels = ["Nearest", "Linear"];
            let blend_sel = (*blend_mode as usize).min(blend_labels.len() - 1);
            ui.horizontal(|ui| {
                ui.label("Blend Mode");
                let combo_id = ui.make_persistent_id(format!("sr_blend_{clip_id:?}_{effect_index}"));
                egui::ComboBox::from_id_salt(combo_id)
                    .selected_text(blend_labels[blend_sel])
                    .show_ui(ui, |ui| {
                        for (i, label) in blend_labels.iter().enumerate() {
                            if ui.selectable_value(blend_mode, i as u32, *label).changed() {
                                changed = true;
                            }
                        }
                    });
            });

            let curve_labels = ["Constant", "Ease In", "Ease Out", "Ease In-Out", "S-Curve"];
            let curve_sel = (*ramp_curve as usize).min(curve_labels.len() - 1);
            ui.horizontal(|ui| {
                ui.label("Ramp Curve");
                let combo_id = ui.make_persistent_id(format!("sr_curve_{clip_id:?}_{effect_index}"));
                egui::ComboBox::from_id_salt(combo_id)
                    .selected_text(curve_labels[curve_sel])
                    .show_ui(ui, |ui| {
                        for (i, label) in curve_labels.iter().enumerate() {
                            if ui.selectable_value(ramp_curve, i as u32, *label).changed() {
                                changed = true;
                            }
                        }
                    });
            });

            track_slider!(ui.add(egui::Slider::new(ramp_start, 0.0..=1.0).text("Ramp Start")));
            track_slider!(ui.add(egui::Slider::new(ramp_end, 0.0..=1.0).text("Ramp End")));
            if ui.checkbox(reverse, "Reverse").changed() {
                changed = true;
            }
            if ui.checkbox(freeze, "Freeze Frame").changed() {
                changed = true;
            }
            if *freeze {
                track_slider!(ui.add(egui::Slider::new(freeze_at, 0.0..=1.0).text("Freeze At")));
            }
        }
        klipp_effects::BuiltinEffect::Invert { mix } => {
            track_slider!(ui.add(egui::Slider::new(mix, 0.0..=1.0).text("Mix")));
        }
        klipp_effects::BuiltinEffect::Posterize { levels } => {
            track_slider!(ui.add(egui::Slider::new(levels, 2.0..=256.0).text("Levels")));
        }
        klipp_effects::BuiltinEffect::Mosaic { block_size } => {
            track_slider!(ui.add(egui::Slider::new(block_size, 1.0..=100.0).text("Block Size")));
        }
        klipp_effects::BuiltinEffect::EdgeDetect { strength, mix } => {
            track_slider!(ui.add(egui::Slider::new(strength, 0.0..=5.0).text("Strength")));
            track_slider!(ui.add(egui::Slider::new(mix, 0.0..=1.0).text("Mix")));
        }
        klipp_effects::BuiltinEffect::ChannelMixer {
            red_red, red_green, red_blue,
            green_red, green_green, green_blue,
            blue_red, blue_green, blue_blue,
        } => {
            ui.label("Red Output");
            track_slider!(ui.add(egui::Slider::new(red_red, -1.0..=2.0).text("Red")));
            track_slider!(ui.add(egui::Slider::new(red_green, -1.0..=2.0).text("Green")));
            track_slider!(ui.add(egui::Slider::new(red_blue, -1.0..=2.0).text("Blue")));
            ui.label("Green Output");
            track_slider!(ui.add(egui::Slider::new(green_red, -1.0..=2.0).text("Red")));
            track_slider!(ui.add(egui::Slider::new(green_green, -1.0..=2.0).text("Green")));
            track_slider!(ui.add(egui::Slider::new(green_blue, -1.0..=2.0).text("Blue")));
            ui.label("Blue Output");
            track_slider!(ui.add(egui::Slider::new(blue_red, -1.0..=2.0).text("Red")));
            track_slider!(ui.add(egui::Slider::new(blue_green, -1.0..=2.0).text("Green")));
            track_slider!(ui.add(egui::Slider::new(blue_blue, -1.0..=2.0).text("Blue")));
        }
        klipp_effects::BuiltinEffect::DirectionalBlur { angle, distance, samples } => {
            track_slider!(ui.add(egui::Slider::new(angle, 0.0..=360.0).text("Angle")));
            track_slider!(ui.add(egui::Slider::new(distance, 0.0..=100.0).text("Distance")));
            let mut s_f = *samples as f32;
            let r = ui.add(egui::Slider::new(&mut s_f, 3.0..=32.0).text("Samples"));
            *samples = s_f as u32;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
        }
        klipp_effects::BuiltinEffect::Emboss { angle, strength, mix } => {
            track_slider!(ui.add(egui::Slider::new(angle, 0.0..=360.0).text("Angle")));
            track_slider!(ui.add(egui::Slider::new(strength, 0.0..=5.0).text("Strength")));
            track_slider!(ui.add(egui::Slider::new(mix, 0.0..=1.0).text("Mix")));
        }
        klipp_effects::BuiltinEffect::Mirror { horizontal, vertical } => {
            let mut h_bool = *horizontal != 0;
            let mut v_bool = *vertical != 0;
            if ui.checkbox(&mut h_bool, "Horizontal").changed() {
                *horizontal = if h_bool { 1 } else { 0 };
                changed = true;
            }
            if ui.checkbox(&mut v_bool, "Vertical").changed() {
                *vertical = if v_bool { 1 } else { 0 };
                changed = true;
            }
        }
        klipp_effects::BuiltinEffect::LensDistortion { distortion, scale } => {
            track_slider!(ui.add(egui::Slider::new(distortion, -1.0..=1.0).text("Distortion")));
            track_slider!(ui.add(egui::Slider::new(scale, 0.5..=2.0).text("Scale")));
        }
        klipp_effects::BuiltinEffect::Dilate { radius } => {
            let mut r_f = *radius as f32;
            let r = ui.add(egui::Slider::new(&mut r_f, 1.0..=10.0).text("Radius"));
            *radius = r_f as u32;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
        }
        klipp_effects::BuiltinEffect::Erode { radius } => {
            let mut r_f = *radius as f32;
            let r = ui.add(egui::Slider::new(&mut r_f, 1.0..=10.0).text("Radius"));
            *radius = r_f as u32;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
        }
        klipp_effects::BuiltinEffect::Denoise { spatial_sigma, color_sigma, iterations } => {
            track_slider!(ui.add(egui::Slider::new(spatial_sigma, 1.0..=20.0).text("Spatial Sigma")));
            track_slider!(ui.add(egui::Slider::new(color_sigma, 0.01..=1.0).text("Color Sigma")));
            let mut iter_f = *iterations as f32;
            let r = ui.add(egui::Slider::new(&mut iter_f, 1.0..=3.0).text("Iterations"));
            *iterations = iter_f as u32;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
        }
        klipp_effects::BuiltinEffect::Gamma { gamma } => {
            track_slider!(ui.add(egui::Slider::new(gamma, 0.1..=5.0).text("Gamma")));
        }
        klipp_effects::BuiltinEffect::HSVAdjust { hue_shift, saturation, value } => {
            track_slider!(ui.add(egui::Slider::new(hue_shift, -180.0..=180.0).text("Hue Shift")));
            track_slider!(ui.add(egui::Slider::new(saturation, 0.0..=3.0).text("Saturation")));
            track_slider!(ui.add(egui::Slider::new(value, 0.0..=3.0).text("Value")));
        }
        klipp_effects::BuiltinEffect::NoiseGenerator { amount, monochrome, seed } => {
            track_slider!(ui.add(egui::Slider::new(amount, 0.0..=1.0).text("Amount")));
            let mut mono_f = *monochrome as f32;
            let r = ui.add(egui::Slider::new(&mut mono_f, 0.0..=1.0).step_by(1.0_f64).text("Monochrome"));
            *monochrome = mono_f as u32;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
            let mut seed_f = *seed as f32;
            let r = ui.add(egui::Slider::new(&mut seed_f, 0.0..=10000.0).text("Seed"));
            *seed = seed_f as u32;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
        }
        klipp_effects::BuiltinEffect::MedianFilter { radius } => {
            let mut r_f = *radius as f32;
            let r = ui.add(egui::Slider::new(&mut r_f, 1.0..=5.0).text("Radius"));
            *radius = r_f as u32;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
        }
        klipp_effects::BuiltinEffect::RadialBlur { center_x, center_y, amount, samples, mode } => {
            track_slider!(ui.add(egui::Slider::new(center_x, 0.0..=1.0).text("Center X")));
            track_slider!(ui.add(egui::Slider::new(center_y, 0.0..=1.0).text("Center Y")));
            track_slider!(ui.add(egui::Slider::new(amount, 0.0..=50.0).text("Amount")));
            let mut s_f = *samples as f32;
            let r = ui.add(egui::Slider::new(&mut s_f, 4.0..=32.0).text("Samples"));
            *samples = s_f as u32;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
            let mode_labels = ["Zoom", "Spin"];
            let mode_sel = (*mode as usize).min(mode_labels.len() - 1);
            ui.horizontal(|ui| {
                ui.label("Mode");
                let combo_id = ui.make_persistent_id(format!("rb_mode_{clip_id:?}_{effect_index}"));
                egui::ComboBox::from_id_salt(combo_id)
                    .selected_text(mode_labels[mode_sel])
                    .show_ui(ui, |ui| {
                        for (i, label) in mode_labels.iter().enumerate() {
                            if ui.selectable_value(mode, i as u32, *label).changed() {
                                changed = true;
                            }
                        }
                    });
            });
        }
        klipp_effects::BuiltinEffect::Levels { in_black, in_white, gamma, out_black, out_white } => {
            track_slider!(ui.add(egui::Slider::new(in_black, 0.0..=1.0).text("In Black")));
            track_slider!(ui.add(egui::Slider::new(in_white, 0.0..=1.0).text("In White")));
            track_slider!(ui.add(egui::Slider::new(gamma, 0.1..=5.0).text("Gamma")));
            track_slider!(ui.add(egui::Slider::new(out_black, 0.0..=1.0).text("Out Black")));
            track_slider!(ui.add(egui::Slider::new(out_white, 0.0..=1.0).text("Out White")));
        }
        klipp_effects::BuiltinEffect::Equalize { strength } => {
            track_slider!(ui.add(egui::Slider::new(strength, 0.0..=1.0).text("Strength")));
        }
        klipp_effects::BuiltinEffect::Letterbox { aspect_ratio, bar_color, opacity } => {
            track_slider!(ui.add(egui::Slider::new(aspect_ratio, 0.5..=4.0).text("Aspect Ratio")));
            let mut bc_f = *bar_color as f32;
            let r = ui.add(egui::Slider::new(&mut bc_f, 0.0..=1.0).step_by(1.0_f64).text("Bar Color (0=Black, 1=White)"));
            *bar_color = bc_f as u32;
            if r.drag_started() { any_drag_started = true; }
            if r.drag_stopped() { any_drag_stopped = true; }
            changed |= r.changed();
            track_slider!(ui.add(egui::Slider::new(opacity, 0.0..=1.0).text("Opacity")));
        }
        klipp_effects::BuiltinEffect::ChromaticAberration { red_shift, blue_shift, center_x, center_y } => {
            track_slider!(ui.add(egui::Slider::new(red_shift, -20.0..=20.0).text("Red Shift")));
            track_slider!(ui.add(egui::Slider::new(blue_shift, -20.0..=20.0).text("Blue Shift")));
            track_slider!(ui.add(egui::Slider::new(center_x, 0.0..=1.0).text("Center X")));
            track_slider!(ui.add(egui::Slider::new(center_y, 0.0..=1.0).text("Center Y")));
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
