//! The Inspector's Keyframes column (#2305): each keyed row's keys on a
//! strip beside it, over the selected clip's length, with the playhead.
//! The Inspector keeps the room and notes where each row was drawn; the
//! app draws the strips here, where it can change the key selection, the
//! keys and the playhead, through the same key selection as the
//! timeline, the Curve Editor and the Dope Sheet (#1958).

use crate::key_selection;
use crate::state::{AppState, KeyframeContextMenu, KeyframeParamKind};
use crate::theme::KlippTheme;
use egui::{pos2, Rect, Stroke};
use klipp_core::{AppCommand, ClipId};

/// The narrowest the column may be, and the room the rows keep beside it:
/// below both together the column is not shown.
pub const MIN_W: f32 = 80.0;
pub const ROWS_MIN_W: f32 = 260.0;

/// How near a key the pointer must be to take it, and to snap to a time,
/// in points.
const HIT: f32 = 6.0;
const SNAP_PX: f32 = 6.0;

/// What the Inspector hands the app to draw the column: the clip, the
/// column's place across, the part of the panel the rows are seen in, and
/// each keyed row with where it was drawn.
#[derive(Debug, Clone)]
pub struct KeyArea {
    pub clip_id: ClipId,
    pub column: Rect,
    pub rows: Vec<(KeyframeParamKind, Rect)>,
}

/// What the column keeps between frames: a drag of the selected keys.
#[derive(Debug, Default)]
pub struct KeyColumn {
    /// The pressed key's time when a drag of the selected keys began.
    dragging_from: Option<f64>,
    /// The time the drag last snapped to, for its guide.
    snapped_to: Option<f64>,
}

impl KeyColumn {
    /// The strips for `area`, drawn into `ui`, their picks applied to
    /// `state` and the edits they make added to `cmds`.
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, area: &KeyArea, cmds: &mut Vec<AppCommand>) {
        let Some(clip) = state.sequence().all_clips().find(|c| c.id == area.clip_id).cloned() else { return };
        let column = area.column;
        let painter = ui.painter_at(column);
        painter.rect_filled(column, 0.0, theme.bg_deep.gamma_multiply(0.5));
        painter.vline(column.min.x, column.y_range(), Stroke::new(1.0_f32, theme.border));
        let duration = clip.duration().to_secs_f64().max(1e-3);
        let lane = column.shrink2(egui::vec2(8.0, 0.0));
        let px_per_sec = (lane.width() as f64 / duration).max(1e-3) as f32;
        let x_of = |t: f64| lane.min.x + t as f32 * px_per_sec;
        let t_of = |x: f32| (((x - lane.min.x) / px_per_sec) as f64).clamp(0.0, duration);
        let fps = state.sequence().fps.as_f64().max(1.0);
        let playhead = (state.playhead - clip.timeline_in).to_secs_f64();
        let on_key = |t: f64| (t - playhead).abs() < 0.5 / fps;
        let keys: Vec<Vec<klipp_core::Keyframe>> = area.rows.iter().map(|(kind, _)| key_selection::track_keys(&clip, kind)).collect();
        let selected = |kind: &KeyframeParamKind, t: f64| {
            state.key_selection.holds(clip.id, kind, t)
                || state.selected_keyframe.as_ref().is_some_and(|sk| sk.clip_id == clip.id && sk.param_kind == *kind && key_selection::track_keys(&clip, kind).get(sk.keyframe_index).is_some_and(|k| (k.time_secs - t).abs() < 1e-6))
        };

        // The strips, each in line with its row, and their keys.
        for (i, (kind, row)) in area.rows.iter().enumerate() {
            let strip = Rect::from_x_y_ranges(lane.x_range(), row.y_range());
            painter.hline(strip.x_range(), strip.center().y, Stroke::new(1.0_f32, theme.border.gamma_multiply(0.6)));
            for k in &keys[i] {
                let c = pos2(x_of(k.time_secs), strip.center().y);
                let s = 4.5;
                let points = vec![pos2(c.x, c.y - s), pos2(c.x + s, c.y), pos2(c.x, c.y + s), pos2(c.x - s, c.y)];
                if selected(kind, k.time_secs) {
                    painter.add(egui::Shape::convex_polygon(points, theme.text_primary, Stroke::new(1.0_f32, theme.accent)));
                } else if on_key(k.time_secs) {
                    painter.add(egui::Shape::convex_polygon(points, theme.accent, Stroke::NONE));
                } else {
                    painter.add(egui::Shape::closed_line(points, Stroke::new(1.1_f32, theme.accent)));
                }
            }
        }
        if (0.0..=duration).contains(&playhead) {
            painter.vline(x_of(playhead), column.y_range(), Stroke::new(1.0_f32, theme.accent.gamma_multiply(0.7)));
        }
        if let Some(t) = self.snapped_to.filter(|_| self.dragging_from.is_some()) {
            painter.vline(x_of(t), column.y_range(), Stroke::new(1.0_f32, theme.text_secondary.gamma_multiply(0.6)));
        }

        // What a point lands on: a key of a row, a row's empty strip, or
        // nothing.
        let hit = |p: egui::Pos2| -> Option<(usize, Option<usize>)> {
            let i = area.rows.iter().position(|(_, r)| r.y_range().contains(p.y))?;
            let j = keys[i]
                .iter()
                .enumerate()
                .map(|(j, k)| (j, (x_of(k.time_secs) - p.x).abs()))
                .filter(|(_, d)| *d <= HIT)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(j, _)| j);
            Some((i, j))
        };

        let response = ui.interact(column, ui.id().with("inspector_key_column"), egui::Sense::click_and_drag());
        let shift = ui.input(|i| i.modifiers.shift);
        if response.drag_started() {
            if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
                if let Some((i, Some(j))) = hit(origin) {
                    let kind = &area.rows[i].0;
                    if !state.key_selection.holds(clip.id, kind, keys[i][j].time_secs) {
                        key_selection::click(state, &clip, kind, j, shift);
                    }
                    cmds.extend(key_selection::begin_drag(state, &clip, None));
                    if let Some(d) = state.multi_key_drag.as_mut() {
                        d.anchor_x = Some(origin.x);
                        d.px_per_sec = px_per_sec;
                    }
                    self.dragging_from = Some(keys[i][j].time_secs);
                }
            }
        }
        if let (true, Some(from)) = (response.dragged(), self.dragging_from) {
            if let (Some(p), Some(anchor)) = (response.interact_pointer_pos(), state.multi_key_drag.as_ref().and_then(|d| d.anchor_x)) {
                // The pressed key's new time snapped as the Curve Editor's
                // drag is (#2279): onto the playhead or another key's time
                // within a few points, else a whole frame; Shift, free.
                let moving: Vec<(KeyframeParamKind, f64)> = state.key_selection.keys.iter().map(|k| (k.param_kind.clone(), k.time_secs)).collect();
                let to = self.snap(shift, t_of(x_of(from) + (p.x - anchor)), from, playhead, &area.rows, &keys, &moving, fps, &x_of);
                // By the time itself, not back through a point, so a frame
                // stays a whole frame.
                cmds.extend(key_selection::drag_to(state, &clip, to - from, 0.0));
            }
        }
        if response.drag_stopped() && self.dragging_from.is_some() {
            cmds.extend(key_selection::end_drag(state));
            self.dragging_from = None;
            self.snapped_to = None;
        }
        if response.clicked() {
            match response.interact_pointer_pos().and_then(hit) {
                Some((i, Some(j))) => key_selection::click(state, &clip, &area.rows[i].0, j, shift),
                // An empty spot puts the playhead there.
                _ => {
                    if let Some(p) = response.interact_pointer_pos() {
                        let rate = state.sequence().fps.rate();
                        cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(clip.timeline_in.to_secs_f64() + t_of(p.x), rate)));
                    }
                    if !shift {
                        key_selection::clear(state);
                    }
                }
            }
        }
        if response.secondary_clicked() {
            if let Some((p, (i, Some(j)))) = response.interact_pointer_pos().and_then(|p| hit(p).map(|h| (p, h))) {
                let kind = area.rows[i].0.clone();
                if !state.key_selection.holds(clip.id, &kind, keys[i][j].time_secs) {
                    key_selection::click(state, &clip, &kind, j, false);
                }
                state.keyframe_context_menu = Some(KeyframeContextMenu { clip_id: clip.id, param_kind: kind, keyframe_index: j, screen_pos: p });
            }
        }
        if let Some(p) = response.hover_pos() {
            if let Some((i, j)) = hit(p) {
                let text = match j {
                    Some(j) => format!("Key at {}: drag to move it, Shift-click to add it to the selection, right-click for its menu", state.timecode_display().position_secs(clip.timeline_in.to_secs_f64() + keys[i][j].time_secs)),
                    None => "Click to move the playhead here".to_string(),
                };
                response.on_hover_text(text);
            }
        }
    }

    /// Where the pressed key, from `from`, lands when dragged to `t`.
    #[allow(clippy::too_many_arguments)]
    fn snap(&mut self, free: bool, t: f64, from: f64, playhead: f64, rows: &[(KeyframeParamKind, Rect)], keys: &[Vec<klipp_core::Keyframe>], moving: &[(KeyframeParamKind, f64)], fps: f64, x_of: &dyn Fn(f64) -> f32) -> f64 {
        self.snapped_to = None;
        if free {
            return t;
        }
        let x = x_of(t);
        let others = rows.iter().zip(keys).flat_map(|((kind, _), ks)| ks.iter().filter(move |k| !moving.iter().any(|(mk, mt)| mk == kind && (mt - k.time_secs).abs() < 1e-9)).map(|k| k.time_secs));
        let near = std::iter::once(playhead)
            .chain(others)
            .filter(|c| (c - from).abs() > 1e-9)
            .map(|c| (c, (x_of(c) - x).abs()))
            .filter(|(_, d)| *d <= SNAP_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((c, _)) = near {
            self.snapped_to = Some(c);
            return c;
        }
        (t * fps).round() / fps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_state::InspectorTab;
    use klipp_timeline::test_clip::TestClip;
    use klipp_timeline::Clip;

    /// The Inspector `width` wide with its Keyframes column switched on,
    /// and the column drawn after it as the app draws it.
    struct Shown {
        state: AppState,
        panel: crate::inspector::NewInspectorPanel,
        column: KeyColumn,
        ctx: egui::Context,
        width: f32,
        clock: f64,
        area: Option<KeyArea>,
        cmds: Vec<AppCommand>,
        applied: usize,
    }

    impl Shown {
        fn new(width: f32) -> Self {
            let t = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
            let clip = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0))).lane("opacity", 1.0, 0.5).lane("pos_x", 1.0, 10.0).lane("pos_x", 3.0, 20.0).build();
            let mut state = AppState::default();
            let v = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
            state.selected_clip = Some(clip.id);
            state.sequences[0].tracks[v].clips.push(clip);
            let ctx = egui::Context::default();
            crate::icons::install(&ctx);
            let panel = crate::inspector::NewInspectorPanel { key_column: true, key_column_w: 200.0, ..Default::default() };
            let mut s = Shown { state, panel, column: KeyColumn::default(), ctx, width, clock: 1.0, area: None, cmds: Vec::new(), applied: 0 };
            s.frame(Vec::new());
            s.frame(Vec::new());
            s
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
            self.clock += 0.05;
            let prefs = crate::prefs::Preferences::default();
            let (state, panel, column, cmds) = (&mut self.state, &mut self.panel, &mut self.column, &mut self.cmds);
            let mut area = None;
            let out = self.ctx.run(egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(self.width, 1600.0))), time: Some(self.clock), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut tab = InspectorTab::Video;
                    cmds.extend(panel.show(ui, state, &prefs, &KlippTheme::default(), &mut tab));
                    area = panel.key_area.take();
                    if let Some(a) = &area {
                        column.show(ui, state, &KlippTheme::default(), a, cmds);
                    }
                });
            });
            self.area = area;
            // The keys' edits applied, as the app applies them.
            for c in &self.cmds[self.applied..] {
                let _ = self.state.path_command(c);
            }
            self.applied = self.cmds.len();
            out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
        }

        fn press(&mut self, at: egui::Pos2, button: egui::PointerButton) {
            self.frame(vec![egui::Event::PointerMoved(at)]);
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE }]);
            }
        }

        /// Where the key at `t` seconds sits on `kind`'s strip.
        fn key_at(&self, kind: &KeyframeParamKind, t: f64) -> egui::Pos2 {
            let area = self.area.as_ref().expect("the column drawn");
            let row = area.rows.iter().find(|(k, _)| k == kind).unwrap_or_else(|| panic!("a strip for {kind:?}: {:?}", area.rows)).1;
            let lane = area.column.shrink2(egui::vec2(8.0, 0.0));
            pos2(lane.min.x + (t / 4.0) as f32 * lane.width(), row.center().y)
        }
    }

    /// Each keyed row's strip is in line with the row's name; a key
    /// clicked is selected, an empty spot moves the playhead, and a
    /// right-click opens the key's menu.
    #[test]
    fn a_strip_beside_each_row_takes_clicks() {
        let mut s = Shown::new(560.0);
        let texts = s.frame(Vec::new());
        let px = KeyframeParamKind::Video(klipp_core::VideoParam::PositionX);
        let name_y = texts.iter().find(|(t, _)| t == "Position X").expect("the row").1.y;
        assert!((s.key_at(&px, 1.0).y - name_y).abs() < 6.0, "the strip beside its row");
        let key = s.key_at(&px, 3.0);
        s.press(key, egui::PointerButton::Primary);
        let clip_id = s.state.selected_clip.unwrap();
        assert!(s.state.selected_keyframe.as_ref().is_some_and(|k| k.param_kind == px && k.keyframe_index == 1), "the key at 3 s selected");
        let empty = s.key_at(&px, 2.0);
        s.press(empty, egui::PointerButton::Primary);
        assert!(s.cmds.iter().any(|c| matches!(c, AppCommand::SeekTo(tc) if (tc.to_secs_f64() - 2.0).abs() < 0.05)), "{:?}", s.cmds);
        let key = s.key_at(&px, 1.0);
        s.press(key, egui::PointerButton::Secondary);
        assert!(s.state.keyframe_context_menu.as_ref().is_some_and(|m| m.clip_id == clip_id && m.param_kind == px && m.keyframe_index == 0));
    }

    /// A key dragged along its strip is moved, its new time on a frame.
    #[test]
    fn a_key_dragged_along_its_strip_moves() {
        let mut s = Shown::new(560.0);
        let px = KeyframeParamKind::Video(klipp_core::VideoParam::PositionX);
        let (from, to) = (s.key_at(&px, 1.0), s.key_at(&px, 1.6));
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: egui::Modifiers::NONE };
        s.frame(vec![egui::Event::PointerMoved(from), b(from, true)]);
        for i in 1..=6 {
            s.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 6.0))]);
        }
        s.frame(vec![egui::Event::PointerMoved(to), b(to, false)]);
        let keys = crate::key_selection::track_keys(s.state.sequences[0].all_clips().find(|c| Some(c.id) == s.state.selected_clip).unwrap(), &px);
        let moved = keys[0].time_secs;
        assert!(((moved * 24.0) - (moved * 24.0).round()).abs() < 1e-6 && (moved - 1.6).abs() < 0.1, "{keys:?}");
    }

    /// Too narrow for the rows and the column, the column is not drawn.
    #[test]
    fn a_narrow_inspector_hides_the_column() {
        let s = Shown::new(300.0);
        assert!(s.area.is_none());
    }
}
