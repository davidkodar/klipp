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
    /// The static opacity, clamped to 0..1 as the renderer does.
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
}
