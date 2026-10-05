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

/// Every file so far is revision 0, and a save at revision 0 leaves the
/// field out, so this change writes files exactly as before and older
/// builds read them as before.
#[test]
fn revision_zero_is_not_written() {
    assert_eq!(FORMAT_REVISION, 0, "raising the revision is a deliberate change: update this test with the reason");
    let dir = saved("revision_zero");
    let json = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert!(!json.contains("format_revision"), "revision 0 is omitted: {json}");
    load_project(&dir).expect("and it opens");
    let _ = std::fs::remove_dir_all(&dir);
}
