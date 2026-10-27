//! Crash recovery (#2789): whether a project's auto-save holds work
//! newer than its last save, judged by the saves themselves.
//!
//! The project folder's own time moves whenever anything in it is
//! written, the lock file included, so after any open a crash's
//! auto-save looked older than the project and was never offered again.
//! The saved times written into the two `project.json` files are what is
//! compared; a project whose saved version was chosen over its auto-save
//! keeps a note of which auto-save that was, inside it.

use std::path::{Path, PathBuf};

/// The note, inside an auto-save, naming the auto-save the saved version
/// was kept over: an auto-save written later is offered again.
const KEPT: &str = "kept-saved-version";

/// The auto-save folder of a project: `<stem>_autosave.klipp` beside it.
pub fn autosave_path_for(project: &Path) -> PathBuf {
    let stem = project.file_stem().unwrap_or_default().to_string_lossy();
    project.with_file_name(format!("{stem}_autosave.klipp"))
}

/// An auto-save holding work newer than the project's last save.
#[derive(Debug, Clone, PartialEq)]
pub struct NewerAutosave {
    pub autosave: PathBuf,
    /// When the auto-save was written, and the project last saved, as the
    /// files say (ISO 8601, UTC); `None` where a file doesn't.
    pub autosaved_at: Option<String>,
    pub saved_at: Option<String>,
}

/// What one save says about itself: when, and which save it was.
struct Stamp {
    saved_at: Option<String>,
    generation: Option<String>,
    modified: Option<std::time::SystemTime>,
}

fn stamp(folder: &Path) -> Option<Stamp> {
    let file = folder.join("project.json");
    let modified = std::fs::metadata(&file).and_then(|m| m.modified()).ok()?;
    #[derive(serde::Deserialize)]
    struct Head {
        #[serde(default)]
        saved_at: Option<String>,
        #[serde(default)]
        generation: Option<String>,
    }
    // Read within the cap a project file is loaded with.
    let head: Option<Head> = crate::path_util::read_capped(&file, 64 * 1024 * 1024).ok().and_then(|b| serde_json::from_slice(&b).ok());
    Some(Stamp { saved_at: head.as_ref().and_then(|h| h.saved_at.clone()), generation: head.and_then(|h| h.generation), modified: Some(modified) })
}

/// Which save the auto-save is, for the note: its generation, else its time.
fn identity(s: &Stamp) -> Option<String> {
    s.generation.clone().or_else(|| s.saved_at.clone())
}

/// The project's auto-save when it is newer than the project's last save
/// and the saved version wasn't kept over it; `None` when there is none.
pub fn newer_autosave(project: &Path) -> Option<NewerAutosave> {
    let autosave = autosave_path_for(project);
    let auto = stamp(&autosave)?;
    if let (Some(kept), Some(this)) = (std::fs::read_to_string(autosave.join(KEPT)).ok(), identity(&auto)) {
        if kept.trim() == this {
            return None;
        }
    }
    let found = |saved_at: Option<String>| Some(NewerAutosave { autosave: autosave.clone(), autosaved_at: auto.saved_at.clone(), saved_at });
    let Some(saved) = stamp(project) else { return found(None) };
    let newer = match (&auto.saved_at, &saved.saved_at) {
        // The same fixed form, UTC to the second: in order as text.
        (Some(a), Some(s)) if a != s => a > s,
        _ => auto.modified > saved.modified,
    };
    if newer { found(saved.saved_at) } else { None }
}

/// The folder an open of `project` loads (#2789): its auto-save when the
/// work in it is to be restored and there is a newer one, else the
/// project itself.
pub fn source_for(project: &Path, restore: bool) -> PathBuf {
    match newer_autosave(project) {
        Some(found) if restore => found.autosave,
        _ => project.to_path_buf(),
    }
}

/// The saved version chosen over the auto-save there is now: it isn't
/// offered again, and one written later is. The auto-save stays on disk,
/// going as it always has on the next clean save or close.
pub fn keep_saved(project: &Path) -> std::io::Result<()> {
    let autosave = autosave_path_for(project);
    let Some(this) = stamp(&autosave).as_ref().and_then(identity) else { return Ok(()) };
    std::fs::write(autosave.join(KEPT), this)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn save(folder: &Path, saved_at: &str, generation: &str) {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::write(folder.join("project.json"), format!(r#"{{"saved_at":"{saved_at}","generation":"{generation}","sequences":[]}}"#)).unwrap();
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("klipp-recovery-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// An auto-save written after the last save is offered, and a lock
    /// written into the project folder afterwards doesn't hide it, as the
    /// folder's own time did; one older than the save isn't.
    #[test]
    fn a_newer_autosave_is_found_by_the_saves_not_the_folders() {
        let d = dir("newer");
        let project = d.join("Film.klipp");
        save(&project, "2026-10-26T22:30:00Z", "a");
        save(&autosave_path_for(&project), "2026-10-26T22:35:59Z", "b");
        // Later: the lock written, the project folder's time moving past.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(project.join("project.lock"), "pid 1").unwrap();
        let found = newer_autosave(&project).expect("the crash's auto-save");
        assert_eq!((found.autosaved_at.as_deref(), found.saved_at.as_deref()), (Some("2026-10-26T22:35:59Z"), Some("2026-10-26T22:30:00Z")));
        // Saved after the auto-save: nothing to recover.
        save(&project, "2026-10-26T22:40:00Z", "c");
        assert_eq!(newer_autosave(&project), None);
        // No auto-save at all.
        assert_eq!(newer_autosave(&d.join("Other.klipp")), None);
    }

    /// Keeping the saved version sets that auto-save aside; one written
    /// after it is offered again.
    #[test]
    fn keeping_the_saved_version_sets_only_that_autosave_aside() {
        let d = dir("kept");
        let project = d.join("Film.klipp");
        save(&project, "2026-10-26T22:30:00Z", "a");
        save(&autosave_path_for(&project), "2026-10-26T22:35:59Z", "b");
        keep_saved(&project).unwrap();
        assert_eq!(newer_autosave(&project), None, "set aside");
        save(&autosave_path_for(&project), "2026-10-26T22:50:00Z", "d");
        assert!(newer_autosave(&project).is_some(), "a later auto-save is offered");
    }
}
