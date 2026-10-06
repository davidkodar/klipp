//! `canon()`: a v1 picture with only what cannot change its meaning
//! normalised, the form the conversion's oracle is held to (#1061):
//! `to_v1(to_v2(x)) == canon(x)`.
//!
//! It reads the picture's raw JSON, as the conversion does: whether a key
//! is present in the file is part of what is carried, and a typed reading
//! would lose it. It keeps the fields the converted record takes and no
//! others (a clip's `blend_mode` or audio modifiers stay on the clip).
//!
//! What is normalised, and nothing else:
//!
//! * an effect list's enabled flags (`builtin_effects_enabled`, a track's
//!   `video_enabled`) padded with `true` or trimmed to the list's length,
//!   as the app reads a flag past the end as on and ignores one with no
//!   effect;
//! * the order of a map's keys;
//! * trailing `None`s in a grade node's `input_links`, which name no port;
//! * what v1 writes for "nothing" and reads back as nothing, read as
//!   absent: an effect list, keyframe map or modifier map that is empty or
//!   `null`; a `null` `node_graph` or track `grade` (an empty graph object
//!   is a graph, and stays); a node's empty `input_links` and `null`
//!   `output_link`; a graph's empty `node_positions`, `frames` and
//!   `sticky_notes`.
//!
//! Everything else that is odd is kept as it is: duplicate or empty
//! keyframe tracks, empty keyframe lists and modifier stacks, `"i:k"` keys
//! that do not parse or name no slot, edges to ports a kind lacks or two
//! into one port, the order of a graph's edges (where two feed one port,
//! the first is the one the executor reads: `NodeGraph::input_of`), a
//! node's `params` or `keyframes` absent beside present,
//! and a grade group's stage left unset beside one set to the identity.
//! Each of those may mean something to a reader, so the conversion must
//! carry it, and the oracle must see it carried.

use serde_json::{Map, Value};

/// The kind of picture a raw value is, which decides what of it the
/// converted record takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    /// A clip's picture fields.
    Clip,
    /// A track's `effects`.
    Track,
    /// A grade graph on its own: a timeline grade, a group stage, a
    /// grade version's or a Look's or a still's graph.
    Graph,
}

/// The clip fields a converted clip record takes, besides its modifiers.
pub const CLIP_FIELDS: [&str; 12] = [
    "node_graph",
    "effects",
    "builtin_effects",
    "builtin_effects_enabled",
    "effect_keyframes",
    "opacity_keyframes",
    "transform_pos_x_keyframes",
    "transform_pos_y_keyframes",
    "transform_scale_keyframes",
    "transform_rotation_keyframes",
    "transform_tilt_x_keyframes",
    "transform_tilt_y_keyframes",
];

/// The clip modifier keys that are sound, not picture, and stay on the
/// clip.
pub const AUDIO_MODIFIERS: [&str; 3] = ["volume", "pan", "gain"];

/// The picture in `raw`, a value of kind `store`, in canonical form.
pub fn canon(store: Store, raw: &Value) -> Value {
    let v = match store {
        Store::Clip => clip(raw),
        Store::Track => track(raw),
        Store::Graph => {
            let mut g = raw.clone();
            grade_graph(&mut g);
            g
        }
    };
    sorted(v)
}

fn clip(raw: &Value) -> Value {
    let mut out = Map::new();
    let Some(obj) = raw.as_object() else { return Value::Object(out) };
    for f in CLIP_FIELDS {
        if let Some(v) = obj.get(f) {
            out.insert(f.to_string(), v.clone());
        }
    }
    if let Some(Value::Object(mods)) = obj.get("param_modifiers") {
        let picture: Map<String, Value> = mods.iter().filter(|(k, _)| !AUDIO_MODIFIERS.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
        out.insert("param_modifiers".into(), Value::Object(picture));
    }
    // A null graph is read as no graph (the default pipeline); a present
    // one, even empty, is a graph and stays.
    if out.get("node_graph").is_some_and(Value::is_null) {
        out.remove("node_graph");
    }
    if let Some(g) = out.get_mut("node_graph") {
        grade_graph(g);
    }
    pad_flags(&mut out, "builtin_effects", "builtin_effects_enabled");
    for f in ["effects", "builtin_effects", "effect_keyframes", "param_modifiers"] {
        drop_if_empty(&mut out, f);
    }
    Value::Object(out)
}

/// A track's picture: the `grade`, `video` and `video_enabled` of its
/// `effects`. Its audio chain stays a track field, outside the record.
const TRACK_FIELDS: [&str; 3] = ["grade", "video", "video_enabled"];

fn track(raw: &Value) -> Value {
    let all = raw.as_object().cloned().unwrap_or_default();
    let mut out: Map<String, Value> = all.into_iter().filter(|(k, _)| TRACK_FIELDS.contains(&k.as_str())).collect();
    if out.get("grade").is_some_and(Value::is_null) {
        out.remove("grade");
    }
    if let Some(g) = out.get_mut("grade") {
        grade_graph(g);
    }
    pad_flags(&mut out, "video", "video_enabled");
    drop_if_empty(&mut out, "video");
    Value::Object(out)
}

/// Remove `key` when it holds an empty list or map, or `null`.
fn drop_if_empty(obj: &mut Map<String, Value>, key: &str) {
    let empty = match obj.get(key) {
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        Some(Value::Null) => true,
        _ => false,
    };
    if empty {
        obj.remove(key);
    }
}

/// The flags list `flags` padded with `true` or trimmed to the length of
/// the list `list` beside it. With nothing to flag it goes.
fn pad_flags(obj: &mut Map<String, Value>, list: &str, flags: &str) {
    let n = obj.get(list).and_then(Value::as_array).map_or(0, Vec::len);
    let mut have: Vec<Value> = obj.get(flags).and_then(Value::as_array).cloned().unwrap_or_default();
    have.resize(n, Value::Bool(true));
    if have.is_empty() {
        obj.remove(flags);
    } else {
        obj.insert(flags.to_string(), Value::Array(have));
    }
}

/// Normalise a v1 grade graph. A Compound node's subgraph is left as it
/// is: it is part of the node's params, which the conversion carries
/// verbatim as the node's payload, so nothing in it is re-expressed and
/// nothing may be normalised.
fn grade_graph(v: &mut Value) {
    let Some(obj) = v.as_object_mut() else { return };
    drop_if_empty(obj, "node_positions");
    for f in ["frames", "sticky_notes"] {
        drop_if_empty(obj, f);
    }
    if let Some(Value::Array(nodes)) = obj.get_mut("nodes") {
        for node in nodes.iter_mut().filter_map(Value::as_object_mut) {
            if let Some(Value::Array(links)) = node.get_mut("input_links") {
                while links.last().is_some_and(Value::is_null) {
                    links.pop();
                }
            }
            drop_if_empty(node, "input_links");
            if node.get("output_link").is_some_and(Value::is_null) {
                node.remove("output_link");
            }
        }
    }
}

/// `v` with every map's keys in order, whatever map type JSON is built
/// with in this build.
pub fn sorted(v: Value) -> Value {
    match v {
        Value::Object(obj) => {
            let mut entries: Vec<(String, Value)> = obj.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OPACITY: &str = r#"{"type": "opacity", "value": 0.5}"#;

    fn fx() -> Value {
        serde_json::from_str(OPACITY).unwrap()
    }

    #[test]
    fn effect_flags_are_padded_with_on_and_trimmed_to_the_list() {
        let c = json!({ "builtin_effects": [fx(), fx()], "builtin_effects_enabled": [false] });
        assert_eq!(canon(Store::Clip, &c)["builtin_effects_enabled"], json!([false, true]));
        let c = json!({ "builtin_effects": [fx()], "builtin_effects_enabled": [true, false, false] });
        assert_eq!(canon(Store::Clip, &c)["builtin_effects_enabled"], json!([true]));
        let c = json!({ "builtin_effects_enabled": [false] });
        assert!(canon(Store::Clip, &c).get("builtin_effects_enabled").is_none(), "flags with no effect go");
        let t = json!({ "video": [fx()] });
        assert_eq!(canon(Store::Track, &t)["video_enabled"], json!([true]));
        // A track's audio chain is not picture: it stays on the track.
        let with_audio = json!({ "video": [fx()], "audio": [{"type": "gain"}] });
        assert!(canon(Store::Track, &with_audio).get("audio").is_none());
    }

    #[test]
    fn only_the_fields_the_record_takes_and_no_audio_modifiers() {
        let c = json!({
            "name": "a clip", "blend_mode": "Screen", "grade_group": "g",
            "opacity_keyframes": [],
            "param_modifiers": { "volume": {"modifiers": []}, "opacity": {"modifiers": []} }
        });
        assert_eq!(canon(Store::Clip, &c), json!({ "opacity_keyframes": [], "param_modifiers": { "opacity": {"modifiers": []} } }));
        let audio_only = json!({ "param_modifiers": { "pan": {"modifiers": []} } });
        assert_eq!(canon(Store::Clip, &audio_only), json!({}));
    }

    fn graph(edges: Value, node: Value) -> Value {
        json!({ "nodes": [node], "edges": edges, "output_node_id": "00000000-0000-0000-0000-000000000001" })
    }

    fn node(extra: Value) -> Value {
        let mut n = json!({"id": "00000000-0000-0000-0000-000000000001", "kind": "Source", "label": "", "enabled": true});
        n.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        n
    }

    #[test]
    fn written_nothings_read_as_absent_and_edge_order_is_kept() {
        let a = json!({"from_node": "a", "from_port": 0, "to_node": "b", "to_port": 1});
        let b = json!({"from_node": "a", "from_port": 0, "to_node": "b", "to_port": 0});
        let mut written = graph(json!([a.clone(), b.clone()]), node(json!({"input_links": ["x", null, null], "output_link": null})));
        written["node_positions"] = json!({});
        let lean = graph(json!([a.clone(), b.clone()]), node(json!({"input_links": ["x"]})));
        assert_eq!(canon(Store::Graph, &written), canon(Store::Graph, &lean));
        // Edge order is kept: with two sources into one port, the first is
        // the one the executor reads.
        let c = json!({"from_node": "c", "from_port": 0, "to_node": "b", "to_port": 0});
        assert_ne!(canon(Store::Graph, &graph(json!([b.clone(), c.clone()]), node(json!({})))), canon(Store::Graph, &graph(json!([c, b]), node(json!({})))));
        let all_none = graph(json!([]), node(json!({"input_links": [null]})));
        assert!(canon(Store::Graph, &all_none)["nodes"][0].get("input_links").is_none());
        // A None before a name names a port: it stays.
        let mid = graph(json!([]), node(json!({"input_links": [null, "y"]})));
        assert_eq!(canon(Store::Graph, &mid)["nodes"][0]["input_links"], json!([null, "y"]));
    }

    #[test]
    fn the_odd_cases_are_kept_as_they_are() {
        let dup = json!({"from_node": "a", "from_port": 0, "to_node": "b", "to_port": 7});
        let g = graph(json!([dup.clone(), dup]), node(json!({})));
        assert_eq!(canon(Store::Graph, &g)["edges"].as_array().unwrap().len(), 2);
        let c = json!({ "effect_keyframes": {"9:radius": [], "not a key": []}, "opacity_keyframes": [] });
        let v = canon(Store::Clip, &c);
        assert_eq!(v["effect_keyframes"], json!({"9:radius": [], "not a key": []}));
        assert_eq!(v["opacity_keyframes"], json!([]));
        // A node's params absent stays absent; present stays present.
        assert!(canon(Store::Graph, &g)["nodes"][0].get("params").is_none());
        let p = graph(json!([]), node(json!({"params": {}})));
        assert_eq!(canon(Store::Graph, &p)["nodes"][0]["params"], json!({}));
    }

    #[test]
    fn written_empties_read_as_absent_and_an_empty_graph_stays() {
        let written = json!({ "node_graph": null, "builtin_effects": null, "builtin_effects_enabled": null, "effects": [], "effect_keyframes": {}, "param_modifiers": null });
        assert_eq!(canon(Store::Clip, &written), json!({}));
        let empty_graph = json!({ "node_graph": { "nodes": [], "edges": [], "output_node_id": "00000000-0000-0000-0000-000000000000" } });
        assert!(canon(Store::Clip, &empty_graph).get("node_graph").is_some(), "an empty graph is a graph");
        assert_eq!(canon(Store::Track, &json!({ "grade": null, "video": null })), json!({}));
    }

    /// The lead's rule (#1002), as a test: two saved forms of a container
    /// are merged by `canon()` exactly when today's reader loads them as
    /// the same value. Each form is parsed with the frozen reader (a form
    /// it refuses is a stop before `canon()`, and is skipped), then loaded
    /// as `klipp-project`'s `load.rs` loads it: an unset effect list,
    /// keyframe map or modifier map is an empty one, and an unset grade
    /// graph is the default pipeline, which no saved graph is.
    #[test]
    fn canon_merges_exactly_the_forms_the_reader_loads_alike() {
        let graph = json!({ "nodes": [], "edges": [], "output_node_id": "00000000-0000-0000-0000-000000000000" });
        let containers: [(&str, Value, Value); 5] = [
            ("node_graph", json!({}), graph),
            ("builtin_effects", json!([]), json!([fx()])),
            ("effects", json!([]), json!([{ "plugin_name": "p", "enabled": true, "param_values": {} }])),
            ("effect_keyframes", json!({}), json!({ "0:value": [] })),
            ("param_modifiers", json!({}), json!({ "opacity": { "modifiers": [] } })),
        ];
        for (field, empty, filled) in containers {
            let forms: Vec<(&str, Value)> = vec![("absent", json!({})), ("null", json!({ field: null })), ("empty", json!({ field: empty })), ("filled", json!({ field: filled }))];
            let loaded = |raw: &Value| -> Option<Value> {
                let typed: crate::v1::ClipPicture = serde_json::from_value(raw.clone()).ok()?;
                let mut v = serde_json::to_value(&typed).ok()?;
                let get = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
                let list_or_empty = |x: Value, e: Value| if x.is_null() { e } else { x };
                let node_graph = get("node_graph");
                let effects = get("effects");
                let builtin = list_or_empty(get("builtin_effects"), json!([]));
                let keys = list_or_empty(get("effect_keyframes"), json!({}));
                let mods = list_or_empty(get("param_modifiers"), json!({}));
                v = json!({ "node_graph": node_graph, "effects": effects, "builtin_effects": builtin, "effect_keyframes": keys, "param_modifiers": mods });
                Some(sorted(v))
            };
            let readable: Vec<(&str, Value, Value)> = forms.iter().filter_map(|(name, raw)| loaded(raw).map(|l| (*name, raw.clone(), l))).collect();
            for must in ["absent", "filled"] {
                assert!(readable.iter().any(|(n, ..)| *n == must), "{field}: the {must} form must parse, or the test checks nothing");
            }
            for (a, raw_a, load_a) in &readable {
                for (b, raw_b, load_b) in &readable {
                    let merged = canon(Store::Clip, raw_a) == canon(Store::Clip, raw_b);
                    assert_eq!(merged, load_a == load_b, "{field}: canon {} {a} and {b}, which the reader loads {}", if merged { "merges" } else { "keeps apart" }, if load_a == load_b { "alike" } else { "differently" });
                }
            }
        }
    }

    #[test]
    fn a_compound_nodes_graph_is_carried_as_it_is() {
        let a = json!({"from_node": "a", "from_port": 0, "to_node": "b", "to_port": 1});
        let b = json!({"from_node": "a", "from_port": 0, "to_node": "b", "to_port": 0});
        let with = |edges: Value| graph(json!([]), node(json!({"kind": "Compound", "params": { "compound_subgraph": graph(edges, node(json!({"input_links": []}))) }})));
        // Its payload is verbatim, so two orders stay two, and an empty
        // link list inside it stays.
        assert_ne!(canon(Store::Graph, &with(json!([a.clone(), b.clone()]))), canon(Store::Graph, &with(json!([b, a]))));
        let c = canon(Store::Graph, &with(json!([])));
        assert_eq!(c["nodes"][0]["params"]["compound_subgraph"]["nodes"][0]["input_links"], json!([]));
    }
}
