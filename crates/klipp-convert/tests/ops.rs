//! Graph ops by path (#1083): each op, each refusal as a sentence, and
//! undo from the edit's `before` giving the original back.

use klipp_convert::ops::{apply, apply_with_catalog, apply_batch_with_catalog, undo, GraphOp};
use klipp_convert::paths::{resolve, Documents};
use klipp_convert::revision2;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root, TrackPart};
use klipp_graph_format::{types, Animation, Legacy, Node, NodeId, ParamKey, ParamValue};
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

fn legacy_project() -> Value {
    let grade = json!({
        "nodes": [{"id": S, "kind": "Source", "label": "S", "enabled": true}, {"id": E, "kind": "Exposure", "label": "E", "enabled": true, "params": {"exposure": 0.5}},
                  {"id": O, "kind": "Output", "label": "O", "enabled": true}],
        "edges": [{"from_node": S, "from_port": 0, "to_node": E, "to_port": 0}, {"from_node": E, "from_port": 0, "to_node": O, "to_port": 0}],
        "output_node_id": O
    });
    let blur = |r: f64| json!({"type": "gaussian_blur", "radius": r});
    json!({"version": "1.0.0", "sequences": [{"id": "00000000-0000-0000-0000-000000000501", "name": "S", "tracks": [{
        "id": TRACK, "name": "V1", "kind": "Video", "effects": {"video": [blur(1.0), blur(2.0)]},
        "clips": [{"id": CLIP, "kind": {"type": "media", "media_id": "00000000-0000-0000-0000-0000000000d1"}, "node_graph": grade, "builtin_effects": [blur(3.0)]}]
    }]}]})
}

fn docs() -> Documents {
    Documents::of(&revision2::project(&legacy_project()).unwrap().0).unwrap()
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
    // A loop: Exposure's picture back into itself.
    let r = apply(&mut d, &GraphOp::Connect { from: clip_path([g, u(E)]), from_port: "out".into(), to: clip_path([g, u(E)]), to_port: "in".into() });
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

/// Ops reach every root, not only a clip's graph: the timeline grade, a
/// group stage and a track's grade, each checked as a body. Undo restores
/// one root and leaves another root's edit alone (#1102 review).
#[test]
fn ops_reach_every_root_and_undo_keeps_to_its_own() {
    let grade = |tag: u8| {
        let n = |i: u8| format!("00000000-0000-0000-0000-0000000{tag:02x}{i:03}");
        json!({
            "nodes": [{"id": n(1), "kind": "Source", "label": "S", "enabled": true}, {"id": n(2), "kind": "Exposure", "label": "E", "enabled": true},
                      {"id": n(3), "kind": "Output", "label": "O", "enabled": true}],
            "edges": [{"from_node": n(1), "from_port": 0, "to_node": n(2), "to_port": 0}, {"from_node": n(2), "from_port": 0, "to_node": n(3), "to_port": 0}],
            "output_node_id": n(3)
        })
    };
    let ex = |tag: u8| u(&format!("00000000-0000-0000-0000-0000000{tag:02x}002"));
    const SEQ: &str = "00000000-0000-0000-0000-000000000501";
    const GROUP: &str = "00000000-0000-0000-0000-000000000601";
    let p = json!({"version": "1.0.0", "sequences": [{"id": SEQ, "name": "S", "timeline_graph": grade(1),
        "grade_groups": [{"id": GROUP, "name": "G", "pre": grade(2)}],
        "tracks": [{"id": TRACK, "name": "V1", "kind": "Video", "effects": {"grade": grade(3)},
            "clips": [{"id": CLIP, "kind": {"type": "media", "media_id": "00000000-0000-0000-0000-0000000000d1"}, "node_graph": grade(4), "grade_group": GROUP}]}]}]});
    let mut d = Documents::of(&revision2::project(&p).unwrap().0).unwrap();
    let start = serde_json::to_value((&d.timeline, d.stages.iter().collect::<Vec<_>>(), &d.track_grades)).unwrap();
    let timeline = NodePath::new(Root::TimelineGrade { sequence: u(SEQ) }, [ex(1)]);
    let stage = NodePath::new(Root::GroupStage { group: u(GROUP), stage: klipp_graph_format::lifted::Stage::Pre }, [ex(2)]);
    let track = NodePath::new(Root::Track { track: u(TRACK), part: TrackPart::Grade }, [ex(3)]);
    let a = apply(&mut d, &GraphOp::SetParam { path: timeline.clone(), key: "exposure".into(), value: ParamValue::Float(0.5) }).unwrap();
    let b = apply(&mut d, &GraphOp::AddNode { after: stage.clone(), node: grade_node(0x71, "Saturation") }).unwrap();
    apply(&mut d, &GraphOp::SetEnabled { path: track.clone(), enabled: false }).unwrap();
    assert_eq!(resolve(&d, &timeline).unwrap().legacy.as_ref().unwrap().payload["params"]["exposure"], 0.5);
    assert!(resolve(&d, &NodePath::new(stage.root, [Uuid::from_u128(0x71)])).is_ok());
    assert!(!resolve(&d, &track).unwrap().enabled);
    // A body's ends stay put at every root.
    assert!(apply(&mut d, &GraphOp::RemoveNode { path: NodePath::new(timeline.root, [u("00000000-0000-0000-0000-000000001003")]) }).is_err());
    // Undo the timeline's edit: the stage's and the track's stay.
    undo(&mut d, &a).unwrap();
    assert!(resolve(&d, &timeline).unwrap().legacy.as_ref().unwrap().payload["params"].get("exposure").is_none());
    assert!(resolve(&d, &NodePath::new(stage.root, [Uuid::from_u128(0x71)])).is_ok(), "another root's edit is untouched");
    undo(&mut d, &b).unwrap();
    apply(&mut d, &GraphOp::SetEnabled { path: track, enabled: true }).unwrap();
    assert_eq!(serde_json::to_value((&d.timeline, d.stages.iter().collect::<Vec<_>>(), &d.track_grades)).unwrap(), start, "every root as it was");
}

/// A track's effects take no end node and no group, refused whole
/// (#1102 re-review).
#[test]
fn a_track_list_refuses_an_end_and_a_group() {
    let mut d = docs();
    let before = snapshot(&d);
    let first = d.track_effects[&u(TRACK)][0].id;
    let tp = |id| NodePath::new(Root::Track { track: u(TRACK), part: TrackPart::Effects }, [id]);
    let mut end = (*grade_node(0x81, "Output")).clone();
    end.type_id = lt::OUTPUT.into();
    end.legacy = None;
    assert!(apply(&mut d, &GraphOp::AddNode { after: tp(first), node: Box::new(end) }).unwrap_err().contains("effect and plug-in entries"));
    let group = d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().clone();
    let mut group = group;
    group.id = Uuid::from_u128(0x82);
    assert!(apply(&mut d, &GraphOp::AddNode { after: tp(first), node: Box::new(group) }).unwrap_err().contains("effect and plug-in entries"));
    assert_eq!(snapshot(&d), before);
}

/// A move that would make a loop is refused whole: a key moved after the
/// correction its matte gates would feed itself (#1102 re-review).
#[test]
fn a_move_that_would_loop_is_refused() {
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
    let before = snapshot(&d);
    let g = group(&d);
    let r = apply(&mut d, &GraphOp::MoveAfter { path: clip_path([g, u(K)]), after: u(E) });
    assert!(r.unwrap_err().contains("Cycle"), "the key would read Exposure, whose mask it feeds");
    assert_eq!(snapshot(&d), before, "nothing moved");
}

/// Removing a key that gates a correction disconnects the matte and the
/// gate with it; the correction isn't handed the key's input as a matte
/// (#1102 re-review).
#[test]
fn removing_a_gating_key_ungates_the_correction() {
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
    apply(&mut d, &GraphOp::RemoveNode { path: clip_path([g, u(K)]) }).unwrap();
    let body = &d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
    assert!(!body.edges.iter().any(|e| e.to.0 == u(E) && e.to.1.as_str() == "mask"), "no matte, not the source as one");
    let e = body.nodes.iter().find(|n| n.id == u(E)).unwrap();
    assert!(!e.legacy_flags.as_ref().is_some_and(|f| f.alpha_one), "and no gate");
}


use klipp_timeline::RegistryCatalog;

const MATTE_ID: u128 = 0xe00;
const TARGET_ID: u128 = 0xe01;
fn publishing_docs() -> Documents {
    let mut d = docs();
    let group = group(&d);
    let b = d.clips.get_mut(&u(CLIP)).unwrap().graph.nodes.iter_mut().find(|n| n.id == group).unwrap().group.as_mut().unwrap();
    let source = b.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP_INPUT).unwrap().id;
    b.graph.nodes.push(*grade_node(MATTE_ID, "Qualifier"));
    b.graph.edges.push(klipp_graph_format::Edge { from: (source, "out".into()), to: (Uuid::from_u128(MATTE_ID), "in".into()) });
    let mut effect = grade_node(TARGET_ID, "Exposure");
    effect.type_id = "klipp.fx.gaussian_blur".into();
    effect.legacy = None;
    apply_with_catalog(&mut d, &GraphOp::AddNode { after: clip_path([group]), node: effect }, &RegistryCatalog).unwrap();
    d
}
fn publish(d: &Documents) -> GraphOp {
    GraphOp::PublishOutput {
        group: clip_path([group(d)]),
        endpoint: klipp_graph_format::PortBinding { node: Uuid::from_u128(MATTE_ID), port: "out".into() },
        key: "matte_selection".into(), label: "Selection".into(),
    }
}
fn connect_published(d: &Documents) -> GraphOp {
    GraphOp::Connect { from: clip_path([group(d)]), from_port: "matte_selection".into(), to: clip_path([Uuid::from_u128(TARGET_ID)]), to_port: "mask".into() }
}
fn channel(d: &Documents) -> &str {
    match resolve(d, &clip_path([Uuid::from_u128(TARGET_ID)])).unwrap().params.get(&"mask.channel".into()) {
        Some(ParamValue::Text(s)) => s,
        None => klipp_effects::registry::MASK_CHANNEL.migrated,
        _ => panic!("channel must be text"),
    }
}

#[test]
fn publish_and_connect_is_one_undo_and_new_masks_use_luminance() {
    let mut d = publishing_docs();
    let before = snapshot(&d);
    let ops = [publish(&d), connect_published(&d)];
    let edit = apply_batch_with_catalog(&mut d, &ops, &RegistryCatalog).unwrap();
    assert_eq!(channel(&d), klipp_effects::registry::MASK_CHANNEL.default);
    assert_eq!(channel(&d), "luminance");
    let body = resolve(&d, &clip_path([group(&d)])).unwrap().group.as_ref().unwrap();
    assert_eq!(body.interface[0].ty, klipp_graph_format::PortType::Matte);
    assert_eq!(body.interface[0].binding.node, Uuid::from_u128(MATTE_ID));
    assert_eq!(serde_json::to_value(body).unwrap()["requires"], json!(["group-interface-1"]));
    undo(&mut d, &edit).unwrap();
    assert_eq!(snapshot(&d), before);
    assert_eq!(channel(&d), "red");
    let redo = klipp_convert::ops::GraphEdit { root: edit.root, before: edit.after.clone(), after: edit.before.clone() };
    undo(&mut d, &redo).unwrap();
    assert_eq!(serde_json::to_value(&d.clips[&u(CLIP)]).unwrap(), edit.after);
}

#[test]
fn publication_and_batch_refusals_leave_the_whole_root_unchanged() {
    let mut d = publishing_docs();
    let before = snapshot(&d);
    let mut not_group = publish(&d);
    if let GraphOp::PublishOutput { group, .. } = &mut not_group { *group = clip_path([Uuid::from_u128(TARGET_ID)]); }
    assert!(apply_with_catalog(&mut d, &not_group, &RegistryCatalog).is_err());
    let missing = GraphOp::Unpublish { group: clip_path([group(&d)]), key: "missing".into() };
    assert!(apply_with_catalog(&mut d, &missing, &RegistryCatalog).is_err());
    assert_eq!(snapshot(&d), before);
    let mut bad = publish(&d);
    if let GraphOp::PublishOutput { endpoint, .. } = &mut bad { endpoint.node = Uuid::nil(); }
    assert!(apply_with_catalog(&mut d, &bad, &RegistryCatalog).is_err());
    assert_eq!(snapshot(&d), before);
    let mut wrong_direction = publish(&d);
    if let GraphOp::PublishOutput { endpoint, .. } = &mut wrong_direction { endpoint.port = "in".into(); }
    assert!(apply_with_catalog(&mut d, &wrong_direction, &RegistryCatalog).is_err());
    assert_eq!(snapshot(&d), before);
    for key in ["in", "out", "frame", ""] {
        let mut bad = publish(&d);
        if let GraphOp::PublishOutput { key: k, .. } = &mut bad { *k = key.into(); }
        assert!(apply_with_catalog(&mut d, &bad, &RegistryCatalog).is_err());
        assert_eq!(snapshot(&d), before);
    }
    let duplicates = [publish(&d), publish(&d)];
    assert!(apply_batch_with_catalog(&mut d, &duplicates, &RegistryCatalog).is_err());
    assert_eq!(snapshot(&d), before);
    let mut bad_connect = connect_published(&d);
    if let GraphOp::Connect { from_port, .. } = &mut bad_connect { *from_port = "missing".into(); }
    let ops = [publish(&d), bad_connect];
    assert!(apply_batch_with_catalog(&mut d, &ops, &RegistryCatalog).is_err());
    assert_eq!(snapshot(&d), before, "publication must roll back too");
}

#[test]
fn unpublish_refuses_readers_and_disconnect_unpublish_undo_together() {
    let mut d = publishing_docs();
    let ops = [publish(&d), connect_published(&d)];
    apply_batch_with_catalog(&mut d, &ops, &RegistryCatalog).unwrap();
    let before = snapshot(&d);
    let unpublish = GraphOp::Unpublish { group: clip_path([group(&d)]), key: "matte_selection".into() };
    assert!(apply_with_catalog(&mut d, &unpublish, &RegistryCatalog).is_err());
    assert_eq!(snapshot(&d), before);
    let edit = apply_batch_with_catalog(&mut d, &[GraphOp::Disconnect { to: clip_path([Uuid::from_u128(TARGET_ID)]), port: "mask".into() }, unpublish], &RegistryCatalog).unwrap();
    assert!(resolve(&d, &clip_path([group(&d)])).unwrap().group.as_ref().unwrap().interface.is_empty());
    undo(&mut d, &edit).unwrap();
    assert_eq!(snapshot(&d), before);
}

#[test]
fn channel_edits_clear_to_red_and_undo_exactly() {
    let mut d = publishing_docs();
    let path = clip_path([Uuid::from_u128(TARGET_ID)]);
    for value in [Some("alpha"), Some("luminance"), None] {
        let before = snapshot(&d);
        let edit = apply_with_catalog(&mut d, &GraphOp::SetMaskChannel { path: path.clone(), channel: value.map(str::to_owned) }, &RegistryCatalog).unwrap();
        assert_eq!(channel(&d), value.unwrap_or("red"));
        undo(&mut d, &edit).unwrap();
        assert_eq!(snapshot(&d), before);
    }
    let before = snapshot(&d);
    assert!(apply_with_catalog(&mut d, &GraphOp::SetMaskChannel { path, channel: Some("green".into()) }, &RegistryCatalog).is_err());
    assert_eq!(snapshot(&d), before);
}

#[test]
fn mask_connections_preserve_saved_channels_and_existing_red_semantics() {
    for wireless in [false, true] {
        for saved in [None, Some("alpha")] {
            let mut d = publishing_docs();
            let group_id = group(&d);
            let target = Uuid::from_u128(TARGET_ID);
            let ops = [publish(&d), connect_published(&d)];
            apply_batch_with_catalog(&mut d, &ops, &RegistryCatalog).unwrap();
            let graph = &mut d.clips.get_mut(&u(CLIP)).unwrap().graph;
            let node = graph.nodes.iter_mut().find(|n| n.id == target).unwrap();
            node.params.remove(&"mask.channel".into());
            if let Some(channel) = saved { node.params.insert("mask.channel".into(), ParamValue::Text(channel.into())); }
            if wireless {
                node.wireless.input_links.insert("mask".into(), "selection".into());
                graph.nodes.iter_mut().find(|n| n.id == group_id).unwrap().wireless.output_link = Some("selection".into());
                graph.edges.retain(|e| e.to != (target, "mask".into()));
            }
            let before = snapshot(&d);
            let edit = apply_with_catalog(&mut d, &GraphOp::Connect { from: clip_path([group_id]), from_port: "out".into(), to: clip_path([target]), to_port: "mask".into() }, &RegistryCatalog).unwrap();
            assert_eq!(channel(&d), saved.unwrap_or("red"));
            assert_eq!(resolve(&d, &clip_path([target])).unwrap().params.contains_key(&"mask.channel".into()), saved.is_some());
            undo(&mut d, &edit).unwrap();
            assert_eq!(snapshot(&d), before);
        }
    }
}

#[test]
fn channel_set_before_connect_survives_connect_and_reconnect() {
    let mut d = publishing_docs();
    let path = clip_path([Uuid::from_u128(TARGET_ID)]);
    apply_with_catalog(&mut d, &GraphOp::SetMaskChannel { path: path.clone(), channel: Some("alpha".into()) }, &RegistryCatalog).unwrap();
    let ops = [publish(&d), connect_published(&d)];
    apply_batch_with_catalog(&mut d, &ops, &RegistryCatalog).unwrap();
    assert_eq!(channel(&d), "alpha");
    apply_with_catalog(&mut d, &GraphOp::Disconnect { to: path, port: "mask".into() }, &RegistryCatalog).unwrap();
    let op = connect_published(&d);
    apply_with_catalog(&mut d, &op, &RegistryCatalog).unwrap();
    assert_eq!(channel(&d), "alpha");
}

#[test]
fn nonfinite_mix_and_nonmixable_nodes_refuse_without_landing() {
    let mut d = publishing_docs();
    let path = clip_path([Uuid::from_u128(TARGET_ID)]);
    for mix in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let before = snapshot(&d);
        assert!(apply_with_catalog(&mut d, &GraphOp::SetMix { path: path.clone(), mix: Some(mix) }, &RegistryCatalog).is_err());
        assert_eq!(snapshot(&d), before);
    }
    let before = snapshot(&d);
    let edit = apply_with_catalog(&mut d, &GraphOp::SetMix { path: path.clone(), mix: Some(0.5) }, &RegistryCatalog).unwrap();
    undo(&mut d, &edit).unwrap();
    assert_eq!(snapshot(&d), before);
    // Invert owns its own blend control, not universal mask/mix.
    d.clips.get_mut(&u(CLIP)).unwrap().graph.nodes.iter_mut().find(|n| n.id == Uuid::from_u128(TARGET_ID)).unwrap().type_id = "klipp.fx.invert".into();
    let before = snapshot(&d);
    for op in [
        GraphOp::SetMix { path: path.clone(), mix: Some(0.5) },
        GraphOp::SetMaskChannel { path: path.clone(), channel: Some("red".into()) },
        GraphOp::Connect { from: clip_path([group(&d)]), from_port: "out".into(), to: path, to_port: "mask".into() },
    ] {
        assert!(apply_with_catalog(&mut d, &op, &RegistryCatalog).is_err());
        assert_eq!(snapshot(&d), before);
    }
}


fn migrated_grade_docs() -> Documents {
    let mut project = legacy_project();
    let grade = &mut project["sequences"][0]["tracks"][0]["clips"][0]["node_graph"];
    let matte = Uuid::from_u128(MATTE_ID);
    grade["nodes"].as_array_mut().unwrap().push(json!({"id": matte, "kind": "Qualifier", "enabled": true, "params": {}}));
    grade["edges"].as_array_mut().unwrap().extend([
        json!({"from_node": S, "from_port": 0, "to_node": matte, "to_port": 0}),
        json!({"from_node": matte, "from_port": 0, "to_node": E, "to_port": 1}),
    ]);
    Documents::of(&revision2::project(&project).unwrap().0).unwrap()
}

#[test]
fn lifted_grade_masks_stay_red_and_refuse_a_channel_setting() {
    let mut d = migrated_grade_docs();
    let g = group(&d);
    let matte = Uuid::from_u128(MATTE_ID);
    let path = clip_path([g, u(E)]);
    let n = resolve(&d, &path).unwrap();
    assert!(!n.params.contains_key(&"mask.channel".into()));
    assert_eq!(klipp_effects::registry::MASK_CHANNEL.migrated, "red");
    d.clips[&u(CLIP)].graph.check_lifted_with_catalog(false, &RegistryCatalog).unwrap();
    let before = snapshot(&d);
    let error = apply_with_catalog(&mut d, &GraphOp::SetMaskChannel { path: path.clone(), channel: Some("alpha".into()) }, &RegistryCatalog).unwrap_err();
    assert!(error.contains("reads red"), "{error}");
    assert_eq!(snapshot(&d), before);
    // Reconnecting a grade mask remains red-only, even though effects
    // with the channel setting start a new connection at luminance.
    let flags = resolve(&d, &path).unwrap().legacy_flags.clone();
    let edit = apply_with_catalog(&mut d, &GraphOp::Connect { from: clip_path([g, matte]), from_port: "out".into(), to: path.clone(), to_port: "mask".into() }, &RegistryCatalog).unwrap();
    assert!(!resolve(&d, &path).unwrap().params.contains_key(&"mask.channel".into()));
    assert_eq!(resolve(&d, &path).unwrap().legacy_flags, flags, "rewiring a migrated mask preserves every flag");
    undo(&mut d, &edit).unwrap();
    assert_eq!(snapshot(&d), before);
}


#[test]
fn a_new_grade_gate_records_legacy_alpha_and_undo_restores_it() {
    let mut d = publishing_docs();
    let g = group(&d);
    let path = clip_path([g, u(E)]);
    assert!(resolve(&d, &path).unwrap().legacy_flags.as_ref().is_none_or(|f| !f.alpha_one));
    let before = snapshot(&d);
    let edit = apply_with_catalog(&mut d, &GraphOp::Connect { from: clip_path([g, Uuid::from_u128(MATTE_ID)]), from_port: "out".into(), to: path.clone(), to_port: "mask".into() }, &RegistryCatalog).unwrap();
    let node = resolve(&d, &path).unwrap();
    assert!(node.legacy_flags.as_ref().unwrap().alpha_one);
    let migrated = migrated_grade_docs();
    assert_eq!(node.legacy_flags, resolve(&migrated, &clip_path([group(&migrated), u(E)])).unwrap().legacy_flags);
    assert!(!node.params.contains_key(&"mask.channel".into()));
    let gated = snapshot(&d);
    let disconnect = apply_with_catalog(&mut d, &GraphOp::Disconnect { to: path.clone(), port: "mask".into() }, &RegistryCatalog).unwrap();
    assert!(resolve(&d, &path).unwrap().legacy_flags.as_ref().is_none_or(|f| !f.alpha_one));
    undo(&mut d, &disconnect).unwrap();
    assert_eq!(snapshot(&d), gated);
    undo(&mut d, &edit).unwrap();
    assert_eq!(snapshot(&d), before);
}


#[test]
fn a_batch_refuses_empty_or_multiple_roots_without_any_edit() {
    let mut d = publishing_docs();
    let before = snapshot(&d);
    assert!(apply_batch_with_catalog(&mut d, &[], &RegistryCatalog).is_err());
    let track_node = d.track_effects[&u(TRACK)][0].id;
    let ops = [
        GraphOp::SetEnabled { path: clip_path([Uuid::from_u128(TARGET_ID)]), enabled: false },
        GraphOp::SetEnabled { path: NodePath::new(Root::Track { track: u(TRACK), part: TrackPart::Effects }, [track_node]), enabled: false },
    ];
    assert!(apply_batch_with_catalog(&mut d, &ops, &RegistryCatalog).is_err());
    assert_eq!(snapshot(&d), before);
}

/// A key as the renderer reads it.
fn key_at(t: f64, v: f64) -> Value {
    json!({"time_secs": t, "value": v, "interpolation": "Linear", "tangent_out": [0.0, 0.0], "tangent_in": [0.0, 0.0]})
}

fn times(d: &Documents, fx: NodeId, key: &str) -> Vec<f64> {
    let n = d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == fx).unwrap();
    n.anim.get(&ParamKey::from(key)).map(|a| a.keys.iter().map(|k| k["time_secs"].as_f64().unwrap()).collect()).unwrap_or_default()
}

/// A grade node's label lives in its stored entry and its place in the
/// grade body's layout, so both come back out as the file writes them;
/// the picture itself is untouched.
#[test]
fn node_meta_names_and_places_a_node_and_round_trips() {
    let mut d = docs();
    let g = group(&d);
    let canon_before = klipp_convert::canon::canon(klipp_convert::canon::Store::Clip, &serde_json::to_value(&d.clips[&u(CLIP)]).unwrap());
    let edit = apply(&mut d, &GraphOp::SetNodeMeta { path: clip_path([g, u(E)]), label: Some("Lift".into()), position: Some(json!({"x": 120.0, "y": 40.0})) }).unwrap();
    let record = klipp_convert::delift::clip(&d.clips[&u(CLIP)]).unwrap();
    let v1 = klipp_convert::to_v1::clip(&serde_json::to_value(&record).unwrap()).unwrap();
    let grade = &v1["node_graph"];
    let node = grade["nodes"].as_array().unwrap().iter().find(|n| n["id"] == E).unwrap();
    assert_eq!(node["label"], "Lift");
    assert_eq!(grade["node_positions"][E], json!({"x": 120.0, "y": 40.0}));
    let canon_after = klipp_convert::canon::canon(klipp_convert::canon::Store::Clip, &serde_json::to_value(&d.clips[&u(CLIP)]).unwrap());
    assert_eq!(canon_after, canon_before, "a name and a place change no picture");
    undo(&mut d, &edit).unwrap();
    assert_eq!(serde_json::to_value(&d.clips[&u(CLIP)]).unwrap(), edit.before);

    // An effect node's label is its own; a track's list has no layout.
    let fx = effect(&d);
    apply(&mut d, &GraphOp::SetNodeMeta { path: clip_path([fx]), label: Some("Soft".into()), position: None }).unwrap();
    assert_eq!(d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == fx).unwrap().label, "Soft");
    let first = d.track_effects[&u(TRACK)][0].id;
    let on_track = NodePath::new(Root::Track { track: u(TRACK), part: TrackPart::Effects }, [first]);
    let before = snapshot(&d);
    assert!(apply(&mut d, &GraphOp::SetNodeMeta { path: on_track.clone(), label: None, position: Some(json!({"x": 0.0, "y": 0.0})) }).is_err());
    assert_eq!(snapshot(&d), before);
    apply(&mut d, &GraphOp::SetNodeMeta { path: on_track, label: Some("Edge".into()), position: None }).unwrap();
    assert_eq!(d.track_effects[&u(TRACK)][0].label, "Edge");
}

/// Keys go in time order, one per time; taking the last key off leaves
/// the parameter unanimated; a whole animation is set and cleared.
#[test]
fn key_ops_edit_a_parameters_animation() {
    let mut d = docs();
    let fx = effect(&d);
    let p = clip_path([fx]);
    for (t, v) in [(1.0, 4.0), (0.0, 3.0), (2.0, 6.0), (1.0, 5.0)] {
        apply(&mut d, &GraphOp::AddKeyAt { path: p.clone(), key: "radius".into(), keyframe: key_at(t, v) }).unwrap();
    }
    assert_eq!(times(&d, fx, "radius"), [0.0, 1.0, 2.0]);
    let n = d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == fx).unwrap();
    assert_eq!(n.anim[&ParamKey::from("radius")].keys[1]["value"], 5.0, "a key at the same time replaces it");

    let before = snapshot(&d);
    assert!(apply(&mut d, &GraphOp::RemoveKeyAt { path: p.clone(), key: "radius".into(), time_secs: 0.5 }).is_err(), "no key there");
    assert!(apply(&mut d, &GraphOp::AddKeyAt { path: p.clone(), key: "radius".into(), keyframe: json!({"time_secs": "soon", "value": 1.0}) }).is_err());
    assert_eq!(snapshot(&d), before, "a refused key op changes nothing");

    for t in [0.0, 1.0, 2.0] {
        apply(&mut d, &GraphOp::RemoveKeyAt { path: p.clone(), key: "radius".into(), time_secs: t }).unwrap();
    }
    assert!(!d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == fx).unwrap().anim.contains_key(&ParamKey::from("radius")));

    let wiggle = json!({"modifiers": [{"type": "wiggle", "amplitude": 0.7, "frequency": 3.0, "blend": "Multiply"}]});
    let edit = apply(&mut d, &GraphOp::SetAnim { path: p.clone(), key: "radius".into(), anim: Some(Animation { keys: vec![key_at(0.0, 2.0)], modifiers: Some(wiggle) }) }).unwrap();
    assert_eq!(times(&d, fx, "radius"), [0.0]);
    apply(&mut d, &GraphOp::SetAnim { path: p.clone(), key: "radius".into(), anim: None }).unwrap();
    assert!(times(&d, fx, "radius").is_empty());
    undo(&mut d, &edit).unwrap();
    assert_eq!(serde_json::to_value(&d.clips[&u(CLIP)]).unwrap(), edit.before);

    // On a track's list too.
    let first = d.track_effects[&u(TRACK)][0].id;
    let on_track = NodePath::new(Root::Track { track: u(TRACK), part: TrackPart::Effects }, [first]);
    apply(&mut d, &GraphOp::AddKeyAt { path: on_track, key: "radius".into(), keyframe: key_at(0.0, 1.0) }).unwrap();
    assert_eq!(d.track_effects[&u(TRACK)][0].anim[&ParamKey::from("radius")].keys.len(), 1);
}

/// A float lands as a file writes it, the shortest decimal for its f32,
/// so an op stores what a save and a reload would.
#[test]
fn a_float_param_is_stored_as_the_file_writes_it() {
    let mut d = docs();
    let g = group(&d);
    apply(&mut d, &GraphOp::SetParam { path: clip_path([g, u(E)]), key: "exposure".into(), value: ParamValue::Float(0.065) }).unwrap();
    let body = &d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == g).unwrap().group.as_ref().unwrap().graph;
    let e = body.nodes.iter().find(|n| n.id == u(E)).unwrap();
    let stored = &e.legacy.as_ref().unwrap().payload["params"]["exposure"];
    assert_eq!(stored.to_string(), "0.065");
    assert_eq!(serde_json::to_string(&0.065f32).unwrap(), "0.065", "what a save writes");
}

/// A payload is replaced whole, keeping its kind and reading as the entry
/// it replaces; anything else is refused and changes nothing.
#[test]
fn set_payload_replaces_an_entry_of_the_same_kind() {
    let mut d = docs();
    let fx = effect(&d);
    let p = clip_path([fx]);
    let edit = apply(&mut d, &GraphOp::SetPayload { path: p.clone(), payload: json!({"type": "gaussian_blur", "radius": 7.0}) }).unwrap();
    let payload = |d: &Documents| d.clips[&u(CLIP)].graph.nodes.iter().find(|n| n.id == fx).unwrap().legacy.as_ref().unwrap().payload.clone();
    assert_eq!(payload(&d)["radius"], 7.0);
    let before = snapshot(&d);
    assert!(apply(&mut d, &GraphOp::SetPayload { path: p.clone(), payload: json!({"type": "sharpen", "amount": 0.5}) }).is_err(), "another kind");
    assert!(apply(&mut d, &GraphOp::SetPayload { path: p.clone(), payload: json!({"type": "gaussian_blur", "radius": "wide"}) }).is_err(), "unreadable");
    assert_eq!(snapshot(&d), before, "a refused payload changes nothing");
    undo(&mut d, &edit).unwrap();
    assert_eq!(payload(&d)["radius"], 3.0);

    // A grade node keeps its kind too.
    let g = group(&d);
    apply(&mut d, &GraphOp::SetPayload { path: clip_path([g, u(E)]), payload: json!({"kind": "Exposure", "label": "E", "params": {"exposure": -0.5}}) }).unwrap();
    assert!(apply(&mut d, &GraphOp::SetPayload { path: clip_path([g, u(E)]), payload: json!({"kind": "Contrast", "label": "E", "params": {}}) }).is_err());
}
