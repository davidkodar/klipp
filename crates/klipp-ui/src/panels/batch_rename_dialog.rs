//! Batch rename modal for media-pool items. Mirrors the new-project
//! dialog's shape: centered `egui::Window`, simple `show()` → `Vec<AppCommand>`
//! contract, closes itself by flipping `state.show_batch_rename_dialog`.
//!
//! Modes: Find & Replace (literal, optional case) and Sequential Numbering
//! (`{prefix}{padded counter}{suffix}{.ext}`). Regex, case conversion, and
//! template variables are deferred.

use crate::app::{preview_batch_rename, BatchRenameMode};
use crate::state::AppState;
use klipp_core::{AppCommand, MediaId};

/// Render the batch rename dialog. `items` is the pre-resolved list of
/// `(MediaId, current filename)` pairs in the order they'll be renumbered
/// in Sequential mode. Returns any commands dispatched by the dialog.
/// Caller flips `state.show_batch_rename_dialog` to render this.
pub fn show_batch_rename_dialog(
    ctx: &egui::Context,
    state: &mut AppState,
    items: &[(MediaId, String)],
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let count = items.len();

    // Build the preview every frame — `preview_batch_rename` is pure and
    // cheap for realistic selection sizes.
    let preview = preview_batch_rename(
        items,
        &state.batch_rename_mode,
        &state.batch_rename_find,
        &state.batch_rename_replace,
        state.batch_rename_match_case,
        &state.batch_rename_prefix,
        &state.batch_rename_suffix,
        state.batch_rename_start,
        state.batch_rename_padding,
    );

    let changed_count = preview.iter().filter(|(_, old, new)| old != new).count();

    egui::Window::new("Rename Items")
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);

            // Header
            ui.label(
                egui::RichText::new(format!("Rename {count} item{}", if count == 1 { "" } else { "s" }))
                    .strong()
                    .size(14.0),
            );
            ui.add_space(4.0);

            // Mode selector
            ui.horizontal(|ui| {
                ui.radio_value(&mut state.batch_rename_mode, BatchRenameMode::FindReplace, "Find & Replace");
                ui.radio_value(&mut state.batch_rename_mode, BatchRenameMode::Sequential, "Sequential Numbering");
            });
            ui.separator();

            // Mode-specific fields
            match state.batch_rename_mode {
                BatchRenameMode::FindReplace => {
                    ui.horizontal(|ui| {
                        ui.label("Find:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.batch_rename_find)
                                .desired_width(200.0)
                                .hint_text("substring to match"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Replace:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.batch_rename_replace)
                                .desired_width(200.0),
                        );
                    });
                    ui.checkbox(&mut state.batch_rename_match_case, "Match case");
                }
                BatchRenameMode::Sequential => {
                    ui.horizontal(|ui| {
                        ui.label("Prefix:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.batch_rename_prefix)
                                .desired_width(180.0)
                                .hint_text("Clip_"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Suffix:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.batch_rename_suffix)
                                .desired_width(180.0)
                                .hint_text(""),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Start:");
                        ui.add(egui::DragValue::new(&mut state.batch_rename_start).range(0..=9999));
                        ui.add_space(16.0);
                        ui.label("Zero padding:");
                        ui.add(egui::DragValue::new(&mut state.batch_rename_padding).range(1..=8));
                    });
                }
            }

            ui.separator();

            // Preview section
            ui.label(
                egui::RichText::new(format!(
                    "Preview ({changed_count} of {count} will change)"
                ))
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 170, 180)),
            );
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (_, old, new) in preview.iter().take(50) {
                        let changed = old != new;
                        let row_color = if changed {
                            egui::Color32::from_rgb(230, 230, 235)
                        } else {
                            egui::Color32::from_rgb(120, 120, 130)
                        };
                        ui.horizontal(|ui| {
                            ui.monospace(
                                egui::RichText::new(old.as_str())
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(130, 130, 140)),
                            );
                            ui.label(egui::RichText::new("\u{2192}").size(11.0).color(row_color));
                            ui.monospace(
                                egui::RichText::new(new.as_str())
                                    .size(11.0)
                                    .color(row_color),
                            );
                        });
                    }
                    if preview.len() > 50 {
                        ui.label(
                            egui::RichText::new(format!("\u{2026} and {} more", preview.len() - 50))
                                .size(10.5)
                                .color(egui::Color32::from_rgb(140, 140, 150)),
                        );
                    }
                });

            ui.separator();

            // Footer
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let rename_enabled = changed_count > 0;
                    if ui
                        .add_enabled(rename_enabled, egui::Button::new("Rename"))
                        .clicked()
                    {
                        let changes: Vec<(MediaId, String)> = preview
                            .iter()
                            .filter(|(_, old, new)| old != new)
                            .map(|(id, _, new)| (*id, new.clone()))
                            .collect();
                        if !changes.is_empty() {
                            cmds.push(AppCommand::BatchRenameMediaItems { changes });
                        }
                        state.show_batch_rename_dialog = false;
                    }
                    if ui.button("Cancel").clicked() {
                        state.show_batch_rename_dialog = false;
                    }
                });
            });
        });

    cmds
}
