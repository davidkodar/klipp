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
    if let Some(name) = structural_name(node.type_id.as_str()) {
        return name.to_owned();
    }
    // Through the one reader of a grade node's kind (`structure::kind_of`).
    // A node with no kind reads as `Unknown` (the enum's catch-all), which
    // named a lifted display step "Unknown" in a refusal: not taken.
    let kind = serde_json::from_value::<klipp_nodes::NodeKind>(serde_json::Value::String(crate::structure::kind_of(node).to_owned()))
        .ok()
        .filter(|k| *k != klipp_nodes::NodeKind::Unknown);
    if let Some(kind) = kind {
        return kind.display_name().to_owned();
    }
    node.type_id.as_str().rsplit('.').next().unwrap_or_default().replace('_', " ")
}

/// A lifted graph's structural node by the name the node graph shows it
/// under (klipp-ui's `node_graph_v2::picture`); `None` for any other type.
fn structural_name(type_id: &str) -> Option<&'static str> {
    use klipp_graph_format::lifted::types as lt;
    Some(match type_id {
        lt::SOURCE => "Source",
        lt::OUTPUT | lt::GROUP_OUTPUT => "Output",
        lt::GROUP => "Grade",
        lt::GROUP_INPUT => "Input",
        lt::GROUP_FRAME => "Frame",
        lt::PLACEMENT => "Placement",
        lt::SCENE_TO_DISPLAY => "Scene to Display",
        lt::TEMPORAL_DENOISE => "Temporal Denoise",
        lt::FRAME_BLEND => "Frame Blend",
        _ => return None,
    })
}

pub(crate) fn node_label(node: &klipp_graph_format::Node) -> &str {
    node.legacy.as_ref()
        .filter(|_| matches!(node.type_id.as_str(), klipp_graph_format::types::LEGACY_NODE | klipp_graph_format::lifted::types::MERGE | klipp_graph_format::lifted::types::GROUP))
        .and_then(|legacy| legacy.payload.get("label"))
        .and_then(serde_json::Value::as_str)
        .filter(|label| !label.is_empty())
        .unwrap_or(&node.label)
}
