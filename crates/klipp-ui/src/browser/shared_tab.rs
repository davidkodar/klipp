//! The Library's SHARED view (#614): the Shared Library's collections of
//! files, the same in every project. A file is brought in with Add to
//! Project or Add to Timeline, or dragged onto the pool or a track; one
//! moved or deleted shows Missing, with Relink. What is here is a
//! reference to a file, not a pool item: the pool item is made when the
//! file is brought in, and a file already in the pool is used from there.
//!
//! Above the collection, what else every project shares: saved title
//! templates (each one whose font or picture isn't here says so), grades
//! and tool presets.

use std::collections::HashMap;
use std::path::PathBuf;

use klipp_core::events::{AppCommand, ClipDrop};

use crate::shared_library::DEFAULT_COLLECTION;
use crate::state::AppState;
use crate::theme::KlippTheme;

/// How long a file's being there is trusted before it is looked at again.
const LOOK_AGAIN_SECS: f64 = 2.0;

/// What else every project shares, as last looked at.
#[derive(Default, Clone, PartialEq, Debug)]
pub struct Overview {
    pub title_templates: usize,
    /// Each title template a font or a picture of is missing from, and what.
    pub title_warnings: Vec<(String, Vec<String>)>,
    /// The tool presets, by tool, for the tools that have any.
    pub tool_presets: Vec<(&'static str, usize)>,
}

/// What `template` uses that isn't here: a font `installed` doesn't
/// know, a picture whose file is gone.
pub fn template_warnings(template: &crate::title_templates::UserTitleTemplate, installed: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = crate::missing_fonts::fonts_of(&template.data)
        .into_iter()
        .filter(|f| !f.is_empty() && !installed(f))
        .map(|f| format!("the font {f} isn't installed"))
        .collect();
    for o in &template.data.objects {
        if let klipp_timeline::TitleObjectContent::Image(image) = &o.content {
            if !image.path.is_empty() && !std::path::Path::new(&image.path).exists() {
                out.push(format!("the picture {} is missing", image.path));
            }
        }
    }
    out
}

/// The view's own state.
pub struct SharedTabState {
    pub search: String,
    /// Whether each file was there when last looked, so a long collection
    /// isn't stat'ed every frame.
    present: HashMap<PathBuf, bool>,
    looked_at: Option<f64>,
    /// A collection name being typed: `None` for a new collection, the
    /// collection's name for a rename.
    pub naming: Option<(Option<String>, String)>,
    overview: Overview,
    /// Whether a font family is here: the title renderer's, but for tests.
    font_installed: fn(&str) -> bool,
}

impl Default for SharedTabState {
    fn default() -> Self {
        Self { search: String::new(), present: HashMap::new(), looked_at: None, naming: None, overview: Overview::default(), font_installed: crate::missing_fonts::installed }
    }
}

impl SharedTabState {
    /// Whether `path` was there when last looked.
    fn is_present(&self, path: &std::path::Path) -> bool {
        self.present.get(path).copied().unwrap_or(true)
    }
}

/// The collection the view shows: the one picked, or the first.
pub fn shown_collection(state: &AppState) -> String {
    let lib = &state.shared_library;
    lib.collection(&state.shared_collection)
        .or_else(|| lib.collections.first())
        .map(|c| c.name.clone())
        .unwrap_or_else(|| DEFAULT_COLLECTION.to_string())
}

/// `category` is the Library's; the overview's Titles opens that view.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, tab: &mut SharedTabState, category: &mut crate::layout_state::LibraryCategory) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let current = shown_collection(state);
    let names: Vec<String> = state.shared_library.collections.iter().map(|c| c.name.clone()).collect();

    // Files are looked at again now and then, and whenever the library
    // holds one not looked at yet.
    let now = ui.input(|i| i.time);
    let items: Vec<PathBuf> = state.shared_library.collection(&current).map(|c| c.items.iter().map(|i| i.path.clone()).collect()).unwrap_or_default();
    if tab.looked_at.is_none_or(|t| now - t > LOOK_AGAIN_SECS) || items.iter().any(|p| !tab.present.contains_key(p)) {
        tab.present = items.iter().map(|p| (p.clone(), p.exists())).collect();
        tab.looked_at = Some(now);
        tab.overview = Overview {
            title_templates: state.title_templates.len(),
            title_warnings: state
                .title_templates
                .templates()
                .map(|t| (t.name.clone(), template_warnings(t, &tab.font_installed)))
                .filter(|(_, w)| !w.is_empty())
                .collect(),
            tool_presets: [
                crate::presets::ToolScope::Wheels,
                crate::presets::ToolScope::Curves,
                crate::presets::ToolScope::HueSat,
                crate::presets::ToolScope::Warper,
                crate::presets::ToolScope::Color,
            ]
            .iter()
            .map(|t| (t.label(), crate::presets::load_tool_presets(t).len()))
            .filter(|(_, n)| *n > 0)
            .collect(),
        };
    }

    ui.add_space(6.0);
    // ── The collection, and its New / Rename / Delete ──
    let name_field = egui::Id::new("shared_collection_name");
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        if let Some((renaming, name)) = &mut tab.naming {
            let resp = ui.add(egui::TextEdit::singleline(name).id(name_field).hint_text("Collection name").desired_width((ui.available_width() - 70.0).max(90.0)));
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let cancel = ui.small_button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            if enter && !name.trim().is_empty() {
                cmds.push(match renaming {
                    Some(from) => AppCommand::RenameSharedCollection { from: from.clone(), to: name.trim().to_string() },
                    None => AppCommand::CreateSharedCollection { name: name.trim().to_string() },
                });
                tab.naming = None;
            } else if cancel {
                tab.naming = None;
            }
            return;
        }
        egui::ComboBox::from_id_salt("shared_collection")
            .selected_text(&current)
            .width((ui.available_width() - 150.0).max(90.0))
            .show_ui(ui, |ui| {
                for name in &names {
                    if ui.selectable_label(*name == current, name).clicked() {
                        state.shared_collection = name.clone();
                    }
                }
                if names.is_empty() {
                    ui.label(egui::RichText::new("No collections yet").color(theme.text_secondary));
                }
            });
        if ui.small_button("New").on_hover_text("A new, empty collection").clicked() {
            tab.naming = Some((None, String::new()));
            ui.memory_mut(|m| m.request_focus(name_field));
        }
        let exists = state.shared_library.collection(&current).is_some();
        if ui.add_enabled(exists, egui::Button::new("Rename").small()).clicked() {
            tab.naming = Some((Some(current.clone()), current.clone()));
            ui.memory_mut(|m| m.request_focus(name_field));
        }
        if ui.add_enabled(exists, egui::Button::new("Delete").small())
            .on_hover_text("Delete the collection. Its files stay where they are, and projects that use them keep them.")
            .clicked()
        {
            cmds.push(AppCommand::DeleteSharedCollection { name: current.clone() });
        }
    });

    // ── What else every project shares ──
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let o = &tab.overview;
        let warned = o.title_warnings.len();
        let heading = if warned > 0 { format!("Also for every project  \u{b7}  {warned} title template{} need{} attention", if warned == 1 { "" } else { "s" }, if warned == 1 { "s" } else { "" }) } else { "Also for every project".to_string() };
        egui::CollapsingHeader::new(egui::RichText::new(heading).size(11.0).color(if warned > 0 { theme.marker_warning } else { theme.text_secondary }))
            .id_salt("shared_overview")
            .default_open(warned > 0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("{} title template{}", o.title_templates, if o.title_templates == 1 { "" } else { "s" })).color(theme.text_primary));
                    if ui.small_button("Titles").on_hover_text("Open them in Titles & Graphics, under User").clicked() {
                        *category = crate::layout_state::LibraryCategory::Titles;
                    }
                });
                for (name, warnings) in &o.title_warnings {
                    ui.add(egui::Label::new(egui::RichText::new(format!("{name}: {}", warnings.join("; "))).small().color(theme.marker_warning)).wrap());
                }
                let grades = state.grade_library.len();
                ui.label(egui::RichText::new(format!("{grades} saved grade{}, in the Colour page's Library row", if grades == 1 { "" } else { "s" })).color(theme.text_primary));
                if o.tool_presets.is_empty() {
                    ui.label(egui::RichText::new("No colour tool presets").color(theme.text_secondary));
                } else {
                    let line = o.tool_presets.iter().map(|(t, n)| format!("{t} {n}")).collect::<Vec<_>>().join("  \u{b7}  ");
                    ui.label(egui::RichText::new(format!("Colour tool presets: {line}")).color(theme.text_primary));
                }
            });
    });

    // ── Search, and Add Files… ──
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.add(egui::TextEdit::singleline(&mut tab.search).hint_text("Search the shared library\u{2026}").desired_width((ui.available_width() - 86.0).max(90.0)));
        if ui.button("Add Files\u{2026}").on_hover_text("Keep files here, where they are: nothing is copied.").clicked() {
            let collection = current.clone();
            crate::file_dialog::spawn(move || {
                rfd::FileDialog::new()
                    .set_title("Add to the Shared Library")
                    .pick_files()
                    .filter(|p| !p.is_empty())
                    .map(|paths| vec![AppCommand::AddToSharedLibrary { paths, collection }])
                    .unwrap_or_default()
            });
        }
    });
    ui.add_space(6.0);

    let shown: Vec<crate::shared_library::SharedItem> = state.shared_library.search(&current, &tab.search).into_iter().cloned().collect();
    if shown.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let why = if items.is_empty() {
                "Keep files here for every project: Add Files\u{2026}, or right-click media in the pool and choose Add to Shared Library."
            } else {
                "Nothing here matches the search."
            };
            ui.add(egui::Label::new(egui::RichText::new(why).color(theme.text_secondary)).wrap());
        });
        return cmds;
    }

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for item in &shown {
            let present = tab.is_present(&item.path);
            let in_project = state.media_pool.find_by_path(&item.path).is_some();
            ui.add_space(4.0);
            let row = ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(item.name()).strong().color(if present { theme.text_primary } else { theme.text_secondary }));
                        if !present {
                            ui.label(egui::RichText::new("Missing").small().color(theme.marker_warning))
                                .on_hover_text("The file is not where it was: moved, renamed or deleted. Relink it to find it again.");
                        } else if in_project {
                            ui.label(egui::RichText::new("In project").small().color(theme.text_secondary));
                        }
                    });
                    let folder = item.path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                    ui.label(egui::RichText::new(folder).small().color(theme.text_dim));
                });
            });
            // The row drags its file onto the pool or a track, as a file
            // from the Files browser does.
            let drag = ui.interact(row.response.rect, ui.id().with(("shared_row", &item.path)), egui::Sense::drag());
            if drag.drag_started() && present {
                state.drag_drop.begin(crate::drag_drop::DragPayload::FilePaths { paths: vec![item.path.clone()] });
            }
            if state.drag_drop.is_active() {
                if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                    state.drag_drop.update_pos(p);
                }
            }
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if present {
                    if ui.small_button("Add to Project").on_hover_text("Into this project's media pool").clicked() {
                        cmds.push(AppCommand::ImportMediaTo { path: item.path.clone(), drop: ClipDrop::PoolOnly });
                    }
                    if ui.small_button("Add to Timeline").on_hover_text("Into the pool, and onto the end of the timeline").clicked() {
                        cmds.push(AppCommand::ImportMediaTo { path: item.path.clone(), drop: ClipDrop::EndOfTimeline });
                    }
                } else if ui.small_button("Relink\u{2026}").clicked() {
                    let (collection, old, title) = (current.clone(), item.path.clone(), format!("Locate {}", item.name()));
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title(title)
                            .pick_file()
                            .map(|new| vec![AppCommand::RelinkSharedLibraryItem { collection, old, new }])
                            .unwrap_or_default()
                    });
                }
                if ui.small_button("Remove")
                    .on_hover_text("Take it out of this collection. The file, and projects that use it, are left as they are.")
                    .clicked()
                {
                    cmds.push(AppCommand::RemoveFromSharedLibrary { collection: current.clone(), path: item.path.clone() });
                }
            });
            ui.add_space(4.0);
            ui.separator();
        }
    });
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_library::SharedLibrary;

    struct View {
        ctx: egui::Context,
        state: AppState,
        tab: SharedTabState,
        category: crate::layout_state::LibraryCategory,
        clock: f64,
        sent: Vec<AppCommand>,
    }

    impl View {
        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
            self.clock += 0.05;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(520.0, 600.0))), time: Some(self.clock), events, ..Default::default() };
            let theme = KlippTheme::default();
            let View { ctx, state, tab, category, .. } = self;
            let mut cmds = Vec::new();
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| cmds = show(ui, state, &theme, tab, category));
            });
            self.sent.extend(cmds);
            out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
        }

        fn at(&mut self, text: &str) -> Vec<egui::Pos2> {
            self.frame(Vec::new()).into_iter().filter(|(t, _)| t == text).map(|(_, p)| p).collect()
        }

        fn click_at(&mut self, at: egui::Pos2) {
            self.frame(vec![egui::Event::PointerMoved(at)]);
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
            }
        }

        fn key(&mut self, key: egui::Key) {
            self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        }
    }

    /// Driven with pointer and key events (#614): a file that is there
    /// offers Add to Project and Add to Timeline, as imports into the pool
    /// (and onto the timeline); a missing one says Missing and offers
    /// Relink instead; Remove takes an entry out; search narrows the list;
    /// dragging a row carries its file as a Files-browser drag does, and a
    /// missing file doesn't drag; New names a collection.
    #[test]
    fn the_shared_view_brings_files_in_and_shows_missing_ones() {
        let root = std::env::temp_dir().join(format!("klipp-shared-tab-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let (logo, gone) = (root.join("logo.png"), root.join("interview.mov"));
        std::fs::write(&logo, b"png").unwrap();
        let mut lib = SharedLibrary::default();
        lib.add("Brand", &[logo.clone(), gone.clone()]);
        let state = AppState { shared_library: lib, shared_collection: "Brand".into(), ..Default::default() };
        let mut view = View { ctx: egui::Context::default(), state, tab: SharedTabState::default(), category: crate::layout_state::LibraryCategory::Shared, clock: 0.0, sent: Vec::new() };
        view.frame(Vec::new());

        assert_eq!(view.at("Missing").len(), 1, "the deleted file says so");
        assert_eq!(view.at("Relink\u{2026}").len(), 1, "and offers Relink");
        assert_eq!(view.at("Add to Project").len(), 1, "only the file that is there is offered to bring in");

        let add = view.at("Add to Project")[0];
        view.click_at(add);
        let timeline = view.at("Add to Timeline")[0];
        view.click_at(timeline);
        assert!(view.sent.iter().any(|c| matches!(c, AppCommand::ImportMediaTo { path, drop: ClipDrop::PoolOnly } if *path == logo)), "{:?}", view.sent);
        assert!(view.sent.iter().any(|c| matches!(c, AppCommand::ImportMediaTo { path, drop: ClipDrop::EndOfTimeline } if *path == logo)), "{:?}", view.sent);

        // The deleted file's Remove: the second row's.
        let removes = view.at("Remove");
        assert_eq!(removes.len(), 2);
        let gone_row_y = view.at("interview.mov")[0].y;
        let remove = *removes.iter().min_by(|a, b| (a.y - gone_row_y).abs().partial_cmp(&(b.y - gone_row_y).abs()).unwrap()).unwrap();
        view.click_at(remove);
        assert!(view.sent.iter().any(|c| matches!(c, AppCommand::RemoveFromSharedLibrary { collection, path } if collection == "Brand" && *path == gone)), "{:?}", view.sent);

        // Dragging the file that is there carries it; the missing one, nothing.
        let from = view.at("logo.png")[0];
        view.frame(vec![egui::Event::PointerMoved(from), egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
        for step in 1..=4 {
            view.frame(vec![egui::Event::PointerMoved(from + egui::vec2(0.0, 12.0 * step as f32))]);
        }
        assert!(matches!(view.state.drag_drop.payload.as_ref(), Some(crate::drag_drop::DragPayload::FilePaths { paths }) if *paths == vec![logo.clone()]), "the row's file is dragged");
        view.frame(vec![egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
        view.state.drag_drop = Default::default();
        let from = view.at("interview.mov")[0];
        view.frame(vec![egui::Event::PointerMoved(from), egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
        for step in 1..=4 {
            view.frame(vec![egui::Event::PointerMoved(from + egui::vec2(0.0, 12.0 * step as f32))]);
        }
        assert!(view.state.drag_drop.payload.as_ref().is_none(), "a missing file has nothing to drag");
        view.frame(vec![egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);

        // Search narrows the list.
        view.tab.search = "LOGO".into();
        assert!(view.at("interview.mov").is_empty() && view.at("logo.png").len() == 1);
        view.tab.search.clear();

        // New: a field, a name typed, Enter.
        let new = view.at("New")[0];
        view.click_at(new);
        view.frame(vec![egui::Event::Text("Client A".into())]);
        view.key(egui::Key::Enter);
        assert!(view.sent.iter().any(|c| matches!(c, AppCommand::CreateSharedCollection { name } if name == "Client A")), "{:?}", view.sent);
        assert!(view.tab.naming.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    /// #614: the overview lists what else every project shares; a saved
    /// title template whose font isn't installed, or whose picture is
    /// gone, says so (and opens the overview); its Titles button opens
    /// Titles & Graphics.
    #[test]
    fn the_overview_warns_of_a_title_template_missing_its_font_or_picture() {
        use klipp_timeline::{TextBox, TitleData, TitleObjectContent};
        let root = std::env::temp_dir().join(format!("klipp-shared-overview-{}", uuid::Uuid::new_v4()));
        let mut store = crate::title_templates::TitleTemplateStore::load(Some(root.join("titles")));
        let text = |font: &str| TitleData::single_text(TextBox { text: "Opening".into(), font_name: font.into(), ..Default::default() });
        let broken = text("Gone Grotesk");
        let mut with_picture = text("Inter");
        with_picture.objects[0].content = TitleObjectContent::Image(serde_json::from_value(serde_json::json!({ "path": root.join("logo.png").to_string_lossy() })).unwrap());
        store.save("Opener", "Mine", broken).unwrap();
        store.save("Bug", "Mine", with_picture).unwrap();
        store.save("Fine", "Mine", text("Inter")).unwrap();
        let state = AppState { title_templates: store, ..Default::default() };
        let tab = SharedTabState { font_installed: |f| f == "Inter", ..Default::default() };
        let mut view = View { ctx: egui::Context::default(), state, tab, category: crate::layout_state::LibraryCategory::Shared, clock: 0.0, sent: Vec::new() };
        view.frame(Vec::new());
        let shown: Vec<String> = view.frame(Vec::new()).into_iter().map(|(t, _)| t).collect();
        assert!(shown.iter().any(|t| t.contains("2 title templates need attention")), "{shown:?}");
        assert!(shown.iter().any(|t| t == "3 title templates"), "{shown:?}");
        assert!(shown.iter().any(|t| t.starts_with("Opener: the font Gone Grotesk isn't installed")), "{shown:?}");
        assert!(shown.iter().any(|t| t.starts_with("Bug: the picture") && t.ends_with("is missing")), "{shown:?}");
        assert!(!shown.iter().any(|t| t.starts_with("Fine:")), "{shown:?}");
        let titles = view.at("Titles")[0];
        view.click_at(titles);
        assert_eq!(view.category, crate::layout_state::LibraryCategory::Titles);
        let _ = std::fs::remove_dir_all(root);
    }
}
