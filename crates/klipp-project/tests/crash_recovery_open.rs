//! Crash recovery (#2789), with real saves: a project saved, then its
//! auto-save written with more work, then a lock written into the
//! project (as any open does). The auto-save is still found; restoring
//! loads its work, keeping loads the saved version and sets the
//! auto-save aside.

use klipp_media::MediaPool;
use klipp_project::recovery::{autosave_path_for, keep_saved, newer_autosave, source_for};
use klipp_timeline::Sequence;

fn named(name: &str) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.name = name.into();
    seq
}

fn first_name(dir: &std::path::Path) -> String {
    klipp_project::load_project(dir).expect("loads").0[0].name.clone()
}

#[test]
fn a_crashs_autosave_is_offered_restored_or_kept() {
    let dir = std::env::temp_dir().join(format!("klipp-crash-recovery-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let project = dir.join("Film.klipp");
    klipp_project::save_project(&project, &[named("Saved")], &MediaPool::new()).unwrap();
    // The saved time is to the second: the auto-save a second later.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    klipp_project::save_project(&autosave_path_for(&project), &[named("Twenty-six edits")], &MediaPool::new()).unwrap();
    // An open writes the lock: the project folder's time moves past.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(project.join("project.lock"), "pid 1").unwrap();

    assert!(newer_autosave(&project).is_some(), "offered after the lock was written");
    assert_eq!(first_name(&source_for(&project, true)), "Twenty-six edits", "restoring gives the auto-save's work");
    assert_eq!(first_name(&source_for(&project, false)), "Saved", "keeping gives the saved version");
    keep_saved(&project).unwrap();
    assert!(newer_autosave(&project).is_none(), "kept: not offered again");
    assert_eq!(first_name(&source_for(&project, true)), "Saved", "nothing left to restore");
    let _ = std::fs::remove_dir_all(&dir);
}
