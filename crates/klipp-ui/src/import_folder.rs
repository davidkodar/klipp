//! Where the Import Media picker opens (#2964): the folder the last
//! import came from, else `~/Videos`, else home.
//!
//! Stored as a tiny TOML file at `~/.config/klipp/import_folder.toml`:
//!
//! ```toml
//! folder = "/home/me/Videos/Trip"
//! ```
//!
//! The file is read as untrusted: a size cap, and a folder used only when
//! it is an absolute path to a folder that exists now. Anything else, or a
//! missing file, falls back to the default; nothing here stops an import.

use std::path::{Path, PathBuf};

const FILE_NAME: &str = "import_folder.toml";

/// More than any path fits in; a larger file is not ours.
const MAX_BYTES: u64 = 8 * 1024;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct OnDisk {
    #[serde(default)]
    folder: Option<PathBuf>,
}

fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("klipp").join(FILE_NAME))
}

/// The folder the picker opens in.
pub fn start_folder() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    start_folder_in(config_path().as_deref(), &home)
}

/// [`start_folder`] with the file and the home folder given.
pub fn start_folder_in(file: Option<&Path>, home: &Path) -> PathBuf {
    file.and_then(read)
        .filter(|dir| dir.is_absolute() && dir.is_dir())
        .or_else(|| Some(home.join("Videos")).filter(|d| d.is_dir()))
        .unwrap_or_else(|| home.to_path_buf())
}

fn read(file: &Path) -> Option<PathBuf> {
    if std::fs::metadata(file).ok()?.len() > MAX_BYTES {
        return None;
    }
    toml::from_str::<OnDisk>(&std::fs::read_to_string(file).ok()?).ok()?.folder
}

/// Remember the folder `picked` came from, for the next import.
pub fn remember(picked: &[PathBuf]) {
    if let Some(file) = config_path() {
        remember_in(&file, picked);
    }
}

/// [`remember`] into `file`. A failure is logged, never shown: the next
/// picker opens in the default instead.
pub fn remember_in(file: &Path, picked: &[PathBuf]) {
    let Some(folder) = picked.first().and_then(|p| p.parent()).filter(|d| d.is_absolute()) else { return };
    let Ok(text) = toml::to_string(&OnDisk { folder: Some(folder.to_path_buf()) }) else { return };
    let written = file.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(file, text));
    if let Err(e) = written {
        tracing::warn!("could not remember the import folder in {}: {e}", file.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder of the test's own under the temp dir.
    fn scratch(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("klipp-import-folder-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// The last import's folder is where the next picker opens; with none
    /// remembered, or one since removed, `~/Videos`; without that, home.
    #[test]
    fn the_picker_opens_where_the_last_import_came_from() {
        let root = scratch("last");
        let home = root.join("home");
        let trip = home.join("Footage").join("Trip");
        std::fs::create_dir_all(&trip).unwrap();
        let file = root.join("config").join(FILE_NAME);

        assert_eq!(start_folder_in(Some(&file), &home), home, "nothing remembered, no Videos: home");
        std::fs::create_dir_all(home.join("Videos")).unwrap();
        assert_eq!(start_folder_in(Some(&file), &home), home.join("Videos"));

        remember_in(&file, &[trip.join("a.mov"), trip.join("b.wav")]);
        assert_eq!(start_folder_in(Some(&file), &home), trip, "the last import's folder");
        std::fs::remove_dir_all(&trip).unwrap();
        assert_eq!(start_folder_in(Some(&file), &home), home.join("Videos"), "a folder since removed is passed over");
    }

    /// The file is read as untrusted: a relative path, a path to a file, a
    /// malformed file and one past the size cap all fall back.
    #[test]
    fn a_hostile_file_falls_back_to_the_default() {
        let root = scratch("hostile");
        let home = root.join("home");
        std::fs::create_dir_all(home.join("Videos")).unwrap();
        let file = root.join(FILE_NAME);
        let videos = home.join("Videos");
        let not_a_folder = root.join("clip.mov");
        std::fs::write(&not_a_folder, b"").unwrap();
        let huge = format!("folder = \"{}\"\n# {}", home.display(), "x".repeat(MAX_BYTES as usize));
        for text in ["folder = \"Videos/../..\"".to_string(), format!("folder = \"{}\"", not_a_folder.display()), "folder = [1, 2]".to_string(), "\u{0}\u{1}".to_string(), huge] {
            std::fs::write(&file, &text).unwrap();
            assert_eq!(start_folder_in(Some(&file), &home), videos, "{:.40}", text);
        }
    }
}
