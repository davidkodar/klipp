//! `to_v1`: a revision-1 record back to the v1 picture it came from, the
//! oracle that proves the conversion loses nothing (#1061). It lives only
//! here and in the tests: nothing writes a file with it.
//!
//! It reads the layout `to_v2` writes (fixed on #1002): a clip's graph is
//! the `klipp.legacy.grade` group (when the clip had a `node_graph`),
//! then its OFX entries, then its built-in effects, in v1 order, each
//! carrying its old entry verbatim. The order of the nodes is the order
//! of the lists; the chain's edges carry nothing back. It refuses
//! anything `to_v2` does not write: a node outside the legacy vocabulary,
//! one out of order or in the wrong slot, a port key a kind does not have.
//!
//! The result is JSON in [`crate::canon`]'s form, so the property is
//! `to_v1(to_v2(x)) == canon(x)`.

use klipp_graph_format::{ports, types, Graph, Node, NodeId, PortKey};
use serde_json::{Map, Value};

use crate::canon::{canon, Store};

/// Why a record is not one `to_v2` writes.
pub type Refusal = String;

/// A revision-1 clip record (`ClipRecordV2` as JSON) back to the clip's v1
/// picture fields.
pub fn clip(record: &Value) -> Result<Value, Refusal> {
    let graph: Graph = serde_json::from_value(record.get("graph").cloned().ok_or("a clip record without a graph")?).map_err(|e| format!("the record's graph: {e}"))?;
    let mut out = Map::new();
    let mut lists = Lists::default();
    lists.read(&graph, &mut out, Allow::Clip)?;
    let anim = record.get("legacy_clip").cloned().unwrap_or(Value::Null);
    if let Some(Value::Object(fields)) = anim.get("fields") {
        for (k, v) in fields {
            out.insert(k.clone(), v.clone());
        }
    }
    if let Some(Value::Object(keys)) = anim.get("effect_keyframes") {
        lists.keyframes.extend(keys.clone());
    }
    if let Some(Value::Object(mods)) = anim.get("param_modifiers") {
        lists.modifiers.extend(mods.clone());
    }
    lists.write(&mut out, "effects", "builtin_effects", "builtin_effects_enabled");
    if !lists.keyframes.is_empty() {
        out.insert("effect_keyframes".into(), Value::Object(lists.keyframes));
    }
    if !lists.modifiers.is_empty() {
        out.insert("param_modifiers".into(), Value::Object(lists.modifiers));
    }
    // In canonical form, which `canon()` leaves as it is: the grade's
    // edges in order.
    Ok(canon(Store::Clip, &Value::Object(out)))
}

/// A track's revision-1 graph back to its v1 `effects` (`grade`, `video`,
/// `video_enabled`).
pub fn track(graph: &Graph) -> Result<Value, Refusal> {
    let mut out = Map::new();
    let mut lists = Lists::default();
    lists.read(graph, &mut out, Allow::Track)?;
    if let Some(g) = out.remove("node_graph") {
        out.insert("grade".into(), g);
    }
    if !lists.keyframes.is_empty() || !lists.modifiers.is_empty() {
        return Err("a track's effects carry keyframes or modifiers, which v1 tracks do not have".into());
    }
    lists.write(&mut out, "", "video", "video_enabled");
    Ok(canon(Store::Track, &Value::Object(out)))
}

/// A graph holding only a grade (a timeline grade, a group stage, a grade
/// version's, a Look's or a still's graph) back to the v1 grade graph.
pub fn grade_only(graph: &Graph) -> Result<Value, Refusal> {
    match graph.nodes.as_slice() {
        [n] if n.type_id.as_str() == types::LEGACY_GRADE => grade_body(n).map(|g| canon(Store::Graph, &g)),
        _ => Err(format!("a grade-only graph holds {} nodes, not one grade group", graph.nodes.len())),
    }
}

/// What a store's graph may hold besides its grade.
#[derive(Clone, Copy, PartialEq)]
enum Allow {
    Clip,
    Track,
}

/// The lists a graph's nodes fill, in the order `to_v2` writes them.
#[derive(Default)]
struct Lists {
    ofx: Vec<Value>,
    effects: Vec<Value>,
    flags: Vec<Value>,
    keyframes: Map<String, Value>,
    modifiers: Map<String, Value>,
}

/// Where in the chain a node may come: the grade first, then OFX, then
/// built-in effects.
fn stage(type_id: &str) -> Option<u8> {
    match type_id {
        types::LEGACY_GRADE => Some(0),
        types::LEGACY_OFX => Some(1),
        types::LEGACY_EFFECT | types::LEGACY_OPACITY | types::LEGACY_TRANSFORM | types::LEGACY_SPEED_RAMP | types::LEGACY_TEMPORAL_DENOISE => Some(2),
        _ => None,
    }
}

/// The legacy node type `to_v2` gives a built-in effect, by its v1 `type`.
fn effect_type(payload: &Value) -> &'static str {
    match payload.get("type").and_then(Value::as_str) {
        Some("opacity") => types::LEGACY_OPACITY,
        Some("transform") => types::LEGACY_TRANSFORM,
        Some("speed_ramp") => types::LEGACY_SPEED_RAMP,
        Some("temporal_denoise") => types::LEGACY_TEMPORAL_DENOISE,
        _ => types::LEGACY_EFFECT,
    }
}

impl Lists {
    fn read(&mut self, graph: &Graph, out: &mut Map<String, Value>, allow: Allow) -> Result<(), Refusal> {
        let mut last = 0u8;
        for n in &graph.nodes {
            let t = n.type_id.as_str();
            let s = stage(t).ok_or_else(|| format!("node {} is a {t}, which revision 1 does not hold here", n.id))?;
            if s < last || (s == 0 && out.contains_key("node_graph")) {
                return Err(format!("node {} ({t}) is out of order", n.id));
            }
            last = s;
            let legacy = n.legacy.as_ref().ok_or_else(|| format!("node {} carries no old entry", n.id))?;
            match t {
                types::LEGACY_GRADE => {
                    out.insert("node_graph".into(), grade_body(n)?);
                }
                types::LEGACY_OFX => {
                    if allow == Allow::Track {
                        return Err(format!("node {} is a plug-in on a track, which v1 tracks do not hold", n.id));
                    }
                    check_slot(n, legacy.slot, self.ofx.len())?;
                    if !n.enabled {
                        return Err(format!("plug-in node {} is switched off on the node; its payload holds the flag", n.id));
                    }
                    self.ofx.push(legacy.payload.clone());
                }
                _ => {
                    check_slot(n, legacy.slot, self.effects.len())?;
                    if effect_type(&legacy.payload) != t {
                        return Err(format!("node {} is a {t} holding a {:?}", n.id, legacy.payload.get("type")));
                    }
                    self.effects.push(legacy.payload.clone());
                    self.flags.push(Value::Bool(n.enabled));
                    if let Some(k) = &legacy.keyframes {
                        let k = k.as_object().ok_or_else(|| format!("node {}'s keyframes are not keyed", n.id))?;
                        self.keyframes.extend(k.clone());
                    }
                    self.modifiers.extend(legacy.modifiers.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            }
        }
        Ok(())
    }

    /// The lists into `out`: OFX under `ofx_key` (none for a track), the
    /// built-in effects and their flags under theirs. Empty lists are left
    /// out, as `canon()` reads them.
    fn write(&self, out: &mut Map<String, Value>, ofx_key: &str, effects_key: &str, flags_key: &str) {
        if !ofx_key.is_empty() && !self.ofx.is_empty() {
            out.insert(ofx_key.into(), Value::Array(self.ofx.clone()));
        }
        if !self.effects.is_empty() {
            out.insert(effects_key.into(), Value::Array(self.effects.clone()));
            out.insert(flags_key.into(), Value::Array(self.flags.clone()));
        }
    }
}

fn check_slot(n: &Node, slot: Option<u32>, at: usize) -> Result<(), Refusal> {
    if slot != Some(at as u32) {
        return Err(format!("node {} says slot {slot:?} but stands at {at}", n.id));
    }
    Ok(())
}

/// A `klipp.legacy.grade` group's body back to the v1 grade graph.
fn grade_body(group: &Node) -> Result<Value, Refusal> {
    let body = &group.group.as_ref().ok_or_else(|| format!("grade group {} has no body", group.id))?.graph;
    // The kind that names an edge's ports: the first node with that id,
    // or "" when there is none, as `to_v2` names them.
    let kind_of = |id: &NodeId| -> String {
        body.nodes.iter().find(|n| n.id == *id).and_then(|n| n.legacy.as_ref()).and_then(|l| l.payload.get("kind")).and_then(Value::as_str).unwrap_or("").to_string()
    };
    let mut nodes = Vec::with_capacity(body.nodes.len());
    for n in &body.nodes {
        if n.type_id.as_str() != types::LEGACY_NODE {
            return Err(format!("grade node {} is a {}, not a {}", n.id, n.type_id, types::LEGACY_NODE));
        }
        let legacy = n.legacy.as_ref().ok_or_else(|| format!("grade node {} carries no old entry", n.id))?;
        let payload = legacy.payload.as_object().ok_or_else(|| format!("grade node {}'s payload is not an object", n.id))?;
        let kind = payload.get("kind").and_then(Value::as_str).ok_or_else(|| format!("grade node {} has no kind", n.id))?;
        let mut v = Map::new();
        v.insert("id".into(), Value::String(n.id.to_string()));
        v.insert("kind".into(), Value::String(kind.to_string()));
        v.insert("enabled".into(), Value::Bool(n.enabled));
        // The label and params travel in the payload, present or not, as
        // the file had them.
        for key in ["label", "params"] {
            if let Some(p) = payload.get(key) {
                v.insert(key.into(), p.clone());
            }
        }
        if let Some(k) = &legacy.keyframes {
            v.insert("keyframes".into(), k.clone());
        }
        if let Some(link) = &n.wireless.output_link {
            v.insert("output_link".into(), Value::String(link.clone()));
        }
        if !n.wireless.input_links.is_empty() {
            let mut links: Vec<Value> = Vec::new();
            for (key, name) in &n.wireless.input_links {
                let i = port_index(kind, key, true).ok_or_else(|| format!("grade node {} ({kind}) has no input {key}", n.id))?;
                if links.len() <= usize::from(i) {
                    links.resize(usize::from(i) + 1, Value::Null);
                }
                links[usize::from(i)] = Value::String(name.clone());
            }
            v.insert("input_links".into(), Value::Array(links));
        }
        nodes.push(Value::Object(v));
    }
    let mut edges = Vec::with_capacity(body.edges.len());
    for e in &body.edges {
        let (from_kind, to_kind) = (kind_of(&e.from.0), kind_of(&e.to.0));
        let from_port = port_index(&from_kind, &e.from.1, false).ok_or_else(|| format!("an edge leaves {} by {}, which a {from_kind:?} lacks", e.from.0, e.from.1))?;
        let to_port = port_index(&to_kind, &e.to.1, true).ok_or_else(|| format!("an edge enters {} by {}, which a {to_kind:?} lacks", e.to.0, e.to.1))?;
        edges.push(serde_json::json!({ "from_node": e.from.0, "from_port": from_port, "to_node": e.to.0, "to_port": to_port }));
    }
    let mut g = Map::new();
    g.insert("nodes".into(), Value::Array(nodes));
    g.insert("edges".into(), Value::Array(edges));
    g.insert("output_node_id".into(), Value::String(body.output.to_string()));
    if !body.ui.positions.is_empty() {
        let pos: Map<String, Value> = body.ui.positions.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        g.insert("node_positions".into(), Value::Object(pos));
    }
    if !body.ui.frames.is_empty() {
        g.insert("frames".into(), Value::Array(body.ui.frames.clone()));
    }
    if !body.ui.sticky_notes.is_empty() {
        g.insert("sticky_notes".into(), Value::Array(body.ui.sticky_notes.clone()));
    }
    if let Some(w) = &body.working_space_override {
        g.insert("working_space_override".into(), w.clone());
    }
    Ok(Value::Object(g))
}

/// A port key back to its v1 number on a node of `kind`: the table's, or
/// the `"p<n>"` that stands for a number the kind lacks.
fn port_index(kind: &str, key: &PortKey, input: bool) -> Option<u8> {
    let named = if input { ports::input_index(kind, key) } else { ports::output_index(kind, key) };
    named.or_else(|| key.as_str().strip_prefix('p').and_then(|n| n.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::{canon, Store};
    use serde_json::json;

    const GRADE_ID: &str = "00000000-0000-0000-0000-0000000000a0";
    const SRC: &str = "00000000-0000-0000-0000-000000000001";
    const CORR: &str = "00000000-0000-0000-0000-000000000002";
    const MASK: &str = "00000000-0000-0000-0000-000000000003";

    /// A clip as v1 saves it: a grade with a gated corrector, one plug-in,
    /// two built-in effects (the first an opacity), a flag list one short,
    /// keyframes and modifiers that name slot 1, keys that name no slot,
    /// clip keyframes, and an audio modifier that is not picture.
    fn v1_clip() -> Value {
        json!({
            "name": "shot",
            "node_graph": {
                "nodes": [
                    { "id": SRC, "kind": "Source", "label": "", "enabled": true, "params": {}, "keyframes": {"tracks": []}, "output_link": null, "input_links": [] },
                    { "id": CORR, "kind": "Corrector", "label": "Balance", "enabled": false, "params": {"exposure": 0.5}, "keyframes": {"tracks": []}, "output_link": "graded", "input_links": [null, "matte", null] },
                    { "id": MASK, "kind": "Mask", "label": "", "enabled": true, "keyframes": {"tracks": []}, "output_link": null, "input_links": [] }
                ],
                "edges": [
                    { "from_node": MASK, "from_port": 0, "to_node": CORR, "to_port": 1 },
                    { "from_node": SRC, "from_port": 0, "to_node": CORR, "to_port": 0 },
                    { "from_node": SRC, "from_port": 0, "to_node": CORR, "to_port": 7 }
                ],
                "output_node_id": CORR,
                "node_positions": { SRC: {"x": 0.0, "y": 0.0} }
            },
            "effects": [ { "plugin_name": "org.example.Glow", "enabled": false, "param_values": {} } ],
            "builtin_effects": [ {"type": "opacity", "value": 0.5}, {"type": "gaussian_blur", "radius": 4.0} ],
            "builtin_effects_enabled": [false],
            "effect_keyframes": { "1:radius": [], "9:radius": [], "nonsense": [] },
            "param_modifiers": { "effect:1:radius": {"modifiers": []}, "opacity": {"modifiers": []}, "volume": {"modifiers": []}, "effect:x": {"modifiers": []} },
            "opacity_keyframes": [],
            "blend_mode": "Screen"
        })
    }

    /// The record `to_v2` writes for [`v1_clip`], by the layout on #1002.
    fn record() -> Value {
        json!({
            "graph": {
                "graph_version": 2,
                "nodes": [
                    { "id": GRADE_ID, "type_id": "klipp.legacy.grade", "legacy": { "payload": null },
                      "group": { "graph": {
                          "graph_version": 2,
                          "nodes": [
                              { "id": SRC, "type_id": "klipp.legacy.node", "legacy": { "payload": {"kind": "Source", "label": "", "params": {}}, "keyframes": {"tracks": []} } },
                              { "id": CORR, "type_id": "klipp.legacy.node", "enabled": false,
                                "wireless": { "output_link": "graded", "input_links": { "mask": "matte" } },
                                "legacy": { "payload": {"kind": "Corrector", "label": "Balance", "params": {"exposure": 0.5}}, "keyframes": {"tracks": []} } },
                              { "id": MASK, "type_id": "klipp.legacy.node", "legacy": { "payload": {"kind": "Mask", "label": ""}, "keyframes": {"tracks": []} } }
                          ],
                          "edges": [
                              { "from": [MASK, "out"], "to": [CORR, "mask"] },
                              { "from": [SRC, "out"], "to": [CORR, "in"] },
                              { "from": [SRC, "out"], "to": [CORR, "p7"] }
                          ],
                          "output": CORR,
                          "ui": { "positions": { SRC: {"x": 0.0, "y": 0.0} } }
                      } } },
                    { "id": "00000000-0000-0000-0000-0000000000b0", "type_id": "klipp.legacy.ofx",
                      "legacy": { "slot": 0, "payload": { "plugin_name": "org.example.Glow", "enabled": false, "param_values": {} } } },
                    { "id": "00000000-0000-0000-0000-0000000000c0", "type_id": "klipp.legacy.opacity", "enabled": false,
                      "legacy": { "slot": 0, "payload": {"type": "opacity", "value": 0.5} } },
                    { "id": "00000000-0000-0000-0000-0000000000c1", "type_id": "klipp.legacy.effect",
                      "legacy": { "slot": 1, "payload": {"type": "gaussian_blur", "radius": 4.0},
                                  "keyframes": { "1:radius": [] }, "modifiers": { "effect:1:radius": {"modifiers": []} } } }
                ],
                "edges": [],
                "output": "00000000-0000-0000-0000-0000000000c1"
            },
            "legacy_clip": {
                "fields": { "opacity_keyframes": [] },
                "effect_keyframes": { "9:radius": [], "nonsense": [] },
                "param_modifiers": { "opacity": {"modifiers": []}, "effect:x": {"modifiers": []} }
            }
        })
    }

    #[test]
    fn a_clip_record_comes_back_as_its_canonical_v1_picture() {
        assert_eq!(clip(&record()).unwrap(), canon(Store::Clip, &v1_clip()));
    }

    #[test]
    fn interior_empty_links_come_back_and_a_port_the_kind_lacks_survives() {
        let v = clip(&record()).unwrap();
        let corr = v["node_graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == CORR).unwrap().clone();
        assert_eq!(corr["input_links"], json!([null, "matte"]));
        assert!(v["node_graph"]["edges"].as_array().unwrap().iter().any(|e| e["to_port"] == 7));
    }

    #[test]
    fn what_to_v2_never_writes_is_refused() {
        let with = |f: &dyn Fn(&mut Value)| {
            let mut r = record();
            f(&mut r);
            clip(&r)
        };
        assert!(with(&|r| r["graph"]["nodes"][1]["type_id"] = json!("klipp.fx.gaussian_blur")).is_err(), "outside the vocabulary");
        assert!(with(&|r| r["graph"]["nodes"].as_array_mut().unwrap().swap(1, 2)).is_err(), "an effect before a plug-in");
        assert!(with(&|r| r["graph"]["nodes"][3]["legacy"]["slot"] = json!(5)).is_err(), "a slot out of place");
        assert!(with(&|r| r["graph"]["nodes"][2]["type_id"] = json!("klipp.legacy.effect")).is_err(), "an opacity under the generic type");
        assert!(with(&|r| r["graph"]["nodes"][1]["enabled"] = json!(false)).is_err(), "a plug-in's flag on the node");
        assert!(with(&|r| r["graph"]["nodes"][0]["group"]["graph"]["nodes"][1]["wireless"]["input_links"] = json!({"bogus": "x"})).is_err(), "a port key the kind lacks");
    }

    #[test]
    fn an_empty_clip_is_an_empty_record() {
        let r = json!({ "graph": { "graph_version": 2, "nodes": [], "edges": [], "output": "00000000-0000-0000-0000-000000000000" } });
        assert_eq!(clip(&r).unwrap(), canon(Store::Clip, &json!({ "effects": [], "builtin_effects": null })));
    }
}
