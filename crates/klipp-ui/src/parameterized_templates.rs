//! User-authored composition templates with typed, exposed parameters.
//!
//! A [`CompositionTemplate`] is a normal Klipp composition (usually a
//! precomposition) plus the small amount of metadata needed to make it
//! reusable from the app.  The renderer does not need a template-specific
//! path: an instance resolves its overrides onto the ordinary composition
//! before rendering.  Keeping this metadata separate from the layer model
//! also means old projects continue to load unchanged.

/// File extension for user-authored composition templates.
pub use klipp_timeline::composition_template::{EXTENSION, FORMAT_VERSION};

/// Shared with persisted timeline instances so templates can be edited by the
/// UI without making the timeline depend on this crate.
pub use klipp_core::TemplateValue;

pub use klipp_timeline::composition_template::{CompositionTemplate, CompositionTemplateStore, NumberRange, TemplateBinding, TemplateParameter};

/// The composition templates and recipes the app shows (#2757): the
/// user's own in Klipp's data folder, the installed packs' and Klipp's,
/// what was kept in the config folder moved over first.
pub fn load_store() -> CompositionTemplateStore {
    use klipp_timeline::template_places::{data_root, ensure_moved, mine_templates};
    ensure_moved();
    let root = data_root();
    let store = CompositionTemplateStore::load(root.as_deref().map(mine_templates));
    match root.as_deref() {
        Some(root) => store.with_packs(root),
        None => store,
    }
    .with_builtins()
}

/// Build a starter template from a composition. The first text object becomes
/// an exposed `text` parameter; more elaborate parameter authoring remains a
/// later slice, while the saved composition is already reusable and editable.
pub fn template_from_sequence(sequence: &klipp_timeline::Sequence, all: &[klipp_timeline::Sequence], name: impl Into<String>) -> CompositionTemplate {
    let mut template = CompositionTemplate::new(name, sequence.id.0);
    // The composition and what it nests go with it (#2757).
    template.sequences = klipp_timeline::composition_template::composition_sequences(sequence, all);
    if let Some(title) = sequence.tracks.iter().flat_map(|track| &track.clips).find_map(|clip| match &clip.kind {
        klipp_timeline::ClipKind::Title(title) => Some(title),
        _ => None,
    }) {
        if let Some((index, text)) = title.objects.iter().enumerate().find_map(|(index, object)| match &object.content {
            klipp_timeline::TitleObjectContent::TextBox(text) => Some((index, text.text.clone())),
            _ => None,
        }) {
            template.parameters.insert("text".into(), TemplateParameter {
                label: "Text".into(),
                default: TemplateValue::Text(text),
                range: None,
                choices: Vec::new(),
            });
            template.bindings.push(TemplateBinding { parameter: "text".into(), property: format!("object:{index}.text"), link: None, take: None });
        }
    }
    template
}

/// Save a composition as a template in the store. The composition's
/// existing template, found by `composition_id`, is replaced — same id,
/// same file — so saving twice does not make two; a composition with no
/// template yet gets a new one. Returns whether one was replaced.
pub fn save_composition_as_template(
    store: &mut CompositionTemplateStore,
    sequence: &klipp_timeline::Sequence,
    all: &[klipp_timeline::Sequence],
    name: impl Into<String>,
) -> Result<bool, String> {
    let mut template = template_from_sequence(sequence, all, name);
    let existing = store.templates().find(|existing| existing.composition_id == sequence.id.0).map(|existing| existing.id);
    if let Some(id) = existing {
        template.id = id;
    }
    store.save(template)?;
    Ok(existing.is_some())
}

/// An inserted template instance. The data lives in core so it can be stored
/// on a timeline clip; these helpers keep catalogue validation in the UI.
pub use klipp_core::TemplateInstance;

trait TemplateInstanceExt {
    #[allow(dead_code)]
    fn set(
        &mut self,
        template: &CompositionTemplate,
        name: &str,
        value: TemplateValue,
    ) -> Result<(), String>;
    #[allow(dead_code)]
    fn value<'a>(
        &'a self,
        template: &'a CompositionTemplate,
        name: &str,
    ) -> Option<&'a TemplateValue>;
}

impl TemplateInstanceExt for TemplateInstance {
    fn set(
        &mut self,
        template: &CompositionTemplate,
        name: &str,
        value: TemplateValue,
    ) -> Result<(), String> {
        let parameter = template
            .parameters
            .get(name)
            .ok_or_else(|| format!("template has no parameter '{name}'"))?;
        parameter.validate(&value)?;
        self.overrides.insert(name.to_string(), value);
        Ok(())
    }

    fn value<'a>(
        &'a self,
        template: &'a CompositionTemplate,
        name: &str,
    ) -> Option<&'a TemplateValue> {
        self.overrides.get(name).or_else(|| {
            template
                .parameters
                .get(name)
                .map(|parameter| &parameter.default)
        })
    }
}

/// Nodes of a clip's picture saved together as a recipe (#2757 step 6):
/// the clip, the nodes, the recipe's name and category, and which of the
/// nodes' numbers become its controls, each ticked or not.
#[derive(Debug, Clone)]
pub struct RecipeSaveDialog {
    pub clip: klipp_timeline::Clip,
    pub nodes: Vec<uuid::Uuid>,
    pub name: String,
    pub category: String,
    /// A control's label, the node and parameter it drives, the range it
    /// moves across, and whether it is published.
    pub picks: Vec<(String, uuid::Uuid, &'static str, [f64; 2], bool)>,
}

impl RecipeSaveDialog {
    /// For `nodes` of `clip`: every number of theirs that moves over time
    /// offered as a control, none ticked.
    pub fn new(clip: klipp_timeline::Clip, nodes: Vec<uuid::Uuid>) -> Self {
        let mut picks = Vec::new();
        if let Ok(lifted) = clip.picture_v2() {
            for n in lifted.picture.graph.nodes.iter().filter(|n| nodes.contains(&n.id)) {
                let desc = match klipp_graph_lift::effect_params::tag(n) {
                    Some(tag) => klipp_effects::registry::effect_type(tag).map(|t| &t.desc),
                    None => klipp_timeline::grade_node_for_run(n).ok().and_then(|r| klipp_nodes::registry::node_type(r.kind)).map(|t| &t.desc),
                };
                let Some(desc) = desc else { continue };
                let name = if n.label.is_empty() { desc.name.to_string() } else { n.label.clone() };
                for p in desc.params.iter().filter(|p| p.keyframable && matches!(p.kind, klipp_effects::registry::ParamKind::Float)) {
                    let (lo, hi) = p.soft.or(p.range).unwrap_or((0.0, 1.0));
                    picks.push((format!("{name} {}", p.label), n.id, p.key, [f64::from(lo), f64::from(hi)], false));
                }
            }
        }
        Self { clip, nodes, name: "My Recipe".into(), category: "My Recipes".into(), picks }
    }

    /// The ticked numbers as the recipe's controls: each across its range,
    /// driving its parameter across the same.
    pub fn controls(&self) -> Vec<klipp_timeline::composition_template::ControlSpec> {
        use klipp_timeline::composition_template::{ControlSpec, DriveSpec};
        self.picks
            .iter()
            .filter(|p| p.4)
            .map(|(label, node, key, range, _)| ControlSpec { label: label.clone(), min: Some(range[0]), max: Some(range[1]), default: None, drives: vec![DriveSpec { target: format!("node:{node}.{key}"), to: *range, curve: Default::default() }] })
            .collect()
    }
}

/// The dialog that saves nodes of a clip's picture as a recipe (#2757
/// step 6), under the user's own.
pub fn show_recipe_save_dialog(ctx: &egui::Context, state: &mut crate::state::AppState) {
    let Some(dialog) = state.recipe_save_dialog.as_mut() else { return };
    let (mut open, mut close) = (true, false);
    let mut save = false;
    egui::Window::new("Save as Recipe")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_min_width(300.0);
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut dialog.name).request_focus();
            });
            ui.horizontal(|ui| {
                ui.label("Category:");
                ui.text_edit_singleline(&mut dialog.category);
            });
            if !dialog.picks.is_empty() {
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Controls it has once it is placed").strong());
                egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                    for (label, .., on) in dialog.picks.iter_mut() {
                        ui.checkbox(on, label.as_str());
                    }
                });
            }
            ui.add_space(8.0);
            save = crate::title_templates::dialog_buttons(ui, "Save", !dialog.name.trim().is_empty(), &mut close);
        });
    if save {
        let made = klipp_timeline::composition_template::recipe_from_nodes(&dialog.clip, &dialog.nodes, &dialog.name, Some(dialog.category.trim().to_string()), &dialog.controls()).and_then(|mut r| {
            // Saved over a recipe of the same name, as a template is.
            if let Some(existing) = state.composition_templates.recipes().find(|e| e.name.eq_ignore_ascii_case(r.name.trim())) {
                r.id = existing.id;
            }
            let name = r.name.clone();
            state.composition_templates.save(r).map(|_| name)
        });
        match made {
            Ok(name) => {
                state.toast.show(format!("Saved recipe \"{name}\""));
                close = true;
            }
            Err(e) => state.toast.show(format!("Recipe not saved: {e}")),
        }
    }
    if close || !open {
        state.recipe_save_dialog = None;
    }
}

/// Clips saved together as a composition template (#2757): their scene,
/// its name and category, and what it exposes, each by label, ticked or
/// not, a media slot marked.
#[derive(Debug, Clone)]
pub struct CompositionSaveDialog {
    pub scene: klipp_timeline::Sequence,
    pub name: String,
    pub category: String,
    pub picks: Vec<(String, bool, bool)>,
}

impl CompositionSaveDialog {
    /// For `scene`: its texts and media slots ticked, its colours, fonts
    /// and pictures offered.
    pub fn new(scene: klipp_timeline::Sequence) -> Self {
        let picks = klipp_timeline::composition_template::candidates(&scene).into_iter().map(|c| (c.label.clone(), c.by_default(), c.parameter.is_none())).collect();
        Self { scene, name: "My Template".into(), category: "My Templates".into(), picks }
    }
}

/// The dialog that saves clips as a composition template (#2757).
pub fn show_save_dialog(ctx: &egui::Context, state: &mut crate::state::AppState) {
    let Some(dialog) = state.composition_save_dialog.as_mut() else { return };
    let (mut open, mut close) = (true, false);
    let mut save = false;
    egui::Window::new("Save as Template")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_min_width(300.0);
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut dialog.name).request_focus();
            });
            ui.horizontal(|ui| {
                ui.label("Category:");
                ui.text_edit_singleline(&mut dialog.category);
            });
            if !dialog.picks.is_empty() {
                ui.add_space(6.0);
                ui.label(egui::RichText::new("What can be changed once it is placed").strong());
                for (label, on, slot) in dialog.picks.iter_mut() {
                    let text = if *slot { format!("{label} (filled with media)") } else { label.clone() };
                    ui.checkbox(on, text);
                }
            }
            ui.add_space(8.0);
            save = crate::title_templates::dialog_buttons(ui, "Save", !dialog.name.trim().is_empty(), &mut close);
        });
    if save {
        let chosen: Vec<String> = dialog.picks.iter().filter(|(_, on, _)| *on).map(|(l, ..)| l.clone()).collect();
        let mut scene = dialog.scene.clone();
        scene.name = dialog.name.trim().to_string();
        let made = klipp_timeline::composition_template::template_from_scene(&scene, &state.sequences, &dialog.name, Some(&chosen)).and_then(|mut t| {
            t.category = dialog.category.trim().to_string();
            // Saved over a template of the same name, as a title template is.
            if let Some(existing) = state.composition_templates.templates().find(|e| e.name.eq_ignore_ascii_case(t.name.trim())) {
                t.id = existing.id;
            }
            let name = t.name.clone();
            state.composition_templates.save(t).map(|_| name)
        });
        match made {
            Ok(name) => {
                state.toast.show(format!("Saved template \"{name}\""));
                close = true;
            }
            Err(e) => state.toast.show(format!("Template not saved: {e}")),
        }
    }
    if close || !open {
        state.composition_save_dialog = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The title of the Terminal Card Klipp ships: a title of three layers,
    /// its text the second.
    fn card_title() -> klipp_timeline::TitleData {
        let card = klipp_timeline::composition_template::builtin_templates().into_iter().find(|t| t.name == "Terminal Card").expect("shipped");
        card.composition().and_then(|s| s.tracks.iter().flat_map(|t| &t.clips).find_map(|c| c.title_data().cloned())).expect("its title")
    }

    /// A template of `name` carrying an empty composition of its own.
    fn carried(name: &str) -> CompositionTemplate {
        let seq = klipp_timeline::Sequence::new_default();
        let mut t = CompositionTemplate::new(name, seq.id.0);
        t.sequences = vec![seq];
        t
    }

    #[test]
    fn instance_uses_defaults_and_validates_overrides() {
        let mut template = carried("Terminal card");
        template.parameters.insert(
            "text".into(),
            TemplateParameter {
                label: "Terminal text".into(),
                default: TemplateValue::Text("hello".into()),
                range: None,
                choices: vec![],
            },
        );
        template.parameters.insert(
            "speed".into(),
            TemplateParameter {
                label: "Typing speed".into(),
                default: TemplateValue::Number(0.25),
                range: Some(NumberRange {
                    min: Some(0.05),
                    max: Some(2.0),
                }),
                choices: vec![],
            },
        );
        template.validate().unwrap();
        let mut instance = TemplateInstance {
            template_id: template.id,
            bindings: std::collections::BTreeMap::new(),
            overrides: std::collections::BTreeMap::new(),
            media_slots: std::collections::BTreeMap::new(),
            links: Vec::new(),
        };
        assert_eq!(
            instance.value(&template, "text"),
            Some(&TemplateValue::Text("hello".into()))
        );
        instance
            .set(&template, "speed", TemplateValue::Number(0.5))
            .unwrap();
        assert!(instance
            .set(&template, "speed", TemplateValue::Number(4.0))
            .is_err());
    }

    #[test]
    fn template_builder_exposes_the_first_text_object() {
        let mut sequence = klipp_timeline::Sequence::new_default();
        let mut clip = klipp_timeline::Clip::gap(
            klipp_core::Timecode::from_frames(0, 24),
            klipp_core::Timecode::from_frames(24, 24),
        );
        clip.kind = klipp_timeline::ClipKind::Title(card_title());
        sequence.tracks[0].clips.push(clip);
        let template = template_from_sequence(&sequence, &[], "Card");
        assert_eq!(template.composition_id, sequence.id.0);
        assert!(template.parameters.contains_key("text"));
        assert_eq!(template.bindings[0].property, "object:1.text");
        template.validate().unwrap();
    }

    /// A composition with no text object is still a template — of the
    /// whole composition, with nothing exposed — and it validates.
    #[test]
    fn a_composition_without_text_makes_a_parameterless_template() {
        let mut sequence = klipp_timeline::Sequence::new_default();
        sequence.tracks[0].clips.push(klipp_timeline::Clip::gap(
            klipp_core::Timecode::from_frames(0, 24),
            klipp_core::Timecode::from_frames(24, 24),
        ));
        let template = template_from_sequence(&sequence, &[], "Plain");
        assert_eq!(template.composition_id, sequence.id.0);
        assert!(template.parameters.is_empty());
        assert!(template.bindings.is_empty());
        template.validate().unwrap();
    }

    /// Saving the same composition twice replaces its template — one
    /// entry, one file, the same id, the new name — while another
    /// composition gets a template of its own.
    #[test]
    fn saving_a_composition_twice_replaces_its_template() {
        let dir = std::env::temp_dir().join(format!("klipp_save_twice_{}_{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = CompositionTemplateStore::load(Some(dir.clone()));
        let mut card = klipp_timeline::Sequence::new_default();
        let mut clip = klipp_timeline::Clip::gap(klipp_core::Timecode::from_frames(0, 24), klipp_core::Timecode::from_frames(24, 24));
        clip.kind = klipp_timeline::ClipKind::Title(card_title());
        card.tracks[0].clips.push(clip);

        assert!(!save_composition_as_template(&mut store, &card, &[], "Card").unwrap(), "the first save is new");
        let first_id = store.templates().next().unwrap().id;
        assert!(save_composition_as_template(&mut store, &card, &[], "Card, renamed").unwrap(), "the second replaces");
        assert_eq!(store.templates().count(), 1, "one template for one composition");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "and one file");
        let only = store.templates().next().unwrap();
        assert_eq!(only.id, first_id, "the same template, kept by id");
        assert_eq!(only.name, "Card, renamed");
        assert_eq!(only.composition_id, card.id.0);

        let other = klipp_timeline::Sequence::new_default();
        assert!(!save_composition_as_template(&mut store, &other, &[], "Other").unwrap());
        assert_eq!(store.templates().count(), 2, "another composition is another template");
        let reloaded = CompositionTemplateStore::load(Some(dir.clone()));
        assert_eq!(reloaded.templates().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_binding_is_rejected() {
        let mut template = carried("Card");
        template.bindings.push(TemplateBinding {
            parameter: "missing".into(),
            property: "layer.text".into(),
         link: None,
         take: None
        });
        assert!(template.validate().is_err());
    }

    #[test]
    fn template_round_trips() {
        let mut template = carried("Terminal card");
        template.parameters.insert(
            "accent".into(),
            TemplateParameter {
                label: "Accent".into(),
                default: TemplateValue::Color([0.6, 0.4, 1.0, 1.0]),
                range: None,
                choices: vec![],
            },
        );
        let json = serde_json::to_string(&template).unwrap();
        let loaded: CompositionTemplate = serde_json::from_str(&json).unwrap();
        assert_eq!(serde_json::to_string(&loaded).unwrap(), json);
    }

    /// A number that JSON cannot carry is refused before it is saved.
    #[test]
    fn a_non_finite_number_is_refused() {
        let parameter = TemplateParameter {
            label: "Speed".into(),
            default: TemplateValue::Number(0.5),
            range: None,
            choices: vec![],
        };
        assert!(parameter
            .validate(&TemplateValue::Number(f64::NAN))
            .is_err());
        assert!(parameter
            .validate(&TemplateValue::Number(f64::INFINITY))
            .is_err());
        assert!(parameter.validate(&TemplateValue::Number(0.25)).is_ok());
    }

    /// Two templates that share a name are two files.
    #[test]
    fn a_second_template_with_the_same_name_gets_its_own_file() {
        let dir = std::env::temp_dir().join(format!(
            "klipp_templates_{}_{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = CompositionTemplateStore::load(Some(dir.clone()));
        store
            .save(carried("Card"))
            .unwrap();
        store
            .save(carried("Card"))
            .unwrap();
        assert_eq!(store.templates().count(), 2);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        let reloaded = CompositionTemplateStore::load(Some(dir.clone()));
        assert_eq!(reloaded.templates().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
