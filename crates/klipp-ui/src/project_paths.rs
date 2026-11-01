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

/// Where new projects go: the projects root chosen in Preferences, else
/// `~/Videos`, or home itself where there is no Videos folder — never a
/// project's own folder. A root that is a project
/// (`Trip.klipp`) or the folder a project was made to sit in alone
/// (`Golden Days/` holding `Golden Days.klipp`) is passed over for the
/// default: projects made there would land inside another project.
///
/// It looks at the chosen folder, so it is worked out when the preference
/// changes, not every frame.
pub fn projects_root(pref: Option<&PathBuf>) -> PathBuf {
    projects_root_in(pref, &dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")))
}

/// [`projects_root`] with the home folder given.
pub fn projects_root_in(pref: Option<&PathBuf>, home: &Path) -> PathBuf {
    pref.filter(|dir| !is_a_projects_own_folder(dir))
        .cloned()
        .unwrap_or_else(|| Some(home.join("Videos")).filter(|v| v.is_dir()).unwrap_or_else(|| home.to_path_buf()))
}

/// Whether `dir` is a project, or the folder one project sits in under
/// the folder's own name.
pub fn is_a_projects_own_folder(dir: &Path) -> bool {
    if is_project_folder_name(dir) || holds_project(dir) {
        return true;
    }
    dir.file_name()
        .map(|name| dir.join(format!("{}.klipp", name.to_string_lossy())))
        .is_some_and(|own| holds_project(&own))
}

/// `base`, or `base 2`, `base 3` and on: the first with no project of
/// that name in `dir`.
pub fn free_project_name(dir: &Path, base: &str) -> String {
    (1..1000)
        .map(|n| if n == 1 { base.to_string() } else { format!("{base} {n}") })
        .find(|name| !dir.join(format!("{name}.klipp")).exists())
        .unwrap_or_else(|| base.to_string())
}

/// What an untitled project's path is until it is written: a placeholder,
/// not a folder.
pub const UNTITLED: &str = "Untitled";

/// Whether `path` is the untitled placeholder.
pub fn is_untitled(path: Option<&Path>) -> bool {
    path == Some(Path::new(UNTITLED))
}

/// Where an untitled project is written on its first real edit: a free
/// "Untitled" in the projects `root`. Nothing while it is untouched, so
/// one opened and closed leaves nothing behind; nothing for a project
/// that has a folder already.
pub fn first_write(path: Option<&Path>, dirty: bool, root: &Path) -> Option<PathBuf> {
    (is_untitled(path) && dirty).then(|| root.join(format!("{}.klipp", free_project_name(root, UNTITLED))))
}

/// The name Save offers for a project not named yet: the name it was
/// written under on its first edit (`written`, in `root`), which is the
/// name the status bar shows; else a free "Untitled" in `root`. It offered
/// "Untitled 2" for a project shown as Untitled.klipp, the name taken by
/// the project itself.
pub fn offered_name(root: &Path, written: Option<&Path>) -> String {
    written
        .filter(|w| w.parent() == Some(root))
        .and_then(|w| w.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| free_project_name(root, UNTITLED))
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

/// Copy the project folder `path` beside it as `<name> copy` (or
/// `<name> copy 2`, and on), and return the copy's path. The lock is not
/// copied: the copy is nobody's open project.
pub fn duplicate_project(path: &Path) -> std::io::Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| std::io::Error::other("the project has no folder"))?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Project".into());
    let target = (1..1000)
        .map(|n| parent.join(if n == 1 { format!("{stem} copy.klipp") } else { format!("{stem} copy {n}.klipp") }))
        .find(|p| !p.exists())
        .ok_or_else(|| std::io::Error::other("no free name for the copy"))?;
    copy_project_tree(path, &target)?;
    Ok(target)
}

/// Copy the project folder `from` to `to`, which must not exist yet,
/// leaving the lock behind. A half-made copy is removed: it is ours,
/// nothing else knows of it. `from` is never touched.
pub fn copy_project_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    copy_project_tree_counting(from, to, &std::sync::atomic::AtomicU64::new(0))
}

/// The bytes in the files under `dir`, for a copy's progress.
pub fn tree_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries.flatten().map(|e| match e.file_type() {
        Ok(t) if t.is_dir() => tree_size(&e.path()),
        Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
        _ => 0,
    }).sum()
}

/// [`copy_project_tree`], adding each file's bytes to `copied` as it
/// goes, so a copy run on its own thread can show how far it is.
pub fn copy_project_tree_counting(from: &Path, to: &Path, copied: &std::sync::atomic::AtomicU64) -> std::io::Result<()> {
    fn copy(from: &Path, to: &Path, copied: &std::sync::atomic::AtomicU64) -> std::io::Result<()> {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == klipp_project::lock::LOCK_FILE {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                copy(&entry.path(), &to.join(&name), copied)?;
            } else if kind.is_symlink() {
                std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, to.join(&name))?;
            } else {
                let n = std::fs::copy(entry.path(), to.join(&name))?;
                copied.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
            }
        }
        Ok(())
    }
    if to.exists() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} is there already", to.display())));
    }
    if let Err(e) = copy(from, to, copied) {
        let _ = std::fs::remove_dir_all(to);
        return Err(e);
    }
    Ok(())
}

/// How a folder got to its new place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    /// Renamed: it is only at the new place now.
    Renamed,
    /// Copied to another filesystem: the original is kept as it was.
    Copied,
}

/// Move the project folder `from` to `to` (#1864): a rename, or across
/// filesystems a copy that keeps the original. Nothing is deleted, and
/// nothing is replaced: a `to` that exists is an error.
pub fn move_project_folder(from: &Path, to: &Path) -> std::io::Result<Moved> {
    move_project_folder_with(from, to, |a, b| std::fs::rename(a, b))
}

/// [`move_project_folder`] with the rename given, so a move across
/// filesystems can be tested.
pub fn move_project_folder_with(from: &Path, to: &Path, rename: impl Fn(&Path, &Path) -> std::io::Result<()>) -> std::io::Result<Moved> {
    if to.exists() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} is there already", to.display())));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match rename(from, to) {
        Ok(()) => Ok(Moved::Renamed),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_project_tree(from, to)?;
            Ok(Moved::Copied)
        }
        Err(e) => Err(e),
    }
}

/// What renaming a project folder came to.
#[derive(Debug, PartialEq, Eq)]
pub enum RenameOutcome {
    Renamed,
    /// On another filesystem: it must be copied, which takes time, so
    /// the caller copies it on a thread of its own and keeps the original.
    NeedsCopy,
}

/// Rename the project folder `from` to `to` where that is one rename.
/// Refuses a `to` that exists; nothing is replaced or deleted.
pub fn rename_project_folder(from: &Path, to: &Path) -> std::io::Result<RenameOutcome> {
    rename_project_folder_with(from, to, |a, b| std::fs::rename(a, b))
}

/// [`rename_project_folder`] with the rename given, for tests.
pub fn rename_project_folder_with(from: &Path, to: &Path, rename: impl Fn(&Path, &Path) -> std::io::Result<()>) -> std::io::Result<RenameOutcome> {
    if to.exists() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} is there already", to.display())));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match rename(from, to) {
        Ok(()) => Ok(RenameOutcome::Renamed),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => Ok(RenameOutcome::NeedsCopy),
        Err(e) => Err(e),
    }
}

/// Why Save As must not write to `target`: a project is there already
/// that is not the one open. Saving would write over its project.json.
pub fn save_as_refusal(target: &Path, current: Option<&Path>) -> Option<String> {
    if current == Some(target) {
        return None;
    }
    if target.join("project.json").exists() {
        return Some(format!("{} is another project; choose another name", target.display()));
    }
    if target.exists() && !target.is_dir() {
        return Some(format!("{} is a file; choose another name", target.display()));
    }
    None
}

/// Move a project's auto-save beside it along with it: `<stem>_autosave`
/// from beside `from` to beside `to`, so a later project of the old name
/// is not offered this one's recovery. Nothing when there is none.
pub fn move_autosave(from_autosave: &Path, to_autosave: &Path) -> std::io::Result<()> {
    if !from_autosave.exists() {
        return Ok(());
    }
    move_project_folder(from_autosave, to_autosave).map(|_| ())
}

/// The media items whose files are inside `from`, with where each is now
/// that the folder is at `to`; their paths in `pool` are changed to it.
/// The caller reopens them in the engine.
pub fn rewrite_media_paths(pool: &mut klipp_media::MediaPool, from: &Path, to: &Path) -> Vec<(klipp_core::MediaId, PathBuf)> {
    let mut moved = Vec::new();
    let follow = |p: &mut Option<PathBuf>| {
        if let Some(rest) = p.as_deref().and_then(|p| p.strip_prefix(from).ok()) {
            *p = Some(to.join(rest));
        }
    };
    for item in pool.all_mut() {
        // Its proxy and skim file too, where they were kept with it.
        follow(&mut item.proxy_path);
        follow(&mut item.skim_file);
        if let Ok(rest) = item.path.strip_prefix(from) {
            item.path = to.join(rest);
            item.offline = !item.path.exists();
            moved.push((item.id, item.path.clone()));
        }
    }
    moved
}

/// Where Untitled projects keep a template's copied media until they are
/// written: one folder each under the cache.
pub fn untitled_cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("klipp").join("untitled")
}

/// Move a template's copied media from the Untitled project's `staging`
/// folder into the written project, as its `Template Media`; across
/// filesystems a copy, the cache's left for [`discard_staging`].
pub fn adopt_staged_media(staging: &Path, project: &Path) -> std::io::Result<bool> {
    let media = staging.join("Template Media");
    if !media.is_dir() {
        return Ok(false);
    }
    move_project_folder(&media, &project.join("Template Media")).map(|_| true)
}

/// Take a written Untitled project's staged media from `staging` into
/// `project`, and only then let the staging folder go. When the media
/// cannot be moved the staging folder is kept, so the paths the media
/// pool holds into it still lead to the files.
pub fn finish_staging(staging: &Path, project: &Path, cache: &Path) -> std::io::Result<bool> {
    let adopted = adopt_staged_media(staging, project)?;
    discard_staging(staging, cache);
    Ok(adopted)
}

/// Make an Untitled project's staging folder in the untitled `cache` and
/// lock it as the open project's folders are, so another Klipp's clean-up
/// leaves it alone while this one runs.
pub fn make_staging(cache: &Path, me: &klipp_project::lock::Holder) -> std::io::Result<PathBuf> {
    let staging = cache.join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&staging)?;
    let _ = klipp_project::lock::acquire(&staging, me);
    Ok(staging)
}

/// What marks a staging folder whose media a written project still uses:
/// moving it in failed, so the clean-up must never take it.
pub const KEPT_MARK: &str = "kept-for-project";

/// Mark `staging` as still used by `project`, for good.
pub fn keep_staging(staging: &Path, project: &Path) {
    let _ = std::fs::write(staging.join(KEPT_MARK), project.to_string_lossy().as_bytes());
}

/// Remove an Untitled project's staging folder: only one directly in the
/// untitled cache `cache`, never anything else.
pub fn discard_staging(staging: &Path, cache: &Path) -> bool {
    staging.parent() == Some(cache) && staging.is_dir() && !staging.join(KEPT_MARK).exists() && std::fs::remove_dir_all(staging).is_ok()
}

/// Remove staging folders a crash left in the untitled cache `cache`.
/// Each is locked by the Klipp using it: one whose holder's process is
/// gone is removed; one held by a running Klipp, here or elsewhere, is
/// kept. One with no lock at all (being made right now, or from before
/// locks) is removed only once older than `max_age` at `now`. Returns
/// how many went.
pub fn cleanup_untitled_cache(cache: &Path, me: &klipp_project::lock::Holder, now: std::time::SystemTime, max_age: std::time::Duration) -> usize {
    use klipp_project::lock::{self, Liveness, LockState};
    let Ok(entries) = std::fs::read_dir(cache) else { return 0 };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| match lock::inspect(&e.path(), me) {
            LockState::Held(_, Liveness::Dead) => true,
            LockState::Free => e.metadata().and_then(|m| m.modified()).is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > max_age)),
            LockState::Ours | LockState::Held(..) => false,
        })
        .filter(|e| discard_staging(&e.path(), cache))
        .count()
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

    /// #1864: new projects go in the projects root, never inside a
    /// project or the folder a project sits in alone; ~/Videos when no
    /// root is chosen.
    #[test]
    fn the_projects_root_is_never_a_projects_own_folder() {
        let home = std::env::temp_dir().join(format!("klipp-root-1864-{}", uuid::Uuid::new_v4()));
        let videos = home.join("Videos");
        let golden = videos.join("Golden Days");
        project(&golden, "Golden Days");
        let mine = home.join("Work");
        std::fs::create_dir_all(&mine).unwrap();
        assert_eq!(projects_root_in(None, &home), videos);
        assert_eq!(projects_root_in(Some(&mine), &home), mine, "a chosen root");
        assert_eq!(projects_root_in(Some(&golden), &home), videos, "not the folder Golden Days sits in");
        assert_eq!(projects_root_in(Some(&golden.join("Golden Days.klipp")), &home), videos, "not a project");
        assert_eq!(projects_root_in(Some(&videos), &home), videos, "a folder of several projects is a root");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// #1864: an untitled project is written only on its first real
    /// edit, under a free "Untitled" in the root; untouched, nothing.
    #[test]
    fn an_untitled_project_is_written_only_once_edited() {
        let root = std::env::temp_dir().join(format!("klipp-untitled-1864-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let untitled = Some(Path::new(UNTITLED));
        assert_eq!(first_write(untitled, false, &root), None, "untouched: nothing");
        assert_eq!(first_write(untitled, true, &root), Some(root.join("Untitled.klipp")));
        project(&root, "Untitled");
        assert_eq!(first_write(untitled, true, &root), Some(root.join("Untitled 2.klipp")), "a free name");
        // Save offers the name the project was written under, not the next
        // free one, which its own folder had taken.
        let written = root.join("Untitled.klipp");
        assert_eq!(offered_name(&root, Some(&written)), "Untitled");
        assert_eq!(offered_name(&root, None), "Untitled 2", "never written: a free name");
        assert_eq!(offered_name(&root, Some(&std::path::PathBuf::from("/elsewhere/Film.klipp"))), "Untitled 2", "not in the projects folder: a free name there");
        assert_eq!(first_write(Some(&root.join("Trip.klipp")), true, &root), None, "a project with a folder");
        assert_eq!(first_write(None, true, &root), None, "no project");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #1864 review: with no ~/Videos the root is home itself, and the
    /// first write makes only the project folder there.
    #[test]
    fn without_videos_the_root_is_home_and_only_the_project_is_made() {
        let home = std::env::temp_dir().join(format!("klipp-home-1864-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let root = projects_root_in(None, &home);
        assert_eq!(root, home);
        let target = first_write(Some(Path::new(UNTITLED)), true, &root).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let made: Vec<_> = std::fs::read_dir(&home).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(made, ["Untitled.klipp"], "nothing else, no Videos folder");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// #1864 review: a move is a rename; across filesystems a copy that
    /// keeps the original whole; onto something there, refused.
    #[test]
    fn a_project_folder_moves_or_is_copied_never_lost() {
        let root = std::env::temp_dir().join(format!("klipp-move-1864-{}", uuid::Uuid::new_v4()));
        let from = project(&root, "Untitled");
        std::fs::write(from.join(klipp_project::lock::LOCK_FILE), b"ours").unwrap();
        let to = root.join("Films/Trip.klipp");
        assert_eq!(move_project_folder(&from, &to).unwrap(), Moved::Renamed);
        assert!(!from.exists() && to.join("media/manifest.json").exists());

        let cross = |_: &Path, _: &Path| Err(std::io::Error::new(std::io::ErrorKind::CrossesDevices, "another disk"));
        let elsewhere = root.join("Other/Trip.klipp");
        assert_eq!(move_project_folder_with(&to, &elsewhere, cross).unwrap(), Moved::Copied);
        assert!(to.join("project.json").exists(), "the original is kept");
        assert!(elsewhere.join("media/manifest.json").exists(), "the copy is whole");

        assert_eq!(move_project_folder(&to, &elsewhere).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists, "nothing is replaced");
        assert!(to.exists() && elsewhere.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #1864 review: media inside the moved folder is pointed at its new
    /// place; media elsewhere is left alone.
    #[test]
    fn media_inside_a_moved_project_follows_it() {
        let mut pool = klipp_media::MediaPool::new();
        let a = pool.add(klipp_media::MediaItem::synthetic_for_test(PathBuf::from("/v/Untitled.klipp/Template Media/a.mov"), None));
        let b = pool.add(klipp_media::MediaItem::synthetic_for_test(PathBuf::from("/footage/b.mov"), None));
        let moved = rewrite_media_paths(&mut pool, Path::new("/v/Untitled.klipp"), Path::new("/v/Trip.klipp"));
        assert_eq!(moved, [(a, PathBuf::from("/v/Trip.klipp/Template Media/a.mov"))]);
        assert_eq!(pool.get(&a).unwrap().path, PathBuf::from("/v/Trip.klipp/Template Media/a.mov"));
        assert_eq!(pool.get(&b).unwrap().path, PathBuf::from("/footage/b.mov"));
    }

    /// #1864 review: staged template media goes into the written project;
    /// a staging folder is removed only from the untitled cache, and on
    /// start-up only those a day old.
    #[test]
    fn staging_is_adopted_and_only_old_cache_is_cleared() {
        let root = std::env::temp_dir().join(format!("klipp-stage-1864-{}", uuid::Uuid::new_v4()));
        let cache = root.join("untitled");
        let staging = cache.join("one");
        std::fs::create_dir_all(staging.join("Template Media")).unwrap();
        std::fs::write(staging.join("Template Media/a.mov"), b"x").unwrap();
        let project = root.join("Untitled.klipp");
        std::fs::create_dir_all(&project).unwrap();
        assert!(adopt_staged_media(&staging, &project).unwrap());
        assert!(project.join("Template Media/a.mov").exists());
        assert!(!adopt_staged_media(&staging, &project).unwrap(), "nothing left to adopt");

        assert!(!discard_staging(&project, &cache), "not a staging folder");
        assert!(project.exists());
        assert!(discard_staging(&staging, &cache) && !staging.exists());

        let me = klipp_project::lock::Holder::current();
        let day = std::time::Duration::from_secs(24 * 3600);
        let now = std::time::SystemTime::now();
        // One in use by this Klipp, one by another running Klipp, one a
        // crashed Klipp left, and one with no lock.
        let ours = make_staging(&cache, &me).unwrap();
        let lock_of = |dir: &Path, pid: u32| {
            std::fs::create_dir_all(dir).unwrap();
            let info = klipp_project::lock::LockInfo { pid, machine: me.machine.clone(), session: format!("other-{pid}"), heartbeat_at: klipp_project::lock::now_secs(), ..Default::default() };
            std::fs::write(dir.join(klipp_project::lock::LOCK_FILE), serde_json::to_vec(&info).unwrap()).unwrap();
        };
        let running = cache.join("running");
        lock_of(&running, std::process::id());
        let crashed = cache.join("crashed");
        lock_of(&crashed, u32::MAX - 7);
        let bare = cache.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(cleanup_untitled_cache(&cache, &me, now, day), 1, "only the crashed one");
        assert!(!crashed.exists() && ours.exists() && running.exists() && bare.exists());
        // A day on, the unlocked one too; those in use never, whatever
        // their age.
        assert_eq!(cleanup_untitled_cache(&cache, &me, now + 2 * day, day), 1);
        assert!(!bare.exists() && ours.exists() && running.exists() && project.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #1864 review: staged media that cannot be moved keeps its staging
    /// folder, so the paths into it still lead to the files.
    #[test]
    fn staging_is_kept_when_its_media_cannot_move() {
        let root = std::env::temp_dir().join(format!("klipp-keep-1864-{}", uuid::Uuid::new_v4()));
        let cache = root.join("untitled");
        let staging = cache.join("one");
        std::fs::create_dir_all(staging.join("Template Media")).unwrap();
        std::fs::write(staging.join("Template Media/a.mov"), b"x").unwrap();
        let project = root.join("Untitled.klipp");
        std::fs::create_dir_all(project.join("Template Media")).unwrap();
        assert!(finish_staging(&staging, &project, &cache).is_err(), "Template Media is there already");
        assert!(staging.join("Template Media/a.mov").exists(), "the staged media is kept");
        // Kept for good: no clean-up takes it, however old and unlocked.
        keep_staging(&staging, &project);
        let me = klipp_project::lock::Holder::current();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(9 * 24 * 3600);
        assert_eq!(cleanup_untitled_cache(&cache, &me, later, std::time::Duration::from_secs(3600)), 0);
        assert!(!discard_staging(&staging, &cache) && staging.exists());
        std::fs::remove_file(staging.join(KEPT_MARK)).unwrap();
        std::fs::remove_dir(project.join("Template Media")).unwrap();
        assert!(finish_staging(&staging, &project, &cache).unwrap());
        assert!(project.join("Template Media/a.mov").exists() && !staging.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #1864 review: Save As never writes over another project.
    #[test]
    fn save_as_refuses_another_project() {
        let root = std::env::temp_dir().join(format!("klipp-saveas-1864-{}", uuid::Uuid::new_v4()));
        let other = project(&root, "Trip");
        let mine = project(&root, "Untitled");
        assert!(save_as_refusal(&other, Some(&mine)).is_some(), "another project");
        assert!(save_as_refusal(&mine, Some(&mine)).is_none(), "its own place");
        assert!(save_as_refusal(&root.join("New.klipp"), Some(&mine)).is_none());
        assert_eq!(rename_project_folder(&mine, &other).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #1864 review: a rename across filesystems is left to a copy, the
    /// original untouched; on one filesystem it is renamed.
    #[test]
    fn a_rename_across_filesystems_is_left_to_a_copy() {
        let root = std::env::temp_dir().join(format!("klipp-rename-1864-{}", uuid::Uuid::new_v4()));
        let mine = project(&root, "Untitled");
        let cross = |_: &Path, _: &Path| Err(std::io::Error::new(std::io::ErrorKind::CrossesDevices, "another disk"));
        assert_eq!(rename_project_folder_with(&mine, &root.join("New.klipp"), cross).unwrap(), RenameOutcome::NeedsCopy);
        assert!(mine.exists(), "nothing moved yet");
        assert_eq!(rename_project_folder(&mine, &root.join("New.klipp")).unwrap(), RenameOutcome::Renamed);
        assert!(!mine.exists() && root.join("New.klipp/project.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #1864 review: the auto-save goes along with the project, so the
    /// old name offers no recovery to a later project of that name.
    #[test]
    fn the_autosave_moves_with_the_project() {
        let root = std::env::temp_dir().join(format!("klipp-auto-1864-{}", uuid::Uuid::new_v4()));
        let from = root.join("Untitled_autosave.klipp");
        std::fs::create_dir_all(&from).unwrap();
        move_autosave(&from, &root.join("New_autosave.klipp")).unwrap();
        assert!(!from.exists() && root.join("New_autosave.klipp").exists());
        move_autosave(&from, &root.join("New_autosave.klipp")).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #1864 review: a copy on its own thread counts its bytes for the
    /// progress shown.
    #[test]
    fn a_copy_counts_what_it_has_copied() {
        let root = std::env::temp_dir().join(format!("klipp-count-1864-{}", uuid::Uuid::new_v4()));
        let from = project(&root, "New");
        let copied = std::sync::atomic::AtomicU64::new(0);
        copy_project_tree_counting(&from, &root.join("Copy.klipp"), &copied).unwrap();
        assert!(tree_size(&from) > 0);
        assert_eq!(copied.load(std::sync::atomic::Ordering::Relaxed), tree_size(&from));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #1864 review: a moved item's proxy follows it when it was kept in
    /// the project; one kept elsewhere is left.
    #[test]
    fn a_moved_items_proxy_follows_it() {
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test(PathBuf::from("/v/Untitled.klipp/a.mov"), None);
        item.proxy_path = Some(PathBuf::from("/v/Untitled.klipp/Proxies/a.mov"));
        item.skim_file = Some(PathBuf::from("/cache/skim/a.mp4"));
        let id = pool.add(item);
        rewrite_media_paths(&mut pool, Path::new("/v/Untitled.klipp"), Path::new("/v/New.klipp"));
        let item = pool.get(&id).unwrap();
        assert_eq!(item.proxy_path.as_deref(), Some(Path::new("/v/New.klipp/Proxies/a.mov")));
        assert_eq!(item.skim_file.as_deref(), Some(Path::new("/cache/skim/a.mp4")), "kept elsewhere, left");
    }
}
