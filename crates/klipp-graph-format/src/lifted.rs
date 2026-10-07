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

use crate::{Animation, Graph, NodeId, ParamKey, PortCatalog, PortDirection, PortType, PortSpec};

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
    /// What a reader must understand to read this picture whole; empty for
    /// a picture lifted from a clip's fields. [`PICTURE_TRUTH_CAPABILITY`]
    /// marks a clip whose picture is its truth, holding what its fields
    /// can't (an effect's mix or mask, a published grade port). An unknown
    /// capability is refused, and a reader from before this field refuses
    /// the picture as an unknown field, so neither reads it with the rest
    /// dropped (#1034).
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "known_capabilities")]
    pub requires: Vec<String>,
}

/// The capability a picture-truth clip's stored picture requires.
pub const PICTURE_TRUTH_CAPABILITY: &str = "picture-truth-1";

fn known_capabilities<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let requires = Vec::<String>::deserialize(deserializer)?;
    match requires.iter().find(|r| r.as_str() != PICTURE_TRUTH_CAPABILITY) {
        Some(unknown) => Err(serde::de::Error::custom(format!("unsupported picture capability: {unknown}"))),
        None => Ok(requires),
    }
}

impl ClipPicture {
    /// Whether this is a picture-truth clip's picture.
    pub fn is_truth(&self) -> bool {
        self.requires.iter().any(|r| r == PICTURE_TRUTH_CAPABILITY)
    }
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
    /// Where each parked edge stood among the saved edges, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked_at: Vec<u32>,
    /// On a Grade group body's input: wireless links on a correction's
    /// matte port where its picture port is unwired, never a gate, kept
    /// outside the graph as a parked edge is; each `{"node": <id>, "port":
    /// <port>, "name": <name>}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked_links: Vec<Value>,
    /// The stored enabled bit, where the lift set `enabled` otherwise (an
    /// entry kept off for the record, a head entry that never ran).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was_enabled: Option<bool>,
    /// On a Grade group body's input: saved edges today never reads (to a
    /// node that isn't there, into a port the kind doesn't read, a second
    /// into one port), each `{"at": <index>, "edge": <edge>}`, the index
    /// among the saved edges once the parked ones are out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped_edges: Vec<Value>,
    /// On a Grade group body's input: saved edges from a node that isn't
    /// there, which the lift reads from the frame, each `{"to": <port>,
    /// "from": <the saved source>}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rewired: Vec<Value>,
    /// On a Grade group body's input: the ports the lift wired from
    /// another input (Blend's B, RGBCombiner's G and B), each `[node, port]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filled: Vec<Value>,
    /// On a Grade group body's input: wireless links that went with a
    /// dropped wire, each `{"node": <id>, "port": <port>, "name": <name>}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped_wireless: Vec<Value>,
    /// On a clip's source: the clip's animation fields as stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_anim: Option<crate::LegacyClipAnim>,
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
    /// A clip in a grading group whose wireless link names meet a stage's,
    /// with a sender on either side. Today's bake merges the stage and the
    /// clip into one graph, so a sender on one side can take over a
    /// receiver on the other; in revision 2 links stay inside their group
    /// body. The render could change, so the conversion stops the project.
    CrossGroupWireless { clip: uuid::Uuid, group: uuid::Uuid, names: Vec<String> },
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
    /// A matte on a correction's `mask`, drawn or wireless, without the
    /// gate's `alpha_one`: it would read as gated where today it isn't.
    MaskWithoutGate(NodeId),
    /// A group without a body, or a body on a node that isn't a group.
    GroupBody(NodeId),
    /// The other level's end: a clip's source or output inside a body, a
    /// body's input, output or frame at clip level.
    WrongLevel(NodeId),
    /// An edge from or into a port the node doesn't have.
    UnknownPort { node: NodeId, port: String },
    UnknownMergeOp(NodeId),
    TwoPlacements,
    NonFiniteMix(NodeId),
    MixNotSupported(NodeId),
    InvalidMaskChannel(NodeId),
    DuplicateInterfaceKey { group: NodeId, key: String },
    ReservedInterfaceKey { group: NodeId, key: String },
    InterfaceEndpoint { group: NodeId, key: String },
    InterfaceType { group: NodeId, key: String },
    PortTypeMismatch { node: NodeId, port: String },
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

/// Whether `n` has the port: a grade node its kind's ports, a group its
/// picture and frame inputs, anything else `in` and `out`.
fn has_port(n: &crate::Node, port: &crate::PortKey, input: bool) -> bool {
    use crate::ports::{input_index, output_index};
    match n.type_id.as_str() {
        crate::types::LEGACY_NODE | types::MERGE | types::GROUP_INPUT | types::GROUP_OUTPUT => {
            // A named port: the `p<n>` a kind falls back to is no port of it.
            let index = if input { input_index(kind(n), port) } else { output_index(kind(n), port) };
            index.is_some_and(|i| port.as_str() != format!("p{i}"))
        }
        types::GROUP if input => matches!(port.as_str(), "in" | "frame"),
        _ => port.as_str() == if input { "in" } else { "out" },
    }
}

/// A matte reaches `node`'s `mask`: only a gateable correction takes one,
/// and only gated, with `alpha_one` as the lift sets it.
fn check_mask(g: &Graph, node: NodeId) -> Result<(), LiftedError> {
    let target = g.nodes.iter().find(|n| n.id == node);
    if !target.is_some_and(|n| crate::ports::GATEABLE.contains(&kind(n))) {
        return Err(LiftedError::MaskNotGateable(node));
    }
    if !target.is_some_and(|n| n.legacy_flags.as_ref().is_some_and(|f| f.alpha_one)) {
        return Err(LiftedError::MaskWithoutGate(node));
    }
    Ok(())
}

fn check_level(g: &Graph, level: &str, body: bool, render_copy: bool, catalog: Option<&dyn PortCatalog>) -> Result<(), LiftedError> {
    check_level_running(g, level, body, render_copy, true, catalog, &[])
}

fn structural_direction(n: &crate::Node) -> Option<PortDirection> {
    match n.type_id.as_str() {
        types::SOURCE | types::GROUP_INPUT | types::GROUP_FRAME => Some(PortDirection::Output),
        types::OUTPUT | types::GROUP_OUTPUT => Some(PortDirection::Input),
        _ => None,
    }
}

fn port_spec(n: &crate::Node, port: &crate::PortKey, input: bool, catalog: Option<&dyn PortCatalog>) -> Option<PortSpec> {
    if let Some(direction) = structural_direction(n) {
        return ((direction == PortDirection::Input) == input && port.as_str() == if input { "in" } else { "out" }).then(|| PortSpec {
            key: port.to_string(), ty: PortType::Image, optional: false, params: Vec::new(),
        });
    }
    if n.type_id.as_str() == types::GROUP {
        if let Some(p) = n.group.as_ref()?.interface.iter().find(|p| &p.key == port && (p.direction == PortDirection::Input) == input) {
            let optional = input && n.group.as_ref().and_then(|b| b.graph.nodes.iter().find(|n| n.id == p.binding.node))
                .and_then(|child| port_spec(child, &p.binding.port, true, catalog))
                .map_or(p.ty == PortType::Matte, |bound| bound.optional);
            return Some(PortSpec { key: p.key.to_string(), ty: p.ty, optional, params: Vec::new() });
        }
        if matches!((input, port.as_str()), (true, "in" | "frame") | (false, "out")) {
            return Some(PortSpec { key: port.to_string(), ty: PortType::Image, optional: false, params: Vec::new() });
        }
        return None;
    }
    let spec = catalog?.node_spec(n)?;
    (if input { spec.inputs } else { spec.outputs }).into_iter().find(|p| p.key == port.as_str())
}

fn port_exists(n: &crate::Node, port: &crate::PortKey, input: bool, catalog: Option<&dyn PortCatalog>) -> bool {
    if structural_direction(n).is_some() || n.type_id.as_str() == types::GROUP || catalog.is_some_and(|c| c.node_spec(n).is_some()) {
        port_spec(n, port, input, catalog).is_some()
    } else {
        has_port(n, port, input)
    }
}

fn inputs_required(n: &crate::Node, catalog: Option<&dyn PortCatalog>) -> Vec<crate::PortKey> {
    if let Some(spec) = catalog.and_then(|c| c.node_spec(n)) {
        return spec.inputs.into_iter().filter(|p| !p.optional).map(|p| crate::PortKey::new(p.key)).collect();
    }
    let mut required = required_inputs(n);
    if let Some(body) = &n.group {
        required.extend(body.interface.iter().filter(|p| p.direction == PortDirection::Input && port_spec(n, &p.key, true, catalog).is_some_and(|s| !s.optional)).map(|p| p.key.clone()));
    }
    required
}

fn check_settings(n: &crate::Node, catalog: Option<&dyn PortCatalog>) -> Result<(), LiftedError> {
    if n.mix.is_some_and(|m| !m.is_finite()) { return Err(LiftedError::NonFiniteMix(n.id)); }
    let channel = n.params.get(&ParamKey::from("mask.channel"));
    if channel.is_some_and(|v| !matches!(v, crate::ParamValue::Text(s) if matches!(s.as_str(), "red" | "luminance" | "alpha"))) {
        return Err(LiftedError::InvalidMaskChannel(n.id));
    }
    if let Some(spec) = catalog.and_then(|c| c.node_spec(n)) {
        if n.mix.is_some() && !spec.mixable { return Err(LiftedError::MixNotSupported(n.id)); }
        if let Some(crate::ParamValue::Text(value)) = channel {
            let valid = spec.inputs.iter().find(|p| p.key == "mask").is_some_and(|p| p.params.iter().any(|(key, values)| key == "channel" && values.contains(value)));
            if !valid { return Err(LiftedError::InvalidMaskChannel(n.id)); }
        }
    }
    Ok(())
}

fn check_interface(n: &crate::Node, catalog: Option<&dyn PortCatalog>) -> Result<(), LiftedError> {
    let Some(body) = &n.group else { return Ok(()) };
    let mut keys = std::collections::BTreeSet::new();
    let mut input_bindings = std::collections::BTreeSet::new();
    for p in &body.interface {
        if p.key.as_str().is_empty() || matches!(p.key.as_str(), "in" | "out" | "frame") || body.ports.contains(&p.key) {
            return Err(LiftedError::ReservedInterfaceKey { group: n.id, key: p.key.to_string() });
        }
        if !keys.insert(&p.key) { return Err(LiftedError::DuplicateInterfaceKey { group: n.id, key: p.key.to_string() }); }
        let input = p.direction == PortDirection::Input;
        let endpoint_error = || LiftedError::InterfaceEndpoint { group: n.id, key: p.key.to_string() };
        let child = body.graph.nodes.iter().find(|c| c.id == p.binding.node).ok_or_else(endpoint_error)?;
        if !port_exists(child, &p.binding.port, input, catalog) { return Err(endpoint_error()); }
        if port_spec(child, &p.binding.port, input, catalog).is_some_and(|s| s.ty != p.ty) {
            return Err(LiftedError::InterfaceType { group: n.id, key: p.key.to_string() });
        }
        if input && !input_bindings.insert((p.binding.node, &p.binding.port)) { return Err(endpoint_error()); }
    }
    Ok(())
}

fn check_mask_with_catalog(g: &Graph, node: NodeId, catalog: Option<&dyn PortCatalog>) -> Result<(), LiftedError> {
    if let Some(n) = g.nodes.iter().find(|n| n.id == node) {
        // A published group input named mask is a typed interface port,
        // not an effect's implicit mask/mix control.
        if n.type_id.as_str() == types::GROUP && port_spec(n, &"mask".into(), true, catalog).is_some() { return Ok(()); }
        if let Some(spec) = catalog.and_then(|c| c.node_spec(n)) {
            return if spec.inputs.iter().any(|p| p.key == "mask" && p.ty == PortType::Matte) { Ok(()) } else { Err(LiftedError::MaskNotGateable(node)) };
        }
    }
    check_mask(g, node)
}

/// `running` is false inside a disabled group, which never runs: a loop
/// there is kept as saved (today's renderer refused it and passed the
/// picture through), so only there is it allowed.
fn check_level_running(g: &Graph, level: &str, body: bool, render_copy: bool, running: bool, catalog: Option<&dyn PortCatalog>, supplied: &[(NodeId, crate::PortKey)]) -> Result<(), LiftedError> {
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
    let other_level: &[&str] = if body { &[types::SOURCE, types::OUTPUT] } else { &[types::GROUP_INPUT, types::GROUP_OUTPUT, types::GROUP_FRAME] };
    for n in &g.nodes {
        check_settings(n, catalog)?;
        check_interface(n, catalog)?;
        if !ids.insert(n.id) {
            return Err(LiftedError::DuplicateNode(n.id));
        }
        if (n.type_id.as_str() == types::GROUP) != n.group.is_some() {
            return Err(LiftedError::GroupBody(n.id));
        }
        if other_level.contains(&n.type_id.as_str()) {
            return Err(LiftedError::WrongLevel(n.id));
        }
        if n.origin.is_some() && !render_copy {
            return Err(LiftedError::Origin(n.id));
        }
    }
    let mut into: std::collections::BTreeSet<_> = supplied.iter().cloned().collect();
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
            check_mask_with_catalog(g, e.to.0, catalog)?;
        }
        for (id, port, input) in [(e.from.0, &e.from.1, false), (e.to.0, &e.to.1, true)] {
            let n = g.nodes.iter().find(|n| n.id == id);
            if !n.is_some_and(|n| port_exists(n, port, input, catalog)) {
                return Err(LiftedError::UnknownPort { node: id, port: port.to_string() });
            }
        }
        let from = g.nodes.iter().find(|n| n.id == e.from.0).and_then(|n| port_spec(n, &e.from.1, false, catalog));
        let to = g.nodes.iter().find(|n| n.id == e.to.0).and_then(|n| port_spec(n, &e.to.1, true, catalog));
        if let (Some(from), Some(to)) = (from, to) {
            // Image masks deliberately extract coverage; other ports are exact typed connections.
            let extracts_mask = e.to.1.as_str() == "mask" && g.nodes.iter().any(|n| n.id == e.to.0 && n.type_id.as_str() != types::GROUP);
            if from.ty != to.ty && !extracts_mask {
                return Err(LiftedError::PortTypeMismatch { node: e.to.0, port: e.to.1.to_string() });
            }
        }
    }
    let sender = |name: &str, me: NodeId| g.nodes.iter().any(|o| o.id != me && o.wireless.output_link.as_deref() == Some(name));
    for n in &g.nodes {
        if n.wireless.input_links.get(&crate::PortKey::from("mask")).is_some_and(|name| sender(name, n.id)) {
            check_mask_with_catalog(g, n.id, catalog)?;
        }
    }
    for n in &g.nodes {
        // Wired inputs take precedence; validate only wireless dependencies
        // that actually supply a registered or published port.
        if n.type_id.as_str() == types::GROUP || catalog.is_some_and(|c| c.node_spec(n).is_some()) {
            for (port, name) in &n.wireless.input_links {
                if into.contains(&(n.id, port.clone())) { continue; }
                let Some(source) = g.nodes.iter().find(|s| s.id != n.id && s.wireless.output_link.as_deref() == Some(name)) else { continue };
                let Some(target) = port_spec(n, port, true, catalog) else {
                    return Err(LiftedError::UnknownPort { node: n.id, port: port.to_string() });
                };
                if port_spec(source, &"out".into(), false, catalog).is_some_and(|s| s.ty != target.ty) && (port.as_str() != "mask" || n.type_id.as_str() == types::GROUP) {
                    return Err(LiftedError::PortTypeMismatch { node: n.id, port: port.to_string() });
                }
            }
        }
        for port in inputs_required(n, catalog) {
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
                let supplied: Vec<_> = b.interface.iter().filter(|p| p.direction == PortDirection::Input).map(|p| (p.binding.node, p.binding.port.clone())).collect();
                check_level_running(&b.graph, &format!("{level}/{}", n.id), true, render_copy, running && n.enabled, catalog, &supplied)?;
            }
        }
    }
    if running && g.has_cycle() {
        return Err(LiftedError::Cycle { level: level.to_owned() });
    }
    if !body && g.nodes.iter().filter(|n| n.type_id.as_str() == types::PLACEMENT).count() > 1 {
        return Err(LiftedError::TwoPlacements);
    }
    Ok(())
}

impl Graph {
    /// Whether this level's edges and wireless links (receiver after
    /// sender, as today's ordering counts them) loop.
    pub fn has_cycle(&self) -> bool {
        has_cycle(self)
    }
}

fn has_cycle(g: &Graph) -> bool {
    use std::collections::{BTreeMap, VecDeque};
    let mut deps: Vec<(NodeId, NodeId)> = g.edges.iter().map(|e| (e.from.0, e.to.0)).collect();
    for r in &g.nodes {
        for (port, name) in &r.wireless.input_links {
            // A wired port reads its wire, and its wireless link is no
            // dependency, as today's `iter_dependencies` has it.
            if g.edges.iter().any(|e| e.to.0 == r.id && &e.to.1 == port) {
                continue;
            }
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
            if let Some(d) = indeg.get_mut(to) {
                *d -= 1;
                if *d == 0 {
                    queue.push_back(*to);
                }
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
        check_level(self, "clip", false, render_copy, None)
    }

    /// [`Graph::check_lifted`] for a Grade group body standing alone (a
    /// track or timeline grade, a group stage, a version, a Look): it runs
    /// from `group.input` to `group.output`.
    pub fn check_lifted_body(&self) -> Result<(), LiftedError> {
        check_level(self, "body", true, false, None)
    }
}

impl Graph {
    /// Validate registry-defined ports, settings and mixability as well as structure.
    pub fn check_lifted_with_catalog(&self, render_copy: bool, catalog: &dyn PortCatalog) -> Result<(), LiftedError> {
        check_level(self, "clip", false, render_copy, Some(catalog))
    }

    /// Validate a standalone group body against registry metadata.
    pub fn check_lifted_body_with_catalog(&self, catalog: &dyn PortCatalog) -> Result<(), LiftedError> {
        check_level(self, "body", true, false, Some(catalog))
    }
}
