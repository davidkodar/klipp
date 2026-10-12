//! Group and Ungroup on a lifted grade body (#1476): a grade compound made
//! from nodes at one level, or dissolved back into it, on the lifted graph
//! itself, so they run as graph ops by path at any depth.
//!
//! The result is the shape the lift gives a compound (`lift_compounds`): a
//! group node holding the compound's entry, its body the nodes moved in,
//! between a group input and a group output made for it, with the body's
//! frame and the edges the lift derives (an unwired primary reading the
//! frame, a filled secondary, a compound's frame fed what feeds its `in`).
//! The limits are the canvas's: two nodes or more, never a graph's ends,
//! one edge in and one edge out at most, no loop.
//!
//! A level saved with edges the lift set aside (parked, dropped or rewired
//! ones, kept on its input for `delift`) is refused: moving them is not
//! defined, and such graphs are only ever odd saved files.

use std::collections::BTreeSet;

use klipp_graph_format::lifted::{types as lt, LegacyFlags};
use klipp_graph_format::{ports, Edge, Graph, GroupBody, Legacy, Node, NodeId, NodeTypeId, PortKey};
use serde_json::{json, Value};
use uuid::Uuid;

/// The ids of the input and the output a group made by [`group`] holds:
/// fixed by the group's own id, so a batch can name them.
pub fn ends(group: NodeId) -> (NodeId, NodeId) {
    (Uuid::new_v5(&group, b"compound.source"), Uuid::new_v5(&group, b"compound.output"))
}

fn payload(n: &Node) -> &Value {
    n.legacy.as_ref().map(|l| &l.payload).unwrap_or(&Value::Null)
}

fn kind_of(n: &Node) -> &str {
    payload(n).get("kind").and_then(Value::as_str).unwrap_or("")
}

fn type_of(n: &Node) -> &str {
    n.type_id.as_str()
}

fn is_end(n: &Node) -> bool {
    matches!(type_of(n), lt::SOURCE | lt::OUTPUT | lt::GROUP_INPUT | lt::GROUP_OUTPUT | lt::GROUP_FRAME)
}

/// A v1 grade node's entry, as the lift keeps it: its kind, its label, its
/// parameters (every one unset), its keyframes (none).
fn entry(kind: &str, label: &str) -> Legacy {
    let mut params = serde_json::to_value(klipp_nodes::NodeParams::default()).unwrap_or_else(|_| json!({}));
    if kind == "Compound" {
        if let Some(p) = params.as_object_mut() {
            p.remove("compound_subgraph");
        }
    }
    Legacy { slot: None, payload: json!({ "kind": kind, "label": label, "params": params }), keyframes: Some(json!({ "tracks": [] })), modifiers: Default::default() }
}

fn node(id: NodeId, type_id: &str, legacy: Option<Legacy>) -> Node {
    Node {
        id,
        type_id: NodeTypeId::new(type_id),
        type_version: 1,
        label: String::new(),
        enabled: true,
        params: Default::default(),
        anim: Default::default(),
        mix: None,
        wireless: Default::default(),
        group: None,
        legacy,
        legacy_flags: None,
        origin: None,
    }
}

fn input_of(g: &Graph) -> Option<NodeId> {
    g.nodes.iter().find(|n| type_of(n) == lt::GROUP_INPUT).map(|n| n.id)
}

fn frame_of(g: &Graph) -> Option<NodeId> {
    g.nodes.iter().find(|n| type_of(n) == lt::GROUP_FRAME).map(|n| n.id)
}

/// Refused when the level's input keeps edges the lift set aside.
fn refuse_set_aside(g: &Graph) -> Result<(), String> {
    let f = input_of(g).and_then(|i| g.nodes.iter().find(|n| n.id == i)).and_then(|n| n.legacy_flags.as_ref());
    if f.is_some_and(|f| !f.parked_edges.is_empty() || !f.parked_links.is_empty() || !f.dropped_edges.is_empty() || !f.rewired.is_empty() || !f.dropped_wireless.is_empty()) {
        return Err("this grade was saved with edges set aside, so it can't be grouped or ungrouped here".into());
    }
    Ok(())
}

/// The ports the lift filled at a level, as `(node, port)`, from its
/// input's record.
fn filled(g: &Graph) -> BTreeSet<(NodeId, PortKey)> {
    let f = input_of(g).and_then(|i| g.nodes.iter().find(|n| n.id == i)).and_then(|n| n.legacy_flags.as_ref());
    f.map(|f| f.filled.iter().filter_map(|v| serde_json::from_value::<(NodeId, PortKey)>(v.clone()).ok()).collect()).unwrap_or_default()
}

/// Whether the lift derived `e` rather than reading it from the saved
/// graph: a read of the frame, a filled port, a compound's frame.
fn derived(e: &Edge, frame: Option<NodeId>, filled: &BTreeSet<(NodeId, PortKey)>) -> bool {
    Some(e.from.0) == frame || e.to.1.as_str() == "frame" || filled.contains(&(e.to.0, e.to.1.clone()))
}

/// The level's edges as saved: the derived ones taken off, and the input's
/// record of filled ports with them.
fn strip_derived(g: &mut Graph) {
    let (frame, filled) = (frame_of(g), filled(g));
    g.edges.retain(|e| !derived(e, frame, &filled));
    if let Some(input) = input_of(g).and_then(|i| g.nodes.iter_mut().find(|n| n.id == i)) {
        if let Some(f) = input.legacy_flags.as_mut() {
            f.filled.clear();
            if f.is_empty() {
                input.legacy_flags = None;
            }
        }
    }
}

/// The edges the lift derives at a level, put back after its saved ones
/// by the lift's own rules (`lift::derive_reads`, `lift::feed_frames`,
/// #1532): its reads and fills, their record on the level's input, and
/// each compound's frame.
fn derive(g: &mut Graph) {
    let (Some(input), Some(frame)) = (input_of(g), frame_of(g)) else { return };
    let record = crate::lift::derive_reads(g, input, frame);
    if !record.is_empty() {
        if let Some(i) = g.nodes.iter_mut().find(|n| n.id == input) {
            i.legacy_flags.get_or_insert_with(LegacyFlags::default).filled = record;
        }
    }
    let groups: Vec<NodeId> = g.nodes.iter().filter(|n| type_of(n) == lt::GROUP).map(|n| n.id).collect();
    crate::lift::feed_frames(g, &groups);
}

fn pos(v: &Value) -> Option<(f32, f32)> {
    Some((v.get("x")?.as_f64()? as f32, v.get("y")?.as_f64()? as f32))
}

fn at(x: f32, y: f32) -> Value {
    serde_json::to_value(klipp_nodes::NodePos { x, y }).unwrap_or(Value::Null)
}

/// Pack `nodes`, all at level `g`, into a new compound `group` in their
/// place: what fed them feeds the compound, and what they fed reads it.
pub fn group(g: &mut Graph, nodes: &[NodeId], group: NodeId) -> Result<(), String> {
    let inside: BTreeSet<NodeId> = nodes.iter().copied().collect();
    if inside.len() < 2 {
        return Err("Group needs two nodes or more".into());
    }
    if g.nodes.iter().any(|n| n.id == group) {
        return Err(format!("{group} is already in the graph"));
    }
    for id in &inside {
        let n = g.nodes.iter().find(|n| n.id == *id).ok_or_else(|| format!("{id} isn't in the graph"))?;
        if is_end(n) {
            return Err("Source / Output can't be packed into a Compound".into());
        }
    }
    refuse_set_aside(g)?;
    let (src, out) = ends(group);
    let mut level = g.clone();
    strip_derived(&mut level);
    let (mut internal, mut inbound, mut outbound, mut kept) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for e in &level.edges {
        match (inside.contains(&e.from.0), inside.contains(&e.to.0)) {
            (true, true) => internal.push(e.clone()),
            (false, true) => inbound.push(e.clone()),
            (true, false) => outbound.push(e.clone()),
            (false, false) => kept.push(e.clone()),
        }
    }
    if inbound.len() > 1 {
        return Err("Group: only one input edge supported".into());
    }
    if outbound.len() > 1 {
        return Err("Group: only one output edge supported".into());
    }

    // The body: its input and output made for it, the frame after its
    // input, the nodes in their order at the level.
    let frame = Uuid::new_v5(&group, b"group.frame");
    let mut body = Graph { nodes: Vec::new(), edges: internal, output: out, ..Graph::empty() };
    body.nodes.push(node(src, lt::GROUP_INPUT, Some(entry("Source", "Source"))));
    body.nodes.push(node(frame, lt::GROUP_FRAME, None));
    body.nodes.push(node(out, lt::GROUP_OUTPUT, Some(entry("Output", "Output"))));
    body.nodes.extend(level.nodes.iter().filter(|n| inside.contains(&n.id)).cloned());
    if let Some(e) = inbound.first() {
        body.edges.push(Edge { from: (src, ports::output_key("Source", 0)), to: e.to.clone() });
    }
    if let Some(e) = outbound.first() {
        body.edges.push(Edge { from: e.from.clone(), to: (out, ports::input_key("Output", 0)) });
    }
    body.ui.positions.insert(src, at(-200.0, 0.0));
    body.ui.positions.insert(out, at(800.0, 0.0));
    let (mut cx, mut cy, mut count) = (0.0_f32, 0.0_f32, 0u32);
    for id in &inside {
        if let Some(p) = level.ui.positions.remove(id) {
            if let Some((x, y)) = pos(&p) {
                cx += x;
                cy += y;
                count += 1;
            }
            body.ui.positions.insert(*id, p);
        }
    }
    derive(&mut body);

    // The level: the nodes gone, the compound in their place, rewired.
    let mut compound = node(group, lt::GROUP, Some(entry("Compound", "Compound")));
    compound.group = Some(Box::new(GroupBody { graph: body, ports: Vec::new(), interface: Vec::new() }));
    level.nodes.retain(|n| !inside.contains(&n.id));
    level.nodes.push(compound);
    level.edges = kept;
    if let Some(e) = inbound.first() {
        level.edges.push(Edge { from: e.from.clone(), to: (group, ports::input_key("Compound", 0)) });
    }
    if let Some(e) = outbound.first() {
        level.edges.push(Edge { from: (group, ports::output_key("Compound", 0)), to: e.to.clone() });
    }
    let centroid = if count > 0 { (cx / count as f32, cy / count as f32) } else { (0.0, 0.0) };
    level.ui.positions.insert(group, at(centroid.0, centroid.1));
    let survivors: BTreeSet<String> = level.nodes.iter().map(|n| n.id.to_string()).collect();
    level.ui.frames = std::mem::take(&mut level.ui.frames)
        .into_iter()
        .filter_map(|mut f| {
            if let Some(ids) = f.get_mut("contained_node_ids").and_then(Value::as_array_mut) {
                ids.retain(|id| id.as_str().is_some_and(|s| survivors.contains(s)));
                if ids.is_empty() {
                    return None;
                }
            }
            Some(f)
        })
        .collect();
    derive(&mut level);
    level.check_lifted_body().map_err(|e| format!("Group would leave the grade invalid: {e}"))?;
    *g = level;
    Ok(())
}

/// Dissolve the compound `group` at level `g`: its nodes join the level,
/// what fed it feeds what its input fed, and what read it reads what fed
/// its output.
pub fn ungroup(g: &mut Graph, group: NodeId) -> Result<(), String> {
    let n = g.nodes.iter().find(|n| n.id == group).ok_or_else(|| format!("{group} isn't in the graph"))?;
    if type_of(n) != lt::GROUP || kind_of(n) != "Compound" {
        return Err("Ungroup: selection is not a Compound".into());
    }
    let mut body = n.group.as_ref().ok_or("Ungroup: compound has no sub-graph")?.graph.clone();
    refuse_set_aside(g)?;
    refuse_set_aside(&body)?;
    let (src, out, frame) = (input_of(&body), body.output, frame_of(&body));
    strip_derived(&mut body);
    let entry = src.and_then(|s| body.edges.iter().find(|e| e.from.0 == s).map(|e| e.to.clone()));
    let exit = body.edges.iter().find(|e| e.to.0 == out).map(|e| e.from.clone());
    let mut level = g.clone();
    strip_derived(&mut level);
    let fed = level.edges.iter().find(|e| e.to.0 == group).cloned();
    let read = level.edges.iter().find(|e| e.from.0 == group).cloned();
    let anchor = level.ui.positions.remove(&group).and_then(|p| pos(&p)).unwrap_or((0.0, 0.0));
    level.nodes.retain(|n| n.id != group);
    level.edges.retain(|e| e.from.0 != group && e.to.0 != group);
    let mut placed = 0i32;
    for n in &body.nodes {
        if Some(n.id) == src || n.id == out || Some(n.id) == frame {
            continue;
        }
        level.nodes.push(n.clone());
        let p = body.ui.positions.get(&n.id).cloned().unwrap_or_else(|| at(anchor.0 + 30.0 * placed as f32, anchor.1 + 30.0 * placed as f32));
        level.ui.positions.insert(n.id, p);
        placed += 1;
    }
    level.edges.extend(body.edges.iter().filter(|e| Some(e.from.0) != src && e.to.0 != out).cloned());
    if let (Some(fed), Some(to)) = (&fed, entry) {
        level.edges.push(Edge { from: fed.from.clone(), to });
    }
    if let (Some(read), Some(from)) = (&read, exit) {
        level.edges.push(Edge { from, to: read.to.clone() });
    }
    derive(&mut level);
    level.check_lifted_body().map_err(|e| format!("Ungroup would leave the grade invalid: {e}"))?;
    *g = level;
    Ok(())
}

/// The edges the lift would give `node` if it stood unwired at level `g`:
/// what an add by path connects so the node reads what a v1 node added
/// unwired read (the frame into its primary, and the like).
pub fn inputs_of_unwired(g: &Graph, node: &Node) -> Vec<Edge> {
    let mut level = g.clone();
    strip_derived(&mut level);
    level.nodes.push(node.clone());
    derive(&mut level);
    level.edges.into_iter().filter(|e| e.to.0 == node.id).collect()
}
