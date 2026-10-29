//! A matte beside a clip's picture chain is edited under the effect it
//! masks (#1746), through the drawn inspector: a luma key's number typed,
//! a window's shape added from its own menu, a difference key's plate
//! cleared. Each is written to the matte by path, one undo step, and
//! drawn by the renderer.

use super::parity_harness::Run;
use super::parity_specialists::{number_right_of, type_into};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::NodeId;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::Clip;

/// A two-second clip with a blur masked by the matte `tag` beside its
/// chain, and the matte's id.
fn masked(tag: &str) -> (Clip, NodeId) {
    let t = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut c = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 4.0 }).build();
    let graph = c.picture_v2().unwrap().picture.graph.clone();
    let blur = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
    let id = NodeId::new_v4();
    let m = crate::picture_mattes::matte(tag).unwrap();
    let ops = crate::picture_mattes::add_ops(&graph, &NodePath::new(Root::Clip { clip: c.id.0 }, []), m, id, (0.0, 0.0), Some(blur)).unwrap();
    c.apply_picture_ops(&ops).unwrap();
    (c, id)
}

/// The matte's settings, as a grade run reads them.
fn params(run: &Run, id: NodeId) -> klipp_nodes::NodeParams {
    let picture = run.clip().picture_v2().unwrap();
    let node = picture.picture.graph.nodes.iter().find(|n| n.id == id).unwrap();
    klipp_timeline::grade_node_for_run(node).unwrap().params
}

/// One step, undone to the picture before and redone to the picture after.
fn one_step(run: &mut Run, before: &klipp_graph_format::lifted::ClipPicture) -> Result<(), String> {
    if !run.unapplied.is_empty() {
        return Err(format!("unapplied: {:?}", run.unapplied));
    }
    let depth = run.state.active_history().undo_count();
    if depth != 1 {
        return Err(format!("{depth} undo steps, not one"));
    }
    let after = run.clip().picture_v2().unwrap().picture.clone();
    if !run.step(false) || run.clip().picture_v2().unwrap().picture != *before {
        return Err("undo doesn't give back the picture before".into());
    }
    if !run.step(true) || run.clip().picture_v2().unwrap().picture != after {
        return Err("redo doesn't give back the picture after".into());
    }
    Ok(())
}

#[test]
fn a_luma_keys_number_is_typed_under_the_effect_it_masks() {
    let (c, id) = masked("matte.luma_key");
    let mut run = Run::new(c.clone());
    let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
    assert!(drawn.iter().any(|(t, _)| t == "Masked by: Luma Key"), "the chip: {:?}", drawn.iter().map(|d| &d.0).collect::<Vec<_>>());
    let before = run.clip().picture_v2().unwrap().picture.clone();
    let at = number_right_of(&mut run, "Low").expect("the key's Low row");
    type_into(&mut run, at, "0.3");
    assert_eq!(params(&run, id).luma_key.unwrap().low, 0.3);
    one_step(&mut run, &before).unwrap();
    // Drawn: the key at 0.3 masks the blur elsewhere than at 0.6.
    super::parity_pixels::clip_edit_drawn_on(super::parity_pixels::Routes::Both, super::parity_pixels::SMALL, &c, run.clip()).unwrap();
}

/// #2576: a qualifier beside the chain keys the picture by colour for
/// the effect it masks; its hue typed under that effect, one step,
/// drawn on both routes.
#[test]
fn a_qualifiers_hue_is_typed_under_the_effect_it_masks() {
    let (c, id) = masked("matte.qualifier");
    assert!(params(&Run::new(c.clone()), id).qualifier.is_some_and(|q| q.enabled), "it starts on");
    let mut run = Run::new(c.clone());
    let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
    assert!(drawn.iter().any(|(t, _)| t == "Masked by: Qualifier"), "the chip: {:?}", drawn.iter().map(|d| &d.0).collect::<Vec<_>>());
    let before = run.clip().picture_v2().unwrap().picture.clone();
    let at = number_right_of(&mut run, "Hue Center").expect("the qualifier's Hue Center row");
    type_into(&mut run, at, "120");
    assert_eq!(params(&run, id).qualifier.unwrap().hue_center, 120.0);
    one_step(&mut run, &before).unwrap();
    super::parity_pixels::clip_edit_drawn_on(super::parity_pixels::Routes::Both, super::parity_pixels::SMALL, &c, run.clip()).unwrap();
}

#[test]
fn a_windows_shape_is_added_from_its_own_menu() {
    let (c, id) = masked("matte.window");
    let mut run = Run::new(c);
    let before = run.clip().picture_v2().unwrap().picture.clone();
    assert!(run.click_last("+ Add Mask"), "the window's mask menu");
    assert!(run.click_last("Rectangle"), "a rectangle in it");
    let masks = params(&run, id).masks.unwrap().masks;
    assert_eq!(masks.len(), 2, "the circle it started with and the rectangle");
    assert!(matches!(masks[1].shape, klipp_core::MaskShape::Rectangle { .. }));
    one_step(&mut run, &before).unwrap();
}

#[test]
fn a_difference_keys_plate_is_cleared() {
    let (mut c, id) = masked("matte.difference_key");
    // A plate chosen, as Browse leaves it.
    let path = NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    let key = klipp_core::DifferenceKeySettings { enabled: true, plate_path: Some("/nonexistent/klipp-parity/plate.png".into()), ..Default::default() };
    let value = klipp_graph_format::ParamValue::Text(serde_json::to_string(&key).unwrap());
    c.apply_picture_ops(&[klipp_graph_format::op::GraphOp::SetParam { path, key: "difference_key".into(), value }]).unwrap();
    let mut run = Run::new(c);
    let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
    assert!(drawn.iter().any(|(t, _)| t == "plate.png"), "the plate is named");
    let before = run.clip().picture_v2().unwrap().picture.clone();
    assert!(run.click_last("Clear"), "Clear");
    assert_eq!(params(&run, id).difference_key.unwrap().plate_path, None);
    one_step(&mut run, &before).unwrap();
}

/// A merge's gradient beside the chain shows its card under the merge
/// (#1746): its angle typed there is the gradient's, one step, drawn.
#[test]
fn a_merges_gradient_is_edited_under_the_merge() {
    let t = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut c = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(klipp_effects::BuiltinEffect::Merge { blend_mode: klipp_effects::BlendMode::Normal, opacity: 1.0 }).build();
    let graph = c.picture_v2().unwrap().picture.graph.clone();
    let merge = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("merge")).unwrap().id;
    let id = NodeId::new_v4();
    let gradient = crate::picture_mattes::source("source.gradient").unwrap();
    let ops = crate::picture_mattes::source_ops(&graph, &NodePath::new(Root::Clip { clip: c.id.0 }, []), gradient, id, (0.0, 0.0), Some(merge)).unwrap();
    c.apply_picture_ops(&ops).unwrap();
    let mut run = Run::new(c.clone());
    let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
    assert!(drawn.iter().any(|(t, _)| t == "fg ← Gradient"), "the branch: {:?}", drawn.iter().map(|d| &d.0).collect::<Vec<_>>());
    let before = run.clip().picture_v2().unwrap().picture.clone();
    let at = number_right_of(&mut run, "Angle").expect("the gradient's Angle row");
    type_into(&mut run, at, "75");
    let picture = run.clip().picture_v2().unwrap();
    let node = picture.picture.graph.nodes.iter().find(|n| n.id == id).unwrap();
    assert!(matches!(klipp_graph_lift::effect_params::effect_of(node).unwrap(), klipp_effects::BuiltinEffect::Generator { angle, .. } if angle == 75.0));
    one_step(&mut run, &before).unwrap();
    super::parity_pixels::clip_edit_drawn_on(super::parity_pixels::Routes::Both, super::parity_pixels::SMALL, &c, run.clip()).unwrap();
}
