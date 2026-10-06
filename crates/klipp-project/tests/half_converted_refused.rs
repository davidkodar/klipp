//! A project the one-time conversion hasn't finished isn't opened (#1089):
//! its staging copy or set-aside original beside it, or a journal entry
//! short of the last step, refuses the load and names `klipp-convert run`.
//! Its own test binary, since it points HOME at a temp folder.

use std::path::{Path, PathBuf};

use klipp_graph_lift::journal::{self, Step};

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("klipp-half-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn refused(dir: &Path) -> Option<String> {
    klipp_project::load_project(dir).err().filter(|e| e.contains("klipp-convert run") && e.contains("isn't finished"))
}

#[test]
fn a_half_converted_project_is_refused() {
    let home = temp("home");
    std::env::set_var("HOME", &home);
    let dir = temp("projects").join("Film.klipp");
    std::fs::create_dir_all(&dir).unwrap();

    for suffix in [journal::STAGING, journal::ASIDE] {
        let sibling = journal::with_suffix(&dir, suffix);
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(refused(&dir).is_some(), "{suffix} beside it");
        std::fs::remove_dir_all(&sibling).unwrap();
    }

    let root = journal::backup_root(&home);
    std::fs::create_dir_all(journal::journal_dir(&root)).unwrap();
    let entry = journal::journal_path(&root, &dir);
    std::fs::write(&entry, serde_json::json!({ "step": Step::Swapped }).to_string()).unwrap();
    assert!(refused(&dir).is_some(), "a journal stopped at Swapped");
    std::fs::write(&entry, serde_json::json!({ "step": Step::Done }).to_string()).unwrap();
    // Done: no longer refused for being half-converted (the empty folder
    // then fails to load for its own reason).
    assert!(refused(&dir).is_none(), "a finished conversion opens");
}
