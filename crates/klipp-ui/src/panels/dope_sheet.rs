//! The dope sheet (#2240): every keyframed parameter of the selected clip
//! as a row on one time axis, its keys as diamonds. Selection, the drag
//! of the selected keys and Delete are the key selection's (#1958), so a
//! key picked here is the key picked on the timeline and in the curves.

use super::curve_editor::CurveParam;
use crate::key_selection;
use crate::state::{AppState, KeyframeParamKind};
use crate::theme::KlippTheme;
use egui::{pos2, vec2, Rect, Stroke};
use klipp_core::{AppCommand, AudioParam, VideoParam};
use klipp_timeline::Clip;

/// One row: the track, the curve it opens in the Curve Editor, and its name.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub kind: KeyframeParamKind,
    pub curve: CurveParam,
    pub label: String,
}

/// The rows of `clip`: each of its tracks that has keys, in the order the
/// Curve Editor lists them (placement and opacity, sound, effects, grade,
/// masks, audio effects, title objects).
pub fn rows(clip: &Clip) -> Vec<Row> {
    let mut candidates: Vec<(KeyframeParamKind, CurveParam)> = Vec::new();
    for p in [
        VideoParam::Opacity,
        VideoParam::PositionX,
        VideoParam::PositionY,
        VideoParam::Scale,
        VideoParam::ScaleY,
        VideoParam::Rotation,
        VideoParam::TiltX,
        VideoParam::TiltY,
        VideoParam::MatteFeather,
        VideoParam::MatteChoke,
    ] {
        candidates.push((KeyframeParamKind::Video(p), CurveParam::Video(p)));
    }
    for p in [AudioParam::Volume, AudioParam::Pan, AudioParam::Gain] {
        candidates.push((KeyframeParamKind::Audio(p), CurveParam::Audio(p)));
    }
    for (path, key, tag) in clip.animated_effect_keys().into_iter().chain(clip.animated_grade_keys()) {
        candidates.push((KeyframeParamKind::Node { path: path.clone(), key: key.clone() }, CurveParam::Node { path, key, tag }));
    }
    for (track, name) in crate::mask_tracks::keyed(clip) {
        candidates.push((KeyframeParamKind::Mask(track.clone()), CurveParam::Mask { track, name }));
    }
    let mut audio_effects: Vec<&String> = clip.audio_effect_keyframes.keys().collect();
    audio_effects.sort();
    for key in audio_effects {
        let Some((index, name)) = key.split_once(':') else { continue };
        let Ok(effect_index) = index.parse() else { continue };
        candidates.push((KeyframeParamKind::AudioEffect { effect_index, param_name: name.to_string() }, CurveParam::AudioEffect(key.clone())));
    }
    let mut titled: Vec<(KeyframeParamKind, CurveParam, String)> = Vec::new();
    if let klipp_timeline::ClipKind::Title(title) = &clip.kind {
        for object in &title.objects {
            for param in super::title_curve::parameters(object) {
                let label = param.label();
                let object_name = if object.name.is_empty() { format!("Object {}", object.id.0) } else { object.name.clone() };
                titled.push((
                    KeyframeParamKind::Title { object_id: object.id.0, param: param.clone() },
                    CurveParam::Title { object_id: object.id.0, param },
                    format!("{object_name} · {label}"),
                ));
            }
        }
    }
    let mut out: Vec<Row> = candidates
        .into_iter()
        .filter(|(kind, _)| !key_selection::track_keys(clip, kind).is_empty())
        .map(|(kind, curve)| {
            let label = curve.label();
            Row { kind, curve, label }
        })
        .collect();
    out.extend(titled.into_iter().filter(|(kind, ..)| !key_selection::track_keys(clip, kind).is_empty()).map(|(kind, curve, label)| Row { kind, curve, label }));
    out
}

/// What the sheet keeps between frames: a box being drawn, and whether
/// the selected keys are being dragged.
#[derive(Debug, Default)]
pub struct Sheet {
    box_from: Option<egui::Pos2>,
    dragging: bool,
    /// The last frame's lanes: where they start across and down, and the
    /// clip's points a second, for a test to find a key on screen.
    #[cfg(test)]
    last: Option<(egui::Pos2, f32)>,
}

/// The height of a row, and the width of the names' column.
const ROW_H: f32 = 20.0;
const NAMES_W: f32 = 150.0;
/// How near a key the pointer must be to take it, in points.
const HIT: f32 = 6.0;

/// What a click or press on the sheet landed on.
enum Hit {
    Key(usize, usize),
    Name(usize),
    Empty,
}

impl Sheet {
    /// Where the key at `t` seconds on row `row` was drawn last frame.
    #[cfg(test)]
    pub fn screen_at(&self, row: usize, t: f64) -> Option<egui::Pos2> {
        self.last.map(|(o, pps)| pos2(o.x + t as f32 * pps, o.y + (row as f32 + 0.5) * ROW_H))
    }

    /// The sheet for `clip` drawn into `rect`; a row's name double-clicked
    /// gives back its curve, to open in the Curve Editor.
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, rect: Rect, clip: &Clip, cmds: &mut Vec<AppCommand>) -> Option<CurveParam> {
        let rows = rows(clip);
        let painter = ui.painter_at(rect);
        if rows.is_empty() {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "No keyframes on this clip", egui::FontId::proportional(11.0), theme.text_dim);
            return None;
        }
        let duration = clip.duration().to_secs_f64().max(1e-3);
        let fps = state.sequence().fps.as_f64().max(1.0);
        let lanes = Rect::from_min_max(pos2(rect.min.x + NAMES_W, rect.min.y), pos2(rect.max.x - 8.0, rect.max.y));
        let px_per_sec = (lanes.width() as f64 / duration).max(1e-3) as f32;
        let x_of = |t: f64| lanes.min.x + (t as f32) * px_per_sec;
        let row_rect = |i: usize| Rect::from_min_size(pos2(rect.min.x, rect.min.y + i as f32 * ROW_H), vec2(rect.width(), ROW_H));
        let keys: Vec<Vec<klipp_core::Keyframe>> = rows.iter().map(|r| key_selection::track_keys(clip, &r.kind)).collect();
        #[cfg(test)]
        {
            self.last = Some((lanes.min, px_per_sec));
        }

        // Rows, names, keys.
        for (i, row) in rows.iter().enumerate() {
            let r = row_rect(i);
            if r.min.y > rect.max.y {
                break;
            }
            if i % 2 == 1 {
                painter.rect_filled(r, 0.0, theme.bg_raised.gamma_multiply(0.4));
            }
            painter.text(pos2(r.min.x + 8.0, r.center().y), egui::Align2::LEFT_CENTER, &row.label, egui::FontId::proportional(10.5), theme.text_secondary);
            for k in &keys[i] {
                let c = pos2(x_of(k.time_secs), r.center().y);
                let selected = state.key_selection.holds(clip.id, &row.kind, k.time_secs)
                    || state.selected_keyframe.as_ref().is_some_and(|sk| sk.clip_id == clip.id && sk.param_kind == row.kind && key_selection::track_keys(clip, &sk.param_kind).get(sk.keyframe_index).is_some_and(|kf| (kf.time_secs - k.time_secs).abs() < 1e-6));
                let s = 4.5;
                let diamond = vec![pos2(c.x, c.y - s), pos2(c.x + s, c.y), pos2(c.x, c.y + s), pos2(c.x - s, c.y)];
                let fill = if selected { theme.accent } else { theme.text_secondary };
                painter.add(egui::Shape::convex_polygon(diamond, fill, Stroke::new(1.0_f32, theme.bg_deep)));
            }
        }
        painter.vline(lanes.min.x, rect.y_range(), Stroke::new(0.5_f32, theme.border));
        // The playhead, where it is over the clip.
        let at = (state.playhead - clip.timeline_in).to_secs_f64();
        if (0.0..=duration).contains(&at) {
            painter.vline(x_of(at), rect.y_range(), Stroke::new(1.0_f32, theme.accent.gamma_multiply(0.7)));
        }

        let hit = |p: egui::Pos2| -> Hit {
            let i = ((p.y - rect.min.y) / ROW_H).floor();
            if i < 0.0 || i as usize >= rows.len() {
                return Hit::Empty;
            }
            let i = i as usize;
            if p.x < lanes.min.x {
                return Hit::Name(i);
            }
            keys[i]
                .iter()
                .enumerate()
                .map(|(j, k)| (j, (x_of(k.time_secs) - p.x).abs()))
                .filter(|(_, d)| *d <= HIT)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map_or(Hit::Empty, |(j, _)| Hit::Key(i, j))
        };

        let response = ui.interact(rect, ui.id().with("dope_sheet"), egui::Sense::click_and_drag());
        let shift = ui.input(|i| i.modifiers.shift);
        let mut open = None;
        if response.drag_started() {
            if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
                match hit(origin) {
                    Hit::Key(i, j) => {
                        let kind = &rows[i].kind;
                        let t = keys[i][j].time_secs;
                        if !state.key_selection.holds(clip.id, kind, t) {
                            key_selection::click(state, clip, kind, j, shift);
                        }
                        cmds.extend(key_selection::begin_drag(state, clip, None));
                        if let Some(d) = state.multi_key_drag.as_mut() {
                            d.anchor_x = Some(origin.x);
                            d.px_per_sec = px_per_sec;
                        }
                        self.dragging = true;
                    }
                    Hit::Empty | Hit::Name(_) => self.box_from = Some(origin),
                }
            }
        }
        if response.dragged() {
            if let Some(p) = response.interact_pointer_pos() {
                if self.dragging {
                    // Snapped to frames.
                    let anchor = state.multi_key_drag.as_ref().and_then(|d| d.anchor_x).unwrap_or(p.x);
                    let dt = (((p.x - anchor) / px_per_sec) as f64 * fps).round() / fps;
                    cmds.extend(key_selection::lane_drag_frame(state, clip.id, Some(anchor + (dt as f32) * px_per_sec), true));
                } else if let Some(from) = self.box_from {
                    let b = Rect::from_two_pos(from, p);
                    painter.rect(b, 0.0, theme.accent.gamma_multiply(0.12), Stroke::new(1.0_f32, theme.accent.gamma_multiply(0.6)));
                }
            }
        }
        if response.drag_stopped() {
            if self.dragging {
                cmds.extend(key_selection::lane_drag_frame(state, clip.id, None, false));
                self.dragging = false;
            } else if let (Some(from), Some(to)) = (self.box_from.take(), response.interact_pointer_pos()) {
                let b = Rect::from_two_pos(from, to);
                let (t0, t1) = (((b.min.x - lanes.min.x) / px_per_sec) as f64, ((b.max.x - lanes.min.x) / px_per_sec) as f64);
                let mut add = shift;
                for (i, row) in rows.iter().enumerate() {
                    if row_rect(i).intersects(b) {
                        key_selection::select_box(state, clip, &row.kind, (t0, f32::MIN), (t1, f32::MAX), add);
                        add = true;
                    }
                }
                if !add {
                    key_selection::clear(state);
                }
            }
        }
        let at_click = response.interact_pointer_pos().map(hit);
        if let (true, Some(Hit::Name(i))) = (response.double_clicked(), &at_click) {
            open = Some(rows[*i].curve.clone());
        } else if response.clicked() {
            match at_click {
                Some(Hit::Key(i, j)) => key_selection::click(state, clip, &rows[i].kind, j, shift),
                // Its name selects the row's keys.
                Some(Hit::Name(i)) => key_selection::select_box(state, clip, &rows[i].kind, (f64::MIN, f32::MIN), (f64::MAX, f32::MAX), shift),
                _ if !shift => key_selection::clear(state),
                _ => {}
            }
        }
        if response.hovered() {
            if let Some(p) = response.hover_pos() {
                if let Hit::Name(i) = hit(p) {
                    response.clone().on_hover_text(format!("{}: click selects its keys, double-click opens its curve", rows[i].label));
                }
            }
        }
        open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(t: f64, v: f32) -> klipp_core::Keyframe {
        klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    /// Each track with keys is a row, named as the Curve Editor names it;
    /// one without keys is not.
    #[test]
    fn a_clip_s_keyed_tracks_are_its_rows() {
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        assert!(rows(&Clip::gap(tc(0.0), tc(4.0))).is_empty());
        let mut clip = klipp_timeline::test_clip::TestClip::of(Clip::gap(tc(0.0), tc(4.0))).lane("opacity", 0.0, 0.0).lane("opacity", 1.0, 1.0).build();
        clip.volume_keyframes = vec![key(0.5, 0.2), key(2.0, 1.0), key(3.0, 0.5)];
        let rows = rows(&clip);
        let names: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(names, vec!["Opacity", "Volume"]);
        assert_eq!(key_selection::track_keys(&clip, &rows[1].kind).len(), 3);
        assert_eq!(rows[0].curve, CurveParam::Video(VideoParam::Opacity));
    }
}
