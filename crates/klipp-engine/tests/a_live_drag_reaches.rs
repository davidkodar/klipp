//! Which copies a live grade drag reaches in the render copy (#1080).
//!
//! While a slider is dragged, the engine's drain (`apply_param_changes`)
//! writes each change into the sequence it renders: the active sequence
//! flattened (`flatten_sequence`, as `sync_sequence_to_engine` makes it).
//! It finds the clip by id with `all_clips_mut`, the top-level clips of
//! that copy. An editorial compound's clips are flattened into it under
//! their own ids; a precomposition's scene lives inside its clip
//! (`Clip::precomp`), every clip renamed for the instance. So:
//!
//! - a clip of the active sequence: reached;
//! - a clip inside an editorial compound: its first copy only, since a
//!   scene laid in twice gives two clips with one id;
//! - a clip inside a precomposition: never, live; the end-of-drag
//!   re-flatten gives it whatever the document holds;
//! - a grade-group stage node: every member of a group of the active
//!   sequence; a group of a nested scene, never (the copy carries only the
//!   active sequence's groups);
//! - the timeline-grade node: the active sequence's.
//!
//! A track grade has no live drag: its nodes are edited by commands, which
//! sync the whole sequence.
//!
//! Through the UI only the first, fourth and fifth arise: the Colour page
//! drags the active sequence's own clips and stand-ins, and a nested
//! scene is graded by opening it. The phase-2a drain redesign has to keep
//! these answers, or change them on purpose.

use klipp_timeline::GradeAccess;
use std::sync::Arc;

use crossbeam_queue::ArrayQueue;
use klipp_core::{ClipId, MediaId, ParamChange, RationalTime};
use klipp_engine::engine::apply_param_changes;
use klipp_nodes::{NodeGraph, NodeId};
use klipp_timeline::flatten::flatten_sequence;
use klipp_timeline::{Clip, ClipKind, GradeGroup, GradeStage, Sequence, SharedSequence, Track, TrackKind};

fn t(secs: f64) -> RationalTime {
    RationalTime::from_secs_f64(secs, 24)
}

/// A graph whose Corrector exposes `stops`, and that Corrector's id.
fn graded(stops: f32) -> (NodeGraph, NodeId) {
    let mut g = NodeGraph::default();
    let id = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == id).unwrap().params.exposure = Some(stops);
    (g, id)
}

fn media(from: f64, to: f64, grade: NodeGraph) -> Clip {
    let mut c = Clip::media(MediaId::new(), t(from), t(to), t(0.0), t(to - from));
    c.set_grade(grade);
    c
}

fn holding(kind: ClipKind, from: f64, to: f64) -> Clip {
    let mut c = Clip::gap(t(from), t(to));
    c.kind = kind;
    c.source_in = t(0.0);
    c.source_out = t(to - from);
    c
}

/// The project: a scene B with a graded clip in a grade group of B's, and
/// the active sequence A, with a graded clip of its own in a group of A's,
/// B laid in twice as an editorial compound and once as a precomposition,
/// and a timeline grade.
struct Project {
    a: Sequence,
    b: Sequence,
    own_clip: ClipId,
    own_node: NodeId,
    scene_clip: ClipId,
    scene_node: NodeId,
    a_group_post: (ClipId, NodeId),
    b_group_pre: (ClipId, NodeId),
    timeline: (ClipId, NodeId),
}

fn project() -> Project {
    let mut b = Sequence::new_default();
    let (scene_grade, scene_node) = graded(0.1);
    let mut scene = media(0.0, 4.0, scene_grade);
    let mut b_group = GradeGroup::new("Scene");
    let (pre, pre_node) = graded(0.2);
    b_group.stage_slot(klipp_timeline::GradeStage::Pre).set(Some(pre));
    scene.grade_group = Some(b_group.id);
    let scene_clip = scene.id;
    let b_pre_id = b_group.stage_clip_id(GradeStage::Pre);
    b.grade_groups = vec![b_group];
    b.tracks = vec![Track::new("V1", TrackKind::Video)];
    b.tracks[0].clips = vec![scene];

    let mut a = Sequence::new_default();
    let (own_grade, own_node) = graded(0.3);
    let mut own = media(0.0, 4.0, own_grade);
    let mut a_group = GradeGroup::new("Shot");
    let (post, post_node) = graded(0.4);
    a_group.stage_slot(klipp_timeline::GradeStage::Post).set(Some(post));
    own.grade_group = Some(a_group.id);
    let own_clip = own.id;
    let a_post_id = a_group.stage_clip_id(GradeStage::Post);
    a.grade_groups = vec![a_group];
    let (timeline_graph, timeline_node) = graded(0.5);
    a.timeline_grade_slot().set(Some(timeline_graph));
    let timeline_id = a.timeline_grade_clip_id();
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![own];
    let mut v2 = Track::new("V2", TrackKind::Video);
    v2.clips = vec![
        holding(ClipKind::Sequence { sequence_id: b.id }, 0.0, 4.0),
        holding(ClipKind::Sequence { sequence_id: b.id }, 4.0, 8.0),
    ];
    let mut v3 = Track::new("V3", TrackKind::Video);
    v3.clips = vec![holding(ClipKind::Precomposition { sequence_id: b.id }, 0.0, 4.0)];
    a.tracks = vec![v3, v2, v1];
    Project {
        a,
        b,
        own_clip,
        own_node,
        scene_clip,
        scene_node,
        a_group_post: (a_post_id, post_node),
        b_group_pre: (b_pre_id, pre_node),
        timeline: (timeline_id, timeline_node),
    }
}

/// Every exposure `node` has in the picture of the render copy: (where,
/// value) for each top-level picture clip's graph, each precomposition
/// scene's clip graphs, and the timeline grade. A precomposition's sound
/// tracks carry copies of its clips for the mixer, never drawn.
fn occurrences(copy: &Sequence, node: NodeId) -> Vec<(&'static str, f32)> {
    let mut out = Vec::new();
    let exposure = |g: &NodeGraph| g.nodes.iter().find(|n| n.id == node).map(|n| n.params.exposure.unwrap_or(0.0));
    for c in copy.tracks.iter().filter(|t| t.kind != TrackKind::Audio).flat_map(|t| t.clips.iter()) {
        if let Some(v) = exposure(c.grade()) {
            out.push(("top", v));
        }
        if let Some(scene) = &c.precomp {
            for inner in scene.all_clips() {
                if let Some(v) = exposure(inner.grade()) {
                    out.push(("precomp", v));
                }
            }
        }
    }
    if let Some(v) = copy.timeline_grade().and_then(exposure) {
        out.push(("timeline", v));
    }
    out
}

/// The render copy after one live change of `node`'s exposure to 2.0,
/// sent as the Colour page sends it, drained as the engine drains it.
fn dragged(p: &Project, clip_id: ClipId, node: NodeId) -> (Sequence, usize) {
    let copy = flatten_sequence(&p.a, &[p.a.clone(), p.b.clone()]);
    let shared = Arc::new(SharedSequence::new(copy));
    let queue = Arc::new(ArrayQueue::new(8));
    queue.push(ParamChange { clip_id, node_id: node, param: "exposure".into(), value: 2.0 }).unwrap();
    let (mut hash, mut arc) = (1u64, None);
    let applied = apply_param_changes(&shared, &queue, &mut hash, &mut arc);
    let copy = (*shared.read().unwrap()).clone();
    (copy, applied)
}

#[test]
fn a_clip_of_the_active_sequence_is_reached() {
    let p = project();
    let (copy, applied) = dragged(&p, p.own_clip, p.own_node);
    assert_eq!(applied, 1);
    assert_eq!(occurrences(&copy, p.own_node), [("top", 2.0)]);
}

#[test]
fn a_clip_inside_an_editorial_compound_is_reached_in_its_first_copy_only() {
    let p = project();
    let before = flatten_sequence(&p.a, &[p.a.clone(), p.b.clone()]);
    // B laid in twice: two top-level copies under the scene clip's own id.
    assert_eq!(before.all_clips().filter(|c| c.id == p.scene_clip).count(), 2);
    let (copy, applied) = dragged(&p, p.scene_clip, p.scene_node);
    assert_eq!(applied, 1);
    let mut seen = occurrences(&copy, p.scene_node);
    seen.sort_by(|x, y| x.0.cmp(y.0).then(x.1.total_cmp(&y.1)));
    assert_eq!(seen, [("precomp", 0.1), ("top", 0.1), ("top", 2.0)], "one compound copy moves; the other and the precomposition don't");
}

#[test]
fn a_clip_inside_a_precomposition_is_not_reached_live() {
    let p = project();
    // Its instance is renamed, so no change can name it, and the scene's
    // own id doesn't reach it either.
    let before = flatten_sequence(&p.a, &[p.a.clone(), p.b.clone()]);
    let scene = before.all_clips().find_map(|c| c.precomp.clone()).expect("the precomposition's scene");
    let instance = scene.all_clips().next().unwrap().id;
    assert_ne!(instance, p.scene_clip, "an instance has its own id");
    for id in [p.scene_clip, instance] {
        let (copy, _) = dragged(&p, id, p.scene_node);
        assert!(occurrences(&copy, p.scene_node).contains(&("precomp", 0.1)), "the instance kept its value");
    }
}

#[test]
fn a_stage_node_of_the_active_sequences_group_reaches_its_members() {
    let p = project();
    let (clip, node) = p.a_group_post;
    let (copy, applied) = dragged(&p, clip, node);
    assert_eq!(applied, 1, "one member");
    assert_eq!(occurrences(&copy, node), [("top", 2.0)]);
}

#[test]
fn a_stage_node_of_a_nested_scenes_group_is_not_reached() {
    let p = project();
    let (clip, node) = p.b_group_pre;
    let (copy, applied) = dragged(&p, clip, node);
    assert_eq!(applied, 0);
    // Baked into both compound copies and the precomposition's instance,
    // and none of them moves.
    let seen = occurrences(&copy, node);
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(seen.iter().all(|(_, v)| *v == 0.2), "{seen:?}");
}

#[test]
fn the_timeline_grade_node_is_reached() {
    let p = project();
    let (clip, node) = p.timeline;
    let (copy, applied) = dragged(&p, clip, node);
    assert_eq!(applied, 1);
    assert_eq!(occurrences(&copy, node), [("timeline", 2.0)]);
}

