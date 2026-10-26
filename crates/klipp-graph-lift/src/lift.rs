//! The semantic lift (phase 2a, #1068): a revision-1 clip record becomes
//! the lifted [`ClipPicture`], by the rules of the design's §13.2, each
//! paired with a [`LiftNote`] for what it dropped or gave a meaning to.
//!
//! Pure functions over the record, wired into nothing. The proof is the
//! adapter property, `adapter(lift(to_v2(x))) == render_inputs(x)`, where
//! the adapter and `render_inputs` (today's code, read off v1) live with
//! the oracle.
//!
//! The lifted graph, in render order: `klipp.source` → head nodes (Media
//! clips) → the Grade group → `scene_to_display` → OFX → built-in effects
//! (with `klipp.placement` among them) → `klipp.output`.

use std::collections::BTreeMap;

use klipp_graph_format::lifted::{types as lt, ClipPicture, LegacyFlags, LiftNote, PlacementProps};
use klipp_graph_format::{
    types, Animation, ClipRecordV2, Edge, Graph, GraphUi, GroupBody, Legacy, Node, NodeId, NodeTypeId, ParamKey, ParamValue,
    PortKey, GRAPH_VERSION,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::ids::{entry_id, RootKind};
use crate::names::{aspect, KEYFRAME_ALIASES};

/// What the record alone doesn't say about the clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipKind {
    /// A clip of media: the only kind whose head nodes run.
    Media,
    Title,
    Generator,
    Precomposition,
    /// An editorial compound: a sequence placed as a clip.
    Compound,
    Adjustment,
    /// An empty stretch of a track: it draws nothing.
    Gap,
}

impl ClipKind {
    /// The kind a clip's stored `kind` names (`{"type": "media", ...}`).
    pub fn from_json(kind: Option<&Value>) -> Result<Self, String> {
        let tag = match kind {
            None | Some(Value::Null) => return Ok(Self::Media),
            Some(k) => k.get("type").and_then(Value::as_str).unwrap_or_default(),
        };
        Ok(match tag {
            "media" => Self::Media,
            "title" | "title_v2" | "vector_title" => Self::Title,
            "generator" => Self::Generator,
            "sequence" => Self::Compound,
            "precomposition" | "template_precomposition" => Self::Precomposition,
            "adjustment_layer" => Self::Adjustment,
            "gap" => Self::Gap,
            other => return Err(format!("a clip kind the lift doesn't know: {other:?}")),
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LiftCtx {
    pub clip_id: Uuid,
    pub kind: ClipKind,
}

/// The corrections a matte on port 1 gates (`graph.rs`'s `gateable`).
const GATEABLE: [&str; 8] = ["Corrector", "ColorBalance", "Exposure", "Contrast", "Saturation", "ColorWheels", "ToneControls", "RGBCurves"];

/// The effects that draw outside the picture, in the margin a transform
/// leaves (`klipp_effects::draws_outside_the_picture`). Kept in step with
/// it by a test.
pub const DRAWS_OUTSIDE: [&str; 4] = ["drop_shadow", "outer_glow", "fill_bars", "reflection"];

/// Where an `Auto` placement takes effect in a clip's chain as it renders:
/// the enabled effects with the track's spliced after the clip's own, and
/// Opacity, SpeedRamp and TemporalDenoise already gone. It goes before the
/// first that draws outside the picture, else at the end
/// (`render.rs` `clip_effects`). An identity placement is skipped there.
pub fn auto_placement_index(enabled_effects: &[&Value]) -> usize {
    enabled_effects
        .iter()
        .position(|e| e.get("type").and_then(Value::as_str).is_some_and(|t| DRAWS_OUTSIDE.contains(&t)))
        .unwrap_or(enabled_effects.len())
}

/// The modifier types that remap time rather than change a value.
const TIME_MODIFIERS: [&str; 2] = ["loop", "ping_pong"];

fn node(id: NodeId, type_id: &str, enabled: bool, legacy: Option<Legacy>) -> Node {
    Node {
        id,
        type_id: NodeTypeId::new(type_id),
        type_version: 1,
        label: String::new(),
        enabled,
        params: BTreeMap::new(),
        anim: BTreeMap::new(),
        mix: None,
        wireless: Default::default(),
        group: None,
        legacy,
        legacy_flags: None,
        origin: None,
    }
}

fn flags(n: &mut Node) -> &mut LegacyFlags {
    n.legacy_flags.get_or_insert_with(LegacyFlags::default)
}

fn payload(n: &Node) -> &Value {
    n.legacy.as_ref().map(|l| &l.payload).unwrap_or(&Value::Null)
}

fn num(v: &Value, key: &str) -> Option<f32> {
    v.get(key).and_then(Value::as_f64).map(|f| f as f32)
}

fn kind_of(n: &Node) -> &str {
    payload(n).get("kind").and_then(Value::as_str).unwrap_or("")
}

/// Whether input `port` of `n` is connected: an edge into it, or a wireless
/// link naming a sender in the same graph (today's `input_of`).
fn connected(body: &Graph, n: &Node, port: &PortKey) -> bool {
    body.edges.iter().any(|e| e.to.0 == n.id && &e.to.1 == port)
        || n.wireless.input_links.get(port).is_some_and(|name| {
            body.nodes.iter().any(|o| o.id != n.id && o.wireless.output_link.as_deref() == Some(name.as_str()))
        })
}

/// Today's `migrate_graph` (`load.rs`), made once on the structural body: a
/// graph on the old legacy chain with no Corrector gets a Color Warper
/// after its HueSat Curves (else before its Output), wired in after the
/// first edge out of HueSat. Its id is derived from the group's.
fn migrate_color_warper(body: &mut Graph, group: NodeId) -> bool {
    let kinds: Vec<&str> = body.nodes.iter().map(kind_of).collect();
    let legacy_chain = kinds.iter().any(|k| matches!(*k, "Exposure" | "Contrast" | "ColorWheels" | "HueSatCurves"));
    if kinds.contains(&"Corrector") || !legacy_chain || kinds.contains(&"ColorWarper") {
        return false;
    }
    let id = Uuid::new_v5(&group, b"migrate/color_warper");
    let warper = node(
        id,
        types::LEGACY_NODE,
        true,
        Some(Legacy {
            slot: None,
            payload: json!({"kind": "ColorWarper", "label": "Color Warper", "params": {}}),
            keyframes: Some(json!({"tracks": []})),
            modifiers: BTreeMap::new(),
        }),
    );
    let huesat = body.nodes.iter().find(|n| kind_of(n) == "HueSatCurves").map(|n| n.id);
    let at = match body.nodes.iter().position(|n| kind_of(n) == "HueSatCurves") {
        Some(i) => i + 1,
        None => body.nodes.iter().position(|n| kind_of(n) == "Output").unwrap_or(body.nodes.len()),
    };
    body.nodes.insert(at, warper);
    if let Some(h) = huesat {
        if let Some(e) = body.edges.iter_mut().find(|e| e.from.0 == h) {
            let old = std::mem::replace(&mut e.to, (id, klipp_graph_format::ports::input_key("ColorWarper", 0)));
            body.edges.push(Edge { from: (id, klipp_graph_format::ports::output_key("ColorWarper", 0)), to: old });
        }
    }
    true
}

/// Today's default pipeline (`NodeGraph::default()`: Source, Corrector,
/// Output in a row), as its file form, with ids derived from the clip's.
pub fn default_pipeline(clip_id: Uuid) -> Value {
    let id = |what: &str| entry_id(RootKind::Clip, clip_id, &format!("lift/default/{what}"), 0).to_string();
    let (s, c, o) = (id("source"), id("corrector"), id("output"));
    let n = |id: &str, kind: &str| json!({"id": id, "kind": kind, "label": kind, "enabled": true, "params": {}, "keyframes": {"tracks": []}, "output_link": null, "input_links": []});
    json!({
        "nodes": [n(&s, "Source"), n(&c, "Corrector"), n(&o, "Output")],
        "edges": [
            {"from_node": s, "from_port": 0, "to_node": c, "to_port": 0},
            {"from_node": c, "from_port": 0, "to_node": o, "to_port": 0}
        ],
        "output_node_id": o,
        "node_positions": {s.as_str(): {"x": 0.0, "y": 0.0}, c.as_str(): {"x": 220.0, "y": 0.0}, o.as_str(): {"x": 440.0, "y": 0.0}}
    })
}

/// The Grade group's body, lifted: gates decided on the graph as saved,
/// then implicit source reads made explicit, then the merges named.
/// Whether today's renderer refuses to order `body`, the grade graph as its
/// loader leaves it: `NodeGraph::topo_sort` followed line for line, over
/// every drawn edge (a wire into a port its kind never reads, from or to a
/// node that isn't there, repeated) and every wireless link no wire
/// overrides, as `iter_dependencies` counts them. Decided before the lift
/// drops or parks anything, so a loop only those edges made still never
/// ran.
fn today_loops(body: &Graph) -> bool {
    let node_ids: Vec<NodeId> = body.nodes.iter().map(|n| n.id).collect();
    // Repeated ids: a duplicate enters the queue twice and takes the same
    // dependency off twice, which today's ordering never finishes. Refused
    // here without ordering (no count can underflow), and `check_lifted`
    // stops the project on the duplicate (#1102 review).
    if node_ids.iter().collect::<std::collections::BTreeSet<_>>().len() != node_ids.len() {
        return true;
    }
    let mut deps: Vec<(NodeId, NodeId)> = body.edges.iter().map(|e| (e.from.0, e.to.0)).collect();
    for recv in &body.nodes {
        for (port, label) in &recv.wireless.input_links {
            if body.edges.iter().any(|e| e.to.0 == recv.id && &e.to.1 == port) {
                continue;
            }
            if let Some(sender) = body.nodes.iter().find(|s| s.wireless.output_link.as_deref() == Some(label.as_str()) && s.id != recv.id) {
                deps.push((sender.id, recv.id));
            }
        }
    }
    let mut in_degree: BTreeMap<NodeId, usize> = node_ids.iter().map(|id| (*id, 0)).collect();
    for (_, to) in &deps {
        *in_degree.entry(*to).or_insert(0) += 1;
    }
    let mut queue: Vec<NodeId> = node_ids.iter().filter(|id| in_degree.get(id).copied().unwrap_or(0) == 0).copied().collect();
    let mut ordered = 0;
    while let Some(node_id) = queue.pop() {
        ordered += 1;
        for (from, to) in &deps {
            if *from == node_id {
                if let Some(deg) = in_degree.get_mut(to) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push(*to);
                    }
                }
            }
        }
    }
    ordered != node_ids.len()
}

/// A grade group's body lifted, its legacy flags, and whether today's
/// renderer refused to order it ([`today_loops`]).
fn lift_grade_body(group: &Node, migrate: bool, notes: &mut Vec<LiftNote>) -> (Graph, LegacyFlags, bool) {
    lift_grade_body_at(group, migrate, 0, notes)
}

/// How deep today's executor runs compounds inside compounds; one deeper
/// passes its input through (`executor.rs` `COMPOUND_MAX_DEPTH`).
const COMPOUND_MAX_DEPTH: u32 = 8;

/// [`lift_grade_body`] for a body `depth` compounds deep.
fn lift_grade_body_at(group: &Node, migrate: bool, depth: u32, notes: &mut Vec<LiftNote>) -> (Graph, LegacyFlags, bool) {
    let mut body = group.group.as_ref().map(|g| g.graph.clone()).unwrap_or_else(Graph::empty);
    // The graph as today's loader leaves it: migrated first, on the graphs
    // it migrates (a clip's, a track's, a grade version's).
    if migrate && migrate_color_warper(&mut body, group.id) {
        notes.push(LiftNote::LoadMigration { group: group.id, what: "color warper".into() });
    }
    let loops = today_loops(&body);
    let mut group_flags = LegacyFlags::default();
    let input_port = |kind: &str, port: u8| klipp_graph_format::ports::input_key(kind, port);

    // 1. Gates, on the graph as saved: a correction is gated only when its
    // port 0 and port 1 are both connected. A port-1 edge with port 0
    // unwired was never a gate: it is parked, outside the graph.
    let mut gated = Vec::new();
    // A set: repeated saved ids can name one edge twice.
    let mut parked = std::collections::BTreeSet::new();
    // A wireless link on such a port is parked too: never a gate either.
    let mut parked_links: Vec<(NodeId, PortKey)> = Vec::new();
    for n in &body.nodes {
        let kind = kind_of(n);
        if !GATEABLE.contains(&kind) {
            continue;
        }
        let (p0, p1) = (input_port(kind, 0), input_port(kind, 1));
        let has1 = connected(&body, n, &p1);
        if has1 && connected(&body, n, &p0) {
            gated.push(n.id);
        } else if has1 {
            parked.extend(body.edges.iter().enumerate().filter(|(_, e)| e.to.0 == n.id && e.to.1 == p1).map(|(i, _)| i));
            // With a wire there, the link was overridden; either way it
            // goes with the port, or it would turn live once the wire is out.
            if n.wireless.input_links.contains_key(&p1) {
                parked_links.push((n.id, p1.clone()));
            }
        }
    }
    for i in parked.iter().rev() {
        let edge = body.edges.remove(*i);
        let value = serde_json::to_value(&edge).unwrap_or(Value::Null);
        notes.push(LiftNote::ParkedEdge { group: group.id, edge: value.clone() });
        group_flags.parked_edges.insert(0, value);
        group_flags.parked_at.insert(0, *i as u32);
    }
    for (id, port) in parked_links {
        if let Some(n) = body.nodes.iter_mut().find(|n| n.id == id) {
            if let Some(name) = n.wireless.input_links.remove(&port) {
                let link = json!({ "node": id, "port": port, "name": name });
                notes.push(LiftNote::ParkedEdge { group: group.id, edge: link.clone() });
                group_flags.parked_links.push(link);
            }
        }
    }
    for n in body.nodes.iter_mut().filter(|n| gated.contains(&n.id)) {
        flags(n).alpha_one = true;
    }

    // 2. v1's Source and Output become the group's ends.
    let mut input_id = None;
    for n in &mut body.nodes {
        match kind_of(n) {
            "Source" => {
                n.type_id = NodeTypeId::new(lt::GROUP_INPUT);
                input_id.get_or_insert(n.id);
            }
            "Output" => n.type_id = NodeTypeId::new(lt::GROUP_OUTPUT),
            _ => {}
        }
    }
    // A graph with no Source still reads the frame: give it an input.
    let input_id = input_id.unwrap_or_else(|| {
        let id = Uuid::new_v5(&group.id, b"group.input");
        body.nodes.insert(0, node(id, lt::GROUP_INPUT, true, None));
        id
    });

    // The picture entering the grade, which an unwired input reads today
    // wherever its node sits; explicit edges from the v1 Source stay on the
    // group's input, which a splice reroutes as today's bakes do.
    let frame_id = Uuid::new_v5(&group.id, b"group.frame");
    let at = body.nodes.iter().position(|n| n.id == input_id).map_or(0, |i| i + 1);
    body.nodes.insert(at, node(frame_id, lt::GROUP_FRAME, true, None));

    // 2b. Odd edges read as today's executor reads them: an output named
    // but missing is made; an edge to a missing node, into a port the kind
    // never reads (`p<n>`), or after another into the same port is never
    // read and goes; an edge from a missing node into a primary input reads
    // the frame there (`unwrap_or(source)`), so it comes from the input.
    // Anything less certain stays as saved, and `check_lifted` stops it.
    if !body.nodes.iter().any(|n| n.id == body.output) {
        body.nodes.push(node(body.output, lt::GROUP_OUTPUT, true, None));
        notes.push(LiftNote::MissingOutput { group: group.id });
    }
    let kinds: BTreeMap<NodeId, String> = body.nodes.iter().rev().map(|n| (n.id, kind_of(n).to_owned())).collect();
    let mut seen = std::collections::BTreeSet::new();
    let mut overridden: Vec<(NodeId, PortKey)> = Vec::new();
    let mut kept = Vec::with_capacity(body.edges.len());
    for (at, mut e) in std::mem::take(&mut body.edges).into_iter().enumerate() {
        let value = serde_json::to_value(&e).unwrap_or(Value::Null);
        let drop = |why: &str, notes: &mut Vec<LiftNote>, flags: &mut LegacyFlags| {
            notes.push(LiftNote::EdgeDropped { group: group.id, edge: value.clone(), why: why.into() });
            flags.dropped_edges.push(json!({ "at": at, "edge": value }));
        };
        let Some(to_kind) = kinds.get(&e.to.0) else {
            drop("to a node that isn't there", notes, &mut group_flags);
            continue;
        };
        let port = e.to.1.as_str();
        if port.strip_prefix('p').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
            drop("into a port the kind doesn't read", notes, &mut group_flags);
            // A wireless link on that port was overridden by the wire, so
            // today's ordering never counted it; it goes with the wire,
            // or it would become a dependency (even a loop) it never was.
            overridden.push((e.to.0, e.to.1.clone()));
            continue;
        }
        // The Output wired into its own port 1 is how a grade that's off
        // is saved (a loop today's ordering refuses, counted above). That
        // port is the Output's alpha now (#898), which reads a matte, so
        // the marker goes as an edge into a port the kind didn't read did.
        if e.from.0 == e.to.0 && to_kind == "Output" {
            drop("the Output into itself, a grade saved off", notes, &mut group_flags);
            continue;
        }
        if !seen.insert((e.to.0, e.to.1.clone())) {
            drop("after another edge into the same port", notes, &mut group_flags);
            continue;
        }
        if !kinds.contains_key(&e.from.0) {
            let primary = *port == *input_port(to_kind, 0).as_str() || (to_kind == "Layer" && *port == *input_port("Layer", 1).as_str());
            if primary {
                group_flags.rewired.push(json!({ "to": e.to, "from": e.from }));
                e.from = (frame_id, "out".into());
                notes.push(LiftNote::EdgeRewired { group: group.id, edge: value.clone() });
            }
        }
        kept.push(e);
    }
    body.edges = kept;
    for (id, port) in overridden {
        let still_wired = body.edges.iter().any(|e| e.to.0 == id && e.to.1 == port);
        if let Some(n) = body.nodes.iter_mut().find(|n| n.id == id).filter(|_| !still_wired) {
            if let Some(name) = n.wireless.input_links.remove(&port) {
                group_flags.dropped_wireless.push(json!({ "node": id, "port": port, "name": name }));
                notes.push(LiftNote::EdgeDropped { group: group.id, edge: json!({ "wireless": name, "to": [id, port] }), why: "a wireless link a dropped wire overrode".into() });
            }
        }
    }

    // 3. Implicit reads become explicit edges ([`derive_reads`]).
    group_flags.filled.extend(derive_reads(&mut body, input_id, frame_id));

    // Parked edges are kept for the record on the body's own input, so a
    // body standing alone (a track or timeline grade, a stage, a version)
    // keeps them as a clip's does.
    // So is everything else the lift took out of the saved edges, for
    // `delift` to put back where it stood.
    let record = LegacyFlags {
        parked_edges: std::mem::take(&mut group_flags.parked_edges),
        parked_at: std::mem::take(&mut group_flags.parked_at),
        parked_links: std::mem::take(&mut group_flags.parked_links),
        dropped_edges: std::mem::take(&mut group_flags.dropped_edges),
        rewired: std::mem::take(&mut group_flags.rewired),
        filled: std::mem::take(&mut group_flags.filled),
        dropped_wireless: std::mem::take(&mut group_flags.dropped_wireless),
        ..LegacyFlags::default()
    };
    if !record.is_empty() {
        if let Some(input) = body.nodes.iter_mut().find(|n| n.id == input_id) {
            let f = flags(input);
            f.parked_edges = record.parked_edges;
            f.parked_at = record.parked_at;
            f.parked_links = record.parked_links;
            f.dropped_edges = record.dropped_edges;
            f.rewired = record.rewired;
            f.filled = record.filled;
            f.dropped_wireless = record.dropped_wireless;
        }
    }

    // 4. Layer, Blend and LayerMixer become merges with their legacy op.
    for n in &mut body.nodes {
        if let Some((_, op)) = lt::MERGE_OPS.iter().find(|(k, _)| *k == kind_of(n)) {
            n.type_id = NodeTypeId::new(lt::MERGE);
            n.params.insert("op".into(), ParamValue::Text((*op).to_owned()));
        }
    }

    // 5. A compound becomes a group whose body is its subgraph, lifted as
    // this body is, so paths run through it (#1391, picture revision 4).
    lift_compounds(&mut body, depth, notes);
    (body, group_flags, loops)
}

/// Each Compound in `body` that holds a subgraph, as a group: its body the
/// subgraph lifted, its entry kept for `delift` without the subgraph, and
/// its `frame` fed what feeds its `in` (today's executor runs a subgraph on
/// the compound's own input). A subgraph today's executor never ran (one
/// that loops, or past its depth) leaves the group off, which renders the
/// passthrough it gave. A Compound without a subgraph, or one whose
/// subgraph doesn't read or repeats an id, stays the entry it was.
fn lift_compounds(body: &mut Graph, depth: u32, notes: &mut Vec<LiftNote>) {
    let mut feeds = Vec::new();
    for n in body.nodes.iter_mut().filter(|n| kind_of(n) == "Compound") {
        let Some(sub) = payload(n).get("params").and_then(|p| p.get("compound_subgraph")).and_then(Value::as_object) else { continue };
        let Ok(structural) = crate::to_v2::grade_body(sub, "a compound's subgraph") else { continue };
        let mut inner = node(n.id, types::LEGACY_GRADE, true, None);
        inner.group = Some(Box::new(GroupBody { graph: structural, ports: Vec::new(), interface: Vec::new() }));
        if repeated_id(&inner).is_err() {
            continue;
        }
        let (lifted, _, loops) = lift_grade_body_at(&inner, false, depth + 1, notes);
        if let Some(params) = n.legacy.as_mut().and_then(|l| l.payload.get_mut("params")).and_then(Value::as_object_mut) {
            params.remove("compound_subgraph");
        }
        n.type_id = NodeTypeId::new(lt::GROUP);
        n.group = Some(Box::new(GroupBody { graph: lifted, ports: Vec::new(), interface: Vec::new() }));
        let why = if loops {
            Some("its subgraph loops, so today's executor passed the compound's input through")
        } else if depth >= COMPOUND_MAX_DEPTH {
            Some("it is nested past today's compound depth, so its input passed through")
        } else {
            None
        };
        if let Some(why) = why.filter(|_| n.enabled) {
            notes.push(LiftNote::CompoundKeptOff { node: n.id, why: why.into() });
            flags(n).was_enabled = Some(true);
            n.enabled = false;
        }
        feeds.push(n.id);
    }
    feed_frames(body, &feeds);
}

/// The reads a grade body's level makes without a saved edge, made
/// explicit (the lift's rule, and Group and Ungroup's, #1532): an unwired
/// primary reads the frame (`executor.rs`, `unwrap_or(source)`), and so
/// does Layer's unwired background; Blend's unwired B reads its A,
/// RGBCombiner's unwired G and B read its R (`unwrap_or(primary_input)`),
/// and ApplyMatte's unwired pre-grade reads the frame. Each becomes an edge
/// from what it read. Returned: the ports filled from another input, as
/// `[node, port]`, the record `delift` takes them back out by, for the
/// caller to keep on the level's input.
pub(crate) fn derive_reads(body: &mut Graph, input_id: NodeId, frame_id: NodeId) -> Vec<Value> {
    let input_port = |kind: &str, port: u8| klipp_graph_format::ports::input_key(kind, port);
    let mut added = Vec::new();
    for n in &body.nodes {
        let kind = kind_of(n);
        if n.id == input_id || n.id == frame_id || matches!(kind, "Source" | "ExternalMatte") {
            continue;
        }
        let mut ports = vec![input_port(kind, 0)];
        if kind == "Layer" {
            ports.push(input_port(kind, 1));
        }
        for p in ports {
            if !connected(body, n, &p) {
                added.push(Edge { from: (frame_id, "out".into()), to: (n.id, p) });
            }
        }
    }
    body.edges.extend(added);
    let source_of = |body: &Graph, n: &Node, port: &PortKey| -> Option<(NodeId, PortKey)> {
        if let Some(e) = body.edges.iter().find(|e| e.to.0 == n.id && &e.to.1 == port) {
            return Some(e.from.clone());
        }
        let name = n.wireless.input_links.get(port)?;
        let sender = body.nodes.iter().find(|o| o.id != n.id && o.wireless.output_link.as_deref() == Some(name.as_str()))?;
        Some((sender.id, klipp_graph_format::ports::output_key(kind_of(sender), 0)))
    };
    let (mut filled, mut record) = (Vec::new(), Vec::new());
    for n in &body.nodes {
        let kind = kind_of(n);
        let fills: Vec<(u8, Option<u8>)> = match kind {
            "Blend" => vec![(1, Some(0))],
            "RGBCombiner" => vec![(1, Some(0)), (2, Some(0))],
            "ApplyMatte" => vec![(2, None)],
            _ => continue,
        };
        for (port, from_port) in fills {
            let p = input_port(kind, port);
            if connected(body, n, &p) {
                continue;
            }
            let from = match from_port {
                Some(fp) => source_of(body, n, &input_port(kind, fp)),
                None => Some((frame_id, "out".into())),
            };
            if let Some(from) = from {
                if from_port.is_some() {
                    record.push(json!([n.id, p]));
                }
                filled.push(Edge { from, to: (n.id, p) });
            }
        }
    }
    body.edges.extend(filled);
    record
}

/// Each compound group of `ids` fed its `frame` from what feeds its `in`
/// (today's executor runs a subgraph on the compound's own input): the
/// lift's rule, and Group and Ungroup's (#1532).
pub(crate) fn feed_frames(body: &mut Graph, ids: &[NodeId]) {
    for id in ids {
        let id = *id;
        let from = body.edges.iter().find(|e| e.to.0 == id && e.to.1.as_str() == "in").map(|e| e.from.clone()).or_else(|| {
            let n = body.nodes.iter().find(|n| n.id == id)?;
            let name = n.wireless.input_links.get(&PortKey::from("in"))?;
            let sender = body.nodes.iter().find(|o| o.id != id && o.wireless.output_link.as_deref() == Some(name.as_str()))?;
            Some((sender.id, klipp_graph_format::ports::output_key(kind_of(sender), 0)))
        });
        if let Some(from) = from {
            body.edges.push(Edge { from, to: (id, "frame".into()) });
        }
    }
}

/// A grade-only revision-1 graph (one `klipp.legacy.grade` node: a group
/// stage, a track grade, a version) as a lifted Grade group body.
/// `migrate` as today's loader does it: on track grades and grade
/// versions, not on group stages or timeline grades.
pub fn lift_grade_only(structural: &Graph, migrate: bool) -> Result<Graph, String> {
    lift_grade_only_noted(structural, migrate, &mut Vec::new())
}

/// [`lift_grade_only`], keeping its notes.
/// A node id a grade graph repeats, which no ordering of it can finish:
/// the lift refuses it, so its project stops (#1102 review).
fn repeated_id(group: &Node) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for n in group.group.iter().flat_map(|b| &b.graph.nodes) {
        if !seen.insert(n.id) {
            return Err(format!("the grade graph repeats node {}", n.id));
        }
    }
    Ok(())
}

pub fn lift_grade_only_noted(structural: &Graph, migrate: bool, notes: &mut Vec<LiftNote>) -> Result<Graph, String> {
    if let [g] = structural.nodes.as_slice() {
        repeated_id(g)?;
    }
    match structural.nodes.as_slice() {
        [g] if g.type_id.as_str() == types::LEGACY_GRADE => match lift_grade_body(g, migrate, notes) {
            // Today it never ran; standing alone there is no group to keep
            // it off in, so it stops rather than start rendering.
            (_, _, true) => Err("a grade graph that loops, which today's renderer never ran".into()),
            (body, _, false) => Ok(body),
        },
        _ => Err(format!("a grade-only graph holds {} nodes, not one grade group", structural.nodes.len())),
    }
}

/// An effect's keyframes and modifiers by parameter name, its stored
/// `"i:k"` and `effect:i:k` keys read once, and the stored keys that drive
/// nothing: one under the name of a field another name animates (a
/// ChromaKey key saved as `soft_edge`, which only `spill` drives).
fn effect_anim(n: &Node) -> (BTreeMap<ParamKey, Animation>, BTreeMap<String, Value>) {
    let mut orphans = BTreeMap::new();
    let Some(legacy) = &n.legacy else { return (BTreeMap::new(), orphans) };
    let tag = crate::effect_params::tag(n).unwrap_or("");
    let alias = |k: &str| {
        KEYFRAME_ALIASES.iter().find(|(t, from, _)| *t == tag && *from == k).map(|(_, _, to)| (*to).to_owned()).unwrap_or_else(|| k.to_owned())
    };
    let aliased_away = |k: &str| KEYFRAME_ALIASES.iter().any(|(t, from, to)| *t == tag && *to == k && *from != k);
    let mut anim: BTreeMap<ParamKey, Animation> = BTreeMap::new();
    if let Some(keys) = legacy.keyframes.as_ref().and_then(Value::as_object) {
        for (k, v) in keys {
            if let Some((_, param)) = k.split_once(':') {
                if aliased_away(param) {
                    orphans.insert(k.clone(), v.clone());
                    continue;
                }
                let a = anim.entry(ParamKey::new(alias(param))).or_insert(Animation { keys: Vec::new(), modifiers: None });
                a.keys = v.as_array().cloned().unwrap_or_default();
            }
        }
    }
    for (k, stack) in &legacy.modifiers {
        if let Some(param) = k.splitn(3, ':').nth(2) {
            if aliased_away(param) {
                orphans.insert(k.clone(), stack.clone());
                continue;
            }
            let a = anim.entry(ParamKey::new(alias(param))).or_insert(Animation { keys: Vec::new(), modifiers: None });
            a.modifiers = Some(stack.clone());
        }
    }
    (anim, orphans)
}

/// An entry of a track's effect list with its keyframes and modifier
/// stacks on its node, read as a clip's own entries are read
/// ([`effect_anim`]), so one reader serves every effect node a render
/// copy holds. Keys that drive nothing are kept on the node; the track's
/// own lift notes them.
pub(crate) fn track_effect(n: &Node) -> Result<Node, String> {
    let mut e = n.clone();
    let (anim, orphans) = effect_anim(n);
    e.anim = anim;
    keep_orphans(&mut e, orphans, &mut Vec::new());
    crate::effect_params::into_memory(&mut e)?;
    Ok(e)
}

/// Keys an effect holds that drive nothing, kept on its node and noted.
fn keep_orphans(n: &mut Node, orphans: BTreeMap<String, Value>, notes: &mut Vec<LiftNote>) {
    if !orphans.is_empty() {
        notes.push(LiftNote::OrphanKeys { node: Some(n.id), keys: orphans.keys().cloned().collect() });
        flags(n).orphan_keys.extend(orphans);
    }
}

/// The opacity with its animation. A still opacity is clamped to 0..1 as
/// the renderer clamps it; an animated one keeps its value, since the
/// renderer clamps only after the keys and modifiers have run.
fn settled(v: f32, anim: Option<Animation>) -> (f32, Option<Animation>) {
    match anim {
        None => (v.clamp(0.0, 1.0), None),
        a => (v, a),
    }
}

fn stack_modifiers(stack: Option<&Value>) -> Vec<Value> {
    stack.and_then(|s| s.get("modifiers")).and_then(Value::as_array).cloned().unwrap_or_default()
}

fn is_time_modifier(m: &Value) -> bool {
    m.get("type").and_then(Value::as_str).is_some_and(|t| TIME_MODIFIERS.contains(&t))
}

/// An animation from keys and a modifier list, or none when both are empty.
fn animation(keys: Option<&Value>, modifiers: Vec<Value>) -> Option<Animation> {
    let keys: Vec<Value> = keys.and_then(Value::as_array).cloned().unwrap_or_default();
    if keys.is_empty() && modifiers.is_empty() {
        return None;
    }
    let modifiers = (!modifiers.is_empty()).then(|| json!({ "modifiers": modifiers }));
    Some(Animation { keys, modifiers })
}

/// The Opacity entry that is the clip's opacity property: the last enabled
/// one, on a clip and an adjustment layer alike; a disabled one never
/// counts. Every other enabled Opacity multiplies the clip's opacity,
/// wherever it sits (#1386, picture revision 4).
pub fn opacity_anchor(effects: &[&Node]) -> Option<NodeId> {
    effects.iter().rfind(|n| n.type_id.as_str() == types::LEGACY_OPACITY && n.enabled).map(|n| n.id)
}

/// The clip's opacity, by one rule for a clip and an adjustment layer
/// (`render.rs` `clip_opacity`).
fn opacity(record: &ClipRecordV2, effects: &[&Node], notes: &mut Vec<LiftNote>, clip_orphans: &mut BTreeMap<String, Value>) -> (f32, Option<Animation>) {
    let lc = &record.legacy_clip;
    let clip_keys = lc.fields.get("opacity_keyframes");
    let clip_mods = stack_modifiers(lc.param_modifiers.get("opacity"));
    let opacities: Vec<&&Node> = effects.iter().filter(|n| n.type_id.as_str() == types::LEGACY_OPACITY).collect();
    let value = |n: &Node| num(payload(n), "value").unwrap_or(1.0);
    let slot_keys = |n: &Node| -> (Option<Value>, Option<Value>) {
        let l = n.legacy.as_ref();
        let slot = l.and_then(|l| l.slot).unwrap_or(0);
        let keys = l.and_then(|l| l.keyframes.as_ref()).and_then(|k| k.get(format!("{slot}:value"))).cloned();
        let mods = l.and_then(|l| l.modifiers.get(&format!("effect:{slot}:value"))).cloned();
        (keys, mods)
    };
    let orphan = |n: &Node, clip_orphans: &mut BTreeMap<String, Value>, notes: &mut Vec<LiftNote>| {
        let l = n.legacy.as_ref();
        let mut keys = Vec::new();
        for (k, v) in l.and_then(|l| l.keyframes.as_ref()).and_then(Value::as_object).into_iter().flatten() {
            clip_orphans.insert(k.clone(), v.clone());
            keys.push(k.clone());
        }
        for (k, v) in l.map(|l| &l.modifiers).into_iter().flatten() {
            clip_orphans.insert(k.clone(), v.clone());
            keys.push(k.clone());
        }
        if !keys.is_empty() {
            notes.push(LiftNote::OrphanKeys { node: Some(n.id), keys });
        }
    };

    let enabled: Vec<&&&Node> = opacities.iter().filter(|n| n.enabled).collect();
    for n in opacities.iter().filter(|n| !n.enabled) {
        // A disabled Opacity draws nothing; an enabled one other than the
        // anchor multiplies in with its own keys (#1386), kept on it.
        orphan(n, clip_orphans, notes);
    }
    match enabled.last() {
        None => (1.0, animation(clip_keys, clip_mods)),
        Some(last) => {
            let (keys, mods) = slot_keys(last);
            let v = value(last);
            if keys.as_ref().and_then(Value::as_array).is_some_and(|k| !k.is_empty()) {
                // Its own keys, with its own modifiers.
                settled(v, animation(keys.as_ref(), stack_modifiers(mods.as_ref())))
            } else {
                // The clip's keys and modifiers, then its value modifiers on
                // top; its time modifiers acted on no keys and never ran.
                let (time, value_mods): (Vec<Value>, Vec<Value>) = stack_modifiers(mods.as_ref()).into_iter().partition(is_time_modifier);
                if !time.is_empty() {
                    let slot = last.legacy.as_ref().and_then(|l| l.slot).unwrap_or(0);
                    let key = format!("effect:{slot}:value#time");
                    clip_orphans.insert(key.clone(), json!({ "modifiers": time }));
                    notes.push(LiftNote::OrphanKeys { node: Some(last.id), keys: vec![key] });
                }
                let mut all = clip_mods;
                all.extend(value_mods);
                settled(v, animation(clip_keys, all))
            }
        }
    }
}

/// The clip's placement values from its first enabled Transform (#1207),
/// and their animation from the clip's transform keyframes.
fn placement(record: &ClipRecordV2, transforms: &[&Node]) -> (PlacementProps, BTreeMap<ParamKey, Animation>) {
    let mut p = PlacementProps::default();
    if let Some(first) = transforms.iter().find(|n| n.enabled) {
        let v = payload(first);
        p.pos_x = num(v, "position_x");
        p.pos_y = num(v, "position_y");
        // A negative scale is a flip (#2705): kept as one, the scale as
        // its size.
        let (sx, sy) = (num(v, "scale_x"), num(v, "scale_y"));
        p.flip_x = sx.is_some_and(|s| s < 0.0);
        p.flip_y = sy.is_some_and(|s| s < 0.0);
        p.scale = sx.map(f32::abs);
        // Its height against its width, where they differ (#1406): none
        // for a width of 0, which draws nothing whatever the height.
        p.aspect = aspect(p.scale, sy.map(f32::abs));
        p.rotation = num(v, "rotation");
        p.anchor_x = num(v, "anchor_x");
        p.anchor_y = num(v, "anchor_y");
        p.tilt_x = num(v, "rotation_x");
        p.tilt_y = num(v, "rotation_y");
        p.perspective = num(v, "perspective");
        p.focal_ref = v.get("focal_ref").filter(|f| !f.is_null()).cloned();
    }

    let lc = &record.legacy_clip;
    let mut anim = BTreeMap::new();
    for (param, field, modifier) in [
        ("pos_x", "transform_pos_x_keyframes", Some("position_x")),
        ("pos_y", "transform_pos_y_keyframes", Some("position_y")),
        ("scale", "transform_scale_keyframes", Some("scale")),
        // The height keyed apart from the width (#1471).
        ("scale_y", "transform_scale_y_keyframes", None),
        ("rotation", "transform_rotation_keyframes", Some("rotation")),
        ("tilt_x", "transform_tilt_x_keyframes", None),
        ("tilt_y", "transform_tilt_y_keyframes", None),
    ] {
        let mods = modifier.map(|m| stack_modifiers(lc.param_modifiers.get(m))).unwrap_or_default();
        if let Some(a) = animation(lc.fields.get(field), mods) {
            anim.insert(ParamKey::new(param), a);
        }
    }
    (p, anim)
}

/// A clip chain's edges: `out → in` between neighbours, and every group
/// with a `frame` input fed the picture entering the grade (what the node
/// before the first group hands on: the source, or the head nodes after it).
pub fn chain_edges(chain: &[Node]) -> Vec<Edge> {
    let mut edges: Vec<Edge> = chain.windows(2).map(|w| Edge { from: (w[0].id, "out".into()), to: (w[1].id, "in".into()) }).collect();
    let first = chain.iter().position(|n| n.type_id.as_str() == lt::GROUP);
    if let Some(feed) = first.and_then(|i| i.checked_sub(1)).map(|i| chain[i].id) {
        for g in chain.iter().filter(|n| n.type_id.as_str() == lt::GROUP) {
            if g.group.as_ref().is_some_and(|b| b.graph.nodes.iter().any(|x| x.type_id.as_str() == lt::GROUP_FRAME)) {
                edges.push(Edge { from: (feed, "out".into()), to: (g.id, "frame".into()) });
            }
        }
    }
    edges
}

/// Lift one clip's record.
pub fn lift_clip(record: &ClipRecordV2, ctx: &LiftCtx) -> Result<(ClipPicture, Vec<LiftNote>), String> {
    let mut notes = Vec::new();
    let mut clip_orphans: BTreeMap<String, Value> = BTreeMap::new();
    let id = |list: &str| entry_id(RootKind::Clip, ctx.clip_id, &format!("lift/{list}"), 0);
    let nodes = &record.graph.nodes;
    // A clip whose file has no grade graph loads today as the default
    // pipeline: written out here, with ids derived from the clip's.
    let default_grade = if nodes.iter().any(|n| n.type_id.as_str() == types::LEGACY_GRADE) {
        None
    } else {
        let with_default = json!({"id": ctx.clip_id.to_string(), "node_graph": default_pipeline(ctx.clip_id)});
        let record = crate::to_v2::clip_to_v2(with_default.as_object().ok_or("default")?).map_err(|e| e.to_string())?;
        let g = record.graph.nodes.into_iter().next().ok_or("no default grade")?;
        notes.push(LiftNote::DefaultPipeline { group: g.id });
        Some(g)
    };
    let grade = nodes.iter().find(|n| n.type_id.as_str() == types::LEGACY_GRADE).or(default_grade.as_ref());
    let ofx: Vec<&Node> = nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_OFX).collect();
    let effects: Vec<&Node> = nodes
        .iter()
        .filter(|n| !matches!(n.type_id.as_str(), types::LEGACY_GRADE | types::LEGACY_OFX))
        .collect();

    // Head: TNR and frame blending, on Media clips only.
    let media = ctx.kind == ClipKind::Media;
    let mut head = Vec::new();
    let mut tnr_taken = false;
    let anchor = opacity_anchor(&effects);
    for n in effects.iter().filter(|n| n.type_id.as_str() == types::LEGACY_TEMPORAL_DENOISE) {
        // The first enabled one is the clip's, whatever its strength: it
        // runs wherever its strength, keyframed or not, is above 0, and
        // none after it is read (`render.rs` `temporal_denoise_of`).
        let runs = media && n.enabled && !tnr_taken;
        tnr_taken |= runs;
        let mut h = node(n.id, lt::TEMPORAL_DENOISE, runs, n.legacy.clone());
        h.legacy_flags = n.legacy_flags.clone();
        if n.enabled != runs {
            flags(&mut h).was_enabled = Some(n.enabled);
        }
        let (anim, orphans) = effect_anim(n);
        h.anim = anim;
        keep_orphans(&mut h, orphans, &mut notes);
        if !runs {
            let why = if media { "a temporal denoise after the first enabled one" } else { "temporal denoise runs on media clips only" };
            notes.push(LiftNote::NeverRan { node: n.id, why: why.into() });
        }
        head.push(h);
    }
    let blend = effects.iter().find(|n| {
        let v = payload(n);
        n.type_id.as_str() == types::LEGACY_SPEED_RAMP && n.enabled && matches!(v.get("blend_mode").and_then(Value::as_u64), Some(1 | 2))
    });
    if let Some(n) = blend {
        let mut b = node(n.id, lt::FRAME_BLEND, media, n.legacy.clone());
        if !media {
            flags(&mut b).was_enabled = Some(true);
        }
        head.push(b);
        if !media {
            notes.push(LiftNote::NeverRan { node: n.id, why: "frame blending runs on media clips only".into() });
        }
    }

    // The clip's opacity and placement become properties.
    let (opacity, opacity_anim) = opacity(record, &effects, &mut notes, &mut clip_orphans);
    let transforms: Vec<&Node> = effects.iter().copied().filter(|n| n.type_id.as_str() == types::LEGACY_TRANSFORM).collect();
    let (placement, placement_anim) = placement(record, &transforms);
    let fixed_at = transforms.iter().find(|n| n.enabled).map(|n| n.id);

    // The chain.
    let mut chain = vec![node(id("source"), lt::SOURCE, true, None)];
    chain.extend(head);
    if let Some(g) = grade {
        repeated_id(g)?;
        let (body, group_flags, loops) = lift_grade_body(g, true, &mut notes);
        // A grade graph that loops never ran: today's ordering refuses it
        // and the picture passes through. Kept in a group that is off,
        // which renders the same.
        let runs = !loops;
        if !runs {
            notes.push(LiftNote::NeverRan { node: g.id, why: "its grade graph loops, so today's renderer never ran it".into() });
        }
        let mut group = node(g.id, lt::GROUP, runs, g.legacy.clone());
        group.group = Some(Box::new(GroupBody { graph: body, ports: Vec::new(), interface: Vec::new() }));
        if !group_flags.is_empty() {
            group.legacy_flags = Some(group_flags);
        }
        chain.push(group);
    }
    chain.push(node(id("scene_to_display"), lt::SCENE_TO_DISPLAY, true, None));
    chain.extend(ofx.iter().map(|n| (*n).clone()));
    for n in &effects {
        match n.type_id.as_str() {
            // In the head, above.
            types::LEGACY_TEMPORAL_DENOISE => {}
            types::LEGACY_SPEED_RAMP if blend.is_some_and(|b| b.id == n.id) => {}
            // Kept off, for the record: every Opacity entry (the clip's
            // opacity property is what renders) and a speed ramp that
            // doesn't blend frames. The user put each in the list.
            // An enabled Opacity other than the clip's anchor multiplies the
            // clip's opacity (#1386): kept on, with its own keys.
            types::LEGACY_OPACITY if n.enabled && Some(n.id) != anchor => chain.push((*n).clone()),
            types::LEGACY_OPACITY | types::LEGACY_SPEED_RAMP => {
                let mut k = (*n).clone();
                k.enabled = false;
                if n.enabled {
                    flags(&mut k).was_enabled = Some(true);
                }
                chain.push(k);
            }
            types::LEGACY_TRANSFORM if Some(n.id) == fixed_at => {
                let mut p = node(n.id, lt::PLACEMENT, true, n.legacy.clone());
                p.params.insert("mode".into(), ParamValue::Enum(lt::PLACEMENT_FIXED));
                chain.push(p);
            }
            types::LEGACY_TRANSFORM => {
                let mut t = (*n).clone();
                t.enabled = false;
                if n.enabled {
                    flags(&mut t).was_enabled = Some(true);
                }
                notes.push(LiftNote::ExtraTransform { node: n.id });
                chain.push(t);
            }
            _ => {
                let mut e = (*n).clone();
                let (anim, orphans) = effect_anim(n);
                e.anim = anim;
                keep_orphans(&mut e, orphans, &mut notes);
                chain.push(e);
            }
        }
    }
    if fixed_at.is_none() {
        let mut p = node(id("placement"), lt::PLACEMENT, true, None);
        p.params.insert("mode".into(), ParamValue::Enum(lt::PLACEMENT_AUTO));
        chain.push(p);
    }
    chain.push(node(id("output"), lt::OUTPUT, true, None));

    // What claims nothing on the clip: unclaimed effect keys and modifiers.
    let lc = &record.legacy_clip;
    for (k, v) in &lc.effect_keyframes {
        clip_orphans.insert(k.clone(), v.clone());
    }
    for (k, v) in lc.param_modifiers.iter().filter(|(k, _)| !matches!(k.as_str(), "opacity" | "position_x" | "position_y" | "scale" | "rotation")) {
        clip_orphans.insert(k.clone(), v.clone());
    }
    let unclaimed: Vec<String> = lc.effect_keyframes.keys().cloned().chain(lc.param_modifiers.keys().filter(|k| !matches!(k.as_str(), "opacity" | "position_x" | "position_y" | "scale" | "rotation")).cloned()).collect();
    if !unclaimed.is_empty() {
        notes.push(LiftNote::OrphanKeys { node: None, keys: unclaimed });
    }
    if !clip_orphans.is_empty() {
        flags(&mut chain[0]).orphan_keys = clip_orphans;
    }
    if !lc.is_empty() {
        flags(&mut chain[0]).clip_anim = Some(lc.clone());
    }

    let edges = chain_edges(&chain);
    let output = chain.last().map(|n| n.id).unwrap_or_default();
    let graph = Graph { graph_version: GRAPH_VERSION, nodes: chain, edges, output, ui: GraphUi::default(), working_space_override: None };
    let mut picture = ClipPicture { graph, placement, placement_anim, opacity, opacity_anim, requires: Vec::new() };
    // Each effect's values held in its map from here on (M3-2).
    crate::effect_params::picture_into_memory(&mut picture)?;
    Ok((picture, notes))
}

#[cfg(test)]
mod placement_bypass_tests {
    use super::*;

    fn transform(enabled: bool, x: f32, focal: Value) -> Node {
        node(Uuid::new_v4(), types::LEGACY_TRANSFORM, enabled, Some(Legacy {
            slot: None, keyframes: None, modifiers: BTreeMap::new(),
            payload: json!({"type":"transform", "position_x":x, "position_y":x+1.0,
                "scale_x":x+2.0, "scale_y":x+2.0, "rotation":x+3.0,
                "anchor_x":x+4.0, "anchor_y":x+5.0, "rotation_x":x+6.0,
                "rotation_y":x+7.0, "perspective":x+8.0, "focal_ref":focal}),
        }))
    }

    #[test]
    fn every_placement_value_comes_from_the_first_enabled_transform() {
        let mut record = ClipRecordV2 { graph: Graph {
            graph_version: klipp_graph_format::GRAPH_VERSION, nodes: Vec::new(), edges: Vec::new(),
            output: Uuid::nil(), ui: Default::default(), working_space_override: None,
        }, legacy_clip: Default::default() };
        record.legacy_clip.fields.insert("transform_pos_x_keyframes".into(), json!([{"time_secs":0.0,"value":4.0}]));
        let off = transform(false, 20.0, json!({"old":"disabled"}));
        let on = transform(true, 2.0, Value::Null);
        let later = transform(true, 30.0, json!({"old":"later"}));
        let absent = placement(&record, &[]);
        assert_eq!(placement(&record, &[&off]), absent);
        let expected = placement(&record, &[&on]);
        assert_eq!(placement(&record, &[&off, &on, &later]), expected);
        assert_eq!(expected.0.pos_x, Some(2.0));
        assert_eq!(expected.0.tilt_x, Some(8.0));
        assert_eq!(expected.0.perspective, Some(10.0));
        assert_eq!(expected.0.focal_ref, None, "later focal refs cannot override the selected Transform");
        assert_eq!(expected.1, absent.1, "clip animation survives bypass");
    }
}
