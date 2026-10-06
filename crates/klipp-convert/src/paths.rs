//! Paths against a converted project (#1081): resolving a [`NodePath`] to
//! the document node it names, and finding every occurrence of that node
//! in a sequence's spliced render copies, the copies the engine's drain
//! writes a live change into.
//!
//! Occurrences, by root:
//! - a clip's node: in that clip's render copy, among its own nodes;
//! - a grade-group stage's node: in every member's copy, inside the nested
//!   group the splice put there with that stage's origin;
//! - a track's grade node: in every clip of the track, inside the nested
//!   group with the track's origin; a track effect: the effect node with
//!   the track's origin;
//! - a timeline grade's node: none in clip copies (the timeline grade runs
//!   on the sequence); the document node itself.
//!
//! Precomposition instances (a clip's copies inside a precomposition
//! scene) follow the rule settled in #1081, added when it is.

use std::collections::BTreeMap;

use klipp_graph_format::lifted::{types as lt, ClipPicture, Origin, Stage};
use klipp_graph_format::path::{NodePath, Root, TrackPart};
use klipp_graph_format::{Graph, Node, NodeId};
use serde_json::Value;
use uuid::Uuid;

/// Why a path names no node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathError {
    /// No store with the root's id.
    NoRoot,
    /// An id along the path isn't in the graph it should be in.
    NoNode(NodeId),
    /// An id along the path, not the last, names a node that isn't a group.
    NotAGroup(NodeId),
    /// A path with no node ids.
    Empty,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PathError {}

/// A converted (revision-2) project's pictures, by the store that owns
/// each.
#[derive(Debug, Default)]
pub struct Documents {
    pub clips: BTreeMap<Uuid, ClipPicture>,
    pub timeline: BTreeMap<Uuid, Graph>,
    pub stages: BTreeMap<(Uuid, Stage), Graph>,
    pub track_grades: BTreeMap<Uuid, Graph>,
    pub track_effects: BTreeMap<Uuid, Vec<Node>>,
}

fn id(v: &Value) -> Option<Uuid> {
    v.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok())
}

fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
}

impl Documents {
    /// Index a revision-2 project's pictures.
    pub fn of(project: &Value) -> Result<Self, String> {
        let mut d = Self::default();
        let list = |v: &Value, k: &str| v.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
        let seqs = match project.get("sequences") {
            Some(_) => list(project, "sequences"),
            None => project.get("sequence").cloned().into_iter().collect(),
        };
        for seq in &seqs {
            if let (Some(sid), Some(g)) = (id(seq), seq.get("graph_v2")) {
                d.timeline.insert(sid, parse(g)?);
            }
            for group in list(seq, "grade_groups") {
                let (Some(gid), Some(v2)) = (id(&group), group.get("graph_v2")) else { continue };
                for (key, stage) in [("pre", Stage::Pre), ("post", Stage::Post)] {
                    if let Some(g) = v2.get(key) {
                        d.stages.insert((gid, stage), parse(g)?);
                    }
                }
            }
            for track in list(seq, "tracks") {
                let tid = id(&track);
                if let (Some(tid), Some(v2)) = (tid, track.get("effects").and_then(|e| e.get("graph_v2"))) {
                    if let Some(g) = v2.get("grade") {
                        d.track_grades.insert(tid, parse(g)?);
                    }
                    d.track_effects.insert(tid, parse(v2.get("effects").unwrap_or(&Value::Array(Vec::new())))?);
                }
                for clip in list(&track, "clips") {
                    if let (Some(cid), Some(p)) = (id(&clip), clip.get("graph_v2")) {
                        d.clips.insert(cid, parse(p)?);
                    }
                }
            }
        }
        Ok(d)
    }
}

/// Walk `ids` from `nodes`: each id but the last a group whose body holds
/// the next.
fn walk<'a>(mut nodes: &'a [Node], ids: &[NodeId]) -> Result<&'a Node, PathError> {
    let (last, through) = ids.split_last().ok_or(PathError::Empty)?;
    for step in through {
        let n = nodes.iter().find(|n| n.id == *step).ok_or(PathError::NoNode(*step))?;
        nodes = &n.group.as_ref().ok_or(PathError::NotAGroup(*step))?.graph.nodes;
    }
    nodes.iter().find(|n| n.id == *last).ok_or(PathError::NoNode(*last))
}

/// The document node a path names.
pub fn resolve<'a>(docs: &'a Documents, path: &NodePath) -> Result<&'a Node, PathError> {
    match path.root {
        Root::Clip { clip } => walk(&docs.clips.get(&clip).ok_or(PathError::NoRoot)?.graph.nodes, &path.nodes),
        Root::TimelineGrade { sequence } => walk(&docs.timeline.get(&sequence).ok_or(PathError::NoRoot)?.nodes, &path.nodes),
        Root::GroupStage { group, stage } => walk(&docs.stages.get(&(group, stage)).ok_or(PathError::NoRoot)?.nodes, &path.nodes),
        Root::Track { track, part: TrackPart::Grade } => walk(&docs.track_grades.get(&track).ok_or(PathError::NoRoot)?.nodes, &path.nodes),
        Root::Track { track, part: TrackPart::Effects } => walk(docs.track_effects.get(&track).ok_or(PathError::NoRoot)?, &path.nodes),
    }
}

/// One occurrence of a document node in a render copy: the clip whose copy
/// holds it, and the node ids from that copy's graph down to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occurrence {
    pub clip: Uuid,
    pub nodes: Vec<NodeId>,
}

/// Every occurrence of the path's node in `copies` (each clip's spliced
/// render copy), for the drain to write a live change into.
pub fn occurrences(path: &NodePath, copies: &[(Uuid, &ClipPicture)]) -> Vec<Occurrence> {
    let mut out = Vec::new();
    for (clip, copy) in copies {
        let nodes = &copy.graph.nodes;
        let found: Option<Vec<NodeId>> = match path.root {
            Root::Clip { clip: c } if c == *clip => {
                // Among the clip's own nodes only: a spliced stage may hold
                // a node with the same id.
                let own: Vec<Node> = nodes.iter().filter(|n| n.origin.is_none()).cloned().collect();
                walk(&own, &path.nodes).ok().map(|_| path.nodes.clone())
            }
            Root::Clip { .. } | Root::TimelineGrade { .. } => None,
            Root::GroupStage { group, stage } => nested(nodes, Origin::GroupStage { group, stage }, &path.nodes),
            Root::Track { track, part: TrackPart::Grade } => nested(nodes, Origin::Track { track }, &path.nodes),
            Root::Track { track, part: TrackPart::Effects } => nodes
                .iter()
                .find(|n| n.origin == Some(Origin::Track { track }) && n.type_id.as_str() != lt::GROUP && Some(n.id) == path.node() && path.nodes.len() == 1)
                .map(|n| vec![n.id]),
        };
        if let Some(nodes) = found {
            out.push(Occurrence { clip: *clip, nodes });
        }
    }
    out
}

/// The node at `ids` inside the nested group with `origin`, as the ids
/// from the copy's graph: the group's id, then `ids`.
fn nested(nodes: &[Node], origin: Origin, ids: &[NodeId]) -> Option<Vec<NodeId>> {
    let group = nodes.iter().find(|n| n.origin == Some(origin) && n.type_id.as_str() == lt::GROUP)?;
    walk(&group.group.as_ref()?.graph.nodes, ids).ok()?;
    Some(std::iter::once(group.id).chain(ids.iter().copied()).collect())
}
