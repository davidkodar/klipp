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
                ..
            } => Some(TransformParams {
                position_x: *position_x,
                position_y: *position_y,
                scale_x: *scale_x,
                scale_y: *scale_y,
                rotation: *rotation,
                anchor_x: *anchor_x,
                anchor_y: *anchor_y, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0 }),
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
    pub rotation_x: f32,
    pub rotation_y: f32,
    pub perspective: f32,
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
            anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0 }
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
            anchor_y: self.anchor_y, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None }
    }
}

/// Compute KeyframeState for a given keyframe vec at a given clip-relative time.
fn keyframe_state(keyframes: &[klipp_core::Keyframe], clip_time_secs: f64) -> KeyframeState {
    let kf_tolerance = 0.02;
    KeyframeState {
        has_keyframe_at_playhead: keyframes.iter().any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance),
        has_any_keyframes: !keyframes.is_empty(),
        prev_keyframe_time: keyframes.iter().rfind(|k| k.time_secs < clip_time_secs - kf_tolerance)
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
        builtin_effects_enabled: Vec::new(),
        linked_clip_id: None,
        linked_title_asset_id: None,
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
        matte_feather_keyframes: Vec::new(),
        matte_choke_keyframes: Vec::new(),
        clip_markers: Vec::new(),
        effect_keyframes: HashMap::new(),
        role: klipp_core::ClipRole::default(),
        param_modifiers: HashMap::new(),
        speed: 1.0,
        speed_keyframes: Vec::new(),
        source_rotation: 0,
        enabled: true,
        solo: false,
        blend_mode: klipp_core::BlendMode::Normal,
        matte_source: None,
        matte_stencil: None,
        consumed_as_matte: false,
        transform_link: None,
        grade_versions: Vec::new(),
        active_grade_version: None,
        multicam_group: None,
        multicam_angle: 0,
        multicam_cuts: Vec::new(),
        audio_effects: Vec::new(),
        audio_effect_keyframes: std::collections::HashMap::new(),
        stabilization_data: None,
        smoothed_trajectory: None,
        track_points: Vec::new(),
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
    show_role_section(ui, theme, clip, cmds);

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
                theme.text_primary
            } else {
                egui::Color32::from_rgba_premultiplied(theme.text_dim.r(), theme.text_dim.g(), theme.text_dim.b(), 100)
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

                // Source rotation (auto-detected from media metadata, user-overridable)
                if clip.source_rotation != 0 || clip.media_id().is_some() {
                    ui.horizontal(|ui| {
                        ui.label("Source Rotation");
                        let current = clip.source_rotation;
                        let label = match current {
                            0 => "None",
                            90 => "90\u{00b0} CW",
                            180 => "180\u{00b0}",
                            270 => "90\u{00b0} CCW",
                            _ => "Custom",
                        };
                        egui::ComboBox::from_id_salt(format!("{:?}_src_rot", clip_id))
                            .selected_text(label)
                            .width(80.0)
                            .show_ui(ui, |ui| {
                                for (val, text) in [(0, "None"), (90, "90\u{00b0} CW"), (180, "180\u{00b0}"), (270, "90\u{00b0} CCW")] {
                                    if ui.selectable_label(current == val, text).clicked() {
                                        cmds.push(AppCommand::SetSourceRotation { clip_id, degrees: val });
                                    }
                                }
                            });
                    });
                }

                // Separator before opacity
                let sep_w = ui.available_width();
                let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(sep_w, 1.0), egui::Sense::hover());
                ui.painter().hline(sep_rect.x_range(), sep_rect.center().y, egui::Stroke::new(0.5_f32, theme.border));
                ui.add_space(2.0);

                r_op_out = Some(scrubby_param_row(ui, "Opacity", &mut opacity, 0.0..=1.0, 1.0, 0.005, 2, "", Some(opacity_ks)));
            });

            let body_end_y = ui.cursor().min.y;
            ui.painter().rect_filled(
                egui::Rect::from_min_size(body_start, egui::vec2(3.0, body_end_y - body_start.y)),
                0.0, theme.border,
            );
            let full_rect = egui::Rect::from_min_max(header_rect.min, egui::pos2(header_rect.max.x, body_end_y));
            ui.painter().rect_stroke(full_rect.shrink(0.25), 6.0, egui::Stroke::new(0.5_f32, theme.border));
        } else {
            ui.painter().rect_stroke(header_rect.shrink(0.25), 6.0, egui::Stroke::new(0.5_f32, theme.border));
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
    show_ken_burns_section(ui, theme, clip, cmds);

    ui.add_space(8.0);

    // ── BEHAVIORS ──
    show_behaviors_section(ui, theme, clip, cmds);

    ui.add_space(8.0);

    // ── SPEED ──
    show_speed_section(ui, theme, clip, cmds);

    ui.add_space(8.0);

    // ── SPEED RAMP ──
    section_card(ui, "SPEED RAMP", theme, |ui| {
        egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(pad, 6.0))
            .show(ui, |ui| {
                let has_ramp = !clip.speed_keyframes.is_empty();

                // Enable/disable toggle
                let mut ramp_enabled = has_ramp;
                if ui.checkbox(&mut ramp_enabled, egui::RichText::new("Enable Speed Ramp").size(11.0).color(theme.text_primary)).changed() {
                    if ramp_enabled && !has_ramp {
                        // Initialize with a basic two-keyframe ramp
                        let clip_dur = clip.duration().to_secs_f64();
                        cmds.push(AppCommand::SetSpeedKeyframes {
                            clip_id,
                            keyframes: vec![
                                klipp_core::Keyframe {
                                    time_secs: 0.0,
                                    value: 1.0,
                                    interpolation: klipp_core::keyframe::Interpolation::Linear,
                                    tangent_out: (0.0, 0.0),
                                    tangent_in: (0.0, 0.0),
                                },
                                klipp_core::Keyframe {
                                    time_secs: clip_dur,
                                    value: 1.0,
                                    interpolation: klipp_core::keyframe::Interpolation::Linear,
                                    tangent_out: (0.0, 0.0),
                                    tangent_in: (0.0, 0.0),
                                },
                            ],
                        });
                    } else if !ramp_enabled && has_ramp {
                        cmds.push(AppCommand::SetSpeedKeyframes {
                            clip_id,
                            keyframes: Vec::new(),
                        });
                    }
                }

                if has_ramp {
                    ui.add_space(4.0);

                    // Speed ramp curve editor graph
                    let avail_w = ui.available_width();
                    let graph_h = 100.0;
                    let (graph_rect, graph_resp) = ui.allocate_exact_size(
                        egui::vec2(avail_w, graph_h),
                        egui::Sense::click_and_drag(),
                    );

                    let gp = ui.painter_at(graph_rect);

                    // Background
                    gp.rect_filled(graph_rect, 4.0, theme.bg_deep);
                    gp.rect_stroke(graph_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border));

                    let clip_dur = clip.duration().to_secs_f64();
                    if clip_dur > 0.0 {
                        let spd_max = 4.0_f32; // Y axis: 0..4x
                        let margin_x = 2.0_f32;
                        let margin_y = 4.0_f32;
                        let inner = egui::Rect::from_min_max(
                            egui::pos2(graph_rect.left() + margin_x, graph_rect.top() + margin_y),
                            egui::pos2(graph_rect.right() - margin_x, graph_rect.bottom() - margin_y),
                        );

                        let map_x = |t: f64| -> f32 {
                            inner.left() + (t / clip_dur) as f32 * inner.width()
                        };
                        let map_y = |v: f32| -> f32 {
                            inner.bottom() - (v / spd_max).clamp(0.0, 1.0) * inner.height()
                        };
                        let unmap_x = |px: f32| -> f64 {
                            ((px - inner.left()) / inner.width()).clamp(0.0, 1.0) as f64 * clip_dur
                        };
                        let unmap_y = |py: f32| -> f32 {
                            ((inner.bottom() - py) / inner.height()).clamp(0.0, 1.0) * spd_max
                        };

                        // Grid lines at 1x, 2x, 3x
                        for speed_line in [1.0_f32, 2.0, 3.0] {
                            let y = map_y(speed_line);
                            let alpha = if (speed_line - 1.0).abs() < 0.01 { 50 } else { 20 };
                            gp.line_segment(
                                [egui::pos2(inner.left(), y), egui::pos2(inner.right(), y)],
                                egui::Stroke::new(0.5_f32, egui::Color32::from_white_alpha(alpha)),
                            );
                            gp.text(
                                egui::pos2(inner.left() + 2.0, y - 9.0),
                                egui::Align2::LEFT_BOTTOM,
                                format!("{}x", speed_line as i32),
                                egui::FontId::proportional(8.0),
                                egui::Color32::from_white_alpha(60),
                            );
                        }

                        // Draw evaluated speed curve (smooth)
                        let spd_color = theme.marker_info;
                        let num_pts = (inner.width() as usize / 2).clamp(2, 200);
                        let mut curve_pts: Vec<egui::Pos2> = Vec::with_capacity(num_pts);
                        for i in 0..num_pts {
                            let t = (i as f64 / (num_pts - 1) as f64) * clip_dur;
                            let v = klipp_core::keyframe::evaluate(&clip.speed_keyframes, t, clip.speed as f32);
                            curve_pts.push(egui::pos2(map_x(t), map_y(v)));
                        }

                        // Filled area under curve
                        if curve_pts.len() >= 2 {
                            let mut fill_pts = curve_pts.clone();
                            fill_pts.push(egui::pos2(inner.right(), inner.bottom()));
                            fill_pts.push(egui::pos2(inner.left(), inner.bottom()));
                            gp.add(egui::Shape::convex_polygon(
                                fill_pts,
                                egui::Color32::from_rgba_unmultiplied(theme.marker_info.r(), theme.marker_info.g(), theme.marker_info.b(), 15),
                                egui::Stroke::NONE,
                            ));
                        }

                        // Curve line
                        for pair in curve_pts.windows(2) {
                            gp.line_segment(
                                [pair[0], pair[1]],
                                egui::Stroke::new(1.5_f32, spd_color),
                            );
                        }

                        // Keyframe dots (interactive)
                        let kfs = &clip.speed_keyframes;
                        let mut hovered_kf: Option<usize> = None;
                        let pointer = graph_resp.hover_pos();

                        for (i, kf) in kfs.iter().enumerate() {
                            let px = map_x(kf.time_secs);
                            let py = map_y(kf.value);
                            let kf_pos = egui::pos2(px, py);

                            // Check hover
                            let is_hovered = pointer.is_some_and(|p| (p - kf_pos).length() < 8.0);
                            if is_hovered && hovered_kf.is_none() {
                                hovered_kf = Some(i);
                            }

                            let dot_r = if is_hovered { 5.0 } else { 4.0 };
                            gp.circle_filled(kf_pos, dot_r, spd_color);
                            gp.circle_stroke(kf_pos, dot_r, egui::Stroke::new(1.0_f32, egui::Color32::WHITE));

                            // Tooltip on hover
                            if is_hovered {
                                let tip = format!("{:.2}s / {:.2}x", kf.time_secs, kf.value);
                                gp.text(
                                    egui::pos2(px, py - 12.0),
                                    egui::Align2::CENTER_BOTTOM,
                                    tip,
                                    egui::FontId::proportional(9.0),
                                    egui::Color32::WHITE,
                                );
                            }
                        }

                        // Drag keyframe
                        if graph_resp.dragged() {
                            if let Some(pos) = graph_resp.interact_pointer_pos() {
                                // Use persisted drag index
                                let drag_id = ui.id().with("spd_ramp_drag_idx");
                                let drag_idx: Option<usize> = ui.ctx().data(|d| d.get_temp(drag_id));
                                let idx = drag_idx.or(hovered_kf);
                                if let Some(i) = idx {
                                    if i < kfs.len() {
                                        if drag_idx.is_none() {
                                            // Starting drag
                                            ui.ctx().data_mut(|d| d.insert_temp(drag_id, i));
                                            cmds.push(AppCommand::BeginSpeedKeyframeDrag {
                                                key: format!("spd_kf:{}:{}", clip_id.0, i),
                                                clip_id,
                                            });
                                        }
                                        let new_t = unmap_x(pos.x).clamp(0.0, clip_dur);
                                        let new_v = unmap_y(pos.y).clamp(0.1, spd_max) as f64;
                                        cmds.push(AppCommand::UpdateSpeedKeyframe {
                                            clip_id,
                                            index: i,
                                            time_secs: new_t,
                                            speed: new_v,
                                        });
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                                    }
                                }
                            }
                        }

                        // End drag
                        if graph_resp.drag_stopped() {
                            let drag_id = ui.id().with("spd_ramp_drag_idx");
                            let drag_idx: Option<usize> = ui.ctx().data(|d| d.get_temp(drag_id));
                            if let Some(idx) = drag_idx {
                                cmds.push(AppCommand::EndSpeedKeyframeDrag {
                                    key: format!("spd_kf:{}:{}", clip_id.0, idx),
                                    clip_id,
                                });
                                ui.ctx().data_mut(|d| d.remove_by_type::<usize>());
                            }
                        }

                        // Double-click to add keyframe
                        if graph_resp.double_clicked() && hovered_kf.is_none() {
                            if let Some(pos) = graph_resp.interact_pointer_pos() {
                                let t = unmap_x(pos.x).clamp(0.0, clip_dur);
                                let v = (unmap_y(pos.y).clamp(0.1, spd_max)) as f64;
                                cmds.push(AppCommand::AddSpeedKeyframe {
                                    clip_id,
                                    time_secs: t,
                                    speed: v,
                                });
                            }
                        }

                        // Right-click on keyframe to delete
                        if graph_resp.secondary_clicked() {
                            if let Some(i) = hovered_kf {
                                cmds.push(AppCommand::RemoveSpeedKeyframe { clip_id, index: i });
                            }
                        }

                        // Hover cursor
                        if hovered_kf.is_some() && !graph_resp.dragged() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }
                    }

                    ui.add_space(6.0);

                    // Keyframe count info
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{} keyframes", clip.speed_keyframes.len()))
                                .size(10.0)
                                .color(theme.text_dim),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new("Dbl-click: add  |  Right-click: delete")
                                    .size(9.0)
                                    .color(theme.text_dim),
                            );
                        });
                    });

                    ui.add_space(4.0);

                    // Separator
                    let sep_w = ui.available_width();
                    let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(sep_w, 1.0), egui::Sense::hover());
                    ui.painter().hline(sep_rect.x_range(), sep_rect.center().y, egui::Stroke::new(0.5_f32, theme.border));
                    ui.add_space(4.0);

                    // Preset ramp buttons
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
                        let clip_dur = clip.duration().to_secs_f64();
                        let btn = |label: &str| egui::RichText::new(label).size(10.0);

                        if ui.add(egui::Button::new(btn("Ramp Up"))
                            .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                            .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                        ).on_hover_text("0.5x to 2x over clip duration").clicked() {
                            cmds.push(AppCommand::SetSpeedKeyframes {
                                clip_id,
                                keyframes: vec![
                                    klipp_core::Keyframe { time_secs: 0.0, value: 0.5, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                    klipp_core::Keyframe { time_secs: clip_dur, value: 2.0, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                ],
                            });
                        }

                        if ui.add(egui::Button::new(btn("Ramp Down"))
                            .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                            .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                        ).on_hover_text("2x to 0.5x over clip duration").clicked() {
                            cmds.push(AppCommand::SetSpeedKeyframes {
                                clip_id,
                                keyframes: vec![
                                    klipp_core::Keyframe { time_secs: 0.0, value: 2.0, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                    klipp_core::Keyframe { time_secs: clip_dur, value: 0.5, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                ],
                            });
                        }

                        if ui.add(egui::Button::new(btn("Slow-Mo"))
                            .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                            .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                        ).on_hover_text("1x -> 0.25x -> 1x (center slow-mo)").clicked() {
                            cmds.push(AppCommand::SetSpeedKeyframes {
                                clip_id,
                                keyframes: vec![
                                    klipp_core::Keyframe { time_secs: 0.0, value: 1.0, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                    klipp_core::Keyframe { time_secs: clip_dur * 0.3, value: 0.25, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                    klipp_core::Keyframe { time_secs: clip_dur * 0.7, value: 0.25, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                    klipp_core::Keyframe { time_secs: clip_dur, value: 1.0, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                ],
                            });
                        }

                        if ui.add(egui::Button::new(btn("Speed Burst"))
                            .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                            .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                        ).on_hover_text("1x -> 3x -> 1x (center speed burst)").clicked() {
                            cmds.push(AppCommand::SetSpeedKeyframes {
                                clip_id,
                                keyframes: vec![
                                    klipp_core::Keyframe { time_secs: 0.0, value: 1.0, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                    klipp_core::Keyframe { time_secs: clip_dur * 0.3, value: 3.0, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                    klipp_core::Keyframe { time_secs: clip_dur * 0.7, value: 3.0, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                    klipp_core::Keyframe { time_secs: clip_dur, value: 1.0, interpolation: klipp_core::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) },
                                ],
                            });
                        }

                        if ui.add(egui::Button::new(btn("Reset"))
                            .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.marker_error))
                            .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                        ).on_hover_text("Clear all speed keyframes").clicked() {
                            cmds.push(AppCommand::SetSpeedKeyframes {
                                clip_id,
                                keyframes: Vec::new(),
                            });
                        }
                    });
                }
            });
    });

    ui.add_space(8.0);

    // ── TRACKING ──
    show_tracking_section(ui, theme, state, clip, cmds);

    ui.add_space(8.0);

    // ── COLOR ──
    show_color_section(ui, theme, state, clip, cmds);

    ui.add_space(8.0);

    // ── COMPOSITING (blend mode + track matte) ──
    // Phase 3 of track matte: surfaces clip.blend_mode and clip.matte_source.
    // Hidden behind a feature gate would also be reasonable, but Phase 1+2
    // already shipped end-to-end behaviour so the section is safe to show
    // unconditionally — defaults render exactly as before (Normal blend, no
    // matte) for every existing project.
    show_compositing_section(ui, theme, state, clip, cmds, clip_time_secs);

    ui.add_space(8.0);

    // ── TRANSFORM LINK ──
    // Layer 1 of the hyperzoom delivery: bind this clip's transform to
    // another clip's resolved transform with per-axis flags + a
    // composition rule. Hyperzoom (and any future "follow the action"
    // preset) writes through here; a savvy user can also bind directly.
    show_transform_link_section(ui, theme, state, clip, cmds);

    ui.add_space(8.0);

    // ── HYPERZOOM ──
    // Layer 2 preset: scale-keyframe + anchor pair on this clip and
    // the next clip on its track that produces a "fly into the focal
    // point" transition. Spinbox-fallback UI for the focal point
    // (visual letter picker is a deferred follow-up).
    show_hyperzoom_section(ui, theme, state, clip, cmds);

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
                        ui.painter().rect(input_rect, 4.0, theme.bg_raised, egui::Stroke::new(1.0_f32, theme.border));
                        ui.painter().text(
                            input_rect.center(), egui::Align2::CENTER_CENTER,
                            "100 %", egui::FontId::monospace(12.0), theme.text_primary,
                        );
                        // Keyframe diamond
                        let diamond_x = rx - 5.0;
                        draw_keyframe_diamond(ui, egui::pos2(diamond_x, cy),
                            egui::Color32::from_rgba_premultiplied(theme.text_dim.r(), theme.text_dim.g(), theme.text_dim.b(), 90), false);
                    });

                    // Out speed
                    info_row(ui, theme, pad, row_h, w, "Out speed", |ui, rx, cy| {
                        let input_w = 60.0;
                        let input_rect = egui::Rect::from_min_size(
                            egui::pos2(rx - 20.0 - input_w, cy - 13.0), egui::vec2(input_w, 26.0),
                        );
                        ui.painter().rect(input_rect, 4.0, theme.bg_raised, egui::Stroke::new(1.0_f32, theme.border));
                        ui.painter().text(
                            input_rect.center(), egui::Align2::CENTER_CENTER,
                            "100 %", egui::FontId::monospace(12.0), theme.text_primary,
                        );
                        let diamond_x = rx - 5.0;
                        draw_keyframe_diamond(ui, egui::pos2(diamond_x, cy),
                            egui::Color32::from_rgba_premultiplied(theme.text_dim.r(), theme.text_dim.g(), theme.text_dim.b(), 90), false);
                    });

                    // Ease dropdown
                    {
                        let (row_rect, _) = ui.allocate_exact_size(egui::vec2(w, row_h + 4.0), egui::Sense::hover());
                        ui.painter().hline(row_rect.min.x + pad..=row_rect.max.x - pad, row_rect.max.y, egui::Stroke::new(0.5_f32, theme.border));
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
                        ui.painter().rect(combo_rect, 4.0, theme.bg_raised, egui::Stroke::new(1.0_f32, theme.border));
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
                ui.painter().rect_stroke(full_rect.shrink(0.25), 6.0, egui::Stroke::new(0.5_f32, theme.border));
            } else {
                ui.painter().rect_stroke(header_rect.shrink(0.25), 6.0, egui::Stroke::new(0.5_f32, theme.border));
            }
            col_state.store(ui.ctx());
        }
    }

    ui.add_space(8.0);
}

fn show_tracking_section(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip: &Clip, cmds: &mut Vec<AppCommand>) {
    let clip_id = clip.id;
    let pad = 14.0;
    section_card(ui, "TRACKING", theme, |ui| {
        ui.add_space(6.0);
        egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(pad, 6.0))
            .show(ui, |ui| {
                // "Place Tracker" button
                let btn_color = if state.tracking_mode {
                    theme.accent // amber when active
                } else {
                    theme.bg_deep
                };
                let btn_text_color = if state.tracking_mode {
                    theme.text_on_accent // dark on amber
                } else {
                    theme.text_primary
                };
                let btn = egui::Button::new(
                    egui::RichText::new("Place Tracker").color(btn_text_color)
                ).fill(btn_color);
                if ui.add(btn).clicked() {
                    cmds.push(AppCommand::ToggleTrackingMode);
                }

                ui.add_space(4.0);

                // Track active_track_point via egui temp data (inspector state is immutable)
                let tp_sel_id = egui::Id::new("track_point_selected");
                let selected_tp: Option<usize> = ui.ctx().data_mut(|d| d.get_temp(tp_sel_id).unwrap_or(state.active_track_point));

                // List of track points
                let track_points_snapshot: Vec<(usize, String, [u8; 4], usize, usize, bool)> =
                    clip.track_points.iter().enumerate().map(|(i, tp)| {
                        (i, tp.name.clone(), tp.color, tp.positions.len(), tp.start_frame, tp.enabled)
                    }).collect();

                let tracking_active = state.tracking_in_progress.contains(&clip_id);
                let progress = state.tracking_progress.get(&clip_id).copied().unwrap_or(0.0);

                if track_points_snapshot.is_empty() {
                    ui.label(egui::RichText::new("No track points").size(10.0).color(theme.text_dim));
                }

                let mut remove_idx: Option<usize> = None;
                let mut analyze_idx: Option<usize> = None;
                let mut new_selection = selected_tp;

                for (idx, name, color, frame_count, _start_frame, enabled) in &track_points_snapshot {
                    let is_selected = selected_tp == Some(*idx);
                    let _row_resp = ui.horizontal(|ui| {
                        // Colored dot
                        let dot_color = egui::Color32::from_rgba_unmultiplied(
                            color[0], color[1], color[2], color[3],
                        );
                        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot_rect.center(), 4.0, dot_color);

                        // Name + info
                        let display_name = if name.is_empty() {
                            format!("Track {}", idx + 1)
                        } else {
                            name.clone()
                        };
                        let label_color = if *enabled { theme.text_primary } else { theme.text_dim };
                        let label_text = if is_selected {
                            egui::RichText::new(&display_name).size(11.0).color(label_color).strong()
                        } else {
                            egui::RichText::new(&display_name).size(11.0).color(label_color)
                        };
                        if ui.add(egui::Label::new(label_text).sense(egui::Sense::click())).clicked() {
                            new_selection = if is_selected { None } else { Some(*idx) };
                        }

                        ui.label(egui::RichText::new(format!("{} fr", frame_count)).size(9.0).color(theme.text_dim));

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Delete button
                            if ui.add(egui::Button::new(egui::RichText::new("X").size(9.0).color(theme.text_dim)).frame(false)).clicked() {
                                remove_idx = Some(*idx);
                            }
                            // Analyze button
                            if !tracking_active {
                                if ui.add(egui::Button::new(egui::RichText::new("Analyze").size(9.0)).frame(true)).clicked() {
                                    analyze_idx = Some(*idx);
                                }
                            }
                        });
                    });

                    // Show selected track point's link options
                    if is_selected {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);

                            ui.label(egui::RichText::new("Link to:").size(10.0).color(theme.text_dim));

                            if ui.add(egui::Button::new(egui::RichText::new("Transform Pos").size(9.0)).frame(true)).clicked() {
                                cmds.push(AppCommand::GenerateKeyframesFromTrack {
                                    clip_id,
                                    point_index: *idx,
                                    target: klipp_core::TrackTarget::TransformPosition,
                                });
                            }

                            // Mask center targets
                            let mask_targets: Vec<(uuid::Uuid, usize, String)> = clip.node_graph.nodes.iter()
                                .flat_map(|n| {
                                    let masks = n.params.masks.as_ref()
                                        .map(|ms| ms.masks.as_slice())
                                        .unwrap_or(&[]);
                                    let node_id = n.id;
                                    let label = n.label.clone();
                                    masks.iter().enumerate().map(move |(mi, _m)| {
                                        (node_id, mi, format!("{} Mask {}", label, mi + 1))
                                    })
                                })
                                .collect();
                            for (node_uuid, mask_idx, label) in &mask_targets {
                                if ui.add(egui::Button::new(egui::RichText::new(label).size(9.0)).frame(true)).clicked() {
                                    cmds.push(AppCommand::GenerateKeyframesFromTrack {
                                        clip_id,
                                        point_index: *idx,
                                        target: klipp_core::TrackTarget::MaskCenter { node_id: *node_uuid, mask_index: *mask_idx },
                                    });
                                }
                            }
                        });
                    }
                }

                // Store updated selection
                ui.ctx().data_mut(|d| d.insert_temp(tp_sel_id, new_selection));

                // Progress bar during analysis
                if tracking_active {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Analyzing...").size(10.0).color(theme.text_accent));
                        let bar_w = ui.available_width() - 4.0;
                        let (bar_rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, 4.0), egui::Sense::hover());
                        ui.painter().rect_filled(bar_rect, 2.0, theme.bg_deep);
                        let fill_rect = egui::Rect::from_min_size(
                            bar_rect.min,
                            egui::vec2(bar_w * progress, 4.0),
                        );
                        ui.painter().rect_filled(fill_rect, 2.0, theme.accent);
                    });
                }

                // Process deferred commands
                if let Some(idx) = remove_idx {
                    cmds.push(AppCommand::RemoveTrackPoint { clip_id, index: idx });
                }
                if let Some(idx) = analyze_idx {
                    cmds.push(AppCommand::AnalyzeTrackPoint { clip_id, point_index: idx });
                }
            });
    });
}

fn show_color_section(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip: &Clip, cmds: &mut Vec<AppCommand>) {
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
                        .stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(6.0),
                    ).clicked() {
                        cmds.push(AppCommand::ToggleColorPanel);
                    }

                    // C shortcut badge
                    let (badge_rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
                    ui.painter().rect(badge_rect, 4.0, theme.bg_deep, egui::Stroke::new(0.5_f32, theme.border));
                    ui.painter().text(
                        badge_rect.center(), egui::Align2::CENTER_CENTER,
                        "C", egui::FontId::monospace(10.0), theme.text_dim,
                    );
                });
            });
        ui.add_space(8.0);
    });
}

/// Compositing controls — per-clip blend mode + track-matte binding.
/// Surfaces all four matte AppCommands (`SetClipBlendMode`,
/// `SetClipMatte`, `ClearClipMatte`, `SetClipConsumedAsMatte`) shipped
/// in Phase 1, plus a source picker that walks the active sequence
/// for valid candidate clips. Cycle / self-reference / AdjustmentLayer
/// rejections are still enforced inside the handler — the picker just
/// avoids offering ineligible candidates so the common path doesn't
/// trip a rejection toast.
fn show_compositing_section(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
    clip_time_secs: f64,
) {
    let clip_id = clip.id;
    let pad = 14.0;
    section_card(ui, "COMPOSITING", theme, |ui| {
        ui.add_space(6.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: pad, right: pad, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 8.0);

                // ── Blend Mode ──
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Blend Mode")
                            .size(11.0)
                            .color(theme.text_dim),
                    );
                    ui.add_space(8.0);
                    let mut current = clip.blend_mode;
                    let combo = egui::ComboBox::from_id_salt(("video:blend_mode", clip_id))
                        .selected_text(current.display_name())
                        .width(160.0);
                    combo.show_ui(ui, |ui| {
                        for &mode in &klipp_core::BlendMode::ALL {
                            if ui.selectable_value(&mut current, mode, mode.display_name()).clicked() {
                                if mode != clip.blend_mode {
                                    cmds.push(AppCommand::SetClipBlendMode { clip_id, mode });
                                }
                            }
                        }
                    });
                });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Stencil mode (Phase 4B) ─────────────────────────
                // Toggle on the source side: when set, this clip's
                // pixels mask every clip on a lower track at the
                // same time-range. Mutually informative with the
                // consumer-side `Matte Source` below — the two are
                // separate features, not exclusive.
                ui.horizontal(|ui| {
                    let mut stencil_on = clip.matte_stencil.is_some();
                    if ui.checkbox(&mut stencil_on, "Stencil mode (mask all clips below)").changed() {
                        if stencil_on {
                            // Default settings inherit feather/choke
                            // from the consumer-side binding when the
                            // user has one — keeps both edges feeling
                            // similar without an extra dialog.
                            let (feather, choke) = clip.matte_source.as_ref()
                                .map(|b| (b.feather_px, b.choke_px))
                                .unwrap_or((0.0, 0.0));
                            cmds.push(AppCommand::SetClipStencil {
                                clip_id,
                                channel: 1, // Luminance default
                                invert: false,
                                feather_px: feather,
                                choke_px: choke,
                            });
                        } else {
                            cmds.push(AppCommand::ClearClipStencil { clip_id });
                        }
                    }
                });
                if let Some(stencil) = clip.matte_stencil.clone() {
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.label(
                            egui::RichText::new("Channel")
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                        ui.add_space(8.0);
                        let mut current_ch = stencil.channel;
                        for (ch, label) in [
                            (klipp_nodes::params::MatteChannel::Alpha, "Alpha"),
                            (klipp_nodes::params::MatteChannel::Luminance, "Luma"),
                            (klipp_nodes::params::MatteChannel::Red, "R"),
                            (klipp_nodes::params::MatteChannel::Green, "G"),
                            (klipp_nodes::params::MatteChannel::Blue, "B"),
                        ] {
                            if ui.selectable_value(&mut current_ch, ch, label).clicked() && current_ch != stencil.channel {
                                cmds.push(AppCommand::SetClipStencil {
                                    clip_id,
                                    channel: matte_channel_to_u8(current_ch),
                                    invert: stencil.invert,
                                    feather_px: stencil.feather_px,
                                    choke_px: stencil.choke_px,
                                });
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        let mut inv = stencil.invert;
                        if ui.checkbox(&mut inv, "Invert").changed() {
                            cmds.push(AppCommand::SetClipStencil {
                                clip_id,
                                channel: matte_channel_to_u8(stencil.channel),
                                invert: inv,
                                feather_px: stencil.feather_px,
                                choke_px: stencil.choke_px,
                            });
                        }
                    });
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Matte source picker ──
                // Build candidate list: every clip in the active sequence
                // that isn't `clip_id` itself, isn't an AdjustmentLayer,
                // and (defensively) isn't already in this clip's matte
                // chain. The handler still cycle-rejects, but skipping
                // ineligible options keeps the dropdown focused.
                let active_idx = state.active_sequence_idx;
                let candidates: Vec<(klipp_core::ClipId, String)> = state
                    .sequences
                    .get(active_idx)
                    .map(|seq| {
                        seq.tracks.iter().enumerate().flat_map(|(ti, track)| {
                            let track_label = if track.name.is_empty() {
                                format!("Track {}", ti + 1)
                            } else {
                                track.name.clone()
                            };
                            track.clips.iter().filter_map(move |c| {
                                if c.id == clip_id { return None; }
                                if matches!(c.kind, klipp_timeline::ClipKind::AdjustmentLayer) { return None; }
                                let label = format!("{} · clip {}…",
                                    track_label,
                                    short_clip_id(&c.id));
                                Some((c.id, label))
                            }).collect::<Vec<_>>()
                        }).collect()
                    })
                    .unwrap_or_default();

                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Matte Source")
                            .size(11.0)
                            .color(theme.text_dim),
                    );
                    ui.add_space(8.0);
                    let current_label = match clip.matte_source.as_ref() {
                        None => "(none)".to_string(),
                        Some(b) => candidates.iter()
                            .find(|(id, _)| *id == b.source_clip_id)
                            .map(|(_, l)| l.clone())
                            .unwrap_or_else(|| "(missing — see banner)".to_string()),
                    };
                    egui::ComboBox::from_id_salt(("video:matte_source", clip_id))
                        .selected_text(current_label)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            // "(none)" entry — clears any existing binding.
                            let none_selected = clip.matte_source.is_none();
                            if ui.selectable_label(none_selected, "(none)").clicked() {
                                if !none_selected {
                                    cmds.push(AppCommand::ClearClipMatte { clip_id });
                                }
                            }
                            for (cand_id, label) in &candidates {
                                let selected = clip.matte_source.as_ref()
                                    .is_some_and(|b| b.source_clip_id == *cand_id);
                                if ui.selectable_label(selected, label).clicked() {
                                    if !selected {
                                        // Inherit existing channel/invert/feather/choke
                                        // when re-pointing at a different source, default
                                        // to the easy-path Luma binding otherwise.
                                        let (channel_u8, invert, feather_px, choke_px) = match clip.matte_source.as_ref() {
                                            Some(b) => (
                                                matte_channel_to_u8(b.channel),
                                                b.invert,
                                                b.feather_px,
                                                b.choke_px,
                                            ),
                                            None => (1u8, false, 0.0, 0.0), // Luminance default
                                        };
                                        cmds.push(AppCommand::SetClipMatte {
                                            clip_id,
                                            source_clip_id: *cand_id,
                                            channel: channel_u8,
                                            invert,
                                            feather_px,
                                            choke_px,
                                        });
                                    }
                                }
                            }
                        });
                });

                // Detail rows only meaningful when a binding exists.
                if let Some(binding) = clip.matte_source.clone() {
                    let source_id = binding.source_clip_id;
                    let source_present = state.sequences.get(active_idx)
                        .map(|seq| seq.tracks.iter().any(|t| t.clips.iter().any(|c| c.id == source_id)))
                        .unwrap_or(false);
                    if !source_present {
                        ui.add_space(4.0);
                        ui.colored_label(
                            theme.marker_warning,
                            egui::RichText::new("⚠ Matte source clip is no longer in the project — clip will render invisible.")
                                .size(10.5),
                        );
                    }

                    ui.add_space(4.0);

                    // ── Channel ──
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Channel")
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                        ui.add_space(8.0);
                        let mut current_ch = binding.channel;
                        for (ch, label) in [
                            (klipp_nodes::params::MatteChannel::Alpha, "Alpha"),
                            (klipp_nodes::params::MatteChannel::Luminance, "Luma"),
                            (klipp_nodes::params::MatteChannel::Red, "R"),
                            (klipp_nodes::params::MatteChannel::Green, "G"),
                            (klipp_nodes::params::MatteChannel::Blue, "B"),
                        ] {
                            if ui.selectable_value(&mut current_ch, ch, label).clicked() && current_ch != binding.channel {
                                cmds.push(AppCommand::SetClipMatte {
                                    clip_id,
                                    source_clip_id: binding.source_clip_id,
                                    channel: matte_channel_to_u8(current_ch),
                                    invert: binding.invert,
                                    feather_px: binding.feather_px,
                                    choke_px: binding.choke_px,
                                });
                            }
                        }
                    });

                    // ── Invert ──
                    ui.horizontal(|ui| {
                        let mut inv = binding.invert;
                        if ui.checkbox(&mut inv, "Invert").changed() {
                            cmds.push(AppCommand::SetClipMatte {
                                clip_id,
                                source_clip_id: binding.source_clip_id,
                                channel: matte_channel_to_u8(binding.channel),
                                invert: inv,
                                feather_px: binding.feather_px,
                                choke_px: binding.choke_px,
                            });
                        }
                    });

                    // ── Feather ──
                    // Phase 4D — feather/choke can be keyframed via the
                    // existing video-keyframe AppCommands. The slider
                    // edits the binding's static fallback (used when
                    // there are no kfs); the diamond toggles a kf at
                    // the current playhead. When kfs are present, the
                    // render path evaluates them and overrides the
                    // static value frame-by-frame. Mirrors the opacity/
                    // transform UX users already know.
                    let kf_tolerance_secs = 0.02_f64;
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Feather")
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                        ui.add_space(8.0);
                        let mut feather = klipp_core::keyframe::evaluate(
                            &clip.matte_feather_keyframes, clip_time_secs, binding.feather_px,
                        );
                        if ui.add(egui::Slider::new(&mut feather, 0.0..=50.0)
                            .suffix(" px")
                            .clamping(egui::SliderClamping::Always)).changed()
                        {
                            // If kfs exist near the playhead, rewrite
                            // that kf; otherwise fold the static value.
                            let near_kf = clip.matte_feather_keyframes.iter()
                                .position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance_secs);
                            if let Some(idx) = near_kf {
                                cmds.push(AppCommand::MoveVideoKeyframe {
                                    clip_id,
                                    param: klipp_core::VideoParam::MatteFeather,
                                    index: idx,
                                    new_time_secs: clip.matte_feather_keyframes[idx].time_secs,
                                    new_value: feather,
                                });
                            } else if !clip.matte_feather_keyframes.is_empty() {
                                // KFs exist but none at playhead — adding
                                // a new one preserves the curve shape and
                                // is what the user expects from a slider tug.
                                cmds.push(AppCommand::AddVideoKeyframe {
                                    clip_id,
                                    param: klipp_core::VideoParam::MatteFeather,
                                    time_secs: clip_time_secs,
                                    value: feather,
                                });
                            } else {
                                // No kfs at all — edit the static.
                                cmds.push(AppCommand::SetClipMatte {
                                    clip_id,
                                    source_clip_id: binding.source_clip_id,
                                    channel: matte_channel_to_u8(binding.channel),
                                    invert: binding.invert,
                                    feather_px: feather,
                                    choke_px: binding.choke_px,
                                });
                            }
                        }
                        // Keyframe diamond toggle.
                        let has_kf_here = clip.matte_feather_keyframes.iter()
                            .any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance_secs);
                        let diamond_label = if has_kf_here { "◆" } else { "◇" };
                        if ui.small_button(diamond_label).on_hover_text(
                            if has_kf_here { "Remove keyframe at playhead" } else { "Add keyframe at playhead" }
                        ).clicked() {
                            if has_kf_here {
                                cmds.push(AppCommand::RemoveVideoKeyframe {
                                    clip_id, param: klipp_core::VideoParam::MatteFeather,
                                    time_secs: clip_time_secs,
                                });
                            } else {
                                cmds.push(AppCommand::AddVideoKeyframe {
                                    clip_id, param: klipp_core::VideoParam::MatteFeather,
                                    time_secs: clip_time_secs, value: feather,
                                });
                            }
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Choke")
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                        ui.add_space(8.0);
                        let mut choke = klipp_core::keyframe::evaluate(
                            &clip.matte_choke_keyframes, clip_time_secs, binding.choke_px,
                        );
                        if ui.add(egui::Slider::new(&mut choke, 0.0..=50.0)
                            .suffix(" px")
                            .clamping(egui::SliderClamping::Always)).changed()
                        {
                            let near_kf = clip.matte_choke_keyframes.iter()
                                .position(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance_secs);
                            if let Some(idx) = near_kf {
                                cmds.push(AppCommand::MoveVideoKeyframe {
                                    clip_id,
                                    param: klipp_core::VideoParam::MatteChoke,
                                    index: idx,
                                    new_time_secs: clip.matte_choke_keyframes[idx].time_secs,
                                    new_value: choke,
                                });
                            } else if !clip.matte_choke_keyframes.is_empty() {
                                cmds.push(AppCommand::AddVideoKeyframe {
                                    clip_id,
                                    param: klipp_core::VideoParam::MatteChoke,
                                    time_secs: clip_time_secs,
                                    value: choke,
                                });
                            } else {
                                cmds.push(AppCommand::SetClipMatte {
                                    clip_id,
                                    source_clip_id: binding.source_clip_id,
                                    channel: matte_channel_to_u8(binding.channel),
                                    invert: binding.invert,
                                    feather_px: binding.feather_px,
                                    choke_px: choke,
                                });
                            }
                        }
                        let has_kf_here = clip.matte_choke_keyframes.iter()
                            .any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance_secs);
                        let diamond_label = if has_kf_here { "◆" } else { "◇" };
                        if ui.small_button(diamond_label).on_hover_text(
                            if has_kf_here { "Remove keyframe at playhead" } else { "Add keyframe at playhead" }
                        ).clicked() {
                            if has_kf_here {
                                cmds.push(AppCommand::RemoveVideoKeyframe {
                                    clip_id, param: klipp_core::VideoParam::MatteChoke,
                                    time_secs: clip_time_secs,
                                });
                            } else {
                                cmds.push(AppCommand::AddVideoKeyframe {
                                    clip_id, param: klipp_core::VideoParam::MatteChoke,
                                    time_secs: clip_time_secs, value: choke,
                                });
                            }
                        }
                    });

                    // ── Hide source from composite ──
                    let source_consumed = state.sequences.get(active_idx)
                        .and_then(|seq| seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == source_id))
                        .map(|c| c.consumed_as_matte)
                        .unwrap_or(false);
                    let mut hide_source = source_consumed;
                    if ui.checkbox(&mut hide_source, "Hide matte source from composite").changed() {
                        cmds.push(AppCommand::SetClipConsumedAsMatte {
                            clip_id: source_id,
                            consumed: hide_source,
                        });
                    }

                    ui.add_space(8.0);

                    // ── Live mask preview (Phase 4C) ──
                    // Engine emits one ~96×54 grayscale snapshot per
                    // composited frame as long as the inspector has
                    // set this clip as the preview target. We dispatch
                    // the target unconditionally on every render so
                    // the engine knows we're still watching; the cost
                    // is one cheap AppCommand per frame.
                    cmds.push(AppCommand::SetInspectorMattePreviewTarget(Some(clip_id)));

                    ui.label(
                        egui::RichText::new("Mask preview")
                            .size(11.0)
                            .color(theme.text_dim),
                    );
                    let preview_w = 192.0_f32;
                    let preview_h = 108.0_f32;
                    let (preview_rect, _) = ui.allocate_exact_size(
                        egui::vec2(preview_w, preview_h),
                        egui::Sense::hover(),
                    );
                    // Subtle frame so the preview reads as a "tile"
                    // rather than free-floating pixels.
                    ui.painter().rect_filled(preview_rect, 4.0, theme.bg_deep);
                    ui.painter().rect_stroke(preview_rect, 4.0, egui::Stroke::new(0.5, theme.border));
                    if state.matte_thumb_for == Some(clip_id) {
                        if let Some(tex) = state.matte_thumb_handle.as_ref() {
                            ui.painter().image(
                                tex.id(),
                                preview_rect.shrink(2.0),
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                egui::Color32::WHITE,
                            );
                        }
                    } else {
                        // First-frame placeholder: the engine hasn't
                        // emitted a snapshot for this clip yet.
                        ui.painter().text(
                            preview_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "rendering…",
                            egui::FontId::proportional(11.0),
                            theme.text_dim,
                        );
                    }

                    ui.add_space(4.0);

                    // ── Clear ──
                    if ui.add_sized(egui::vec2(120.0, 24.0),
                        egui::Button::new(
                            egui::RichText::new("Clear matte")
                                .size(11.0)
                                .color(theme.text_primary),
                        )
                        .fill(theme.bg_raised)
                        .stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(6.0)).clicked()
                    {
                        cmds.push(AppCommand::ClearClipMatte { clip_id });
                    }
                } else {
                    // No binding on this clip — make sure the engine
                    // stops emitting preview events. Idempotent so
                    // the per-frame dispatch is harmless.
                    cmds.push(AppCommand::SetInspectorMattePreviewTarget(None));
                }
            });
        ui.add_space(8.0);
    });
}

fn show_transform_link_section(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
) {
    let clip_id = clip.id;
    let pad = 14.0;
    // AdjustmentLayer clips are rejected by the AppCommand handler as
    // both link origin and target — there's nothing meaningful to bind
    // for a layer that has no own pixels. Hide the section entirely
    // rather than show a disabled control whose error message would
    // only fire on click.
    if matches!(clip.kind, klipp_timeline::ClipKind::AdjustmentLayer) {
        return;
    }
    section_card(ui, "TRANSFORM LINK", theme, |ui| {
        ui.add_space(6.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: pad, right: pad, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 8.0);

                // Build candidate list: every clip in the active sequence
                // that isn't this clip, isn't an AdjustmentLayer, and
                // isn't already on a chain that would loop back here.
                // The handler enforces all three rules; filtering the
                // dropdown lines up the UI choices with what the handler
                // will accept so the user doesn't pick a path that
                // silently no-ops.
                let active_idx = state.active_sequence_idx;
                let candidates: Vec<(klipp_core::ClipId, String)> = state
                    .sequences
                    .get(active_idx)
                    .map(|seq| {
                        seq.tracks.iter().enumerate().flat_map(|(ti, track)| {
                            let track_label = if track.name.is_empty() {
                                format!("Track {}", ti + 1)
                            } else {
                                track.name.clone()
                            };
                            track.clips.iter().filter_map(move |c| {
                                if c.id == clip_id { return None; }
                                if matches!(c.kind, klipp_timeline::ClipKind::AdjustmentLayer) { return None; }
                                if transform_link_chain_creates_cycle_local(seq, clip_id, c.id) { return None; }
                                let label = format!("{} · clip {}…",
                                    track_label,
                                    short_clip_id(&c.id));
                                Some((c.id, label))
                            }).collect::<Vec<_>>()
                        }).collect()
                    })
                    .unwrap_or_default();

                // ── Target picker ──
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Target")
                            .size(11.0)
                            .color(theme.text_dim),
                    );
                    ui.add_space(8.0);
                    let current_label = match clip.transform_link.as_ref() {
                        None => "(none)".to_string(),
                        Some(tl) => candidates.iter()
                            .find(|(id, _)| *id == tl.target)
                            .map(|(_, l)| l.clone())
                            .unwrap_or_else(|| "(missing — see banner)".to_string()),
                    };
                    egui::ComboBox::from_id_salt(("video:transform_link_target", clip_id))
                        .selected_text(current_label)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            let none_selected = clip.transform_link.is_none();
                            if ui.selectable_label(none_selected, "(none)").clicked() {
                                if !none_selected {
                                    cmds.push(AppCommand::ClearClipTransformLink { clip_id });
                                }
                            }
                            for (cand_id, label) in &candidates {
                                let selected = clip.transform_link.as_ref()
                                    .is_some_and(|tl| tl.target == *cand_id);
                                if ui.selectable_label(selected, label).clicked() {
                                    if !selected {
                                        // Re-pointing at a new target: keep the
                                        // existing mode + flag set so the user
                                        // doesn't lose their per-axis tuning.
                                        // Fresh bind: easy-path defaults (all
                                        // flags on, Absolute) — same as
                                        // `TransformLink::new`.
                                        let (mode, isc, ipo, iro, ian) = match clip.transform_link.as_ref() {
                                            Some(tl) => (
                                                link_mode_to_u8(tl.mode),
                                                tl.inherit_scale,
                                                tl.inherit_position,
                                                tl.inherit_rotation,
                                                tl.inherit_anchor,
                                            ),
                                            None => (0u8, true, true, true, true),
                                        };
                                        cmds.push(AppCommand::SetClipTransformLink {
                                            clip_id,
                                            target: *cand_id,
                                            mode,
                                            inherit_scale: isc,
                                            inherit_position: ipo,
                                            inherit_rotation: iro,
                                            inherit_anchor: ian,
                                        });
                                    }
                                }
                            }
                        });
                });

                // Detail rows only meaningful when a binding exists.
                if let Some(link) = clip.transform_link.clone() {
                    let target_id = link.target;
                    let target_present = state.sequences.get(active_idx)
                        .map(|seq| seq.tracks.iter().any(|t| t.clips.iter().any(|c| c.id == target_id)))
                        .unwrap_or(false);
                    if !target_present {
                        ui.add_space(4.0);
                        ui.colored_label(
                            theme.marker_warning,
                            egui::RichText::new("⚠ Target clip is no longer in the project — this clip falls back to its own transform.")
                                .size(10.5),
                        );
                    }

                    ui.add_space(4.0);

                    // ── Mode (Absolute / Additive) ──
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Mode")
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                        ui.add_space(8.0);
                        let mut mode = link.mode;
                        for (m, label, hover) in [
                            (klipp_timeline::LinkMode::Absolute, "Absolute",
                                "Replace this clip's transform with the target's (per-axis flags decide which axes inherit)."),
                            (klipp_timeline::LinkMode::Additive, "Additive",
                                "Compose: positions add, scales multiply, rotations add. Anchor still inherits absolutely."),
                        ] {
                            let resp = ui.selectable_value(&mut mode, m, label);
                            if resp.on_hover_text(hover).clicked() && mode != link.mode {
                                cmds.push(AppCommand::SetClipTransformLink {
                                    clip_id,
                                    target: link.target,
                                    mode: link_mode_to_u8(mode),
                                    inherit_scale: link.inherit_scale,
                                    inherit_position: link.inherit_position,
                                    inherit_rotation: link.inherit_rotation,
                                    inherit_anchor: link.inherit_anchor,
                                });
                            }
                        }
                    });

                    // ── Per-axis inherit flags ──
                    // 2×2 grid feels tighter than a single column and
                    // makes the four axes legible at a glance.
                    let mut inherit_scale = link.inherit_scale;
                    let mut inherit_position = link.inherit_position;
                    let mut inherit_rotation = link.inherit_rotation;
                    let mut inherit_anchor = link.inherit_anchor;
                    let mut changed = false;
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut inherit_position, "Position").changed() { changed = true; }
                        ui.add_space(12.0);
                        if ui.checkbox(&mut inherit_scale, "Scale").changed() { changed = true; }
                    });
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut inherit_rotation, "Rotation").changed() { changed = true; }
                        ui.add_space(12.0);
                        if ui.checkbox(&mut inherit_anchor, "Anchor").changed() { changed = true; }
                    });
                    if changed {
                        cmds.push(AppCommand::SetClipTransformLink {
                            clip_id,
                            target: link.target,
                            mode: link_mode_to_u8(link.mode),
                            inherit_scale,
                            inherit_position,
                            inherit_rotation,
                            inherit_anchor,
                        });
                    }

                    ui.add_space(4.0);

                    // ── Clear ──
                    if ui.add_sized(egui::vec2(140.0, 24.0),
                        egui::Button::new(
                            egui::RichText::new("Clear transform link")
                                .size(11.0)
                                .color(theme.text_primary),
                        )
                        .fill(theme.bg_raised)
                        .stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(6.0)).clicked()
                    {
                        cmds.push(AppCommand::ClearClipTransformLink { clip_id });
                    }
                }
            });
        ui.add_space(8.0);
    });
}

fn show_hyperzoom_section(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
) {
    let clip_id = clip.id;
    let pad = 14.0;

    // Hyperzoom needs a target clip to fly into. Skip for clip kinds
    // where the operation makes no sense:
    //  • AdjustmentLayer — no own pixels
    //  • Gap — silence; not a visible shot
    //  • Sequence — nested sequences are out of scope for v1
    use klipp_timeline::ClipKind;
    if matches!(
        clip.kind,
        ClipKind::AdjustmentLayer | ClipKind::Gap | ClipKind::Sequence { .. }
    ) {
        return;
    }

    // egui-id-keyed scratch state for the spinboxes — survives
    // across frames as long as this section keeps rendering. The
    // viewer's focal-point crosshair reads/writes the same cell so
    // dragging on the canvas updates the spinboxes live and vice-
    // versa. Defaults: focal centre, half-second ramp, 50× peak.
    let stored = crate::hyperzoom::read_hyperzoom_scratch(ui.ctx(), clip_id);
    let (mut focal_x, mut focal_y, mut duration_secs, mut max_scale, mut target_choice, mut easing, mut hold_secs, mut add_radial_blur, mut radial_blur_amount) = (
        stored.focal_x,
        stored.focal_y,
        stored.duration_secs,
        stored.max_scale,
        stored.target_choice,
        stored.easing,
        stored.hold_secs,
        stored.add_radial_blur,
        stored.radial_blur_amount,
    );

    section_card(ui, "HYPERZOOM", theme, |ui| {
        ui.add_space(6.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: pad, right: pad, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 8.0);

                // ── Target picker ──
                // "Auto" = next clip on this clip's track. Anything
                // else explicitly names a clip elsewhere in the
                // sequence — useful when the next clip in track order
                // isn't the one the user wants to fly into.
                let active_idx = state.active_sequence_idx;
                let candidates: Vec<(klipp_core::ClipId, String)> = state
                    .sequences
                    .get(active_idx)
                    .map(|seq| {
                        seq.tracks.iter().enumerate().flat_map(|(ti, track)| {
                            let track_label = if track.name.is_empty() {
                                format!("Track {}", ti + 1)
                            } else {
                                track.name.clone()
                            };
                            track.clips.iter().filter_map(move |c| {
                                if c.id == clip_id { return None; }
                                if matches!(c.kind, ClipKind::AdjustmentLayer | ClipKind::Gap | ClipKind::Sequence { .. }) {
                                    return None;
                                }
                                let label = format!("{} · clip {}…",
                                    track_label,
                                    short_clip_id(&c.id));
                                Some((c.id, label))
                            }).collect::<Vec<_>>()
                        }).collect()
                    })
                    .unwrap_or_default();

                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Target").size(11.0).color(theme.text_dim));
                    ui.add_space(8.0);
                    let current_label = match target_choice {
                        HyperzoomTargetChoice::Auto => "(next clip on this track)".to_string(),
                        HyperzoomTargetChoice::Explicit(id) => candidates
                            .iter()
                            .find(|(cid, _)| *cid == id)
                            .map(|(_, l)| l.clone())
                            .unwrap_or_else(|| "(missing — falls back to auto)".to_string()),
                    };
                    egui::ComboBox::from_id_salt(("video:hyperzoom_target", clip_id))
                        .selected_text(current_label)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            let auto = matches!(target_choice, HyperzoomTargetChoice::Auto);
                            if ui.selectable_label(auto, "(next clip on this track)").clicked() && !auto {
                                target_choice = HyperzoomTargetChoice::Auto;
                            }
                            for (cand_id, label) in &candidates {
                                let sel = matches!(target_choice, HyperzoomTargetChoice::Explicit(id) if id == *cand_id);
                                if ui.selectable_label(sel, label).clicked() && !sel {
                                    target_choice = HyperzoomTargetChoice::Explicit(*cand_id);
                                }
                            }
                        });
                });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Focal point (X / Y) ──
                // Spinbox fallback for the visual letter picker. 0,0
                // = top-left; 1,1 = bottom-right; 0.5,0.5 = centre.
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Focal X").size(11.0).color(theme.text_dim));
                    ui.add_space(8.0);
                    ui.add(
                        egui::DragValue::new(&mut focal_x)
                            .range(0.0_f32..=1.0)
                            .speed(0.005)
                            .fixed_decimals(3),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Focal Y").size(11.0).color(theme.text_dim));
                    ui.add_space(8.0);
                    ui.add(
                        egui::DragValue::new(&mut focal_y)
                            .range(0.0_f32..=1.0)
                            .speed(0.005)
                            .fixed_decimals(3),
                    );
                });
                ui.label(
                    egui::RichText::new("0.5 / 0.5 = clip centre. Click a letter in the preview below to snap.")
                        .size(10.0)
                        .color(theme.text_dim),
                );

                // ── Visual letter picker ──
                // Renders a small preview of the title clip whose
                // glyphs feed the picker (resolved via
                // letter_picker::picker_target_for_clip — matte
                // source first, overlapping title fallback). Click
                // a glyph → focal X/Y snaps to its centroid; the
                // glyph stays outlined while the picker is visible.
                // Hidden silently when no title context exists; the
                // user falls back to the spinboxes + draggable
                // crosshair on the main viewer.
                let target = crate::inspector::letter_picker::picker_target_for_clip(
                    state.sequence(),
                    clip_id,
                    state.playhead,
                );
                if let Some(title_clip) = target {
                    // Existence guard — bitmap clone is no longer
                    // needed here (App pre-rendered it); the picker
                    // only needs glyph positions, which the cache
                    // helper computes from `title_clip` directly.
                    if title_clip.title_data().is_some() {
                        ui.add_space(6.0);
                        // Preview slot: ~210×118 px, 16:9-ish so an
                        // FHD title fits without distortion.
                        let preview_w = 210.0_f32;
                        let preview_h = 118.0_f32;
                        let (preview_rect, preview_resp) = ui.allocate_exact_size(
                            egui::vec2(preview_w, preview_h),
                            egui::Sense::click_and_drag(),
                        );
                        // Backdrop so the white-on-transparent title
                        // reads. Matches the matte preview's tile
                        // aesthetic in the COMPOSITING section.
                        ui.painter().rect_filled(preview_rect, 4.0, theme.bg_deep);
                        ui.painter().rect_stroke(preview_rect, 4.0,
                            egui::Stroke::new(0.5, theme.border));

                        let frame_w = preview_w as u32;
                        let frame_h = preview_h as u32;

                        // Title bitmap is rendered + uploaded by the
                        // App layer (refresh_letter_picker_texture)
                        // BEFORE this code runs — texture lifecycle
                        // can't be managed from the inspector
                        // because dropping a TextureHandle inline
                        // trips wgpu validation. We just read the
                        // cached handle. Falls back to a placeholder
                        // text when the texture isn't ready yet
                        // (e.g. first paint after selection).
                        if let Some(tex) = state.letter_picker_texture.as_ref() {
                            ui.painter().image(
                                tex.id(),
                                preview_rect.shrink(2.0),
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                egui::Color32::WHITE,
                            );
                        } else {
                            ui.painter().text(
                                preview_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "rendering…",
                                egui::FontId::proportional(11.0),
                                theme.text_dim,
                            );
                        }

                        // Glyph positions — refreshed via the
                        // shared cache helper on hash mismatch.
                        // RefCell borrow is scoped to the helper
                        // call; the .to_vec() decouples the result
                        // from the borrow before any other state
                        // access in this block. Renderer is local
                        // to this lookup; the engine-side title
                        // renderer isn't reachable from here, but
                        // computing positions is far cheaper than
                        // rasterising and the cache pins the cost
                        // to "once per layout-hash change".
                        let positions: Vec<klipp_title::CharPosition> = {
                            let Some(td) = title_clip.title_data() else {
                                // Caller already gated on title_data().is_some()
                                // above; this is defence-in-depth.
                                return;
                            };
                            let mut tr_local = klipp_title::TitleRenderer::new();
                            let mut cache = state.glyph_position_cache.borrow_mut();
                            klipp_title::ensure_glyph_positions(
                                &mut cache, title_clip.id, &mut tr_local, td, frame_w, frame_h,
                            ).to_vec()
                        };

                        // Hover + snapped state derived from the
                        // pointer + persisted scratch. snapped_index
                        // is committed only on click (hover alone
                        // doesn't persist).
                        let pointer = preview_resp.interact_pointer_pos()
                            .or_else(|| ui.input(|i| i.pointer.hover_pos()))
                            .filter(|p| preview_rect.contains(*p));
                        let proj = |fx: f32, fy: f32| {
                            crate::panels::viewer::viewer_frame_px_to_screen(
                                preview_rect.shrink(2.0),
                                frame_w as f32, frame_h as f32, fx, fy,
                            )
                        };
                        let hover_index = pointer.and_then(|p| {
                            match crate::inspector::letter_picker::hit_test_glyphs(&positions, p, proj) {
                                crate::inspector::letter_picker::HitTest::InsideGlyph { char_index }
                                | crate::inspector::letter_picker::HitTest::NearGlyph { char_index } => Some(char_index),
                                crate::inspector::letter_picker::HitTest::FreePosition => None,
                            }
                        });

                        // Snapped (persistent) — stored in scratch as
                        // an Option<usize>. We have no field for it
                        // on HyperzoomScratch yet; use egui Memory
                        // keyed by the clip-id so picker state is
                        // per-source-clip and survives across paints.
                        let snap_id = egui::Id::new(("hyperzoom_picker_snapped", clip_id));
                        let mut snapped_index: Option<usize> = ui.ctx().data_mut(|d|
                            d.get_temp(snap_id).unwrap_or(None)
                        );

                        // Click-to-commit: if a click landed on a
                        // glyph, snap focal_x/y to its centroid +
                        // record the char_index.
                        if preview_resp.clicked() || preview_resp.dragged() {
                            if let Some(p) = pointer {
                                match crate::inspector::letter_picker::hit_test_glyphs(&positions, p, proj) {
                                    crate::inspector::letter_picker::HitTest::InsideGlyph { char_index }
                                    | crate::inspector::letter_picker::HitTest::NearGlyph { char_index } => {
                                        if let Some(cp) = positions.iter().find(|cp| cp.char_index == char_index) {
                                            let (nx, ny) = crate::inspector::letter_picker::glyph_centroid_to_focal(cp, frame_w, frame_h);
                                            focal_x = nx;
                                            focal_y = ny;
                                            snapped_index = Some(char_index);
                                        }
                                    }
                                    crate::inspector::letter_picker::HitTest::FreePosition => {
                                        // Drag past glyphs — free-
                                        // position the focal to the
                                        // pointer, drop snap state.
                                        let (nx, ny) = crate::panels::viewer::viewer_screen_to_norm(preview_rect.shrink(2.0), p);
                                        focal_x = nx.clamp(0.0, 1.0);
                                        focal_y = ny.clamp(0.0, 1.0);
                                        snapped_index = None;
                                    }
                                }
                            }
                        }

                        // Persist snap state for next paint.
                        ui.ctx().data_mut(|d| d.insert_temp(snap_id, snapped_index));

                        // Paint hit-rect overlays + persistent
                        // snapped indicator. Goes ON TOP of the
                        // bitmap so glyph outlines remain visible.
                        crate::inspector::letter_picker::paint_glyph_overlays(
                            ui,
                            preview_rect.shrink(2.0),
                            &positions,
                            frame_w, frame_h,
                            hover_index, snapped_index, theme.accent,
                        );

                        // Crosshair at current focal location —
                        // mirrors the main-viewer focal-handle so
                        // the user sees both their click target
                        // and the resulting focal point.
                        let centre = crate::panels::viewer::viewer_norm_to_screen(
                            preview_rect.shrink(2.0), focal_x, focal_y,
                        );
                        ui.painter().circle_stroke(centre, 5.0,
                            egui::Stroke::new(1.5, theme.accent));
                        ui.painter().line_segment(
                            [egui::pos2(centre.x - 8.0, centre.y), egui::pos2(centre.x + 8.0, centre.y)],
                            egui::Stroke::new(1.0, theme.accent));
                        ui.painter().line_segment(
                            [egui::pos2(centre.x, centre.y - 8.0), egui::pos2(centre.x, centre.y + 8.0)],
                            egui::Stroke::new(1.0, theme.accent));
                    }
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Duration + peak scale ──
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Duration").size(11.0).color(theme.text_dim));
                    ui.add_space(8.0);
                    ui.add(
                        egui::DragValue::new(&mut duration_secs)
                            .range(0.05_f64..=10.0)
                            .speed(0.05)
                            .suffix(" s")
                            .fixed_decimals(2),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Peak scale").size(11.0).color(theme.text_dim));
                    ui.add_space(8.0);
                    ui.add(
                        egui::DragValue::new(&mut max_scale)
                            .range(1.5_f32..=200.0)
                            .speed(0.5)
                            .suffix(" ×")
                            .fixed_decimals(1),
                    );
                });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Easing ──
                // Linear is the back-compat default. Bezier emits a
                // symmetric S-curve via `±duration/3` horizontal
                // tangents — visually a slow-start/fast-middle/slow-
                // end ramp on each side of the cut.
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Easing").size(11.0).color(theme.text_dim));
                    ui.add_space(8.0);
                    let label = match easing {
                        crate::hyperzoom::HyperzoomEasing::Linear => "Linear",
                        crate::hyperzoom::HyperzoomEasing::Bezier => "Bezier (ease-in/out)",
                    };
                    egui::ComboBox::from_id_salt(("video:hyperzoom_easing", clip_id))
                        .selected_text(label)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut easing,
                                crate::hyperzoom::HyperzoomEasing::Linear,
                                "Linear",
                            ).on_hover_text("Constant velocity. Mechanical feel; matches what Apply Hyperzoom emitted before the easing option existed.");
                            ui.selectable_value(
                                &mut easing,
                                crate::hyperzoom::HyperzoomEasing::Bezier,
                                "Bezier (ease-in/out)",
                            ).on_hover_text("S-curve: slow start, fast middle, slow finish. Reads as the cinematic 'rocket' feel.");
                        });
                });

                // ── Hold at peak ──
                // Adds a flat-at-peak segment straddling the cut.
                // Source's last `hold_secs` and target's first
                // `hold_secs` both sit at peak — perceived freeze is
                // 2× this value. 0 (default) reproduces the original
                // two-keyframe ramp.
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Hold at peak").size(11.0).color(theme.text_dim));
                    ui.add_space(8.0);
                    ui.add(
                        egui::DragValue::new(&mut hold_secs)
                            .range(0.0_f64..=1.0)
                            .speed(0.01)
                            .suffix(" s")
                            .fixed_decimals(2),
                    ).on_hover_text(
                        "Frames to hold at peak across the cut. Source's tail and target's head both \
                         sit at peak for this long; total perceived freeze is twice this value.",
                    );
                });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Radial blur (opt-in convenience) ──
                // Off by default. When toggled on, Apply Hyperzoom
                // additionally inserts a `RadialBlur` builtin on
                // source + target with center bound to focal X/Y
                // and amount keyframed 0 → peak → 0 across the same
                // ramp window as the scale animation.
                ui.horizontal(|ui| {
                    ui.checkbox(&mut add_radial_blur, "Add radial blur on apply")
                        .on_hover_text(
                            "Inserts a zoom-mode RadialBlur builtin on the source and target \
                             clips, with center pinned to the focal point and amount ramped \
                             0 → peak → 0 over the hyperzoom window.",
                        );
                });
                if add_radial_blur {
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.label(egui::RichText::new("Amount").size(11.0).color(theme.text_dim));
                        ui.add_space(8.0);
                        ui.add(
                            egui::DragValue::new(&mut radial_blur_amount)
                                .range(0.0_f32..=50.0)
                                .speed(0.5)
                                .fixed_decimals(1),
                        ).on_hover_text(
                            "Peak `RadialBlur::amount` value. 5 = subtle; 15 = the standard \
                             zoom-blur preset; 30+ is stylised. Identical to the standalone \
                             RadialBlur effect's `amount` field — the keyframes just animate \
                             0 → here → 0 across the ramp.",
                        );
                    });
                }

                ui.add_space(8.0);

                // ── Apply + Link beneath ──
                // Two-button row. "Apply hyperzoom" writes the
                // keyframes on source + target. "Link clips beneath"
                // is the optional follow-up that auto-applies
                // transform_link to clips on lower tracks during the
                // ramp window so the underlying shot flies into the
                // focal point alongside the source.
                ui.horizontal(|ui| {
                    let apply_label = "Apply hyperzoom";
                    if ui.add_sized(
                        egui::vec2(150.0, 26.0),
                        egui::Button::new(
                            egui::RichText::new(apply_label)
                                .size(11.0)
                                .color(theme.text_on_accent),
                        )
                        .fill(theme.accent)
                        .rounding(6.0),
                    )
                    .on_hover_text(
                        "Writes anchor + scale keyframes on this clip and the chosen target. \
                         Re-applying replaces the previous ramp; tweak in the keyframe editor afterwards.",
                    )
                    .clicked()
                    {
                        let target_clip_id = match target_choice {
                            HyperzoomTargetChoice::Auto => None,
                            HyperzoomTargetChoice::Explicit(id) => Some(id),
                        };
                        cmds.push(AppCommand::ApplyHyperzoom {
                            source_clip_id: clip_id,
                            target_clip_id,
                            focal_x_norm: focal_x,
                            focal_y_norm: focal_y,
                            duration_secs,
                            max_scale,
                            easing: easing.as_u8(),
                            hold_secs,
                            add_radial_blur,
                            radial_blur_amount,
                        });
                    }

                    ui.add_space(6.0);

                    if ui.add_sized(
                        egui::vec2(150.0, 26.0),
                        egui::Button::new(
                            egui::RichText::new("Link clips beneath")
                                .size(11.0)
                                .color(theme.text_primary),
                        )
                        .fill(theme.bg_raised)
                        .stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(6.0),
                    )
                    .on_hover_text(
                        "Auto-set transform_link on every clip beneath this one within the \
                         ramp window so they fly with the source. Skips clips that already \
                         have a link of their own (your manual parenting choices survive).",
                    )
                    .clicked()
                    {
                        cmds.push(AppCommand::AutoLinkClipsBeneathSource {
                            source_clip_id: clip_id,
                            ramp_secs: duration_secs,
                            // Canonical "fly with parent" defaults.
                            // The user can refine each clip's link in
                            // its own TRANSFORM LINK section afterwards
                            // (mode, per-axis flags) — running
                            // auto-link again is a no-op on already-
                            // linked clips, so the manual tweaks
                            // don't get clobbered.
                            mode: 0, // Absolute
                            inherit_scale: true,
                            inherit_position: true,
                            inherit_rotation: true,
                            inherit_anchor: true,
                        });
                    }
                });
            });
        ui.add_space(8.0);
    });

    // Persist edited spinbox values across frames. Goes through the
    // shared helper so the viewer crosshair sees the same cell.
    crate::hyperzoom::update_hyperzoom_scratch(ui.ctx(), clip_id, |s| {
        s.focal_x = focal_x;
        s.focal_y = focal_y;
        s.duration_secs = duration_secs;
        s.max_scale = max_scale;
        s.target_choice = target_choice;
        s.easing = easing;
        s.hold_secs = hold_secs;
        s.add_radial_blur = add_radial_blur;
        s.radial_blur_amount = radial_blur_amount;
    });
}

// HyperzoomScratch + HyperzoomTargetChoice live in `crate::hyperzoom`
// so the viewer's focal-point gizmo can read/write the same state
// the inspector spinboxes use. Re-import for in-file shorthand.
use crate::hyperzoom::HyperzoomTargetChoice;

/// Local cycle predicate for the inspector's candidate filter. Mirrors
/// `transform_link_chain_creates_cycle` in `app.rs` (which the
/// `SetClipTransformLink` handler uses for hard validation). Lives here
/// so the inspector module doesn't need to reach into a sibling file's
/// private free functions; both walkers share the same 64-link depth
/// cap so they reject the same set of bindings.
fn transform_link_chain_creates_cycle_local(
    seq: &klipp_timeline::Sequence,
    consumer: klipp_core::ClipId,
    target: klipp_core::ClipId,
) -> bool {
    if consumer == target {
        return true;
    }
    let mut current = target;
    for _ in 0..64 {
        let next = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == current)
            .and_then(|c| c.transform_link.as_ref().map(|tl| tl.target));
        match next {
            None => return false,
            Some(n) if n == consumer => return true,
            Some(n) => current = n,
        }
    }
    true
}

/// Mirror of the `LinkMode` discriminant order documented on the
/// `SetClipTransformLink` AppCommand — keep the inspector and the
/// AppCommand wire format in sync without exposing a public conversion
/// surface in `klipp-core`.
fn link_mode_to_u8(mode: klipp_timeline::LinkMode) -> u8 {
    match mode {
        klipp_timeline::LinkMode::Absolute => 0,
        klipp_timeline::LinkMode::Additive => 1,
    }
}

/// Mirror of `matte_channel_from_u8` in `app.rs`. Round-trip helper so
/// the inspector can rebuild the AppCommand payload from the runtime
/// `MatteBinding` (which stores the enum) without a public conversion
/// surface on `klipp-core`. Variant order matches the AppCommand wire
/// contract documented on `SetClipMatte`.
fn matte_channel_to_u8(ch: klipp_nodes::params::MatteChannel) -> u8 {
    use klipp_nodes::params::MatteChannel as M;
    match ch {
        M::Alpha => 0,
        M::Luminance => 1,
        M::Red => 2,
        M::Green => 3,
        M::Blue => 4,
    }
}

/// Format the first 6 hex characters of a clip-id as a short label.
/// The full UUID is unreadable in a dropdown; the short form is enough
/// to disambiguate at human scale (and the track label disambiguates
/// the rest). Future work surfaces actual clip names where available.
fn short_clip_id(id: &klipp_core::ClipId) -> String {
    let s = id.0.to_string();
    s.chars().take(6).collect()
}

fn show_ken_burns_section(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, cmds: &mut Vec<AppCommand>) {
    let clip_id = clip.id;
    let pad = 14.0;
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
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Scale 1.0 to 1.3, centered").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.0, end_scale: 1.3,
                            start_pos_x: 0.0, start_pos_y: 0.0, end_pos_x: 0.0, end_pos_y: 0.0,
                        });
                    }
                    if ui.add(egui::Button::new(btn_text("Zoom Out"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Scale 1.3 to 1.0, centered").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.3, end_scale: 1.0,
                            start_pos_x: 0.0, start_pos_y: 0.0, end_pos_x: 0.0, end_pos_y: 0.0,
                        });
                    }
                    if ui.add(egui::Button::new(btn_text("Pan L>R"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Pan left to right, 1.2\u{00d7} zoom").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.2, end_scale: 1.2,
                            start_pos_x: -100.0, start_pos_y: 0.0, end_pos_x: 100.0, end_pos_y: 0.0,
                        });
                    }
                    if ui.add(egui::Button::new(btn_text("Pan R>L"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Pan right to left, 1.2\u{00d7} zoom").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.2, end_scale: 1.2,
                            start_pos_x: 100.0, start_pos_y: 0.0, end_pos_x: -100.0, end_pos_y: 0.0,
                        });
                    }
                    if has_transform_kfs && ui.add(egui::Button::new(btn_text("Remove"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(3.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Clear all transform keyframes").clicked() {
                        cmds.push(AppCommand::ClearTransformKeyframes { clip_id });
                    }
                });
            });
    });
}

fn show_speed_section(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, cmds: &mut Vec<AppCommand>) {
    let clip_id = clip.id;
    let pad = 14.0;
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

                // Direction row — toggle between Forward and Reverse by flipping
                // the sign of clip.speed. The engine does not yet decode
                // backwards; this stores the user's intent for a follow-up.
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Direction")
                            .size(11.0)
                            .color(theme.text_secondary),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let is_reversed = clip.speed < 0.0;
                        let label = if is_reversed { "⇄ Reverse" } else { "⇄ Forward" };
                        if ui
                            .selectable_label(is_reversed, egui::RichText::new(label).size(11.0))
                            .on_hover_text("Toggle reverse playback (negates clip speed)")
                            .clicked()
                        {
                            let abs = clip.speed.abs().max(0.1);
                            let new_speed = if is_reversed { abs } else { -abs };
                            cmds.push(AppCommand::SetClipSpeed { clip_id, speed: new_speed });
                        }
                    });
                });

                // Separator
                ui.add_space(4.0);
                let sep_w = ui.available_width();
                let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(sep_w, 1.0), egui::Sense::hover());
                ui.painter().hline(sep_rect.x_range(), sep_rect.center().y, egui::Stroke::new(0.5_f32, theme.border));
                ui.add_space(4.0);

                // Speed preset buttons
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
                    let btn_text = |label: &str| egui::RichText::new(label).size(10.5);
                    for (label, speed_val) in [("25%", 0.25), ("50%", 0.5), ("100%", 1.0), ("200%", 2.0), ("400%", 4.0)] {
                        let is_active = (clip.speed - speed_val).abs() < 0.001;
                        let fill = if is_active { theme.accent.linear_multiply(0.25) } else { theme.bg_raised };
                        let stroke = if is_active {
                            egui::Stroke::new(1.0_f32, theme.accent)
                        } else {
                            egui::Stroke::new(0.5_f32, theme.border)
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
}

fn show_behaviors_section(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, cmds: &mut Vec<AppCommand>) {
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
}

fn show_role_section(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, cmds: &mut Vec<AppCommand>) {
    let clip_id = clip.id;
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
pub fn emit_opacity(cmds: &mut Vec<AppCommand>, clip: &Clip, clip_id: klipp_core::ClipId, value: f32) {
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
            let badge_color = theme.marker_info;
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
            theme.marker_success
        } else {
            theme.marker_error
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
            let color = if state.use_proxies { theme.marker_success } else { theme.text_primary };
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
            ui.painter().rect_stroke(swatch_rect, 2.0, egui::Stroke::new(0.5_f32, theme.border_light));
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
            .and_then(disk_free_bytes)
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
            theme.marker_error // red if < 1GB
        } else if free_bytes < 10_000_000_000 {
            theme.accent // amber if < 10GB
        } else {
            theme.marker_success // green
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
        ui.painter().rect_filled(full_bar, 3.0, theme.bg_hover);
        // Project portion (blue)
        let pw = (bar_w * project_frac).max(if project_bytes > 0 { 2.0 } else { 0.0 });
        if pw > 0.0 {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::pos2(bx, by), egui::vec2(pw, bar_h)),
                egui::Rounding { nw: 3.0, ne: 0.0, sw: 3.0, se: 0.0 },
                theme.marker_info,
            );
        }
        // Cache portion (orange)
        let cw = (bar_w * cache_frac).max(if cache_bytes > 0 { 2.0 } else { 0.0 });
        if cw > 0.0 {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::pos2(bx + pw, by), egui::vec2(cw, bar_h)),
                0.0,
                theme.accent,
            );
        }

        // Legend
        ui.add_space(2.0);
        let (legend_rect, _) = ui.allocate_exact_size(egui::vec2(w, 16.0), egui::Sense::hover());
        let ly = legend_rect.center().y;
        let mut lx = legend_rect.min.x + pad;
        let dot_r = 4.0;
        let legend_items = [
            ("Project", theme.marker_info),
            ("Cache", theme.accent),
            ("Free", theme.bg_hover),
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
        let frame = egui::Frame::none()
            .fill(theme.bg_panel)
            .inner_margin(egui::Margin { left: 0.0, right: 0.0, top: 2.0, bottom: 0.0 });
        frame.show(ui, |ui| { add_contents(ui); });
    }

    col_state.store(ui.ctx());
}

fn info_row(
    ui: &mut egui::Ui, theme: &KlippTheme, pad: f32, row_h: f32, total_w: f32,
    label: &str, draw_value: impl FnOnce(&mut egui::Ui, f32, f32),
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total_w, row_h), egui::Sense::hover());
    ui.painter().hline(rect.min.x + pad..=rect.max.x - pad, rect.max.y, egui::Stroke::new(0.5_f32, theme.border));
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
        ui.painter().add(egui::Shape::convex_polygon(pts, egui::Color32::TRANSPARENT, egui::Stroke::new(1.5_f32, color)));
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
            Some(stat.f_bavail * stat.f_frsize)
        } else {
            None
        }
    }
}
