use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::role_pill;
use crate::widgets::scrubby_param::{scrubby_param_row, KeyframeState};
use klipp_core::{AppCommand, ClipRole, Timecode, VideoParam};
use klipp_effects::BuiltinEffect;
use klipp_timeline::{Clip, TrackKind};

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

/// Compute KeyframeState for a given keyframe vec at a given clip-relative time.
fn keyframe_state(keyframes: &[klipp_core::Keyframe], clip_time_secs: f64) -> KeyframeState {
    let kf_tolerance = 0.02;
    KeyframeState {
        has_keyframe_at_playhead: keyframes.iter().any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance),
        has_any_keyframes: !keyframes.is_empty(),
        prev_keyframe_time: keyframes.iter()
            .filter(|k| k.time_secs < clip_time_secs - kf_tolerance)
            .last()
            .map(|k| k.time_secs),
        next_keyframe_time: keyframes.iter()
            .find(|k| k.time_secs > clip_time_secs + kf_tolerance)
            .map(|k| k.time_secs),
        current_interpolation: keyframes.iter()
            .find(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance)
            .map(|k| k.interpolation),
    }
}

pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    clip_ctx: Option<(&Clip, f64, bool)>,
    bin_media: Option<&klipp_media::MediaItem>,
    cmds: &mut Vec<AppCommand>,
) {
    match clip_ctx {
        Some((clip, clip_time_secs, is_timeline)) => show_clip_video(ui, theme, state, clip, cmds, clip_time_secs, is_timeline),
        None => {
            if let Some(media) = bin_media {
                show_bin_media_video(ui, theme, state, media, cmds);
            } else {
                show_sequence_properties(ui, theme, state);
            }
        }
    }
}

/// State 2: Bin/media clip selected — create a temporary default Clip and show
/// the same controls as State 3, minus Speed Ramp and amber banner.
fn show_bin_media_video(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    media: &klipp_media::MediaItem,
    cmds: &mut Vec<AppCommand>,
) {
    use std::collections::HashMap;
    // Construct a temporary default Clip from the media item
    let temp_clip = Clip {
        id: klipp_core::ClipId::new(),
        kind: klipp_timeline::ClipKind::Media { media_id: media.id },
        timeline_in: Timecode { value: 0, rate: media.duration.rate },
        timeline_out: media.duration,
        source_in: Timecode { value: 0, rate: media.duration.rate },
        source_out: media.duration,
        node_graph: klipp_nodes::NodeGraph::default(),
        effects: Vec::new(),
        builtin_effects: Vec::new(),
        linked_clip_id: None,
        group_id: None,
        magnetic_anchor: None,
        gain: 1.0,
        volume: 1.0,
        pan: 0.0,
        fade_in_secs: 0.0,
        fade_out_secs: 0.0,
        fade_in_curve: klipp_timeline::FadeCurve::default(),
        fade_out_curve: klipp_timeline::FadeCurve::default(),
        channel_config: klipp_timeline::ChannelConfig::default(),
        volume_keyframes: Vec::new(),
        pan_keyframes: Vec::new(),
        gain_keyframes: Vec::new(),
        opacity_keyframes: Vec::new(),
        transform_pos_x_keyframes: Vec::new(),
        transform_pos_y_keyframes: Vec::new(),
        transform_scale_keyframes: Vec::new(),
        transform_rotation_keyframes: Vec::new(),
        clip_markers: Vec::new(),
        effect_keyframes: HashMap::new(),
        role: klipp_core::ClipRole::default(),
        param_modifiers: HashMap::new(),
        speed: 1.0,
        audio_effects: Vec::new(),
    };
    // is_timeline = false → no Speed Ramp, no amber banner
    show_clip_video(ui, theme, state, &temp_clip, cmds, 0.0, false);
}

fn show_clip_video(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
    clip_time_secs: f64,
    is_timeline: bool,
) {
    let clip_id = clip.id;
    let clip_timeline_in_secs = clip.timeline_in.to_secs_f64();
    let clip_rate = clip.timeline_in.rate;
    let kf_tolerance = 0.02;
    let pad = 14.0;

    // ── ROLE ──
    section_card(ui, "ROLE", theme, |ui| {
        ui.add_space(8.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: 14.0, right: 14.0, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    for &role in ClipRole::all() {
                        let color = theme.clip_role_color(role);
                        let is_active = clip.role == role;
                        if role_pill::role_pill(ui, theme, role.label(), color, is_active) {
                            cmds.push(AppCommand::SetClipRole { clip_id, role });
                        }
                    }
                });
            });
        ui.add_space(8.0);
    });

    ui.add_space(8.0);

    // ── TRANSFORM ──
    // Read current transform from clip's builtin effects (immutable)
    let transform = get_transform(clip);
    let transform_drag_key = format!("{:?}:video_transform", clip_id);

    // Keyframed values at playhead
    let mut pos_x = klipp_core::keyframe::evaluate(&clip.transform_pos_x_keyframes, clip_time_secs, transform.position_x);
    let mut pos_y = klipp_core::keyframe::evaluate(&clip.transform_pos_y_keyframes, clip_time_secs, transform.position_y);
    let mut scale_val = klipp_core::keyframe::evaluate(&clip.transform_scale_keyframes, clip_time_secs, transform.scale_x);
    let mut rotation = klipp_core::keyframe::evaluate(&clip.transform_rotation_keyframes, clip_time_secs, transform.rotation);

    // Keyframe states
    let pos_x_ks = keyframe_state(&clip.transform_pos_x_keyframes, clip_time_secs);
    let pos_y_ks = keyframe_state(&clip.transform_pos_y_keyframes, clip_time_secs);
    let scale_ks = keyframe_state(&clip.transform_scale_keyframes, clip_time_secs);
    let rot_ks = keyframe_state(&clip.transform_rotation_keyframes, clip_time_secs);

    // Opacity
    let opacity_drag_key = format!("{:?}:video_opacity", clip_id);
    let mut opacity = klipp_core::keyframe::evaluate(&clip.opacity_keyframes, clip_time_secs, get_opacity(clip));
    let opacity_ks = keyframe_state(&clip.opacity_keyframes, clip_time_secs);

    // We need to collect scrubby results inside the card, then handle commands after
    // Use a struct to shuttle results out of the closure
    use crate::widgets::scrubby_param::ScrubbyParamResponse;
    struct TransformResults {
        r_px: ScrubbyParamResponse,
        r_py: ScrubbyParamResponse,
        r_sc: ScrubbyParamResponse,
        r_ro: ScrubbyParamResponse,
        r_op: ScrubbyParamResponse,
        pos_x: f32,
        pos_y: f32,
        scale_val: f32,
        rotation: f32,
        opacity: f32,
        reset_clicked: bool,
    }

    fn noop_response() -> ScrubbyParamResponse {
        ScrubbyParamResponse {
            changed: false,
            drag_started: false,
            drag_stopped: false,
            lost_focus: false,
            reset_clicked: false,
            keyframe_toggled: false,
            keyframe_nav_prev: false,
            keyframe_nav_next: false,
            keyframe_set_interpolation: None,
            keyframe_ease_preset: None,
        }
    }

    let results = {
        let mut reset_clicked = false;
        let mut r_px_out: Option<ScrubbyParamResponse> = None;
        let mut r_py_out: Option<ScrubbyParamResponse> = None;
        let mut r_sc_out: Option<ScrubbyParamResponse> = None;
        let mut r_ro_out: Option<ScrubbyParamResponse> = None;
        let mut r_op_out: Option<ScrubbyParamResponse> = None;

        // Allocate the section card with a custom header that includes the reset button
        let w = ui.available_width();
        let collapsed_key = egui::Id::new("klipp_collapsed_sections");
        let title = "TRANSFORM";
        let is_collapsed: bool = ui.ctx().data_mut(|d| {
            let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
            set.contains(title)
        });

        let id = ui.id().with(title).with("video_section");
        let mut col_state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, !is_collapsed);

        let header_h = 30.0;
        let (header_rect, header_resp) = ui.allocate_exact_size(egui::vec2(w, header_h), egui::Sense::click());
        let is_open = col_state.is_open();
        let rounding = if is_open {
            egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 }
        } else {
            egui::Rounding::same(6.0)
        };
        ui.painter().rect_filled(header_rect, rounding, theme.bg_deep);

        let accent_rounding = if is_open {
            egui::Rounding { nw: 6.0, ne: 0.0, sw: 0.0, se: 0.0 }
        } else {
            egui::Rounding { nw: 6.0, ne: 0.0, sw: 6.0, se: 0.0 }
        };
        ui.painter().rect_filled(
            egui::Rect::from_min_size(header_rect.min, egui::vec2(3.0, header_h)),
            accent_rounding, theme.border,
        );

        ui.painter().text(
            egui::pos2(header_rect.min.x + 18.0, header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            title, egui::FontId::proportional(11.0), theme.text_dim,
        );

        // Reset button in header (right side, before caret)
        let opacity_preview = get_opacity(clip);
        let is_default = (transform.position_x.abs() < f32::EPSILON)
            && (transform.position_y.abs() < f32::EPSILON)
            && ((transform.scale_x - 1.0).abs() < f32::EPSILON)
            && ((transform.scale_y - 1.0).abs() < f32::EPSILON)
            && (transform.rotation.abs() < f32::EPSILON)
            && ((opacity_preview - 1.0).abs() < f32::EPSILON);
        if !is_default {
            let reset_rect = egui::Rect::from_center_size(
                egui::pos2(header_rect.max.x - 40.0, header_rect.center().y),
                egui::vec2(18.0, 16.0),
            );
            let reset_resp = ui.interact(reset_rect, ui.id().with("transform_reset"), egui::Sense::click());
            let tint = if reset_resp.hovered() {
                egui::Color32::from_rgb(220, 220, 225)
            } else {
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)
            };
            let icon_rect = egui::Rect::from_center_size(reset_rect.center(), egui::vec2(12.0, 12.0));
            crate::icons::Icon::ArrowCounterClockwise.image(12.0).tint(tint).paint_at(ui, icon_rect);
            if reset_resp.clicked() {
                reset_clicked = true;
            }
            if reset_resp.hovered() {
                reset_resp.on_hover_text("Reset all transform + opacity");
            }
        }

        let icon = if is_open { Icon::CaretDown } else { Icon::CaretRight };
        let icon_size = 12.0;
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(header_rect.max.x - 20.0, header_rect.center().y),
            egui::vec2(icon_size, icon_size),
        );
        icon.image(icon_size).tint(theme.text_dim).paint_at(ui, icon_rect);

        if header_resp.clicked() {
            col_state.toggle(ui);
            let now_open = col_state.is_open();
            ui.ctx().data_mut(|d| {
                let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
                if now_open { set.remove(title); } else { set.insert(title.to_string()); }
            });
        }

        if col_state.is_open() {
            let body_start = ui.cursor().min;
            let frame = egui::Frame::default()
                .fill(theme.bg_panel)
                .inner_margin(egui::Margin { left: pad, right: pad, top: 6.0, bottom: 8.0 })
                .stroke(egui::Stroke::NONE)
                .shadow(egui::Shadow::NONE);
            frame.show(ui, |ui| {
                r_px_out = Some(scrubby_param_row(ui, "Position X", &mut pos_x, -2000.0..=2000.0, 0.0, 1.0, 0, " px", Some(pos_x_ks)));
                r_py_out = Some(scrubby_param_row(ui, "Position Y", &mut pos_y, -2000.0..=2000.0, 0.0, 1.0, 0, " px", Some(pos_y_ks)));
                r_sc_out = Some(scrubby_param_row(ui, "Scale", &mut scale_val, 0.0..=4.0, 1.0, 0.01, 2, "\u{00d7}", Some(scale_ks)));
                r_ro_out = Some(scrubby_param_row(ui, "Rotation", &mut rotation, -360.0..=360.0, 0.0, 0.5, 1, " \u{00b0}", Some(rot_ks)));

                // Separator before opacity
                let sep_w = ui.available_width();
                let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(sep_w, 1.0), egui::Sense::hover());
                ui.painter().hline(sep_rect.x_range(), sep_rect.center().y, egui::Stroke::new(0.5, theme.border));
                ui.add_space(2.0);

                r_op_out = Some(scrubby_param_row(ui, "Opacity", &mut opacity, 0.0..=1.0, 1.0, 0.005, 2, "", Some(opacity_ks)));
            });

            let body_end_y = ui.cursor().min.y;
            ui.painter().rect_filled(
                egui::Rect::from_min_size(body_start, egui::vec2(3.0, body_end_y - body_start.y)),
                0.0, theme.border,
            );
            let full_rect = egui::Rect::from_min_max(header_rect.min, egui::pos2(header_rect.max.x, body_end_y));
            ui.painter().rect_stroke(full_rect, 6.0, egui::Stroke::new(0.5, theme.border));
        } else {
            ui.painter().rect_stroke(header_rect, 6.0, egui::Stroke::new(0.5, theme.border));
        }

        col_state.store(ui.ctx());

        // Default no-op results if section was collapsed
        TransformResults {
            r_px: r_px_out.unwrap_or_else(noop_response),
            r_py: r_py_out.unwrap_or_else(noop_response),
            r_sc: r_sc_out.unwrap_or_else(noop_response),
            r_ro: r_ro_out.unwrap_or_else(noop_response),
            r_op: r_op_out.unwrap_or_else(noop_response),
            pos_x,
            pos_y,
            scale_val,
            rotation,
            opacity,
            reset_clicked,
        }
    };

    let TransformResults { r_px, r_py, r_sc, r_ro, r_op, pos_x, pos_y, scale_val, rotation, opacity, reset_clicked } = results;

    // Reset button handling
    if reset_clicked {
        emit_transform(cmds, clip, clip_id, TransformParams::default());
        emit_opacity(cmds, clip, clip_id, 1.0);
    }

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
        if r_px.drag_started { cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::PositionX)); }
        else if r_py.drag_started { cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::PositionY)); }
        else if r_sc.drag_started { cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::Scale)); }
        else if r_ro.drag_started { cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::Rotation)); }
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

        if r_px.changed && !clip.transform_pos_x_keyframes.is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::PositionX, time_secs: clip_time_secs, value: pos_x });
        }
        if r_py.changed && !clip.transform_pos_y_keyframes.is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::PositionY, time_secs: clip_time_secs, value: pos_y });
        }
        if r_sc.changed && !clip.transform_scale_keyframes.is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::Scale, time_secs: clip_time_secs, value: scale_val });
        }
        if r_ro.changed && !clip.transform_rotation_keyframes.is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::Rotation, time_secs: clip_time_secs, value: rotation });
        }
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

    // ── Keyframe toggle/nav for Position X ──
    if let Some(interp) = r_px.keyframe_set_interpolation {
        if let Some(ki) = clip.transform_pos_x_keyframes.iter().position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::PositionX, keyframe_index: ki, interpolation: interp });
        }
    }
    if let Some(preset) = r_px.keyframe_ease_preset {
        if let Some(ki) = clip.transform_pos_x_keyframes.iter().position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::PositionX, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
            let kfs = &clip.transform_pos_x_keyframes;
            for idx in [ki.saturating_sub(1), ki] {
                if idx >= kfs.len() { continue; }
                let (t_in, t_out) = preset.compute_tangents(kfs, idx);
                cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: VideoParam::PositionX, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out });
            }
        }
    }
    if r_px.keyframe_toggled {
        if clip.transform_pos_x_keyframes.iter().any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param: VideoParam::PositionX, time_secs: clip_time_secs });
        } else {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::PositionX, time_secs: clip_time_secs, value: pos_x });
        }
        cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::PositionX));
    }
    if r_px.keyframe_nav_prev {
        if let Some(t) = pos_x_ks.prev_keyframe_time {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }
    if r_px.keyframe_nav_next {
        if let Some(t) = pos_x_ks.next_keyframe_time {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }

    // ── Keyframe toggle/nav for Position Y ──
    if let Some(interp) = r_py.keyframe_set_interpolation {
        if let Some(ki) = clip.transform_pos_y_keyframes.iter().position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::PositionY, keyframe_index: ki, interpolation: interp });
        }
    }
    if let Some(preset) = r_py.keyframe_ease_preset {
        if let Some(ki) = clip.transform_pos_y_keyframes.iter().position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::PositionY, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
            let kfs = &clip.transform_pos_y_keyframes;
            for idx in [ki.saturating_sub(1), ki] {
                if idx >= kfs.len() { continue; }
                let (t_in, t_out) = preset.compute_tangents(kfs, idx);
                cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: VideoParam::PositionY, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out });
            }
        }
    }
    if r_py.keyframe_toggled {
        if clip.transform_pos_y_keyframes.iter().any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param: VideoParam::PositionY, time_secs: clip_time_secs });
        } else {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::PositionY, time_secs: clip_time_secs, value: pos_y });
        }
        cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::PositionY));
    }
    if r_py.keyframe_nav_prev {
        if let Some(t) = pos_y_ks.prev_keyframe_time {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }
    if r_py.keyframe_nav_next {
        if let Some(t) = pos_y_ks.next_keyframe_time {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }

    // ── Keyframe toggle/nav for Scale ──
    if let Some(interp) = r_sc.keyframe_set_interpolation {
        if let Some(ki) = clip.transform_scale_keyframes.iter().position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Scale, keyframe_index: ki, interpolation: interp });
        }
    }
    if let Some(preset) = r_sc.keyframe_ease_preset {
        if let Some(ki) = clip.transform_scale_keyframes.iter().position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Scale, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
            let kfs = &clip.transform_scale_keyframes;
            for idx in [ki.saturating_sub(1), ki] {
                if idx >= kfs.len() { continue; }
                let (t_in, t_out) = preset.compute_tangents(kfs, idx);
                cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: VideoParam::Scale, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out });
            }
        }
    }
    if r_sc.keyframe_toggled {
        if clip.transform_scale_keyframes.iter().any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param: VideoParam::Scale, time_secs: clip_time_secs });
        } else {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::Scale, time_secs: clip_time_secs, value: scale_val });
        }
        cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::Scale));
    }
    if r_sc.keyframe_nav_prev {
        if let Some(t) = scale_ks.prev_keyframe_time {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }
    if r_sc.keyframe_nav_next {
        if let Some(t) = scale_ks.next_keyframe_time {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }

    // ── Keyframe toggle/nav for Rotation ──
    if let Some(interp) = r_ro.keyframe_set_interpolation {
        if let Some(ki) = clip.transform_rotation_keyframes.iter().position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Rotation, keyframe_index: ki, interpolation: interp });
        }
    }
    if let Some(preset) = r_ro.keyframe_ease_preset {
        if let Some(ki) = clip.transform_rotation_keyframes.iter().position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Rotation, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
            let kfs = &clip.transform_rotation_keyframes;
            for idx in [ki.saturating_sub(1), ki] {
                if idx >= kfs.len() { continue; }
                let (t_in, t_out) = preset.compute_tangents(kfs, idx);
                cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: VideoParam::Rotation, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out });
            }
        }
    }
    if r_ro.keyframe_toggled {
        if clip.transform_rotation_keyframes.iter().any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param: VideoParam::Rotation, time_secs: clip_time_secs });
        } else {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::Rotation, time_secs: clip_time_secs, value: rotation });
        }
        cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::Rotation));
    }
    if r_ro.keyframe_nav_prev {
        if let Some(t) = rot_ks.prev_keyframe_time {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }
    if r_ro.keyframe_nav_next {
        if let Some(t) = rot_ks.next_keyframe_time {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }

    // ── Opacity keyframe/drag handling ──
    if r_op.drag_started {
        if let Some(idx) = find_opacity_index(clip) {
            cmds.push(AppCommand::BeginEffectDrag {
                key: opacity_drag_key.clone(),
                clip_id,
                effect_index: idx,
                old_effect: BuiltinEffect::Opacity { value: get_opacity(clip) },
            });
        }
        cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::Opacity));
    }
    if r_op.changed {
        emit_opacity(cmds, clip, clip_id, opacity);
        if !clip.opacity_keyframes.is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::Opacity, time_secs: clip_time_secs, value: opacity });
        }
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

    if let Some(interp) = r_op.keyframe_set_interpolation {
        if let Some(ki) = clip.opacity_keyframes.iter().position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Opacity, keyframe_index: ki, interpolation: interp });
        }
    }
    if let Some(preset) = r_op.keyframe_ease_preset {
        if let Some(ki) = clip.opacity_keyframes.iter().position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Opacity, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
            let kfs = &clip.opacity_keyframes;
            for idx in [ki.saturating_sub(1), ki] {
                if idx >= kfs.len() { continue; }
                let (t_in, t_out) = preset.compute_tangents(kfs, idx);
                cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: VideoParam::Opacity, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out });
            }
        }
    }
    if r_op.keyframe_toggled {
        if clip.opacity_keyframes.iter().any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance) {
            cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param: VideoParam::Opacity, time_secs: clip_time_secs });
        } else {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::Opacity, time_secs: clip_time_secs, value: opacity });
        }
        cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::Opacity));
    }
    if r_op.keyframe_nav_prev {
        if let Some(t) = opacity_ks.prev_keyframe_time {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }
    if r_op.keyframe_nav_next {
        if let Some(t) = opacity_ks.next_keyframe_time {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }

    ui.add_space(8.0);

    // ── KEN BURNS ──
    section_card(ui, "KEN BURNS", theme, |ui| {
        let has_transform_kfs = !clip.transform_scale_keyframes.is_empty()
            || !clip.transform_pos_x_keyframes.is_empty()
            || !clip.transform_pos_y_keyframes.is_empty();

        egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(pad, 8.0))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                    let btn_text = |label: &str| egui::RichText::new(label).size(10.5);
                    if ui.add(egui::Button::new(btn_text("Zoom In"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5, theme.border))
                        .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Scale 1.0 to 1.3, centered").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.0, end_scale: 1.3,
                            start_pos_x: 0.0, start_pos_y: 0.0, end_pos_x: 0.0, end_pos_y: 0.0,
                        });
                    }
                    if ui.add(egui::Button::new(btn_text("Zoom Out"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5, theme.border))
                        .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Scale 1.3 to 1.0, centered").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.3, end_scale: 1.0,
                            start_pos_x: 0.0, start_pos_y: 0.0, end_pos_x: 0.0, end_pos_y: 0.0,
                        });
                    }
                    if ui.add(egui::Button::new(btn_text("Pan L>R"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5, theme.border))
                        .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Pan left to right, 1.2\u{00d7} zoom").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.2, end_scale: 1.2,
                            start_pos_x: -100.0, start_pos_y: 0.0, end_pos_x: 100.0, end_pos_y: 0.0,
                        });
                    }
                    if ui.add(egui::Button::new(btn_text("Pan R>L"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5, theme.border))
                        .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Pan right to left, 1.2\u{00d7} zoom").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.2, end_scale: 1.2,
                            start_pos_x: 100.0, start_pos_y: 0.0, end_pos_x: -100.0, end_pos_y: 0.0,
                        });
                    }
                    if has_transform_kfs && ui.add(egui::Button::new(btn_text("Remove"))
                        .fill(egui::Color32::from_rgb(60, 30, 30)).stroke(egui::Stroke::new(0.5, theme.border))
                        .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Clear all transform keyframes").clicked() {
                        cmds.push(AppCommand::ClearTransformKeyframes { clip_id });
                    }
                });
            });
    });

    ui.add_space(8.0);

    // ── BEHAVIORS ──
    section_card(ui, "BEHAVIORS", theme, |ui| {
        ui.add_space(4.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: 14.0, right: 14.0, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE)
            .show(ui, |ui| {
                use klipp_core::ParamId;
                let video_params = [
                    ParamId::Opacity,
                    ParamId::PositionX,
                    ParamId::PositionY,
                    ParamId::Scale,
                    ParamId::Rotation,
                ];
                super::modifier_ui::show_behaviors_content(ui, theme, clip, &video_params, cmds);
            });
        ui.add_space(4.0);
    });

    ui.add_space(8.0);

    // ── SPEED ──
    section_card(ui, "SPEED", theme, |ui| {
        egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(pad, 6.0))
            .show(ui, |ui| {
                let mut rate = (clip.speed * 100.0) as f32;
                let r_rate = scrubby_param_row(ui, "Rate", &mut rate, 10.0..=1600.0, 100.0, 0.5, 0, " %", None);
                if r_rate.changed {
                    let new_speed = (rate as f64 / 100.0).clamp(0.1, 16.0);
                    cmds.push(AppCommand::SetClipSpeed { clip_id, speed: new_speed });
                }

                ui.add_space(4.0);

                // Direction row
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

                // Separator
                ui.add_space(4.0);
                let sep_w = ui.available_width();
                let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(sep_w, 1.0), egui::Sense::hover());
                ui.painter().hline(sep_rect.x_range(), sep_rect.center().y, egui::Stroke::new(0.5, theme.border));
                ui.add_space(4.0);

                // Speed preset buttons
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
                    let btn_text = |label: &str| egui::RichText::new(label).size(10.5);
                    for (label, speed_val) in [("25%", 0.25), ("50%", 0.5), ("100%", 1.0), ("200%", 2.0), ("400%", 4.0)] {
                        let is_active = (clip.speed - speed_val).abs() < 0.001;
                        let fill = if is_active { theme.accent.linear_multiply(0.25) } else { theme.bg_raised };
                        let stroke = if is_active {
                            egui::Stroke::new(1.0, theme.accent)
                        } else {
                            egui::Stroke::new(0.5, theme.border)
                        };
                        if ui.add(egui::Button::new(btn_text(label))
                            .fill(fill).stroke(stroke)
                            .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                        ).clicked() {
                            cmds.push(AppCommand::SetClipSpeed { clip_id, speed: speed_val });
                        }
                    }
                });
            });
    });

    ui.add_space(8.0);

    // ── COLOR ──
    section_card(ui, "COLOR", theme, |ui| {
        ui.add_space(6.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: 14.0, right: 14.0, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE)
            .show(ui, |ui| {
                // Check if any color grading is applied
                let has_lut = state.global_lut_path.is_some();
                let node_count = clip.node_graph.nodes.len();
                // Default graph has 12 nodes; extra nodes = user added grading
                let has_custom_nodes = node_count > 12;
                let grade_applied = has_lut || has_custom_nodes;
                let grade_count = if has_lut { 1 } else { 0 } + if has_custom_nodes { node_count - 12 } else { 0 };

                if grade_applied {
                    // Grade applied state
                    ui.horizontal(|ui| {
                        // Orange dot
                        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot_rect.center(), 4.0, theme.accent);
                        ui.label(
                            egui::RichText::new(format!("{grade_count} grade applied"))
                                .size(12.0)
                                .color(theme.accent),
                        );
                    });
                } else {
                    // No grade applied
                    ui.label(
                        egui::RichText::new("No grade applied")
                            .size(12.0)
                            .italics()
                            .color(theme.text_dim),
                    );
                }

                ui.add_space(6.0);

                // Open Color Panel button with C shortcut badge
                ui.horizontal(|ui| {
                    let btn_w = ui.available_width() - 30.0;
                    if ui.add_sized(
                        egui::vec2(btn_w, 28.0),
                        egui::Button::new(
                            egui::RichText::new("Open Color Panel")
                                .size(11.0)
                                .color(theme.text_primary),
                        )
                        .fill(theme.bg_raised)
                        .stroke(egui::Stroke::new(0.5, theme.border))
                        .rounding(6.0),
                    ).clicked() {
                        cmds.push(AppCommand::ToggleColorPanel);
                    }

                    // C shortcut badge
                    let (badge_rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
                    ui.painter().rect(badge_rect, 4.0, theme.bg_deep, egui::Stroke::new(0.5, theme.border));
                    ui.painter().text(
                        badge_rect.center(), egui::Align2::CENTER_CENTER,
                        "C", egui::FontId::monospace(10.0), theme.text_dim,
                    );
                });
            });
        ui.add_space(8.0);
    });

    // ── SPEED RAMP (timeline only) ──
    if is_timeline {
        ui.add_space(8.0);
        // SPEED RAMP section (timeline clips only)
        {
            let w = ui.available_width();
            let collapsed_key = egui::Id::new("klipp_collapsed_sections");
            let title = "SPEED RAMP";
            let is_collapsed: bool = ui.ctx().data_mut(|d| {
                let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
                set.contains(title)
            });

            let id = ui.id().with(title).with("video_section");
            let mut col_state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, !is_collapsed);

            let header_h = 30.0;
            let (header_rect, header_resp) = ui.allocate_exact_size(egui::vec2(w, header_h), egui::Sense::click());
            let is_open = col_state.is_open();
            let rounding = if is_open {
                egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 }
            } else {
                egui::Rounding::same(6.0)
            };
            ui.painter().rect_filled(header_rect, rounding, theme.bg_deep);
            // Left accent
            ui.painter().rect_filled(
                egui::Rect::from_min_size(header_rect.min, egui::vec2(3.0, header_h)),
                if is_open { egui::Rounding { nw: 6.0, ne: 0.0, sw: 0.0, se: 0.0 } } else { egui::Rounding { nw: 6.0, ne: 0.0, sw: 6.0, se: 0.0 } },
                theme.border,
            );
            // Title
            ui.painter().text(
                egui::pos2(header_rect.min.x + 18.0, header_rect.center().y),
                egui::Align2::LEFT_CENTER,
                title, egui::FontId::proportional(11.0), theme.text_dim,
            );
            // Collapse icon
            let icon = if is_open { Icon::CaretDown } else { Icon::CaretRight };
            icon.image(12.0).tint(theme.text_dim).paint_at(ui,
                egui::Rect::from_center_size(
                    egui::pos2(header_rect.max.x - 20.0, header_rect.center().y),
                    egui::vec2(12.0, 12.0),
                ),
            );

            if header_resp.clicked() {
                col_state.toggle(ui);
                let now_open = col_state.is_open();
                ui.ctx().data_mut(|d| {
                    let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
                    if now_open { set.remove(title); } else { set.insert(title.to_string()); }
                });
            }

            if col_state.is_open() {
                let body_start = ui.cursor().min;
                let frame = egui::Frame::default()
                    .fill(theme.bg_panel)
                    .inner_margin(egui::Margin { left: 0.0, right: 0.0, top: 2.0, bottom: 0.0 })
                    .stroke(egui::Stroke::NONE)
                    .shadow(egui::Shadow::NONE);
                frame.show(ui, |ui| {
                    let w = ui.available_width();
                    let pad = 14.0;
                    let row_h = 30.0;

                    // In speed
                    info_row(ui, theme, pad, row_h, w, "In speed", |ui, rx, cy| {
                        // Value box
                        let input_w = 60.0;
                        let input_rect = egui::Rect::from_min_size(
                            egui::pos2(rx - 20.0 - input_w, cy - 13.0), egui::vec2(input_w, 26.0),
                        );
                        ui.painter().rect(input_rect, 4.0, theme.bg_raised, egui::Stroke::new(1.0, theme.border));
                        ui.painter().text(
                            input_rect.center(), egui::Align2::CENTER_CENTER,
                            "100 %", egui::FontId::monospace(12.0), theme.text_primary,
                        );
                        // Keyframe diamond
                        let diamond_x = rx - 5.0;
                        draw_keyframe_diamond(ui, egui::pos2(diamond_x, cy),
                            egui::Color32::from_rgba_premultiplied(120, 120, 130, 90), false);
                    });

                    // Out speed
                    info_row(ui, theme, pad, row_h, w, "Out speed", |ui, rx, cy| {
                        let input_w = 60.0;
                        let input_rect = egui::Rect::from_min_size(
                            egui::pos2(rx - 20.0 - input_w, cy - 13.0), egui::vec2(input_w, 26.0),
                        );
                        ui.painter().rect(input_rect, 4.0, theme.bg_raised, egui::Stroke::new(1.0, theme.border));
                        ui.painter().text(
                            input_rect.center(), egui::Align2::CENTER_CENTER,
                            "100 %", egui::FontId::monospace(12.0), theme.text_primary,
                        );
                        let diamond_x = rx - 5.0;
                        draw_keyframe_diamond(ui, egui::pos2(diamond_x, cy),
                            egui::Color32::from_rgba_premultiplied(120, 120, 130, 90), false);
                    });

                    // Ease dropdown
                    {
                        let (row_rect, _) = ui.allocate_exact_size(egui::vec2(w, row_h + 4.0), egui::Sense::hover());
                        ui.painter().hline(row_rect.min.x + pad..=row_rect.max.x - pad, row_rect.max.y, egui::Stroke::new(0.5, theme.border));
                        ui.painter().text(
                            egui::pos2(row_rect.min.x + pad, row_rect.center().y),
                            egui::Align2::LEFT_CENTER, "Ease",
                            egui::FontId::proportional(13.0), theme.text_secondary,
                        );
                        let combo_w = 110.0;
                        let combo_rect = egui::Rect::from_min_size(
                            egui::pos2(row_rect.max.x - pad - combo_w, row_rect.center().y - 13.0),
                            egui::vec2(combo_w, 26.0),
                        );
                        ui.painter().rect(combo_rect, 4.0, theme.bg_raised, egui::Stroke::new(1.0, theme.border));
                        ui.painter().text(
                            egui::pos2(combo_rect.min.x + 10.0, combo_rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            "Linear", egui::FontId::proportional(12.0), theme.text_primary,
                        );
                        ui.painter().text(
                            egui::pos2(combo_rect.max.x - 10.0, combo_rect.center().y),
                            egui::Align2::RIGHT_CENTER,
                            "\u{25BE}", egui::FontId::proportional(10.0), theme.text_dim,
                        );
                    }
                    ui.add_space(4.0);
                });

                let body_end_y = ui.cursor().min.y;
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(body_start, egui::vec2(3.0, body_end_y - body_start.y)),
                    0.0, theme.border,
                );
                let full_rect = egui::Rect::from_min_max(header_rect.min, egui::pos2(header_rect.max.x, body_end_y));
                ui.painter().rect_stroke(full_rect, 6.0, egui::Stroke::new(0.5, theme.border));
            } else {
                ui.painter().rect_stroke(header_rect, 6.0, egui::Stroke::new(0.5, theme.border));
            }
            col_state.store(ui.ctx());
        }
    }

    ui.add_space(8.0);
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

// ══════════════════════════════════════════════════
// No clip selected — sequence/project video properties
// ══════════════════════════════════════════════════

fn show_sequence_properties(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState) {
    let seq = state.sequence();
    let pad = 14.0;
    let row_h = 30.0;

    // ── SEQUENCE ──
    section_card(ui, "SEQUENCE", theme, |ui| {
        let w = ui.available_width();

        info_row(ui, theme, pad, row_h, w, "Resolution", |ui, rx, cy| {
            let text = format!("{} \u{00D7} {}", seq.resolution.width, seq.resolution.height);
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                text, egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row(ui, theme, pad, row_h, w, "Frame rate", |ui, rx, cy| {
            let fps = seq.fps.num as f64 / seq.fps.den.max(1) as f64;
            let text = if (fps - fps.round()).abs() < 0.01 {
                format!("{}  fps", fps.round() as u32)
            } else {
                format!("{:.3}  fps", fps)
            };
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                text, egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row(ui, theme, pad, row_h, w, "Colour space", |ui, rx, cy| {
            let badge_text = "SDR";
            let badge_color = egui::Color32::from_rgb(60, 120, 200);
            let sub_text = "BT.709";
            let dim = theme.text_dim;

            let sub_galley = ui.painter().layout_no_wrap(
                sub_text.to_string(), egui::FontId::proportional(12.0), dim,
            );
            let sub_w = sub_galley.size().x;
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                sub_text, egui::FontId::proportional(12.0), dim,
            );
            let badge_galley = ui.painter().layout_no_wrap(
                badge_text.to_string(), egui::FontId::proportional(11.0), egui::Color32::WHITE,
            );
            let bw = badge_galley.size().x + 10.0;
            let bh = 18.0;
            let badge_rect = egui::Rect::from_min_size(
                egui::pos2(rx - sub_w - 8.0 - bw, cy - bh / 2.0),
                egui::vec2(bw, bh),
            );
            ui.painter().rect_filled(badge_rect, 3.0, badge_color);
            ui.painter().text(
                badge_rect.center(), egui::Align2::CENTER_CENTER,
                badge_text, egui::FontId::proportional(11.0), egui::Color32::WHITE,
            );
        });

        info_row(ui, theme, pad, row_h, w, "Aspect ratio", |ui, rx, cy| {
            let (aw, ah) = {
                let g = gcd(seq.resolution.width, seq.resolution.height);
                if g > 0 { (seq.resolution.width / g, seq.resolution.height / g) } else { (0, 0) }
            };
            let text = format!("{}:{}", aw, ah);
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                text, egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row(ui, theme, pad, row_h, w, "Duration", |ui, rx, cy| {
            let dur = state.sequence_duration();
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                dur.to_timecode_string(), egui::FontId::monospace(13.0), theme.accent,
            );
        });
    });

    ui.add_space(8.0);

    // ── TIMECODE ──
    section_card(ui, "TIMECODE", theme, |ui| {
        let w = ui.available_width();
        let fps = (seq.fps.num as f64 / seq.fps.den.max(1) as f64).max(1.0);

        // "PLAYHEAD POSITION" label
        let (label_rect, _) = ui.allocate_exact_size(egui::vec2(w, 18.0), egui::Sense::hover());
        ui.painter().text(
            label_rect.center(), egui::Align2::CENTER_CENTER,
            "PLAYHEAD POSITION", egui::FontId::proportional(9.0), theme.text_dim,
        );

        // Large timecode display
        let total_secs = state.playhead_smooth_secs;
        let total_frames_f = total_secs * fps;
        let total_frames = total_frames_f.floor() as i64;
        let fps_i = fps.round() as i64;
        let ff = if fps_i > 0 { total_frames % fps_i } else { 0 };
        let secs_int = if fps_i > 0 { total_frames / fps_i } else { total_secs as i64 };
        let ss = secs_int % 60;
        let mm = (secs_int / 60) % 60;
        let hh = secs_int / 3600;

        let (tc_rect, _) = ui.allocate_exact_size(egui::vec2(w, 36.0), egui::Sense::hover());
        let tc_parts = [
            format!("{:02}", hh), ":".to_string(),
            format!("{:02}", mm), ":".to_string(),
            format!("{:02}", ss), ":".to_string(),
            format!("{:02}", ff),
        ];
        // Measure total width
        let large_font = egui::FontId::monospace(24.0);
        let colon_font = egui::FontId::monospace(24.0);
        let mut total_w = 0.0f32;
        let widths: Vec<f32> = tc_parts.iter().enumerate().map(|(i, part)| {
            let font = if i % 2 == 1 { &colon_font } else { &large_font };
            let galley = ui.painter().layout_no_wrap(part.clone(), font.clone(), theme.text_primary);
            let w = galley.size().x;
            total_w += w;
            w
        }).collect();

        let mut cx = tc_rect.center().x - total_w / 2.0;
        let cy = tc_rect.center().y;
        for (i, part) in tc_parts.iter().enumerate() {
            let color = if i % 2 == 1 { theme.text_dim } else { theme.text_primary };
            let font = if i % 2 == 1 { &colon_font } else { &large_font };
            ui.painter().text(
                egui::pos2(cx, cy), egui::Align2::LEFT_CENTER,
                part, font.clone(), color,
            );
            cx += widths[i];
        }

        // In / Out points
        info_row(ui, theme, pad, row_h, w, "In point", |ui, rx, cy| {
            let tc = seq.in_point.map(|t| t.to_timecode_string()).unwrap_or_else(|| "00:00:00:00".to_string());
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                tc, egui::FontId::monospace(13.0), theme.text_secondary,
            );
        });

        info_row(ui, theme, pad, row_h, w, "Out point", |ui, rx, cy| {
            let tc = seq.out_point.map(|t| t.to_timecode_string()).unwrap_or_else(|| state.sequence_duration().to_timecode_string());
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                tc, egui::FontId::monospace(13.0), theme.text_secondary,
            );
        });
    });

    ui.add_space(8.0);

    // ── TIMELINE STATS ──
    section_card(ui, "TIMELINE STATS", theme, |ui| {
        let w = ui.available_width();
        let video_tracks = seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).count();
        let audio_tracks = seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio).count();
        let total_clips: usize = seq.tracks.iter().map(|t| t.clips.len()).sum();
        let missing_media = state.offline_media_ids.len();

        let card_w = (w - pad * 2.0 - 8.0) / 2.0;
        let card_h = 52.0;

        // Row 1
        let (row1_rect, _) = ui.allocate_exact_size(egui::vec2(w, card_h + 4.0), egui::Sense::hover());
        let x0 = row1_rect.min.x + pad;
        let y0 = row1_rect.min.y;
        stat_card(ui, theme, egui::Rect::from_min_size(egui::pos2(x0, y0), egui::vec2(card_w, card_h)),
            "VIDEO TRACKS", &video_tracks.to_string(), theme.text_primary);
        stat_card(ui, theme, egui::Rect::from_min_size(egui::pos2(x0 + card_w + 8.0, y0), egui::vec2(card_w, card_h)),
            "AUDIO TRACKS", &audio_tracks.to_string(), theme.text_primary);

        // Row 2
        let (row2_rect, _) = ui.allocate_exact_size(egui::vec2(w, card_h + 4.0), egui::Sense::hover());
        let y1 = row2_rect.min.y;
        stat_card(ui, theme, egui::Rect::from_min_size(egui::pos2(x0, y1), egui::vec2(card_w, card_h)),
            "TOTAL CLIPS", &total_clips.to_string(), theme.text_primary);
        let missing_color = if missing_media == 0 {
            egui::Color32::from_rgb(100, 200, 120)
        } else {
            egui::Color32::from_rgb(220, 80, 80)
        };
        stat_card(ui, theme, egui::Rect::from_min_size(egui::pos2(x0 + card_w + 8.0, y1), egui::vec2(card_w, card_h)),
            "MISSING MEDIA", &missing_media.to_string(), missing_color);
    });

    ui.add_space(8.0);

    // ── OUTPUT ──
    section_card(ui, "OUTPUT", theme, |ui| {
        let w = ui.available_width();

        info_row(ui, theme, pad, row_h, w, "Format", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                "H.264 / AAC", egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row(ui, theme, pad, row_h, w, "Resolution", |ui, rx, cy| {
            let text = format!("{} \u{00D7} {}", seq.resolution.width, seq.resolution.height);
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                text, egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row(ui, theme, pad, row_h, w, "Frame rate", |ui, rx, cy| {
            let fps = seq.fps.num as f64 / seq.fps.den.max(1) as f64;
            let text = if (fps - fps.round()).abs() < 0.01 {
                format!("{}  fps", fps.round() as u32)
            } else {
                format!("{:.3}  fps", fps)
            };
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                text, egui::FontId::proportional(13.0), theme.text_primary,
            );
        });
    });

    ui.add_space(8.0);

    // ── PLAYBACK ──
    section_card(ui, "PLAYBACK", theme, |ui| {
        let w = ui.available_width();

        info_row(ui, theme, pad, row_h, w, "Proxy mode", |ui, rx, cy| {
            let text = if state.use_proxies { "On" } else { "Off" };
            let color = if state.use_proxies { egui::Color32::from_rgb(100, 200, 120) } else { theme.text_primary };
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                text, egui::FontId::proportional(13.0), color,
            );
        });

        info_row(ui, theme, pad, row_h, w, "Quality", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                "Full", egui::FontId::proportional(13.0), theme.text_primary,
            );
        });
    });

    ui.add_space(8.0);

    // ── CANVAS ──
    section_card(ui, "CANVAS", theme, |ui| {
        let w = ui.available_width();

        info_row(ui, theme, pad, row_h, w, "Safe zones", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                "Off", egui::FontId::proportional(13.0), theme.text_secondary,
            );
        });

        info_row(ui, theme, pad, row_h, w, "Grid overlay", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                "Off", egui::FontId::proportional(13.0), theme.text_secondary,
            );
        });

        info_row(ui, theme, pad, row_h, w, "Background", |ui, rx, cy| {
            // Color swatch
            let swatch_size = 14.0;
            let swatch_rect = egui::Rect::from_min_size(
                egui::pos2(rx - 70.0, cy - swatch_size / 2.0),
                egui::vec2(swatch_size, swatch_size),
            );
            ui.painter().rect_filled(swatch_rect, 2.0, egui::Color32::BLACK);
            ui.painter().rect_stroke(swatch_rect, 2.0, egui::Stroke::new(0.5, theme.border_light));
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                "#000000", egui::FontId::monospace(12.0), theme.text_secondary,
            );
        });
    });

    ui.add_space(8.0);

    // ── STORAGE ──
    section_card(ui, "STORAGE", theme, |ui| {
        let w = ui.available_width();
        let project_bytes: u64 = state.media_pool.all().map(|m| m.file_size_bytes).sum();

        // Compute cache size from ~/.cache/klipp
        let cache_bytes: u64 = dirs::cache_dir()
            .map(|d| d.join("klipp"))
            .and_then(|p| dir_size_bytes(&p))
            .unwrap_or(0);

        // Compute free disk space using statvfs on the project or home directory
        let home = dirs::home_dir();
        let free_path = state.project_path.as_deref()
            .or(home.as_deref());
        let free_bytes: u64 = free_path
            .and_then(|p| disk_free_bytes(p))
            .unwrap_or(0);

        info_row(ui, theme, pad, row_h, w, "Project size", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                format_bytes(project_bytes), egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row(ui, theme, pad, row_h, w, "Cache size", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                format_bytes(cache_bytes), egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        let free_color = if free_bytes < 1_000_000_000 {
            egui::Color32::from_rgb(210, 60, 60) // red if < 1GB
        } else if free_bytes < 10_000_000_000 {
            theme.accent // amber if < 10GB
        } else {
            egui::Color32::from_rgb(80, 200, 120) // green
        };
        info_row(ui, theme, pad, row_h, w, "Free space", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                format_bytes(free_bytes), egui::FontId::proportional(13.0), free_color,
            );
        });

        // Colored proportion bar
        let total = project_bytes + cache_bytes + free_bytes;
        let project_frac = if total > 0 { project_bytes as f32 / total as f32 } else { 0.0 };
        let cache_frac = if total > 0 { cache_bytes as f32 / total as f32 } else { 0.0 };

        ui.add_space(4.0);
        let bar_h = 8.0;
        let bar_w = w - pad * 2.0;
        let (bar_rect, _) = ui.allocate_exact_size(egui::vec2(w, bar_h + 8.0), egui::Sense::hover());
        let bx = bar_rect.min.x + pad;
        let by = bar_rect.center().y - bar_h / 2.0;
        let full_bar = egui::Rect::from_min_size(egui::pos2(bx, by), egui::vec2(bar_w, bar_h));
        ui.painter().rect_filled(full_bar, 3.0, egui::Color32::from_rgb(60, 60, 65));
        // Project portion (blue)
        let pw = (bar_w * project_frac).max(if project_bytes > 0 { 2.0 } else { 0.0 });
        if pw > 0.0 {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::pos2(bx, by), egui::vec2(pw, bar_h)),
                egui::Rounding { nw: 3.0, ne: 0.0, sw: 3.0, se: 0.0 },
                egui::Color32::from_rgb(60, 120, 200),
            );
        }
        // Cache portion (orange)
        let cw = (bar_w * cache_frac).max(if cache_bytes > 0 { 2.0 } else { 0.0 });
        if cw > 0.0 {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::pos2(bx + pw, by), egui::vec2(cw, bar_h)),
                0.0,
                egui::Color32::from_rgb(200, 150, 60),
            );
        }

        // Legend
        ui.add_space(2.0);
        let (legend_rect, _) = ui.allocate_exact_size(egui::vec2(w, 16.0), egui::Sense::hover());
        let ly = legend_rect.center().y;
        let mut lx = legend_rect.min.x + pad;
        let dot_r = 4.0;
        let legend_items = [
            ("Project", egui::Color32::from_rgb(60, 120, 200)),
            ("Cache", egui::Color32::from_rgb(200, 150, 60)),
            ("Free", egui::Color32::from_rgb(60, 60, 65)),
        ];
        for (label, color) in &legend_items {
            ui.painter().circle_filled(egui::pos2(lx + dot_r, ly), dot_r, *color);
            lx += dot_r * 2.0 + 4.0;
            let galley = ui.painter().layout_no_wrap(
                label.to_string(), egui::FontId::proportional(10.0), theme.text_dim,
            );
            let tw = galley.size().x;
            ui.painter().text(
                egui::pos2(lx, ly), egui::Align2::LEFT_CENTER,
                *label, egui::FontId::proportional(10.0), theme.text_dim,
            );
            lx += tw + 12.0;
        }
    });

    ui.add_space(8.0);
}

fn stat_card(ui: &mut egui::Ui, theme: &KlippTheme, rect: egui::Rect, label: &str, value: &str, value_color: egui::Color32) {
    ui.painter().rect_filled(rect, 4.0, theme.bg_deep);
    ui.painter().text(
        egui::pos2(rect.center().x, rect.min.y + 16.0),
        egui::Align2::CENTER_CENTER,
        label, egui::FontId::proportional(9.0), theme.text_dim,
    );
    ui.painter().text(
        egui::pos2(rect.center().x, rect.max.y - 14.0),
        egui::Align2::CENTER_CENTER,
        value, egui::FontId::proportional(20.0), value_color,
    );
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn section_card(
    ui: &mut egui::Ui,
    title: &str,
    theme: &KlippTheme,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let w = ui.available_width();

    let collapsed_key = egui::Id::new("klipp_collapsed_sections");
    let is_collapsed: bool = ui.ctx().data_mut(|d| {
        let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
        set.contains(title)
    });

    let id = ui.id().with(title).with("video_section");
    let mut col_state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, !is_collapsed);

    let header_h = 30.0;
    let (header_rect, header_resp) = ui.allocate_exact_size(egui::vec2(w, header_h), egui::Sense::click());
    let is_open = col_state.is_open();
    let rounding = if is_open {
        egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 }
    } else {
        egui::Rounding::same(6.0)
    };
    ui.painter().rect_filled(header_rect, rounding, theme.bg_deep);

    let accent_rounding = if is_open {
        egui::Rounding { nw: 6.0, ne: 0.0, sw: 0.0, se: 0.0 }
    } else {
        egui::Rounding { nw: 6.0, ne: 0.0, sw: 6.0, se: 0.0 }
    };
    ui.painter().rect_filled(
        egui::Rect::from_min_size(header_rect.min, egui::vec2(3.0, header_h)),
        accent_rounding, theme.border,
    );

    ui.painter().text(
        egui::pos2(header_rect.min.x + 18.0, header_rect.center().y),
        egui::Align2::LEFT_CENTER,
        title, egui::FontId::proportional(11.0), theme.text_dim,
    );

    let icon = if is_open { Icon::CaretDown } else { Icon::CaretRight };
    let icon_size = 12.0;
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(header_rect.max.x - 20.0, header_rect.center().y),
        egui::vec2(icon_size, icon_size),
    );
    icon.image(icon_size).tint(theme.text_dim).paint_at(ui, icon_rect);

    if header_resp.clicked() {
        col_state.toggle(ui);
        let now_open = col_state.is_open();
        ui.ctx().data_mut(|d| {
            let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
            if now_open {
                set.remove(title);
            } else {
                set.insert(title.to_string());
            }
        });
    }

    if col_state.is_open() {
        let body_start = ui.cursor().min;
        let frame = egui::Frame::default()
            .fill(theme.bg_panel)
            .inner_margin(egui::Margin { left: 0.0, right: 0.0, top: 2.0, bottom: 0.0 })
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE);
        frame.show(ui, |ui| { add_contents(ui); });

        let body_end_y = ui.cursor().min.y;
        ui.painter().rect_filled(
            egui::Rect::from_min_size(body_start, egui::vec2(3.0, body_end_y - body_start.y)),
            0.0, theme.border,
        );
        let full_rect = egui::Rect::from_min_max(header_rect.min, egui::pos2(header_rect.max.x, body_end_y));
        ui.painter().rect_stroke(full_rect, 6.0, egui::Stroke::new(0.5, theme.border));
    } else {
        ui.painter().rect_stroke(header_rect, 6.0, egui::Stroke::new(0.5, theme.border));
    }

    col_state.store(ui.ctx());
}

fn info_row(
    ui: &mut egui::Ui, theme: &KlippTheme, pad: f32, row_h: f32, total_w: f32,
    label: &str, draw_value: impl FnOnce(&mut egui::Ui, f32, f32),
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total_w, row_h), egui::Sense::hover());
    ui.painter().hline(rect.min.x + pad..=rect.max.x - pad, rect.max.y, egui::Stroke::new(0.5, theme.border));
    ui.painter().text(
        egui::pos2(rect.min.x + pad, rect.center().y), egui::Align2::LEFT_CENTER,
        label, egui::FontId::proportional(13.0), theme.text_secondary,
    );
    draw_value(ui, rect.max.x - pad, rect.center().y);
}

fn draw_keyframe_diamond(ui: &mut egui::Ui, center: egui::Pos2, color: egui::Color32, filled: bool) {
    let s = 5.0;
    let pts = vec![
        egui::pos2(center.x, center.y - s),
        egui::pos2(center.x + s, center.y),
        egui::pos2(center.x, center.y + s),
        egui::pos2(center.x - s, center.y),
    ];
    if filled {
        ui.painter().add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
    } else {
        ui.painter().add(egui::Shape::convex_polygon(pts, egui::Color32::TRANSPARENT, egui::Stroke::new(1.5, color)));
    }
}

/// Recursively compute total size of a directory in bytes.
fn dir_size_bytes(path: &std::path::Path) -> Option<u64> {
    if !path.is_dir() {
        return None;
    }
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = entry.metadata().ok()?;
            if meta.is_file() {
                total += meta.len();
            } else if meta.is_dir() {
                total += dir_size_bytes(&entry.path()).unwrap_or(0);
            }
        }
    }
    Some(total)
}

/// Get free disk space on the filesystem containing the given path.
fn disk_free_bytes(path: &std::path::Path) -> Option<u64> {
    use std::ffi::CString;
    let c_path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
            Some(stat.f_bavail as u64 * stat.f_frsize as u64)
        } else {
            None
        }
    }
}
