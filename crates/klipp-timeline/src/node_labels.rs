//! Colour labels on graph nodes (#1753): a colour a person gives a node
//! of any graph in a sequence, to find it again at a glance, kept on the
//! sequence by the node's path. The graphs themselves have no place for
//! it: a grade graph's stored form keeps no field a label could go in,
//! and the lifted pictures are derived from the clips.
//!
//! A label names its node by path, so a label whose node is gone (the
//! node deleted, its clip removed) names nothing: it is not drawn, and it
//! is left out when the sequence is saved. Undo can bring the node back,
//! and with it the label, until then.

use crate::{Clip, Sequence};
use klipp_core::ClipId;
use klipp_graph_format::path::{NodePath, Root, TrackPart};
use klipp_graph_format::{Graph, NodeId};
use serde::{Deserialize, Serialize};

/// One node's colour label: the node by path, and the colour as the
/// clip colour labels number theirs, 1 to 6 (red, orange, yellow, green,
/// blue, purple). No label is no entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLabel {
    pub node: NodePath,
    pub colour: u8,
}

/// The highest colour a label holds: the six clip label colours.
pub const NODE_LABEL_COLOURS: u8 = 6;

impl Sequence {
    /// The colour label of the node at `node`, 0 for none.
    pub fn node_label(&self, node: &NodePath) -> u8 {
        self.node_labels.iter().find(|l| l.node == *node).map_or(0, |l| l.colour)
    }

    /// The sequence's labels with `node`'s set to `colour`, or taken off
    /// at 0. A colour past the six is none.
    pub fn node_labels_with(&self, node: &NodePath, colour: u8) -> Vec<NodeLabel> {
        let mut labels: Vec<NodeLabel> = self.node_labels.iter().filter(|l| l.node != *node).cloned().collect();
        if (1..=NODE_LABEL_COLOURS).contains(&colour) {
            labels.push(NodeLabel { node: node.clone(), colour });
        }
        labels
    }

    /// The labels whose node is still there, as they are saved.
    pub fn live_node_labels(&self) -> Vec<NodeLabel> {
        self.node_labels.iter().filter(|l| self.holds_node(&l.node)).cloned().collect()
    }

    /// Whether the sequence holds a node at `path`: in a clip's picture,
    /// the timeline grade, a grade group's stage, or a track's grade or
    /// effects.
    pub fn holds_node(&self, path: &NodePath) -> bool {
        match path.root {
            Root::Clip { clip } => self
                .all_clips()
                .find(|c| c.id.0 == clip)
                .and_then(|c| c.picture_v2().ok())
                .is_some_and(|p| lifted_holds(&p.picture.graph, &path.nodes)),
            Root::Track { track, part: TrackPart::Effects } => match path.nodes.as_slice() {
                [node] => self.lift_track_parts(track).is_ok_and(|(nodes, _)| nodes.iter().any(|n| n.id == *node)),
                _ => false,
            },
            root => self.root_grade(root).ok().flatten().is_some_and(|g| grade_holds(g, &path.nodes)),
        }
    }
}

/// Whether `graph` holds a node at `nodes`, through its groups.
fn lifted_holds(graph: &Graph, nodes: &[NodeId]) -> bool {
    let Some((last, through)) = nodes.split_last() else { return false };
    let mut level = graph;
    for id in through {
        match level.nodes.iter().find(|n| n.id == *id).and_then(|n| n.group.as_ref()) {
            Some(body) => level = &body.graph,
            None => return false,
        }
    }
    level.nodes.iter().any(|n| n.id == *last)
}

/// Whether the grade `graph` holds a node at `nodes`, through its
/// compounds.
fn grade_holds(graph: &klipp_nodes::NodeGraph, nodes: &[NodeId]) -> bool {
    let Some((last, through)) = nodes.split_last() else { return false };
    let mut level = graph;
    for id in through {
        match level.nodes.iter().find(|n| n.id == *id).and_then(|n| n.params.compound_subgraph.as_ref()) {
            Some(sub) => level = sub,
            None => return false,
        }
    }
    level.nodes.iter().any(|n| n.id == *last)
}

/// The labels of `from`'s nodes as they belong on `to`, a copy of it (a
/// duplicate, a paste): the node ids a copy keeps are the same; the ids
/// derived from the clip's own (its grade's group, its Source and
/// Output) are matched by kind, in order, at each level. A label whose
/// node `to` has no match for is left behind.
pub fn carry_node_labels(labels: &[NodeLabel], from: &Clip, to: &Clip) -> Vec<NodeLabel> {
    let own: Vec<&NodeLabel> = labels.iter().filter(|l| l.node.root == Root::Clip { clip: from.id.0 }).collect();
    if own.is_empty() {
        return Vec::new();
    }
    let (Ok(a), Ok(b)) = (from.picture_v2(), to.picture_v2()) else { return Vec::new() };
    own.into_iter()
        .filter_map(|l| {
            let nodes = matched_path(&a.picture.graph, &b.picture.graph, &l.node.nodes)?;
            Some(NodeLabel { node: NodePath::new(Root::Clip { clip: to.id.0 }, nodes), colour: l.colour })
        })
        .collect()
}

/// `nodes` in `a` named in `b`, a copy of it.
fn matched_path(a: &Graph, b: &Graph, nodes: &[NodeId]) -> Option<Vec<NodeId>> {
    let (mut ga, mut gb) = (a, b);
    let mut out = Vec::with_capacity(nodes.len());
    for (i, id) in nodes.iter().enumerate() {
        let na = ga.nodes.iter().find(|n| n.id == *id)?;
        let nb = match gb.nodes.iter().find(|n| n.id == *id) {
            Some(n) => n,
            None => {
                // The k-th of its kind among the nodes `b` names otherwise.
                let unmatched = |g: &Graph, other: &Graph| -> Vec<NodeId> {
                    g.nodes.iter().filter(|n| n.type_id == na.type_id && !other.nodes.iter().any(|o| o.id == n.id)).map(|n| n.id).collect()
                };
                let k = unmatched(ga, gb).iter().position(|x| x == id)?;
                let there = *unmatched(gb, ga).get(k)?;
                gb.nodes.iter().find(|n| n.id == there)?
            }
        };
        out.push(nb.id);
        if i + 1 < nodes.len() {
            ga = &na.group.as_ref()?.graph;
            gb = &nb.group.as_ref()?.graph;
        }
    }
    Some(out)
}

/// The id of the group that holds clip `clip`'s grade in its picture.
pub fn grade_group_id(clip: ClipId) -> NodeId {
    klipp_graph_lift::ids::entry_id(klipp_graph_lift::ids::RootKind::Clip, clip.0, "grade", 0)
}

/// One colour label of a grade, by the node's path inside the grade
/// (`[node]`, or through compounds `[compound, …, node]`): how a label
/// travels with a grade copied from one clip to another, or kept in the
/// library or a still (#1866).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GradeNodeLabel {
    pub nodes: Vec<NodeId>,
    pub colour: u8,
}

/// The labels of clip `clip`'s grade, by path inside it.
pub fn grade_node_labels(labels: &[NodeLabel], clip: ClipId) -> Vec<GradeNodeLabel> {
    let group = grade_group_id(clip);
    labels
        .iter()
        .filter(|l| l.node.root == Root::Clip { clip: clip.0 } && l.node.nodes.len() > 1 && l.node.nodes[0] == group)
        .map(|l| GradeNodeLabel { nodes: l.node.nodes[1..].to_vec(), colour: l.colour })
        .collect()
}

/// `labels` with clip `clip`'s grade labelled by `grade` as well; with
/// `replace`, its grade's labels before are taken off, as a grade put in
/// place of its own takes its nodes.
pub fn with_grade_node_labels(labels: &[NodeLabel], clip: ClipId, grade: &[GradeNodeLabel], replace: bool) -> Vec<NodeLabel> {
    let group = grade_group_id(clip);
    let root = Root::Clip { clip: clip.0 };
    let mut out: Vec<NodeLabel> = labels
        .iter()
        .filter(|l| !(replace && l.node.root == root && l.node.nodes.len() > 1 && l.node.nodes[0] == group))
        .cloned()
        .collect();
    for g in grade {
        let node = NodePath::new(root, std::iter::once(group).chain(g.nodes.iter().copied()));
        out.retain(|l| l.node != node);
        out.push(NodeLabel { node, colour: g.colour });
    }
    out
}

/// A grade's labels as they belong on its copy, whose top nodes took the
/// ids `map` gives (a compound's own keep theirs) and which holds the
/// top nodes `kept`: a label of a node that didn't come along is left
/// behind.
pub fn remap_grade_node_labels(grade: &[GradeNodeLabel], map: &std::collections::HashMap<NodeId, NodeId>, kept: &klipp_nodes::NodeGraph) -> Vec<GradeNodeLabel> {
    grade
        .iter()
        .filter_map(|g| {
            let (first, rest) = g.nodes.split_first()?;
            let new = *map.get(first)?;
            let nodes: Vec<NodeId> = std::iter::once(new).chain(rest.iter().copied()).collect();
            grade_holds(kept, &nodes).then_some(GradeNodeLabel { nodes, colour: g.colour })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GradeAccess;

    fn graded() -> (Sequence, Clip, NodeId) {
        let mut seq = Sequence::new_default();
        let mut clip = crate::test_clip::TestClip::media(2.0).build();
        let node = clip.edit_grade(|g| g.ensure_corrector());
        seq.tracks[0].clips.push(clip.clone());
        (seq, clip, node)
    }

    /// A label is set, replaced and taken off by its path; a colour past
    /// the six is none.
    #[test]
    fn a_label_is_set_replaced_and_cleared() {
        let (mut seq, clip, node) = graded();
        let path = NodePath::new(Root::Clip { clip: clip.id.0 }, [grade_group_id(clip.id), node]);
        seq.node_labels = seq.node_labels_with(&path, 2);
        assert_eq!(seq.node_label(&path), 2);
        seq.node_labels = seq.node_labels_with(&path, 5);
        assert_eq!(seq.node_labels.len(), 1);
        assert_eq!(seq.node_label(&path), 5);
        seq.node_labels = seq.node_labels_with(&path, 9);
        assert!(seq.node_labels.is_empty());
    }

    /// Only the labels of nodes still there are kept for saving: a grade
    /// node's is, one of a node deleted, or of a clip removed, is not.
    #[test]
    fn labels_of_gone_nodes_are_not_live() {
        let (mut seq, clip, node) = graded();
        let group = grade_group_id(clip.id);
        let here = NodeLabel { node: NodePath::new(Root::Clip { clip: clip.id.0 }, [group, node]), colour: 1 };
        let gone_node = NodeLabel { node: NodePath::new(Root::Clip { clip: clip.id.0 }, [group, uuid::Uuid::new_v4()]), colour: 2 };
        let gone_clip = NodeLabel { node: NodePath::new(Root::Clip { clip: uuid::Uuid::new_v4() }, [group, node]), colour: 3 };
        seq.node_labels = vec![here.clone(), gone_node, gone_clip];
        assert_eq!(seq.live_node_labels(), vec![here]);
    }

    /// A timeline grade node's label lives while the node does.
    #[test]
    fn a_timeline_grade_node_is_held() {
        let (mut seq, ..) = graded();
        let mut grade = klipp_nodes::NodeGraph::default();
        let node = grade.ensure_corrector();
        seq.timeline_grade_slot().set(Some(grade));
        let root = Root::TimelineGrade { sequence: seq.id.0 };
        assert!(seq.holds_node(&NodePath::new(root, [node])));
        assert!(!seq.holds_node(&NodePath::new(root, [uuid::Uuid::new_v4()])));
    }

    /// A copy of a clip takes its labels: a grade node's under the copy's
    /// own grade group, which has another id.
    #[test]
    fn a_copy_carries_the_labels() {
        let (seq, clip, node) = graded();
        let path = NodePath::new(Root::Clip { clip: clip.id.0 }, [grade_group_id(clip.id), node]);
        let labels = vec![NodeLabel { node: path, colour: 4 }];
        let copy = Clip { id: ClipId::new(), ..clip.clone() };
        let carried = carry_node_labels(&labels, &clip, &copy);
        assert_eq!(carried, vec![NodeLabel { node: NodePath::new(Root::Clip { clip: copy.id.0 }, [grade_group_id(copy.id), node]), colour: 4 }]);
        let mut seq = seq;
        seq.tracks[0].clips.push(copy);
        assert!(seq.holds_node(&carried[0].node), "the carried label names the copy's node");
    }

    /// #1866: a grade's labels come off one clip by path inside the grade,
    /// follow the ids its copy took, and go onto another clip's grade,
    /// replacing its own or beside them; a label of a node the copy
    /// lacks is left behind.
    #[test]
    fn grade_labels_follow_a_copied_grade() {
        let (_, clip, node) = graded();
        let other = ClipId::new();
        let theirs = NodeLabel { node: NodePath::new(Root::Clip { clip: other.0 }, [grade_group_id(other), uuid::Uuid::new_v4()]), colour: 1 };
        let ghost = uuid::Uuid::new_v4();
        let labels = vec![
            NodeLabel { node: NodePath::new(Root::Clip { clip: clip.id.0 }, [grade_group_id(clip.id), node]), colour: 6 },
            NodeLabel { node: NodePath::new(Root::Clip { clip: clip.id.0 }, [grade_group_id(clip.id), ghost]), colour: 2 },
            theirs.clone(),
        ];
        let grade = grade_node_labels(&labels, clip.id);
        assert_eq!(grade.len(), 2);
        let (copy, map) = klipp_nodes::NodeGraph::default().with_grade_of_mapped(clip.grade());
        let moved = remap_grade_node_labels(&grade, &map, &copy);
        assert_eq!(moved, vec![GradeNodeLabel { nodes: vec![map[&node]], colour: 6 }], "the ghost's is left behind");
        let replaced = with_grade_node_labels(&labels, other, &moved, true);
        assert!(!replaced.contains(&theirs));
        assert!(replaced.contains(&NodeLabel { node: NodePath::new(Root::Clip { clip: other.0 }, [grade_group_id(other), map[&node]]), colour: 6 }));
        assert!(with_grade_node_labels(&labels, other, &moved, false).contains(&theirs), "an append keeps them");
    }
}
