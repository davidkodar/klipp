//! The stores beside a project (its Looks and grade stills) never delete
//! or overwrite what this Klipp cannot read in full (#993). They used to
//! read an unreadable entry as nothing and then save that nothing back:
//! a Look a newer Klipp saved was pruned on the next save, and an
//! unreadable stills index was rewritten from an empty list.

use klipp_project::{load_grade_stills, load_looks, save_grade_stills, save_looks, GradeStillData};
use klipp_timeline::Look;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every "Corrector" kind in `path`'s JSON renamed to one from a newer Klipp.
fn from_a_newer_klipp(path: &std::path::Path) -> Vec<u8> {
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("\"Corrector\""), "{path:?} has a grade to change");
    std::fs::write(path, text.replace("\"Corrector\"", "\"HolographicKey\"")).unwrap();
    std::fs::read(path).unwrap()
}

#[test]
fn a_look_from_a_newer_klipp_is_not_loaded_and_survives_a_save() {
    let dir = scratch("look_kept");
    let look = Look { id: uuid::Uuid::new_v4(), name: "Newer".into(), node_graph: klipp_nodes::NodeGraph::default() };
    save_looks(&dir, std::slice::from_ref(&look)).unwrap();
    let path = std::fs::read_dir(dir.join("looks")).unwrap().flatten().map(|e| e.path()).next().unwrap();
    let before = from_a_newer_klipp(&path);

    assert!(load_looks(&dir).is_empty(), "a Look holding an unknown node is not loaded");
    // The project's Looks as this Klipp holds them: none. The save must
    // not take that as "delete the rest".
    save_looks(&dir, &[]).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before, "the newer Look is untouched");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_readable_look_that_was_removed_is_still_pruned() {
    let dir = scratch("look_pruned");
    let look = Look { id: uuid::Uuid::new_v4(), name: "Old".into(), node_graph: klipp_nodes::NodeGraph::default() };
    save_looks(&dir, std::slice::from_ref(&look)).unwrap();
    save_looks(&dir, &[]).unwrap();
    assert_eq!(std::fs::read_dir(dir.join("looks")).map(|d| d.count()).unwrap_or(0), 0, "a Look this Klipp reads is still removed when deleted");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stills_from_a_newer_klipp_are_not_loaded_and_not_overwritten() {
    let dir = scratch("stills_kept");
    let still = GradeStillData {
        name: "A".into(),
        filename: "a.png".into(),
        rgba: vec![128; 4 * 4 * 4],
        width: 4,
        height: 4,
        source: None,
        grade: Some(klipp_nodes::NodeGraph::default()),
    };
    save_grade_stills(&dir, &[still]).unwrap();
    let index = dir.join("media").join("stills.json");
    let before = from_a_newer_klipp(&index);

    assert!(load_grade_stills(&dir).is_empty(), "stills holding an unknown node are not loaded");
    let err = save_grade_stills(&dir, &[]).expect_err("an unreadable index is not overwritten");
    assert!(err.contains("newer Klipp"), "{err}");
    assert_eq!(std::fs::read(&index).unwrap(), before, "the index is untouched");
    let _ = std::fs::remove_dir_all(&dir);
}
