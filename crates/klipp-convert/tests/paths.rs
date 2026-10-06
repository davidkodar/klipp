//! NodePath resolution against a converted project, and the drain's rule:
//! every occurrence of a document node in the sequence's render copies.

use klipp_convert::lift::ClipKind;
use klipp_convert::paths::{occurrences, resolve, Documents, Occurrence, PathError};
use klipp_convert::revision2;
use klipp_convert::splice::{splice, SpliceInputs, StageInputs, TrackInputs};
use klipp_graph_format::lifted::{types as lt, ClipPicture, Stage};
use klipp_graph_format::path::{NodePath, Root, TrackPart};
use serde_json::{json, Value};
use uuid::Uuid;

const SEQ: &str = "00000000-0000-0000-0000-000000000501";
const GROUP: &str = "00000000-0000-0000-0000-000000000601";
const TRACK: &str = "00000000-0000-0000-0000-000000000701";
const C1: &str = "00000000-0000-0000-0000-000000000c01";
const C2: &str = "00000000-0000-0000-0000-000000000c02";

fn u(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

/// A Source -> Exposure -> Output grade whose node ids carry `tag`.
fn grade(tag: u8) -> Value {
    let n = |i: u8| format!("00000000-0000-0000-0000-0000000{tag:02x}{i:03}");
    json!({
        "nodes": [
            {"id": n(1), "kind": "Source", "label": "Source", "enabled": true},
            {"id": n(2), "kind": "Exposure", "label": "E", "enabled": true, "params": {"exposure": 0.25}},
            {"id": n(3), "kind": "Output", "label": "Output", "enabled": true}
        ],
        "edges": [
            {"from_node": n(1), "from_port": 0, "to_node": n(2), "to_port": 0},
            {"from_node": n(2), "from_port": 0, "to_node": n(3), "to_port": 0}
        ],
        "output_node_id": n(3)
    })
}

fn exposure(tag: u8) -> Uuid {
    u(&format!("00000000-0000-0000-0000-0000000{tag:02x}002"))
}

fn clip(id: &str, tag: u8) -> Value {
    json!({"id": id, "kind": {"type": "media", "media_id": "00000000-0000-0000-0000-0000000000d1"}, "node_graph": grade(tag), "grade_group": GROUP})
}

fn project() -> Value {
    json!({"version": "1.0.0", "sequences": [{
        "id": SEQ, "name": "S", "timeline_graph": grade(1),
        "grade_groups": [{"id": GROUP, "name": "G", "pre": grade(2), "post": grade(3)}],
        "tracks": [{"id": TRACK, "name": "V1", "kind": "Video",
            "effects": {"video": [{"type": "gaussian_blur", "radius": 2.0}], "grade": grade(4)},
            "clips": [clip(C1, 5), clip(C2, 6)]}]
    }]})
}

fn docs() -> Documents {
    let (converted, _) = revision2::project(&project()).unwrap();
    Documents::of(&converted).unwrap()
}

/// Each clip's render copy, spliced with its group's stages and its
/// track's grade and effects.
fn copies(d: &Documents) -> Vec<(Uuid, ClipPicture)> {
    let inputs = SpliceInputs {
        stages: Some(StageInputs { group: u(GROUP), pre: d.stages.get(&(u(GROUP), Stage::Pre)).cloned(), post: d.stages.get(&(u(GROUP), Stage::Post)).cloned() }),
        track: Some(TrackInputs { track: u(TRACK), effects: d.track_effects[&u(TRACK)].clone(), grade: d.track_grades.get(&u(TRACK)).cloned() }),
    };
    [C1, C2].iter().map(|c| (u(c), splice(&d.clips[&u(c)], ClipKind::Media, &inputs))).collect()
}

fn group_id(d: &Documents, clip: &str) -> Uuid {
    d.clips[&u(clip)].graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().id
}

#[test]
fn paths_resolve_to_their_document_node() {
    let d = docs();
    // A clip's own grade node: the clip's Grade group, then the node.
    let p = NodePath::new(Root::Clip { clip: u(C1) }, [group_id(&d, C1), exposure(5)]);
    assert_eq!(resolve(&d, &p).unwrap().id, exposure(5));
    // A stage's, a timeline grade's and a track grade's node: straight in
    // their body.
    for (root, tag) in [
        (Root::GroupStage { group: u(GROUP), stage: Stage::Pre }, 2),
        (Root::GroupStage { group: u(GROUP), stage: Stage::Post }, 3),
        (Root::TimelineGrade { sequence: u(SEQ) }, 1),
        (Root::Track { track: u(TRACK), part: TrackPart::Grade }, 4),
    ] {
        assert_eq!(resolve(&d, &NodePath::new(root, [exposure(tag)])).unwrap().id, exposure(tag), "{root:?}");
    }
    // A track effect.
    let fx = d.track_effects[&u(TRACK)][0].id;
    assert_eq!(resolve(&d, &NodePath::new(Root::Track { track: u(TRACK), part: TrackPart::Effects }, [fx])).unwrap().id, fx);
    // Wrong paths say why.
    assert_eq!(resolve(&d, &NodePath::new(Root::Clip { clip: Uuid::nil() }, [fx])), Err(PathError::NoRoot));
    assert_eq!(resolve(&d, &NodePath::new(Root::Clip { clip: u(C1) }, [])), Err(PathError::Empty));
    assert_eq!(resolve(&d, &NodePath::new(Root::Clip { clip: u(C1) }, [exposure(9)])), Err(PathError::NoNode(exposure(9))));
    let src = d.clips[&u(C1)].graph.nodes[0].id;
    assert_eq!(resolve(&d, &NodePath::new(Root::Clip { clip: u(C1) }, [src, exposure(5)])), Err(PathError::NotAGroup(src)));
}

#[test]
fn a_change_reaches_every_occurrence_and_no_other() {
    let d = docs();
    let cs = copies(&d);
    let refs: Vec<(Uuid, &ClipPicture)> = cs.iter().map(|(c, p)| (*c, p)).collect();
    let in_each = |p: &NodePath| occurrences(p, &refs).into_iter().map(|o| o.clip).collect::<Vec<_>>();

    // A clip's own node: that clip only, never a stage node of the same id.
    let own = NodePath::new(Root::Clip { clip: u(C1) }, [group_id(&d, C1), exposure(5)]);
    assert_eq!(occurrences(&own, &refs), vec![Occurrence { clip: u(C1), nodes: own.nodes.clone() }]);

    // A group stage's node: in every member, inside the nested stage.
    for stage in [Stage::Pre, Stage::Post] {
        let tag = if stage == Stage::Pre { 2 } else { 3 };
        let p = NodePath::new(Root::GroupStage { group: u(GROUP), stage }, [exposure(tag)]);
        let found = occurrences(&p, &refs);
        assert_eq!(found.iter().map(|o| o.clip).collect::<Vec<_>>(), [u(C1), u(C2)]);
        for o in &found {
            assert_eq!(o.nodes.len(), 2, "the nested stage's id, then the node");
            assert_eq!(o.nodes[1], exposure(tag));
        }
    }
    // A track's grade node and its effect: in each of its clips.
    assert_eq!(in_each(&NodePath::new(Root::Track { track: u(TRACK), part: TrackPart::Grade }, [exposure(4)])), [u(C1), u(C2)]);
    let fx = d.track_effects[&u(TRACK)][0].id;
    assert_eq!(in_each(&NodePath::new(Root::Track { track: u(TRACK), part: TrackPart::Effects }, [fx])), [u(C1), u(C2)]);
    // The timeline grade runs on the sequence: no clip copy holds it.
    assert!(occurrences(&NodePath::new(Root::TimelineGrade { sequence: u(SEQ) }, [exposure(1)]), &refs).is_empty());
    // A node that isn't there is found nowhere.
    assert!(occurrences(&NodePath::new(Root::GroupStage { group: u(GROUP), stage: Stage::Pre }, [exposure(9)]), &refs).is_empty());
}

/// Every occurrence found resolves, in its clip's copy, to a node with the
/// document node's id and value: the drain writes the same node everywhere.
#[test]
fn occurrences_are_the_document_node() {
    let d = docs();
    let cs = copies(&d);
    let refs: Vec<(Uuid, &ClipPicture)> = cs.iter().map(|(c, p)| (*c, p)).collect();
    let p = NodePath::new(Root::GroupStage { group: u(GROUP), stage: Stage::Post }, [exposure(3)]);
    let doc = resolve(&d, &p).unwrap();
    for o in occurrences(&p, &refs) {
        let copy = &cs.iter().find(|(c, _)| *c == o.clip).unwrap().1;
        let mut nodes = &copy.graph.nodes;
        for step in &o.nodes[..o.nodes.len() - 1] {
            nodes = &nodes.iter().find(|n| n.id == *step).unwrap().group.as_ref().unwrap().graph.nodes;
        }
        let n = nodes.iter().find(|n| Some(n.id) == o.nodes.last().copied()).unwrap();
        assert_eq!(n.legacy, doc.legacy);
    }
}
