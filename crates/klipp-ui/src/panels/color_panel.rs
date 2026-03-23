use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::{color_slider, color_wheel, curves_editor, scopes, vertical_slider};
use klipp_core::{AppCommand, ColorGradeParam};
use klipp_engine::SharedFrameBuffer;

type SliderDef = (f32, &'static str, fn(f32) -> String, f32, std::ops::RangeInclusive<f32>, ColorGradeParam);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorPanelTab {
    Adjustments,
    Wheels,
    Curves,
    HueSat,
}

pub struct ColorPanel {
    pub active_tab: ColorPanelTab,
    pub lift_wheel: color_wheel::ColorWheelState,
    pub gamma_wheel: color_wheel::ColorWheelState,
    pub gain_wheel: color_wheel::ColorWheelState,
    pub offset_wheel: color_wheel::ColorWheelState,
    pub scope_state: scopes::ScopeState,
    pub active_curve_channel: curves_editor::CurveChannel,
}

impl Default for ColorPanel {
    fn default() -> Self {
        Self {
            active_tab: ColorPanelTab::Adjustments,
            lift_wheel: Default::default(),
            gamma_wheel: Default::default(),
            gain_wheel: Default::default(),
            offset_wheel: Default::default(),
            scope_state: Default::default(),
            active_curve_channel: curves_editor::CurveChannel::Master,
        }
    }
}

impl ColorPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        frame_buffer: &SharedFrameBuffer,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let rect = ui.available_rect_before_wrap();
        let show_scopes = state.show_scopes;
        let mut toggle_scopes = false;

        // Panel background + borders
        let panel_bg = egui::Color32::from_rgb(0x11, 0x11, 0x14);
        let border_color = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);
        ui.painter().rect_filled(rect, 0.0, panel_bg);
        ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(0.5, border_color));

        // Tab bar
        let tab_bar_height = 36.0;
        let tab_bar_rect = egui::Rect::from_min_size(
            rect.min,
            egui::vec2(rect.width(), tab_bar_height),
        );
        ui.painter().hline(
            tab_bar_rect.x_range(),
            tab_bar_rect.max.y,
            egui::Stroke::new(0.5, border_color),
        );

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(tab_bar_rect), |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(12.0);
                let tabs = [
                    (ColorPanelTab::Adjustments, "Adjustments", 76.0_f32),
                    (ColorPanelTab::Wheels, "Wheels", 50.0),
                    (ColorPanelTab::Curves, "Curves", 50.0),
                    (ColorPanelTab::HueSat, "Hue / Sat", 62.0),
                ];
                let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
                let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);

                for (tab, label, tab_w) in &tabs {
                    let is_active = self.active_tab == *tab;
                    let (tab_rect, resp) = ui.allocate_exact_size(
                        egui::vec2(*tab_w, tab_bar_height),
                        egui::Sense::click(),
                    );
                    let color = if is_active || resp.hovered() { text_accent } else { text_dim };
                    ui.painter().text(
                        tab_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::proportional(11.0),
                        color,
                    );
                    if is_active {
                        ui.painter().hline(
                            tab_rect.x_range(),
                            tab_rect.max.y - 1.0,
                            egui::Stroke::new(2.0, text_accent),
                        );
                    }
                    if resp.clicked() {
                        self.active_tab = *tab;
                    }
                }

                // Spacer
                let remaining = ui.available_width() - 80.0;
                if remaining > 0.0 {
                    ui.add_space(remaining);
                }

                // SCOPES button
                let scopes_active = show_scopes;
                let scopes_bg = if scopes_active {
                    egui::Color32::from_rgb(0x22, 0x20, 0x18)
                } else {
                    egui::Color32::from_rgb(0x1a, 0x1a, 0x20)
                };
                let scopes_label = if scopes_active { "SCOPES ◂" } else { "SCOPES ▸" };
                let (scopes_rect, scopes_resp) = ui.allocate_exact_size(
                    egui::vec2(68.0, 22.0),
                    egui::Sense::click(),
                );
                ui.painter().rect_filled(scopes_rect, 4.0, scopes_bg);
                let scopes_border = if scopes_active { text_accent } else { border_color };
                ui.painter().rect_stroke(scopes_rect, 4.0, egui::Stroke::new(0.5, scopes_border));
                let scopes_text_c = if scopes_active { text_accent } else { text_dim };
                ui.painter().text(
                    scopes_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    scopes_label,
                    egui::FontId::proportional(10.0),
                    scopes_text_c,
                );
                if scopes_resp.clicked() {
                    toggle_scopes = true;
                }
            });
        });

        // Content area
        let footer_height = 34.0;
        let full_content_rect = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, rect.min.y + tab_bar_height),
            egui::pos2(rect.max.x, rect.max.y - footer_height),
        );

        // Split content: controls | scopes (right side)
        // Controls width adapts: use up to 45% of panel when scopes open, max 420px
        let panel_w = full_content_rect.width();
        let controls_max_w = if show_scopes {
            (panel_w * 0.45).clamp(300.0, 420.0)
        } else {
            panel_w.min(420.0)
        };
        let scope_width = if show_scopes {
            (panel_w - controls_max_w).clamp(180.0, 400.0)
        } else {
            0.0
        };
        let controls_w = (panel_w - scope_width).min(controls_max_w);
        let controls_rect = egui::Rect::from_min_size(
            full_content_rect.min,
            egui::vec2(controls_w, full_content_rect.height()),
        );

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(controls_rect), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("color_panel_content")
                .show(ui, |ui| {
                    match self.active_tab {
                        ColorPanelTab::Adjustments => {
                            self.show_adjustments_tab(ui, state, theme, &mut cmds);
                        }
                        ColorPanelTab::Wheels => {
                            let content_h = full_content_rect.height();
                            self.show_wheels_tab(ui, state, theme, &mut cmds, content_h);
                        }
                        ColorPanelTab::Curves => {
                            self.show_curves_tab(ui, state, theme, &mut cmds);
                        }
                        ColorPanelTab::HueSat => {
                            self.show_huesat_tab(ui, state, theme, &mut cmds);
                        }
                    }
                });
        });

        // Scopes panel (right of controls)
        if show_scopes {
            let scope_rect = egui::Rect::from_min_max(
                egui::pos2(controls_rect.max.x, full_content_rect.min.y),
                egui::pos2(full_content_rect.max.x, full_content_rect.max.y),
            );

            // Divider line
            ui.painter().vline(
                scope_rect.min.x,
                scope_rect.y_range(),
                egui::Stroke::new(0.5, border_color),
            );

            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(scope_rect), |ui| {
                ui.add_space(4.0);

                // Mode tabs
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    let modes = [
                        (crate::state::ScopeMode::Waveform, "Wave"),
                        (crate::state::ScopeMode::Parade, "Parade"),
                        (crate::state::ScopeMode::Vectorscope, "Vector"),
                        (crate::state::ScopeMode::Histogram, "Hist"),
                    ];
                    let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
                    let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
                    for (mode, label) in &modes {
                        let active = state.scope_mode == *mode;
                        let color = if active { text_accent } else { text_dim };
                        let resp = ui.label(egui::RichText::new(*label).size(9.0).color(color));
                        if resp.interact(egui::Sense::click()).clicked() {
                            state.scope_mode = *mode;
                        }
                        ui.add_space(4.0);
                    }
                });

                ui.add_space(4.0);

                scopes::show_scopes(ui, &mut self.scope_state, state.scope_mode, frame_buffer);
            });
        }

        // Footer (constrained to controls width so it doesn't overlap scopes)
        let footer_right = if show_scopes { controls_rect.max.x } else { rect.max.x };
        let footer_rect = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, rect.max.y - footer_height),
            egui::pos2(footer_right, rect.max.y),
        );
        ui.painter().hline(
            footer_rect.x_range(),
            footer_rect.min.y,
            egui::Stroke::new(0.5, border_color),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(footer_rect), |ui| {
            self.show_footer(ui, state, theme, &mut cmds);
        });

        // Apply scope toggle (deferred to avoid borrow issues)
        if toggle_scopes {
            state.show_scopes = !state.show_scopes;
        }

        cmds
    }

    fn show_adjustments_tab(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => {
                ui.add_space(40.0);
                let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("Select a clip to grade").color(text_dim).size(12.0));
                });
                return;
            }
        };

        // Get current grade
        let grade = {
            let seq = &state.sequences[state.active_sequence_idx];
            seq.tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| c.color_grade.clone())
        };

        let Some(grade) = grade else { return };

        let mut any_drag_started = false;
        let mut any_drag_ended = false;
        let mut any_changed = false;

        // ── Wheels row (LIFT / GAMMA / GAIN at 88px) — centred with 20px gaps ──
        ui.add_space(6.0);
        let adj_wheel_diam = 88.0_f32;
        let between_gap = 20.0_f32;

        ui.horizontal(|ui| {
            let wheel_widget_w = adj_wheel_diam + 12.0; // 100px per widget
            let block_w = wheel_widget_w * 3.0 + between_gap * 2.0; // total block
            let avail = ui.available_width();
            let left_margin = ((avail - block_w) / 2.0).max(0.0);
            ui.add_space(left_margin);

            let mut render_wheel = |ui: &mut egui::Ui,
                                     ws: &mut color_wheel::ColorWheelState,
                                     label: &str,
                                     ox: f32, oy: f32, m: f32,
                                     px: ColorGradeParam, py: ColorGradeParam, pm: ColorGradeParam| {
                let mut off = [ox, oy];
                let mut master_val = m;
                let resp = color_wheel::color_wheel_sized(ui, ws, theme, label, &mut off, &mut master_val, adj_wheel_diam);
                if resp.drag_started { any_drag_started = true; }
                if resp.drag_ended { any_drag_ended = true; }
                if resp.changed {
                    any_changed = true;
                    cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: px, value: off[0] });
                    cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: py, value: off[1] });
                    cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: pm, value: master_val });
                }
            };

            render_wheel(ui, &mut self.lift_wheel, "LIFT",
                grade.lift_x, grade.lift_y, grade.lift_master,
                ColorGradeParam::LiftX, ColorGradeParam::LiftY, ColorGradeParam::LiftMaster);
            ui.add_space(between_gap);
            render_wheel(ui, &mut self.gamma_wheel, "GAMMA",
                grade.gamma_x, grade.gamma_y, grade.gamma_master,
                ColorGradeParam::GammaX, ColorGradeParam::GammaY, ColorGradeParam::GammaMaster);
            ui.add_space(between_gap);
            render_wheel(ui, &mut self.gain_wheel, "GAIN",
                grade.gain_x, grade.gain_y, grade.gain_master,
                ColorGradeParam::GainX, ColorGradeParam::GainY, ColorGradeParam::GainMaster);
        });

        // Divider
        let border_color = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);
        let div_rect = egui::Rect::from_min_size(
            egui::pos2(ui.min_rect().min.x + 12.0, ui.cursor().min.y),
            egui::vec2(ui.available_width() - 24.0, 0.5),
        );
        ui.painter().rect_filled(div_rect, 0.0, border_color);
        ui.add_space(4.0);

        // TONE section
        section_header_with_reset(ui, "TONE", || {
            let params = [
                (ColorGradeParam::Exposure, 0.0),
                (ColorGradeParam::Contrast, 0.0),
                (ColorGradeParam::Highlights, 0.0),
                (ColorGradeParam::Shadows, 0.0),
                (ColorGradeParam::Whites, 0.0),
                (ColorGradeParam::Blacks, 0.0),
            ];
            params.iter().map(|(p, v)| {
                AppCommand::SetClipColorGradeParam { clip_id, param: *p, value: *v }
            }).collect()
        }, cmds);

        let tone_sliders: &[SliderDef] = &[
            (grade.exposure,   "Exposure",   |v| format!("{v:+.1}"),  0.0, -5.0..=5.0,   ColorGradeParam::Exposure),
            (grade.contrast,   "Contrast",   |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,   ColorGradeParam::Contrast),
            (grade.highlights, "Highlights", |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,   ColorGradeParam::Highlights),
            (grade.shadows,    "Shadows",    |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,   ColorGradeParam::Shadows),
            (grade.whites,     "Whites",     |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,   ColorGradeParam::Whites),
            (grade.blacks,     "Blacks",     |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,   ColorGradeParam::Blacks),
        ];

        // Get current clip time for keyframe evaluation
        let clip_time_secs = {
            let seq = &state.sequences[state.active_sequence_idx];
            seq.tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| state.playhead_smooth_secs - c.source_in.to_secs_f64())
                .unwrap_or(0.0)
        };

        for &(mut val, label, fmt, def, ref range, param) in tone_sliders {
            ui.horizontal(|ui| {
                // Keyframe diamond (3px left of slider)
                let has_kf = grade.keyframe_tracks.has_keyframes(param);
                let diamond_color = if has_kf {
                    egui::Color32::from_rgb(0xe8, 0xa0, 0x20)
                } else {
                    egui::Color32::from_rgb(0x40, 0x40, 0x50)
                };
                let (diamond_rect, diamond_resp) = ui.allocate_exact_size(
                    egui::vec2(10.0, 14.0), egui::Sense::click(),
                );
                let dc = diamond_rect.center();
                let ds = 3.5;
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![
                        egui::pos2(dc.x, dc.y - ds),
                        egui::pos2(dc.x + ds, dc.y),
                        egui::pos2(dc.x, dc.y + ds),
                        egui::pos2(dc.x - ds, dc.y),
                    ],
                    if has_kf { diamond_color } else { egui::Color32::TRANSPARENT },
                    egui::Stroke::new(1.0, diamond_color),
                ));
                if diamond_resp.clicked() {
                    if has_kf {
                        // Remove keyframe at current time
                        cmds.push(AppCommand::RemoveKeyframe {
                            clip_id, param, time_secs: clip_time_secs,
                        });
                    } else {
                        // Add keyframe at current time
                        cmds.push(AppCommand::AddKeyframe {
                            clip_id, param, time_secs: clip_time_secs, value: val,
                        });
                    }
                    any_changed = true;
                }
                if diamond_resp.hovered() {
                    diamond_resp.on_hover_text(if has_kf { "Remove keyframe" } else { "Add keyframe" });
                }
            });

            let resp = color_slider::color_slider_with_edit(
                ui, theme, label, &mut val, range.clone(), def, fmt,
                Some((label, &mut state.slider_edit)),
            );
            any_drag_started |= resp.drag_started;
            any_drag_ended |= resp.drag_ended;
            if resp.changed {
                any_changed = true;
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param, value: val });
                // Auto-add keyframe when changing a keyframed param
                if grade.keyframe_tracks.has_keyframes(param) {
                    cmds.push(AppCommand::AddKeyframe {
                        clip_id, param, time_secs: clip_time_secs, value: val,
                    });
                }
            }
            ui.add_space(2.0);
        }

        ui.add_space(4.0);

        // Divider
        let div_rect = egui::Rect::from_min_size(
            egui::pos2(ui.min_rect().min.x + 12.0, ui.cursor().min.y),
            egui::vec2(ui.available_width() - 24.0, 0.5),
        );
        ui.painter().rect_filled(div_rect, 0.0, border_color);
        ui.add_space(4.0);

        // COLOR section
        section_header_with_reset(ui, "COLOR", || {
            let params = [
                (ColorGradeParam::Saturation, 1.0),
                (ColorGradeParam::Temperature, 0.0),
                (ColorGradeParam::Tint, 0.0),
            ];
            params.iter().map(|(p, v)| {
                AppCommand::SetClipColorGradeParam { clip_id, param: *p, value: *v }
            }).collect()
        }, cmds);

        // White balance eyedropper
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let wb_active = state.wb_eyedropper_active;
            let wb_color = if wb_active {
                egui::Color32::from_rgb(0xe8, 0xa0, 0x20)
            } else {
                egui::Color32::from_rgb(0x60, 0x60, 0x70)
            };
            let wb_label = if wb_active { "⊙ Click viewer to sample" } else { "⊙ White Balance" };
            let wb_resp = ui.label(egui::RichText::new(wb_label).size(9.0).color(wb_color));
            if wb_resp.interact(egui::Sense::click()).clicked() {
                state.wb_eyedropper_active = !state.wb_eyedropper_active;
            }
        });
        ui.add_space(2.0);

        let color_sliders: &[SliderDef] = &[
            (grade.saturation,  "Saturation",  |v| format!("{v:.2}"),   1.0, 0.0..=2.0,       ColorGradeParam::Saturation),
            (grade.temperature, "Temperature", |v| format!("{v:+.0}"),  0.0, -100.0..=100.0,  ColorGradeParam::Temperature),
            (grade.tint,        "Tint",        |v| format!("{v:+.0}"),  0.0, -100.0..=100.0,  ColorGradeParam::Tint),
        ];

        for &(mut val, label, fmt, def, ref range, param) in color_sliders {
            let resp = color_slider::color_slider_with_edit(
                ui, theme, label, &mut val, range.clone(), def, fmt,
                Some((label, &mut state.slider_edit)),
            );
            any_drag_started |= resp.drag_started;
            any_drag_ended |= resp.drag_ended;
            if resp.changed {
                any_changed = true;
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param, value: val });
            }
            ui.add_space(3.0);
        }

        // ── Qualifier (Secondary Color Correction) ──
        ui.add_space(4.0);
        let border_color2 = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);
        let div_rect = egui::Rect::from_min_size(
            egui::pos2(ui.min_rect().min.x + 12.0, ui.cursor().min.y),
            egui::vec2(ui.available_width() - 24.0, 0.5),
        );
        ui.painter().rect_filled(div_rect, 0.0, border_color2);
        ui.add_space(4.0);

        {
            let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
            let text_dim = egui::Color32::from_rgb(0x60, 0x60, 0x70);
            let mut qual = grade.qualifier.clone();

            // Header with enable toggle
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                let header_color = if qual.enabled { text_accent } else {
                    egui::Color32::from_rgb(0x50, 0x50, 0x60)
                };
                ui.label(egui::RichText::new("QUALIFIER").size(9.0).color(header_color));
                ui.add_space(8.0);
                if ui.checkbox(&mut qual.enabled, "").changed() {
                    // will be pushed below
                }
            });

            if qual.enabled {
                ui.add_space(4.0);

                // Hue range
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("Hue").size(10.0).color(text_dim));
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(format!("{:.0}°", qual.hue_center)).size(9.0).color(text_accent));
                });
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("Center").size(8.0).color(text_dim).weak());
                    ui.add(egui::Slider::new(&mut qual.hue_center, 0.0..=360.0).show_value(false));
                });
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("Width").size(8.0).color(text_dim).weak());
                    ui.add(egui::Slider::new(&mut qual.hue_width, 0.0..=180.0).show_value(false));
                    ui.label(egui::RichText::new(format!("±{:.0}°", qual.hue_width)).size(8.0).color(text_dim));
                });
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("Soft").size(8.0).color(text_dim).weak());
                    ui.add(egui::Slider::new(&mut qual.hue_soft, 0.0..=90.0).show_value(false));
                });

                ui.add_space(2.0);

                // Saturation range
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("Sat").size(10.0).color(text_dim));
                    ui.label(egui::RichText::new(format!("{:.2}–{:.2}", qual.sat_low, qual.sat_high)).size(8.0).color(text_dim));
                });
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.add(egui::Slider::new(&mut qual.sat_low, 0.0..=1.0).show_value(false).text("Lo"));
                });
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.add(egui::Slider::new(&mut qual.sat_high, 0.0..=1.0).show_value(false).text("Hi"));
                });

                ui.add_space(2.0);

                // Luminance range
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("Lum").size(10.0).color(text_dim));
                    ui.label(egui::RichText::new(format!("{:.2}–{:.2}", qual.lum_low, qual.lum_high)).size(8.0).color(text_dim));
                });
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.add(egui::Slider::new(&mut qual.lum_low, 0.0..=1.0).show_value(false).text("Lo"));
                });
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.add(egui::Slider::new(&mut qual.lum_high, 0.0..=1.0).show_value(false).text("Hi"));
                });

                ui.add_space(2.0);

                // Invert
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.checkbox(&mut qual.invert, egui::RichText::new("Invert").size(9.0).color(text_dim));
                });

                ui.add_space(4.0);

                // Secondary adjustments
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("SECONDARY GRADE").size(8.0).color(
                        egui::Color32::from_rgb(0x50, 0x50, 0x60),
                    ));
                });
                ui.add_space(2.0);

                let mut exp = qual.exposure;
                let exp_resp = color_slider::color_slider_with_edit(
                    ui, theme, "Exposure", &mut exp, -3.0..=3.0, 0.0, |v| format!("{v:+.1}"),
                    Some(("q_exp", &mut state.slider_edit)),
                );
                qual.exposure = exp;
                any_drag_started |= exp_resp.drag_started;
                any_drag_ended |= exp_resp.drag_ended;

                let mut sat = qual.saturation;
                let sat_resp = color_slider::color_slider_with_edit(
                    ui, theme, "Saturation", &mut sat, 0.0..=2.0, 1.0, |v| format!("{v:.2}"),
                    Some(("q_sat", &mut state.slider_edit)),
                );
                qual.saturation = sat;
                any_drag_started |= sat_resp.drag_started;
                any_drag_ended |= sat_resp.drag_ended;

                let mut hue_s = qual.hue_shift;
                let hue_resp = color_slider::color_slider_with_edit(
                    ui, theme, "Hue Shift", &mut hue_s, -180.0..=180.0, 0.0, |v| format!("{v:+.0}°"),
                    Some(("q_hue", &mut state.slider_edit)),
                );
                qual.hue_shift = hue_s;
                any_drag_started |= hue_resp.drag_started;
                any_drag_ended |= hue_resp.drag_ended;

                let mut temp = qual.temperature;
                let temp_resp = color_slider::color_slider_with_edit(
                    ui, theme, "Temperature", &mut temp, -100.0..=100.0, 0.0, |v| format!("{v:+.0}"),
                    Some(("q_temp", &mut state.slider_edit)),
                );
                qual.temperature = temp;
                any_drag_started |= temp_resp.drag_started;
                any_drag_ended |= temp_resp.drag_ended;

                if exp_resp.changed || sat_resp.changed || hue_resp.changed || temp_resp.changed {
                    any_changed = true;
                }
            }

            // Push qualifier changes if anything changed
            if qual.enabled != grade.qualifier.enabled
                || qual.hue_center != grade.qualifier.hue_center
                || qual.hue_width != grade.qualifier.hue_width
                || qual.hue_soft != grade.qualifier.hue_soft
                || qual.sat_low != grade.qualifier.sat_low
                || qual.sat_high != grade.qualifier.sat_high
                || qual.lum_low != grade.qualifier.lum_low
                || qual.lum_high != grade.qualifier.lum_high
                || qual.invert != grade.qualifier.invert
                || qual.exposure != grade.qualifier.exposure
                || qual.saturation != grade.qualifier.saturation
                || qual.hue_shift != grade.qualifier.hue_shift
                || qual.temperature != grade.qualifier.temperature
            {
                cmds.push(AppCommand::SetClipQualifier { clip_id, qualifier: qual });
                any_changed = true;
            }
        }

        // ── Power Windows / Masks ──
        ui.add_space(4.0);
        {
            let border_color3 = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);
            let div_rect = egui::Rect::from_min_size(
                egui::pos2(ui.min_rect().min.x + 12.0, ui.cursor().min.y),
                egui::vec2(ui.available_width() - 24.0, 0.5),
            );
            ui.painter().rect_filled(div_rect, 0.0, border_color3);
            ui.add_space(4.0);

            let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
            let text_dim = egui::Color32::from_rgb(0x60, 0x60, 0x70);
            let mut masks = grade.masks.clone();

            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(egui::RichText::new("MASKS").size(9.0).color(
                    egui::Color32::from_rgb(0x50, 0x50, 0x60),
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);
                    // Add circle mask
                    let add_circle = ui.label(egui::RichText::new("+ ○").size(10.0).color(text_accent));
                    if add_circle.interact(egui::Sense::click()).clicked() {
                        masks.masks.push(klipp_core::Mask {
                            shape: klipp_core::MaskShape::Circle {
                                center_x: 0.5, center_y: 0.5, radius: 0.2,
                            },
                            ..Default::default()
                        });
                    }
                    ui.add_space(6.0);
                    // Add rectangle mask
                    let add_rect = ui.label(egui::RichText::new("+ □").size(10.0).color(text_accent));
                    if add_rect.interact(egui::Sense::click()).clicked() {
                        masks.masks.push(klipp_core::Mask {
                            shape: klipp_core::MaskShape::Rectangle {
                                center_x: 0.5, center_y: 0.5, width: 0.3, height: 0.2, rotation: 0.0,
                            },
                            ..Default::default()
                        });
                    }
                    ui.add_space(6.0);
                    // Add gradient mask
                    let add_grad = ui.label(egui::RichText::new("+ ◧").size(10.0).color(text_accent));
                    if add_grad.interact(egui::Sense::click()).clicked() {
                        masks.masks.push(klipp_core::Mask {
                            shape: klipp_core::MaskShape::LinearGradient {
                                start_x: 0.5, start_y: 0.3, end_x: 0.5, end_y: 0.7,
                            },
                            ..Default::default()
                        });
                    }
                });
            });

            // Mask list
            let mut remove_idx = None;
            for (i, mask) in masks.masks.iter_mut().enumerate() {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);

                    // Enable toggle
                    ui.checkbox(&mut mask.enabled, "");

                    // Shape label
                    let shape_label = match &mask.shape {
                        klipp_core::MaskShape::Circle { .. } => "○ Circle",
                        klipp_core::MaskShape::Rectangle { .. } => "□ Rectangle",
                        klipp_core::MaskShape::LinearGradient { .. } => "◧ Gradient",
                    };
                    ui.label(egui::RichText::new(shape_label).size(9.0).color(text_dim));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(16.0);
                        // Delete button
                        let del = ui.label(egui::RichText::new("×").size(11.0).color(
                            egui::Color32::from_rgb(0xc0, 0x50, 0x50),
                        ));
                        if del.interact(egui::Sense::click()).clicked() {
                            remove_idx = Some(i);
                        }
                        ui.add_space(4.0);
                        // Invert toggle
                        ui.checkbox(&mut mask.invert, egui::RichText::new("Inv").size(8.0).color(text_dim));
                    });
                });

                // Feather slider
                ui.horizontal(|ui| {
                    ui.add_space(32.0);
                    ui.label(egui::RichText::new("Feather").size(8.0).color(text_dim));
                    ui.add(egui::Slider::new(&mut mask.feather, 0.0..=0.5).show_value(false));
                });

                // Opacity slider
                ui.horizontal(|ui| {
                    ui.add_space(32.0);
                    ui.label(egui::RichText::new("Opacity").size(8.0).color(text_dim));
                    ui.add(egui::Slider::new(&mut mask.opacity, 0.0..=1.0).show_value(false));
                });
            }

            if let Some(idx) = remove_idx {
                masks.masks.remove(idx);
            }

            // Push changes if masks differ
            if masks.masks.len() != grade.masks.masks.len()
                || masks.masks.iter().zip(grade.masks.masks.iter()).any(|(a, b)| {
                    a.enabled != b.enabled || a.invert != b.invert
                    || (a.feather - b.feather).abs() > 0.001
                    || (a.opacity - b.opacity).abs() > 0.001
                })
                || remove_idx.is_some()
            {
                cmds.push(AppCommand::SetClipMasks { clip_id, masks });
                any_changed = true;
            }
        }

        // ── Undo handling ──
        if any_drag_started {
            let pre_change_grade = {
                let seq = &state.sequences[state.active_sequence_idx];
                seq.tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .map(|c| c.color_grade.clone())
            };
            state.color_grade_drag_start = pre_change_grade;
        }

        if any_drag_ended {
            if let Some(old_grade) = state.color_grade_drag_start.take() {
                let current = state.sequences[state.active_sequence_idx].tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .map(|c| c.color_grade.clone())
                    .unwrap_or_else(|| grade.clone());
                state.history.push(crate::history::HistoryEntry {
                    sequence_idx: state.active_sequence_idx,
                    command: crate::history::EditCommand::SetColorGrade {
                        clip_id,
                        old_grade,
                        new_grade: current,
                    },
                });
            }
        }

        if any_changed && !any_drag_started && !any_drag_ended && state.color_grade_drag_start.is_none() {
            state.history.push(crate::history::HistoryEntry {
                sequence_idx: state.active_sequence_idx,
                command: crate::history::EditCommand::SetColorGrade {
                    clip_id,
                    old_grade: grade.clone(),
                    new_grade: grade.clone(),
                },
            });
        }
    }

    fn show_wheels_tab(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
        content_h: f32,
    ) {
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => {
                ui.add_space(40.0);
                let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("Select a clip to grade").color(text_dim).size(12.0));
                });
                return;
            }
        };

        let grade = {
            let seq = &state.sequences[state.active_sequence_idx];
            seq.tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| c.color_grade.clone())
        };
        let Some(grade) = grade else { return };

        let mut any_drag_started = false;
        let mut any_drag_ended = false;
        let mut any_changed = false;

        let border_color = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);

        // Adaptive wheel size: fit both width and height constraints
        // Each column = (avail - grid_gap) / 2
        // Cell width = 4 + 18 + 4 + widget_w + 4 + 18 + 4 = widget_w + 52
        // widget_w ≈ max(diam+8, pills_row) + 4 ≈ diam * 1.15 + 8 (approx)
        // So diam ≈ (col_w - 60) / 1.15
        let col_w = (ui.available_width() - 8.0) / 2.0;
        let diam_from_w = ((col_w - 60.0) / 1.15).max(40.0);
        let diam_from_h = ((content_h - 180.0) / 2.0 * 0.5).max(40.0);
        let wheel_diam = diam_from_w.min(diam_from_h).clamp(48.0, 80.0);
        let vslider_h = (wheel_diam * 0.75).clamp(36.0, 60.0);

        // ── 2×2 Wheel Grid ──
        // Renders a wheel cell: [Sat slider] [Wheel] [Bri slider] + RGB + reset
        let mut render_wheel_cell = |ui: &mut egui::Ui,
                                      label: &str,
                                      ws: &mut color_wheel::ColorWheelState,
                                      ox: f32, oy: f32, m: f32,
                                      sat: f32, bri: f32,
                                      px: ColorGradeParam, py: ColorGradeParam, pm: ColorGradeParam,
                                      ps: ColorGradeParam, pb: ColorGradeParam| {
            let cell_bg = egui::Color32::from_rgb(0x11, 0x11, 0x14);
            let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
            let text_dim = egui::Color32::from_rgb(0x36, 0x36, 0x40);

            ui.vertical(|ui| {
                ui.painter().rect_filled(ui.available_rect_before_wrap(), 0.0, cell_bg);
                ui.add_space(8.0);

                // Label
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new(label).size(9.0).color(text_accent).strong());
                });
                ui.add_space(4.0);

                // [Sat] [Wheel] [Bri] row
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    // Sat vertical slider
                    let mut sat_val = sat;
                    let sr = vertical_slider::vertical_slider(ui, theme, "Sat", &mut sat_val, -1.0..=1.0, vslider_h);
                    if sr.drag_started { any_drag_started = true; }
                    if sr.drag_ended { any_drag_ended = true; }
                    if sr.changed {
                        any_changed = true;
                        cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: ps, value: sat_val });
                    }

                    ui.add_space(2.0);

                    // Wheel (adaptive size for 2x2 grid)
                    let mut off = [ox, oy];
                    let mut master_val = m;
                    let wr = color_wheel::color_wheel_sized(ui, ws, theme, "", &mut off, &mut master_val, wheel_diam);
                    if wr.drag_started { any_drag_started = true; }
                    if wr.drag_ended { any_drag_ended = true; }
                    if wr.changed {
                        any_changed = true;
                        cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: px, value: off[0] });
                        cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: py, value: off[1] });
                        cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: pm, value: master_val });
                    }

                    ui.add_space(2.0);

                    // Bri vertical slider
                    let mut bri_val = bri;
                    let br = vertical_slider::vertical_slider(ui, theme, "Bri", &mut bri_val, -1.0..=1.0, vslider_h);
                    if br.drag_started { any_drag_started = true; }
                    if br.drag_ended { any_drag_ended = true; }
                    if br.changed {
                        any_changed = true;
                        cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: pb, value: bri_val });
                    }

                    ui.add_space(4.0);
                });

                // Reset button
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 40.0) / 2.0);
                    let resp = ui.label(egui::RichText::new("↺ reset").size(8.0).color(text_dim));
                    if resp.interact(egui::Sense::click()).clicked() {
                        for &(param, val) in &[(px, 0.0), (py, 0.0), (pm, 0.0), (ps, 0.0), (pb, 0.0)] {
                            cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param, value: val });
                        }
                        any_changed = true;
                    }
                });
                ui.add_space(4.0);
            });
        };

        // 2×2 grid with breathing room
        egui::Grid::new("wheels_2x2")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                // Row 1: LIFT, GAMMA
                render_wheel_cell(ui, "LIFT", &mut self.lift_wheel,
                    grade.lift_x, grade.lift_y, grade.lift_master,
                    grade.lift_sat, grade.lift_bri,
                    ColorGradeParam::LiftX, ColorGradeParam::LiftY, ColorGradeParam::LiftMaster,
                    ColorGradeParam::LiftSat, ColorGradeParam::LiftBri);
                render_wheel_cell(ui, "GAMMA", &mut self.gamma_wheel,
                    grade.gamma_x, grade.gamma_y, grade.gamma_master,
                    grade.gamma_sat, grade.gamma_bri,
                    ColorGradeParam::GammaX, ColorGradeParam::GammaY, ColorGradeParam::GammaMaster,
                    ColorGradeParam::GammaSat, ColorGradeParam::GammaBri);
                ui.end_row();
                // Row 2: GAIN, OFFSET
                render_wheel_cell(ui, "GAIN", &mut self.gain_wheel,
                    grade.gain_x, grade.gain_y, grade.gain_master,
                    grade.gain_sat, grade.gain_bri,
                    ColorGradeParam::GainX, ColorGradeParam::GainY, ColorGradeParam::GainMaster,
                    ColorGradeParam::GainSat, ColorGradeParam::GainBri);
                render_wheel_cell(ui, "OFFSET", &mut self.offset_wheel,
                    grade.offset_x, grade.offset_y, grade.offset_master,
                    grade.offset_sat, grade.offset_bri,
                    ColorGradeParam::OffsetX, ColorGradeParam::OffsetY, ColorGradeParam::OffsetMaster,
                    ColorGradeParam::OffsetSat, ColorGradeParam::OffsetBri);
                ui.end_row();
            });

        // Divider
        let div_rect = egui::Rect::from_min_size(
            egui::pos2(ui.min_rect().min.x + 12.0, ui.cursor().min.y),
            egui::vec2(ui.available_width() - 24.0, 0.5),
        );
        ui.painter().rect_filled(div_rect, 0.0, border_color);
        ui.add_space(4.0);

        // ── GLOBAL section ──
        section_header_with_reset(ui, "GLOBAL", || {
            let params = [
                (ColorGradeParam::Temperature, 0.0),
                (ColorGradeParam::Tint, 0.0),
                (ColorGradeParam::Hue, 0.0),
                (ColorGradeParam::GlobalMix, 1.0),
            ];
            params.iter().map(|(p, v)| {
                AppCommand::SetClipColorGradeParam { clip_id, param: *p, value: *v }
            }).collect()
        }, cmds);

        let global_sliders: &[SliderDef] = &[
            (grade.temperature, "Temperature", |v| format!("{v:+.0} K"), 0.0, -100.0..=100.0, ColorGradeParam::Temperature),
            (grade.tint,        "Tint",        |v| format!("{v:+.0}"),   0.0, -100.0..=100.0, ColorGradeParam::Tint),
            (grade.hue,         "Hue",         |v| format!("{v:.0}°"),   0.0, 0.0..=360.0,    ColorGradeParam::Hue),
            (grade.global_mix,  "Mix",         |v| format!("{:.0}%", v * 100.0), 1.0, 0.0..=1.0, ColorGradeParam::GlobalMix),
        ];

        for &(mut val, label, fmt, def, ref range, param) in global_sliders {
            let wid = format!("whl_{label}");
            let resp = color_slider::color_slider_with_edit(
                ui, theme, label, &mut val, range.clone(), def, fmt,
                Some((&wid, &mut state.slider_edit)),
            );
            any_drag_started |= resp.drag_started;
            any_drag_ended |= resp.drag_ended;
            if resp.changed {
                any_changed = true;
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param, value: val });
            }
            ui.add_space(3.0);
        }

        // ── Undo handling ──
        if any_drag_started {
            let pre_grade = state.sequences[state.active_sequence_idx].tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| c.color_grade.clone());
            state.color_grade_drag_start = pre_grade;
        }

        if any_drag_ended {
            if let Some(old_grade) = state.color_grade_drag_start.take() {
                let current = state.sequences[state.active_sequence_idx].tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .map(|c| c.color_grade.clone())
                    .unwrap_or_else(|| grade.clone());
                state.history.push(crate::history::HistoryEntry {
                    sequence_idx: state.active_sequence_idx,
                    command: crate::history::EditCommand::SetColorGrade {
                        clip_id,
                        old_grade,
                        new_grade: current,
                    },
                });
            }
        }

        if any_changed && !any_drag_started && !any_drag_ended && state.color_grade_drag_start.is_none() {
            state.history.push(crate::history::HistoryEntry {
                sequence_idx: state.active_sequence_idx,
                command: crate::history::EditCommand::SetColorGrade {
                    clip_id,
                    old_grade: grade.clone(),
                    new_grade: grade,
                },
            });
        }
    }

    fn show_curves_tab(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => {
                ui.add_space(40.0);
                let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("Select a clip to grade").color(text_dim).size(12.0));
                });
                return;
            }
        };

        let grade = {
            let seq = &state.sequences[state.active_sequence_idx];
            seq.tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| c.color_grade.clone())
        };
        let Some(grade) = grade else { return };
        let mut curves = grade.curves.clone();

        let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
        let text_reset = egui::Color32::from_rgb(0x36, 0x36, 0x40);

        ui.add_space(6.0);

        // ── All/Single toggle + channel buttons ──
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);

            // All/Single toggle
            let all_color = if !state.curves_single_mode { text_accent } else { text_dim };
            let single_color = if state.curves_single_mode { text_accent } else { text_dim };

            let all_resp = ui.label(egui::RichText::new("All").size(10.0).color(all_color));
            if all_resp.interact(egui::Sense::click()).clicked() {
                state.curves_single_mode = false;
            }
            ui.add_space(4.0);
            let single_resp = ui.label(egui::RichText::new("Single").size(10.0).color(single_color));
            if single_resp.interact(egui::Sense::click()).clicked() {
                state.curves_single_mode = true;
            }

            ui.add_space(12.0);

            // Channel selectors (visible in Single mode)
            if state.curves_single_mode {
                let channels = [
                    (curves_editor::CurveChannel::Master, "Luma"),
                    (curves_editor::CurveChannel::Red, "R"),
                    (curves_editor::CurveChannel::Green, "G"),
                    (curves_editor::CurveChannel::Blue, "B"),
                ];
                for (ch, label) in &channels {
                    let active = self.active_curve_channel == *ch;
                    let color = if active {
                        curves_editor::channel_color(*ch)
                    } else {
                        text_dim
                    };
                    let resp = ui.label(egui::RichText::new(*label).size(10.0).color(color));
                    if resp.interact(egui::Sense::click()).clicked() {
                        self.active_curve_channel = *ch;
                    }
                    ui.add_space(6.0);
                }
            }
        });

        ui.add_space(4.0);

        let mut any_changed = false;
        let mut any_drag_started = false;
        let mut any_drag_ended = false;

        // ── Curve blocks ──
        // Mockup heights: Master=84px, R/G/B=62px each
        let curve_defs: &[(curves_editor::CurveChannel, &str, f32)] = if state.curves_single_mode {
            // Single mode: show only active channel at large size
            match self.active_curve_channel {
                curves_editor::CurveChannel::Master => &[(curves_editor::CurveChannel::Master, "Luma", 140.0)],
                curves_editor::CurveChannel::Red => &[(curves_editor::CurveChannel::Red, "Red", 140.0)],
                curves_editor::CurveChannel::Green => &[(curves_editor::CurveChannel::Green, "Green", 140.0)],
                curves_editor::CurveChannel::Blue => &[(curves_editor::CurveChannel::Blue, "Blue", 140.0)],
            }
        } else {
            &[
                (curves_editor::CurveChannel::Master, "Luma", 84.0),
                (curves_editor::CurveChannel::Red, "Red", 62.0),
                (curves_editor::CurveChannel::Green, "Green", 62.0),
                (curves_editor::CurveChannel::Blue, "Blue", 62.0),
            ]
        };

        // Histogram bins from cached frame data (computed in upload_frame_to_egui)
        let empty_hist = [0u32; 256];
        let hist_data = &state.curve_histograms;

        for &(ch, label, height) in curve_defs {
            let curve_color = curves_editor::channel_color(ch);

            // Header: label + reset
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new(label).size(10.0).color(curve_color).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(12.0);
                    let reset_resp = ui.label(egui::RichText::new("↺").size(9.0).color(text_reset));
                    if reset_resp.interact(egui::Sense::click()).clicked() {
                        let default_pts = vec![[0.0, 0.0], [1.0, 1.0]];
                        match ch {
                            curves_editor::CurveChannel::Master => curves.master = default_pts,
                            curves_editor::CurveChannel::Red => curves.red = default_pts,
                            curves_editor::CurveChannel::Green => curves.green = default_pts,
                            curves_editor::CurveChannel::Blue => curves.blue = default_pts,
                        }
                        cmds.push(AppCommand::SetClipCurves { clip_id, curves: curves.clone() });
                        any_changed = true;
                    }
                });
            });

            // Curve editor with histogram background
            let mut pts = match ch {
                curves_editor::CurveChannel::Master => curves.master.clone(),
                curves_editor::CurveChannel::Red => curves.red.clone(),
                curves_editor::CurveChannel::Green => curves.green.clone(),
                curves_editor::CurveChannel::Blue => curves.blue.clone(),
            };

            let hist: &[u32; 256] = match (ch, hist_data) {
                (curves_editor::CurveChannel::Master, Some(h)) => &h.luma,
                (curves_editor::CurveChannel::Red, Some(h)) => &h.r,
                (curves_editor::CurveChannel::Green, Some(h)) => &h.g,
                (curves_editor::CurveChannel::Blue, Some(h)) => &h.b,
                _ => &empty_hist,
            };

            let resp = {
                let mut resp_out = None;
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    resp_out = Some(curves_editor::curves_editor_sized(
                        ui, theme, ch, &mut pts, height, Some(hist),
                    ));
                });
                resp_out.unwrap()
            };

            if resp.changed {
                match ch {
                    curves_editor::CurveChannel::Master => curves.master = pts,
                    curves_editor::CurveChannel::Red => curves.red = pts,
                    curves_editor::CurveChannel::Green => curves.green = pts,
                    curves_editor::CurveChannel::Blue => curves.blue = pts,
                }
                cmds.push(AppCommand::SetClipCurves { clip_id, curves: curves.clone() });
                any_changed = true;
            }
            any_drag_started |= resp.drag_started;
            any_drag_ended |= resp.drag_ended;

            ui.add_space(4.0);
        }

        // ── Preserve Luma checkbox ──
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let mut preserve = grade.curves_preserve_luma;
            if ui.checkbox(&mut preserve, egui::RichText::new("Preserve Luma").size(10.0).color(text_dim)).changed() {
                cmds.push(AppCommand::SetClipColorGradeParam {
                    clip_id,
                    param: ColorGradeParam::CurvesPreserveLuma,
                    value: if preserve { 1.0 } else { 0.0 },
                });
                any_changed = true;
            }
        });

        // ── Mix slider ──
        ui.add_space(2.0);
        let mut mix_val = grade.curves_mix;
        let mix_resp = color_slider::color_slider_with_edit(
            ui, theme, "Mix", &mut mix_val, 0.0..=1.0, 1.0, |v| format!("{:.0}%", v * 100.0),
            Some(("crv_Mix", &mut state.slider_edit)),
        );
        any_drag_started |= mix_resp.drag_started;
        any_drag_ended |= mix_resp.drag_ended;
        if mix_resp.changed {
            cmds.push(AppCommand::SetClipColorGradeParam {
                clip_id, param: ColorGradeParam::CurvesMix, value: mix_val,
            });
            any_changed = true;
        }

        // ── Undo handling ──
        if any_drag_started {
            let old_grade = state.sequences[state.active_sequence_idx].tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| c.color_grade.clone());
            state.color_grade_drag_start = old_grade;
        }

        if any_drag_ended {
            if let Some(old_grade) = state.color_grade_drag_start.take() {
                let new_grade = state.sequences[state.active_sequence_idx].tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .map(|c| c.color_grade.clone())
                    .unwrap_or_else(|| old_grade.clone());
                state.history.push(crate::history::HistoryEntry {
                    sequence_idx: state.active_sequence_idx,
                    command: crate::history::EditCommand::SetColorGrade {
                        clip_id,
                        old_grade,
                        new_grade,
                    },
                });
            }
        }

        if any_changed && !any_drag_started && !any_drag_ended && state.color_grade_drag_start.is_none() {
            let old_grade = state.sequences[state.active_sequence_idx].tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| c.color_grade.clone())
                .unwrap_or_default();
            state.history.push(crate::history::HistoryEntry {
                sequence_idx: state.active_sequence_idx,
                command: crate::history::EditCommand::SetColorGrade {
                    clip_id,
                    old_grade: old_grade.clone(),
                    new_grade: old_grade,
                },
            });
        }
    }

    fn show_huesat_tab(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => {
                ui.add_space(40.0);
                let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("Select a clip to grade").color(text_dim).size(12.0));
                });
                return;
            }
        };

        let grade = {
            let seq = &state.sequences[state.active_sequence_idx];
            seq.tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| c.color_grade.clone())
        };
        let Some(grade) = grade else { return };
        let mut huesat = grade.huesat.clone();

        let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
        let text_reset = egui::Color32::from_rgb(0x36, 0x36, 0x40);
        let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);

        ui.add_space(6.0);

        // All/Single toggle
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let all_color = if !state.huesat_single_mode { text_accent } else { text_dim };
            let single_color = if state.huesat_single_mode { text_accent } else { text_dim };
            let all_resp = ui.label(egui::RichText::new("All").size(10.0).color(all_color));
            if all_resp.interact(egui::Sense::click()).clicked() {
                state.huesat_single_mode = false;
            }
            ui.add_space(4.0);
            let single_resp = ui.label(egui::RichText::new("Single").size(10.0).color(single_color));
            if single_resp.interact(egui::Sense::click()).clicked() {
                state.huesat_single_mode = true;
            }

            if state.huesat_single_mode {
                ui.add_space(12.0);
                let labels = ["HvH", "HvS", "HvL", "LvS", "SvS", "OvS"];
                for (i, label) in labels.iter().enumerate() {
                    let active = state.huesat_active_curve == i as u8;
                    let color = if active { text_accent } else { text_dim };
                    let resp = ui.label(egui::RichText::new(*label).size(9.0).color(color));
                    if resp.interact(egui::Sense::click()).clicked() {
                        state.huesat_active_curve = i as u8;
                    }
                    ui.add_space(4.0);
                }
            }
        });

        ui.add_space(4.0);

        let mut any_changed = false;
        let mut any_drag_started = false;
        let mut any_drag_ended = false;

        // Curve definitions: (index, label, has_hue_bg, has_lum_bg)
        let curve_defs: &[(u8, &str, bool, bool)] = &[
            (0, "Hue vs Hue",    true,  false),
            (1, "Hue vs Sat",    true,  false),
            (2, "Hue vs Lum",    true,  false),
            (3, "Lum vs Sat",    false, true),
            (4, "Sat vs Sat",    false, false),
            (5, "Orange vs Sat", true,  false),
        ];

        for &(idx, label, hue_bg, lum_bg) in curve_defs {
            // Skip in Single mode if not active
            if state.huesat_single_mode && state.huesat_active_curve != idx {
                continue;
            }

            let height = if state.huesat_single_mode { 100.0 } else { 50.0 };

            // Get mutable reference to the right curve
            let points = match idx {
                0 => &mut huesat.hue_vs_hue,
                1 => &mut huesat.hue_vs_sat,
                2 => &mut huesat.hue_vs_lum,
                3 => &mut huesat.lum_vs_sat,
                4 => &mut huesat.sat_vs_sat,
                _ => &mut huesat.orange_vs_sat,
            };

            // Header
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new(label).size(10.0).color(egui::Color32::from_rgb(0x60, 0x60, 0x70)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(12.0);
                    let reset_resp = ui.label(egui::RichText::new("↺").size(9.0).color(text_reset));
                    if reset_resp.interact(egui::Sense::click()).clicked() {
                        points.clear();
                        any_changed = true;
                    }
                });
            });

            // Canvas
            let canvas_width = (ui.available_width() - 24.0).max(100.0);
            let (rect, response) = {
                let mut rect_out = egui::Rect::NOTHING;
                let mut resp_out = None;
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    let (r, resp) = ui.allocate_exact_size(
                        egui::vec2(canvas_width, height),
                        egui::Sense::click_and_drag(),
                    );
                    rect_out = r;
                    resp_out = Some(resp);
                });
                (rect_out, resp_out.unwrap())
            };

            any_drag_started |= response.drag_started();
            any_drag_ended |= response.drag_stopped();

            let painter = ui.painter_at(rect);
            let bg_color = egui::Color32::from_rgb(0x16, 0x16, 0x1a);
            let border_color = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);

            painter.rect_filled(rect, 4.0, bg_color);
            painter.rect_stroke(rect, 4.0, egui::Stroke::new(0.5, border_color));

            // Hue spectrum background
            if hue_bg {
                let w = rect.width() as u32;
                for x in 0..w {
                    let hue_deg = x as f32 / w as f32 * 360.0;
                    let (r, g, b) = klipp_color::hsv_to_rgb(hue_deg, 0.75, 0.55);
                    let color = egui::Color32::from_rgba_unmultiplied(
                        (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 56,
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(rect.min.x + x as f32, rect.min.y),
                            egui::vec2(1.0, rect.height()),
                        ),
                        0.0, color,
                    );
                }
            }

            // Luminance gradient background
            if lum_bg {
                let w = rect.width() as u32;
                for x in 0..w {
                    let v = (x as f32 / w as f32 * 200.0) as u8;
                    let color = egui::Color32::from_rgba_unmultiplied(v, v, v, 76);
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(rect.min.x + x as f32, rect.min.y),
                            egui::vec2(1.0, rect.height()),
                        ),
                        0.0, color,
                    );
                }
            }

            // Flat horizontal identity line at 50% height
            let mid_y = rect.center().y;
            painter.line_segment(
                [egui::pos2(rect.min.x, mid_y), egui::pos2(rect.max.x, mid_y)],
                egui::Stroke::new(0.5, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
            );

            // Hint text when curve is empty — white with black outline for visibility on gradients
            if points.is_empty() {
                let hint_pos = rect.center();
                let hint_font = egui::FontId::proportional(9.0);
                let hint_text = "Click to add points";
                // Black outline (draw offset in 4 directions)
                let outline_color = egui::Color32::from_rgb(0x00, 0x00, 0x00);
                for &(dx, dy) in &[(-1.0_f32, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                    painter.text(
                        egui::pos2(hint_pos.x + dx, hint_pos.y + dy),
                        egui::Align2::CENTER_CENTER,
                        hint_text,
                        hint_font.clone(),
                        outline_color,
                    );
                }
                // White text on top
                painter.text(
                    hint_pos,
                    egui::Align2::CENTER_CENTER,
                    hint_text,
                    hint_font,
                    egui::Color32::from_rgb(0xdd, 0xdd, 0xdd),
                );
            }

            // Draw curve (if points exist)
            if points.len() >= 2 {
                let lut = klipp_color::curves::build_curve_lut_f32(points);
                let mut curve_pts = Vec::with_capacity(256);
                for (i, &val) in lut.iter().enumerate() {
                    let x = rect.min.x + (i as f32 / 255.0) * rect.width();
                    let y = rect.max.y - val * rect.height();
                    curve_pts.push(egui::pos2(x, y));
                }
                for window in curve_pts.windows(2) {
                    painter.line_segment([window[0], window[1]], egui::Stroke::new(1.5, text_accent));
                }
            }

            // Draw control points — hollow circles
            let point_bg = egui::Color32::from_rgb(0x0e, 0x0e, 0x12);
            for pt in points.iter() {
                let px = rect.min.x + pt[0] * rect.width();
                let py = rect.max.y - pt[1] * rect.height();
                let center = egui::pos2(px, py);
                painter.circle_filled(center, 3.5, point_bg);
                painter.circle_stroke(center, 3.5, egui::Stroke::new(1.5, text_accent));
            }

            // Interaction: drag
            let hit_radius = 10.0;
            if let Some(pointer_pos) = response.interact_pointer_pos() {
                if response.dragged() {
                    let mut closest_idx = None;
                    let mut closest_dist = f32::MAX;
                    for (i, pt) in points.iter().enumerate() {
                        let px = rect.min.x + pt[0] * rect.width();
                        let py = rect.max.y - pt[1] * rect.height();
                        let dist = (egui::pos2(px, py) - pointer_pos).length();
                        if dist < closest_dist {
                            closest_dist = dist;
                            closest_idx = Some(i);
                        }
                    }
                    if let Some(i) = closest_idx {
                        if closest_dist < hit_radius || response.drag_started() {
                            let mut new_x = ((pointer_pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                            let mut new_y = ((rect.max.y - pointer_pos.y) / rect.height()).clamp(0.0, 1.0);
                            if ui.input(|inp| inp.modifiers.shift) {
                                new_x = (new_x * 4.0).round() / 4.0;
                                new_y = (new_y * 4.0).round() / 4.0;
                            }
                            if i > 0 && i < points.len() - 1 {
                                let x_min = points[i - 1][0] + 0.01;
                                let x_max = points[i + 1][0] - 0.01;
                                points[i][0] = new_x.clamp(x_min, x_max);
                            } else {
                                points[i][0] = new_x; // endpoints can move freely on hue/sat
                            }
                            points[i][1] = new_y;
                            any_changed = true;
                        }
                    }
                }
            }

            // Double-click: remove point
            if response.double_clicked() {
                if let Some(pointer_pos) = ui.input(|i| i.pointer.interact_pos()) {
                    for i in (0..points.len()).rev() {
                        let px = rect.min.x + points[i][0] * rect.width();
                        let py = rect.max.y - points[i][1] * rect.height();
                        if (egui::pos2(px, py) - pointer_pos).length() < hit_radius {
                            points.remove(i);
                            any_changed = true;
                            break;
                        }
                    }
                }
            }

            // Single click: add point
            if response.clicked() && !any_changed {
                if let Some(pointer_pos) = response.interact_pointer_pos() {
                    let new_x = ((pointer_pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                    let new_y = ((rect.max.y - pointer_pos.y) / rect.height()).clamp(0.0, 1.0);
                    let near = points.iter().any(|pt| {
                        let px = rect.min.x + pt[0] * rect.width();
                        let py = rect.max.y - pt[1] * rect.height();
                        (egui::pos2(px, py) - pointer_pos).length() < hit_radius
                    });
                    if !near {
                        let insert_idx = points.iter().position(|pt| pt[0] > new_x).unwrap_or(points.len());
                        points.insert(insert_idx, [new_x, new_y]);
                        any_changed = true;
                    }
                }
            }

            ui.add_space(3.0);
        }

        // Push huesat changes
        if any_changed {
            cmds.push(AppCommand::SetClipHueSatCurves { clip_id, huesat: huesat.clone() });
        }

        // Mix slider
        ui.add_space(4.0);
        let mut mix_val = grade.huesat_mix;
        let mix_resp = color_slider::color_slider_with_edit(
            ui, theme, "Mix", &mut mix_val, 0.0..=1.0, 1.0, |v| format!("{:.0}%", v * 100.0),
            Some(("hs_Mix", &mut state.slider_edit)),
        );
        any_drag_started |= mix_resp.drag_started;
        any_drag_ended |= mix_resp.drag_ended;
        if mix_resp.changed {
            cmds.push(AppCommand::SetClipColorGradeParam {
                clip_id, param: ColorGradeParam::HueSatMix, value: mix_val,
            });
            any_changed = true;
        }

        // Undo handling
        if any_drag_started {
            let old_grade = state.sequences[state.active_sequence_idx].tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| c.color_grade.clone());
            state.color_grade_drag_start = old_grade;
        }

        if any_drag_ended {
            if let Some(old_grade) = state.color_grade_drag_start.take() {
                let new_grade = state.sequences[state.active_sequence_idx].tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .map(|c| c.color_grade.clone())
                    .unwrap_or_else(|| old_grade.clone());
                state.history.push(crate::history::HistoryEntry {
                    sequence_idx: state.active_sequence_idx,
                    command: crate::history::EditCommand::SetColorGrade {
                        clip_id,
                        old_grade,
                        new_grade,
                    },
                });
            }
        }

        if any_changed && !any_drag_started && !any_drag_ended && state.color_grade_drag_start.is_none() {
            let old_grade = state.sequences[state.active_sequence_idx].tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| c.color_grade.clone())
                .unwrap_or_default();
            state.history.push(crate::history::HistoryEntry {
                sequence_idx: state.active_sequence_idx,
                command: crate::history::EditCommand::SetColorGrade {
                    clip_id,
                    old_grade: old_grade.clone(),
                    new_grade: old_grade,
                },
            });
        }
    }

    #[allow(dead_code)]
    fn show_lut_section(
        &self,
        ui: &mut egui::Ui,
        state: &AppState,
        _theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let border_color = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);
        let text_dim = egui::Color32::from_rgb(0x90, 0x90, 0xa0);
        let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
        let btn_bg = egui::Color32::from_rgb(0x1a, 0x1a, 0x20);

        // Divider
        let div_rect = egui::Rect::from_min_size(
            egui::pos2(ui.min_rect().min.x + 12.0, ui.cursor().min.y),
            egui::vec2(ui.available_width() - 24.0, 0.5),
        );
        ui.painter().rect_filled(div_rect, 0.0, border_color);
        ui.add_space(8.0);

        section_header(ui, "LOOK (LUT)", || true);

        if let Some(lut_path) = &state.global_lut_path {
            // Show loaded LUT name + clear button
            let name = lut_path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Unknown".into());

            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(egui::RichText::new(&name).size(11.0).color(text_accent));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0);
                    let (clear_rect, clear_resp) = ui.allocate_exact_size(
                        egui::vec2(20.0, 18.0),
                        egui::Sense::click(),
                    );
                    ui.painter().rect_filled(clear_rect, 3.0, btn_bg);
                    ui.painter().rect_stroke(clear_rect, 3.0, egui::Stroke::new(0.5, border_color));
                    ui.painter().text(
                        clear_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "×",
                        egui::FontId::proportional(12.0),
                        egui::Color32::from_rgb(0xe0, 0x70, 0x70),
                    );
                    if clear_resp.clicked() {
                        cmds.push(AppCommand::ClearGlobalLut);
                    }
                });
            });

            ui.add_space(4.0);

            // Opacity slider
            let mut opacity = state.global_lut_opacity;
            let resp = color_slider::color_slider(
                ui, _theme, "Opacity", &mut opacity,
                0.0..=1.0, 1.0, |v| format!("{:.0}%", v * 100.0),
            );
            if resp.changed {
                cmds.push(AppCommand::SetGlobalLutOpacity(opacity));
            }
        } else {
            // No LUT loaded — show browse button
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(egui::RichText::new("No LUT loaded").size(10.0).color(text_dim));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                if footer_button(ui, "Browse LUT...", text_dim, btn_bg, border_color) {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("LUT files", &["cube", "3dl", "csp"])
                        .pick_file()
                    {
                        cmds.push(AppCommand::LoadGlobalLut { path });
                    }
                }
            });
        }
    }

    fn show_footer(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        _theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let btn_bg = egui::Color32::from_rgb(0x1a, 0x1a, 0x20);
        let border = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);
        let text_dim = egui::Color32::from_rgb(0x90, 0x90, 0xa0);
        let text_red = egui::Color32::from_rgb(0xe0, 0x70, 0x70);
        let border_red = egui::Color32::from_rgb(0x4a, 0x20, 0x20);
        let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
        let ba_bg = egui::Color32::from_rgba_unmultiplied(0xe8, 0xa0, 0x20, 20);
        let ba_border = egui::Color32::from_rgb(0xa0, 0x6e, 0x14);

        ui.horizontal_centered(|ui| {
            ui.add_space(12.0);

            // Reset All
            if footer_button(ui, "Reset All", text_red, btn_bg, border_red) {
                if let Some(clip_id) = state.selected_clip {
                    cmds.push(AppCommand::ResetClipColorGrade { clip_id });
                }
            }

            ui.add_space(6.0);

            // Copy Grade
            if footer_button(ui, "Copy Grade", text_dim, btn_bg, border) {
                if let Some(clip_id) = state.selected_clip {
                    let grade = state.sequences[state.active_sequence_idx].tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == clip_id)
                        .map(|c| c.color_grade.clone());
                    state.color_grade_clipboard = grade;
                }
            }

            ui.add_space(6.0);

            // Paste Grade
            if footer_button(ui, "Paste Grade", text_dim, btn_bg, border) {
                if let (Some(clip_id), Some(grade)) = (state.selected_clip, state.color_grade_clipboard.clone()) {
                    // Get old grade for undo
                    let old_grade = state.sequences[state.active_sequence_idx].tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == clip_id)
                        .map(|c| c.color_grade.clone())
                        .unwrap_or_default();

                    // Apply all params
                    use ColorGradeParam as P;
                    let params: &[(P, f32)] = &[
                        (P::Exposure, grade.exposure), (P::Contrast, grade.contrast),
                        (P::Saturation, grade.saturation), (P::Temperature, grade.temperature),
                        (P::Tint, grade.tint), (P::Highlights, grade.highlights),
                        (P::Shadows, grade.shadows), (P::Whites, grade.whites),
                        (P::Blacks, grade.blacks),
                        (P::LiftX, grade.lift_x), (P::LiftY, grade.lift_y),
                        (P::LiftMaster, grade.lift_master),
                        (P::GammaX, grade.gamma_x), (P::GammaY, grade.gamma_y),
                        (P::GammaMaster, grade.gamma_master),
                        (P::GainX, grade.gain_x), (P::GainY, grade.gain_y),
                        (P::GainMaster, grade.gain_master),
                        (P::OffsetX, grade.offset_x), (P::OffsetY, grade.offset_y),
                        (P::OffsetMaster, grade.offset_master),
                        (P::LiftSat, grade.lift_sat), (P::LiftBri, grade.lift_bri),
                        (P::GammaSat, grade.gamma_sat), (P::GammaBri, grade.gamma_bri),
                        (P::GainSat, grade.gain_sat), (P::GainBri, grade.gain_bri),
                        (P::OffsetSat, grade.offset_sat), (P::OffsetBri, grade.offset_bri),
                        (P::Hue, grade.hue), (P::GlobalMix, grade.global_mix),
                        (P::CurvesPreserveLuma, if grade.curves_preserve_luma { 1.0 } else { 0.0 }),
                        (P::CurvesMix, grade.curves_mix),
                        (P::HueSatMix, grade.huesat_mix),
                    ];
                    for &(param, value) in params {
                        cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param, value });
                    }

                    // Push undo
                    state.history.push(crate::history::HistoryEntry {
                        sequence_idx: state.active_sequence_idx,
                        command: crate::history::EditCommand::SetColorGrade {
                            clip_id,
                            old_grade,
                            new_grade: grade,
                        },
                    });
                }
            }

            ui.add_space(6.0);

            // Presets dropdown
            let preset_resp = footer_button(ui, "Presets ▾", text_dim, btn_bg, border);
            if preset_resp {
                state.show_preset_menu = !state.show_preset_menu;
            }

            // Spacer
            let remaining = ui.available_width() - 110.0;
            if remaining > 0.0 {
                ui.add_space(remaining);
            }

            // Before / After
            let ba_active = state.before_after_active;
            let ba_bg_actual = if ba_active {
                egui::Color32::from_rgba_unmultiplied(0xe8, 0xa0, 0x20, 40)
            } else {
                ba_bg
            };
            let ba_label = if ba_active { "● Before" } else { "Before / After" };
            let (ba_rect, ba_resp) = ui.allocate_exact_size(
                egui::vec2(100.0, 22.0),
                egui::Sense::click(),
            );
            let painter = ui.painter();
            painter.rect_filled(ba_rect, 4.0, ba_bg_actual);
            painter.rect_stroke(ba_rect, 4.0, egui::Stroke::new(0.5, ba_border));
            painter.text(
                ba_rect.center(),
                egui::Align2::CENTER_CENTER,
                ba_label,
                egui::FontId::proportional(10.0),
                text_accent,
            );
            if ba_resp.clicked() {
                cmds.push(AppCommand::ToggleBeforeAfter);
            }

            ui.add_space(12.0);
        });

        // Preset popup menu (rendered outside the horizontal layout)
        if state.show_preset_menu {
            let menu_bg = egui::Color32::from_rgb(0x18, 0x18, 0x1e);
            let menu_border = egui::Color32::from_rgb(0x3a, 0x3a, 0x44);
            let text_dim = egui::Color32::from_rgb(0x90, 0x90, 0xa0);
            let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
            let hover_bg = egui::Color32::from_rgb(0x28, 0x28, 0x32);

            let popup_id = ui.id().with("preset_popup");
            let mut close_menu = false;

            egui::Area::new(popup_id)
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(ui.min_rect().min.x + 12.0, ui.min_rect().min.y - 4.0))
                .pivot(egui::Align2::LEFT_BOTTOM)
                .show(ui.ctx(), |ui| {
                    egui::Frame::none()
                        .fill(menu_bg)
                        .stroke(egui::Stroke::new(0.5, menu_border))
                        .rounding(4.0)
                        .inner_margin(egui::Margin::same(4.0))
                        .show(ui, |ui| {
                            ui.set_min_width(160.0);

                            // Header
                            ui.label(egui::RichText::new("PRESETS").size(9.0).color(
                                egui::Color32::from_rgb(0x50, 0x50, 0x60),
                            ));
                            ui.add_space(2.0);

                            // Save current
                            let save_resp = ui.horizontal(|ui| {
                                let r = ui.label(egui::RichText::new("Save current...").size(10.0).color(text_accent));
                                r.interact(egui::Sense::click())
                            }).inner;
                            if save_resp.clicked() {
                                state.show_preset_save = true;
                                close_menu = true;
                            }

                            ui.add_space(2.0);
                            let sep_rect = egui::Rect::from_min_size(
                                egui::pos2(ui.min_rect().min.x, ui.cursor().min.y),
                                egui::vec2(ui.available_width(), 0.5),
                            );
                            ui.painter().rect_filled(sep_rect, 0.0, menu_border);
                            ui.add_space(4.0);

                            // Preset list
                            let presets = state.grade_presets.clone();
                            for (name, grade) in &presets {
                                let (item_rect, item_resp) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), 18.0),
                                    egui::Sense::click(),
                                );
                                if item_resp.hovered() {
                                    ui.painter().rect_filled(item_rect, 2.0, hover_bg);
                                }
                                ui.painter().text(
                                    egui::pos2(item_rect.min.x + 6.0, item_rect.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    name,
                                    egui::FontId::proportional(10.0),
                                    text_dim,
                                );
                                if item_resp.clicked() {
                                    if let Some(clip_id) = state.selected_clip {
                                        // Get old grade for undo
                                        let old_grade = state.sequences[state.active_sequence_idx].tracks.iter()
                                            .flat_map(|t| t.clips.iter())
                                            .find(|c| c.id == clip_id)
                                            .map(|c| c.color_grade.clone())
                                            .unwrap_or_default();

                                        // Apply preset by setting all params
                                        cmds.push(AppCommand::ResetClipColorGrade { clip_id });
                                        use ColorGradeParam as P;
                                        let g = grade;
                                        let params: &[(P, f32)] = &[
                                            (P::Exposure, g.exposure), (P::Contrast, g.contrast),
                                            (P::Saturation, g.saturation), (P::Temperature, g.temperature),
                                            (P::Tint, g.tint), (P::Highlights, g.highlights),
                                            (P::Shadows, g.shadows), (P::Whites, g.whites),
                                            (P::Blacks, g.blacks),
                                            (P::LiftX, g.lift_x), (P::LiftY, g.lift_y),
                                            (P::LiftMaster, g.lift_master),
                                            (P::GammaX, g.gamma_x), (P::GammaY, g.gamma_y),
                                            (P::GammaMaster, g.gamma_master),
                                            (P::GainX, g.gain_x), (P::GainY, g.gain_y),
                                            (P::GainMaster, g.gain_master),
                                        ];
                                        for &(param, value) in params {
                                            cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param, value });
                                        }

                                        // Push undo
                                        state.history.push(crate::history::HistoryEntry {
                                            sequence_idx: state.active_sequence_idx,
                                            command: crate::history::EditCommand::SetColorGrade {
                                                clip_id,
                                                old_grade,
                                                new_grade: grade.clone(),
                                            },
                                        });
                                    }
                                    close_menu = true;
                                }
                            }
                        });
                });

            // Close menu on click outside or after selection
            if close_menu || ui.input(|i| i.key_pressed(egui::Key::Escape))
                || ui.input(|i| i.pointer.any_pressed() && !state.show_preset_menu) {
                // Defer close to next frame to avoid flicker
            }
            if close_menu {
                state.show_preset_menu = false;
            }
        }

        // Save preset dialog
        if state.show_preset_save {
            let popup_id = ui.id().with("preset_save_popup");
            egui::Area::new(popup_id)
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(ui.min_rect().min.x + 50.0, ui.min_rect().min.y - 50.0))
                .pivot(egui::Align2::LEFT_BOTTOM)
                .show(ui.ctx(), |ui| {
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(0x18, 0x18, 0x1e))
                        .stroke(egui::Stroke::new(0.5, egui::Color32::from_rgb(0x3a, 0x3a, 0x44)))
                        .rounding(4.0)
                        .inner_margin(egui::Margin::same(8.0))
                        .show(ui, |ui| {
                            ui.set_min_width(180.0);
                            let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
                            let text_dim = egui::Color32::from_rgb(0x90, 0x90, 0xa0);

                            ui.label(egui::RichText::new("Save Preset").size(11.0).color(text_accent));
                            ui.add_space(4.0);

                            let te = egui::TextEdit::singleline(&mut state.preset_save_name)
                                .hint_text("Preset name")
                                .font(egui::FontId::proportional(10.0))
                                .desired_width(160.0);
                            let te_resp = ui.add(te);
                            if (te_resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)))
                                && !state.preset_save_name.trim().is_empty() {
                                    if let Some(clip_id) = state.selected_clip {
                                        let grade = state.sequences[state.active_sequence_idx].tracks.iter()
                                            .flat_map(|t| t.clips.iter())
                                            .find(|c| c.id == clip_id)
                                            .map(|c| c.color_grade.clone());
                                        if let Some(grade) = grade {
                                            let _ = crate::presets::save_preset(&state.preset_save_name, &grade);
                                            // Reload presets
                                            state.grade_presets = crate::presets::load_all_presets();
                                        }
                                    }
                                    state.preset_save_name.clear();
                                    state.show_preset_save = false;
                            }
                            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                state.preset_save_name.clear();
                                state.show_preset_save = false;
                            }

                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                if ui.small_button("Cancel").clicked() {
                                    state.preset_save_name.clear();
                                    state.show_preset_save = false;
                                }
                                ui.label(egui::RichText::new("Enter to save").size(8.0).color(text_dim));
                            });
                        });
                });
        }
    }
}

fn section_header(ui: &mut egui::Ui, label: &str, mut on_reset: impl FnMut() -> bool) {
    let header_color = egui::Color32::from_rgb(0x50, 0x50, 0x60);
    let reset_color = egui::Color32::from_rgb(0x36, 0x36, 0x40);

    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.label(
            egui::RichText::new(label)
                .size(9.0)
                .color(header_color),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(16.0);
            let resp = ui.label(
                egui::RichText::new("↺ reset")
                    .size(9.0)
                    .color(reset_color),
            );
            if resp.interact(egui::Sense::click()).clicked() {
                on_reset();
            }
        });
    });
    ui.add_space(6.0);
}

/// Section header with reset button that dispatches AppCommands.
fn section_header_with_reset(
    ui: &mut egui::Ui,
    label: &str,
    make_reset_cmds: impl FnOnce() -> Vec<AppCommand>,
    cmds: &mut Vec<AppCommand>,
) {
    let header_color = egui::Color32::from_rgb(0x50, 0x50, 0x60);
    let reset_color = egui::Color32::from_rgb(0x36, 0x36, 0x40);

    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.label(
            egui::RichText::new(label)
                .size(9.0)
                .color(header_color),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(16.0);
            let resp = ui.label(
                egui::RichText::new("↺ reset")
                    .size(9.0)
                    .color(reset_color),
            );
            if resp.interact(egui::Sense::click()).clicked() {
                clicked = true;
            }
        });
    });
    if clicked {
        cmds.extend(make_reset_cmds());
    }
    ui.add_space(6.0);
}

fn footer_button(
    ui: &mut egui::Ui,
    label: &str,
    text_color: egui::Color32,
    bg_color: egui::Color32,
    border_color: egui::Color32,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(72.0, 22.0),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, bg_color);
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(0.5, border_color));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(10.0),
        text_color,
    );
    resp.clicked()
}
