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
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
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
    // Moving across the display boundary is refused by the planner;
    // adding there is refused when writing back to the clip's list.
    for (op, reason) in [
        (GraphOp::MoveAfter { path: at(&c, [fx[2]]), after: g }, "cannot move from display-referred to scene-referred"),
        (GraphOp::AddNode { after: at(&c, [g]), node: new_effect(0xa4, &bloom, BTreeMap::new()) }, "display transform"),
    ] {
        let mut unchanged = c.clone();
        let before = unchanged.picture_v2().unwrap();
        let stored_before = saved(&unchanged);
        let err = unchanged.apply_picture_ops(std::slice::from_ref(&op)).unwrap_err();
        assert!(err.contains(reason), "{op:?}: {err}");
        assert_eq!(unchanged.picture_v2().unwrap(), before, "a refused {op:?} must not change the graph");
        assert_eq!(saved(&unchanged), stored_before, "a refused {op:?} must not change the saved fields");
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

/// An HDR Zones node (#240) in a clip's grade is set by its zone keys
/// through the picture's ops, which read it with the frozen v1 types,
/// the same both ways. Control: the zones not named keep their defaults.
#[test]
fn an_hdr_zones_node_is_set_by_its_zone_keys() {
    use klipp_timeline::GradeAccess;
    let mut with = clip();
    let z = uuid::Uuid::from_u128(0xb2);
    with.edit_grade(|g| {
        g.splice_before_output(klipp_nodes::Node { id: z, kind: klipp_nodes::NodeKind::HdrZones, label: "HDR Zones".into(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] })
    });
    let p = picture(&with);
    let g = grade_group(&p);
    let set = |key: &str, v: f32| GraphOp::SetParam { path: at(&with, [g, z]), key: key.into(), value: ParamValue::Float(v) };
    same_both_ways(&with, set("hdr_highlight_exposure", 1.25));
    same_both_ways(&with, set("hdr_light_boundary", 0.5));
    let mut written = with.clone();
    written.apply_picture_ops(&[set("hdr_highlight_exposure", 1.25), set("hdr_light_boundary", 0.5)]).unwrap();
    let node = written.grade().nodes.iter().find(|n| n.id == z).expect("the node kept").clone();
    assert_eq!(node.kind, klipp_nodes::NodeKind::HdrZones);
    let settings = node.params.hdr_zones.expect("its zones");
    assert_eq!((settings.exposure[5], settings.boundary[4]), (1.25, 0.5));
    let untouched = klipp_core::HdrZonesSettings::default();
    assert_eq!((settings.exposure[4], settings.boundary[5]), (untouched.exposure[4], untouched.boundary[5]), "the other zones kept");
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
    // change included. (An effect's mix makes the clip picture-truth since
    // M2 step 9c: `picture_truth.rs`.)
    let refusals = [
        GraphOp::SetParam { path: at(&c, [fx[0]]), key: "radius".into(), value: ParamValue::Text("wide".into()) },
        GraphOp::SetNodeMeta { path: at(&c, [uuid::Uuid::nil()]), label: Some("Soft".into()), position: None },
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

/// A batch that adds a grade node and an effect together is written whole:
/// the grade from a copy that took the grade's ops alone (the full copy
/// holds the new effect, which has no place in the stored list yet), and
/// the result is the engine's.
#[test]
fn a_grade_op_and_an_added_effect_go_in_one_batch() {
    let c = clip();
    let p = picture(&c);
    let g = grade_group(&p);
    let fx = effects(&p);
    let body = &p.graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
    let source = body.nodes.iter().find(|n| n.legacy.as_ref().is_some_and(|l| l.payload["kind"] == "Source")).unwrap().id;
    let mut exposure = new_effect(0xd1, &E::Sharpen { amount: 0.5 }, BTreeMap::new());
    exposure.type_id = types::LEGACY_NODE.into();
    exposure.legacy = Some(Legacy { slot: None, payload: json!({"kind": "Exposure", "label": "Exposure", "params": {"exposure": 0.5}}), keyframes: Some(json!({"tracks": []})), modifiers: Default::default() });
    let ops = vec![
        GraphOp::AddNode { after: at(&c, [fx[2]]), node: new_effect(0xd2, &E::Sharpen { amount: 0.25 }, BTreeMap::new()) },
        GraphOp::AddNode { after: at(&c, [g, source]), node: exposure },
        GraphOp::SetParam { path: at(&c, [g, uuid::Uuid::from_u128(0xd1)]), key: "exposure".into(), value: ParamValue::Float(-0.5) },
    ];
    let mut engine = klipp_graph_lift::paths::Documents::default();
    engine.clips.insert(c.id.0, p.clone());
    klipp_graph_lift::ops::apply_batch_with_catalog(&mut engine, &ops, &klipp_timeline::RegistryCatalog).unwrap();
    let mut written = c.clone();
    written.apply_picture_ops(&ops).unwrap();
    assert_eq!(written.builtin_effects().len(), 4);
    assert_eq!(support::picture_ops_check::normalised(&picture(&written)), support::picture_ops_check::normalised(&engine.clips[&c.id.0]));
}

/// Today's effect commands built as graph ops write exactly what the
/// effect-list mutators write: list, flags, keys and stacks.
#[test]
fn effect_commands_as_ops_write_what_the_mutators_write() {
    use klipp_timeline::EffectEdit;
    let mut base = clip();
    let id1 = base.effect_id_at(1).unwrap();
    base.set_effect_enabled(id1, false);
    let sharpen = E::Sharpen { amount: 0.25 };
    let wider = E::Vignette { amount: 0.75, size: 0.25, softness: 0.5, roundness: 0.5 };
    let cases: Vec<(EffectEdit, Box<dyn Fn(&mut Clip)>)> = vec![
        (EffectEdit::Add(&sharpen), Box::new(|c: &mut Clip| { c.push_effect(E::Sharpen { amount: 0.25 }); })),
        (EffectEdit::Remove(0), Box::new(|c: &mut Clip| { c.remove_builtin_effect(0); })),
        (EffectEdit::Remove(1), Box::new(|c: &mut Clip| { c.remove_builtin_effect(1); })),
        (EffectEdit::SetEnabled(1, true), Box::new(|c: &mut Clip| { let id = c.effect_id_at(1).unwrap(); c.set_effect_enabled(id, true); })),
        (EffectEdit::SetEnabled(2, false), Box::new(|c: &mut Clip| { let id = c.effect_id_at(2).unwrap(); c.set_effect_enabled(id, false); })),
        (EffectEdit::Update(1, &wider), Box::new(|c: &mut Clip| { let id = c.effect_id_at(1).unwrap(); c.replace_effect(id, E::Vignette { amount: 0.75, size: 0.25, softness: 0.5, roundness: 0.5 }); })),
        (EffectEdit::Move { from: 0, to: 2 }, Box::new(|c: &mut Clip| { c.move_builtin_effect(0, 2); })),
        (EffectEdit::Move { from: 2, to: 0 }, Box::new(|c: &mut Clip| { c.move_builtin_effect(2, 0); })),
        (EffectEdit::Move { from: 0, to: 1 }, Box::new(|c: &mut Clip| { c.move_builtin_effect(0, 1); })),
        (EffectEdit::Move { from: 2, to: 1 }, Box::new(|c: &mut Clip| { c.move_builtin_effect(2, 1); })),
    ];
    for (edit, old) in cases {
        let mut by_ops = base.clone();
        let ops = by_ops.effect_edit_ops(edit).unwrap_or_else(|| panic!("{edit:?} has no ops"));
        by_ops.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{edit:?}: {e}"));
        let mut by_mutator = base.clone();
        old(&mut by_mutator);
        assert_eq!(saved(&by_ops), saved(&by_mutator), "{edit:?}");
    }
    // An empty list takes its first effect as a push does.
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let empty = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    let mut by_ops = empty.clone();
    let ops = by_ops.effect_edit_ops(EffectEdit::Add(&sharpen)).unwrap();
    by_ops.apply_picture_ops(&ops).unwrap();
    let mut pushed = empty.clone();
    pushed.push_effect(sharpen.clone());
    assert_eq!(saved(&by_ops), saved(&pushed));
}

/// An edit touching a kind the clip places itself has no ops: it stays on
/// the mutators, as today (4c's condition: no user regression).
#[test]
fn effect_commands_on_placed_kinds_stay_on_the_mutators() {
    use klipp_timeline::EffectEdit;
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let transform = E::Transform {
        position_x: 0.1, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0,
        anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
    };
    let placed = [
        transform,
        E::Opacity { value: 0.5 },
        E::TemporalDenoise { strength: 0.5, threshold: 0.25 },
    ];
    for kind in placed {
        let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        c.push_effect(E::GaussianBlur { radius: 2.0 });
        c.push_effect(kind.clone());
        assert!(c.effect_edit_ops(EffectEdit::Add(&kind)).is_none(), "{kind:?}: add");
        assert!(c.effect_edit_ops(EffectEdit::Add(&E::Sharpen { amount: 0.5 })).is_none(), "{kind:?}: add after it");
        assert!(c.effect_edit_ops(EffectEdit::Remove(1)).is_none(), "{kind:?}: remove");
        assert!(c.effect_edit_ops(EffectEdit::SetEnabled(1, false)).is_none(), "{kind:?}: toggle");
        assert!(c.effect_edit_ops(EffectEdit::Update(1, &kind)).is_none(), "{kind:?}: update");
        assert!(c.effect_edit_ops(EffectEdit::Move { from: 1, to: 0 }).is_none(), "{kind:?}: move");
        assert!(c.effect_edit_ops(EffectEdit::Remove(0)).is_some(), "{kind:?}: the plain blur still takes ops");
    }
}

/// AR3's case on a clip stored as fields: a window added beside the grade's
/// chain and connected to a correction's mask, in one batch, writes back
/// through the grade, which then holds the window and the edge; the
/// picture is the engine's. Beside the clip's own chain, unwired, it is
/// refused.
#[test]
fn a_window_beside_the_grade_masks_a_correction_through_the_fields() {
    use klipp_graph_format::ports::{input_key, output_key};
    let c = clip();
    let p = picture(&c);
    let g = grade_group(&p);
    let body = &p.graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
    let corrector = body.nodes.iter().find(|n| n.legacy.as_ref().is_some_and(|l| l.payload["kind"] == "Corrector")).expect("a corrector in the grade").id;
    let mut window = new_effect(0xe1, &E::Sharpen { amount: 0.5 }, BTreeMap::new());
    window.type_id = types::LEGACY_NODE.into();
    window.legacy = Some(Legacy { slot: None, payload: json!({"kind": "Mask", "label": "Window", "params": {}}), keyframes: Some(json!({"tracks": []})), modifiers: Default::default() });
    let frame = body.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP_FRAME).expect("the grade's frame").id;
    let ops = vec![
        GraphOp::AddNodeBeside { at: at(&c, [g, corrector]), node: window.clone() },
        GraphOp::Connect { from: at(&c, [g, frame]), from_port: "out".into(), to: at(&c, [g, window.id]), to_port: input_key("Mask", 0) },
        GraphOp::Connect { from: at(&c, [g, window.id]), from_port: output_key("Mask", 0), to: at(&c, [g, corrector]), to_port: input_key("Corrector", 1) },
    ];
    let mut engine = klipp_graph_lift::paths::Documents::default();
    engine.clips.insert(c.id.0, p.clone());
    klipp_graph_lift::ops::apply_batch_with_catalog(&mut engine, &ops, &klipp_timeline::RegistryCatalog).unwrap();
    let mut written = c.clone();
    written.apply_picture_ops(&ops).unwrap();
    assert_eq!(support::picture_ops_check::normalised(&picture(&written)), support::picture_ops_check::normalised(&engine.clips[&c.id.0]));
    let grade = klipp_timeline::GradeAccess::grade(&written);
    let w = grade.nodes.iter().find(|n| n.id == window.id).expect("the window is in the grade");
    assert_eq!(w.kind, klipp_nodes::NodeKind::Mask);
    assert!(grade.edges.iter().any(|e| e.from_node == window.id && e.to_node == corrector && e.to_port == 1), "and masks the corrector");

    // Beside the clip's own chain a node is the picture's alone (it makes
    // the clip picture-truth, `picture_truth.rs`), and one left unwired
    // there is refused by the batch rule.
    let fx = effects(&p);
    let err = c.clone().apply_picture_ops(&[GraphOp::AddNodeBeside { at: at(&c, [fx[0]]), node: new_effect(0xe2, &E::Sharpen { amount: 0.5 }, BTreeMap::new()) }]).unwrap_err();
    assert!(err.contains("Unwired"), "{err}");
}

/// The engine's batch rule holds on a clip stored as fields too: a node
/// whose type needs an input (a corrector), added beside the grade and
/// connected in the same batch, is applied; added alone, it is refused,
/// naming the input, and the clip is as it was.
#[test]
fn a_node_beside_waits_for_its_input_until_the_batch_ends() {
    use klipp_graph_format::ports::input_key;
    let c = clip();
    let p = picture(&c);
    let g = grade_group(&p);
    let body = &p.graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
    let frame = body.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP_FRAME).expect("the grade's frame").id;
    let corrector = body.nodes.iter().find(|n| n.legacy.as_ref().is_some_and(|l| l.payload["kind"] == "Corrector")).expect("a corrector").id;
    let mut second = new_effect(0xf1, &E::Sharpen { amount: 0.5 }, BTreeMap::new());
    second.type_id = types::LEGACY_NODE.into();
    second.legacy = Some(Legacy { slot: None, payload: json!({"kind": "Corrector", "label": "Second", "params": {}}), keyframes: Some(json!({"tracks": []})), modifiers: Default::default() });
    let beside = GraphOp::AddNodeBeside { at: at(&c, [g, corrector]), node: second.clone() };

    let mut alone = c.clone();
    let (index, err) = alone.apply_picture_ops_indexed(std::slice::from_ref(&beside)).unwrap_err();
    assert_eq!(index, Some(0));
    assert_eq!(alone.apply_picture_ops_indexed(&[]).unwrap_err().0, None);
    assert!(err.contains("Unwired") || err.contains("needs its"), "{err}");
    assert_eq!(saved(&alone), saved(&c), "a refusal changes nothing");

    let fed = GraphOp::Connect { from: at(&c, [g, frame]), from_port: "out".into(), to: at(&c, [g, second.id]), to_port: input_key("Corrector", 0) };
    let mut both = c.clone();
    both.apply_picture_ops(&[beside, fed]).unwrap_or_else(|e| panic!("the batch connects it: {e}"));
    assert!(klipp_timeline::GradeAccess::grade(&both).nodes.iter().any(|n| n.id == second.id), "the second corrector is in the grade");
}

/// A clip's plug-ins run between its grade and its built-in effects, and
/// the picture can't put an effect before them until plug-ins are nodes
/// (step 11c-2 relies on the order): an effect added after the grade or
/// the display step, ahead of the plug-in, is refused and changes nothing.
/// After the plug-in it lands, as the first of the effects.
#[test]
fn an_effect_is_never_placed_before_a_plugin() {
    let mut c = clip();
    c.set_ofx_effects(vec![klipp_plugin::AppliedEffect { plugin_name: "com.klipp.test.Invert".into(), enabled: true, ..Default::default() }]);
    let p = picture(&c);
    let find = |t: &str| p.graph.nodes.iter().find(|n| n.type_id.as_str() == t).unwrap_or_else(|| panic!("a {t} node")).id;
    let (group, display, ofx) = (find(lt::GROUP), find(lt::SCENE_TO_DISPLAY), find(types::LEGACY_OFX));
    let before = saved(&c);
    for (what, after) in [("the grade", group), ("the display step", display)] {
        let err = c.clone().apply_picture_ops(&[GraphOp::AddNode { after: at(&c, [after]), node: new_effect(0xa0, &E::Gamma { gamma: 1.2 }, BTreeMap::new()) }]).unwrap_err();
        eprintln!("after {what}: {err}");
        let mut tried = c.clone();
        let _ = tried.apply_picture_ops(&[GraphOp::AddNode { after: at(&c, [after]), node: new_effect(0xa0, &E::Gamma { gamma: 1.2 }, BTreeMap::new()) }]);
        assert_eq!(saved(&tried), before, "after {what}: nothing changed");
    }
    // Moving an effect there is refused the same way.
    let first = effects(&p)[0];
    assert!(c.clone().apply_picture_ops(&[GraphOp::MoveAfter { path: at(&c, [first]), after: group }]).is_err(), "an effect moved before the plug-in");
    // After the plug-in is where effects go.
    let mut ok = c.clone();
    ok.apply_picture_ops(&[GraphOp::AddNode { after: at(&c, [ofx]), node: new_effect(0xa1, &E::Gamma { gamma: 1.2 }, BTreeMap::new()) }]).unwrap();
    assert!(matches!(ok.builtin_effects()[0], E::Gamma { .. }), "the added effect leads the list: {:?}", ok.builtin_effects());
}

/// An effect a batch adds to a clip whose fields hold its effects keeps
/// the id the batch gave it, and every effect the insert moved down a slot
/// keeps its own (M3 proposal 6): the batch renames nothing, and the next
/// batch reaches each effect by the id it had (#1256 needed a rename while
/// the write-back named effects by slot).
#[test]
fn an_added_effect_keeps_its_id_and_the_effects_it_moves_keep_theirs() {
    let mut c = clip();
    let fx = effects(&picture(&c));
    let added = uuid::Uuid::from_u128(0xc1);
    let blur = E::GaussianBlur { radius: 11.0 };
    let (_, renamed) = c.apply_picture_ops_renamed(&[GraphOp::AddNode { after: at(&c, [fx[0]]), node: new_effect(0xc1, &blur, BTreeMap::new()) }]).unwrap();
    assert!(renamed.is_empty(), "{renamed:?}");
    assert_eq!(effects(&picture(&c)), vec![fx[0], added, fx[1], fx[2]]);
    c.apply_picture_ops(&[GraphOp::SetParam { path: at(&c, [added]), key: "radius".into(), value: ParamValue::Float(2.5) }]).unwrap();
    assert!(matches!(c.builtin_effects()[1], E::GaussianBlur { radius } if (radius - 2.5).abs() < 1e-6), "{:?}", c.builtin_effects());
    c.apply_picture_ops(&[GraphOp::SetEnabled { path: at(&c, [fx[1]]), enabled: false }]).unwrap();
    assert!(matches!(c.builtin_effects()[2], E::Vignette { .. }));
    assert_eq!(c.builtin_effects_enabled().get(2), Some(&false));
}

/// A picture-truth clip keeps the batch's ids, so its batch renames
/// nothing.
#[test]
fn a_picture_truth_clip_renames_nothing() {
    let mut c = clip();
    let fx = effects(&picture(&c));
    // A mix makes the clip picture-truth (the fields can't hold it).
    c.apply_picture_ops(&[GraphOp::SetMix { path: at(&c, [fx[0]]), mix: Some(0.5) }]).unwrap();
    assert!(c.is_picture_truth());
    let (_, renamed) = c.apply_picture_ops_renamed(&[GraphOp::AddNode { after: at(&c, [fx[0]]), node: new_effect(0xc2, &E::GaussianBlur { radius: 4.0 }, BTreeMap::new()) }]).unwrap();
    assert!(renamed.is_empty(), "{renamed:?}");
    assert!(effects(&picture(&c)).contains(&uuid::Uuid::from_u128(0xc2)));
}

/// P-B2 (M3 proposal 6): on a clip whose fields hold its effects, a move,
/// a mid-list insert and a remove keep every surviving effect's node id,
/// through a save and a reload (the clip's record read back, its picture
/// as the file stores it adopted), and ops on the reloaded clip reach the
/// same effects by those ids.
#[test]
fn a_reorder_keeps_effect_node_ids_through_save_and_reload() {
    // Three effects and no modifier stacks: the record goes through serde
    // here, and the project writer, not serde, writes a stack's keys.
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    c.push_effect(E::GaussianBlur { radius: 3.0 });
    c.push_effect(E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 });
    c.push_effect(E::Bloom { threshold: 0.75, radius: 6.0, intensity: 0.5 });
    let fx = effects(&picture(&c));
    let added = uuid::Uuid::from_u128(0xc3);
    c.apply_picture_ops(&[GraphOp::MoveAfter { path: at(&c, [fx[0]]), after: fx[2] }]).unwrap();
    c.apply_picture_ops(&[GraphOp::AddNode { after: at(&c, [fx[1]]), node: new_effect(0xc3, &E::Sharpen { amount: 0.5 }, BTreeMap::new()) }]).unwrap();
    c.apply_picture_ops(&[GraphOp::RemoveNode { path: at(&c, [fx[2]]) }]).unwrap();
    assert!(!c.is_picture_truth(), "a clip whose fields hold it all");
    let after = picture(&c);
    assert_eq!(effects(&after), vec![fx[1], added, fx[0]], "each effect keeps its id wherever it moved");

    // Saved and read back, as the file does it: the record, and the
    // picture the file stores.
    let record = serde_json::to_string(&c).unwrap();
    let versions = c.picture_v2().unwrap().versions.clone();
    let mut reloaded: Clip = serde_json::from_str(&record).unwrap();
    reloaded.adopt_stored_picture(klipp_timeline::LiftedPicture { picture: after.clone(), versions }).unwrap();
    assert_eq!(picture(&reloaded), after, "the reloaded clip's picture is the saved one, ids and all");
    reloaded.apply_picture_ops(&[GraphOp::SetParam { path: at(&reloaded, [added]), key: "amount".into(), value: ParamValue::Float(0.25) }]).unwrap();
    assert!(matches!(reloaded.builtin_effects()[1], E::Sharpen { amount } if (amount - 0.25).abs() < 1e-6), "{:?}", reloaded.builtin_effects());
    reloaded.apply_picture_ops(&[GraphOp::SetEnabled { path: at(&reloaded, [fx[0]]), enabled: false }]).unwrap();
    assert_eq!(reloaded.builtin_effects_enabled().get(2), Some(&false), "the blur, moved last, by its own id");
}

/// P-B3: a copy under a new clip id (a duplicate, a paste, a split half)
/// shares no effect id with its source, and keeps its own through edits.
#[test]
fn a_copied_clip_gets_fresh_effect_ids() {
    let c = clip();
    let source = effects(&picture(&c));
    let mut copy = c.clone();
    copy.id = klipp_core::ClipId::new();
    let ids = effects(&picture(&copy));
    assert!(ids.iter().all(|id| !source.contains(id)), "{ids:?} against {source:?}");
    copy.apply_picture_ops(&[GraphOp::MoveAfter { path: at(&copy, [ids[0]]), after: ids[2] }]).unwrap();
    assert_eq!(effects(&picture(&copy)), vec![ids[1], ids[2], ids[0]]);
}

/// An id an effect gave up is never given again, across a save and a
/// reload too: add an effect, remove it, save and reload, add another,
/// and the new one has a new id, so a client holding the removed one's
/// path is refused rather than handed the new effect.
#[test]
fn a_removed_effects_id_is_never_given_again_after_a_reload() {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    c.push_effect(E::GaussianBlur { radius: 3.0 });
    let newest = c.push_effect(E::Sharpen { amount: 0.5 }).as_uuid();
    c.apply_picture_ops(&[GraphOp::RemoveNode { path: at(&c, [newest]) }]).unwrap();
    let record = serde_json::to_string(&c).unwrap();
    let lifted = c.picture_v2().unwrap();
    let mut reloaded: Clip = serde_json::from_str(&record).unwrap();
    reloaded.adopt_stored_picture(klipp_timeline::LiftedPicture { picture: lifted.picture.clone(), versions: lifted.versions.clone() }).unwrap();
    let again = reloaded.push_effect(E::Sharpen { amount: 0.5 }).as_uuid();
    assert_ne!(again, newest, "a new effect, a new id");
    assert!(effects(&picture(&reloaded)).contains(&again));
    assert!(reloaded.clone().apply_picture_ops(&[GraphOp::SetParam { path: at(&reloaded, [newest]), key: "amount".into(), value: ParamValue::Float(1.0) }]).is_err(), "the removed effect's path is refused");
}

/// A whole-list replacement's new ids are never ones an earlier session
/// gave out: replace the list, remove an effect, save and reload, replace
/// the list again, and no id comes back, so the removed effect's path is
/// refused rather than handed a new effect (the run of new ids isn't
/// saved, so it can't start again where it was).
#[test]
fn a_whole_list_replacement_after_a_reload_repeats_no_id() {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    let list = || vec![E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }];
    c.set_effect_list(list(), vec![true, true]);
    let first = effects(&picture(&c));
    c.apply_picture_ops(&[GraphOp::RemoveNode { path: at(&c, [first[1]]) }]).unwrap();
    let record = serde_json::to_string(&c).unwrap();
    let lifted = c.picture_v2().unwrap();
    let mut reloaded: Clip = serde_json::from_str(&record).unwrap();
    reloaded.adopt_stored_picture(klipp_timeline::LiftedPicture { picture: lifted.picture.clone(), versions: lifted.versions.clone() }).unwrap();
    assert_eq!(effects(&picture(&reloaded)), vec![first[0]], "the kept effect keeps its id");
    for replace in [|c: &mut Clip, l| c.set_effect_list(l, vec![true, true]), |c: &mut Clip, l| c.set_effects(l)] {
        let mut again = reloaded.clone();
        replace(&mut again, list());
        let ids = effects(&picture(&again));
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| !first.contains(id)), "{ids:?} against {first:?}");
        assert!(again.clone().apply_picture_ops(&[GraphOp::SetParam { path: at(&again, [first[1]]), key: "amount".into(), value: ParamValue::Float(1.0) }]).is_err(), "the removed effect's path is refused");
    }
}
