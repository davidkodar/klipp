//! The check that a graph op written through a clip's fields gives the
//! picture the op engine makes of the lifted copy (node-graph M2 step
//! 15a, amendment 11). Shared by the synthetic tests and the corpus run.

use klipp_graph_format::lifted::ClipPicture;
use klipp_graph_format::{types, NodeId};
use klipp_graph_lift::ops::{apply_with_catalog, GraphOp};
use klipp_graph_lift::paths::Documents;
use klipp_timeline::{Clip, RegistryCatalog};
use serde_json::Value;

/// What one op came to on both paths. (A test that only panics with an
/// outcome reads its reasons through `Debug`.)
#[derive(Debug)]
#[allow(dead_code)]
pub enum Outcome {
    Same,
    /// The engine refuses it; the clip is then not asked.
    EngineRefused(String),
    /// The engine takes it and the clip's fields refuse it, with a reason.
    ClipRefused(String),
    /// Both take it and the pictures differ.
    Differ { written: Box<Value>, engine: Box<Value> },
}

pub fn picture(c: &Clip) -> ClipPicture {
    c.picture_v2().expect("the clip lifts").picture.clone()
}

/// The effect nodes in chain order.
pub fn effects(p: &ClipPicture) -> Vec<NodeId> {
    p.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT).map(|n| n.id).collect()
}

/// Edges as a set (the engine appends an edge it makes, a lift writes
/// them in chain order), and inside a group the node list too (its order
/// is the editor's; the edges are the picture).
fn sort_edges(graph: &mut Value) {
    if let Some(edges) = graph.get_mut("edges").and_then(Value::as_array_mut) {
        edges.sort_by_key(|e| e.to_string());
    }
    if let Some(nodes) = graph.get_mut("nodes").and_then(Value::as_array_mut) {
        for n in nodes {
            if let Some(body) = n.get_mut("group").and_then(|g| g.get_mut("graph")) {
                sort_edges(body);
                if let Some(inner) = body.get_mut("nodes").and_then(Value::as_array_mut) {
                    inner.sort_by_key(|n| n["id"].to_string());
                }
            }
        }
    }
}

/// A lifted picture with what only the slot decides taken out: an effect
/// node's id (derived from its slot) renamed by its place among the
/// effects, and the stored slot and slot-keyed keys it carries beside the
/// animation compared on the node.
pub fn normalised(p: &ClipPicture) -> Value {
    let mut text = serde_json::to_string(p).unwrap();
    for (i, id) in effects(p).iter().enumerate() {
        text = text.replace(&id.to_string(), &format!("effect-{i}"));
    }
    let mut v: Value = serde_json::from_str(&text).unwrap();
    sort_edges(&mut v["graph"]);
    for n in v["graph"]["nodes"].as_array_mut().unwrap() {
        if n["type_id"] == types::LEGACY_EFFECT {
            if let Some(l) = n.get_mut("legacy").and_then(Value::as_object_mut) {
                l.remove("slot");
                l.remove("keyframes");
                l.remove("modifiers");
            }
        }
    }
    v
}

/// `op` on `c` both ways.
pub fn check(c: &Clip, op: &GraphOp) -> Outcome {
    let mut docs = Documents::default();
    docs.clips.insert(c.id.0, picture(c));
    if let Err(e) = apply_with_catalog(&mut docs, op, &RegistryCatalog) {
        return Outcome::EngineRefused(e);
    }
    let engine = docs.clips.remove(&c.id.0).unwrap();
    let mut written = c.clone();
    if let Err(e) = written.apply_picture_ops(std::slice::from_ref(op)) {
        return Outcome::ClipRefused(e);
    }
    let (w, e) = (normalised(&picture(&written)), normalised(&engine));
    if w == e {
        Outcome::Same
    } else {
        Outcome::Differ { written: Box::new(w), engine: Box::new(e) }
    }
}
