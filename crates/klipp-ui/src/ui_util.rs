//! Shared egui layout helpers.

/// Allocate a child UI region whose `max_rect` AND `clip_rect` are both set
/// to the requested rect (intersected with the parent clip rect).
///
/// Use this instead of `ui.allocate_new_ui(UiBuilder::new().max_rect(r), ...)`
/// followed by a manual `ui.set_clip_rect(...)`. It prevents child widgets
/// from painting outside their allocated rect — the root cause of the
/// historical status-bar bleed bugs.
pub fn allocate_clipped_ui<R>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let clipped_rect = rect.intersect(ui.clip_rect());
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.set_clip_rect(clipped_rect);
        add_contents(ui)
    })
    .inner
}
