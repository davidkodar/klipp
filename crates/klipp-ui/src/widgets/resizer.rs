use crate::theme::KlippTheme;
use egui::{CursorIcon, Rect, Sense, Ui};

/// Draw a vertical resizer strip and return the horizontal drag delta.
/// Idle: subtle border color. Hover/drag: accent highlight.
/// The hit area is wider than the visible strip for easier grabbing.
pub fn vertical_resizer(ui: &mut Ui, rect: Rect, theme: &KlippTheme) -> f32 {
    // Expand hit area to 12px wide (centered on the visual strip) for easier grabbing
    let hit_rect = rect.expand2(egui::vec2(4.0, 0.0));
    let response = ui.allocate_rect(hit_rect, Sense::drag());
    let painter = ui.painter();

    if response.hovered() || response.dragged() {
        painter.rect_filled(rect, 0.0, theme.accent);
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    } else {
        painter.rect_filled(rect, 0.0, theme.border);
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

    if response.hovered() || response.dragged() {
        painter.rect_filled(rect, 0.0, theme.accent);
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    } else {
        painter.rect_filled(rect, 0.0, theme.border);
    }

    if response.dragged() {
        response.drag_delta().y
    } else {
        0.0
    }
}
