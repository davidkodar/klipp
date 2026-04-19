use crate::theme::KlippTheme;
use egui::{CursorIcon, Rect, Sense, Ui};

/// Draw a vertical resizer strip and return the horizontal drag delta.
/// Idle: a single 1px line down the centre (DaVinci/FCP-style — modern and
/// thin). Hover/drag: the full hit strip lights up with the accent colour so
/// the affordance is obvious while interacting. The wider hit area makes it
/// easy to grab without bloating the visual divider.
pub fn vertical_resizer(ui: &mut Ui, rect: Rect, theme: &KlippTheme) -> f32 {
    let response = ui.allocate_rect(rect, Sense::drag());
    let painter = ui.painter();

    let active = response.hovered() || response.dragged();
    if active {
        painter.rect_filled(rect, 0.0, theme.accent);
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    } else {
        let center_x = rect.center().x.round();
        painter.line_segment(
            [egui::pos2(center_x, rect.top()), egui::pos2(center_x, rect.bottom())],
            egui::Stroke::new(1.0_f32, theme.border),
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
        let center_y = rect.center().y.round();
        painter.line_segment(
            [egui::pos2(rect.left(), center_y), egui::pos2(rect.right(), center_y)],
            egui::Stroke::new(1.0_f32, theme.border),
        );
    }

    if response.dragged() {
        response.drag_delta().y
    } else {
        0.0
    }
}
