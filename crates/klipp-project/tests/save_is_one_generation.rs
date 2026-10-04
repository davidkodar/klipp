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

// ── #840: what the generation pairing alone did not cover ───────────

/// A save with the user's backups turned off.
fn save_without_backups(dir: &Path, media: &[&str]) {
    klipp_project::save_project_with_color_and_backups(
        dir, &[Sequence::new_default()], &pool_of(dir, media), &klipp_project::ColorSettings::default(), None, None, "", 0, None, None, None,
    )
    .unwrap();
}

fn generation(path: &Path) -> String {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    v["generation"].as_str().unwrap().to_string()
}

/// The state a save leaves when it stops after its manifest, before
/// project.json: the new manifest, the previous project.json, and its
/// pending mark. `previous_project` is the project.json from before it.
fn stopped_between(dir: &Path, previous_project: &[u8]) {
    let manifest = dir.join("media").join("manifest.json");
    std::fs::write(dir.join(".recovery").join("pending"), generation(&manifest)).unwrap();
    std::fs::write(dir.join("project.json"), previous_project).unwrap();
}

/// With backups off there is no .bak to mend from: the recovery copy the
/// save kept is opened instead.
#[test]
fn with_backups_off_a_stopped_save_opens_as_the_save_before() {
    let dir = fresh("nobackups");
    save_without_backups(&dir, &["a.mov"]);
    let before = std::fs::read(dir.join("project.json")).unwrap();
    save_without_backups(&dir, &["b.mov"]);
    assert!(!dir.join("project.json.bak.1").exists(), "no backups were made");
    stopped_between(&dir, &before);
    assert_eq!(names(&dir), vec!["a.mov"], "the pair from before the stopped save");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A project saved before generations: its first save since upgrading
/// stops between the files, and it still opens as it was.
#[test]
fn a_project_from_before_generations_survives_its_first_stopped_save() {
    let dir = fresh("legacy");
    save_project(&dir, &[Sequence::new_default()], &pool_of(&dir, &["a.mov"])).unwrap();
    for f in [dir.join("project.json"), dir.join("media").join("manifest.json")] {
        let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        v.as_object_mut().unwrap().remove("generation");
        std::fs::write(&f, serde_json::to_vec(&v).unwrap()).unwrap();
    }
    let legacy = std::fs::read(dir.join("project.json")).unwrap();
    save_project(&dir, &[Sequence::new_default()], &pool_of(&dir, &["b.mov"])).unwrap();
    stopped_between(&dir, &legacy);
    assert_eq!(names(&dir), vec!["a.mov"], "the old timeline with its own media list, not the new one");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A save that finished all but removing its mark opens as saved; one
/// that stopped before its manifest opens as the pair from before.
#[test]
fn a_pending_mark_reads_how_far_the_save_got() {
    let dir = fresh("mark");
    save_without_backups(&dir, &["a.mov"]);
    save_without_backups(&dir, &["b.mov"]);
    let finished = generation(&dir.join("project.json"));
    std::fs::write(dir.join(".recovery").join("pending"), &finished).unwrap();
    assert_eq!(names(&dir), vec!["b.mov"], "finished but for the mark");
    std::fs::write(dir.join(".recovery").join("pending"), "a-save-that-wrote-nothing").unwrap();
    assert_eq!(names(&dir), vec!["b.mov"], "stopped before its manifest: the files are whole");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every save leaves no mark behind, and keeps the pair it replaced.
#[test]
fn a_finished_save_keeps_its_recovery_copy_and_no_mark() {
    let dir = fresh("finished");
    save_without_backups(&dir, &["a.mov"]);
    let first = generation(&dir.join("project.json"));
    save_without_backups(&dir, &["b.mov"]);
    assert!(!dir.join(".recovery").join("pending").exists());
    assert_eq!(generation(&dir.join(".recovery").join("project.json")), first);
    assert_eq!(generation(&dir.join(".recovery").join("manifest.json")), first);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A timeline and media list from different saves with nothing to mend
/// them is refused, said, rather than opened as if whole.
#[test]
fn an_unmendable_pair_is_refused() {
    let dir = fresh("refused");
    save_without_backups(&dir, &["a.mov"]);
    let before = std::fs::read(dir.join("project.json")).unwrap();
    save_without_backups(&dir, &["b.mov"]);
    // Different saves, no pending mark, no recovery copy, no backups.
    std::fs::write(dir.join("project.json"), before).unwrap();
    std::fs::remove_dir_all(dir.join(".recovery")).unwrap();
    let err = load_project(&dir).unwrap_err();
    assert!(err.contains("from different saves") && err.contains(".bak"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A save that stops, an open, then a second save that stops the same
/// way (a full disk does it twice): the project still opens as the save
/// before the first, because the second does not copy the broken pair
/// over the recovery copy (#858 review).
#[test]
fn two_stopped_saves_in_a_row_still_open_as_the_save_before() {
    let dir = fresh("twice");
    save_without_backups(&dir, &["a.mov"]);
    let a = std::fs::read(dir.join("project.json")).unwrap();
    save_without_backups(&dir, &["b.mov"]);
    stopped_between(&dir, &a);
    assert_eq!(names(&dir), vec!["a.mov"], "after the first stop");
    // The second save sees the broken pair and keeps the recovery copy.
    save_without_backups(&dir, &["c.mov"]);
    stopped_between(&dir, &a);
    assert_eq!(names(&dir), vec!["a.mov"], "after the second stop, still the save before both");
    let _ = std::fs::remove_dir_all(&dir);
}
