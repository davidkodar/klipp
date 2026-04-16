use egui::{Response, Sense, Ui, Widget, vec2, Color32, Rounding, Stroke};

/// iOS-style toggle switch widget.
pub fn toggle(on: &mut bool) -> impl Widget + '_ {
    move |ui: &mut Ui| -> Response { toggle_ui(ui, on) }
}

fn toggle_ui(ui: &mut Ui, on: &mut bool) -> Response {
    let desired_size = vec2(40.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, Sense::click());

    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }

    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_with_time(response.id, *on, 0.15);
        let bg = Color32::from_rgb(
            (0x2a as f32 + (0xe8 - 0x2a) as f32 * how_on) as u8,
            (0x2a as f32 + (0xa0 - 0x2a) as f32 * how_on) as u8,
            (0x32 as f32 + (0x20 - 0x32) as f32 * how_on.min(0.0_f32.max(how_on))) as u8,
        );
        let rounding = rect.height() / 2.0;
        ui.painter().rect_filled(rect, Rounding::same(rounding), bg);
        ui.painter().rect_stroke(rect, Rounding::same(rounding), Stroke::new(1.0_f32, Color32::from_rgb(0x38, 0x38, 0x42)));

        let circle_r = rect.height() / 2.0 - 2.0;
        let circle_x = egui::lerp(rect.left() + circle_r + 3.0..=rect.right() - circle_r - 3.0, how_on);
        let circle_center = egui::pos2(circle_x, rect.center().y);
        ui.painter().circle_filled(circle_center, circle_r, Color32::WHITE);
    }

    response
}
