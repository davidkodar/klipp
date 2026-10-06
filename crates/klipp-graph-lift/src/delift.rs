//! `delift`: a lifted picture back to the revision-1 record it was lifted
//! from, as today's loader leaves that record (its Color Warper migration
//! and default pipeline made). Its property, with `r` such a record:
//! `delift(lift(r)) == r`, exactly. With `to_v1(to_v2(x)) == canon(x)`,
//! that makes `to_v1(delift(p))` the v1 picture a stored revision-2
//! picture came from (#1089).
//!
//! Nothing here decides what renders (the adapter does): it reads only
//! what the lift keeps for the record, the legacy entries, their slots,
//! `was_enabled`, and what a Grade group body's input holds of the edges
//! the lift took out.

use std::collections::BTreeSet;

use klipp_graph_format::lifted::{types as lt, ClipPicture, LegacyFlags};
use klipp_graph_format::{types, ClipRecordV2, Edge, Graph, GroupBody, LegacyClipAnim, Node, NodeId, NodeTypeId, PortKey};
use serde_json::Value;

/// Why a picture is not one the lift makes.
pub type Refusal = String;

/// A node's record flags taken off, `enabled` put back as stored.
fn restore(mut n: Node, type_id: &str) -> Node {
    n.type_id = NodeTypeId::new(type_id);
    if let Some(f) = n.legacy_flags.take() {
        if let Some(on) = f.was_enabled {
            n.enabled = on;
        }
    }
    n.anim.clear();
    n.params.clear();
    n
}

fn edge_of(v: &Value) -> Result<Edge, Refusal> {
    serde_json::from_value(v.clone()).map_err(|e| format!("a recorded edge: {e}"))
}

fn port_of(v: &Value) -> Result<(NodeId, PortKey), Refusal> {
    serde_json::from_value(v.clone()).map_err(|e| format!("a recorded port: {e}"))
}

fn at_of(v: &Value) -> Result<(usize, Edge), Refusal> {
    let at = v.get("at").and_then(Value::as_u64).ok_or("a dropped edge without its place")? as usize;
    Ok((at, edge_of(v.get("edge").ok_or("a dropped edge without the edge")?)?))
}

/// A lifted Grade group body back to the structural body.
pub fn body(lifted: &Graph) -> Result<Graph, Refusal> {
    let mut g = lifted.clone();
    let frame = g.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP_FRAME).map(|n| n.id);
    let record: LegacyFlags = g
        .nodes
        .iter()
        .find(|n| n.type_id.as_str() == lt::GROUP_INPUT)
        .and_then(|n| n.legacy_flags.clone())
        .unwrap_or_default();

    // The edges the lift made go: every edge from the frame but a rewired
    // one, and the ports it filled from another input.
    let rewired: Vec<(Edge, (NodeId, PortKey))> = record
        .rewired
        .iter()
        .map(|r| Ok((Edge { from: port_of(r.get("from").ok_or("a rewired edge without its source")?)?, to: port_of(r.get("to").ok_or("a rewired edge without its port")?)? }, port_of(&r["to"])?)))
        .collect::<Result<_, Refusal>>()?;
    let filled: BTreeSet<(NodeId, PortKey)> = record.filled.iter().map(port_of).collect::<Result<_, _>>()?;
    let is_rewired = |e: &Edge| rewired.iter().any(|(_, to)| *to == e.to);
    g.edges.retain(|e| !(Some(e.from.0) == frame && !is_rewired(e)) && !filled.contains(&e.to));
    // A rewired edge reads its saved source again.
    for e in &mut g.edges {
        if let Some((saved, _)) = rewired.iter().find(|(_, to)| *to == e.to) {
            e.from = saved.from.clone();
        }
    }
    // The dropped edges, then the parked ones, each back where it stood.
    let mut dropped: Vec<(usize, Edge)> = record.dropped_edges.iter().map(at_of).collect::<Result<_, _>>()?;
    dropped.sort_by_key(|(at, _)| *at);
    for (at, e) in dropped {
        if at > g.edges.len() {
            return Err(format!("a dropped edge at {at}, past the {} kept", g.edges.len()));
        }
        g.edges.insert(at, e);
    }
    if record.parked_at.len() != record.parked_edges.len() {
        return Err("parked edges without their places".into());
    }
    let mut parked: Vec<(usize, Edge)> = record.parked_at.iter().zip(&record.parked_edges).map(|(at, e)| Ok((*at as usize, edge_of(e)?))).collect::<Result<_, Refusal>>()?;
    parked.sort_by_key(|(at, _)| *at);
    for (at, e) in parked {
        if at > g.edges.len() {
            return Err(format!("a parked edge at {at}, past the {} kept", g.edges.len()));
        }
        g.edges.insert(at, e);
    }
    for w in record.dropped_wireless.iter().chain(&record.parked_links) {
        let (node, port, name) = (w.get("node"), w.get("port"), w.get("name").and_then(Value::as_str));
        let (Some(node), Some(port), Some(name)) = (node, port, name) else { return Err("a dropped wireless link, incomplete".into()) };
        let node: NodeId = serde_json::from_value(node.clone()).map_err(|e| e.to_string())?;
        let port: PortKey = serde_json::from_value(port.clone()).map_err(|e| e.to_string())?;
        let n = g.nodes.iter_mut().find(|n| n.id == node).ok_or("a dropped wireless link on a node that isn't there")?;
        n.wireless.input_links.insert(port, name.to_owned());
    }

    // The nodes the lift made go; the rest are structural nodes again.
    let mut nodes = Vec::with_capacity(g.nodes.len());
    for n in std::mem::take(&mut g.nodes) {
        match n.type_id.as_str() {
            lt::GROUP_FRAME => {}
            lt::GROUP_INPUT | lt::GROUP_OUTPUT if n.legacy.is_none() => {}
            lt::GROUP_INPUT | lt::GROUP_OUTPUT | lt::MERGE | types::LEGACY_NODE => nodes.push(restore(n, types::LEGACY_NODE)),
            other => return Err(format!("grade node {} is a {other}", n.id)),
        }
    }
    g.nodes = nodes;
    Ok(g)
}

/// A lifted, unspliced clip picture back to its revision-1 record.
pub fn clip(p: &ClipPicture) -> Result<ClipRecordV2, Refusal> {
    let mut grade = None;
    let mut ofx = Vec::new();
    let mut builtins: Vec<(u32, Node)> = Vec::new();
    let mut legacy_clip = LegacyClipAnim::default();
    for n in &p.graph.nodes {
        if n.origin.is_some() {
            return Err("a spliced render copy, not a stored picture".into());
        }
        let slot = |n: &Node| n.legacy.as_ref().and_then(|l| l.slot).ok_or_else(|| format!("entry {} without its slot", n.id));
        match n.type_id.as_str() {
            lt::SOURCE => {
                if let Some(a) = n.legacy_flags.as_ref().and_then(|f| f.clip_anim.clone()) {
                    legacy_clip = a;
                }
            }
            lt::SCENE_TO_DISPLAY | lt::OUTPUT => {}
            lt::PLACEMENT if n.legacy.is_none() => {}
            lt::PLACEMENT => builtins.push((slot(n)?, restore(n.clone(), types::LEGACY_TRANSFORM))),
            lt::TEMPORAL_DENOISE => builtins.push((slot(n)?, restore(n.clone(), types::LEGACY_TEMPORAL_DENOISE))),
            lt::FRAME_BLEND => builtins.push((slot(n)?, restore(n.clone(), types::LEGACY_SPEED_RAMP))),
            lt::GROUP => {
                let lifted = &n.group.as_ref().ok_or("a grade group without a body")?.graph;
                let mut g = restore(n.clone(), types::LEGACY_GRADE);
                // The structural grade group is always on: a looping grade
                // the lift kept off was on as stored.
                g.enabled = true;
                g.group = Some(Box::new(GroupBody { graph: body(lifted)?, ports: Vec::new() }));
                grade = Some(g);
            }
            types::LEGACY_OFX => ofx.push(n.clone()),
            t @ (types::LEGACY_EFFECT | types::LEGACY_OPACITY | types::LEGACY_TRANSFORM | types::LEGACY_SPEED_RAMP) => {
                let t = t.to_owned();
                builtins.push((slot(n)?, restore(n.clone(), &t)));
            }
            other => return Err(format!("node {} is a {other}, which the lift does not put in a clip's chain", n.id)),
        }
    }
    builtins.sort_by_key(|(s, _)| *s);
    let mut nodes: Vec<Node> = grade.into_iter().collect();
    nodes.extend(ofx);
    nodes.extend(builtins.into_iter().map(|(_, n)| n));
    let graph = match nodes.last().map(|n| n.id) {
        None => Graph::empty(),
        Some(last) => {
            let edges = nodes.windows(2).map(|w| Edge { from: (w[0].id, "out".into()), to: (w[1].id, "in".into()) }).collect();
            Graph { nodes, edges, output: last, ..Graph::empty() }
        }
    };
    Ok(ClipRecordV2 { graph, legacy_clip })
}

/// A stored Grade group body standing alone back to its grade-only
/// revision-1 graph: one `klipp.legacy.grade` node around the body.
pub fn grade_only(lifted: &Graph, group: NodeId) -> Result<Graph, Refusal> {
    let mut g = Node {
        id: group,
        type_id: NodeTypeId::new(types::LEGACY_GRADE),
        type_version: 1,
        label: String::new(),
        enabled: true,
        params: Default::default(),
        anim: Default::default(),
        mix: None,
        wireless: Default::default(),
        group: Some(Box::new(GroupBody { graph: body(lifted)?, ports: Vec::new() })),
        legacy: Some(klipp_graph_format::Legacy { slot: None, payload: Value::Null, keyframes: None, modifiers: Default::default() }),
        legacy_flags: None,
        origin: None,
    };
    g.enabled = true;
    Ok(Graph { output: g.id, nodes: vec![g], ..Graph::empty() })
}
