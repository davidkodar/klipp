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
        // A plug-in's stored values (picture revision 4) have no v1 grade
        // or effect field.
        ParamValue::Color { .. }
        | ParamValue::Matrix3(_)
        | ParamValue::Float64(_)
        | ParamValue::Point2F64(_)
        | ParamValue::Point3F64(_)
        | ParamValue::Int2(_)
        | ParamValue::Int3(_)
        | ParamValue::Choice(_)
        | ParamValue::ColorF64 { .. } => return None,
    })
}

/// A grade node's parameters with `key` set as the grade sets it: a
/// number `NodeParams::apply_override` takes, the way it takes it (a
/// neutral value unset, a keyed struct field into its struct); a struct
/// field named `struct.field` into its struct, made at its default when
/// unset; a structure or list field (`masks`) whole, from its JSON as
/// text. `None` for a key none of these names, or parameters that don't
/// read.
fn grade_param(params: &Value, key: &str, value: &ParamValue) -> Option<Value> {
    let before: klipp_nodes::NodeParams = serde_json::from_value(params.clone()).ok()?;
    let mut typed = before.clone();
    let number = match value {
        ParamValue::Float(f) => Some(*f),
        ParamValue::Int(i) => Some(*i as f32),
        ParamValue::Enum(e) => Some(*e as f32),
        ParamValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    };
    if number.is_some_and(|n| typed.apply_override(key, n)) {
        return written(params, &before, &typed);
    }
    // A difference key's clean frame (#2758), the source seconds of a frame
    // of the clip's own: unset until picked, so not a saved field to write
    // into; its shortest decimal, as a save writes it.
    if let ("difference_key.plate_frame", Some(secs)) = (key, number) {
        typed.difference_key.get_or_insert_with(Default::default).plate_frame = Some(secs.to_string().parse().ok()?);
        return written(params, &before, &typed);
    }
    // A setting that is a structure or a list (a mask stack, a key's
    // settings), written whole as its JSON (#1746): what a node beside a
    // picture's chain, outside its grade, is edited by. A plain string
    // setting (a LUT's file) is left to the field write below.
    if let (ParamValue::Text(json), false) = (value, key.contains('.')) {
        let structured = |v: &Value| matches!(v, Value::Object(_) | Value::Array(_));
        let mut whole = serde_json::to_value(&typed).ok()?;
        let held = whole.get(key).cloned();
        if let Some(new) = serde_json::from_str::<Value>(json).ok().filter(structured) {
            if held.as_ref().is_none_or(|h| structured(h) || h.is_null()) {
                whole.as_object_mut()?.insert(key.to_owned(), new);
                let after: klipp_nodes::NodeParams = serde_json::from_value(whole).ok()?;
                // Unset, the key must name a field, not be dropped as unknown.
                serde_json::to_value(&after).ok()?.get(key).is_some().then_some(())?;
                return written(params, &before, &after);
            }
        }
    }
    let (name, field) = key.split_once('.')?;
    let unset = serde_json::to_value(&typed).ok()?.get(name).is_none_or(Value::is_null);
    if unset {
        match name {
            "qualifier" => typed.qualifier = Some(Default::default()),
            "luma_key" => typed.luma_key = Some(Default::default()),
            "difference_key" => typed.difference_key = Some(Default::default()),
            "soft_clip" => typed.soft_clip = Some(Default::default()),
            "grain" => typed.grain = Some(Default::default()),
            "color_match" => typed.color_match = Some(Default::default()),
            _ => return None,
        }
    }
    let mut out = written(params, &before, &typed)?;
    let held = out.get_mut(name)?.as_object_mut()?;
    held.contains_key(field).then_some(())?;
    held.insert(field.to_owned(), as_v1(value)?);
    Some(out)
}

/// `params` with the fields that differ between `before` and `after` (the
/// typed parameters an edit started from and made) written as the file
/// writes them: a changed field's numbers in their shortest decimal (what
/// a save writes, `0.065` and not the f32's widened `0.0649999976`), an
/// unset one taken out, and every other field kept byte for byte.
fn written(params: &Value, before: &klipp_nodes::NodeParams, after: &klipp_nodes::NodeParams) -> Option<Value> {
    let (old, new) = (serde_json::to_value(before).ok()?, serde_json::to_value(after).ok()?);
    let (old, new) = (old.as_object()?, new.as_object()?);
    let mut out = params.as_object()?.clone();
    for key in old.keys().chain(new.keys()) {
        if old.get(key) == new.get(key) {
            continue;
        }
        match new.get(key).filter(|v| !v.is_null()) {
            Some(v) => out.insert(key.clone(), shortest(v)),
            None => out.remove(key),
        };
    }
    Some(Value::Object(out))
}

/// `v` with every number that isn't whole written as an f32's shortest
/// decimal, as [`float`] writes one.
fn shortest(v: &Value) -> Value {
    match v {
        Value::Number(n) if !(n.is_i64() || n.is_u64()) => n.as_f64().map_or(Value::Null, |f| float(f as f32)),
        Value::Array(a) => Value::Array(a.iter().map(shortest).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), shortest(v))).collect()),
        other => other.clone(),
    }
}

/// Set a parameter on a node: a legacy node's v1 entry, which must still
/// read (a grade node's as the frozen v1 types read it, an effect's as
/// [`effect_entry_reads`] does); any other node's own parameters.
fn set_param(n: &mut Node, key: &ParamKey, value: &ParamValue) -> Result<(), String> {
    let t = n.type_id.as_str().to_owned();
    // An effect's values are its map (M3-2): the value is written there,
    // and the entry the map stores must still read.
    if t == types::LEGACY_EFFECT && crate::effect_params::tag(n).is_some() && n.legacy.as_ref().and_then(|l| l.payload.as_object()).is_some_and(|o| o.len() == 1) {
        if matches!(key.as_str(), "kind" | "type") {
            return Err(format!("{key} names what the node is, and isn't set"));
        }
        // An effect's colour is its sRGB bytes, as the entry stores it
        // (#2758).
        let v = match value {
            ParamValue::Color { rgb, space } if space == crate::effect_params::SRGB => {
                serde_json::json!(rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8))
            }
            _ => as_v1(value).ok_or_else(|| format!("{key} is set through the effect's own editor until effects become nodes"))?,
        };
        if !crate::effect_params::is_field_key(key) {
            return Err(format!("{key} is the node's own, not a setting of its effect"));
        }
        let tag = crate::effect_params::tag(n).unwrap_or_default().to_owned();
        // A value named by a keyframe alias (a ChromaKey's `spill`) is the
        // field it stands for (`soft_edge`): set under the alias, it would
        // be a key no field reads, and the value would be lost.
        let key = &ParamKey::from(crate::names::anim_key(&tag, key.as_str()));
        let mut params = n.params.clone();
        params.insert(key.clone(), crate::effect_params::held(&tag, key, &v));
        let entry = crate::effect_params::guard_params(&tag, &mut params)?;
        if !effect_entry_reads(&entry) {
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
    match grade_node.then(|| grade_param(target, key.as_str(), value)).flatten() {
        Some(written) => *target = written,
        None => {
            target.as_object_mut().ok_or_else(|| format!("the node's stored entry isn't an object, so {key} has nowhere to go"))?.insert(key.as_str().to_owned(), v);
        }
    }
    let reads = if grade_node {
        serde_json::from_value::<crate::v1::params::NodeParams>(entry["params"].clone()).is_ok()
    } else if t == types::LEGACY_OFX {
        true
    } else {
        effect_entry_reads(&entry)
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

/// A key edit on a grade node: a grade node's keys are its v1 keyframe
/// tracks, held on its record (not its `anim`, which the record never
/// reads), changed as the grade changes them: a key is put in at its
/// time (`NodeKeyframeTrack::set_keyframe`, keeping the interpolation of
/// a key already there, a new one taking its own) or taken out at its time; an animation set whole
/// replaces the track's keys.
fn apply_grade_anim(n: &mut Node, op: &GraphOp) -> Result<(), String> {
    let legacy = n.legacy.as_mut().ok_or("a grade node without its record")?;
    let mut tracks: klipp_nodes::NodeKeyframeTracks = match &legacy.keyframes {
        Some(k) => serde_json::from_value(k.clone()).map_err(|e| format!("the node's keyframes don't read: {e}"))?,
        None => Default::default(),
    };
    // The keys as the grade's own type, read through its track.
    let read = |key: &ParamKey, keys: &[Value]| -> Result<klipp_nodes::NodeKeyframeTrack, String> {
        serde_json::from_value(serde_json::json!({ "param_name": key.as_str(), "keyframes": keys })).map_err(|e| format!("a key for {key} doesn't read: {e}"))
    };
    match op {
        GraphOp::AddKeyAt { key, keyframe, .. } => {
            for k in read(key, std::slice::from_ref(keyframe))?.keyframes {
                let track = tracks.track_mut(key.as_str());
                let before = track.keyframes.len();
                track.set_keyframe(k.time_secs, k.value);
                // A new key comes in as it was made (a switch's step,
                // #1318); one already there keeps its own interpolation.
                if track.keyframes.len() > before {
                    if let Some(new) = track.keyframes.iter_mut().min_by(|a, b| (a.time_secs - k.time_secs).abs().total_cmp(&(b.time_secs - k.time_secs).abs())) {
                        new.interpolation = k.interpolation;
                        new.tangent_in = k.tangent_in;
                        new.tangent_out = k.tangent_out;
                    }
                }
            }
        }
        GraphOp::RemoveKeyAt { key, time_secs, .. } => {
            if !tracks.track_mut(key.as_str()).remove_keyframe(*time_secs) {
                return Err(format!("{key} has no key at {time_secs} s"));
            }
        }
        GraphOp::SetAnim { key, anim, .. } => {
            let keys = match anim {
                Some(a) if a.modifiers.is_some() => return Err(format!("a grade parameter's behaviours aren't kept, so {key} takes none")),
                Some(a) => read(key, &a.keys)?.keyframes,
                None => Vec::new(),
            };
            tracks.track_mut(key.as_str()).keyframes = keys;
        }
        _ => return Err("not a key edit".into()),
    }
    legacy.keyframes = Some(serde_json::to_value(&tracks).map_err(|e| format!("the node's keyframes: {e}"))?);
    Ok(())
}

/// The animation edits, the same on a graph and on a track's list. A key
/// named by an effect's keyframe alias (a ChromaKey's `spill`) edits the
/// animation it is stored under (`soft_edge`, [`crate::names::anim_key`]):
/// never a second one beside it, whose write would replace the first.
fn apply_anim(n: &mut Node, op: &GraphOp) -> Result<(), String> {
    let tag = crate::effect_params::tag(n).unwrap_or("").to_owned();
    let stored = |key: &ParamKey| ParamKey::from(crate::names::anim_key(&tag, key.as_str()));
    match op {
        GraphOp::AddKeyAt { key, keyframe, .. } => {
            let key = &stored(key);
            let t = key_time(keyframe)?;
            let mut keyframe = keyframe.clone();
            guard_key(&tag, key, &mut keyframe)?;
            let a = n.anim.entry(key.clone()).or_insert(Animation { keys: Vec::new(), modifiers: None });
            a.keys.retain(|k| key_time(k).map_or(true, |kt| (kt - t).abs() > SAME_TIME));
            let at = a.keys.iter().position(|k| key_time(k).is_ok_and(|kt| kt > t)).unwrap_or(a.keys.len());
            a.keys.insert(at, keyframe);
        }
        GraphOp::RemoveKeyAt { key, time_secs, .. } => {
            let key = &stored(key);
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
        GraphOp::SetAnim { key, anim, .. } => match (&stored(key), anim) {
            (key, Some(a)) => {
                let mut a = a.clone();
                for k in &mut a.keys {
                    key_time(k)?;
                    guard_key(&tag, key, k)?;
                }
                n.anim.insert(key.clone(), a);
            }
            (key, None) => {
                n.anim.remove(key);
            }
        },
        _ => unreachable!("only the animation ops come here"),
    }
    Ok(())
}

/// A key's value held inside its parameter's safe range (#2995), as
/// [`guard_params`] holds a value.
fn guard_key(tag: &str, key: &ParamKey, keyframe: &mut Value) -> Result<(), String> {
    let Some(v) = keyframe.get("value").and_then(Value::as_f64) else { return Ok(()) };
    let (now, guarded) = klipp_effects::registry::guard_number(tag, key.as_str(), v, klipp_effects::registry::NotFinite::Refuse)?;
    if let Some(g) = guarded {
        tracing::info!("{tag}'s {} key held at {} (asked {})", g.key, g.now, g.was);
        keyframe["value"] = serde_json::json!(now);
    }
    Ok(())
}

/// Whether an effect's entry reads as an effect: as the frozen v1 types
/// read it, or as today's do, which also know the types made since (a 3D
/// LUT, a colour-space transform, #1746).
fn effect_entry_reads(entry: &Value) -> bool {
    serde_json::from_value::<crate::v1::effects::BuiltinEffect>(entry.clone()).is_ok() || serde_json::from_value::<klipp_effects::BuiltinEffect>(entry.clone()).is_ok()
}

/// Replace a legacy entry's payload: the same kind (`kind` for a grade
/// node, `type` for an effect), reading as the frozen v1 types read it
/// (an effect's as [`effect_entry_reads`] does).
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
        effect_entry_reads(payload)
    };
    if !reads {
        return Err("that payload doesn't read as the entry it replaces".into());
    }
    // An effect held in memory takes the payload's values into its map.
    if t == types::LEGACY_EFFECT && legacy.payload.as_object().is_some_and(|o| o.len() == 1) && payload.as_object().is_some_and(|o| o.len() > 1) {
        let (tag, mut values) = crate::effect_params::to_params(payload)?;
        crate::effect_params::guard_params(&tag, &mut values)?;
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
    if let Some(tag) = crate::effect_params::tag(&n).filter(|_| crate::effect_params::in_memory(&n)).map(str::to_owned) {
        crate::effect_params::guard_params(&tag, &mut n.params)?;
    }
    Ok(n)
}

/// The canvas's pitch between nodes along a chain, and between rows.
const STEP_X: f32 = 230.0;
const STEP_Y: f32 = 140.0;
/// The largest card the canvas draws (every node's is 120 by 60; a
/// reroute's dot is smaller), and how close two node centres may come,
/// across and down, before their cards overlap: a card plus a margin.
const CARD_W: f32 = 120.0;
const CARD_H: f32 = 60.0;
const CLEAR_X: f32 = CARD_W + 20.0;
const CLEAR_Y: f32 = CARD_H + 20.0;
/// How many rows down a spot is looked for before the last is taken.
const MAX_ROWS: usize = 64;

/// Where node `id` sits on the canvas, if anywhere.
fn spot_of(g: &Graph, id: NodeId) -> Option<(f32, f32)> {
    let v = g.ui.positions.get(&id)?;
    Some((v.get("x")?.as_f64()? as f32, v.get("y")?.as_f64()? as f32))
}

/// Node `id` at `(x, y)`: its place's `x` and `y` set, whatever else the
/// place keeps left as it is.
fn put_spot(g: &mut Graph, id: NodeId, (x, y): (f32, f32)) {
    let at = g.ui.positions.entry(id).or_insert_with(|| serde_json::json!({}));
    if !at.is_object() {
        *at = serde_json::json!({});
    }
    at["x"] = serde_json::json!(x);
    at["y"] = serde_json::json!(y);
}

/// `(x, y)`, or the first spot a row at a time below it that no node
/// but `id` covers. The look down is capped at `MAX_ROWS` rows, so a
/// crowded column costs a bounded scan; past it, the spot is a row below
/// the lowest node in the graph, which nothing covers.
fn clear_spot(g: &Graph, id: NodeId, (x, y): (f32, f32)) -> (f32, f32) {
    let others = || g.nodes.iter().filter(|n| n.id != id).filter_map(|n| spot_of(g, n.id));
    let taken = |y: f32| others().any(|(nx, ny)| (nx - x).abs() < CLEAR_X && (ny - y).abs() < CLEAR_Y);
    (0..MAX_ROWS)
        .map(|row| y + row as f32 * STEP_Y)
        .find(|y| !taken(*y))
        .map_or_else(|| (x, others().map(|(_, ny)| ny).fold(y, f32::max) + STEP_Y), |y| (x, y))
}

/// A node added after `target` (#1905): on its row, a step right of it.
/// When the node `target` fed is two steps or more away, the new one goes
/// midway and nothing moves; else every node right of `target` (side
/// branches, masks and keys too) moves a step right, so no wire is left
/// stretched back over the new node, and so does every sticky note right
/// of it. A frame keeps no place of its own (its box is drawn round its
/// members), so it follows its nodes. Nothing is placed in a graph whose
/// `target` has no place (one built without a layout keeps none), and a
/// place the node already has is kept; a later `SetNodeMeta` places it
/// as asked. In node order, so the same edit always lands the same way.
fn place_after(g: &mut Graph, id: NodeId, target: NodeId, fed: &[NodeId]) {
    if g.ui.positions.contains_key(&id) {
        return;
    }
    let Some((tx, ty)) = spot_of(g, target) else { return };
    let first = fed.iter().filter_map(|n| spot_of(g, *n)).map(|(x, _)| x).fold(None, |m: Option<f32>, x| Some(m.map_or(x, |m| m.min(x))));
    let spot = match first {
        Some(dx) if dx - tx >= 2.0 * STEP_X => ((tx + dx) / 2.0, ty),
        _ => {
            let right: Vec<(NodeId, (f32, f32))> = g.nodes.iter().filter(|n| n.id != id && n.id != target).filter_map(|n| spot_of(g, n.id).filter(|(x, _)| *x > tx + 1.0).map(|p| (n.id, p))).collect();
            for (n, (x, y)) in right {
                put_spot(g, n, (x + STEP_X, y));
            }
            for note in &mut g.ui.sticky_notes {
                let x = note.pointer("/pos/x").and_then(Value::as_f64);
                if let Some(x) = x.filter(|x| *x as f32 > tx + 1.0) {
                    note["pos"]["x"] = serde_json::json!(x as f32 + STEP_X);
                }
            }
            (tx + STEP_X, ty)
        }
    };
    let spot = clear_spot(g, id, spot);
    put_spot(g, id, spot);
}

/// A node added beside `target` (#1905): a row below it, or further down
/// past any node there. Placed only when `target` has a place.
fn place_beside(g: &mut Graph, id: NodeId, target: NodeId) {
    if g.ui.positions.contains_key(&id) {
        return;
    }
    let Some((tx, ty)) = spot_of(g, target) else { return };
    let spot = clear_spot(g, id, (tx, ty + STEP_Y));
    put_spot(g, id, spot);
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
            let fed: Vec<NodeId> = g.edges.iter().filter(|e| e.from == (target, PortKey::from("out"))).map(|e| e.to.0).collect();
            for e in g.edges.iter_mut().filter(|e| e.from == (target, PortKey::from("out"))) {
                e.from = (node.id, "out".into());
            }
            g.edges.push(Edge { from: (target, "out".into()), to: (node.id, primary(node)) });
            g.nodes.insert(at + 1, held_node(node)?);
            place_after(g, node.id, target, &fed);
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
            place_beside(g, node.id, target);
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
                    // A new effect mask reads luminance. A grade gate's
                    // reads red, as every gate always has: its channel
                    // lives only in the picture (M3 step 17a), so writing
                    // one on every new secondary would make each clip
                    // with one picture-truth; it is set when picked.
                    let effect = g.nodes[at].type_id.as_str() != types::LEGACY_NODE;
                    if effect && !connected && !g.nodes[at].params.contains_key(&ParamKey::from("mask.channel")) && mask.params.iter().any(|(key, _)| key == "channel") {
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
        GraphOp::SetAnnotations { frames, sticky_notes, .. } => {
            find(g, target)?;
            g.ui.frames = frames.clone();
            g.ui.sticky_notes = sticky_notes.clone();
        }
        GraphOp::AddKeyAt { .. } | GraphOp::RemoveKeyAt { .. } | GraphOp::SetAnim { .. } => {
            let at = find(g, target)?;
            if matches!(g.nodes[at].type_id.as_str(), types::LEGACY_NODE | lt::MERGE) {
                apply_grade_anim(&mut g.nodes[at], op)?;
            } else {
                apply_anim(&mut g.nodes[at], op)?;
            }
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
        GraphOp::Group { nodes, group, .. } => crate::structure::group(g, nodes, *group)?,
        GraphOp::Ungroup { .. } => crate::structure::ungroup(g, target)?,
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
        GraphOp::Group { .. } | GraphOp::Ungroup { .. } => return Err("a track's effects are a list, with no compound".into()),
        GraphOp::SetAnnotations { .. } => return Err("a track's effects are a list, with no canvas to annotate".into()),
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
    apply_pending(docs, op, catalog, &[], true)
}

/// `domains`: check the op's colour domains (#1290) on its own. A batch's
/// ops aren't: an insert between two nodes rewires through a state that
/// mixes them, so the batch is checked whole at its end.
fn apply_pending(docs: &mut Documents, op: &GraphOp, catalog: Option<&dyn PortCatalog>, pending: &[NodeId], domains: bool) -> Result<GraphEdit, String> {
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
            // A picture-truth clip's grade holds no compound (picture
            // revision 4 refuses one).
            if matches!(op, GraphOp::Group { .. }) && picture.is_truth() {
                return Err("this clip's picture keeps settings its fields can't, so its grade takes no compound".into());
            }
            let interface = if matches!(op, GraphOp::RemoveNode { .. } | GraphOp::MoveAfter { .. }) { containing_interface(&picture.graph, through)? } else { Vec::new() };
            // The clip's placement and opacity are its picture's, set
            // through its PLACEMENT node (M3 step 13); every other op edits
            // its graph.
            if crate::clip_properties::property_of(&picture, op).is_some() {
                crate::clip_properties::apply(&mut picture, op)?;
            } else {
                apply_graph(level(&mut picture.graph, through)?, op, last, catalog, &interface)?;
            }
            // A grade correction's mix or mask channel needs a reader that
            // knows them (M3 step 17a); the mark comes and goes with them.
            picture.mark_grade_mix();
            tolerate(if let Some(catalog) = catalog { picture.graph.check_lifted_with_catalog(false, catalog) } else { picture.graph.check_lifted(false) }, pending)
                .map_err(|e| unwired_beside(op, &e).unwrap_or_else(|| format!("that would leave the clip's graph invalid: {e}")))?;
            // Every node still able to take the pictures it reads (#1290).
            if let (true, Some(catalog)) = (domains, catalog) {
                let was: klipp_graph_format::lifted::ClipPicture = serde_json::from_value(before.clone()).map_err(err)?;
                crate::domains::check_edit(&was.graph, &picture.graph, crate::domains::source_of(root), catalog)?;
            }
            serde_json::to_value(picture).map_err(err)?
        }
        _ => {
            // A clip's picture keeps a correction's mix or mask channel
            // (M3 step 17a), and so do the timeline grade and a grade
            // group's stages (#1321); a track's grade has no place for
            // them yet.
            if matches!(op, GraphOp::SetMix { .. } | GraphOp::SetMaskChannel { .. }) && !matches!(root, Root::TimelineGrade { .. } | Root::GroupStage { .. }) {
                return Err("a track's grade doesn't keep a correction's mix or mask channel".into());
            }
            let mut body: Graph = serde_json::from_value(std::mem::take(&mut copy)).map_err(err)?;
            let interface = if matches!(op, GraphOp::RemoveNode { .. } | GraphOp::MoveAfter { .. }) { containing_interface(&body, through)? } else { Vec::new() };
            apply_graph(level(&mut body, through)?, op, last, catalog, &interface)?;
            tolerate(if let Some(catalog) = catalog { body.check_lifted_body_with_catalog(catalog) } else { body.check_lifted_body() }, pending)
                .map_err(|e| unwired_beside(op, &e).unwrap_or_else(|| format!("that would leave the grade invalid: {e}")))?;
            if let (true, Some(catalog)) = (domains, catalog) {
                let was: Graph = serde_json::from_value(before.clone()).map_err(err)?;
                crate::domains::check_edit(&was, &body, crate::domains::source_of(root), catalog)?;
            }
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

/// A root's picture checked whole, as a batch leaves it: its structure,
/// and its colour domains against the picture the batch started from
/// (#1290).
fn check_root(docs: &Documents, start: &Documents, root: Root, catalog: Option<&dyn PortCatalog>) -> Result<(), BatchEndError> {
    let v = snapshot(docs, root)?;
    let err = |e: serde_json::Error| BatchEndError::from(e.to_string());
    let graph_of = |v: Value| -> Result<Option<Graph>, BatchEndError> {
        Ok(match root {
            Root::Track { part: TrackPart::Effects, .. } => None,
            Root::Clip { .. } => Some(serde_json::from_value::<klipp_graph_format::lifted::ClipPicture>(v).map_err(err)?.graph),
            _ => Some(serde_json::from_value::<Graph>(v).map_err(err)?),
        })
    };
    let now = graph_of(v.clone())?;
    let structure = match root {
        Root::Track { part: TrackPart::Effects, .. } => check_track_list(&serde_json::from_value::<Vec<Node>>(v).map_err(err)?).map_err(|e| BatchEndError::from(format!("that would leave the track's effects invalid: {e}"))),
        Root::Clip { .. } => {
            let picture: klipp_graph_format::lifted::ClipPicture = serde_json::from_value(v).map_err(err)?;
            (if let Some(catalog) = catalog { picture.graph.check_lifted_with_catalog(false, catalog) } else { picture.graph.check_lifted(false) }).map_err(|e| batch_end(e, "the clip's graph"))
        }
        _ => {
            let body: Graph = serde_json::from_value(v).map_err(err)?;
            (if let Some(catalog) = catalog { body.check_lifted_body_with_catalog(catalog) } else { body.check_lifted_body() }).map_err(|e| batch_end(e, "the grade"))
        }
    };
    structure?;
    if let (Some(was), Some(now), Some(catalog)) = (graph_of(snapshot(start, root)?)?, now, catalog) {
        crate::domains::check_edit(&was, &now, crate::domains::source_of(root), catalog).map_err(BatchEndError::from)?;
    }
    Ok(())
}

/// One op of a batch applied op by op, with registry checks: a node the
/// batch added beside so far (`pending`) may wait for its inputs, which
/// [`check_batch_end`] then checks whole. What `apply_batch_with_catalog`
/// does inside, for a caller that has to see each op's result (where an
/// op leaves the picture, the op a refusal stops at).
pub fn apply_in_batch(docs: &mut Documents, op: &GraphOp, catalog: &dyn PortCatalog, pending: &[NodeId]) -> Result<GraphEdit, String> {
    apply_pending(docs, op, Some(catalog), pending, false)
}

/// The end of a batch applied with [`apply_in_batch`]: the root checked
/// whole, nothing waiting, and its colour domains against `start`, the
/// documents the batch began from (#1290).
pub fn check_batch_end(docs: &Documents, start: &Documents, root: Root, catalog: &dyn PortCatalog) -> Result<(), BatchEndError> {
    check_root(docs, start, root, Some(catalog))
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
        apply_pending(&mut copy, op, catalog, &pending, false)?;
    }
    check_root(&copy, docs, root, catalog)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_effects::registry::ParamKind;
    use klipp_nodes::NodeParams;

    /// A difference key's clean frame (#2758) is written into its settings,
    /// made when unset, as its shortest decimal; the rest kept.
    #[test]
    fn a_clean_frame_is_written_into_the_key() {
        let unset = serde_json::json!({});
        let got = grade_param(&unset, "difference_key.plate_frame", &ParamValue::Float(2.4)).expect("written");
        let typed: NodeParams = serde_json::from_value(got.clone()).unwrap();
        assert_eq!(typed.difference_key.as_ref().and_then(|d| d.plate_frame), Some(2.4));
        assert_eq!(got["difference_key"]["plate_frame"], serde_json::json!(2.4));
        let keyed = serde_json::json!({ "difference_key": { "enabled": true, "tolerance": 0.2, "soft": 0.02, "invert": true } });
        let got: NodeParams = serde_json::from_value(grade_param(&keyed, "difference_key.plate_frame", &ParamValue::Float(0.5)).unwrap()).unwrap();
        let dk = got.difference_key.unwrap();
        assert_eq!((dk.plate_frame, dk.tolerance, dk.invert), (Some(0.5), 0.2, true));
    }

    /// Every grade choice in the catalog, at every index, writes through
    /// the generated row's path (`grade_param`, a choice by its index) the
    /// typed parameters the old panel's `SetNodeParam` gave
    /// (`apply_override`), and each index a different one: a choice the
    /// lift can't write, or one written as another, fails here.
    #[test]
    fn every_grade_choice_index_writes_as_set_node_param() {
        let mut checked = 0;
        for ty in klipp_nodes::registry::node_types() {
            for desc in ty.desc.params {
                let ParamKind::Choice(names) = desc.kind else { continue };
                let mut seen = Vec::new();
                for i in 0..names.len() {
                    let params = serde_json::to_value(NodeParams::default()).unwrap();
                    let written = grade_param(&params, desc.key, &ParamValue::Int(i as i32)).unwrap_or_else(|| panic!("{} {} [{i}]: not written", ty.desc.id, desc.key));
                    let new: NodeParams = serde_json::from_value(written).unwrap();
                    let mut old = NodeParams::default();
                    assert!(old.apply_override(desc.key, i as f32), "{} {}: no SetNodeParam", ty.desc.id, desc.key);
                    assert_eq!(new, old, "{} {} [{i}] {:?}", ty.desc.id, desc.key, names[i]);
                    assert!(!seen.contains(&new), "{} {} [{i}] {:?} writes what another item does", ty.desc.id, desc.key, names[i]);
                    seen.push(new);
                    checked += 1;
                }
            }
        }
        assert!(checked > 20, "only {checked} choice items");
    }
}

#[cfg(test)]
mod whole_struct_tests {
    use super::*;

    /// A grade node's mask stack, unset before, written whole by path from
    /// its JSON (#1746); a key's settings replaced whole; a plain string
    /// setting, or a name that is no field, is not taken this way.
    #[test]
    fn a_struct_setting_is_written_whole_from_its_json() {
        let mut want = klipp_nodes::NodeParams { masks: Some(Default::default()), ..Default::default() };
        want.masks.as_mut().unwrap().masks.push(Default::default());
        let json = serde_json::to_string(&want.masks).unwrap();
        let out = grade_param(&serde_json::json!({}), "masks", &ParamValue::Text(json)).expect("taken");
        let back: klipp_nodes::NodeParams = serde_json::from_value(out.clone()).unwrap();
        assert_eq!(back.masks, want.masks);
        want.luma_key = Some(Default::default());
        want.luma_key.as_mut().unwrap().low = 0.25;
        let out = grade_param(&out, "luma_key", &ParamValue::Text(serde_json::to_string(&want.luma_key).unwrap())).expect("taken");
        let back: klipp_nodes::NodeParams = serde_json::from_value(out).unwrap();
        assert_eq!(back.luma_key, want.luma_key);
        assert!(back.masks.is_some(), "the other settings kept");
        assert_eq!(grade_param(&serde_json::json!({}), "not_a_field", &ParamValue::Text("{}".into())), None);
        assert_eq!(grade_param(&serde_json::json!({}), "masks", &ParamValue::Text("\"a string\"".into())), None);
    }
}

#[cfg(test)]
mod placement_tests {
    use super::*;

    fn node(ty: &str) -> Node {
        serde_json::from_value(serde_json::json!({"id": uuid::Uuid::new_v4(), "type_id": ty})).unwrap()
    }

    /// Source, a corrector and Output in a row at 0, 230 and 460.
    fn graph() -> (Graph, [NodeId; 3]) {
        let ids = [node("klipp.source"), node("klipp.grade.corrector"), node("klipp.output")];
        let id: [NodeId; 3] = [ids[0].id, ids[1].id, ids[2].id];
        let mut g: Graph = serde_json::from_value(serde_json::json!({"graph_version": klipp_graph_format::GRAPH_VERSION, "nodes": [], "edges": [], "output": id[2]})).unwrap();
        g.nodes.extend(ids);
        g.edges.push(Edge { from: (id[0], "out".into()), to: (id[1], "in".into()) });
        g.edges.push(Edge { from: (id[1], "out".into()), to: (id[2], "in".into()) });
        for (i, n) in id.iter().enumerate() {
            put_spot(&mut g, *n, (i as f32 * STEP_X, 0.0));
        }
        (g, id)
    }

    fn no_overlap(g: &Graph) {
        let spots: Vec<_> = g.nodes.iter().map(|n| (n.id, spot_of(g, n.id).unwrap_or_else(|| panic!("{} unplaced", n.type_id.as_str())))).collect();
        for (i, (a, pa)) in spots.iter().enumerate() {
            for (b, pb) in &spots[i + 1..] {
                assert!((pa.0 - pb.0).abs() >= CLEAR_X || (pa.1 - pb.1).abs() >= CLEAR_Y, "{a} at {pa:?} covers {b} at {pb:?}");
            }
        }
    }

    /// A corrector added after a corrector sits right of it, Output moved
    /// along, Source where it was, and no card on another.
    #[test]
    fn a_node_added_after_one_goes_right_of_it_and_moves_the_rest_along() {
        let (mut g, [s, c, o]) = graph();
        let new = node("klipp.grade.corrector");
        let nid = new.id;
        g.nodes.insert(2, new);
        g.edges.retain(|e| e.from.0 != c);
        g.edges.push(Edge { from: (c, "out".into()), to: (nid, "in".into()) });
        g.edges.push(Edge { from: (nid, "out".into()), to: (o, "in".into()) });
        place_after(&mut g, nid, c, &[o]);
        assert_eq!(spot_of(&g, nid), Some((2.0 * STEP_X, 0.0)));
        assert_eq!(spot_of(&g, o), Some((3.0 * STEP_X, 0.0)), "Output moved along");
        assert_eq!(spot_of(&g, s), Some((0.0, 0.0)), "Source stays");
        no_overlap(&g);
    }

    /// A side branch right of the corrector (a mask below Output) moves
    /// along with Output; a node at the corrector's column stays.
    #[test]
    fn side_branches_right_of_the_slot_move_along_too() {
        let (mut g, [_, c, o]) = graph();
        let (side, under) = (node("klipp.grade.mask"), node("klipp.grade.mask"));
        let (sid, uid) = (side.id, under.id);
        g.nodes.push(side);
        g.nodes.push(under);
        put_spot(&mut g, sid, (2.0 * STEP_X, STEP_Y));
        put_spot(&mut g, uid, (STEP_X, STEP_Y));
        let new = node("klipp.grade.corrector");
        let nid = new.id;
        g.nodes.push(new);
        place_after(&mut g, nid, c, &[o]);
        assert_eq!(spot_of(&g, sid), Some((3.0 * STEP_X, STEP_Y)), "the side branch moved along");
        assert_eq!(spot_of(&g, uid), Some((STEP_X, STEP_Y)), "the node under the corrector stays");
        no_overlap(&g);
    }

    /// A wire running back (what the corrector fed sits left of it) still
    /// takes the shift: the new node a step right, every node right of the
    /// corrector moved, nothing left of it, and a sticky note right of it
    /// moved, its other fields and a position's own extra fields kept.
    #[test]
    fn a_backward_wire_shifts_only_what_is_right_of_the_node() {
        let (mut g, [s, c, o]) = graph();
        put_spot(&mut g, o, (-300.0, 0.0));
        let right = node("klipp.grade.mask");
        let rid = right.id;
        g.nodes.push(right);
        g.ui.positions.insert(rid, serde_json::json!({"x": 500.0, "y": 0.0, "kept": true}));
        g.ui.sticky_notes.push(serde_json::json!({"id": uuid::Uuid::new_v4(), "pos": {"x": 600.0, "y": 50.0}, "text": "note"}));
        g.ui.sticky_notes.push(serde_json::json!({"id": uuid::Uuid::new_v4(), "pos": {"x": -50.0, "y": 50.0}, "text": "left"}));
        let new = node("klipp.grade.corrector");
        let nid = new.id;
        g.nodes.push(new);
        place_after(&mut g, nid, c, &[o]);
        assert_eq!(spot_of(&g, nid), Some((2.0 * STEP_X, 0.0)));
        assert_eq!(spot_of(&g, o), Some((-300.0, 0.0)), "left of the corrector: stays");
        assert_eq!(spot_of(&g, s), Some((0.0, 0.0)));
        assert_eq!(spot_of(&g, rid), Some((500.0 + STEP_X, 0.0)), "right of it: moved");
        assert_eq!(g.ui.positions[&rid]["kept"], serde_json::json!(true), "the place's other fields kept");
        assert_eq!(g.ui.sticky_notes[0]["pos"]["x"].as_f64(), Some((600.0 + STEP_X) as f64));
        assert_eq!(g.ui.sticky_notes[0]["text"], "note");
        assert_eq!(g.ui.sticky_notes[1]["pos"]["x"].as_f64(), Some(-50.0), "a note left of it stays");
        no_overlap(&g);
    }

    /// A place the new node already has is kept.
    #[test]
    fn a_place_given_with_the_node_is_kept() {
        let (mut g, [_, c, o]) = graph();
        let new = node("klipp.grade.corrector");
        let nid = new.id;
        g.nodes.push(new);
        put_spot(&mut g, nid, (999.0, 555.0));
        place_after(&mut g, nid, c, &[o]);
        place_beside(&mut g, nid, c);
        assert_eq!(spot_of(&g, nid), Some((999.0, 555.0)));
        assert_eq!(spot_of(&g, o), Some((2.0 * STEP_X, 0.0)), "nothing moved for it");
    }

    /// With room in the gap already, the node goes midway and nothing moves.
    #[test]
    fn a_node_added_into_a_wide_gap_goes_midway() {
        let (mut g, [_, c, o]) = graph();
        put_spot(&mut g, o, (800.0, 0.0));
        let new = node("klipp.grade.corrector");
        let nid = new.id;
        g.nodes.push(new);
        place_after(&mut g, nid, c, &[o]);
        assert_eq!(spot_of(&g, nid), Some(((STEP_X + 800.0) / 2.0, 0.0)));
        assert_eq!(spot_of(&g, o), Some((800.0, 0.0)));
    }

    /// Beside a node: a row below it; a second beside the same node goes
    /// a row further down.
    #[test]
    fn nodes_added_beside_one_go_below_it_each_clear_of_the_last() {
        let (mut g, [_, c, _]) = graph();
        let (a, b) = (node("klipp.grade.mask"), node("klipp.grade.mask"));
        let (aid, bid) = (a.id, b.id);
        g.nodes.push(a);
        place_beside(&mut g, aid, c);
        g.nodes.push(b);
        place_beside(&mut g, bid, c);
        assert_eq!(spot_of(&g, aid), Some((STEP_X, STEP_Y)));
        assert_eq!(spot_of(&g, bid), Some((STEP_X, 2.0 * STEP_Y)));
        no_overlap(&g);
    }

    /// With every row below taken, the node goes a row below the lowest.
    #[test]
    fn a_full_column_places_below_the_lowest_node() {
        let (mut g, [_, c, _]) = graph();
        let mut ids = Vec::new();
        for row in 1..=(MAX_ROWS as i32) {
            let n = node("klipp.grade.mask");
            ids.push(n.id);
            put_spot(&mut g, n.id, (STEP_X, row as f32 * STEP_Y));
            g.nodes.push(n);
        }
        let new = node("klipp.grade.mask");
        let nid = new.id;
        g.nodes.push(new);
        place_beside(&mut g, nid, c);
        assert_eq!(spot_of(&g, nid), Some((STEP_X, (MAX_ROWS as f32 + 1.0) * STEP_Y)));
        no_overlap(&g);
    }

    /// A graph with no layout gets none.
    #[test]
    fn a_graph_without_places_stays_without() {
        let (mut g, [_, c, o]) = graph();
        g.ui.positions.clear();
        let new = node("klipp.grade.corrector");
        let nid = new.id;
        g.nodes.push(new);
        place_after(&mut g, nid, c, &[o]);
        place_beside(&mut g, nid, c);
        assert!(g.ui.positions.is_empty());
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    /// A Gaussian blur node, its values held in its map.
    fn blur() -> Node {
        let n: Node = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(),
            "type_id": types::LEGACY_EFFECT,
            "legacy": {"payload": {"type": "gaussian_blur", "radius": 4.0}},
        }))
        .unwrap();
        held_node(&n).unwrap()
    }

    fn radius(n: &Node) -> f32 {
        match crate::effect_params::effect_of(n).unwrap() {
            klipp_effects::BuiltinEffect::GaussianBlur { radius } => radius,
            e => panic!("{e:?}"),
        }
    }

    /// A hostile value through every edit (#2995): a huge radius held at
    /// the top of its range, a NaN or an infinity refused and the old
    /// value kept, the same for a key and a payload, and a new node with
    /// one comes in held.
    #[test]
    fn an_edit_never_takes_an_effect_past_its_range() {
        let hi = klipp_effects::registry::effect_type("gaussian_blur").unwrap().desc.params[0].hard().unwrap().1;
        let mut n = blur();
        set_param(&mut n, &"radius".into(), &ParamValue::Float(1e30)).unwrap();
        assert_eq!(radius(&n), hi);
        set_param(&mut n, &"radius".into(), &ParamValue::Float(-5.0)).unwrap();
        assert_eq!(radius(&n), 0.0);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(set_param(&mut n, &"radius".into(), &ParamValue::Float(bad)).is_err(), "{bad} refused");
            assert_eq!(radius(&n), 0.0, "{bad}: the old value kept");
        }
        set_payload(&mut n, &serde_json::json!({"type": "gaussian_blur", "radius": 1e9})).unwrap();
        assert_eq!(radius(&n), hi);
        assert!(set_payload(&mut n, &serde_json::json!({"type": "gaussian_blur", "radius": null})).is_err());
        let key = GraphOp::AddKeyAt { path: NodePath::new(Root::Clip { clip: uuid::Uuid::nil() }, [n.id]), key: "radius".into(), keyframe: serde_json::json!({"time_secs": 0.0, "value": 1e12}) };
        apply_anim(&mut n, &key).unwrap();
        assert_eq!(n.anim[&ParamKey::from("radius")].keys[0]["value"].as_f64(), Some(hi as f64));
        let mut added: Node = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(),
            "type_id": types::LEGACY_EFFECT,
            "legacy": {"payload": {"type": "gaussian_blur", "radius": 1e20}},
        }))
        .unwrap();
        added = held_node(&added).unwrap();
        assert_eq!(radius(&added), hi);
    }
}
