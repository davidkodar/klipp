//! Shared effective dependency rules for projection and edit planning.
use klipp_graph_format::{Edge, Graph};

/// Effective references: a drawn edge wins over a wireless input, and the
/// first matching sender supplies an unwired port, as lifted validation does.
pub(crate) fn effective_edges(graph: &Graph) -> Vec<Edge> {
    let mut edges = graph.edges.clone();
    for node in &graph.nodes {
        for (port, name) in &node.wireless.input_links {
            if edges.iter().any(|e| e.to == (node.id, port.clone())) {
                continue;
            }
            if let Some(sender) = graph
                .nodes
                .iter()
                .find(|s| s.id != node.id && s.wireless.output_link.as_ref() == Some(name))
            {
                edges.push(Edge {
                    from: (sender.id, "out".into()),
                    to: (node.id, port.clone()),
                });
            }
        }
    }
    edges
}

/// Lifted grade nodes retain their authored label in the legacy payload,
/// a compound's group (#1391) too.
/// What to call `node` to a person: its label, else its effect's or grade
/// node's name, else its type's last word. Never its id or a type id
/// ("klipp.legacy.effect"), which mean nothing to them.
pub fn readable_name(node: &klipp_graph_format::Node) -> String {
    let label = node_label(node);
    if !label.is_empty() {
        return label.to_owned();
    }
    if let Some(ty) = crate::effect_params::tag(node).and_then(|tag| klipp_effects::registry::effect_types().find(|t| t.tag == tag)) {
        return ty.desc.name.to_owned();
    }
    let kind = node.legacy.as_ref().and_then(|l| l.payload.get("kind")).and_then(|k| serde_json::from_value::<klipp_nodes::NodeKind>(k.clone()).ok());
    if let Some(kind) = kind {
        return kind.display_name().to_owned();
    }
    node.type_id.as_str().rsplit('.').next().unwrap_or_default().replace('_', " ")
}

pub(crate) fn node_label(node: &klipp_graph_format::Node) -> &str {
    node.legacy.as_ref()
        .filter(|_| matches!(node.type_id.as_str(), klipp_graph_format::types::LEGACY_NODE | klipp_graph_format::lifted::types::MERGE | klipp_graph_format::lifted::types::GROUP))
        .and_then(|legacy| legacy.payload.get("label"))
        .and_then(serde_json::Value::as_str)
        .filter(|label| !label.is_empty())
        .unwrap_or(&node.label)
}
