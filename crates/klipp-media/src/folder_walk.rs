//! The media files of a folder and its subfolders (#3096), as Import
//! Folder and `import_media {folder}` take them.
//!
//! The folder may come from anywhere (a card, a download, a shared
//! drive), so the walk is bounded: links are never followed, hidden
//! files and folders are left out, it goes at most [`MAX_DEPTH`] folders
//! down, reads at most [`MAX_FOLDERS`] folders and takes at most
//! [`MAX_FILES`] files. A limit reached is said, not passed over.

use std::path::{Path, PathBuf};

/// How many folders below the one picked are read.
pub const MAX_DEPTH: usize = 8;
/// How many media files one import takes.
pub const MAX_FILES: usize = 5_000;
/// How many folders one import reads, the picked one among them.
pub const MAX_FOLDERS: usize = 2_000;
/// The longest bin name a folder's path makes, in characters.
const MAX_BIN_CHARS: usize = 120;

/// A media file found, and the subfolder it is in.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundFile {
    pub path: PathBuf,
    /// Its folder below the one picked, as `video/day 1`; `None` in the
    /// picked folder itself. Made printable and cut to a bin name's
    /// length, so it can name a bin as it is.
    pub folder: Option<String>,
}

/// What a walk found, and what it left out and why.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FolderMedia {
    pub files: Vec<FoundFile>,
    /// Symbolic links, to a file or a folder: not followed.
    pub links: usize,
    /// Hidden files and folders (a name starting with a dot).
    pub hidden: usize,
    /// A camera card's proxies, thumbnails and indexes: not clips.
    pub card_extras: usize,
    /// Subfolders that could not be read.
    pub unreadable: usize,
    /// Subfolders not read: the walk is not recursive, or they are
    /// deeper than [`MAX_DEPTH`].
    pub folders_not_read: usize,
    /// A folder deeper than [`MAX_DEPTH`] was left unread.
    pub depth_limit: bool,
    /// [`MAX_FILES`] files were taken and the walk stopped there.
    pub file_limit: bool,
    /// [`MAX_FOLDERS`] folders were read and the walk stopped there.
    pub folder_limit: bool,
}

impl FolderMedia {
    /// Files per folder, in the order found: `None` is the picked folder.
    pub fn by_folder(&self) -> Vec<(Option<String>, usize)> {
        let mut out: Vec<(Option<String>, usize)> = Vec::new();
        for f in &self.files {
            match out.iter_mut().find(|(k, _)| *k == f.folder) {
                Some((_, n)) => *n += 1,
                None => out.push((f.folder.clone(), 1)),
            }
        }
        out
    }

    /// What was left out, and the limits reached, in words; empty when
    /// nothing was.
    pub fn left_out(&self) -> Vec<String> {
        let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut out = Vec::new();
        if self.links > 0 {
            out.push(format!("{} not followed", plural(self.links, "link", "links")));
        }
        if self.hidden > 0 {
            out.push(format!("{} skipped", plural(self.hidden, "hidden item", "hidden items")));
        }
        if self.card_extras > 0 {
            out.push(format!("{} skipped", plural(self.card_extras, "card proxy or index file", "card proxy and index files")));
        }
        if self.unreadable > 0 {
            out.push(format!("{} could not be read", plural(self.unreadable, "subfolder", "subfolders")));
        }
        if self.depth_limit {
            out.push(format!("folders more than {MAX_DEPTH} levels down were not read"));
        } else if self.folders_not_read > 0 {
            out.push(format!("{} not read", plural(self.folders_not_read, "subfolder", "subfolders")));
        }
        if self.file_limit {
            out.push(format!("stopped at {MAX_FILES} files"));
        }
        if self.folder_limit {
            out.push(format!("stopped after reading {MAX_FOLDERS} folders"));
        }
        out
    }
}

/// The media files in `root`, and with `recursive` in its subfolders,
/// each folder's files in name order before its subfolders. Only `root`
/// not being readable is an error; anything below it that can't be read
/// is counted and passed over.
pub fn media_in_folder(root: &Path, recursive: bool) -> std::io::Result<FolderMedia> {
    let mut found = FolderMedia::default();
    let mut folders_read = 0usize;
    // Depth first, each folder's subfolders in name order: a stack of
    // (folder, its path below root, depth, the camera card it is on).
    let mut stack: Vec<(PathBuf, Vec<String>, usize, Option<PathBuf>)> = vec![(root.to_path_buf(), Vec::new(), 0, None)];
    while let Some((dir, below, depth, card)) = stack.pop() {
        if folders_read == MAX_FOLDERS {
            found.folder_limit = true;
            break;
        }
        folders_read += 1;
        // A card's extras are told by its folder names below the card
        // (`SUB`, `PROXY`, `VOICE`): only on a card, so a person's own
        // `sfx/voice` or `video/sub` is imported.
        let card = card.or_else(|| crate::card::card_kind(&dir).map(|_| dir.clone()));
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if depth == 0 => return Err(e),
            Err(_) => {
                found.unreadable += 1;
                continue;
            }
        };
        let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        let folder = (!below.is_empty()).then(|| bin_name(&below));
        let mut subfolders = Vec::new();
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            // The entry's own type: a link is a link, never what it
            // points at.
            let Ok(kind) = entry.file_type() else {
                found.unreadable += 1;
                continue;
            };
            if kind.is_symlink() {
                found.links += 1;
            } else if name.starts_with('.') {
                found.hidden += 1;
            } else if kind.is_dir() {
                subfolders.push((entry.path(), name));
            } else if kind.is_file() && klipp_core::media_extensions::path_is_media(&entry.path()) {
                if card.as_ref().is_some_and(|c| entry.path().strip_prefix(c).is_ok_and(crate::card::is_card_side_file)) {
                    found.card_extras += 1;
                    continue;
                }
                if found.files.len() == MAX_FILES {
                    found.file_limit = true;
                    return Ok(found);
                }
                found.files.push(FoundFile { path: entry.path(), folder: folder.clone() });
            }
        }
        if !recursive || depth == MAX_DEPTH {
            found.folders_not_read += subfolders.len();
            found.depth_limit |= recursive && !subfolders.is_empty();
            continue;
        }
        // Pushed last first, so the first by name is read next.
        for (path, name) in subfolders.into_iter().rev() {
            let mut below = below.clone();
            below.push(name);
            stack.push((path, below, depth + 1, card.clone()));
        }
    }
    Ok(found)
}

/// A folder's path below the picked one as a bin's name: its names
/// joined with `/`, with no control characters, cut to a bin name's
/// length.
fn bin_name(below: &[String]) -> String {
    let joined = below.join("/");
    let clean: String = joined.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let clean = clean.trim();
    match clean.char_indices().nth(MAX_BIN_CHARS) {
        Some((cut, _)) => format!("{}\u{2026}", &clean[..cut]),
        None => clean.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-folder-walk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    /// #3096: media two levels of subfolders down is found, each file
    /// with its folder, in name order; other files are passed over.
    #[test]
    fn two_levels_of_subfolders_are_read_in_order() {
        let root = scratch("levels");
        for f in ["top.mp4", "video/a.mov", "video/day 2/b.mp4", "music/song.wav", "sfx/notes.txt", "sfx/hit.flac"] {
            touch(&root.join(f));
        }
        let found = media_in_folder(&root, true).unwrap();
        let got: Vec<(String, Option<&str>)> = found.files.iter().map(|f| (f.path.strip_prefix(&root).unwrap().display().to_string(), f.folder.as_deref())).collect();
        assert_eq!(got, [
            ("top.mp4".to_string(), None),
            ("music/song.wav".to_string(), Some("music")),
            ("sfx/hit.flac".to_string(), Some("sfx")),
            ("video/a.mov".to_string(), Some("video")),
            ("video/day 2/b.mp4".to_string(), Some("video/day 2")),
        ]);
        assert_eq!(found.by_folder(), [(None, 1), (Some("music".into()), 1), (Some("sfx".into()), 1), (Some("video".into()), 1), (Some("video/day 2".into()), 1)]);
        assert!(found.left_out().is_empty(), "{:?}", found.left_out());

        let flat = media_in_folder(&root, false).unwrap();
        assert_eq!(flat.files.len(), 1, "not recursive: the picked folder's own files");
        assert_eq!(flat.folders_not_read, 3);
        assert!(!flat.depth_limit && flat.left_out() == ["3 subfolders not read"], "{:?}", flat.left_out());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Hostile folder: links to files and folders outside it (one a loop
    /// back to itself) are not followed, hidden files and folders are
    /// left out, folders past the depth limit are not read, and a
    /// folder name with control characters makes a printable bin name.
    #[cfg(unix)]
    #[test]
    fn links_hidden_items_and_the_depth_limit_are_kept_to() {
        let root = scratch("hostile");
        let outside = scratch("hostile-outside");
        touch(&outside.join("secret.mp4"));
        touch(&root.join("ok.mp4"));
        std::os::unix::fs::symlink(outside.join("secret.mp4"), root.join("linked.mp4")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked-folder")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        touch(&root.join(".hidden/in-hidden.mp4"));
        touch(&root.join(".dot.mp4"));
        let mut deep = root.join("d");
        for level in 1..=MAX_DEPTH + 2 {
            touch(&deep.join(format!("level{level}.mp4")));
            deep = deep.join("d");
        }
        touch(&root.join("bad\u{1b}[2Jname/x.mp4"));

        let found = media_in_folder(&root, true).unwrap();
        let names: Vec<String> = found.files.iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert!(!names.iter().any(|n| n == "secret.mp4" || n == "linked.mp4" || n.contains("hidden") || n == ".dot.mp4"), "{names:?}");
        assert!(found.files.iter().all(|f| f.path.starts_with(&root)), "nothing from outside the folder");
        assert_eq!((found.links, found.hidden), (3, 2));
        let levels = names.iter().filter(|n| n.starts_with("level")).count();
        assert_eq!(levels, MAX_DEPTH, "levels 1..={MAX_DEPTH} read, deeper not");
        assert!(found.depth_limit && found.left_out().iter().any(|s| s.contains("levels down")), "{:?}", found.left_out());
        let bad = found.files.iter().find(|f| f.path.ends_with("x.mp4")).unwrap();
        assert_eq!(bad.folder.as_deref(), Some("bad [2Jname"), "no control characters in a bin name");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// The file limit stops the walk and is said; a long folder name is
    /// cut to a bin name's length.
    #[test]
    fn the_file_limit_stops_the_walk_and_long_names_are_cut() {
        let root = scratch("limit");
        for i in 0..MAX_FILES + 3 {
            std::fs::write(root.join(format!("{i:05}.wav")), b"x").unwrap();
        }
        let found = media_in_folder(&root, true).unwrap();
        assert_eq!(found.files.len(), MAX_FILES);
        assert!(found.file_limit && found.left_out().iter().any(|s| s == &format!("stopped at {MAX_FILES} files")), "{:?}", found.left_out());
        let _ = std::fs::remove_dir_all(&root);

        let long = bin_name(&["x".repeat(300)]);
        assert_eq!(long.chars().count(), MAX_BIN_CHARS + 1, "cut, with an ellipsis");
    }

    /// A card's proxies and thumbnails are left out on a card, found
    /// below the picked folder too; folders with the same names off a
    /// card are a person's own and are read.
    #[test]
    fn card_extras_are_left_out_only_on_a_card() {
        let root = scratch("card");
        for f in ["sfx/voice/line.wav", "video/sub/take.mp4", "cam A/PRIVATE/M4ROOT/CLIP/C0001.MP4", "cam A/PRIVATE/M4ROOT/SUB/C0001S03.MP4"] {
            touch(&root.join(f));
        }
        let found = media_in_folder(&root, true).unwrap();
        let names: Vec<String> = found.files.iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["C0001.MP4", "line.wav", "take.mp4"]);
        assert_eq!(found.card_extras, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The picked folder not readable is an error; a subfolder that
    /// can't be read is counted and passed over.
    #[test]
    fn only_the_picked_folder_unreadable_is_an_error() {
        let missing = std::env::temp_dir().join(format!("klipp-folder-walk-missing-{}", std::process::id()));
        assert!(media_in_folder(&missing, true).is_err());
    }
}
