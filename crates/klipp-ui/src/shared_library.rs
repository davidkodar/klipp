//! The Shared Library's media collections (#614): files kept in named
//! collections across projects, to browse from any project and bring into
//! it. An entry is a reference to a file where it is, never a copy: adding
//! a large source file costs nothing, and a file moved or deleted shows as
//! missing, to relink. Taking an entry out of a collection leaves any
//! project that imported it as it is.
//!
//! Kept in the user's config folder, `shared_library/collections.json`,
//! written whole and moved into place.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The format this build writes and reads.
pub const FORMAT: u32 = 1;

/// The collection an entry goes to when none is named.
pub const DEFAULT_COLLECTION: &str = "Shared";

/// Every collection, by name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SharedLibrary {
    #[serde(default)]
    pub format: u32,
    #[serde(default)]
    pub collections: Vec<Collection>,
}

/// A named collection of files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collection {
    pub name: String,
    #[serde(default)]
    pub items: Vec<SharedItem>,
}

/// A file in a collection, where it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedItem {
    pub path: PathBuf,
}

impl SharedItem {
    /// Its file's name.
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }
}

/// Where the library is kept.
pub fn dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("klipp").join("shared_library"))
}

impl SharedLibrary {
    /// The library kept in `dir`: empty when none is there yet, or one this
    /// build doesn't read (left on disk as it is until something is saved).
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("collections.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<SharedLibrary>(&t).ok())
            .filter(|l| l.format <= FORMAT)
            .unwrap_or_default()
    }

    /// Kept in `dir`, written whole and moved into place.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut out = self.clone();
        out.format = FORMAT;
        let json = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
        let partial = dir.join("collections.json.partial");
        std::fs::write(&partial, json).map_err(|e| e.to_string())?;
        std::fs::rename(&partial, dir.join("collections.json")).map_err(|e| e.to_string())
    }

    pub fn collection(&self, name: &str) -> Option<&Collection> {
        self.collections.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }

    fn collection_mut(&mut self, name: &str) -> Option<&mut Collection> {
        self.collections.iter_mut().find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// A new, empty collection `name`; refused blank or when one of that
    /// name (any case) is there.
    pub fn create(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Give the collection a name".into());
        }
        if self.collection(name).is_some() {
            return Err(format!("There is a collection \"{name}\" already"));
        }
        self.collections.push(Collection { name: name.to_string(), items: Vec::new() });
        self.collections.sort_by_key(|c| c.name.to_lowercase());
        Ok(())
    }

    /// `paths` added to `collection` (made when it isn't there): a file
    /// already in it isn't added twice. How many were added.
    pub fn add(&mut self, collection: &str, paths: &[PathBuf]) -> usize {
        let name = if collection.trim().is_empty() { DEFAULT_COLLECTION } else { collection.trim() };
        if self.collection(name).is_none() {
            let _ = self.create(name);
        }
        let c = self.collection_mut(name).expect("made above");
        let mut added = 0;
        for p in paths {
            if !c.items.iter().any(|i| same_file(&i.path, p)) {
                c.items.push(SharedItem { path: p.clone() });
                added += 1;
            }
        }
        added
    }

    /// `path` taken out of `collection`. Whether it was there.
    pub fn remove(&mut self, collection: &str, path: &Path) -> bool {
        let Some(c) = self.collection_mut(collection) else { return false };
        let before = c.items.len();
        c.items.retain(|i| i.path != path);
        c.items.len() != before
    }

    /// The entry for `old` in `collection` pointed at `new`: a moved file
    /// found again. Refused when `new` is in the collection already.
    pub fn relink(&mut self, collection: &str, old: &Path, new: &Path) -> Result<(), String> {
        let c = self.collection_mut(collection).ok_or("No such collection")?;
        if c.items.iter().any(|i| i.path != old && same_file(&i.path, new)) {
            return Err("That file is in the collection already".into());
        }
        let item = c.items.iter_mut().find(|i| i.path == old).ok_or("No such entry")?;
        item.path = new.to_path_buf();
        Ok(())
    }

    /// Collection `from` renamed `to`; refused blank, or onto another's name.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let to = to.trim();
        if to.is_empty() {
            return Err("Give the collection a name".into());
        }
        if !from.eq_ignore_ascii_case(to) && self.collection(to).is_some() {
            return Err(format!("There is a collection \"{to}\" already"));
        }
        let c = self.collection_mut(from).ok_or("No such collection")?;
        c.name = to.to_string();
        self.collections.sort_by_key(|c| c.name.to_lowercase());
        Ok(())
    }

    /// Collection `name` deleted: its entries, not their files.
    pub fn delete(&mut self, name: &str) -> bool {
        let before = self.collections.len();
        self.collections.retain(|c| !c.name.eq_ignore_ascii_case(name));
        self.collections.len() != before
    }

    /// The entries of `collection` whose file name holds `query` (any
    /// case), every one for an empty query.
    pub fn search<'a>(&'a self, collection: &str, query: &'a str) -> Vec<&'a SharedItem> {
        let q = query.trim().to_lowercase();
        self.collection(collection).map(|c| c.items.iter().filter(|i| q.is_empty() || i.name().to_lowercase().contains(&q)).collect()).unwrap_or_default()
    }
}

/// The Shared Library's commands run on `state` (#614): the library
/// changed, kept on disk and read back, and what happened told. Whether
/// `cmd` was one of them.
pub fn run_command(state: &mut crate::state::AppState, cmd: &klipp_core::events::AppCommand) -> bool {
    use klipp_core::events::AppCommand;
    let Some(dir) = state.shared_library_dir.clone().or_else(dir) else {
        if matches!(cmd, AppCommand::AddToSharedLibrary { .. } | AppCommand::AddMediaToSharedLibrary { .. }) {
            state.toast.show("No config folder to keep the Shared Library in");
        }
        return true;
    };
    // Always from disk: another Klipp may have changed it since.
    let mut lib = SharedLibrary::load(&dir);
    let current = if state.shared_collection.is_empty() { DEFAULT_COLLECTION.to_string() } else { state.shared_collection.clone() };
    let told = match cmd {
        AppCommand::AddToSharedLibrary { collection, .. } | AppCommand::AddMediaToSharedLibrary { collection, .. } => {
            let paths: Vec<PathBuf> = match cmd {
                AppCommand::AddMediaToSharedLibrary { media_ids, .. } => media_ids
                    .iter()
                    .filter_map(|id| state.media_pool.get(id))
                    .filter(|m| m.kind == klipp_media::MediaItemKind::File && !m.path.as_os_str().is_empty())
                    .map(|m| m.path.clone())
                    .collect(),
                AppCommand::AddToSharedLibrary { paths, .. } => paths.clone(),
                _ => Vec::new(),
            };
            let name = if collection.trim().is_empty() { current.clone() } else { collection.trim().to_string() };
            let added = lib.add(&name, &paths);
            let shown = lib.collection(&name).map(|c| c.name.clone()).unwrap_or(name);
            state.shared_collection = shown.clone();
            match (added, paths.len() - added) {
                (0, 0) => "Nothing to add: only files can go in the Shared Library".to_string(),
                (0, _) => format!("Already in \"{shown}\""),
                (n, 0) => format!("Added {n} file{} to \"{shown}\"", if n == 1 { "" } else { "s" }),
                (n, d) => format!("Added {n} to \"{shown}\"; {d} already there"),
            }
        }
        AppCommand::RemoveFromSharedLibrary { collection, path } => {
            if !lib.remove(collection, path) {
                return true;
            }
            format!("Took {} out of \"{collection}\"", SharedItem { path: path.clone() }.name())
        }
        AppCommand::RelinkSharedLibraryItem { collection, old, new } => match lib.relink(collection, old, new) {
            Ok(()) => format!("Relinked to {}", new.display()),
            Err(why) => {
                state.toast.show(why);
                return true;
            }
        },
        AppCommand::CreateSharedCollection { name } => match lib.create(name) {
            Ok(()) => {
                state.shared_collection = name.trim().to_string();
                format!("Made the collection \"{}\"", name.trim())
            }
            Err(why) => {
                state.toast.show(why);
                return true;
            }
        },
        AppCommand::RenameSharedCollection { from, to } => match lib.rename(from, to) {
            Ok(()) => {
                if state.shared_collection.eq_ignore_ascii_case(from) {
                    state.shared_collection = to.trim().to_string();
                }
                format!("Renamed \"{from}\" \"{}\"", to.trim())
            }
            Err(why) => {
                state.toast.show(why);
                return true;
            }
        },
        AppCommand::DeleteSharedCollection { name } => {
            if !lib.delete(name) {
                return true;
            }
            if state.shared_collection.eq_ignore_ascii_case(name) {
                state.shared_collection = lib.collections.first().map(|c| c.name.clone()).unwrap_or_default();
            }
            format!("Deleted the collection \"{name}\"; its files are where they were")
        }
        _ => return false,
    };
    lib.format = FORMAT;
    match lib.save(&dir) {
        Ok(()) => {
            state.shared_library = lib;
            state.toast.show(told);
        }
        Err(why) => state.toast.show(format!("The Shared Library could not be saved: {why}")),
    }
    true
}

/// Whether two paths name the same file: the same path, or the same file
/// once both are resolved.
fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #614: collections are made, filled without doubles, searched,
    /// renamed and deleted, and come back from disk as they were; an entry
    /// is a reference (relinked when the file moves), and taking it out
    /// touches no file.
    #[test]
    fn collections_keep_references_across_sessions() {
        let root = std::env::temp_dir().join(format!("klipp-shared-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let (logo, sting) = (root.join("logo.png"), root.join("sting.wav"));
        std::fs::write(&logo, b"png").unwrap();
        std::fs::write(&sting, b"wav").unwrap();
        let mut lib = SharedLibrary::default();
        assert_eq!(lib.add("Brand", &[logo.clone(), sting.clone()]), 2, "made and filled");
        assert_eq!(lib.add("brand", &[logo.clone(), root.join(".").join("logo.png")]), 0, "the same file, by any path, once");
        assert_eq!(lib.add("", std::slice::from_ref(&sting)), 1, "the default collection");
        assert!(lib.collection(DEFAULT_COLLECTION).is_some());
        assert!(lib.create("BRAND").is_err() && lib.create(" ").is_err());
        assert_eq!(lib.search("Brand", "LOGO").iter().map(|i| i.name()).collect::<Vec<_>>(), ["logo.png"]);
        assert_eq!(lib.search("Brand", "").len(), 2);

        // Moved on disk: relinked.
        let moved = root.join("logo-2.png");
        std::fs::rename(&logo, &moved).unwrap();
        lib.relink("Brand", &logo, &moved).unwrap();
        assert!(lib.relink("Brand", &moved, &sting).is_err(), "not onto another entry's file");
        // Through disk, as another session sees it.
        let dir = root.join("library");
        lib.save(&dir).unwrap();
        let back = SharedLibrary::load(&dir);
        assert_eq!(back.collections, lib.collections);
        assert_eq!(back.format, FORMAT);

        // Renamed, an entry taken out (its file stays), deleted.
        lib.rename("Brand", "Client A").unwrap();
        assert!(lib.rename("Client A", DEFAULT_COLLECTION).is_err());
        assert!(lib.remove("Client A", &sting));
        assert!(sting.exists(), "taking it out leaves the file");
        assert!(lib.delete("client a") && lib.collection("Client A").is_none());
        assert!(moved.exists() && sting.exists());
        // A library of a newer format is not read as empty-and-overwritten
        // by mistake: it reads empty, and stays on disk.
        std::fs::write(dir.join("collections.json"), r#"{"format": 99, "collections": []}"#).unwrap();
        assert_eq!(SharedLibrary::load(&dir), SharedLibrary::default());
        let _ = std::fs::remove_dir_all(root);
    }

    /// #614, through the commands: pool items' files go in (a title, with
    /// no file, doesn't), kept on disk; a name left empty means the
    /// collection shown; taking an entry out, or deleting its collection,
    /// leaves the project's pool as it was.
    #[test]
    fn the_commands_keep_the_library_on_disk_and_leave_the_project_alone() {
        use klipp_core::events::AppCommand;
        let root = std::env::temp_dir().join(format!("klipp-shared-cmd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let clip = root.join("b-roll.mov");
        std::fs::write(&clip, b"mov").unwrap();
        let mut state = crate::state::AppState { shared_library_dir: Some(root.join("library")), shared_collection: String::new(), ..Default::default() };
        let fps = klipp_core::Fps::new(25, 1);
        let mut file = klipp_media::MediaItem::new_synthetic_title(klipp_core::MediaId::new(), "b-roll.mov".into(), klipp_core::Timecode::from_secs_f64(10.0, 25), fps);
        file.kind = klipp_media::MediaItemKind::File;
        file.path = clip.clone();
        let title = klipp_media::MediaItem::new_synthetic_title(klipp_core::MediaId::new(), "Lower third".into(), klipp_core::Timecode::from_secs_f64(10.0, 25), fps);
        let ids = [state.media_pool.add(file), state.media_pool.add(title)];

        assert!(run_command(&mut state, &AppCommand::AddMediaToSharedLibrary { media_ids: ids.to_vec(), collection: String::new() }));
        let on_disk = SharedLibrary::load(&root.join("library"));
        let shared = on_disk.collection(DEFAULT_COLLECTION).expect("the default collection");
        assert_eq!(shared.items, vec![SharedItem { path: clip.clone() }], "the file, not the title");
        assert_eq!(state.shared_library, on_disk);
        assert_eq!(state.shared_collection, DEFAULT_COLLECTION);

        run_command(&mut state, &AppCommand::CreateSharedCollection { name: "Client A".into() });
        assert_eq!(state.shared_collection, "Client A", "the new collection is shown");
        run_command(&mut state, &AppCommand::AddToSharedLibrary { paths: vec![clip.clone()], collection: String::new() });
        assert_eq!(state.shared_library.collection("Client A").unwrap().items.len(), 1, "into the collection shown");
        run_command(&mut state, &AppCommand::RenameSharedCollection { from: "Client A".into(), to: "Client B".into() });
        assert_eq!(state.shared_collection, "Client B");

        run_command(&mut state, &AppCommand::RemoveFromSharedLibrary { collection: DEFAULT_COLLECTION.into(), path: clip.clone() });
        run_command(&mut state, &AppCommand::DeleteSharedCollection { name: "Client B".into() });
        let on_disk = SharedLibrary::load(&root.join("library"));
        assert!(on_disk.collection("Client B").is_none() && on_disk.collection(DEFAULT_COLLECTION).unwrap().items.is_empty());
        assert!(state.media_pool.get(&ids[0]).is_some() && clip.exists(), "the project's media and the file stay");
        assert!(!run_command(&mut state, &AppCommand::Undo), "not one of its commands");
        let _ = std::fs::remove_dir_all(root);
    }
}
