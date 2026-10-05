//! A project whose grade holds a node kind or a setting this Klipp does
//! not know is refused by name, as an unknown effect is, instead of
//! opening with the node run as a passthrough and losing it on the next
//! save (#993).

use klipp_core::Timecode;
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, Sequence};

fn saved_with_a_grade(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let scene = Sequence::new_default();
    let mut outer = Sequence::new_default();
    let clip = Clip::sequence(scene.id, tc(0.0), tc(4.0));
    outer.tracks[0].clips = vec![clip];
    save_project(&dir, &[outer, scene], &MediaPool::new()).unwrap();
    dir
}

fn edit(dir: &std::path::Path, from: &str, to: &str) -> Vec<u8> {
    let path = dir.join("project.json");
    let json = std::fs::read_to_string(&path).unwrap();
    assert!(json.contains(from), "the saved project has {from}");
    std::fs::write(&path, json.replacen(from, to, 1)).unwrap();
    std::fs::read(&path).unwrap()
}

#[test]
fn a_node_kind_from_a_newer_klipp_refuses_the_project_by_name() {
    let dir = saved_with_a_grade("unknown_node_kind");
    let before = edit(&dir, "\"Corrector\"", "\"HolographicKey\"");
    let err = load_project(&dir).expect_err("an unknown node kind is refused");
    assert!(err.contains("HolographicKey") && err.contains("newer Klipp"), "{err}");
    assert_eq!(std::fs::read(dir.join("project.json")).unwrap(), before, "refusing is not touching");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_node_setting_from_a_newer_klipp_refuses_the_project_by_name() {
    let dir = saved_with_a_grade("unknown_node_setting");
    // A setting a newer Klipp added to the Corrector, as a newer save
    // would write it.
    let path = dir.join("project.json");
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    fn corrector(v: &mut serde_json::Value) -> Option<&mut serde_json::Value> {
        if v.get("kind").and_then(|k| k.as_str()) == Some("Corrector") {
            return Some(v);
        }
        match v {
            serde_json::Value::Object(o) => o.values_mut().find_map(corrector),
            serde_json::Value::Array(a) => a.iter_mut().find_map(corrector),
            _ => None,
        }
    }
    corrector(&mut v).expect("the default grade has a Corrector")["params"]["sparkle"] = serde_json::Value::from(2.0);
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let err = load_project(&dir).expect_err("an unknown node setting is refused");
    assert!(err.contains("sparkle"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_project_from_this_klipp_opens() {
    let dir = saved_with_a_grade("known_nodes");
    load_project(&dir).expect("every node and setting this build writes, it reads back");
    let _ = std::fs::remove_dir_all(&dir);
}
