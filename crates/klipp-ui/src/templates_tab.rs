//! Reusable compositions inside Library → Titles & Graphics.
//!
//! Each carries its composition (#2757): inserted into a project without
//! it, the sequences it is made of are copied in. The composition and
//! inspector support stays the same as before, without advertising
//! unfinished built-in catalogue entries.

use crate::state::AppState;
use crate::theme::KlippTheme;

/// Show saved project graphics and whether their source is available.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search: &mut String,
) -> Vec<klipp_core::AppCommand> {
    let mut commands = Vec::new();
    crate::browser::browser_search_field(ui, theme, search, "Search project graphics…", state);
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Reuse a sequence as a graphic in another sequence, in this project or any other.")
            .color(theme.text_secondary),
    );
    if ui.button("Save active sequence as graphic").on_hover_text("Keeps this sequence and what it nests, so it inserts into any project; in this project, changes to the sequence update its instances.").clicked() {
        let sequence = state.sequence();
        let name = if sequence.name.trim().is_empty() { "My graphic".to_string() } else { sequence.name.clone() };
        commands.push(klipp_core::AppCommand::SaveCompositionTemplate { sequence_id: sequence.id.0, name });
    }
    ui.add_space(6.0);

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
    egui::ScrollArea::vertical()
        .id_salt("project_graphics_scroll")
        .show(ui, |ui| {
            if templates.is_empty() {
                ui.label(
                    egui::RichText::new(if needle.is_empty() {
                        "No project graphics saved yet."
                    } else {
                        "No project graphics match the search."
                    })
                    .color(theme.text_dim),
                );
                return;
            }
            for template in templates {
                ui.push_id(template.id, |ui| {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&template.name).strong());
                            // Where it is from (#2757): Built-in, Mine or a pack.
                            ui.label(egui::RichText::new(state.composition_templates.source_of(template.id).label()).small().color(theme.text_dim));
                        });
                        if !template.category.is_empty() {
                            ui.label(
                                egui::RichText::new(&template.category)
                                    .small()
                                    .color(theme.text_dim),
                            );
                        }
                        let unavailable = insertion_unavailable(state, template);
                        if let Some(reason) = unavailable {
                            ui.label(egui::RichText::new(reason).small().color(theme.text_dim));
                        } else if !template.parameters.is_empty() {
                            ui.label(
                                egui::RichText::new(
                                    "Edit exposed controls in the Inspector after insertion.",
                                )
                                .small()
                                .color(theme.text_secondary),
                            );
                        }
                        if ui
                            .add_enabled(
                                unavailable.is_none(),
                                egui::Button::new("Insert at playhead"),
                            )
                            .clicked()
                        {
                            commands.push(klipp_core::AppCommand::InsertCompositionTemplate {
                                template_id: template.id,
                            });
                        }
                    });
                });
                ui.add_space(4.0);
            }
        });
    commands
}

fn insertion_unavailable(state: &AppState, template: &klipp_timeline::composition_template::CompositionTemplate) -> Option<&'static str> {
    // The project's copy when it has one, else the one the template carries.
    let Some(sequence) = state
        .sequences
        .iter()
        .find(|sequence| sequence.id.0 == template.composition_id)
        .or_else(|| template.composition())
    else {
        return Some("This graphic does not carry its composition: save it again.");
    };
    if sequence.id == state.sequence().id {
        Some("Switch to another sequence to insert this graphic.")
    } else if sequence.total_duration().to_secs_f64() <= 0.0 {
        Some("Add content to the source sequence before inserting this graphic.")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parameterized_templates::{CompositionTemplate, CompositionTemplateStore};
    use klipp_core::{AppCommand, MediaId, RationalTime};
    use klipp_timeline::{Clip, Sequence};

    fn frame(
        ctx: &egui::Context,
        state: &mut AppState,
        search: &mut String,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<AppCommand>) {
        let mut commands = Vec::new();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(480.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    commands.extend(show(ui, state, &KlippTheme::default(), search));
                });
            },
        );
        (output, commands)
    }

    #[test]
    fn saving_from_an_empty_or_filtered_view_emits_the_command() {
        let dir = std::env::temp_dir().join(format!("klipp-graphics-ui-{}", uuid::Uuid::new_v4()));
        let mut state = AppState {
            composition_templates: CompositionTemplateStore::load(Some(dir.clone())),
            ..Default::default()
        };
        for search in ["", "no matching graphic"] {
            let ctx = egui::Context::default();
            let mut query = search.to_string();
            // First iteration has no templates; second has one excluded by search.
            let (output, _) = frame(&ctx, &mut state, &mut query, Vec::new());
            let at = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text)
                        if text.galley.text() == "Save active sequence as graphic" =>
                    {
                        Some(text.pos + text.galley.size() / 2.0)
                    }
                    _ => None,
                })
                .expect("save button is visible even with no results");
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(
                &ctx,
                &mut state,
                &mut query,
                vec![egui::Event::PointerMoved(at), button(true)],
            );
            let (_, commands) = frame(&ctx, &mut state, &mut query, vec![button(false)]);
            assert!(
                matches!(commands.as_slice(), [AppCommand::SaveCompositionTemplate { sequence_id, name }]
                if *sequence_id == state.sequence().id.0 && name == &state.sequence().name)
            );
            let mut existing = CompositionTemplate::new("Existing graphic", state.sequence().id.0);
            existing.sequences = vec![state.sequence().clone()];
            state.composition_templates.save(existing).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A graphic inserts from the composition it carries into a project
    /// without it (#2757), not into its own composition, and not while
    /// that is empty.
    #[test]
    fn insertion_requires_a_nonempty_composition_in_another_sequence() {
        let state = AppState::default();
        let carrying = |source: &Sequence| {
            let mut t = CompositionTemplate::new("Graphic", source.id.0);
            t.sequences = vec![source.clone()];
            t
        };
        assert!(insertion_unavailable(&state, &CompositionTemplate::new("Bare", uuid::Uuid::new_v4())).unwrap().contains("save it again"));
        assert!(insertion_unavailable(&state, &carrying(state.sequence())).unwrap().contains("another sequence"));
        let mut source = Sequence::new_default();
        assert!(insertion_unavailable(&state, &carrying(&source)).unwrap().contains("Add content"));
        let zero = RationalTime::from_frames(0, 24);
        let end = RationalTime::from_frames(48, 24);
        source.tracks[0].clips.push(Clip::media(MediaId::new(), zero, end, zero, end));
        assert!(insertion_unavailable(&state, &carrying(&source)).is_none(), "carried from another project: insertable here");
    }
}
