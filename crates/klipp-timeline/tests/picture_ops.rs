//! A clip's picture edited by graph ops (node-graph M2 step 15a): written
//! through the clip's own fields, the picture lifted again is the one the
//! op engine makes of the lifted copy, so the two paths can't drift apart.

use klipp_core::{Interpolation, Keyframe, MediaId, ModifierStack, ParamId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::{types as lt, ClipPicture};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{types, Animation, Legacy, Node, NodeId, ParamValue};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::Clip;
use serde_json::{json, Value};
use std::collections::BTreeMap;

mod support;
use support::picture_ops_check::{check, effects, picture, Outcome};

/// The op through the clip's fields and through the engine on the lifted
/// copy give the same picture.
fn same_both_ways(c: &Clip, op: GraphOp) {
    match check(c, &op) {
        Outcome::Same => {}
        Outcome::Differ { written, engine } => {
            if let Ok(dir) = std::env::var("PICTURE_OPS_DUMP") {
                std::fs::write(format!("{dir}/written.json"), serde_json::to_string_pretty(&written).unwrap()).unwrap();
                std::fs::write(format!("{dir}/engine.json"), serde_json::to_string_pretty(&engine).unwrap()).unwrap();
            }
            panic!("{op:?}: the clip's fields and the engine disagree");
        }
        other => panic!("{op:?}: {other:?}"),
    }
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) }
}

fn wiggle() -> ModifierStack {
    ModifierStack { modifiers: vec![serde_json::from_str(r#"{"type":"wiggle","amplitude":0.5,"frequency":2.0,"blend":"Multiply"}"#).unwrap()] }
}

/// A media clip with three effects, keys on two, a stack on one.
fn clip() -> Clip {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    c.push_effect(E::GaussianBlur { radius: 3.0 });
    c.push_effect(E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 });
    c.push_effect(E::Bloom { threshold: 0.75, radius: 6.0, intensity: 0.5 });
    c.edit_effect_keyframes(|m| {
        m.insert("0:radius".into(), vec![key(0.0, 3.0), key(2.0, 9.0)]);
        m.insert("2:intensity".into(), vec![key(1.0, 0.5)]);
    });
    c.edit_param_modifiers(|m| {
        m.insert(ParamId::Effect { effect_index: 1, param_name: "amount".into() }, wiggle());
    });
    c
}

fn grade_group(p: &ClipPicture) -> NodeId {
    p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).expect("a grade group").id
}

fn at(c: &Clip, ids: impl IntoIterator<Item = NodeId>) -> NodePath {
    NodePath::new(Root::Clip { clip: c.id.0 }, ids)
}

/// A new effect node carrying `effect` as its entry.
fn new_effect(id: u128, effect: &E, anim: BTreeMap<klipp_graph_format::ParamKey, Animation>) -> Box<Node> {
    Box::new(Node {
        id: uuid::Uuid::from_u128(id),
        type_id: types::LEGACY_EFFECT.into(),
        type_version: 1,
        label: String::new(),
        enabled: true,
        params: Default::default(),
        anim,
        mix: None,
        wireless: Default::default(),
        group: None,
        legacy: Some(Legacy { slot: None, payload: serde_json::to_value(effect).unwrap(), keyframes: None, modifiers: Default::default() }),
        legacy_flags: None,
        origin: None,
    })
}

/// The clip's picture fields as it saves them.
fn saved(c: &Clip) -> Value {
    serde_json::to_value(klipp_timeline::picture_v1::ClipPictureV1::of(c)).unwrap()
}

fn key_json(t: f64, v: f32) -> Value {
    serde_json::to_value(key(t, v)).unwrap()
}

#[test]
fn every_effect_op_written_through_the_fields_is_the_engines() {
    let c = clip();
    let p = picture(&c);
    let fx = effects(&p);
    let g = grade_group(&p);
    // A clip's effects follow the display transform: the first place one
    // can go is after it.
    let first = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).expect("the display transform").id;
    let bloom = E::Bloom { threshold: 0.5, radius: 4.0, intensity: 0.25 };
    let with_keys = BTreeMap::from([("radius".into(), Animation { keys: vec![key_json(0.0, 4.0), key_json(1.0, 8.0)], modifiers: Some(serde_json::to_value(wiggle()).unwrap()) })]);
    let ops = vec![
        GraphOp::SetParam { path: at(&c, [fx[1]]), key: "size".into(), value: ParamValue::Float(0.75) },
        GraphOp::SetEnabled { path: at(&c, [fx[0]]), enabled: false },
        GraphOp::RemoveNode { path: at(&c, [fx[0]]) },
        GraphOp::RemoveNode { path: at(&c, [fx[1]]) },
        GraphOp::MoveAfter { path: at(&c, [fx[0]]), after: fx[2] },
        GraphOp::MoveAfter { path: at(&c, [fx[2]]), after: first },
        GraphOp::MoveAfter { path: at(&c, [fx[1]]), after: fx[2] },
        GraphOp::AddNode { after: at(&c, [fx[0]]), node: new_effect(0xa1, &bloom, BTreeMap::new()) },
        GraphOp::AddNode { after: at(&c, [first]), node: new_effect(0xa2, &bloom, with_keys.clone()) },
        GraphOp::AddNode { after: at(&c, [fx[2]]), node: new_effect(0xa3, &bloom, with_keys) },
        GraphOp::AddKeyAt { path: at(&c, [fx[0]]), key: "radius".into(), keyframe: key_json(1.0, 5.0) },
        GraphOp::AddKeyAt { path: at(&c, [fx[1]]), key: "amount".into(), keyframe: key_json(0.5, 0.25) },
        GraphOp::RemoveKeyAt { path: at(&c, [fx[2]]), key: "intensity".into(), time_secs: 1.0 },
        GraphOp::RemoveKeyAt { path: at(&c, [fx[0]]), key: "radius".into(), time_secs: 2.0 },
        GraphOp::SetAnim { path: at(&c, [fx[1]]), key: "amount".into(), anim: None },
        GraphOp::SetAnim { path: at(&c, [fx[2]]), key: "radius".into(), anim: Some(Animation { keys: vec![], modifiers: Some(serde_json::to_value(wiggle()).unwrap()) }) },
        GraphOp::SetAnim { path: at(&c, [fx[0]]), key: "radius".into(), anim: Some(Animation { keys: vec![key_json(0.0, 1.0)], modifiers: None }) },
    ];
    for op in ops {
        same_both_ways(&c, op);
    }
    // Before the display transform, where the engine can put one, a
    // clip's list can't hold it.
    for op in [
        GraphOp::MoveAfter { path: at(&c, [fx[2]]), after: g },
        GraphOp::AddNode { after: at(&c, [g]), node: new_effect(0xa4, &bloom, BTreeMap::new()) },
    ] {
        let err = c.clone().apply_picture_ops(std::slice::from_ref(&op)).unwrap_err();
        assert!(err.contains("display transform"), "{op:?}: {err}");
    }
}

/// An alias's key (ChromaKey's `soft_edge`, stored as `spill`) is written
/// under the name the effect is keyed by.
#[test]
fn a_key_on_an_aliased_parameter_lands_on_its_stored_name() {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    c.push_effect(E::ChromaKey { key_color: [0, 255, 0], tolerance: 0.25, soft_edge: 0.125, spill_method: klipp_effects::SpillMethod::None, spill_strength: 0.5 });
    let fx = effects(&picture(&c));
    same_both_ways(&c, GraphOp::AddKeyAt { path: at(&c, [fx[0]]), key: "soft_edge".into(), keyframe: key_json(1.0, 0.25) });
    let mut written = c.clone();
    written.apply_picture_ops(&[GraphOp::AddKeyAt { path: at(&c, [fx[0]]), key: "soft_edge".into(), keyframe: key_json(1.0, 0.25) }]).unwrap();
    assert!(written.effect_keyframes().contains_key("0:spill"), "{:?}", written.effect_keyframes().keys());
}

/// Ops inside the grade write the grade back from the edited copy.
#[test]
fn grade_ops_written_through_the_fields_are_the_engines() {
    let c = clip();
    let p = picture(&c);
    let g = grade_group(&p);
    let body = &p.graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
    let first = body.nodes.iter().find(|n| n.legacy.as_ref().is_some_and(|l| l.payload["kind"] == "Source")).map(|n| n.id).expect("a source in the grade");
    let exposure = Box::new(Node {
        id: uuid::Uuid::from_u128(0xb1),
        type_id: types::LEGACY_NODE.into(),
        type_version: 1,
        label: String::new(),
        enabled: true,
        params: Default::default(),
        anim: Default::default(),
        mix: None,
        wireless: Default::default(),
        group: None,
        legacy: Some(Legacy { slot: None, payload: json!({"kind": "Exposure", "label": "Exposure", "params": {"exposure": 0.5}}), keyframes: Some(json!({"tracks": []})), modifiers: Default::default() }),
        legacy_flags: None,
        origin: None,
    });
    same_both_ways(&c, GraphOp::AddNode { after: at(&c, [g, first]), node: exposure.clone() });
    let mut with = c.clone();
    with.apply_picture_ops(&[GraphOp::AddNode { after: at(&c, [g, first]), node: exposure }]).unwrap();
    let e = uuid::Uuid::from_u128(0xb1);
    same_both_ways(&with, GraphOp::SetParam { path: at(&with, [g, e]), key: "exposure".into(), value: ParamValue::Float(-0.5) });
    same_both_ways(&with, GraphOp::SetEnabled { path: at(&with, [g, e]), enabled: false });
    same_both_ways(&with, GraphOp::SetNodeMeta { path: at(&with, [g, e]), label: Some("Lift".into()), position: Some(json!({"x": 10.0, "y": 20.0})) });
    same_both_ways(&with, GraphOp::RemoveNode { path: at(&with, [g, e]) });
}

/// A batch is one edit: written whole or not at all, and undone whole.
#[test]
fn a_batch_is_one_edit_and_a_refusal_changes_nothing() {
    let c = clip();
    let p = picture(&c);
    let fx = effects(&p);
    let mut edited = c.clone();
    let edit = edited
        .apply_picture_ops(&[
            GraphOp::SetParam { path: at(&c, [fx[0]]), key: "radius".into(), value: ParamValue::Float(4.0) },
            GraphOp::MoveAfter { path: at(&c, [fx[0]]), after: fx[2] },
        ])
        .unwrap();
    assert!(matches!(edited.builtin_effects()[2], E::GaussianBlur { radius } if radius == 4.0), "{:?}", edited.builtin_effects());
    assert_eq!(edited.effect_keyframes().get("2:radius").map(Vec::len), Some(2), "the keys moved with the blur");
    let mut undone = edited.clone();
    undone.restore_picture(&edit.before);
    assert_eq!(picture(&undone), picture(&c));
    undone.restore_picture(&edit.after);
    assert_eq!(picture(&undone), picture(&edited));

    // A refused op anywhere in the batch writes nothing, the first op's
    // change included.
    let refusals = [
        GraphOp::SetParam { path: at(&c, [fx[0]]), key: "radius".into(), value: ParamValue::Text("wide".into()) },
        GraphOp::SetNodeMeta { path: at(&c, [fx[0]]), label: Some("Soft".into()), position: None },
        GraphOp::SetMix { path: at(&c, [fx[0]]), mix: Some(0.5) },
        GraphOp::RemoveNode { path: at(&c, [grade_group(&p)]) },
        GraphOp::RemoveKeyAt { path: at(&c, [fx[1]]), key: "amount".into(), time_secs: 3.0 },
        GraphOp::SetParam { path: NodePath::new(Root::Clip { clip: uuid::Uuid::nil() }, [fx[0]]), key: "radius".into(), value: ParamValue::Float(1.0) },
    ];
    for refused in refusals {
        let mut tried = c.clone();
        let first = GraphOp::SetParam { path: at(&c, [fx[1]]), key: "size".into(), value: ParamValue::Float(0.75) };
        assert!(tried.apply_picture_ops(&[first, refused.clone()]).is_err(), "{refused:?} went through");
        assert_eq!(saved(&tried), saved(&c), "{refused:?} changed the clip");
        assert!(tried.has_lifted_picture() || !c.has_lifted_picture(), "{refused:?} dropped the lifted picture");
    }
}

/// The nodes the lift places itself are refused for now, by name.
#[test]
fn nodes_the_clip_places_itself_are_refused_with_a_reason() {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    c.push_effect(E::TemporalDenoise { strength: 0.5, threshold: 0.25 });
    c.push_effect(E::GaussianBlur { radius: 2.0 });
    let p = picture(&c);
    let head = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::TEMPORAL_DENOISE).expect("a head node").id;
    let err = c.clone().apply_picture_ops(&[GraphOp::SetEnabled { path: at(&c, [head]), enabled: false }]).unwrap_err();
    assert!(err.contains("placed by the clip"), "{err}");
}

/// A picture-truth clip keeps its node ids through structural ops and a
/// save and reload (decision §10, T1).
#[test]
#[ignore = "un-ignored by the PR that adds the picture-truth predicate (M2 9c/10c); on the M2 close-out checklist"]
fn a_picture_truth_clip_keeps_node_ids_through_structural_ops_and_save() {
    unimplemented!("needs picture-truth clips, which arrive with 9c/10c");
}

/// A busy clip: about 30 grade nodes and 8 effects.
fn busy() -> Clip {
    let mut c = clip();
    for i in 0..5 {
        c.push_effect(E::Sharpen { amount: 0.1 * (i + 1) as f32 });
    }
    let p = picture(&c);
    let g = grade_group(&p);
    let body = &p.graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
    let mut after = body.nodes.iter().find(|n| n.legacy.as_ref().is_some_and(|l| l.payload["kind"] == "Source")).unwrap().id;
    let kinds = ["Exposure", "Contrast", "Saturation"];
    let ops: Vec<GraphOp> = (0..28u128)
        .map(|i| {
            let id = uuid::Uuid::from_u128(0xc00 + i);
            let kind = kinds[i as usize % kinds.len()];
            let node = Box::new(Node {
                id,
                type_id: types::LEGACY_NODE.into(),
                type_version: 1,
                label: String::new(),
                enabled: true,
                params: Default::default(),
                anim: Default::default(),
                mix: None,
                wireless: Default::default(),
                group: None,
                legacy: Some(Legacy { slot: None, payload: json!({"kind": kind, "label": kind, "params": {}}), keyframes: Some(json!({"tracks": []})), modifiers: Default::default() }),
                legacy_flags: None,
                origin: None,
            });
            let op = GraphOp::AddNode { after: at(&c, [g, after]), node };
            after = id;
            op
        })
        .collect();
    c.apply_picture_ops(&ops).unwrap();
    c
}

/// P1, the drag budget (amendment 8): one parameter step on a busy clip
/// through the op path, the lift the next step needs included. Run by
/// hand in the test profile (optimised): `cargo test -p klipp-timeline
/// --test picture_ops one_param_step -- --ignored --nocapture`.
#[test]
#[ignore = "a timing probe, run by hand; not a gate"]
fn one_param_step_on_a_busy_clip() {
    let mut c = busy();
    let fx = effects(&picture(&c));
    assert_eq!(fx.len(), 8);
    let mut times: Vec<f64> = (0..50)
        .map(|i| {
            let op = GraphOp::SetParam { path: at(&c, [fx[1]]), key: "size".into(), value: ParamValue::Float(0.3 + i as f32 * 0.01) };
            let start = std::time::Instant::now();
            c.apply_picture_ops(&[op]).unwrap();
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    let nodes = picture(&c).graph.nodes.iter().filter_map(|n| n.group.as_ref()).map(|g| g.graph.nodes.len()).sum::<usize>();
    println!("P1: one effect SetParam on a clip with {nodes} grade nodes and 8 effects: median {:.3} ms, p90 {:.3} ms", times[25], times[45]);

    // A grade parameter writes the grade back whole.
    let g = grade_group(&picture(&c));
    let node = uuid::Uuid::from_u128(0xc00);
    let mut times: Vec<f64> = (0..50)
        .map(|i| {
            let op = GraphOp::SetParam { path: at(&c, [g, node]), key: "exposure".into(), value: ParamValue::Float(0.1 + i as f32 * 0.01) };
            let start = std::time::Instant::now();
            c.apply_picture_ops(&[op]).unwrap();
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    println!("P1: one grade SetParam on the same clip: median {:.3} ms, p90 {:.3} ms", times[25], times[45]);
}
