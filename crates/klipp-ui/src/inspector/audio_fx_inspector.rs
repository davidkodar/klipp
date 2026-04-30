use crate::theme::KlippTheme;
use klipp_audio::ladspa::LadspaPluginInfo;
use klipp_core::{AppCommand, AudioEffectInstance, ClipId};

/// Render the body of the "+ Add Effect" popup. Contents:
///   - Search box (filters by name; for LADSPA also matches maker + label)
///   - "BUILT-IN" section with the 13 built-in audio effects
///   - "LADSPA" section grouped by maker (alphabetical)
/// Calls `on_select(effect_type)` when the user picks an item; the caller
/// is responsible for pushing the right `AppCommand::AddAudioEffect`.
/// `popup_anchor_id` keys the search-query memory so each popup keeps its
/// own filter state across frames.
fn add_effect_picker(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    ladspa_plugins: &[LadspaPluginInfo],
    popup_anchor_id: egui::Id,
    on_select: &mut dyn FnMut(String),
) {
    // Width: wide enough for typical LADSPA plugin names without truncation,
    // but not so wide it dominates the inspector. ~280 px matches the
    // patterns used elsewhere in the UI (AI Models dialog).
    ui.set_min_width(280.0);
    ui.set_max_width(320.0);

    // ── Search box ──
    let query_key = popup_anchor_id.with("search_query");
    let mut query: String = ui
        .ctx()
        .data_mut(|d| d.get_temp::<String>(query_key).unwrap_or_default());
    let resp = ui.add(
        egui::TextEdit::singleline(&mut query)
            .hint_text("Search effects...")
            .desired_width(f32::INFINITY),
    );
    if resp.changed() {
        ui.ctx().data_mut(|d| d.insert_temp(query_key, query.clone()));
    }
    let search_lower = query.to_lowercase();

    ui.add_space(4.0);

    // The popup uses CloseOnClick — any button click closes the popup. To
    // avoid losing the popup on every keystroke in the search box, the
    // outer wiring (egui::popup_below_widget) handles this fine because
    // TextEdit::changed() doesn't fire popup-close. But we still want to
    // be defensive: don't bail on empty results, just show a message.

    let max_height = 320.0_f32;
    egui::ScrollArea::vertical()
        .max_height(max_height)
        .show(ui, |ui| {
            // ── Built-in section ──
            let builtins: Vec<(&'static str, &'static str)> = klipp_audio::dsp::available_effects()
                .into_iter()
                .filter(|(_, name)| {
                    search_lower.is_empty() || name.to_lowercase().contains(&search_lower)
                })
                .collect();

            if !builtins.is_empty() {
                ui.label(
                    egui::RichText::new("BUILT-IN")
                        .size(9.0)
                        .strong()
                        .color(theme.text_dim),
                );
                ui.add_space(2.0);
                for (type_id, name) in &builtins {
                    if ui.button(*name).clicked() {
                        on_select(type_id.to_string());
                    }
                }
            }

            // ── LADSPA section, grouped by maker ──
            let mut visible_ladspa: Vec<&LadspaPluginInfo> = ladspa_plugins
                .iter()
                .filter(|p| {
                    if search_lower.is_empty() {
                        return true;
                    }
                    let display = if p.name.is_empty() { &p.label } else { &p.name };
                    display.to_lowercase().contains(&search_lower)
                        || p.maker.to_lowercase().contains(&search_lower)
                        || p.label.to_lowercase().contains(&search_lower)
                })
                .collect();
            visible_ladspa.sort_by(|a, b| {
                a.maker
                    .to_lowercase()
                    .cmp(&b.maker.to_lowercase())
                    .then_with(|| {
                        let an = if a.name.is_empty() { &a.label } else { &a.name };
                        let bn = if b.name.is_empty() { &b.label } else { &b.name };
                        an.to_lowercase().cmp(&bn.to_lowercase())
                    })
            });

            if !visible_ladspa.is_empty() {
                if !builtins.is_empty() {
                    ui.add_space(8.0);
                }
                ui.label(
                    egui::RichText::new("LADSPA")
                        .size(9.0)
                        .strong()
                        .color(theme.text_dim),
                );
                ui.add_space(2.0);

                let mut last_maker: Option<String> = None;
                for plugin in &visible_ladspa {
                    let maker = if plugin.maker.is_empty() {
                        "Other".to_string()
                    } else {
                        plugin.maker.clone()
                    };
                    if last_maker.as_ref() != Some(&maker) {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(maker.to_uppercase())
                                .size(8.0)
                                .color(theme.text_secondary),
                        );
                        last_maker = Some(maker);
                    }

                    let display = if plugin.name.is_empty() {
                        &plugin.label
                    } else {
                        &plugin.name
                    };
                    if ui.button(display).clicked() {
                        let type_id = format!(
                            "ladspa:{}:{}",
                            plugin.path.to_string_lossy(),
                            plugin.label
                        );
                        on_select(type_id);
                    }
                }
            }

            if builtins.is_empty() && visible_ladspa.is_empty() {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(if search_lower.is_empty() {
                            "No audio effects available".to_string()
                        } else {
                            format!("No effects match \"{query}\"")
                        })
                        .size(10.0)
                        .color(theme.text_dim),
                    );
                });
            }
        });

    // Footer count line so the user knows how many things are searchable.
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(format!(
            "{} built-in   |   {} LADSPA",
            klipp_audio::dsp::available_effects().len(),
            ladspa_plugins.len(),
        ))
        .size(9.0)
        .color(theme.text_dim),
    );
}

/// Show the audio effects chain for a clip.
/// Returns commands for the app to process.
pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    audio_effects: &[AudioEffectInstance],
    ladspa_plugins: &[LadspaPluginInfo],
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
                .stroke(egui::Stroke::new(1.0_f32, theme.accent.linear_multiply(0.5))),
            );
            let popup_id = ui.id().with("add_audio_fx_popup");
            if btn.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            egui::popup_below_widget(ui, popup_id, &btn, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                add_effect_picker(ui, theme, ladspa_plugins, popup_id, &mut |type_id| {
                    cmds.push(AppCommand::AddAudioEffect { clip_id, effect_type: type_id });
                });
            });
        });
    });

    ui.add_space(4.0);

    show_content(ui, theme, clip_id, audio_effects, ladspa_plugins, cmds);
}

/// Show the audio effects content without the section header.
/// Used when the header is provided by a section_card wrapper.
pub fn show_content(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    audio_effects: &[AudioEffectInstance],
    ladspa_plugins: &[LadspaPluginInfo],
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
                .stroke(egui::Stroke::new(1.0_f32, theme.accent.linear_multiply(0.5))),
            );
            let popup_id = ui.id().with("add_audio_fx_popup_card");
            if btn.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            egui::popup_below_widget(ui, popup_id, &btn, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                add_effect_picker(ui, theme, ladspa_plugins, popup_id, &mut |type_id| {
                    cmds.push(AppCommand::AddAudioEffect { clip_id, effect_type: type_id });
                });
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
        // LADSPA lookup — avoid dlopen-per-frame by short-circuiting on the
        // "ladspa:" prefix and resolving the display name from the discovery
        // cache.
        let ladspa_info: Option<&LadspaPluginInfo> =
            parse_ladspa_type_id(&inst.effect_type).and_then(|(path, label)| {
                ladspa_plugins.iter().find(|p| {
                    p.path.to_string_lossy() == path && p.label == label
                })
            });

        let effect_name = if let Some(info) = ladspa_info {
            if info.name.is_empty() { info.label.clone() } else { info.name.clone() }
        } else if inst.effect_type.starts_with("ladspa:") {
            // Plugin uninstalled — show the raw effect_type so the user knows
            // what's missing.
            inst.effect_type.clone()
        } else {
            klipp_audio::dsp::create_effect(&inst.effect_type, 48000)
                .map(|fx| fx.name().to_string())
                .unwrap_or_else(|| inst.effect_type.clone())
        };

        let card_id = ui.id().with(("audio_fx_card", idx));
        let collapsed_key = card_id.with("collapsed");
        let is_collapsed: bool = ui.ctx().data_mut(|d| d.get_temp(collapsed_key).unwrap_or(false));

        egui::Frame::none()
            .fill(theme.bg_raised)
            .stroke(egui::Stroke::new(1.0_f32, theme.border))
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
                    if let Some(info) = ladspa_info {
                        render_ladspa_params(ui, theme, clip_id, idx, inst, info, cmds);
                    } else if inst.effect_type.starts_with("ladspa:") {
                        ui.label(
                            egui::RichText::new(format!(
                                "LADSPA plugin not found: {}",
                                inst.effect_type.strip_prefix("ladspa:").unwrap_or("")
                            ))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(200, 140, 60)),
                        );
                    } else if let Some(effect) =
                        klipp_audio::dsp::create_effect(&inst.effect_type, 48000)
                    {
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

/// Parse `"ladspa:{path}:{label}"` into `(path, label)`. Returns None for
/// non-LADSPA effect_types or malformed strings. Splits on the LAST `:` so
/// Windows paths with drive letters don't get chopped.
fn parse_ladspa_type_id(type_id: &str) -> Option<(&str, &str)> {
    let rest = type_id.strip_prefix("ladspa:")?;
    let last_colon = rest.rfind(':')?;
    Some((&rest[..last_colon], &rest[last_colon + 1..]))
}

/// Render parameter sliders for a LADSPA plugin. Uses the port metadata
/// from discovery (min / max / integer / logarithmic / toggle) to pick
/// an appropriate widget per port.
fn render_ladspa_params(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    effect_index: usize,
    inst: &AudioEffectInstance,
    info: &LadspaPluginInfo,
    cmds: &mut Vec<AppCommand>,
) {
    if info.control_inputs.is_empty() {
        ui.label(
            egui::RichText::new("(no parameters)")
                .size(10.0)
                .color(theme.text_dim),
        );
        return;
    }

    for port in &info.control_inputs {
        let current_value = inst
            .params
            .iter()
            .find(|(k, _)| k == &port.name)
            .map(|(_, v)| *v)
            .unwrap_or(port.default);

        let label = if port.name.is_empty() {
            format!("port {}", port.index)
        } else {
            port.name.clone()
        };

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(&label)
                    .size(10.0)
                    .color(theme.text_secondary),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut val = current_value;

                if port.is_toggle {
                    let mut on = val >= 0.5;
                    let resp = ui.checkbox(&mut on, "");
                    if resp.changed() {
                        let new_val = if on { 1.0 } else { 0.0 };
                        cmds.push(AppCommand::UpdateAudioEffectParam {
                            clip_id,
                            effect_index,
                            param_name: port.name.clone(),
                            value: new_val,
                        });
                    }
                    return;
                }

                let range = (port.max - port.min).max(f32::EPSILON);
                let speed = (range * 0.003).max(0.0001);
                let decimals = if port.is_integer {
                    0
                } else if range > 100.0 {
                    1
                } else if range > 10.0 {
                    2
                } else {
                    3
                };

                let unit_suffix = match klipp_audio::ladspa::unit_hint_for_port_name(&port.name) {
                    klipp_audio::dsp::ParamUnit::Db => " dB",
                    klipp_audio::dsp::ParamUnit::Hz => " Hz",
                    klipp_audio::dsp::ParamUnit::Ms => " ms",
                    klipp_audio::dsp::ParamUnit::Percent => "%",
                    klipp_audio::dsp::ParamUnit::Ratio => ":1",
                    klipp_audio::dsp::ParamUnit::None => "",
                };

                let mut drag = egui::DragValue::new(&mut val)
                    .range(port.min..=port.max)
                    .speed(speed)
                    .max_decimals(decimals)
                    .suffix(unit_suffix);
                if port.is_integer {
                    drag = drag.fixed_decimals(0);
                }

                let resp = ui.add(drag);
                if resp.changed() {
                    cmds.push(AppCommand::UpdateAudioEffectParam {
                        clip_id,
                        effect_index,
                        param_name: port.name.clone(),
                        value: val,
                    });
                }
            });
        });
    }
}
