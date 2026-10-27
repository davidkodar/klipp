use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::kinds::{NodeKind, PortRole};
use crate::params::{NodeKeyframeTracks, NodeParams};

pub type NodeId = Uuid;

/// A directed acyclic graph of color processing nodes.
/// Replaces ClipColorGrade entirely.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
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
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
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
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
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
    /// Members. A set, so a node is in it at most once, and an ordered one,
    /// so it serialises the same in every process: a clip's render-cache
    /// key hashes it (#1132). Its order means nothing. Cleanup invariant:
    /// when a node is removed from the graph, its id is dropped from every
    /// frame's set; frames that end up empty are pruned.
    pub contained_node_ids: std::collections::BTreeSet<NodeId>,
    /// The recipe whose nodes the frame holds (#2758), with its live
    /// controls; `None` for a frame drawn by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<klipp_core::RecipeInstance>,
}

pub type NodeFrameId = uuid::Uuid;

impl Default for NodeGraph {
    fn default() -> Self {
        crate::defaults::default_graph()
    }
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
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
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from_node: NodeId,
    pub from_port: u8,
    pub to_node: NodeId,
    pub to_port: u8,
}

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
        self.insert_corrector(NodeId::new_v4())
    }

    /// Add an enabled Corrector under `new_id` where [`Self::ensure_corrector`]
    /// puts one, whatever Correctors the graph holds already: between the
    /// Output and what fed it, placed just before the Output on the canvas.
    pub fn insert_corrector(&mut self, new_id: NodeId) -> NodeId {
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
            // The key's matte goes to the correction's port 1 — or, once
            // the secondary has been given a window, to a combiner whose
            // own output goes there. Both shapes are the same secondary,
            // so following through the combiner is what stops a second
            // one being built over the first.
            let mut consumer = self.edges.iter().find(|e| e.from_node == q.id && e.to_port <= 1)?.to_node;
            if self.nodes.iter().any(|n| n.id == consumer && n.kind == NodeKind::CombineMattes) {
                consumer = self.edges.iter().find(|e| e.from_node == consumer && e.to_port == 1)?.to_node;
            }
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

    /// A secondary held inside a window: the keyer's matte *and* a
    /// shape, so the correction reaches only what is both the right
    /// colour and in the right place.
    ///
    /// This is what a hue key alone cannot do, and the reason every
    /// grading suite has power windows. A skin key is a band of hue,
    /// and a red jumper, a brick wall, a wooden table and a tanned hand
    /// are all inside that band: pull the face warmer and they all turn
    /// with it. A window says *where*, the key says *what*, and the two
    /// together say "this face". Tracked across the shot, it stays on
    /// the face while the subject moves — which is the only way a
    /// window survives anything but a locked-off camera.
    ///
    /// Builds, or finds, `source → corr(0)`, `source → qualifier`,
    /// `qualifier → combine(0)`, `mask → combine(1)`,
    /// `combine → corr(1)`, `corr → output`, and answers with the
    /// qualifier's, the mask's and the correction's ids. The combiner
    /// intersects by default, which is what "both" means.
    pub fn ensure_windowed_secondary(&mut self, kind: NodeKind) -> (NodeId, NodeId, NodeId) {
        let (qual_id, corr_id) = self.ensure_masked_secondary(kind);
        // Already windowed: a combiner on the correction's matte port
        // with a Mask feeding it.
        let existing = self
            .edges
            .iter()
            .find(|e| e.to_node == corr_id && e.to_port == 1)
            .map(|e| e.from_node)
            .and_then(|combine| {
                let is_combine = self.nodes.iter().any(|n| n.id == combine && n.kind == NodeKind::CombineMattes);
                if !is_combine {
                    return None;
                }
                let mask = self.edges.iter().find(|e| e.to_node == combine && e.to_port == 1)?.from_node;
                self.nodes.iter().find(|n| n.id == mask && n.kind == NodeKind::Mask).map(|n| n.id)
            });
        if let Some(mask_id) = existing {
            return (qual_id, mask_id, corr_id);
        }
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
        let (mask_id, combine_id) = (NodeId::new_v4(), NodeId::new_v4());
        let insert_at = self.nodes.iter().position(|n| n.id == corr_id).unwrap_or(self.nodes.len());
        self.nodes.insert(insert_at, make(combine_id, NodeKind::CombineMattes, "Key ∩ Window"));
        self.nodes.insert(insert_at, make(mask_id, NodeKind::Mask, "Window"));
        // The keyer's matte now goes into the combiner instead of
        // straight to the correction.
        for edge in &mut self.edges {
            if edge.from_node == qual_id && edge.to_node == corr_id && edge.to_port == 1 {
                edge.to_node = combine_id;
                edge.to_port = 0;
            }
        }
        self.edges.push(Edge { from_node: mask_id, from_port: 0, to_node: combine_id, to_port: 1 });
        self.edges.push(Edge { from_node: combine_id, from_port: 0, to_node: corr_id, to_port: 1 });
        let base = self.node_positions.get(&corr_id).copied().unwrap_or(NodePos { x: 440.0, y: 0.0 });
        self.node_positions.insert(combine_id, NodePos { x: base.x - 160.0, y: base.y - 140.0 });
        self.node_positions.insert(mask_id, NodePos { x: base.x - 320.0, y: base.y - 260.0 });
        (qual_id, mask_id, corr_id)
    }

    /// Set a window's shape to a circle following `track` — each entry
    /// a time in clip seconds and a box in frame fractions
    /// `[x, y, w, h]`, as a face detector gives them. `aspect` is the
    /// frame's height over its width (0.5625 for 16:9).
    ///
    /// One entry is a still window. Several are keyframed, so the window
    /// stays on a subject who moves; without that a window is only good
    /// for a locked-off camera, and a face that walks out of it takes
    /// the correction off with it half way through the shot.
    ///
    /// `MaskShape::Circle` measures its radius in frame *widths*, both
    /// ways (#663): it reaches `radius` across and `radius / aspect` of
    /// the height down. A face box is usually taller than it is wide, so
    /// whichever of the two half-extents needs the bigger radius wins.
    ///
    /// The window is padded past the face and feathered generously,
    /// because a hard-edged window on a face reads as a sticker the
    /// moment the subject moves — a colourist would soften it until the
    /// edge cannot be found by looking.
    pub fn set_window_track(&mut self, mask_id: NodeId, track: &[(f64, [f32; 4])], aspect: f32, feather: f32) {
        use klipp_core::color_grade::{Mask, MaskShape, MaskStack};
        use klipp_core::keyframe::Keyframe;
        let Some(node) = self.nodes.iter_mut().find(|n| n.id == mask_id) else { return };
        let Some(first) = track.first() else { return };
        let aspect = if aspect.is_finite() && aspect > 0.01 { aspect } else { 9.0 / 16.0 };
        let centre = |b: [f32; 4]| (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0);
        // A quarter past the face on every side: the detector's box is
        // the face, and a grade that stops at the jaw is a mask.
        const PAD: f32 = 1.25;
        let radius = |b: [f32; 4]| ((b[2] / 2.0).max(b[3] * aspect / 2.0) * PAD).clamp(0.02, 1.2);
        let (cx, cy) = centre(first.1);
        let mut mask = Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
            shape: MaskShape::Circle { center_x: cx, center_y: cy, radius: radius(first.1) },
            feather: feather.clamp(0.0, 1.0),
            invert: false,
            opacity: 1.0,
            enabled: true,
            keyframes: Default::default(),
        };
        if track.len() > 1 {
            for (t, b) in track {
                let (cx, cy) = centre(*b);
                let key = |v: f32| Keyframe {
                    time_secs: *t,
                    value: v,
                    // A face moves smoothly; a stepped window would jump
                    // between samples and read as a glitch.
                    interpolation: klipp_core::keyframe::Interpolation::Linear,
                    tangent_out: (0.0, 0.0),
                    tangent_in: (0.0, 0.0),
                    handles: Default::default(),
                };
                mask.keyframes.center_x.push(key(cx));
                mask.keyframes.center_y.push(key(cy));
                mask.keyframes.radius.push(key(radius(*b)));
            }
        }
        node.params.masks = Some(MaskStack { masks: vec![mask] });
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
    /// Effective input-port count, accounting for nodes (`LayerMixer`,
    /// `KeyMixer`) whose port count is data-driven by their params
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
            NodeKind::KeyMixer => self
                .params
                .key_mixer
                .as_ref()
                .map(|m| m.inputs.len() as u8)
                .unwrap_or(0)
                .max(static_count),
            _ => static_count,
        }
    }

    /// Grow a mixer's settings (`LayerMixer`, `KeyMixer`) to cover input
    /// `port`, as a wire dropped on it does, so the executor finds a
    /// setting at that index. Other kinds are left as they are.
    pub fn grow_to_port(&mut self, port: u8) {
        let needed = usize::from(port) + 1;
        match self.kind {
            NodeKind::LayerMixer => {
                let mixer = self.params.layer_mixer.get_or_insert_with(Default::default);
                if mixer.inputs.len() < needed {
                    mixer.inputs.resize_with(needed, klipp_core::LayerMixerInput::default);
                }
            }
            NodeKind::KeyMixer => {
                let mixer = self.params.key_mixer.get_or_insert_with(Default::default);
                if mixer.inputs.len() < needed {
                    mixer.inputs.resize_with(needed, crate::params::KeyMixerInput::default);
                }
            }
            _ => {}
        }
    }
}

/// Screen position for the node editor UI.
#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct NodePos {
    pub x: f32,
    pub y: f32,
}

/// Error returned when a cycle is detected in the graph.
#[derive(Debug, Clone, thiserror::Error)]
#[error("cycle detected in node graph")]
pub struct CycleError;

/// Why [`NodeGraph::append`] would not run one graph after another.
/// Worded for the person who asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CannotAppend {
    #[error("the grade to add has no Source node")]
    NoSource,
    #[error("nothing feeds this grade's Output, so there is nowhere to add after")]
    NothingFeedsOutput,
    #[error("the grade to add is pinned to the {theirs:?} working space and this one to {ours:?}; convert one first")]
    WorkingSpace { ours: klipp_core::WorkingColorspace, theirs: klipp_core::WorkingColorspace },
}

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
        // A matte into the Output's alpha (#898) cuts the picture, whatever
        // the nodes making it hold.
        if self.input_of(self.output_node_id, 1).is_some() {
            return false;
        }
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

    /// Splice a clip's resolved effects in before the output, one node
    /// per group in order (phase 2b M2, step 10b): `Effect` for one
    /// effect, `EffectSegment` for effects that run together. `id(i)`
    /// names group `i`'s node; ids stable across frames keep the
    /// executor's cached plan. The edge into the output wins over a
    /// wireless link there, as it does everywhere.
    pub fn splice_effects_before_output(&mut self, groups: &[(NodeKind, &[klipp_effects::BuiltinEffect])], id: impl Fn(usize) -> NodeId) {
        let out = self.output_node_id;
        let Some(mut prev) = self.input_of(out, 0) else { return };
        self.edges.retain(|e| !(e.to_node == out && e.to_port == 0));
        for (i, (kind, effects)) in groups.iter().enumerate() {
            let node = Node {
                id: id(i),
                kind: *kind,
                label: String::new(),
                enabled: true,
                params: crate::params::NodeParams { effects: effects.to_vec(), ..Default::default() },
                keyframes: Default::default(),
                output_link: None,
                input_links: Vec::new(),
            };
            self.edges.push(Edge { from_node: prev, from_port: 0, to_node: node.id, to_port: 0 });
            prev = node.id;
            self.nodes.push(node);
        }
        self.edges.push(Edge { from_node: prev, from_port: 0, to_node: out, to_port: 0 });
    }

    /// Put `node` straight after the graph's Source: everything the
    /// source fed, it feeds. A graph without a Source is left alone.
    pub fn splice_after_source(&mut self, node: Node) {
        let Some(source) = self.nodes.iter().find(|n| n.kind == NodeKind::Source).map(|n| n.id) else { return };
        for e in self.edges.iter_mut().filter(|e| e.from_node == source) {
            e.from_node = node.id;
        }
        self.edges.push(Edge { from_node: source, from_port: 0, to_node: node.id, to_port: 0 });
        self.nodes.push(node);
    }

    /// Splice one node in before the output, between it and what fed it
    /// (port 0 in and out): how the renderer adds a run's own step, such
    /// as a scene-referred clip's display step (step 11c).
    pub fn splice_before_output(&mut self, node: Node) {
        let out = self.output_node_id;
        let Some(prev) = self.input_of(out, 0) else { return };
        self.edges.retain(|e| !(e.to_node == out && e.to_port == 0));
        self.edges.push(Edge { from_node: prev, from_port: 0, to_node: node.id, to_port: 0 });
        self.edges.push(Edge { from_node: node.id, from_port: 0, to_node: out, to_port: 0 });
        self.nodes.push(node);
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

    /// Take every Input Transform node out, wiring what fed each one to
    /// what it fed, so the rest runs unchanged. The Input Transform is
    /// the clip's input colour space, not part of its grade: a grade
    /// carried from one clip to another — a still's, a Look — must leave
    /// it behind, or log footage is linearised twice, or with the other
    /// camera's curve. What fed it may itself be a wireless link, and a
    /// node reading it wirelessly reads its feed instead, as a drawn
    /// edge, since a wireless link carries only an output's port 0 and
    /// the feed need not be one. A frame the removal leaves with no
    /// members goes with it.
    pub fn strip_input_transforms(&mut self) {
        let transforms: Vec<NodeId> =
            self.nodes.iter().filter(|n| n.kind == NodeKind::InputTransform).map(|n| n.id).collect();
        for it in transforms {
            let feed = self
                .edges
                .iter()
                .find(|e| e.to_node == it && e.to_port == 0)
                .map(|e| (e.from_node, e.from_port))
                .or_else(|| self.input_of(it, 0).map(|sender| (sender, 0)));
            let label = self.nodes.iter().find(|n| n.id == it).and_then(|n| n.output_link.clone());
            self.edges.retain(|e| e.to_node != it);
            match feed {
                Some((from_node, from_port)) => {
                    for e in self.edges.iter_mut().filter(|e| e.from_node == it) {
                        e.from_node = from_node;
                        e.from_port = from_port;
                    }
                }
                None => self.edges.retain(|e| e.from_node != it),
            }
            if let Some(label) = label {
                let mut drawn = Vec::new();
                for recv in self.nodes.iter_mut().filter(|n| n.id != it) {
                    for (port, slot) in recv.input_links.iter_mut().enumerate() {
                        if slot.as_ref() != Some(&label) {
                            continue;
                        }
                        *slot = None;
                        let port = port as u8;
                        let wired = self.edges.iter().any(|e| e.to_node == recv.id && e.to_port == port);
                        if let Some((from_node, from_port)) = feed.filter(|_| !wired) {
                            drawn.push(Edge { from_node, from_port, to_node: recv.id, to_port: port });
                        }
                    }
                }
                self.edges.extend(drawn);
            }
            self.nodes.retain(|n| n.id != it);
            self.node_positions.remove(&it);
            self.frames.retain_mut(|f| !f.contained_node_ids.remove(&it) || !f.contained_node_ids.is_empty());
        }
    }

    /// `other`'s grade in place of this one: `other`'s nodes with fresh
    /// ids, less any Input Transform of its own, and this graph's Input
    /// Transform kept, right after Source. The input colour space
    /// belongs to the clip and stays; the grade is what travels, as a
    /// still's grade or a Look applied to another shot.
    pub fn with_grade_of(&self, other: &NodeGraph) -> NodeGraph {
        self.with_grade_of_mapped(other).0
    }

    /// [`Self::with_grade_of`], and the id each of `other`'s nodes took,
    /// for what is kept by node id beside a grade (its colour labels,
    /// #1866). A node left behind (an Input Transform) is in it too.
    pub fn with_grade_of_mapped(&self, other: &NodeGraph) -> (NodeGraph, HashMap<NodeId, NodeId>) {
        let (new, map) = other.remap_ids_mapped();
        (self.with_remapped_grade(new), map)
    }

    fn with_remapped_grade(&self, mut new: NodeGraph) -> NodeGraph {
        new.strip_input_transforms();
        let Some(own) = self.nodes.iter().find(|n| n.kind == NodeKind::InputTransform) else { return new };
        let Some(src) = new.nodes.iter().find(|n| n.kind == NodeKind::Source).map(|n| n.id) else { return new };
        let mut transform = own.clone();
        transform.id = NodeId::new_v4();
        transform.output_link = None;
        transform.input_links.clear();
        for e in new.edges.iter_mut().filter(|e| e.from_node == src) {
            e.from_node = transform.id;
            e.from_port = 0;
        }
        new.edges.push(Edge { from_node: src, from_port: 0, to_node: transform.id, to_port: 0 });
        if let Some(p) = new.node_positions.get(&src).copied() {
            new.node_positions.insert(transform.id, NodePos { x: p.x + 110.0, y: p.y });
        }
        let at = new.nodes.iter().position(|n| n.id == src).map_or(0, |i| i + 1);
        new.nodes.insert(at, transform);
        new
    }

    /// Run `other` after this graph: its nodes, with fresh ids, go
    /// between whatever fed this graph's Output and the Output, wired as
    /// they were between its own Source and Output, and its frames and
    /// notes come along, laid out to the right. This graph's nodes stay
    /// as they are, so a look put on after a clip's balance keeps the
    /// balance. `other`'s Input Transform, if it has one, is left behind
    /// (see [`Self::strip_input_transforms`]): this graph's own already
    /// decoded the footage.
    ///
    /// Refused, and nothing changed, when either graph has no
    /// Source-to-Output path to splice into, or when the two are pinned
    /// to different working spaces: `other`'s luma-weighted nodes would
    /// run in this graph's space, a colour shift. A graph pinned to
    /// nothing follows the project, and joins or takes a pinned one as
    /// it is; the pin, if any, stays this graph's.
    pub fn append(&mut self, other: &NodeGraph) -> Result<(), CannotAppend> {
        self.splice_after(other.remap_ids())
    }

    /// [`Self::append`], and the id each of `other`'s nodes took (see
    /// [`Self::with_grade_of_mapped`]): its Source, Output and Input
    /// Transform are in it though they don't come along.
    pub fn append_mapped(&mut self, other: &NodeGraph) -> Result<HashMap<NodeId, NodeId>, CannotAppend> {
        let (new, map) = other.remap_ids_mapped();
        self.splice_after(new).map(|()| map)
    }

    /// [`Self::append`] with `other`'s node ids kept: a group's post-clip
    /// grade run after each member clip's own (#237). The same node then
    /// sits in every member's graph, so a change to it, a slider drag
    /// included, finds it by its id in all of them. `other`'s ids must not
    /// be this graph's, which ids made fresh for each node never are.
    pub fn append_keeping_ids(&mut self, other: &NodeGraph) -> Result<(), CannotAppend> {
        self.splice_after(other.clone())
    }

    fn working_spaces_agree(&self, other: &NodeGraph) -> Result<(), CannotAppend> {
        match (self.working_space_override, other.working_space_override) {
            (Some(ours), Some(theirs)) if ours != theirs => Err(CannotAppend::WorkingSpace { ours, theirs }),
            _ => Ok(()),
        }
    }

    fn splice_after(&mut self, mut other: NodeGraph) -> Result<(), CannotAppend> {
        self.working_spaces_agree(&other)?;
        other.strip_input_transforms();
        let Some(src) = other.nodes.iter().find(|n| n.kind == NodeKind::Source).map(|n| n.id) else {
            return Err(CannotAppend::NoSource);
        };
        let o_out = other.output_node_id;
        let out = self.output_node_id;
        let Some(feed) = self.edges.iter().find(|e| e.to_node == out && e.to_port == 0).map(|e| (e.from_node, e.from_port)) else {
            return Err(CannotAppend::NothingFeedsOutput);
        };
        let middle: Vec<Node> = other.nodes.iter().filter(|n| n.id != src && n.id != o_out).cloned().collect();
        if middle.is_empty() {
            // An identity graph: nothing to run after.
            return Ok(());
        }
        // Laid out to the right of what is there, so the two read in order.
        let right = self.node_positions.values().map(|p| p.x).fold(f32::MIN, f32::max);
        let left = middle.iter().filter_map(|n| other.node_positions.get(&n.id)).map(|p| p.x).fold(f32::MAX, f32::min);
        let shift = if right.is_finite() && left.is_finite() { right - left + 140.0 } else { 0.0 };
        for n in &middle {
            if let Some(p) = other.node_positions.get(&n.id) {
                self.node_positions.insert(n.id, NodePos { x: p.x + shift, y: p.y });
            }
        }
        // Past our Output's own position too, which the shift was taken from.
        if let Some(p) = self.node_positions.get_mut(&out) {
            p.x += middle.len() as f32 * 140.0;
        }
        self.edges.retain(|e| !(e.to_node == out && e.to_port == 0));
        let insert_at = self.nodes.iter().position(|n| n.id == out).unwrap_or(self.nodes.len());
        let spliced: HashSet<NodeId> = middle.iter().map(|n| n.id).collect();
        self.nodes.splice(insert_at..insert_at, middle);
        for e in &other.edges {
            if e.to_node == src {
                continue;
            }
            let (from_node, from_port) = if e.from_node == src { feed } else { (e.from_node, e.from_port) };
            let to_node = if e.to_node == o_out { out } else { e.to_node };
            self.edges.push(Edge { from_node, from_port, to_node, to_port: e.to_port });
        }
        // A frame around the other graph's Source or Output has no such
        // node here: it keeps the members that came.
        for mut frame in other.frames {
            frame.contained_node_ids.retain(|id| spliced.contains(id));
            if !frame.contained_node_ids.is_empty() {
                self.frames.push(frame);
            }
        }
        for mut note in other.sticky_notes {
            note.pos.x += shift;
            self.sticky_notes.push(note);
        }
        Ok(())
    }

    /// Run `other` before this graph's grade, its node ids kept: a group's
    /// pre-clip grade (#237). Its nodes go between the footage as this
    /// graph decodes it — the Source, or the Input Transform the Source
    /// feeds — and everything that read it, so the clip's own grade, and
    /// any node of it keyed on the footage, reads the group's balance.
    /// `other`'s own Input Transform is left behind: this graph's decodes.
    /// Refused, and nothing changed, as [`Self::append`] refuses.
    pub fn prepend_keeping_ids(&mut self, other: &NodeGraph) -> Result<(), CannotAppend> {
        self.working_spaces_agree(other)?;
        let mut other = other.clone();
        other.strip_input_transforms();
        let Some(o_src) = other.nodes.iter().find(|n| n.kind == NodeKind::Source).map(|n| n.id) else {
            return Err(CannotAppend::NoSource);
        };
        let Some(src) = self.nodes.iter().find(|n| n.kind == NodeKind::Source).map(|n| n.id) else {
            return Err(CannotAppend::NoSource);
        };
        let o_out = other.output_node_id;
        let middle: Vec<Node> = other.nodes.iter().filter(|n| n.id != o_src && n.id != o_out).cloned().collect();
        if middle.is_empty() {
            return Ok(());
        }
        let Some(o_feed) = other.edges.iter().find(|e| e.to_node == o_out && e.to_port == 0).map(|e| (e.from_node, e.from_port)) else {
            return Err(CannotAppend::NothingFeedsOutput);
        };
        // The footage as this graph decodes it.
        let head = self
            .edges
            .iter()
            .find(|e| e.from_node == src && self.nodes.iter().any(|n| n.id == e.to_node && n.kind == NodeKind::InputTransform))
            .map_or(src, |e| e.to_node);
        let tail = if o_feed.0 == o_src { (head, 0) } else { o_feed };
        for e in self.edges.iter_mut().filter(|e| e.from_node == head && e.from_port == 0) {
            (e.from_node, e.from_port) = tail;
        }
        for e in &other.edges {
            if e.to_node == o_out || e.to_node == o_src {
                continue;
            }
            let (from_node, from_port) = if e.from_node == o_src { (head, 0) } else { (e.from_node, e.from_port) };
            self.edges.push(Edge { from_node, from_port, to_node: e.to_node, to_port: e.to_port });
        }
        for n in &middle {
            if let Some(p) = other.node_positions.get(&n.id) {
                self.node_positions.insert(n.id, *p);
            }
        }
        let at = self.nodes.iter().position(|n| n.id == head).map_or(0, |i| i + 1);
        self.nodes.splice(at..at, middle);
        Ok(())
    }

    /// This graph made ready to splice into a graph that already holds
    /// the ids `taken` names: each node of one of those ids takes
    /// [`spliced_id`]'s instead, the same every time, and every edge,
    /// position and frame membership follows it. Other nodes keep their
    /// ids. Without this, two stages holding a node of the same id (a
    /// grade copied into a group) put two nodes of one id in the graph the
    /// executor runs (M3 step 16a). The renamed ids, old to new.
    pub fn with_unique_ids(&self, scope: Uuid, taken: impl Fn(NodeId) -> bool) -> (NodeGraph, HashMap<NodeId, NodeId>) {
        let id_map: HashMap<NodeId, NodeId> = self.nodes.iter().filter(|n| taken(n.id)).map(|n| (n.id, spliced_id(scope, n.id))).collect();
        if id_map.is_empty() {
            return (self.clone(), id_map);
        }
        let to = |id: NodeId| id_map.get(&id).copied().unwrap_or(id);
        let mut g = self.clone();
        for node in &mut g.nodes {
            node.id = to(node.id);
        }
        for edge in &mut g.edges {
            edge.from_node = to(edge.from_node);
            edge.to_node = to(edge.to_node);
        }
        g.output_node_id = to(self.output_node_id);
        g.node_positions = self.node_positions.iter().map(|(id, p)| (to(*id), *p)).collect();
        for frame in &mut g.frames {
            frame.contained_node_ids = frame.contained_node_ids.iter().map(|id| to(*id)).collect();
        }
        (g, id_map)
    }

    /// This graph with fresh ids throughout: every node, and every edge,
    /// position and frame membership that names one; frames and notes
    /// get new ids of their own. For a copied graph pasted onto another
    /// clip, so the two share nothing the executor's per-node caches are
    /// keyed by.
    pub fn remap_ids(&self) -> NodeGraph {
        self.remap_ids_mapped().0
    }

    /// [`Self::remap_ids`], and the new id of each node, old to new. A
    /// compound's own nodes keep theirs: only this graph's are renamed.
    pub fn remap_ids_mapped(&self) -> (NodeGraph, HashMap<NodeId, NodeId>) {
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
        for frame in &mut new_graph.frames {
            frame.id = NodeFrameId::new_v4();
            // A member the graph no longer has is not one the copy has.
            frame.contained_node_ids = frame.contained_node_ids.iter().filter_map(|id| id_map.get(id).copied()).collect();
        }
        for note in &mut new_graph.sticky_notes {
            note.id = NodeStickyNoteId::new_v4();
        }
        (new_graph, id_map)
    }
}

/// The id a node of id `id`, spliced from the graph named by `scope` (a
/// group's stage, a track's grade) into a graph that already holds `id`,
/// takes there: derived from both, so every bake and every reader that
/// resolves a path into that stage agree on it.
pub fn spliced_id(scope: Uuid, id: NodeId) -> NodeId {
    Uuid::new_v5(&scope, id.as_bytes())
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

/// One node on a clip's chain, in the order the picture flows through
/// it, with the stage it belongs to.
#[derive(Debug, Clone)]
pub struct ChainLink {
    pub id: NodeId,
    pub kind: NodeKind,
    pub label: String,
    pub enabled: bool,
    pub stage: crate::GradeStage,
    /// A correction gated by a matte is a secondary whatever its kind:
    /// `Corrector` alone balances the whole picture, and the same
    /// `Corrector` on port 1 of a key changes one thing only.
    pub gated: bool,
}

/// The corrections a matte on port 1 limits: a secondary whatever the
/// kind.
fn gateable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Corrector
            | NodeKind::ColorBalance
            | NodeKind::Exposure
            | NodeKind::Contrast
            | NodeKind::Saturation
            | NodeKind::ColorWheels
            | NodeKind::ToneControls
            | NodeKind::RGBCurves
    )
}

/// A correction whose mix fades it against its input with no matte to
/// gate it (M3 step 17a): it gets a gate of its own, which takes the
/// whole frame by its mix.
fn mixed_alone(n: &Node) -> bool {
    n.params.correction_blend.is_some_and(|b| b.mix != 1.0)
}

/// Whether `n` runs with a gate inserted after it.
pub(crate) fn gets_a_gate(graph: &NodeGraph, n: &Node) -> bool {
    gateable(n.kind) && graph.input_of(n.id, 0).is_some() && (graph.input_of(n.id, 1).is_some() || mixed_alone(n))
}

/// The id of the gate inserted after the correction `corr`, the same from
/// frame to frame.
pub(crate) fn gate_id(corr: NodeId) -> NodeId {
    Uuid::from_u128(corr.as_u128() ^ 0x6761_7465_645f_6d61_7474_655f_6170_706c)
}

/// The settings of the gate after the correction `corr`: its mix and the
/// channel its matte is read from (M3 step 17a), or a whole gate read from
/// red, as every gate was before; and whether it takes the matte inverted
/// (`key_invert`, #898), an outside node's.
pub(crate) fn gate_params(corr: &NodeParams) -> NodeParams {
    let blend = corr.correction_blend;
    NodeParams { mix: blend.map(|b| b.mix), correction_blend: blend, key_invert: corr.key_invert, ..NodeParams::default() }
}

impl NodeGraph {
    /// The graph as the executor runs it, when it holds a correction
    /// gated by a matte on its port 1: each such correction followed by
    /// an ApplyMatte that takes the graded picture where the matte is
    /// and the correction's own input elsewhere. `None` when there is
    /// none, and the graph runs as it is.
    ///
    /// A gated correction is how a secondary is built (a qualifier's
    /// matte, a window, an AI mask), but no correction node reads its
    /// port 1, so each graded the whole frame. The ApplyMatte shape is
    /// the one the executor blends by a matte, on the GPU chain as well.
    /// The ApplyMatte's id is derived from the correction's, so it is the
    /// same node from frame to frame.
    ///
    /// A correction with a mix (M3 step 17a) gets one too, gated or not:
    /// the gate takes its mix, and reads its matte from the correction's
    /// mask channel; with no matte it takes the whole frame by the mix.
    ///
    /// A matte wired into the Output's alpha (#898) is taken where the
    /// grade ends, too ([`Self::with_output_alpha_applied`]).
    pub fn with_gates_applied(&self) -> Option<NodeGraph> {
        let gated: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|n| gets_a_gate(self, n))
            .map(|n| n.id)
            .collect();
        if gated.is_empty() {
            return self.with_output_alpha_applied();
        }
        let mut g = self.clone();
        for corr in gated {
            // Read from the graph as rewritten so far: a correction fed
            // by one gated before it reads that one's blend, not its
            // whole-frame grade.
            let Some(plain) = g.input_of(corr, 0) else { continue };
            let matte = g.input_of(corr, 1);
            let apply = gate_id(corr);
            let corr_params = g.nodes.iter().find(|n| n.id == corr).map(|n| n.params.clone()).unwrap_or_default();
            let at = g.nodes.iter().position(|n| n.id == corr).map_or(g.nodes.len(), |i| i + 1);
            // Whoever read the correction reads the blend now.
            for e in &mut g.edges {
                if e.from_node == corr {
                    e.from_node = apply;
                }
            }
            // The matte leaves the correction's port 1, drawn or wireless.
            let output_link = g.nodes.iter_mut().find(|n| n.id == corr).and_then(|n| {
                if let Some(l) = n.input_links.get_mut(1) {
                    *l = None;
                }
                n.output_link.take()
            });
            if g.output_node_id == corr {
                g.output_node_id = apply;
            }
            g.edges.retain(|e| !(e.to_node == corr && e.to_port == 1));
            g.edges.push(Edge { from_node: corr, from_port: 0, to_node: apply, to_port: 0 });
            if let Some(matte) = matte {
                g.edges.push(Edge { from_node: matte, from_port: 0, to_node: apply, to_port: 1 });
            }
            g.edges.push(Edge { from_node: plain, from_port: 0, to_node: apply, to_port: 2 });
            g.nodes.insert(
                at,
                Node {
                    id: apply,
                    kind: NodeKind::ApplyMatte,
                    label: "Gate".into(),
                    enabled: true,
                    params: gate_params(&corr_params),
                    keyframes: NodeKeyframeTracks::default(),
                    output_link,
                    input_links: Vec::new(),
                },
            );
        }
        Some(g.with_output_alpha_applied().unwrap_or(g))
    }

    /// The graph with the matte wired into its Output's alpha (#898) taken
    /// where the grade ends: an `OutputAlpha` between what fed the Output
    /// and the Output, reading the picture and that matte. `None` when no
    /// matte is wired there. The renderer splices a clip's effects in
    /// before the Output, so they then come after the alpha and move
    /// picture and alpha together, as the CPU route draws them. Its id is
    /// derived from the Output's, the same from frame to frame.
    pub fn with_output_alpha_applied(&self) -> Option<NodeGraph> {
        let out = self.output_node_id;
        let matte = self.input_of(out, 1)?;
        let mut g = self.clone();
        let alpha = Uuid::from_u128(out.as_u128() ^ 0x6f75_7470_7574_5f61_6c70_6861_5f6e_6f64);
        g.edges.retain(|e| !(e.to_node == out && e.to_port == 1));
        // An Output with no picture wired leaves with none: the alpha has
        // nothing to cut.
        if let Some(e) = g.edges.iter_mut().find(|e| e.to_node == out && e.to_port == 0) {
            e.to_node = alpha;
            g.edges.push(Edge { from_node: alpha, from_port: 0, to_node: out, to_port: 0 });
            g.edges.push(Edge { from_node: matte, from_port: 0, to_node: alpha, to_port: 1 });
            let at = g.nodes.iter().position(|n| n.id == out).unwrap_or(g.nodes.len());
            g.nodes.insert(at, Node {
                id: alpha,
                kind: NodeKind::OutputAlpha,
                label: "Output Alpha".into(),
                enabled: true,
                params: NodeParams::default(),
                keyframes: NodeKeyframeTracks::default(),
                output_link: None,
                input_links: Vec::new(),
            });
        }
        Some(g)
    }

    /// Add the outside node of the gated correction `inside` (#898): a
    /// Corrector straight after it on the chain, reading the same key on
    /// its port 1 with the gate the other way round (`key_invert`), so it
    /// grades where `inside` doesn't. Only the chain's reader of `inside`
    /// moves to read the new node; anything else reading `inside` (a
    /// mixer's other input, a side branch) keeps reading it. The new
    /// node's id, or `None` when `inside` is no correction gated by a key
    /// on the chain.
    pub fn add_outside_node(&mut self, inside: NodeId) -> Option<NodeId> {
        let node = self.nodes.iter().find(|n| n.id == inside).filter(|n| gateable(n.kind))?;
        let key = self.input_of(inside, 1)?;
        let invert = node.params.key_invert != Some(true);
        let chain = self.chain();
        let at = chain.iter().position(|l| l.id == inside)?;
        let next = chain.get(at + 1)?.id;
        let reader = self.edges.iter().position(|e| e.from_node == inside && e.to_node == next && e.to_port == 0)?;
        let outside = NodeId::new_v4();
        self.edges[reader].from_node = outside;
        self.edges.push(Edge { from_node: inside, from_port: 0, to_node: outside, to_port: 0 });
        self.edges.push(Edge { from_node: key, from_port: 0, to_node: outside, to_port: 1 });
        let index = self.nodes.iter().position(|n| n.id == inside).map_or(self.nodes.len(), |i| i + 1);
        self.nodes.insert(index, Node {
            id: outside,
            kind: NodeKind::Corrector,
            label: "Outside".into(),
            enabled: true,
            params: NodeParams { key_invert: invert.then_some(true), ..NodeParams::default() },
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        if let Some(pos) = self.node_positions.get(&inside).copied() {
            self.node_positions.insert(outside, NodePos { x: pos.x + 180.0, y: pos.y });
        }
        Some(outside)
    }

    /// The chain from the source to the output, in flow order, with each
    /// node's grading stage — what a colourist means by "the order of
    /// operations".
    ///
    /// Only the main signal path: matte producers and the nodes that
    /// feed them are named by the correction they gate, not walked as a
    /// separate branch, because their position in the picture's order is
    /// the position of the thing they gate.
    pub fn chain(&self) -> Vec<ChainLink> {
        let by_id: std::collections::HashMap<NodeId, &Node> = self.nodes.iter().map(|n| (n.id, n)).collect();
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut at = Some(self.output_node_id);
        // Walk backwards from the output along port 0 — the picture's
        // own path — then reverse, so a graph with dangling branches
        // still reports the chain that renders.
        while let Some(id) = at {
            if !seen.insert(id) {
                break;
            }
            if let Some(n) = by_id.get(&id) {
                let gated = self.edges.iter().any(|e| e.to_node == id && e.to_port == 1) && gateable(n.kind);
                out.push(ChainLink {
                    id,
                    kind: n.kind,
                    label: n.label.clone(),
                    enabled: n.enabled,
                    stage: if gated { crate::GradeStage::Secondary } else { n.kind.grade_stage() },
                    gated,
                });
            }
            at = self.input_of(id, 0);
        }
        out.reverse();
        out
    }

    /// Where the chain departs from the order a colourist works in, as
    /// sentences. Empty when the chain is in order.
    ///
    /// Plumbing and the source are skipped: they have no place of their
    /// own. Only the stages that carry meaning are held to the order.
    pub fn chain_out_of_order(&self) -> Vec<String> {
        let chain = self.chain();
        let meaningful: Vec<&ChainLink> = chain
            .iter()
            .filter(|l| l.enabled && !matches!(l.stage, crate::GradeStage::Plumbing))
            .collect();
        let mut said = Vec::new();
        for pair in meaningful.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (a_label, b_label) = (klipp_core::quote::quoted(&a.label), klipp_core::quote::quoted(&b.label));
            if b.stage < a.stage {
                said.push(match (a.stage, b.stage) {
                    (crate::GradeStage::Look, crate::GradeStage::Balance) => format!(
                        "the balance ({}) sits after the look ({}) — the balance is then correcting the look's own cast, and the loop will never settle",
                        b_label, a_label
                    ),
                    (crate::GradeStage::Look, crate::GradeStage::Secondary) => format!(
                        "the secondary ({}) keys off the look ({}) rather than the balanced picture — move the look last or the key drifts with it",
                        b_label, a_label
                    ),
                    (crate::GradeStage::Secondary, crate::GradeStage::Balance) => format!(
                        "the balance ({}) sits after the secondary ({}) — the key was pulled on an uncorrected picture and will drift as the balance moves under it",
                        b_label, a_label
                    ),
                    (_, crate::GradeStage::Input) => format!(
                        "the input transform ({}) is not first — everything before it grades the camera's own encoding, where the numbers mean nothing",
                        b_label
                    ),
                    _ => format!(
                        "{} ({}) comes after {} ({}); the order is input, balance, secondary, look, output",
                        b_label,
                        b.stage.label(),
                        a_label,
                        a.stage.label()
                    ),
                });
            }
        }
        said
    }
}

#[cfg(test)]
mod chain_order_tests {
    use super::*;
    use crate::GradeStage;

    fn node(kind: NodeKind, label: &str) -> Node {
        Node {
            id: NodeId::new_v4(),
            kind,
            label: label.to_string(),
            enabled: true,
            params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        }
    }

    /// A chain from a list of kinds, wired source → ... → output.
    fn chain_of(kinds: &[NodeKind]) -> NodeGraph {
        let mut nodes = vec![node(NodeKind::Source, "Source")];
        for k in kinds {
            nodes.push(node(*k, k.display_name()));
        }
        nodes.push(node(NodeKind::Output, "Output"));
        let edges = (0..nodes.len() - 1)
            .map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 })
            .collect();
        let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
        NodeGraph {
            output_node_id: nodes.last().unwrap().id,
            nodes,
            edges,
            node_positions,
            working_space_override: None,
            frames: vec![],
            sticky_notes: vec![],
        }
    }

    /// The node kinds from Source to Output along port 0.
    fn path_kinds(g: &NodeGraph) -> Vec<NodeKind> {
        let mut at = g.output_node_id;
        let mut out = vec![];
        loop {
            let n = g.nodes.iter().find(|n| n.id == at).unwrap();
            out.push(n.kind);
            match g.edges.iter().find(|e| e.to_node == at && e.to_port == 0) {
                Some(e) => at = e.from_node,
                None => break,
            }
        }
        out.reverse();
        out
    }

    /// #237: a group's pre-clip grade runs on the decoded footage before
    /// the clip's own, its post-clip grade after, their node ids kept so
    /// one change reaches every member; their own Input Transforms stay
    /// behind.
    #[test]
    fn a_group_grade_wraps_the_clip_grade_with_its_ids() {
        let mut clip = chain_of(&[NodeKind::InputTransform, NodeKind::Corrector]);
        let pre = chain_of(&[NodeKind::InputTransform, NodeKind::Lut3D]);
        let post = chain_of(&[NodeKind::Qualifier]);
        clip.prepend_keeping_ids(&pre).unwrap();
        clip.append_keeping_ids(&post).unwrap();
        assert_eq!(
            path_kinds(&clip),
            vec![NodeKind::Source, NodeKind::InputTransform, NodeKind::Lut3D, NodeKind::Corrector, NodeKind::Qualifier, NodeKind::Output]
        );
        let lut = pre.nodes.iter().find(|n| n.kind == NodeKind::Lut3D).unwrap().id;
        let q = post.nodes.iter().find(|n| n.kind == NodeKind::Qualifier).unwrap().id;
        assert!(clip.nodes.iter().any(|n| n.id == lut) && clip.nodes.iter().any(|n| n.id == q), "the group's nodes keep their ids");
        assert_eq!(clip.nodes.iter().filter(|n| n.kind == NodeKind::InputTransform).count(), 1, "the clip decodes once");
        assert!(clip.topo_sort().is_ok());

        // A clip with no Input Transform: the pre-clip grade reads the Source.
        let mut plain = chain_of(&[NodeKind::Corrector]);
        plain.prepend_keeping_ids(&pre).unwrap();
        assert_eq!(path_kinds(&plain), vec![NodeKind::Source, NodeKind::Lut3D, NodeKind::Corrector, NodeKind::Output]);
        // An empty group stage changes nothing.
        let before = plain.clone();
        plain.prepend_keeping_ids(&chain_of(&[])).unwrap();
        plain.append_keeping_ids(&chain_of(&[])).unwrap();
        assert!(plain == before, "an empty stage left the graph as it was");
    }

    #[test]
    fn the_professional_order_passes() {
        let g = chain_of(&[NodeKind::InputTransform, NodeKind::Corrector, NodeKind::Qualifier, NodeKind::Lut3D]);
        assert!(g.chain_out_of_order().is_empty(), "{:?}", g.chain_out_of_order());
        let stages: Vec<GradeStage> = g.chain().iter().map(|l| l.stage).collect();
        assert_eq!(
            stages,
            vec![GradeStage::Plumbing, GradeStage::Input, GradeStage::Balance, GradeStage::Secondary, GradeStage::Look, GradeStage::Output]
        );
    }

    #[test]
    fn a_balance_after_a_look_is_named_as_the_fault_it_is() {
        // The commonest way a grading loop fails to converge: a LUT on
        // the front, so every measured correction is measured through it.
        let g = chain_of(&[NodeKind::Lut3D, NodeKind::Corrector]);
        let said = g.chain_out_of_order();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("correcting the look's own cast"), "{}", said[0]);
    }

    #[test]
    fn a_key_pulled_before_the_balance_is_named() {
        let g = chain_of(&[NodeKind::Qualifier, NodeKind::Corrector]);
        let said = g.chain_out_of_order();
        assert!(said.iter().any(|s| s.contains("drift")), "{said:?}");
    }

    #[test]
    fn an_input_transform_that_is_not_first_is_named() {
        let g = chain_of(&[NodeKind::Corrector, NodeKind::InputTransform]);
        let said = g.chain_out_of_order();
        assert!(said.iter().any(|s| s.contains("input transform")), "{said:?}");
    }

    #[test]
    fn a_gated_correction_reads_as_a_secondary_not_a_balance() {
        // The shape `ensure_masked_secondary` builds: the same kind of
        // node that balances the whole picture, gated by a key, is a
        // secondary — and so the chain is in order, not out of it.
        let mut g = chain_of(&[NodeKind::Corrector]);
        let (_qual, _corr) = g.ensure_masked_secondary(NodeKind::ColorBalance);
        let chain = g.chain();
        let gated: Vec<&ChainLink> = chain.iter().filter(|l| l.gated).collect();
        assert_eq!(gated.len(), 1, "one gated correction: {:?}", chain.iter().map(|l| (&l.label, l.stage)).collect::<Vec<_>>());
        assert_eq!(gated[0].stage, GradeStage::Secondary);
        assert!(g.chain_out_of_order().is_empty(), "{:?}", g.chain_out_of_order());
    }

    #[test]
    fn a_disabled_node_is_not_held_to_the_order() {
        let mut g = chain_of(&[NodeKind::Lut3D, NodeKind::Corrector]);
        for n in g.nodes.iter_mut() {
            if n.kind == NodeKind::Lut3D {
                n.enabled = false;
            }
        }
        assert!(g.chain_out_of_order().is_empty(), "a bypassed look is not in the picture's way");
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use klipp_core::color_grade::MaskShape;

    fn graph_with_corrector() -> NodeGraph {
        crate::defaults::default_graph()
    }

    #[test]
    fn a_window_intersects_the_key_rather_than_replacing_it() {
        // "This face" is where and what: the correction must reach only
        // what is both inside the window and inside the hue key.
        let mut g = graph_with_corrector();
        let (qual, mask, corr) = g.ensure_windowed_secondary(NodeKind::ColorBalance);
        let combine = g.edges.iter().find(|e| e.to_node == corr && e.to_port == 1).expect("the correction is gated").from_node;
        assert_eq!(g.nodes.iter().find(|n| n.id == combine).unwrap().kind, NodeKind::CombineMattes);
        assert!(g.edges.iter().any(|e| e.from_node == qual && e.to_node == combine && e.to_port == 0), "the key feeds the combiner");
        assert!(g.edges.iter().any(|e| e.from_node == mask && e.to_node == combine && e.to_port == 1), "the window feeds the combiner");
        // Nothing left wired straight from the key to the correction.
        assert!(!g.edges.iter().any(|e| e.from_node == qual && e.to_node == corr), "the key no longer bypasses the window");
        // And the chain still reads as a secondary, not a balance.
        assert!(g.chain_out_of_order().is_empty(), "{:?}", g.chain_out_of_order());
    }

    #[test]
    fn asking_twice_does_not_build_a_second_window() {
        let mut g = graph_with_corrector();
        let first = g.ensure_windowed_secondary(NodeKind::ColorBalance);
        let before = g.nodes.len();
        let second = g.ensure_windowed_secondary(NodeKind::ColorBalance);
        assert_eq!(first, second);
        assert_eq!(g.nodes.len(), before);
    }

    #[test]
    fn a_window_built_over_a_plain_secondary_keeps_its_key() {
        // The shape `qualify` has been building all along, upgraded.
        let mut g = graph_with_corrector();
        let (qual, corr) = g.ensure_masked_secondary(NodeKind::ColorBalance);
        let (qual2, _mask, corr2) = g.ensure_windowed_secondary(NodeKind::ColorBalance);
        assert_eq!((qual, corr), (qual2, corr2), "the same key and correction, now windowed");
    }

    #[test]
    fn the_window_covers_the_face_under_the_circles_own_convention() {
        // MaskShape::Circle reaches `radius` across and `radius / aspect`
        // of the height down. A tall face box is the case a radius from
        // its width alone gets wrong: it would cut the chin off.
        let mut g = graph_with_corrector();
        let (_q, mask, _c) = g.ensure_windowed_secondary(NodeKind::ColorBalance);
        let aspect = 9.0f32 / 16.0;
        let face = [0.45f32, 0.20, 0.10, 0.30]; // narrow and tall
        g.set_window_track(mask, &[(0.0, face)], aspect, 0.5);
        let m = &g.nodes.iter().find(|n| n.id == mask).unwrap().params.masks.as_ref().unwrap().masks[0];
        let MaskShape::Circle { center_x, center_y, radius } = m.shape else { panic!("a circle") };
        assert!((center_x - 0.5).abs() < 1e-5 && (center_y - 0.35).abs() < 1e-5, "centred on the face");
        assert!(radius / aspect >= face[3] / 2.0, "the window reaches the chin: {radius}");
        assert!(radius >= face[2] / 2.0, "and the cheeks");
    }

    #[test]
    fn a_face_that_moves_keyframes_the_window() {
        let mut g = graph_with_corrector();
        let (_q, mask, _c) = g.ensure_windowed_secondary(NodeKind::ColorBalance);
        let track = [
            (0.0f64, [0.10f32, 0.30, 0.10, 0.18]),
            (1.0, [0.40, 0.30, 0.10, 0.18]),
            (2.0, [0.70, 0.32, 0.12, 0.20]),
        ];
        g.set_window_track(mask, &track, 9.0 / 16.0, 0.5);
        let m = &g.nodes.iter().find(|n| n.id == mask).unwrap().params.masks.as_ref().unwrap().masks[0];
        assert_eq!(m.keyframes.center_x.len(), 3);
        assert!(!m.keyframes.is_empty());
        // Half way across the shot the window is half way across the
        // frame: the correction travels with the subject.
        let at = m.evaluated_at(1.0);
        let MaskShape::Circle { center_x, .. } = at.shape else { panic!("a circle") };
        assert!((center_x - 0.45).abs() < 1e-3, "the window follows the face: {center_x}");
    }

    #[test]
    fn a_window_with_no_track_is_left_alone() {
        let mut g = graph_with_corrector();
        let (_q, mask, _c) = g.ensure_windowed_secondary(NodeKind::ColorBalance);
        g.set_window_track(mask, &[], 9.0 / 16.0, 0.5);
        assert!(g.nodes.iter().find(|n| n.id == mask).unwrap().params.masks.is_none());
    }
}

#[cfg(test)]
mod append_tests {
    use super::*;

    fn exposure_graph(stops: f32) -> NodeGraph {
        let mut g = NodeGraph::default();
        let id = g.ensure_corrector();
        if let Some(n) = g.nodes.iter_mut().find(|n| n.id == id) {
            n.params.exposure = Some(stops);
        }
        g
    }

    #[test]
    fn an_appended_look_runs_after_the_graph_it_joins() {
        let mut clip = exposure_graph(0.4);
        let balance = clip.nodes.iter().find(|n| n.kind == NodeKind::Corrector).map(|n| n.id).unwrap();
        let look = exposure_graph(-0.2);
        assert!(clip.append(&look).is_ok());
        let order = clip.topo_sort().expect("no cycle");
        let correctors: Vec<NodeId> = order
            .iter()
            .copied()
            .filter(|id| clip.nodes.iter().any(|n| n.id == *id && n.kind == NodeKind::Corrector))
            .collect();
        assert_eq!(correctors.len(), 2, "the clip's own and the look's");
        assert_eq!(correctors[0], balance, "the clip's balance first, the look after");
        let feeding = clip.nodes_feeding_output();
        assert!(correctors.iter().all(|c| feeding.contains(c)), "both reach the Output");
        assert!(clip.nodes.iter().all(|n| !look.nodes.iter().any(|l| l.id == n.id)), "fresh ids");
    }

    /// An Input Transform decoding `format`, spliced in after Source the
    /// way a clip gets one when it is added to the timeline.
    fn with_input_transform(g: &mut NodeGraph, format: klipp_core::LogFormat) -> NodeId {
        let src = g.nodes.iter().find(|n| n.kind == NodeKind::Source).map(|n| n.id).unwrap();
        let id = NodeId::new_v4();
        for e in g.edges.iter_mut().filter(|e| e.from_node == src) {
            e.from_node = id;
        }
        g.edges.push(Edge { from_node: src, from_port: 0, to_node: id, to_port: 0 });
        g.nodes.insert(1, Node {
            id,
            kind: NodeKind::InputTransform,
            label: "Input Transform".into(),
            enabled: true,
            params: NodeParams { log_format: Some(format), ..Default::default() },
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        id
    }

    fn input_formats(g: &NodeGraph) -> Vec<klipp_core::LogFormat> {
        g.nodes.iter().filter(|n| n.kind == NodeKind::InputTransform).filter_map(|n| n.params.log_format).collect()
    }

    fn chain_kinds(g: &NodeGraph) -> Vec<NodeKind> {
        let feeding = g.nodes_feeding_output();
        g.topo_sort()
            .expect("no cycle")
            .into_iter()
            .filter(|id| feeding.contains(id))
            .filter_map(|id| g.nodes.iter().find(|n| n.id == id).map(|n| n.kind))
            .collect()
    }

    /// A still grabbed on a LogC shot, added after a Sony shot's own
    /// grade: the Sony shot keeps its one Input Transform, the still's
    /// stays behind, and the look still runs.
    #[test]
    fn an_appended_grade_leaves_its_input_transform_behind() {
        use klipp_core::LogFormat;
        let mut clip = exposure_graph(0.4);
        let own = with_input_transform(&mut clip, LogFormat::SLog3);
        let mut look = exposure_graph(-0.2);
        with_input_transform(&mut look, LogFormat::LogC3);
        assert!(clip.append(&look).is_ok());
        assert_eq!(input_formats(&clip), vec![LogFormat::SLog3], "the clip's own, and only that");
        assert!(clip.nodes.iter().any(|n| n.id == own), "the clip's own transform node, untouched");
        assert_eq!(
            chain_kinds(&clip),
            vec![NodeKind::Source, NodeKind::InputTransform, NodeKind::Corrector, NodeKind::Corrector, NodeKind::Output],
        );
    }

    /// The same still applied in place of the Sony shot's grade: the
    /// grade is the still's, the input colour space is still the shot's.
    #[test]
    fn a_grade_applied_in_place_keeps_the_clips_input_transform() {
        use klipp_core::LogFormat;
        let mut clip = exposure_graph(0.4);
        with_input_transform(&mut clip, LogFormat::SLog3);
        let mut look = exposure_graph(-0.2);
        with_input_transform(&mut look, LogFormat::LogC3);
        let new = clip.with_grade_of(&look);
        assert_eq!(input_formats(&new), vec![LogFormat::SLog3]);
        assert_eq!(chain_kinds(&new), vec![NodeKind::Source, NodeKind::InputTransform, NodeKind::Corrector, NodeKind::Output]);
        let exposure = new.nodes.iter().find(|n| n.kind == NodeKind::Corrector).and_then(|n| n.params.exposure);
        assert_eq!(exposure, Some(-0.2), "the still's grade");
        assert!(new.nodes.iter().all(|n| !look.nodes.iter().any(|l| l.id == n.id)), "fresh ids");
    }

    /// A node reading the Input Transform wirelessly reads what fed it
    /// once the transform is gone, and one the transform read
    /// wirelessly is what its edges are wired to.
    #[test]
    fn stripping_an_input_transform_repoints_its_wireless_links() {
        let mut g = exposure_graph(0.4);
        let it = with_input_transform(&mut g, klipp_core::LogFormat::LogC3);
        let src = g.nodes.iter().find(|n| n.kind == NodeKind::Source).map(|n| n.id).unwrap();
        let corrector = g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).map(|n| n.id).unwrap();
        // Source → Input Transform goes wireless, and a Corrector reads
        // the transform wirelessly on a port nothing is drawn to.
        g.edges.retain(|e| !(e.from_node == src && e.to_node == it));
        g.nodes.iter_mut().find(|n| n.id == src).unwrap().output_link = Some("src".into());
        let transform = g.nodes.iter_mut().find(|n| n.id == it).unwrap();
        transform.input_links = vec![Some("src".into())];
        transform.output_link = Some("it".into());
        g.nodes.iter_mut().find(|n| n.id == corrector).unwrap().input_links = vec![None, Some("it".into())];
        assert_eq!(g.input_of(corrector, 1), Some(it));

        g.strip_input_transforms();
        assert!(g.nodes.iter().all(|n| n.kind != NodeKind::InputTransform));
        assert_eq!(g.input_of(corrector, 0), Some(src), "the drawn edge, wired to the wireless feed");
        assert_eq!(g.input_of(corrector, 1), Some(src), "the wireless read, now a drawn edge from the feed");
        assert!(g.nodes.iter().all(|n| n.input_links.iter().all(|l| l.as_deref() != Some("it"))), "no slot names it");
        assert!(g.topo_sort().is_ok());
    }

    /// A wireless read of an Input Transform nothing fed reads nothing,
    /// as it did through the transform.
    #[test]
    fn stripping_an_unfed_input_transform_clears_its_wireless_readers() {
        let mut g = exposure_graph(0.4);
        let it = with_input_transform(&mut g, klipp_core::LogFormat::LogC3);
        let corrector = g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).map(|n| n.id).unwrap();
        g.edges.retain(|e| e.to_node != it);
        g.nodes.iter_mut().find(|n| n.id == it).unwrap().output_link = Some("it".into());
        g.nodes.iter_mut().find(|n| n.id == corrector).unwrap().input_links = vec![None, Some("it".into())];
        g.strip_input_transforms();
        assert_eq!(g.input_of(corrector, 1), None);
        assert_eq!(g.nodes.iter().find(|n| n.id == corrector).unwrap().input_links, vec![None, None]);
    }

    /// Only a frame the removal empties goes: one that was empty
    /// already, and one with other members, stay.
    #[test]
    fn stripping_an_input_transform_prunes_only_the_frame_it_empties() {
        let mut g = exposure_graph(0.4);
        let it = with_input_transform(&mut g, klipp_core::LogFormat::LogC3);
        let corrector = g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).map(|n| n.id).unwrap();
        let frame = |label: &str, members: &[NodeId]| NodeFrame {
            id: NodeFrameId::new_v4(),
            label: label.into(),
            description: String::new(),
            color_rgba: 0,
            contained_node_ids: members.iter().copied().collect(),
            recipe: None,
        };
        g.frames = vec![frame("Input", &[it]), frame("Both", &[it, corrector]), frame("Empty", &[])];
        g.strip_input_transforms();
        let labels: Vec<&str> = g.frames.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, vec!["Both", "Empty"]);
        assert_eq!(g.frames[0].contained_node_ids, [corrector].into_iter().collect());
    }

    /// A Rec.709 shot has no Input Transform, and gets none from the
    /// LogC still.
    #[test]
    fn a_grade_applied_to_a_clip_without_an_input_transform_brings_none() {
        let clip = exposure_graph(0.4);
        let mut look = exposure_graph(-0.2);
        with_input_transform(&mut look, klipp_core::LogFormat::LogC3);
        let new = clip.with_grade_of(&look);
        assert!(input_formats(&new).is_empty());
        assert_eq!(chain_kinds(&new), vec![NodeKind::Source, NodeKind::Corrector, NodeKind::Output]);
    }

    /// A frame around `g`'s Corrector, and a note beside it.
    fn with_frame_and_note(g: &mut NodeGraph) -> (NodeId, f32) {
        let corrector = g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).map(|n| n.id).unwrap();
        g.frames.push(NodeFrame {
            id: NodeFrameId::new_v4(),
            label: "Look".into(),
            description: String::new(),
            color_rgba: 0,
            contained_node_ids: [corrector].into_iter().collect(),
            recipe: None,
        });
        let note_x = 220.0;
        g.sticky_notes.push(NodeStickyNote {
            id: NodeStickyNoteId::new_v4(),
            pos: NodePos { x: note_x, y: -80.0 },
            width: 200.0,
            text: "warm it".into(),
            color_rgba: 0,
        });
        (corrector, note_x)
    }

    #[test]
    fn grades_pinned_to_different_working_spaces_do_not_join() {
        use klipp_core::WorkingColorspace as Ws;
        let mut clip = exposure_graph(0.4);
        clip.working_space_override = Some(Ws::Rec709Linear);
        let mut look = exposure_graph(-0.2);
        look.working_space_override = Some(Ws::AcesCG);
        let before = clip.nodes.len();
        assert_eq!(
            clip.append(&look),
            Err(CannotAppend::WorkingSpace { ours: Ws::Rec709Linear, theirs: Ws::AcesCG }),
        );
        assert_eq!(clip.nodes.len(), before, "nothing changed");
        assert!(!clip.append(&look).unwrap_err().to_string().is_empty(), "a reason to show");
    }

    #[test]
    fn a_pinned_grade_joins_an_unpinned_graph_which_stays_unpinned() {
        let mut clip = exposure_graph(0.4);
        let mut look = exposure_graph(-0.2);
        look.working_space_override = Some(klipp_core::WorkingColorspace::AcesCG);
        assert!(clip.append(&look).is_ok());
        assert_eq!(clip.working_space_override, None);
        assert_eq!(count(&clip, NodeKind::Corrector), 2);
    }

    #[test]
    fn an_appended_grade_brings_its_frames_and_notes_on_the_new_ids() {
        let mut clip = exposure_graph(0.4);
        let mut look = exposure_graph(-0.2);
        let (old_corrector, note_x) = with_frame_and_note(&mut look);
        assert!(clip.append(&look).is_ok());
        assert_eq!(clip.frames.len(), 1);
        let members: Vec<NodeId> = clip.frames[0].contained_node_ids.iter().copied().collect();
        assert_eq!(members.len(), 1);
        assert_ne!(members[0], old_corrector, "the frame follows the fresh id");
        assert!(clip.nodes.iter().any(|n| n.id == members[0] && n.kind == NodeKind::Corrector));
        assert_ne!(clip.frames[0].id, look.frames[0].id);
        assert_eq!(clip.sticky_notes.len(), 1);
        assert!(clip.sticky_notes[0].pos.x > note_x, "moved right with the nodes it sat beside");
    }

    #[test]
    fn remapped_ids_keep_frame_membership() {
        let mut g = exposure_graph(0.4);
        let (old_corrector, _) = with_frame_and_note(&mut g);
        g.frames[0].contained_node_ids.insert(NodeId::new_v4()); // a stale member
        let new = g.remap_ids();
        let ids: HashSet<NodeId> = new.nodes.iter().map(|n| n.id).collect();
        assert_eq!(new.frames.len(), 1);
        assert_eq!(new.frames[0].contained_node_ids.len(), 1, "the stale member is dropped");
        assert!(new.frames[0].contained_node_ids.iter().all(|id| ids.contains(id)));
        assert!(!new.frames[0].contained_node_ids.contains(&old_corrector));
        assert_ne!(new.frames[0].id, g.frames[0].id);
        assert_ne!(new.sticky_notes[0].id, g.sticky_notes[0].id);
    }

    fn count(g: &NodeGraph, kind: NodeKind) -> usize {
        g.nodes.iter().filter(|n| n.kind == kind).count()
    }

    #[test]
    fn appending_an_identity_graph_changes_nothing() {
        let mut clip = exposure_graph(0.4);
        let before = clip.nodes.len();
        let empty = NodeGraph::default();
        let identity_nodes = empty.nodes.iter().filter(|n| !matches!(n.kind, NodeKind::Source | NodeKind::Output)).count();
        assert!(clip.append(&empty).is_ok());
        assert_eq!(clip.nodes.len(), before + identity_nodes);
    }
}

impl NodeGraph {
    /// Where node `id` is: the Compounds, outermost first, whose graphs
    /// lead from this one to the graph that holds it. Empty when it is in
    /// this graph, `None` when it is in none.
    pub fn path_to_node(&self, id: NodeId) -> Option<Vec<NodeId>> {
        if self.nodes.iter().any(|n| n.id == id) {
            return Some(Vec::new());
        }
        self.nodes.iter().find_map(|n| {
            let mut path = n.params.compound_subgraph.as_ref()?.path_to_node(id)?;
            path.insert(0, n.id);
            Some(path)
        })
    }
}

#[cfg(test)]
mod path_to_node_tests {
    use super::*;

    fn node(kind: NodeKind) -> Node {
        Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] }
    }

    #[test]
    fn a_node_is_found_where_it_is_however_deep() {
        let (top, inner, deep) = (node(NodeKind::Exposure), node(NodeKind::Contrast), node(NodeKind::Saturation));
        let (top_id, inner_id, deep_id) = (top.id, inner.id, deep.id);
        let mut innermost = NodeGraph::default();
        innermost.nodes.push(deep);
        let mut middle = NodeGraph::default();
        middle.nodes.push(inner);
        let mut c2 = node(NodeKind::Compound);
        c2.params.compound_subgraph = Some(innermost);
        let c2_id = c2.id;
        middle.nodes.push(c2);
        let mut c1 = node(NodeKind::Compound);
        c1.params.compound_subgraph = Some(middle);
        let c1_id = c1.id;
        let mut root = NodeGraph::default();
        root.nodes.push(top);
        root.nodes.push(c1);
        assert_eq!(root.path_to_node(top_id), Some(vec![]));
        assert_eq!(root.path_to_node(c1_id), Some(vec![]), "a Compound is in the graph that holds it");
        assert_eq!(root.path_to_node(inner_id), Some(vec![c1_id]));
        assert_eq!(root.path_to_node(deep_id), Some(vec![c1_id, c2_id]));
        assert_eq!(root.path_to_node(NodeId::new_v4()), None);
    }
}

#[cfg(test)]
mod remap_map_tests {
    use super::*;

    /// #1866: a pasted or appended grade says which id each node took;
    /// a compound's own nodes keep theirs, and the map names only nodes
    /// of the copied graph's top.
    #[test]
    fn the_remap_names_each_nodes_new_id() {
        let mut other = NodeGraph::default();
        let corrector = other.ensure_corrector();
        let mut inner = NodeGraph::default();
        let inside = inner.ensure_corrector();
        let mut compound = Node { id: NodeId::new_v4(), kind: NodeKind::Compound, label: String::new(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] };
        compound.params.compound_subgraph = Some(inner);
        let compound_id = compound.id;
        other.nodes.push(compound);

        let (pasted, map) = NodeGraph::default().with_grade_of_mapped(&other);
        assert_eq!(map.len(), other.nodes.len());
        for id in [corrector, compound_id] {
            let new = map[&id];
            assert_ne!(new, id);
            assert!(pasted.nodes.iter().any(|n| n.id == new), "{id} is pasted as {new}");
        }
        let body = pasted.nodes.iter().find(|n| n.id == map[&compound_id]).unwrap().params.compound_subgraph.as_ref().unwrap();
        assert!(body.nodes.iter().any(|n| n.id == inside), "inside a compound the ids are kept");
        assert_eq!(pasted.nodes.len(), NodeGraph::default().with_grade_of(&other).nodes.len());

        let mut base = NodeGraph::default();
        let map = base.append_mapped(&other).unwrap();
        assert!(base.nodes.iter().any(|n| n.id == map[&corrector]), "appended under its mapped id");
    }
}
