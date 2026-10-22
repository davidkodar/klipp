//! The placed kinds take graph ops (M4 step 12b-0): an edit that adds,
//! removes, moves or switches a Transform, an Opacity, a Speed Ramp or a
//! Temporal Noise Reduction goes through the clip's picture, and leaves
//! the roles the lift gives them exactly as the lift gives them to the
//! edited list. The clip's opacity is its last enabled Opacity, other
//! enabled ones multiply; its placement is its first enabled Transform,
//! the rest kept off; a blending Speed Ramp and the denoiser run at the
//! chain's head.
//!
//! Each case: a clip with list `before`, edited through
//! `apply_effect_edits`, against a clip given the edited list whole
//! (`after`) and lifted. The two pictures must hold the same roles: the
//! placement and its keys, the opacity and its keys, and the chain's
//! nodes in order with their types, kinds and flags. Node ids aren't
//! compared: an edit keeps its nodes', a whole list makes new ones.

use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_timeline::test_clip::{self, TestClip};
use klipp_timeline::{Clip, EffectEdit};

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn clip(effects: Vec<E>, enabled: Vec<bool>) -> Clip {
    let mut b = TestClip::of(Clip::media(MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0)));
    for (i, e) in effects.into_iter().enumerate() {
        b = b.effect(e);
        if !enabled.get(i).copied().unwrap_or(true) {
            b = b.off();
        }
    }
    b.build()
}

/// `c` with `plugins` as its OFX plug-ins.
fn with_plugins(c: Clip, plugins: Vec<klipp_plugin::AppliedEffect>) -> Clip {
    plugins.into_iter().fold(TestClip::of(c), |b, p| b.plugin(p)).build()
}

fn transform(x: f32) -> E {
    E::Transform { position_x: x, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

fn opacity(v: f32) -> E {
    E::Opacity { value: v }
}

fn blur() -> E {
    E::GaussianBlur { radius: 2.0 }
}

/// What the lift makes of a clip's placed kinds, without node ids.
fn roles(c: &Clip) -> serde_json::Value {
    let p = c.picture_v2().expect("the clip's picture");
    let pic = &p.picture;
    let chain: Vec<serde_json::Value> = pic
        .graph
        .nodes
        .iter()
        .filter(|n| !matches!(n.type_id.as_str(), lt::GROUP))
        .map(|n| {
            // A node's kind: an effect's tag, a placement's mode.
            let kind = n.legacy.as_ref().and_then(|l| l.payload.get("type")).cloned().unwrap_or(serde_json::Value::Null);
            let mode = n.params.get(&klipp_graph_format::ParamKey::from("mode")).map(|m| format!("{m:?}"));
            serde_json::json!({ "type": n.type_id.as_str(), "kind": kind, "mode": mode, "enabled": n.enabled })
        })
        .collect();
    serde_json::json!({
        "placement": serde_json::to_value(&pic.placement).unwrap(),
        "placement_anim": serde_json::to_value(&pic.placement_anim).unwrap(),
        "opacity": pic.opacity,
        "opacity_anim": serde_json::to_value(&pic.opacity_anim).unwrap(),
        "chain": chain,
    })
}

/// `before` edited by `edits` holds the roles `after` lifts to.
fn holds(name: &str, before: (Vec<E>, Vec<bool>), edits: &[EffectEdit], after: (Vec<E>, Vec<bool>)) {
    let mut edited = clip(before.0, before.1);
    edited
        .apply_effect_edits(edits)
        .unwrap_or_else(|| panic!("{name}: the placed kinds take graph ops"))
        .unwrap_or_else(|e| panic!("{name}: the picture refused the edit: {e}"));
    let whole = clip(after.0.clone(), after.1.clone());
    assert_eq!(roles(&edited), roles(&whole), "{name}");
    assert_eq!((test_clip::effects(&edited), test_clip::enabled(&edited)), after, "{name}: the list it reads back as");
}

/// A second Opacity added: it becomes the anchor, and the first, still
/// enabled, multiplies.
#[test]
fn a_second_opacity_becomes_the_anchor() {
    let added = opacity(0.5);
    holds(
        "a second opacity",
        (vec![blur(), opacity(0.8)], vec![true, true]),
        &[EffectEdit::Add(&added)],
        (vec![blur(), opacity(0.8), opacity(0.5)], vec![true, true, true]),
    );
}

/// The anchor switched off: the Opacity before it is the anchor now.
#[test]
fn the_anchor_switched_off_hands_it_on() {
    holds(
        "the anchor off",
        (vec![opacity(0.8), blur(), opacity(0.5)], vec![true, true, true]),
        &[EffectEdit::SetEnabled(2, false)],
        (vec![opacity(0.8), blur(), opacity(0.5)], vec![true, true, false]),
    );
}

/// A Transform added to a clip with none: the placement is fixed at it.
#[test]
fn a_first_transform_fixes_the_placement() {
    let t = transform(40.0);
    holds("a first transform", (vec![blur()], vec![true]), &[EffectEdit::Add(&t)], (vec![blur(), transform(40.0)], vec![true, true]));
}

/// The fixed Transform removed: the next enabled one is the placement.
#[test]
fn removing_the_fixed_transform_fixes_the_next() {
    holds(
        "the fixed transform removed",
        (vec![transform(10.0), blur(), transform(30.0)], vec![true, true, true]),
        &[EffectEdit::Remove(0)],
        (vec![blur(), transform(30.0)], vec![true, true]),
    );
}

/// The fixed Transform switched off: the next enabled one is the
/// placement, and the first is kept off.
#[test]
fn the_fixed_transform_switched_off_hands_it_on() {
    holds(
        "the fixed transform off",
        (vec![transform(10.0), transform(30.0)], vec![true, true]),
        &[EffectEdit::SetEnabled(0, false)],
        (vec![transform(10.0), transform(30.0)], vec![false, true]),
    );
}

/// A blending Speed Ramp moved in the list: it still runs at the head.
#[test]
fn a_blending_ramp_moved_stays_at_the_head() {
    holds(
        "a ramp moved",
        (vec![blur(), E::SpeedRamp { speed: 0.5, blend_mode: 1 }, opacity(0.9)], vec![true, true, true]),
        &[EffectEdit::Move { from: 1, to: 0 }],
        (vec![E::SpeedRamp { speed: 0.5, blend_mode: 1 }, blur(), opacity(0.9)], vec![true, true, true]),
    );
}

/// Temporal Noise Reduction added: it runs at the head, before the grade.
#[test]
fn a_denoiser_added_runs_at_the_head() {
    let d = E::TemporalDenoise { strength: 0.6, threshold: 0.1 };
    holds(
        "a denoiser added",
        (vec![blur(), opacity(0.7)], vec![true, true]),
        &[EffectEdit::Add(&d)],
        (vec![blur(), opacity(0.7), E::TemporalDenoise { strength: 0.6, threshold: 0.1 }], vec![true, true, true]),
    );
}

/// One edit of several, mixing a placed kind and a plain effect, is one
/// picture edit with the roles of the list it ends at.
#[test]
fn placed_and_plain_edits_together() {
    let t = transform(5.0);
    let o = opacity(0.4);
    holds(
        "placed and plain together",
        (vec![blur(), opacity(0.8)], vec![true, true]),
        &[EffectEdit::Add(&t), EffectEdit::Remove(0), EffectEdit::Add(&o)],
        (vec![opacity(0.8), transform(5.0), opacity(0.4)], vec![true, true, true]),
    );
}

/// A Speed Ramp that starts blending moves to the head; one that stops
/// is kept off in the list.
#[test]
fn a_ramp_starts_and_stops_blending() {
    let blending = E::SpeedRamp { speed: 0.5, blend_mode: 1 };
    holds(
        "a ramp starts blending",
        (vec![blur(), E::SpeedRamp { speed: 0.5, blend_mode: 0 }], vec![true, true]),
        &[EffectEdit::Update(1, &blending)],
        (vec![blur(), E::SpeedRamp { speed: 0.5, blend_mode: 1 }], vec![true, true]),
    );
    let still = E::SpeedRamp { speed: 0.5, blend_mode: 0 };
    holds(
        "a ramp stops blending",
        (vec![blur(), E::SpeedRamp { speed: 0.5, blend_mode: 1 }], vec![true, true]),
        &[EffectEdit::Update(1, &still)],
        (vec![blur(), E::SpeedRamp { speed: 0.5, blend_mode: 0 }], vec![true, true]),
    );
}

/// The denoiser moved in the list: still at the head, its place in the
/// list kept for the list's reader.
#[test]
fn a_denoiser_moved_stays_at_the_head() {
    holds(
        "a denoiser moved",
        (vec![blur(), E::TemporalDenoise { strength: 0.6, threshold: 0.1 }, opacity(0.7)], vec![true, true, true]),
        &[EffectEdit::Move { from: 1, to: 2 }],
        (vec![blur(), opacity(0.7), E::TemporalDenoise { strength: 0.6, threshold: 0.1 }], vec![true, true, true]),
    );
}

/// Roles change, ids don't: an Opacity that becomes the anchor, and a
/// Transform that becomes the placement, keep the node ids the clip names
/// them by.
#[test]
fn a_role_change_keeps_every_id() {
    let mut c = clip(vec![opacity(0.8), transform(10.0), transform(30.0), opacity(0.5)], vec![true, true, true, true]);
    let ids: Vec<_> = (0..4).map(|i| c.effect_node_at(false, i)).collect();
    c.apply_effect_edits(&[EffectEdit::SetEnabled(3, false), EffectEdit::SetEnabled(1, false)])
        .expect("the placed kinds take graph ops")
        .expect("the picture takes the edit");
    let now: Vec<_> = (0..4).map(|i| c.effect_node_at(false, i)).collect();
    assert_eq!(now, ids, "the anchor and the placement changed hands; no entry was renamed");
}

/// Every node of the picture, its id by its type: what a held path names.
fn node_ids(c: &Clip) -> Vec<(String, uuid::Uuid)> {
    let p = c.picture_v2().expect("the clip's picture");
    let mut ids: Vec<(String, uuid::Uuid)> = p.picture.graph.nodes.iter().map(|n| (n.type_id.as_str().to_owned(), n.id)).collect();
    ids.sort();
    ids
}

/// A move that changes no role renames no node, the role nodes included
/// (the placement, the head's denoiser): a path the inspector, the
/// history or the assistant holds stays good.
#[test]
fn a_move_that_changes_no_role_renames_nothing() {
    let mut c = clip(vec![blur(), E::Gamma { gamma: 1.1 }, transform(10.0), E::TemporalDenoise { strength: 0.5, threshold: 0.1 }, opacity(0.8)], vec![true; 5]);
    let before = node_ids(&c);
    c.apply_effect_edits(&[EffectEdit::Move { from: 3, to: 0 }]).expect("the placed kinds take graph ops").expect("the picture takes the edit");
    assert_eq!(node_ids(&c), before);
}

/// When the placement changes hands it is the node of the Transform that
/// holds it: the first Transform switched off, the placement node's id is
/// the second Transform's.
#[test]
fn the_placement_node_is_its_transforms() {
    let mut c = clip(vec![transform(10.0), transform(30.0)], vec![true, true]);
    let second = c.effect_node_at(false, 1).expect("the second transform's node");
    c.apply_effect_edits(&[EffectEdit::SetEnabled(0, false)]).expect("the placed kinds take graph ops").expect("the picture takes the edit");
    let p = c.picture_v2().unwrap();
    let placement = p.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::PLACEMENT).expect("a placement node");
    assert_eq!(placement.id, second, "the placement is the second transform's node");
}

/// A picture-truth clip (an effect mixed, which its list can't hold)
/// refuses an edit of a placed kind, by name, for now, and keeps its mix:
/// its record is its picture's view, and a lift of it would drop the mix.
#[test]
fn a_picture_truth_clip_refuses_a_placed_edit_and_keeps_its_mix() {
    let mut c = clip(vec![blur()], vec![true]);
    let node = c.effect_node_at(false, 0).expect("the blur's node");
    let path = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: c.id.0 }, [node]);
    c.apply_picture_ops(&[klipp_graph_lift::ops::GraphOp::SetMix { path, mix: Some(0.5) }]).expect("a mix on the blur");
    assert!(c.is_picture_truth(), "the mix makes the clip picture-truth");
    let t = transform(20.0);
    let refused = c.apply_effect_edits(&[EffectEdit::Add(&t)]).expect("routed to the placed edit");
    assert!(refused.as_ref().is_err_and(|e| e.contains("mix or mask")), "{refused:?}");
    let p = c.picture_v2().unwrap();
    assert!(p.picture.graph.nodes.iter().any(|n| n.id == node && n.mix == Some(0.5)), "the mix is kept");
    assert!(test_clip::effects(&c).len() == 1, "nothing added");
}

fn key(t: f64, v: f32) -> klipp_core::Keyframe {
    klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }
}

/// A key naming a slot past the list is an orphan the clip keeps. Growing
/// the list onto that slot doesn't hand the orphan to the new entry: the
/// new entry has no keys, and the orphan is still kept, past the list.
#[test]
fn an_orphan_key_stays_an_orphan_when_the_list_grows() {
    let mut c = clip(vec![blur(), E::Gamma { gamma: 1.1 }, opacity(0.8)], vec![true; 3]);
    test_clip::orphan_keys(&mut c, 3, "radius", vec![key(0.0, 1.0), key(1.0, 9.0)]);
    let t = transform(20.0);
    c.apply_effect_edits(&[EffectEdit::Add(&t)]).expect("the placed kinds take graph ops").expect("the picture takes the edit");
    assert_eq!(test_clip::effects(&c).len(), 4);
    assert!(!test_clip::effect_keys(&c).keys().any(|k| k.starts_with("3:")), "the new entry at 3 has no keys: {:?}", test_clip::effect_keys(&c).keys().collect::<Vec<_>>());
    assert_eq!(test_clip::effect_keys(&c).get("4:radius").map(Vec::len), Some(2), "the orphan is kept, past the list");
    let p = c.picture_v2().unwrap();
    let added = c.effect_node_at(false, 3).expect("the added entry's node");
    assert!(p.picture.graph.nodes.iter().filter(|n| n.id == added).all(|n| n.anim.is_empty()), "nothing animates the added entry");
}

/// When the opacity anchor changes hands the clip's opacity row is still
/// the placement node's `opacity`, at the placement's stable path; each
/// Opacity entry keeps its own node id, the one that held the anchor too.
#[test]
fn the_opacity_row_survives_the_anchor_changing_hands() {
    let mut c = clip(vec![opacity(0.8), blur(), opacity(0.5)], vec![true, true, true]);
    let row = c.placement_path().expect("the placement's path");
    let ids = (c.effect_node_at(false, 0), c.effect_node_at(false, 2));
    c.apply_effect_edits(&[EffectEdit::SetEnabled(2, false)]).expect("the placed kinds take graph ops").expect("the picture takes the edit");
    assert_eq!(c.placement_path(), Some(row), "the opacity row's path");
    assert_eq!((c.effect_node_at(false, 0), c.effect_node_at(false, 2)), ids, "each Opacity keeps its node");
    assert!((c.picture_v2().unwrap().picture.opacity - 0.8).abs() < 1e-6, "the first Opacity is the anchor now");
}

/// A batch on a clip with an OFX plug-in: the plug-in keeps its node, its
/// place and its entry while the list around it is edited.
#[test]
fn an_ofx_plug_in_keeps_its_place_through_a_placed_batch() {
    let mut c = clip(vec![blur(), opacity(0.8)], vec![true, true]);
    c = with_plugins(c, vec![klipp_plugin::AppliedEffect { plugin_name: "org.example.Grain".into(), enabled: true, ..Default::default() }]);
    let ofx = |c: &Clip| {
        let p = c.picture_v2().unwrap();
        p.picture.graph.nodes.iter().filter(|n| matches!(n.type_id.as_str(), klipp_graph_format::types::LEGACY_OFX | lt::OFX)).map(|n| (n.id, n.legacy.as_ref().and_then(|l| l.slot))).collect::<Vec<_>>()
    };
    let before = ofx(&c);
    assert_eq!(before.len(), 1);
    let t = transform(15.0);
    c.apply_effect_edits(&[EffectEdit::Add(&t), EffectEdit::Move { from: 2, to: 0 }, EffectEdit::SetEnabled(1, false)])
        .expect("the placed kinds take graph ops")
        .expect("the picture takes the edit");
    assert_eq!(ofx(&c), before, "the plug-in's node and slot");
    assert_eq!(test_clip::plugins(&c).len(), 1);
    assert_eq!(test_clip::plugins(&c)[0].plugin_name, "org.example.Grain");
    holds(
        "with a plug-in",
        (vec![blur(), opacity(0.8)], vec![true, true]),
        &[EffectEdit::Add(&t), EffectEdit::Move { from: 2, to: 0 }, EffectEdit::SetEnabled(1, false)],
        (vec![transform(15.0), blur(), opacity(0.8)], vec![true, false, true]),
    );
}

/// A plain effect added onto an orphan's slot gets none of its keys, and a
/// plain effect removed under it leaves it an orphan: every list edit
/// renumbers in one place.
#[test]
fn plain_edits_keep_orphans_orphans() {
    let with_orphan = || {
        let mut c = clip(vec![blur(), E::Gamma { gamma: 1.1 }, E::Vibrance { amount: 0.2 }], vec![true; 3]);
        test_clip::orphan_keys(&mut c, 3, "radius", vec![key(0.0, 1.0), key(1.0, 9.0)]);
        c
    };
    let mut added = with_orphan();
    let more = E::GaussianBlur { radius: 4.0 };
    added.apply_effect_edits(&[EffectEdit::Add(&more)]).expect("a list edit").expect("the picture takes it");
    assert!(!test_clip::effect_keys(&added).keys().any(|k| k.starts_with("3:")), "the blur added at 3 has no keys");
    assert_eq!(test_clip::effect_keys(&added).get("4:radius").map(Vec::len), Some(2), "the orphan, past the list");
    let mut removed = with_orphan();
    removed.apply_effect_edits(&[EffectEdit::Remove(0)]).expect("a list edit").expect("the picture takes it");
    assert_eq!(test_clip::effects(&removed).len(), 2);
    assert_eq!(test_clip::effect_keys(&removed).get("2:radius").map(Vec::len), Some(2), "the orphan, past the shorter list");
    let p = removed.picture_v2().unwrap();
    assert!(p.picture.graph.nodes.iter().all(|n| n.anim.keys().all(|k| k.as_str() != "radius")), "nothing animates by the orphan");
}

/// A plain effect added by graph ops sent straight to the picture (a path
/// command's or the assistant's AddNode), onto an orphan's slot: the
/// orphan must stay one. The route writes the fields directly today.
#[test]
fn a_direct_graph_op_add_keeps_an_orphan_an_orphan() {
    let mut c = clip(vec![blur(), E::Gamma { gamma: 1.1 }, E::Vibrance { amount: 0.2 }], vec![true; 3]);
    test_clip::orphan_keys(&mut c, 3, "radius", vec![key(0.0, 1.0), key(1.0, 9.0)]);
    let more = E::GaussianBlur { radius: 4.0 };
    let ops = c.effect_edit_ops(EffectEdit::Add(&more)).expect("a plain add's ops");
    c.apply_picture_ops(&ops).expect("the picture takes them");
    assert!(!test_clip::effect_keys(&c).keys().any(|k| k.starts_with("3:")), "the blur added at 3 has no keys");
    assert_eq!(test_clip::effect_keys(&c).get("4:radius").map(Vec::len), Some(2), "the orphan, past the list");
}

/// The same on a picture-truth clip, whose plain edits stay on the graph
/// ops: the orphan must stay one there too, with the mix kept.
#[test]
fn a_picture_truth_clips_plain_add_keeps_an_orphan_an_orphan() {
    let mut c = clip(vec![blur(), E::Gamma { gamma: 1.1 }, E::Vibrance { amount: 0.2 }], vec![true; 3]);
    test_clip::orphan_keys(&mut c, 3, "radius", vec![key(0.0, 1.0), key(1.0, 9.0)]);
    let node = c.effect_node_at(false, 0).expect("the blur's node");
    let path = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: c.id.0 }, [node]);
    c.apply_picture_ops(&[klipp_graph_lift::ops::GraphOp::SetMix { path, mix: Some(0.5) }]).expect("a mix on the blur");
    assert!(c.is_picture_truth());
    let more = E::GaussianBlur { radius: 4.0 };
    c.apply_effect_edits(&[EffectEdit::Add(&more)]).expect("a list edit").expect("the picture takes it");
    assert!(!test_clip::effect_keys(&c).keys().any(|k| k.starts_with("3:")), "the blur added at 3 has no keys");
    assert_eq!(test_clip::effect_keys(&c).get("4:radius").map(Vec::len), Some(2), "the orphan, past the list");
}

fn plugin(name: &str) -> klipp_plugin::AppliedEffect {
    klipp_plugin::AppliedEffect { plugin_name: name.into(), enabled: true, ..Default::default() }
}

fn plugins(c: &Clip) -> Vec<(String, bool)> {
    test_clip::plugins(c).iter().map(|e| (e.plugin_name.clone(), e.enabled)).collect()
}

/// The OFX list's edits go through the picture and come back as the list
/// given whole would be: added at the end, switched, moved, removed, a
/// value set; the built-in list beside it is untouched.
#[test]
fn ofx_edits_come_back_as_the_list() {
    use klipp_timeline::OfxEdit;
    let mut c = clip(vec![blur()], vec![true]);
    c = with_plugins(c, vec![plugin("org.example.A"), plugin("org.example.B")]);
    let added = plugin("org.example.C");
    c.apply_ofx_edits(&[OfxEdit::Add(&added), OfxEdit::SetEnabled(0, false), OfxEdit::Move { from: 2, to: 0 }]).expect("the edits");
    assert_eq!(plugins(&c), vec![("org.example.C".into(), true), ("org.example.A".into(), false), ("org.example.B".into(), true)]);
    let v = klipp_core::OfxValue::Double(0.25);
    c.apply_ofx_edits(&[OfxEdit::SetParam(2, "strength", &v), OfxEdit::Remove(0)]).expect("a value and a removal");
    assert_eq!(plugins(&c), vec![("org.example.A".into(), false), ("org.example.B".into(), true)]);
    assert_eq!(test_clip::plugins(&c)[1].param_values.get("strength"), Some(&v), "the value is set on B");
    assert!(test_clip::plugins(&c)[0].param_values.is_empty(), "A has none");
    assert_eq!(test_clip::effects(&c), vec![blur()], "the built-in list is untouched");
    // The picture holds what the list given whole would.
    let mut whole = clip(vec![blur()], vec![true]);
    whole = with_plugins(whole, test_clip::plugins(&c).to_vec());
    assert_eq!(roles(&c), roles(&whole));
}

/// A switched-off plug-in stays in the chain as a node that runs and is
/// told not to: its entry's flag, the node itself kept on.
#[test]
fn an_ofx_switch_is_its_entrys_flag() {
    use klipp_timeline::OfxEdit;
    let mut c = clip(vec![], vec![]);
    c = with_plugins(c, vec![plugin("org.example.A")]);
    let whole = {
        let mut w = clip(vec![], vec![]);
        w = with_plugins(w, vec![klipp_plugin::AppliedEffect { enabled: false, ..plugin("org.example.A") }]);
        roles(&w)
    };
    c.apply_ofx_edits(&[OfxEdit::SetEnabled(0, false)]).expect("the switch");
    assert_eq!(roles(&c), whole);
}

/// A picture-truth clip refuses an OFX edit, by name, and keeps its list.
#[test]
fn a_picture_truth_clip_refuses_an_ofx_edit() {
    use klipp_timeline::OfxEdit;
    let mut c = clip(vec![blur()], vec![true]);
    c = with_plugins(c, vec![plugin("org.example.A")]);
    let node = c.effect_node_at(false, 0).expect("the blur's node");
    let path = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: c.id.0 }, [node]);
    c.apply_picture_ops(&[klipp_graph_lift::ops::GraphOp::SetMix { path, mix: Some(0.5) }]).expect("a mix on the blur");
    let refused = c.apply_ofx_edits(&[OfxEdit::Remove(0)]);
    assert!(refused.as_ref().is_err_and(|e| e.contains("mix or mask")), "{refused:?}");
    assert_eq!(plugins(&c), vec![("org.example.A".into(), true)]);
}

/// Today's known gap (#1500): a plug-in has no id of its own, so its node
/// is named by its place, and a moved plug-in's node is named anew. The
/// place finds it. When plug-ins keep ids, this flips on purpose.
#[test]
fn a_moved_plugins_node_is_named_by_its_new_place() {
    use klipp_timeline::OfxEdit;
    let mut c = clip(vec![], vec![]);
    c = with_plugins(c, vec![plugin("org.example.A"), plugin("org.example.B")]);
    let a_before = c.effect_node_at(true, 0).expect("A's node");
    c.apply_ofx_edits(&[OfxEdit::Move { from: 0, to: 1 }]).expect("the move");
    assert_eq!(plugins(&c), vec![("org.example.B".into(), true), ("org.example.A".into(), true)]);
    let a_after = c.effect_node_at(true, 1).expect("A's node at its new place");
    assert_ne!(a_after, a_before, "the node is named by its place (#1500)");
    assert_eq!(c.effect_node_at(true, 0), Some(a_before), "B now has the first place's name");
    assert_eq!(c.effect_slot_of_node(a_after), Some((true, 1)), "the place finds it");
}

/// The last entry taken off leaves an empty list, for the built-in effects
/// and the plug-ins alike: the record writes an empty list as nothing,
/// which reads back as none.
#[test]
fn the_last_entry_taken_off_leaves_an_empty_list() {
    use klipp_timeline::OfxEdit;
    let mut c = with_plugins(clip(vec![blur()], vec![true]), vec![plugin("org.example.A")]);
    c.apply_effect_edits(&[EffectEdit::Remove(0)]).expect("a list edit").expect("the picture takes it");
    assert!(test_clip::effects(&c).is_empty(), "no built-in effects");
    c.apply_ofx_edits(&[OfxEdit::Remove(0)]).expect("the plug-in comes off");
    assert!(test_clip::plugins(&c).is_empty(), "no plug-ins");
}
