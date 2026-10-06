//! Revision 2 (#1070): the converted project and side stores, their
//! proof, and what today's build does with the side files: it must leave
//! them alone, neither loading nor pruning nor overwriting them.

use klipp_convert::revision2::{self, REVISION};
use klipp_graph_format::lifted::ClipPicture;
use klipp_graph_format::Graph;
use serde_json::{json, Value};
use std::path::PathBuf;

const N: &str = "00000000-0000-0000-0000-0000000000a1";
const S: &str = "00000000-0000-0000-0000-0000000000a0";
const O: &str = "00000000-0000-0000-0000-0000000000a2";

fn grade() -> Value {
    json!({
        "nodes": [
            {"id": S, "kind": "Source", "label": "Source", "enabled": true},
            {"id": N, "kind": "Exposure", "label": "E", "enabled": true, "params": {"exposure": 0.5}},
            {"id": O, "kind": "Output", "label": "Output", "enabled": true}
        ],
        "edges": [
            {"from_node": S, "from_port": 0, "to_node": N, "to_port": 0},
            {"from_node": N, "from_port": 0, "to_node": O, "to_port": 0}
        ],
        "output_node_id": O
    })
}

fn project() -> Value {
    let crop = json!({"type": "crop", "left": 0.1, "right": 0.0, "top": 0.0, "bottom": 0.0, "feather": 0.0});
    json!({
        "version": "1.0.0", "notes": "keep",
        "sequences": [{
            "id": "00000000-0000-0000-0000-000000000501", "name": "Main", "timeline_graph": grade(),
            "grade_groups": [{"id": "00000000-0000-0000-0000-000000000601", "name": "Ext", "post": grade()}],
            "tracks": [{
                "id": "00000000-0000-0000-0000-000000000701", "name": "V1", "kind": "Video",
                "effects": {"video": [crop.clone()], "grade": grade(), "audio": [{"kind": "eq"}]},
                "clips": [{
                    "id": "00000000-0000-0000-0000-000000000c01", "kind": {"type": "media", "media_id": "00000000-0000-0000-0000-0000000000d1"},
                    "node_graph": grade(), "builtin_effects": [crop, {"type": "opacity", "value": 0.5}],
                    "param_modifiers": {"volume": {"modifiers": []}},
                    "grade_versions": [{"name": "Warm", "node_graph": grade()}]
                }]
            }]
        }]
    })
}

#[test]
fn a_project_converts_to_revision_2_and_proves_itself() {
    let p = project();
    let (out, _notes) = revision2::project(&p).unwrap();
    assert_eq!(out["format_revision"], REVISION);
    let text = serde_json::to_string(&out).unwrap();
    revision2::verify_project(&p, &text).unwrap();

    let seq = &out["sequences"][0];
    let clip = &seq["tracks"][0]["clips"][0];
    let picture: ClipPicture = serde_json::from_value(clip["graph_v2"].clone()).unwrap();
    picture.graph.check_lifted(false).unwrap();
    assert_eq!(picture.opacity, 0.5, "the clip's opacity is a property now");
    for body in [&seq["graph_v2"], &seq["grade_groups"][0]["graph_v2"]["post"], &seq["tracks"][0]["effects"]["graph_v2"]["grade"], &clip["grade_versions"][0]["graph_v2"]] {
        let g: Graph = serde_json::from_value(body.clone()).unwrap();
        g.check_lifted_body().unwrap();
    }
    assert!(seq["grade_groups"][0]["graph_v2"].get("pre").is_none(), "an absent stage stays absent");
    // Old fields as revision 1: the sentinel, placeholders, sound kept.
    assert_eq!(clip["builtin_effects"], json!([{"type": "graph_v2"}]));
    assert_eq!(seq["tracks"][0]["effects"]["audio"], json!([{"kind": "eq"}]));
    assert_eq!(clip["param_modifiers"], json!({"volume": {"modifiers": []}}));
}

#[test]
fn a_damaged_revision_2_project_is_caught() {
    let p = project();
    let (good, _) = revision2::project(&p).unwrap();
    let damage = |f: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        f(&mut v);
        revision2::verify_project(&p, &serde_json::to_string(&v).unwrap())
    };
    assert!(damage(&|v| v["format_revision"] = json!(1)).is_err());
    assert!(damage(&|v| v["notes"] = json!("changed")).is_err());
    assert!(damage(&|v| { v["sequences"][0].as_object_mut().unwrap().remove("graph_v2"); }).is_err());
    assert!(damage(&|v| v["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["opacity"] = json!(0.9)).is_err());
    assert!(damage(&|v| v["sequences"][0]["tracks"][0]["effects"]["audio"] = json!([])).is_err());
}

#[test]
fn side_stores_take_their_own_revision_and_drop_their_old_graph() {
    let l = revision2::look(&json!({"id": "00000000-0000-0000-0000-00000000e001", "name": "Teal", "node_graph": grade()})).unwrap();
    assert_eq!(l["format_revision"], REVISION);
    assert!(l.get("node_graph").is_none(), "no placeholder a reader could take for an identity Look");
    serde_json::from_value::<Graph>(l["graph_v2"].clone()).unwrap().check_lifted_body().unwrap();

    let s = revision2::stills(&json!([{"name": "Hero", "filename": "hero.png", "grade": grade()}, {"name": "Old", "filename": "old.png"}])).unwrap();
    assert_eq!(s["format_revision"], REVISION);
    assert!(s["stills"][0].get("grade").is_none() && s["stills"][0].get("graph_v2").is_some());
    assert!(s["stills"][1].get("graph_v2").is_none(), "a still without a grade has none");

    let c = revision2::colour_preset("Bleach.json", &grade()).unwrap();
    assert_eq!(c.as_object().unwrap().keys().collect::<Vec<_>>(), ["format_revision", "graph_v2"]);
    revision2::verify_side(&c, &serde_json::to_string(&c).unwrap()).unwrap();
    let mut changed = c.clone();
    changed["format_revision"] = json!(1);
    assert!(revision2::verify_side(&c, &serde_json::to_string(&changed).unwrap()).is_err());
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("klipp-rev2-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Today's build reads a revision-2 Look as nothing, and its save (which
/// prunes Looks it didn't load) leaves the file alone, byte for byte.
#[test]
fn todays_build_leaves_a_revision_2_look_alone() {
    let dir = scratch("look");
    let id = "00000000-0000-0000-0000-00000000e001";
    let l = revision2::look(&json!({"id": id, "name": "Teal", "node_graph": grade()})).unwrap();
    let text = serde_json::to_string_pretty(&l).unwrap();
    std::fs::create_dir_all(dir.join("looks")).unwrap();
    let path = dir.join("looks").join(format!("{id}.json"));
    std::fs::write(&path, &text).unwrap();
    assert!(klipp_project::load_looks(&dir).is_empty(), "not loaded");
    klipp_project::save_looks(&dir, &[]).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "not pruned or rewritten");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Today's build reads a revision-2 stills index as no stills, and saving
/// stills over it leaves it alone, byte for byte.
#[test]
fn todays_build_leaves_a_revision_2_stills_index_alone() {
    let dir = scratch("stills");
    let s = revision2::stills(&json!([{"name": "Hero", "filename": "hero.png", "grade": grade()}])).unwrap();
    let text = serde_json::to_string_pretty(&s).unwrap();
    std::fs::create_dir_all(dir.join("media")).unwrap();
    let path = dir.join("media/stills.json");
    std::fs::write(&path, &text).unwrap();
    assert!(klipp_project::load_grade_stills(&dir).is_empty(), "not loaded");
    // A new still saved over it: today's save refuses rather than overwrite.
    let still = klipp_project::GradeStillData { name: "New".into(), filename: "new.png".into(), rgba: vec![255; 16], width: 2, height: 2, source: None, grade: None };
    assert!(klipp_project::save_grade_stills(&dir, &[still]).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "not overwritten");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A project with no clip and no picture is still written at revision 2:
/// after the switch the app reads nothing older.
#[test]
fn an_empty_project_is_revision_2_too() {
    let p = json!({"version": "1.0.0", "sequences": [{"id": "00000000-0000-0000-0000-000000000501", "name": "S", "tracks": []}]});
    let (out, _) = revision2::project(&p).unwrap();
    assert_eq!(out["format_revision"], REVISION);
    revision2::verify_project(&p, &serde_json::to_string(&out).unwrap()).unwrap();
}
