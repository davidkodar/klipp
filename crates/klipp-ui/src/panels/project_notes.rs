use crate::state::AppState;
use crate::theme::KlippTheme;

/// Floating Project Notes window. Free-form text persisted to project.json.
/// Edits set `state.dirty = true` so autosave + status bar dirty marker
/// pick them up; no AppCommand round-trip and no undo (matches the
/// subtitle / title editor convention).
pub fn show_project_notes(
    ctx: &egui::Context,
    state: &mut AppState,
    theme: &KlippTheme,
    open: &mut bool,
) {
    egui::Window::new("Project Notes")
        .open(open)
        .default_size([340.0, 400.0])
        .min_width(200.0)
        .min_height(100.0)
        .resizable(true)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;

            // Word count hint at top
            let word_count = state.project_notes.split_whitespace().count();
            ui.label(
                egui::RichText::new(format!("{} words", word_count))
                    .small()
                    .color(theme.text_secondary),
            );

            ui.separator();

            // Scrollable text area
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let response = ui.add(
                        egui::TextEdit::multiline(&mut state.project_notes)
                            .desired_width(f32::INFINITY)
                            .desired_rows(20)
                            .hint_text("Add project notes, shot lists, delivery specs\u{2026}")
                            .frame(false),
                    );
                    if response.changed() {
                        state.dirty = true;
                    }
                });
        });
}
