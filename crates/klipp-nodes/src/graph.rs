use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::kinds::NodeKind;
use crate::params::{NodeKeyframeTracks, NodeParams};

pub type NodeId = Uuid;

/// A directed acyclic graph of color processing nodes.
/// Replaces ClipColorGrade entirely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// The node whose output is the final graded frame.
    pub output_node_id: NodeId,
    /// UI layout positions for the node editor.
    #[serde(default)]
    pub node_positions: HashMap<NodeId, NodePos>,
}

impl Default for NodeGraph {
    fn default() -> Self {
        crate::defaults::default_graph()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    /// User-editable display name.
    pub label: String,
    /// Bypassed when false — input passes through unchanged.
    pub enabled: bool,
    /// The node's parameter values.
    #[serde(default)]
    pub params: NodeParams,
    /// Per-param keyframe animation.
    #[serde(default)]
    pub keyframes: NodeKeyframeTracks,
}

/// A connection from one node's output to another node's input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from_node: NodeId,
    pub from_port: u8,
    pub to_node: NodeId,
    pub to_port: u8,
}

/// Screen position for the node editor UI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct NodePos {
    pub x: f32,
    pub y: f32,
}

/// Error returned when a cycle is detected in the graph.
#[derive(Debug, Clone, thiserror::Error)]
#[error("cycle detected in node graph")]
pub struct CycleError;

impl NodeGraph {
    /// Returns true if the graph has no active processing nodes.
    pub fn is_identity(&self) -> bool {
        self.nodes
            .iter()
            .all(|n| !n.enabled
                || matches!(n.kind, NodeKind::Source | NodeKind::Output | NodeKind::InputTransform)
                || n.params.is_default())
    }

    /// Topologically sorted execution order using Kahn's algorithm.
    /// Returns Err if the graph contains a cycle.
    pub fn topo_sort(&self) -> Result<Vec<NodeId>, CycleError> {
        let node_ids: Vec<NodeId> = self.nodes.iter().map(|n| n.id).collect();
        let mut in_degree: HashMap<NodeId, usize> = HashMap::new();
        for id in &node_ids {
            in_degree.insert(*id, 0);
        }
        for edge in &self.edges {
            *in_degree.entry(edge.to_node).or_insert(0) += 1;
        }

        // Start with all nodes that have no incoming edges.
        let mut queue: Vec<NodeId> = node_ids
            .iter()
            .filter(|id| in_degree.get(id).copied().unwrap_or(0) == 0)
            .copied()
            .collect();

        let mut order = Vec::with_capacity(node_ids.len());

        while let Some(node_id) = queue.pop() {
            order.push(node_id);
            for edge in &self.edges {
                if edge.from_node == node_id {
                    if let Some(deg) = in_degree.get_mut(&edge.to_node) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push(edge.to_node);
                        }
                    }
                }
            }
        }

        if order.len() != node_ids.len() {
            return Err(CycleError);
        }

        Ok(order)
    }

    /// Find the node feeding into the given node's port.
    pub fn input_of(&self, node_id: NodeId, port: u8) -> Option<NodeId> {
        self.edges
            .iter()
            .find(|e| e.to_node == node_id && e.to_port == port)
            .map(|e| e.from_node)
    }

    /// Regenerate all NodeIds and update edge/position references.
    /// Used when pasting a copied graph onto a different clip.
    pub fn remap_ids(&self) -> NodeGraph {
        let id_map: HashMap<NodeId, NodeId> = self
            .nodes
            .iter()
            .map(|n| (n.id, NodeId::new_v4()))
            .collect();
        let mut new_graph = self.clone();
        for node in &mut new_graph.nodes {
            node.id = id_map[&node.id];
        }
        for edge in &mut new_graph.edges {
            edge.from_node = id_map[&edge.from_node];
            edge.to_node = id_map[&edge.to_node];
        }
        new_graph.output_node_id = id_map[&self.output_node_id];
        new_graph.node_positions = self
            .node_positions
            .iter()
            .map(|(old_id, pos)| (id_map[old_id], pos.clone()))
            .collect();
        new_graph
    }
}
