//! A mask's keys on the timeline (#1947), in the clip's key lane where a
//! picture parameter's band otherwise is: a value's keys as a band over
//! the clip, a freeform shape's as diamonds along its top. The pill names
//! the track ("Face · Shape") and, clicked, goes on to the clip's next
//! keyed track. A key is selected by a click, opened by a right-click,
//! and dragged in time (and value, for a value's), each drag one step.

use crate::mask_tracks::{self, MaskTrack};
use crate::state::{AppState, KeyframeContextMenu, KeyframeParamKind, SelectedKeyframe};
use klipp_core::AppCommand;
use klipp_timeline::Clip;

/// The mask track the lane shows on `clip`: the one chosen for it while
/// it still holds keys, else, when no picture parameter is keyed, the
/// clip's first keyed mask track.
pub(crate) fn shown(state: &AppState, clip: &Clip, picture_keyed: bool) -> Option<MaskTrack> {
    let keyed = mask_tracks::keyed(clip);
    if let Some((id, track)) = &state.timeline_mask_lane {
        if *id == clip.id && keyed.iter().any(|(t, _)| t == track) {
            return Some(track.clone());
        }
    }
    (!picture_keyed).then(|| keyed.first().map(|(t, _)| t.clone())).flatten()
}

/// The track after `track` among `clip`'s keyed mask tracks: `None` past
/// the last, back to the picture's parameters when the clip has keyed
/// ones, else round to the first.
pub(crate) fn next(clip: &Clip, track: Option<&MaskTrack>, picture_keyed: bool) -> Option<MaskTrack> {
    let keyed: Vec<MaskTrack> = mask_tracks::keyed(clip).into_iter().map(|(t, _)| t).collect();
    let after = match track {
        None => keyed.first().cloned(),
        Some(t) => keyed.iter().position(|k| k == t).and_then(|i| keyed.get(i + 1)).cloned(),
    };
    after.or_else(|| (!picture_keyed).then(|| keyed.first().cloned()).flatten())
}

/// How near, in pixels, a press must be to take a key.
const HIT_PX: f32 = 7.0;

/// The lane for `track` over `clip`, drawn in `clip_rect`. `interactive`
/// lanes take the pointer; the pill's click is the caller's to cycle
/// (returned as `true`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    ui: &egui::Ui,
    painter: &egui::Painter,
    clip_rect: egui::Rect,
    clip: &Clip,
    track: &MaskTrack,
    state: &mut AppState,
    cmds: &mut Vec<AppCommand>,
    interactive: bool,
    pointer_pos: Option<egui::Pos2>,
    theme: &crate::theme::KlippTheme,
) -> bool {
    let keys = mask_tracks::keys(clip, track);
    let duration = clip.duration().to_secs_f64().max(1e-6);
    let painter = painter.with_clip_rect(clip_rect);
    let (lo, hi, _) = mask_tracks::range(&track.param);
    let shape = track.is_shape();
    let to_x = |t: f64| clip_rect.left() + ((t / duration).clamp(0.0, 1.0) as f32) * clip_rect.width();
    let to_y = |v: f32| if shape { clip_rect.top() + 7.0 } else { clip_rect.bottom() - ((v - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0) * clip_rect.height() };
    let from_x = |x: f32| (((x - clip_rect.left()) / clip_rect.width().max(1.0)) as f64 * duration).clamp(0.0, duration);
    let from_y = |y: f32| lo + ((clip_rect.bottom() - y) / clip_rect.height().max(1.0)).clamp(0.0, 1.0) * (hi - lo);
    let under = egui::Color32::from_black_alpha(150);
    let line = theme.accent;

    // The band, or the shape's row.
    let pts: Vec<egui::Pos2> = keys.iter().map(|k| egui::pos2(to_x(k.time_secs), to_y(k.value))).collect();
    if !shape && pts.len() > 1 {
        painter.add(egui::Shape::line(pts.clone(), egui::Stroke::new(3.0_f32, under)));
        painter.add(egui::Shape::line(pts.clone(), egui::Stroke::new(1.6_f32, line)));
    } else if shape && pts.len() > 1 {
        painter.line_segment([pts[0], pts[pts.len() - 1]], egui::Stroke::new(1.0_f32, line.gamma_multiply(0.6)));
    }
    let selected = state.selected_keyframe.as_ref().filter(|s| s.clip_id == clip.id && s.param_kind == KeyframeParamKind::Mask(track.clone())).map(|s| s.keyframe_index);
    for (i, p) in pts.iter().enumerate() {
        let d = 4.5;
        let diamond = vec![*p + egui::vec2(0.0, -d), *p + egui::vec2(d, 0.0), *p + egui::vec2(0.0, d), *p + egui::vec2(-d, 0.0)];
        painter.add(egui::Shape::convex_polygon(diamond.clone(), under, egui::Stroke::NONE));
        let marked = keys.get(i).is_some_and(|k| crate::key_selection::marked(state, clip.id, &KeyframeParamKind::Mask(track.clone()), k.time_secs));
        let fill = if selected == Some(i) || marked { theme.text_primary } else { line };
        painter.add(egui::Shape::convex_polygon(diamond.iter().map(|q| *p + (*q - *p) * 0.75).collect(), fill, egui::Stroke::NONE));
    }

    // The pill: the track's name, always.
    let name = mask_tracks::name(clip, track);
    let font = egui::FontId::proportional(crate::theme::type_scale::CAPTION);
    let galley = painter.layout_no_wrap(name, font, theme.text_primary);
    let pill = egui::Rect::from_min_size(egui::pos2(clip_rect.left() + 4.0, clip_rect.bottom() - 18.0), galley.size() + egui::vec2(8.0, 4.0));
    let mut cycle = false;
    if clip_rect.width() > 30.0 {
        painter.rect_filled(pill, 4.0, under);
        painter.galley(pill.min + egui::vec2(4.0, 2.0), galley, theme.text_primary);
    }
    if !interactive {
        return false;
    }
    let (pressed, released, secondary, down) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_released(), i.pointer.secondary_clicked(), i.pointer.primary_down()));
    let (shift, clicked, decided, press_at) = ui.input(|i| (i.modifiers.shift, i.pointer.primary_clicked(), i.pointer.is_decidedly_dragging(), i.pointer.press_origin()));
    let hit = pointer_pos.and_then(|p| pts.iter().position(|q| q.distance(p) <= HIT_PX));
    let kind = KeyframeParamKind::Mask(track.clone());
    // Whether key `i` is one of several selected: a press on it may begin
    // their drag, so it leaves the selection be until a click (#1958).
    let in_several = |state: &AppState, i: usize| keys.get(i).is_some_and(|k| crate::key_selection::marked(state, clip.id, &kind, k.time_secs));

    // A press: the pill, or a key (selected, and its drag begun). With
    // Shift, or on a key of several selected, the click or drag decides.
    if pressed {
        if let Some(p) = pointer_pos {
            if pill.contains(p) && clip_rect.width() > 30.0 {
                cycle = true;
            } else if let Some(i) = hit.filter(|i| !shift && !in_several(state, *i)) {
                state.selected_keyframe = Some(SelectedKeyframe { clip_id: clip.id, param_kind: KeyframeParamKind::Mask(track.clone()), keyframe_index: i });
                state.mask_lane_drag = Some((clip.id, track.clone(), keys[i].time_secs, false));
            }
        }
    }
    // A click with Shift adds the key or takes it out; one on a key of
    // several selected selects it alone.
    if clicked && state.multi_key_drag.is_none() {
        if let Some(i) = hit.filter(|i| shift || in_several(state, *i)) {
            crate::key_selection::lane_click(state, clip.id, kind.clone(), i, shift);
        }
    }
    // A key of several selected dragged: every one moves with it, in time.
    if decided && down && state.multi_key_drag.is_none() && state.mask_lane_drag.is_none() && !shift {
        if let Some(origin) = press_at {
            if let Some(i) = pts.iter().position(|q| q.distance(origin) <= HIT_PX).filter(|i| in_several(state, *i)) {
                let pps = clip_rect.width() / duration as f32;
                cmds.extend(crate::key_selection::lane_drag_begin(state, clip.id, &kind, i, origin.x, pps).unwrap_or_default());
            }
        }
    }
    if secondary {
        if let (Some(i), Some(p)) = (hit, pointer_pos) {
            state.keyframe_context_menu = Some(KeyframeContextMenu { clip_id: clip.id, param_kind: KeyframeParamKind::Mask(track.clone()), keyframe_index: i, screen_pos: p });
        }
    }
    // The drag followed: the key by its time now; never onto another key.
    if let Some((id, t, now, began)) = state.mask_lane_drag.clone() {
        if id == clip.id && t == *track {
            if down {
                if let Some(p) = pointer_pos {
                    let new_t = from_x(p.x);
                    let value = (!shape).then(|| from_y(p.y));
                    let clash = keys.iter().any(|k| (k.time_secs - now).abs() > 1e-9 && (k.time_secs - new_t).abs() < 1e-9);
                    let moved = (new_t - now).abs() > 1e-9 || value.is_some_and(|v| keys.iter().find(|k| (k.time_secs - now).abs() < 1e-9).is_some_and(|k| (k.value - v).abs() > 1e-6));
                    if moved && !clash {
                        if !began {
                            cmds.push(AppCommand::BeginNodeParamEdit { clip_id: clip.id });
                        }
                        cmds.push(AppCommand::MoveMaskKeyframe { clip_id: clip.id, node_id: track.node, mask_index: track.mask, param: track.param.clone(), time_secs: now, new_time_secs: new_t, value });
                        state.mask_lane_drag = Some((id, t, new_t, true));
                    }
                }
            }
            if released || !down {
                if began {
                    cmds.push(AppCommand::EndNodeParamEdit { clip_id: clip.id });
                    // The key keeps its selection where it now sorts.
                    let at = keys.iter().filter(|k| k.time_secs < now - 1e-9).count();
                    state.selected_keyframe = Some(SelectedKeyframe { clip_id: clip.id, param_kind: KeyframeParamKind::Mask(track.clone()), keyframe_index: at.min(keys.len().saturating_sub(1)) });
                }
                state.mask_lane_drag = None;
            }
        }
    }
    cycle
}
