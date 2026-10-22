//! The adapter (#1068): a lifted [`ClipPicture`] back to the v1 picture
//! fields today's renderer reads, so the lift can be held to today's
//! renderer: `render_inputs(adapter(lift(to_v2(x)))) == render_inputs(x)`.
//!
//! It is built from what the lift says the picture means, not from the
//! entries the lifted nodes carry for the record: the opacity and the
//! placement from the clip's properties, an effect's animation from its
//! `anim`, a gate from its `alpha_one` flag, the grade's merges from their
//! op, the head nodes from whether they are on. What the lift keeps only
//! for the record (orphan keys, parked edges, extra transforms, nodes that
//! never ran) it leaves out, so if any of those did drive something, the
//! property sees the difference.
//!
//! A spliced render copy (`crate::splice`) comes back flattened, as
//! today's bakes leave it: the stages and the track's grade folded into the
//! clip's grade as `prepend_keeping_ids` and `append_keeping_ids` fold
//! them, the track's effects after the clip's own.
//!
//! Lives only here and in the tests, as `to_v1` does: nothing writes a
//! file with it.

use std::collections::BTreeMap;

use klipp_graph_format::lifted::{types as lt, ClipPicture, PlacementProps};
use klipp_graph_format::{ports, types, Animation, Graph, Node, NodeId, ParamValue, PortKey};
use serde_json::{json, Map, Value};

use crate::lift::{ClipKind, LiftCtx};
use klipp_graph_lift::names::KEYFRAME_ALIASES;

/// Why a lifted picture is not one the lift makes.
pub type Refusal = String;

/// The corrections a matte on port 1 gates (`graph.rs`'s `gateable`).
const GATEABLE: [&str; 8] = ["Corrector", "ColorBalance", "Exposure", "Contrast", "Saturation", "ColorWheels", "ToneControls", "RGBCurves"];

/// A lifted clip picture as the v1 picture fields of a clip of `ctx.kind`.
pub fn clip(p: &ClipPicture, ctx: &LiftCtx) -> Result<Value, Refusal> {
    // A plug-in a file stores as its own node (picture revision 4) is read
    // as the entry it holds.
    let mut entries = p.clone();
    for n in &mut entries.graph.nodes {
        klipp_graph_lift::effect_params::plugin_entry(n)?;
    }
    let p = &entries;
    let mut out = Map::new();
    let mut ofx = Vec::new();
    let mut fx = Fx::default();
    let mut placed = false;
    let nodes = chain(&p.graph)?;
    // An Auto placement's values ride on a switched-off Transform, first in
    // the list: today the first Transform gives the clip its values, on or
    // off, and a track's Transform spliced after the clip's own effects
    // must not take its place.
    let auto = nodes.iter().any(|n| n.type_id.as_str() == lt::PLACEMENT && n.params.get(&klipp_graph_format::ParamKey::new("mode")) == Some(&ParamValue::Enum(lt::PLACEMENT_AUTO)));
    if auto && has_values(&p.placement) {
        fx.push(transform(&p.placement, false), false, &BTreeMap::new());
    }
    // Each other enabled Opacity, which multiplies the clip's opacity
    // (#1386), with its own keys; before the property's, so the property's
    // is still the last enabled one, which carries the clip's keys.
    let mut multiplied = false;
    for n in nodes.iter().filter(|n| n.origin.is_none() && n.enabled && n.type_id.as_str() == types::LEGACY_OPACITY) {
        fx.push(payload(n)?.clone(), true, &own_keys(n));
        multiplied = true;
    }
    // The opacity: one Opacity effect with the value, its animation on the
    // clip's own opacity keys. First too: the last enabled Opacity sets the
    // result, and a track's spliced after must still be that one. At 1 it
    // is still written after a multiplying one, so it stays the last.
    if p.opacity != 1.0 || multiplied {
        fx.push(json!({ "type": "opacity", "value": p.opacity }), true, &BTreeMap::new());
    }
    // The clip's own grade, and the spliced groups before and after it.
    let mut own: Option<Grade> = None;
    let (mut before, mut after) = (Vec::new(), Vec::new());
    for n in nodes {
        // What the splice put in from the track, as the bake appends it.
        if n.origin.is_some() && n.type_id.as_str() != lt::GROUP {
            fx.push(entry(n)?, n.enabled, &BTreeMap::new());
            continue;
        }
        match n.type_id.as_str() {
            lt::SOURCE | lt::SCENE_TO_DISPLAY | lt::OUTPUT => {}
            lt::TEMPORAL_DENOISE | lt::FRAME_BLEND if n.enabled && ctx.kind != ClipKind::Media => {
                // Today's renderer runs the head on a Media clip's source
                // only: the lift keeps it off on any other clip.
                return Err(format!("head node {} ({}) is on in a {:?} clip", n.id, n.type_id, ctx.kind));
            }
            lt::TEMPORAL_DENOISE => {
                // Off when it never ran: v1 runs the first enabled one.
                if n.enabled {
                    fx.push(payload(n)?.clone(), true, &n.anim);
                }
            }
            lt::FRAME_BLEND => {
                if n.enabled {
                    let mut ramp = payload(n)?.clone();
                    // A frame blend cross-fades (1) or follows the motion
                    // (2, #2215).
                    let mode = if ramp.get("blend_mode").and_then(Value::as_u64) == Some(2) { 2 } else { 1 };
                    ramp["blend_mode"] = json!(mode);
                    fx.push(ramp, true, &BTreeMap::new());
                }
            }
            lt::GROUP => {
                let g = grade(n)?;
                if n.origin.is_some() && !n.enabled {
                    return Err(format!("spliced grade {} is off: a standalone grade that loops stops the conversion", n.id));
                }
                match (n.origin, &own) {
                    (None, None) => own = Some(g),
                    (None, Some(_)) => return Err(format!("a second grade of the clip's own, {}", n.id)),
                    (Some(_), None) => before.push(g),
                    (Some(_), Some(_)) => after.push(g),
                }
            }
            types::LEGACY_OFX => ofx.push(payload(n)?.clone()),
            lt::PLACEMENT => {
                if placed {
                    return Err(format!("a second placement node, {}", n.id));
                }
                placed = true;
                match n.params.get(&klipp_graph_format::ParamKey::new("mode")) {
                    Some(ParamValue::Enum(lt::PLACEMENT_FIXED)) => fx.push(transform(&p.placement, true), true, &BTreeMap::new()),
                    // The renderer places an Auto placement itself (its
                    // values went first, above).
                    Some(ParamValue::Enum(lt::PLACEMENT_AUTO)) => {}
                    other => return Err(format!("placement {} has mode {other:?}", n.id)),
                }
            }
            // Kept off for the record: only the first Transform placed.
            types::LEGACY_TRANSFORM if !n.enabled => {}
            // Kept off for the record: every Opacity entry (the clip's
            // opacity property renders), a speed ramp that doesn't blend.
            types::LEGACY_OPACITY | types::LEGACY_SPEED_RAMP if !n.enabled => {}
            // A multiplying Opacity, written above.
            types::LEGACY_OPACITY => {}
            types::LEGACY_EFFECT => fx.push(entry(n)?, n.enabled, &n.anim),
            other => return Err(format!("node {} is a {other}, which the lift does not put in a clip's chain", n.id)),
        }
    }
    if !placed {
        return Err("a lifted clip without a placement node".into());
    }
    if let Some(Grade { graph: mut g, mut reads }) = own {
        // A stage before the clip's grade is prepended, every one after
        // appended, as the bakes do (`GradeGroup::wrap`, then the track's).
        for b in before.iter().rev() {
            prepend(&mut g, &b.graph)?;
            reads.extend(b.reads.iter().cloned());
        }
        for a in &after {
            append(&mut g, &a.graph)?;
            reads.extend(a.reads.iter().cloned());
        }
        // What each group read of the frame entering the grade, drawn last
        // from the footage, so no flattening step moves it: today an
        // unwired port reads the decoded source wherever its node sits.
        let source = first_of(&g, "Source").ok_or("frame reads in a grade with no Source")?.to_owned();
        let mut es = edges(&g).clone();
        es.extend(reads.iter().map(|(to, port)| edge(&(source.clone(), 0), to, *port)));
        g["edges"] = Value::Array(es);
        out.insert("node_graph".into(), g);
    } else if !before.is_empty() || !after.is_empty() {
        return Err("spliced grades on a clip with no grade of its own".into());
    }

    if let Some(a) = &p.opacity_anim {
        animate(&mut out, &mut fx.modifiers, "opacity_keyframes", Some("opacity"), a);
    }
    for (param, a) in &p.placement_anim {
        let (field, modifier) = match param.as_str() {
            "pos_x" => ("transform_pos_x_keyframes", Some("position_x")),
            "pos_y" => ("transform_pos_y_keyframes", Some("position_y")),
            "scale" => ("transform_scale_keyframes", Some("scale")),
            "scale_y" => ("transform_scale_y_keyframes", None),
            "rotation" => ("transform_rotation_keyframes", Some("rotation")),
            "tilt_x" => ("transform_tilt_x_keyframes", None),
            "tilt_y" => ("transform_tilt_y_keyframes", None),
            other => return Err(format!("placement animates {other}, which no v1 field holds")),
        };
        if modifier.is_none() && a.modifiers.is_some() {
            return Err(format!("placement's {param} has modifiers, which v1 never applied"));
        }
        animate(&mut out, &mut fx.modifiers, field, modifier, a);
    }

    if !ofx.is_empty() {
        out.insert("effects".into(), Value::Array(ofx));
    }
    if !fx.effects.is_empty() {
        out.insert("builtin_effects".into(), Value::Array(fx.effects));
        out.insert("builtin_effects_enabled".into(), Value::Array(fx.flags));
    }
    if !fx.keyframes.is_empty() {
        out.insert("effect_keyframes".into(), Value::Object(fx.keyframes));
    }
    if !fx.modifiers.is_empty() {
        out.insert("param_modifiers".into(), Value::Object(fx.modifiers));
    }
    Ok(Value::Object(out))
}

/// An entry's own keys and modifiers on its `value`, as the lift keeps
/// them on a multiplying Opacity (#1386): in its old entry, by slot.
fn own_keys(n: &Node) -> BTreeMap<klipp_graph_format::ParamKey, Animation> {
    let Some(l) = n.legacy.as_ref() else { return BTreeMap::new() };
    let slot = l.slot.unwrap_or(0);
    let keys = l.keyframes.as_ref().and_then(|k| k.get(format!("{slot}:value"))).and_then(Value::as_array).cloned().unwrap_or_default();
    let modifiers = l.modifiers.get(&format!("effect:{slot}:value")).cloned();
    if keys.is_empty() && modifiers.is_none() {
        return BTreeMap::new();
    }
    BTreeMap::from([(klipp_graph_format::ParamKey::new("value"), Animation { keys, modifiers })])
}

/// The built-in effect list being written, with its flags and the keys
/// that name its slots.
#[derive(Default)]
struct Fx {
    effects: Vec<Value>,
    flags: Vec<Value>,
    keyframes: Map<String, Value>,
    modifiers: Map<String, Value>,
}

impl Fx {
    /// One effect, its animation keyed by the slot it lands in, under the
    /// names v1 stored (an alias back to the name it was saved under).
    fn push(&mut self, effect: Value, enabled: bool, anim: &BTreeMap<klipp_graph_format::ParamKey, Animation>) {
        let slot = self.effects.len();
        let tag = effect.get("type").and_then(Value::as_str).unwrap_or("").to_owned();
        for (param, a) in anim {
            let name = KEYFRAME_ALIASES
                .iter()
                .find(|(t, _, to)| *t == tag && *to == param.as_str())
                .map(|(_, from, _)| *from)
                .unwrap_or(param.as_str());
            if !a.keys.is_empty() {
                self.keyframes.insert(format!("{slot}:{name}"), Value::Array(a.keys.clone()));
            }
            if let Some(m) = &a.modifiers {
                self.modifiers.insert(format!("effect:{slot}:{name}"), m.clone());
            }
        }
        self.effects.push(effect);
        self.flags.push(Value::Bool(enabled));
    }
}

/// A clip-level animation into its v1 keyframe field and modifier key.
fn animate(out: &mut Map<String, Value>, modifiers: &mut Map<String, Value>, field: &str, modifier: Option<&str>, a: &Animation) {
    if !a.keys.is_empty() {
        out.insert(field.into(), Value::Array(a.keys.clone()));
    }
    if let (Some(key), Some(m)) = (modifier, &a.modifiers) {
        modifiers.insert(key.into(), m.clone());
    }
}

fn has_values(p: &PlacementProps) -> bool {
    [p.pos_x, p.pos_y, p.scale, p.aspect, p.rotation, p.anchor_x, p.anchor_y].iter().any(Option::is_some)
}

/// A Transform effect holding the placement, each value it lacks at the
/// identity.
fn transform(p: &PlacementProps, with_tilt: bool) -> Value {
    let scale = p.scale.unwrap_or(1.0);
    let tilt = |v: Option<f32>| if with_tilt { v.unwrap_or(0.0) } else { 0.0 };
    json!({
        "type": "transform",
        "position_x": p.pos_x.unwrap_or(0.0),
        "position_y": p.pos_y.unwrap_or(0.0),
        "scale_x": scale,
        // The height keeps the clip's aspect under the width (#1406).
        "scale_y": scale * p.aspect.unwrap_or(1.0),
        "rotation": p.rotation.unwrap_or(0.0),
        "anchor_x": p.anchor_x.unwrap_or(0.5),
        "anchor_y": p.anchor_y.unwrap_or(0.5),
        "rotation_x": tilt(p.tilt_x),
        "rotation_y": tilt(p.tilt_y),
        "perspective": tilt(p.perspective),
        "focal_ref": p.focal_ref.clone().unwrap_or(Value::Null),
    })
}

/// A list entry as stored: an effect's written from its map (M3-2).
fn entry(n: &Node) -> Result<Value, Refusal> {
    if n.type_id.as_str() == types::LEGACY_EFFECT {
        return klipp_graph_lift::effect_params::entry_of(n);
    }
    payload(n).cloned()
}

fn payload(n: &Node) -> Result<&Value, Refusal> {
    n.legacy.as_ref().map(|l| &l.payload).ok_or_else(|| format!("node {} ({}) carries no entry", n.id, n.type_id))
}

/// The clip graph's nodes from its source to its output, by its edges:
/// the lift writes one chain, and feeds each Grade group's `frame` from
/// what enters the first one.
fn chain(g: &Graph) -> Result<Vec<&Node>, Refusal> {
    let by_id: BTreeMap<NodeId, &Node> = g.nodes.iter().map(|n| (n.id, n)).collect();
    let start = g.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).ok_or("a lifted clip without a source")?;
    let (frames, links): (Vec<_>, Vec<_>) = g.edges.iter().partition(|e| e.to.1.as_str() == "frame");
    let mut order = vec![start];
    let mut at = start.id;
    while at != g.output {
        let next: Vec<_> = links.iter().filter(|e| e.from.0 == at).collect();
        let [e] = next.as_slice() else { return Err(format!("node {at} feeds {} nodes, not one", next.len())) };
        at = e.to.0;
        order.push(by_id.get(&at).ok_or_else(|| format!("an edge into {at}, which is not there"))?);
        if order.len() > g.nodes.len() {
            return Err("the chain loops".into());
        }
    }
    if order.len() != g.nodes.len() || links.len() != order.len() - 1 {
        return Err(format!("the chain holds {} of {} nodes and {} edges", order.len(), g.nodes.len(), links.len()));
    }
    // What enters the grade is what the node before the first group hands
    // on; today's unwired reads see that picture, not a stage's output.
    let first = order.iter().position(|n| n.type_id.as_str() == lt::GROUP);
    let feed = first.and_then(|i| i.checked_sub(1)).map(|i| order[i].id);
    for e in &frames {
        if Some(e.from.0) != feed || by_id.get(&e.to.0).is_none_or(|n| n.type_id.as_str() != lt::GROUP) {
            return Err(format!("a frame edge from {} into {}, not from what enters the grade into a group", e.from.0, e.to.0));
        }
    }
    Ok(order)
}

/// A grade group back in v1: its graph, and the ports its body reads the
/// frame entering the grade at, as (node, port), drawn from the footage
/// once the groups are flattened.
struct Grade {
    graph: Value,
    reads: Vec<(String, u64)>,
}

/// The Grade group's body back to a v1 grade graph: its ends as Source
/// and Output, its merges by their op, a gate wherever the lift flagged
/// one. The frame entering the grade, and an input the lift added (v1 had
/// no Source), are what an unwired port reads today: their edges come
/// back as reads of the footage, from a Source made for them when v1 had
/// none. An output the lift added is a bare Output. A correction's mask,
/// drawn or wireless, without the gate is refused: v1 never read it, and
/// the lift parks it.
///
/// A group the lift switched off (its grade loops, so today's renderer
/// never ran it) comes back as a graph today's renderer refuses to order,
/// whatever is spliced around it: an edge from its Output into itself.
/// Whether the lift switched off just the grades that loop is the tests'
/// to check, against today's ordering.
fn grade(group: &Node) -> Result<Grade, Refusal> {
    let body = &group.group.as_ref().ok_or_else(|| format!("grade group {} has no body", group.id))?.graph;
    let mut kinds: BTreeMap<NodeId, String> = BTreeMap::new();
    let mut minted = Vec::new();
    let mut source = None;
    let mut nodes = Vec::new();
    for n in &body.nodes {
        let mut subgraph = None;
        let kind = match n.type_id.as_str() {
            lt::GROUP_FRAME | lt::GROUP_INPUT if n.legacy.is_none() => {
                minted.push(n.id);
                continue;
            }
            lt::GROUP_OUTPUT if n.legacy.is_none() => {
                kinds.insert(n.id, "Output".to_owned());
                nodes.push(json!({ "id": n.id, "kind": "Output", "label": "", "enabled": true }));
                continue;
            }
            lt::GROUP_INPUT => {
                source.get_or_insert(n.id);
                "Source".to_owned()
            }
            lt::GROUP_OUTPUT => "Output".to_owned(),
            // A compound (#1391): its body back to the subgraph, the frame's
            // reads from its own Source (the compound's input, as today's
            // executor runs it). One the lift switched off comes back as a
            // subgraph today's executor refuses, so it passes through.
            lt::GROUP => {
                let inner = grade(n)?;
                let mut sub = inner.graph;
                if !inner.reads.is_empty() {
                    let src = first_of(&sub, "Source").ok_or_else(|| format!("compound {} reads its input with no Source", n.id))?.to_owned();
                    let reads: Vec<Value> = inner.reads.iter().map(|(to, port)| edge(&(src.clone(), 0), to, *port)).collect();
                    if let Some(Value::Array(e)) = sub.get_mut("edges") {
                        e.extend(reads);
                    }
                }
                subgraph = Some(sub);
                "Compound".to_owned()
            }
            lt::MERGE => {
                let op = match n.params.get(&klipp_graph_format::ParamKey::new("op")) {
                    Some(ParamValue::Text(op)) => op,
                    other => return Err(format!("merge {} has op {other:?}", n.id)),
                };
                let (kind, _) = lt::MERGE_OPS.iter().find(|(_, o)| o == op).ok_or_else(|| format!("merge {} has an unknown op {op}", n.id))?;
                (*kind).to_owned()
            }
            types::LEGACY_NODE => payload(n)?.get("kind").and_then(Value::as_str).ok_or_else(|| format!("grade node {} has no kind", n.id))?.to_owned(),
            other => return Err(format!("grade node {} is a {other}", n.id)),
        };
        let p = payload(n)?;
        let mut v = Map::new();
        v.insert("id".into(), json!(n.id));
        v.insert("kind".into(), json!(kind));
        v.insert("enabled".into(), json!(n.enabled));
        for key in ["label", "params"] {
            if let Some(x) = p.get(key) {
                v.insert(key.into(), x.clone());
            }
        }
        if let Some(sub) = subgraph {
            let params = v.entry("params").or_insert_with(|| json!({}));
            params.as_object_mut().ok_or_else(|| format!("compound {}'s params aren't an object", n.id))?.insert("compound_subgraph".into(), sub);
        }
        if let Some(k) = n.legacy.as_ref().and_then(|l| l.keyframes.as_ref()) {
            v.insert("keyframes".into(), k.clone());
        }
        if let Some(link) = &n.wireless.output_link {
            v.insert("output_link".into(), json!(link));
        }
        let gated = n.legacy_flags.as_ref().is_some_and(|f| f.alpha_one);
        let mut links: Vec<Value> = Vec::new();
        for (key, name) in &n.wireless.input_links {
            let i = port(&kind, key, true).ok_or_else(|| format!("grade node {} ({kind}) has no input {key}", n.id))?;
            if i == 1 && GATEABLE.contains(&kind.as_str()) && !gated {
                return Err(format!("grade node {} ({kind}) has a link into its mask without the gate, which the lift parks", n.id));
            }
            if links.len() <= usize::from(i) {
                links.resize(usize::from(i) + 1, Value::Null);
            }
            links[usize::from(i)] = json!(name);
        }
        if !links.is_empty() {
            v.insert("input_links".into(), Value::Array(links));
        }
        kinds.insert(n.id, kind);
        nodes.push(Value::Object(v));
    }
    let mut edges = Vec::new();
    let mut reads = Vec::new();
    for e in &body.edges {
        let to_kind = kinds.get(&e.to.0).map(String::as_str).unwrap_or("");
        // A compound's frame is its input, which the subgraph reads.
        if to_kind == "Compound" && e.to.1.as_str() == "frame" {
            continue;
        }
        let to_port = port(to_kind, &e.to.1, true).ok_or_else(|| format!("an edge enters {} by {}", e.to.0, e.to.1))?;
        let gated = body.nodes.iter().find(|n| n.id == e.to.0).and_then(|n| n.legacy_flags.as_ref()).is_some_and(|f| f.alpha_one);
        if to_port == 1 && GATEABLE.contains(&to_kind) && !gated {
            return Err(format!("an edge into {}'s mask without the gate, which the lift parks", e.to.0));
        }
        if minted.contains(&e.from.0) {
            reads.push((e.to.0.to_string(), u64::from(to_port)));
            continue;
        }
        let from_kind = kinds.get(&e.from.0).map(String::as_str).unwrap_or("");
        let from_port = port(from_kind, &e.from.1, false).ok_or_else(|| format!("an edge leaves {} by {}", e.from.0, e.from.1))?;
        edges.push(json!({ "from_node": e.from.0, "from_port": from_port, "to_node": e.to.0, "to_port": to_port }));
    }
    // The frame's reads need a Source to come from: v1 had none, so one
    // stands in, under the id of the input the lift added.
    if source.is_none() && !reads.is_empty() {
        let id = body.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP_INPUT).map_or(group.id, |n| n.id);
        nodes.insert(0, json!({ "id": id, "kind": "Source", "label": "", "enabled": true }));
    }
    if !group.enabled {
        edges.push(json!({ "from_node": body.output, "from_port": 0, "to_node": body.output, "to_port": 1 }));
    }
    let mut g = Map::new();
    g.insert("nodes".into(), Value::Array(nodes));
    g.insert("edges".into(), Value::Array(edges));
    g.insert("output_node_id".into(), json!(body.output));
    if let Some(w) = &body.working_space_override {
        g.insert("working_space_override".into(), w.clone());
    }
    let graph = Value::Object(g);
    Ok(Grade { graph, reads })
}

// The flattening, on v1 grade graphs as JSON, step for step as today's
// `NodeGraph::prepend_keeping_ids`, `append_keeping_ids` (`splice_after`)
// and `strip_input_transforms` do it (crates/klipp-nodes/src/graph.rs).
// The splice only nests what those accept, so a refusal here is a
// disagreement with them.

fn nodes(g: &Value) -> &Vec<Value> {
    static EMPTY: Vec<Value> = Vec::new();
    g["nodes"].as_array().unwrap_or(&EMPTY)
}

fn edges(g: &Value) -> &Vec<Value> {
    static EMPTY: Vec<Value> = Vec::new();
    g["edges"].as_array().unwrap_or(&EMPTY)
}

fn sid(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// The first node of `kind`.
fn first_of<'a>(g: &'a Value, kind: &str) -> Option<&'a str> {
    nodes(g).iter().find(|n| n["kind"] == kind).map(|n| sid(&n["id"]))
}

/// What feeds `port` of `node`: the first edge into it, else the wireless
/// sender its link names (port 0 of it), as `input_of` reads it.
fn feed_of(g: &Value, node: &str, port: u64) -> Option<(String, u64)> {
    if let Some(e) = edges(g).iter().find(|e| sid(&e["to_node"]) == node && e["to_port"] == port) {
        return Some((sid(&e["from_node"]).to_owned(), e["from_port"].as_u64().unwrap_or(0)));
    }
    let link = nodes(g).iter().find(|n| sid(&n["id"]) == node)?["input_links"].get(port as usize)?.as_str()?;
    nodes(g).iter().find(|n| n["output_link"] == link && sid(&n["id"]) != node).map(|n| (sid(&n["id"]).to_owned(), 0))
}

fn edge(from: &(String, u64), to: &str, to_port: u64) -> Value {
    json!({ "from_node": from.0, "from_port": from.1, "to_node": to, "to_port": to_port })
}

fn strip_input_transforms(g: &mut Value) {
    let transforms: Vec<String> = nodes(g).iter().filter(|n| n["kind"] == "InputTransform").map(|n| sid(&n["id"]).to_owned()).collect();
    for it in transforms {
        let feed = feed_of(g, &it, 0);
        let label = nodes(g).iter().find(|n| sid(&n["id"]) == it).and_then(|n| n["output_link"].as_str()).map(str::to_owned);
        let mut es: Vec<Value> = edges(g).iter().filter(|e| sid(&e["to_node"]) != it).cloned().collect();
        match &feed {
            Some((from, port)) => {
                for e in es.iter_mut().filter(|e| sid(&e["from_node"]) == it) {
                    e["from_node"] = json!(from);
                    e["from_port"] = json!(port);
                }
            }
            None => es.retain(|e| sid(&e["from_node"]) != it),
        }
        let mut ns = nodes(g).clone();
        if let Some(label) = label {
            let mut drawn = Vec::new();
            for recv in ns.iter_mut().filter(|n| sid(&n["id"]) != it) {
                let rid = sid(&recv["id"]).to_owned();
                let Some(links) = recv.get_mut("input_links").and_then(Value::as_array_mut) else { continue };
                for (port, slot) in links.iter_mut().enumerate() {
                    if slot.as_str() != Some(label.as_str()) {
                        continue;
                    }
                    *slot = Value::Null;
                    let wired = es.iter().any(|e| sid(&e["to_node"]) == rid && e["to_port"] == port as u64);
                    if let Some(f) = feed.as_ref().filter(|_| !wired) {
                        drawn.push(edge(f, &rid, port as u64));
                    }
                }
            }
            es.extend(drawn);
        }
        ns.retain(|n| sid(&n["id"]) != it);
        g["nodes"] = Value::Array(ns);
        g["edges"] = Value::Array(es);
    }
}

fn working_spaces_agree(ours: &Value, theirs: &Value) -> Result<(), Refusal> {
    match (ours.get("working_space_override"), theirs.get("working_space_override")) {
        (Some(a), Some(b)) if !a.is_null() && !b.is_null() && a != b => Err(format!("working spaces {a} and {b}")),
        _ => Ok(()),
    }
}

/// `other` after `g`: its middle nodes between what fed `g`'s Output and
/// the Output.
fn append(g: &mut Value, other: &Value) -> Result<(), Refusal> {
    working_spaces_agree(g, other)?;
    let mut other = other.clone();
    strip_input_transforms(&mut other);
    let src = first_of(&other, "Source").ok_or("an appended grade with no Source")?.to_owned();
    let o_out = sid(&other["output_node_id"]).to_owned();
    let out = sid(&g["output_node_id"]).to_owned();
    let feed = edges(g)
        .iter()
        .find(|e| sid(&e["to_node"]) == out && e["to_port"] == 0)
        .map(|e| (sid(&e["from_node"]).to_owned(), e["from_port"].as_u64().unwrap_or(0)))
        .ok_or("nothing feeds the Output")?;
    let middle: Vec<Value> = nodes(&other).iter().filter(|n| sid(&n["id"]) != src && sid(&n["id"]) != o_out).cloned().collect();
    if middle.is_empty() {
        return Ok(());
    }
    let mut es: Vec<Value> = edges(g).iter().filter(|e| !(sid(&e["to_node"]) == out && e["to_port"] == 0)).cloned().collect();
    let mut ns = nodes(g).clone();
    let at = ns.iter().position(|n| sid(&n["id"]) == out).unwrap_or(ns.len());
    ns.splice(at..at, middle);
    for e in edges(&other) {
        if sid(&e["to_node"]) == src {
            continue;
        }
        let from = if sid(&e["from_node"]) == src { feed.clone() } else { (sid(&e["from_node"]).to_owned(), e["from_port"].as_u64().unwrap_or(0)) };
        let to = if sid(&e["to_node"]) == o_out { out.as_str() } else { sid(&e["to_node"]) };
        es.push(edge(&from, to, e["to_port"].as_u64().unwrap_or(0)));
    }
    g["nodes"] = Value::Array(ns);
    g["edges"] = Value::Array(es);
    Ok(())
}

/// `other` before `g`'s grade: its middle nodes between the footage as `g`
/// decodes it and everything that read it.
fn prepend(g: &mut Value, other: &Value) -> Result<(), Refusal> {
    working_spaces_agree(g, other)?;
    let mut other = other.clone();
    strip_input_transforms(&mut other);
    let o_src = first_of(&other, "Source").ok_or("a prepended grade with no Source")?.to_owned();
    let src = first_of(g, "Source").ok_or("a grade with no Source to prepend to")?.to_owned();
    let o_out = sid(&other["output_node_id"]).to_owned();
    let middle: Vec<Value> = nodes(&other).iter().filter(|n| sid(&n["id"]) != o_src && sid(&n["id"]) != o_out).cloned().collect();
    if middle.is_empty() {
        return Ok(());
    }
    let o_feed = edges(&other)
        .iter()
        .find(|e| sid(&e["to_node"]) == o_out && e["to_port"] == 0)
        .map(|e| (sid(&e["from_node"]).to_owned(), e["from_port"].as_u64().unwrap_or(0)))
        .ok_or("nothing feeds the prepended grade's Output")?;
    let head = edges(g)
        .iter()
        .find(|e| sid(&e["from_node"]) == src && nodes(g).iter().any(|n| n["id"] == e["to_node"] && n["kind"] == "InputTransform"))
        .map_or(src.clone(), |e| sid(&e["to_node"]).to_owned());
    let tail = if o_feed.0 == o_src { (head.clone(), 0) } else { o_feed };
    let mut es = edges(g).clone();
    for e in es.iter_mut().filter(|e| sid(&e["from_node"]) == head && e["from_port"] == 0) {
        e["from_node"] = json!(tail.0);
        e["from_port"] = json!(tail.1);
    }
    for e in edges(&other) {
        if sid(&e["to_node"]) == o_out || sid(&e["to_node"]) == o_src {
            continue;
        }
        let from = if sid(&e["from_node"]) == o_src { (head.clone(), 0) } else { (sid(&e["from_node"]).to_owned(), e["from_port"].as_u64().unwrap_or(0)) };
        es.push(edge(&from, sid(&e["to_node"]), e["to_port"].as_u64().unwrap_or(0)));
    }
    let mut ns = nodes(g).clone();
    let at = ns.iter().position(|n| sid(&n["id"]) == head).map_or(0, |i| i + 1);
    ns.splice(at..at, middle);
    g["nodes"] = Value::Array(ns);
    g["edges"] = Value::Array(es);
    Ok(())
}

/// A port key back to its v1 number on a node of `kind`.
fn port(kind: &str, key: &PortKey, input: bool) -> Option<u8> {
    let named = if input { ports::input_index(kind, key) } else { ports::output_index(kind, key) };
    named.or_else(|| key.as_str().strip_prefix('p').and_then(|n| n.parse().ok()))
}
