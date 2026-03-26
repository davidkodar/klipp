use crate::theme::KlippTheme;
use crate::widgets::role_pill;
use crate::widgets::scrubby_param::scrubby_param_row;
use klipp_core::AppCommand;
use klipp_effects::BuiltinEffect;
use klipp_timeline::Clip;

/// Extracts transform params from a clip's builtin effects.
/// Returns identity defaults if no Transform effect exists.
pub fn get_transform(clip: &Clip) -> TransformParams {
    clip.builtin_effects
        .iter()
        .find_map(|e| match e {
            BuiltinEffect::Transform {
                position_x,
                position_y,
                scale_x,
                scale_y,
                rotation,
                anchor_x,
                anchor_y,
            } => Some(TransformParams {
                position_x: *position_x,
                position_y: *position_y,
                scale_x: *scale_x,
                scale_y: *scale_y,
                rotation: *rotation,
                anchor_x: *anchor_x,
                anchor_y: *anchor_y,
            }),
            _ => None,
        })
        .unwrap_or_default()
}

/// Extracts the index of the Transform effect in builtin_effects, if any.
pub fn find_transform_index(clip: &Clip) -> Option<usize> {
    clip.builtin_effects.iter().position(|e| matches!(e, BuiltinEffect::Transform { .. }))
}

/// Extracts opacity value from a clip's builtin effects.
/// Returns 1.0 (fully opaque) if no Opacity effect exists.
pub fn get_opacity(clip: &Clip) -> f32 {
    clip.builtin_effects
        .iter()
        .find_map(|e| match e {
            BuiltinEffect::Opacity { value } => Some(*value),
            _ => None,
        })
        .unwrap_or(1.0)
}

/// Extracts the index of the Opacity effect in builtin_effects, if any.
pub fn find_opacity_index(clip: &Clip) -> Option<usize> {
    clip.builtin_effects.iter().position(|e| matches!(e, BuiltinEffect::Opacity { .. }))
}

#[derive(Debug, Clone, Copy)]
pub struct TransformParams {
    pub position_x: f32,
    pub position_y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation: f32,
    pub anchor_x: f32,
    pub anchor_y: f32,
}

impl Default for TransformParams {
    fn default() -> Self {
        Self {
            position_x: 0.0,
            position_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
            anchor_x: 0.5,
            anchor_y: 0.5,
        }
    }
}

impl TransformParams {
    pub fn to_builtin_effect(self) -> BuiltinEffect {
        BuiltinEffect::Transform {
            position_x: self.position_x,
            position_y: self.position_y,
            scale_x: self.scale_x,
            scale_y: self.scale_y,
            rotation: self.rotation,
            anchor_x: self.anchor_x,
            anchor_y: self.anchor_y,
        }
    }
}

pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
) {
    let clip_id = clip.id;

    // ── Role section ──
    section_header(ui, theme, "Role");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
        let roles = [
            ("Video", theme.role_video),
            ("Dialogue", theme.role_dialogue),
            ("Music", theme.role_music),
            ("SFX", theme.role_sfx),
            ("B-Roll", theme.role_broll),
        ];
        // Determine active role (placeholder — no role field on Clip yet)
        let active_idx = 0usize; // Default to Video
        for (i, (label, color)) in roles.iter().enumerate() {
            if role_pill::role_pill(ui, theme, label, *color, i == active_idx) {
                // Would set clip role here
                let _ = cmds;
            }
        }
    });
    ui.add_space(14.0);

    // ── Transform section ──
    ui.horizontal(|ui| {
        section_header(ui, theme, "Transform");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let transform_preview = get_transform(clip);
            let opacity_preview = get_opacity(clip);
            let is_default = (transform_preview.position_x.abs() < f32::EPSILON)
                && (transform_preview.position_y.abs() < f32::EPSILON)
                && ((transform_preview.scale_x - 1.0).abs() < f32::EPSILON)
                && ((transform_preview.scale_y - 1.0).abs() < f32::EPSILON)
                && (transform_preview.rotation.abs() < f32::EPSILON)
                && ((opacity_preview - 1.0).abs() < f32::EPSILON);
            if !is_default {
                let (reset_rect, reset_resp) = ui.allocate_exact_size(
                    egui::vec2(18.0, 16.0),
                    egui::Sense::click(),
                );
                let tint = if reset_resp.hovered() {
                    egui::Color32::from_rgb(220, 220, 225)
                } else {
                    egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)
                };
                let icon_rect = egui::Rect::from_center_size(reset_rect.center(), egui::vec2(12.0, 12.0));
                crate::icons::Icon::ArrowCounterClockwise.image(12.0).tint(tint).paint_at(ui, icon_rect);
                if reset_resp.clicked() {
                    emit_transform(cmds, clip, clip_id, TransformParams::default());
                    emit_opacity(cmds, clip, clip_id, 1.0);
                }
                if reset_resp.hovered() {
                    reset_resp.on_hover_text("Reset all transform + opacity");
                }
            }
        });
    });

    // Read current transform from clip's builtin effects (immutable)
    let transform = get_transform(clip);
    let transform_drag_key = format!("{:?}:video_transform", clip_id);

    // Local copies for scrubby rows
    let mut pos_x = transform.position_x;
    let mut pos_y = transform.position_y;
    let mut scale_val = transform.scale_x;
    let mut rotation = transform.rotation;

    let r_px = scrubby_param_row(ui, "Position X", &mut pos_x, -2000.0..=2000.0, 0.0, 1.0, 0, " px", None);
    let r_py = scrubby_param_row(ui, "Position Y", &mut pos_y, -2000.0..=2000.0, 0.0, 1.0, 0, " px", None);
    let r_sc = scrubby_param_row(ui, "Scale", &mut scale_val, 0.0..=4.0, 1.0, 0.01, 2, "\u{00d7}", None);
    let r_ro = scrubby_param_row(ui, "Rotation", &mut rotation, -360.0..=360.0, 0.0, 0.5, 1, " \u{00b0}", None);

    // Undo coalescing for transform drags
    if r_px.drag_started || r_py.drag_started || r_sc.drag_started || r_ro.drag_started {
        if let Some(idx) = find_transform_index(clip) {
            cmds.push(AppCommand::BeginEffectDrag {
                key: transform_drag_key.clone(),
                clip_id,
                effect_index: idx,
                old_effect: transform.to_builtin_effect(),
            });
        }
    }

    // Emit command for any changed transform value
    if r_px.changed || r_py.changed || r_sc.changed || r_ro.changed {
        let mut t = transform;
        t.position_x = pos_x;
        t.position_y = pos_y;
        t.scale_x = scale_val;
        t.scale_y = scale_val; // uniform scale
        t.rotation = rotation;
        emit_transform(cmds, clip, clip_id, t);
    }

    if r_px.drag_stopped || r_py.drag_stopped || r_sc.drag_stopped || r_ro.drag_stopped {
        if let Some(idx) = find_transform_index(clip) {
            let mut t = transform;
            t.position_x = pos_x;
            t.position_y = pos_y;
            t.scale_x = scale_val;
            t.scale_y = scale_val;
            t.rotation = rotation;
            cmds.push(AppCommand::EndEffectDrag {
                key: transform_drag_key,
                clip_id,
                effect_index: idx,
                new_effect: t.to_builtin_effect(),
            });
        }
    }

    // Opacity
    let opacity_drag_key = format!("{:?}:video_opacity", clip_id);
    let mut opacity = get_opacity(clip);
    let r_op = scrubby_param_row(ui, "Opacity", &mut opacity, 0.0..=1.0, 1.0, 0.005, 2, "", None);

    if r_op.drag_started {
        if let Some(idx) = find_opacity_index(clip) {
            cmds.push(AppCommand::BeginEffectDrag {
                key: opacity_drag_key.clone(),
                clip_id,
                effect_index: idx,
                old_effect: BuiltinEffect::Opacity { value: get_opacity(clip) },
            });
        }
    }
    if r_op.changed {
        emit_opacity(cmds, clip, clip_id, opacity);
    }
    if r_op.drag_stopped {
        if let Some(idx) = find_opacity_index(clip) {
            cmds.push(AppCommand::EndEffectDrag {
                key: opacity_drag_key,
                clip_id,
                effect_index: idx,
                new_effect: BuiltinEffect::Opacity { value: opacity },
            });
        }
    }

    ui.add_space(14.0);

    // ── Speed section ──
    section_header(ui, theme, "Speed");

    // Rate is a placeholder for now -- no speed field on Clip yet
    let mut rate = 100.0f32;
    let _r_rate = scrubby_param_row(ui, "Rate", &mut rate, 10.0..=400.0, 100.0, 0.5, 0, " %", None);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Direction")
                .size(11.0)
                .color(theme.text_secondary),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new("Forward")
                    .size(11.0)
                    .monospace()
                    .color(theme.text_primary),
            );
        });
    });

    ui.add_space(16.0);

    // Color grading — link to dedicated panel
    section_header(ui, theme, "Color");
    if ui.add(egui::Button::new(
        egui::RichText::new("Open Color Panel (C)")
            .size(11.0)
            .color(theme.accent),
    ).fill(theme.bg_raised).stroke(egui::Stroke::new(0.5, theme.border)))
    .clicked()
    {
        cmds.push(AppCommand::ToggleColorPanel);
    }
}

/// Emit an UpdateBuiltinEffect or AddBuiltinEffect for a Transform.
/// If a Transform effect already exists, update it; otherwise add it.
fn emit_transform(cmds: &mut Vec<AppCommand>, clip: &Clip, clip_id: klipp_core::ClipId, params: TransformParams) {
    if let Some(index) = find_transform_index(clip) {
        cmds.push(AppCommand::UpdateBuiltinEffect {
            clip_id,
            index,
            effect: params.to_builtin_effect(),
        });
    } else {
        cmds.push(AppCommand::AddBuiltinEffect {
            clip_id,
            effect: params.to_builtin_effect(),
        });
    }
}

/// Emit an UpdateBuiltinEffect or AddBuiltinEffect for Opacity.
fn emit_opacity(cmds: &mut Vec<AppCommand>, clip: &Clip, clip_id: klipp_core::ClipId, value: f32) {
    if let Some(index) = find_opacity_index(clip) {
        cmds.push(AppCommand::UpdateBuiltinEffect {
            clip_id,
            index,
            effect: BuiltinEffect::Opacity { value },
        });
    } else {
        cmds.push(AppCommand::AddBuiltinEffect {
            clip_id,
            effect: BuiltinEffect::Opacity { value },
        });
    }
}

fn section_header(ui: &mut egui::Ui, theme: &KlippTheme, label: &str) {
    ui.label(
        egui::RichText::new(label.to_uppercase())
            .size(10.0)
            .strong()
            .color(theme.text_dim),
    );
    ui.add_space(7.0);
}

