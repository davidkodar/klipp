//! The splice over lifted clips: its two properties, its order, the
//! stages' ids, the clip's own grade, and each of today's skips decided by
//! today's own code (`GradeGroup::wrap`'s prepend and append,
//! `NodeGraph::is_identity`).

use klipp_convert::lift::{lift_clip, ClipKind, LiftCtx};
use klipp_convert::splice::{own_grade, splice, stage_body, track_parts, unsplice, SpliceInputs, StageInputs, TrackInputs};
use klipp_convert::to_v2;
use klipp_graph_format::lifted::{types as lt, ClipPicture, Origin, Stage};
use klipp_graph_format::Graph;
use serde_json::{json, Value};
use uuid::Uuid;

const CLIP: &str = "00000000-0000-0000-0000-00000000c001";
const TRACK: &str = "00000000-0000-0000-0000-00000000a001";
const GROUP: &str = "00000000-0000-0000-0000-00000000b001";

fn id(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

fn obj(v: &Value) -> &serde_json::Map<String, Value> {
    v.as_object().unwrap()
}

/// A Source -> one node -> Output grade graph whose node ids carry `tag`.
fn grade(tag: u8, kind: &str, params: Value) -> Value {
    let n = |i: u8| format!("00000000-0000-0000-0000-0000000000{tag:x}{i}");
    json!({
        "nodes": [
            {"id": n(1), "kind": "Source", "label": "Source", "enabled": true},
            {"id": n(2), "kind": kind, "label": kind, "enabled": true, "params": params},
            {"id": n(3), "kind": "Output", "label": "Output", "enabled": true}
        ],
        "edges": [
            {"from_node": n(1), "from_port": 0, "to_node": n(2), "to_port": 0},
            {"from_node": n(2), "from_port": 0, "to_node": n(3), "to_port": 0}
        ],
        "output_node_id": n(3)
    })
}

fn lifted(clip: &Value, kind: ClipKind) -> ClipPicture {
    let record = to_v2::clip_to_v2(obj(clip)).unwrap();
    lift_clip(&record, &LiftCtx { clip_id: id(CLIP), kind }).unwrap().0
}

/// Whether today's `wrap` would add `stage` to `base`, decided by its own
/// code: an identity stage is skipped, and a prepend or append that fails
/// (a working space that doesn't match, no source) or adds nothing leaves
/// the clip's graph as it was.
fn applies(base: &Value, stage: &Value, prepend: bool) -> bool {
    let base: klipp_nodes::NodeGraph = serde_json::from_value(base.clone()).unwrap();
    let stage: klipp_nodes::NodeGraph = serde_json::from_value(stage.clone()).unwrap();
    if stage.is_identity() {
        return false;
    }
    let mut g = base.clone();
    let r = if prepend { g.prepend_keeping_ids(&stage) } else { g.append_keeping_ids(&stage) };
    r.is_ok() && g != base
}

/// The splice inputs for a clip in `group` (pre, post) on a track with
/// `track_effects` and `track_grade`, with today's skips applied.
fn inputs(clip_grade: &Value, pre: Option<Value>, post: Option<Value>, track_effects: Value, track_grade: Option<Value>) -> SpliceInputs {
    let stage = |g: &Option<Value>, prepend: bool, which: &str| {
        g.as_ref().filter(|g| applies(clip_grade, g, prepend)).map(|g| {
            let mut group = json!({"id": GROUP, "name": "G"});
            group[which] = g.clone();
            let (pre, post) = to_v2::stages_to_v2(obj(&group)).unwrap();
            stage_body(&if which == "pre" { pre } else { post }.unwrap()).unwrap()
        })
    };
    let pre_body = stage(&pre, true, "pre");
    let post_body = stage(&post, false, "post");
    let mut effects = json!({"video": track_effects});
    if let Some(g) = track_grade.as_ref().filter(|g| applies(clip_grade, g, false)) {
        effects["grade"] = g.clone();
    }
    let structural = to_v2::track_to_v2(obj(&json!({"id": TRACK, "effects": effects}))).unwrap();
    let (fx, grade) = track_parts(&structural).unwrap();
    SpliceInputs {
        stages: Some(StageInputs { group: id(GROUP), pre: pre_body, post: post_body }),
        track: Some(TrackInputs { track: id(TRACK), effects: fx, grade }),
    }
}

fn types_of(g: &Graph) -> Vec<&str> {
    g.nodes.iter().map(|n| n.type_id.as_str()).collect()
}

fn clip_with(grade_v: &Value) -> Value {
    json!({"id": CLIP, "node_graph": grade_v, "builtin_effects": [{"type": "gaussian_blur", "radius": 2.0}]})
}

#[test]
fn splice_and_unsplice_invert_and_splicing_twice_is_once() {
    let own = grade(1, "Corrector", json!({"exposure": 0.2}));
    let c = lifted(&clip_with(&own), ClipKind::Media);
    let i = inputs(&own, Some(grade(2, "Saturation", json!({"saturation": 1.3}))), Some(grade(3, "Contrast", json!({"contrast": 1.2}))),
                   json!([{"type": "drop_shadow"}]), Some(grade(4, "Exposure", json!({"exposure": -0.1}))));
    let s = splice(&c, ClipKind::Media, &i);
    assert_eq!(unsplice(&s), c, "un-splicing gives the clip back");
    assert_eq!(splice(&s, ClipKind::Media, &i), s, "splicing a spliced copy changes nothing");
    assert_eq!(splice(&c, ClipKind::Media, &SpliceInputs::default()), c, "nothing to splice: the clip itself");
}

#[test]
fn spliced_pieces_go_where_the_bake_puts_them() {
    let own = grade(1, "Corrector", json!({"exposure": 0.2}));
    let c = lifted(&clip_with(&own), ClipKind::Media);
    let i = inputs(&own, Some(grade(2, "Saturation", json!({"saturation": 1.3}))), Some(grade(3, "Contrast", json!({"contrast": 1.2}))),
                   json!([{"type": "drop_shadow"}]), Some(grade(4, "Exposure", json!({"exposure": -0.1}))));
    let s = splice(&c, ClipKind::Media, &i);
    // pre, the clip's grade, post, the track's grade; the track's effects
    // after the clip's own, before the Auto placement that ends the chain.
    assert_eq!(
        types_of(&s.graph),
        [lt::SOURCE, lt::GROUP, lt::GROUP, lt::GROUP, lt::GROUP, lt::SCENE_TO_DISPLAY, "klipp.legacy.effect", "klipp.legacy.effect", lt::PLACEMENT, lt::OUTPUT]
    );
    let origins: Vec<Option<Origin>> = s.graph.nodes.iter().map(|n| n.origin).collect();
    assert_eq!(origins[1], Some(Origin::GroupStage { group: id(GROUP), stage: Stage::Pre }));
    assert_eq!(origins[2], None, "the clip's own grade");
    assert_eq!(origins[3], Some(Origin::GroupStage { group: id(GROUP), stage: Stage::Post }));
    assert_eq!(origins[4], Some(Origin::Track { track: id(TRACK) }));
    assert_eq!((origins[6], origins[7]), (None, Some(Origin::Track { track: id(TRACK) })));
    for (e, w) in s.graph.edges.iter().zip(s.graph.nodes.windows(2)) {
        assert_eq!((e.from.0, e.to.0), (w[0].id, w[1].id), "chained in order");
    }
}

/// The live-edit drain writes a stage node by its id in every member's
/// copy, so a stage's nodes keep their ids inside the nested group.
#[test]
fn stage_nodes_keep_their_ids() {
    let own = grade(1, "Corrector", json!({"exposure": 0.2}));
    let pre = grade(2, "Saturation", json!({"saturation": 1.3}));
    let c = lifted(&clip_with(&own), ClipKind::Media);
    let s = splice(&c, ClipKind::Media, &inputs(&own, Some(pre.clone()), None, json!([]), None));
    let nested = s.graph.nodes[1].group.as_ref().unwrap();
    let ids: Vec<String> = nested.graph.nodes.iter().map(|n| n.id.to_string()).collect();
    let saved: Vec<String> = pre["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap().to_owned()).collect();
    assert_eq!(ids, saved);
}

/// The clip's own grade is its own nodes, not a pre stage's: a stage's
/// Corrector, which today's prepend puts first, can't shadow the clip's.
#[test]
fn the_clips_own_grade_is_never_a_stages() {
    let own = grade(1, "Corrector", json!({"exposure": 0.2}));
    let c = lifted(&clip_with(&own), ClipKind::Media);
    let s = splice(&c, ClipKind::Media, &inputs(&own, Some(grade(2, "Corrector", json!({"exposure": -0.5}))), None, json!([]), None));
    let body = own_grade(&s).unwrap();
    let corrector = body.nodes.iter().find(|n| n.legacy.as_ref().is_some_and(|l| l.payload["kind"] == "Corrector")).unwrap();
    assert_eq!(corrector.id.to_string(), "00000000-0000-0000-0000-000000000012", "the clip's Corrector, not the stage's");
}

/// Each skip as today's `wrap` decides it, and the splice adding nothing.
#[test]
fn todays_skips_hold() {
    let own = grade(1, "Corrector", json!({"exposure": 0.2}));
    let c = lifted(&clip_with(&own), ClipKind::Media);
    // An identity stage and an identity track grade: skipped.
    let identity = grade(2, "Corrector", json!({}));
    let i = inputs(&own, Some(identity.clone()), Some(identity.clone()), json!([]), Some(identity));
    assert_eq!((i.stages.as_ref().unwrap().pre.as_ref(), i.stages.as_ref().unwrap().post.as_ref()), (None, None));
    assert!(i.track.as_ref().unwrap().grade.is_none());
    assert_eq!(splice(&c, ClipKind::Media, &i), c);
    // A stage whose working space differs from the clip's: today's
    // prepend refuses it, so it isn't spliced.
    let mut ours = own.clone();
    ours["working_space_override"] = json!("Rec709Linear");
    let mut theirs = grade(2, "Saturation", json!({"saturation": 1.3}));
    theirs["working_space_override"] = json!("AcesCG");
    let i = inputs(&ours, Some(theirs), None, json!([]), None);
    assert!(i.stages.unwrap().pre.is_none());
    // Gaps and adjustment layers take nothing of their track's.
    let i = inputs(&own, None, None, json!([{"type": "drop_shadow"}]), Some(grade(4, "Exposure", json!({"exposure": -0.1}))));
    for kind in [ClipKind::Gap, ClipKind::Adjustment] {
        let c = lifted(&clip_with(&own), kind);
        assert_eq!(splice(&c, kind, &i), c, "{kind:?}");
    }
}
