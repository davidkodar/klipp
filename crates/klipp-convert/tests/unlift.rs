//! `unlift` (#1089): a revision-2 store back to the v1 form today's reader
//! loads. The way back to revision 2 must give the same file, byte for
//! byte: `revision2(unlift(v)) == v`, for projects, Looks, stills indexes
//! and colour presets, over fixtures and (with `KLIPP_SWEEP_SCAN`) every
//! real project.

use std::path::{Path, PathBuf};

use klipp_convert::revision2;
use klipp_convert::unlift;
use serde_json::{json, Value};

const S: &str = "00000000-0000-0000-0000-000000000011";
const N: &str = "00000000-0000-0000-0000-000000000012";
const O: &str = "00000000-0000-0000-0000-000000000013";

fn grade() -> Value {
    json!({
        "nodes": [
            {"id": S, "kind": "Source", "label": "Source", "enabled": true},
            {"id": N, "kind": "Exposure", "label": "Exposure", "enabled": true, "params": {"exposure": 0.25}},
            {"id": O, "kind": "Output", "label": "Output", "enabled": true}
        ],
        "edges": [{"from_node": S, "from_port": 0, "to_node": N, "to_port": 0}, {"from_node": N, "from_port": 0, "to_node": O, "to_port": 0}],
        "output_node_id": O,
        "node_positions": {S: {"x": 0.0, "y": 0.0}, N: {"x": 220.0, "y": 0.0}, O: {"x": 440.0, "y": 0.0}}
    })
}

/// A project using every store: a timeline grade, a group's stages, a
/// track's grade and effects, clips with effects, keys, a version, a kept-off
/// Opacity, a disabled effect.
fn project() -> Value {
    let crop = json!({"type": "crop", "left": 0.1, "right": 0.0, "top": 0.0, "bottom": 0.0, "feather": 0.0});
    json!({
        "version": "1.0.0", "notes": "keep",
        "sequences": [{
            "id": "00000000-0000-0000-0000-000000000501", "name": "Main", "timeline_graph": grade(),
            "grade_groups": [{"id": "00000000-0000-0000-0000-000000000601", "name": "Ext", "pre": grade(), "post": grade()}],
            "tracks": [{
                "id": "00000000-0000-0000-0000-000000000701", "name": "V1", "kind": "Video",
                "effects": {"video": [crop.clone()], "video_enabled": [false], "grade": grade(), "audio": [{"kind": "eq"}]},
                "clips": [{
                    "id": "00000000-0000-0000-0000-000000000c01", "kind": {"type": "media", "media_id": "00000000-0000-0000-0000-0000000000d1"},
                    "node_graph": grade(), "grade_group": "00000000-0000-0000-0000-000000000601",
                    "builtin_effects": [crop, {"type": "opacity", "value": 0.5}, {"type": "opacity", "value": 0.2}],
                    "builtin_effects_enabled": [true, false, true],
                    "effect_keyframes": {"0:left": [{"time_secs": 0.0, "value": 0.1}, {"time_secs": 1.0, "value": 0.3}]},
                    "param_modifiers": {"volume": {"modifiers": []}},
                    "grade_versions": [{"name": "Warm", "node_graph": grade()}]
                }, {
                    "id": "00000000-0000-0000-0000-000000000c02", "kind": {"type": "media", "media_id": "00000000-0000-0000-0000-0000000000d1"}
                }]
            }]
        }]
    })
}

/// `revision2(unlift(v)) == v`, compared as written text.
fn back_and_forth(v2: &Value) -> Result<(), String> {
    let v1 = unlift::project(v2)?;
    let (again, _) = revision2::project(&v1)?;
    let (a, b) = (serde_json::to_string(v2).unwrap(), serde_json::to_string(&again).unwrap());
    if a != b {
        return Err(first_diff(v2, &again, ""));
    }
    Ok(())
}

fn first_diff(a: &Value, b: &Value, at: &str) -> String {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => x.keys().chain(y.keys()).map(|k| first_diff(x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), &format!("{at}/{k}"))).find(|d| !d.is_empty()).unwrap_or_default(),
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x.iter().zip(y).enumerate().map(|(i, (p, q))| first_diff(p, q, &format!("{at}/{i}"))).find(|d| !d.is_empty()).unwrap_or_default(),
        _ if a == b => String::new(),
        _ => format!("{at}: {a} vs {b}"),
    }
}

#[test]
fn a_project_comes_back_to_the_same_revision_2_file() {
    let (v2, _) = revision2::project(&project()).unwrap();
    back_and_forth(&v2).unwrap();
    // And unlift reads it back to the v1 picture it came from, as today's
    // loader leaves it: every entry, the kept-off ones included.
    let v1 = unlift::project(&v2).unwrap();
    let clip = &v1["sequences"][0]["tracks"][0]["clips"][0];
    assert_eq!(clip["builtin_effects"].as_array().unwrap().len(), 3, "the disabled and the superseded Opacity too");
    assert_eq!(clip["builtin_effects_enabled"], json!([true, false, true]));
    assert!(v1.get("format_revision").is_none(), "today's reader reads no revision");
}

#[test]
fn side_stores_come_back_to_the_same_revision_2_files() {
    let look = json!({"id": "00000000-0000-0000-0000-00000000100c", "name": "Teal", "node_graph": grade()});
    let v2 = revision2::look(&look).unwrap();
    assert_eq!(revision2::look(&unlift::look(&v2).unwrap()).unwrap(), v2);
    let stills = json!([{"name": "Hero", "filename": "hero.png", "source": "clip-7", "grade": grade()}, {"name": "Plain", "filename": "plain.png"}]);
    let v2 = revision2::stills(&stills).unwrap();
    assert_eq!(revision2::stills(&unlift::stills(&v2).unwrap()).unwrap(), v2);
    let v2 = revision2::colour_preset("Bleach.json", &grade()).unwrap();
    assert_eq!(revision2::colour_preset("Bleach.json", &unlift::colour_preset("Bleach.json", &v2).unwrap()).unwrap(), v2);
}

/// The repository's v1 fixtures, each converted and brought back.
#[test]
fn every_repository_fixture_comes_back() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../klipp-project/tests/fixtures");
    let mut seen = 0;
    for entry in std::fs::read_dir(&root).unwrap().flatten() {
        let file = entry.path().join("project.json");
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let raw: Value = serde_json::from_str(&text).unwrap();
        let (v2, _) = revision2::project(&raw).unwrap();
        back_and_forth(&v2).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        seen += 1;
    }
    assert!(seen > 0, "no fixture project found under {}", root.display());
}

/// Every real project a klipp-convert scan lists, with its Looks and its
/// stills index: converted, brought back and converted again, unchanged.
#[test]
#[ignore = "reads every project on this machine; run with KLIPP_SWEEP_SCAN"]
fn every_real_project_comes_back() {
    let Some(scan) = std::env::var_os("KLIPP_SWEEP_SCAN") else {
        eprintln!("skipped: set KLIPP_SWEEP_SCAN to a klipp-convert scan report");
        return;
    };
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&scan).unwrap()).unwrap();
    let (mut projects, mut stores, mut failures) = (0, 0, Vec::new());
    for p in report["projects"].as_array().unwrap() {
        let dir = PathBuf::from(p["path"].as_str().unwrap());
        let Ok(text) = std::fs::read_to_string(dir.join("project.json")) else { continue };
        let Ok(raw) = serde_json::from_str::<Value>(&text) else { continue };
        let Ok((v2, _)) = revision2::project(&raw) else { continue };
        projects += 1;
        if let Err(e) = back_and_forth(&v2) {
            failures.push(format!("{}: {e}", dir.display()));
        }
        for look in std::fs::read_dir(dir.join("looks")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")) {
            let raw: Value = serde_json::from_str(&std::fs::read_to_string(&look).unwrap()).unwrap();
            let v2 = revision2::look(&raw).unwrap();
            stores += 1;
            if revision2::look(&unlift::look(&v2).unwrap()).unwrap() != v2 {
                failures.push(format!("{}: changed", look.display()));
            }
        }
        if let Ok(text) = std::fs::read_to_string(dir.join("media/stills.json")) {
            let raw: Value = serde_json::from_str(&text).unwrap();
            let v2 = revision2::stills(&raw).unwrap();
            stores += 1;
            if revision2::stills(&unlift::stills(&v2).unwrap()).unwrap() != v2 {
                failures.push(format!("{}: stills changed", dir.display()));
            }
        }
    }
    eprintln!("real projects: {projects} and {stores} side stores back and forth");
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}
