//! The structural conversion: each picture store of a v1 project, read as
//! the file's JSON, becomes a revision-1 [`Graph`] that loses nothing.
//!
//! Nothing is reinterpreted. A store's graph is its grade graph as a
//! `klipp.legacy.grade` group (when the file has one), then its OFX
//! entries, then its built-in effects, in list order, chained `out → in`;
//! the graph's output is the last node, and a store with none of these is
//! [`Graph::empty`]. Every entry keeps its v1 JSON verbatim as its node's
//! payload. `to_v1`, the inverse, proves it.
//!
//! The v1 JSON is taken after `canon()`'s view of empty containers: an
//! absent, `null` or empty list or map holds nothing, and a `null` grade
//! graph is no grade graph (both read as the default pipeline).

use std::collections::BTreeMap;

use klipp_graph_format::{
    ports, types, ClipRecordV2, Edge, Graph, GraphUi, GroupBody, Legacy, LegacyClipAnim, Node, NodeId, NodeTypeId,
    GRAPH_VERSION,
};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::ids::{entry_id, RootKind};

/// Why a store can't be converted. The project stops, and is reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToV2Error {
    /// An id that should be a UUID isn't one.
    NotAnId { what: &'static str, value: String },
    /// A field has a shape today's reader would refuse too.
    Shape { what: &'static str, field: &'static str },
}

impl std::fmt::Display for ToV2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnId { what, value } => write!(f, "{what} id {value:?} isn't a UUID"),
            Self::Shape { what, field } => write!(f, "{what}: {field} isn't shaped as today's reader expects"),
        }
    }
}

impl std::error::Error for ToV2Error {}

/// A store's root: its kind, its id, and the prefix of its lists inside
/// that root (empty for the root's own graph).
#[derive(Clone, Copy)]
struct Root<'a> {
    kind: RootKind,
    id: Uuid,
    prefix: &'a str,
}

impl Root<'_> {
    fn id_of(&self, list: &str, slot: usize) -> NodeId {
        entry_id(self.kind, self.id, &format!("{}{list}", self.prefix), slot)
    }
}

/// The clip fields of `LegacyClipAnim::FIELDS` and the modifier keys that
/// stay clip fields.
const AUDIO_MODIFIERS: [&str; 3] = ["volume", "pan", "gain"];

fn parse_id(what: &'static str, v: Option<&Value>) -> Result<Uuid, ToV2Error> {
    let s = v.and_then(Value::as_str).unwrap_or_default();
    Uuid::parse_str(s).map_err(|_| ToV2Error::NotAnId { what, value: s.to_owned() })
}

/// The entries of a `Vec` field with a serde default: absent is no
/// entries, and `null` is refused, as today's reader refuses it.
fn vec<'a>(o: &'a Map<String, Value>, field: &'static str, what: &'static str) -> Result<&'a [Value], ToV2Error> {
    match o.get(field) {
        None => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        Some(_) => Err(ToV2Error::Shape { what, field }),
    }
}

/// The entries of a required `Vec` field.
fn required_vec<'a>(o: &'a Map<String, Value>, field: &'static str, what: &'static str) -> Result<&'a [Value], ToV2Error> {
    o.get(field).and_then(Value::as_array).map(Vec::as_slice).ok_or(ToV2Error::Shape { what, field })
}

/// The entries of an `Option` list field, `canon()`-style: absent, `null`
/// and `[]` are no entries.
fn list<'a>(o: &'a Map<String, Value>, field: &'static str, what: &'static str) -> Result<&'a [Value], ToV2Error> {
    match o.get(field) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        Some(_) => Err(ToV2Error::Shape { what, field }),
    }
}

/// The entries of a map field: absent, `null` and `{}` are none.
fn map<'a>(o: &'a Map<String, Value>, field: &'static str, what: &'static str) -> Result<Option<&'a Map<String, Value>>, ToV2Error> {
    match o.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => Ok(Some(m)),
        Some(_) => Err(ToV2Error::Shape { what, field }),
    }
}

/// A grade graph, when the field holds one: absent and `null` are none.
fn grade_of<'a>(o: &'a Map<String, Value>, field: &'static str, what: &'static str) -> Result<Option<&'a Map<String, Value>>, ToV2Error> {
    map(o, field, what)
}

fn legacy_node(id: NodeId, type_id: &str, enabled: bool, legacy: Legacy) -> Node {
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
        legacy: Some(legacy),
        legacy_flags: None,
        origin: None,
    }
}

/// The node type a built-in effect becomes, by its v1 `type` tag.
fn effect_type(payload: &Value) -> &'static str {
    match payload.get("type").and_then(Value::as_str) {
        Some("opacity") => types::LEGACY_OPACITY,
        Some("transform") => types::LEGACY_TRANSFORM,
        Some("speed_ramp") => types::LEGACY_SPEED_RAMP,
        Some("temporal_denoise") => types::LEGACY_TEMPORAL_DENOISE,
        _ => types::LEGACY_EFFECT,
    }
}

/// The kind of the first node with `id` in a v1 graph, or `""` when none
/// has it: both directions of the conversion use the same rule, so an edge
/// to a missing node keeps its port.
fn kind_of<'a>(nodes: &'a [Value], id: &str) -> &'a str {
    nodes
        .iter()
        .find(|n| n.get("id").and_then(Value::as_str) == Some(id))
        .and_then(|n| n.get("kind").and_then(Value::as_str))
        .unwrap_or("")
}

/// A v1 grade graph as the body of a `klipp.legacy.grade` group, kept as
/// saved: odd edges, a missing output and repeated ids included.
fn grade_body(g: &Map<String, Value>, what: &'static str) -> Result<Graph, ToV2Error> {
    let shape = |field| ToV2Error::Shape { what, field };
    let nodes = required_vec(g, "nodes", what)?;
    let mut out = Vec::with_capacity(nodes.len());
    for n in nodes {
        let n = n.as_object().ok_or(shape("nodes"))?;
        let id = parse_id(what, n.get("id"))?;
        let kind = n.get("kind").cloned().ok_or(shape("nodes.kind"))?;
        let kind_name = kind.as_str().unwrap_or("").to_owned();
        let mut payload = Map::new();
        payload.insert("kind".into(), kind);
        for key in ["label", "params"] {
            if let Some(v) = n.get(key) {
                payload.insert(key.into(), v.clone());
            }
        }
        let enabled = match n.get("enabled") {
            Some(Value::Bool(b)) => *b,
            _ => return Err(shape("nodes.enabled")),
        };
        let mut node = legacy_node(
            id,
            types::LEGACY_NODE,
            enabled,
            Legacy { slot: None, payload: Value::Object(payload), keyframes: n.get("keyframes").cloned(), modifiers: BTreeMap::new() },
        );
        match n.get("output_link") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => node.wireless.output_link = Some(s.clone()),
            Some(_) => return Err(shape("nodes.output_link")),
        }
        for (i, link) in vec(n, "input_links", what)?.iter().enumerate() {
            match link {
                Value::Null => {}
                Value::String(s) => {
                    let port = u8::try_from(i).map_err(|_| shape("nodes.input_links"))?;
                    node.wireless.input_links.insert(ports::input_key(&kind_name, port), s.clone());
                }
                _ => return Err(shape("nodes.input_links")),
            }
        }
        out.push(node);
    }
    // Edges in the order saved: where two feed one port, the first is the
    // one the executor reads (`NodeGraph::input_of`), so order is meaning.
    let v1_edges = required_vec(g, "edges", what)?;
    let mut edges = Vec::new();
    for e in v1_edges {
        let field = |k: &str| e.get(k).cloned().unwrap_or(Value::Null);
        let (from, to) = (field("from_node"), field("to_node"));
        let (from_s, to_s) = (from.as_str().unwrap_or(""), to.as_str().unwrap_or(""));
        let port = |k: &str| e.get(k).and_then(Value::as_u64).and_then(|p| u8::try_from(p).ok()).ok_or(shape("edges"));
        edges.push(Edge {
            from: (parse_id(what, Some(&from))?, ports::output_key(kind_of(nodes, from_s), port("from_port")?)),
            to: (parse_id(what, Some(&to))?, ports::input_key(kind_of(nodes, to_s), port("to_port")?)),
        });
    }
    let mut ui = GraphUi::default();
    if let Some(pos) = match g.get("node_positions") {
        None => None,
        Some(Value::Object(m)) => Some(m),
        Some(_) => return Err(shape("node_positions")),
    } {
        for (id, p) in pos {
            ui.positions.insert(parse_id(what, Some(&Value::String(id.clone())))?, p.clone());
        }
    }
    ui.frames = vec(g, "frames", what)?.to_vec();
    ui.sticky_notes = vec(g, "sticky_notes", what)?.to_vec();
    Ok(Graph {
        graph_version: GRAPH_VERSION,
        nodes: out,
        edges,
        output: parse_id(what, g.get("output_node_id"))?,
        ui,
        working_space_override: g.get("working_space_override").cloned(),
    })
}

/// The `klipp.legacy.grade` group node around a v1 grade graph.
fn grade_group(root: Root, g: &Map<String, Value>, what: &'static str) -> Result<Node, ToV2Error> {
    let mut node = legacy_node(root.id_of("grade", 0), types::LEGACY_GRADE, true, Legacy {
        slot: None,
        payload: Value::Null,
        keyframes: None,
        modifiers: BTreeMap::new(),
    });
    node.group = Some(Box::new(GroupBody { graph: grade_body(g, what)?, ports: Vec::new(), interface: Vec::new() }));
    Ok(node)
}

/// A store's built-in effects: each entry, its enabled flag (a missing
/// one reads `true`), and the keyframes and modifiers its slot claims.
struct Effects<'a> {
    list: &'a [Value],
    enabled: &'a [Value],
    keyframes: BTreeMap<usize, BTreeMap<String, Value>>,
    modifiers: BTreeMap<usize, BTreeMap<String, Value>>,
}

/// Chain the nodes of one store: its grade group, then its OFX entries,
/// then its built-in effects, each linked `out → in`.
fn chain(root: Root, grade: Option<&Map<String, Value>>, ofx: &[Value], fx: Effects, what: &'static str) -> Result<Graph, ToV2Error> {
    let mut nodes = Vec::new();
    if let Some(g) = grade {
        nodes.push(grade_group(root, g, what)?);
    }
    for (slot, entry) in ofx.iter().enumerate() {
        nodes.push(legacy_node(root.id_of("ofx", slot), types::LEGACY_OFX, true, Legacy {
            slot: Some(slot as u32),
            payload: entry.clone(),
            keyframes: None,
            modifiers: BTreeMap::new(),
        }));
    }
    let Effects { list, enabled, mut keyframes, mut modifiers } = fx;
    for (slot, entry) in list.iter().enumerate() {
        let on = match enabled.get(slot) {
            None => true,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(ToV2Error::Shape { what, field: "builtin_effects_enabled" }),
        };
        let keys = keyframes.remove(&slot).map(|m| Value::Object(m.into_iter().collect()));
        nodes.push(legacy_node(root.id_of("effects", slot), effect_type(entry), on, Legacy {
            slot: Some(slot as u32),
            payload: entry.clone(),
            keyframes: keys,
            modifiers: modifiers.remove(&slot).unwrap_or_default(),
        }));
    }
    let Some(last) = nodes.last().map(|n| n.id) else {
        return Ok(Graph::empty());
    };
    let edges = nodes.windows(2).map(|w| Edge { from: (w[0].id, "out".into()), to: (w[1].id, "in".into()) }).collect();
    Ok(Graph { graph_version: GRAPH_VERSION, nodes, edges, output: last, ui: GraphUi::default(), working_space_override: None })
}

/// The built-in slot an `effect_keyframes` key `"<slot>:<param>"` names,
/// if it names one of `len`.
fn keyframe_slot(key: &str, len: usize) -> Option<usize> {
    let (slot, _param) = key.split_once(':')?;
    slot.parse::<usize>().ok().filter(|s| *s < len)
}

/// The built-in slot a modifier key `effect:<slot>:<param>` names, if it
/// names one of `len` (the split today's `ParamId::from_key` uses).
fn modifier_slot(key: &str, len: usize) -> Option<usize> {
    let parts: Vec<&str> = key.splitn(3, ':').collect();
    if parts.len() != 3 || parts[0] != "effect" {
        return None;
    }
    parts[1].parse::<usize>().ok().filter(|s| *s < len)
}

/// A clip's picture as its revision-1 record. The fields it takes are the
/// grade graph, OFX list, built-in effects with their flags and keyframes,
/// the clip's opacity and transform keyframe fields, and the picture
/// entries of its modifier stacks; everything else stays a clip field.
pub fn clip_to_v2(clip: &Map<String, Value>) -> Result<ClipRecordV2, ToV2Error> {
    const WHAT: &str = "clip";
    let root = Root { kind: RootKind::Clip, id: parse_id(WHAT, clip.get("id"))?, prefix: "" };
    let effects = list(clip, "builtin_effects", WHAT)?;
    let mut legacy_clip = LegacyClipAnim::default();
    let mut keyframes: BTreeMap<usize, BTreeMap<String, Value>> = BTreeMap::new();
    if let Some(m) = map(clip, "effect_keyframes", WHAT)? {
        for (k, v) in m {
            match keyframe_slot(k, effects.len()) {
                Some(slot) => {
                    keyframes.entry(slot).or_default().insert(k.clone(), v.clone());
                }
                None => {
                    legacy_clip.effect_keyframes.insert(k.clone(), v.clone());
                }
            }
        }
    }
    let mut modifiers: BTreeMap<usize, BTreeMap<String, Value>> = BTreeMap::new();
    if let Some(m) = map(clip, "param_modifiers", WHAT)? {
        for (k, v) in m.iter().filter(|(k, _)| !AUDIO_MODIFIERS.contains(&k.as_str())) {
            match modifier_slot(k, effects.len()) {
                Some(slot) => {
                    modifiers.entry(slot).or_default().insert(k.clone(), v.clone());
                }
                None => {
                    legacy_clip.param_modifiers.insert(k.clone(), v.clone());
                }
            }
        }
    }
    for field in LegacyClipAnim::FIELDS {
        if let Some(v) = clip.get(field) {
            legacy_clip.fields.insert(field.to_owned(), v.clone());
        }
    }
    let fx = Effects { list: effects, enabled: vec(clip, "builtin_effects_enabled", WHAT)?, keyframes, modifiers };
    let graph = chain(root, grade_of(clip, "node_graph", WHAT)?, vec(clip, "effects", WHAT)?, fx, WHAT)?;
    Ok(ClipRecordV2 { graph, legacy_clip })
}

/// A track's picture: its `effects.grade` and `effects.video` (with
/// `video_enabled`). Its audio chain stays a track field.
pub fn track_to_v2(track: &Map<String, Value>) -> Result<Graph, ToV2Error> {
    const WHAT: &str = "track";
    let root = Root { kind: RootKind::Track, id: parse_id(WHAT, track.get("id"))?, prefix: "" };
    let Some(effects) = map(track, "effects", WHAT)? else {
        return Ok(Graph::empty());
    };
    let fx = Effects { list: list(effects, "video", WHAT)?, enabled: vec(effects, "video_enabled", WHAT)?, keyframes: BTreeMap::new(), modifiers: BTreeMap::new() };
    chain(root, grade_of(effects, "grade", WHAT)?, &[], fx, WHAT)
}

fn grade_only(root: Root, graph: Option<&Map<String, Value>>, what: &'static str) -> Result<Graph, ToV2Error> {
    let none = Effects { list: &[], enabled: &[], keyframes: BTreeMap::new(), modifiers: BTreeMap::new() };
    chain(root, graph, &[], none, what)
}

/// A sequence's timeline grade, when it has one.
pub fn timeline_to_v2(sequence: &Map<String, Value>) -> Result<Option<Graph>, ToV2Error> {
    const WHAT: &str = "sequence";
    let Some(g) = grade_of(sequence, "timeline_graph", WHAT)? else {
        return Ok(None);
    };
    let root = Root { kind: RootKind::Sequence, id: parse_id(WHAT, sequence.get("id"))?, prefix: "timeline/" };
    grade_only(root, Some(g), WHAT).map(Some)
}

/// A grade group's two stages, each kept as present or not.
pub fn stages_to_v2(group: &Map<String, Value>) -> Result<(Option<Graph>, Option<Graph>), ToV2Error> {
    const WHAT: &str = "grade group";
    let id = parse_id(WHAT, group.get("id"))?;
    let stage = |field: &'static str, prefix: &'static str| -> Result<Option<Graph>, ToV2Error> {
        match grade_of(group, field, WHAT)? {
            None => Ok(None),
            Some(g) => grade_only(Root { kind: RootKind::GradeGroup, id, prefix }, Some(g), WHAT).map(Some),
        }
    };
    Ok((stage("pre", "pre/")?, stage("post", "post/")?))
}

/// A clip's grade version `n`: its graph as a grade group. The version's
/// `name` stays beside it.
pub fn version_to_v2(clip_id: Uuid, n: usize, version: &Map<String, Value>) -> Result<Graph, ToV2Error> {
    const WHAT: &str = "grade version";
    let prefix = format!("version/{n}/");
    let root = Root { kind: RootKind::Clip, id: clip_id, prefix: &prefix };
    let graph = version.get("node_graph").and_then(Value::as_object).ok_or(ToV2Error::Shape { what: WHAT, field: "node_graph" })?;
    grade_only(root, Some(graph), WHAT)
}

/// A project Look (`looks/<id>.json`): its graph as a grade group.
pub fn look_to_v2(look: &Map<String, Value>) -> Result<Graph, ToV2Error> {
    const WHAT: &str = "look";
    let root = Root { kind: RootKind::Look, id: parse_id(WHAT, look.get("id"))?, prefix: "" };
    let graph = look.get("node_graph").and_then(Value::as_object).ok_or(ToV2Error::Shape { what: WHAT, field: "node_graph" })?;
    grade_only(root, Some(graph), WHAT)
}

/// A grade still's grade (an entry of `media/stills.json`), when it has
/// one. Stills have no id; the root is the nil id and the list carries
/// the still's file name, which is unique in its project.
pub fn still_to_v2(still: &Map<String, Value>) -> Result<Option<Graph>, ToV2Error> {
    const WHAT: &str = "grade still";
    let Some(grade) = grade_of(still, "grade", WHAT)? else {
        return Ok(None);
    };
    let name = still.get("filename").and_then(Value::as_str).ok_or(ToV2Error::Shape { what: WHAT, field: "filename" })?;
    let prefix = format!("{name}/");
    grade_only(Root { kind: RootKind::Still, id: Uuid::nil(), prefix: &prefix }, Some(grade), WHAT).map(Some)
}

/// A colour preset: a whole grade graph saved as its own file
/// (`presets/color/<name>.json`), as a grade group. Rooted at the nil id
/// with the preset's file name in the list.
pub fn colour_preset_to_v2(file_name: &str, graph: &Map<String, Value>) -> Result<Graph, ToV2Error> {
    let prefix = format!("{file_name}/");
    grade_only(Root { kind: RootKind::Preset, id: Uuid::nil(), prefix: &prefix }, Some(graph), "colour preset")
}
