//! How a clip's picture graph is written down: one definition that the
//! app and the project converter both read.
//!
//! This is revision 1 of the project format, the structural shape. Every
//! entry of today's model (an effect, a plug-in, a grade node, the clip's
//! opacity and transform keys) becomes a `klipp.legacy.*` node carrying
//! its old payload verbatim as JSON, so converting into this shape loses
//! nothing and can be undone exactly. Nothing here reinterprets an old
//! value; lifted content (revision 2) is defined when the app moves to it.
//!
//! # Key types
//!
//! * [`Graph`], [`Node`], [`Edge`]: parameters and ports are named by
//!   string keys ([`ParamKey`], [`PortKey`]), never by index.
//! * [`Legacy`]: an old entry's payload, list slot, keyframes and
//!   modifiers, kept as they were read.
//! * [`ClipRecordV2`]: a clip's graph plus the clip-level animation fields
//!   kept verbatim ([`LegacyClipAnim`]).
//! * [`ports`]: today's grade-node port numbers as port keys, and back.
//!
//! Every map is ordered, so the same value always writes the same bytes.
//!
//! Every struct refuses fields it doesn't know: a file written by a newer
//! build must fail to read, never lose what this build can't hold.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

/// The schema of a serialised [`Graph`], written as `"graph_version"`.
pub const GRAPH_VERSION: u32 = 2;

/// The project `format_revision` whose graphs hold only the legacy
/// vocabulary ([`types`]).
pub const STRUCTURAL_REVISION: u32 = 1;

/// A node's id: stable for the node's life. Old grade nodes keep their
/// UUIDs; entries that had none get a fixed id from the converter.
pub type NodeId = Uuid;

macro_rules! string_key {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(key: impl Into<String>) -> Self {
                Self(key.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(key: &str) -> Self {
                Self(key.to_owned())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_key!(
    /// A parameter's name on its node (`"radius"`, `"center.x"`).
    ParamKey
);
string_key!(
    /// A port's name on its node (`"in"`, `"mask"`, `"fg"`, `"out"`).
    PortKey
);
string_key!(
    /// What a node is: `"klipp.legacy.effect"`, later `"klipp.fx.gaussian_blur"`,
    /// `"ofx:<identifier>"` and so on.
    NodeTypeId
);

/// The node types of revision 1. Each carries a [`Legacy`] payload.
pub mod types {
    /// The clip's grade graph as it was, unwired ports and all, held as
    /// the group's body.
    pub const LEGACY_GRADE: &str = "klipp.legacy.grade";
    /// One node of an old grade graph, keeping its UUID; the payload holds
    /// its kind and parameters, the keyframes its keyframe tracks.
    pub const LEGACY_NODE: &str = "klipp.legacy.node";
    /// An OFX plug-in instance from the clip's or track's plug-in list.
    pub const LEGACY_OFX: &str = "klipp.legacy.ofx";
    /// A built-in effect from an effect list.
    pub const LEGACY_EFFECT: &str = "klipp.legacy.effect";
    /// The built-in effects that later become clip properties or move in
    /// the render order, kept apart so the lift can find them.
    pub const LEGACY_OPACITY: &str = "klipp.legacy.opacity";
    pub const LEGACY_TRANSFORM: &str = "klipp.legacy.transform";
    pub const LEGACY_SPEED_RAMP: &str = "klipp.legacy.speed_ramp";
    pub const LEGACY_TEMPORAL_DENOISE: &str = "klipp.legacy.temporal_denoise";

    /// Every type a revision-1 graph may hold.
    pub const STRUCTURAL: [&str; 8] = [
        LEGACY_GRADE,
        LEGACY_NODE,
        LEGACY_OFX,
        LEGACY_EFFECT,
        LEGACY_OPACITY,
        LEGACY_TRANSFORM,
        LEGACY_SPEED_RAMP,
        LEGACY_TEMPORAL_DENOISE,
    ];
}

/// One picture graph: a clip's, a track's, a timeline grade, a group
/// stage or a group's body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    /// Always [`GRAPH_VERSION`]; [`Graph::check`] refuses anything else.
    pub graph_version: u32,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// The node whose output is the graph's picture.
    pub output: NodeId,
    #[serde(default, skip_serializing_if = "GraphUi::is_empty")]
    pub ui: GraphUi,
    /// The graph's own working colour space, as the old graph wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub working_space_override: Option<Value>,
}

/// Where the node editor draws things. Pure layout, kept as it was read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphUi {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub positions: BTreeMap<NodeId, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sticky_notes: Vec<Value>,
}

impl GraphUi {
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty() && self.frames.is_empty() && self.sticky_notes.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: NodeId,
    pub type_id: NodeTypeId,
    #[serde(default = "first_version", skip_serializing_if = "is_first_version")]
    pub type_version: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub enabled: bool,
    /// The parameters that are present. Absence can mean "don't run this
    /// part", not only "use the default".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<ParamKey, ParamValue>,
    /// Animated parameters. Empty on legacy nodes, whose keyframes stay in
    /// [`Legacy::keyframes`] as stored.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub anim: BTreeMap<ParamKey, Animation>,
    /// Only on node types that declare themselves mixable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix: Option<f32>,
    #[serde(default, skip_serializing_if = "Wireless::is_empty")]
    pub wireless: Wireless,
    /// The body of a group node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Box<GroupBody>>,
    /// The old entry this node holds, on every `klipp.legacy.*` node and
    /// every node the lift made from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy: Option<Legacy>,
    /// v1 behaviours a lifted node keeps ([`lifted::LegacyFlags`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_flags: Option<lifted::LegacyFlags>,
    /// Where a spliced node came from; `None` for the clip's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<lifted::Origin>,
}

/// Inputs and outputs joined by name instead of by a drawn edge.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wireless {
    /// The name this node's main output is sent under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_link: Option<String>,
    /// For each receiving port, the name it listens to.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input_links: BTreeMap<PortKey, String>,
}

impl Wireless {
    pub fn is_empty(&self) -> bool {
        self.output_link.is_none() && self.input_links.is_empty()
    }
}

/// One parameter's animation: its keys and modifier stack in today's
/// stored form. The lifted form (revision 2) gives them their own types.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Animation {
    pub keys: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub modifiers: Option<Value>,
}

/// A connection's value type, independent of rendering crates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    Image,
    Matte,
}

/// Whether a published port receives or produces a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortDirection {
    Input,
    Output,
}

/// A port on a direct child of a group, addressed independently of order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortBinding {
    pub node: NodeId,
    pub port: PortKey,
}

/// A group's stable public port and the child endpoint it exposes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupPort {
    pub key: PortKey,
    pub direction: PortDirection,
    pub ty: PortType,
    pub binding: PortBinding,
    /// Display-only: changing this never changes connections.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

/// Capability required to interpret a nonempty typed group interface.
pub const GROUP_INTERFACE_CAPABILITY: &str = "group-interface-1";

/// What a group node holds: its graph and the ports it shows outside.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupBody {
    pub graph: Graph,
    /// Legacy names, kept byte-for-byte; not reinterpreted as bindings.
    pub ports: Vec<PortKey>,
    pub interface: Vec<GroupPort>,
}

impl Serialize for GroupBody {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            graph: &'a Graph,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            ports: &'a Vec<PortKey>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            interface: &'a Vec<GroupPort>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            requires: Vec<&'static str>,
        }
        Wire {
            graph: &self.graph,
            ports: &self.ports,
            interface: &self.interface,
            requires: if self.interface.is_empty() { Vec::new() } else { vec![GROUP_INTERFACE_CAPABILITY] },
        }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for GroupBody {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            graph: Graph,
            #[serde(default)]
            ports: Vec<PortKey>,
            #[serde(default)]
            interface: Vec<GroupPort>,
            #[serde(default)]
            requires: Vec<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        for capability in &wire.requires {
            if capability != GROUP_INTERFACE_CAPABILITY {
                return Err(serde::de::Error::custom(format!("unsupported graph capability: {capability}")));
            }
        }
        if !wire.interface.is_empty() && !wire.requires.iter().any(|r| r == GROUP_INTERFACE_CAPABILITY) {
            return Err(serde::de::Error::custom("typed group interface requires group-interface-1"));
        }
        Ok(Self { graph: wire.graph, ports: wire.ports, interface: wire.interface })
    }
}

/// Registry information passed into format validation, without renderer dependencies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortSpec {
    pub key: String,
    pub ty: PortType,
    pub optional: bool,
    /// Port setting key and its allowed text values (for example channel).
    pub params: Vec<(String, Vec<String>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeSpec {
    pub inputs: Vec<PortSpec>,
    pub outputs: Vec<PortSpec>,
    pub mixable: bool,
}

/// Resolve registered types. Unknown types retain structural validation.
pub trait PortCatalog {
    fn node(&self, type_id: &str) -> Option<NodeSpec>;

    /// Adapters may resolve a legacy payload's kind without changing its saved type id.
    fn node_spec(&self, node: &Node) -> Option<NodeSpec> {
        self.node(node.type_id.as_str())
    }

    /// The declared picture input followed by a stack projection. This
    /// reads descriptors, never edge order. A source has no primary input.
    /// Adapters can override it for registered types with another spine.
    fn primary_input(&self, node: &Node) -> Option<PortKey> {
        use lifted::types as lt;
        if matches!(node.type_id.as_str(), lt::SOURCE | lt::GROUP_INPUT | lt::GROUP_FRAME) {
            return None;
        }
        if let Some(spec) = self.node_spec(node) {
            return spec.inputs.iter().find(|p| p.key == "bg")
                .or_else(|| spec.inputs.iter().find(|p| p.key == "in"))
                .or_else(|| spec.inputs.iter().find(|p| p.ty == PortType::Image))
                .map(|p| PortKey::new(&p.key));
        }
        if matches!(node.type_id.as_str(), types::LEGACY_NODE | lt::MERGE | lt::GROUP_OUTPUT) {
            let kind = node.legacy.as_ref().and_then(|l| l.payload.get("kind")).and_then(Value::as_str).unwrap_or("");
            return match kind {
                "Source" | "ExternalMatte" => None,
                "Layer" => Some("bg".into()),
                _ => Some(ports::input_key(kind, 0)),
            };
        }
        Some("in".into())
    }

    /// Explicit permission to move across the scene-to-display boundary.
    /// A numeric kernel's working encoding is not this capability. Unknown
    /// types and catalogs without a dual-domain declaration refuse the move.
    fn allows_both_picture_domains(&self, _node: &Node) -> bool { false }

    /// Whether the node reads neighbouring source frames, rather than
    /// neighbours on the effect chain. Analysis-driven effects are not
    /// source-neighbour nodes. Existing op validation does not call this.
    fn needs_source_neighbours(&self, node: &Node) -> bool {
        matches!(node.type_id.as_str(), lifted::types::TEMPORAL_DENOISE | lifted::types::FRAME_BLEND)
    }
}

/// An old entry, exactly as it was read. Each part is JSON so that no
/// default, rename or normalisation can creep in: an absent part stays
/// absent and a `null` stays `null`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Legacy {
    /// Its position in the list it came from (an effect list, a plug-in
    /// list); `None` for a grade node, whose graph holds its place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u32>,
    /// The entry itself: an effect, a plug-in instance, or a grade node's
    /// kind and parameters.
    pub payload: Value,
    /// Its keyframes as stored: a grade node's tracks, or the effect
    /// keyframes that named this entry's slot.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub keyframes: Option<Value>,
    /// The modifier stacks that named this entry, keyed as stored.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub modifiers: BTreeMap<String, Value>,
}

/// An edge from one node's output port to another's input port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: (NodeId, PortKey),
    pub to: (NodeId, PortKey),
}

/// A parameter's value. Revision 1 keeps every old value inside
/// [`Legacy::payload`]; these are the typed values new node types use.
/// Curves, gradients, masks and file references join with the types that
/// need them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ParamValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    Enum(u32),
    Color { rgb: [f32; 3], space: String },
    Point2([f32; 2]),
    Text(String),
    Matrix3([f32; 9]),
    /// A number an f32 can't hold exactly: a frei0r plug-in's double
    /// (picture revision 4). A reader before it refuses the variant.
    Float64(f64),
    /// A position an f32 can't hold exactly: a frei0r plug-in's.
    Point2F64([f64; 2]),
    /// An OFX plug-in's three-number value, at full precision.
    Point3F64([f64; 3]),
    /// An OFX plug-in's two- and three-integer values.
    Int2([i32; 2]),
    Int3([i32; 3]),
    /// An OFX plug-in's choice: its index, which may be negative.
    Choice(i32),
    /// An OFX plug-in's colour at full precision. Without `alpha` (an RGB
    /// value) `rgba[3]` is 0 and never read, so equal colours compare equal.
    ColorF64 { rgba: [f64; 4], alpha: bool },
}

/// A clip's picture as revision 1 writes it: the graph, plus the clip's
/// own opacity and transform keyframes and their modifier stacks, kept
/// verbatim because only the lift decides where they belong.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipRecordV2 {
    pub graph: Graph,
    #[serde(default, skip_serializing_if = "LegacyClipAnim::is_empty")]
    pub legacy_clip: LegacyClipAnim,
}

/// The clip-level animation fields, as stored. Each keeps the old field's
/// name and value; absent stays absent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyClipAnim {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Value>,
    /// The clip's picture modifier stacks that belong to no effect node:
    /// opacity, position, scale and rotation, keys today's reader doesn't
    /// know, and effect keys that name no slot. Keyed as stored. (Audio
    /// stacks stay clip fields.)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub param_modifiers: BTreeMap<String, Value>,
    /// Effect keyframes that claim no effect: a key that doesn't read as
    /// `"<slot>:<param>"`, or names a slot past the list. Keyed as stored.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effect_keyframes: BTreeMap<String, Value>,
}

impl LegacyClipAnim {
    /// The clip fields this record may hold.
    pub const FIELDS: [&'static str; 7] = [
        "opacity_keyframes",
        "transform_pos_x_keyframes",
        "transform_pos_y_keyframes",
        "transform_scale_keyframes",
        "transform_rotation_keyframes",
        "transform_tilt_x_keyframes",
        "transform_tilt_y_keyframes",
    ];

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.param_modifiers.is_empty() && self.effect_keyframes.is_empty()
    }
}

/// Why a graph isn't a valid revision-1 graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    WrongGraphVersion(u32),
    DuplicateNode(NodeId),
    MissingOutput(NodeId),
    DanglingEdge(NodeId),
    NotStructural { node: NodeId, type_id: String },
    MissingLegacy(NodeId),
    MissingGroupBody(NodeId),
    UnsupportedGroupInterface(NodeId),
    UnknownClipField(String),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongGraphVersion(v) => write!(f, "graph_version {v}, expected {GRAPH_VERSION}"),
            Self::DuplicateNode(id) => write!(f, "node {id} appears twice in one graph"),
            Self::MissingOutput(id) => write!(f, "the output node {id} isn't in the graph"),
            Self::DanglingEdge(id) => write!(f, "an edge names node {id}, which isn't in the graph"),
            Self::NotStructural { node, type_id } => {
                write!(f, "node {node} is a {type_id}, which revision {STRUCTURAL_REVISION} can't hold")
            }
            Self::MissingLegacy(id) => write!(f, "legacy node {id} carries no old entry"),
            Self::MissingGroupBody(id) => write!(f, "group node {id} has no body"),
            Self::UnsupportedGroupInterface(id) => write!(f, "legacy group {id} cannot hold a typed interface"),
            Self::UnknownClipField(name) => write!(f, "{name} isn't a clip animation field"),
        }
    }
}

impl std::error::Error for FormatError {}

impl Graph {
    /// A graph with nothing in it: no nodes, no edges, and the nil id as
    /// its output. A store with no picture of its own is written so.
    pub fn empty() -> Self {
        Self {
            graph_version: GRAPH_VERSION,
            nodes: Vec::new(),
            edges: Vec::new(),
            output: Uuid::nil(),
            ui: GraphUi::default(),
            working_space_override: None,
        }
    }

    /// Whether this is [`Graph::empty`], the one converter-built shape
    /// whose output names no node.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
            && self.edges.is_empty()
            && self.output.is_nil()
            && self.ui.is_empty()
            && self.working_space_override.is_none()
    }

    /// Check the graph is a valid revision-1 graph, its grade bodies too.
    ///
    /// A graph the converter builds is checked strictly: the version,
    /// unique node ids, an output and edges that name nodes of this level,
    /// and only legacy nodes, each carrying its old entry. The body of a
    /// `klipp.legacy.grade` group is an old grade graph kept **as it was
    /// saved**, so its ids, edges and output aren't checked: a dangling
    /// edge or a missing output that today's reader accepted stays as it
    /// is, for the lift to judge. Its nodes must still be
    /// `klipp.legacy.node`s carrying their old entry.
    pub fn check(&self) -> Result<(), FormatError> {
        self.check_level(false)
    }

    fn check_level(&self, grade_body: bool) -> Result<(), FormatError> {
        if self.graph_version != GRAPH_VERSION {
            return Err(FormatError::WrongGraphVersion(self.graph_version));
        }
        if !grade_body && self.is_empty() {
            return Ok(());
        }
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            if !grade_body && !ids.insert(node.id) {
                return Err(FormatError::DuplicateNode(node.id));
            }
            let type_id = node.type_id.as_str();
            let allowed = if grade_body { type_id == types::LEGACY_NODE } else { types::STRUCTURAL.contains(&type_id) && type_id != types::LEGACY_NODE };
            if !allowed {
                return Err(FormatError::NotStructural { node: node.id, type_id: node.type_id.0.clone() });
            }
            if node.legacy.is_none() {
                return Err(FormatError::MissingLegacy(node.id));
            }
            match (&node.group, type_id == types::LEGACY_GRADE) {
                (Some(body), true) => {
                    if !body.interface.is_empty() { return Err(FormatError::UnsupportedGroupInterface(node.id)); }
                    body.graph.check_level(true)?;
                },
                (None, true) => return Err(FormatError::MissingGroupBody(node.id)),
                (Some(_), false) => return Err(FormatError::NotStructural { node: node.id, type_id: node.type_id.0.clone() }),
                (None, false) => {}
            }
        }
        if grade_body {
            return Ok(());
        }
        if !ids.contains(&self.output) {
            return Err(FormatError::MissingOutput(self.output));
        }
        for edge in &self.edges {
            for (node, _) in [&edge.from, &edge.to] {
                if !ids.contains(node) {
                    return Err(FormatError::DanglingEdge(*node));
                }
            }
        }
        Ok(())
    }
}

impl ClipRecordV2 {
    pub fn check(&self) -> Result<(), FormatError> {
        if let Some(name) = self.legacy_clip.fields.keys().find(|k| !LegacyClipAnim::FIELDS.contains(&k.as_str())) {
            return Err(FormatError::UnknownClipField(name.clone()));
        }
        self.graph.check()
    }
}

/// Read an optional JSON part so that a present `null` stays `Some(Null)`
/// instead of turning into `None`, which would drop it on the next write.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

fn first_version() -> u32 {
    1
}

fn is_first_version(v: &u32) -> bool {
    *v == 1
}

fn yes() -> bool {
    true
}

fn is_yes(v: &bool) -> bool {
    *v
}

pub mod lifted;
pub mod op;
pub mod path;
pub mod ports;

#[cfg(test)]
mod tests;
