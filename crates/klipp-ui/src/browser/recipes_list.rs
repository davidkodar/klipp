//! The Library's Recipes list: Klipp's composite recipes and the ones saved
//! or installed here, each put on the selected clip with a click. They were
//! reachable only over the control socket, so a person could save a recipe
//! but never apply one.

use klipp_core::{AppCommand, ClipId};
use klipp_timeline::composition_template::{CompositionTemplate, SlotKind};

use crate::state::AppState;
use crate::theme::KlippTheme;

/// One recipe as the list shows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RecipeRow {
    pub name: String,
    pub category: String,
    /// Its controls' labels, as the inspector will show them.
    pub controls: Vec<String>,
    /// What it needs to be given: its name, label and kind.
    pub slots: Vec<(String, String, SlotKind)>,
    /// Saved here or installed, not shipped with Klipp.
    pub own: bool,
}

/// The recipes to list: the saved and installed ones first, then Klipp's,
/// each name once, as the recipe op looks them up. A saved recipe of the
/// same name as one of Klipp's is the one the op uses, so it is the one
/// listed.
pub(crate) fn rows(saved: &[CompositionTemplate], builtin: &[CompositionTemplate]) -> Vec<RecipeRow> {
    let row = |r: &CompositionTemplate, own: bool| RecipeRow {
        name: r.name.clone(),
        category: if r.category.is_empty() { "Other".into() } else { r.category.clone() },
        controls: r.parameters.values().map(|p| p.label.clone()).collect(),
        slots: r.slots.iter().map(|(n, s)| (n.clone(), s.label.clone(), s.kind)).collect(),
        own,
    };
    let mut out: Vec<RecipeRow> = saved.iter().map(|r| row(r, true)).collect();
    for r in builtin {
        if !out.iter().any(|o| o.name.eq_ignore_ascii_case(&r.name)) {
            out.push(row(r, false));
        }
    }
    out.sort_by(|a, b| a.category.cmp(&b.category).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Whether `row` answers a search: its name, its category or a control.
pub(crate) fn matches(row: &RecipeRow, search: &str) -> bool {
    let q = search.trim().to_lowercase();
    q.is_empty()
        || row.name.to_lowercase().contains(&q)
        || row.category.to_lowercase().contains(&q)
        || row.controls.iter().any(|c| c.to_lowercase().contains(&q))
}

/// The clip a recipe goes on: the selected clip, when it is a picture on a
/// video track.
pub(crate) fn target(state: &AppState) -> Option<(ClipId, String)> {
    let clip = state.selected_clip?;
    let seq = state.sequence();
    let c = seq
        .tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
        .find_map(|t| t.clips.iter().find(|c| c.id == clip))?;
    let name = c
        .media_id()
        .and_then(|m| state.media_pool.get(&m))
        .map_or_else(|| "the selected clip".to_string(), |it| klipp_core::quote::quoted(&it.filename));
    Some((clip, name))
}

/// Every recipe the app knows: the saved and installed ones, then Klipp's.
pub(crate) fn all_rows(state: &AppState) -> Vec<RecipeRow> {
    let saved: Vec<CompositionTemplate> = state.composition_templates.recipes().cloned().collect();
    rows(&saved, &klipp_timeline::composition_template::builtin_recipes())
}

/// Where a slot's chosen value is kept while the list is open.
fn slot_id(recipe: &str, slot: &str) -> egui::Id {
    egui::Id::new(("recipe_slot", recipe.to_string(), slot.to_string()))
}

/// One slot to fill before Apply: a colour, a media item, a surface of the
/// clip, a title's text, or which subject matte.
fn slot_row(ui: &mut egui::Ui, state: &AppState, clip: Option<ClipId>, recipe: &str, slot: &str, label: &str, kind: SlotKind) {
    let id = slot_id(recipe, slot);
    let mut value = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_default();
    let before = value.clone();
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(label).size(11.0));
        match kind {
            SlotKind::Colour => {
                let mut rgb = hex_rgb(&value).unwrap_or([0, 177, 64]);
                if ui.color_edit_button_srgb(&mut rgb).changed() || value.is_empty() && ui.small_button("Use").clicked() {
                    value = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
                }
            }
            SlotKind::Media => {
                let shown = state.media_pool.all().find(|m| m.id.0.to_string() == value).map_or("Choose\u{2026}".to_string(), |m| m.filename.clone());
                egui::ComboBox::from_id_salt(id.with("media")).selected_text(shown).width(160.0).show_ui(ui, |ui| {
                    for m in state.media_pool.all() {
                        ui.selectable_value(&mut value, m.id.0.to_string(), &m.filename);
                    }
                });
            }
            SlotKind::Surface => {
                let surfaces: Vec<String> = clip
                    .and_then(|c| state.sequence().all_clips().find(|x| x.id == c).map(|x| x.surfaces.iter().map(|s| s.name.clone()).collect()))
                    .unwrap_or_default();
                if surfaces.is_empty() {
                    ui.label(egui::RichText::new("none yet: track one in the Video tab's Surface section").size(10.0));
                } else {
                    egui::ComboBox::from_id_salt(id.with("surface")).selected_text(if value.is_empty() { "Choose\u{2026}" } else { value.as_str() }).show_ui(ui, |ui| {
                        for s in surfaces {
                            ui.selectable_value(&mut value, s.clone(), s);
                        }
                    });
                }
            }
            SlotKind::Title => {
                ui.add(egui::TextEdit::singleline(&mut value).hint_text("The title's text").desired_width(160.0));
            }
            SlotKind::SubjectMatte => {
                egui::ComboBox::from_id_salt(id.with("subject")).selected_text(if value.is_empty() { "Choose\u{2026}" } else { value.as_str() }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut value, "subject".to_string(), "subject").on_hover_text("One detected subject");
                    ui.selectable_value(&mut value, "foreground".to_string(), "foreground").on_hover_text("Everything in front");
                });
            }
            SlotKind::Clip => {
                ui.label(egui::RichText::new("a clip on the timeline: set it with the recipe op").size(10.0));
            }
        }
    });
    if value != before {
        ui.data_mut(|d| d.insert_temp(id, value));
    }
}

/// `#rrggbb` as bytes.
fn hex_rgb(v: &str) -> Option<[u8; 3]> {
    let h = v.strip_prefix('#').filter(|h| h.len() == 6)?;
    let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([b(0)?, b(2)?, b(4)?])
}

/// The list, under the Library's search field.
pub fn show(ui: &mut egui::Ui, state: &AppState, theme: &KlippTheme, search: &str, cmds: &mut Vec<AppCommand>) {
    let target = target(state);
    egui::Frame::none().inner_margin(egui::Margin::symmetric(10.0, 4.0)).show(ui, |ui| {
        let line = match &target {
            Some((_, name)) => format!("Click Apply to build a recipe into {name}. Its controls are in the inspector; Undo takes it off."),
            None => "Select a clip on the timeline to apply a recipe to it.".to_string(),
        };
        ui.label(egui::RichText::new(line).size(11.0).color(theme.text_dim));
    });
    let rows: Vec<RecipeRow> = all_rows(state).into_iter().filter(|r| matches(r, search)).collect();
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.vertical_centered(|ui| ui.label(egui::RichText::new("No recipe matches the search.").color(theme.text_dim)));
        return;
    }
    egui::ScrollArea::vertical().id_salt("recipes_list_scroll").auto_shrink([false, false]).show(ui, |ui| {
        let mut category = None;
        for row in &rows {
            if category.as_deref() != Some(row.category.as_str()) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new(row.category.to_uppercase()).size(10.0).color(theme.text_dim));
                });
                category = Some(row.category.clone());
            }
            egui::Frame::none().inner_margin(egui::Margin::symmetric(10.0, 3.0)).show(ui, |ui| {
                let filled: Vec<(String, String)> = row
                    .slots
                    .iter()
                    .filter_map(|(slot, _, _)| ui.data(|d| d.get_temp::<String>(slot_id(&row.name, slot))).filter(|v| !v.is_empty()).map(|v| (slot.clone(), v)))
                    .collect();
                let missing: Vec<&str> = row.slots.iter().filter(|(slot, _, _)| !filled.iter().any(|(f, _)| f == slot)).map(|(_, label, _)| label.as_str()).collect();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&row.name).size(12.0).color(theme.text_primary));
                    if row.own {
                        ui.label(egui::RichText::new("saved").size(10.0).color(theme.text_dim));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let why_not = if target.is_none() {
                            "Select a clip on the timeline first".to_string()
                        } else {
                            format!("Choose {} first", missing.join(" and "))
                        };
                        let apply = ui
                            .add_enabled(target.is_some() && missing.is_empty(), egui::Button::new("Apply").small())
                            .on_hover_text(format!("Build \"{}\" into the selected clip's picture, as one undo step", row.name))
                            .on_disabled_hover_text(why_not);
                        if apply.clicked() {
                            if let Some((clip_id, _)) = &target {
                                cmds.push(AppCommand::ApplyRecipe { clip_id: *clip_id, name: row.name.clone(), slots: filled.clone() });
                            }
                        }
                    });
                });
                if !row.controls.is_empty() {
                    ui.label(egui::RichText::new(format!("Controls: {}", row.controls.join(", "))).size(10.0).color(theme.text_dim));
                }
                for (slot, label, kind) in &row.slots {
                    slot_row(ui, state, target.as_ref().map(|(c, _)| *c), &row.name, slot, label, *kind);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Klipp's recipes are all listed, by category, and a saved one of the
    /// same name takes the place of Klipp's, as the recipe op does.
    #[test]
    fn klipps_recipes_are_listed_and_a_saved_one_comes_first() {
        let builtin = klipp_timeline::composition_template::builtin_recipes();
        let listed = rows(&[], &builtin);
        assert_eq!(listed.len(), builtin.len());
        assert!(listed.iter().any(|r| r.name == "Green screen" && r.controls.contains(&"Tolerance".to_string())), "{listed:?}");
        assert!(listed.windows(2).all(|w| w[0].category <= w[1].category), "grouped by category");
        let green = listed.iter().find(|r| r.name == "Green screen").unwrap();
        assert_eq!(green.slots, vec![("screen".to_string(), "Screen colour".to_string(), SlotKind::Colour)], "what it needs before Apply");
        assert_eq!(super::hex_rgb("#00b140"), Some([0, 177, 64]));
        let mut mine = builtin.iter().find(|r| r.name == "Glitch").unwrap().clone();
        mine.category = "Mine".into();
        let both = rows(std::slice::from_ref(&mine), &builtin);
        assert_eq!(both.len(), builtin.len(), "one Glitch, not two");
        assert!(both.iter().any(|r| r.name == "Glitch" && r.own && r.category == "Mine"));
    }

    /// A search reads the name, the category and the controls.
    #[test]
    fn a_search_finds_a_recipe_by_what_it_does() {
        let listed = rows(&[], &klipp_timeline::composition_template::builtin_recipes());
        let found = |q: &str| listed.iter().filter(|r| matches(r, q)).map(|r| r.name.clone()).collect::<Vec<_>>();
        assert!(found("green").contains(&"Green screen".to_string()));
        assert!(found("keying").contains(&"Green screen".to_string()));
        assert_eq!(found("").len(), listed.len());
        assert!(found("no such thing").is_empty());
    }
}
