//! A selected clip's position keys as a path on the picture (#2276).
//!
//! The clip's placement position, Position X and Y as its picture keys
//! them, drawn where it puts the clip's anchor over the keyed range: a
//! line through the frames, a tick at each frame (close where it moves
//! slowly, apart where fast), a dot at each key, and at a key both lanes
//! have, its Bezier handles. A dot dragged moves the key, a handle bends
//! the path through it; both write the same keys the Curve Editor shows,
//! the gesture one undo step (an edit over the placement node's values,
//! as the transform gizmo's).

use egui::{pos2, vec2, Color32, Pos2, Rect, Stroke};
use klipp_core::{AppCommand, Keyframe};

use crate::state::AppState;

/// What of a key a drag holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Part {
    Key,
    Out,
    In,
}

/// A drag on the path: the key's time (clip time), what of it is held,
/// and whether its edit was opened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionPathDrag {
    pub time: f64,
    pub part: Part,
    /// Where the press was: a press that doesn't move changes nothing.
    pub from: Pos2,
    pub started: bool,
}

/// How near the pointer must be to a dot or a handle to take it, in points.
const GRAB: f32 = 7.0;

/// The keys on `lane` at `t`, if one is there.
fn key_at(keys: &[Keyframe], t: f64) -> Option<&Keyframe> {
    keys.iter().find(|k| (k.time_secs - t).abs() < klipp_core::keyframe::KEYFRAME_TIME_EPSILON)
}

/// The key times of both lanes, in order, each once.
fn key_times(xs: &[Keyframe], ys: &[Keyframe]) -> Vec<f64> {
    let mut times: Vec<f64> = xs.iter().chain(ys).map(|k| k.time_secs).collect();
    times.sort_by(f64::total_cmp);
    times.dedup_by(|a, b| (*a - *b).abs() < klipp_core::keyframe::KEYFRAME_TIME_EPSILON);
    times
}

/// Draw the selected clip's motion path over `image_rect` (the picture,
/// `frame_w × frame_h` frame pixels), and take a drag on it. `true` while
/// the pointer is on the path's dots or handles or a drag of it runs: the
/// transform gizmo leaves the press alone then.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, image_rect: Rect, frame_w: f32, frame_h: f32, cmds: &mut Vec<AppCommand>) -> bool {
    let Some(clip_id) = state.selected_clip else { return false };
    let Some(clip) = state.sequence().all_clips().find(|c| c.id == clip_id).cloned() else { return false };
    let Some(addr) = clip.placement_path() else { return false };
    let xs = crate::inspector::placement_by_path::lane_keys(&clip, "pos_x");
    let ys = crate::inspector::placement_by_path::lane_keys(&clip, "pos_y");
    let times = key_times(&xs, &ys);
    if times.len() < 2 {
        state.motion_path_drag = None;
        return false;
    }
    let now = crate::inspector::placement_by_path::placement(&clip);
    let (sx, sy) = (image_rect.width() / frame_w.max(1.0), image_rect.height() / frame_h.max(1.0));
    let centre = image_rect.center();
    let at = |t: f64| (klipp_core::keyframe::evaluate(&xs, t, now.position_x), klipp_core::keyframe::evaluate(&ys, t, now.position_y));
    let screen = |(x, y): (f32, f32)| pos2(centre.x + x * sx, centre.y + y * sy);
    let painter = ui.painter_at(image_rect);
    let line = Color32::from_rgba_unmultiplied(255, 255, 255, 170);
    let (first, last) = (times[0], times[times.len() - 1]);

    // The path, and a tick at each frame.
    let frame = 1.0 / state.sequence().fps.as_f64().max(1.0);
    let frames = ((last - first) / frame).ceil() as usize;
    let points: Vec<Pos2> = (0..=frames).map(|i| screen(at((first + i as f64 * frame).min(last)))).collect();
    painter.add(egui::Shape::line(points.clone(), Stroke::new(1.0_f32, line)));
    for p in &points {
        painter.circle_filled(*p, 1.4, line);
    }

    // The keys and, where both lanes have one, their handles.
    let handle_of = |t: f64, out: bool| -> Option<(Pos2, Pos2)> {
        let (kx, ky) = (key_at(&xs, t)?, key_at(&ys, t)?);
        let (dx, dy) = if out { (kx.tangent_out.1, ky.tangent_out.1) } else { (kx.tangent_in.1, ky.tangent_in.1) };
        let key = screen(at(t));
        Some((key, pos2(key.x + dx * sx, key.y + dy * sy)))
    };
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let mut hit: Option<(f64, Part)> = None;
    for &t in &times {
        let key = screen(at(t));
        for (out, part) in [(true, Part::Out), (false, Part::In)] {
            if let Some((k, h)) = handle_of(t, out).filter(|(k, h)| k.distance(*h) > 1.0) {
                painter.line_segment([k, h], Stroke::new(1.0_f32, line));
                painter.circle_stroke(h, 3.5, Stroke::new(1.2_f32, Color32::WHITE));
                if pointer.is_some_and(|p| p.distance(h) <= GRAB) {
                    hit = Some((t, part));
                }
            }
        }
        painter.rect_filled(Rect::from_center_size(key, vec2(7.0, 7.0)), 1.0, Color32::WHITE);
        if hit.is_none() && pointer.is_some_and(|p| p.distance(key) <= GRAB) {
            hit = Some((t, Part::Key));
        }
    }

    let (pressed, down, released) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released()));
    if state.motion_path_drag.is_none() && pressed {
        if let (Some((time, part)), Some(from)) = (hit, pointer) {
            state.motion_path_drag = Some(MotionPathDrag { time, part, from, started: false });
        }
    }
    let Some(mut drag) = state.motion_path_drag else { return hit.is_some() };
    let whole = klipp_graph_format::ParamKey::from("");
    if let (Some(p), true) = (pointer, down) {
        if !drag.started && p.distance(drag.from) < 3.0 {
            state.motion_path_drag = Some(drag);
            return true;
        }
        if !drag.started {
            cmds.push(AppCommand::BeginParamEditAt { addr: addr.clone(), key: whole.clone() });
            drag.started = true;
        }
        let (x, y) = ((p.x - centre.x) / sx, (p.y - centre.y) / sy);
        let (kx, ky) = at(drag.time);
        for (lane, keys, to, at_now) in [("pos_x", &xs, x, kx), ("pos_y", &ys, y, ky)] {
            let key = match drag.part {
                // The key moved where the pointer is; a lane with no key
                // at its time gets one, its curve shape as it was.
                Part::Key => klipp_core::keyframe::upserted(keys, drag.time, to),
                // A handle: both lanes' tangents toward the pointer, the
                // other handle kept in line with it.
                Part::Out | Part::In => {
                    let Some(k) = key_at(keys, drag.time) else { continue };
                    let mut k = k.clone();
                    let sign = if drag.part == Part::Out { 1.0 } else { -1.0 };
                    let dv = (to - at_now) * sign;
                    let dt = if k.tangent_out.0 > 0.0 { k.tangent_out.0 } else { frame * 4.0 };
                    k.tangent_out = (dt, dv);
                    k.tangent_in = (-dt, -dv);
                    k.handles = klipp_core::keyframe::Handles::Aligned;
                    k.interpolation = klipp_core::keyframe::Interpolation::Bezier;
                    k
                }
            };
            if let Ok(keyframe) = serde_json::to_value(key) {
                cmds.push(AppCommand::AddKeyAt { addr: addr.clone(), key: lane.into(), keyframe });
            }
        }
        state.motion_path_drag = Some(drag);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        ui.ctx().request_repaint();
        return true;
    }
    if released || !down {
        if drag.started {
            cmds.push(AppCommand::EndParamEditAt { addr, key: whole });
        }
        state.motion_path_drag = None;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(t: f64, v: f32) -> Keyframe {
        Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    /// The path's keys are both lanes' times, each once, in order.
    #[test]
    fn a_paths_keys_are_both_lanes_times() {
        let xs = [key(0.0, 0.0), key(2.0, 10.0)];
        let ys = [key(0.0, 5.0), key(1.0, 6.0), key(2.0, 7.0)];
        assert_eq!(key_times(&xs, &ys), vec![0.0, 1.0, 2.0]);
        assert!(key_at(&xs, 1.0).is_none());
        assert_eq!(key_at(&ys, 1.0).map(|k| k.value), Some(6.0));
    }
}
