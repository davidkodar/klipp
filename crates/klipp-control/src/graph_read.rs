//! Bounded reads of the same lifted documents graph operations address.

use klipp_graph_format::{Graph, Node, path::{NodePath, Root, TrackPart}};
use klipp_graph_lift::paths::{Documents, resolve};
use serde_json::{Value, json};
use crate::graph_model;

const BUDGET: usize = 6_000;

pub(crate) fn read(docs: &Documents, path: &NodePath, depth: usize) -> Result<Value, String> {
    if depth > 4 { return Err("graph reads allow depth 0 to 4".into()); }
    let graph = if path.nodes.is_empty() {
        match path.root {
            Root::Clip { clip } => &docs.clips.get(&clip).ok_or("no such clip")?.graph,
            Root::TimelineGrade { sequence } => docs.timeline.get(&sequence).ok_or("no such timeline grade")?,
            Root::GroupStage { group, stage } => docs.stages.get(&(group, stage)).ok_or("no such group stage")?,
            Root::Track { track, part: TrackPart::Grade } => docs.track_grades.get(&track).ok_or("no such track grade")?,
            Root::Track { track, part: TrackPart::Effects } => {
                let nodes = docs.track_effects.get(&track).ok_or("no such track effects")?;
                return Ok(bounded(json!({"nodes":nodes.iter().map(|n| node(n, depth)).collect::<Vec<_>>(),"edges":[],"ordered":true})));
            }
        }
    } else {
        let n = resolve(docs, path).map_err(|e| e.to_string())?;
        match &n.group {
            Some(group) => &group.graph,
            None => return Ok(bounded(json!({"nodes":[node(n, depth)],"edges":[]}))),
        }
    };
    Ok(bounded(graph_value(graph, depth)))
}

fn graph_value(graph: &Graph, depth: usize) -> Value {
    let edges: Vec<_> = graph.edges.iter().map(|e| json!({"from":e.from.0,"from_port":e.from.1,"to":e.to.0,"to_port":e.to.1})).collect();
    json!({"nodes":graph.nodes.iter().map(|n| node(n,depth)).collect::<Vec<_>>(),"edges":edges,"output":graph.output})
}

fn node(n: &Node, depth: usize) -> Value {
    let desc = graph_model::descriptor(n);
    let mut params = serde_json::Map::new();
    if let (Some(desc), Some(legacy)) = (desc, &n.legacy) {
        let payload = legacy.payload.get("params").unwrap_or(&legacy.payload);
        for p in desc.params.iter().filter(|p| !p.derived) {
            let mut value = Some(payload);
            for field in p.field.split('.') { value = value.and_then(|v| v.get(field)); }
            if let Some(index) = p.index { value = value.and_then(|v| v.get(index)); }
            if let Some(value) = value.filter(|v| !v.is_null()) {
                if value.as_f64().is_some_and(|v| v as f32 == p.default)
                    || value.as_bool().is_some_and(|v| v == (p.default != 0.0)) { continue; }
                params.insert(p.key.into(), value.clone());
            }
        }
    }
    for (key, value) in &n.params {
        if key.as_str() != "mask.channel" { params.insert(key.as_str().into(), json!(value)); }
    }
    let channel = n.params.iter().find(|(k,_)| k.as_str() == "mask.channel").map(|(_,v)| match v {
        klipp_graph_format::ParamValue::Text(s) => json!(s),
        other => json!(other),
    }).unwrap_or(json!("red (unset)"));
    let mut out = json!({"id":n.id,"type":desc.map(|d| d.id).unwrap_or(n.type_id.as_str()),"label":n.label,"enabled":n.enabled,"params":params,"mix":n.mix,"mask_channel":channel});
    if let Some(group) = &n.group {
        out["ports"] = json!(group.interface);
        if depth > 0 { out["body"] = graph_value(&group.graph, depth - 1); }
    }
    out
}

// Omitted material is explicit. A caller can request a narrower node path to
// inspect large parameter values or bodies without mistaking a partial read.
fn bounded(mut value: Value) -> Value {
    let mut omitted = 0;
    while value.to_string().len() > BUDGET - 128 {
        if !trim(&mut value) { break; }
        omitted += 1;
    }
    if omitted > 0 {
        value["omitted_items"] = json!(omitted);
        value["note"] = json!("Partial read; request a narrower node path.");
    }
    value
}

fn trim(value: &mut Value) -> bool {
    if let Some(nodes) = value.get_mut("nodes").and_then(Value::as_array_mut) {
        for n in nodes.iter_mut().rev() {
            if let Some(body) = n.get_mut("body") {
                if trim(body) { return true; }
            }
            for key in ["params", "ports"] {
                if n.get(key).is_some_and(|v| v.to_string().len() > 256) {
                    n[key] = Value::Null;
                    n["omitted_details"] = json!(true);
                    return true;
                }
            }
            if let Some(label) = n.get("label").and_then(Value::as_str).filter(|s| s.len() > 160) {
                n["label"] = json!(label.chars().take(40).collect::<String>());
                n["omitted_details"] = json!(true);
                return true;
            }
        }
    }
    for key in ["edges", "nodes"] {
        if value.get_mut(key).and_then(Value::as_array_mut).is_some_and(|a| a.pop().is_some()) { return true; }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_graph_is_explicitly_partial_and_bounded() {
        let graph = json!({"nodes":(0..60).map(|i| json!({"id":i,"label":"λ".repeat(400),"params":{"large":["x".repeat(10000)]}})).collect::<Vec<_>>(),"edges":[]});
        let read = bounded(graph);
        assert!(read.to_string().len() <= BUDGET);
        assert!(read["omitted_items"].as_u64().unwrap() > 0);
    }
}
