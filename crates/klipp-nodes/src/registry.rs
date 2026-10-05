//! The grade's node types, described with the same types as the built-in
//! effects ([`klipp_effects::registry`]): ports, parameters with their
//! defaults, ranges, units and whether they can be keyframed (#1003).
//!
//! Empty until #1009 fills it in. `tests/registry_coverage.rs` lists every
//! [`NodeKind`] not yet described and checks each description against
//! the code: the keyframable parameters against
//! [`NodeParams::accepts`](crate::NodeParams::accepts), the ports against
//! [`NodeKind::input_port_count`] and [`NodeKind::output_port_count`].

use crate::NodeKind;
use klipp_effects::registry::NodeTypeDesc;

/// A grade node's description, tied to its kind.
#[derive(Debug, Clone, Copy)]
pub struct NodeType {
    pub desc: NodeTypeDesc,
    pub kind: NodeKind,
}

/// Every grade node type described so far.
pub fn node_types() -> impl Iterator<Item = &'static NodeType> {
    TYPES.iter()
}

/// The description of `kind`, if it is described.
pub fn node_type(kind: NodeKind) -> Option<&'static NodeType> {
    node_types().find(|t| t.kind == kind)
}

static TYPES: &[NodeType] = &[];
