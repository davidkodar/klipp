//! A grade group that publishes a port can't be held by today's grade
//! model, so the project is refused at load, by name, and its file is
//! left as it was, instead of opening without the port and saving it
//! back gone (#1183).

use klipp_core::Timecode;
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, Sequence};

#[test]
fn a_grade_group_publishing_a_port_refuses_the_project() {
    let dir = std::env::temp_dir().join(format!("klipp_published_port_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let scene = Sequence::new_default();
    let mut outer = Sequence::new_default();
    outer.tracks[0].clips = vec![Clip::sequence(scene.id, tc(0.0), tc(4.0))];
    save_project(&dir, &[outer, scene], &MediaPool::new()).unwrap();

    let path = dir.join("project.json");
    let mut json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let group = json["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|n| n["type_id"] == "klipp.group")
        .expect("the clip's grade is a group");
    let body = &mut group["group"];
    let corrector = body["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["type_id"] == "klipp.legacy.node").unwrap()["id"].clone();
    body["interface"] = serde_json::json!([{ "key": "graded", "direction": "output", "ty": "image", "binding": { "node": corrector, "port": "out" } }]);
    body["requires"] = serde_json::json!(["group-interface-1"]);
    std::fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();

    let err = load_project(&dir).expect_err("a published port is refused");
    assert!(err.contains("publishes ports (graded)"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), before, "refusing is not touching");
    let _ = std::fs::remove_dir_all(&dir);
}
