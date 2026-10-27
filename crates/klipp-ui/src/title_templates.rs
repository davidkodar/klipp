//! User title templates: a title styled on the timeline and kept.
//!
//! Each template is one `.klipptitle` file in
//! `~/.config/klipp/title_templates/`: the `TitleData` as JSON plus a
//! name, a category and the Klipp version that wrote it. Fonts stay
//! referenced by name and image layers by path, so a template opens in
//! any project. The Titles browser lists these under User, beside the
//! built-in set.

use std::path::{Path, PathBuf};

use klipp_timeline::TitleData;

/// File extension of a saved template.
pub const EXTENSION: &str = klipp_timeline::TEMPLATE_EXTENSION;

/// One saved template, as written to disk.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserTitleTemplate {
    pub name: String,
    pub category: String,
    /// The Klipp version that wrote the file.
    #[serde(default)]
    pub written_by: String,
    /// The file's format (#2757): one newer than this Klipp reads is left
    /// out with a warning.
    #[serde(default = "format_one")]
    pub format: u32,
    pub data: TitleData,
    /// Where it is from (#2757): the user's own, or an installed pack's,
    /// which is read-only.
    #[serde(skip)]
    pub source: klipp_timeline::template_places::Source,
    /// Bumped each time this session saves the template, so a browser
    /// card never shows the preview of what the name used to hold.
    #[serde(skip)]
    pub revision: u64,
}

impl UserTitleTemplate {
    /// Text a browser card shows when the user has typed none: the
    /// template's own primary text.
    pub fn preview_text(&self) -> String {
        self.data.primary_text().map(|tb| tb.text.clone()).unwrap_or_default()
    }
}

/// The user's templates, loaded from and written to one directory.
#[derive(Debug, Default)]
pub struct TitleTemplateStore {
    dir: Option<PathBuf>,
    /// Sorted by name; each with the file it came from.
    entries: Vec<(PathBuf, UserTitleTemplate)>,
    /// The installed packs' title templates (#2757), read-only, listed
    /// after the user's own.
    packs: Vec<UserTitleTemplate>,
    saves: u64,
}

impl TitleTemplateStore {
    /// The user's own store in Klipp's data folder, with the installed
    /// packs' title templates beside it (#2757).
    pub fn load_default() -> Self {
        use klipp_timeline::template_places::{data_root, ensure_moved, installed, mine_titles, Source};
        ensure_moved();
        let root = data_root();
        let mut store = Self::load(root.as_deref().map(mine_titles));
        for (pack, folder) in root.as_deref().map(installed).unwrap_or_default() {
            for path in std::fs::read_dir(folder.join("titles")).into_iter().flatten().flatten().map(|e| e.path()) {
                if path.extension().and_then(|e| e.to_str()) == Some(EXTENSION) {
                    if let Ok(mut t) = read_template(&path) {
                        t.source = Source::Pack(pack.clone());
                        store.packs.push(t);
                    }
                }
            }
        }
        store.packs.sort_by(|a, b| (a.name.to_lowercase(), &a.source).cmp(&(b.name.to_lowercase(), &b.source)));
        store
    }

    /// Read every `.klipptitle` in `dir`. Files that do not parse are
    /// skipped with a warning rather than failing the whole browser.
    pub fn load(dir: Option<PathBuf>) -> Self {
        let mut entries = Vec::new();
        if let Some(Ok(read)) = dir.as_deref().map(std::fs::read_dir) {
            for path in read.flatten().map(|e| e.path()) {
                if path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
                    continue;
                }
                match read_template(&path) {
                    Ok(t) => entries.push((path, t)),
                    Err(e) => tracing::warn!("Skipping title template {}: {e}", path.display()),
                }
            }
        }
        let mut store = Self { dir, entries, packs: Vec::new(), saves: 0 };
        store.sort();
        store
    }

    /// The user's own, then the installed packs' (#2757): the same name
    /// may be in both, none hiding another.
    pub fn templates(&self) -> impl Iterator<Item = &UserTitleTemplate> {
        self.entries.iter().map(|(_, t)| t).chain(self.packs.iter())
    }

    /// The template called `name`: the user's own before a pack's.
    pub fn get(&self, name: &str) -> Option<&UserTitleTemplate> {
        self.templates().find(|t| t.name == name)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Save `data` as a template. A template of the same name is
    /// replaced in place.
    pub fn save(&mut self, name: &str, category: &str, data: TitleData) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("A template needs a name".into());
        }
        let mut data = data;
        data.migrate_legacy_animation();
        self.saves += 1;
        let template = UserTitleTemplate {
            name: name.to_string(),
            category: category.to_string(),
            written_by: env!("CARGO_PKG_VERSION").to_string(),
            format: klipp_timeline::TEMPLATE_FORMAT,
            data,
            source: klipp_timeline::template_places::Source::Mine,
            revision: self.saves,
        };
        let path = match self.entries.iter().find(|(_, t)| t.name == name) {
            Some((p, _)) => p.clone(),
            None => self.fresh_path(name)?,
        };
        write_template(&path, &template)?;
        self.entries.retain(|(_, t)| t.name != name);
        self.entries.push((path, template));
        self.sort();
        Ok(())
    }

    /// Rename a template. The file keeps its path; only its contents
    /// change, so a rename can never leave two files behind.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let to = to.trim();
        if to.is_empty() {
            return Err("A template needs a name".into());
        }
        if to != from && self.get(to).is_some() {
            return Err(format!("A template called \"{to}\" already exists"));
        }
        let (path, template) = self
            .entries
            .iter_mut()
            .find(|(_, t)| t.name == from)
            .ok_or_else(|| format!("No template called \"{from}\""))?;
        let mut renamed = template.clone();
        renamed.name = to.to_string();
        write_template(path, &renamed)?;
        *template = renamed;
        self.sort();
        Ok(())
    }

    /// Delete a template and its file.
    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        let idx = self
            .entries
            .iter()
            .position(|(_, t)| t.name == name)
            .ok_or_else(|| format!("No template called \"{name}\""))?;
        std::fs::remove_file(&self.entries[idx].0).map_err(|e| e.to_string())?;
        self.entries.remove(idx);
        Ok(())
    }

    fn sort(&mut self) {
        self.entries.sort_by_key(|(_, t)| t.name.to_lowercase());
    }

    /// A file path for a new template that no existing file uses.
    fn fresh_path(&self, name: &str) -> Result<PathBuf, String> {
        let dir = self.dir.as_ref().ok_or("No config directory to save templates in")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let stem: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        (1..)
            .map(|n| {
                let file = if n == 1 { format!("{stem}.{EXTENSION}") } else { format!("{stem}-{n}.{EXTENSION}") };
                dir.join(file)
            })
            .find(|p| !p.exists())
            .ok_or_else(|| "No free file name".into())
    }
}

fn format_one() -> u32 {
    1
}

fn read_template(path: &Path) -> Result<UserTitleTemplate, String> {
    let json = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut t: UserTitleTemplate = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    if t.format > klipp_timeline::TEMPLATE_FORMAT {
        return Err(format!("written by a newer Klipp (format {}, this one reads {})", t.format, klipp_timeline::TEMPLATE_FORMAT));
    }
    t.data.migrate_legacy_animation();
    Ok(t)
}

fn write_template(path: &Path, template: &UserTitleTemplate) -> Result<(), String> {
    let json = serde_json::to_string_pretty(template).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// The save / rename / delete dialog the Titles browser, the title
/// inspector and the timeline open.
#[derive(Debug, Clone)]
pub enum TemplateDialog {
    /// `fields`: what may stay editable once the template is applied
    /// (#1139), each ticked or not.
    Save { data: Box<TitleData>, name: String, category: String, fields: Vec<(klipp_timeline::PublishedField, bool)> },
    Rename { from: String, name: String },
    Delete { name: String },
}

impl TemplateDialog {
    /// Save `data`, a title `clip_secs` long, named after its own text:
    /// its default fields ticked, then the optional ones (#2757), its
    /// Duration ticked and each font and picture not.
    pub fn save(data: TitleData, clip_secs: f32) -> Self {
        let text = data.primary_text().map(|tb| tb.text.trim().to_string()).unwrap_or_default();
        let name = if text.is_empty() || text.chars().count() > 40 { "My Title".to_string() } else { text };
        let mut fields: Vec<_> = data.default_fields().into_iter().map(|f| (f, true)).collect();
        fields.extend(data.optional_fields(clip_secs).into_iter().map(|f| {
            let on = matches!(f.kind, klipp_timeline::FieldKind::Duration { .. });
            (f, on)
        }));
        Self::Save { data: Box::new(data), name, category: "Lower Thirds".to_string(), fields }
    }
}

/// `data` as a template keeps it (#1139): with `fields`, those ticked, as
/// what stays editable once it's applied.
pub fn with_published(mut data: TitleData, fields: &[(klipp_timeline::PublishedField, bool)]) -> TitleData {
    data.published = fields.iter().filter(|(_, on)| *on).map(|(f, _)| f.clone()).collect();
    data
}

/// Draw the open dialog, if any, and apply it on confirm.
pub fn show_dialog(ctx: &egui::Context, state: &mut crate::state::AppState) {
    let Some(dialog) = state.title_template_dialog.as_mut() else { return };
    let title = match dialog {
        TemplateDialog::Save { .. } => "Save as Title Template",
        TemplateDialog::Rename { .. } => "Rename Title Template",
        TemplateDialog::Delete { .. } => "Delete Title Template",
    };
    let mut open = true;
    let mut close = false;
    let mut result: Option<Result<String, String>> = None;
    let store = &mut state.title_templates;
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_min_width(280.0);
            let confirm = match dialog {
                TemplateDialog::Save { data, name, category, fields } => {
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(name).request_focus();
                    });
                    ui.horizontal(|ui| {
                        ui.label("Category:");
                        egui::ComboBox::from_id_salt("title_template_category")
                            .selected_text(category.as_str())
                            .show_ui(ui, |ui| {
                                for cat in crate::browser::titles_tab::CATEGORIES {
                                    ui.selectable_value(category, cat.to_string(), *cat);
                                }
                            });
                    });
                    if store.get(name.trim()).is_some() {
                        ui.label(egui::RichText::new("Replaces the template of this name.").small().weak());
                    }
                    if !fields.is_empty() {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("What stays editable").strong());
                        for (field, on) in fields.iter_mut() {
                            ui.checkbox(on, field.label.as_str());
                        }
                    }
                    ui.add_space(8.0);
                    let go = dialog_buttons(ui, "Save", !name.trim().is_empty(), &mut close);
                    go.then(|| store.save(name, category, with_published((**data).clone(), fields)).map(|_| format!("Saved title template \"{}\"", name.trim())))
                }
                TemplateDialog::Rename { from, name } => {
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(name).request_focus();
                    });
                    ui.add_space(8.0);
                    let go = dialog_buttons(ui, "Rename", !name.trim().is_empty(), &mut close);
                    go.then(|| store.rename(from, name).map(|_| format!("Renamed to \"{}\"", name.trim())))
                }
                TemplateDialog::Delete { name } => {
                    ui.label(format!("Delete the title template \"{name}\"? This removes its file."));
                    ui.add_space(8.0);
                    let go = dialog_buttons(ui, "Delete", true, &mut close);
                    go.then(|| store.delete(name).map(|_| format!("Deleted title template \"{name}\"")))
                }
            };
            result = confirm;
        });
    match result {
        Some(Ok(msg)) => {
            state.toast.show(msg);
            close = true;
        }
        Some(Err(e)) => state.toast.show(format!("Title template not saved: {e}")),
        None => {}
    }
    if close || !open {
        state.title_template_dialog = None;
    }
}

/// Cancel and a confirm button; true when confirm was clicked.
pub(crate) fn dialog_buttons(ui: &mut egui::Ui, confirm: &str, enabled: bool, close: &mut bool) -> bool {
    let mut go = false;
    ui.horizontal(|ui| {
        if ui.button("Cancel").clicked() {
            *close = true;
        }
        go = ui.add_enabled(enabled, egui::Button::new(confirm)).clicked()
            || (enabled && ui.input(|i| i.key_pressed(egui::Key::Enter)));
    });
    go
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(text: &str) -> TitleData {
        TitleData::single_text(klipp_timeline::TextBox { text: text.into(), ..Default::default() })
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-title-templates-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_saved_template_loads_back_unchanged() {
        let dir = scratch();
        let mut store = TitleTemplateStore::load(Some(dir.clone()));
        store.save("Guest", "Lower Thirds", title("Jane Doe")).unwrap();
        let reloaded = TitleTemplateStore::load(Some(dir.clone()));
        let t = reloaded.get("Guest").expect("template reloads");
        assert_eq!(t.category, "Lower Thirds");
        assert_eq!(t.data, title("Jane Doe"));
        assert_eq!(t.written_by, env!("CARGO_PKG_VERSION"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn saving_a_name_again_replaces_it_and_names_that_sanitise_alike_do_not_collide() {
        let dir = scratch();
        let mut store = TitleTemplateStore::load(Some(dir.clone()));
        store.save("Guest", "Lower Thirds", title("one")).unwrap();
        store.save("Guest", "Openers", title("two")).unwrap();
        store.save("Hi?", "Openers", title("a")).unwrap();
        store.save("Hi!", "Openers", title("b")).unwrap();
        let reloaded = TitleTemplateStore::load(Some(dir.clone()));
        assert_eq!(reloaded.len(), 3);
        assert_eq!(reloaded.get("Guest").unwrap().data, title("two"));
        assert_eq!(reloaded.get("Hi?").unwrap().data, title("a"));
        assert_eq!(reloaded.get("Hi!").unwrap().data, title("b"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rename_and_delete_reach_the_disk() {
        let dir = scratch();
        let mut store = TitleTemplateStore::load(Some(dir.clone()));
        store.save("A", "Openers", title("a")).unwrap();
        store.save("B", "Openers", title("b")).unwrap();
        assert!(store.rename("A", "B").is_err(), "a rename onto an existing name is refused");
        store.rename("A", "C").unwrap();
        store.delete("B").unwrap();
        let reloaded = TitleTemplateStore::load(Some(dir.clone()));
        let names: Vec<_> = reloaded.templates().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["C"]);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    /// A template keeps what stays editable (#1139): the fields ticked in
    /// the save dialog, the unticked left out, through its file.
    #[test]
    fn a_template_keeps_its_ticked_fields_through_its_file() {
        let dir = scratch();
        let mut store = TitleTemplateStore::load(Some(dir.clone()));
        let data = title("Jane Doe");
        let TemplateDialog::Save { fields, .. } = TemplateDialog::save(data.clone(), 4.0) else { panic!("a save dialog") };
        assert_eq!(fields.iter().map(|(f, on)| (f.label.as_str(), *on)).collect::<Vec<_>>(), [("Text 1", true), ("Text 1 colour", true), ("Text 1 font", false)], "the defaults ticked, a font offered");
        let mut fields = fields;
        fields[1].1 = false;
        store.save("Guest", "Lower Thirds", with_published(data, &fields)).unwrap();
        let reloaded = TitleTemplateStore::load(Some(dir.clone()));
        let kept = &reloaded.get("Guest").unwrap().data.published;
        assert_eq!(kept.iter().map(|f| f.label.as_str()).collect::<Vec<_>>(), ["Text 1"], "the unticked one left out");
        std::fs::remove_dir_all(dir).ok();
    }

    /// A template the browser saves is one the socket reads (#1139): the
    /// shared reader takes the file as written.
    #[test]
    fn the_socket_reads_what_the_browser_saves() {
        let dir = scratch();
        let mut store = TitleTemplateStore::load(Some(dir.clone()));
        let mut data = title("Jane Doe");
        data.published = data.default_fields();
        store.save("Guest", "Lower Thirds", data.clone()).unwrap();
        let read = klipp_timeline::read_templates(&dir);
        assert_eq!(read.len(), 1);
        assert_eq!((read[0].name.as_str(), read[0].category.as_str()), ("Guest", "Lower Thirds"));
        assert_eq!(read[0].data, data);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_broken_file_is_skipped_not_fatal() {
        let dir = scratch();
        std::fs::write(dir.join(format!("junk.{EXTENSION}")), "{ not json").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let mut store = TitleTemplateStore::load(Some(dir.clone()));
        assert!(store.is_empty());
        store.save("Good", "Openers", title("x")).unwrap();
        assert_eq!(TitleTemplateStore::load(Some(dir.clone())).len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
