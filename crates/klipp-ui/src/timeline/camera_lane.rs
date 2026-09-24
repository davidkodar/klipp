//! The sequence camera's keys on a lane of their own, under the ruler.
//!
//! A camera key belongs to the sequence, not to a clip, so it has no
//! clip rectangle to be drawn inside of and no track to be scrolled
//! with. The lane sits between the ruler and the tracks, one row per
//! moving value (X, Y, Zoom, Roll), and shows every key at its stored
//! moment. Keys are selected by the same rule as the inspector's rows:
//! the nearest key within the displayed frame, edited at its own
//! timestamp, never moved by being looked at. Dragging moves a key in
//! time only; its value is the inspector's and the curve editor's.
use crate::inspector::keyframe_selection::{frame_tolerance, nearest, timeline_key_time};
use crate::state::{AppState, CameraKeyMenu, CameraKeyRef, DraggingCameraKey};
use crate::theme::KlippTheme;
use crate::timeline::time_utils;
use klipp_core::keyframe::{evaluate, Ease, Interpolation, KEYFRAME_TIME_EPSILON};
use klipp_core::{AppCommand, CameraParam, Keyframe};
use klipp_timeline::Camera;

/// The four rows, top to bottom.
pub const PARAMS: [CameraParam; 4] = [CameraParam::X, CameraParam::Y, CameraParam::Zoom, CameraParam::Roll];
/// One row of the lane.
pub const ROW_HEIGHT: f32 = 13.0;
/// The lane when shown: four rows and a rule under them.
pub const HEIGHT: f32 = ROW_HEIGHT * 4.0 + 2.0;
/// How near, in pixels, the pointer must be to a diamond when the
/// frame window is narrower than that.
const HIT_PX: f32 = 6.0;
/// How near, in pixels, a dragged key snaps to the playhead or to
/// another key.
const SNAP_PX: f32 = 8.0;
const DIAMOND: f32 = 4.0;

/// The lane's height in the timeline this frame: nothing when hidden.
pub fn height(state: &AppState) -> f32 {
    if state.show_camera_lane { HEIGHT } else { 0.0 }
}

/// The list a param names.
pub fn keys(camera: &Camera, param: CameraParam) -> &[Keyframe] {
    match param {
        CameraParam::X => &camera.x_keyframes,
        CameraParam::Y => &camera.y_keyframes,
        CameraParam::Zoom => &camera.zoom_keyframes,
        CameraParam::Roll => &camera.roll_keyframes,
    }
}

/// The list a param names, to edit.
pub fn keys_mut(camera: &mut Camera, param: CameraParam) -> &mut Vec<Keyframe> {
    match param {
        CameraParam::X => &mut camera.x_keyframes,
        CameraParam::Y => &mut camera.y_keyframes,
        CameraParam::Zoom => &mut camera.zoom_keyframes,
        CameraParam::Roll => &mut camera.roll_keyframes,
    }
}

/// Whether the camera carries any key at all.
pub fn has_keys(camera: &Camera) -> bool {
    PARAMS.iter().any(|p| !keys(camera, *p).is_empty())
}

/// The colour a row's keys are drawn in, the same on the lane, in its
/// header and in the curve editor.
pub fn colour(param: CameraParam, theme: &KlippTheme) -> egui::Color32 {
    match param {
        CameraParam::X => theme.accent,
        CameraParam::Y => theme.marker_info,
        CameraParam::Zoom => theme.marker_success,
        CameraParam::Roll => theme.marker_warning,
    }
}

/// Which row a param is on.
pub fn row_of(param: CameraParam) -> usize {
    PARAMS.iter().position(|p| *p == param).unwrap_or(0)
}

/// The row under a y position within the lane.
pub fn param_at_y(lane_rect: egui::Rect, y: f32) -> Option<CameraParam> {
    if y < lane_rect.top() || y >= lane_rect.bottom() {
        return None;
    }
    let row = ((y - lane_rect.top()) / ROW_HEIGHT).floor() as usize;
    PARAMS.get(row).copied()
}

/// The centre line of a row.
pub fn row_y(lane_rect: egui::Rect, param: CameraParam) -> f32 {
    lane_rect.top() + (row_of(param) as f32 + 0.5) * ROW_HEIGHT
}

/// The index of the key stored at exactly that moment, if one is.
pub fn index_at(keys: &[Keyframe], time_secs: f64) -> Option<usize> {
    keys.iter().position(|k| (k.time_secs - time_secs).abs() < KEYFRAME_TIME_EPSILON)
}

/// How far from the pointer's moment a key may be and still be the one
/// under it: half a displayed frame, as the inspector's rows use, or a
/// few pixels when the timeline is zoomed out so far that half a frame
/// is thinner than the diamond.
pub fn hit_window(pps: f32, frame_tolerance: f64) -> f64 {
    let pixels = (HIT_PX / pps.max(f32::EPSILON)) as f64;
    pixels.max(frame_tolerance)
}

/// The key under the pointer on one row: the nearest within the window,
/// named by its stored moment.
pub fn key_under(keys: &[Keyframe], time_secs: f64, window: f64) -> Option<f64> {
    nearest(keys, time_secs, window).map(|i| keys[i].time_secs)
}

/// Where a dragged key lands: the pointer's moment, snapped to the
/// nearest target within `threshold` if any, and kept within the
/// sequence.
pub fn drop_time(raw_secs: f64, targets: &[f64], threshold: f64, duration_secs: f64) -> f64 {
    let snapped = targets
        .iter()
        .copied()
        .filter(|t| (t - raw_secs).abs() <= threshold)
        .min_by(|a, b| (a - raw_secs).abs().total_cmp(&(b - raw_secs).abs()))
        .unwrap_or(raw_secs);
    snapped.clamp(0.0, duration_secs.max(0.0))
}

/// What a dragged key snaps to: the playhead, the start, and every
/// other camera key, on any row, except the one being dragged.
pub fn snap_targets(camera: &Camera, playhead_secs: f64, dragged: CameraKeyRef) -> Vec<f64> {
    let mut targets = vec![0.0, playhead_secs];
    for p in PARAMS {
        for k in keys(camera, p) {
            if p == dragged.param && (k.time_secs - dragged.time_secs).abs() < KEYFRAME_TIME_EPSILON {
                continue;
            }
            targets.push(k.time_secs);
        }
    }
    targets
}

/// The lane itself: the rows, the keys, the playhead, and what the
/// pointer does to them.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    lane_rect: egui::Rect,
    pps: f32,
    scroll_x: f32,
    cmds: &mut Vec<AppCommand>,
) {
    let sequence_idx = state.active_sequence_idx;
    let (fps, tolerance, camera) = {
        let seq = state.sequence();
        (seq.fps.as_f64(), frame_tolerance(seq), seq.camera.clone())
    };
    let duration_secs = state.sequence_duration().to_secs_f64();
    let playhead_secs = state.playhead.to_secs_f64();
    let window = hit_window(pps, tolerance);
    let to_time = |x: f32| time_utils::x_to_time(x - lane_rect.left(), pps, scroll_x);
    let to_x = |t: f64| lane_rect.left() + time_utils::time_to_x(t, pps, scroll_x);

    let painter = ui.painter().with_clip_rect(lane_rect);
    painter.rect_filled(lane_rect, 0.0, theme.bg_deep);
    for (i, _) in PARAMS.iter().enumerate() {
        let y = lane_rect.top() + (i as f32 + 1.0) * ROW_HEIGHT;
        painter.hline(lane_rect.x_range(), y, egui::Stroke::new(0.5_f32, theme.border.gamma_multiply(0.6)));
    }
    painter.hline(lane_rect.x_range(), lane_rect.bottom() - 0.5, egui::Stroke::new(1.0_f32, theme.border));

    let resp = ui.interact(lane_rect, ui.id().with("sequence_camera_lane"), egui::Sense::click_and_drag());
    let pointer = resp.hover_pos().filter(|p| lane_rect.contains(*p));
    let hovered: Option<CameraKeyRef> = pointer.and_then(|p| {
        let param = param_at_y(lane_rect, p.y)?;
        key_under(keys(&camera, param), to_time(p.x), window).map(|time_secs| CameraKeyRef { param, time_secs })
    });
    let alt = ui.input(|i| i.modifiers.alt);

    // Keys, at their stored moments.
    for param in PARAMS {
        let y = row_y(lane_rect, param);
        let tint = colour(param, theme);
        for k in keys(&camera, param) {
            let x = to_x(k.time_secs);
            if x < lane_rect.left() - DIAMOND || x > lane_rect.right() + DIAMOND {
                continue;
            }
            let this = CameraKeyRef { param, time_secs: k.time_secs };
            let selected = state.selected_camera_key == Some(this);
            let hot = hovered == Some(this);
            let fill = if selected { theme.text_primary } else if hot { tint.gamma_multiply(1.3) } else { tint };
            let stroke = if selected { egui::Stroke::new(1.0_f32, tint) } else { egui::Stroke::new(0.5_f32, theme.bg_deep) };
            let r = if selected || hot { DIAMOND + 1.0 } else { DIAMOND };
            painter.add(egui::Shape::convex_polygon(
                vec![egui::pos2(x, y - r), egui::pos2(x + r, y), egui::pos2(x, y + r), egui::pos2(x - r, y)],
                fill,
                stroke,
            ));
        }
    }

    // The playhead, through the lane.
    let px = to_x(playhead_secs);
    if px >= lane_rect.left() && px <= lane_rect.right() {
        painter.vline(px, lane_rect.y_range(), egui::Stroke::new(1.0_f32, theme.accent));
    }

    if let Some(key) = hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        resp.clone().on_hover_text(format!("{} at {:.3} s\nDrag to move, double-click to remove", key.param.label(), key.time_secs));
    } else if pointer.is_some() && alt {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }

    // Removing comes before selecting: the second click of a double
    // click also reports as a click.
    if resp.double_clicked() {
        if let Some(key) = hovered {
            cmds.push(AppCommand::RemoveSequenceCameraKey { sequence_idx, param: key.param, time_secs: key.time_secs });
            state.selected_camera_key = None;
        }
    } else if resp.clicked() {
        if let (Some(p), true) = (pointer, alt) {
            if let Some(param) = param_at_y(lane_rect, p.y) {
                let list = keys(&camera, param);
                let time_secs = timeline_key_time(list, to_time(p.x), fps).clamp(0.0, duration_secs.max(0.0));
                let value = evaluate(list, time_secs, param.resting());
                cmds.push(AppCommand::SetSequenceCameraKey { sequence_idx, param, time_secs, value });
                state.selected_camera_key = Some(CameraKeyRef { param, time_secs });
            }
        } else {
            state.selected_camera_key = hovered;
        }
    }
    if resp.secondary_clicked() {
        if let (Some(key), Some(p)) = (hovered, pointer) {
            state.selected_camera_key = Some(key);
            state.camera_key_menu = Some(CameraKeyMenu { key, screen_pos: p });
        }
    }

    // A drag moves the key along the lane, in time only.
    if resp.drag_started_by(egui::PointerButton::Primary) && !alt {
        if let Some(key) = hovered {
            state.selected_camera_key = Some(key);
            state.dragging_camera_key = Some(DraggingCameraKey { key, handle: None });
            cmds.push(AppCommand::BeginSequenceCameraDrag { sequence_idx });
        }
    }
    if resp.dragged_by(egui::PointerButton::Primary) {
        if let (Some(drag), Some(p)) = (state.dragging_camera_key, ui.input(|i| i.pointer.latest_pos())) {
            if drag.handle.is_none() {
                let list = keys(&camera, drag.key.param);
                if let Some(index) = index_at(list, drag.key.time_secs) {
                    let threshold = (SNAP_PX / pps.max(f32::EPSILON)) as f64;
                    let targets = snap_targets(&camera, playhead_secs, drag.key);
                    let new_time_secs = drop_time(to_time(p.x), &targets, threshold, duration_secs);
                    if (new_time_secs - drag.key.time_secs).abs() >= KEYFRAME_TIME_EPSILON {
                        cmds.push(AppCommand::MoveSequenceCameraKey {
                            sequence_idx,
                            param: drag.key.param,
                            index,
                            new_time_secs,
                            new_value: list[index].value,
                        });
                        let moved = CameraKeyRef { param: drag.key.param, time_secs: new_time_secs };
                        state.dragging_camera_key = Some(DraggingCameraKey { key: moved, handle: None });
                        state.selected_camera_key = Some(moved);
                    }
                }
            }
        }
    }
    if resp.drag_stopped() && state.dragging_camera_key.take().is_some() {
        cmds.push(AppCommand::EndSequenceCameraDrag { sequence_idx });
    }
}

/// The lane's cell in the track header column: its name and the four
/// row labels in their colours.
pub fn draw_header(ui: &mut egui::Ui, theme: &KlippTheme, rect: egui::Rect) {
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, theme.bg_deep);
    painter.hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0_f32, theme.border));
    painter.text(
        egui::pos2(rect.min.x + 6.0, rect.top() + ROW_HEIGHT * 0.5),
        egui::Align2::LEFT_CENTER,
        "CAMERA",
        egui::FontId::monospace(9.0),
        theme.text_dim,
    );
    for param in PARAMS {
        painter.text(
            egui::pos2(rect.max.x - 6.0, row_y(rect, param)),
            egui::Align2::RIGHT_CENTER,
            param.label(),
            egui::FontId::monospace(9.0),
            colour(param, theme),
        );
    }
}

/// The diamond in the ruler spacer that shows or hides the lane. Lit
/// when the camera has keys, so a move set over the socket is not
/// invisible until someone thinks to look.
pub fn header_toggle(ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, spacer_rect: egui::Rect) {
    let keyed = has_keys(&state.sequence().camera);
    let rect = egui::Rect::from_center_size(egui::pos2(spacer_rect.max.x - 12.0, spacer_rect.center().y), egui::vec2(16.0, 16.0));
    let resp = ui.interact(rect, ui.id().with("sequence_camera_lane_toggle"), egui::Sense::click());
    let tint = if state.show_camera_lane { theme.accent } else if keyed { theme.accent_dim } else { theme.text_dim };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        if keyed { "\u{25C6}" } else { "\u{25C7}" },
        egui::FontId::proportional(11.0),
        tint,
    );
    let resp = resp.on_hover_text(if state.show_camera_lane { "Hide the camera's keys" } else { "Show the camera's keys" });
    if resp.clicked() {
        state.show_camera_lane = !state.show_camera_lane;
        if !state.show_camera_lane {
            state.camera_key_menu = None;
        }
    }
}

/// The right-click menu on a camera key, from the lane or the curve
/// editor: the curve out of it, a named ease into it, or removal.
/// Drawn once a frame, by the app, wherever it was opened.
pub fn show_key_menu(ui: &mut egui::Ui, state: &mut AppState, cmds: &mut Vec<AppCommand>) {
    let Some(menu) = state.camera_key_menu else { return };
    let sequence_idx = state.active_sequence_idx;
    let list = keys(&state.sequence().camera, menu.key.param);
    let Some(index) = index_at(list, menu.key.time_secs) else {
        state.camera_key_menu = None;
        return;
    };
    let current = list[index].interpolation;
    let has_previous = index > 0;
    let mut close = false;
    let area = egui::Area::new(egui::Id::new("sequence_camera_key_menu"))
        .fixed_pos(menu.screen_pos)
        .order(egui::Order::Foreground)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            egui::Frame::menu(ui.style()).inner_margin(egui::Margin::same(4.0)).show(ui, |ui| {
                ui.set_min_width(150.0);
                ui.label(egui::RichText::new(format!("{} key at {:.3} s", menu.key.param.label(), menu.key.time_secs)).size(10.0).weak());
                ui.separator();
                for (label, interpolation) in [("Linear", Interpolation::Linear), ("Hold", Interpolation::Step), ("Bezier", Interpolation::Bezier)] {
                    if ui.selectable_label(current == interpolation, label).clicked() {
                        cmds.push(AppCommand::SetSequenceCameraKeyInterpolation { sequence_idx, param: menu.key.param, index, interpolation });
                        close = true;
                    }
                }
                ui.separator();
                ui.add_enabled_ui(has_previous, |ui| {
                    ui.label(egui::RichText::new("Ease into this key").size(10.0).weak());
                    for (label, ease) in [
                        ("Ease in", Ease::In),
                        ("Ease out", Ease::Out),
                        ("Ease in and out", Ease::InOut),
                        ("Expo out", Ease::ExpoOut),
                        ("Back out", Ease::BackOut),
                    ] {
                        if ui.button(label).clicked() {
                            cmds.push(AppCommand::EaseSequenceCameraKey { sequence_idx, param: menu.key.param, index, ease });
                            close = true;
                        }
                    }
                });
                ui.separator();
                if ui.button("Remove key").clicked() {
                    cmds.push(AppCommand::RemoveSequenceCameraKey { sequence_idx, param: menu.key.param, time_secs: menu.key.time_secs });
                    state.selected_camera_key = None;
                    close = true;
                }
            });
        });
    let clicked_away = ui.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer();
    if close || clicked_away || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.camera_key_menu = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::keyframe::upsert_keyframe;
    use klipp_core::Fps;
    use klipp_timeline::Sequence;

    fn tolerance_at(fps: Fps) -> f64 {
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        frame_tolerance(&seq)
    }

    fn list(times: &[f64]) -> Vec<Keyframe> {
        let mut keys = Vec::new();
        for &t in times {
            upsert_keyframe(&mut keys, t, 0.5);
        }
        keys
    }

    /// A key at 0.7 s in a 24 fps sequence, with the pointer on frame 17
    /// (0.708333…): the key under the pointer is the one at 0.7, named
    /// by 0.7, so what the lane emits goes to the key and not beside it.
    #[test]
    fn the_key_under_the_pointer_is_named_by_its_stored_moment() {
        let keys = list(&[0.7]);
        let window = hit_window(200.0, tolerance_at(Fps::film_24()));
        assert_eq!(key_under(&keys, 17.0 / 24.0, window), Some(0.7));
        assert_eq!(index_at(&keys, 0.7), Some(0));
        assert_eq!(index_at(&keys, 17.0 / 24.0), None, "the frame time is not the key's identity");
    }

    /// Zoomed out until half a frame is thinner than a pixel, the
    /// diamond is still hittable within its own width; zoomed in until a
    /// frame is wide, the window is the frame, as the inspector's is.
    #[test]
    fn the_hit_window_is_the_wider_of_half_a_frame_and_the_diamond() {
        let half_frame = tolerance_at(Fps::film_24());
        assert_eq!(hit_window(2000.0, half_frame), half_frame, "at 2000 px/s six pixels are 3 ms, less than half a frame");
        let wide = hit_window(2.0, half_frame);
        assert!(wide > half_frame && (wide - 3.0).abs() < 1e-9, "at 2 px/s six pixels are three seconds: {wide}");
    }

    /// Consecutive keys on a fast timeline are each the one under their
    /// own frame, never a neighbour.
    #[test]
    fn keys_a_frame_apart_are_told_apart() {
        let fps = 120.0;
        let times: Vec<f64> = (0..12).map(|i| i as f64 / fps).collect();
        let keys = list(&times);
        let window = hit_window(2000.0, 0.5 / fps);
        for (i, &t) in times.iter().enumerate() {
            assert_eq!(key_under(&keys, t + 0.2 / fps, window), Some(t), "frame {i}");
        }
    }

    /// A dragged key lands on the playhead or another key when within
    /// the snap threshold, on the pointer when not, and never outside
    /// the sequence.
    #[test]
    fn a_dragged_key_snaps_within_the_threshold_and_stays_inside() {
        let targets = [0.0, 2.0, 3.5];
        assert_eq!(drop_time(2.03, &targets, 0.05, 10.0), 2.0);
        assert_eq!(drop_time(2.3, &targets, 0.05, 10.0), 2.3);
        assert_eq!(drop_time(3.46, &targets, 0.05, 10.0), 3.5);
        assert_eq!(drop_time(-0.2, &targets, 0.05, 10.0), 0.0);
        assert_eq!(drop_time(12.0, &targets, 0.05, 10.0), 10.0);
    }

    /// The dragged key is not its own snap target, its neighbours on
    /// every row are.
    #[test]
    fn snap_targets_are_every_other_key_and_the_playhead() {
        let mut cam = Camera::default();
        upsert_keyframe(&mut cam.x_keyframes, 1.0, 0.5);
        upsert_keyframe(&mut cam.x_keyframes, 2.0, 0.5);
        upsert_keyframe(&mut cam.roll_keyframes, 4.0, 10.0);
        let dragged = CameraKeyRef { param: CameraParam::X, time_secs: 1.0 };
        let mut targets = snap_targets(&cam, 3.0, dragged);
        targets.sort_by(f64::total_cmp);
        assert_eq!(targets, vec![0.0, 2.0, 3.0, 4.0]);
    }

    /// The pointer's row is one of the four, top to bottom, and nothing
    /// outside the lane.
    #[test]
    fn rows_are_the_four_params_in_order() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(300.0, HEIGHT));
        assert_eq!(param_at_y(rect, 100.5), Some(CameraParam::X));
        assert_eq!(param_at_y(rect, 100.0 + ROW_HEIGHT * 1.5), Some(CameraParam::Y));
        assert_eq!(param_at_y(rect, 100.0 + ROW_HEIGHT * 2.5), Some(CameraParam::Zoom));
        assert_eq!(param_at_y(rect, 100.0 + ROW_HEIGHT * 3.5), Some(CameraParam::Roll));
        assert_eq!(param_at_y(rect, 99.0), None);
        assert_eq!(param_at_y(rect, 100.0 + HEIGHT), None);
    }
}
