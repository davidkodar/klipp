//! A mask made in the clip's picture graph (a Window, a Roto Shape from
//! its Mattes menu) has its shape and handles in the viewer, as a grade's
//! Mask node has: picked in the graph, or the effect it masks picked, its
//! outline is drawn there, and a drag on its handles or, with the Mask
//! tool, on its points edits it by its path, one undo step.

use super::viewer_harness::Viewer;
use super::*;
use klipp_core::{MaskShape, Tool};
use klipp_graph_format::path::{NodePath, Root};
use klipp_timeline::GradeAccess;

/// A paused 4 s clip, selected, with a Gaussian Blur masked by the matte
/// `tag` added from the Mattes menu: the matte's path and the blur's.
fn viewer(tag: &str) -> (Viewer, klipp_core::ClipId, NodePath, NodePath) {
    let mut v = Viewer::new();
    let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    clip.push_effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 30.0 });
    let graph = clip.picture_v2().unwrap().picture.graph.clone();
    let blur = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).expect("the blur").id;
    let level = NodePath::new(Root::Clip { clip: clip.id.0 }, []);
    let matte = uuid::Uuid::new_v4();
    let ops = crate::picture_mattes::add_ops(&graph, &level, crate::picture_mattes::matte(tag).expect("the matte"), matte, (100.0, 100.0), Some(blur)).expect("its ops");
    clip.apply_picture_ops(&ops).expect("added");
    let id = clip.id;
    let track = v.state.sequences[0].tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    track.clips.push(clip);
    v.state.selected_clip = Some(id);
    v.state.active_tool = Tool::Select;
    let path = |n| NodePath::new(Root::Clip { clip: id.0 }, [n]);
    v.frame(Vec::new());
    (v, id, path(matte), path(blur))
}

/// The commands sent since `from`, applied as the app's dispatch applies
/// a path's.
fn apply(v: &mut Viewer, from: usize) -> Vec<AppCommand> {
    let sent: Vec<AppCommand> = v.sent[from..].to_vec();
    for cmd in &sent {
        v.state.before_dispatch(cmd);
        let _ = v.state.path_command(cmd);
    }
    sent
}

/// The matte's first shape, as its node holds it.
fn shape(v: &Viewer, clip: klipp_core::ClipId, matte: &NodePath) -> MaskShape {
    let c = v.state.sequences[0].all_clips().find(|c| c.id == clip).unwrap();
    let picture = c.picture_v2().unwrap();
    let node = picture.picture.graph.nodes.iter().find(|n| Some(&n.id) == matte.nodes.first()).expect("the matte");
    let run = klipp_timeline::grade_node_for_run(node).expect("a grade node");
    run.params.masks.expect("its masks").masks[0].shape.clone()
}

fn at(v: &Viewer, x: f32, y: f32) -> egui::Pos2 {
    let r = v.image_rect();
    egui::pos2(r.min.x + x * r.width(), r.min.y + y * r.height())
}

/// A hand's drag: pressed at `from`, moved to `to` in steps, held there a
/// frame, let go.
fn drag(v: &mut Viewer, from: egui::Pos2, to: egui::Pos2) {
    v.press(from);
    for i in 1..=4 {
        v.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 4.0))]);
    }
    v.frame(vec![egui::Event::PointerMoved(to)]);
    v.release(to);
}

/// Undo one step, as the app's document path does.
fn undo(v: &mut Viewer) {
    v.state.before_dispatch(&AppCommand::Undo);
    let entry = v.state.take_undo().expect("a step to undo");
    let mut doc = klipp_document::apply::DocumentView { sequences: &mut v.state.sequences, active: v.state.active_sequence_idx, media_pool: &mut v.state.media_pool, fps: &mut v.state.fps };
    doc.apply_inverse(&entry.command);
}

fn centre(s: &MaskShape) -> (f32, f32) {
    match s {
        MaskShape::Circle { center_x, center_y, .. } => (*center_x, *center_y),
        other => panic!("not a circle: {other:?}"),
    }
}

/// A Window picked in the graph: its centre dragged a tenth of the frame
/// right moves it there, by its path, one undo step, which undo takes back.
/// The clip stays where it is, and the grade's own window is left alone.
#[test]
fn a_picked_windows_centre_drags_by_its_path() {
    for picked_blur in [false, true] {
        let (mut v, clip, matte, blur) = viewer("matte.window");
        // A window of the grade's own too, elsewhere: left alone.
        let corner = klipp_core::Mask { shape: MaskShape::Circle { center_x: 0.2, center_y: 0.2, radius: 0.1 }, ..Default::default() };
        v.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).unwrap().edit_grade(|g| {
            let id = g.ensure_corrector_window();
            g.nodes.iter_mut().find(|n| n.id == id).unwrap().params.masks = Some(klipp_core::MaskStack { masks: vec![corner] });
        });
        v.state.selected_node_addr = Some(if picked_blur { blur.clone() } else { matte.clone() });
        v.frame(Vec::new());
        let grade = v.state.sequences[0].all_clips().find(|c| c.id == clip).unwrap().grade().clone();
        let steps = v.state.active_history().undo_count();
        let from = v.sent.len();
        let (start, end) = (at(&v, 0.5, 0.5), at(&v, 0.6, 0.5));
        drag(&mut v, start, end);
        let sent = apply(&mut v, from);
        let (x, y) = centre(&shape(&v, clip, &matte));
        assert!((x - 0.6).abs() < 0.01 && (y - 0.5).abs() < 0.01, "picked {}: the centre is at ({x}, {y}); sent {sent:?}", if picked_blur { "the blur" } else { "the window" });
        assert!(sent.iter().any(|c| matches!(c, AppCommand::SetParamAt { addr, .. } if *addr == matte)), "written by its path: {sent:?}");
        assert!(!sent.iter().any(|c| matches!(c, AppCommand::SetNodeMasks { .. })), "nothing for the grade: {sent:?}");
        assert_eq!(v.state.active_history().undo_count(), steps + 1, "one step");
        let after = v.state.sequences[0].all_clips().find(|c| c.id == clip).unwrap();
        assert_eq!(after.grade(), &grade, "the grade as it was");
        assert!(!sent.iter().any(|c| matches!(c, AppCommand::SetParamAt { key, .. } if key.as_str() == "pos_x")), "the clip not moved: {sent:?}");
        undo(&mut v);
        assert_eq!(centre(&shape(&v, clip, &matte)), (0.5, 0.5), "undone");
    }
}

/// Nothing picked: no picture matte's handles (the grade has no mask), so
/// a drag there leaves the window where it is.
#[test]
fn an_unpicked_window_is_left_alone() {
    let (mut v, clip, matte, _) = viewer("matte.window");
    let from = v.sent.len();
    let (start, end) = (at(&v, 0.5, 0.5), at(&v, 0.6, 0.5));
    drag(&mut v, start, end);
    apply(&mut v, from);
    assert_eq!(centre(&shape(&v, clip, &matte)), (0.5, 0.5));
}

/// A Roto Shape picked, the Mask tool up: its four points are there to
/// take, and one dragged moves that point alone, by its path, one step.
#[test]
fn a_picked_roto_shapes_point_drags_with_the_mask_tool() {
    let (mut v, clip, matte, _) = viewer("matte.roto");
    v.state.selected_node_addr = Some(matte.clone());
    v.state.active_tool = Tool::Mask;
    v.frame(Vec::new());
    v.frame(Vec::new());
    let points = |v: &Viewer| match shape(v, clip, &matte) {
        MaskShape::Bezier { points } => points.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(),
        other => panic!("not freeform: {other:?}"),
    };
    let before = points(&v);
    assert_eq!(before.len(), 4);
    let steps = v.state.active_history().undo_count();
    // The right-hand point, (0.7, 0.5), taken and moved down a tenth.
    let from = v.sent.len();
    v.click(at(&v, 0.7, 0.5));
    apply(&mut v, from);
    let from = v.sent.len();
    let (start, end) = (at(&v, 0.7, 0.5), at(&v, 0.7, 0.6));
    drag(&mut v, start, end);
    let sent = apply(&mut v, from);
    let after = points(&v);
    assert!((after[1].0 - 0.7).abs() < 0.01 && (after[1].1 - 0.6).abs() < 0.01, "the point moved: {after:?}; sent {sent:?}");
    for i in [0, 2, 3] {
        assert_eq!(after[i], before[i], "point {i} stays");
    }
    assert!(sent.iter().any(|c| matches!(c, AppCommand::SetParamAt { addr, .. } if *addr == matte)), "written by its path: {sent:?}");
    assert_eq!(v.state.active_history().undo_count(), steps + 1, "one step");
}
