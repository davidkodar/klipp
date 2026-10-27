//! Where templates come from (#2757): three sources, each its own place.
//!
//! - **Built-in**: shipped inside Klipp, read-only, updated with releases.
//! - **Mine**: what the user makes or saves, in the data folder.
//! - **Packs**: each installed pack in a folder of its own,
//!   `packs/<safe pack name>/`, holding its templates, recipes, pictures
//!   and fonts and its `pack.json`. Read-only, removed or updated whole,
//!   and treated as untrusted as any file from elsewhere.
//!
//! The same name may be in more than one source: nothing overwrites
//! another source's template.

use std::path::{Path, PathBuf};

/// Which source a template is from.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// Ships with Klipp.
    Builtin,
    /// The user's own.
    #[default]
    Mine,
    /// An installed pack, by the name its folder has.
    Pack(String),
}

impl Source {
    /// As a listing says it: `builtin`, `mine`, `pack:<name>`.
    pub fn name(&self) -> String {
        match self {
            Source::Builtin => "builtin".into(),
            Source::Mine => "mine".into(),
            Source::Pack(p) => format!("pack:{p}"),
        }
    }

    /// From a listing's form, as an op names it.
    pub fn parse(s: &str) -> Option<Source> {
        match s.trim() {
            "builtin" => Some(Source::Builtin),
            "mine" => Some(Source::Mine),
            other => other.strip_prefix("pack:").filter(|p| !p.trim().is_empty()).map(|p| Source::Pack(p.trim().to_string())),
        }
    }

    /// As a browser's badge says it: "Built-in", "Mine", the pack's name.
    pub fn label(&self) -> String {
        match self {
            Source::Builtin => "Built-in".into(),
            Source::Mine => "Mine".into(),
            Source::Pack(p) => p.clone(),
        }
    }

    /// Which wins when a name is asked for without a source: the user's
    /// own, then a pack's, then the shipped one.
    pub fn rank(&self) -> u8 {
        match self {
            Source::Mine => 0,
            Source::Pack(_) => 1,
            Source::Builtin => 2,
        }
    }
}

/// Klipp's data folder: `KLIPP_DATA_DIR` when set (tests), else `klipp` in
/// the user's data folder (`$XDG_DATA_HOME`, `~/.local/share`).
pub fn data_root() -> Option<PathBuf> {
    std::env::var_os("KLIPP_DATA_DIR").map(PathBuf::from).or_else(|| dirs::data_dir().map(|d| d.join("klipp")))
}

/// The user's title templates under `root` (see [`data_root`]).
pub fn mine_titles(root: &Path) -> PathBuf {
    root.join("title_templates")
}

/// The user's composition templates and recipes under `root`.
pub fn mine_templates(root: &Path) -> PathBuf {
    root.join("templates")
}

/// Pictures the user's own templates draw that came with them.
pub fn mine_assets(root: &Path) -> PathBuf {
    root.join("template_assets")
}

/// The installed packs under `root`, a folder each.
pub fn packs(root: &Path) -> PathBuf {
    root.join("packs")
}

/// Each installed pack under `root`: its folder's name and the folder.
pub fn installed(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(packs(root))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
        .collect();
    out.sort();
    out
}

/// What lived in the config folder before #2757, moved once into the data
/// folder: the title templates, composition templates and their pictures,
/// each only when the data folder has none yet. The templates' paths to
/// pictures that moved are made the new ones. Returns what moved.
pub fn move_from_config(config_klipp: &Path, root: &Path) -> Vec<PathBuf> {
    let mut moved = Vec::new();
    for name in ["title_templates", "templates", "template_assets"] {
        let (from, to) = (config_klipp.join(name), root.join(name));
        if !from.is_dir() || to.exists() {
            continue;
        }
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let done = std::fs::rename(&from, &to).is_ok() || (copy_tree(&from, &to).is_ok() && std::fs::remove_dir_all(&from).is_ok());
        if done {
            moved.push(to);
        }
    }
    // Pictures named by their old place in the templates that moved.
    let (old, new) = (config_klipp.join("template_assets"), root.join("template_assets"));
    let (old, new) = (old.to_string_lossy().into_owned(), new.to_string_lossy().into_owned());
    for dir in [mine_titles(root), mine_templates(root)] {
        for file in std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()) {
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            if text.contains(&old) {
                let _ = std::fs::write(&file, text.replace(&old, &new));
            }
        }
    }
    moved
}

/// [`move_from_config`] from the user's config folder into
/// [`data_root`], once per run, before anything reads the templates.
pub fn ensure_moved() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let (Some(config), Some(root)) = (dirs::config_dir(), data_root()) {
            for moved in move_from_config(&config.join("klipp"), &root) {
                tracing::info!("templates moved to {}", moved.display());
            }
        }
    });
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source reads back from its listing form; the user's own wins a
    /// name, then a pack's, then the shipped one.
    #[test]
    fn sources_name_and_rank() {
        for s in [Source::Builtin, Source::Mine, Source::Pack("Bold Cards".into())] {
            assert_eq!(Source::parse(&s.name()), Some(s.clone()));
        }
        assert_eq!(Source::parse("pack:"), None);
        assert!(Source::Mine.rank() < Source::Pack("x".into()).rank() && Source::Pack("x".into()).rank() < Source::Builtin.rank());
    }

    /// The config folder's templates move once into the data folder, their
    /// pictures' paths with them; a second run moves nothing.
    #[test]
    fn templates_move_once_from_config_to_data() {
        let root = std::env::temp_dir().join(format!("klipp-places-{}", uuid::Uuid::new_v4()));
        let (config, data) = (root.join("config/klipp"), root.join("data/klipp"));
        std::fs::create_dir_all(config.join("templates")).unwrap();
        std::fs::create_dir_all(config.join("template_assets/pack")).unwrap();
        let old_pic = config.join("template_assets/pack/1-photo.png");
        std::fs::write(&old_pic, b"PNG").unwrap();
        std::fs::write(config.join("templates/card.klipptemplate"), format!("{{\"picture\": \"{}\"}}", old_pic.display())).unwrap();
        let moved = move_from_config(&config, &data);
        assert_eq!(moved.len(), 2, "{moved:?}");
        assert!(!config.join("templates").exists());
        let text = std::fs::read_to_string(data.join("templates/card.klipptemplate")).unwrap();
        assert!(text.contains(&data.join("template_assets/pack/1-photo.png").display().to_string()), "{text}");
        assert_eq!(std::fs::read(data.join("template_assets/pack/1-photo.png")).unwrap(), b"PNG");
        assert!(move_from_config(&config, &data).is_empty(), "once");
        let _ = std::fs::remove_dir_all(&root);
    }
}
