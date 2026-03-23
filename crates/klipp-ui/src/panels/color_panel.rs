use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::{color_slider, color_wheel, curves_editor, scopes};
use klipp_core::{AppCommand, ColorGradeParam};
use klipp_engine::SharedFrameBuffer;

type SliderDef = (f32, &'static str, fn(f32) -> String, f32, std::ops::RangeInclusive<f32>, ColorGradeParam);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorPanelTab {
    Basic,
    Wheels,
    Curves,
}

pub struct ColorPanel {
    pub active_tab: ColorPanelTab,
    pub lift_wheel: color_wheel::ColorWheelState,
    pub gamma_wheel: color_wheel::ColorWheelState,
    pub gain_wheel: color_wheel::ColorWheelState,
    pub scope_state: scopes::ScopeState,
    pub active_curve_channel: curves_editor::CurveChannel,
}

impl Default for ColorPanel {
    fn default() -> Self {
        Self {
            active_tab: ColorPanelTab::Wheels,
            lift_wheel: Default::default(),
            gamma_wheel: Default::default(),
            gain_wheel: Default::default(),
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
                    (ColorPanelTab::Basic, "BASIC"),
                    (ColorPanelTab::Wheels, "WHEELS"),
                    (ColorPanelTab::Curves, "CURVES"),
                ];
                let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
                let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);

                for (tab, label) in &tabs {
                    let is_active = self.active_tab == *tab;
                    let (tab_rect, resp) = ui.allocate_exact_size(
                        egui::vec2(56.0, tab_bar_height),
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

        // Split content: scopes on right when active
        let scope_width = if show_scopes { 220.0_f32.min(full_content_rect.width() * 0.35) } else { 0.0 };
        let controls_rect = egui::Rect::from_min_max(
            full_content_rect.min,
            egui::pos2(full_content_rect.max.x - scope_width, full_content_rect.max.y),
        );

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(controls_rect), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("color_panel_content")
                .show(ui, |ui| {
                    match self.active_tab {
                        ColorPanelTab::Basic => {
                            self.show_basic_tab(ui, state, theme, &mut cmds);
                        }
                        ColorPanelTab::Wheels => {
                            self.show_wheels_tab(ui, state, theme, &mut cmds);
                        }
                        ColorPanelTab::Curves => {
                            self.show_curves_tab(ui, state, theme, &mut cmds);
                        }
                    }
                });
        });

        // Scopes panel (right side when active)
        if show_scopes {
            let scope_rect = egui::Rect::from_min_max(
                egui::pos2(full_content_rect.max.x - scope_width, full_content_rect.min.y),
                full_content_rect.max,
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

        // Footer
        let footer_rect = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, rect.max.y - footer_height),
            rect.max,
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

    fn show_basic_tab(
        &self,
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

        ui.add_space(8.0);

        let mut any_drag_started = false;
        let mut any_drag_ended = false;
        let mut any_changed = false;

        // TONE section
        section_header(ui, "TONE", || true);

        let tone_sliders: &[SliderDef] = &[
            (grade.exposure,   "Exposure",   |v| format!("{v:+.1}"),  0.0, -5.0..=5.0,   ColorGradeParam::Exposure),
            (grade.contrast,   "Contrast",   |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,   ColorGradeParam::Contrast),
            (grade.highlights, "Highlights", |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,   ColorGradeParam::Highlights),
            (grade.shadows,    "Shadows",    |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,   ColorGradeParam::Shadows),
            (grade.whites,     "Whites",     |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,   ColorGradeParam::Whites),
            (grade.blacks,     "Blacks",     |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,   ColorGradeParam::Blacks),
        ];

        for &(mut val, label, fmt, def, ref range, param) in tone_sliders {
            let resp = color_slider::color_slider(ui, theme, label, &mut val, range.clone(), def, fmt);
            any_drag_started |= resp.drag_started;
            any_drag_ended |= resp.drag_ended;
            if resp.changed {
                any_changed = true;
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param, value: val });
            }
            ui.add_space(3.0);
        }

        ui.add_space(8.0);

        // COLOR section
        section_header(ui, "COLOR", || true);

        let color_sliders: &[SliderDef] = &[
            (grade.saturation,  "Saturation",  |v| format!("{v:.2}"),   1.0, 0.0..=2.0,       ColorGradeParam::Saturation),
            (grade.temperature, "Temperature", |v| format!("{v:+.0}"),  0.0, -100.0..=100.0,  ColorGradeParam::Temperature),
            (grade.tint,        "Tint",        |v| format!("{v:+.0}"),  0.0, -100.0..=100.0,  ColorGradeParam::Tint),
        ];

        for &(mut val, label, fmt, def, ref range, param) in color_sliders {
            let resp = color_slider::color_slider(ui, theme, label, &mut val, range.clone(), def, fmt);
            any_drag_started |= resp.drag_started;
            any_drag_ended |= resp.drag_ended;
            if resp.changed {
                any_changed = true;
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param, value: val });
            }
            ui.add_space(3.0);
        }

        ui.add_space(12.0);

        // LUT section
        self.show_lut_section(ui, state, theme, cmds);

        // Undo: capture start grade on drag start, push undo on drag end
        if any_drag_started {
            // Capture the grade BEFORE this frame's changes
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
                // Current clip grade has all changes applied from previous frames
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

        // For single-click changes (not drag), push undo immediately
        if any_changed && !any_drag_started && !any_drag_ended && state.color_grade_drag_start.is_none() {
            state.history.push(crate::history::HistoryEntry {
                sequence_idx: state.active_sequence_idx,
                command: crate::history::EditCommand::SetColorGrade {
                    clip_id,
                    old_grade: grade.clone(), // grade is the pre-change snapshot
                    new_grade: grade.clone(), // will be updated when command is applied; close enough for single-click
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
        let Some(mut grade) = grade else { return };

        let mut any_drag_started = false;
        let mut any_drag_ended = false;
        let mut any_changed = false;

        ui.add_space(8.0);

        // ── Three wheels in a row ──
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let wheel_spacing = (ui.available_width() - 120.0 * 3.0 - 16.0).max(0.0) / 2.0;

            // LIFT
            let mut lift_off = [grade.lift_x, grade.lift_y];
            let mut lift_m = grade.lift_master;
            let resp = color_wheel::color_wheel(ui, &mut self.lift_wheel, theme, "LIFT", &mut lift_off, &mut lift_m);
            any_drag_started |= resp.drag_started;
            any_drag_ended |= resp.drag_ended;
            if resp.changed {
                any_changed = true;
                grade.lift_x = lift_off[0];
                grade.lift_y = lift_off[1];
                grade.lift_master = lift_m;
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: ColorGradeParam::LiftX, value: lift_off[0] });
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: ColorGradeParam::LiftY, value: lift_off[1] });
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: ColorGradeParam::LiftMaster, value: lift_m });
            }

            ui.add_space(wheel_spacing);

            // GAMMA
            let mut gamma_off = [grade.gamma_x, grade.gamma_y];
            let mut gamma_m = grade.gamma_master;
            let resp = color_wheel::color_wheel(ui, &mut self.gamma_wheel, theme, "GAMMA", &mut gamma_off, &mut gamma_m);
            any_drag_started |= resp.drag_started;
            any_drag_ended |= resp.drag_ended;
            if resp.changed {
                any_changed = true;
                grade.gamma_x = gamma_off[0];
                grade.gamma_y = gamma_off[1];
                grade.gamma_master = gamma_m;
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: ColorGradeParam::GammaX, value: gamma_off[0] });
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: ColorGradeParam::GammaY, value: gamma_off[1] });
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: ColorGradeParam::GammaMaster, value: gamma_m });
            }

            ui.add_space(wheel_spacing);

            // GAIN
            let mut gain_off = [grade.gain_x, grade.gain_y];
            let mut gain_m = grade.gain_master;
            let resp = color_wheel::color_wheel(ui, &mut self.gain_wheel, theme, "GAIN", &mut gain_off, &mut gain_m);
            any_drag_started |= resp.drag_started;
            any_drag_ended |= resp.drag_ended;
            if resp.changed {
                any_changed = true;
                grade.gain_x = gain_off[0];
                grade.gain_y = gain_off[1];
                grade.gain_master = gain_m;
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: ColorGradeParam::GainX, value: gain_off[0] });
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: ColorGradeParam::GainY, value: gain_off[1] });
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param: ColorGradeParam::GainMaster, value: gain_m });
            }

            ui.add_space(8.0);
        });

        // Divider
        let border_color = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);
        let div_rect = egui::Rect::from_min_size(
            egui::pos2(ui.min_rect().min.x + 12.0, ui.cursor().min.y),
            egui::vec2(ui.available_width() - 24.0, 0.5),
        );
        ui.painter().rect_filled(div_rect, 0.0, border_color);
        ui.add_space(8.0);

        // ── Sliders below wheels ──
        // TONE section
        section_header(ui, "TONE", || true);

        let sliders: &[SliderDef] = &[
            (grade.exposure,   "Exposure",   |v| format!("{v:+.1}"),  0.0, -5.0..=5.0,     ColorGradeParam::Exposure),
            (grade.contrast,   "Contrast",   |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,     ColorGradeParam::Contrast),
            (grade.highlights, "Highlights", |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,     ColorGradeParam::Highlights),
            (grade.shadows,    "Shadows",    |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,     ColorGradeParam::Shadows),
            (grade.whites,     "Whites",     |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,     ColorGradeParam::Whites),
            (grade.blacks,     "Blacks",     |v| format!("{v:+.2}"),  0.0, -1.0..=1.0,     ColorGradeParam::Blacks),
        ];

        for &(mut val, label, fmt, def, ref range, param) in sliders {
            let resp = color_slider::color_slider(ui, theme, label, &mut val, range.clone(), def, fmt);
            any_drag_started |= resp.drag_started;
            any_drag_ended |= resp.drag_ended;
            if resp.changed {
                any_changed = true;
                cmds.push(AppCommand::SetClipColorGradeParam { clip_id, param, value: val });
            }
            ui.add_space(3.0);
        }

        ui.add_space(8.0);

        // COLOR section
        section_header(ui, "COLOR", || true);

        let color_sliders: &[SliderDef] = &[
            (grade.saturation,  "Saturation",  |v| format!("{v:.2}"),   1.0, 0.0..=2.0,       ColorGradeParam::Saturation),
            (grade.temperature, "Temperature", |v| format!("{v:+.0}"),  0.0, -100.0..=100.0,  ColorGradeParam::Temperature),
            (grade.tint,        "Tint",        |v| format!("{v:+.0}"),  0.0, -100.0..=100.0,  ColorGradeParam::Tint),
        ];

        for &(mut val, label, fmt, def, ref range, param) in color_sliders {
            let resp = color_slider::color_slider(ui, theme, label, &mut val, range.clone(), def, fmt);
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

        let mut curves = {
            let seq = &state.sequences[state.active_sequence_idx];
            seq.tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| c.color_grade.curves.clone())
                .unwrap_or_default()
        };

        ui.add_space(8.0);

        // Channel tabs
        let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);

        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let channels = [
                (curves_editor::CurveChannel::Master, "Master", egui::Color32::from_rgb(0xcc, 0xcc, 0xcc)),
                (curves_editor::CurveChannel::Red, "Red", egui::Color32::from_rgb(0xe0, 0x60, 0x60)),
                (curves_editor::CurveChannel::Green, "Green", egui::Color32::from_rgb(0x60, 0xc0, 0x60)),
                (curves_editor::CurveChannel::Blue, "Blue", egui::Color32::from_rgb(0x60, 0x70, 0xe0)),
            ];
            for (ch, label, color) in &channels {
                let active = self.active_curve_channel == *ch;
                let text_c = if active { *color } else { text_dim };
                let resp = ui.label(egui::RichText::new(*label).size(10.0).color(text_c));
                if active {
                    let r = resp.rect;
                    ui.painter().hline(r.x_range(), r.max.y + 1.0, egui::Stroke::new(1.5, *color));
                }
                if resp.interact(egui::Sense::click()).clicked() {
                    self.active_curve_channel = *ch;
                }
                ui.add_space(8.0);
            }

            // Reset curves button
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(16.0);
                let reset_resp = ui.label(egui::RichText::new("↺ reset").size(9.0).color(text_dim));
                if reset_resp.interact(egui::Sense::click()).clicked() {
                    curves = klipp_timeline::CurveSet::default();
                    cmds.push(AppCommand::SetClipCurves { clip_id, curves: curves.clone() });
                }
            });
        });

        ui.add_space(8.0);

        // Curves editor — use a local copy of the active channel's points
        // to avoid borrow conflicts with the full CurveSet
        let mut channel_pts = match self.active_curve_channel {
            curves_editor::CurveChannel::Master => curves.master.clone(),
            curves_editor::CurveChannel::Red => curves.red.clone(),
            curves_editor::CurveChannel::Green => curves.green.clone(),
            curves_editor::CurveChannel::Blue => curves.blue.clone(),
        };

        let resp = {
            let mut resp_out = None;
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                resp_out = Some(curves_editor::curves_editor(
                    ui,
                    theme,
                    self.active_curve_channel,
                    &mut channel_pts,
                ));
            });
            resp_out.unwrap()
        };

        // Write back changed points
        if resp.changed {
            match self.active_curve_channel {
                curves_editor::CurveChannel::Master => curves.master = channel_pts.clone(),
                curves_editor::CurveChannel::Red => curves.red = channel_pts.clone(),
                curves_editor::CurveChannel::Green => curves.green = channel_pts.clone(),
                curves_editor::CurveChannel::Blue => curves.blue = channel_pts,
            }
            cmds.push(AppCommand::SetClipCurves { clip_id, curves: curves.clone() });
        }

        if resp.drag_started {
            let old_grade = state.sequences[state.active_sequence_idx].tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|c| c.color_grade.clone());
            state.color_grade_drag_start = old_grade;
        }

        if resp.drag_ended {
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

        // Push undo for single-click add/remove
        if resp.changed && !resp.drag_started && !resp.drag_ended && state.color_grade_drag_start.is_none() {
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
                        .add_filter("LUT files", &["cube"])
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
