//! Opening a project from whatever was picked (#1143), and the projects
//! in a projects folder.
//!
//! A project is a folder, so a file dialog shows its insides: picking its
//! `project.json`, or anything else inside it, opens the project it is in,
//! the same project as picking the folder itself or clicking it on the
//! welcome screen. The path is only resolved here; reading the project is
//! still `klipp_project::load_project`'s.

use std::path::{Path, PathBuf};

use klipp_core::AppCommand;

use crate::prefs::RecentProject;

/// Whether `dir` holds a project: its `project.json` is there.
fn holds_project(dir: &Path) -> bool {
    dir.join("project.json").is_file()
}

fn is_project_folder_name(dir: &Path) -> bool {
    dir.extension().is_some_and(|e| e == "klipp")
}

/// The project folder a picked path means: the folder itself when it holds
/// a project; the folder a `project.json` is in; otherwise the nearest
/// `.klipp` folder holding a project that the path is inside. Anything
/// else comes back as it was, for the loader to refuse with its reason.
pub fn project_dir_of(picked: &Path) -> PathBuf {
    if picked.is_dir() && holds_project(picked) {
        return picked.to_path_buf();
    }
    if picked.file_name().is_some_and(|n| n == "project.json") {
        if let Some(dir) = picked.parent().filter(|d| holds_project(d)) {
            return dir.to_path_buf();
        }
    }
    picked
        .ancestors()
        .skip(1)
        .find(|d| is_project_folder_name(d) && holds_project(d))
        .map(Path::to_path_buf)
        .unwrap_or_else(|| picked.to_path_buf())
}

/// The Open Project dialog: a file dialog, so double-clicking into a
/// project and picking its `project.json` opens it.
pub fn pick_project_commands() -> Vec<AppCommand> {
    rfd::FileDialog::new()
        .set_title("Open Project")
        .add_filter("Klipp project (project.json)", &["json"])
        .pick_file()
        .map(|path| vec![AppCommand::OpenProject { path: project_dir_of(&path) }])
        .unwrap_or_default()
}

/// The folder the welcome screen lists projects from: the one new
/// projects go in (Preferences), else `~/Projects` as the new-project
/// dialog has it, when it exists. Called every frame, so a chosen folder
/// isn't looked at here (it may be on a mount that blocks; the listing
/// reads it on its own thread); only the fallback under home is.
pub fn projects_folder(pref: Option<&PathBuf>) -> Option<PathBuf> {
    match pref {
        Some(dir) => Some(dir.clone()),
        None => dirs::home_dir().map(|h| h.join("Projects")).filter(|d| d.is_dir()),
    }
}

/// The projects directly in `dir`, newest first: each `.klipp` folder
/// holding a project, auto-saves left out (they open from their project).
pub fn projects_in(dir: &Path) -> Vec<RecentProject> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<(std::time::SystemTime, RecentProject)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_project_folder_name(p) && p.is_dir() && holds_project(p))
        .filter(|p| !p.file_stem().is_some_and(|s| s.to_string_lossy().ends_with("_autosave")))
        .map(|path| {
            let modified = std::fs::metadata(path.join("project.json")).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            let local: chrono::DateTime<chrono::Local> = modified.into();
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let entry = RecentProject {
                path,
                name,
                last_saved: Some(local.format("%Y-%m-%d %H:%M:%S").to_string()),
                duration_secs: None,
                fps: None,
                resolution: None,
                thumbnail_path: None,
                total_size_bytes: None,
                clip_count: None,
                track_count: None,
                missing_media: None,
                sequence_count: None,
                pinned: false,
            };
            (modified, entry)
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    found.into_iter().map(|(_, rp)| rp).collect()
}

/// Copy the project folder `path` beside it as "<name> copy" (or
/// "<name> copy 2", and on), and return the copy's path. The lock is not
/// copied: the copy is nobody's open project.
pub fn duplicate_project(path: &Path) -> std::io::Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| std::io::Error::other("the project has no folder"))?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Project".into());
    let target = (1..1000)
        .map(|n| parent.join(if n == 1 { format!("{stem} copy.klipp") } else { format!("{stem} copy {n}.klipp") }))
        .find(|p| !p.exists())
        .ok_or_else(|| std::io::Error::other("no free name for the copy"))?;
    fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == klipp_project::lock::LOCK_FILE {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                copy(&entry.path(), &to.join(&name))?;
            } else if kind.is_symlink() {
                std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, to.join(&name))?;
            } else {
                std::fs::copy(entry.path(), to.join(&name))?;
            }
        }
        Ok(())
    }
    if let Err(e) = copy(path, &target) {
        // A half-made copy is ours to clear: nothing else knows of it.
        let _ = std::fs::remove_dir_all(&target);
        return Err(e);
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(parent: &Path, name: &str) -> PathBuf {
        let dir = parent.join(format!("{name}.klipp"));
        std::fs::create_dir_all(dir.join("media")).unwrap();
        std::fs::write(dir.join("project.json"), b"{}").unwrap();
        std::fs::write(dir.join("media/manifest.json"), b"{}").unwrap();
        dir
    }

    /// Picking the folder, its project.json, or anything inside it, and
    /// the welcome screen's list, all name the same project (#1143).
    #[test]
    fn every_way_in_lands_in_the_same_project() {
        let root = std::env::temp_dir().join(format!("klipp-open-1143-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = project(&root, "Trip");
        project(&root, "Trip_autosave");
        assert_eq!(project_dir_of(&dir), dir, "the folder");
        assert_eq!(project_dir_of(&dir.join("project.json")), dir, "its project.json");
        assert_eq!(project_dir_of(&dir.join("media/manifest.json")), dir, "a file deeper inside");
        assert_eq!(project_dir_of(&dir.join("media")), dir, "a folder inside");
        let listed = projects_in(&root);
        assert_eq!(listed.iter().map(|p| p.path.clone()).collect::<Vec<_>>(), vec![dir.clone()], "listed once, the auto-save left out");
        assert_eq!(listed[0].name, "Trip");
        // Something that is no project comes back as it was.
        let stray = root.join("notes.txt");
        std::fs::write(&stray, b"x").unwrap();
        assert_eq!(project_dir_of(&stray), stray);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #1847: Duplicate copies the whole folder beside it under a free
    /// "copy" name, leaves the lock behind, and touches the original not.
    #[test]
    fn duplicate_copies_the_project_beside_it() {
        let root = std::env::temp_dir().join(format!("klipp-dup-1847-{}", uuid::Uuid::new_v4()));
        let dir = project(&root, "Trip");
        std::fs::write(dir.join(klipp_project::lock::LOCK_FILE), b"held").unwrap();
        let copy = duplicate_project(&dir).unwrap();
        assert_eq!(copy, root.join("Trip copy.klipp"));
        assert!(copy.join("media/manifest.json").exists() && copy.join("project.json").exists());
        assert!(!copy.join(klipp_project::lock::LOCK_FILE).exists(), "the copy is nobody's open project");
        assert!(dir.join(klipp_project::lock::LOCK_FILE).exists(), "the original is untouched");
        assert_eq!(duplicate_project(&dir).unwrap(), root.join("Trip copy 2.klipp"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
