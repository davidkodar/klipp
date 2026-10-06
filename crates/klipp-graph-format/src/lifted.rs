//! The lifted model (phase 2a): what the semantic lift turns a revision-1
//! clip record into. The clip's opacity and placement become properties of
//! the clip, its graph runs in render order from a source to an output, and
//! every implicit read is an explicit edge.
//!
//! In 2a only the structure is lifted. Grade nodes other than the merges
//! stay `klipp.legacy.node` and effects stay `klipp.legacy.effect` or
//! `klipp.legacy.ofx`, each carrying its v1 entry; they become real node
//! types in 2b.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Animation, Graph, NodeId, ParamKey};

/// The node types the lift adds to the legacy vocabulary.
pub mod types {
    /// Where a clip's picture enters its graph, and where it leaves.
    pub const SOURCE: &str = "klipp.source";
    pub const OUTPUT: &str = "klipp.output";
    /// The Grade group around the clip's grade graph, and its two ends
    /// (v1's `Source` and `Output` nodes).
    pub const GROUP: &str = "klipp.group";
    pub const GROUP_INPUT: &str = "klipp.group.input";
    pub const GROUP_OUTPUT: &str = "klipp.group.output";
    /// A Grade group's second input: the picture entering the grade, which
    /// an unwired input reads today wherever its node sits. The group's
    /// main input carries the picture its stages hand on; unspliced the
    /// two are the same.
    pub const GROUP_FRAME: &str = "klipp.group.frame";
    /// v1's `Layer`, `Blend` and `LayerMixer`, with their legacy op in the
    /// `op` parameter ([`MERGE_OPS`]).
    pub const MERGE: &str = "klipp.merge";
    /// Where the clip's placement is applied; its `mode` parameter is
    /// [`PLACEMENT_AUTO`] or [`PLACEMENT_FIXED`].
    pub const PLACEMENT: &str = "klipp.placement";
    /// The source-neighbour nodes at the head of a Media clip's graph.
    pub const TEMPORAL_DENOISE: &str = "klipp.temporal_denoise";
    pub const FRAME_BLEND: &str = "klipp.frame_blend";
    /// The display tone map after the Grade group: always present, a
    /// passthrough unless the source is scene-referred at render time.
    pub const SCENE_TO_DISPLAY: &str = "klipp.colour.scene_to_display";

    /// The merge ops, by the v1 kind they come from.
    pub const MERGE_OPS: [(&str, &str); 3] = [("Layer", "layer_v1"), ("Blend", "blend_v1"), ("LayerMixer", "mixer_v1")];
    pub const PLACEMENT_AUTO: u32 = 0;
    pub const PLACEMENT_FIXED: u32 = 1;
}

/// A clip's picture after the lift.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipPicture {
    pub graph: Graph,
    #[serde(default, skip_serializing_if = "PlacementProps::is_empty")]
    pub placement: PlacementProps,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub placement_anim: BTreeMap<ParamKey, Animation>,
    /// The static opacity: clamped to 0..1 as the renderer does when
    /// nothing animates it, and as stored under an animation, which the
    /// renderer clamps only after its keys and modifiers.
    pub opacity: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity_anim: Option<Animation>,
}

/// The clip's placement values. Each is `Some` only when the clip's own
/// effect list supplied it; `None` means "none of its own", which a track's
/// transform may fill at render time.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perspective: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focal_ref: Option<Value>,
}

impl PlacementProps {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// v1 behaviours a lifted node keeps, and what it holds for the record.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyFlags {
    /// A gated correction forces alpha to 1, as v1 did.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alpha_one: bool,
    /// Keyframes and modifiers that drive nothing, kept by their stored key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub orphan_keys: BTreeMap<String, Value>,
    /// On a Grade group: v1 edges that were never executed (a port-1 edge
    /// on a correction whose port 0 is unwired), kept outside the graph.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked_edges: Vec<Value>,
}

impl LegacyFlags {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One thing the lift dropped, moved or gave a meaning to. The dry run
/// lists them; the real run stops a project on any outside the known list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "note", rename_all = "snake_case")]
pub enum LiftNote {
    /// Kept as a disabled node because v1 never ran it: a head entry on a
    /// clip that isn't Media, or a TNR after the first that ran.
    NeverRan { node: NodeId, why: String },
    /// A port-1 edge on a correction whose port 0 is unwired: never a gate.
    ParkedEdge { group: NodeId, edge: Value },
    /// Keyframes or modifiers that drove nothing, kept on a node (or the
    /// clip, for `None`) by their stored keys.
    OrphanKeys { node: Option<NodeId>, keys: Vec<String> },
    /// A Transform after the first, kept disabled for the record.
    ExtraTransform { node: NodeId },
    /// A stored value no reader used, with a non-default value.
    DroppedField { what: String, field: String },
    /// A rewrite today's loader makes on every load, made once here so the
    /// converted project shows what the user sees today (`migrate_graph`'s
    /// Color Warper).
    LoadMigration { group: NodeId, what: String },
    /// A clip with no grade graph in its file, which today loads as the
    /// default pipeline; the conversion writes it out.
    DefaultPipeline { group: NodeId },
    /// An old grade graph's output named a node that isn't there; it is
    /// made, with that id, so the edges into it read as today.
    MissingOutput { group: NodeId },
    /// An edge today's executor never reads (to a missing node, into a
    /// port the kind doesn't read, or after another into the same port),
    /// kept here for the record.
    EdgeDropped { group: NodeId, edge: Value, why: String },
    /// An edge from a missing node into a primary input, which today reads
    /// the frame: now an edge from the group's input.
    EdgeRewired { group: NodeId, edge: Value },
}

/// Where a node spliced into a clip's render copy came from. The clip's
/// own nodes have none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Origin {
    /// The clip's track: its effects, or its grade.
    Track { track: uuid::Uuid },
    /// A stage of the clip's grade group.
    GroupStage { group: uuid::Uuid, stage: Stage },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Pre,
    Post,
}

/// Why a graph isn't a valid lifted graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiftedError {
    /// Not exactly one start or end node at a level.
    Ends { level: String, what: &'static str, count: usize },
    /// The graph's `output` isn't its end node.
    Output { level: String },
    DuplicateNode(NodeId),
    DanglingEdge(NodeId),
    /// A required input with no edge or wireless link: an implicit read,
    /// which the lifted model doesn't have.
    Unwired { node: NodeId, port: String },
    /// Two edges into one port.
    TwoInputs { node: NodeId, port: String },
    /// A `mask` edge into a node that isn't a gateable correction.
    MaskNotGateable(NodeId),
    UnknownMergeOp(NodeId),
    TwoPlacements,
    /// An origin on a graph that isn't a render copy.
    Origin(NodeId),
    /// The graph feeds back into itself (wireless links count, as they do
    /// in today's ordering).
    Cycle { level: String },
}

impl std::fmt::Display for LiftedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for LiftedError {}

fn kind(n: &crate::Node) -> &str {
    n.legacy.as_ref().and_then(|l| l.payload.get("kind")).and_then(Value::as_str).unwrap_or("")
}

/// The inputs a node must have wired, by its type and kind.
fn required_inputs(n: &crate::Node) -> Vec<crate::PortKey> {
    use crate::ports::input_key;
    match n.type_id.as_str() {
        types::SOURCE | types::GROUP_INPUT | types::GROUP_FRAME => Vec::new(),
        types::GROUP => {
            let frame = n.group.as_ref().is_some_and(|b| b.graph.nodes.iter().any(|x| x.type_id.as_str() == types::GROUP_FRAME));
            if frame { vec!["in".into(), "frame".into()] } else { vec!["in".into()] }
        }
        crate::types::LEGACY_NODE | types::MERGE | types::GROUP_OUTPUT => match kind(n) {
            "Source" | "ExternalMatte" => Vec::new(),
            "Layer" => vec![input_key("Layer", 0), input_key("Layer", 1)],
            // Inputs today fills from another when unwired, which the lift
            // makes explicit.
            "Blend" => vec![input_key("Blend", 0), input_key("Blend", 1)],
            "RGBCombiner" => (0..3).map(|p| input_key("RGBCombiner", p)).collect(),
            "ApplyMatte" => vec![input_key("ApplyMatte", 0), input_key("ApplyMatte", 2)],
            k => vec![input_key(k, 0)],
        },
        _ => vec!["in".into()],
    }
}

fn check_level(g: &Graph, level: &str, body: bool, render_copy: bool) -> Result<(), LiftedError> {
    let (start, end) = if body { (types::GROUP_INPUT, types::GROUP_OUTPUT) } else { (types::SOURCE, types::OUTPUT) };
    for (what, t) in [("start", start), ("end", end)] {
        let count = g.nodes.iter().filter(|n| n.type_id.as_str() == t).count();
        if count != 1 {
            return Err(LiftedError::Ends { level: level.to_owned(), what, count });
        }
    }
    let frames = g.nodes.iter().filter(|n| n.type_id.as_str() == types::GROUP_FRAME).count();
    if frames > usize::from(body) {
        return Err(LiftedError::Ends { level: level.to_owned(), what: "frame", count: frames });
    }
    if g.nodes.iter().find(|n| n.type_id.as_str() == end).map(|n| n.id) != Some(g.output) {
        return Err(LiftedError::Output { level: level.to_owned() });
    }
    let mut ids = std::collections::BTreeSet::new();
    for n in &g.nodes {
        if !ids.insert(n.id) {
            return Err(LiftedError::DuplicateNode(n.id));
        }
        if n.origin.is_some() && !render_copy {
            return Err(LiftedError::Origin(n.id));
        }
    }
    let mut into = std::collections::BTreeSet::new();
    for e in &g.edges {
        for id in [e.from.0, e.to.0] {
            if !ids.contains(&id) {
                return Err(LiftedError::DanglingEdge(id));
            }
        }
        if !into.insert((e.to.0, e.to.1.clone())) {
            return Err(LiftedError::TwoInputs { node: e.to.0, port: e.to.1.to_string() });
        }
        if e.to.1.as_str() == "mask" {
            let target = g.nodes.iter().find(|n| n.id == e.to.0);
            if !target.is_some_and(|n| crate::ports::GATEABLE.contains(&kind(n))) {
                return Err(LiftedError::MaskNotGateable(e.to.0));
            }
        }
    }
    let sender = |name: &str, me: NodeId| g.nodes.iter().any(|o| o.id != me && o.wireless.output_link.as_deref() == Some(name));
    for n in &g.nodes {
        for port in required_inputs(n) {
            let wired = into.contains(&(n.id, port.clone())) || n.wireless.input_links.get(&port).is_some_and(|name| sender(name, n.id));
            if !wired {
                return Err(LiftedError::Unwired { node: n.id, port: port.to_string() });
            }
        }
        if n.type_id.as_str() == types::MERGE {
            let known = matches!(n.params.get(&crate::ParamKey::from("op")), Some(crate::ParamValue::Text(op)) if types::MERGE_OPS.iter().any(|(_, o)| o == op));
            if !known {
                return Err(LiftedError::UnknownMergeOp(n.id));
            }
        }
        if n.type_id.as_str() == types::GROUP {
            if let Some(b) = &n.group {
                check_level(&b.graph, &format!("{level}/{}", n.id), true, render_copy)?;
            }
        }
    }
    if has_cycle(g) {
        return Err(LiftedError::Cycle { level: level.to_owned() });
    }
    if !body && g.nodes.iter().filter(|n| n.type_id.as_str() == types::PLACEMENT).count() > 1 {
        return Err(LiftedError::TwoPlacements);
    }
    Ok(())
}

/// Whether a level's edges and wireless links (receiver after sender) loop.
fn has_cycle(g: &Graph) -> bool {
    use std::collections::{BTreeMap, VecDeque};
    let mut deps: Vec<(NodeId, NodeId)> = g.edges.iter().map(|e| (e.from.0, e.to.0)).collect();
    for r in &g.nodes {
        for name in r.wireless.input_links.values() {
            if let Some(s) = g.nodes.iter().find(|s| s.id != r.id && s.wireless.output_link.as_deref() == Some(name.as_str())) {
                deps.push((s.id, r.id));
            }
        }
    }
    let mut indeg: BTreeMap<NodeId, usize> = g.nodes.iter().map(|n| (n.id, 0)).collect();
    for (_, to) in &deps {
        *indeg.entry(*to).or_default() += 1;
    }
    let mut queue: VecDeque<NodeId> = indeg.iter().filter(|(_, d)| **d == 0).map(|(id, _)| *id).collect();
    let mut seen = 0;
    while let Some(id) = queue.pop_front() {
        seen += 1;
        for (_, to) in deps.iter().filter(|(from, _)| *from == id) {
            let d = indeg.get_mut(to).expect("every edge's end is a node, checked above");
            *d -= 1;
            if *d == 0 {
                queue.push_back(*to);
            }
        }
    }
    seen < indeg.len()
}

impl Graph {
    /// Check a lifted clip graph (§5.3): one source and one output at each
    /// level, every edge naming nodes of its level, one edge at most into a
    /// port, **no unwired required input** (the lifted model has no
    /// implicit reads), `mask` edges only into gateable corrections, known
    /// merge ops, at most one placement, and origins only on a render copy.
    pub fn check_lifted(&self, render_copy: bool) -> Result<(), LiftedError> {
        check_level(self, "clip", false, render_copy)
    }

    /// [`Graph::check_lifted`] for a Grade group body standing alone (a
    /// track or timeline grade, a group stage, a version, a Look): it runs
    /// from `group.input` to `group.output`.
    pub fn check_lifted_body(&self) -> Result<(), LiftedError> {
        check_level(self, "body", true, false)
    }
}
