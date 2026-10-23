//! The viewer's Mask tool (#74) driven with real pointer and key events
//! through `ViewerPanel::show`, its mask commands applied to the clip's
//! grade as the app applies them.

use super::viewer_harness::{button, Viewer};
use super::*;
use klipp_core::mask_path::BezierPoint;
use klipp_core::{MaskShape, Tool};
use klipp_timeline::GradeAccess;

/// The viewer on a paused clip, selected, with the Mask tool up.
fn viewer() -> (Viewer, klipp_core::ClipId) {
    let mut v = Viewer::new();
    let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
    let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    let id = clip.id;
    let track = v.state.sequences[0].tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    track.clips.push(clip);
    v.state.selected_clip = Some(id);
    v.state.active_tool = Tool::Mask;
    v.frame(Vec::new());
    v.frame(Vec::new());
    (v, id)
}

/// The commands sent since `from`, the mask ones applied to the clip's
/// grade as the app's colour handlers write them.
fn apply(v: &mut Viewer, clip: klipp_core::ClipId, from: usize) -> Vec<AppCommand> {
    let sent: Vec<AppCommand> = v.sent[from..].to_vec();
    for cmd in &sent {
        let c = v.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).unwrap();
        match cmd {
            AppCommand::AddMaskShape { .. } => {
                c.edit_grade(|g| super::colour::write_node_settings(g, cmd));
            }
            AppCommand::SetNodeMasks { node_id, masks, .. } => {
                c.edit_grade(|g| g.nodes.iter_mut().find(|n| n.id == *node_id).unwrap().params.masks = Some(masks.clone()));
            }
            _ => {}
        }
    }
    sent
}

fn masks(v: &Viewer, clip: klipp_core::ClipId) -> Vec<klipp_core::Mask> {
    let c = v.state.sequences[0].all_clips().find(|c| c.id == clip).unwrap();
    c.grade().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Mask).and_then(|n| n.params.masks.clone()).map_or(Vec::new(), |m| m.masks)
}

fn points(v: &Viewer, clip: klipp_core::ClipId, i: usize) -> Vec<BezierPoint> {
    match &masks(v, clip)[i].shape {
        MaskShape::Bezier { points } => points.clone(),
        other => panic!("not freeform: {other:?}"),
    }
}

/// Where the frame fraction `(x, y)` is on the screen.
fn at(v: &Viewer, x: f32, y: f32) -> egui::Pos2 {
    let r = v.image_rect();
    egui::pos2(r.min.x + x * r.width(), r.min.y + y * r.height())
}

fn close(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() < 2e-3 && (a.1 - b.1).abs() < 2e-3
}

fn key(v: &mut Viewer, key: egui::Key) {
    let mods = v.modifiers;
    v.frame(vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: mods }]);
    v.frame(Vec::new());
}

/// A press at `from`, moved there in steps to `to`, let go: with the
/// held modifiers.
fn drag(v: &mut Viewer, from: egui::Pos2, to: egui::Pos2) {
    let mods = v.modifiers;
    let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: mods };
    v.frame(vec![egui::Event::PointerMoved(from), b(from, true)]);
    for i in 1..=4 {
        v.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 4.0))]);
    }
    v.frame(vec![egui::Event::PointerMoved(to), b(to, false)]);
    v.frame(Vec::new());
}

fn click(v: &mut Viewer, p: egui::Pos2) {
    let mods = v.modifiers;
    v.frame(vec![egui::Event::PointerMoved(p), egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: mods }]);
    v.frame(vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: mods }]);
    v.frame(Vec::new());
}

/// A clip with one freeform mask of `pts`, drawn, the tool up.
fn with_shape(pts: Vec<BezierPoint>) -> (Viewer, klipp_core::ClipId) {
    let (mut v, id) = viewer();
    let from = v.sent.len();
    v.sent.push(AppCommand::AddMaskShape { clip_id: id, shape: MaskShape::Bezier { points: pts } });
    apply(&mut v, id, from);
    v.frame(Vec::new());
    (v, id)
}

fn square() -> Vec<BezierPoint> {
    [(0.3, 0.3), (0.7, 0.3), (0.7, 0.7), (0.3, 0.7)].into_iter().map(BezierPoint::from).collect()
}

/// The pen: three clicks put three corners down, a click on the first
/// closes them into a mask on a Mask node made for it, as one command,
/// and the new shape's points are then selected.
#[test]
fn the_pen_draws_a_shape_and_closes_it_on_its_first_point() {
    let (mut v, id) = viewer();
    let from = v.sent.len();
    for (x, y) in [(0.3, 0.3), (0.7, 0.35), (0.5, 0.7)] {
        let p = at(&v, x, y);
        click(&mut v, p);
    }
    assert_eq!(v.state.mask_edit.pen.len(), 3);
    assert!(masks(&v, id).is_empty(), "no mask until it's closed");
    let first = at(&v, 0.3, 0.3);
    click(&mut v, first);
    let sent = apply(&mut v, id, from);
    assert_eq!(sent.iter().filter(|c| matches!(c, AppCommand::AddMaskShape { .. })).count(), 1, "{sent:?}");
    v.frame(Vec::new());
    let pts = points(&v, id, 0);
    assert_eq!(pts.len(), 3);
    assert!(pts.iter().all(BezierPoint::is_corner));
    assert!(close(pts[1].pos(), (0.7, 0.35)), "{pts:?}");
    assert!(v.state.mask_edit.pen.is_empty());
    assert_eq!(v.state.mask_edit.selected.len(), 3, "the new shape's points selected");
}

/// A drag with the pen pulls out a smooth point's handles; Enter closes
/// the shape, Escape drops one being drawn.
#[test]
fn a_pen_drag_pulls_handles_and_enter_or_escape_ends_the_shape() {
    let (mut v, id) = viewer();
    let from = v.sent.len();
    let (a, b) = (at(&v, 0.3, 0.3), at(&v, 0.4, 0.3));
    drag(&mut v, a, b);
    let p = v.state.mask_edit.pen[0];
    assert!(p.smooth && close(p.out_handle(), (0.4, 0.3)) && close(p.in_handle(), (0.2, 0.3)), "{p:?}");
    for (x, y) in [(0.7, 0.4), (0.5, 0.75)] {
        let q = at(&v, x, y);
        click(&mut v, q);
    }
    key(&mut v, egui::Key::Enter);
    apply(&mut v, id, from);
    v.frame(Vec::new());
    let pts = points(&v, id, 0);
    assert!(pts[0].smooth && !pts[1].smooth);

    let q = at(&v, 0.1, 0.1);
    v.state.mask_edit.selected.clear();
    click(&mut v, q);
    assert_eq!(v.state.mask_edit.pen.len(), 1);
    key(&mut v, egui::Key::Escape);
    assert!(v.state.mask_edit.pen.is_empty(), "Escape drops the shape being drawn");
    assert_eq!(masks(&v, id).len(), 1);
}

/// A point dragged moves, its handles with it, in one undo step; a
/// smooth point's handle drags its partner in line, and Alt breaks it.
#[test]
fn points_and_handles_drag_as_one_step_each() {
    let mut pts = square();
    pts[1] = BezierPoint { in_dx: -0.1, in_dy: 0.0, out_dx: 0.1, out_dy: 0.0, smooth: true, ..pts[1] };
    let (mut v, id) = with_shape(pts);
    let from = v.sent.len();
    let (a, b) = (at(&v, 0.7, 0.3), at(&v, 0.72, 0.25));
    drag(&mut v, a, b);
    let sent = apply(&mut v, id, from);
    assert_eq!(sent.iter().filter(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. })).count(), 1, "{sent:?}");
    assert_eq!(sent.iter().filter(|c| matches!(c, AppCommand::EndNodeParamEdit { .. })).count(), 1);
    let p = points(&v, id, 0)[1];
    assert!(close(p.pos(), (0.72, 0.25)) && close(p.out_handle(), (0.82, 0.25)), "{p:?}");
    assert_eq!(v.state.mask_edit.selected, [1].into());

    // The out handle swung up: the in handle follows, in line.
    let (a, b) = (at(&v, 0.82, 0.25), at(&v, 0.72, 0.1));
    let from = v.sent.len();
    drag(&mut v, a, b);
    apply(&mut v, id, from);
    let p = points(&v, id, 0)[1];
    assert!(close(p.out_handle(), (0.72, 0.1)), "{p:?}");
    assert!(p.in_dy > 0.0 && p.in_dx.abs() < 1e-3, "in line: {p:?}");

    // Alt: the handles part.
    v.modifiers = egui::Modifiers::ALT;
    let before = p;
    let (a, b) = (at(&v, 0.72, 0.1), at(&v, 0.9, 0.2));
    let from = v.sent.len();
    drag(&mut v, a, b);
    v.modifiers = egui::Modifiers::NONE;
    apply(&mut v, id, from);
    let p = points(&v, id, 0)[1];
    assert!(!p.smooth && (p.in_dx, p.in_dy) == (before.in_dx, before.in_dy), "{p:?}");
}

/// Alt-click makes a corner smooth and back; double-click on the curve
/// adds a point there without changing the shape; Delete takes the
/// selected point away, and keeps a shape at three.
#[test]
fn alt_click_insert_and_delete_points() {
    let (mut v, id) = with_shape(square());
    v.modifiers = egui::Modifiers::ALT;
    let p = at(&v, 0.7, 0.3);
    let from = v.sent.len();
    click(&mut v, p);
    v.modifiers = egui::Modifiers::NONE;
    apply(&mut v, id, from);
    assert!(points(&v, id, 0)[1].smooth);

    let from = v.sent.len();
    let edge = at(&v, 0.5, 0.7);
    v.frame(vec![egui::Event::PointerMoved(edge), button(edge, true)]);
    v.frame(vec![button(edge, false)]);
    v.frame(vec![button(edge, true)]);
    v.frame(vec![button(edge, false)]);
    v.frame(Vec::new());
    apply(&mut v, id, from);
    let pts = points(&v, id, 0);
    assert_eq!(pts.len(), 5, "a point added");
    assert!(close(pts[3].pos(), (0.5, 0.7)), "{pts:?}");
    assert_eq!(v.state.mask_edit.selected, [3].into());

    assert!(v.state.mask_keys_owned(), "the viewer has the keys");
    let from = v.sent.len();
    key(&mut v, egui::Key::Delete);
    apply(&mut v, id, from);
    assert_eq!(points(&v, id, 0).len(), 4);
}

/// Delete with mask points selected is the mask's: the timeline's
/// Delete stands aside and the clip stays. With the tool down, or no
/// points selected, Delete removes the selected clip as before.
#[test]
fn delete_takes_points_not_the_clip_only_while_the_tool_has_the_keys() {
    use super::shortcuts::{delete_reaches_timeline, delete_selection};
    let (mut v, id) = with_shape(square());
    let p = at(&v, 0.3, 0.7);
    click(&mut v, p);
    assert!(!v.state.mask_edit.selected.is_empty());
    assert!(!delete_reaches_timeline(&v.state), "the points have Delete");
    let from = v.sent.len();
    key(&mut v, egui::Key::Delete);
    apply(&mut v, id, from);
    assert_eq!(points(&v, id, 0).len(), 3, "the point went");
    assert!(v.state.sequences[0].all_clips().any(|c| c.id == id), "the clip stayed");

    // Controls: the tool down, then the tool up with nothing selected.
    v.state.mask_edit.selected = [0].into();
    v.state.active_tool = Tool::Select;
    assert!(delete_reaches_timeline(&v.state));
    assert!(matches!(delete_selection(&mut v.state).as_slice(), [AppCommand::RemoveClip { clip_id }] if *clip_id == id));
    v.state.active_tool = Tool::Mask;
    v.state.mask_edit.selected.clear();
    assert!(delete_reaches_timeline(&v.state));
    // And with points selected but another panel focused.
    v.state.mask_edit.selected = [0].into();
    v.state.active_panel = Some(crate::state::PanelKind::Timeline);
    assert!(delete_reaches_timeline(&v.state));
}

/// A marquee over two points selects them; dragging inside the shape
/// moves all of it.
#[test]
fn a_marquee_selects_and_the_shape_moves_whole() {
    let (mut v, id) = with_shape(square());
    let p = at(&v, 0.3, 0.3);
    click(&mut v, p);
    let (a, b) = (at(&v, 0.6, 0.2), at(&v, 0.8, 0.8));
    drag(&mut v, a, b);
    assert_eq!(v.state.mask_edit.selected, [1, 2].into());
    let from = v.sent.len();
    let (a, b) = (at(&v, 0.5, 0.5), at(&v, 0.55, 0.5));
    drag(&mut v, a, b);
    apply(&mut v, id, from);
    let pts = points(&v, id, 0);
    assert!(close(pts[0].pos(), (0.35, 0.3)) && close(pts[2].pos(), (0.75, 0.7)), "{pts:?}");
}

/// The overlay is drawn in the theme's colours, each line over a dark
/// stroke so it reads on any picture: the selected point in the accent,
/// the others in the panel's dark.
#[test]
fn the_overlay_reads_on_any_picture() {
    let (mut v, _) = with_shape(square());
    let p = at(&v, 0.7, 0.3);
    click(&mut v, p);
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))), time: Some(v.clock + 0.05), ..Default::default() };
    let (panel, state, theme) = (&mut v.panel, &mut v.state, &v.theme);
    let out = v.ctx.run(input, |ctx| {
        egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| { panel.show(ui, state, theme, &crate::keybindings::KeyBindings::default()); });
    });
    let fills: Vec<(egui::Rect, egui::Color32)> = out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Rect(r) => Some((r.rect, r.fill)), _ => None }).collect();
    let point = |x: f32, y: f32| at(&v, x, y);
    let filled = |p: egui::Pos2, c: egui::Color32| fills.iter().any(|(r, f)| *f == c && r.contains(p) && r.width() < 10.0);
    assert!(filled(point(0.7, 0.3), v.theme.accent), "the selected point in the accent");
    assert!(filled(point(0.3, 0.3), v.theme.bg_deep), "the others dark");
    let under = out.shapes.iter().any(|s| match &s.shape {
        egui::Shape::Path(p) => p.stroke.color == egui::epaint::ColorMode::Solid(egui::Color32::from_black_alpha(170)) && p.closed,
        _ => false,
    });
    assert!(under, "the outline over a dark stroke");
}

/// A locked mask is drawn but takes no edits: a drag on its point starts
/// none, and a click there starts the pen instead.
#[test]
fn a_locked_mask_takes_no_edits() {
    let (mut v, id) = with_shape(square());
    let c = v.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(id)).unwrap();
    c.edit_grade(|g| g.nodes.iter_mut().find(|n| n.kind == klipp_nodes::NodeKind::Mask).unwrap().params.masks.as_mut().unwrap().masks[0].locked = true);
    v.frame(Vec::new());
    let from = v.sent.len();
    let (a, b) = (at(&v, 0.7, 0.3), at(&v, 0.75, 0.25));
    drag(&mut v, a, b);
    assert!(!v.sent[from..].iter().any(|c| matches!(c, AppCommand::SetNodeMasks { .. })), "{:?}", &v.sent[from..]);
    assert!(close(points(&v, id, 0)[1].pos(), (0.7, 0.3)));
}

/// Moving a mask in its stack keeps every other mask in order, and a
/// tracker's mask index follows its mask.
#[test]
fn a_moved_mask_keeps_the_rest_in_order() {
    use super::colour::{move_mask, moved_index};
    let mut g = klipp_nodes::NodeGraph::default();
    let (node, _) = g.ensure_mask_cutout();
    let names = ["a", "b", "c", "d"];
    g.nodes.iter_mut().find(|n| n.id == node).unwrap().params.masks = Some(klipp_core::MaskStack { masks: names.iter().map(|n| klipp_core::Mask { name: (*n).into(), ..klipp_core::Mask::default() }).collect() });
    assert!(move_mask(&mut g, node, 0, 2));
    let now: Vec<String> = g.nodes.iter().find(|n| n.id == node).unwrap().params.masks.as_ref().unwrap().masks.iter().map(|m| m.name.clone()).collect();
    assert_eq!(now, ["b", "c", "a", "d"]);
    for (i, name) in names.iter().enumerate() {
        assert_eq!(now[moved_index(i, 0, 2)], *name, "{name}");
        assert_eq!(["d", "a", "b", "c"].iter().position(|n| n == name).unwrap(), moved_index(i, 3, 0), "{name} up");
    }
    assert!(!move_mask(&mut g, node, 1, 9), "no place 9");
}

/// P arms the Mask tool and nothing else; the performance overlay and
/// its recording, developer aids, are on Ctrl+Shift+P and Ctrl+Alt+P.
#[test]
fn p_is_the_mask_tool() {
    let kb = crate::keybindings::KeyBindings::defaults();
    assert_eq!(kb.display_for("mask_tool"), "P");
    assert_eq!(kb.display_for("toggle_perf_overlay"), "Ctrl+Shift+P");
    assert_eq!(kb.display_for("toggle_perf_recording"), "Ctrl+Alt+P");
    let ctx = egui::Context::default();
    let press = |mods: egui::Modifiers| {
        let mut on = Vec::new();
        let input = egui::RawInput { events: vec![egui::Event::Key { key: egui::Key::P, physical_key: None, pressed: true, repeat: false, modifiers: mods }], modifiers: mods, ..Default::default() };
        let _ = ctx.run(input, |ctx| {
            ctx.input(|i| on = ["mask_tool", "toggle_perf_overlay", "toggle_perf_recording"].into_iter().filter(|a| kb.is_action_pressed(a, i)).collect());
        });
        on
    };
    assert_eq!(press(egui::Modifiers::NONE), ["mask_tool"]);
    assert_eq!(press(egui::Modifiers::CTRL | egui::Modifiers::SHIFT), ["toggle_perf_overlay"]);
    assert_eq!(press(egui::Modifiers::CTRL | egui::Modifiers::ALT), ["toggle_perf_recording"]);
    // Every editor layout gives P to the pen, and to nothing else.
    for preset in [crate::keymap_presets::PREMIERE, crate::keymap_presets::AVID, crate::keymap_presets::RESOLVE, crate::keymap_presets::FINAL_CUT] {
        let on_p: Vec<&str> = preset.iter().filter(|(_, keys)| keys.contains(&"P")).map(|(a, _)| *a).collect();
        assert_eq!(on_p, ["mask_tool"]);
    }
    assert!(include_str!("shortcuts.rs").contains("cmds.push(AppCommand::SetActiveTool(Tool::Mask))"));
}

/// A press in another panel takes the keys from the viewer: the points
/// selected there are let go, so the Delete that follows is the other
/// panel's and the points are not shown as still taking it.
#[test]
fn points_are_let_go_when_another_panel_takes_the_focus() {
    let (mut v, _) = with_shape(square());
    let p = at(&v, 0.3, 0.3);
    click(&mut v, p);
    assert!(v.state.mask_keys_owned());
    // As the timeline notes a press inside it.
    v.state.active_panel = Some(crate::state::PanelKind::Timeline);
    v.frame(Vec::new());
    assert!(v.state.mask_edit.selected.is_empty(), "the selection went with the focus");
    assert!(!v.state.mask_keys_owned());
    // The pen's shape too.
    let q = at(&v, 0.1, 0.1);
    click(&mut v, q);
    assert_eq!(v.state.mask_edit.pen.len(), 1);
    v.state.active_panel = Some(crate::state::PanelKind::Timeline);
    v.frame(Vec::new());
    assert!(v.state.mask_edit.pen.is_empty());
}

/// Ctrl+X, Lift and Extract leave the clip while the Mask tool has the
/// keys, as Delete does; with the tool down Ctrl+X cuts it as before.
#[test]
fn cut_lift_and_extract_stand_aside_for_the_mask_points() {
    use super::shortcuts::removal_keys;
    let (mut v, id) = with_shape(square());
    let p = at(&v, 0.3, 0.3);
    click(&mut v, p);
    let kb = crate::keybindings::KeyBindings::defaults();
    let ctx = egui::Context::default();
    // Punctuation arrives as text, as egui reports it.
    let pressed = |state: &crate::state::AppState, key: egui::Key, mods: egui::Modifiers| {
        let mut out = Vec::new();
        let event = match key {
            egui::Key::Semicolon => egui::Event::Text(";".into()),
            egui::Key::Quote => egui::Event::Text("'".into()),
            _ => egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: mods },
        };
        let input = egui::RawInput { events: vec![event], modifiers: mods, ..Default::default() };
        let _ = ctx.run(input, |ctx| ctx.input(|i| out = removal_keys(state, &kb, i)));
        out
    };
    assert!(v.state.mask_keys_owned());
    assert!(pressed(&v.state, egui::Key::X, egui::Modifiers::CTRL).is_empty(), "Ctrl+X");
    assert!(pressed(&v.state, egui::Key::Semicolon, egui::Modifiers::NONE).is_empty(), "Lift");
    assert!(pressed(&v.state, egui::Key::Quote, egui::Modifiers::NONE).is_empty(), "Extract");
    v.state.active_tool = Tool::Select;
    assert!(matches!(pressed(&v.state, egui::Key::X, egui::Modifiers::CTRL).as_slice(), [AppCommand::CutClip { clip_id }] if *clip_id == id));
    assert!(matches!(pressed(&v.state, egui::Key::Semicolon, egui::Modifiers::NONE).as_slice(), [AppCommand::LiftEdit]));
    assert!(matches!(pressed(&v.state, egui::Key::Quote, egui::Modifiers::NONE).as_slice(), [AppCommand::ExtractEdit]));
}

/// A keyed shape (#74) is drawn and edited as its keys have it at the
/// playhead: a point dragged there makes a key there and leaves the
/// others; a point added on the curve goes into every key.
#[test]
fn a_keyed_shape_is_edited_at_the_playhead() {
    use klipp_core::mask_path::{PathEase, PathKey};
    let moved = |off: f32| square().into_iter().map(|p| BezierPoint { x: p.x + off, ..p }).collect::<Vec<_>>();
    let (mut v, id) = with_shape(square());
    let c = v.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(id)).unwrap();
    c.edit_grade(|g| {
        let m = &mut g.nodes.iter_mut().find(|n| n.kind == klipp_nodes::NodeKind::Mask).unwrap().params.masks.as_mut().unwrap().masks[0];
        m.keyframes.path = vec![
            PathKey { time_secs: 0.0, points: moved(0.0), ease: PathEase::Linear },
            PathKey { time_secs: 2.0, points: moved(0.2), ease: PathEase::Linear },
        ];
    });
    // At 1 s the shape is halfway: its first point at 0.4.
    v.state.playhead = klipp_core::Timecode::from_secs_f64(1.0, 25);
    v.state.playhead_smooth_secs = 1.0;
    v.frame(Vec::new());
    let from = v.sent.len();
    let (a, b) = (at(&v, 0.4, 0.3), at(&v, 0.4, 0.2));
    drag(&mut v, a, b);
    apply(&mut v, id, from);
    let keys = masks(&v, id)[0].keyframes.path.clone();
    assert_eq!(keys.iter().map(|k| k.time_secs).collect::<Vec<_>>(), [0.0, 1.0, 2.0], "a key at the playhead");
    assert!(close(keys[1].points[0].pos(), (0.4, 0.2)), "{:?}", keys[1].points[0]);
    assert!(close(keys[0].points[0].pos(), (0.3, 0.3)) && close(keys[2].points[0].pos(), (0.5, 0.3)), "the others as they were");

    let from = v.sent.len();
    let edge = at(&v, 0.5, 0.7);
    v.frame(vec![egui::Event::PointerMoved(edge), button(edge, true)]);
    v.frame(vec![button(edge, false)]);
    v.frame(vec![button(edge, true)]);
    v.frame(vec![button(edge, false)]);
    v.frame(Vec::new());
    apply(&mut v, id, from);
    let m = &masks(&v, id)[0];
    assert!(m.keyframes.path.iter().all(|k| k.points.len() == 5), "{:?}", m.keyframes.path.iter().map(|k| k.points.len()).collect::<Vec<_>>());
    assert_eq!(points(&v, id, 0).len(), 5);
}
