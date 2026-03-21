use crate::theme::KlippTheme;

/// Custom styled slider matching the design spec: 3px track, accent fill, 11px circle thumb with glow.
/// Returns the new value if changed.
pub fn styled_slider(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    width: f32,
) -> bool {
    let desired_size = egui::vec2(width, 16.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click_and_drag());

    let track_y = rect.center().y;
    let track_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, track_y - 1.5),
        egui::vec2(rect.width(), 3.0),
    );

    // Handle input
    let mut changed = false;
    if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
            let min = *range.start();
            let max = *range.end();
            *value = min + t * (max - min);
            changed = true;
        }
    }

    let min = *range.start();
    let max = *range.end();
    let t = if (max - min).abs() > f32::EPSILON {
        (*value - min) / (max - min)
    } else {
        0.0
    };
    let thumb_x = rect.min.x + t * rect.width();

    let painter = ui.painter();

    // Track background
    painter.rect_filled(track_rect, 2.0, theme.bg_raised);

    // Filled portion
    if t > 0.0 {
        let fill_rect = egui::Rect::from_min_size(
            track_rect.min,
            egui::vec2(t * track_rect.width(), 3.0),
        );
        painter.rect_filled(fill_rect, 2.0, theme.accent);
    }

    // Thumb
    let thumb_center = egui::pos2(thumb_x, track_y);
    // Glow
    painter.circle_filled(thumb_center, 7.0, theme.accent_glow);
    // Solid thumb
    painter.circle_filled(thumb_center, 5.5, theme.accent);

    changed
}
