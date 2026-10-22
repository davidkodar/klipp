//! A clip's mask keys on the curve editor and the timeline's key lane
//! (#1947), driven with pointer events as the app draws them, the mask
//! commands applied to the grade as the app's colour handler writes them.

use crate::mask_tracks::{self, MaskTrack};
use crate::panels::curve_editor::CurveEditorPanel;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::mask_path::{BezierPoint, PathEase, PathKey};
use klipp_core::{AppCommand, ClipId, Keyframe, Mask, MaskShape, MaskStack};
use klipp_timeline::GradeAccess;

fn key(t: f64, v: f32) -> Keyframe {
    Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

fn square(off: f32) -> Vec<BezierPoint> {
    [(0.2, 0.2), (0.4, 0.2), (0.4, 0.4), (0.2, 0.4)].into_iter().map(|(x, y)| BezierPoint::corner(x + off, y)).collect()
}

/// A 4 s clip on a Mask node: "Face", a circle whose radius is keyed at
/// 0.5 s and 1.5 s; "Spot", a freeform shape keyed at 1 s and 3 s.
fn state() -> (AppState, ClipId, klipp_nodes::NodeId) {
    let mut state = AppState::default();
    let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    let mut face = Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.2 }, name: "Face".into(), ..Mask::default() };
    face.keyframes.radius = vec![key(0.5, 0.2), key(1.5, 0.3)];
    let mut spot = Mask { shape: MaskShape::Bezier { points: square(0.0) }, name: "Spot".into(), ..Mask::default() };
    spot.keyframes.path = vec![PathKey { time_secs: 1.0, points: square(0.0), ease: PathEase::Linear }, PathKey { time_secs: 3.0, points: square(0.3), ease: PathEase::Linear }];
    let node = clip.edit_grade(|g| {
        let (node, _) = g.ensure_mask_cutout();
        g.nodes.iter_mut().find(|n| n.id == node).unwrap().params.masks = Some(MaskStack { masks: vec![face, spot] });
        node
    });
    let id = clip.id;
    let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    state.sequences[0].tracks[v1].clips.push(clip);
    state.selected_clip = Some(id);
    (state, id, node)
}

fn clip(state: &AppState, id: ClipId) -> klipp_timeline::Clip {
    state.sequences[0].all_clips().find(|c| c.id == id).unwrap().clone()
}

fn track(node: klipp_nodes::NodeId, mask: usize, param: &str) -> MaskTrack {
    MaskTrack { node, mask, param: param.into() }
}

/// The mask commands applied as the app's colour handler applies them,
/// on the clip each names.
fn apply(state: &mut AppState, _id: ClipId, cmds: &[AppCommand]) {
    for cmd in cmds {
        super::colour::mask_key_edit(state, cmd, true);
    }
}

struct Editor {
    state: AppState,
    clip: ClipId,
    node: klipp_nodes::NodeId,
    panel: CurveEditorPanel,
    ctx: egui::Context,
    clock: f64,
    sent: Vec<AppCommand>,
}

impl Editor {
    fn new() -> Self {
        let (state, clip, node) = state();
        let ctx = egui::Context::default();
        crate::icons::install(&ctx);
        let mut e = Editor { state, clip, node, panel: CurveEditorPanel::new(), ctx, clock: 0.0, sent: Vec::new() };
        e.frame(Vec::new());
        e
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
        self.clock += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 500.0))), time: Some(self.clock), events, ..Default::default() };
        let theme = KlippTheme::default();
        let mut cmds = Vec::new();
        let (state, panel) = (&mut self.state, &mut self.panel);
        let out = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                cmds.extend(panel.show(ui, state, &theme));
                crate::panels::timeline::TimelinePanel::show_keyframe_context_menu(ui, state, &mut cmds);
            });
        });
        apply(&mut self.state, self.clip, &cmds);
        self.sent.extend(cmds);
        out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
    }

    fn press(&mut self, at: egui::Pos2, button: egui::PointerButton) {
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE }]);
        }
    }

    fn click_text(&mut self, label: &str) {
        let at = self.frame(Vec::new()).into_iter().rev().find(|(t, _)| t.trim() == label).map(|(_, p)| p).unwrap_or_else(|| panic!("no {label:?}"));
        self.press(at, egui::PointerButton::Primary);
    }

    fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: egui::Modifiers::NONE };
        self.frame(vec![egui::Event::PointerMoved(from), b(from, true)]);
        // Just past egui's drag threshold first, still on the key: the
        // editor finds the dragged key where the drag starts.
        self.frame(vec![egui::Event::PointerMoved(from + (to - from).normalized() * 7.0)]);
        for i in 1..=5 {
            self.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 5.0))]);
        }
        self.frame(vec![egui::Event::PointerMoved(to), b(to, false)]);
        self.frame(Vec::new());
    }

    fn keys(&self, mask: usize, param: &str) -> Vec<Keyframe> {
        mask_tracks::keys(&clip(&self.state, self.clip), &track(self.node, mask, param))
    }
}

/// The clip's keyed mask tracks are listed by their mask's name; a
/// radius key dragged later in time moves there, and the drag is one
/// gesture (begun once, ended once).
#[test]
fn mask_tracks_are_listed_and_a_key_drags_in_time() {
    let mut e = Editor::new();
    let drawn = e.frame(Vec::new());
    for name in ["Face \u{00B7} Radius", "Spot \u{00B7} Shape"] {
        assert!(drawn.iter().any(|(t, _)| t == name), "{name} not listed: {:?}", drawn.iter().map(|(t, _)| t).collect::<Vec<_>>());
    }
    e.click_text("Face \u{00B7} Radius");
    e.frame(Vec::new());
    let from = e.panel.screen_at(0.5, 0.2).unwrap();
    let to = e.panel.screen_at(1.0, 0.2).unwrap();
    let start = e.sent.len();
    e.drag(from, to);
    let sent = &e.sent[start..];
    assert_eq!(sent.iter().filter(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. })).count(), 1, "{sent:?}");
    assert_eq!(sent.iter().filter(|c| matches!(c, AppCommand::EndNodeParamEdit { .. })).count(), 1);
    let keys = e.keys(0, "radius");
    assert!(keys.len() == 2 && (keys[0].time_secs - 1.0).abs() < 0.05, "{keys:?}");
}

/// The shape's keys drag in time only; their menu sets how the shape
/// leaves the key (Ease), and Delete takes a selected one off.
#[test]
fn shape_keys_drag_in_time_take_an_ease_and_delete() {
    let mut e = Editor::new();
    e.click_text("Spot \u{00B7} Shape");
    e.frame(Vec::new());
    let from = e.panel.screen_at(3.0, 0.5).unwrap();
    let to = e.panel.screen_at(2.5, 0.9).unwrap();
    let start = e.sent.len();
    e.drag(from, to);
    assert!(e.sent[start..].iter().all(|c| !matches!(c, AppCommand::MoveMaskKeyframe { value: Some(_), .. })), "a shape key has no value to move");
    let keys = e.keys(1, "shape");
    assert!((keys[1].time_secs - 2.5).abs() < 0.05, "{keys:?}");

    let at = e.panel.screen_at(keys[0].time_secs, 0.5).unwrap();
    e.press(at, egui::PointerButton::Secondary);
    e.click_text("Ease");
    assert_eq!(e.keys(1, "shape")[0].interpolation, klipp_core::Interpolation::Bezier, "{:?}", &e.sent[start..]);
    let m = mask_tracks::mask_of(&clip(&e.state, e.clip), &track(e.node, 1, "shape")).unwrap();
    assert_eq!(m.keyframes.path[0].ease, PathEase::Ease);

    let at = e.panel.screen_at(1.0, 0.5).unwrap();
    e.press(at, egui::PointerButton::Primary);
    let sk = e.state.selected_keyframe.clone().expect("a key selected");
    let remove = super::shortcuts::remove_selected_keyframe(&e.state, &sk).expect("Delete takes it");
    apply(&mut e.state, e.clip, &remove);
    assert_eq!(e.keys(1, "shape").len(), 1);
}

/// "+ Track" offers a mask's unkeyed parameters; picking one keys it at
/// the playhead with its value now, and it is then listed.
#[test]
fn a_mask_track_starts_from_the_track_menu() {
    let mut e = Editor::new();
    e.state.playhead = klipp_core::Timecode::from_secs_f64(2.0, 25);
    e.click_text("+ Track");
    let at = e.frame(Vec::new()).into_iter().rev().find(|(t, _)| t == "Face").unwrap().1;
    for _ in 0..3 {
        e.frame(vec![egui::Event::PointerMoved(at)]);
    }
    e.click_text("Opacity");
    let keys = e.keys(0, "opacity");
    assert!(keys.len() == 1 && (keys[0].time_secs - 2.0).abs() < 1e-6 && keys[0].value == 1.0, "{keys:?}");
    assert!(e.frame(Vec::new()).iter().any(|(t, _)| t == "Face \u{00B7} Opacity"));
}

/// The timeline's key lane: its pill names the track, and clicked goes on
/// to the clip's next keyed one; a shape key pressed and dragged along
/// the clip's top moves in time, as one gesture.
#[test]
fn the_lane_names_its_track_cycles_and_drags_a_key() {
    let (mut state, id, node) = state();
    let rect = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(800.0, 60.0));
    let ctx = egui::Context::default();
    let mut clock = 0.0;
    let mut frame = |state: &mut AppState, events: Vec<egui::Event>, track: &MaskTrack| -> (bool, Vec<AppCommand>, Vec<String>) {
        clock += 0.05;
        let mut cmds = Vec::new();
        let mut cycle = false;
        let c = clip(state, id);
        let pointer = events.iter().find_map(|e| match e { egui::Event::PointerMoved(p) => Some(*p), egui::Event::PointerButton { pos, .. } => Some(*pos), _ => None });
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 300.0))), time: Some(clock), events, ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let pointer = pointer.or_else(|| ui.input(|i| i.pointer.interact_pos()));
                cycle = crate::timeline::mask_lane::draw(ui, ui.painter(), rect, &c, track, state, &mut cmds, true, pointer, &KlippTheme::default());
            });
        });
        apply(state, id, &cmds);
        (cycle, cmds, out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some(t.galley.text().to_string()), _ => None }).collect())
    };
    // Shown with no picture keys: the clip's first keyed mask track.
    let shown = crate::timeline::mask_lane::shown(&state, &clip(&state, id), false).unwrap();
    assert_eq!(shown, track(node, 0, "radius"));
    let shape = track(node, 1, "shape");
    let (_, _, texts) = frame(&mut state, Vec::new(), &shape);
    assert!(texts.iter().any(|t| t == "Spot \u{00B7} Shape"), "{texts:?}");
    let pill = rect.left_bottom() + egui::vec2(12.0, -10.0);
    let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: egui::Modifiers::NONE };
    let (cycle, _, _) = frame(&mut state, vec![egui::Event::PointerMoved(pill), b(pill, true)], &shape);
    frame(&mut state, vec![b(pill, false)], &shape);
    assert!(cycle, "the pill goes on");
    assert_eq!(crate::timeline::mask_lane::next(&clip(&state, id), Some(&shape), false), Some(track(node, 0, "radius")), "round to the first");
    assert_eq!(crate::timeline::mask_lane::next(&clip(&state, id), Some(&shape), true), None, "back to the picture's");

    // The key at 3 s, on the clip's top, dragged to 2 s.
    let x = |t: f32| rect.left() + t / 4.0 * rect.width();
    let (from, to) = (egui::pos2(x(3.0), rect.top() + 7.0), egui::pos2(x(2.0), rect.top() + 7.0));
    let mut sent = Vec::new();
    sent.extend(frame(&mut state, vec![egui::Event::PointerMoved(from), b(from, true)], &shape).1);
    for i in 1..=4 {
        sent.extend(frame(&mut state, vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 4.0))], &shape).1);
    }
    sent.extend(frame(&mut state, vec![b(to, false)], &shape).1);
    assert_eq!(sent.iter().filter(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. })).count(), 1, "{sent:?}");
    assert_eq!(sent.iter().filter(|c| matches!(c, AppCommand::EndNodeParamEdit { .. })).count(), 1);
    let keys = mask_tracks::keys(&clip(&state, id), &shape);
    assert!((keys[1].time_secs - 2.0).abs() < 0.02, "{keys:?}");
}

/// A Mask node inside a compound is found where it is, so its keys are
/// written there.
#[test]
fn a_nested_mask_is_found_by_its_graph() {
    let (state, id, node) = state();
    let mut outer = klipp_nodes::NodeGraph::default();
    let mut compound = klipp_nodes::Node { id: klipp_nodes::NodeId::new_v4(), kind: klipp_nodes::NodeKind::Compound, label: String::new(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] };
    compound.params.compound_subgraph = Some(clip(&state, id).grade().clone());
    let cid = compound.id;
    outer.nodes.push(compound);
    assert_eq!(super::colour::graph_path_for(&outer, &[node], &[]), Some(vec![cid]));
    // And its tracks are listed from inside it.
    let mut c = clip(&state, id);
    c.set_grade(outer);
    assert!(mask_tracks::keyed(&c).iter().any(|(t, n)| t.node == node && n == "Spot \u{00B7} Shape"));
}

/// A Mask node's id is only unique within one clip's grade: a duplicate's
/// can be another clip's. A key moved on one clip leaves the other's
/// mask, under the same node id, alone; and a command naming a clip
/// without the node changes nothing and records nothing.
#[test]
fn a_key_moves_only_on_the_clip_it_names() {
    let (mut state, a, node) = state();
    let mut twin = clip(&state, a);
    twin.id = ClipId::new();
    let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
    twin.timeline_in = t(5.0);
    twin.timeline_out = t(9.0);
    let b = twin.id;
    state.sequences[0].tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().clips.push(twin);
    let radius = track(node, 0, "radius");
    let keys = |state: &AppState, c: ClipId| mask_tracks::keys(&clip(state, c), &radius);
    assert_eq!(keys(&state, a), keys(&state, b), "the same mask under the same id on both");
    let cmd = AppCommand::MoveMaskKeyframe { clip_id: b, node_id: node, mask_index: 0, param: "radius".into(), time_secs: 0.5, new_time_secs: 1.0, value: Some(0.25) };
    assert_eq!(super::colour::mask_key_edit(&mut state, &cmd, true), Some(Ok(())));
    assert_eq!(keys(&state, b)[0].time_secs, 1.0, "moved on the clip named");
    assert_eq!(keys(&state, a)[0].time_secs, 0.5, "the other clip's mask left alone");
    // A clip whose grade has no such node: nothing, and no step.
    let steps = state.active_history().undo_count();
    let elsewhere = AppCommand::MoveMaskKeyframe { clip_id: a, node_id: klipp_nodes::NodeId::new_v4(), mask_index: 0, param: "radius".into(), time_secs: 0.5, new_time_secs: 1.0, value: None };
    assert_eq!(super::colour::mask_key_edit(&mut state, &elsewhere, true), None);
    assert_eq!(state.active_history().undo_count(), steps);
}
