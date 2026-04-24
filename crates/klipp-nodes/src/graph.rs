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
    /// Wireless-link output label. When `Some(name)`, any input port
    /// whose `input_links[port]` matches `name` reads this node's
    /// primary output (port 0) without a drawn edge. `None` disables
    /// wireless sending from this node.
    #[serde(default)]
    pub output_link: Option<String>,
    /// Per-input-port wireless receiver labels. `Some(name)` at index
    /// `port` means that port receives from the node whose
    /// `output_link == Some(name)`, without a drawn edge. Empty vec /
    /// `None` entries fall back to normal wired-edge resolution.
    #[serde(default)]
    pub input_links: Vec<Option<String>>,
}

/// A connection from one node's output to another node's input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from_node: NodeId,
    pub from_port: u8,
    pub to_node: NodeId,
    pub to_port: u8,
}

/// Stable phantom NodeId used by the inspector when a tab is rendered
/// against a clip that has no grade node yet. The real Corrector is
/// materialised lazily on first slider touch (see app.rs handler for
/// SetNodeParam et al.); until then, widgets read/write a scratch
/// `NodeParams::default()` and dispatch their commands tagged with
/// this id. Kept stable so re-dispatches inside one frame land on the
/// same eventually-real node.
pub const PHANTOM_CORRECTOR_ID: NodeId = Uuid::from_u128(0xC0DE_C0DE_C0DE_C0DE_C0DE_C0DE_C0DE_C0DE);

impl NodeGraph {
    /// Find an existing Corrector, or splice a new one in before
    /// Output. Returns the Corrector's id either way. Used both by the
    /// inspector's first-touch lazy-create flow and by load.rs project
    /// migrations.
    pub fn ensure_corrector(&mut self) -> NodeId {
        if let Some(idx) = self.nodes.iter().position(|n| n.kind == NodeKind::Corrector) {
            return self.nodes[idx].id;
        }
        let new_id = NodeId::new_v4();
        let output_id = self.output_node_id;
        let insert_at = self.nodes.iter().position(|n| n.id == output_id)
            .unwrap_or(self.nodes.len());
        self.nodes.insert(insert_at, Node {
            id: new_id,
            kind: NodeKind::Corrector,
            label: "Corrector".to_string(),
            enabled: true,
            params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        // Re-route any edge ending at Output's port 0 to the Corrector
        // first, then wire Corrector → Output.
        for edge in &mut self.edges {
            if edge.to_node == output_id && edge.to_port == 0 {
                edge.to_node = new_id;
                edge.to_port = 0;
            }
        }
        self.edges.push(Edge {
            from_node: new_id,
            from_port: 0,
            to_node: output_id,
            to_port: 0,
        });
        // Place it just before Output in the canvas layout.
        if let Some(out_pos) = self.node_positions.get(&output_id).copied() {
            self.node_positions.insert(new_id, NodePos { x: out_pos.x - 220.0, y: out_pos.y });
        } else {
            self.node_positions.insert(new_id, NodePos { x: 220.0, y: 0.0 });
        }
        new_id
    }
}

impl Node {
    /// Effective input-port count, accounting for nodes (currently just
    /// `LayerMixer`) whose port count is data-driven by their params
    /// rather than fixed by the kind alone.
    ///
    /// Always at least `kind.input_port_count()` — i.e. a freshly added
    /// LayerMixer with no params still shows its 2 starter ports.
    pub fn dynamic_input_port_count(&self) -> u8 {
        let static_count = self.kind.input_port_count();
        match self.kind {
            NodeKind::LayerMixer => self
                .params
                .layer_mixer
                .as_ref()
                .map(|m| m.inputs.len() as u8)
                .unwrap_or(0)
                .max(static_count),
            _ => static_count,
        }
    }
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
    /// Returns Err if the graph contains a cycle. Wireless-link deps
    /// count toward in-degree just like drawn edges, so a receiver
    /// node never runs before its sender.
    pub fn topo_sort(&self) -> Result<Vec<NodeId>, CycleError> {
        let node_ids: Vec<NodeId> = self.nodes.iter().map(|n| n.id).collect();
        let deps: Vec<(NodeId, NodeId)> = self.iter_dependencies().collect();
        let mut in_degree: HashMap<NodeId, usize> = HashMap::new();
        for id in &node_ids {
            in_degree.insert(*id, 0);
        }
        for (_, to) in &deps {
            *in_degree.entry(*to).or_insert(0) += 1;
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
            for (from, to) in &deps {
                if *from == node_id {
                    if let Some(deg) = in_degree.get_mut(to) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push(*to);
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

    /// Find the node feeding into the given node's port. Edges win; if
    /// no edge lands on the port, the node's own wireless-receiver slot
    /// for that port is resolved against every other node's
    /// `output_link`. Edge-only graphs behave identically to the
    /// pre-wireless world (the fallback is skipped when no receiver
    /// slot is set).
    pub fn input_of(&self, node_id: NodeId, port: u8) -> Option<NodeId> {
        if let Some(edge) = self
            .edges
            .iter()
            .find(|e| e.to_node == node_id && e.to_port == port)
        {
            return Some(edge.from_node);
        }
        let recv_label = self
            .nodes
            .iter()
            .find(|n| n.id == node_id)
            .and_then(|n| n.input_links.get(port as usize).and_then(|l| l.clone()))?;
        self.nodes
            .iter()
            .find(|n| n.output_link.as_ref() == Some(&recv_label) && n.id != node_id)
            .map(|n| n.id)
    }

    /// Iterator over every (from, to) dependency pair in the graph,
    /// including wireless-link receivers. Used by topo sort so a
    /// wireless receiver runs after its sender even without a drawn
    /// edge.
    pub fn iter_dependencies(&self) -> impl Iterator<Item = (NodeId, NodeId)> + '_ {
        let wired = self.edges.iter().map(|e| (e.from_node, e.to_node));
        let wireless = self.nodes.iter().flat_map(move |recv| {
            recv.input_links
                .iter()
                .enumerate()
                .filter_map(move |(port, slot)| {
                    let label = slot.as_ref()?;
                    if self.edges.iter().any(|e| e.to_node == recv.id && e.to_port == port as u8) {
                        return None;
                    }
                    self.nodes
                        .iter()
                        .find(|s| s.output_link.as_ref() == Some(label) && s.id != recv.id)
                        .map(|sender| (sender.id, recv.id))
                })
        });
        wired.chain(wireless)
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
