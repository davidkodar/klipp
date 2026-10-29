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
            Some(group) => {
                let mut value = graph_value(&group.graph, depth);
                value["ports"] = json!(group.interface);
                return Ok(bounded(value));
            },
            None => return Ok(bounded(json!({"nodes":[node(n, depth)],"edges":[]}))),
        }
    };
    Ok(bounded(graph_value(graph, depth)))
}

fn graph_value(graph: &Graph, depth: usize) -> Value {
    let edges: Vec<_> = graph.edges.iter().map(|e| json!({"from":e.from.0,"from_port":e.from.1,"to":e.to.0,"to_port":e.to.1})).collect();
    json!({"nodes":graph.nodes.iter().map(|n| node(n,depth)).collect::<Vec<_>>(),"edges":edges,"output":graph.output})
}

/// A node as `graph_read` gives it: its type id, label and parameters by
/// key.
pub(crate) fn node(n: &Node, depth: usize) -> Value {
    let desc = graph_model::descriptor(n);
    let mut params = serde_json::Map::new();
    // An effect's values as its stored entry, written from its map (M3-2).
    let effect = n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT;
    let entry = effect.then(|| klipp_graph_lift::effect_params::entry_of(n).ok()).flatten();
    if let (Some(desc), Some(legacy)) = (desc, &n.legacy) {
        let payload = match &entry {
            Some(entry) => entry,
            None => legacy.payload.get("params").unwrap_or(&legacy.payload),
        };
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
        // An effect's field keys are read above, from its entry.
        if effect && klipp_graph_lift::effect_params::is_field_key(key) { continue; }
        if key.as_str() != "mask.channel" { params.insert(key.as_str().into(), json!(value)); }
    }
    let stored_channel = n.params.iter().find(|(k,_)| k.as_str() == "mask.channel");
    let channel = stored_channel.map(|(_,v)| match v {
        klipp_graph_format::ParamValue::Text(s) => json!(s),
        other => json!(other),
    }).unwrap_or(json!("red"));
    // Grade labels live in the saved legacy entry, including lifted merges.
    let grade_label = matches!(n.type_id.as_str(), klipp_graph_format::types::LEGACY_NODE | klipp_graph_format::lifted::types::MERGE);
    let label = n.legacy.as_ref().filter(|_| grade_label)
        .and_then(|legacy| legacy.payload.get("label")).and_then(Value::as_str).unwrap_or(&n.label);
    let mut out = json!({"id":n.id,"type":desc.map(|d| d.id).unwrap_or(n.type_id.as_str()),"label":label,"enabled":n.enabled,"params":params,"mix":n.mix,"mask_channel":channel,"mask_channel_set":stored_channel.is_some()});
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
        let removed = trim(&mut value);
        if removed == 0 { break; }
        omitted += removed;
    }
    if omitted > 0 {
        value["omitted_items"] = json!(omitted);
        value["note"] = json!("Partial read; request a narrower node path.");
    }
    value
}

fn trim(value: &mut Value) -> usize {
    if let Some(nodes) = value.get_mut("nodes").and_then(Value::as_array_mut) {
        for n in nodes.iter_mut().rev() {
            if let Some(body) = n.get_mut("body") {
                let removed = trim(body);
                if removed > 0 { return removed; }
            }
            for key in ["params", "ports"] {
                if n.get(key).is_some_and(|v| v.to_string().len() > 256) {
                    n[key] = Value::Null;
                    n["omitted_details"] = json!(true);
                    return 1;
                }
            }
            if let Some(label) = n.get("label").and_then(Value::as_str).filter(|s| s.len() > 160) {
                n["label"] = json!(label.chars().take(40).collect::<String>());
                n["omitted_details"] = json!(true);
                return 1;
            }
        }
    }
    if let Some(removed) = value.get_mut("nodes").and_then(Value::as_array_mut).and_then(Vec::pop) {
        let mut count = 1;
        if let Some(id) = removed.get("id") {
            if let Some(edges) = value.get_mut("edges").and_then(Value::as_array_mut) {
                let before = edges.len();
                edges.retain(|edge| edge.get("from") != Some(id) && edge.get("to") != Some(id));
                count += before - edges.len();
            }
            if value.get("output") == Some(id) {
                value.as_object_mut().unwrap().remove("output");
                value["output_omitted"] = json!(true);
            }
        }
        return count;
    }
    // A direct group read is also the way to recover its interface from a
    // partial parent read. Preserve those ports while trimming child details.
    if value.get("ports").is_some_and(|v| v.to_string().len() > 256) {
        value["ports"] = Value::Null;
        value["omitted_details"] = json!(true);
        return 1;
    }
    0
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

    #[test]
    fn a_group_read_preserves_its_interface_before_child_details() {
        let ports = json!((0..8).map(|i| json!({"key":format!("matte_{i}"),"direction":"output","ty":"matte", "binding":{"node":i,"port":"out"}})).collect::<Vec<_>>());
        assert!(ports.to_string().len() > 256);
        let read = bounded(json!({"ports":ports,"nodes":(0..60).map(|i| json!({"id":i,"label":"x".repeat(100),"params":{}})).collect::<Vec<_>>(),"edges":[]}));
        assert!(read.to_string().len() <= BUDGET);
        assert_eq!(read["ports"], ports);
        assert!(read["omitted_items"].as_u64().unwrap() > 0);
        let oversized = bounded(json!({"ports":[{"label":"x".repeat(BUDGET * 2)}],"nodes":[],"edges":[]}));
        assert!(oversized.to_string().len() <= BUDGET);
        assert_eq!(oversized["omitted_details"], true);
        assert!(oversized["ports"].is_null());
    }

    #[test]
    fn partial_reads_keep_the_wiring_between_retained_nodes() {
        let graph = json!({
            "nodes":(0..60).map(|i| json!({"id":i,"label":"x".repeat(100),"params":{}})).collect::<Vec<_>>(),
            "edges":(1..60).map(|i| json!({"from":i-1,"from_port":"out","to":i,"to_port":"in"})).collect::<Vec<_>>(),
            "output":59
        });
        let read = bounded(graph);
        let nodes = read["nodes"].as_array().unwrap();
        let edges = read["edges"].as_array().unwrap();
        assert!(read.to_string().len() <= BUDGET);
        assert!(nodes.len() > 1 && nodes.len() < 60);
        assert_eq!(edges.len(), nodes.len() - 1);
        for edge in edges {
            assert!(nodes.iter().any(|n| n["id"] == edge["from"]));
            assert!(nodes.iter().any(|n| n["id"] == edge["to"]));
        }
        assert!(read.get("output").is_none());
        assert_eq!(read["output_omitted"], true);
    }

    #[test]
    fn read_channel_distinguishes_migrated_red_from_explicit_red() {
        let mut n = graph_model::new_node("klipp.fx.gaussian_blur", uuid::Uuid::new_v4(), None).unwrap();
        let absent = node(&n, 0);
        assert_eq!(absent["mask_channel"], "red");
        assert_eq!(absent["mask_channel_set"], false);
        n.params.insert("mask.channel".into(), klipp_graph_format::ParamValue::Text("red".into()));
        let explicit = node(&n, 0);
        assert_eq!(explicit["mask_channel"], "red");
        assert_eq!(explicit["mask_channel_set"], true);
        // Both returned states can be expressed in the write vocabulary.
        for read in [absent, explicit] {
            let channel = if read["mask_channel_set"] == true { read["mask_channel"].clone() } else { Value::Null };
            let op = json!({"op":"mask_channel","path":{"root":{"clip":uuid::Uuid::nil()},"nodes":[n.id]},"channel":channel});
            assert!(serde_json::from_value::<crate::graph_wire::GraphWireOp>(op).is_ok());
        }
    }
}
