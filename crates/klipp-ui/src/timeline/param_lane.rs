//! Any keyed parameter of a clip on its band (#2307): an effect's, a
//! grade node's or a title layer's keys, picked from the clip's menu
//! (Show Keys on Clip), drawn in the clip's key lane where a picture
//! parameter's band otherwise is. The pill names the track and, clicked,
//! goes back to the picture's band. A key is selected by a click, opened
//! by a right-click, added by an Alt+click on the line, and dragged in
//! time and value, each drag one step; Delete takes the selected one.

use crate::panels::dope_sheet::{self, Row};
use crate::state::{AppState, KeyframeContextMenu, KeyframeParamKind, SelectedKeyframe};
use klipp_core::{AppCommand, Keyframe};
use klipp_timeline::Clip;

/// The track the lane shows on `clip`: the one picked for it, while it
/// still holds keys.
pub(crate) fn shown(state: &AppState, clip: &Clip) -> Option<Row> {
    let (id, kind) = state.timeline_param_lane.as_ref()?;
    if *id != clip.id {
        return None;
    }
    dope_sheet::rows(clip).into_iter().find(|r| r.kind == *kind)
}

/// Show `kind`'s keys on clip `clip_id`'s band, in the lane that draws
/// them: the picture's band for placement and opacity, the mask lane for
/// a mask's, the sound's band for the sound's, this lane for the rest.
pub(crate) fn pick(state: &mut AppState, clip_id: klipp_core::ClipId, kind: &KeyframeParamKind) {
    state.timeline_param_lane = None;
    state.timeline_mask_lane = None;
    match kind {
        KeyframeParamKind::Video(p) => state.video_rubber_band_param = *p,
        KeyframeParamKind::Mask(track) => state.timeline_mask_lane = Some((clip_id, track.clone())),
        KeyframeParamKind::Audio(p) => state.rubber_band_param = crate::state::RubberBandParam::ClipParam(*p),
        KeyframeParamKind::AudioEffect { effect_index, param_name } => {
            state.rubber_band_param = crate::state::RubberBandParam::EffectParam { effect_index: *effect_index, param_name: param_name.clone() }
        }
        KeyframeParamKind::Node { .. } | KeyframeParamKind::Title { .. } => state.timeline_param_lane = Some((clip_id, kind.clone())),
    }
}

/// How near, in pixels, a press must be to take a key, or an Alt+click
/// to the line to add one.
const HIT_PX: f32 = 7.0;

/// The values the lane spans for `keys`: theirs, a fifth more each side,
/// never less than a hundredth wide.
fn span(keys: &[Keyframe]) -> (f32, f32) {
    let lo = keys.iter().map(|k| k.value).fold(f32::INFINITY, f32::min);
    let hi = keys.iter().map(|k| k.value).fold(f32::NEG_INFINITY, f32::max);
    if !lo.is_finite() {
        return (0.0, 1.0);
    }
    let width = (hi - lo).max(0.01);
    (lo - width * 0.2, hi + width * 0.2)
}

/// The lane for `row` over `clip`, drawn in `clip_rect`. `interactive`
/// lanes take the pointer; the pill's click is the caller's (returned as
/// `true`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    ui: &egui::Ui,
    painter: &egui::Painter,
    clip_rect: egui::Rect,
    clip: &Clip,
    row: &Row,
    state: &mut AppState,
    cmds: &mut Vec<AppCommand>,
    interactive: bool,
    pointer_pos: Option<egui::Pos2>,
    theme: &crate::theme::KlippTheme,
) -> bool {
    let kind = &row.kind;
    let keys = crate::key_selection::track_keys(clip, kind);
    let duration = clip.duration().to_secs_f64().max(1e-6);
    let painter = painter.with_clip_rect(clip_rect);
    // While a key is dragged the lane keeps the span it had when the drag
    // began, so the key stays under the pointer.
    let (lo, hi) = match &state.param_lane_drag {
        Some(d) if d.clip_id == clip.id && d.kind == *kind => d.span,
        _ => span(&keys),
    };
    let to_x = |t: f64| clip_rect.left() + ((t / duration).clamp(0.0, 1.0) as f32) * clip_rect.width();
    let to_y = |v: f32| clip_rect.bottom() - ((v - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0) * clip_rect.height();
    let from_x = |x: f32| (((x - clip_rect.left()) / clip_rect.width().max(1.0)) as f64 * duration).clamp(0.0, duration);
    let from_y = |y: f32| lo + ((clip_rect.bottom() - y) / clip_rect.height().max(1.0)).clamp(0.0, 1.0) * (hi - lo);
    let under = egui::Color32::from_black_alpha(150);
    let line = theme.accent;
    let rest = keys.first().map_or(0.0, |k| k.value);

    // The curve, sampled across the clip so every kind of move shows.
    let samples = ((clip_rect.width() / 3.0) as usize).clamp(8, 300);
    let curve: Vec<egui::Pos2> = (0..=samples)
        .map(|i| {
            let t = duration * i as f64 / samples as f64;
            egui::pos2(to_x(t), to_y(klipp_core::keyframe::evaluate(&keys, t, rest)))
        })
        .collect();
    painter.add(egui::Shape::line(curve.clone(), egui::Stroke::new(3.0_f32, under)));
    painter.add(egui::Shape::line(curve, egui::Stroke::new(1.6_f32, line)));
    let pts: Vec<egui::Pos2> = keys.iter().map(|k| egui::pos2(to_x(k.time_secs), to_y(k.value))).collect();
    let selected = state.selected_keyframe.as_ref().filter(|s| s.clip_id == clip.id && s.param_kind == *kind).map(|s| s.keyframe_index);
    for (i, p) in pts.iter().enumerate() {
        let d = 4.5;
        let diamond = vec![*p + egui::vec2(0.0, -d), *p + egui::vec2(d, 0.0), *p + egui::vec2(0.0, d), *p + egui::vec2(-d, 0.0)];
        painter.add(egui::Shape::convex_polygon(diamond.clone(), under, egui::Stroke::NONE));
        let marked = keys.get(i).is_some_and(|k| crate::key_selection::marked(state, clip.id, kind, k.time_secs));
        let fill = if selected == Some(i) || marked { theme.text_primary } else { line };
        painter.add(egui::Shape::convex_polygon(diamond.iter().map(|q| *p + (*q - *p) * 0.75).collect(), fill, egui::Stroke::NONE));
    }

    // The pill: the track's name, always.
    let font = egui::FontId::proportional(crate::theme::type_scale::CAPTION);
    let galley = painter.layout_no_wrap(row.label.clone(), font, theme.text_primary);
    let pill = egui::Rect::from_min_size(egui::pos2(clip_rect.left() + 4.0, clip_rect.bottom() - 18.0), galley.size() + egui::vec2(8.0, 4.0));
    if clip_rect.width() > 30.0 {
        painter.rect_filled(pill, 4.0, under);
        painter.galley(pill.min + egui::vec2(4.0, 2.0), galley, theme.text_primary);
    }
    if !interactive {
        return false;
    }
    let (pressed, released, secondary, down) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_released(), i.pointer.secondary_clicked(), i.pointer.primary_down()));
    let (shift, alt, clicked, decided, press_at) = ui.input(|i| (i.modifiers.shift, i.modifiers.alt, i.pointer.primary_clicked(), i.pointer.is_decidedly_dragging(), i.pointer.press_origin()));
    let hit = pointer_pos.and_then(|p| pts.iter().position(|q| q.distance(p) <= HIT_PX));
    let in_several = |state: &AppState, i: usize| keys.get(i).is_some_and(|k| crate::key_selection::marked(state, clip.id, kind, k.time_secs));
    let mut back = false;

    // A press: the pill, or a key (selected, and its drag begun). With
    // Shift, or on a key of several selected, the click or drag decides.
    if pressed {
        if let Some(p) = pointer_pos {
            if pill.contains(p) && clip_rect.width() > 30.0 {
                back = true;
            } else if let Some(i) = hit.filter(|i| !shift && !alt && !in_several(state, *i)) {
                state.selected_keyframe = Some(SelectedKeyframe { clip_id: clip.id, param_kind: kind.clone(), keyframe_index: i });
                state.param_lane_drag = Some(crate::state::ParamLaneDrag { clip_id: clip.id, kind: kind.clone(), at: keys[i].time_secs, span: (lo, hi), began: false });
            }
        }
    }
    // Alt+click on the line: a key there, on the curve as it runs, so the
    // picture doesn't change.
    if clicked && alt && hit.is_none() {
        if let Some(p) = pointer_pos.filter(|p| clip_rect.contains(*p)) {
            let t = from_x(p.x);
            let v = klipp_core::keyframe::evaluate(&keys, t, rest);
            if (to_y(v) - p.y).abs() <= HIT_PX {
                let t = crate::inspector::keyframe_selection::timeline_key_time(&keys, t, state.sequence().fps.as_f64());
                cmds.extend(crate::panels::curve_editor::add_key_commands(&row.curve, clip.id, t, v));
            }
        }
    }
    // A click with Shift adds the key or takes it out; one on a key of
    // several selected selects it alone.
    if clicked && !alt && state.multi_key_drag.is_none() {
        if let Some(i) = hit.filter(|i| shift || in_several(state, *i)) {
            crate::key_selection::lane_click(state, clip.id, kind.clone(), i, shift);
        }
    }
    // A key of several selected dragged: every one moves with it, in time.
    if decided && down && state.multi_key_drag.is_none() && state.param_lane_drag.is_none() && !shift {
        if let Some(origin) = press_at {
            if let Some(i) = pts.iter().position(|q| q.distance(origin) <= HIT_PX).filter(|i| in_several(state, *i)) {
                let pps = clip_rect.width() / duration as f32;
                cmds.extend(crate::key_selection::lane_drag_begin(state, clip.id, kind, i, origin.x, pps).unwrap_or_default());
            }
        }
    }
    if secondary {
        if let (Some(i), Some(p)) = (hit, pointer_pos) {
            state.keyframe_context_menu = Some(KeyframeContextMenu { clip_id: clip.id, param_kind: kind.clone(), keyframe_index: i, screen_pos: p });
        }
    }
    // The drag followed: the key to the pointer's time and value; never
    // onto another key. One step from press to release.
    if let Some(drag) = state.param_lane_drag.clone() {
        if drag.clip_id == clip.id && drag.kind == *kind {
            if down && decided {
                if let Some(p) = pointer_pos {
                    let (t, v) = (from_x(p.x), from_y(p.y));
                    let clash = keys.iter().any(|k| (k.time_secs - drag.at).abs() > 1e-9 && (k.time_secs - t).abs() < 1e-6);
                    if !clash {
                        if let Some(mv) = crate::key_selection::move_command(clip, kind, drag.at, t, Some(v)) {
                            if !drag.began {
                                cmds.push(AppCommand::BeginHistoryBatch);
                            }
                            cmds.push(mv);
                            state.param_lane_drag = Some(crate::state::ParamLaneDrag { at: t, began: true, ..drag.clone() });
                        }
                    }
                }
            }
            if released || !down {
                if drag.began {
                    cmds.push(AppCommand::EndHistoryBatch);
                    // The key keeps its selection where it now sorts.
                    let at = keys.iter().filter(|k| k.time_secs < drag.at - 1e-9).count();
                    state.selected_keyframe = Some(SelectedKeyframe { clip_id: clip.id, param_kind: kind.clone(), keyframe_index: at.min(keys.len().saturating_sub(1)) });
                }
                state.param_lane_drag = None;
            }
        }
    }
    back
}
