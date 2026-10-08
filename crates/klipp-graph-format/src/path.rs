//! Addressing a node anywhere in a project (§5.5): a root, then the ids of
//! the group nodes down to it. Every op, every live parameter change and
//! every assistant call carries a full path, so nothing depends on which
//! group the interface has open.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::lifted::Stage;
use crate::{NodeId, ParamKey, ParamValue};

/// Where a path starts: the store that owns the graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "root", rename_all = "snake_case")]
pub enum Root {
    /// A clip's own graph.
    Clip { clip: Uuid },
    /// A sequence's timeline grade.
    TimelineGrade { sequence: Uuid },
    /// A stage of a grade group.
    GroupStage { group: Uuid, stage: Stage },
    /// A track's grade, or its effect list.
    Track { track: Uuid, part: TrackPart },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackPart {
    Grade,
    Effects,
}

/// One output of a node, by the node's path and the output's port: what
/// an inspection reads (a thumbnail, the scope's tap, a solo, a matte;
/// M3 step 16a).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeOutput {
    pub path: NodePath,
    pub port: crate::PortKey,
}

impl NodeOutput {
    /// The node at `path`, at its picture output.
    pub fn out(path: NodePath) -> Self {
        Self { path, port: crate::PortKey::from("out") }
    }
}

/// A node, by its root and the node ids from the root's graph down: each
/// id but the last names a group node whose body holds the next.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodePath {
    pub root: Root,
    pub nodes: Vec<NodeId>,
}

impl NodePath {
    pub fn new(root: Root, nodes: impl IntoIterator<Item = NodeId>) -> Self {
        Self { root, nodes: nodes.into_iter().collect() }
    }

    /// The node the path names.
    pub fn node(&self) -> Option<NodeId> {
        self.nodes.last().copied()
    }
}

/// One parameter of one node set to a value, the unit a live edit and the
/// engine's drain exchange.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamChange {
    pub path: NodePath,
    pub param: ParamKey,
    pub value: ParamValue,
}
