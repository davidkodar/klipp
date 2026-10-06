use crate::ids::{entry_id, RootKind, NAMESPACE};
use crate::to_v2::{clip_to_v2, colour_preset_to_v2, look_to_v2, stages_to_v2, still_to_v2, timeline_to_v2, track_to_v2, version_to_v2, ToV2Error};
use klipp_graph_format::{types, Graph, PortKey};
use serde_json::{json, Map, Value};
use uuid::Uuid;

const CLIP: &str = "00000000-0000-0000-0000-000000001234";
const N1: &str = "00000000-0000-0000-0000-0000000000a1";
const N2: &str = "00000000-0000-0000-0000-0000000000a2";
const N3: &str = "00000000-0000-0000-0000-0000000000a3";
const GONE: &str = "00000000-0000-0000-0000-0000000000ff";

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

fn crop(left: f64) -> Value {
    json!({"type": "crop", "left": left, "right": 0.0, "top": 0.0, "bottom": 0.0, "feather": 0.0})
}

/// A v1 grade graph as today's writer saves it, with a gated corrector, a
/// layer, wireless links and the odd shapes old files carry: an edge from
/// a node that isn't there, and an output naming nothing.
fn grade_graph() -> Value {
    json!({
        "nodes": [
            {"id": N1, "kind": "Corrector", "label": "Balance", "enabled": true,
             "params": {"exposure": 0.0}, "keyframes": {"tracks": {"gain": []}},
             "output_link": null, "input_links": [null, "matte"]},
            {"id": N2, "kind": "Layer", "label": "Layer", "enabled": false,
             "output_link": "plate", "input_links": []},
            {"id": N3, "kind": "RGBSplitter", "label": "Split", "enabled": true, "input_links": []}
        ],
        "edges": [
            {"from_node": N1, "from_port": 0, "to_node": N2, "to_port": 1},
            {"from_node": N3, "from_port": 2, "to_node": N1, "to_port": 1},
            {"from_node": GONE, "from_port": 0, "to_node": N2, "to_port": 0},
            {"from_node": N1, "from_port": 0, "to_node": N1, "to_port": 5}
        ],
        "output_node_id": GONE,
        "node_positions": {N1: {"x": 1.0, "y": 2.0}},
        "frames": [{"label": "keys"}],
        "working_space_override": "Rec709"
    })
}

fn clip() -> Map<String, Value> {
    obj(json!({
        "id": CLIP,
        "node_graph": grade_graph(),
        "effects": [{"plugin_name": "Glow", "enabled": false, "params": {}}],
        "builtin_effects": [crop(0.1), {"type": "opacity", "opacity": 0.5}, crop(0.3)],
        "builtin_effects_enabled": [true, false],
        "effect_keyframes": {"0:left": [{"time_secs": 0.0, "value": 0.1}], "2:left": [], "9:left": [], "junk": []},
        "param_modifiers": {
            "opacity": {"modifiers": []}, "volume": {"modifiers": []}, "pan": {"modifiers": []},
            "effect:1:opacity": {"modifiers": []}, "effect:7:left": {"modifiers": []}, "mystery": {"modifiers": []}
        },
        "opacity_keyframes": [{"time_secs": 0.0, "value": 1.0}],
        "transform_tilt_y_keyframes": [],
        "volume_keyframes": [{"time_secs": 0.0, "value": 1.0}],
        "grade_group": "g"
    }))
}

/// Ids are fixed values, checked against Python's `uuid.uuid5` with the
/// same namespace and name.
#[test]
fn ids_are_the_same_on_every_run_and_machine() {
    let clip = Uuid::from_u128(0x1234);
    assert_eq!(NAMESPACE.to_string(), "6b6c6970-702d-6772-6170-682d69647331");
    assert_eq!(entry_id(RootKind::Clip, clip, "effects", 0).to_string(), "dac523e5-3f85-55be-bac8-54e9e2f9e5a8");
    assert_eq!(entry_id(RootKind::Clip, clip, "version/1/grade", 0).to_string(), "e9840a2a-6db2-5ae1-a655-3e17748babcf");
    assert_ne!(entry_id(RootKind::Clip, clip, "effects", 0), entry_id(RootKind::Track, clip, "effects", 0));
    assert_ne!(entry_id(RootKind::Clip, clip, "effects", 0), entry_id(RootKind::Clip, clip, "ofx", 0));
}

#[test]
fn a_clip_becomes_grade_then_ofx_then_effects() {
    let record = clip_to_v2(&clip()).unwrap();
    record.check().unwrap();
    let g = &record.graph;
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.type_id.as_str()).collect();
    assert_eq!(kinds, [types::LEGACY_GRADE, types::LEGACY_OFX, types::LEGACY_EFFECT, types::LEGACY_OPACITY, types::LEGACY_EFFECT]);
    assert_eq!(g.output, g.nodes[4].id, "the output is the last node");
    assert_eq!(g.edges.len(), 4, "chained out -> in");
    for (e, w) in g.edges.iter().zip(g.nodes.windows(2)) {
        assert_eq!((e.from.0, e.to.0), (w[0].id, w[1].id));
        assert_eq!((e.from.1.as_str(), e.to.1.as_str()), ("out", "in"));
    }
    let root = uuid(CLIP);
    assert_eq!(g.nodes[0].id, entry_id(RootKind::Clip, root, "grade", 0));
    assert_eq!(g.nodes[1].id, entry_id(RootKind::Clip, root, "ofx", 0));
    assert_eq!(g.nodes[4].id, entry_id(RootKind::Clip, root, "effects", 2));

    // OFX: always enabled; the payload holds its own flag, verbatim.
    assert!(g.nodes[1].enabled);
    assert_eq!(g.nodes[1].legacy.as_ref().unwrap().payload["enabled"], false);
    // Built-ins: the flag by slot, a missing one reads true.
    assert_eq!([g.nodes[2].enabled, g.nodes[3].enabled, g.nodes[4].enabled], [true, false, true]);
    let fx = |i: usize| g.nodes[i].legacy.as_ref().unwrap();
    assert_eq!((fx(2).slot, fx(3).slot, fx(4).slot), (Some(0), Some(1), Some(2)));
    assert_eq!(fx(2).payload, crop(0.1));
    assert_eq!(fx(2).keyframes, Some(json!({"0:left": [{"time_secs": 0.0, "value": 0.1}]})));
    assert_eq!(fx(3).keyframes, None);
    assert_eq!(fx(4).keyframes, Some(json!({"2:left": []})));
    assert_eq!(fx(3).modifiers.keys().collect::<Vec<_>>(), ["effect:1:opacity"]);

    // What no node claims, and the clip's own fields, on the record.
    let lc = &record.legacy_clip;
    assert_eq!(lc.effect_keyframes.keys().collect::<Vec<_>>(), ["9:left", "junk"]);
    assert_eq!(lc.param_modifiers.keys().collect::<Vec<_>>(), ["effect:7:left", "mystery", "opacity"]);
    assert_eq!(lc.fields.keys().collect::<Vec<_>>(), ["opacity_keyframes", "transform_tilt_y_keyframes"]);
}

#[test]
fn the_grade_body_is_kept_as_saved() {
    let record = clip_to_v2(&clip()).unwrap();
    let body = &record.graph.nodes[0].group.as_ref().unwrap().graph;
    assert_eq!(body.output, uuid(GONE), "an output naming nothing stays");
    assert_eq!(body.working_space_override, Some(json!("Rec709")));
    assert_eq!(body.ui.positions[&uuid(N1)], json!({"x": 1.0, "y": 2.0}));
    assert_eq!(body.ui.frames, [json!({"label": "keys"})]);

    let n1 = &body.nodes[0];
    assert_eq!((n1.id, n1.type_id.as_str(), n1.enabled), (uuid(N1), types::LEGACY_NODE, true));
    let l1 = n1.legacy.as_ref().unwrap();
    assert_eq!(l1.payload, json!({"kind": "Corrector", "label": "Balance", "params": {"exposure": 0.0}}));
    assert_eq!(l1.keyframes, Some(json!({"tracks": {"gain": []}})));
    assert_eq!(n1.wireless.input_links.get(&PortKey::from("mask")).map(String::as_str), Some("matte"));
    assert_eq!(n1.wireless.input_links.len(), 1, "None links drop out");
    let n2 = &body.nodes[1];
    assert!(!n2.enabled);
    assert_eq!(n2.wireless.output_link.as_deref(), Some("plate"));
    let l2 = n2.legacy.as_ref().unwrap();
    assert_eq!(l2.payload, json!({"kind": "Layer", "label": "Layer"}), "absent params stay absent");
    assert_eq!(l2.keyframes, None);

    let ports: Vec<(&str, &str)> = body.edges.iter().map(|e| (e.from.1.as_str(), e.to.1.as_str())).collect();
    // In the order saved, which is meaning where two edges feed one port:
    // Corrector -> Layer bg; RGBSplitter output 2 -> Corrector's gate; a
    // missing node's port 0 is "out"; Corrector -> itself on a port the
    // kind lacks (p5).
    assert_eq!(ports, [("out", "bg"), ("b", "mask"), ("out", "fg"), ("out", "p5")]);
}

#[test]
fn a_clip_with_no_picture_is_the_empty_graph() {
    let bare = obj(json!({"id": CLIP, "effects": [], "builtin_effects": null, "effect_keyframes": {}, "param_modifiers": {"volume": {}}}));
    let record = clip_to_v2(&bare).unwrap();
    assert_eq!(record.graph, Graph::empty());
    assert!(record.legacy_clip.is_empty(), "audio modifiers stay clip fields");
    record.check().unwrap();
    // An absent and a null grade graph are both no grade graph.
    let null_grade = obj(json!({"id": CLIP, "node_graph": null}));
    assert_eq!(clip_to_v2(&null_grade).unwrap().graph, Graph::empty());
    // An empty graph object is a graph, kept.
    let empty_grade = obj(json!({"id": CLIP, "node_graph": {"nodes": [], "edges": [], "output_node_id": N1}}));
    let g = clip_to_v2(&empty_grade).unwrap().graph;
    assert_eq!(g.nodes.len(), 1);
    assert_eq!(g.nodes[0].type_id.as_str(), types::LEGACY_GRADE);
}

/// What today's reader refuses, the conversion refuses: it never guesses.
#[test]
fn shapes_todays_reader_refuses_are_refused() {
    let shape = |field| ToV2Error::Shape { what: "clip", field };
    let with = |k: &str, v: Value| {
        let mut c = clip();
        c.insert(k.into(), v);
        c
    };
    assert_eq!(clip_to_v2(&with("effects", Value::Null)), Err(shape("effects")));
    assert_eq!(clip_to_v2(&with("builtin_effects_enabled", Value::Null)), Err(shape("builtin_effects_enabled")));
    assert_eq!(clip_to_v2(&with("builtin_effects_enabled", json!([true, 1]))), Err(shape("builtin_effects_enabled")));
    assert_eq!(clip_to_v2(&with("builtin_effects", json!({}))), Err(shape("builtin_effects")));
    assert_eq!(clip_to_v2(&with("effect_keyframes", json!([]))), Err(shape("effect_keyframes")));
    assert!(matches!(clip_to_v2(&with("id", json!("clip-1"))), Err(ToV2Error::NotAnId { .. })));
    let mut no_edges = grade_graph();
    no_edges.as_object_mut().unwrap().remove("edges");
    assert_eq!(clip_to_v2(&with("node_graph", no_edges)), Err(shape("edges")));
    let mut null_links = grade_graph();
    null_links["nodes"][0]["input_links"] = Value::Null;
    assert_eq!(clip_to_v2(&with("node_graph", null_links)), Err(shape("input_links")));
}

#[test]
fn tracks_timelines_stages_and_versions_use_the_same_pieces() {
    let track = obj(json!({
        "id": N1,
        "effects": {"grade": grade_graph(), "video": [crop(0.2), crop(0.4)], "video_enabled": [false], "audio": [{"kind": "eq"}]}
    }));
    let g = track_to_v2(&track).unwrap();
    g.check().unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.type_id.as_str()).collect();
    assert_eq!(kinds, [types::LEGACY_GRADE, types::LEGACY_EFFECT, types::LEGACY_EFFECT]);
    assert_eq!((g.nodes[1].enabled, g.nodes[2].enabled), (false, true));
    assert_eq!(g.nodes[1].id, entry_id(RootKind::Track, uuid(N1), "effects", 0));
    assert_eq!(track_to_v2(&obj(json!({"id": N1}))).unwrap(), Graph::empty());
    assert_eq!(track_to_v2(&obj(json!({"id": N1, "effects": {"audio": []}}))).unwrap(), Graph::empty());

    let seq = obj(json!({"id": N2, "timeline_graph": grade_graph()}));
    let t = timeline_to_v2(&seq).unwrap().unwrap();
    assert_eq!(t.nodes.len(), 1);
    assert_eq!((t.output, t.nodes[0].id), (t.nodes[0].id, entry_id(RootKind::Sequence, uuid(N2), "timeline/grade", 0)));
    assert_eq!(timeline_to_v2(&obj(json!({"id": N2, "timeline_graph": null}))).unwrap(), None);

    let group = obj(json!({"id": N3, "name": "Ext", "post": grade_graph()}));
    let (pre, post) = stages_to_v2(&group).unwrap();
    assert!(pre.is_none(), "an absent stage stays absent");
    let post = post.unwrap();
    assert_eq!(post.nodes[0].id, entry_id(RootKind::GradeGroup, uuid(N3), "post/grade", 0));

    let version = obj(json!({"name": "Warm", "node_graph": grade_graph()}));
    let v = version_to_v2(uuid(CLIP), 1, &version).unwrap();
    assert_eq!(v.nodes[0].id, entry_id(RootKind::Clip, uuid(CLIP), "version/1/grade", 0));
    assert!(version_to_v2(uuid(CLIP), 0, &obj(json!({"name": "x"}))).is_err(), "a version's graph is required");
}

/// Converting twice gives the same graph, byte for byte.
#[test]
fn converting_again_gives_the_same_bytes() {
    let a = serde_json::to_string(&clip_to_v2(&clip()).unwrap()).unwrap();
    let b = serde_json::to_string(&clip_to_v2(&clip()).unwrap()).unwrap();
    assert_eq!(a, b);
}

/// Every picture store of a project file, converted and checked.
fn convert_project(project: &Value) -> Result<usize, String> {
    let mut stores = 0;
    for seq in project["sequences"].as_array().into_iter().flatten() {
        let seq = seq.as_object().ok_or("a sequence isn't an object")?;
        timeline_to_v2(seq).map_err(|e| e.to_string())?.map(|g| g.check()).transpose().map_err(|e| e.to_string())?;
        for group in seq.get("grade_groups").and_then(Value::as_array).into_iter().flatten() {
            let (pre, post) = stages_to_v2(group.as_object().ok_or("group")?).map_err(|e| e.to_string())?;
            for g in pre.iter().chain(post.iter()) {
                g.check().map_err(|e| e.to_string())?;
            }
        }
        for track in seq.get("tracks").and_then(Value::as_array).into_iter().flatten() {
            let track = track.as_object().ok_or("track")?;
            track_to_v2(track).map_err(|e| e.to_string())?.check().map_err(|e| e.to_string())?;
            for c in track.get("clips").and_then(Value::as_array).into_iter().flatten() {
                let c = c.as_object().ok_or("clip")?;
                clip_to_v2(c).map_err(|e| e.to_string())?.check().map_err(|e| e.to_string())?;
                let id = uuid(c["id"].as_str().unwrap_or_default());
                for (n, v) in c.get("grade_versions").and_then(Value::as_array).into_iter().flatten().enumerate() {
                    version_to_v2(id, n, v.as_object().ok_or("version")?).map_err(|e| e.to_string())?.check().map_err(|e| e.to_string())?;
                }
                stores += 1;
            }
        }
    }
    Ok(stores)
}

#[test]
fn the_repository_fixture_converts() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../klipp-project/tests/fixtures/pre_layer_fx/project.json");
    let project: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let clips = convert_project(&project).unwrap();
    assert!(clips > 0);
}

/// Every project under `$KLIPP_CONVERT_SWEEP` (one directory per line of
/// the file it names, as #1054's scan lists them), converted and checked.
/// Writes nothing. Run with `--ignored`.
#[test]
#[ignore]
fn every_listed_project_converts() {
    let list = std::env::var("KLIPP_CONVERT_SWEEP").expect("KLIPP_CONVERT_SWEEP names a file listing project directories");
    let (mut ok, mut failed) = (0, Vec::new());
    for dir in std::fs::read_to_string(list).unwrap().lines().filter(|l| !l.trim().is_empty()) {
        let file = std::path::Path::new(dir.trim()).join("project.json");
        let result = std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()))
            .and_then(|p| convert_project(&p));
        match result {
            Ok(_) => ok += 1,
            Err(e) => failed.push(format!("{}: {e}", file.display())),
        }
    }
    eprintln!("converted {ok}, failed {}", failed.len());
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn looks_stills_and_colour_presets_become_grade_groups() {
    let look = obj(json!({"id": N1, "name": "Teal", "node_graph": grade_graph()}));
    let g = look_to_v2(&look).unwrap();
    g.check().unwrap();
    assert_eq!(g.nodes.len(), 1);
    assert_eq!((g.output, g.nodes[0].id), (g.nodes[0].id, entry_id(RootKind::Look, uuid(N1), "grade", 0)));
    assert!(look_to_v2(&obj(json!({"id": N1, "name": "x"}))).is_err(), "a Look's graph is required");

    let still = obj(json!({"name": "Hero", "filename": "hero.png", "grade": grade_graph()}));
    let s = still_to_v2(&still).unwrap().unwrap();
    s.check().unwrap();
    assert_eq!(s.nodes[0].id, entry_id(RootKind::Still, Uuid::nil(), "hero.png/grade", 0));
    assert_eq!(still_to_v2(&obj(json!({"name": "Old", "filename": "old.png"}))).unwrap(), None, "a still without a grade has none");

    let p = colour_preset_to_v2("Bleach.json", &obj(grade_graph())).unwrap();
    p.check().unwrap();
    assert_eq!(p.nodes[0].id, entry_id(RootKind::Preset, Uuid::nil(), "Bleach.json/grade", 0));
    assert_ne!(p.nodes[0].id, colour_preset_to_v2("Other.json", &obj(grade_graph())).unwrap().nodes[0].id);
    // The body is the same faithful grade body as a clip's.
    assert_eq!(p.nodes[0].group.as_ref().unwrap().graph, look_to_v2(&look).unwrap().nodes[0].group.as_ref().unwrap().graph);
}
