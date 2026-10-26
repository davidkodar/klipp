//! A project saved by a newer Klipp, one whose format this build cannot
//! read correctly, is refused when it is opened, with a sentence that
//! says so, rather than read with something missing (#994).

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project, FORMAT_REVISION};
use klipp_timeline::Sequence;

fn saved(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[Sequence::new_default()], &MediaPool::new()).unwrap();
    dir
}

fn with_revision(dir: &std::path::Path, revision: u32) {
    let path = dir.join("project.json");
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    v["format_revision"] = serde_json::Value::from(revision);
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

#[test]
fn a_newer_format_is_refused_with_a_sentence() {
    let dir = saved("newer_format");
    with_revision(&dir, FORMAT_REVISION + 1);
    let before = std::fs::read(dir.join("project.json")).unwrap();

    let err = load_project(&dir).expect_err("a newer format is refused");
    assert!(err.contains("newer Klipp"), "says why: {err}");
    assert!(err.contains(&(FORMAT_REVISION + 1).to_string()), "names the revision: {err}");
    // Refusing is not touching: the file is exactly as it was.
    assert_eq!(std::fs::read(dir.join("project.json")).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_current_format_opens() {
    let dir = saved("current_format");
    with_revision(&dir, FORMAT_REVISION);
    load_project(&dir).expect("this build's own revision opens");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Revision 5 (#2754): a save writes it, and it opens again.
#[test]
fn a_save_writes_revision_5() {
    // 5: a tracked surface's keys are maps from its reference corners
    // with the tracker's confidence; a revision-4 reader would find no
    // track (4 was picture revision 4, M4 step F).
    assert_eq!(FORMAT_REVISION, 5, "raising the revision is a deliberate change: update this test with the reason");
    let dir = saved("revision_five");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    assert_eq!(v["format_revision"], 5);
    load_project(&dir).expect("and it opens");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file from before revision 2 is refused with a sentence naming the
/// tool that converts it, and left exactly as it was (#1089).
#[test]
fn a_file_from_before_revision_2_is_refused_naming_the_converter() {
    let dir = saved("unconverted");
    with_revision(&dir, 0);
    let before = std::fs::read(dir.join("project.json")).unwrap();
    let err = load_project(&dir).expect_err("an unconverted file is refused");
    assert!(err.contains("klipp-convert run"), "names the tool: {err}");
    assert_eq!(std::fs::read(dir.join("project.json")).unwrap(), before, "and the file is untouched");
    let _ = std::fs::remove_dir_all(&dir);
}
