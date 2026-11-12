//! Where the Import Media picker and the Files tab open (#2964, #2580):
//! for each project, the folder its last import came from and the folder
//! its Files tab was last in; else the folder the project is in; for a
//! project never saved, `~/Videos`; without that, home.
//!
//! Stored as a small TOML file at `~/.config/klipp/import_folder.toml`,
//! the projects by their paths, the latest first:
//!
//! ```toml
//! folder = "/home/me/Videos/Trip"   # a project never saved: its import
//! files = "/home/me/Videos"         # and its Files tab
//!
//! [[projects]]
//! project = "/home/me/Films/Trip.klipp"
//! import = "/home/me/Footage/Day 1"
//! files = "/home/me/Footage"
//! ```
//!
//! The file is read as untrusted: a size cap, and a folder used only when
//! it is an absolute path to a folder that exists now. Anything else, or a
//! missing file, falls back to the default; nothing here stops an import.
//! It is written whole to a file beside it and moved into place, so two
//! Klipps writing at once leave one of them, never a mix.

use std::path::{Path, PathBuf};

const FILE_NAME: &str = "import_folder.toml";

/// More than fifty projects' paths fit in; a larger file is not ours.
const MAX_BYTES: u64 = 64 * 1024;

/// The projects remembered, the latest kept.
const MAX_PROJECTS: usize = 50;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct OnDisk {
    /// A project never saved: the folder its last import came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<PathBuf>,
    /// A project never saved: the folder its Files tab was last in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    projects: Vec<ProjectFolders>,
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct ProjectFolders {
    project: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    import: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files: Option<PathBuf>,
}

/// Which of a project's two folders.
#[derive(Clone, Copy)]
enum Kind {
    Import,
    Files,
}

fn config_path() -> Option<PathBuf> {
    // A test never reads or writes the person's own.
    if cfg!(test) {
        return None;
    }
    Some(dirs::config_dir()?.join("klipp").join(FILE_NAME))
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// The project the picker and the Files tab are for, as the app last
/// said: `None` for one never saved.
static PROJECT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Say which project is open; `None` for one never saved.
pub fn set_project(project: Option<&Path>) {
    *PROJECT.lock().unwrap_or_else(|e| e.into_inner()) = project.map(Path::to_path_buf);
}

fn current_project() -> Option<PathBuf> {
    PROJECT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The folder the picker opens in, for the project open.
pub fn start_folder() -> PathBuf {
    start_folder_in(config_path().as_deref(), &home(), current_project().as_deref())
}

/// The folder the Files tab opens in for `project`.
pub fn files_folder(project: Option<&Path>) -> PathBuf {
    files_folder_in(config_path().as_deref(), &home(), project)
}

/// [`start_folder`] with the file, the home folder and the project given.
pub fn start_folder_in(file: Option<&Path>, home: &Path, project: Option<&Path>) -> PathBuf {
    folder_in(file, home, project, Kind::Import)
}

/// [`files_folder`] with the file and the home folder given.
pub fn files_folder_in(file: Option<&Path>, home: &Path, project: Option<&Path>) -> PathBuf {
    folder_in(file, home, project, Kind::Files)
}

fn folder_in(file: Option<&Path>, home: &Path, project: Option<&Path>, kind: Kind) -> PathBuf {
    let disk = file.map(read).unwrap_or_default();
    let remembered = match project {
        None => match kind {
            Kind::Import => disk.folder,
            Kind::Files => disk.files,
        },
        Some(p) => {
            let key = key(p);
            disk.projects.into_iter().find(|e| e.project == key).and_then(|e| match kind {
                Kind::Import => e.import,
                Kind::Files => e.files,
            })
        }
    };
    remembered
        .filter(|dir| dir.is_absolute() && dir.is_dir())
        .or_else(|| project.and_then(Path::parent).filter(|d| d.is_absolute() && d.is_dir()).map(Path::to_path_buf))
        .or_else(|| Some(home.join("Videos")).filter(|d| d.is_dir()))
        .unwrap_or_else(|| home.to_path_buf())
}

/// A project as it is kept: its path made canonical, so one project
/// reached two ways is one entry.
fn key(project: &Path) -> PathBuf {
    std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf())
}

fn read(file: &Path) -> OnDisk {
    let read = || -> Option<OnDisk> {
        if std::fs::metadata(file).ok()?.len() > MAX_BYTES {
            return None;
        }
        toml::from_str(&std::fs::read_to_string(file).ok()?).ok()
    };
    read().unwrap_or_default()
}

/// Remember the folder `picked` came from, for the next import into the
/// project open.
pub fn remember(picked: &[PathBuf]) {
    if let Some(file) = config_path() {
        remember_in(&file, current_project().as_deref(), picked);
    }
}

/// Remember `folder` as where `project`'s Files tab is.
pub fn remember_files(project: Option<&Path>, folder: &Path) {
    if let Some(file) = config_path() {
        remember_files_in(&file, project, folder);
    }
}

/// [`remember`] into `file`, for `project`. A failure is logged, never
/// shown: the next picker opens in the default instead.
pub fn remember_in(file: &Path, project: Option<&Path>, picked: &[PathBuf]) {
    if let Some(folder) = picked.first().and_then(|p| p.parent()) {
        keep(file, project, folder, Kind::Import);
    }
}

/// [`remember_files`] into `file`.
pub fn remember_files_in(file: &Path, project: Option<&Path>, folder: &Path) {
    keep(file, project, folder, Kind::Files);
}

fn keep(file: &Path, project: Option<&Path>, folder: &Path, kind: Kind) {
    if !folder.is_absolute() {
        return;
    }
    let mut disk = read(file);
    match project {
        None => match kind {
            Kind::Import => disk.folder = Some(folder.to_path_buf()),
            Kind::Files => disk.files = Some(folder.to_path_buf()),
        },
        Some(p) => {
            let key = key(p);
            let mut entry = disk.projects.iter().position(|e| e.project == key).map(|i| disk.projects.remove(i)).unwrap_or(ProjectFolders { project: key, ..Default::default() });
            match kind {
                Kind::Import => entry.import = Some(folder.to_path_buf()),
                Kind::Files => entry.files = Some(folder.to_path_buf()),
            }
            disk.projects.insert(0, entry);
            disk.projects.truncate(MAX_PROJECTS);
        }
    }
    if let Err(e) = write(file, &disk) {
        tracing::warn!("could not remember a start folder in {}: {e}", file.display());
    }
}

/// `disk` written to a file beside `file`, then moved over it.
fn write(file: &Path, disk: &OnDisk) -> std::io::Result<()> {
    let text = toml::to_string(disk).map_err(std::io::Error::other)?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = file.with_extension(format!("toml.{}.part", std::process::id()));
    std::fs::write(&part, text)?;
    std::fs::rename(&part, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })
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

        assert_eq!(start_folder_in(Some(&file), &home, None), home, "nothing remembered, no Videos: home");
        std::fs::create_dir_all(home.join("Videos")).unwrap();
        assert_eq!(start_folder_in(Some(&file), &home, None), home.join("Videos"));

        remember_in(&file, None, &[trip.join("a.mov"), trip.join("b.wav")]);
        assert_eq!(start_folder_in(Some(&file), &home, None), trip, "the last import's folder");
        std::fs::remove_dir_all(&trip).unwrap();
        assert_eq!(start_folder_in(Some(&file), &home, None), home.join("Videos"), "a folder since removed is passed over");
    }

    /// #2580: two projects that last imported from different folders each
    /// open the picker in their own, and their Files tabs in theirs; a
    /// project with none yet opens in the folder it is in; one remembered
    /// folder since removed falls back to that.
    #[test]
    fn each_project_opens_in_its_own_folders() {
        let root = scratch("projects");
        let home = root.join("home");
        let (films, a, b) = (root.join("Films"), root.join("Footage A"), root.join("Footage B"));
        for d in [&films, &a, &b, &home.join("Videos")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (one, two) = (films.join("One.klipp"), films.join("Two.klipp"));
        std::fs::create_dir_all(&one).unwrap();
        let file = root.join("config").join(FILE_NAME);

        assert_eq!(start_folder_in(Some(&file), &home, Some(&one)), films, "a project with none: the folder it is in");
        assert_eq!(files_folder_in(Some(&file), &home, Some(&one)), films);
        remember_in(&file, Some(&one), &[a.join("shot.mov")]);
        remember_in(&file, Some(&two), &[b.join("shot.mov")]);
        remember_files_in(&file, Some(&one), &b);
        assert_eq!(start_folder_in(Some(&file), &home, Some(&one)), a);
        assert_eq!(start_folder_in(Some(&file), &home, Some(&two)), b);
        assert_eq!(files_folder_in(Some(&file), &home, Some(&one)), b, "its Files tab, apart from its imports");
        assert_eq!(files_folder_in(Some(&file), &home, Some(&two)), films);
        assert_eq!(start_folder_in(Some(&file), &home, None), home.join("Videos"), "a project never saved keeps its own");
        // The same project reached another way, through a link to its
        // folder, is the same one.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&films, root.join("Linked")).unwrap();
            assert_eq!(start_folder_in(Some(&file), &home, Some(&root.join("Linked").join("One.klipp"))), a);
        }

        std::fs::remove_dir_all(&a).unwrap();
        assert_eq!(start_folder_in(Some(&file), &home, Some(&one)), films, "a folder since removed: the default");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains(".part") && std::fs::read_dir(root.join("config")).unwrap().count() == 1, "written whole, nothing left beside it");
    }

    /// No more projects are kept than the cap, the latest first.
    #[test]
    fn the_latest_projects_are_kept() {
        let root = scratch("cap");
        let file = root.join(FILE_NAME);
        for i in 0..MAX_PROJECTS + 5 {
            remember_files_in(&file, Some(&root.join(format!("p{i}.klipp"))), &root);
        }
        let disk = read(&file);
        assert_eq!(disk.projects.len(), MAX_PROJECTS);
        assert_eq!(disk.projects[0].project, root.join(format!("p{}.klipp", MAX_PROJECTS + 4)), "the latest first");
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
        let project = root.join("P.klipp");
        let relative = format!("[[projects]]\nproject = \"{}\"\nimport = \"Footage\"", project.display());
        for text in ["folder = \"Videos/../..\"".to_string(), format!("folder = \"{}\"", not_a_folder.display()), "folder = [1, 2]".to_string(), "\u{0}\u{1}".to_string(), huge] {
            std::fs::write(&file, &text).unwrap();
            assert_eq!(start_folder_in(Some(&file), &home, None), videos, "{:.40}", text);
        }
        std::fs::write(&file, relative).unwrap();
        assert_eq!(start_folder_in(Some(&file), &home, Some(&project)), root, "a project's relative folder: the default");
    }
}
