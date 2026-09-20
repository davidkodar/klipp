use crate::theme::KlippTheme;
use egui::{CursorIcon, Rect, Sense, Ui};

/// Draw a vertical resizer strip and return the horizontal drag delta.
/// Idle: a single 1px line down the centre (DaVinci/FCP-style — modern and
/// thin). Hover/drag: the full hit strip lights up with the accent colour so
/// the affordance is obvious while interacting. The wider hit area makes it
/// easy to grab without bloating the visual divider.
pub fn vertical_resizer(ui: &mut Ui, rect: Rect, theme: &KlippTheme) -> f32 {
    vertical_resizer_full(ui, rect, theme).0
}

/// What a vertical resizer was asked to do: a drag delta, and whether it
/// was double-clicked.
///
/// Double-click to collapse is the gesture every pane-based editor
/// shares — Resolve, Premiere, VS Code, every IDE — and it costs
/// nothing to honour, because the divider is already the thing under
/// the pointer when someone wants the panel gone.
pub fn vertical_resizer_full(ui: &mut Ui, rect: Rect, theme: &KlippTheme) -> (f32, bool) {
    let response = ui.allocate_rect(rect, Sense::click_and_drag());
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

    let delta = if response.dragged() { response.drag_delta().x } else { 0.0 };
    (delta, response.double_clicked())
}

/// Draw a horizontal resizer strip and return the vertical drag delta.
pub fn horizontal_resizer(ui: &mut Ui, rect: Rect, theme: &KlippTheme) -> f32 {
    horizontal_resizer_with_idle_color(ui, rect, theme, theme.border)
}

/// Like [`horizontal_resizer`] but the idle 1 px line is painted in the
/// given colour instead of the default `theme.border`. Callers adjacent
/// to a black scope dock pass `theme.bg_void` so the resizer line
/// blends into the scope's own background and reads as a flush edge.
pub fn horizontal_resizer_with_idle_color(
    ui: &mut Ui,
    rect: Rect,
    theme: &KlippTheme,
    idle_color: egui::Color32,
) -> f32 {
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
            egui::Stroke::new(1.0_f32, idle_color),
        );
    }

    if response.dragged() {
        response.drag_delta().y
    } else {
        0.0
    }
}
