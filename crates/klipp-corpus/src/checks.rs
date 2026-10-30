//! Saved-project checks. Inspector interaction belongs to the separate app probe.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

#[derive(Debug, PartialEq)]
enum Entry {
    Directory,
    File {
        size: u64,
        modified: SystemTime,
        sha256: String,
    },
}

fn tree(root: &Path) -> Result<BTreeMap<PathBuf, Entry>, String> {
    fn visit(
        root: &Path,
        directory: &Path,
        out: &mut BTreeMap<PathBuf, Entry>,
    ) -> Result<(), String> {
        for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            let relative = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_owned();
            if metadata.is_dir() {
                out.insert(relative, Entry::Directory);
                visit(root, &path, out)?;
            } else if metadata.is_file() {
                out.insert(
                    relative,
                    Entry::File {
                        size: metadata.len(),
                        modified: metadata.modified().map_err(|e| e.to_string())?,
                        sha256: crate::sha256(&std::fs::read(&path).map_err(|e| e.to_string())?),
                    },
                );
            } else {
                return Err(format!(
                    "{}: project check refuses links and special files",
                    path.display()
                ));
            }
        }
        Ok(())
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries)?;
    Ok(entries)
}

fn unchanged(
    before: &BTreeMap<PathBuf, Entry>,
    after: &BTreeMap<PathBuf, Entry>,
) -> Result<(), String> {
    for path in before.keys().chain(after.keys()) {
        if before.get(path) != after.get(path) {
            return Err(format!(
                "open wrote {} (contents, size, mtime or file set changed)",
                path.display()
            ));
        }
    }
    Ok(())
}

fn open_with(
    directory: &Path,
    after_load: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let before = tree(directory)?;
    let loaded = klipp_project::load_project_strict(directory);
    let hook = after_load();
    unchanged(&before, &tree(directory)?)?;
    loaded?;
    hook
}

pub fn open(directory: &Path) -> Result<(), String> {
    open_with(directory, || Ok(()))
}

fn json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Only save-owned stamps are ignored. Created-at, active sequence, authored
/// values, and unknown future fields all remain part of the comparison.
fn document(directory: &Path, name: &str) -> Result<Value, String> {
    fn paths(value: &mut Value, directory: &Path) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if matches!(
                        key.as_str(),
                        "path" | "proxy_path" | "global_lut_path" | "texture_fill_path" | "File"
                    ) {
                        if let Value::String(s) = value {
                            let scheme = s
                                .split_once(':')
                                .is_some_and(|(head, _)| !head.contains('/'));
                            if !s.is_empty() && !scheme {
                                *s = klipp_project::path_util::resolve_media_path(
                                    directory,
                                    Path::new(s),
                                )
                                .to_string_lossy()
                                .into_owned();
                            }
                        }
                    } else {
                        paths(value, directory);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    paths(item, directory);
                }
            }
            _ => {}
        }
    }
    let mut value = json(&directory.join(name))?;
    let map = value
        .as_object_mut()
        .ok_or("project document is not an object")?;
    for stamp in ["generation", "saved_at", "klipp_version", "platform"] {
        map.remove(stamp);
    }
    if name == "media/manifest.json" {
        let items = map
            .get_mut("items")
            .and_then(Value::as_array_mut)
            .ok_or("manifest items missing")?;
        items.sort_by_key(|item| item["id"].as_str().unwrap_or("").to_owned());
        // A save writes where a file outside the project is, from its
        // relative path (#2800): a stamp of that, never a value of its own,
        // but it must name the same file (#3175).
        for item in items.iter_mut() {
            // The disk cache key a save takes from the file is this
            // machine's, a stamp as well.
            if let Some(m) = item.as_object_mut() {
                m.remove("cache_key");
            }
            let Some(at) = item.as_object_mut().and_then(|m| m.remove("absolute_path")) else { continue };
            let relative = item["path"].as_str().unwrap_or("");
            let named = klipp_project::path_util::resolve_media_path(directory, Path::new(relative));
            if at.as_str().map(Path::new) != Some(named.as_path()) {
                return Err(format!("{name}: an absolute path {at} that is not {}", named.display()));
            }
        }
    }
    paths(&mut value, directory);
    Ok(value)
}

fn difference(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for key in a.keys().chain(b.keys()) {
                let child = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                match (a.get(key), b.get(key)) {
                    (Some(a), Some(b)) => {
                        if let Some(p) = difference(a, b, &child) {
                            return Some(p);
                        }
                    }
                    _ => return Some(child),
                }
            }
            None
        }
        (Value::Array(a), Value::Array(b)) => {
            for index in 0..a.len().max(b.len()) {
                let child = format!("{path}/{index}");
                match (a.get(index), b.get(index)) {
                    (Some(a), Some(b)) => {
                        if let Some(p) = difference(a, b, &child) {
                            return Some(p);
                        }
                    }
                    _ => return Some(child),
                }
            }
            None
        }
        _ => (a != b).then(|| path.to_owned()),
    }
}

fn same_project(a: &Path, b: &Path) -> Result<(), String> {
    for name in ["project.json", "media/manifest.json"] {
        if let Some(path) = difference(&document(a, name)?, &document(b, name)?, "") {
            return Err(format!("reopen changed {name}{path}"));
        }
    }
    Ok(())
}

fn save_copy(from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() {
        return Err(format!("{} already exists", to.display()));
    }
    let (sequences, pool, colour, active, created, notes, zoom, scroll, scale, titles) =
        klipp_project::load_project_strict(from)?;
    klipp_project::save_project_full(
        to,
        &sequences,
        &pool,
        &colour,
        active.as_deref(),
        created.as_deref(),
        &notes,
        0,
        zoom,
        scroll,
        scale,
        &titles,
    )
}

fn reopen_with(
    directory: &Path,
    out: &Path,
    after_save: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let before = tree(directory)?;
    let first = out.join("first.klipp");
    let second = out.join("second.klipp");
    save_copy(directory, &first)?;
    after_save(&first)?;
    same_project(directory, &first)?;
    save_copy(&first, &second)?;
    same_project(&first, &second)?;
    unchanged(&before, &tree(directory)?)
}

pub fn reopen(directory: &Path, out: &Path) -> Result<(), String> {
    reopen_with(directory, out, |_| Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generated() -> PathBuf {
        let root = crate::run_directory(&std::env::temp_dir().join("klipp-corpus-checks")).unwrap();
        crate::media::generate(&root.join("media")).unwrap();
        crate::projects::generate(&root).unwrap();
        root
    }

    #[test]
    fn opening_and_two_saves_preserve_every_case() {
        let root = generated();
        for case in crate::projects::CASES {
            let directory = root.join("projects").join(format!("{case}.klipp"));
            open(&directory).unwrap();
            reopen(&directory, &root.join("reopen").join(case)).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_write_on_open_is_named() {
        let root = generated();
        let directory = root.join("projects/edit.klipp");
        let error = open_with(&directory, || {
            let path = directory.join("project.json");
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.push(b' ');
            std::fs::write(path, bytes).map_err(|e| e.to_string())
        })
        .unwrap_err();
        assert!(error.contains("open wrote project.json"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// #3175: the absolute path a save writes beside a file's relative one
    /// is a stamp of it, left out of the comparison, but one naming another
    /// file is named.
    #[test]
    fn an_absolute_path_naming_another_file_is_named() {
        let root = generated();
        let error = reopen_with(
            &root.join("projects/edit.klipp"),
            &root.join("reopen"),
            |copy| {
                let path = copy.join("media/manifest.json");
                let mut value = json(&path)?;
                for item in value["items"].as_array_mut().unwrap() {
                    item["absolute_path"] = Value::String("/elsewhere/motion.mkv".into());
                }
                std::fs::write(path, serde_json::to_vec(&value).unwrap()).map_err(|e| e.to_string())
            },
        )
        .unwrap_err();
        assert!(error.contains("/elsewhere/motion.mkv"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_field_lost_on_save_is_named() {
        let root = generated();
        let error = reopen_with(
            &root.join("projects/effects.klipp"),
            &root.join("reopen"),
            |copy| {
                let path = copy.join("project.json");
                let mut value = json(&path)?;
                value.as_object_mut().unwrap().remove("notes");
                std::fs::write(path, serde_json::to_vec(&value).unwrap()).map_err(|e| e.to_string())
            },
        )
        .unwrap_err();
        assert!(error.contains("project.json/notes"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
