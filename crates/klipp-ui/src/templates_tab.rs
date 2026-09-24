//! The in-app composition-template browser.
//!
//! This is deliberately an app-owned surface: users can discover the same
//! template metadata without MCP or an external script, and insert a saved
//! template at the playhead. Editing a template's parameters is not here
//! yet.

use crate::state::AppState;
use crate::theme::KlippTheme;

/// Show saved composition templates and their exposed controls.
pub fn show(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    search: &str,
) -> Vec<klipp_core::AppCommand> {
    let mut commands = Vec::new();
    let needle = search.trim().to_lowercase();
    let templates: Vec<_> = state
        .composition_templates
        .templates()
        .filter(|template| {
            needle.is_empty()
                || template.name.to_lowercase().contains(&needle)
                || template.category.to_lowercase().contains(&needle)
        })
        .collect();

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.heading("Composition templates");
        ui.label(
            egui::RichText::new("Reusable cards, terminals and animated panels")
                .color(theme.text_dim),
        );
    });
    ui.add_space(4.0);

    ui.label(egui::RichText::new("Built-in motion language").strong());
    ui.label(egui::RichText::new("The starting library for launch films. These are catalogue entries: their compositions are not built yet, so they cannot be inserted.").small().color(theme.text_dim));
    for manifest in crate::parameterized_templates::builtin_manifests()
        .iter()
        .filter(|manifest| {
            needle.is_empty()
                || manifest.name.to_lowercase().contains(&needle)
                || manifest.category.to_lowercase().contains(&needle)
                || manifest.description.to_lowercase().contains(&needle)
        })
    {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&manifest.name).strong());
                ui.label(
                    egui::RichText::new(&manifest.category)
                        .small()
                        .color(theme.text_dim),
                );
                ui.label(
                    egui::RichText::new(match manifest.asset_kind {
                        crate::parameterized_templates::TemplateAssetKind::Procedural => {
                            "procedural"
                        }
                        crate::parameterized_templates::TemplateAssetKind::MediaBacked => {
                            "media-backed"
                        }
                    })
                    .small()
                    .color(theme.text_dim),
                );
            });
            ui.label(
                egui::RichText::new(&manifest.description)
                    .small()
                    .color(theme.text_secondary),
            );
        });
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Your templates").strong());

    if templates.is_empty() {
        ui.group(|ui| {
            ui.label(egui::RichText::new(if needle.is_empty() {
                "No composition templates saved yet."
            } else {
                "No templates match the search."
            }).color(theme.text_dim));
            if needle.is_empty() {
                ui.label("Save a composition as a template to expose its text, colors and motion controls here.");
            }
        });
        return Vec::new();
    }

    for template in templates {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&template.name).strong());
                if !template.category.is_empty() {
                    ui.label(
                        egui::RichText::new(&template.category)
                            .small()
                            .color(theme.text_dim),
                    );
                }
            });
            ui.label(
                egui::RichText::new(format!("{} exposed parameters", template.parameters.len()))
                    .small()
                    .color(theme.text_dim),
            );
            for (name, parameter) in &template.parameters {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&parameter.label).color(theme.text_secondary));
                    ui.label(
                        egui::RichText::new(format!("{name}: {}", value_kind(&parameter.default)))
                            .small()
                            .color(theme.text_dim),
                    );
                });
            }
            if ui.button("Insert at playhead").clicked() {
                commands.push(klipp_core::AppCommand::InsertCompositionTemplate {
                    template_id: template.id,
                });
            }
        });
        ui.add_space(4.0);
    }
    commands
}

fn value_kind(value: &crate::parameterized_templates::TemplateValue) -> &'static str {
    use crate::parameterized_templates::TemplateValue;
    match value {
        TemplateValue::Text(_) => "text",
        TemplateValue::Color(_) => "color",
        TemplateValue::Number(_) => "number",
        TemplateValue::Bool(_) => "toggle",
        TemplateValue::Enum(_) => "choice",
    }
}
