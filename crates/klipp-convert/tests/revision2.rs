//! Revision 2 (#1070): the converted project and side stores, their
//! proof, and what today's build does with the side files: it must leave
//! them alone, neither loading nor pruning nor overwriting them.

use klipp_convert::revision2::{self, REVISION};
use klipp_graph_format::lifted::{ClipPicture, LiftNote};
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
    assert_eq!(seq["tracks"][0]["effects"]["video"], json!([{"type": "graph_v2"}]), "the track's sentinel");
    assert_eq!(clip["grade_versions"][0]["node_graph"], klipp_convert::old_fields::placeholder_graph(), "a version's placeholder");
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

/// A written side store is held to the original outside its pictures: a
/// Look's name or a still's source that changed or went is refused
/// (#1102 review, 9).
#[test]
fn a_side_store_keeps_everything_outside_its_pictures() {
    let original = json!({"id": "00000000-0000-0000-0000-00000000100c", "name": "Teal", "node_graph": grade()});
    let written = revision2::look(&original).unwrap();
    revision2::verify_look(&original, &serde_json::to_string(&written).unwrap()).unwrap();
    let mut renamed = written.clone();
    renamed["name"] = json!("Orange");
    assert!(revision2::verify_look(&original, &serde_json::to_string(&renamed).unwrap()).is_err());

    let original = json!([{"name": "Hero", "filename": "hero.png", "source": "clip-7", "grade": grade()}]);
    let written = revision2::stills(&original).unwrap();
    revision2::verify_stills(&original, &serde_json::to_string(&written).unwrap()).unwrap();
    let mut lost = written.clone();
    lost["stills"][0].as_object_mut().unwrap().remove("source");
    assert!(revision2::verify_stills(&original, &serde_json::to_string(&lost).unwrap()).is_err());

    let preset = revision2::colour_preset("Bleach.json", &grade()).unwrap();
    revision2::verify_colour_preset("Bleach.json", &grade(), &serde_json::to_string(&preset).unwrap()).unwrap();
    let mut extra = preset.clone();
    extra["name"] = json!("Bleach");
    assert!(revision2::verify_colour_preset("Bleach.json", &grade(), &serde_json::to_string(&extra).unwrap()).is_err());
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

/// A body standing alone keeps its parked edges: a track grade whose
/// correction has a port-1 edge and no picture writes it on the body's own
/// input, kept for the record as a clip's is.
#[test]
fn a_bare_body_keeps_its_parked_edges() {
    let s = "00000000-0000-0000-0000-0000000000b0";
    let c = "00000000-0000-0000-0000-0000000000b1";
    let k = "00000000-0000-0000-0000-0000000000b2";
    let o = "00000000-0000-0000-0000-0000000000b3";
    let g = json!({
        "nodes": [
            {"id": s, "kind": "Source", "label": "Source", "enabled": true},
            {"id": c, "kind": "Corrector", "label": "C", "enabled": true},
            {"id": k, "kind": "Qualifier", "label": "Q", "enabled": true},
            {"id": o, "kind": "Output", "label": "Output", "enabled": true}
        ],
        "edges": [
            {"from_node": k, "from_port": 0, "to_node": c, "to_port": 1},
            {"from_node": c, "from_port": 0, "to_node": o, "to_port": 0}
        ],
        "output_node_id": o
    });
    let mut p = project();
    p["sequences"][0]["tracks"][0]["effects"]["grade"] = g;
    let (out, notes) = revision2::project(&p).unwrap();
    let body: Graph = serde_json::from_value(out["sequences"][0]["tracks"][0]["effects"]["graph_v2"]["grade"].clone()).unwrap();
    let input = body.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::GROUP_INPUT).unwrap();
    assert_eq!(input.legacy_flags.as_ref().unwrap().parked_edges.len(), 1);
    assert!(notes.iter().any(|n| matches!(n, klipp_graph_format::lifted::LiftNote::ParkedEdge { .. })), "and reported");
}

/// A grade standing alone that loops (a track grade here) is a stop for
/// that project: merged into a clip today it fails the clip's whole grade,
/// which is for a person to settle, not for the conversion to guess.
#[test]
fn a_looping_track_grade_stops_the_project() {
    let (s, a, b, o) = ("00000000-0000-0000-0000-0000000000c0", "00000000-0000-0000-0000-0000000000c1", "00000000-0000-0000-0000-0000000000c2", "00000000-0000-0000-0000-0000000000c3");
    let g = json!({
        "nodes": [
            {"id": s, "kind": "Source", "label": "S", "enabled": true},
            {"id": a, "kind": "Corrector", "label": "A", "enabled": true, "input_links": [null, "loop"]},
            {"id": b, "kind": "Saturation", "label": "B", "enabled": true, "output_link": "loop"},
            {"id": o, "kind": "Output", "label": "O", "enabled": true}
        ],
        "edges": [
            {"from_node": s, "from_port": 0, "to_node": a, "to_port": 0},
            {"from_node": a, "from_port": 0, "to_node": b, "to_port": 0},
            {"from_node": b, "from_port": 0, "to_node": o, "to_port": 0}
        ],
        "output_node_id": o
    });
    let mut p = project();
    p["sequences"][0]["tracks"][0]["effects"]["grade"] = g;
    assert!(revision2::project(&p).unwrap_err().contains("loops"));
}

/// A clip in a grading group whose wireless link names meet a stage's,
/// with a sender on either side, is noted: today's bake can cross them,
/// revision 2 keeps them apart (#1102 review).
#[test]
fn a_stage_and_a_clip_sharing_link_names_are_noted() {
    let node = |id: &str, kind: &str, extra: Value| {
        let mut n = json!({"id": id, "kind": kind, "label": kind, "enabled": true});
        n.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        n
    };
    let g = |nodes: Vec<Value>, ids: [&str; 3]| json!({
        "nodes": nodes,
        "edges": [{"from_node": ids[0], "from_port": 0, "to_node": ids[1], "to_port": 0}, {"from_node": ids[1], "from_port": 0, "to_node": ids[2], "to_port": 0}],
        "output_node_id": ids[2]
    });
    let (s1, e1, o1) = ("00000000-0000-0000-0000-0000000000b1", "00000000-0000-0000-0000-0000000000b2", "00000000-0000-0000-0000-0000000000b3");
    let (s2, e2, o2) = ("00000000-0000-0000-0000-0000000000c1", "00000000-0000-0000-0000-0000000000c2", "00000000-0000-0000-0000-0000000000c3");
    let project = |stage_link: &str| {
        let pre = g(vec![node(s1, "Source", json!({})), node(e1, "Exposure", json!({"output_link": stage_link})), node(o1, "Output", json!({}))], [s1, e1, o1]);
        let own = g(vec![node(s2, "Source", json!({})), node(e2, "Saturation", json!({"input_links": [null, "bus"]})), node(o2, "Output", json!({}))], [s2, e2, o2]);
        json!({"version": "1.0.0", "sequences": [{"id": "00000000-0000-0000-0000-000000000501", "name": "S",
            "grade_groups": [{"id": "00000000-0000-0000-0000-000000000601", "name": "G", "pre": pre}],
            "tracks": [{"id": "00000000-0000-0000-0000-000000000701", "name": "V1", "kind": "Video",
                "clips": [{"id": "00000000-0000-0000-0000-000000000c01", "kind": {"type": "media", "media_id": "00000000-0000-0000-0000-0000000000d1"}, "node_graph": own, "grade_group": "00000000-0000-0000-0000-000000000601"}]}]}]})
    };
    let (_, notes) = revision2::project(&project("bus")).unwrap();
    let cross: Vec<&LiftNote> = notes.iter().filter(|n| matches!(n, LiftNote::CrossGroupWireless { .. })).collect();
    assert_eq!(cross.len(), 1, "the stage sends what the clip receives");
    assert!(matches!(cross[0], LiftNote::CrossGroupWireless { names, .. } if names == &["bus".to_owned()]));
    let (_, notes) = revision2::project(&project("other")).unwrap();
    assert!(!notes.iter().any(|n| matches!(n, LiftNote::CrossGroupWireless { .. })), "no shared name, no note");
    // The track's grade merges into the same namespace, group or none.
    let mut p = project("other");
    let pre = p["sequences"][0]["grade_groups"][0]["pre"].clone();
    let mut grade = pre;
    grade["nodes"][1]["output_link"] = json!("bus");
    p["sequences"][0]["tracks"][0]["effects"] = json!({"grade": grade});
    p["sequences"][0]["tracks"][0]["clips"][0].as_object_mut().unwrap().remove("grade_group");
    let (_, notes) = revision2::project(&p).unwrap();
    assert!(notes.iter().any(|n| matches!(n, LiftNote::CrossGroupWireless { group, names, .. } if group.is_nil() && names == &["bus".to_owned()])), "the track's grade sends what the clip receives");
}
