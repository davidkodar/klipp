//! A grade in a picture's chain after its display step (#1746): a
//! `klipp.grade_compound` node holding a grade body, as the clip's own
//! Grade group holds one. Only the picture keeps it. The renderer runs it
//! as the executor's compound node, its body lowered the way a stored
//! grade is (`klipp_timeline::lowered_grade`).

use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::{GroupBody, Node, NodeId};

type Refusal = String;

/// Whether `n` is a grade compound.
pub fn is_compound(n: &Node) -> bool {
    n.type_id.as_str() == lt::GRADE_COMPOUND
}

/// A new grade compound `id` with one Corrector, as the Picture view adds
/// one: a grade graph lifted as a stored grade is.
pub fn new_compound(id: NodeId) -> Result<Node, Refusal> {
    let mut grade = klipp_nodes::NodeGraph::default();
    grade.ensure_corrector();
    compound_of(&grade, id)
}

/// A grade compound `id` holding `grade`, lifted as a stored grade is.
pub fn compound_of(grade: &klipp_nodes::NodeGraph, id: NodeId) -> Result<Node, Refusal> {
    let look = serde_json::json!({ "id": id, "node_graph": grade });
    let structural = crate::to_v2::look_to_v2(look.as_object().expect("an object")).map_err(|e| format!("{e:?}"))?;
    let body = crate::lift::lift_grade_only(&structural, false)?;
    let mut node: Node = serde_json::from_value(serde_json::json!({ "id": id, "type_id": lt::GRADE_COMPOUND, "label": "Grade" })).map_err(|e| e.to_string())?;
    node.group = Some(Box::new(GroupBody { graph: body, ports: Vec::new(), interface: Vec::new() }));
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new compound holds a grade body with one Corrector, between the
    /// body's input and output ends.
    #[test]
    fn a_new_compound_holds_one_corrector() {
        let n = new_compound(NodeId::new_v4()).unwrap();
        assert!(is_compound(&n));
        let body = &n.group.as_ref().unwrap().graph;
        let kinds: Vec<&str> = body.nodes.iter().map(crate::structure::kind_of).collect();
        assert_eq!(kinds.iter().filter(|k| **k == "Corrector").count(), 1, "{kinds:?}");
        assert!(body.nodes.iter().any(|n| n.type_id.as_str() == lt::GROUP_INPUT) && body.nodes.iter().any(|n| n.type_id.as_str() == lt::GROUP_OUTPUT));
    }
}
