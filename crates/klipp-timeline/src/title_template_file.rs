//! A saved title template as its file holds it (#1139): what the Titles
//! browser writes, one `.klipptitle` file each in the user's template
//! folder, read here too by what can't reach the browser's store (the
//! control socket and the assistant, through it).

use crate::TitleData;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// File extension of a saved title template.
pub const TEMPLATE_EXTENSION: &str = "klipptitle";

/// One saved title template: its name, its category, the title.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TemplateFile {
    pub name: String,
    pub category: String,
    pub data: TitleData,
}

/// The user's template folder under `config` (the config directory).
pub fn template_dir(config: &Path) -> PathBuf {
    config.join("klipp").join("title_templates")
}

/// Every template in `dir`, by name; a file that doesn't read is left out.
pub fn read_templates(dir: &Path) -> Vec<TemplateFile> {
    let mut out: Vec<TemplateFile> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(TEMPLATE_EXTENSION))
        .filter_map(|p| {
            let mut t: TemplateFile = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
            t.data.migrate_legacy_animation();
            Some(t)
        })
        .collect();
    out.sort_by_key(|t| t.name.to_lowercase());
    out
}
