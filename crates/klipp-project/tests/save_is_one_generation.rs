//! A save writes project.json and the media manifest as one step that can
//! be recovered (#821): a project never opens with its timeline from one
//! save and its media list from another.

use klipp_media::{MediaItem, MediaPool};
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;
use std::path::{Path, PathBuf};

fn fresh(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_generation_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn pool_of(dir: &Path, names: &[&str]) -> MediaPool {
    let mut pool = MediaPool::new();
    for n in names {
        pool.add(MediaItem::synthetic_for_test(dir.join(n), None));
    }
    pool
}

fn names(dir: &Path) -> Vec<String> {
    let (_, pool, ..) = load_project(dir).unwrap();
    let mut n: Vec<String> = pool.all().map(|m| m.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
    n.sort();
    n
}

/// A save that stopped after its manifest, before project.json: the
/// project opens with the media list its timeline was saved with.
#[test]
fn a_save_cut_between_its_files_opens_as_the_save_before() {
    let dir = fresh("cut");
    save_project(&dir, &[Sequence::new_default()], &pool_of(&dir, &["a.mov"])).unwrap();
    let first_project = std::fs::read(dir.join("project.json")).unwrap();
    save_project(&dir, &[Sequence::new_default()], &pool_of(&dir, &["b.mov"])).unwrap();
    // As if the second save had written its manifest and then stopped.
    std::fs::write(dir.join("project.json"), first_project).unwrap();
    assert_eq!(names(&dir), vec!["a.mov"], "the timeline's own media list, from the backup");
    let _ = std::fs::remove_dir_all(&dir);
}

/// project.json failing to write puts the previous manifest back.
#[test]
fn a_failed_project_write_leaves_the_previous_pair() {
    let dir = fresh("fail");
    save_project(&dir, &[Sequence::new_default()], &pool_of(&dir, &["a.mov"])).unwrap();
    let manifest_before = std::fs::read(dir.join("media").join("manifest.json")).unwrap();
    // A directory where project.json would be renamed into place: the write fails.
    let project = dir.join("project.json");
    std::fs::rename(&project, dir.join("project.keep")).unwrap();
    std::fs::create_dir_all(project.join("blocker")).unwrap();
    assert!(save_project(&dir, &[Sequence::new_default()], &pool_of(&dir, &["b.mov"])).is_err());
    assert_eq!(std::fs::read(dir.join("media").join("manifest.json")).unwrap(), manifest_before, "the media list it went with");
    std::fs::remove_dir_all(&project).unwrap();
    std::fs::rename(dir.join("project.keep"), &project).unwrap();
    assert_eq!(names(&dir), vec!["a.mov"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A project from before generations opens as it always did.
#[test]
fn a_project_without_generations_opens_as_before() {
    let dir = fresh("old");
    save_project(&dir, &[Sequence::new_default()], &pool_of(&dir, &["a.mov"])).unwrap();
    for f in [dir.join("project.json"), dir.join("media").join("manifest.json")] {
        let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        v.as_object_mut().unwrap().remove("generation");
        std::fs::write(&f, serde_json::to_vec(&v).unwrap()).unwrap();
    }
    assert_eq!(names(&dir), vec!["a.mov"]);
    let _ = std::fs::remove_dir_all(&dir);
}
