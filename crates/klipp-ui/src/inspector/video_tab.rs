use klipp_timeline::GradeAccess;
use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::role_pill;
use crate::widgets::scrubby_param::KeyframeState;
use super::rows::{param_row, switch};
use klipp_core::{AppCommand, ClipRole, Timecode, VideoParam};
use klipp_effects::BuiltinEffect;
use klipp_timeline::Clip;


/// A placement or opacity lane's keys, as the clip's picture holds them
/// (M4 step 12c): what the card's rows, diamonds and carets read.
fn lane(clip: &Clip, key: &str) -> Vec<klipp_core::Keyframe> {
    crate::inspector::placement_by_path::lane_keys(clip, key)
}

/// Extracts transform params from a clip's builtin effects.
/// Returns identity defaults if no Transform effect exists.
pub fn get_transform(clip: &Clip) -> TransformParams {
    clip.builtin_effects()
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
                rotation_x,
                rotation_y,
                perspective,
                ..
            } => Some(TransformParams {
                position_x: *position_x,
                position_y: *position_y,
                scale_x: *scale_x,
                scale_y: *scale_y,
                rotation: *rotation,
                anchor_x: *anchor_x,
                anchor_y: *anchor_y,
                rotation_x: *rotation_x,
                rotation_y: *rotation_y,
                perspective: *perspective,
            }),
            _ => None,
        })
        .unwrap_or_default()
}

/// Extracts the index of the Transform effect in builtin_effects, if any.
pub fn find_transform_index(clip: &Clip) -> Option<usize> {
    clip.builtin_effects().iter().position(|e| matches!(e, BuiltinEffect::Transform { .. }))
}

/// Extracts opacity value from a clip's builtin effects.
/// Returns 1.0 (fully opaque) if no Opacity effect exists.
pub fn get_opacity(clip: &Clip) -> f32 {
    clip.builtin_effects()
        .iter()
        .find_map(|e| match e {
            BuiltinEffect::Opacity { value } => Some(*value),
            _ => None,
        })
        .unwrap_or(1.0)
}

/// Extracts the index of the Opacity effect in builtin_effects, if any.
pub fn find_opacity_index(clip: &Clip) -> Option<usize> {
    clip.builtin_effects().iter().position(|e| matches!(e, BuiltinEffect::Opacity { .. }))
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
            anchor_y: self.anchor_y,
            // Written back as read: editing position must not flatten a tilt.
            rotation_x: self.rotation_x,
            rotation_y: self.rotation_y,
            perspective: self.perspective,
            focal_ref: None }
    }
}

/// Compute KeyframeState for a given keyframe vec at a given clip-relative time.
fn keyframe_state(keyframes: &[klipp_core::Keyframe], clip_time_secs: f64, keyframe_tolerance: f64) -> KeyframeState {
    crate::inspector::keyframe_selection::state(keyframes, clip_time_secs, keyframe_tolerance)
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
        Some((clip, clip_time_secs, is_timeline)) => {
            let start = cmds.len();
            // The clip's placement and opacity are its properties, read from
            // its picture and set by path (M3 step 13); a timeline's
            // stand-in keeps the effect-list commands.
            let by_path = !is_timeline;
            show_clip_video(ui, theme, state, clip, cmds, clip_time_secs, is_timeline, by_path);
            if by_path {
                let mine = cmds.split_off(start);
                cmds.extend(super::placement_by_path::by_path(mine, clip));
            }
        }
        None => {
            if let Some(media) = bin_media {
                show_bin_media_video(ui, theme, state, media, cmds);
            } else {
                super::sequence_view::show(ui, theme, state, cmds);
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
    // Construct a temporary default Clip from the media item
    let temp_clip = Clip {
        id: klipp_core::ClipId::new(),
        kind: klipp_timeline::ClipKind::Media { media_id: media.id },
        timeline_in: Timecode { value: 0, rate: media.duration.rate },
        timeline_out: media.duration,
        source_in: Timecode { value: 0, rate: media.duration.rate },
        source_out: media.duration,
        picture: Default::default(),
        linked_clip_id: None,
        linked_title_asset_id: None,
        group_id: None,
        grade_group: None,
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
        matte_feather_keyframes: Vec::new(),
        matte_choke_keyframes: Vec::new(),
        clip_markers: Vec::new(),
        frame_glide: None,
        role: klipp_core::ClipRole::default(),
        role_source: klipp_core::RoleSource::Unset,
        matched_to: None,
        speed: 1.0,
        speed_keyframes: Vec::new(),
        keep_pitch: false,
        source_rotation: 0,
        enabled: true,
        solo: false,
        blend_mode: klipp_core::BlendMode::Normal,
        depth: 1.0,
        matte_source: None,
        matte_stencil: None,
        consumed_as_matte: false,
        transform_link: None,
        multicam_group: None,
        multicam_angle: 0,
        multicam_cuts: Vec::new(),
        multicam_sound: Default::default(),
        audio_effects: Vec::new(),
        audio_effect_keyframes: std::collections::HashMap::new(),
        track_points: Vec::new(),
        surfaces: Vec::new(),
        surface_pin: None,
        name: None,
        colour_label: 0,
        grade_flag: Default::default(),
        precomp: None,
        baked_own: None,
    };
    // is_timeline = false → no Speed Ramp, no amber banner
    show_clip_video(ui, theme, state, &temp_clip, cmds, 0.0, false, false);
}

fn show_clip_video(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
    clip_time_secs: f64,
    _is_timeline: bool,
    by_path: bool,
) {
    let keyframe_tolerance = crate::inspector::keyframe_selection::frame_tolerance(state.sequence());
    let clip_id = clip.id;
    let clip_timeline_in_secs = clip.timeline_in.to_secs_f64();
    let clip_rate = clip.timeline_in.rate;
    let kf_tolerance = keyframe_tolerance;
    let pad = 14.0;

    // ── ROLE ──
    show_role_section(ui, theme, clip, cmds);

    ui.add_space(8.0);

    // ── TRANSFORM ──
    // Read current transform from clip's builtin effects (immutable);
    // behind the switch, from the clip's placement and opacity as its
    // picture holds them, which the card's edits are compared against
    // (M3 step 13).
    let transform = if by_path { super::placement_by_path::placement(clip) } else { get_transform(clip) };
    let base_opacity = if by_path { super::placement_by_path::opacity(clip) } else { get_opacity(clip) };
    // By path a drag is the placement node's, whatever entry holds it.
    let transform_index = find_transform_index(clip).or(by_path.then_some(0));
    let opacity_index = find_opacity_index(clip).or(by_path.then_some(0));
    let transform_drag_key = format!("{:?}:video_transform", clip_id);

    // Keyframed values at playhead
    let mut pos_x = crate::inspector::keyframe_selection::editable_value(&lane(clip, "pos_x"), clip_time_secs, transform.position_x, keyframe_tolerance);
    let mut pos_y = crate::inspector::keyframe_selection::editable_value(&lane(clip, "pos_y"), clip_time_secs, transform.position_y, keyframe_tolerance);
    let mut scale_val = crate::inspector::keyframe_selection::editable_value(&lane(clip, "scale"), clip_time_secs, transform.scale_x, keyframe_tolerance);
    let mut rotation = crate::inspector::keyframe_selection::editable_value(&lane(clip, "rotation"), clip_time_secs, transform.rotation, keyframe_tolerance);

    // Keyframe states
    let pos_x_ks = keyframe_state(&lane(clip, "pos_x"), clip_time_secs, keyframe_tolerance);
    let pos_y_ks = keyframe_state(&lane(clip, "pos_y"), clip_time_secs, keyframe_tolerance);
    let scale_ks = keyframe_state(&lane(clip, "scale"), clip_time_secs, keyframe_tolerance);
    let rot_ks = keyframe_state(&lane(clip, "rotation"), clip_time_secs, keyframe_tolerance);
    // The height: on a lane of its own where it has keys (#1471), else the
    // width at the clip's aspect. Its row shows where the clip scales
    // unevenly or its scale is keyed, so a height can be keyed apart.
    let mut scale_y_val = crate::inspector::keyframe_selection::editable_value(&lane(clip, "scale_y"), clip_time_secs, keep_aspect(&transform, scale_val), keyframe_tolerance);
    let scale_y_ks = keyframe_state(&lane(clip, "scale_y"), clip_time_secs, keyframe_tolerance);
    let shows_scale_y = transform.scale_y != transform.scale_x || !lane(clip, "scale").is_empty() || !lane(clip, "scale_y").is_empty();
    let mut tilt_x = crate::inspector::keyframe_selection::editable_value(&lane(clip, "tilt_x"), clip_time_secs, transform.rotation_x, keyframe_tolerance);
    let mut tilt_y = crate::inspector::keyframe_selection::editable_value(&lane(clip, "tilt_y"), clip_time_secs, transform.rotation_y, keyframe_tolerance);
    let tilt_x_ks = keyframe_state(&lane(clip, "tilt_x"), clip_time_secs, keyframe_tolerance);
    let tilt_y_ks = keyframe_state(&lane(clip, "tilt_y"), clip_time_secs, keyframe_tolerance);
    // A tilt is drawn in moderate perspective when none is set (see the
    // renderer's `tilt`), so that is what the row shows.
    let mut persp = if transform.perspective > 0.0 { transform.perspective } else { 1.0 };

    // Opacity
    let opacity_drag_key = format!("{:?}:video_opacity", clip_id);
    let mut opacity = crate::inspector::keyframe_selection::editable_value(&lane(clip, "opacity"), clip_time_secs, base_opacity, keyframe_tolerance);
    let opacity_ks = keyframe_state(&lane(clip, "opacity"), clip_time_secs, keyframe_tolerance);

    // We need to collect scrubby results inside the card, then handle commands after
    // Use a struct to shuttle results out of the closure
    use crate::widgets::scrubby_param::ScrubbyParamResponse;
    struct TransformResults {
        r_px: ScrubbyParamResponse,
        r_py: ScrubbyParamResponse,
        r_sc: ScrubbyParamResponse,
        r_sy: ScrubbyParamResponse,
        r_ro: ScrubbyParamResponse,
        r_tx: ScrubbyParamResponse,
        r_ty: ScrubbyParamResponse,
        r_pe: ScrubbyParamResponse,
        r_op: ScrubbyParamResponse,
        pos_x: f32,
        pos_y: f32,
        scale_val: f32,
        scale_y_val: f32,
        rotation: f32,
        tilt_x: f32,
        tilt_y: f32,
        persp: f32,
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
        let mut r_sy_out: Option<ScrubbyParamResponse> = None;
        let mut r_ro_out: Option<ScrubbyParamResponse> = None;
        let mut r_tx_out: Option<ScrubbyParamResponse> = None;
        let mut r_ty_out: Option<ScrubbyParamResponse> = None;
        let mut r_pe_out: Option<ScrubbyParamResponse> = None;
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

        let header_h = crate::widgets::inspector_group::SECTION_HEADER_H;
        let (header_rect, header_resp) = ui.allocate_exact_size(egui::vec2(w, header_h), egui::Sense::click());
        let is_open = col_state.is_open();
        crate::widgets::inspector_group::paint_section_header(ui, header_rect, title, is_open, None, header_resp.hovered(), theme);

        // Reset button in header (right side, before caret)
        let opacity_preview = base_opacity;
        let is_default = (transform.position_x.abs() < f32::EPSILON)
            && (transform.position_y.abs() < f32::EPSILON)
            && ((transform.scale_x - 1.0).abs() < f32::EPSILON)
            && ((transform.scale_y - 1.0).abs() < f32::EPSILON)
            && (transform.rotation.abs() < f32::EPSILON)
            && transform.rotation_x.abs() < f32::EPSILON
            && transform.rotation_y.abs() < f32::EPSILON
            && ((opacity_preview - 1.0).abs() < f32::EPSILON);
        if !is_default {
            let reset_rect = egui::Rect::from_center_size(
                egui::pos2(header_rect.max.x - 18.0, header_rect.center().y),
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

        if header_resp.clicked() {
            col_state.toggle(ui);
            let now_open = col_state.is_open();
            ui.ctx().data_mut(|d| {
                let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
                if now_open { set.remove(title); } else { set.insert(title.to_string()); }
            });
        }

        if col_state.is_open() {
            let frame = egui::Frame::none()
                .inner_margin(egui::Margin { left: pad, right: pad, top: 4.0, bottom: 4.0 });
            frame.show(ui, |ui| {
                r_px_out = Some(param_row(ui, theme, "Position X", &mut pos_x, -2000.0..=2000.0, 0.0, 1.0, 0, " px", Some(pos_x_ks)));
                r_py_out = Some(param_row(ui, theme, "Position Y", &mut pos_y, -2000.0..=2000.0, 0.0, 1.0, 0, " px", Some(pos_y_ks)));
                r_sc_out = Some(param_row(ui, theme, "Scale", &mut scale_val, 0.0..=4.0, 1.0, 0.01, 2, "\u{00d7}", Some(scale_ks)));
                if shows_scale_y {
                    r_sy_out = Some(param_row(ui, theme, "Scale Y", &mut scale_y_val, 0.0..=4.0, 1.0, 0.01, 2, "\u{00d7}", Some(scale_y_ks)));
                }
                r_ro_out = Some(param_row(ui, theme, "Rotation", &mut rotation, -360.0..=360.0, 0.0, 0.5, 1, " \u{00b0}", Some(rot_ks)));
                // Tilt out of the picture's plane: the top leans away for
                // positive X, the right edge swings away for positive Y.
                r_tx_out = Some(param_row(ui, theme, "Tilt X", &mut tilt_x, -80.0..=80.0, 0.0, 0.5, 1, " \u{00b0}", Some(tilt_x_ks)));
                r_ty_out = Some(param_row(ui, theme, "Tilt Y", &mut tilt_y, -80.0..=80.0, 0.0, 0.5, 1, " \u{00b0}", Some(tilt_y_ks)));
                if tilt_x != 0.0 || tilt_y != 0.0 || !lane(clip, "tilt_x").is_empty() || !lane(clip, "tilt_y").is_empty() {
                    r_pe_out = Some(param_row(ui, theme, "Perspective", &mut persp, 0.1..=3.0, 1.0, 0.01, 2, "", None));
                }

                // Source rotation (auto-detected from media metadata, user-overridable)
                if clip.source_rotation != 0 || clip.media_id().is_some() {
                    row(ui, theme, "Source rotation", |ui| {
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

                // How far back it sits for the sequence camera: 1 is the
                // composed plane, further back drifts less, 0 holds the clip
                // on the screen.
                let mut depth = clip.depth;
                let r = param_row(ui, theme, "Depth", &mut depth, 0.0..=20.0, 1.0, 0.01, 2, "", None);
                if r.changed {
                    cmds.push(AppCommand::SetClipDepth { clip_id, depth });
                }

                r_op_out = Some(param_row(ui, theme, "Opacity", &mut opacity, 0.0..=1.0, 1.0, 0.005, 2, "", Some(opacity_ks)));
            });
            // Behind the switch opacity is the clip's property, edited here
            // and not on a card, so its presets are offered here (M3 step 14).
            if by_path {
                opacity_presets(ui, theme, state, clip_id, opacity_index.unwrap_or(0), base_opacity, cmds);
            }

        }

        col_state.store(ui.ctx());

        // Default no-op results if section was collapsed
        TransformResults {
            r_px: r_px_out.unwrap_or_else(noop_response),
            r_py: r_py_out.unwrap_or_else(noop_response),
            r_sc: r_sc_out.unwrap_or_else(noop_response),
            r_sy: r_sy_out.unwrap_or_else(noop_response),
            r_ro: r_ro_out.unwrap_or_else(noop_response),
            r_tx: r_tx_out.unwrap_or_else(noop_response),
            r_ty: r_ty_out.unwrap_or_else(noop_response),
            r_pe: r_pe_out.unwrap_or_else(noop_response),
            r_op: r_op_out.unwrap_or_else(noop_response),
            pos_x,
            pos_y,
            scale_val,
            scale_y_val,
            rotation,
            tilt_x,
            tilt_y,
            persp,
            opacity,
            reset_clicked,
        }
    };

    let TransformResults { r_px, r_py, r_sc, r_sy, r_ro, r_tx, r_ty, r_pe, r_op, pos_x, pos_y, scale_val, scale_y_val, rotation, tilt_x, tilt_y, persp, opacity, reset_clicked } = results;
    let tilt_started = r_tx.drag_started || r_ty.drag_started || r_pe.drag_started;
    let tilt_changed = r_tx.changed || r_ty.changed || r_pe.changed;
    let tilt_stopped = r_tx.drag_stopped || r_ty.drag_stopped || r_pe.drag_stopped;

    // Reset button handling
    if reset_clicked {
        emit_transform(cmds, clip, clip_id, TransformParams::default());
        emit_opacity(cmds, clip, clip_id, 1.0);
    }

    let transform_started = r_px.drag_started || r_py.drag_started || r_sc.drag_started || r_sy.drag_started || r_ro.drag_started || tilt_started;
    let transform_changed = r_px.changed || r_py.changed || r_sc.changed || r_sy.changed || r_ro.changed || tilt_changed;
    let transform_stopped = r_px.drag_stopped || r_py.drag_stopped || r_sc.drag_stopped || r_sy.drag_stopped || r_ro.drag_stopped || tilt_stopped;
    let transform_added = egui::Id::new(&transform_drag_key).with("added_by_drag");
    let transform_escaped = transform_stopped
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if transform_escaped {
        let added = ui.data_mut(|d| d.remove_temp::<(usize, bool)>(transform_added));
        if let Some(index) = transform_index.or(added.map(|(index, _)| index)) {
            cmds.push(AppCommand::AbandonEffectDrag { clip_id, effect_index: index, added: added.is_some() });
        }
        ui.ctx().stop_dragging();
    }

    // The old view needs an effect before opening the gesture. The
    // generated view edits placement directly and already has an index.
    let mut transform_started_index = None;
    if transform_started {
        // A hidden row may miss release; never inherit its add marker.
        ui.data_mut(|d| d.remove_temp::<(usize, bool)>(transform_added));
        let index = transform_index.unwrap_or_else(|| {
            let index = clip.builtin_effects().len();
            cmds.push(AppCommand::AddBuiltinEffect { clip_id, effect: transform.to_builtin_effect() });
            ui.data_mut(|d| d.insert_temp(transform_added, (index, false)));
            transform_started_index = Some(index);
            index
        });
        cmds.push(AppCommand::BeginEffectDrag {
            key: transform_drag_key.clone(),
            clip_id,
            effect_index: index,
            old_effect: transform.to_builtin_effect(),
        });
        if r_px.drag_started { cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::PositionX)); }
        else if r_py.drag_started { cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::PositionY)); }
        else if r_sc.drag_started { cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::Scale)); }
        else if r_sy.drag_started { cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::ScaleY)); }
        else if r_ro.drag_started { cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::Rotation)); }
        else if r_tx.drag_started { cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::TiltX)); }
        else if r_ty.drag_started { cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::TiltY)); }
    }

    // Emit command for any changed transform value
    if transform_changed && !transform_escaped {
        ui.data_mut(|d| {
            if let Some((index, _)) = d.get_temp::<(usize, bool)>(transform_added) {
                d.insert_temp(transform_added, (index, true));
            }
        });
        let mut t = transform;
        t.position_x = pos_x;
        t.position_y = pos_y;
        t.scale_x = scale_val;
        // A height set apart from the width is written with the width the
        // row shows, so their aspect is the one shown (#1471).
        t.scale_y = if r_sy.changed { scale_y_val } else { keep_aspect(&transform, scale_val) };
        t.rotation = rotation;
        t.rotation_x = tilt_x;
        t.rotation_y = tilt_y;
        t.perspective = stored_perspective(persp);
        match transform_started_index {
            Some(index) => cmds.push(AppCommand::UpdateBuiltinEffect { clip_id, index, effect: t.to_builtin_effect() }),
            None => emit_transform(cmds, clip, clip_id, t),
        }
        if r_tx.changed && !lane(clip, "tilt_x").is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::TiltX, time_secs: clip_time_secs, value: tilt_x });
        }
        if r_ty.changed && !lane(clip, "tilt_y").is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::TiltY, time_secs: clip_time_secs, value: tilt_y });
        }

        if r_px.changed && !lane(clip, "pos_x").is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::PositionX, time_secs: clip_time_secs, value: pos_x });
        }
        if r_py.changed && !lane(clip, "pos_y").is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::PositionY, time_secs: clip_time_secs, value: pos_y });
        }
        if r_sc.changed && !lane(clip, "scale").is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::Scale, time_secs: clip_time_secs, value: scale_val });
        }
        if r_sy.changed && !lane(clip, "scale_y").is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::ScaleY, time_secs: clip_time_secs, value: scale_y_val });
        }
        if r_ro.changed && !lane(clip, "rotation").is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::Rotation, time_secs: clip_time_secs, value: rotation });
        }
    }

    let unmoved_transform_add = if transform_stopped && !transform_escaped {
        ui.data_mut(|d| d.remove_temp::<(usize, bool)>(transform_added))
            .filter(|(_, moved)| !moved)
            .map(|(index, _)| index)
    } else {
        None
    };
    if let Some(index) = unmoved_transform_add {
        cmds.push(AppCommand::AbandonEffectDrag { clip_id, effect_index: index, added: true });
    } else if transform_stopped && !transform_escaped {
        if let Some(idx) = transform_index.or(transform_started_index) {
            let mut t = transform;
            t.position_x = pos_x;
            t.position_y = pos_y;
            t.scale_x = scale_val;
            t.scale_y = keep_aspect(&transform, scale_val);
            t.rotation = rotation;
            t.rotation_x = tilt_x;
            t.rotation_y = tilt_y;
            t.perspective = stored_perspective(persp);
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
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(&lane(clip, "pos_x"), clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::PositionX, keyframe_index: ki, interpolation: interp });
        }
    }
    if let Some(preset) = r_px.keyframe_ease_preset {
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(&lane(clip, "pos_x"), clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::PositionX, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
            let kfs = lane(clip, "pos_x");
            for idx in [ki.saturating_sub(1), ki] {
                if idx >= kfs.len() { continue; }
                let (t_in, t_out) = preset.compute_tangents(&kfs, idx);
                cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: VideoParam::PositionX, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out });
            }
        }
    }
    if r_px.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(&lane(clip, "pos_x"), clip_time_secs, kf_tolerance).is_some() {
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
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(&lane(clip, "pos_y"), clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::PositionY, keyframe_index: ki, interpolation: interp });
        }
    }
    if let Some(preset) = r_py.keyframe_ease_preset {
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(&lane(clip, "pos_y"), clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::PositionY, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
            let kfs = lane(clip, "pos_y");
            for idx in [ki.saturating_sub(1), ki] {
                if idx >= kfs.len() { continue; }
                let (t_in, t_out) = preset.compute_tangents(&kfs, idx);
                cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: VideoParam::PositionY, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out });
            }
        }
    }
    if r_py.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(&lane(clip, "pos_y"), clip_time_secs, kf_tolerance).is_some() {
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
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(&lane(clip, "scale"), clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Scale, keyframe_index: ki, interpolation: interp });
        }
    }
    if let Some(preset) = r_sc.keyframe_ease_preset {
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(&lane(clip, "scale"), clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Scale, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
            let kfs = lane(clip, "scale");
            for idx in [ki.saturating_sub(1), ki] {
                if idx >= kfs.len() { continue; }
                let (t_in, t_out) = preset.compute_tangents(&kfs, idx);
                cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: VideoParam::Scale, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out });
            }
        }
    }
    if r_sc.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(&lane(clip, "scale"), clip_time_secs, kf_tolerance).is_some() {
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
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(&lane(clip, "rotation"), clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Rotation, keyframe_index: ki, interpolation: interp });
        }
    }
    if let Some(preset) = r_ro.keyframe_ease_preset {
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(&lane(clip, "rotation"), clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Rotation, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
            let kfs = lane(clip, "rotation");
            for idx in [ki.saturating_sub(1), ki] {
                if idx >= kfs.len() { continue; }
                let (t_in, t_out) = preset.compute_tangents(&kfs, idx);
                cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: VideoParam::Rotation, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out });
            }
        }
    }
    if r_ro.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(&lane(clip, "rotation"), clip_time_secs, kf_tolerance).is_some() {
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

    // ── Keyframe toggle/nav for Scale Y, Tilt X and Tilt Y ──
    let seek = (clip_timeline_in_secs, clip_rate);
    keyframe_controls(cmds, &r_sy, &lane(clip, "scale_y"), &scale_y_ks, VideoParam::ScaleY, scale_y_val, clip_id, clip_time_secs, seek, keyframe_tolerance);
    keyframe_controls(cmds, &r_tx, &lane(clip, "tilt_x"), &tilt_x_ks, VideoParam::TiltX, tilt_x, clip_id, clip_time_secs, seek, keyframe_tolerance);
    keyframe_controls(cmds, &r_ty, &lane(clip, "tilt_y"), &tilt_y_ks, VideoParam::TiltY, tilt_y, clip_id, clip_time_secs, seek, keyframe_tolerance);

    // ── Opacity keyframe/drag handling ──
    // Escape ends the drag (egui reports it as drag_stopped). It takes the
    // gesture back, rather than committing it as a release does: the
    // drag's snapshot restores the picture, keys included, with no undo
    // step, and the pointer moving on changes nothing (#1364, IC14).
    //
    // A clip with no Opacity effect in the old view (the generated view
    // drags the clip's own opacity, by path) has the effect added as the
    // drag starts, and the drag opened on it: Escape then takes the add
    // back too, and only an add this drag made (#1364).
    let added_by_drag = egui::Id::new(&opacity_drag_key).with("added_by_drag");
    let opacity_escaped = r_op.drag_stopped
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if opacity_escaped {
        let added = ui.data_mut(|d| d.remove_temp::<(usize, f32)>(added_by_drag)).map(|(index, _)| index);
        if let Some(idx) = opacity_index.or(added) {
            cmds.push(AppCommand::AbandonEffectDrag { clip_id, effect_index: idx, added: added.is_some() });
        }
        ui.ctx().stop_dragging();
    }
    // The effect this frame's drag added, not yet on `clip`.
    let mut started_index = None;
    if r_op.drag_started {
        // Every gesture starts clean: a gesture that ended without this
        // row seeing it (the row not drawn on the release) must not leave
        // its add to a later Escape, which would take an effect the user
        // kept. The key is the clip's, so no other clip reads it.
        ui.data_mut(|d| d.remove_temp::<(usize, f32)>(added_by_drag));
        let index = opacity_index.unwrap_or_else(|| {
            let index = clip.builtin_effects().len();
            cmds.push(AppCommand::AddBuiltinEffect { clip_id, effect: BuiltinEffect::Opacity { value: base_opacity } });
            // With the value it starts from: by the release the clip holds
            // the dragged value, so only this tells a drag from a click.
            ui.data_mut(|d| d.insert_temp(added_by_drag, (index, base_opacity)));
            started_index = Some(index);
            index
        });
        cmds.push(AppCommand::BeginEffectDrag {
            key: opacity_drag_key.clone(),
            clip_id,
            effect_index: index,
            old_effect: BuiltinEffect::Opacity { value: base_opacity },
        });
        cmds.push(AppCommand::SetVideoRubberBandParam(VideoParam::Opacity));
    }
    if r_op.changed && !opacity_escaped {
        match started_index {
            Some(index) => cmds.push(AppCommand::UpdateBuiltinEffect { clip_id, index, effect: BuiltinEffect::Opacity { value: opacity } }),
            None => emit_opacity(cmds, clip, clip_id, opacity),
        }
        if !lane(clip, "opacity").is_empty() {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: VideoParam::Opacity, time_secs: clip_time_secs, value: opacity });
        }
    }
    // A drag that added the effect and moved nothing (a press and a
    // release) takes the add back: nothing was asked for.
    let unmoved_add = if r_op.drag_stopped && !opacity_escaped {
        ui.data_mut(|d| d.remove_temp::<(usize, f32)>(added_by_drag))
            .filter(|(_, start)| (opacity - start).abs() < f32::EPSILON)
            .map(|(index, _)| index)
    } else {
        None
    };
    if let Some(idx) = unmoved_add {
        cmds.push(AppCommand::AbandonEffectDrag { clip_id, effect_index: idx, added: true });
    } else if r_op.drag_stopped && !opacity_escaped {
        if let Some(idx) = opacity_index.or(started_index) {
            cmds.push(AppCommand::EndEffectDrag {
                key: opacity_drag_key,
                clip_id,
                effect_index: idx,
                new_effect: BuiltinEffect::Opacity { value: opacity },
            });
        }
    }

    if let Some(interp) = r_op.keyframe_set_interpolation {
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(&lane(clip, "opacity"), clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Opacity, keyframe_index: ki, interpolation: interp });
        }
    }
    if let Some(preset) = r_op.keyframe_ease_preset {
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(&lane(clip, "opacity"), clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: VideoParam::Opacity, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
            let kfs = lane(clip, "opacity");
            for idx in [ki.saturating_sub(1), ki] {
                if idx >= kfs.len() { continue; }
                let (t_in, t_out) = preset.compute_tangents(&kfs, idx);
                cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: VideoParam::Opacity, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out });
            }
        }
    }
    if r_op.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(&lane(clip, "opacity"), clip_time_secs, kf_tolerance).is_some() {
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

    // ── FRAME GLIDE ──
    if matches!(clip.kind, klipp_timeline::ClipKind::Media { .. }) {
        show_frame_glide_section(ui, theme, state, clip, cmds);
        ui.add_space(8.0);
    }

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
                let mut toggled = false;
                row(ui, theme, "Speed ramp", |ui| {
                    if switch(ui, theme, has_ramp).clicked() {
                        ramp_enabled = !has_ramp;
                        toggled = true;
                    }
                });
                if toggled {
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
                                egui::FontId::proportional(10.0),
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
                                    egui::FontId::proportional(10.0),
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
                                    .size(10.0)
                                    .color(theme.text_dim),
                            );
                        });
                    });

                    ui.add_space(8.0);

                    // Preset ramp buttons
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
                        let clip_dur = clip.duration().to_secs_f64();
                        let btn = |label: &str| egui::RichText::new(label).size(10.0);

                        if ui.add(egui::Button::new(btn("Ramp Up"))
                            .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                            .rounding(2.0).min_size(egui::vec2(0.0, 20.0))
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
                            .rounding(2.0).min_size(egui::vec2(0.0, 20.0))
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
                            .rounding(2.0).min_size(egui::vec2(0.0, 20.0))
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
                            .rounding(2.0).min_size(egui::vec2(0.0, 20.0))
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
                            .rounding(2.0).min_size(egui::vec2(0.0, 20.0))
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

    // ── COLOR FADE ──
    // Per-clip fade-from / fade-to a target colour. When set, the
    // clip's first / last `duration_secs` mix from the picked colour
    // toward the source pixels (or vice-versa). Implemented as
    // BuiltinEffect::ColorFade entries on the clip's effects chain;
    // the engine patches each effect's `progress` per-frame from
    // clip-time + duration + mode (same pattern as Opacity).
    show_color_fade_section(ui, theme, clip, cmds);

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

                // Surfaces (#684): a flat thing in the shot followed in
                // perspective, for a picture to be pinned to.
                let footage = clip
                    .media_id()
                    .and_then(|m| state.media_pool.get(&m))
                    .is_some_and(|m| !m.is_still);
                let busy = state.tracking_in_progress.contains(&clip_id);
                let open = state.surface_tool.as_ref().is_some_and(|t| t.clip_id == clip_id);
                let track = ui
                    .add_enabled(footage && !busy && !open, egui::Button::new("Track Surface\u{2026}"))
                    .on_hover_text("Set four corners on a screen, a sign or a wall in the viewer, and follow it through the clip")
                    .on_disabled_hover_text(if !footage { "Only footage from a file can be tracked" } else if busy { "This clip is being tracked" } else { "The surface tool is open in the viewer" });
                if track.clicked() {
                    cmds.push(AppCommand::OpenSurfaceTool { clip_id, surface: None });
                }
                let fps = state.sequence().fps.as_f64();
                for surface in &clip.surfaces {
                    ui.horizontal(|ui| {
                        let span = surface.tracked_secs().map(|(a, b)| {
                            let on = |src: f64| clip.timeline_in.to_secs_f64() + clip.elapsed_for_source_secs(src, fps);
                            let (a, b) = (on(a), on(b));
                            format!("{:.1}\u{2013}{:.1} s", a.min(b), a.max(b))
                        });
                        ui.label(egui::RichText::new(&surface.name).size(11.0).color(theme.text_primary));
                        ui.label(egui::RichText::new(span.unwrap_or_else(|| "not tracked".into())).size(10.0).color(theme.text_dim));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("\u{2715}").on_hover_text("Remove the surface; what is pinned to it stays where it was").clicked() {
                                cmds.push(AppCommand::RemoveSurface { clip_id, surface: surface.id });
                            }
                            if ui
                                .add_enabled(!busy && !open, egui::Button::new("Fix here").small())
                                .on_hover_text("Put its corners right at the playhead and track it again from there")
                                .clicked()
                            {
                                cmds.push(AppCommand::OpenSurfaceTool { clip_id, surface: Some(surface.id) });
                            }
                        });
                    });
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

                        ui.label(egui::RichText::new(format!("{} fr", frame_count)).size(10.0).color(theme.text_dim));

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Delete button
                            if ui.add(egui::Button::new(egui::RichText::new("X").size(10.0).color(theme.text_dim)).frame(false)).clicked() {
                                remove_idx = Some(*idx);
                            }
                            // Analyze button
                            if !tracking_active
                                && ui.add(egui::Button::new(egui::RichText::new("Analyze").size(10.0)).frame(true)).clicked() {
                                    analyze_idx = Some(*idx);
                                }
                        });
                    });

                    // Show selected track point's link options
                    if is_selected {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);

                            ui.label(egui::RichText::new("Link to:").size(10.0).color(theme.text_dim));

                            if ui.add(egui::Button::new(egui::RichText::new("Transform Pos").size(10.0)).frame(true)).clicked() {
                                cmds.push(AppCommand::GenerateKeyframesFromTrack {
                                    clip_id,
                                    point_index: *idx,
                                    target: klipp_core::TrackTarget::TransformPosition,
                                });
                            }

                            // Mask center targets
                            let mask_targets: Vec<(uuid::Uuid, usize, String)> = clip.grade().nodes.iter()
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
                                if ui.add(egui::Button::new(egui::RichText::new(label).size(10.0)).frame(true)).clicked() {
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
    section_card(ui, "COLOUR", theme, |ui| {
        ui.add_space(6.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: 14.0, right: 14.0, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE)
            .show(ui, |ui| {
                // Check if any color grading is applied
                let has_lut = state.global_lut_path.is_some();
                let node_count = clip.grade().nodes.len();
                // Default graph has 12 nodes; extra nodes = user added grading
                let has_custom_nodes = node_count > 12;
                let grade_applied = has_lut || has_custom_nodes;
                let grade_count = if has_lut { 1 } else { 0 } + if has_custom_nodes { node_count - 12 } else { 0 };

                row(ui, theme, "Grade", |ui| {
                    if grade_applied {
                        ui.label(egui::RichText::new(format!("{grade_count} applied")).size(super::rows::TEXT).color(theme.text_primary));
                    } else {
                        ui.label(egui::RichText::new("None").size(super::rows::TEXT).color(theme.text_secondary));
                    }
                });
                ui.add_space(6.0);

                // Open Color Panel button with C shortcut badge
                ui.horizontal(|ui| {
                    let btn_w = ui.available_width() - 30.0;
                    if ui.add_sized(
                        egui::vec2(btn_w, 28.0),
                        egui::Button::new(
                            egui::RichText::new("Open Colour Panel")
                                .size(11.0)
                                .color(theme.text_primary),
                        )
                        .fill(theme.bg_raised)
                        .stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(4.0),
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
    let keyframe_tolerance = crate::inspector::keyframe_selection::frame_tolerance(state.sequence());
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
                row(ui, theme, "Blend Mode", |ui| {
                    let mut current = clip.blend_mode;
                    let combo = egui::ComboBox::from_id_salt(("video:blend_mode", clip_id))
                        .selected_text(current.display_name())
                        .width((ui.available_width() - 24.0).clamp(80.0, 160.0));
                    combo.show_ui(ui, |ui| {
                        for &mode in &klipp_core::BlendMode::ALL {
                            if ui.selectable_value(&mut current, mode, mode.display_name()).clicked()
                                && mode != clip.blend_mode {
                                    cmds.push(AppCommand::SetClipBlendMode { clip_id, mode });
                                }
                        }
                    });
                });

                ui.add_space(6.0);

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
                                .size(super::rows::TEXT)
                                .color(theme.text_secondary),
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

                ui.add_space(6.0);

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

                row(ui, theme, "Matte Source", |ui| {
                    let current_label = match clip.matte_source.as_ref() {
                        None => "(none)".to_string(),
                        Some(b) => candidates.iter()
                            .find(|(id, _)| *id == b.source_clip_id)
                            .map(|(_, l)| l.clone())
                            .unwrap_or_else(|| "(missing — see banner)".to_string()),
                    };
                    egui::ComboBox::from_id_salt(("video:matte_source", clip_id))
                        .selected_text(current_label)
                        .width((ui.available_width() - 24.0).clamp(80.0, 220.0))
                        .show_ui(ui, |ui| {
                            // "(none)" entry — clears any existing binding.
                            let none_selected = clip.matte_source.is_none();
                            if ui.selectable_label(none_selected, "(none)").clicked()
                                && !none_selected {
                                    cmds.push(AppCommand::ClearClipMatte { clip_id });
                                }
                            for (cand_id, label) in &candidates {
                                let selected = clip.matte_source.as_ref()
                                    .is_some_and(|b| b.source_clip_id == *cand_id);
                                if ui.selectable_label(selected, label).clicked()
                                    && !selected {
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
                                .size(11.0),
                        );
                    }

                    ui.add_space(4.0);

                    // ── Channel ──
                    row(ui, theme, "Channel", |ui| {
                        let mut current_ch = binding.channel;
                        for (ch, label) in [
                            (klipp_nodes::params::MatteChannel::Alpha, "Alpha"),
                            (klipp_nodes::params::MatteChannel::Luminance, "Luma"),
                            (klipp_nodes::params::MatteChannel::Red, "R"),
                            (klipp_nodes::params::MatteChannel::Green, "G"),
                            (klipp_nodes::params::MatteChannel::Blue, "B"),
                        ].into_iter().rev() {
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
                    let kf_tolerance_secs = keyframe_tolerance;
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Feather").size(super::rows::TEXT).color(theme.text_secondary));
                        ui.add_space(8.0);
                        let mut feather = crate::inspector::keyframe_selection::editable_value(
                            &clip.matte_feather_keyframes, clip_time_secs, binding.feather_px,
                            keyframe_tolerance);
                        if ui.add(egui::Slider::new(&mut feather, 0.0..=50.0)
                            .suffix(" px")
                            .clamping(egui::SliderClamping::Always)).changed()
                        {
                            // If kfs exist near the playhead, rewrite
                            // that kf; otherwise fold the static value.
                            let near_kf = crate::inspector::keyframe_selection::nearest(&clip.matte_feather_keyframes, clip_time_secs, kf_tolerance_secs);
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
                        let has_kf_here = crate::inspector::keyframe_selection::nearest(&clip.matte_feather_keyframes, clip_time_secs, kf_tolerance_secs).is_some();
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
                        ui.label(egui::RichText::new("Choke").size(super::rows::TEXT).color(theme.text_secondary));
                        ui.add_space(8.0);
                        let mut choke = crate::inspector::keyframe_selection::editable_value(
                            &clip.matte_choke_keyframes, clip_time_secs, binding.choke_px,
                            keyframe_tolerance);
                        if ui.add(egui::Slider::new(&mut choke, 0.0..=50.0)
                            .suffix(" px")
                            .clamping(egui::SliderClamping::Always)).changed()
                        {
                            let near_kf = crate::inspector::keyframe_selection::nearest(&clip.matte_choke_keyframes, clip_time_secs, kf_tolerance_secs);
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
                        let has_kf_here = crate::inspector::keyframe_selection::nearest(&clip.matte_choke_keyframes, clip_time_secs, kf_tolerance_secs).is_some();
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
                            .size(super::rows::TEXT)
                            .color(theme.text_secondary),
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
                    ui.painter().rect_stroke(preview_rect, 4.0, egui::Stroke::new(0.5_f32, theme.border));
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
                        .rounding(4.0)).clicked()
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
                row(ui, theme, "Target", |ui| {
                    // The dropdown's selected_text needs to read the
                    // right thing for both target shapes. For
                    // `Clip(...)` we look up the clip-candidates
                    // list; for `Tracker { ... }` we enumerate
                    // tracker targets and find the matching one.
                    let tracker_candidates = state.sequences.get(active_idx)
                        .map(crate::inspector::tracker_picker::enumerate_tracker_targets)
                        .unwrap_or_default();
                    let current_label = match clip.transform_link.as_ref() {
                        None => "(none)".to_string(),
                        Some(tl) => match &tl.target {
                            klipp_timeline::TransformLinkTarget::Clip(id) => {
                                candidates.iter()
                                    .find(|(cid, _)| cid == id)
                                    .map(|(_, l)| l.clone())
                                    .unwrap_or_else(|| "(missing — see banner)".to_string())
                            }
                            klipp_timeline::TransformLinkTarget::Tracker { clip_id: hid, point_index } => {
                                tracker_candidates.iter()
                                    .find(|(cid, pi, _)| cid == hid && *pi == *point_index)
                                    .map(|(_, _, l)| l.clone())
                                    .unwrap_or_else(|| "(missing tracker — see banner)".to_string())
                            }
                        },
                    };
                    egui::ComboBox::from_id_salt(("video:transform_link_target", clip_id))
                        .selected_text(current_label)
                        .width((ui.available_width() - 24.0).clamp(80.0, 260.0))
                        .show_ui(ui, |ui| {
                            let none_selected = clip.transform_link.is_none();
                            if ui.selectable_label(none_selected, "(none)").clicked()
                                && !none_selected {
                                    cmds.push(AppCommand::ClearClipTransformLink { clip_id });
                                }
                            // ── Clips section ──
                            // Headline only when there's something to
                            // show beneath it; same for the Trackers
                            // section below.
                            if !candidates.is_empty() {
                                ui.label(egui::RichText::new("── Clips ──").size(10.0).color(theme.text_dim));
                            }
                            for (cand_id, label) in &candidates {
                                let selected = clip.transform_link.as_ref()
                                    .is_some_and(|tl| match &tl.target {
                                        klipp_timeline::TransformLinkTarget::Clip(id) => id == cand_id,
                                        _ => false,
                                    });
                                if ui.selectable_label(selected, label).clicked()
                                    && !selected {
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
                            // ── Trackers section ──
                            // (HZ-MATTE-NEXT-TRACKER-DRIVER) — bind
                            // the link to a tracker on any clip in
                            // the active sequence. Picker logic is
                            // shared with the HYPERZOOM "Track this
                            // point" surface via tracker_picker.
                            if !tracker_candidates.is_empty() {
                                ui.label(egui::RichText::new("── Trackers ──").size(10.0).color(theme.text_dim));
                                if let Some(seq) = state.sequences.get(active_idx) {
                                    let cur = clip.transform_link.clone();
                                    let mode_flags = match cur.as_ref() {
                                        Some(tl) => (
                                            link_mode_to_u8(tl.mode),
                                            tl.inherit_scale, tl.inherit_position,
                                            tl.inherit_rotation, tl.inherit_anchor,
                                        ),
                                        None => (0u8, true, true, true, true),
                                    };
                                    crate::inspector::tracker_picker::tracker_menu_items(
                                        ui, seq,
                                        |hid, pi| cur.as_ref().is_some_and(|tl| match &tl.target {
                                            klipp_timeline::TransformLinkTarget::Tracker { clip_id, point_index } =>
                                                *clip_id == hid && *point_index == pi,
                                            _ => false,
                                        }),
                                        |hid, pi, _label| {
                                            cmds.push(AppCommand::SetClipTransformLinkTracker {
                                                clip_id,
                                                host_clip_id: hid,
                                                point_index: pi,
                                                mode: mode_flags.0,
                                                inherit_scale: mode_flags.1,
                                                inherit_position: mode_flags.2,
                                                inherit_rotation: mode_flags.3,
                                                inherit_anchor: mode_flags.4,
                                            });
                                        },
                                    );
                                }
                            }
                        });
                });

                // Help text — distinguishes one-shot keyframe paste
                // (the existing `GenerateKeyframesFromTrack` flow
                // surfaced in the TRACKING section) from this live
                // binding. Audit handoff implementation note (iv).
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        "Clip targets follow another clip's resolved transform. Tracker targets bind live — re-tracking updates this link automatically. (For a static one-shot copy, use 'Link to → Transform Pos' in the TRACKING section instead.)"
                    )
                    .size(10.0)
                    .italics()
                    .color(theme.text_dim),
                );

                // Detail rows only meaningful when a binding exists.
                if let Some(link) = clip.transform_link.clone() {
                    let target_id = link.target.referenced_clip_id();
                    let target_present = state.sequences.get(active_idx)
                        .map(|seq| seq.tracks.iter().any(|t| t.clips.iter().any(|c| c.id == target_id)))
                        .unwrap_or(false);
                    if !target_present {
                        ui.add_space(4.0);
                        ui.colored_label(
                            theme.marker_warning,
                            egui::RichText::new("⚠ Target clip is no longer in the project — this clip falls back to its own transform.")
                                .size(11.0),
                        );
                    }

                    ui.add_space(4.0);

                    // ── Mode (Absolute / Additive) ──
                    row(ui, theme, "Mode", |ui| {
                        let mut mode = link.mode;
                        for (m, label, hover) in [
                            (klipp_timeline::LinkMode::Absolute, "Absolute",
                                "Replace this clip's transform with the target's (per-axis flags decide which axes inherit)."),
                            (klipp_timeline::LinkMode::Additive, "Additive",
                                "Compose: positions add, scales multiply, rotations add. Anchor still inherits absolutely."),
                        ].into_iter().rev() {
                            let resp = ui.selectable_value(&mut mode, m, label);
                            if resp.on_hover_text(hover).clicked() && mode != link.mode {
                                cmds.push(AppCommand::SetClipTransformLink {
                                    clip_id,
                                    target: link.target.referenced_clip_id(),
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
                            target: link.target.referenced_clip_id(),
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
                        .rounding(4.0)).clicked()
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
    let (
        mut focal_x, mut focal_y, mut duration_secs, mut max_scale,
        mut target_choice, mut easing, mut hold_secs,
        mut add_radial_blur, mut radial_blur_amount,
        mut add_chromatic_aberration, mut chromatic_aberration_amount,
        mut add_light_leak, mut light_leak_intensity,
        mut whoosh_enabled, mut whoosh_variant_tag,
    ) = (
        stored.focal_x,
        stored.focal_y,
        stored.duration_secs,
        stored.max_scale,
        stored.target_choice,
        stored.easing,
        stored.hold_secs,
        stored.add_radial_blur,
        stored.radial_blur_amount,
        stored.add_chromatic_aberration,
        stored.chromatic_aberration_amount,
        stored.add_light_leak,
        stored.light_leak_intensity,
        stored.whoosh_enabled,
        stored.whoosh_variant,
    );

    section_card(ui, "HYPERZOOM", theme, |ui| {
        // Open and on screen: the viewer's focal handle is there (#2032).
        if ui.is_rect_visible(ui.available_rect_before_wrap()) {
            crate::hyperzoom::mark_section_shown(ui.ctx(), clip_id);
        }
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

                row(ui, theme, "Target", |ui| {
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
                        .width((ui.available_width() - 24.0).clamp(80.0, 220.0))
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

                ui.add_space(6.0);

                // ── Focal point (X / Y) ──
                // Spinbox fallback for the visual letter picker. 0,0
                // = top-left; 1,1 = bottom-right; 0.5,0.5 = centre.
                row(ui, theme, "Focal X", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut focal_x)
                            .range(0.0_f32..=1.0)
                            .speed(0.005)
                            .fixed_decimals(3),
                    );
                });
                row(ui, theme, "Focal Y", |ui| {
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

                // ── Track this point (HZ-MATTE-NEXT-TRACKER-DRIVER) ──
                // Alternative to the static focal X/Y / glyph snap:
                // bind the focal point to a tracker on any clip in
                // the active sequence. At Apply time, this becomes
                // a `FocalRef::Tracker` on the source clip's
                // Transform builtin — engine resolves the tracker
                // position each frame so the focal follows it
                // through the source clip's ramp window.
                let tracker_focal_id = egui::Id::new(("hyperzoom_picker_tracker", clip_id));
                let tracker_target: Option<(klipp_core::ClipId, u32)> = ui.ctx().data_mut(|d|
                    d.get_temp(tracker_focal_id).unwrap_or(None)
                );
                let tracker_label = match tracker_target {
                    None => "(static focal — no tracker)".to_string(),
                    Some((hid, pi)) => state.sequences.get(state.active_sequence_idx)
                        .and_then(|seq| crate::inspector::tracker_picker::enumerate_tracker_targets(seq)
                            .into_iter().find(|(c, p, _)| *c == hid && *p == pi)
                            .map(|(_, _, l)| l))
                        .unwrap_or_else(|| "(missing tracker)".to_string()),
                };
                row(ui, theme, "Track this point", |ui| {
                    egui::ComboBox::from_id_salt(("video:hyperzoom_tracker_target", clip_id))
                        .selected_text(tracker_label)
                        .width((ui.available_width() - 24.0).clamp(80.0, 260.0))
                        .show_ui(ui, |ui| {
                            // "(off)" entry — clears the tracker
                            // binding so Apply falls back to focal
                            // X/Y or the glyph snap.
                            let off_selected = tracker_target.is_none();
                            if ui.selectable_label(off_selected, "(static focal — no tracker)").clicked()
                                && !off_selected {
                                    ui.ctx().data_mut(|d| d.insert_temp::<Option<(klipp_core::ClipId, u32)>>(tracker_focal_id, None));
                                }
                            if let Some(seq) = state.sequences.get(state.active_sequence_idx) {
                                let trackers = crate::inspector::tracker_picker::enumerate_tracker_targets(seq);
                                if !trackers.is_empty() {
                                    ui.label(egui::RichText::new("── Trackers ──").size(10.0).color(theme.text_dim));
                                }
                                // Hoist the ctx clone out so the
                                // on_pick closure doesn't re-borrow
                                // ui while tracker_menu_items already
                                // holds &mut ui (egui Context is
                                // cheap to clone — Arc inside).
                                let ctx_for_pick = ui.ctx().clone();
                                crate::inspector::tracker_picker::tracker_menu_items(
                                    ui, seq,
                                    |hid, pi| tracker_target == Some((hid, pi)),
                                    |hid, pi, _label| {
                                        ctx_for_pick.data_mut(|d| d.insert_temp::<Option<(klipp_core::ClipId, u32)>>(tracker_focal_id, Some((hid, pi))));
                                    },
                                );
                            }
                        });
                });

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
                    if let Some(td_for_caveat) = title_clip.title_data() {
                        ui.add_space(6.0);

                        // Animated-text caveat (audit handoff
                        // implementation note (i)). The picker
                        // computes glyph positions from the title's
                        // STATIC layout — typewriter / scroll-in /
                        // char-animation effects evolve glyphs over
                        // clip-time and the cut-frame position
                        // won't match the snap. Tell the user up
                        // front instead of letting them debug it.
                        let has_animated_text = title_data_has_animated_chars(td_for_caveat);
                        if has_animated_text {
                            ui.label(
                                egui::RichText::new("Focal locks to final glyph position. Animated text effects ignored.")
                                    .size(10.0)
                                    .italics()
                                    .color(theme.text_dim),
                            );
                            ui.add_space(2.0);
                        }

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
                            egui::Stroke::new(0.5_f32, theme.border));

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

                        // Stale-pick indicator: the user picked a
                        // glyph at index N, but the title has since
                        // been edited and N is past the end of the
                        // current glyph layout. Fallback to static
                        // anchor_x/y kicks in at render time; here
                        // we tell the user about it so a "why isn't
                        // my pick following the letter anymore?"
                        // mystery becomes a one-line answer.
                        // Polarity-aware soft chrome (per
                        // feedback_popup_bg_polarity memory) — uses
                        // theme.marker_warning's tint at low alpha
                        // rather than a hardcoded amber.
                        let snap_is_stale = snapped_index
                            .is_some_and(|idx| idx >= positions.len());
                        if snap_is_stale {
                            ui.label(
                                egui::RichText::new("⚠ Picked letter no longer in title — using static fallback. Re-pick to relock.")
                                    .size(10.0)
                                    .color(theme.marker_warning),
                            );
                        }

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
                            egui::Stroke::new(1.5_f32, theme.accent));
                        ui.painter().line_segment(
                            [egui::pos2(centre.x - 8.0, centre.y), egui::pos2(centre.x + 8.0, centre.y)],
                            egui::Stroke::new(1.0_f32, theme.accent));
                        ui.painter().line_segment(
                            [egui::pos2(centre.x, centre.y - 8.0), egui::pos2(centre.x, centre.y + 8.0)],
                            egui::Stroke::new(1.0_f32, theme.accent));
                    }
                }

                ui.add_space(6.0);

                // ── Duration + peak scale ──
                row(ui, theme, "Duration", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut duration_secs)
                            .range(0.05_f64..=10.0)
                            .speed(0.05)
                            .suffix(" s")
                            .fixed_decimals(2),
                    );
                });
                row(ui, theme, "Peak scale", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut max_scale)
                            .range(1.5_f32..=200.0)
                            .speed(0.5)
                            .suffix(" ×")
                            .fixed_decimals(1),
                    );
                });

                ui.add_space(6.0);

                // ── Easing ──
                // Linear is the back-compat default. Bezier emits a
                // symmetric S-curve via `±duration/3` horizontal
                // tangents — visually a slow-start/fast-middle/slow-
                // end ramp on each side of the cut.
                row(ui, theme, "Easing", |ui| {
                    let label = match easing {
                        crate::hyperzoom::HyperzoomEasing::Linear => "Linear",
                        crate::hyperzoom::HyperzoomEasing::Bezier => "Bezier (ease-in/out)",
                    };
                    egui::ComboBox::from_id_salt(("video:hyperzoom_easing", clip_id))
                        .selected_text(label)
                        .width((ui.available_width() - 24.0).clamp(80.0, 220.0))
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
                row(ui, theme, "Hold at peak", |ui| {
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

                ui.add_space(6.0);

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
                        ui.label(egui::RichText::new("Amount").size(super::rows::TEXT).color(theme.text_secondary));
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

                ui.add_space(4.0);

                // ── Chromatic aberration (opt-in convenience) ──
                // Inserts/updates a `ChromaticAberration` builtin
                // on source + target with center pinned to the
                // focal point and red/blue keyframed
                // 0 → ∓amount → 0 (red converges, blue diverges).
                ui.horizontal(|ui| {
                    ui.checkbox(&mut add_chromatic_aberration, "Add chromatic aberration on apply")
                        .on_hover_text(
                            "Inserts a ChromaticAberration builtin on source + target with \
                             center pinned to the focal point. Red converges and blue \
                             diverges from the focal point at peak — the canonical \
                             trailer-RGB-split look. Power users wanting asymmetric splits \
                             can tweak the inserted builtin afterwards.",
                        );
                });
                if add_chromatic_aberration {
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.label(egui::RichText::new("Amount").size(super::rows::TEXT).color(theme.text_secondary));
                        ui.add_space(8.0);
                        ui.add(
                            egui::DragValue::new(&mut chromatic_aberration_amount)
                                .range(0.0_f32..=30.0)
                                .speed(0.25)
                                .fixed_decimals(1),
                        ).on_hover_text(
                            "Peak split magnitude in pixels. 3 = subtle fringe; 6 = the \
                             default punchy-but-readable trailer split; 15+ is stylised. \
                             Drives both red_shift (0 → -amount → 0) and blue_shift \
                             (0 → +amount → 0) symmetrically across the ramp.",
                        );
                    });
                }

                ui.add_space(4.0);

                // ── Light leak / FilmHalation (opt-in convenience) ──
                // Rides BuiltinEffect::FilmHalation: bright pixels
                // bloom warmly across the cut.
                ui.horizontal(|ui| {
                    ui.checkbox(&mut add_light_leak, "Add light leak on apply")
                        .on_hover_text(
                            "Inserts a FilmHalation builtin on source + target with intensity \
                             keyframed 0 → peak → 0 across the ramp. Bright pixels bloom \
                             warmly (orange-yellow hue) at the cut — the trailer-style warm-\
                             bleed look. Threshold/radius/hue use trailer defaults; tweak the \
                             inserted builtin for custom looks.",
                        );
                });
                if add_light_leak {
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.label(egui::RichText::new("Intensity").size(super::rows::TEXT).color(theme.text_secondary));
                        ui.add_space(8.0);
                        ui.add(
                            egui::DragValue::new(&mut light_leak_intensity)
                                .range(0.0_f32..=1.0)
                                .speed(0.01)
                                .fixed_decimals(2),
                        ).on_hover_text(
                            "Peak FilmHalation intensity (0..1). 0.2 = subtle; 0.4 = the \
                             default punchy trailer-bleed; 0.7+ is heavy bloom. Same value \
                             scale as the standalone FilmHalation effect's intensity field.",
                        );
                    });
                }

                ui.add_space(4.0);

                // ── Whoosh stinger (HZ-NEXT-WHOOSH) ──
                // Off by default. When enabled, Apply additionally
                // inserts an audio whoosh on the lowest audio
                // track centred on the cut frame.
                ui.horizontal(|ui| {
                    ui.checkbox(&mut whoosh_enabled, "Add whoosh on apply")
                        .on_hover_text(
                            "Inserts a short audio stinger on the lowest audio track, \
                             timed so its peak lands on the cut frame. Synthesised \
                             whooshes ship in three lengths; pick a custom WAV for \
                             your own pack.",
                        );
                });
                if whoosh_enabled {
                    let mut choice = crate::hyperzoom::WhooshChoice::from_u8(whoosh_variant_tag);
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.label(egui::RichText::new("Variant").size(super::rows::TEXT).color(theme.text_secondary));
                        ui.add_space(8.0);
                        let prev_label = choice.label();
                        egui::ComboBox::from_id_salt(("hyperzoom_whoosh_variant", clip_id))
                            .selected_text(prev_label)
                            .show_ui(ui, |ui| {
                                for opt in crate::hyperzoom::WhooshChoice::ALL {
                                    ui.selectable_value(&mut choice, opt, opt.label());
                                }
                            });
                    });
                    whoosh_variant_tag = choice.as_u8();
                    if matches!(choice, crate::hyperzoom::WhooshChoice::Custom) {
                        let custom = crate::hyperzoom::read_whoosh_custom(ui.ctx(), clip_id);
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(egui::RichText::new("Sample").size(super::rows::TEXT).color(theme.text_secondary));
                            ui.add_space(8.0);
                            let label = custom
                                .path
                                .as_ref()
                                .and_then(|p| p.file_name())
                                .and_then(|n| n.to_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "Pick file\u{2026}".to_string());
                            let picker = egui::Id::new("hyperzoom-whoosh-sample").with(clip_id);
                            if let Some(crate::file_dialog::Picked::Path(picked)) = crate::file_dialog::take_path(picker) {
                                let length = crate::hyperzoom::probe_wav_duration_secs(&picked);
                                crate::hyperzoom::update_whoosh_custom(ui.ctx(), clip_id, |s| {
                                    s.path = Some(picked);
                                    s.length_secs = length;
                                });
                            }
                            if ui
                                .button(label)
                                .on_hover_text(
                                    "Pick a WAV (or other supported audio format) to use \
                                     as the whoosh. The file must already be imported in \
                                     the media pool — drag it into the media browser if \
                                     it isn't yet. Length probe and the soft warning only \
                                     work for WAV today.",
                                )
                                .clicked()
                            {
                                crate::file_dialog::pick_path(picker, || {
                                    rfd::FileDialog::new()
                                        .set_title("Pick custom whoosh sample")
                                        .add_filter("Audio", klipp_core::media_extensions::AUDIO_EXTENSIONS)
                                        .pick_file()
                                });
                            }
                        });
                        // Soft length warning per audit note (v).
                        // Warning only — no truncation, no length
                        // picker.
                        if let Some(msg) =
                            crate::hyperzoom::whoosh_custom_length_warning(custom.length_secs)
                        {
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label(
                                    egui::RichText::new(format!("\u{26A0} {msg}"))
                                        .size(11.0)
                                        .color(theme.marker_warning),
                                );
                            });
                        }
                    }
                }

                ui.add_space(4.0);

                // ── Saved presets (HZ-NEXT-SAVED-PRESETS) ──
                // Save the current hyperzoom config (without focal
                // / target — those stay per-cut) under a name, or
                // load a previously-saved preset into this clip's
                // scratch. Files live at
                // `~/.config/klipp/hyperzoom_presets/`.
                ui.horizontal(|ui| {
                    if ui
                        .button("Save as preset\u{2026}")
                        .on_hover_text(
                            "Save the current hyperzoom config under a name. Focal point + \
                             target clip stay per-cut and are NOT part of the saved preset — \
                             every load on a new clip re-uses your existing focal pick.",
                        )
                        .clicked()
                    {
                        cmds.push(AppCommand::OpenSaveHyperzoomPresetDialog {
                            source_clip: clip_id,
                        });
                    }
                    ui.add_space(8.0);
                    let presets = state.hyperzoom_preset_registry.presets();
                    let combo_label = if presets.is_empty() {
                        "Load preset (none saved)"
                    } else {
                        "Load preset"
                    };
                    egui::ComboBox::from_id_salt(("hyperzoom_load_preset", clip_id))
                        .selected_text(combo_label)
                        .show_ui(ui, |ui| {
                            if presets.is_empty() {
                                ui.label(
                                    egui::RichText::new("No presets yet — Save as preset\u{2026} to start")
                                        .size(super::rows::TEXT)
                                        .color(theme.text_secondary),
                                );
                            } else {
                                for preset in presets {
                                    if ui.button(&preset.name).clicked() {
                                        let p = preset.clone();
                                        crate::hyperzoom::update_hyperzoom_scratch(
                                            ui.ctx(), clip_id, |s| p.apply_to_scratch(s),
                                        );
                                        // Refresh local mut vars from the
                                        // post-load scratch — otherwise
                                        // the bottom-of-function persist
                                        // step writes back the stale
                                        // pre-load locals and undoes the
                                        // load. Only the preset-worthy
                                        // fields need refreshing; focal
                                        // / target / per-cut survive
                                        // untouched.
                                        let fresh = crate::hyperzoom::read_hyperzoom_scratch(ui.ctx(), clip_id);
                                        duration_secs = fresh.duration_secs;
                                        max_scale = fresh.max_scale;
                                        easing = fresh.easing;
                                        hold_secs = fresh.hold_secs;
                                        add_radial_blur = fresh.add_radial_blur;
                                        radial_blur_amount = fresh.radial_blur_amount;
                                        add_chromatic_aberration = fresh.add_chromatic_aberration;
                                        chromatic_aberration_amount = fresh.chromatic_aberration_amount;
                                        add_light_leak = fresh.add_light_leak;
                                        light_leak_intensity = fresh.light_leak_intensity;
                                        whoosh_enabled = fresh.whoosh_enabled;
                                        whoosh_variant_tag = fresh.whoosh_variant;
                                    }
                                }
                            }
                        })
                        .response
                        .on_hover_text(format!(
                            "Click a preset to load it into this clip's hyperzoom scratch. \
                             Focal point + target clip are preserved (preset doesn't carry \
                             those). To delete presets, remove their JSON files from \
                             {}",
                            state.hyperzoom_preset_registry.presets_dir().display(),
                        ));
                });

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
                        .rounding(4.0),
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
                        // Build the glyph-anchored override if (and
                        // only if) the picker has a snapped glyph
                        // AND there's still a resolvable title clip
                        // for it. The picker_target lookup is the
                        // same one the preview slot used; reading it
                        // again at dispatch time avoids a stale
                        // snapped_index pointing at a clip that's
                        // been removed since the user picked.
                        let snap_id = egui::Id::new(("hyperzoom_picker_snapped", clip_id));
                        let snapped: Option<usize> = ui.ctx().data_mut(|d|
                            d.get_temp(snap_id).unwrap_or(None)
                        );
                        let tracker_focal_id = egui::Id::new(("hyperzoom_picker_tracker", clip_id));
                        let tracker_target: Option<(klipp_core::ClipId, u32)> =
                            ui.ctx().data_mut(|d| d.get_temp(tracker_focal_id).unwrap_or(None));
                        // Precedence at Apply time: tracker pick wins
                        // over glyph snap (more specific binding,
                        // user-explicit). Glyph snap wins over None
                        // (today's behaviour). Static focal_x/y is
                        // always written into anchor_x/y as the
                        // resolution-failure fallback regardless.
                        let focal_ref = if let Some((hid, pi)) = tracker_target {
                            Some(klipp_core::FocalRef::Tracker {
                                clip_id: hid.0,
                                point_index: pi,
                                fallback_xy: (focal_x, focal_y),
                            })
                        } else {
                            snapped.and_then(|char_idx| {
                                crate::inspector::letter_picker::picker_target_for_clip(
                                    state.sequence(), clip_id, state.playhead,
                                ).map(|title_clip| klipp_core::FocalRef::Glyph {
                                    title_clip_id: title_clip.id.0,
                                    char_index: char_idx as u32,
                                    fallback_xy: (focal_x, focal_y),
                                })
                            })
                        };
                        // Whoosh fields routed through the pure
                        // build_whoosh_fields_for_apply helper so
                        // the scratch → AppCommand mapping is
                        // testable without standing up egui. When
                        // the checkbox is off, the helper returns
                        // canonical defaults — the AppCommand
                        // handler short-circuits before any audio
                        // work runs.
                        let whoosh_scratch_snapshot = crate::hyperzoom::HyperzoomScratch {
                            whoosh_enabled,
                            whoosh_variant: whoosh_variant_tag,
                            // Other fields not consulted by the
                            // helper — pass defaults to keep the
                            // construction local.
                            ..crate::hyperzoom::HyperzoomScratch::default()
                        };
                        let custom_state = crate::hyperzoom::read_whoosh_custom(ui.ctx(), clip_id);
                        let (add_whoosh, whoosh_variant, whoosh_custom_path) =
                            crate::hyperzoom::build_whoosh_fields_for_apply(
                                &whoosh_scratch_snapshot,
                                &custom_state,
                            );
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
                            focal_ref,
                            add_whoosh,
                            whoosh_variant,
                            whoosh_custom_path,
                            // Chromatic-aberration + light-leak fields
                            // from the inspector's scratch slot. When
                            // their checkboxes are off, we still pass
                            // the scratch's stored amount/intensity —
                            // the AppCommand handler short-circuits on
                            // the bool before any work runs, so the
                            // magnitude doesn't matter.
                            add_chromatic_aberration,
                            chromatic_aberration_amount,
                            add_light_leak,
                            light_leak_intensity,
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
                        .rounding(4.0),
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
        s.add_chromatic_aberration = add_chromatic_aberration;
        s.chromatic_aberration_amount = chromatic_aberration_amount;
        s.add_light_leak = add_light_leak;
        s.light_leak_intensity = light_leak_intensity;
        s.whoosh_enabled = whoosh_enabled;
        s.whoosh_variant = whoosh_variant_tag;
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
            .and_then(|c| c.transform_link.as_ref().map(|tl| tl.target.referenced_clip_id()));
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

/// Does this title data have an animation that moves glyphs over
/// clip-time? Used by the HYPERZOOM picker preview to surface a
/// caveat — the picker computes glyph positions from the static
/// layout (audit-deferred from HZ-NEXT-LETTER-PICKER), so a
/// scrolling / typewritering title's glyph centroid won't match
/// what's on screen at the cut frame.
///
/// FadeIn / FadeOut deliberately do NOT count: opacity changes
/// don't move glyphs, so the picker's static-layout assumption
/// holds. Only positional animations and typewriter-style
/// reveals are caveat-worthy.
fn title_data_has_animated_chars(td: &klipp_timeline::TitleData) -> bool {
    use klipp_timeline::{MotionKind, TitleLoop, TitleObjectContent};
    td.objects.iter().any(|o| match &o.content {
        TitleObjectContent::TextBox(_) => {
            let fx = o.fx();
            let moves = |m: &klipp_timeline::Motion| !matches!(m.kind, MotionKind::Fade | MotionKind::Blur);
            fx.motion.enter.as_ref().is_some_and(moves)
                || fx.motion.exit.as_ref().is_some_and(moves)
                || fx.loops.iter().any(|l| matches!(l, TitleLoop::Scroll { .. } | TitleLoop::Crawl { .. } | TitleLoop::Shake { .. } | TitleLoop::Wave { .. }))
        }
        _ => false,
    })
}

/// Frame Glide (#611): the clip's move between a Start and an End
/// framing, framed in the viewer; its easing, Swap, Reset and Remove.
/// A clip animated by hand is not glided over without asking.
fn show_frame_glide_section(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip: &Clip, cmds: &mut Vec<AppCommand>) {
    use klipp_core::frame_glide::{FrameGlide, GlideFrame};
    use klipp_timeline::frame_glide::GlideEasing;
    let clip_id = clip.id;
    let seq = state.sequence();
    let (w, h) = (seq.resolution.width as f32, seq.resolution.height as f32);
    let free = klipp_timeline::frame_glide::keys_are_free(clip, w, h);
    let framing = state.frame_glide_clip == Some(clip_id);
    section_card(ui, "FRAME GLIDE", theme, |ui| {
        egui::Frame::default().inner_margin(egui::Margin::symmetric(14.0, 8.0)).show(ui, |ui| {
            let set = |glide: FrameGlide, replace: bool| AppCommand::SetFrameGlide { clip_id, glide: Some(glide), replace_animation: replace };
            if !free && clip.frame_glide.is_none() {
                ui.label(egui::RichText::new("This clip's position or scale is animated already. Frame Glide would replace that animation.").size(11.0).color(theme.text_secondary));
                if ui.button("Replace Animation with a Glide").clicked() {
                    cmds.push(set(FrameGlide::default(), true));
                    cmds.push(AppCommand::SetFrameGlideMode(Some(clip_id)));
                }
                return;
            }
            ui.horizontal_wrapped(|ui| {
                let label = if framing { "Done" } else { "Frame in Viewer" };
                let hover = if framing { "Back to the clip as it plays" } else { "Show the whole picture with the Start and End framings over it: drag one to move it, a corner to size it" };
                if ui.button(label).on_hover_text(hover).clicked() {
                    cmds.push(AppCommand::SetFrameGlideMode((!framing).then_some(clip_id)));
                }
                if let Some(glide) = clip.frame_glide {
                    if ui.button("Swap Start/End").clicked() {
                        cmds.push(set(FrameGlide { start: glide.end, end: glide.start, ..glide }, false));
                    }
                    if ui.button("Reset").on_hover_text("Both framings back to the whole frame").clicked() {
                        cmds.push(set(FrameGlide { start: GlideFrame::FULL, end: GlideFrame::FULL, ..glide }, false));
                    }
                    if ui.button("Remove Glide").on_hover_text("Take the glide off, its keys with it").clicked() {
                        cmds.push(AppCommand::SetFrameGlide { clip_id, glide: None, replace_animation: false });
                    }
                }
            });
            if let Some(glide) = clip.frame_glide {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Easing").size(11.0).color(theme.text_secondary));
                    let mut easing = glide.easing;
                    egui::ComboBox::from_id_salt(("frame_glide_easing", clip_id)).selected_text(easing.label()).show_ui(ui, |ui| {
                        for e in GlideEasing::ALL {
                            ui.selectable_value(&mut easing, e, e.label());
                        }
                    });
                    if easing != glide.easing {
                        cmds.push(set(FrameGlide { easing, ..glide }, false));
                    }
                });
                let pct = |f: GlideFrame| format!("{:.0}%", 100.0 / f.width.max(0.01));
                ui.label(egui::RichText::new(format!("Start {} \u{2192} End {} zoom", pct(glide.start), pct(glide.end))).size(11.0).color(theme.text_secondary));
            } else if !framing {
                ui.label(egui::RichText::new("Compose where the shot starts and ends over the picture; the move runs the clip's length, however it is trimmed.").size(11.0).color(theme.text_secondary));
            }
        });
    });
}

fn show_ken_burns_section(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, cmds: &mut Vec<AppCommand>) {
    let clip_id = clip.id;
    let pad = 14.0;
    section_card(ui, "KEN BURNS", theme, |ui| {
        let has_transform_kfs = !lane(clip, "scale").is_empty()
            || !lane(clip, "pos_x").is_empty()
            || !lane(clip, "pos_y").is_empty();

        egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(pad, 8.0))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                    let btn_text = |label: &str| egui::RichText::new(label).size(11.0);
                    if ui.add(egui::Button::new(btn_text("Zoom In"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(2.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Scale 1.0 to 1.3, centered").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.0, end_scale: 1.3,
                            start_pos_x: 0.0, start_pos_y: 0.0, end_pos_x: 0.0, end_pos_y: 0.0,
                        });
                    }
                    if ui.add(egui::Button::new(btn_text("Zoom Out"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(2.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Scale 1.3 to 1.0, centered").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.3, end_scale: 1.0,
                            start_pos_x: 0.0, start_pos_y: 0.0, end_pos_x: 0.0, end_pos_y: 0.0,
                        });
                    }
                    if ui.add(egui::Button::new(btn_text("Pan L>R"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(2.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Pan left to right, 1.2\u{00d7} zoom").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.2, end_scale: 1.2,
                            start_pos_x: -100.0, start_pos_y: 0.0, end_pos_x: 100.0, end_pos_y: 0.0,
                        });
                    }
                    if ui.add(egui::Button::new(btn_text("Pan R>L"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(2.0).min_size(egui::vec2(0.0, 20.0))
                    ).on_hover_text("Pan right to left, 1.2\u{00d7} zoom").clicked() {
                        cmds.push(AppCommand::ApplyKenBurns {
                            clip_id, start_scale: 1.2, end_scale: 1.2,
                            start_pos_x: 100.0, start_pos_y: 0.0, end_pos_x: -100.0, end_pos_y: 0.0,
                        });
                    }
                    if has_transform_kfs && ui.add(egui::Button::new(btn_text("Remove"))
                        .fill(theme.bg_raised).stroke(egui::Stroke::new(0.5_f32, theme.border))
                        .rounding(2.0).min_size(egui::vec2(0.0, 20.0))
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
                // The rate is the magnitude; a reversed clip keeps its
                // direction through a change of rate (#617).
                let sign = if clip.is_reversed() { -1.0 } else { 1.0 };
                let mut rate = (clip.speed.abs() * 100.0) as f32;
                let r_rate = param_row(ui, theme, "Rate", &mut rate, 10.0..=1600.0, 100.0, 0.5, 0, " %", None);
                if r_rate.changed {
                    let new_speed = (rate as f64 / 100.0).clamp(0.1, 16.0) * sign;
                    cmds.push(AppCommand::SetClipSpeed { clip_id, speed: new_speed, ripple: true });
                }

                ui.add_space(4.0);

                // Direction row — toggle between Forward and Reverse by flipping
                // the sign of clip.speed (#617). A media clip's only: a title
                // or a compound has no file to read backward.
                let can_reverse = matches!(clip.kind, klipp_timeline::ClipKind::Media { .. });
                if can_reverse { row(ui, theme, "Direction", |ui| {
                    {
                        let is_reversed = clip.speed < 0.0;
                        let label = if is_reversed { "⇄ Reverse" } else { "⇄ Forward" };
                        if ui
                            .selectable_label(is_reversed, egui::RichText::new(label).size(super::rows::TEXT))
                            .on_hover_text("Play the clip from its last frame to its first. Its sound plays backward with it.")
                            .clicked()
                        {
                            let abs = clip.speed.abs().max(0.1);
                            let new_speed = if is_reversed { abs } else { -abs };
                            cmds.push(AppCommand::SetClipSpeed { clip_id, speed: new_speed, ripple: true });
                        }
                    }
                }); }

                ui.add_space(4.0);

                // Speed preset buttons
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
                    let btn_text = |label: &str| egui::RichText::new(label).size(11.0);
                    for (label, speed_val) in [("25%", 0.25), ("50%", 0.5), ("100%", 1.0), ("200%", 2.0), ("400%", 4.0)] {
                        let is_active = (clip.speed.abs() - speed_val).abs() < 0.001;
                        let fill = if is_active { theme.accent.linear_multiply(0.25) } else { theme.bg_raised };
                        let stroke = if is_active {
                            egui::Stroke::new(1.0_f32, theme.accent)
                        } else {
                            egui::Stroke::new(0.5_f32, theme.border)
                        };
                        if ui.add(egui::Button::new(btn_text(label))
                            .fill(fill).stroke(stroke)
                            .rounding(2.0).min_size(egui::vec2(0.0, 20.0))
                        ).clicked() {
                            cmds.push(AppCommand::SetClipSpeed { clip_id, speed: speed_val * sign, ripple: true });
                        }
                    }
                });
                // A retimed clip's sound at its own pitch (#925).
                ui.add_space(4.0);
                row(ui, theme, "Keep pitch", |ui| {
                    let on = clip.keep_pitch;
                    let hover = "The sound plays at its own pitch, longer or shorter. Off, its pitch moves with the speed, as tape's does.";
                    let resp = ui.add_enabled_ui(clip.speed > 0.0, |ui| switch(ui, theme, on)).inner;
                    if resp.on_hover_text(hover).on_disabled_hover_text(hover).clicked() {
                        cmds.push(AppCommand::SetClipKeepPitch { clip_id, keep_pitch: !on });
                    }
                });
            });
    });
}

/// An edit writes the Colour Fade's node by its path, as the stack's own
/// card does, so the two edit one source of truth (M3 step 6).
fn show_color_fade_section(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, cmds: &mut Vec<AppCommand>) {
    section_card(ui, "COLOUR FADE", theme, |ui| {
        ui.add_space(4.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: 14.0, right: 14.0, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE)
            .show(ui, |ui| {
                color_fade_row(ui, theme, clip, cmds, klipp_effects::ColorFadeMode::In, "Fade from colour");
                color_fade_row(ui, theme, clip, cmds, klipp_effects::ColorFadeMode::Out, "Fade to colour");
            });
        ui.add_space(6.0);
    });
}

/// One row of the COLOR FADE section. Renders the effect's index in
/// the clip's chain (when present) so Update / Remove commands target
/// the right entry; if the effect doesn't exist yet, an Add button
/// creates one with sane defaults (1.0 s, opaque black).
fn color_fade_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
    mode: klipp_effects::ColorFadeMode,
    label: &str,
) {
    use klipp_effects::BuiltinEffect;
    let update = |idx: usize, effect: BuiltinEffect| color_fade_update(clip, idx, effect);
    let existing = clip.builtin_effects().iter().enumerate().find_map(|(i, e)| match e {
        BuiltinEffect::ColorFade { color, duration_secs, mode: m, .. } if *m == mode => {
            Some((i, *color, *duration_secs))
        }
        _ => None,
    });
    let clip_id = clip.id;

    row(ui, theme, label, |ui| {
        // Right to left: the switch at the edge, then the colour and the
        // length when the fade is on.
        let enabled = existing.is_some();
        if switch(ui, theme, enabled).clicked() {
            let want_enabled = !enabled;
            if want_enabled && !enabled {
                cmds.push(AppCommand::AddBuiltinEffect {
                    clip_id,
                    effect: BuiltinEffect::ColorFade {
                        color: [0, 0, 0, 255],
                        duration_secs: 1.0,
                        mode,
                        progress: 0.0,
                    },
                });
            } else if !want_enabled && enabled {
                if let Some((idx, _, _)) = existing {
                    cmds.push(AppCommand::RemoveBuiltinEffect { clip_id, index: idx });
                }
            }
        }
        if let Some((idx, color, duration)) = existing {
            ui.add_space(8.0);
            // Duration scrubby. egui's DragValue is enough — no need for
            // a heavyweight scrubby with keyframe affordances since the
            // ColorFade's progress is engine-driven, not user-keyframed.
            let mut dur = duration;
            if ui.add(egui::DragValue::new(&mut dur)
                .range(0.05..=10.0)
                .speed(0.05)
                .suffix(" s")
                .fixed_decimals(2)
            ).changed() {
                cmds.push(update(idx, BuiltinEffect::ColorFade { color, duration_secs: dur, mode, progress: 0.0 }));
            }
            ui.add_space(4.0);
            // Colour picker — reads/writes on change.
            let mut c = egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]);
            if ui.color_edit_button_srgba(&mut c).changed() {
                cmds.push(update(idx, BuiltinEffect::ColorFade { color: [c.r(), c.g(), c.b(), c.a()], duration_secs: duration, mode, progress: 0.0 }));
            }
        }
    });
}

/// An edit of the clip's Colour Fade at `idx` of its effect list: its node
/// by path, or its place in the list should the picture not name one.
pub(crate) fn color_fade_update(clip: &Clip, idx: usize, effect: klipp_effects::BuiltinEffect) -> AppCommand {
    match clip.effect_node_at(false, idx) {
        Some(node) => AppCommand::SetPayloadAt {
            addr: klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: clip.id.0 }, [node]),
            payload: serde_json::to_value(&effect).unwrap_or(serde_json::Value::Null),
        },
        None => AppCommand::UpdateBuiltinEffect { clip_id: clip.id, index: idx, effect },
    }
}

#[cfg(test)]
mod color_fade_tests {
    use super::*;
    use klipp_timeline::test_clip::TestClip;

    /// The Video tab's Colour Fade edit writes the very node the stack's
    /// Colour Fade row addresses.
    #[test]
    fn color_fade_has_one_source_of_truth() {
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let fade = klipp_effects::BuiltinEffect::ColorFade { color: [0, 0, 0, 255], duration_secs: 1.0, mode: klipp_effects::ColorFadeMode::In, progress: 0.0 };
        let clip = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 }).effect(fade.clone()).build();
        let lifted = clip.picture_v2().unwrap();
        let view = klipp_graph_lift::stack_view::project(klipp_graph_format::path::Root::Clip { clip: clip.id.0 }, &lifted.picture.graph, &klipp_timeline::RegistryCatalog, None).unwrap();
        let row = view.rows().find(|r| klipp_graph_lift::effect_params::tag(crate::inspector::stack_view::node_at(&lifted.picture.graph, &r.addr).unwrap()) == Some("color_fade")).expect("the fade's row");
        match color_fade_update(&clip, 1, fade.clone()) {
            AppCommand::SetPayloadAt { addr, payload } => {
                assert_eq!(addr, row.addr, "the stack's own node");
                assert_eq!(payload["type"], "color_fade");
            }
            other => panic!("{other:?}"),
        }
    }
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
/// A video row's keyframe button, interpolation menu, ease presets and
/// previous/next navigation, as each transform row above spells out.
#[allow(clippy::too_many_arguments)]
fn keyframe_controls(
    cmds: &mut Vec<AppCommand>,
    r: &crate::widgets::scrubby_param::ScrubbyParamResponse,
    kfs: &[klipp_core::Keyframe],
    ks: &KeyframeState,
    param: VideoParam,
    value: f32,
    clip_id: klipp_core::ClipId,
    clip_time_secs: f64,
    (timeline_in_secs, rate): (f64, u32),
    keyframe_tolerance: f64,
) {
    let kf_tolerance = keyframe_tolerance;
    let here = crate::inspector::keyframe_selection::nearest(kfs, clip_time_secs, kf_tolerance);
    if let (Some(interp), Some(ki)) = (r.keyframe_set_interpolation, here) {
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param, keyframe_index: ki, interpolation: interp });
    }
    if let (Some(preset), Some(ki)) = (r.keyframe_ease_preset, here) {
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
        for idx in [ki.saturating_sub(1), ki] {
            if idx >= kfs.len() {
                continue;
            }
            let (t_in, t_out) = preset.compute_tangents(kfs, idx);
            cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out });
        }
    }
    if r.keyframe_toggled {
        if let Some(i) = here {
            cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param, time_secs: kfs[i].time_secs });
        } else {
            cmds.push(AppCommand::AddVideoKeyframe { clip_id, param, time_secs: clip_time_secs, value });
        }
        cmds.push(AppCommand::SetVideoRubberBandParam(param));
    }
    let target = if r.keyframe_nav_prev { ks.prev_keyframe_time } else if r.keyframe_nav_next { ks.next_keyframe_time } else { None };
    if let Some(t) = target {
        cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(timeline_in_secs + t, rate)));
    }
}

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

/// The height a scale of `width` gives `t`'s clip: its aspect (height
/// against width) kept, as the scale lane keys both (#1406); a width of 0
/// gives a height of 0.
pub(crate) fn keep_aspect(t: &TransformParams, width: f32) -> f32 {
    if t.scale_x != 0.0 { width * t.scale_y / t.scale_x } else { width }
}

/// The Perspective the row stores for the value it shows: its default,
/// 1, is unset (0), which the renderer draws as 1, as the stack's card
/// resets it to the catalog's unset (#1381). So a reset of the row and a
/// reset of the card store the same, and an edit of another transform
/// value never writes a Perspective the clip didn't have.
fn stored_perspective(shown: f32) -> f32 {
    if shown == 1.0 { 0.0 } else { shown }
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

/// A row in the inspector's row style inside a section's padded frame.
fn row(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, value: impl FnOnce(&mut egui::Ui)) {
    super::rows::row_padded(ui, theme, label, 0.0, value);
}

fn section_card(
    ui: &mut egui::Ui,
    title: &str,
    theme: &KlippTheme,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    crate::widgets::inspector_group::section_card(ui, title, None, theme, add_contents);
}

#[cfg(test)]
mod tilt_tests {
    use super::*;
    use klipp_timeline::test_clip::TestClip;

    fn clip_with(effect: BuiltinEffect) -> Clip {
        TestClip::of(Clip::gap(Timecode::from_secs_f64(0.0, 24), Timecode::from_secs_f64(2.0, 24))).effect(effect).build()
    }

    #[test]
    fn editing_the_transform_keeps_a_still_tilt() {
        let t = TransformParams { rotation_y: 25.0, perspective: 1.5, ..Default::default() };
        let clip = clip_with(t.to_builtin_effect());
        // Read back, moved, written again: the tilt is still there.
        let mut read = get_transform(&clip);
        assert_eq!((read.rotation_y, read.perspective), (25.0, 1.5));
        read.position_x = 40.0;
        let BuiltinEffect::Transform { rotation_y, perspective, position_x, .. } = read.to_builtin_effect() else { panic!("a transform") };
        assert_eq!((rotation_y, perspective, position_x), (25.0, 1.5, 40.0));
    }

    /// Every piece of text the inspector's video tab draws for `clip`.
    fn drawn_text(clip: &Clip) -> String {
        let ctx = egui::Context::default();
        let state = AppState::default();
        let theme = KlippTheme::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut cmds = Vec::new();
                show_clip_video(ui, &theme, &state, clip, &mut cmds, 0.5, true, false);
            });
        });
        fn walk(shape: &egui::Shape, out: &mut String) {
            match shape {
                egui::Shape::Text(t) => {
                    out.push_str(t.galley.text());
                    out.push('\n');
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut text = String::new();
        out.shapes.iter().for_each(|c| walk(&c.shape, &mut text));
        text
    }

    /// The Perspective row's shown default stores as unset, the value the
    /// card's reset stores (#1381); any other value as itself.
    #[test]
    fn perspective_shown_as_one_is_stored_unset() {
        assert_eq!(stored_perspective(1.0), 0.0);
        assert_eq!(stored_perspective(1.7), 1.7);
        assert_eq!(stored_perspective(0.1), 0.1);
    }

    #[test]
    fn the_inspector_shows_tilt_and_depth_and_perspective_once_tilted() {
        let flat = drawn_text(&clip_with(TransformParams::default().to_builtin_effect()));
        for row in ["Rotation", "Tilt X", "Tilt Y", "Depth"] {
            assert!(flat.contains(row), "no {row} row in:\n{flat}");
        }
        assert!(!flat.contains("Perspective"), "a flat card has no perspective to set");
        let tilted = drawn_text(&clip_with(TransformParams { rotation_x: 15.0, ..Default::default() }.to_builtin_effect()));
        assert!(tilted.contains("Perspective"), "a tilted card offers its perspective:\n{tilted}");
    }

    /// The Scale Y row (#1471): not on an evenly scaled clip with no keys,
    /// where Scale is both axes; on an unevenly scaled one, under keys on
    /// the scale, where the height can be keyed apart, and on a clip whose
    /// height has keys of its own.
    #[test]
    fn the_inspector_shows_scale_y_where_the_height_can_be_keyed_apart() {
        let even = clip_with(TransformParams::default().to_builtin_effect());
        assert!(!drawn_text(&even).contains("Scale Y"), "an even clip has one scale");
        let uneven = clip_with(TransformParams { scale_y: 0.5, ..Default::default() }.to_builtin_effect());
        assert!(drawn_text(&uneven).contains("Scale Y"), "an uneven clip shows its height");
        for lane in ["scale", "scale_y"] {
            let keyed = TestClip::of(even.clone()).lane(lane, 0.0, 1.5).build();
            assert!(drawn_text(&keyed).contains("Scale Y"), "keys on {lane}: the height can be keyed apart");
        }
    }

    #[test]
    fn the_tilt_rows_keyframe_button_keys_the_tilt_track() {
        let keyframe_tolerance = 0.5 / 24.0;
        let clip_id = klipp_core::ClipId::new();
        let mut r = crate::widgets::scrubby_param::ScrubbyParamResponse {
            changed: false, drag_started: false, drag_stopped: false, lost_focus: false, reset_clicked: false,
            keyframe_toggled: true, keyframe_nav_prev: false, keyframe_nav_next: false,
            keyframe_set_interpolation: None, keyframe_ease_preset: None,
        };
        let ks = keyframe_state(&[], 1.0, keyframe_tolerance);
        let mut cmds = Vec::new();
        keyframe_controls(&mut cmds, &r, &[], &ks, VideoParam::TiltY, 30.0, clip_id, 1.0, (0.0, 24), keyframe_tolerance);
        assert!(matches!(cmds[0], AppCommand::AddVideoKeyframe { param: VideoParam::TiltY, value, .. } if value == 30.0));
        assert!(matches!(cmds[1], AppCommand::SetVideoRubberBandParam(VideoParam::TiltY)));

        // With a key already there, the button takes it off.
        let key = klipp_core::Keyframe { time_secs: 1.0, value: 30.0, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) };
        cmds.clear();
        keyframe_controls(&mut cmds, &r, std::slice::from_ref(&key), &keyframe_state(std::slice::from_ref(&key), 1.0, keyframe_tolerance), VideoParam::TiltY, 30.0, clip_id, 1.0, (0.0, 24), keyframe_tolerance);
        assert!(matches!(cmds[0], AppCommand::RemoveVideoKeyframe { param: VideoParam::TiltY, .. }));

        // Nothing clicked, nothing sent.
        r.keyframe_toggled = false;
        cmds.clear();
        keyframe_controls(&mut cmds, &r, &[], &ks, VideoParam::TiltX, 0.0, clip_id, 1.0, (0.0, 24), keyframe_tolerance);
        assert!(cmds.is_empty());
    }
}

/// The opacity's presets and Save, under its row: what the Opacity card
/// offers, for the clip's opacity property. A preset is sent as the
/// opacity entry's update, which the Video tab turns into the opacity
/// anchor's one path edit.
fn opacity_presets(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip_id: klipp_core::ClipId, index: usize, current: f32, cmds: &mut Vec<AppCommand>) {
    const OPACITY: &str = "klipp.fx.opacity";
    let presets = state.preset_manager.presets_for_type(OPACITY);
    let id = ui.make_persistent_id(("opacity_presets", clip_id));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Preset").size(10.0).color(theme.text_dim));
        ui.add_space(4.0);
        egui::ComboBox::from_id_salt(id.with("combo"))
            .selected_text("Select preset...")
            .width((ui.available_width() - 80.0).max(60.0))
            .show_ui(ui, |ui| {
                let (builtins, user): (Vec<&crate::effect_presets::EffectPreset>, Vec<&crate::effect_presets::EffectPreset>) = presets.iter().copied().partition(|p| p.is_builtin);
                for (i, preset) in builtins.iter().chain(user.iter()).enumerate() {
                    if i == builtins.len() && !builtins.is_empty() {
                        ui.separator();
                    }
                    ui.horizontal(|ui| {
                        let r = ui.selectable_label(false, &preset.name);
                        let r = if preset.description.is_empty() { r } else { r.on_hover_text(&preset.description) };
                        if r.clicked() {
                            if let Some(effect) = preset.builtin.clone() {
                                cmds.push(AppCommand::UpdateBuiltinEffect { clip_id, index, effect });
                            }
                        }
                        if !preset.is_builtin && ui.small_button("\u{00d7}").on_hover_text("Delete preset").clicked() {
                            cmds.push(AppCommand::DeleteBuiltinEffectPreset { effect_key: preset.effect_key.clone(), name: preset.name.clone() });
                        }
                    });
                }
            });
        let popup = id.with("save");
        let button = ui.small_button("Save...");
        if button.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup));
        }
        egui::popup_below_widget(ui, popup, &button, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(200.0);
            ui.label(egui::RichText::new("Save Preset").size(11.0).strong());
            let buffer = id.with("name");
            let mut name: String = ui.data_mut(|d| d.get_temp(buffer).unwrap_or_default());
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut name);
            });
            ui.data_mut(|d| d.insert_temp(buffer, name.clone()));
            if ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Save")).clicked() {
                cmds.push(AppCommand::SaveBuiltinEffectPreset { name: name.trim().to_owned(), description: String::new(), effect_key: OPACITY.to_owned(), effect: BuiltinEffect::Opacity { value: current } });
                ui.data_mut(|d| d.remove::<String>(buffer));
                ui.memory_mut(|m| m.close_popup());
            }
        });
    });
}

#[cfg(test)]
mod row_style_tests {
    use super::*;
    use crate::inspector::rows::test_frame::{click, frame};
    use klipp_core::RationalTime;

    fn media_clip() -> Clip {
        let t = |s: f64| RationalTime::from_secs_f64(s, 24);
        Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))
    }

    /// Where a row's right-hand control sits: the panel's edge less its
    /// margin, the section's padding and half the control.
    fn right_control(texts: &[(String, egui::Pos2)], label: &str, half: f32) -> egui::Pos2 {
        let at = texts.iter().find(|(t, _)| t == label).unwrap_or_else(|| panic!("no {label}: {texts:?}")).1;
        egui::pos2(360.0 - 8.0 - 14.0 - half, at.y)
    }

    /// Every row the Video tab had for a clip is still there, each once,
    /// and the card of fixed "100 %" speeds and a "Linear" ease that
    /// changed nothing is gone.
    #[test]
    fn every_control_is_kept_and_nothing_fake_is_drawn() {
        let ctx = egui::Context::default();
        let state = AppState::default();
        let clip = media_clip();
        let (texts, _) = frame(&ctx, Vec::new(), |ui, theme, cmds| show_clip_video(ui, theme, &state, &clip, cmds, 0.5, true, false));
        let all: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        for label in [
            "ROLE", "TRANSFORM", "Position X", "Position Y", "Scale", "Rotation", "Tilt X", "Tilt Y", "Source rotation", "Depth", "Opacity",
            "TRACKING", "COLOUR", "Grade", "Open Colour Panel", "COMPOSITING", "COLOUR FADE", "Fade from colour", "Fade to colour",
            "TRANSFORM LINK", "HYPERZOOM", "KEN BURNS", "Zoom In", "BEHAVIORS", "SPEED", "Rate", "Direction", "Keep pitch",
            "SPEED RAMP", "Speed ramp",
        ] {
            assert!(all.contains(&label), "{label} is no longer shown: {all:?}");
        }
        assert_eq!(all.iter().filter(|t| **t == "SPEED RAMP").count(), 1, "one speed ramp section: {all:?}");
        for fake in ["In speed", "Out speed", "Ease"] {
            assert!(!all.contains(&fake), "{fake} was a painted value, not a setting: {all:?}");
        }
        assert_eq!(all.iter().filter(|t| **t == "100 %").count(), 1, "the Rate's own value only: {all:?}");
    }

    /// No rule between the rows of the restyled sections: between the
    /// TRANSFORM header and the next section's, and through SPEED and
    /// SPEED RAMP, the only hairlines are the headers' own.
    #[test]
    fn no_rule_between_rows() {
        let ctx = egui::Context::default();
        let state = AppState::default();
        let clip = media_clip();
        let theme = KlippTheme::default();
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 6000.0))), ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| show_clip_video(ui, &theme, &state, &clip, &mut Vec::new(), 0.5, true, false));
        });
        let y_of = |label: &str| out.shapes.iter().find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == label => Some(t.pos.y),
            _ => None,
        }).unwrap_or_else(|| panic!("no {label}"));
        let rules_between = |a: f32, b: f32| out.shapes.iter().filter(|s| match &s.shape {
            egui::Shape::LineSegment { points, .. } => points[0].y == points[1].y && points[0].y > a + 24.0 && points[0].y < b - 4.0,
            _ => false,
        }).count();
        assert_eq!(rules_between(y_of("TRANSFORM"), y_of("KEN BURNS")), 0, "rules inside TRANSFORM");
        assert_eq!(rules_between(y_of("SPEED"), y_of("TRACKING")), 1, "only SPEED RAMP's header rule from SPEED to TRACKING");
        assert_eq!(rules_between(y_of("COLOUR"), y_of("COMPOSITING")), 0, "rules inside COLOUR");
        assert_eq!(rules_between(y_of("COLOUR FADE"), y_of("TRANSFORM LINK")), 0, "rules inside COLOUR FADE");
        // From TRACKING on, the only rules are the five headers after it
        // (COLOUR, COMPOSITING, COLOUR FADE, TRANSFORM LINK, HYPERZOOM).
        assert_eq!(rules_between(y_of("TRACKING"), f32::MAX), 5, "rules between the rows of the later sections");
    }

    /// The switches and the keyframe mark answer real clicks with the
    /// commands they did before.
    #[test]
    fn the_controls_do_what_they_say() {
        let ctx = egui::Context::default();
        let state = AppState::default();
        let clip = media_clip();
        let mut draw = |ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>| show_clip_video(ui, theme, &state, &clip, cmds, 0.5, true, false);
        let (texts, _) = frame(&ctx, Vec::new(), &mut draw);
        // Every frame tells the viewer which matte to preview; that is
        // not what a click sends.
        let click = |at, draw: &mut dyn FnMut(&mut egui::Ui, &KlippTheme, &mut Vec<AppCommand>)| {
            let mut cmds = click(&ctx, at, draw);
            cmds.retain(|c| !matches!(c, AppCommand::SetInspectorMattePreviewTarget(_)));
            cmds
        };

        let cmds = click(right_control(&texts, "Keep pitch", 16.0), &mut draw);
        assert!(matches!(cmds.as_slice(), [AppCommand::SetClipKeepPitch { keep_pitch: true, .. }]), "{cmds:?}");

        let cmds = click(right_control(&texts, "Speed ramp", 16.0), &mut draw);
        assert!(matches!(cmds.as_slice(), [AppCommand::SetSpeedKeyframes { keyframes, .. }] if keyframes.len() == 2), "{cmds:?}");

        let cmds = click(right_control(&texts, "Fade from colour", 16.0), &mut draw);
        assert!(matches!(cmds.as_slice(), [AppCommand::AddBuiltinEffect { effect: BuiltinEffect::ColorFade { mode: klipp_effects::ColorFadeMode::In, .. }, .. }]), "{cmds:?}");

        let cmds = click(right_control(&texts, "Position X", 8.0), &mut draw);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::AddVideoKeyframe { param: VideoParam::PositionX, .. })), "{cmds:?}");

        // The blend mode's menu, opened from its row's right-hand side,
        // picks a mode.
        let normal = texts.iter().find(|(t, p)| t == "Normal" && (p.y - right_control(&texts, "Blend Mode", 0.0).y).abs() < 4.0).expect("the blend mode's value").1;
        assert!(normal.x > 180.0, "the value sits on the right: {normal:?}");
        let cmds = click(normal, &mut draw);
        assert!(cmds.is_empty(), "{cmds:?}");
        let (open, _) = frame(&ctx, Vec::new(), &mut draw);
        let multiply = open.iter().find(|(t, _)| t == "Multiply").expect("the blend menu").1;
        let cmds = click(multiply, &mut draw);
        assert!(matches!(cmds.as_slice(), [AppCommand::SetClipBlendMode { mode: klipp_core::BlendMode::Multiply, .. }]), "{cmds:?}");
    }
}

#[cfg(test)]
mod frame_glide_section_tests {
    use super::*;
    use klipp_core::frame_glide::{FrameGlide, GlideFrame};

    /// The Video tab for `clip` in a sequence of its own, with `events`:
    /// the commands pushed and the texts drawn.
    fn frame(ctx: &egui::Context, clip: &Clip, events: Vec<egui::Event>) -> (Vec<AppCommand>, Vec<(String, egui::Pos2)>) {
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips = vec![clip.clone()];
        let mut cmds = Vec::new();
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 9000.0))), events, ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| show(ui, &KlippTheme::default(), &state, Some((clip, 0.0, true)), None, &mut cmds));
        });
        let texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        (cmds, texts)
    }

    fn click(ctx: &egui::Context, clip: &Clip, label: &str) -> Vec<AppCommand> {
        frame(ctx, clip, Vec::new());
        let texts = frame(ctx, clip, Vec::new()).1;
        let pos = texts.iter().find(|(t, _)| t == label).map(|(_, p)| *p).unwrap_or_else(|| panic!("{label} is drawn: {:?}", texts.iter().map(|t| &t.0).collect::<Vec<_>>()));
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = frame(ctx, clip, vec![egui::Event::PointerMoved(pos), button(true)]).0;
        cmds.extend(frame(ctx, clip, vec![button(false)]).0);
        // The tab says each frame which matte the viewer previews: not the
        // click's.
        cmds.retain(|c| !matches!(c, AppCommand::SetInspectorMattePreviewTarget(_)));
        cmds
    }

    fn media() -> Clip {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0))
    }

    /// #611: the Video tab enters Frame Glide for the clip; a glided clip
    /// has Swap, Reset and Remove, each one edit; a clip animated by hand
    /// is glided only by replacing its animation, said so.
    #[test]
    fn the_frame_glide_section_frames_swaps_resets_and_asks_before_replacing() {
        let ctx = egui::Context::default();
        let plain = media();
        let cmds = click(&ctx, &plain, "Frame in Viewer");
        assert!(matches!(&cmds[..], [AppCommand::SetFrameGlideMode(Some(id))] if *id == plain.id), "{cmds:?}");

        let glide = FrameGlide { start: GlideFrame::FULL, end: GlideFrame { x: 0.1, y: 0.0, width: 0.5 }, ..FrameGlide::default() };
        let (w, h) = (1920.0, 1080.0);
        let glided = klipp_timeline::frame_glide::set(&plain, Some(glide), false, w, h).unwrap();
        let cmds = click(&ctx, &glided, "Swap Start/End");
        assert!(matches!(&cmds[..], [AppCommand::SetFrameGlide { glide: Some(g), replace_animation: false, .. }] if g.start == glide.end && g.end == glide.start), "{cmds:?}");
        let cmds = click(&ctx, &glided, "Reset");
        assert!(matches!(&cmds[..], [AppCommand::SetFrameGlide { glide: Some(g), .. }] if g.start == GlideFrame::FULL && g.end == GlideFrame::FULL), "{cmds:?}");
        let cmds = click(&ctx, &glided, "Remove Glide");
        assert!(matches!(&cmds[..], [AppCommand::SetFrameGlide { glide: None, .. }]), "{cmds:?}");

        let mut animated = media();
        // Its position and scale keyed by hand.
        klipp_timeline::frame_glide::hold_at_rest(&mut animated);
        let (_, texts) = frame(&ctx, &animated, Vec::new());
        assert!(!texts.iter().any(|(t, _)| t == "Frame in Viewer"), "not glided over without asking");
        let cmds = click(&ctx, &animated, "Replace Animation with a Glide");
        assert!(matches!(&cmds[..], [AppCommand::SetFrameGlide { replace_animation: true, .. }, AppCommand::SetFrameGlideMode(Some(_))]), "{cmds:?}");
    }
}
