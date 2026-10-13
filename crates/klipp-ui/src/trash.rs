//! Moving files to the desktop's trash, as the freedesktop.org Trash
//! specification lays it out: the item goes to `Trash/files`, and a
//! `.trashinfo` beside it in `Trash/info` says where it came from and
//! when, so the file manager can put it back.
//!
//! Nothing here deletes anything. A move that cannot be made is an
//! error, and the item stays where it was.

use std::io;
use std::path::{Path, PathBuf};

/// Move `path` to the user's trash, and return where it now is.
///
/// The home trash is used when the item is on the same filesystem as it;
/// an item on another filesystem is handed to `gio trash`, which knows
/// that filesystem's own trash folder. Either way the item stays put when
/// the move fails.
pub fn move_to_trash(path: &Path) -> io::Result<PathBuf> {
    let home_trash = dirs::data_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no data folder for the trash"))?
        .join("Trash");
    match move_to_trash_in(path, &home_trash) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            let status = std::process::Command::new("gio").arg("trash").arg("--").arg(path).status()?;
            if status.success() && !path.exists() {
                Ok(path.to_path_buf())
            } else {
                Err(io::Error::other(format!("gio trash could not move {}", path.display())))
            }
        }
        other => other,
    }
}

/// Move `path` into the trash folder `trash` (holding `files` and `info`).
pub fn move_to_trash_in(path: &Path, trash: &Path) -> io::Result<PathBuf> {
    let path = std::path::absolute(path)?;
    if std::fs::symlink_metadata(&path).is_err() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is not there", path.display())));
    }
    let files = trash.join("files");
    let info = trash.join("info");
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;
    let base = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "nothing to trash"))?
        .to_string_lossy()
        .into_owned();
    let deleted_at = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    let body = format!("[Trash Info]\nPath={}\nDeletionDate={deleted_at}\n", encode_path(&path));
    // The info file is made first, and made new: that claims the name
    // against another program trashing a file of the same name.
    for n in 1..10_000u32 {
        let name = if n == 1 { base.clone() } else { format!("{base}.{n}") };
        let info_path = info.join(format!("{name}.trashinfo"));
        let target = files.join(&name);
        if target.exists() {
            continue;
        }
        let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(&info_path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        std::io::Write::write_all(&mut file, body.as_bytes())?;
        drop(file);
        return match std::fs::rename(&path, &target) {
            Ok(()) => Ok(target),
            Err(e) => {
                let _ = std::fs::remove_file(&info_path);
                Err(e)
            }
        };
    }
    Err(io::Error::other("no free name in the trash"))
}

/// The path as the specification stores it: percent-encoded bytes.
fn encode_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The folder goes to `files`, whole, and `info` says where it was;
    /// a second of the same name is kept apart rather than replacing it.
    #[test]
    fn a_folder_moves_to_the_trash_with_its_origin() {
        let root = std::env::temp_dir().join(format!("klipp-trash-{}", uuid::Uuid::new_v4()));
        let trash = root.join("Trash");
        let project = root.join("My Film.klipp");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("project.json"), b"{}").unwrap();
        let moved = move_to_trash_in(&project, &trash).unwrap();
        assert!(!project.exists());
        assert_eq!(moved, trash.join("files/My Film.klipp"));
        assert_eq!(std::fs::read(moved.join("project.json")).unwrap(), b"{}");
        let info = std::fs::read_to_string(trash.join("info/My Film.klipp.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\n"));
        assert!(info.contains(&format!("Path={}/My%20Film.klipp\n", root.display())), "{info}");
        assert!(info.contains("DeletionDate="));

        std::fs::create_dir_all(&project).unwrap();
        let again = move_to_trash_in(&project, &trash).unwrap();
        assert_eq!(again, trash.join("files/My Film.klipp.2"));
        assert!(trash.join("files/My Film.klipp/project.json").exists(), "the first is untouched");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Nothing there, nothing claimed: no info file is left behind.
    #[test]
    fn a_missing_item_is_an_error_and_leaves_nothing() {
        let root = std::env::temp_dir().join(format!("klipp-trash-{}", uuid::Uuid::new_v4()));
        assert!(move_to_trash_in(&root.join("gone.klipp"), &root.join("Trash")).is_err());
        assert!(!root.join("Trash/info/gone.klipp.trashinfo").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
