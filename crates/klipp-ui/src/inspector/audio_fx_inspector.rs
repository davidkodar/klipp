use crate::theme::KlippTheme;
use klipp_core::{AppCommand, AudioEffectInstance, ClipId};

/// Show the audio effects chain for a clip.
/// Returns commands for the app to process.
pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    audio_effects: &[AudioEffectInstance],
    cmds: &mut Vec<AppCommand>,
) {
    // ── Section header ──
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("AUDIO EFFECTS")
                .size(10.0)
                .strong()
                .color(theme.text_dim),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Add Effect dropdown
            let btn = ui.add(
                egui::Button::new(
                    egui::RichText::new("+ Add Effect")
                        .size(10.0)
                        .color(theme.text_accent),
                )
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(1.0, theme.accent.linear_multiply(0.5))),
            );
            let popup_id = ui.id().with("add_audio_fx_popup");
            if btn.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            egui::popup_below_widget(ui, popup_id, &btn, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(160.0);
                for (type_id, name) in klipp_audio::dsp::available_effects() {
                    if ui.button(name).clicked() {
                        cmds.push(AppCommand::AddAudioEffect {
                            clip_id,
                            effect_type: type_id.to_string(),
                        });
                    }
                }
            });
        });
    });

    ui.add_space(4.0);

    show_content(ui, theme, clip_id, audio_effects, cmds);
}

/// Show the audio effects content without the section header.
/// Used when the header is provided by a section_card wrapper.
pub fn show_content(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    audio_effects: &[AudioEffectInstance],
    cmds: &mut Vec<AppCommand>,
) {
    // "+ Add Effect" button
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let btn = ui.add(
                egui::Button::new(
                    egui::RichText::new("+ Add Effect")
                        .size(10.0)
                        .color(theme.text_accent),
                )
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(1.0, theme.accent.linear_multiply(0.5))),
            );
            let popup_id = ui.id().with("add_audio_fx_popup_card");
            if btn.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            egui::popup_below_widget(ui, popup_id, &btn, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(160.0);
                for (type_id, name) in klipp_audio::dsp::available_effects() {
                    if ui.button(name).clicked() {
                        cmds.push(AppCommand::AddAudioEffect {
                            clip_id,
                            effect_type: type_id.to_string(),
                        });
                    }
                }
            });
        });
    });

    ui.add_space(4.0);

    if audio_effects.is_empty() {
        ui.label(
            egui::RichText::new("No audio effects")
                .size(10.0)
                .color(theme.text_dim),
        );
        return;
    }

    // ── Effect cards ──
    let mut remove_idx = None;
    for (idx, inst) in audio_effects.iter().enumerate() {
        let effect_name = klipp_audio::dsp::create_effect(&inst.effect_type, 48000)
            .map(|fx| fx.name().to_string())
            .unwrap_or_else(|| inst.effect_type.clone());

        let card_id = ui.id().with(("audio_fx_card", idx));
        let collapsed_key = card_id.with("collapsed");
        let is_collapsed: bool = ui.ctx().data_mut(|d| d.get_temp(collapsed_key).unwrap_or(false));

        egui::Frame::none()
            .fill(theme.bg_raised)
            .stroke(egui::Stroke::new(1.0, theme.border))
            .rounding(4.0)
            .inner_margin(egui::Margin::symmetric(8.0, 6.0))
            .outer_margin(egui::Margin::symmetric(0.0, 2.0))
            .show(ui, |ui| {
                // ── Header row ──
                ui.horizontal(|ui| {
                    // Collapse triangle
                    let arrow = if is_collapsed { "\u{25B6}" } else { "\u{25BC}" };
                    let arrow_resp = ui.add(
                        egui::Label::new(
                            egui::RichText::new(arrow)
                                .size(9.0)
                                .color(theme.text_dim),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if arrow_resp.clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(collapsed_key, !is_collapsed));
                    }

                    // Effect name
                    let name_resp = ui.add(
                        egui::Label::new(
                            egui::RichText::new(&effect_name)
                                .size(11.0)
                                .strong()
                                .color(if inst.enabled { theme.text_primary } else { theme.text_dim }),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if name_resp.clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(collapsed_key, !is_collapsed));
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Remove button
                        let rm_resp = ui.add(
                            egui::Label::new(
                                egui::RichText::new("\u{00D7}")
                                    .size(11.0)
                                    .color(theme.text_dim),
                            )
                            .sense(egui::Sense::click()),
                        );
                        if rm_resp.clicked() {
                            remove_idx = Some(idx);
                        }
                        if rm_resp.hovered() {
                            ui.painter().rect_filled(rm_resp.rect, 2.0, egui::Color32::from_rgba_premultiplied(200, 60, 60, 40));
                        }

                        // Enabled checkbox
                        let check_text = if inst.enabled { "\u{2611}" } else { "\u{2610}" };
                        let check_color = if inst.enabled { theme.accent } else { theme.text_dim };
                        let check_resp = ui.add(
                            egui::Label::new(
                                egui::RichText::new(check_text)
                                    .size(13.0)
                                    .color(check_color),
                            )
                            .sense(egui::Sense::click()),
                        );
                        if check_resp.clicked() {
                            cmds.push(AppCommand::ToggleAudioEffect {
                                clip_id,
                                effect_index: idx,
                            });
                        }
                    });
                });

                if !is_collapsed {
                    ui.add_space(4.0);

                    // ── Parameters ──
                    if let Some(effect) = klipp_audio::dsp::create_effect(&inst.effect_type, 48000) {
                        let descs = effect.param_descriptors();
                        for desc in descs {
                            let current_value = inst.params.iter()
                                .find(|(k, _)| k == desc.name)
                                .map(|(_, v)| *v)
                                .unwrap_or(desc.default);

                            let mut val = current_value;
                            let unit_suffix = match desc.unit {
                                klipp_audio::dsp::ParamUnit::Db => " dB",
                                klipp_audio::dsp::ParamUnit::Hz => " Hz",
                                klipp_audio::dsp::ParamUnit::Ms => " ms",
                                klipp_audio::dsp::ParamUnit::Percent => "%",
                                klipp_audio::dsp::ParamUnit::Ratio => ":1",
                                klipp_audio::dsp::ParamUnit::None => "",
                            };

                            // Determine drag speed based on range
                            let range = desc.max - desc.min;
                            let speed = range * 0.003;
                            let decimals = if range > 100.0 { 0 } else if range > 10.0 { 1 } else { 2 };

                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(desc.label)
                                        .size(10.0)
                                        .color(theme.text_secondary),
                                );

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let drag = ui.add(
                                        egui::DragValue::new(&mut val)
                                            .range(desc.min..=desc.max)
                                            .speed(speed)
                                            .max_decimals(decimals)
                                            .suffix(unit_suffix),
                                    );
                                    if drag.changed() {
                                        cmds.push(AppCommand::UpdateAudioEffectParam {
                                            clip_id,
                                            effect_index: idx,
                                            param_name: desc.name.to_string(),
                                            value: val,
                                        });
                                    }
                                });
                            });
                        }
                    }

                    ui.add_space(4.0);

                    // ── Wet/Dry slider ──
                    let mut wet_dry = inst.wet_dry;
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Wet/Dry")
                                .size(10.0)
                                .color(theme.text_secondary),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let wd = ui.add(
                                egui::DragValue::new(&mut wet_dry)
                                    .range(0.0..=1.0)
                                    .speed(0.005)
                                    .max_decimals(2)
                                    .suffix("%")
                                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                            );
                            if wd.changed() {
                                cmds.push(AppCommand::SetAudioEffectWetDry {
                                    clip_id,
                                    effect_index: idx,
                                    wet_dry,
                                });
                            }
                        });
                    });
                }
            });
    }

    if let Some(idx) = remove_idx {
        cmds.push(AppCommand::RemoveAudioEffect {
            clip_id,
            effect_index: idx,
        });
    }
}
