//! The Placement row's mode and moves in the generated Effects tab (M3
//! DEC-§2), driven by clicks on the drawn view: a move fixes the placement
//! where it goes in one undo step, choosing Auto takes it back to where
//! automatic placement puts it, and a fixed placement's move gives the
//! picture the old tab's reorder of its Transform gives. Placement and
//! opacity written by path are what the keyframe hotkeys and the rubber
//! bands read.

use super::parity_harness::Run;
use crate::inspector::stack_view;
use klipp_core::{AppCommand, VideoParam};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{Node, ParamValue};

fn clip(effects: &[E]) -> klipp_timeline::Clip {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    for e in effects {
        clip.push_effect(e.clone());
    }
    clip
}

fn transform(x: f32) -> E {
    E::Transform { position_x: x, position_y: -4.0, scale_x: 0.6, scale_y: 0.6, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

const IDENTITY: E = E::Transform { position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };

fn glow() -> E {
    E::OuterGlow { radius: 0.05, intensity: 0.5, color: [255, 255, 255] }
}

/// The clip's PLACEMENT node.
fn placement(run: &Run) -> Node {
    klipp_graph_lift::clip_properties::placement_node(&run.clip().picture_v2().unwrap().picture).cloned().expect("a placement")
}

fn mode(run: &Run) -> Option<ParamValue> {
    placement(run).params.get(&"mode".into()).cloned()
}

/// The Placement row's label as the stack draws it.
fn label(run: &Run) -> String {
    let picture = run.clip().picture_v2().unwrap();
    let node = placement(run);
    stack_view::display_name(&picture.picture.graph, &NodePath::new(Root::Clip { clip: run.clip.0 }, [node.id]), &node.label, false)
}

fn generated(effects: &[E]) -> Run {
    Run::new(clip(effects), true)
}

fn undos(run: &Run) -> usize {
    run.state.active_history().undo_count()
}

/// Click `text` under the Placement row.
fn click(run: &mut Run, text: &str) {
    let row = label(run);
    assert!(run.click_text(&row, text), "{text} under {row}");
}

/// Pick `choice` from the Placement row's mode menu.
fn choose(run: &mut Run, choice: &str) {
    let shown = if mode(run) == Some(ParamValue::Enum(lt::PLACEMENT_FIXED)) { "Fixed" } else { "Auto" };
    click(run, shown);
    assert!(run.click_last(choice), "{choice} in the mode menu");
}

/// A move of an automatic placement fixes it where it goes, as an
/// identity Transform in the list, and a move of a fixed one moves its
/// Transform: each one undo step, which puts the picture back whole.
#[test]
fn dragging_placement_sets_fixed_in_one_step() {
    let mut run = generated(&[E::GaussianBlur { radius: 3.0 }, glow(), E::Sharpen { amount: 0.5 }]);
    let start = run.clip().picture_v2().unwrap().picture.clone();
    assert_eq!(mode(&run), Some(ParamValue::Enum(lt::PLACEMENT_AUTO)));
    let steps = undos(&run);
    // From the end, up past Sharpen: after the glow.
    click(&mut run, "↑");
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    assert_eq!(undos(&run), steps + 1, "one step");
    assert_eq!(run.clip().builtin_effects(), &[E::GaussianBlur { radius: 3.0 }, glow(), IDENTITY, E::Sharpen { amount: 0.5 }]);
    assert_eq!(mode(&run), Some(ParamValue::Enum(lt::PLACEMENT_FIXED)));
    // Fixed now: up again moves its Transform, past the glow.
    click(&mut run, "↑");
    assert_eq!(undos(&run), steps + 2, "one step");
    assert_eq!(run.clip().builtin_effects(), &[E::GaussianBlur { radius: 3.0 }, IDENTITY, glow(), E::Sharpen { amount: 0.5 }]);
    assert!(run.step(false) && run.step(false));
    assert_eq!(run.clip().picture_v2().unwrap().picture, start, "undone whole");
}

/// Fixed puts the placement where automatic placement had it, before the
/// first effect drawn outside the picture, so the picture is the same;
/// Auto takes that unmoved Transform away again. A placement that moves
/// the clip can't be made automatic yet: Auto is greyed with the reason,
/// and a click on it changes nothing.
#[test]
fn choosing_auto_restores_it() {
    let mut run = generated(&[E::GaussianBlur { radius: 3.0 }, glow()]);
    let start = run.clip().picture_v2().unwrap().picture.clone();
    let steps = undos(&run);
    choose(&mut run, "Fixed");
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    assert_eq!(run.clip().builtin_effects(), &[E::GaussianBlur { radius: 3.0 }, IDENTITY, glow()], "fixed before the glow");
    assert_eq!(mode(&run), Some(ParamValue::Enum(lt::PLACEMENT_FIXED)));
    choose(&mut run, "Auto");
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    assert_eq!(undos(&run), steps + 2, "a step each");
    assert_eq!(run.clip().picture_v2().unwrap().picture, start, "back to automatic");
    assert!(run.step(false));
    assert_eq!(mode(&run), Some(ParamValue::Enum(lt::PLACEMENT_FIXED)), "undo fixes it again");

    let mut moved = generated(&[E::GaussianBlur { radius: 3.0 }, transform(40.0)]);
    assert!(moved.clip().placement_auto_refusal().is_some());
    let before = moved.clip().picture_v2().unwrap().picture.clone();
    let steps = undos(&moved);
    choose(&mut moved, "Auto");
    assert_eq!(undos(&moved), steps, "nothing recorded");
    assert_eq!(moved.clip().picture_v2().unwrap().picture, before, "nothing changed");
    // Sent by path anyway, it is refused with the same reason.
    let addr = NodePath::new(Root::Clip { clip: moved.clip.0 }, [placement(&moved).id]);
    moved.apply(AppCommand::SetParamAt { addr, key: "mode".into(), value: ParamValue::Enum(lt::PLACEMENT_AUTO) });
    assert!(moved.unapplied.iter().any(|u| u.contains("reset its placement")), "{:?}", moved.unapplied);
    assert_eq!(moved.clip().picture_v2().unwrap().picture, before);
}

/// Moving a fixed placement down past an effect gives the picture the old
/// tab's reorder of its Transform gives, one step each, down to every id.
#[test]
fn a_placement_move_matches_the_old_reorder() {
    let effects = [E::GaussianBlur { radius: 3.0 }, transform(40.0), E::Sharpen { amount: 0.5 }];
    let c = clip(&effects);
    let (mut old, mut new) = (Run::new(c.clone(), false), Run::new(c, true));
    let steps = undos(&old);
    old.apply(AppCommand::ReorderBuiltinEffect { clip_id: old.clip, from_index: 1, to_index: 2 });
    click(&mut new, "↓");
    assert!(old.unapplied.is_empty() && new.unapplied.is_empty(), "{:?} {:?}", old.unapplied, new.unapplied);
    assert_eq!((undos(&old), undos(&new)), (steps + 1, steps + 1), "one step each");
    assert_eq!(new.clip().builtin_effects(), &[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }, transform(40.0)]);
    assert_eq!(*new.clip().picture_v2().unwrap(), *old.clip().picture_v2().unwrap(), "the same picture");
}

/// The keyframe hotkeys and the rubber bands start a key from
/// `Clip::video_param_value`: with the generated view on, that is the
/// placement and opacity the path edits wrote, and a move of the placement
/// keeps them.
#[test]
fn rubber_bands_and_hotkeys_read_the_properties() {
    let mut run = generated(&[E::GaussianBlur { radius: 3.0 }, E::Opacity { value: 0.2 }, E::Sharpen { amount: 0.5 }]);
    let addr = NodePath::new(Root::Clip { clip: run.clip.0 }, [klipp_graph_lift::clip_properties::placement_id(run.clip.0)]);
    for (key, v) in [("pos_x", 40.0), ("scale", 0.6), ("opacity", 0.3)] {
        run.apply(AppCommand::SetParamAt { addr: addr.clone(), key: key.into(), value: ParamValue::Float(v) });
    }
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    let read = |run: &Run| [VideoParam::PositionX, VideoParam::Scale, VideoParam::Opacity].map(|p| run.clip().video_param_value(p));
    assert_eq!(read(&run), [40.0, 0.6, 0.3]);
    click(&mut run, "↑");
    assert!(run.unapplied.is_empty(), "{:?}", run.unapplied);
    assert_eq!(read(&run), [40.0, 0.6, 0.3], "moved, the same values");
}

/// On a clip whose picture is stored whole (an effect's mix makes it so),
/// the placement's place and mode wait for the next format revision: the
/// row says so in place of its controls, and a move or a mode sent by path is refused
/// with that reason, writing nothing.
#[test]
fn a_picture_truth_placement_is_refused_and_unwritten() {
    let mut run = generated(&[E::GaussianBlur { radius: 3.0 }, transform(40.0), E::Sharpen { amount: 0.5 }]);
    let blur = NodePath::new(Root::Clip { clip: run.clip.0 }, [run.clip().effect_node_at(false, 0).unwrap()]);
    run.apply(AppCommand::SetMixAt { addr: blur, mix: Some(0.5) });
    assert!(run.clip().is_picture_truth(), "a mix makes the clip picture-truth");
    let before = run.clip().picture_v2().unwrap().picture.clone();
    let steps = undos(&run);
    let drawn: Vec<String> = run.frame(Vec::new(), egui::Modifiers::NONE).into_iter().map(|(text, _)| text).collect();
    assert!(drawn.iter().any(|t| t == "This clip's placement moves with the next project format"), "the row says why: {drawn:?}");
    assert!(!drawn.iter().any(|t| t == "Auto" || t == "Fixed"), "no mode drawn: {drawn:?}");
    let placement = NodePath::new(Root::Clip { clip: run.clip.0 }, [placement(&run).id]);
    let sharpen = placement_after(&run);
    run.apply(AppCommand::MoveNodeAt { addr: placement.clone(), after: sharpen });
    run.apply(AppCommand::SetParamAt { addr: placement, key: "mode".into(), value: ParamValue::Enum(lt::PLACEMENT_AUTO) });
    assert_eq!(run.unapplied.len(), 2, "{:?}", run.unapplied);
    assert!(run.unapplied.iter().all(|u| u.contains("next format revision")), "{:?}", run.unapplied);
    assert_eq!(undos(&run), steps, "nothing recorded");
    assert_eq!(run.clip().picture_v2().unwrap().picture, before, "nothing written");
}

/// The node after the placement in the clip's chain.
fn placement_after(run: &Run) -> klipp_graph_format::NodeId {
    let picture = run.clip().picture_v2().unwrap();
    let id = placement(run).id;
    let edge = picture.picture.graph.edges.iter().find(|e| e.from.0 == id && e.from.1.as_str() == "out").expect("the placement feeds a node");
    edge.to.0
}
