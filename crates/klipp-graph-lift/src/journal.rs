//! Where the one-time conversion keeps its journal and staging folders,
//! shared by the run that writes them (`klipp-convert run`, #1087) and the
//! app that must refuse a project the run hasn't finished (#1089).
//!
//! Per project, the run copies the folder to `<dir>.klipp-convert`,
//! converts the copy, sets the original aside as `<dir>.klipp-v1`, swaps
//! the copy in, and moves the original into the backup root. A journal
//! entry in the backup root records each step, so a crash anywhere ends
//! either fully rolled back or fully converted. Until the entry says
//! [`Step::Done`], the project is half-converted and the app must not open
//! it: it names the run, which finishes or rolls back from the journal.
//!
//! These definitions are the run's own, moved here unchanged so the two
//! sides can't drift; the tests pin the exact paths.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The suffix of a project's converted copy while the run works on it.
pub const STAGING: &str = ".klipp-convert";
/// The suffix of the original while it is set aside for the swap.
pub const ASIDE: &str = ".klipp-v1";

/// A project's step in the journal, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Step {
    Started,
    Copied,
    Written,
    Verified,
    /// The original renamed aside: from here the run only goes forward.
    OriginalAside,
    Swapped,
    BackedUp,
    AutosaveMoved,
    Done,
}

impl Step {
    pub const ALL: [Step; 9] = [
        Step::Started,
        Step::Copied,
        Step::Written,
        Step::Verified,
        Step::OriginalAside,
        Step::Swapped,
        Step::BackedUp,
        Step::AutosaveMoved,
        Step::Done,
    ];
}

/// Where backups and the journal go: `<home>/.local/share/klipp/pre-v2`.
pub fn backup_root(home: &Path) -> PathBuf {
    home.join(".local/share/klipp/pre-v2")
}

/// The journal's folder in a backup root.
pub fn journal_dir(backup_root: &Path) -> PathBuf {
    backup_root.join("journal")
}

/// The journal entry of the project at `dir`: named by a UUID v5 of the
/// folder's path bytes, so any path, UTF-8 or not, has one fixed name.
pub fn journal_path(backup_root: &Path, dir: &Path) -> PathBuf {
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, dir.as_os_str().as_encoded_bytes());
    journal_dir(backup_root).join(format!("{id}.json"))
}

/// `path` with `suffix` added to its last component (`a/b` → `a/b.x`).
pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Why the project at `dir` is half-converted, or `None` when it isn't: its
/// staging copy or set-aside original is there, or its journal entry exists
/// and hasn't reached [`Step::Done`]. An entry that can't be read counts as
/// unfinished: the app can't tell it's safe.
pub fn unfinished(backup_root: &Path, dir: &Path) -> Option<String> {
    for suffix in [STAGING, ASIDE] {
        let sibling = with_suffix(dir, suffix);
        if sibling.exists() {
            return Some(format!("the conversion left {} beside it", sibling.display()));
        }
    }
    let entry = journal_path(backup_root, dir);
    let text = match std::fs::read_to_string(&entry) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => return Some(format!("its conversion journal {} can't be read: {e}", entry.display())),
    };
    let step = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("step").cloned())
        .and_then(|s| serde_json::from_value::<Step>(s).ok());
    match step {
        Some(Step::Done) => None,
        Some(step) => Some(format!("its conversion stopped at {step:?} ({})", entry.display())),
        None => Some(format!("its conversion journal {} can't be read", entry.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("klipp-journal-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The exact paths, as `klipp-convert run` (#1087) writes them.
    #[test]
    fn the_paths_are_pinned() {
        assert_eq!(backup_root(Path::new("/home/u")), Path::new("/home/u/.local/share/klipp/pre-v2"));
        assert_eq!(journal_dir(Path::new("/b")), Path::new("/b/journal"));
        assert_eq!(with_suffix(Path::new("/p/Film.klipp"), STAGING), Path::new("/p/Film.klipp.klipp-convert"));
        assert_eq!(with_suffix(Path::new("/p/Film.klipp"), ASIDE), Path::new("/p/Film.klipp.klipp-v1"));
        let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"/p/Film.klipp");
        assert_eq!(journal_path(Path::new("/b"), Path::new("/p/Film.klipp")), Path::new(&format!("/b/journal/{id}.json")));
        // The literal, so a change of scheme (namespace, version, bytes)
        // fails here rather than leaving old journals unfound.
        assert_eq!(id.to_string(), "592c5b54-3cdb-5b01-9a5a-89b9fb4bc7b4");
        assert_eq!(serde_json::to_string(&Step::OriginalAside).unwrap(), "\"OriginalAside\"");
        assert_eq!(Step::ALL.last(), Some(&Step::Done));
    }

    /// A folder name that isn't UTF-8 still has one fixed journal entry.
    #[test]
    fn a_non_utf8_folder_has_its_own_entry() {
        use std::os::unix::ffi::OsStrExt;
        let dir = Path::new(std::ffi::OsStr::from_bytes(b"/p/Fil\xffm.klipp"));
        let a = journal_path(Path::new("/b"), dir);
        assert_eq!(a, journal_path(Path::new("/b"), dir), "the same name every time");
        let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"/p/Fil\xffm.klipp");
        assert_eq!(a, Path::new(&format!("/b/journal/{id}.json")));
        assert_ne!(a, journal_path(Path::new("/b"), Path::new("/p/Film.klipp")));
    }

    #[test]
    fn a_project_is_unfinished_until_its_entry_says_done() {
        let root = temp("root");
        let parent = temp("projects");
        let dir = parent.join("Film.klipp");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unfinished(&root, &dir), None, "no journal, no staging: a normal project");

        std::fs::create_dir_all(journal_dir(&root)).unwrap();
        let entry = journal_path(&root, &dir);
        for step in Step::ALL {
            std::fs::write(&entry, serde_json::json!({ "dir": dir, "step": step }).to_string()).unwrap();
            assert_eq!(unfinished(&root, &dir).is_none(), step == Step::Done, "{step:?}");
        }
        std::fs::write(&entry, "not json").unwrap();
        assert!(unfinished(&root, &dir).is_some(), "an unreadable entry isn't trusted");
        std::fs::remove_file(&entry).unwrap();

        for suffix in [STAGING, ASIDE] {
            let sibling = with_suffix(&dir, suffix);
            std::fs::create_dir_all(&sibling).unwrap();
            assert!(unfinished(&root, &dir).unwrap().contains(suffix), "{suffix}");
            std::fs::remove_dir_all(&sibling).unwrap();
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&parent);
    }
}
