//! Inspection by path (M3 step 16a): `Sequence::grade_node_at` names the
//! node of a clip's grade, as its render copy runs it, that a path names:
//! in the clip's own grade group, in a stage of its grade group, in its
//! track's grade. Two of those holding a node of the same id (a grade
//! copied into a stage) are two nodes when they run, and each path finds
//! its own. A path that names nothing to inspect says why; it is never
//! taken for another node.

use klipp_core::{MediaId, RationalTime};
use klipp_graph_format::lifted::Stage;
use klipp_graph_format::path::{NodePath, Root, TrackPart};
use klipp_graph_lift::ids::{entry_id, RootKind};
use klipp_nodes::{graph::spliced_id, NodeGraph, NodeId};
use klipp_timeline::{Clip, GradeAccess, GradeGroup, GradeStage, Sequence, TrackKind};

fn t(secs: f64) -> RationalTime {
    RationalTime::from_secs_f64(secs, 24)
}

/// A grade that does something: a Corrector lifting exposure by `stops`,
/// and its id.
fn grade(stops: f32) -> (NodeGraph, NodeId) {
    let mut g = NodeGraph::default();
    let id = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == id).unwrap().params.exposure = Some(stops);
    (g, id)
}

/// The same grade under another id for its Corrector.
fn with_id(g: &NodeGraph, from: NodeId, to: NodeId) -> NodeGraph {
    let mut g = g.clone();
    g.nodes.iter_mut().filter(|n| n.id == from).for_each(|n| n.id = to);
    g.edges.iter_mut().for_each(|e| {
        if e.from_node == from {
            e.from_node = to;
        }
        if e.to_node == from {
            e.to_node = to;
        }
    });
    if let Some(p) = g.node_positions.remove(&from) {
        g.node_positions.insert(to, p);
    }
    g
}

/// One clip graded with Corrector `x`, in a group whose pre and post
/// stages and whose track's grade each hold a Corrector of the same id.
fn fixture() -> (Sequence, Clip, NodeId, GradeGroup) {
    let (own, x) = grade(0.5);
    let mut clip = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    clip.set_grade(own.clone());
    let mut group = GradeGroup::new("Night");
    // Each stage's own Corrector carries id `x`.
    let (pre, pre_x) = grade(0.25);
    group.stage_slot(GradeStage::Pre).set(Some(with_id(&pre, pre_x, x)));
    let (post, post_x) = grade(-0.5);
    group.stage_slot(GradeStage::Post).set(Some(with_id(&post, post_x, x)));
    clip.grade_group = Some(group.id);
    let mut seq = Sequence::new_default();
    seq.grade_groups.push(group.clone());
    let v1 = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
    let (track_grade, track_x) = grade(0.1);
    seq.tracks[v1].effects.grade_slot().set(Some(with_id(&track_grade, track_x, x)));
    seq.tracks[v1].clips.push(clip.clone());
    (seq, clip, x, group)
}

fn at(root: Root, nodes: &[NodeId]) -> NodePath {
    NodePath::new(root, nodes.iter().copied())
}

#[test]
fn same_local_id_in_two_groups_gives_two_results() {
    let (seq, clip, x, group) = fixture();
    let v1 = seq.tracks.iter().find(|t| t.clips.iter().any(|c| c.id == clip.id)).unwrap();
    let grade_group = entry_id(RootKind::Clip, clip.id.0, "grade", 0);
    let paths = [
        at(Root::Clip { clip: clip.id.0 }, &[grade_group, x]),
        at(Root::GroupStage { group: group.id.0, stage: Stage::Pre }, &[x]),
        at(Root::GroupStage { group: group.id.0, stage: Stage::Post }, &[x]),
        at(Root::Track { track: v1.id.0, part: TrackPart::Grade }, &[x]),
    ];
    let render = seq.render_grade(clip.id).unwrap().into_owned();
    let baked = seq.baked_for_render().into_owned();
    let copy = baked.all_clips().find(|c| c.id == clip.id).unwrap().grade().clone();
    let mut found = Vec::new();
    for path in &paths {
        // The same answer on the sequence and on its render copy.
        let id = seq.grade_node_at(clip.id, path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        assert_eq!(baked.grade_node_at(clip.id, path), Ok(id), "{path:?} on the render copy");
        assert!(render.nodes.iter().any(|n| n.id == id), "{path:?}: {id} runs in the preview's grade");
        assert!(copy.nodes.iter().any(|n| n.id == id), "{path:?}: {id} runs in the render copy's grade");
        found.push(id);
    }
    // Four nodes of one local id, four nodes when they run.
    let mut distinct = found.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 4, "{found:?}");
    assert_eq!(found[0], x, "the clip's own node keeps its id");
    assert_eq!(found[1], spliced_id(group.stage_clip_id(GradeStage::Pre).0, x));
    // And no two nodes of one id run, in either bake.
    for g in [&render, &copy] {
        let mut ids: Vec<_> = g.nodes.iter().map(|n| n.id).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "a node id twice in a running grade");
    }
}

#[test]
fn a_missing_path_is_unavailable_not_stale() {
    let (seq, clip, x, group) = fixture();
    let grade_group = entry_id(RootKind::Clip, clip.id.0, "grade", 0);
    let gone = NodeId::new_v4();
    for (path, why) in [
        (at(Root::Clip { clip: clip.id.0 }, &[grade_group, gone]), "isn't in the clip's grade"),
        (at(Root::GroupStage { group: group.id.0, stage: Stage::Pre }, &[gone]), "doesn't run in that stage"),
        (at(Root::Clip { clip: uuid::Uuid::new_v4() }, &[grade_group, x]), "another clip's picture"),
        (at(Root::Clip { clip: clip.id.0 }, &[grade_group]), "empty path"),
        (at(Root::TimelineGrade { sequence: seq.id.0 }, &[x]), "timeline grade"),
    ] {
        let e = seq.grade_node_at(clip.id, &path).expect_err(why);
        assert!(e.contains(why), "{path:?}: {e}");
    }
}

#[test]
fn a_node_inside_a_compound_is_unavailable() {
    let (seq, clip, x, _) = fixture();
    let grade_group = entry_id(RootKind::Clip, clip.id.0, "grade", 0);
    let e = seq.grade_node_at(clip.id, &at(Root::Clip { clip: clip.id.0 }, &[grade_group, x, NodeId::new_v4()])).unwrap_err();
    assert!(e.contains("compound"), "{e}");
}
