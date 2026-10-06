//! Looks and the grade stills index in revision 2 (#1089): saved as
//! revision 2 and read back; one from before revision 2 is neither read nor
//! removed, and isn't written over.

use klipp_project::{load_grade_stills, load_looks, save_grade_stills, save_looks, GradeStillData};
use klipp_timeline::Look;

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("klipp-side-rev2-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn look(name: &str) -> Look {
    Look { id: uuid::Uuid::new_v4(), name: name.into(), node_graph: klipp_nodes::NodeGraph::default() }
}

#[test]
fn a_look_saves_as_revision_2_and_reads_back() {
    let dir = scratch("look");
    let l = look("Teal");
    save_looks(&dir, std::slice::from_ref(&l)).unwrap();
    let file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("looks").join(format!("{}.json", l.id))).unwrap()).unwrap();
    assert_eq!(file["format_revision"], 2);
    assert!(file.get("node_graph").is_none() && file.get("graph_v2").is_some());
    let back = load_looks(&dir);
    assert_eq!(back.len(), 1);
    assert_eq!((back[0].id, back[0].name.as_str()), (l.id, "Teal"));
    assert_eq!(back[0].node_graph, l.node_graph);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A Look from before revision 2 isn't read, and a later save, which prunes
/// the Looks it didn't load, leaves it alone byte for byte.
#[test]
fn an_unconverted_look_is_neither_read_nor_removed() {
    let dir = scratch("old_look");
    let old = look("Old");
    std::fs::create_dir_all(dir.join("looks")).unwrap();
    let path = dir.join("looks").join(format!("{}.json", old.id));
    let text = serde_json::to_string_pretty(&old).unwrap();
    std::fs::write(&path, &text).unwrap();
    assert!(load_looks(&dir).is_empty(), "not read");
    save_looks(&dir, &[look("New")]).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "and not removed by the save's pruning");
    let _ = std::fs::remove_dir_all(&dir);
}

fn still(name: &str) -> GradeStillData {
    GradeStillData { name: name.into(), filename: format!("{name}.png"), rgba: vec![128; 4 * 4 * 4], width: 4, height: 4, source: None, grade: Some(klipp_nodes::NodeGraph::default()) }
}

#[test]
fn the_stills_index_saves_as_revision_2_and_reads_back() {
    let dir = scratch("stills");
    let hero = still("hero");
    save_grade_stills(&dir, std::slice::from_ref(&hero)).unwrap();
    let index: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("media/stills.json")).unwrap()).unwrap();
    assert_eq!(index["format_revision"], 2);
    let back = load_grade_stills(&dir);
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].grade, hero.grade, "the grade the still was saved with");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An index from before revision 2 isn't read, and a save refuses to write
/// over it, naming the converter.
#[test]
fn an_unconverted_stills_index_is_left_alone() {
    let dir = scratch("old_stills");
    std::fs::create_dir_all(dir.join("media")).unwrap();
    let text = serde_json::to_string_pretty(&serde_json::json!([{"name": "Old", "filename": "old.png"}])).unwrap();
    std::fs::write(dir.join("media/stills.json"), &text).unwrap();
    assert!(load_grade_stills(&dir).is_empty(), "not read");
    let err = save_grade_stills(&dir, &[still("new")]).unwrap_err();
    assert!(err.contains("klipp-convert run"), "{err}");
    assert_eq!(std::fs::read_to_string(dir.join("media/stills.json")).unwrap(), text, "and not written over");
    let _ = std::fs::remove_dir_all(&dir);
}
