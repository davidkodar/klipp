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

use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root, TrackPart};
use klipp_graph_format::{ports, types, Edge, Graph, Node, NodeId, ParamKey, ParamValue, PortKey};
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
        | GraphOp::MoveAfter { path, .. } => path,
        GraphOp::Connect { to, .. } | GraphOp::Disconnect { to, .. } => to,
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
        n.params.insert(key.clone(), value.clone());
        return Ok(());
    };
    let v = as_v1(value).ok_or_else(|| format!("{key} is set through the effect's own editor until effects become nodes"))?;
    let mut entry = legacy.payload.clone();
    let grade_node = t == types::LEGACY_NODE || t == lt::MERGE;
    let target = if grade_node { entry.get_mut("params").filter(|p| p.is_object()) } else { Some(&mut entry) };
    let target = match target {
        Some(t) => t,
        None => {
            entry["params"] = serde_json::json!({});
            &mut entry["params"]
        }
    };
    target[key.as_str()] = v;
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

/// The edits on a graph level.
fn apply_graph(g: &mut Graph, op: &GraphOp, target: NodeId) -> Result<(), String> {
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
            match feed {
                Some(feed) => {
                    for e in g.edges.iter_mut().filter(|e| e.from.0 == target) {
                        e.from = feed.clone();
                    }
                }
                None => g.edges.retain(|e| e.from.0 != target),
            }
            g.nodes.remove(at);
        }
        GraphOp::Connect { from, from_port, to_port, .. } => {
            let from_id = from.node().ok_or("an empty path")?;
            find(g, from_id)?;
            find(g, target)?;
            g.edges.retain(|e| e.to != (target, to_port.clone()));
            g.edges.push(Edge { from: (from_id, from_port.clone()), to: (target, to_port.clone()) });
        }
        GraphOp::Disconnect { port, .. } => {
            find(g, target)?;
            g.edges.retain(|e| e.to != (target, port.clone()));
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
            g.nodes[at].mix = *mix;
        }
        GraphOp::MoveAfter { after, .. } => {
            let node = g.nodes[find(g, target)?].clone();
            apply_graph(g, &GraphOp::RemoveNode { path: NodePath::new(Root::Clip { clip: uuid::Uuid::nil() }, [target]) }, target)?;
            apply_graph(g, &GraphOp::AddNode { after: NodePath::new(Root::Clip { clip: uuid::Uuid::nil() }, [*after]), node: Box::new(node) }, *after)?;
        }
    }
    Ok(())
}

/// The edits on a track's effect list, which has no edges.
fn apply_list(list: &mut Vec<Node>, op: &GraphOp, target: NodeId) -> Result<(), String> {
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
            list[at].mix = *mix;
        }
        GraphOp::MoveAfter { after, .. } => {
            let at = find(list, target)?;
            let node = list.remove(at);
            let to = find(list, *after)?;
            list.insert(to + 1, node);
        }
        GraphOp::Connect { .. } | GraphOp::Disconnect { .. } => return Err("a track's effects are a list, with nothing to wire".into()),
    }
    Ok(())
}

/// Apply `op` to `docs`, checked, and return the edit for undo.
pub fn apply(docs: &mut Documents, op: &GraphOp) -> Result<GraphEdit, String> {
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
            apply_list(&mut list, op, last)?;
            serde_json::to_value(list).map_err(err)?
        }
        Root::Clip { .. } => {
            let mut picture: klipp_graph_format::lifted::ClipPicture = serde_json::from_value(copy).map_err(err)?;
            apply_graph(level(&mut picture.graph, through)?, op, last)?;
            picture.graph.check_lifted(false).map_err(|e| format!("that would leave the clip's graph invalid: {e}"))?;
            serde_json::to_value(picture).map_err(err)?
        }
        _ => {
            let mut body: Graph = serde_json::from_value(std::mem::take(&mut copy)).map_err(err)?;
            apply_graph(level(&mut body, through)?, op, last)?;
            body.check_lifted_body().map_err(|e| format!("that would leave the grade invalid: {e}"))?;
            serde_json::to_value(body).map_err(err)?
        }
    };
    put(docs, root, &after)?;
    Ok(GraphEdit { root, before, after })
}

/// Undo an edit: put the root's picture back as it was.
pub fn undo(docs: &mut Documents, edit: &GraphEdit) -> Result<(), String> {
    put(docs, edit.root, &edit.before)
}
