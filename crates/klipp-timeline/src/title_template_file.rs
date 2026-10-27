//! A saved title template as its file holds it (#1139): what the Titles
//! browser writes, one `.klipptitle` file each in the user's template
//! folder, read here too by what can't reach the browser's store (the
//! control socket and the assistant, through it).

use crate::TitleData;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File extension of a saved title template.
pub const TEMPLATE_EXTENSION: &str = "klipptitle";

/// The template file's format (#2757): a file of a newer format was
/// written by a newer Klipp and is left out, not guessed at. A file
/// without one is format 1.
pub const TEMPLATE_FORMAT: u32 = 1;

fn format_one() -> u32 {
    1
}

/// One saved title template: its name, its category, the title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateFile {
    pub name: String,
    pub category: String,
    pub data: TitleData,
    #[serde(default = "format_one")]
    pub format: u32,
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
            if t.format > TEMPLATE_FORMAT {
                return None;
            }
            t.data.migrate_legacy_animation();
            Some(t)
        })
        .collect();
    out.sort_by_key(|t| t.name.to_lowercase());
    out
}

/// `data` saved as the template `name` in `dir` (#2757), the file the
/// Titles browser writes: a template of that name is replaced when
/// `replace`, else refused. Returns the file.
pub fn save_template(dir: &Path, name: &str, category: &str, data: &TitleData, replace: bool) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a template needs a name".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let named = |p: &Path| -> Option<String> {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
        v.get("name")?.as_str().map(str::to_string)
    };
    let files: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|e| e.to_str()) == Some(TEMPLATE_EXTENSION)).collect();
    let path = match files.iter().find(|p| named(p).as_deref() == Some(name)) {
        Some(_) if !replace => return Err(format!("a template called \"{name}\" is saved already")),
        Some(p) => p.clone(),
        None => {
            let stem: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
            (1..).map(|n| if n == 1 { dir.join(format!("{stem}.{TEMPLATE_EXTENSION}")) } else { dir.join(format!("{stem}-{n}.{TEMPLATE_EXTENSION}")) }).find(|p| !p.exists()).expect("a free file name")
        }
    };
    let json = serde_json::json!({
        "name": name,
        "category": category,
        "written_by": env!("CARGO_PKG_VERSION"),
        "format": TEMPLATE_FORMAT,
        "data": data,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(path)
}

/// The title of `clip_secs` saved as a template keeps (#2757): the fields
/// named, by label in any case, from its defaults and the optional ones
/// (fonts, pictures, Duration); none named, its defaults and Duration.
/// An unknown label is refused with the ones there are.
pub fn template_for_save(data: &TitleData, clip_secs: f32, fields: Option<&[String]>) -> Result<TitleData, String> {
    let mut all = data.default_fields();
    all.extend(data.optional_fields(clip_secs));
    let chosen = match fields {
        None => all.iter().filter(|f| !matches!(f.kind, crate::FieldKind::Font { .. } | crate::FieldKind::Picture { .. })).cloned().collect(),
        Some(names) => {
            let mut out = Vec::new();
            for n in names {
                let f = all.iter().find(|f| f.label.eq_ignore_ascii_case(n.trim())).ok_or_else(|| {
                    format!("the title has no field \"{n}\": its fields are {}", all.iter().map(|f| f.label.as_str()).collect::<Vec<_>>().join(", "))
                })?;
                out.push(f.clone());
            }
            out
        }
    };
    let mut td = data.clone();
    td.published = chosen;
    td.template = None;
    Ok(td)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TextBox, TitleData};
    use klipp_core::title_motion::{Motion, MotionKind};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-templates-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn rising(text: &str) -> TitleData {
        let mut td = TitleData::single_text(TextBox { text: text.into(), ..Default::default() });
        td.objects[0].fx_mut().motion.enter = Some(Motion::new(MotionKind::Rise).duration(0.6));
        td
    }

    /// #2757: a title saved as a template reads back as it was, its
    /// fields those named (defaults and Duration when none are); a name
    /// taken is refused without replace; a newer format is left out.
    #[test]
    fn a_saved_template_reads_back_as_it_was() {
        let dir = scratch("save");
        let td = rising("Opening night");
        let data = template_for_save(&td, 4.0, None).unwrap();
        let labels: Vec<&str> = data.published.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, ["Text 1", "Text 1 colour", "Speed", "Duration"]);
        save_template(&dir, "Opener", "My Templates", &data, false).unwrap();
        let read = read_templates(&dir);
        assert_eq!(read.len(), 1);
        assert_eq!((read[0].name.as_str(), read[0].category.as_str(), read[0].format), ("Opener", "My Templates", TEMPLATE_FORMAT));
        assert_eq!(read[0].data.objects, td.objects, "the title as it was");
        assert!(save_template(&dir, "Opener", "x", &data, false).unwrap_err().contains("saved already"));
        save_template(&dir, "Opener", "Openers", &data, true).unwrap();
        assert_eq!(read_templates(&dir)[0].category, "Openers", "replaced in place");
        assert_eq!(read_templates(&dir).len(), 1);

        let picked = template_for_save(&td, 4.0, Some(&["text 1".into(), "Text 1 font".into()])).unwrap();
        assert_eq!(picked.published.iter().map(|f| f.label.as_str()).collect::<Vec<_>>(), ["Text 1", "Text 1 font"]);
        assert!(template_for_save(&td, 4.0, Some(&["Logo".into()])).unwrap_err().contains("its fields are Text 1"));

        std::fs::write(dir.join(format!("future.{TEMPLATE_EXTENSION}")), r#"{"name":"Future","category":"x","format":99,"data":{"objects":[],"next_object_id":0}}"#).unwrap();
        assert!(read_templates(&dir).iter().all(|t| t.name != "Future"), "a newer format is left out");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
