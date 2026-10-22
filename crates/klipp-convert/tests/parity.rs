// The app keeps this module private. Include its source only in this test
// rather than widening its public API or copying a second oracle.
#[rustfmt::skip]
#[allow(dead_code)]
#[path = "../../klipp-project/src/format.rs"]
mod current_format;

use klipp_convert::{v1, Document, Picture, StoreKind};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::path::Path;

fn parsed<T: DeserializeOwned + Serialize>(raw: &Value) -> Value {
    serde_json::to_value(serde_json::from_value::<T>(raw.clone()).unwrap()).unwrap()
}

fn same_fields(frozen: Value, current: Value, at: &str) {
    for (field, value) in frozen.as_object().unwrap() {
        assert_eq!(Some(value), current.get(field), "{at}/{field}");
    }
}

pub fn compare(raw: Value, kind: StoreKind) -> usize {
    let doc = Document::from_value(raw.clone(), kind).unwrap();
    assert_eq!(doc.raw(), &raw, "unconverted JSON must be identical");
    let encoded = serde_json::to_string(doc.raw()).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), raw);
    for field in &doc.pictures {
        let input = raw.pointer(&field.pointer).unwrap();
        let at = &field.pointer;
        match &field.picture {
            Picture::Clip(c) => {
                let mut current = parsed::<current_format::ClipJson>(input);
                if !input["builtin_effects"].is_null() {
                    current["builtin_effects"] =
                        parsed::<Vec<klipp_effects::BuiltinEffect>>(&input["builtin_effects"]);
                }
                same_fields(serde_json::to_value(c).unwrap(), current, at);
                if let Some(effects) = &c.builtin_effects {
                    assert_eq!(
                        serde_json::to_value(effects).unwrap(),
                        parsed::<Vec<klipp_effects::BuiltinEffect>>(&input["builtin_effects"])
                    );
                }
            }
            Picture::Track(t) => {
                let mut current = parsed::<current_format::TrackEffectsJson>(input);
                if !input["video"].is_null() {
                    current["video"] = parsed::<Vec<klipp_effects::BuiltinEffect>>(&input["video"]);
                }
                same_fields(serde_json::to_value(t).unwrap(), current, at);
            }
            Picture::Graph(g) => assert_eq!(
                serde_json::to_value(g).unwrap(),
                parsed::<klipp_nodes::NodeGraph>(input),
                "{at}"
            ),
            Picture::Group(g) => {
                for (name, graph) in [("pre", &g.pre), ("post", &g.post)] {
                    if let Some(graph) = graph {
                        assert_eq!(
                            serde_json::to_value(graph).unwrap(),
                            parsed::<klipp_nodes::NodeGraph>(&input[name]),
                            "{at}/{name}"
                        );
                    }
                }
            }
            Picture::Look(l) => assert_eq!(
                serde_json::to_value(&l.node_graph).unwrap(),
                parsed::<klipp_nodes::NodeGraph>(&input["node_graph"])
            ),
            Picture::Still(s) => {
                let current = parsed::<klipp_project::grade_stills::GradeStillMeta>(input);
                same_fields(serde_json::to_value(s).unwrap(), current, at);
            }
            Picture::Tool(t) => assert_eq!(
                serde_json::to_value(&t.params).unwrap(),
                parsed::<klipp_nodes::NodeParams>(&input["params"])
            ),
            Picture::Effect(e) => {
                if let Some(b) = &e.builtin {
                    assert_eq!(
                        serde_json::to_value(b).unwrap(),
                        parsed::<klipp_effects::BuiltinEffect>(&input["builtin"])
                    );
                }
                if let Some(p) = &e.ofx_params {
                    assert_eq!(
                        serde_json::to_value(p).unwrap(),
                        parsed::<std::collections::HashMap<String, klipp_core::OfxValue>>(
                            &input["ofx_params"]
                        )
                    );
                }
            }
        }
    }
    doc.pictures.len()
}

fn visit_projects(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            visit_projects(&path, files);
        } else if path.file_name().is_some_and(|n| n == "project.json") {
            files.push(path);
        }
    }
}

#[test]
fn every_repository_v1_fixture_matches() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../klipp-project/tests/fixtures");
    let mut files = Vec::new();
    visit_projects(&root, &mut files);
    assert!(!files.is_empty());
    for path in files {
        compare(
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap(),
            StoreKind::Project,
        );
    }
}

#[test]
fn picture_locations_and_side_stores_match() {
    let graph = serde_json::to_value(klipp_nodes::defaults::default_full_chain_graph()).unwrap();
    let mut raw: Value = serde_json::from_str(include_str!(
        "../../klipp-project/tests/fixtures/pre_layer_fx/project.json"
    ))
    .unwrap();
    let seq = &mut raw["sequences"][0];
    seq["timeline_graph"] = graph.clone();
    seq["grade_groups"] =
        json!([{ "id": "kept", "name": "group", "pre": graph, "post": graph, "future": false }]);
    seq["tracks"][0]["effects"] =
        json!({"video": [{"type":"gaussian_blur", "radius": 4.0}], "grade": graph, "future": 0});
    let clip = &mut seq["tracks"][0]["clips"][0];
    clip["node_graph"] = graph.clone();
    clip["grade_versions"] = json!([{ "name": "first", "node_graph": graph }]);
    clip["effects"] = json!([{ "plugin_name":"test", "param_values": {"legacy":2, "tagged":{"t":"Double", "v":0.25}} }]);
    clip["effect_keyframes"] = json!({"0:radius": [{"time_secs":0.0,"value":1.0,"interpolation":"Linear"}], "unknown": []});
    clip["param_modifiers"] =
        json!({"effect:0:radius": {"modifiers":[]}, "future": {"modifiers":[]}});
    raw["unrelated"] = json!({"kind":"Future", "params":{}, "audio": [false, 0, null]});
    compare(raw.clone(), StoreKind::Project);
    let seq = raw.as_object_mut().unwrap().remove("sequences").unwrap()[0].clone();
    raw["sequence"] = seq;
    compare(raw, StoreKind::Project);
    compare(graph.clone(), StoreKind::GradePreset);
    compare(
        json!({"id":"00000000-0000-0000-0000-000000000001", "name":"look", "node_graph":graph}),
        StoreKind::Look,
    );
    compare(
        json!([{ "name":"still", "filename":"still.png", "grade":graph, "extra":false }]),
        StoreKind::Stills,
    );
    compare(
        json!({"name":"tool", "tool":"Wheels", "params": {"lift_x":0.1, "compound_subgraph":graph}}),
        StoreKind::ToolPreset,
    );
    compare(
        json!({"name":"effect", "description":"", "effect_key":"blur", "builtin":{"type":"gaussian_blur","radius":3.0}, "ofx_params":{"p":{"Double2":[1.0,2.0]}}}),
        StoreKind::EffectPreset,
    );
}

#[test]
fn ofx_legacy_and_tagged_values_match() {
    for value in [
        json!(false),
        json!(12),
        json!(1.25),
        json!("a"),
        json!({"t":"Double3","v":[1.0,2.0,3.0]}),
        json!({"t":"Int", "v":4294967297_i64}),
        json!({"t":"Rgba","v":[0.1,0.2,0.3,1.0]}),
    ] {
        assert_eq!(
            parsed::<v1::OfxValueJson>(&value),
            parsed::<current_format::OfxValueJson>(&value)
        );
    }
}

#[test]
fn modifier_keys_match_including_unrecognised_names() {
    for key in [
        "opacity",
        "position_x",
        "position_y",
        "scale",
        "rotation",
        "volume",
        "pan",
        "gain",
        "effect:0:a:b",
        "effect:x:k",
        "effect:2:",
        "future",
        "effect:9999999999999999999999999:x",
    ] {
        assert_eq!(
            v1::ParamId::from_key(key).map(|p| p.to_key()),
            klipp_core::ParamId::from_key(key).map(|p| p.to_key())
        );
    }
    assert_eq!(
        klipp_convert::document::effect_key("12:a:b"),
        Some((12, "a:b"))
    );
    assert_eq!(klipp_convert::document::effect_key("bad:x"), None);
}

#[test]
fn frozen_copy_size_is_reported() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../klipp-graph-lift/src/v1");
    let lines: usize = std::fs::read_dir(root)
        .unwrap()
        .map(|e| {
            std::fs::read_to_string(e.unwrap().path())
                .unwrap()
                .lines()
                .count()
        })
        .sum();
    println!("Frozen v1 copy: {lines} lines");
    assert!(lines > 0);
}

/// Read-only. The lead supplies a newline-delimited list of project.json paths.
#[test]
#[ignore = "requires KLIPP_V1_PROJECT_LIST naming a file containing real project paths"]
fn all_real_projects_match() {
    let list = std::env::var_os("KLIPP_V1_PROJECT_LIST").expect("set KLIPP_V1_PROJECT_LIST");
    let paths = std::fs::read_to_string(list).unwrap();
    let mut files = 0;
    let mut sections = 0;
    for path in paths.lines().filter(|p| !p.is_empty()) {
        let raw = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        sections += compare(raw, StoreKind::Project);
        files += 1;
    }
    assert!(files > 0, "the list must not be empty");
    println!("Compared {files} projects, {sections} picture sections; no files written");
}

#[test]
fn structured_grade_settings_defaults_and_animation_match() {
    let key = klipp_core::Keyframe {
        time_secs: 1.25,
        value: 0.37,
        interpolation: klipp_core::Interpolation::Bezier,
        tangent_out: (0.2, 0.1),
        tangent_in: (-0.1, -0.2),
        handles: Default::default(),
    };
    let mut mask = klipp_core::Mask::default();
    mask.keyframes.center_x.push(key.clone());
    let params = klipp_nodes::NodeParams {
        qualifier: Some(klipp_core::QualifierSettings::default()),
        curves: Some(klipp_core::CurveSet::default()),
        huesat: Some(klipp_core::HueSatCurves::default()),
        luma_key: Some(klipp_core::LumaKeySettings::default()),
        difference_key: Some(klipp_core::DifferenceKeySettings::default()),
        soft_clip: Some(klipp_core::SoftClipSettings::default()),
        grain: Some(klipp_core::FilmGrainSettings::default()),
        warper_params: Some(klipp_core::ColorWarperParams::default()),
        color_match: Some(klipp_core::ColorMatchSettings::default()),
        masks: Some(klipp_core::MaskStack { masks: vec![mask] }),
        lut_path: Some("bundled:film-slide/kodak_kodachrome_64.cube".into()),
        log_format: Some(klipp_core::LogFormat::SLog3),
        ..Default::default()
    };
    let value = serde_json::to_value(params).unwrap();
    assert_eq!(
        parsed::<v1::NodeParams>(&value),
        parsed::<klipp_nodes::NodeParams>(&value)
    );
    let node = json!({"id":"00000000-0000-0000-0000-000000000001", "kind":"Corrector", "label":"test", "enabled":false, "params":value,
        "keyframes":{"tracks":[{"param_name":"exposure","keyframes":[key]}]}, "output_link":"test", "input_links":[null,"source"]});
    assert_eq!(
        parsed::<v1::Node>(&node),
        parsed::<klipp_nodes::Node>(&node)
    );
    // Legacy settings omit newly added optional fields: compare their defaults.
    for value in [
        json!({"curves":{}}),
        json!({"huesat":{}}),
        json!({"warper_params":{}}),
    ] {
        assert_eq!(
            parsed::<v1::NodeParams>(&value),
            parsed::<klipp_nodes::NodeParams>(&value)
        );
    }
}

#[test]
fn effect_and_modifier_variants_match() {
    for value in [
        json!({"type":"gaussian_blur","radius":3.25}),
        json!({"type":"frei0r","type_id":"plugin","params":[{"Bool":true},{"Double":0.2},{"Color":[0.1,0.2,0.3]},{"Position":[0.2,0.3]},{"String":"s"}]}),
        json!({"type":"corner_pin","tl":[0.1,0.2],"tr":[0.8,0.1],"br":[0.9,0.8],"bl":[0.2,0.9]}),
    ] {
        assert_eq!(
            parsed::<v1::BuiltinEffect>(&value),
            parsed::<klipp_effects::BuiltinEffect>(&value)
        );
    }
    for value in [
        json!({"type":"oscillate","amplitude":2.0,"frequency":0.5}),
        json!({"type":"ramp","start_value":0.2,"end_value":0.8}),
        json!({"type":"wiggle","amplitude":1.0,"frequency":0.4}),
        json!({"type":"throw","velocity":3.0}),
        json!({"type":"loop","period_secs":2.0}),
        json!({"type":"ping_pong","period_secs":2.0}),
        json!({"type":"clamp","min":0.1,"max":0.9}),
    ] {
        assert_eq!(
            parsed::<v1::Modifier>(&value),
            parsed::<klipp_core::Modifier>(&value)
        );
    }
}

#[test]
fn unreadable_picture_content_is_refused_without_writing() {
    let node = json!({"id":"00000000-0000-0000-0000-000000000001", "kind":"FutureGrade", "label":"n", "enabled":true, "params":{}});
    let graph =
        json!({"nodes":[node],"edges":[],"output_node_id":"00000000-0000-0000-0000-000000000001"});
    assert_eq!(
        v1::json_check::losses(&graph),
        klipp_nodes::json_check::losses(&graph)
    );
    assert!(Document::from_value(graph, StoreKind::GradePreset).is_err());
    let preset = json!({"name":"x","tool":"FullGrade","params":{"compound_subgraph":{"nodes":[node],"edges":[],"output_node_id":"00000000-0000-0000-0000-000000000001"}}});
    assert!(Document::from_value(preset, StoreKind::ToolPreset).is_err());
    for value in [json!([{"type":"future_effect"}]), json!({}), json!([null])] {
        assert!(
            serde_json::from_value::<Vec<klipp_effects::BuiltinEffect>>(value.clone()).is_err()
        );
        assert!(serde_json::from_value::<Vec<v1::BuiltinEffect>>(value).is_err());
    }
    assert!(Document::from_value(
        json!({"format_revision":1,"sequences":[]}),
        StoreKind::Project
    )
    .is_err());
}

#[test]
fn preservation_follow_up_keeps_unknown_empty_values_distinct_from_defaults() {
    for empty in [json!(0), json!(0.0), json!(false), json!(""), json!([])] {
        // Slash and tilde also exercise the refusal probe's JSON Pointer escaping.
        let params = json!({"exposure":0.5, "future/~mix":empty});
        assert_eq!(
            v1::json_check::params_losses(&params),
            vec!["params.future/~mix"]
        );
        let preset = json!({"name":"future", "tool":"FullGrade", "params":params});
        assert!(Document::from_value(preset, StoreKind::ToolPreset).is_err());
    }
    let mut mask = serde_json::to_value(klipp_core::Mask::default()).unwrap();
    mask["keyframes"]["track_x"] = json!([]);
    let preset = json!({"name":"known", "tool":"FullGrade", "params":{"masks":{"masks":[mask]},"unused":null,"empty":{}}});
    let doc = Document::from_value(preset.clone(), StoreKind::ToolPreset).unwrap();
    assert_eq!(doc.raw(), &preset);
}
