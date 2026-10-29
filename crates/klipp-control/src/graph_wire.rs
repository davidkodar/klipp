//! The public graph vocabulary: full paths, and references to earlier adds.

use std::collections::BTreeMap;
use klipp_graph_format::path::{NodePath, Root, TrackPart};
use klipp_graph_format::lifted::Stage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Check raw keys before serde flattens a command into its socket envelope.
/// MCP arguments omit `id` and `method`; socket requests may carry them.
pub fn validate_fields(value: &Value, method: &str, envelope: bool) -> Result<(), String> {
    let allowed: &[&str] = match method {
        "graph_ops" => &["ops", "label", "expected_revision", "expected_project_identity", "idempotency_key"],
        "graph_read" => &["path", "depth", "idempotency_key"],
        _ => return Ok(()),
    };
    let fields = value.as_object().ok_or("graph arguments must be an object")?;
    for key in fields.keys() {
        if !allowed.contains(&key.as_str()) && !(envelope && matches!(key.as_str(), "id" | "method")) {
            return Err(format!("unknown {method} field {key:?}"));
        }
    }
    Ok(())
}

fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum GraphRoot {
    Clip { clip: Uuid },
    Timeline { timeline_grade: Uuid },
    Group { group_stage: Uuid, stage: Stage },
    Track { track: Uuid, part: TrackPart },
}

impl GraphRoot {
    pub fn root(&self) -> Root {
        match *self {
            Self::Clip { clip } => Root::Clip { clip },
            Self::Timeline { timeline_grade } => Root::TimelineGrade { sequence: timeline_grade },
            Self::Group { group_stage, stage } => Root::GroupStage { group: group_stage, stage },
            Self::Track { track, part } => Root::Track { track, part },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum GraphId {
    Existing(Uuid),
    New { new: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphPath {
    pub root: GraphRoot,
    pub nodes: Vec<GraphId>,
}

impl From<NodePath> for GraphPath {
    fn from(path: NodePath) -> Self {
        let root = match path.root {
            Root::Clip { clip } => GraphRoot::Clip { clip },
            Root::TimelineGrade { sequence } => GraphRoot::Timeline { timeline_grade: sequence },
            Root::GroupStage { group, stage } => GraphRoot::Group { group_stage: group, stage },
            Root::Track { track, part } => GraphRoot::Track { track, part },
        };
        Self { root, nodes: path.nodes.into_iter().map(GraphId::Existing).collect() }
    }
}

impl GraphPath {
    /// A new id is valid only where that add actually created the node.
    pub fn resolve(&self, created: &BTreeMap<usize, NodePath>) -> Result<NodePath, String> {
        if self.nodes.len() > 32 { return Err("a graph path is at most 32 nodes deep".into()); }
        let root = self.root.root();
        let mut nodes = Vec::with_capacity(self.nodes.len());
        for id in &self.nodes {
            let id = match id {
                GraphId::Existing(id) => *id,
                GraphId::New { new } => {
                    let path = created.get(new).ok_or_else(|| format!("new {new} must name an earlier add in this batch"))?;
                    let (id, parent) = path.nodes.split_last().ok_or("an add returned an empty path")?;
                    if path.root != root || parent != nodes {
                        return Err(format!("new {new} was created under a different parent or root"));
                    }
                    *id
                }
            };
            nodes.push(id);
        }
        Ok(NodePath { root, nodes })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum GraphWireOp {
    /// `entry`, when given, is the node's whole stored entry (a saved
    /// recipe's node as it was, #2757): checked to read as that node's.
    Add { after: GraphPath, r#type: String, #[serde(default)] params: BTreeMap<String, Value>, label: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] entry: Option<Value> },
    AddBeside { at: GraphPath, r#type: String, #[serde(default)] params: BTreeMap<String, Value>, label: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] entry: Option<Value> },
    Remove { path: GraphPath },
    Connect { from: GraphPath, from_port: String, to: GraphPath, to_port: String },
    Disconnect { to: GraphPath, port: String },
    Publish { path: GraphPath, from: GraphPath, from_port: String, key: String, label: Option<String> },
    Unpublish { path: GraphPath, key: String },
    Set { path: GraphPath, key: String, value: Value },
    Enable { path: GraphPath, enabled: bool },
    Mix { path: GraphPath, #[serde(deserialize_with = "nullable")] mix: Option<f32> },
    MaskChannel { path: GraphPath, #[serde(deserialize_with = "nullable")] channel: Option<String> },
    MoveAfter { path: GraphPath, after: GraphId },
    /// The behaviours on a parameter (#2758): a wiggle, an oscillation, a
    /// ramp…, as a clip's behaviours store them; none takes them off.
    Modify { path: GraphPath, key: String, modifiers: Vec<Value> },
    /// The keys on a parameter (#2886): `{time_secs, value, interpolation?}`
    /// each, seconds into the clip; none takes them off. Its
    /// behaviours stay.
    Keys { path: GraphPath, key: String, keys: Vec<Value> },
}

impl GraphWireOp {
    pub fn path(&self) -> &GraphPath {
        match self {
            Self::Add { after, .. } => after,
            Self::AddBeside { at, .. } => at,
            Self::Publish { path, .. } | Self::Unpublish { path, .. } | Self::Remove { path } | Self::Set { path, .. } | Self::Enable { path, .. }
            | Self::Mix { path, .. } | Self::MaskChannel { path, .. } | Self::MoveAfter { path, .. } | Self::Modify { path, .. } | Self::Keys { path, .. } => path,
            Self::Connect { to, .. } | Self::Disconnect { to, .. } => to,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_references_cannot_escape_their_creation_path() {
        let root = Root::Clip { clip: Uuid::from_u128(1) };
        let created = BTreeMap::from([(0, NodePath::new(root, [Uuid::from_u128(2), Uuid::from_u128(3)]))]);
        let mut path = GraphPath { root: GraphRoot::Clip { clip: Uuid::from_u128(1) }, nodes: vec![GraphId::Existing(Uuid::from_u128(2)), GraphId::New { new: 0 }] };
        assert_eq!(path.resolve(&created).unwrap(), created[&0]);
        path.nodes.remove(0);
        assert!(path.resolve(&created).is_err());
        path.nodes = vec![GraphId::New { new: 1 }];
        assert!(path.resolve(&created).is_err());
    }

    #[test]
    fn incomplete_publications_and_ambiguous_roots_are_refused() {
        assert!(serde_json::from_value::<GraphWireOp>(json!({"op":"publish"})).is_err());
        assert!(serde_json::from_value::<GraphWireOp>(json!({"op":"unpublish"})).is_err());
        assert!(serde_json::from_value::<GraphRoot>(json!({"clip":Uuid::nil(),"track":Uuid::nil(),"part":"grade"})).is_err());
    }

    #[test]
    fn nullable_settings_require_an_explicit_value() {
        let path = GraphPath::from(NodePath::new(Root::Clip { clip: Uuid::nil() }, [Uuid::nil()]));
        for (op, field) in [("mix", "mix"), ("mask_channel", "channel")] {
            let mut value = json!({"op":op,"path":path});
            assert!(serde_json::from_value::<GraphWireOp>(value.clone()).is_err());
            value[field] = Value::Null;
            assert!(serde_json::from_value::<GraphWireOp>(value).is_ok());
        }
    }
}
