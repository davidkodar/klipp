//! The graph ops as data (node-graph M2 step 15d): what a writer sends
//! and the app's command carries. Applying them is klipp-graph-lift's
//! `ops` engine; naming them here keeps the crates that only carry a batch
//! (klipp-core's `AppCommand::GraphOps`) off the engine and its rebuilds.

use crate::path::NodePath;
use crate::{Animation, Node, NodeId, ParamKey, ParamValue, PortBinding, PortKey};
use serde_json::Value;

/// One edit of one root's graph.
#[derive(Clone, Debug, PartialEq)]
pub enum GraphOp {
    /// Put `node` after the node at `after`: whatever read that node reads
    /// the new one, which reads it.
    AddNode { after: NodePath, node: Box<Node> },
    /// Put `node` at the level of the node `at` names, wired to nothing (a
    /// matte made beside the picture's chain): its inputs and its reader
    /// are fed by `Connect`, and nothing else in the graph changes. The
    /// inputs its type needs are connected in the same batch, which is
    /// checked whole at its end; alone, such a node is refused. Its output
    /// may go unread: an unused producer is harmless, and stays.
    AddNodeBeside { at: NodePath, node: Box<Node> },
    /// Take a node out: whatever read it reads its primary input.
    RemoveNode { path: NodePath },
    /// Feed `to`'s `to_port` from `from`'s `from_port`, replacing what fed
    /// it. Both nodes at one level.
    Connect { from: NodePath, from_port: PortKey, to: NodePath, to_port: PortKey },
    /// Leave `to`'s `port` unfed.
    Disconnect { to: NodePath, port: PortKey },
    SetParam { path: NodePath, key: ParamKey, value: ParamValue },
    SetEnabled { path: NodePath, enabled: bool },
    SetMix { path: NodePath, mix: Option<f32> },
    /// Expose a direct child's output under a stable public key.
    /// A clip grade group with published ports cannot yet load into the
    /// legacy app model; delift refuses it until that model can preserve them.
    PublishOutput { group: NodePath, endpoint: PortBinding, key: PortKey, label: String },
    /// Remove an exported port; existing readers must be disconnected first.
    Unpublish { group: NodePath, key: PortKey },
    /// None clears the saved setting, restoring migrated red semantics.
    SetMaskChannel { path: NodePath, channel: Option<String> },
    /// Move a node to just after another at its level.
    MoveAfter { path: NodePath, after: NodeId },
    /// Name a node, or place it in the node editor: what the editor shows,
    /// never the picture. `None` leaves that part as it is.
    SetNodeMeta { path: NodePath, label: Option<String>, position: Option<Value> },
    /// Set the frames and sticky notes drawn on the node editor's canvas
    /// at the level of the node `path` names, each list whole (#1743):
    /// what the editor shows, never the picture.
    SetAnnotations { path: NodePath, frames: Vec<Value>, sticky_notes: Vec<Value> },
    /// Put `keyframe` on a parameter's animation, in time order, replacing
    /// a key at the same time. Stored as the keyframe's own JSON (time,
    /// value, interpolation, tangents), which the renderer reads typed.
    AddKeyAt { path: NodePath, key: ParamKey, keyframe: Value },
    /// Take the key at `time_secs` off a parameter's animation; the
    /// parameter is still animated while it has keys or a modifier stack.
    RemoveKeyAt { path: NodePath, key: ParamKey, time_secs: f64 },
    /// Set a parameter's animation whole (generated keys, a modifier
    /// stack); `None` takes it off.
    SetAnim { path: NodePath, key: ParamKey, anim: Option<Animation> },
    /// Replace a legacy entry's stored payload whole: an effect edited in
    /// its own panel, whose colours and lists no parameter value carries,
    /// until effects take typed parameters (M3 step 19). Of the same kind,
    /// and it must read as the frozen v1 types read it.
    SetPayload { path: NodePath, payload: Value },
    /// Pack `nodes`, all at the level of the node `path` names (one of
    /// them), into a new grade compound `group` in their place: what fed
    /// them feeds it, what they fed reads it (#1476).
    Group { path: NodePath, nodes: Vec<NodeId>, group: NodeId },
    /// Dissolve the grade compound `path` names into its level: what fed
    /// it feeds what its input fed, what read it reads what fed its output.
    Ungroup { path: NodePath },
}

impl GraphOp {
    /// The node the op names: the one it edits, or the one an added node
    /// goes after. Its root is the picture the op edits.
    pub fn path(&self) -> &NodePath {
        match self {
            Self::AddNode { after, .. } => after,
            Self::AddNodeBeside { at, .. } => at,
            Self::RemoveNode { path }
            | Self::SetParam { path, .. }
            | Self::SetEnabled { path, .. }
            | Self::SetMix { path, .. }
            | Self::SetMaskChannel { path, .. }
            | Self::MoveAfter { path, .. }
            | Self::SetNodeMeta { path, .. }
            | Self::SetAnnotations { path, .. }
            | Self::AddKeyAt { path, .. }
            | Self::RemoveKeyAt { path, .. }
            | Self::SetAnim { path, .. }
            | Self::SetPayload { path, .. }
            | Self::Group { path, .. }
            | Self::Ungroup { path } => path,
            Self::Connect { to, .. } | Self::Disconnect { to, .. } => to,
            Self::PublishOutput { group, .. } | Self::Unpublish { group, .. } => group,
        }
    }
}
