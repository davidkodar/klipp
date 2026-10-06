use crate::old_fields::{placeholder_graph, strip_sentinels, write_old_fields, OldFieldsError, SENTINEL_TYPE};
use crate::revision::{needed_revision, read_revision, write_revision, RevisionError, KNOWN_REVISION};
use klipp_graph_format::{types, ClipRecordV2, Graph, Legacy, LegacyClipAnim, Node, GRAPH_VERSION};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// A clip's converted picture in the structural form, built with the
/// format types: its old effects as legacy nodes.
fn clip_record(effects: &Value) -> Value {
    let nodes: Vec<Node> = effects
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(slot, fx)| Node {
            id: uuid_n(slot as u128 + 1),
            type_id: types::LEGACY_EFFECT.into(),
            type_version: 1,
            label: String::new(),
            enabled: true,
            params: BTreeMap::new(),
            anim: BTreeMap::new(),
            mix: None,
            wireless: Default::default(),
            group: None,
            legacy: Some(Legacy { slot: Some(slot as u32), payload: fx.clone(), keyframes: None, modifiers: BTreeMap::new() }),
            legacy_flags: None,
        origin: None,
        })
        .collect();
    let output = nodes.last().map(|n| n.id).unwrap_or(uuid_n(1));
    let mut legacy_clip = LegacyClipAnim::default();
    legacy_clip.fields.insert("opacity_keyframes".into(), json!([{"time_secs": 0.0, "value": 0.5}]));
    let record = ClipRecordV2 {
        graph: Graph { graph_version: GRAPH_VERSION, nodes, edges: Vec::new(), output, ui: Default::default(), working_space_override: None },
        legacy_clip,
    };
    record.check().unwrap();
    serde_json::to_value(record).unwrap()
}

fn uuid_n(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(n)
}

/// A project in today's shape whose clip and track carry a converted
/// picture: what the converter holds just before writing old fields.
fn converted_project() -> Value {
    let effects = json!([{"type": "crop", "left": 0.1, "right": 0.0, "top": 0.0, "bottom": 0.0, "feather": 0.0}]);
    json!({
        "version": "1.0.0",
        "sequences": [{
            "id": "seq-1",
            "timeline_graph": {"nodes": [], "edges": [], "output_node_id": "x"},
            "grade_groups": [{"id": "g1", "name": "Exteriors", "pre": {}, "post": {}, "departs": true}],
            "tracks": [{
                "id": "track-1",
                "effects": {"video": effects, "video_enabled": [true], "grade": {}, "audio": [{"kind": "eq"}],
                            "graph_v2": {"graph_version": 2, "nodes": [], "edges": [], "output": uuid_n(9)}},
                "clips": [{
                    "id": "clip-1",
                    "builtin_effects": effects,
                    "builtin_effects_enabled": [true],
                    "effects": [{"plugin_name": "ofx"}],
                    "effect_keyframes": {"0:left": []},
                    "node_graph": {"nodes": []},
                    "opacity_keyframes": [{"time_secs": 0.0, "value": 0.5}],
                    "transform_scale_keyframes": [],
                    "param_modifiers": {"volume": {}, "pan": {}, "opacity": {}, "effect:0:left": {}, "gain": {}},
                    "volume_keyframes": [{"time_secs": 0.0, "value": 1.0}],
                    "grade_versions": [{"name": "Warm", "node_graph": {"nodes": ["full"]}}],
                    "grade_group": "g1",
                    "graph_v2": clip_record(&effects)
                }]
            }, {
                "id": "track-2",
                "effects": {"audio": [{"kind": "comp"}]},
                "clips": []
            }]
        }]
    })
}

fn clip(p: &Value) -> &Value {
    &p["sequences"][0]["tracks"][0]["clips"][0]
}

#[test]
fn old_fields_follow_the_table() {
    let mut p = converted_project();
    write_old_fields(&mut p).unwrap();
    let c = clip(&p);
    assert_eq!(c["builtin_effects"], json!([{"type": SENTINEL_TYPE}]));
    assert_eq!(c["effects"], json!([]));
    for gone in ["builtin_effects_enabled", "effect_keyframes", "node_graph", "opacity_keyframes", "transform_scale_keyframes"] {
        assert!(c.get(gone).is_none(), "{gone} left in");
    }
    let mut kept: Vec<_> = c["param_modifiers"].as_object().unwrap().keys().cloned().collect();
    kept.sort();
    assert_eq!(kept, ["gain", "pan", "volume"], "audio modifiers only");
    assert_eq!(c["volume_keyframes"], json!([{"time_secs": 0.0, "value": 1.0}]), "sound fields untouched");
    assert_eq!(c["grade_group"], "g1");
    assert_eq!(c["grade_versions"][0]["name"], "Warm");
    assert_eq!(c["grade_versions"][0]["node_graph"], placeholder_graph());
    assert!(c.get("graph_v2").is_some(), "the converted picture stays");

    let seq = &p["sequences"][0];
    assert!(seq.get("timeline_graph").is_none());
    assert_eq!(seq["grade_groups"][0], json!({"id": "g1", "name": "Exteriors", "departs": true}));
    let t = &seq["tracks"][0]["effects"];
    assert_eq!(t["video"], json!([{"type": SENTINEL_TYPE}]));
    assert!(t.get("video_enabled").is_none() && t.get("grade").is_none());
    assert_eq!(t["audio"], json!([{"kind": "eq"}]), "the track's sound stays");
    // A track with no converted picture gets no sentinel.
    assert_eq!(seq["tracks"][1]["effects"], json!({"audio": [{"kind": "comp"}]}));
}

/// The placeholder is the bare Source -> Output graph, and today's reader
/// takes it as such: no correction an old build would run.
#[test]
fn the_grade_version_placeholder_is_source_to_output() {
    let g: klipp_nodes::NodeGraph = serde_json::from_value(placeholder_graph()).unwrap();
    let kinds: Vec<_> = g.nodes.iter().map(|n| n.kind).collect();
    assert_eq!(kinds, [klipp_nodes::NodeKind::Source, klipp_nodes::NodeKind::Output]);
    assert_eq!(g.edges.len(), 1);
    assert_eq!((g.edges[0].from_node, g.edges[0].to_node), (g.nodes[0].id, g.nodes[1].id));
    assert_eq!(g.output_node_id, g.nodes[1].id);
    assert!(g.is_identity());
}

#[test]
fn a_clip_without_a_converted_picture_is_not_written() {
    let mut p = converted_project();
    p["sequences"][0]["tracks"][0]["clips"][0].as_object_mut().unwrap().remove("graph_v2");
    assert_eq!(write_old_fields(&mut p), Err(OldFieldsError::NoPicture { what: "clip", id: "clip-1".into() }));
    assert!(matches!(write_old_fields(&mut json!({})), Err(OldFieldsError::NotAProject(_))));
}

#[test]
fn the_reader_strips_the_sentinels_it_wrote() {
    let mut p = converted_project();
    write_old_fields(&mut p).unwrap();
    strip_sentinels(&mut p).unwrap();
    assert!(clip(&p).get("builtin_effects").is_none());
    assert!(p["sequences"][0]["tracks"][0]["effects"].get("video").is_none());
    assert_eq!(p["sequences"][0]["tracks"][0]["effects"]["audio"], json!([{"kind": "eq"}]));
    // A file with no sentinels passes untouched.
    let mut plain = json!({"sequences": [{"tracks": [{"id": "t", "clips": [{"id": "c", "builtin_effects": [{"type": "crop"}]}]}]}]});
    let before = plain.clone();
    strip_sentinels(&mut plain).unwrap();
    assert_eq!(plain, before);
}

#[test]
fn a_sentinel_without_its_picture_refuses_naming_the_clip_or_track() {
    let mut p = converted_project();
    write_old_fields(&mut p).unwrap();
    let mut clip_case = p.clone();
    clip_case["sequences"][0]["tracks"][0]["clips"][0].as_object_mut().unwrap().remove("graph_v2");
    let e = strip_sentinels(&mut clip_case).unwrap_err();
    assert_eq!(e, OldFieldsError::SentinelWithoutPicture { what: "clip", id: "clip-1".into() });
    assert!(e.to_string().starts_with("clip clip-1:"));

    let mut track_case = p.clone();
    track_case["sequences"][0]["tracks"][0]["effects"].as_object_mut().unwrap().remove("graph_v2");
    assert_eq!(
        strip_sentinels(&mut track_case),
        Err(OldFieldsError::SentinelWithoutPicture { what: "track", id: "track-1".into() })
    );

    let mut mixed = p;
    mixed["sequences"][0]["tracks"][0]["clips"][0]["builtin_effects"] = json!([{"type": SENTINEL_TYPE}, {"type": "crop"}]);
    assert_eq!(strip_sentinels(&mut mixed), Err(OldFieldsError::SentinelMixed { what: "clip", id: "clip-1".into() }));
}

#[test]
fn revisions_are_whole_numbers_the_converter_knows() {
    assert_eq!(read_revision(&json!({})), Ok(0));
    assert_eq!(read_revision(&json!({"format_revision": 0})), Ok(0));
    assert_eq!(read_revision(&json!({"format_revision": 1})), Ok(1));
    for bad in [json!("1"), json!(-1), json!(1.5), json!(true), json!(null), json!([1])] {
        assert_eq!(read_revision(&json!({"format_revision": bad.clone()})), Err(RevisionError::NotARevision(bad)));
    }
    assert_eq!(read_revision(&json!({"format_revision": KNOWN_REVISION + 1})), Err(RevisionError::Newer(KNOWN_REVISION + 1)));
    assert_eq!(read_revision(&json!({"format_revision": u64::MAX})), Err(RevisionError::Newer(u64::MAX)));
}

#[test]
fn a_file_is_written_with_the_revision_its_content_needs() {
    let mut p = converted_project();
    assert_eq!(needed_revision(&p), 1);
    write_old_fields(&mut p).unwrap();
    write_revision(&mut p);
    assert_eq!(p["format_revision"], 1);
    assert_eq!(read_revision(&p), Ok(1));

    let mut plain = json!({"format_revision": 1, "sequences": [{"tracks": [{"clips": [{"id": "c"}]}]}]});
    assert_eq!(needed_revision(&plain), 0);
    write_revision(&mut plain);
    assert!(plain.get("format_revision").is_none(), "a file with nothing converted claims nothing");

    // A track's converted picture alone needs revision 1 too.
    let track_only = json!({"sequences": [{"tracks": [{"effects": {"graph_v2": {}}, "clips": []}]}]});
    assert_eq!(needed_revision(&track_only), 1);
    // A timeline grade, a group or a grade version alone needs it too, in
    // the old single-sequence shape as well.
    for converted in [
        json!({"sequences": [{"graph_v2": {}, "tracks": []}]}),
        json!({"sequences": [{"grade_groups": [{"graph_v2": {}}]}]}),
        json!({"sequence": {"tracks": [{"clips": [{"grade_versions": [{"graph_v2": {}}]}]}]}}),
    ] {
        assert_eq!(needed_revision(&converted), 1, "{converted}");
    }
}

/// `canon`'s rule against the frozen reader, which `klipp-graph-lift` can't
/// see: two saved forms of a container are merged exactly when today's
/// reader loads them as the same value.
mod canon_against_the_reader {
    use crate::canon::{canon, sorted, Store};
    use serde_json::{json, Value};

    fn fx() -> Value {
        json!({"type": "opacity", "value": 0.5})
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
}
