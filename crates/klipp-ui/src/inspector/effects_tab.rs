use crate::theme::KlippTheme;
use crate::state::AppState;
use klipp_core::AppCommand;
use klipp_timeline::Clip;

pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
    show_add_builtin: &mut bool,
    show_add_ofx: &mut bool,
) {
    let clip_id = clip.id;

    // ── Applied Effects (built-in + OFX combined) ──
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("APPLIED EFFECTS")
                .size(10.0)
                .strong()
                .color(theme.text_dim),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new("drag to reorder")
                    .size(9.0)
                    .color(theme.text_dim),
            );
        });
    });
    ui.add_space(7.0);

    let has_any = !clip.builtin_effects.is_empty() || !clip.effects.is_empty();

    if !has_any {
        ui.label(
            egui::RichText::new("No effects applied")
                .size(11.0)
                .color(theme.text_dim),
        );
        ui.add_space(8.0);
    }

    // ── Built-in effects ──
    let mut builtin_remove = None;
    let mut builtin_move_up = None;
    let mut builtin_move_down = None;

    for (i, effect) in clip.builtin_effects.iter().enumerate() {
        let name = builtin_effect_name(effect);
        let header_id = ui.make_persistent_id(format!("bfx_{clip_id:?}_{i}"));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), header_id, true)
            .show_header(ui, |ui| {
                ui.label(egui::RichText::new(name).size(11.0).strong().color(theme.text_primary));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("\u{2715}").on_hover_text("Remove").clicked() {
                        builtin_remove = Some(i);
                    }
                    if i + 1 < clip.builtin_effects.len()
                        && ui.small_button("\u{25BC}").on_hover_text("Move down").clicked()
                    {
                        builtin_move_down = Some(i);
                    }
                    if i > 0
                        && ui.small_button("\u{25B2}").on_hover_text("Move up").clicked()
                    {
                        builtin_move_up = Some(i);
                    }
                });
            })
            .body(|ui| {
                if let Some(updated) = show_builtin_effect_params(ui, theme, effect) {
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

    // ── OFX effects ──
    let mut ofx_remove = None;
    let mut ofx_move_up = None;
    let mut ofx_move_down = None;

    for (i, effect) in clip.effects.iter().enumerate() {
        let display_name = effect.plugin_name
            .rsplit_once('.')
            .map(|(_, name)| name)
            .unwrap_or(&effect.plugin_name);

        let header_id = ui.make_persistent_id(format!("ofx_{clip_id:?}_{i}"));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), header_id, true)
            .show_header(ui, |ui| {
                let mut enabled = effect.enabled;
                if ui.checkbox(&mut enabled, "").changed() {
                    cmds.push(AppCommand::SetEffectEnabled {
                        clip_id,
                        effect_index: i,
                        enabled,
                    });
                }
                ui.label(egui::RichText::new(display_name).size(11.0).strong().color(theme.text_primary));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("\u{2715}").on_hover_text("Remove").clicked() {
                        ofx_remove = Some(i);
                    }
                    if i + 1 < clip.effects.len()
                        && ui.small_button("\u{25BC}").on_hover_text("Move down").clicked()
                    {
                        ofx_move_down = Some(i);
                    }
                    if i > 0
                        && ui.small_button("\u{25B2}").on_hover_text("Move up").clicked()
                    {
                        ofx_move_up = Some(i);
                    }
                });
            })
            .body(|ui| {
                // Show parameter controls if plugin has declared params
                if let Some(plugin_info) = state.available_plugins.iter().find(|p| p.name == effect.plugin_name) {
                    for desc in &plugin_info.parameters {
                        if desc.secret {
                            continue;
                        }
                        match desc.param_type {
                            klipp_plugin::ParamType::Group | klipp_plugin::ParamType::Page => continue,
                            _ => {}
                        }
                        show_ofx_param(ui, theme, clip_id, i, effect, desc, cmds);
                    }
                    if plugin_info.parameters.is_empty() {
                        ui.label(
                            egui::RichText::new("No parameters")
                                .size(10.0)
                                .color(theme.text_dim),
                        );
                    }
                }
            });
        ui.add_space(2.0);
    }

    if let Some(idx) = ofx_remove {
        cmds.push(AppCommand::RemoveEffect { clip_id, effect_index: idx });
    }
    if let Some(idx) = ofx_move_up {
        cmds.push(AppCommand::ReorderEffect { clip_id, from_index: idx, to_index: idx - 1 });
    }
    if let Some(idx) = ofx_move_down {
        cmds.push(AppCommand::ReorderEffect { clip_id, from_index: idx, to_index: idx + 1 });
    }

    // ── Add Effect buttons ──
    ui.add_space(8.0);

    // Built-in add button
    let add_btn = egui::Button::new(
        egui::RichText::new("\u{FF0B} Add Built-in Effect")
            .size(11.0)
            .color(theme.text_dim),
    )
    .fill(egui::Color32::TRANSPARENT)
    .stroke(egui::Stroke::new(1.0, theme.border_light))
    .rounding(5.0)
    .min_size(egui::vec2(ui.available_width() - 20.0, 28.0));

    if ui.add(add_btn).clicked() {
        *show_add_builtin = !*show_add_builtin;
        *show_add_ofx = false;
    }

    if *show_add_builtin {
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
                *show_add_builtin = false;
            }
        }
        ui.add_space(4.0);
        if ui.small_button("Cancel").clicked() {
            *show_add_builtin = false;
        }
    }

    // OFX add button
    ui.add_space(4.0);
    let add_ofx_btn = egui::Button::new(
        egui::RichText::new("\u{FF0B} Add OFX Plugin")
            .size(11.0)
            .color(theme.text_dim),
    )
    .fill(egui::Color32::TRANSPARENT)
    .stroke(egui::Stroke::new(1.0, theme.border_light))
    .rounding(5.0)
    .min_size(egui::vec2(ui.available_width() - 20.0, 28.0));

    if ui.add(add_ofx_btn).clicked() {
        *show_add_ofx = !*show_add_ofx;
        *show_add_builtin = false;
    }

    if *show_add_ofx {
        ui.add_space(4.0);
        if state.available_plugins.is_empty() {
            ui.label(
                egui::RichText::new("No OFX plugins found")
                    .size(10.0)
                    .color(theme.text_dim),
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
                *show_add_ofx = false;
            }
        }
        ui.add_space(4.0);
        if ui.small_button("Cancel").clicked() {
            *show_add_ofx = false;
        }
    }
}

fn show_ofx_param(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    effect_index: usize,
    effect: &klipp_plugin::AppliedEffect,
    desc: &klipp_plugin::ParamDescriptor,
    cmds: &mut Vec<AppCommand>,
) {
    let current_value = effect.param_values.get(&desc.name);

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(&desc.label)
                .size(10.0)
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
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Double(v as f64),
                    });
                }
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
                        effect_index,
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
                        effect_index,
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
                egui::ComboBox::from_id_salt(egui::Id::new(("ofx_choice", clip_id, effect_index, &desc.name)))
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for (ci, choice) in desc.choices.iter().enumerate() {
                            if ui.selectable_value(&mut v, ci as i32, choice).changed() {
                                cmds.push(AppCommand::SetEffectParam {
                                    clip_id,
                                    effect_index,
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
                        effect_index,
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
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Rgb(
                            color.r() as f64 / 255.0,
                            color.g() as f64 / 255.0,
                            color.b() as f64 / 255.0,
                        ),
                    });
                }
            }
            _ => {
                ui.label(
                    egui::RichText::new(format!("{:?}", desc.param_type))
                        .size(10.0)
                        .color(egui::Color32::from_rgb(100, 100, 110)),
                );
            }
        }
    });
}

fn builtin_effect_name(effect: &klipp_effects::BuiltinEffect) -> &'static str {
    match effect {
        klipp_effects::BuiltinEffect::Opacity { .. } => "Opacity",
        klipp_effects::BuiltinEffect::GaussianBlur { .. } => "Gaussian Blur",
        klipp_effects::BuiltinEffect::Sharpen { .. } => "Sharpen",
        klipp_effects::BuiltinEffect::Crop { .. } => "Crop",
        klipp_effects::BuiltinEffect::Transform { .. } => "Transform",
        klipp_effects::BuiltinEffect::ChromaKey { .. } => "Chroma Key",
    }
}

fn show_builtin_effect_params(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    effect: &klipp_effects::BuiltinEffect,
) -> Option<klipp_effects::BuiltinEffect> {
    let mut updated = effect.clone();
    let mut changed = false;

    match &mut updated {
        klipp_effects::BuiltinEffect::Opacity { value } => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Value").size(10.0));
                changed |= ui.add(egui::Slider::new(value, 0.0..=1.0).step_by(0.01_f64)).changed();
            });
        }
        klipp_effects::BuiltinEffect::GaussianBlur { radius } => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Radius").size(10.0));
                changed |= ui.add(egui::Slider::new(radius, 0.0..=100.0).step_by(0.5_f64)).changed();
            });
        }
        klipp_effects::BuiltinEffect::Sharpen { amount } => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Amount").size(10.0));
                changed |= ui.add(egui::Slider::new(amount, 0.0..=2.0).step_by(0.01_f64)).changed();
            });
        }
        klipp_effects::BuiltinEffect::Crop { left, right, top, bottom, feather } => {
            for (label, val) in [("Left", left as &mut f32), ("Right", right), ("Top", top), ("Bottom", bottom), ("Feather", feather)] {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).size(10.0));
                    changed |= ui.add(egui::Slider::new(val, 0.0..=1.0).step_by(0.01_f64)).changed();
                });
            }
        }
        klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y } => {
            for (label, val, range) in [
                ("Pos X", position_x as &mut f32, -2000.0..=2000.0_f32),
                ("Pos Y", position_y, -2000.0..=2000.0),
                ("Scale X", scale_x, 0.0..=4.0),
                ("Scale Y", scale_y, 0.0..=4.0),
                ("Rotation", rotation, -360.0..=360.0),
                ("Anchor X", anchor_x, 0.0..=1.0),
                ("Anchor Y", anchor_y, 0.0..=1.0),
            ] {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).size(10.0));
                    changed |= ui.add(egui::Slider::new(val, range).step_by(0.01_f64)).changed();
                });
            }
        }
        klipp_effects::BuiltinEffect::ChromaKey { key_color, tolerance, spill_suppression } => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Key Color").size(10.0));
                let mut color = egui::Color32::from_rgb(key_color[0], key_color[1], key_color[2]);
                if ui.color_edit_button_srgba(&mut color).changed() {
                    key_color[0] = color.r();
                    key_color[1] = color.g();
                    key_color[2] = color.b();
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Tolerance").size(10.0));
                changed |= ui.add(egui::Slider::new(tolerance, 0.0..=1.0).step_by(0.01_f64)).changed();
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Spill").size(10.0));
                changed |= ui.add(egui::Slider::new(spill_suppression, 0.0..=1.0).step_by(0.01_f64)).changed();
            });
        }
    }

    if changed { Some(updated) } else { None }
}
