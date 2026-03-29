use crate::icons::Icon;
use crate::theme::KlippTheme;
use crate::state::AppState;
use crate::widgets::scrubby_param::{scrubby_param_row, KeyframeState};
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
    clip_time_secs: f64,
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
    let mut builtin_reset: Option<(usize, klipp_effects::BuiltinEffect)> = None;

    for (i, effect) in clip.builtin_effects.iter().enumerate() {
        let name = builtin_effect_name(effect);
        let header_id = ui.make_persistent_id(format!("bfx_{clip_id:?}_{i}"));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), header_id, true)
            .show_header(ui, |ui| {
                ui.label(egui::RichText::new(name).size(11.0).strong().color(theme.text_primary));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::ImageButton::new(Icon::X.image(12.0).tint(theme.text_dim)).frame(false)).on_hover_text("Remove").clicked() {
                        builtin_remove = Some(i);
                    }
                    if i + 1 < clip.builtin_effects.len()
                        && ui.add(egui::ImageButton::new(Icon::CaretDown.image(12.0).tint(theme.text_dim)).frame(false)).on_hover_text("Move down").clicked()
                    {
                        builtin_move_down = Some(i);
                    }
                    if i > 0
                        && ui.add(egui::ImageButton::new(Icon::CaretUp.image(12.0).tint(theme.text_dim)).frame(false)).on_hover_text("Move up").clicked()
                    {
                        builtin_move_up = Some(i);
                    }
                    // Reset-all icon for effects with multiple params
                    if builtin_effect_has_multi_params(effect) && !builtin_effect_is_default(effect) {
                        let reset = ui.add(egui::ImageButton::new(
                            Icon::ArrowCounterClockwise.image(12.0).tint(theme.text_dim)
                        ).frame(false));
                        if reset.on_hover_text("Reset all parameters").clicked() {
                            builtin_reset = Some((i, builtin_effect_default(effect)));
                        }
                    }
                });
            })
            .body(|ui| {
                if let Some(updated) = show_builtin_effect_params(ui, theme, effect, clip_id, i, cmds, clip, clip_time_secs) {
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
    if let Some((idx, default_effect)) = builtin_reset {
        cmds.push(AppCommand::UpdateBuiltinEffect { clip_id, index: idx, effect: default_effect });
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
                ui.label(egui::RichText::new("OFX").size(8.0).color(egui::Color32::from_rgb(80, 120, 200)).strong());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::ImageButton::new(Icon::X.image(12.0).tint(theme.text_dim)).frame(false)).on_hover_text("Remove").clicked() {
                        ofx_remove = Some(i);
                    }
                    if i + 1 < clip.effects.len()
                        && ui.add(egui::ImageButton::new(Icon::CaretDown.image(12.0).tint(theme.text_dim)).frame(false)).on_hover_text("Move down").clicked()
                    {
                        ofx_move_down = Some(i);
                    }
                    if i > 0
                        && ui.add(egui::ImageButton::new(Icon::CaretUp.image(12.0).tint(theme.text_dim)).frame(false)).on_hover_text("Move up").clicked()
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
        egui::RichText::new("+ Add Built-in Effect")
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
        egui::RichText::new("+ Add OFX Plugin")
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
        klipp_effects::BuiltinEffect::Bloom { .. } => "Bloom",
        klipp_effects::BuiltinEffect::Glow { .. } => "Glow",
        klipp_effects::BuiltinEffect::LightRays { .. } => "Light Rays",
    }
}

/// Returns the default (identity) version of the same effect variant.
fn builtin_effect_default(effect: &klipp_effects::BuiltinEffect) -> klipp_effects::BuiltinEffect {
    use klipp_effects::BuiltinEffect;
    match effect {
        BuiltinEffect::Opacity { .. } => BuiltinEffect::Opacity { value: 1.0 },
        BuiltinEffect::GaussianBlur { .. } => BuiltinEffect::GaussianBlur { radius: 0.0 },
        BuiltinEffect::Sharpen { .. } => BuiltinEffect::Sharpen { amount: 0.0 },
        BuiltinEffect::Crop { .. } => BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0 },
        BuiltinEffect::Transform { .. } => BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
        },
        BuiltinEffect::ChromaKey { .. } => BuiltinEffect::ChromaKey {
            key_color: [0, 255, 0], tolerance: 0.2, spill_suppression: 0.1,
        },
        BuiltinEffect::Bloom { .. } => BuiltinEffect::Bloom {
            threshold: 0.7, radius: 20.0, intensity: 0.5,
        },
        BuiltinEffect::Glow { .. } => BuiltinEffect::Glow {
            radius: 30.0, intensity: 0.4,
        },
        BuiltinEffect::LightRays { .. } => BuiltinEffect::LightRays {
            center_x: 0.5, center_y: 0.3, threshold: 0.6, samples: 64, decay: 0.3, intensity: 0.5,
        },
    }
}

/// Returns true if the effect has more than 1 adjustable parameter (worth a reset-all button).
fn builtin_effect_has_multi_params(effect: &klipp_effects::BuiltinEffect) -> bool {
    use klipp_effects::BuiltinEffect;
    matches!(effect, BuiltinEffect::Crop { .. } | BuiltinEffect::Transform { .. } | BuiltinEffect::ChromaKey { .. } | BuiltinEffect::Bloom { .. } | BuiltinEffect::LightRays { .. })
}

/// Returns true if the effect is at its default values.
fn builtin_effect_is_default(effect: &klipp_effects::BuiltinEffect) -> bool {
    let def = builtin_effect_default(effect);
    // Compare by debug string — simple and correct for all variants
    format!("{:?}", effect) == format!("{:?}", def)
}

/// Compute KeyframeState for a given effect param keyframe vec at a given clip-relative time.
fn effect_keyframe_state(clip: &Clip, effect_index: usize, param_name: &str, clip_time_secs: f64) -> KeyframeState {
    let key = format!("{}:{}", effect_index, param_name);
    let kfs = clip.effect_keyframes.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
    let kf_tolerance = 0.02;
    KeyframeState {
        has_keyframe_at_playhead: kfs.iter().any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance),
        has_any_keyframes: !kfs.is_empty(),
        prev_keyframe_time: kfs.iter()
            .filter(|k| k.time_secs < clip_time_secs - kf_tolerance)
            .last()
            .map(|k| k.time_secs),
        next_keyframe_time: kfs.iter()
            .find(|k| k.time_secs > clip_time_secs + kf_tolerance)
            .map(|k| k.time_secs),
        current_interpolation: kfs.iter()
            .find(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance)
            .map(|k| k.interpolation),
    }
}

/// Evaluate effect keyframes for a param, returning the keyframed value or the static default.
fn eval_effect_kf(clip: &Clip, effect_index: usize, param_name: &str, clip_time_secs: f64, default: f32) -> f32 {
    let key = format!("{}:{}", effect_index, param_name);
    if let Some(kfs) = clip.effect_keyframes.get(&key) {
        if !kfs.is_empty() {
            return klipp_core::keyframe::evaluate(kfs, clip_time_secs, default);
        }
    }
    default
}

/// Check if any keyframes exist for a given effect param.
fn has_effect_kfs(clip: &Clip, effect_index: usize, param_name: &str) -> bool {
    let key = format!("{}:{}", effect_index, param_name);
    clip.effect_keyframes.get(&key).map_or(false, |kfs| !kfs.is_empty())
}

fn show_builtin_effect_params(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    effect: &klipp_effects::BuiltinEffect,
    clip_id: klipp_core::ClipId,
    effect_index: usize,
    cmds: &mut Vec<AppCommand>,
    clip: &Clip,
    clip_time_secs: f64,
) -> Option<klipp_effects::BuiltinEffect> {
    let mut updated = effect.clone();
    let mut changed = false;
    let mut any_drag_started = false;
    let mut any_drag_stopped = false;
    let drag_key = format!("{:?}:builtin:{}", clip_id, effect_index);
    let clip_timeline_in_secs = clip.timeline_in.to_secs_f64();
    let clip_rate = clip.timeline_in.rate;
    let kf_tolerance = 0.02;

    // Collect keyframe responses: (param_name, ScrubbyParamResponse, current_value)
    let mut kf_responses: Vec<(&str, crate::widgets::scrubby_param::ScrubbyParamResponse, f32)> = Vec::new();

    // Helper: process a scrubby response for drag coalescing
    macro_rules! track_scrubby {
        ($param_name:expr, $r:expr, $val:expr) => {{
            let r = $r;
            changed |= r.changed;
            if r.drag_started { any_drag_started = true; }
            if r.drag_stopped { any_drag_stopped = true; }
            kf_responses.push(($param_name, r, $val));
        }};
    }

    match &mut updated {
        klipp_effects::BuiltinEffect::Opacity { value } => {
            *value = eval_effect_kf(clip, effect_index, "value", clip_time_secs, *value);
            let ks = Some(effect_keyframe_state(clip, effect_index, "value", clip_time_secs));
            let r = scrubby_param_row(ui, "Opacity", value, 0.0..=1.0, 1.0, 0.005, 2, "", ks);
            track_scrubby!("value", r, *value);
        }
        klipp_effects::BuiltinEffect::GaussianBlur { radius } => {
            *radius = eval_effect_kf(clip, effect_index, "radius", clip_time_secs, *radius);
            let ks = Some(effect_keyframe_state(clip, effect_index, "radius", clip_time_secs));
            let r = scrubby_param_row(ui, "Radius", radius, 0.0..=100.0, 0.0, 0.5, 1, " px", ks);
            track_scrubby!("radius", r, *radius);
        }
        klipp_effects::BuiltinEffect::Sharpen { amount } => {
            *amount = eval_effect_kf(clip, effect_index, "amount", clip_time_secs, *amount);
            let ks = Some(effect_keyframe_state(clip, effect_index, "amount", clip_time_secs));
            let r = scrubby_param_row(ui, "Amount", amount, 0.0..=2.0, 0.5, 0.01, 2, "", ks);
            track_scrubby!("amount", r, *amount);
        }
        klipp_effects::BuiltinEffect::Crop { left, right, top, bottom, feather } => {
            *left = eval_effect_kf(clip, effect_index, "left", clip_time_secs, *left);
            *right = eval_effect_kf(clip, effect_index, "right", clip_time_secs, *right);
            *top = eval_effect_kf(clip, effect_index, "top", clip_time_secs, *top);
            *bottom = eval_effect_kf(clip, effect_index, "bottom", clip_time_secs, *bottom);
            *feather = eval_effect_kf(clip, effect_index, "feather", clip_time_secs, *feather);

            let ks_l = Some(effect_keyframe_state(clip, effect_index, "left", clip_time_secs));
            let ks_r = Some(effect_keyframe_state(clip, effect_index, "right", clip_time_secs));
            let ks_t = Some(effect_keyframe_state(clip, effect_index, "top", clip_time_secs));
            let ks_b = Some(effect_keyframe_state(clip, effect_index, "bottom", clip_time_secs));
            let ks_f = Some(effect_keyframe_state(clip, effect_index, "feather", clip_time_secs));

            let r_l = scrubby_param_row(ui, "Left", left, 0.0..=1.0, 0.0, 0.005, 3, "", ks_l);
            track_scrubby!("left", r_l, *left);
            let r_r = scrubby_param_row(ui, "Right", right, 0.0..=1.0, 0.0, 0.005, 3, "", ks_r);
            track_scrubby!("right", r_r, *right);
            let r_t = scrubby_param_row(ui, "Top", top, 0.0..=1.0, 0.0, 0.005, 3, "", ks_t);
            track_scrubby!("top", r_t, *top);
            let r_b = scrubby_param_row(ui, "Bottom", bottom, 0.0..=1.0, 0.0, 0.005, 3, "", ks_b);
            track_scrubby!("bottom", r_b, *bottom);
            let r_f = scrubby_param_row(ui, "Feather", feather, 0.0..=1.0, 0.0, 0.005, 3, "", ks_f);
            track_scrubby!("feather", r_f, *feather);
        }
        klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y } => {
            // Transform in effects tab: no keyframes (handled by video tab's dedicated keyframes)
            let r = scrubby_param_row(ui, "Pos X", position_x, -2000.0..=2000.0, 0.0, 1.0, 0, " px", None);
            track_scrubby!("", r, *position_x);
            let r = scrubby_param_row(ui, "Pos Y", position_y, -2000.0..=2000.0, 0.0, 1.0, 0, " px", None);
            track_scrubby!("", r, *position_y);
            let r = scrubby_param_row(ui, "Scale X", scale_x, 0.0..=4.0, 1.0, 0.01, 2, "\u{00d7}", None);
            track_scrubby!("", r, *scale_x);
            let r = scrubby_param_row(ui, "Scale Y", scale_y, 0.0..=4.0, 1.0, 0.01, 2, "\u{00d7}", None);
            track_scrubby!("", r, *scale_y);
            let r = scrubby_param_row(ui, "Rotation", rotation, -360.0..=360.0, 0.0, 0.5, 1, " \u{00b0}", None);
            track_scrubby!("", r, *rotation);
            let r = scrubby_param_row(ui, "Anchor X", anchor_x, 0.0..=1.0, 0.5, 0.005, 3, "", None);
            track_scrubby!("", r, *anchor_x);
            let r = scrubby_param_row(ui, "Anchor Y", anchor_y, 0.0..=1.0, 0.5, 0.005, 3, "", None);
            track_scrubby!("", r, *anchor_y);
        }
        klipp_effects::BuiltinEffect::ChromaKey { key_color, tolerance, spill_suppression } => {
            // Color picker stays as-is (not a scrubby candidate)
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
            *tolerance = eval_effect_kf(clip, effect_index, "tolerance", clip_time_secs, *tolerance);
            *spill_suppression = eval_effect_kf(clip, effect_index, "spill", clip_time_secs, *spill_suppression);

            let ks_t = Some(effect_keyframe_state(clip, effect_index, "tolerance", clip_time_secs));
            let ks_s = Some(effect_keyframe_state(clip, effect_index, "spill", clip_time_secs));

            let r_t = scrubby_param_row(ui, "Tolerance", tolerance, 0.0..=1.0, 0.2, 0.005, 3, "", ks_t);
            track_scrubby!("tolerance", r_t, *tolerance);
            let r_s = scrubby_param_row(ui, "Spill", spill_suppression, 0.0..=1.0, 0.1, 0.005, 3, "", ks_s);
            track_scrubby!("spill", r_s, *spill_suppression);
        }
        klipp_effects::BuiltinEffect::Bloom { threshold, radius, intensity } => {
            *threshold = eval_effect_kf(clip, effect_index, "threshold", clip_time_secs, *threshold);
            *radius = eval_effect_kf(clip, effect_index, "radius", clip_time_secs, *radius);
            *intensity = eval_effect_kf(clip, effect_index, "intensity", clip_time_secs, *intensity);

            let ks_th = Some(effect_keyframe_state(clip, effect_index, "threshold", clip_time_secs));
            let ks_r = Some(effect_keyframe_state(clip, effect_index, "radius", clip_time_secs));
            let ks_i = Some(effect_keyframe_state(clip, effect_index, "intensity", clip_time_secs));

            let r_th = scrubby_param_row(ui, "Threshold", threshold, 0.0..=1.0, 0.7, 0.005, 2, "", ks_th);
            track_scrubby!("threshold", r_th, *threshold);
            let r_r = scrubby_param_row(ui, "Radius", radius, 0.0..=100.0, 20.0, 0.5, 1, " px", ks_r);
            track_scrubby!("radius", r_r, *radius);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=2.0, 0.5, 0.01, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
        }
        klipp_effects::BuiltinEffect::Glow { radius, intensity } => {
            *radius = eval_effect_kf(clip, effect_index, "radius", clip_time_secs, *radius);
            *intensity = eval_effect_kf(clip, effect_index, "intensity", clip_time_secs, *intensity);

            let ks_r = Some(effect_keyframe_state(clip, effect_index, "radius", clip_time_secs));
            let ks_i = Some(effect_keyframe_state(clip, effect_index, "intensity", clip_time_secs));

            let r_r = scrubby_param_row(ui, "Radius", radius, 0.0..=100.0, 30.0, 0.5, 1, " px", ks_r);
            track_scrubby!("radius", r_r, *radius);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=2.0, 0.4, 0.01, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
        }
        klipp_effects::BuiltinEffect::LightRays { center_x, center_y, threshold, samples, decay, intensity } => {
            *center_x = eval_effect_kf(clip, effect_index, "center_x", clip_time_secs, *center_x);
            *center_y = eval_effect_kf(clip, effect_index, "center_y", clip_time_secs, *center_y);
            *threshold = eval_effect_kf(clip, effect_index, "threshold", clip_time_secs, *threshold);
            *decay = eval_effect_kf(clip, effect_index, "decay", clip_time_secs, *decay);
            *intensity = eval_effect_kf(clip, effect_index, "intensity", clip_time_secs, *intensity);

            let ks_cx = Some(effect_keyframe_state(clip, effect_index, "center_x", clip_time_secs));
            let ks_cy = Some(effect_keyframe_state(clip, effect_index, "center_y", clip_time_secs));
            let ks_th = Some(effect_keyframe_state(clip, effect_index, "threshold", clip_time_secs));
            let ks_d = Some(effect_keyframe_state(clip, effect_index, "decay", clip_time_secs));
            let ks_i = Some(effect_keyframe_state(clip, effect_index, "intensity", clip_time_secs));

            let r_cx = scrubby_param_row(ui, "Center X", center_x, 0.0..=1.0, 0.5, 0.005, 3, "", ks_cx);
            track_scrubby!("center_x", r_cx, *center_x);
            let r_cy = scrubby_param_row(ui, "Center Y", center_y, 0.0..=1.0, 0.3, 0.005, 3, "", ks_cy);
            track_scrubby!("center_y", r_cy, *center_y);
            let r_th = scrubby_param_row(ui, "Threshold", threshold, 0.0..=1.0, 0.6, 0.005, 2, "", ks_th);
            track_scrubby!("threshold", r_th, *threshold);
            // samples is u32, convert to f32 for slider then back
            let mut samples_f = *samples as f32;
            let ks_s = None; // samples not typically keyframed
            let r_s = scrubby_param_row(ui, "Samples", &mut samples_f, 4.0..=128.0, 64.0, 1.0, 0, "", ks_s);
            *samples = samples_f as u32;
            track_scrubby!("", r_s, samples_f);
            let r_d = scrubby_param_row(ui, "Decay", decay, 0.0..=1.0, 0.3, 0.005, 3, "", ks_d);
            track_scrubby!("decay", r_d, *decay);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=2.0, 0.5, 0.01, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
        }
    }

    // Emit begin_drag at the start of a drag gesture
    if any_drag_started {
        cmds.push(AppCommand::BeginEffectDrag {
            key: drag_key.clone(),
            clip_id,
            effect_index,
            old_effect: effect.clone(),
        });
    }

    // Emit end_drag when the drag gesture completes
    if any_drag_stopped {
        cmds.push(AppCommand::EndEffectDrag {
            key: drag_key,
            clip_id,
            effect_index,
            new_effect: updated.clone(),
        });
    }

    // Handle keyframe toggle/nav and auto-update for each param
    for (param_name, resp, value) in &kf_responses {
        if param_name.is_empty() {
            continue; // Transform/Opacity in effects tab -- no effect keyframes
        }

        // Auto-update keyframe at playhead when slider changes and keyframes exist
        if resp.changed && has_effect_kfs(clip, effect_index, param_name) {
            cmds.push(AppCommand::AddEffectKeyframe {
                clip_id,
                effect_index,
                param_name: param_name.to_string(),
                time_secs: clip_time_secs,
                value: *value,
            });
        }

        // Toggle keyframe on diamond click
        if resp.keyframe_toggled {
            let key = format!("{}:{}", effect_index, param_name);
            let has_at_playhead = clip.effect_keyframes.get(&key)
                .map_or(false, |kfs| kfs.iter().any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance));
            if has_at_playhead {
                cmds.push(AppCommand::RemoveEffectKeyframe {
                    clip_id,
                    effect_index,
                    param_name: param_name.to_string(),
                    time_secs: clip_time_secs,
                });
            } else {
                cmds.push(AppCommand::AddEffectKeyframe {
                    clip_id,
                    effect_index,
                    param_name: param_name.to_string(),
                    time_secs: clip_time_secs,
                    value: *value,
                });
            }
        }

        // Navigate to previous keyframe
        if resp.keyframe_nav_prev {
            let ks = effect_keyframe_state(clip, effect_index, param_name, clip_time_secs);
            if let Some(t) = ks.prev_keyframe_time {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
            }
        }

        // Navigate to next keyframe
        if resp.keyframe_nav_next {
            let ks = effect_keyframe_state(clip, effect_index, param_name, clip_time_secs);
            if let Some(t) = ks.next_keyframe_time {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
            }
        }
    }

    if changed { Some(updated) } else { None }
}
