//! Node-graph data model and executor for clip-level colour grading.
//!
//! A [`NodeGraph`] is a directed acyclic graph of [`Node`]s connected
//! by [`Edge`]s. Each clip owns one. Graph nodes are strongly typed by
//! [`NodeKind`] (Exposure, Saturation, Wheels, Curves, Qualifier,
//! LUT3D, Tone, Huesat, ColorMatch, ApplyMatte, Blend, …); the
//! [`executor::GraphExecutor`] walks them in topological order and
//! produces the final RGBA buffer.
//!
//! # Execution
//!
//! [`GraphExecutor::execute`] runs one frame through the graph:
//!
//! 1. `topo_sort` (Kahn's algorithm) — rejects cycles.
//! 2. For each node in order, dispatch on [`NodeKind`] to the
//!    matching op in [`klipp_color`] or [`klipp_effects`]. GPU paths
//!    run through a shared wgpu handle; CPU paths are the fallback.
//! 3. Keyframed params ([`NodeParams`] / [`NodeKeyframeTracks`]) are
//!    evaluated at the current time before each node runs.
//!
//! [`bake_graph_to_lut`] pre-renders a full-chain LUT for static
//! grades — lets the viewer skip the graph walk on cached frames.
//!
//! # Gotchas
//!
//! * The CPU path clones `primary_input.to_vec()` at the start of each
//!   node arm; with N nodes on a 4K frame that's N × 32 MB of
//!   allocations per frame. A `Cow<[u8]>` pipeline is the logical
//!   improvement.
//! * [`params::NodeParams::apply_override`] returns whether it recognised the name,
//!   and [`params::NodeParams::accepts`] asks the same question without
//!   writing anything — use one of them on any path where the name did
//!   not come from the inspector, or a typo reads as "the grade did not
//!   work".

pub mod graph;
pub mod opacity_gradient;
mod restore_background;
pub mod kinds;
pub mod params;
pub mod defaults;
pub mod domains;
pub mod executor;
pub mod decline;
pub mod fault;
pub mod frame_context;
pub mod frei0r_node;
pub mod held;
pub mod plugin;
mod plan;
pub mod auto_layout;
pub mod json_check;
pub mod matte_sequence;
pub mod registry;
pub mod side_picture;

pub use graph::{CannotAppend, Edge, Node, NodeFrame, NodeFrameId, NodeGraph, NodeId, NodePos, NodeStickyNote, NodeStickyNoteId, effective_signal_role_at};
pub use auto_layout::auto_layout;
pub use decline::{Decline, DeclineOutcome};
pub use kinds::{GradeStage, NodeKind, PortRole};
pub use params::{
    KeyMixerInput, KeyMixerParams, LayerMixerParams, MatteChannel, MatteCombineOp, NodeKeyframeTrack, NodeKeyframeTracks, NodeParams, SubjectMatte,
};
// Re-exported from klipp_core; surfaced here so callers writing to
// node params don't need to reach across crates.
pub use klipp_core::{BlendMode, LayerMixerInput};
pub use executor::{
    ExecutorError, GraphExecutor, bake_graph_to_lut, effective_enabled, ENABLED_TRACK, THUMB_W,
    THUMB_H,
};
