use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::kinds::{NodeKind, PortRole};
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
    /// Optional per-graph working-colorspace pin. When set, the
    /// executor uses this instead of the project-level working
    /// colorspace for every `_ws` luma-weighted op inside the chain
    /// (Saturation, Vibrance, Tone Controls, Color Wheels,
    /// Wheel Sat/Bri, Clarity, Log Wheels, LUT3D bracket).
    ///
    /// The migration flow (Project Info → Convert grades) sets this
    /// to the project's *previous* working colorspace and wraps the
    /// chain with a `PrimariesMatrix` head + tail. The result: an
    /// existing Rec.709-tuned grade keeps its luma weighting and
    /// still reads the same pixels even though the surrounding
    /// project is now ACEScg. Without this override the luma vector
    /// would silently switch to AP1's row and the grade would shift.
    ///
    /// `None` (the default and the case for fresh projects) means
    /// "fall back to whatever the project is on" — exactly the
    /// pre-Phase-E-S7b behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_space_override: Option<klipp_core::WorkingColorspace>,
    /// Lightweight visual groupings — labelled rectangles painted
    /// behind a set of nodes for organisation. Distinct from the
    /// `Compound` NodeKind, which is a *collapsible sub-graph* with
    /// executor recursion. Frames are pure UI: the frame's visible
    /// rectangle each frame is computed from `union(member_rects)` +
    /// padding, so dragging a frame moves its members and the
    /// frame stretches/shrinks to follow them. Save/load round-trips
    /// via the existing NodeGraph serde derive — no format work.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<NodeFrame>,
    /// Free-floating annotation notes — distinct from frames in
    /// that they DON'T enclose nodes. Sit above wires but below
    /// nodes in the paint order, so they're visible without
    /// hiding the functional graph. Auto-fit height from wrapped
    /// text; width is user-set in v1 (200 graph-px default).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sticky_notes: Vec<NodeStickyNote>,
}

/// Free-floating annotation card. Stores position + width + text +
/// colour; height is computed each render from the wrapped text so
/// the user doesn't have to manage it. v1 keeps width fixed via
/// `width` (no resize handle); a future v2 could add a corner-
/// drag resize gesture without a serde format change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStickyNote {
    /// Stable id for AppCommands targeting this specific note.
    pub id: NodeStickyNoteId,
    /// Top-left in graph coordinates.
    pub pos: NodePos,
    /// Width in graph-pixel units. Default 200 (set by the
    /// AppCommand handler for `CreateStickyNote`); the data model
    /// stores it explicitly so future width-resize gestures can
    /// persist.
    #[serde(default = "default_sticky_width")]
    pub width: f32,
    /// User-editable multi-line text. Plain text in v1.
    #[serde(default)]
    pub text: String,
    /// Packed RGBA u32 (little-endian: r, g, b, a). v1 stores a
    /// single warm beige default; a future recolour menu would
    /// route through the existing colour-update pattern.
    pub color_rgba: u32,
}

pub type NodeStickyNoteId = uuid::Uuid;

fn default_sticky_width() -> f32 { 200.0 }

/// A lightweight labelled rectangle drawn behind a set of nodes.
/// See `NodeGraph::frames` for the semantics. The frame's visible
/// rect is recomputed each render from member positions; the data
/// model stores membership (`contained_node_ids`) plus user-edited
/// metadata (label, colour). Empty `contained_node_ids` is illegal
/// at rest — frames left with no members get pruned by the
/// AppCommand layer's cleanup pass on `RemoveNode`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeFrame {
    /// Stable id used by AppCommands targeting this specific frame
    /// (rename / recolour / delete / member ops). Re-mapped alongside
    /// node ids on copy/paste.
    pub id: NodeFrameId,
    /// User-editable label. Empty string is rendered as a "—"
    /// placeholder so the frame chip still shows a hit target.
    #[serde(default)]
    pub label: String,
    /// Multi-line annotation painted inside the frame's interior.
    /// Empty by default (rendered as nothing — the frame's interior
    /// stays empty and the contained nodes have full breathing
    /// room). Edited via the frame's right-click "Edit description…"
    /// entry. Plain text in v1; could grow to markdown later
    /// without a serde format change since it's just a String.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Frame tint, picked from a small palette in the right-click
    /// menu. Stored as packed RGBA u32 (`Color32::to_array`-style
    /// bytes in NCBA order is what egui uses; we use plain RGBA u32
    /// here for serde simplicity — the UI converts to Color32
    /// per-frame).
    pub color_rgba: u32,
    /// Members. Iteration order is unstable and irrelevant; the
    /// HashSet shape rejects accidental duplicates and gives O(1)
    /// `contains` for hit-tests. Cleanup invariant: when a node is
    /// removed from the graph, its id is dropped from every frame's
    /// set; frames that end up empty are pruned.
    pub contained_node_ids: std::collections::HashSet<NodeId>,
}

pub type NodeFrameId = uuid::Uuid;

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
    /// Find an existing ColorMatch node, or splice a new one in before
    /// Output. Returns its id. Used by the clip-to-clip match workflow
    /// so the inspector and timeline right-click both converge on one
    /// node per clip.
    pub fn ensure_color_match(&mut self) -> NodeId {
        if let Some(idx) = self.nodes.iter().position(|n| n.kind == NodeKind::ColorMatch) {
            return self.nodes[idx].id;
        }
        let new_id = NodeId::new_v4();
        let output_id = self.output_node_id;
        let insert_at = self.nodes.iter().position(|n| n.id == output_id)
            .unwrap_or(self.nodes.len());
        self.nodes.insert(insert_at, Node {
            id: new_id,
            kind: NodeKind::ColorMatch,
            label: "Color Match".to_string(),
            enabled: true,
            params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        for edge in &mut self.edges {
            if edge.to_node == output_id && edge.to_port == 0 {
                edge.to_node = new_id;
                edge.to_port = 0;
            }
        }
        self.edges.push(Edge {
            from_node: new_id, from_port: 0, to_node: output_id, to_port: 0,
        });
        if let Some(out_pos) = self.node_positions.get(&output_id).copied() {
            self.node_positions.insert(new_id, NodePos { x: out_pos.x - 220.0, y: out_pos.y });
        } else {
            self.node_positions.insert(new_id, NodePos { x: 220.0, y: 0.0 });
        }
        new_id
    }

    /// Find an existing Mask node, or splice a new one in before
    /// Output. Returns its id. Used by the colour-workspace "+ Add
    /// Mask" control so users who don't open the node canvas can still
    /// land a mask shape on the clip.
    /// Ensure a `MaskCutout` sits between the current end-of-chain
    /// node and `Output`, fed by the clip's first `Mask` node's
    /// matte on port 1. Returns `(mask_node_id, cutout_node_id)`.
    ///
    /// Idempotent: if the cutout is already wired, just returns the
    /// existing ids. The colour-workspace "+ Add Mask" flow calls
    /// this so a first-time mask user gets a visible "clip in
    /// shape" result instantly without having to open the node
    /// canvas and rewire manually.
    pub fn ensure_mask_cutout(&mut self) -> (NodeId, NodeId) {
        let output_id = self.output_node_id;
        // Snapshot the original chain end BEFORE ensure_mask runs —
        // ensure_mask rewires `[end] → Output` to `[end] → Mask →
        // Output`, so reading after that returns the Mask, not the
        // actual colour signal we want as MaskCutout's source.
        let original_chain_end: Option<NodeId> = self
            .edges
            .iter()
            .find(|e| e.to_node == output_id && e.to_port == 0)
            .map(|e| e.from_node);

        let mask_id = self.ensure_mask();
        if let Some(idx) = self.nodes.iter().position(|n| n.kind == NodeKind::MaskCutout) {
            return (mask_id, self.nodes[idx].id);
        }

        let cutout_id = NodeId::new_v4();
        let insert_at = self.nodes.iter().position(|n| n.id == output_id)
            .unwrap_or(self.nodes.len());
        self.nodes.insert(insert_at, Node {
            id: cutout_id,
            kind: NodeKind::MaskCutout,
            label: "Mask Cutout".to_string(),
            enabled: true,
            params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        });

        // Drop the Mask→Output feed ensure_mask just laid down — we
        // route Mask through the cutout's matte port instead.
        self.edges.retain(|e| !(e.to_node == output_id && e.to_port == 0));
        // Wire the original chain end into the cutout's source port.
        // If the original was the Mask itself (or Output was unwired),
        // leave port 0 unwired — the executor falls back to passthrough
        // so the user doesn't see a black frame.
        if let Some(src) = original_chain_end.filter(|id| *id != mask_id && *id != cutout_id) {
            self.edges.push(Edge { from_node: src, from_port: 0, to_node: cutout_id, to_port: 0 });
        }
        self.edges.push(Edge { from_node: mask_id, from_port: 0, to_node: cutout_id, to_port: 1 });
        self.edges.push(Edge { from_node: cutout_id, from_port: 0, to_node: output_id, to_port: 0 });

        if let Some(out_pos) = self.node_positions.get(&output_id).copied() {
            self.node_positions.insert(cutout_id, NodePos { x: out_pos.x - 110.0, y: out_pos.y });
        } else {
            self.node_positions.insert(cutout_id, NodePos { x: 330.0, y: 0.0 });
        }
        (mask_id, cutout_id)
    }

    pub fn ensure_mask(&mut self) -> NodeId {
        if let Some(idx) = self.nodes.iter().position(|n| n.kind == NodeKind::Mask) {
            return self.nodes[idx].id;
        }
        let new_id = NodeId::new_v4();
        let output_id = self.output_node_id;
        let insert_at = self.nodes.iter().position(|n| n.id == output_id)
            .unwrap_or(self.nodes.len());
        self.nodes.insert(insert_at, Node {
            id: new_id,
            kind: NodeKind::Mask,
            label: "Mask".to_string(),
            enabled: true,
            params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        for edge in &mut self.edges {
            if edge.to_node == output_id && edge.to_port == 0 {
                edge.to_node = new_id;
                edge.to_port = 0;
            }
        }
        self.edges.push(Edge {
            from_node: new_id, from_port: 0, to_node: output_id, to_port: 0,
        });
        if let Some(out_pos) = self.node_positions.get(&output_id).copied() {
            self.node_positions.insert(new_id, NodePos { x: out_pos.x - 220.0, y: out_pos.y });
        } else {
            self.node_positions.insert(new_id, NodePos { x: 220.0, y: 0.0 });
        }
        new_id
    }

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

    /// Ensure a single Qualifier node (an HSL secondary), wired after the
    /// corrector so it keys and corrects the graded image, and return its
    /// id. If one exists already it is reused.
    /// A secondary: a keyer whose matte gates a correction, which is
    /// the only shape that grades one colour and passes the rest of the
    /// picture through. A Qualifier alone in the chain writes its matte
    /// where the picture should be — it is a matte source, not a grade.
    ///
    /// Builds, or finds, `source → corr(port 0)`, `source → qualifier`,
    /// `qualifier → corr(port 1)`, `corr → output`, and answers with the
    /// qualifier's and the correction's ids. `kind` is the correction's
    /// node kind: `ColorBalance` turns a hue, `Corrector` lifts or
    /// saturates.
    pub fn ensure_masked_secondary(&mut self, kind: NodeKind) -> (NodeId, NodeId) {
        // One already there, keying exactly this kind of correction.
        let found = self.nodes.iter().filter(|n| n.kind == NodeKind::Qualifier).find_map(|q| {
            let consumer = self.edges.iter().find(|e| e.from_node == q.id && e.to_port == 1)?.to_node;
            self.nodes.iter().find(|n| n.id == consumer && n.kind == kind).map(|n| (q.id, n.id))
        });
        if let Some(pair) = found {
            return pair;
        }
        let output_id = self.output_node_id;
        let source = self.input_of(output_id, 0);
        let insert_at = self.nodes.iter().position(|n| n.id == output_id).unwrap_or(self.nodes.len());
        let (qual_id, corr_id) = (NodeId::new_v4(), NodeId::new_v4());
        let make = |id: NodeId, kind: NodeKind, label: &str| Node {
            id,
            kind,
            label: label.to_string(),
            enabled: true,
            params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        };
        self.nodes.insert(insert_at, make(corr_id, kind, "Secondary"));
        self.nodes.insert(insert_at, make(qual_id, NodeKind::Qualifier, "Qualifier"));
        // The correction takes the output's place at the end of the chain.
        for edge in &mut self.edges {
            if edge.to_node == output_id && edge.to_port == 0 {
                edge.to_node = corr_id;
                edge.to_port = 0;
            }
        }
        self.edges.push(Edge { from_node: corr_id, from_port: 0, to_node: output_id, to_port: 0 });
        // The keyer looks at the same picture the correction does.
        if let Some(src) = source {
            self.edges.push(Edge { from_node: src, from_port: 0, to_node: qual_id, to_port: 0 });
        }
        self.edges.push(Edge { from_node: qual_id, from_port: 0, to_node: corr_id, to_port: 1 });
        let base = self.node_positions.get(&output_id).copied().unwrap_or(NodePos { x: 440.0, y: 0.0 });
        self.node_positions.insert(corr_id, NodePos { x: base.x - 160.0, y: base.y });
        self.node_positions.insert(qual_id, NodePos { x: base.x - 320.0, y: base.y - 140.0 });
        (qual_id, corr_id)
    }

    pub fn ensure_qualifier(&mut self) -> NodeId {
        if let Some(idx) = self.nodes.iter().position(|n| n.kind == NodeKind::Qualifier) {
            return self.nodes[idx].id;
        }
        let new_id = NodeId::new_v4();
        let output_id = self.output_node_id;
        let insert_at = self.nodes.iter().position(|n| n.id == output_id)
            .unwrap_or(self.nodes.len());
        self.nodes.insert(insert_at, Node {
            id: new_id,
            kind: NodeKind::Qualifier,
            label: "Qualifier".to_string(),
            enabled: true,
            params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        for edge in &mut self.edges {
            if edge.to_node == output_id && edge.to_port == 0 {
                edge.to_node = new_id;
                edge.to_port = 0;
            }
        }
        self.edges.push(Edge { from_node: new_id, from_port: 0, to_node: output_id, to_port: 0 });
        if let Some(out_pos) = self.node_positions.get(&output_id).copied() {
            self.node_positions.insert(new_id, NodePos { x: out_pos.x - 160.0, y: out_pos.y - 120.0 });
        } else {
            self.node_positions.insert(new_id, NodePos { x: 280.0, y: -120.0 });
        }
        new_id
    }

    /// Ensure a single 3D-LUT node, wired last in the chain (after the
    /// corrector, so a creative look sits on the graded image), and
    /// return its id. If one exists already it is reused.
    pub fn ensure_lut3d(&mut self) -> NodeId {
        if let Some(idx) = self.nodes.iter().position(|n| n.kind == NodeKind::Lut3D) {
            return self.nodes[idx].id;
        }
        let new_id = NodeId::new_v4();
        let output_id = self.output_node_id;
        let insert_at = self.nodes.iter().position(|n| n.id == output_id)
            .unwrap_or(self.nodes.len());
        self.nodes.insert(insert_at, Node {
            id: new_id,
            kind: NodeKind::Lut3D,
            label: "3D LUT".to_string(),
            enabled: true,
            params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        // Whatever fed Output now feeds the LUT; the LUT feeds Output.
        for edge in &mut self.edges {
            if edge.to_node == output_id && edge.to_port == 0 {
                edge.to_node = new_id;
                edge.to_port = 0;
            }
        }
        self.edges.push(Edge { from_node: new_id, from_port: 0, to_node: output_id, to_port: 0 });
        if let Some(out_pos) = self.node_positions.get(&output_id).copied() {
            self.node_positions.insert(new_id, NodePos { x: out_pos.x - 110.0, y: out_pos.y + 120.0 });
        } else {
            self.node_positions.insert(new_id, NodePos { x: 330.0, y: 120.0 });
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
    /// The nodes with a path to the output node, over drawn edges and
    /// wireless links: the grade. A node left in the graph without a way
    /// to the output — dragged in and never wired, or cut out of a chain —
    /// does nothing to the picture, and is not part of what the renderer
    /// or the GPU chain runs.
    pub fn nodes_feeding_output(&self) -> HashSet<NodeId> {
        self.nodes_feeding([self.output_node_id])
    }

    /// The nodes with a path to any of `roots`, the roots included: what
    /// has to run for those to be evaluated. The executor's roots are the
    /// output and any node being tapped for a scope, previewed as a
    /// matte, or thumbnailed — those are looked at even when unwired.
    pub fn nodes_feeding(&self, roots: impl IntoIterator<Item = NodeId>) -> HashSet<NodeId> {
        let mut feeding = HashSet::new();
        let mut stack: Vec<NodeId> = roots.into_iter().collect();
        while let Some(id) = stack.pop() {
            if !feeding.insert(id) {
                continue;
            }
            stack.extend(self.iter_dependencies().filter(|(_, to)| *to == id).map(|(from, _)| from));
        }
        feeding
    }

    /// Returns true if the graph has no active processing nodes on the
    /// way to its output. A node not feeding the output is not counted:
    /// it does nothing to the picture, whatever its settings.
    pub fn is_identity(&self) -> bool {
        let feeding = self.nodes_feeding_output();
        // A keyframed node is not an identity even when its static params
        // are neutral: an exposure ramp starting at 0 or a fade-in grade
        // animates *from* default. Checking static params alone let every
        // identity fast path (viewer and export) skip such a graph, and
        // with it the animation.
        self.nodes
            .iter()
            .all(|n| !n.enabled
                || !feeding.contains(&n.id)
                || matches!(n.kind, NodeKind::Source | NodeKind::Output | NodeKind::InputTransform)
                || (n.params.is_default() && n.keyframes.tracks.iter().all(|t| t.keyframes.is_empty())))
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

    /// Wrap the existing chain with primaries-conversion nodes so it
    /// keeps producing the SAME pixels after the project's working
    /// colorspace flips from `old` to `new`. Returns `true` when the
    /// graph was actually rewritten, `false` when nothing changed
    /// (already migrated, identical from/to, or unsupported pair).
    ///
    /// What gets inserted:
    ///
    /// ```text
    /// Source → ... → Output
    /// ```
    ///
    /// becomes
    ///
    /// ```text
    /// Source → PM(new→old) → ... → PM(old→new) → Output
    /// ```
    ///
    /// and `working_space_override` is pinned to `old` so every
    /// luma-keyed op inside the chain still uses the basis the grade
    /// was tuned against.
    ///
    /// Idempotent: running it twice on the same graph is a no-op
    /// because the `working_space_override.is_some()` gate trips on
    /// the second call.
    pub fn migrate_working_space(
        &mut self,
        old: klipp_core::WorkingColorspace,
        new: klipp_core::WorkingColorspace,
    ) -> bool {
        if old == new {
            return false;
        }
        // A pair with no matrix in both directions. Every pair of the
        // working spaces that exist today has one; this stays as the
        // guard for whatever is added next, because a passthrough node
        // would silently corrupt the look — better to skip migration and
        // let the caller surface a "look will shift" warning.
        if klipp_color::primaries::working_space_conversion_matrix(old, new).is_none() {
            return false;
        }
        if klipp_color::primaries::working_space_conversion_matrix(new, old).is_none() {
            return false;
        }
        if self.working_space_override.is_some() {
            return false;
        }

        let output_id = self.output_node_id;
        let Some(source_id) = self
            .nodes
            .iter()
            .find(|n| n.kind == crate::kinds::NodeKind::Source)
            .map(|n| n.id)
        else {
            return false;
        };

        // Head wrapper: PM(new → old). Sits between Source and the
        // current chain heads. New project ws flows in (the IDT now
        // delivers `new`), wrapper converts it back to the basis the
        // chain was tuned against.
        let head_id = NodeId::new_v4();
        let head_params = NodeParams {
            primaries_from: Some(new),
            primaries_to: Some(old),
            ..Default::default()
        };
        self.nodes.push(Node {
            id: head_id,
            kind: crate::kinds::NodeKind::PrimariesMatrix,
            label: "Migration: input".into(),
            enabled: true,
            params: head_params,
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        for edge in &mut self.edges {
            if edge.from_node == source_id {
                edge.from_node = head_id;
            }
        }
        self.edges.push(Edge {
            from_node: source_id,
            from_port: 0,
            to_node: head_id,
            to_port: 0,
        });

        // Tail wrapper: PM(old → new). Sits between the current chain
        // tails and Output. Brings the graded pixels back into the
        // new project ws so anything downstream (Output, exporters,
        // viewer ODT) sees pixels in the correct basis.
        let tail_id = NodeId::new_v4();
        let tail_params = NodeParams {
            primaries_from: Some(old),
            primaries_to: Some(new),
            ..Default::default()
        };
        self.nodes.push(Node {
            id: tail_id,
            kind: crate::kinds::NodeKind::PrimariesMatrix,
            label: "Migration: output".into(),
            enabled: true,
            params: tail_params,
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        for edge in &mut self.edges {
            if edge.to_node == output_id {
                edge.to_node = tail_id;
            }
        }
        self.edges.push(Edge {
            from_node: tail_id,
            from_port: 0,
            to_node: output_id,
            to_port: 0,
        });

        self.working_space_override = Some(old);

        // Best-effort layout: place head 110 px right of Source, tail
        // 110 px left of Output. Doesn't bother nudging downstream
        // nodes because the editor's auto-layout pass will tidy the
        // graph on next open.
        if let Some(src_pos) = self.node_positions.get(&source_id).copied() {
            self.node_positions.insert(
                head_id,
                NodePos {
                    x: src_pos.x + 110.0,
                    y: src_pos.y,
                },
            );
        }
        if let Some(out_pos) = self.node_positions.get(&output_id).copied() {
            self.node_positions.insert(
                tail_id,
                NodePos {
                    x: out_pos.x - 110.0,
                    y: out_pos.y,
                },
            );
        }

        true
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
            .map(|(old_id, pos)| (id_map[old_id], *pos))
            .collect();
        new_graph
    }
}

/// Walk back through any chain of `NodeKind::Reroute` to find the
/// actual signal-producing source — i.e. what kind of signal is
/// flowing OUT of `(node_id, port)`. For non-Reroute nodes this is
/// just `node.kind.output_port_role(port)`. For Reroute, the role
/// is whatever feeds the Reroute's input port 0; chained Reroutes
/// follow the chain transitively.
///
/// Used by the V2 editor's wire/port colour-coding so a Reroute
/// dropped into a matte chain doesn't visually break the magenta
/// run with an amber stub. Returns `PortRole::Primary` when the
/// chain hits a disconnected Reroute, an unknown node, or the hop
/// cap (cycle protection — the engine's chain-builder rejects
/// cycles upstream, but the cap keeps this fn safe to call from
/// any UI context).
pub fn effective_signal_role_at(graph: &NodeGraph, node_id: NodeId, port: u8) -> PortRole {
    const MAX_HOPS: usize = 32;
    let mut current_node = node_id;
    let mut current_port = port;
    for _ in 0..MAX_HOPS {
        let Some(n) = graph.nodes.iter().find(|n| n.id == current_node) else {
            return PortRole::Primary;
        };
        if n.kind != NodeKind::Reroute {
            return n.kind.output_port_role(current_port);
        }
        // Reroute — walk back to the upstream source via input port 0.
        let Some(upstream) = graph.edges.iter()
            .find(|e| e.to_node == current_node && e.to_port == 0)
        else {
            return PortRole::Primary; // disconnected reroute
        };
        current_node = upstream.from_node;
        current_port = upstream.from_port;
    }
    PortRole::Primary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::default_graph;
    use crate::kinds::{NodeKind, PortRole};
    use klipp_core::WorkingColorspace;

    fn count_kind(g: &NodeGraph, kind: NodeKind) -> usize {
        g.nodes.iter().filter(|n| n.kind == kind).count()
    }

    /// The default graph is an identity until a node is keyframed — even
    /// keyframed starting from its neutral value.
    #[test]
    fn a_keyframed_node_is_not_an_identity() {
        let mut g = default_graph();
        assert!(g.is_identity());
        let n = g.nodes.iter_mut().find(|n| n.kind == NodeKind::Corrector).expect("default graph has a Corrector");
        let mut keys = Vec::new();
        klipp_core::keyframe::upsert_keyframe(&mut keys, 0.0, 0.0);
        klipp_core::keyframe::upsert_keyframe(&mut keys, 2.0, 2.0);
        n.keyframes.tracks.push(crate::params::NodeKeyframeTrack { param_name: "exposure".into(), keyframes: keys });
        assert!(n.params.is_default(), "static params stay neutral");
        assert!(!g.is_identity(), "a node animating from neutral must still run");
    }

    /// A node left unwired does nothing to the picture: the graph is still
    /// an identity, and the node is not among those feeding the output.
    /// It used to count, so a stray node with a setting sent every frame
    /// of the clip through the full render path — and the GPU chain,
    /// finding a node with no input, refused the graph altogether.
    #[test]
    fn a_node_not_wired_to_the_output_is_not_part_of_the_grade() {
        let mut g = default_graph();
        let mut stray = g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).expect("a Corrector").clone();
        stray.id = NodeId::new_v4();
        stray.params.exposure = Some(1.0);
        g.nodes.push(stray.clone());
        assert!(!g.nodes_feeding_output().contains(&stray.id));
        assert!(g.is_identity(), "a stray node with a setting is not a grade");
        // Wired in, it is.
        let output = g.output_node_id;
        let into_output = g.edges.iter().position(|e| e.to_node == output).expect("something feeds the output");
        let before = g.edges[into_output].from_node;
        g.edges[into_output].from_node = stray.id;
        g.edges.push(Edge { from_node: before, from_port: 0, to_node: stray.id, to_port: 0 });
        assert!(g.nodes_feeding_output().contains(&stray.id));
        assert!(!g.is_identity());
    }

    #[test]
    fn migrate_no_op_when_old_equals_new() {
        let mut g = default_graph();
        let nodes_before = g.nodes.len();
        let edges_before = g.edges.len();
        let did = g.migrate_working_space(WorkingColorspace::Rec709Linear, WorkingColorspace::Rec709Linear);
        assert!(!did);
        assert_eq!(g.nodes.len(), nodes_before);
        assert_eq!(g.edges.len(), edges_before);
        assert!(g.working_space_override.is_none());
    }

    #[test]
    fn migrate_rec709_to_acescg_inserts_two_primaries_matrix_nodes() {
        let mut g = default_graph();
        let nodes_before = g.nodes.len();
        let did = g.migrate_working_space(WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG);
        assert!(did);
        assert_eq!(count_kind(&g, NodeKind::PrimariesMatrix), 2);
        assert_eq!(g.nodes.len(), nodes_before + 2);
        assert_eq!(g.working_space_override, Some(WorkingColorspace::Rec709Linear));
        assert!(g.topo_sort().is_ok(), "migrated graph must remain acyclic");
    }

    #[test]
    fn migrate_is_idempotent() {
        let mut g = default_graph();
        let _ = g.migrate_working_space(WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG);
        let nodes_after_first = g.nodes.len();
        let did_again = g.migrate_working_space(WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG);
        assert!(!did_again, "second migrate must be a no-op");
        assert_eq!(g.nodes.len(), nodes_after_first);
    }

    #[test]
    fn migrate_wraps_the_bt2020_pairs_like_any_other() {
        // BT.2020 is a working space in its own right now, so switching a
        // project into or out of it preserves the look the same way the
        // ACES pairs do — wrapper nodes, not a refusal. While it was a
        // stub the matrix helper returned None and migration skipped.
        for (from, to) in [
            (WorkingColorspace::Rec709Linear, WorkingColorspace::Bt2020Linear),
            (WorkingColorspace::Bt2020Linear, WorkingColorspace::Rec709Linear),
            (WorkingColorspace::AcesCG, WorkingColorspace::Bt2020Linear),
            (WorkingColorspace::Bt2020Linear, WorkingColorspace::AcesCG),
        ] {
            let mut g = default_graph();
            let nodes_before = g.nodes.len();
            assert!(g.migrate_working_space(from, to), "{from:?} → {to:?} refused to migrate");
            assert_eq!(count_kind(&g, NodeKind::PrimariesMatrix), 2, "{from:?} → {to:?}");
            assert_eq!(g.nodes.len(), nodes_before + 2, "{from:?} → {to:?}");
            assert_eq!(g.working_space_override, Some(from), "{from:?} → {to:?}");
            assert!(g.topo_sort().is_ok(), "{from:?} → {to:?} left a cycle");
        }
    }

    #[test]
    fn migrate_routes_source_through_head_pm_and_output_after_tail_pm() {
        let mut g = default_graph();
        let source_id = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
        let output_id = g.output_node_id;
        let _ = g.migrate_working_space(WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG);
        // Source's only outgoing edge must land on a PrimariesMatrix.
        let head_targets: Vec<_> = g.edges.iter()
            .filter(|e| e.from_node == source_id)
            .map(|e| e.to_node).collect();
        assert_eq!(head_targets.len(), 1);
        let head_node = g.nodes.iter().find(|n| n.id == head_targets[0]).unwrap();
        assert_eq!(head_node.kind, NodeKind::PrimariesMatrix);
        assert_eq!(head_node.params.primaries_from, Some(WorkingColorspace::AcesCG));
        assert_eq!(head_node.params.primaries_to, Some(WorkingColorspace::Rec709Linear));
        // Output's only incoming edge must come from a PrimariesMatrix.
        let tail_sources: Vec<_> = g.edges.iter()
            .filter(|e| e.to_node == output_id)
            .map(|e| e.from_node).collect();
        assert_eq!(tail_sources.len(), 1);
        let tail_node = g.nodes.iter().find(|n| n.id == tail_sources[0]).unwrap();
        assert_eq!(tail_node.kind, NodeKind::PrimariesMatrix);
        assert_eq!(tail_node.params.primaries_from, Some(WorkingColorspace::Rec709Linear));
        assert_eq!(tail_node.params.primaries_to, Some(WorkingColorspace::AcesCG));
    }

    fn make_node(kind: NodeKind) -> Node {
        Node {
            id: NodeId::new_v4(),
            kind,
            label: kind.display_name().to_string(),
            enabled: true,
            params: Default::default(),
            keyframes: Default::default(),
            output_link: None,
            input_links: Vec::new(),
        }
    }

    #[test]
    fn reroute_inherits_upstream_matte_role() {
        // Qualifier (matte producer) → Reroute → Reroute → ApplyMatte
        // The two intermediate Reroute outputs should both report
        // PortRole::Matte rather than the static Primary fallback.
        let mut g = NodeGraph::default();
        let qual = make_node(NodeKind::Qualifier);
        let r1 = make_node(NodeKind::Reroute);
        let r2 = make_node(NodeKind::Reroute);
        let qual_id = qual.id;
        let r1_id = r1.id;
        let r2_id = r2.id;
        g.nodes = vec![qual, r1, r2];
        g.edges = vec![
            Edge { from_node: qual_id, from_port: 0, to_node: r1_id,  to_port: 0 },
            Edge { from_node: r1_id,   from_port: 0, to_node: r2_id,  to_port: 0 },
        ];

        // Direct from Qualifier — static role wins (Matte).
        assert_eq!(
            effective_signal_role_at(&g, qual_id, 0),
            PortRole::Matte,
        );
        // Reroute output — walks back through the chain, lands on Matte.
        assert_eq!(
            effective_signal_role_at(&g, r1_id, 0),
            PortRole::Matte,
        );
        assert_eq!(
            effective_signal_role_at(&g, r2_id, 0),
            PortRole::Matte,
        );
    }

    #[test]
    fn reroute_disconnected_falls_back_to_primary() {
        let mut g = NodeGraph::default();
        let r = make_node(NodeKind::Reroute);
        let r_id = r.id;
        g.nodes = vec![r];
        // No edges — Reroute has nothing feeding it.
        assert_eq!(
            effective_signal_role_at(&g, r_id, 0),
            PortRole::Primary,
        );
    }

    #[test]
    fn reroute_chain_cycle_capped() {
        // Pathological: R1 → R2 → R1 (the engine validator rejects
        // this on the executor side, but the role-walker must still
        // terminate). We just verify the function returns without
        // hanging — the actual role result on a cycle is unspecified
        // (Primary is what we've documented).
        let mut g = NodeGraph::default();
        let r1 = make_node(NodeKind::Reroute);
        let r2 = make_node(NodeKind::Reroute);
        let r1_id = r1.id;
        let r2_id = r2.id;
        g.nodes = vec![r1, r2];
        g.edges = vec![
            Edge { from_node: r1_id, from_port: 0, to_node: r2_id, to_port: 0 },
            Edge { from_node: r2_id, from_port: 0, to_node: r1_id, to_port: 0 },
        ];
        let _ = effective_signal_role_at(&g, r1_id, 0); // must not loop
    }
}
