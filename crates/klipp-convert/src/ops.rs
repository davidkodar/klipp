//! Graph ops by path (#1083, design §11.1): the one op set the interface,
//! the socket and the assistant will share after the switch, as pure
//! functions over a converted project's pictures.
//!
//! Each op is applied to a copy of the root's picture and checked before
//! it lands (`check_lifted` for a clip, `check_lifted_body` for a body);
//! a refusal is a sentence the assistant can act on. Each returns a
//! [`GraphEdit`], the root's picture before and after, which is the undo
//! the design keeps: restoring `before` undoes it exactly.
//!
//! Until 2b gives effects their own node types, nearly every node is a
//! `klipp.legacy.*` node carrying its v1 entry, so setting one of its
//! parameters edits that entry; the result must still read as the frozen
//! v1 types read it, which is the range check there is before the
//! registry takes over.

use klipp_graph_format::lifted::{types as lt, LegacyFlags};
use klipp_graph_format::path::{NodePath, Root, TrackPart};
use klipp_graph_format::{ports, types, Edge, Graph, Node, NodeId, ParamKey, ParamValue, PortKey, PortBinding, PortCatalog, PortDirection, PortType, GroupPort};
use serde_json::Value;

use crate::paths::Documents;

/// One edit of one root's graph.
#[derive(Clone, Debug, PartialEq)]
pub enum GraphOp {
    /// Put `node` after the node at `after`: whatever read that node reads
    /// the new one, which reads it.
    AddNode { after: NodePath, node: Box<Node> },
    /// Take a node out: whatever read it reads its primary input.
    RemoveNode { path: NodePath },
    /// Feed `to`'s `to_port` from `from`'s `from_port`, replacing what fed
    /// it. Both nodes at one level.
    Connect { from: NodePath, from_port: PortKey, to: NodePath, to_port: PortKey },
    /// Leave `to`'s `port` unfed.
    Disconnect { to: NodePath, port: PortKey },
    SetParam { path: NodePath, key: ParamKey, value: ParamValue },
    SetEnabled { path: NodePath, enabled: bool },
    SetMix { path: NodePath, mix: Option<f32> },
    /// Expose a direct child's output under a stable public key.
    /// A clip grade group with published ports cannot yet load into the
    /// legacy app model; delift refuses it until that model can preserve them.
    PublishOutput { group: NodePath, endpoint: PortBinding, key: PortKey, label: String },
    /// Remove an exported port; existing readers must be disconnected first.
    Unpublish { group: NodePath, key: PortKey },
    /// None clears the saved setting, restoring migrated red semantics.
    SetMaskChannel { path: NodePath, channel: Option<String> },
    /// Move a node to just after another at its level.
    MoveAfter { path: NodePath, after: NodeId },
}

/// What an op did to its root: the root's picture before and after, as
/// stored. Restoring `before` undoes the op.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphEdit {
    pub root: Root,
    pub before: Value,
    pub after: Value,
}

fn path_of(op: &GraphOp) -> &NodePath {
    match op {
        GraphOp::AddNode { after, .. } => after,
        GraphOp::RemoveNode { path }
        | GraphOp::SetParam { path, .. }
        | GraphOp::SetEnabled { path, .. }
        | GraphOp::SetMix { path, .. }
        | GraphOp::SetMaskChannel { path, .. }
        | GraphOp::MoveAfter { path, .. } => path,
        GraphOp::Connect { to, .. } | GraphOp::Disconnect { to, .. } => to,
        GraphOp::PublishOutput { group, .. } | GraphOp::Unpublish { group, .. } => group,
    }
}

/// A root's picture, taken out as JSON to edit a copy of.
fn snapshot(docs: &Documents, root: Root) -> Result<Value, String> {
    let v = match root {
        Root::Clip { clip } => serde_json::to_value(docs.clips.get(&clip).ok_or("no such clip")?),
        Root::TimelineGrade { sequence } => serde_json::to_value(docs.timeline.get(&sequence).ok_or("no such timeline grade")?),
        Root::GroupStage { group, stage } => serde_json::to_value(docs.stages.get(&(group, stage)).ok_or("no such grade-group stage")?),
        Root::Track { track, part: TrackPart::Grade } => serde_json::to_value(docs.track_grades.get(&track).ok_or("no such track grade")?),
        Root::Track { track, part: TrackPart::Effects } => serde_json::to_value(docs.track_effects.get(&track).ok_or("no such track")?),
    };
    v.map_err(|e| e.to_string())
}

/// Put a root's picture back from JSON.
fn put(docs: &mut Documents, root: Root, v: &Value) -> Result<(), String> {
    let e = |e: serde_json::Error| e.to_string();
    match root {
        Root::Clip { clip } => {
            docs.clips.insert(clip, serde_json::from_value(v.clone()).map_err(e)?);
        }
        Root::TimelineGrade { sequence } => {
            docs.timeline.insert(sequence, serde_json::from_value(v.clone()).map_err(e)?);
        }
        Root::GroupStage { group, stage } => {
            docs.stages.insert((group, stage), serde_json::from_value(v.clone()).map_err(e)?);
        }
        Root::Track { track, part: TrackPart::Grade } => {
            docs.track_grades.insert(track, serde_json::from_value(v.clone()).map_err(e)?);
        }
        Root::Track { track, part: TrackPart::Effects } => {
            docs.track_effects.insert(track, serde_json::from_value(v.clone()).map_err(e)?);
        }
    }
    Ok(())
}

/// The graph a path's last id sits in: the root graph, then down through
/// the groups the path names.
fn level<'a>(mut g: &'a mut Graph, through: &[NodeId]) -> Result<&'a mut Graph, String> {
    for step in through {
        let n = g.nodes.iter_mut().find(|n| n.id == *step).ok_or_else(|| format!("{step} isn't in the graph"))?;
        g = &mut n.group.as_mut().ok_or_else(|| format!("{step} isn't a group"))?.graph;
    }
    Ok(g)
}

fn kind(n: &Node) -> &str {
    n.legacy.as_ref().and_then(|l| l.payload.get("kind")).and_then(Value::as_str).unwrap_or("")
}

/// The input a node's picture comes in by.
fn primary(n: &Node) -> PortKey {
    if n.type_id.as_str() == types::LEGACY_NODE || n.type_id.as_str() == lt::MERGE {
        ports::input_key(kind(n), 0)
    } else {
        "in".into()
    }
}

/// A graph's ends, which no op may remove or add.
fn is_end(n: &Node) -> bool {
    matches!(n.type_id.as_str(), lt::SOURCE | lt::OUTPUT | lt::GROUP_INPUT | lt::GROUP_OUTPUT | lt::GROUP_FRAME)
}

/// A parameter value as the v1 entry stores it, or `None` for one that
/// isn't set that way before 2b.
fn as_v1(value: &ParamValue) -> Option<Value> {
    Some(match value {
        ParamValue::Float(f) => serde_json::json!(f),
        ParamValue::Int(i) => serde_json::json!(i),
        ParamValue::Bool(b) => serde_json::json!(b),
        ParamValue::Enum(e) => serde_json::json!(e),
        ParamValue::Text(t) => serde_json::json!(t),
        ParamValue::Point2(p) => serde_json::json!(p),
        ParamValue::Color { .. } | ParamValue::Matrix3(_) => return None,
    })
}

/// Set a parameter on a node: a legacy node's v1 entry, which must still
/// read as the frozen v1 types read it; any other node's own parameters.
fn set_param(n: &mut Node, key: &ParamKey, value: &ParamValue) -> Result<(), String> {
    let t = n.type_id.as_str().to_owned();
    let Some(legacy) = n.legacy.as_mut().filter(|_| t.starts_with("klipp.legacy.") || t == lt::MERGE) else {
        // A placement's mode is one of its two.
        if t == lt::PLACEMENT && key.as_str() == "mode" && !matches!(value, ParamValue::Enum(m) if *m == lt::PLACEMENT_FIXED || *m == lt::PLACEMENT_AUTO) {
            return Err(format!("a placement's mode is Fixed or Auto, not {value:?}"));
        }
        n.params.insert(key.clone(), value.clone());
        return Ok(());
    };
    // What the entry is, not a setting of it.
    if matches!(key.as_str(), "kind" | "type") {
        return Err(format!("{key} names what the node is, and isn't set"));
    }
    let v = as_v1(value).ok_or_else(|| format!("{key} is set through the effect's own editor until effects become nodes"))?;
    let mut entry = legacy.payload.clone();
    if !entry.is_object() {
        return Err(format!("the node's stored entry isn't an object, so {key} has nowhere to go"));
    }
    let grade_node = t == types::LEGACY_NODE || t == lt::MERGE;
    let target = if grade_node { entry.get_mut("params").filter(|p| p.is_object()) } else { Some(&mut entry) };
    let target = match target {
        Some(t) => t,
        None => {
            entry["params"] = serde_json::json!({});
            &mut entry["params"]
        }
    };
    target.as_object_mut().ok_or_else(|| format!("the node's stored entry isn't an object, so {key} has nowhere to go"))?.insert(key.as_str().to_owned(), v);
    let reads = if grade_node {
        serde_json::from_value::<crate::v1::params::NodeParams>(entry["params"].clone()).is_ok()
    } else if t == types::LEGACY_OFX {
        true
    } else {
        serde_json::from_value::<crate::v1::effects::BuiltinEffect>(entry.clone()).is_ok()
    };
    if !reads {
        return Err(format!("{key} doesn't take {value:?}"));
    }
    legacy.payload = entry;
    Ok(())
}

fn set_mix(node: &mut Node, mix: Option<f32>, catalog: Option<&dyn PortCatalog>) -> Result<(), String> {
    if mix.is_some_and(|v| !v.is_finite()) { return Err("mix must be finite".into()); }
    if let Some(catalog) = catalog {
        if !catalog.node_spec(node).is_some_and(|s| s.mixable) { return Err("this node type is not mixable".into()); }
    }
    node.mix = mix;
    Ok(())
}

fn set_mask_channel(node: &mut Node, channel: Option<&str>, catalog: Option<&dyn PortCatalog>) -> Result<(), String> {
    let spec = catalog.and_then(|c| c.node_spec(node)).ok_or("a mask channel needs a registered node description")?;
    let mask = spec.inputs.iter().find(|p| p.key == "mask").ok_or("this node has no mask input")?;
    let options = mask.params.iter().find(|(key, _)| key == "channel").ok_or("this mask input has no channel setting; it reads red")?;
    if channel.is_some_and(|c| !options.1.iter().any(|v| v == c)) { return Err("unknown mask channel".into()); }
    let key = ParamKey::from("mask.channel");
    if let Some(value) = channel { node.params.insert(key, ParamValue::Text(value.into())); }
    else { node.params.remove(&key); }
    Ok(())
}

fn published_output_type(node: &Node, port: &PortKey, catalog: Option<&dyn PortCatalog>) -> Result<PortType, String> {
    if node.type_id.as_str() == lt::GROUP {
        if port.as_str() == "out" { return Ok(PortType::Image); }
        return node.group.as_ref().and_then(|b| b.interface.iter().find(|p| &p.key == port && p.direction == PortDirection::Output)).map(|p| p.ty).ok_or_else(|| "the child group has no such output".into());
    }
    let spec = catalog.and_then(|c| c.node_spec(node)).ok_or("publishing this endpoint needs a registered node description")?;
    spec.outputs.iter().find(|p| p.key == port.as_str()).map(|p| p.ty).ok_or_else(|| "the child has no such output".into())
}

/// The edits on a graph level.
fn apply_graph(g: &mut Graph, op: &GraphOp, target: NodeId, catalog: Option<&dyn PortCatalog>) -> Result<(), String> {
    let find = |g: &Graph, id: NodeId| g.nodes.iter().position(|n| n.id == id).ok_or_else(|| format!("{id} isn't in the graph"));
    match op {
        GraphOp::AddNode { node, .. } => {
            if g.nodes.iter().any(|n| n.id == node.id) {
                return Err(format!("{} is already in the graph", node.id));
            }
            if is_end(node) {
                return Err("a graph's ends can't be added".into());
            }
            let at = find(g, target)?;
            for e in g.edges.iter_mut().filter(|e| e.from == (target, PortKey::from("out"))) {
                e.from = (node.id, "out".into());
            }
            g.edges.push(Edge { from: (target, "out".into()), to: (node.id, primary(node)) });
            g.nodes.insert(at + 1, (**node).clone());
        }
        GraphOp::RemoveNode { .. } => {
            let at = find(g, target)?;
            if is_end(&g.nodes[at]) {
                return Err("a graph's ends can't be removed".into());
            }
            let port = primary(&g.nodes[at]);
            let feed = g.edges.iter().find(|e| e.to == (target, port.clone())).map(|e| e.from.clone());
            g.edges.retain(|e| e.to.0 != target);
            // Its readers' pictures take its input back; anything else it fed
            // (a gate's matte, a merge's second picture) is disconnected,
            // not handed a picture it never was.
            let primaries: Vec<(NodeId, PortKey)> = g.nodes.iter().map(|n| (n.id, primary(n))).collect();
            let reads_picture = |e: &Edge| primaries.iter().any(|(id, p)| *id == e.to.0 && *p == e.to.1);
            let mut unmasked = Vec::new();
            g.edges.retain_mut(|e| {
                if e.from.0 != target {
                    return true;
                }
                match &feed {
                    Some(feed) if reads_picture(e) => {
                        e.from = feed.clone();
                        true
                    }
                    _ => {
                        if e.to.1.as_str() == "mask" {
                            unmasked.push(e.to.0);
                        }
                        false
                    }
                }
            });
            // A correction that lost its matte isn't gated any more.
            for n in g.nodes.iter_mut().filter(|n| unmasked.contains(&n.id)) {
                if let Some(f) = n.legacy_flags.as_mut() {
                    f.alpha_one = false;
                    if *f == LegacyFlags::default() {
                        n.legacy_flags = None;
                    }
                }
            }
            g.nodes.remove(at);
        }
        GraphOp::Connect { from, from_port, to_port, .. } => {
            let from_id = from.node().ok_or("an empty path")?;
            find(g, from_id)?;
            let at = find(g, target)?;
            let connected = g.edges.iter().any(|e| e.to == (target, to_port.clone()))
                || g.nodes[at].wireless.input_links.get(to_port).is_some_and(|name| g.nodes.iter().any(|n| n.id != target && n.wireless.output_link.as_deref() == Some(name)));
            if to_port.as_str() == "mask" && g.nodes[at].type_id.as_str() != lt::GROUP {
                if let Some(catalog) = catalog {
                    let spec = catalog.node_spec(&g.nodes[at]).ok_or("a mask connection needs a registered node description")?;
                    let mask = spec.inputs.iter().find(|p| p.key == "mask" && p.ty == PortType::Matte).ok_or("this node has no mask input")?;
                    if !connected && !g.nodes[at].params.contains_key(&ParamKey::from("mask.channel")) && mask.params.iter().any(|(key, _)| key == "channel") {
                        set_mask_channel(&mut g.nodes[at], Some("luminance"), Some(catalog))?;
                    }
                }
            }
            if !connected && to_port.as_str() == "mask" && g.nodes[at].type_id.as_str() == types::LEGACY_NODE && ports::GATEABLE.contains(&kind(&g.nodes[at])) {
                g.nodes[at].legacy_flags.get_or_insert_with(LegacyFlags::default).alpha_one = true;
            }
            g.edges.retain(|e| e.to != (target, to_port.clone()));
            g.edges.push(Edge { from: (from_id, from_port.clone()), to: (target, to_port.clone()) });
        }
        GraphOp::Disconnect { port, .. } => {
            let at = find(g, target)?;
            g.edges.retain(|e| e.to != (target, port.clone()));
            let wireless_mask = g.nodes[at].wireless.input_links.get(&PortKey::from("mask")).is_some_and(|name| g.nodes.iter().any(|n| n.id != target && n.wireless.output_link.as_deref() == Some(name)));
            if port.as_str() == "mask" && !wireless_mask && g.nodes[at].type_id.as_str() == types::LEGACY_NODE {
                if let Some(flags) = g.nodes[at].legacy_flags.as_mut() {
                    flags.alpha_one = false;
                    if *flags == LegacyFlags::default() { g.nodes[at].legacy_flags = None; }
                }
            }
        }
        GraphOp::SetParam { key, value, .. } => {
            let at = find(g, target)?;
            set_param(&mut g.nodes[at], key, value)?;
        }
        GraphOp::SetEnabled { enabled, .. } => {
            let at = find(g, target)?;
            g.nodes[at].enabled = *enabled;
        }
        GraphOp::SetMix { mix, .. } => {
            let at = find(g, target)?;
            set_mix(&mut g.nodes[at], *mix, catalog)?;
        }
        GraphOp::PublishOutput { endpoint, key, label, .. } => {
            let at = find(g, target)?;
            let body = g.nodes[at].group.as_mut().ok_or("the target is not a group")?;
            let child = body.graph.nodes.iter().find(|n| n.id == endpoint.node).ok_or("the endpoint is not a direct child of this group")?;
            let ty = published_output_type(child, &endpoint.port, catalog)?;
            body.interface.push(GroupPort { key: key.clone(), direction: PortDirection::Output, ty, binding: endpoint.clone(), label: label.clone() });
        }
        GraphOp::Unpublish { key, .. } => {
            let at = find(g, target)?;
            let body = g.nodes[at].group.as_mut().ok_or("the target is not a group")?;
            let index = body.interface.iter().position(|p| &p.key == key && p.direction == PortDirection::Output).ok_or("no such published output")?;
            body.interface.remove(index);
        }
        GraphOp::SetMaskChannel { channel, .. } => {
            let at = find(g, target)?;
            set_mask_channel(&mut g.nodes[at], channel.as_deref(), catalog)?;
        }
        GraphOp::MoveAfter { after, .. } => {
            let node = g.nodes[find(g, target)?].clone();
            // Its other inputs (a gate's mask, a merge's second picture)
            // move with it; only the primary is rewired. A loop this makes
            // is refused by the check that follows every op.
            let port = primary(&node);
            let others: Vec<Edge> = g.edges.iter().filter(|e| e.to.0 == target && e.to.1 != port).cloned().collect();
            // And what it feeds other than a picture (its matte into a gate)
            // stays its own, where the removal would disconnect it.
            let primaries: Vec<(NodeId, PortKey)> = g.nodes.iter().map(|n| (n.id, primary(n))).collect();
            let outs: Vec<Edge> = g.edges.iter().filter(|e| e.from.0 == target && !primaries.iter().any(|(id, p)| *id == e.to.0 && *p == e.to.1)).cloned().collect();
            let gated: Vec<(NodeId, Option<LegacyFlags>)> = outs.iter().filter_map(|e| g.nodes.iter().find(|n| n.id == e.to.0)).map(|n| (n.id, n.legacy_flags.clone())).collect();
            apply_graph(g, &GraphOp::RemoveNode { path: NodePath::new(Root::Clip { clip: uuid::Uuid::nil() }, [target]) }, target, catalog)?;
            apply_graph(g, &GraphOp::AddNode { after: NodePath::new(Root::Clip { clip: uuid::Uuid::nil() }, [*after]), node: Box::new(node) }, *after, catalog)?;
            g.edges.extend(others);
            g.edges.extend(outs);
            for (id, flags) in gated {
                if let Some(n) = g.nodes.iter_mut().find(|n| n.id == id) {
                    n.legacy_flags = flags;
                }
            }
        }
    }
    Ok(())
}

/// The entry types a track's effect list holds.
const TRACK_ENTRY_TYPES: [&str; 6] =
    [types::LEGACY_EFFECT, types::LEGACY_OPACITY, types::LEGACY_TRANSFORM, types::LEGACY_SPEED_RAMP, types::LEGACY_TEMPORAL_DENOISE, types::LEGACY_OFX];

/// A track's effect list as an op may leave it: effect and plug-in entries
/// only, each once, none a graph's end, a group or a spliced node.
fn check_track_list(list: &[Node]) -> Result<(), String> {
    let mut ids = std::collections::BTreeSet::new();
    for n in list {
        if !ids.insert(n.id) {
            return Err(format!("{} is on the track twice", n.id));
        }
        if is_end(n) || n.group.is_some() || n.origin.is_some() || !TRACK_ENTRY_TYPES.contains(&n.type_id.as_str()) {
            return Err(format!("a track's effects hold effect and plug-in entries, not a {} like {}", n.type_id.as_str(), n.id));
        }
    }
    Ok(())
}

/// The edits on a track's effect list, which has no edges.
fn apply_list(list: &mut Vec<Node>, op: &GraphOp, target: NodeId, catalog: Option<&dyn PortCatalog>) -> Result<(), String> {
    let find = |l: &Vec<Node>, id: NodeId| l.iter().position(|n| n.id == id).ok_or_else(|| format!("{id} isn't on the track"));
    match op {
        GraphOp::AddNode { node, .. } => {
            if list.iter().any(|n| n.id == node.id) {
                return Err(format!("{} is already on the track", node.id));
            }
            let at = find(list, target)?;
            list.insert(at + 1, (**node).clone());
        }
        GraphOp::RemoveNode { .. } => {
            let at = find(list, target)?;
            list.remove(at);
        }
        GraphOp::SetParam { key, value, .. } => {
            let at = find(list, target)?;
            set_param(&mut list[at], key, value)?;
        }
        GraphOp::SetEnabled { enabled, .. } => {
            let at = find(list, target)?;
            list[at].enabled = *enabled;
        }
        GraphOp::SetMix { mix, .. } => {
            let at = find(list, target)?;
            set_mix(&mut list[at], *mix, catalog)?;
        }
        GraphOp::MoveAfter { after, .. } => {
            let at = find(list, target)?;
            let node = list.remove(at);
            let to = find(list, *after)?;
            list.insert(to + 1, node);
        }
        GraphOp::Connect { .. } | GraphOp::Disconnect { .. } | GraphOp::PublishOutput { .. } | GraphOp::Unpublish { .. } | GraphOp::SetMaskChannel { .. } => return Err("a track's effects are a list, with nothing to wire".into()),
    }
    Ok(())
}

/// Apply `op` to `docs`, checked, and return the edit for undo.
pub fn apply(docs: &mut Documents, op: &GraphOp) -> Result<GraphEdit, String> {
    apply_inner(docs, op, None)
}

/// Apply with registry-defined type, mask and mix checks.
pub fn apply_with_catalog(docs: &mut Documents, op: &GraphOp, catalog: &dyn PortCatalog) -> Result<GraphEdit, String> {
    apply_inner(docs, op, Some(catalog))
}

fn apply_inner(docs: &mut Documents, op: &GraphOp, catalog: Option<&dyn PortCatalog>) -> Result<GraphEdit, String> {
    let path = path_of(op);
    let (through, last) = match path.nodes.split_last() {
        Some((last, through)) => (through, *last),
        None => return Err("an empty path names no node".into()),
    };
    if let GraphOp::Connect { from, .. } = op {
        if from.root != path.root || from.nodes.split_last().map(|(_, t)| t) != Some(through) {
            return Err("a connection joins two nodes of one graph".into());
        }
    }
    let root = path.root;
    let before = snapshot(docs, root)?;
    let mut copy = before.clone();
    let err = |e: serde_json::Error| e.to_string();
    let after = match root {
        Root::Track { part: TrackPart::Effects, .. } => {
            let mut list: Vec<Node> = serde_json::from_value(copy).map_err(err)?;
            if !through.is_empty() {
                return Err("a track effect has no inside to reach into".into());
            }
            apply_list(&mut list, op, last, catalog)?;
            check_track_list(&list).map_err(|e| format!("that would leave the track's effects invalid: {e}"))?;
            serde_json::to_value(list).map_err(err)?
        }
        Root::Clip { .. } => {
            let mut picture: klipp_graph_format::lifted::ClipPicture = serde_json::from_value(copy).map_err(err)?;
            apply_graph(level(&mut picture.graph, through)?, op, last, catalog)?;
            (if let Some(catalog) = catalog { picture.graph.check_lifted_with_catalog(false, catalog) } else { picture.graph.check_lifted(false) }).map_err(|e| format!("that would leave the clip's graph invalid: {e}"))?;
            serde_json::to_value(picture).map_err(err)?
        }
        _ => {
            let mut body: Graph = serde_json::from_value(std::mem::take(&mut copy)).map_err(err)?;
            apply_graph(level(&mut body, through)?, op, last, catalog)?;
            (if let Some(catalog) = catalog { body.check_lifted_body_with_catalog(catalog) } else { body.check_lifted_body() }).map_err(|e| format!("that would leave the grade invalid: {e}"))?;
            serde_json::to_value(body).map_err(err)?
        }
    };
    put(docs, root, &after)?;
    Ok(GraphEdit { root, before, after })
}

/// Apply edits to one root atomically, returning one undo record.
pub fn apply_batch(docs: &mut Documents, ops: &[GraphOp]) -> Result<GraphEdit, String> {
    apply_batch_inner(docs, ops, None)
}

/// Atomically publish and connect (or make other edits) with registry checks.
pub fn apply_batch_with_catalog(docs: &mut Documents, ops: &[GraphOp], catalog: &dyn PortCatalog) -> Result<GraphEdit, String> {
    apply_batch_inner(docs, ops, Some(catalog))
}

fn apply_batch_inner(docs: &mut Documents, ops: &[GraphOp], catalog: Option<&dyn PortCatalog>) -> Result<GraphEdit, String> {
    let root = path_of(ops.first().ok_or("an empty batch has no edit")?).root;
    if ops.iter().any(|op| path_of(op).root != root) { return Err("a batch edits one graph root".into()); }
    let before = snapshot(docs, root)?;
    let mut copy = Documents::default();
    put(&mut copy, root, &before)?;
    for op in ops { apply_inner(&mut copy, op, catalog)?; }
    let after = snapshot(&copy, root)?;
    put(docs, root, &after)?;
    Ok(GraphEdit { root, before, after })
}

/// Undo an edit: put the root's picture back as it was.
///
/// The undo stack is the caller's: edits on one root undo in the reverse
/// order they were made, since each restores its root whole. An edit on
/// another root is untouched by it.
pub fn undo(docs: &mut Documents, edit: &GraphEdit) -> Result<(), String> {
    put(docs, edit.root, &edit.before)
}
