//! The splice (phase 2a, #1068 follow-on): a clip's render copy takes its
//! grade group's stages and its track's grade and effects, as today's
//! `baked_for_render` does, and gives them back on [`unsplice`].
//!
//! Nested, not merged: each stage and the track grade is one
//! `klipp.group` node in the clip's chain (the pre stage before the
//! clip's Grade group, the post stage and then the track grade after it),
//! and the track's effects follow the clip's own. Every spliced node
//! carries its [`Origin`], so un-splicing removes exactly those and
//! splicing twice is splicing once. Flattening a nested group as
//! `prepend_keeping_ids` / `append_keeping_ids` do (input transforms
//! stripped) is the adapter's, and the adapter property checks it against
//! today's baked graph.
//!
//! What today skips (an identity stage or track grade, a working space
//! that doesn't match, a stage with no source) is decided by the caller,
//! with today's own functions, and arrives here as `None`.

use klipp_graph_format::lifted::{types as lt, ClipPicture, Origin, Stage};
use klipp_graph_format::{types, Edge, Graph, GroupBody, Node, NodeId, NodeTypeId};
use uuid::Uuid;

use crate::ids::{entry_id, RootKind};
use crate::lift::{lift_grade_only, ClipKind};

/// A clip's grade group, its stages lifted. `None` for a stage that is
/// absent or that today skips.
#[derive(Clone, Debug, PartialEq)]
pub struct StageInputs {
    pub group: Uuid,
    pub pre: Option<Graph>,
    pub post: Option<Graph>,
}

/// A clip's track. `effects` empty unless the track draws pictures (Video
/// or Text) and has video effects; `grade` `None` when absent or skipped.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackInputs {
    pub track: Uuid,
    pub effects: Vec<Node>,
    pub grade: Option<Graph>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpliceInputs {
    pub stages: Option<StageInputs>,
    pub track: Option<TrackInputs>,
}

/// A stage of a grade group as the splice takes it: the stage's
/// revision-1 graph (one grade group node) lifted to a body.
pub fn stage_body(structural: &Graph) -> Result<Graph, String> {
    lift_grade_only(structural)
}

/// A track's revision-1 graph split into what the splice takes: its
/// effects (the entries after its grade group) and its grade, lifted.
pub fn track_parts(structural: &Graph) -> Result<(Vec<Node>, Option<Graph>), String> {
    let mut effects = Vec::new();
    let mut grade = None;
    for n in &structural.nodes {
        if n.type_id.as_str() == types::LEGACY_GRADE {
            let only = Graph { nodes: vec![n.clone()], edges: Vec::new(), output: n.id, ..Graph::empty() };
            grade = Some(lift_grade_only(&only)?);
        } else {
            effects.push(n.clone());
        }
    }
    Ok((effects, grade))
}

fn group_node(id: NodeId, body: &Graph, origin: Origin) -> Node {
    Node {
        id,
        type_id: NodeTypeId::new(lt::GROUP),
        type_version: 1,
        label: String::new(),
        enabled: true,
        params: Default::default(),
        anim: Default::default(),
        mix: None,
        wireless: Default::default(),
        group: Some(Box::new(GroupBody { graph: body.clone(), ports: Vec::new() })),
        legacy: None,
        legacy_flags: None,
        origin: Some(origin),
    }
}

/// The chain's edges, `out → in` between neighbours.
fn chained(mut picture: ClipPicture) -> ClipPicture {
    picture.graph.edges = picture.graph.nodes.windows(2).map(|w| Edge { from: (w[0].id, "out".into()), to: (w[1].id, "in".into()) }).collect();
    picture.graph.output = picture.graph.nodes.last().map(|n| n.id).unwrap_or_default();
    picture
}

/// The clip's picture without anything spliced into it.
pub fn unsplice(clip: &ClipPicture) -> ClipPicture {
    let mut c = clip.clone();
    c.graph.nodes.retain(|n| n.origin.is_none());
    chained(c)
}

/// The clip's render copy: its own picture with its group's stages and
/// its track's grade and effects spliced in. Splicing a spliced copy gives
/// the same copy.
pub fn splice(clip: &ClipPicture, kind: ClipKind, inputs: &SpliceInputs) -> ClipPicture {
    let mut c = unsplice(clip);
    let nodes = &mut c.graph.nodes;
    // Where the grade runs: the clip's Grade group, else just before the
    // display tone map.
    let grade_at = nodes.iter().position(|n| n.type_id.as_str() == lt::GROUP);
    let before = grade_at.unwrap_or_else(|| nodes.iter().position(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap_or(nodes.len()));
    let mut after = grade_at.map_or(before, |i| i + 1);

    if let Some(s) = &inputs.stages {
        if let Some(pre) = &s.pre {
            let id = entry_id(RootKind::GradeGroup, s.group, "splice/pre", 0);
            nodes.insert(before, group_node(id, pre, Origin::GroupStage { group: s.group, stage: Stage::Pre }));
            after += 1;
        }
        if let Some(post) = &s.post {
            let id = entry_id(RootKind::GradeGroup, s.group, "splice/post", 0);
            nodes.insert(after, group_node(id, post, Origin::GroupStage { group: s.group, stage: Stage::Post }));
            after += 1;
        }
    }
    // Gaps and adjustment layers take nothing of their track's.
    let takes_track = !matches!(kind, ClipKind::Gap | ClipKind::Adjustment);
    if let Some(t) = inputs.track.as_ref().filter(|_| takes_track) {
        if let Some(grade) = &t.grade {
            let id = entry_id(RootKind::Track, t.track, "splice/grade", 0);
            nodes.insert(after, group_node(id, grade, Origin::Track { track: t.track }));
        }
        // The track's effects after the clip's own: before an Auto
        // placement that ends the chain (its place is computed over the
        // spliced list), else before the output.
        let auto_last = nodes.len() >= 2 && nodes[nodes.len() - 2].type_id.as_str() == lt::PLACEMENT && nodes[nodes.len() - 2].legacy.is_none();
        let at = if auto_last { nodes.len() - 2 } else { nodes.len().saturating_sub(1) };
        let spliced = t.effects.iter().map(|e| Node { origin: Some(Origin::Track { track: t.track }), ..e.clone() });
        nodes.splice(at..at, spliced);
    }
    chained(c)
}

/// The clip's own grade on a render copy: the clip-origin Grade group's
/// body, never a stage's (today's `own_grade()` callers read the clip's
/// own Corrector, which a prepended pre stage used to shadow).
pub fn own_grade(clip: &ClipPicture) -> Option<&Graph> {
    clip.graph
        .nodes
        .iter()
        .find(|n| n.origin.is_none() && n.type_id.as_str() == lt::GROUP)
        .and_then(|n| n.group.as_ref())
        .map(|g| &g.graph)
}
