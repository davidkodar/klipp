use crate::theme::KlippTheme;
use egui::{CursorIcon, Rect, Sense, Ui};

/// Draw a vertical resizer strip and return the horizontal drag delta.
/// Idle: subtle border color. Hover/drag: accent highlight.
/// The hit area is wider than the visible strip for easier grabbing.
pub fn vertical_resizer(ui: &mut Ui, rect: Rect, theme: &KlippTheme) -> f32 {
    // Hit rect equals the visual strip — no expansion. Previously the hit was
    // expanded ±4px horizontally, which allocated input space inside adjacent
    // panels and registered hover/drag interactions there. The 5px strip is
    // grabbable enough on its own.
    let response = ui.allocate_rect(rect, Sense::drag());
    let painter = ui.painter();

    let active = response.hovered() || response.dragged();
    if active {
        painter.rect_filled(rect, 0.0, theme.accent);
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    } else {
        painter.rect_filled(rect, 0.0, theme.resizer_idle);
        // Center groove: 1px dark vertical line down the middle of the strip,
        // gives the strip a tactile "grabbable" affordance in its idle state.
        let center_x = rect.center().x;
        painter.line_segment(
            [egui::pos2(center_x, rect.top()), egui::pos2(center_x, rect.bottom())],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(16, 16, 20)),
        );
    }

    if response.dragged() {
        response.drag_delta().x
    } else {
        0.0
    }
}

/// Draw a horizontal resizer strip and return the vertical drag delta.
pub fn horizontal_resizer(ui: &mut Ui, rect: Rect, theme: &KlippTheme) -> f32 {
    let response = ui.allocate_rect(rect, Sense::drag());
    let painter = ui.painter();

    let active = response.hovered() || response.dragged();
    if active {
        painter.rect_filled(rect, 0.0, theme.accent);
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    } else {
        painter.rect_filled(rect, 0.0, theme.resizer_idle);
        // Center groove: 1px dark horizontal line across the middle of the strip.
        let center_y = rect.center().y;
        painter.line_segment(
            [egui::pos2(rect.left(), center_y), egui::pos2(rect.right(), center_y)],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(16, 16, 20)),
        );
    }

    if response.dragged() {
        response.drag_delta().y
    } else {
        0.0
    }
}
