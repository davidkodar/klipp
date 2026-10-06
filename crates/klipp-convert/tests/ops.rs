//! Graph ops by path (#1083): each op, each refusal as a sentence, and
//! undo from the edit's `before` giving the original back.

use klipp_convert::ops::{apply, undo, GraphOp};
use klipp_convert::paths::{resolve, Documents};
use klipp_convert::revision2;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root, TrackPart};
use klipp_graph_format::{types, Legacy, Node, NodeId, ParamValue};
use serde_json::{json, Value};
use uuid::Uuid;

const CLIP: &str = "00000000-0000-0000-0000-000000000c01";
const TRACK: &str = "00000000-0000-0000-0000-000000000701";
const S: &str = "00000000-0000-0000-0000-0000000000a0";
const E: &str = "00000000-0000-0000-0000-0000000000a1";
const O: &str = "00000000-0000-0000-0000-0000000000a2";

fn u(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

fn docs() -> Documents {
    let grade = json!({
        "nodes": [{"id": S, "kind": "Source", "label": "S", "enabled": true}, {"id": E, "kind": "Exposure", "label": "E", "enabled": true, "params": {"exposure": 0.5}},
                  {"id": O, "kind": "Output", "label": "O", "enabled": true}],
        "edges": [{"from_node": S, "from_port": 0, "to_node": E, "to_port": 0}, {"from_node": E, "from_port": 0, "to_node": O, "to_port": 0}],
        "output_node_id": O
    });
    let blur = |r: f64| json!({"type": "gaussian_blur", "radius": r});
    let p = json!({"version": "1.0.0", "sequences": [{"id": "00000000-0000-0000-0000-000000000501", "name": "S", "tracks": [{
        "id": TRACK, "name": "V1", "kind": "Video", "effects": {"video": [blur(1.0), blur(2.0)]},
        "clips": [{"id": CLIP, "kind": {"type": "media", "media_id": "00000000-0000-0000-0000-0000000000d1"}, "node_graph": grade, "builtin_effects": [blur(3.0)]}]
    }]}]});
    Documents::of(&revision2::project(&p).unwrap().0).unwrap()
}

fn group(d: &Documents) -> NodeId {
    d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().id
}

fn effect(d: &Documents) -> NodeId {
    d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.type_id.as_str() == types::LEGACY_EFFECT).unwrap().id
}

fn clip_path(ids: impl IntoIterator<Item = NodeId>) -> NodePath {
    NodePath::new(Root::Clip { clip: u(CLIP) }, ids)
}

fn snapshot(d: &Documents) -> Value {
    serde_json::to_value((&d.clips, &d.track_effects)).unwrap()
}

/// A grade node of kind `kind`, carrying its v1 entry.
fn grade_node(id: u128, kind: &str) -> Box<Node> {
    Box::new(Node {
        id: Uuid::from_u128(id),
        type_id: types::LEGACY_NODE.into(),
        type_version: 1,
        label: String::new(),
        enabled: true,
        params: Default::default(),
        anim: Default::default(),
        mix: None,
        wireless: Default::default(),
        group: None,
        legacy: Some(Legacy { slot: None, payload: json!({"kind": kind, "label": kind}), keyframes: None, modifiers: Default::default() }),
        legacy_flags: None,
        origin: None,
    })
}

#[test]
fn set_param_edits_the_entry_and_undoes() {
    let mut d = docs();
    let before = snapshot(&d);
    let p = clip_path([effect(&d)]);
    let edit = apply(&mut d, &GraphOp::SetParam { path: p.clone(), key: "radius".into(), value: ParamValue::Float(5.0) }).unwrap();
    assert_eq!(resolve(&d, &p).unwrap().legacy.as_ref().unwrap().payload["radius"], 5.0);
    undo(&mut d, &edit).unwrap();
    assert_eq!(snapshot(&d), before, "undo gives the original back");
    // A grade node's parameter lands in its v1 params.
    let e = clip_path([group(&d), u(E)]);
    apply(&mut d, &GraphOp::SetParam { path: e.clone(), key: "exposure".into(), value: ParamValue::Float(-1.0) }).unwrap();
    assert_eq!(resolve(&d, &e).unwrap().legacy.as_ref().unwrap().payload["params"]["exposure"], -1.0);
}

#[test]
fn a_value_the_entry_cant_take_is_refused_and_nothing_changes() {
    let mut d = docs();
    let before = snapshot(&d);
    let fx = effect(&d);
    let r = apply(&mut d, &GraphOp::SetParam { path: clip_path([fx]), key: "radius".into(), value: ParamValue::Text("wide".into()) });
    assert_eq!(r.unwrap_err(), "radius doesn't take Text(\"wide\")");
    assert_eq!(snapshot(&d), before);
}

#[test]
fn nodes_go_in_and_out_of_a_grade_rewired() {
    let mut d = docs();
    let before = snapshot(&d);
    let g = group(&d);
    // In after Exposure: what read Exposure now reads the new node.
    let add = apply(&mut d, &GraphOp::AddNode { after: clip_path([g, u(E)]), node: grade_node(0x51, "Saturation") }).unwrap();
    let body = &d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
    let sat = Uuid::from_u128(0x51);
    assert!(body.edges.iter().any(|e| e.from.0 == u(E) && e.to.0 == sat));
    assert!(body.edges.iter().any(|e| e.from.0 == sat && e.to.0 == u(O)));
    assert!(!body.edges.iter().any(|e| e.from.0 == u(E) && e.to.0 == u(O)));
    // Out again: its reader takes its input back.
    apply(&mut d, &GraphOp::RemoveNode { path: clip_path([g, sat]) }).unwrap();
    let body = &d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
    assert!(body.edges.iter().any(|e| e.from.0 == u(E) && e.to.0 == u(O)));
    undo(&mut d, &add).unwrap();
    assert_eq!(snapshot(&d), before);
    // The graph's ends stay put.
    assert_eq!(apply(&mut d, &GraphOp::RemoveNode { path: clip_path([g, u(O)]) }).unwrap_err(), "a graph's ends can't be removed");
}

#[test]
fn wiring_that_breaks_the_graph_is_refused() {
    let mut d = docs();
    let before = snapshot(&d);
    let g = group(&d);
    // A loop: the output's picture back into Exposure.
    let r = apply(&mut d, &GraphOp::Connect { from: clip_path([g, u(O)]), from_port: "out".into(), to: clip_path([g, u(E)]), to_port: "in".into() });
    assert!(r.unwrap_err().contains("Cycle"));
    // An unwired required input.
    let r = apply(&mut d, &GraphOp::Disconnect { to: clip_path([g, u(E)]), port: "in".into() });
    assert!(r.unwrap_err().contains("Unwired"));
    // Across levels.
    let fx = effect(&d);
    let r = apply(&mut d, &GraphOp::Connect { from: clip_path([fx]), from_port: "out".into(), to: clip_path([g, u(E)]), to_port: "in".into() });
    assert_eq!(r.unwrap_err(), "a connection joins two nodes of one graph");
    assert_eq!(snapshot(&d), before, "nothing landed");
}

#[test]
fn enable_and_mix_are_plain_edits() {
    let mut d = docs();
    let p = clip_path([effect(&d)]);
    apply(&mut d, &GraphOp::SetEnabled { path: p.clone(), enabled: false }).unwrap();
    apply(&mut d, &GraphOp::SetMix { path: p.clone(), mix: Some(0.5) }).unwrap();
    let n = resolve(&d, &p).unwrap();
    assert_eq!((n.enabled, n.mix), (false, Some(0.5)));
}

#[test]
fn a_tracks_effects_are_a_list() {
    let mut d = docs();
    let list = |d: &Documents| d.track_effects[&u(TRACK)].iter().map(|n| n.legacy.as_ref().unwrap().payload["radius"].as_f64().unwrap()).collect::<Vec<_>>();
    let ids: Vec<NodeId> = d.track_effects[&u(TRACK)].iter().map(|n| n.id).collect();
    let tp = |id| NodePath::new(Root::Track { track: u(TRACK), part: TrackPart::Effects }, [id]);
    apply(&mut d, &GraphOp::MoveAfter { path: tp(ids[0]), after: ids[1] }).unwrap();
    assert_eq!(list(&d), [2.0, 1.0]);
    apply(&mut d, &GraphOp::SetParam { path: tp(ids[0]), key: "radius".into(), value: ParamValue::Float(4.0) }).unwrap();
    assert_eq!(list(&d), [2.0, 4.0]);
    apply(&mut d, &GraphOp::RemoveNode { path: tp(ids[1]) }).unwrap();
    assert_eq!(list(&d), [4.0]);
    let r = apply(&mut d, &GraphOp::Disconnect { to: tp(ids[0]), port: "in".into() });
    assert_eq!(r.unwrap_err(), "a track's effects are a list, with nothing to wire");
}

/// Moving a node in a grade takes it out and puts it back after another,
/// rewired both times.
#[test]
fn a_node_moves_within_a_grade() {
    let mut d = docs();
    let g = group(&d);
    let sat = Uuid::from_u128(0x51);
    apply(&mut d, &GraphOp::AddNode { after: clip_path([g, u(E)]), node: grade_node(0x51, "Saturation") }).unwrap();
    apply(&mut d, &GraphOp::MoveAfter { path: clip_path([g, u(E)]), after: sat }).unwrap();
    let body = &d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
    let fed = |to: Uuid| body.edges.iter().find(|e| e.to.0 == to && e.to.1.as_str() == "in").map(|e| e.from.0);
    assert_eq!(fed(sat), Some(u(S)), "Saturation now reads the source");
    assert_eq!(fed(u(E)), Some(sat), "Exposure reads Saturation");
    assert_eq!(fed(u(O)), Some(u(E)), "and the output reads Exposure");
}

/// Moving a gated correction keeps its mask: only its picture input is
/// rewired (#1102 review, 5).
#[test]
fn a_masked_node_moves_with_its_mask() {
    const K: &str = "00000000-0000-0000-0000-0000000000a3";
    const SAT: &str = "00000000-0000-0000-0000-0000000000a4";
    let grade = json!({
        "nodes": [{"id": S, "kind": "Source", "label": "S", "enabled": true}, {"id": K, "kind": "LumaKey", "label": "K", "enabled": true},
                  {"id": E, "kind": "Exposure", "label": "E", "enabled": true}, {"id": SAT, "kind": "Saturation", "label": "Sat", "enabled": true},
                  {"id": O, "kind": "Output", "label": "O", "enabled": true}],
        "edges": [{"from_node": S, "from_port": 0, "to_node": K, "to_port": 0}, {"from_node": S, "from_port": 0, "to_node": E, "to_port": 0},
                  {"from_node": K, "from_port": 0, "to_node": E, "to_port": 1}, {"from_node": E, "from_port": 0, "to_node": SAT, "to_port": 0},
                  {"from_node": SAT, "from_port": 0, "to_node": O, "to_port": 0}],
        "output_node_id": O
    });
    let p = json!({"version": "1.0.0", "sequences": [{"id": "00000000-0000-0000-0000-000000000501", "name": "S", "tracks": [{
        "id": TRACK, "name": "V1", "kind": "Video",
        "clips": [{"id": CLIP, "kind": {"type": "media", "media_id": "00000000-0000-0000-0000-0000000000d1"}, "node_graph": grade}]
    }]}]});
    let mut d = Documents::of(&revision2::project(&p).unwrap().0).unwrap();
    let g = group(&d);
    let mask_of = |d: &Documents| {
        let body = &d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
        body.edges.iter().find(|e| e.to.0 == u(E) && e.to.1.as_str() == "mask").map(|e| e.from.0)
    };
    assert_eq!(mask_of(&d), Some(u(K)), "gated as saved");
    apply(&mut d, &GraphOp::MoveAfter { path: clip_path([g, u(E)]), after: u(SAT) }).unwrap();
    assert_eq!(mask_of(&d), Some(u(K)), "the mask moved with it");
    let body = &d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
    assert!(body.edges.iter().any(|e| e.from.0 == u(SAT) && e.to.0 == u(E) && e.to.1.as_str() == "in"), "and reads Saturation now");
}

/// Every op on a track's effect list is checked before it lands: no end,
/// group, spliced node or non-effect entry gets in (#1102 review, 7).
#[test]
fn a_track_list_takes_effect_entries_only() {
    let mut d = docs();
    let before = snapshot(&d);
    let first = d.track_effects[&u(TRACK)][0].id;
    let tp = |id| NodePath::new(Root::Track { track: u(TRACK), part: TrackPart::Effects }, [id]);
    // A grade node isn't an effect entry.
    let r = apply(&mut d, &GraphOp::AddNode { after: tp(first), node: grade_node(0x61, "Saturation") });
    assert!(r.unwrap_err().contains("effect and plug-in entries"));
    // Nor is a node that came from a splice.
    let mut spliced = d.track_effects[&u(TRACK)][0].clone();
    spliced.id = Uuid::from_u128(0x62);
    spliced.origin = Some(klipp_graph_format::lifted::Origin::Track { track: u(TRACK) });
    let r = apply(&mut d, &GraphOp::AddNode { after: tp(first), node: Box::new(spliced) });
    assert!(r.unwrap_err().contains("effect and plug-in entries"));
    assert_eq!(snapshot(&d), before, "nothing landed");
}

/// A malformed stored entry is an error, not a panic; what a node is isn't
/// a setting; a placement's mode is one of its two (#1102 review, 8).
#[test]
fn set_param_refuses_what_it_cant_set() {
    let mut d = docs();
    let fx = effect(&d);
    let r = apply(&mut d, &GraphOp::SetParam { path: clip_path([fx]), key: "type".into(), value: ParamValue::Text("vignette".into()) });
    assert!(r.unwrap_err().contains("names what the node is"));
    let g = group(&d);
    let r = apply(&mut d, &GraphOp::SetParam { path: clip_path([g, u(E)]), key: "kind".into(), value: ParamValue::Text("Saturation".into()) });
    assert!(r.unwrap_err().contains("names what the node is"));
    // A stored entry that isn't an object.
    let clip = d.clips.get_mut(&u(CLIP)).unwrap();
    clip.graph.nodes.iter_mut().find(|n| n.id == fx).unwrap().legacy.as_mut().unwrap().payload = json!("broken");
    let r = apply(&mut d, &GraphOp::SetParam { path: clip_path([fx]), key: "radius".into(), value: ParamValue::Float(1.0) });
    assert!(r.unwrap_err().contains("isn't an object"));
    // A placement's mode.
    let place = d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.type_id.as_str() == lt::PLACEMENT).unwrap().id;
    let r = apply(&mut d, &GraphOp::SetParam { path: clip_path([place]), key: "mode".into(), value: ParamValue::Enum(7) });
    assert!(r.unwrap_err().contains("Fixed or Auto"));
}
