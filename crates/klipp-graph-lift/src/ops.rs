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
use klipp_graph_format::{ports, types, Animation, Edge, Graph, Node, NodeId, ParamKey, ParamValue, PortKey, PortCatalog, PortDirection, PortType, GroupPort};
use serde_json::Value;

use crate::paths::Documents;

pub use klipp_graph_format::op::GraphOp;

/// What an op did to its root: the root's picture before and after, as
/// stored. Restoring `before` undoes the op.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphEdit {
    pub root: Root,
    pub before: Value,
    pub after: Value,
}

fn path_of(op: &GraphOp) -> &NodePath {
    op.path()
}

/// A root's picture, taken out as JSON to edit a copy of.
pub(crate) fn snapshot(docs: &Documents, root: Root) -> Result<Value, String> {
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
pub(crate) fn put(docs: &mut Documents, root: Root, v: &Value) -> Result<(), String> {
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

/// The containing group's published child bindings are external references
/// when a planner edits the body. They must survive even without local wires.
fn containing_interface(mut graph: &Graph, through: &[NodeId]) -> Result<Vec<GroupPort>, String> {
    let mut interface = Vec::new();
    for step in through {
        let node = graph.nodes.iter().find(|n| n.id == *step).ok_or_else(|| format!("{step} isn't in the graph"))?;
        let body = node.group.as_ref().ok_or_else(|| format!("{step} isn't a group"))?;
        interface = body.interface.clone();
        graph = &body.graph;
    }
    Ok(interface)
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

/// A float as a file writes it: the shortest decimal that reads back as
/// the same `f32` (`0.065`, not the `0.06499999761581421` its widening to
/// `f64` gives), so an op stores what a save and a reload would.
fn float(f: f32) -> Value {
    if !f.is_finite() {
        return Value::Null;
    }
    f.to_string().parse::<f64>().ok().and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number)
}

/// A parameter value as the v1 entry stores it, or `None` for one that
/// isn't set that way before 2b.
fn as_v1(value: &ParamValue) -> Option<Value> {
    Some(match value {
        ParamValue::Float(f) => float(*f),
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
    // An effect's values are its map (M3-2): the value is written there,
    // and the entry the map stores must still read.
    if t == types::LEGACY_EFFECT && crate::effect_params::tag(n).is_some() && n.legacy.as_ref().and_then(|l| l.payload.as_object()).is_some_and(|o| o.len() == 1) {
        if matches!(key.as_str(), "kind" | "type") {
            return Err(format!("{key} names what the node is, and isn't set"));
        }
        let v = as_v1(value).ok_or_else(|| format!("{key} is set through the effect's own editor until effects become nodes"))?;
        if !crate::effect_params::is_field_key(key) {
            return Err(format!("{key} is the node's own, not a setting of its effect"));
        }
        let tag = crate::effect_params::tag(n).unwrap_or_default().to_owned();
        let mut params = n.params.clone();
        params.insert(key.clone(), crate::effect_params::held(&tag, key, &v));
        let entry = crate::effect_params::from_params(&tag, &params)?;
        if serde_json::from_value::<crate::v1::effects::BuiltinEffect>(entry).is_err() {
            return Err(format!("{key} doesn't take {value:?}"));
        }
        n.params = params;
        return Ok(());
    }
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

/// A key's time, if it reads as a key: finite seconds and a finite value.
fn key_time(keyframe: &Value) -> Result<f64, String> {
    let time = keyframe.get("time_secs").and_then(Value::as_f64).filter(|t| t.is_finite()).ok_or("a key needs a finite time_secs")?;
    keyframe.get("value").and_then(Value::as_f64).filter(|v| v.is_finite()).ok_or("a key needs a finite value")?;
    Ok(time)
}

/// Keys at the same time are one key.
const SAME_TIME: f64 = 1e-9;

/// The animation edits, the same on a graph and on a track's list.
fn apply_anim(n: &mut Node, op: &GraphOp) -> Result<(), String> {
    match op {
        GraphOp::AddKeyAt { key, keyframe, .. } => {
            let t = key_time(keyframe)?;
            let a = n.anim.entry(key.clone()).or_insert(Animation { keys: Vec::new(), modifiers: None });
            a.keys.retain(|k| key_time(k).map_or(true, |kt| (kt - t).abs() > SAME_TIME));
            let at = a.keys.iter().position(|k| key_time(k).is_ok_and(|kt| kt > t)).unwrap_or(a.keys.len());
            a.keys.insert(at, keyframe.clone());
        }
        GraphOp::RemoveKeyAt { key, time_secs, .. } => {
            let a = n.anim.get_mut(key).ok_or_else(|| format!("{key} isn't animated"))?;
            let before = a.keys.len();
            a.keys.retain(|k| key_time(k).map_or(true, |kt| (kt - time_secs).abs() > SAME_TIME));
            if a.keys.len() == before {
                return Err(format!("{key} has no key at {time_secs} s"));
            }
            if a.keys.is_empty() && a.modifiers.is_none() {
                n.anim.remove(key);
            }
        }
        GraphOp::SetAnim { key, anim, .. } => match anim {
            Some(a) => {
                for k in &a.keys {
                    key_time(k)?;
                }
                n.anim.insert(key.clone(), a.clone());
            }
            None => {
                n.anim.remove(key);
            }
        },
        _ => unreachable!("only the animation ops come here"),
    }
    Ok(())
}

/// Replace a legacy entry's payload: the same kind (`kind` for a grade
/// node, `type` for an effect), reading as the frozen v1 types read it.
fn set_payload(n: &mut Node, payload: &Value) -> Result<(), String> {
    let t = n.type_id.as_str().to_owned();
    let legacy = n.legacy.as_mut().filter(|_| t.starts_with("klipp.legacy.") || t == lt::MERGE).ok_or("only a stored entry has a payload to replace")?;
    let grade_node = t == types::LEGACY_NODE || t == lt::MERGE;
    let what = if grade_node { "kind" } else { "type" };
    if payload.get(what) != legacy.payload.get(what) {
        return Err(format!("a payload keeps its {what}: {} isn't {}", payload.get(what).unwrap_or(&Value::Null), legacy.payload.get(what).unwrap_or(&Value::Null)));
    }
    let reads = if grade_node {
        payload.get("params").is_none_or(|p| serde_json::from_value::<crate::v1::params::NodeParams>(p.clone()).is_ok())
    } else if t == types::LEGACY_OFX {
        true
    } else {
        serde_json::from_value::<crate::v1::effects::BuiltinEffect>(payload.clone()).is_ok()
    };
    if !reads {
        return Err("that payload doesn't read as the entry it replaces".into());
    }
    // An effect held in memory takes the payload's values into its map.
    if t == types::LEGACY_EFFECT && legacy.payload.as_object().is_some_and(|o| o.len() == 1) && payload.as_object().is_some_and(|o| o.len() > 1) {
        let (_, values) = crate::effect_params::to_params(payload)?;
        n.params.retain(|k, _| !crate::effect_params::is_field_key(k));
        n.params.extend(values);
        return Ok(());
    }
    legacy.payload = payload.clone();
    Ok(())
}

/// A node's label: a grade node's lives in its stored entry, as the file
/// had it; any other node's on the node.
fn set_label(n: &mut Node, label: &str) {
    let grade_node = matches!(n.type_id.as_str(), types::LEGACY_NODE) || n.type_id.as_str() == lt::MERGE;
    match n.legacy.as_mut().filter(|l| grade_node && l.payload.is_object()) {
        Some(legacy) => legacy.payload["label"] = Value::String(label.to_owned()),
        None => n.label = label.to_owned(),
    }
}

/// A node added to a lifted picture, an effect's values held in its map
/// as the lift holds them (M3-2), whichever form it came in.
fn held_node(node: &Node) -> Result<Node, String> {
    let mut n = node.clone();
    crate::effect_params::added_into_memory(&mut n)?;
    Ok(n)
}

/// The edits on a graph level.
fn apply_graph(g: &mut Graph, op: &GraphOp, target: NodeId, catalog: Option<&dyn PortCatalog>, interface: &[GroupPort]) -> Result<(), String> {
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
            g.nodes.insert(at + 1, held_node(node)?);
        }
        GraphOp::AddNodeBeside { node, .. } => {
            if g.nodes.iter().any(|n| n.id == node.id) {
                return Err(format!("{} is already in the graph", node.id));
            }
            if is_end(node) {
                return Err("a graph's ends can't be added".into());
            }
            find(g, target)?;
            g.nodes.push(held_node(node)?);
        }
        GraphOp::RemoveNode { path } => {
            if is_end(&g.nodes[find(g, target)?]) { return Err("a graph's ends can't be removed".into()); }
            crate::edit_planner::rewrite(g, &crate::edit_planner::Request {
                path: path.clone(), action: crate::edit_planner::Action::Remove,
            }, catalog, interface).map_err(|e| e.to_string())?;
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
        GraphOp::SetNodeMeta { label, position, .. } => {
            let at = find(g, target)?;
            if let Some(label) = label {
                set_label(&mut g.nodes[at], label);
            }
            if let Some(position) = position {
                g.ui.positions.insert(target, position.clone());
            }
        }
        GraphOp::AddKeyAt { .. } | GraphOp::RemoveKeyAt { .. } | GraphOp::SetAnim { .. } => {
            let at = find(g, target)?;
            apply_anim(&mut g.nodes[at], op)?;
        }
        GraphOp::SetPayload { payload, .. } => {
            let at = find(g, target)?;
            set_payload(&mut g.nodes[at], payload)?;
        }
        GraphOp::MoveAfter { path, after } => {
            crate::edit_planner::rewrite(g, &crate::edit_planner::Request {
                path: path.clone(), action: crate::edit_planner::Action::MoveAfter(*after),
            }, catalog, interface).map_err(|e| e.to_string())?;
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
        GraphOp::SetNodeMeta { label, position, .. } => {
            if position.is_some() {
                return Err("a track's effects are a list, with no place in a node editor".into());
            }
            let at = find(list, target)?;
            if let Some(label) = label {
                set_label(&mut list[at], label);
            }
        }
        GraphOp::AddKeyAt { .. } | GraphOp::RemoveKeyAt { .. } | GraphOp::SetAnim { .. } => {
            let at = find(list, target)?;
            apply_anim(&mut list[at], op)?;
        }
        GraphOp::SetPayload { payload, .. } => {
            let at = find(list, target)?;
            set_payload(&mut list[at], payload)?;
        }
        GraphOp::Connect { .. } | GraphOp::Disconnect { .. } | GraphOp::PublishOutput { .. } | GraphOp::Unpublish { .. } | GraphOp::SetMaskChannel { .. } | GraphOp::AddNodeBeside { .. } => return Err("a track's effects are a list, with nothing to wire".into()),
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

/// A check's result, with a node added beside in the same batch allowed
/// an input still unwired: the batch connects it, and is checked whole at
/// its end.
fn tolerate(checked: Result<(), klipp_graph_format::lifted::LiftedError>, pending: &[NodeId]) -> Result<(), klipp_graph_format::lifted::LiftedError> {
    match checked {
        Err(klipp_graph_format::lifted::LiftedError::Unwired { node, .. }) if pending.contains(&node) => Ok(()),
        other => other,
    }
}

/// Why a node added beside can't stand alone.
fn unwired_beside(op: &GraphOp, e: &klipp_graph_format::lifted::LiftedError) -> Option<String> {
    match (op, e) {
        (GraphOp::AddNodeBeside { node, .. }, klipp_graph_format::lifted::LiftedError::Unwired { node: n, port }) if *n == node.id => {
            Some(format!("{} needs its {port} input: add it beside and connect it in one batch", node.id))
        }
        _ => None,
    }
}

fn apply_inner(docs: &mut Documents, op: &GraphOp, catalog: Option<&dyn PortCatalog>) -> Result<GraphEdit, String> {
    apply_pending(docs, op, catalog, &[])
}

fn apply_pending(docs: &mut Documents, op: &GraphOp, catalog: Option<&dyn PortCatalog>, pending: &[NodeId]) -> Result<GraphEdit, String> {
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
            let interface = if matches!(op, GraphOp::RemoveNode { .. } | GraphOp::MoveAfter { .. }) { containing_interface(&picture.graph, through)? } else { Vec::new() };
            // The clip's placement and opacity are its picture's, set
            // through its PLACEMENT node (M3 step 13); every other op edits
            // its graph.
            if crate::clip_properties::property_of(&picture, op).is_some() {
                crate::clip_properties::apply(&mut picture, op)?;
            } else {
                apply_graph(level(&mut picture.graph, through)?, op, last, catalog, &interface)?;
            }
            tolerate(if let Some(catalog) = catalog { picture.graph.check_lifted_with_catalog(false, catalog) } else { picture.graph.check_lifted(false) }, pending)
                .map_err(|e| unwired_beside(op, &e).unwrap_or_else(|| format!("that would leave the clip's graph invalid: {e}")))?;
            serde_json::to_value(picture).map_err(err)?
        }
        _ => {
            let mut body: Graph = serde_json::from_value(std::mem::take(&mut copy)).map_err(err)?;
            let interface = if matches!(op, GraphOp::RemoveNode { .. } | GraphOp::MoveAfter { .. }) { containing_interface(&body, through)? } else { Vec::new() };
            apply_graph(level(&mut body, through)?, op, last, catalog, &interface)?;
            tolerate(if let Some(catalog) = catalog { body.check_lifted_body_with_catalog(catalog) } else { body.check_lifted_body() }, pending)
                .map_err(|e| unwired_beside(op, &e).unwrap_or_else(|| format!("that would leave the grade invalid: {e}")))?;
            serde_json::to_value(body).map_err(err)?
        }
    };
    put(docs, root, &after)?;
    Ok(GraphEdit { root, before, after })
}

/// Why a batch's picture isn't valid when the batch ends: the reason,
/// and, when it is a node's input left unfed (a node added beside and
/// never connected), that node and port, so a caller can point at the op
/// that added it. Its text is the reason; it reads as a `String` refusal.
#[derive(Clone, Debug, PartialEq)]
pub struct BatchEndError {
    pub unwired: Option<(NodeId, PortKey)>,
    pub why: String,
}

impl std::fmt::Display for BatchEndError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

impl std::error::Error for BatchEndError {}

impl From<BatchEndError> for String {
    fn from(e: BatchEndError) -> String {
        e.why
    }
}

impl From<String> for BatchEndError {
    fn from(why: String) -> Self {
        BatchEndError { unwired: None, why }
    }
}

fn batch_end(e: klipp_graph_format::lifted::LiftedError, what: &str) -> BatchEndError {
    let unwired = match &e {
        klipp_graph_format::lifted::LiftedError::Unwired { node, port } => Some((*node, PortKey::from(port.as_str()))),
        _ => None,
    };
    BatchEndError { unwired, why: format!("the batch would leave {what} invalid: {e}") }
}

/// A root's picture checked whole, as a batch leaves it.
fn check_root(docs: &Documents, root: Root, catalog: Option<&dyn PortCatalog>) -> Result<(), BatchEndError> {
    let v = snapshot(docs, root)?;
    let err = |e: serde_json::Error| BatchEndError::from(e.to_string());
    match root {
        Root::Track { part: TrackPart::Effects, .. } => check_track_list(&serde_json::from_value::<Vec<Node>>(v).map_err(err)?).map_err(|e| BatchEndError::from(format!("that would leave the track's effects invalid: {e}"))),
        Root::Clip { .. } => {
            let picture: klipp_graph_format::lifted::ClipPicture = serde_json::from_value(v).map_err(err)?;
            (if let Some(catalog) = catalog { picture.graph.check_lifted_with_catalog(false, catalog) } else { picture.graph.check_lifted(false) }).map_err(|e| batch_end(e, "the clip's graph"))
        }
        _ => {
            let body: Graph = serde_json::from_value(v).map_err(err)?;
            (if let Some(catalog) = catalog { body.check_lifted_body_with_catalog(catalog) } else { body.check_lifted_body() }).map_err(|e| batch_end(e, "the grade"))
        }
    }
}

/// One op of a batch applied op by op, with registry checks: a node the
/// batch added beside so far (`pending`) may wait for its inputs, which
/// [`check_batch_end`] then checks whole. What `apply_batch_with_catalog`
/// does inside, for a caller that has to see each op's result (where an
/// op leaves the picture, the op a refusal stops at).
pub fn apply_in_batch(docs: &mut Documents, op: &GraphOp, catalog: &dyn PortCatalog, pending: &[NodeId]) -> Result<GraphEdit, String> {
    apply_pending(docs, op, Some(catalog), pending)
}

/// The end of a batch applied with [`apply_in_batch`]: the root checked
/// whole, nothing waiting.
pub fn check_batch_end(docs: &Documents, root: Root, catalog: &dyn PortCatalog) -> Result<(), BatchEndError> {
    check_root(docs, root, Some(catalog))
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
    // A node added beside may wait for its inputs until the batch ends;
    // then the root is checked whole, with nothing waiting.
    let mut pending = Vec::new();
    for op in ops {
        if let GraphOp::AddNodeBeside { node, .. } = op {
            pending.push(node.id);
        }
        apply_pending(&mut copy, op, catalog, &pending)?;
    }
    check_root(&copy, root, catalog)?;
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
