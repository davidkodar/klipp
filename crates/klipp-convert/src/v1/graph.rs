// Frozen from crates/klipp-nodes/src/graph.rs at 7d309e9f. Do not follow app type changes.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;
pub type NodeId = Uuid;

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct NodeGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub output_node_id: NodeId,
    #[serde(default)]
    pub node_positions: HashMap<NodeId, NodePos>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_space_override: Option<super::WorkingColorspace>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<NodeFrame>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sticky_notes: Vec<NodeStickyNote>,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct NodeStickyNote {
    pub id: NodeStickyNoteId,
    pub pos: NodePos,
    #[serde(default = "default_sticky_width")]
    pub width: f32,
    #[serde(default)]
    pub text: String,
    pub color_rgba: u32,
}

pub type NodeStickyNoteId = uuid::Uuid;

fn default_sticky_width() -> f32 {
    200.0
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct NodeFrame {
    pub id: NodeFrameId,
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub color_rgba: u32,
    pub contained_node_ids: std::collections::HashSet<NodeId>,
}

pub type NodeFrameId = uuid::Uuid;

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub label: String,
    pub enabled: bool,
    #[serde(default)]
    pub params: NodeParams,
    #[serde(default)]
    pub keyframes: NodeKeyframeTracks,
    #[serde(default)]
    pub output_link: Option<String>,
    #[serde(default)]
    pub input_links: Vec<Option<String>>,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from_node: NodeId,
    pub from_port: u8,
    pub to_node: NodeId,
    pub to_port: u8,
}

#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct NodePos {
    pub x: f32,
    pub y: f32,
}
