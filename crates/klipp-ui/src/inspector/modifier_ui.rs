use crate::theme::KlippTheme;
use klipp_core::modifier::{Modifier, ModifierBlend, ModifierStack, ParamId, Waveform};
use klipp_core::{AppCommand, ClipId};

/// All modifier variant names for the "Add Behavior" dropdown.
const MODIFIER_LABELS: &[&str] = &[
    "Oscillate",
    "Wiggle",
    "Throw",
    "Ramp",
    "Loop",
    "Ping Pong",
    "Clamp",
    "Outside Keys",
];

/// Show a "Behaviors" section for a list of parameter IDs.
/// Used by both video and audio tabs.
pub fn show_behaviors_section(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &klipp_timeline::Clip,
    params: &[ParamId],
    cmds: &mut Vec<AppCommand>,
) {
    let clip_id = clip.id;

    // Section header
    ui.label(
        egui::RichText::new("BEHAVIORS")
            .size(10.0)
            .strong()
            .color(theme.text_dim),
    );
    ui.add_space(4.0);

    // For each parameter, show its modifier stack and add dropdown
    for param in params {
        show_behavior_for_param(ui, theme, clip_id, clip, param, cmds);
    }
}

/// Render behaviors content without the section header (for use inside a section_card).
pub fn show_behaviors_content(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &klipp_timeline::Clip,
    params: &[ParamId],
    cmds: &mut Vec<AppCommand>,
) {
    let clip_id = clip.id;
    for param in params {
        show_behavior_for_param(ui, theme, clip_id, clip, param, cmds);
    }
}

fn show_behavior_for_param(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    clip: &klipp_timeline::Clip,
    param: &ParamId,
    cmds: &mut Vec<AppCommand>,
) {
    {
        let stack = clip.param_modifiers().get(param);
        let count = stack.map_or(0, |s| s.modifiers.len());

        // Parameter sub-header
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(param.label())
                    .size(10.0)
                    .color(theme.text_secondary),
            );
            if count > 0 {
                ui.label(
                    egui::RichText::new(format!("({count})"))
                        .size(10.0)
                        .color(theme.text_dim),
                );
            }
        });
        ui.add_space(2.0);

        // Show existing modifiers
        if let Some(s) = stack {
            if !s.modifiers.is_empty() {
                show_modifier_stack(ui, theme, clip_id, param, s, cmds);
            }
        }

        // "Add Behavior" dropdown
        show_add_dropdown(ui, theme, clip_id, param, cmds);
        ui.add_space(6.0);
    }
}

/// Show the full modifier stack UI for a single parameter.
fn show_modifier_stack(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    param_id: &ParamId,
    stack: &ModifierStack,
    cmds: &mut Vec<AppCommand>,
) {
    let id_salt = format!("mod_stack_{:?}_{:?}", clip_id, param_id);

    let mut remove_idx: Option<usize> = None;
    for (idx, modifier) in stack.modifiers.iter().enumerate() {
        let mod_id = format!("{id_salt}_{idx}");
        egui::Frame::none()
            .fill(theme.bg_raised)
            .stroke(egui::Stroke::new(0.5_f32, theme.border))
            .rounding(2.0)
            .inner_margin(egui::Margin::symmetric(6.0, 4.0))
            .show(ui, |ui| {
                // Header row: name + enabled toggle + remove button
                ui.horizontal(|ui| {
                    let label_color = if modifier.is_enabled() {
                        theme.text_primary
                    } else {
                        theme.text_dim
                    };
                    ui.label(
                        egui::RichText::new(modifier.label())
                            .size(11.0)
                            .strong()
                            .color(label_color),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Remove button
                        let remove_resp = ui.add(
                            egui::Button::new(
                                egui::RichText::new("\u{00D7}").size(10.0).color(theme.text_dim),
                            )
                            .fill(egui::Color32::TRANSPARENT)
                            .min_size(egui::vec2(16.0, 16.0)),
                        );
                        if remove_resp.clicked() {
                            remove_idx = Some(idx);
                        }
                        remove_resp.on_hover_text("Remove behavior");

                        // Enabled toggle
                        let mut enabled = modifier.is_enabled();
                        if ui.checkbox(&mut enabled, "").changed() {
                            cmds.push(AppCommand::ToggleModifier {
                                clip_id,
                                param: param_id.clone(),
                                modifier_index: idx,
                            });
                        }
                    });
                });

                // Per-modifier parameter editors
                show_modifier_params(ui, clip_id, param_id, idx, modifier, cmds, &mod_id);
            });
        ui.add_space(2.0);
    }

    // Process deferred removal
    if let Some(idx) = remove_idx {
        cmds.push(AppCommand::RemoveModifier {
            clip_id,
            param: param_id.clone(),
            modifier_index: idx,
        });
    }
}

/// Show the "Add Behavior" dropdown for a parameter.
fn show_add_dropdown(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    param_id: &ParamId,
    cmds: &mut Vec<AppCommand>,
) {
    let id_salt = format!("add_mod_{:?}_{:?}", clip_id, param_id);
    let mut add_cmd = None;
    let mut job = egui::text::LayoutJob::default();
    job.append("+ ", 0.0, egui::TextFormat {
        font_id: egui::FontId::proportional(10.0),
        color: theme.accent,
        ..Default::default()
    });
    job.append("Add Behavior\u{2026}", 0.0, egui::TextFormat {
        font_id: egui::FontId::proportional(10.0),
        color: theme.text_secondary,
        ..Default::default()
    });
    egui::ComboBox::from_id_salt(&id_salt)
        .selected_text(job)
        .width(ui.available_width().min(140.0))
        .show_ui(ui, |ui| {
            for label in MODIFIER_LABELS {
                if ui.selectable_label(false, *label).clicked() {
                    add_cmd = Some(AppCommand::AddModifier {
                        clip_id,
                        param: param_id.clone(),
                        modifier: default_modifier_for(label),
                    });
                }
            }
        });
    if let Some(cmd) = add_cmd {
        cmds.push(cmd);
    }
}

/// Show editable parameters for a single modifier.
fn show_modifier_params(
    ui: &mut egui::Ui,
    clip_id: ClipId,
    param_id: &ParamId,
    modifier_index: usize,
    modifier: &Modifier,
    cmds: &mut Vec<AppCommand>,
    id_salt: &str,
) {
    match modifier {
        Modifier::Oscillate {
            amplitude,
            frequency,
            phase,
            waveform,
            blend,
            enabled,
        } => {
            let mut amp = *amplitude;
            let mut freq = *frequency;
            let mut ph = *phase;
            let mut wf = *waveform;
            let mut bl = *blend;
            let en = *enabled;

            let mut changed = false;
            changed |= drag_row(ui, "Amplitude", &mut amp, 0.0..=100.0, 0.01, 2, "", id_salt, "amp");
            changed |= drag_row(ui, "Frequency", &mut freq, 0.01..=100.0, 0.1, 1, " Hz", id_salt, "freq");
            changed |= drag_row(ui, "Phase", &mut ph, -360.0..=360.0, 1.0, 1, "\u{00b0}", id_salt, "phase");
            changed |= waveform_combo(ui, &mut wf, id_salt);
            changed |= blend_combo(ui, &mut bl, id_salt);

            if changed {
                cmds.push(AppCommand::UpdateModifier {
                    clip_id,
                    param: param_id.clone(),
                    modifier_index,
                    modifier: Modifier::Oscillate {
                        amplitude: amp,
                        frequency: freq,
                        phase: ph,
                        waveform: wf,
                        blend: bl,
                        enabled: en,
                    },
                });
            }
        }
        Modifier::Wiggle {
            amplitude,
            frequency,
            blend,
            enabled,
        } => {
            let mut amp = *amplitude;
            let mut freq = *frequency;
            let mut bl = *blend;
            let en = *enabled;

            let mut changed = false;
            changed |= drag_row(ui, "Amount", &mut amp, 0.0..=100.0, 0.1, 1, "", id_salt, "amt");
            changed |= drag_row(ui, "Frequency", &mut freq, 0.01..=100.0, 0.1, 1, " Hz", id_salt, "freq");
            changed |= blend_combo(ui, &mut bl, id_salt);

            if changed {
                cmds.push(AppCommand::UpdateModifier {
                    clip_id,
                    param: param_id.clone(),
                    modifier_index,
                    modifier: Modifier::Wiggle {
                        amplitude: amp,
                        frequency: freq,
                        blend: bl,
                        enabled: en,
                    },
                });
            }
        }
        Modifier::Throw {
            velocity,
            acceleration,
            blend,
            enabled,
        } => {
            let mut vel = *velocity;
            let mut acc = *acceleration;
            let mut bl = *blend;
            let en = *enabled;

            let mut changed = false;
            changed |= drag_row(ui, "Velocity", &mut vel, -1000.0..=1000.0, 0.5, 1, " /s", id_salt, "vel");
            changed |= drag_row(ui, "Accel", &mut acc, -1000.0..=1000.0, 0.5, 1, " /s\u{00b2}", id_salt, "acc");
            changed |= blend_combo(ui, &mut bl, id_salt);

            if changed {
                cmds.push(AppCommand::UpdateModifier {
                    clip_id,
                    param: param_id.clone(),
                    modifier_index,
                    modifier: Modifier::Throw {
                        velocity: vel,
                        acceleration: acc,
                        blend: bl,
                        enabled: en,
                    },
                });
            }
        }
        Modifier::Ramp {
            start_value,
            end_value,
            start_norm,
            end_norm,
            blend,
            enabled,
        } => {
            let mut sv = *start_value;
            let mut ev = *end_value;
            let mut sn = *start_norm;
            let mut en_norm = *end_norm;
            let mut bl = *blend;
            let en = *enabled;

            let mut changed = false;
            changed |= drag_row(ui, "Start", &mut sv, -1000.0..=1000.0, 0.01, 2, "", id_salt, "sv");
            changed |= drag_row(ui, "End", &mut ev, -1000.0..=1000.0, 0.01, 2, "", id_salt, "ev");
            changed |= drag_row(ui, "Start Time", &mut sn, 0.0..=1.0, 0.01, 2, "", id_salt, "sn");
            changed |= drag_row(ui, "End Time", &mut en_norm, 0.0..=1.0, 0.01, 2, "", id_salt, "en");
            changed |= blend_combo(ui, &mut bl, id_salt);

            if changed {
                cmds.push(AppCommand::UpdateModifier {
                    clip_id,
                    param: param_id.clone(),
                    modifier_index,
                    modifier: Modifier::Ramp {
                        start_value: sv,
                        end_value: ev,
                        start_norm: sn,
                        end_norm: en_norm,
                        blend: bl,
                        enabled: en,
                    },
                });
            }
        }
        Modifier::Loop {
            period_secs,
            enabled,
        } => {
            let mut period = *period_secs as f32;
            let en = *enabled;

            let changed = drag_row(ui, "Period", &mut period, 0.01..=60.0, 0.05, 2, " s", id_salt, "per");
            if changed {
                cmds.push(AppCommand::UpdateModifier {
                    clip_id,
                    param: param_id.clone(),
                    modifier_index,
                    modifier: Modifier::Loop {
                        period_secs: period as f64,
                        enabled: en,
                    },
                });
            }
        }
        Modifier::PingPong {
            period_secs,
            enabled,
        } => {
            let mut period = *period_secs as f32;
            let en = *enabled;

            let changed = drag_row(ui, "Period", &mut period, 0.01..=60.0, 0.05, 2, " s", id_salt, "per");
            if changed {
                cmds.push(AppCommand::UpdateModifier {
                    clip_id,
                    param: param_id.clone(),
                    modifier_index,
                    modifier: Modifier::PingPong {
                        period_secs: period as f64,
                        enabled: en,
                    },
                });
            }
        }
        Modifier::Clamp { min, max, enabled } => {
            let mut mn = *min;
            let mut mx = *max;
            let en = *enabled;

            let mut changed = false;
            changed |= drag_row(ui, "Min", &mut mn, -10000.0..=10000.0, 0.01, 2, "", id_salt, "min");
            changed |= drag_row(ui, "Max", &mut mx, -10000.0..=10000.0, 0.01, 2, "", id_salt, "max");

            if changed {
                cmds.push(AppCommand::UpdateModifier {
                    clip_id,
                    param: param_id.clone(),
                    modifier_index,
                    modifier: Modifier::Clamp {
                        min: mn,
                        max: mx,
                        enabled: en,
                    },
                });
            }
        }
        // Before the first key and after the last (#2277).
        Modifier::Extend { before, after, enabled } => {
            let (mut b, mut a, en) = (*before, *after, *enabled);
            let mut changed = false;
            for (label, value, salt) in [("Before", &mut b, "ext_before"), ("After", &mut a, "ext_after")] {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).size(10.0));
                    egui::ComboBox::from_id_salt((id_salt, salt)).selected_text(value.label()).show_ui(ui, |ui| {
                        for option in klipp_core::Extrapolation::ALL {
                            changed |= ui.selectable_value(value, option, option.label()).changed();
                        }
                    });
                });
            }
            if changed {
                cmds.push(AppCommand::UpdateModifier { clip_id, param: param_id.clone(), modifier_index, modifier: Modifier::Extend { before: b, after: a, enabled: en } });
            }
        }
    }
}

/// A compact parameter row with a label and DragValue.
/// Returns true if the value changed.
fn drag_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    speed: f32,
    decimals: usize,
    suffix: &str,
    id_salt: &str,
    field: &str,
) -> bool {
    let before = *value;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(label)
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 160, 170)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !suffix.is_empty() {
                ui.label(
                    egui::RichText::new(suffix)
                        .size(10.0)
                        .color(egui::Color32::from_rgb(120, 120, 130)),
                );
            }
            ui.add(
                egui::DragValue::new(value)
                    .range(range)
                    .speed(speed)
                    .max_decimals(decimals)
                    .update_while_editing(false),
            )
            .on_hover_text(format!("{id_salt}:{field}"));
        });
    });
    (*value - before).abs() > f32::EPSILON
}

/// Waveform ComboBox. Returns true if changed.
fn waveform_combo(ui: &mut egui::Ui, wf: &mut Waveform, id_salt: &str) -> bool {
    let before = *wf;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Waveform")
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 160, 170)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt(format!("{id_salt}_wf"))
                .selected_text(waveform_label(*wf))
                .width(80.0)
                .show_ui(ui, |ui| {
                    for &variant in &[
                        Waveform::Sine,
                        Waveform::Square,
                        Waveform::Triangle,
                        Waveform::Sawtooth,
                    ] {
                        ui.selectable_value(wf, variant, waveform_label(variant));
                    }
                });
        });
    });
    *wf != before
}

/// Blend mode ComboBox. Returns true if changed.
fn blend_combo(ui: &mut egui::Ui, bl: &mut ModifierBlend, id_salt: &str) -> bool {
    let before = *bl;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Blend")
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 160, 170)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt(format!("{id_salt}_bl"))
                .selected_text(blend_label(*bl))
                .width(80.0)
                .show_ui(ui, |ui| {
                    for &variant in &[
                        ModifierBlend::Add,
                        ModifierBlend::Multiply,
                        ModifierBlend::Replace,
                    ] {
                        ui.selectable_value(bl, variant, blend_label(variant));
                    }
                });
        });
    });
    *bl != before
}

fn waveform_label(wf: Waveform) -> &'static str {
    match wf {
        Waveform::Sine => "Sine",
        Waveform::Square => "Square",
        Waveform::Triangle => "Triangle",
        Waveform::Sawtooth => "Sawtooth",
    }
}

fn blend_label(bl: ModifierBlend) -> &'static str {
    match bl {
        ModifierBlend::Add => "Add",
        ModifierBlend::Multiply => "Multiply",
        ModifierBlend::Replace => "Replace",
    }
}

/// Create a default modifier instance for a given label string.
fn default_modifier_for(label: &str) -> Modifier {
    match label {
        "Oscillate" => Modifier::Oscillate {
            amplitude: 0.1,
            frequency: 2.0,
            phase: 0.0,
            waveform: Waveform::Sine,
            blend: ModifierBlend::Add,
            enabled: true,
        },
        "Wiggle" => Modifier::Wiggle {
            amplitude: 10.0,
            frequency: 5.0,
            blend: ModifierBlend::Add,
            enabled: true,
        },
        "Throw" => Modifier::Throw {
            velocity: 50.0,
            acceleration: 0.0,
            blend: ModifierBlend::Add,
            enabled: true,
        },
        "Ramp" => Modifier::Ramp {
            start_value: 0.0,
            end_value: 1.0,
            start_norm: 0.0,
            end_norm: 1.0,
            blend: ModifierBlend::Replace,
            enabled: true,
        },
        "Loop" => Modifier::Loop {
            period_secs: 1.0,
            enabled: true,
        },
        "Ping Pong" => Modifier::PingPong {
            period_secs: 2.0,
            enabled: true,
        },
        "Clamp" => Modifier::Clamp {
            min: 0.0,
            max: 1.0,
            enabled: true,
        },
        // Repeats the keyed span after the last key (#2277).
        "Outside Keys" => Modifier::Extend {
            before: klipp_core::Extrapolation::Hold,
            after: klipp_core::Extrapolation::Cycle,
            enabled: true,
        },
        _ => Modifier::Oscillate {
            amplitude: 0.1,
            frequency: 2.0,
            phase: 0.0,
            waveform: Waveform::Sine,
            blend: ModifierBlend::Add,
            enabled: true,
        },
    }
}
