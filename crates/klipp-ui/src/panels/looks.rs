//! Looks strip — a project-level reusable colour-grade library.
//!
//! Renders a compact horizontal/vertical row of saved Looks above the
//! colour workspace's tabbed inspector. Lets the user save the current
//! clip's grade, apply a saved Look to one or more selected clips,
//! rename, and delete. State + persistence live in `klipp_project`
//! (`save_looks` / `load_looks`); this panel is purely UI + emits the
//! corresponding `AppCommand` variants.
//!
//! UX notes:
//! - Save target = the currently selected clip's whole node graph.
//!   Honest about what's snapshot — full graph, not just the
//!   Corrector's params, so future graphs with extra grade nodes
//!   round-trip cleanly.
//! - Apply target = every clip in `selected_clips` (timeline rubber-
//!   band selection); falls back to the single `selected_clip` when
//!   the multi-select is empty so the typical one-clip case still
//!   works without ceremony.
//! - Empty state shows a hint instead of a wall of nothing.

use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId};
use klipp_timeline::Look;

#[derive(Default)]
pub struct LooksPanelState {
    /// Buffer for the "save as" inline text field. Not stored in
    /// `AppState` because the panel is the only consumer.
    pub new_look_name: String,
    /// Look the user is currently renaming, plus the in-progress text.
    pub renaming: Option<(uuid::Uuid, String)>,
    /// Set when the user clicks the "Save current as Look" button —
    /// expands the inline name field on the next paint.
    pub save_field_open: bool,
}


pub fn show(
    ui: &mut egui::Ui,
    state: &mut LooksPanelState,
    looks: &[Look],
    selected_clip: Option<ClipId>,
    selected_clips: &[ClipId],
    theme: &KlippTheme,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();

    // Header row: "LOOKS" label + "Save current" button on the right.
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("LOOKS")
                .size(10.0).strong().color(theme.text_dim)
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(6.0);
            let label = if state.save_field_open { "Cancel" } else { "+ Save current" };
            let resp = ui.add(
                egui::Button::new(
                    egui::RichText::new(label).size(10.0).color(theme.text_accent)
                ).frame(false)
            );
            let resp = resp.on_hover_text(
                "Snapshot the selected clip's grade as a reusable Look"
            );
            if resp.clicked() {
                state.save_field_open = !state.save_field_open;
                if !state.save_field_open {
                    state.new_look_name.clear();
                }
            }
        });
    });

    // Inline "save as" field — only shown after the user clicks the
    // header button so the panel stays compact at idle.
    if state.save_field_open {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut state.new_look_name)
                    .hint_text("Look name…")
                    .desired_width(160.0)
            );
            let enter = resp.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let save_clicked = ui.add_enabled(
                !state.new_look_name.trim().is_empty() && selected_clip.is_some(),
                egui::Button::new(
                    egui::RichText::new("Save").size(10.0).color(theme.text_accent)
                ).frame(false),
            ).clicked();
            if enter || save_clicked {
                cmds.push(AppCommand::SaveLook { name: state.new_look_name.trim().to_string() });
                state.new_look_name.clear();
                state.save_field_open = false;
            }
        });
        if selected_clip.is_none() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("Select a clip first.")
                        .small().color(theme.text_dim)
                );
            });
        }
    }

    ui.add_space(2.0);

    // Empty state — clearer than a blank panel.
    if looks.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("No saved Looks yet — save the current grade to start.")
                    .small().color(theme.text_dim)
            );
        });
        return cmds;
    }

    // Look rows. Each row: name (or rename field) + Apply button +
    // Rename + Delete. Compact — one row per Look.
    egui::ScrollArea::vertical()
        .id_salt("looks_strip_scroll")
        .max_height(140.0)
        .show(ui, |ui| {
            // Resolve the apply target: prefer the multi-select if
            // present, otherwise the single selected clip.
            let apply_targets: Vec<ClipId> = if !selected_clips.is_empty() {
                selected_clips.to_vec()
            } else if let Some(c) = selected_clip {
                vec![c]
            } else {
                Vec::new()
            };
            let can_apply = !apply_targets.is_empty();

            for look in looks {
                let is_renaming = state.renaming
                    .as_ref()
                    .map(|(id, _)| *id == look.id)
                    .unwrap_or(false);

                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    if is_renaming {
                        let text_ref = state.renaming.as_mut().map(|(_, t)| t).unwrap();
                        let resp = ui.add(
                            egui::TextEdit::singleline(text_ref)
                                .desired_width(140.0)
                        );
                        let commit = resp.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if commit {
                            let new_name = text_ref.trim().to_string();
                            if !new_name.is_empty() {
                                cmds.push(AppCommand::RenameLook {
                                    look_id: look.id,
                                    new_name,
                                });
                            }
                            state.renaming = None;
                        } else if cancel {
                            state.renaming = None;
                        }
                    } else {
                        ui.label(
                            egui::RichText::new(&look.name)
                                .size(11.0).color(theme.text_primary)
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(6.0);
                        // Delete (✕). Right-most so it's hard to fat-
                        // finger when reaching for Apply.
                        let del_resp = ui.add(
                            egui::Button::new(
                                egui::RichText::new("✕").size(11.0).color(theme.text_dim)
                            ).frame(false)
                        ).on_hover_text("Delete this Look");
                        if del_resp.clicked() {
                            cmds.push(AppCommand::DeleteLook { look_id: look.id });
                            if is_renaming {
                                state.renaming = None;
                            }
                        }
                        ui.add_space(2.0);
                        let rename_resp = ui.add(
                            egui::Button::new(
                                egui::RichText::new("✎").size(11.0).color(theme.text_dim)
                            ).frame(false)
                        ).on_hover_text("Rename");
                        if rename_resp.clicked() {
                            state.renaming = Some((look.id, look.name.clone()));
                        }
                        ui.add_space(2.0);
                        let apply_resp = ui.add_enabled(
                            can_apply,
                            egui::Button::new(
                                egui::RichText::new("Apply").size(10.0).color(theme.text_accent)
                            ).frame(false),
                        );
                        let apply_resp = if can_apply {
                            apply_resp.on_hover_text(format!(
                                "Apply to {} selected clip{}",
                                apply_targets.len(),
                                if apply_targets.len() == 1 { "" } else { "s" },
                            ))
                        } else {
                            apply_resp.on_hover_text("Select one or more clips to apply this Look")
                        };
                        if apply_resp.clicked() {
                            cmds.push(AppCommand::ApplyLook {
                                look_id: look.id,
                                clip_ids: apply_targets.clone(),
                            });
                        }
                    });
                });
            }
        });

    cmds
}
