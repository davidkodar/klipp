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

/// Lifted grade nodes retain their authored label in the legacy payload.
pub(crate) fn node_label(node: &klipp_graph_format::Node) -> &str {
    node.legacy.as_ref()
        .filter(|_| matches!(node.type_id.as_str(), klipp_graph_format::types::LEGACY_NODE | klipp_graph_format::lifted::types::MERGE))
        .and_then(|legacy| legacy.payload.get("label"))
        .and_then(serde_json::Value::as_str)
        .filter(|label| !label.is_empty())
        .unwrap_or(&node.label)
}
