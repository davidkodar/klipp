use crate::theme::KlippTheme;
use std::ops::RangeInclusive;

pub struct ColorSliderResponse {
    pub changed: bool,
    pub drag_started: bool,
    pub drag_ended: bool,
}

/// Center-origin slider for color grading parameters.
/// Fill extends from the center-point (where `default_value` sits) toward the thumb.
/// Styled to match the color panel mockup.
pub fn color_slider(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    default_value: f32,
    format_fn: fn(f32) -> String,
) -> ColorSliderResponse {
    let min = *range.start();
    let max = *range.end();

    // Layout: 82px label | flexible track | 38px value
    let available_width = ui.available_width();
    let label_w = 82.0;
    let value_w = 38.0;
    let track_w = (available_width - label_w - value_w - 16.0).max(40.0);
    let row_height = 16.0;

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(available_width, row_height),
        egui::Sense::click_and_drag(),
    );

    let drag_started = response.drag_started();
    let drag_ended = response.drag_stopped();

    // Track rect
    let track_left = rect.min.x + label_w;
    let track_y = rect.center().y;
    let track_rect = egui::Rect::from_min_size(
        egui::pos2(track_left, track_y - 1.5),
        egui::vec2(track_w, 3.0),
    );

    // Handle input — only on track area
    let mut changed = false;
    if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.x - track_left) / track_w).clamp(0.0, 1.0);
            *value = min + t * (max - min);
            changed = true;
        }
    }

    // Double-click resets to default
    if response.double_clicked() {
        *value = default_value;
        changed = true;
    }

    let painter = ui.painter_at(rect);

    // Colors
    let text_dim = egui::Color32::from_rgb(0x90, 0x90, 0xa0);
    let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
    let track_bg = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);
    let fill_color = text_accent;
    let thumb_fill = egui::Color32::from_rgb(0xe8, 0xe8, 0xee);
    let glow_color = egui::Color32::from_rgba_unmultiplied(0xe8, 0xa0, 0x20, 38); // ~0.15 alpha

    // Label (11px, left-aligned)
    let label_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, rect.min.y),
        egui::vec2(label_w, row_height),
    );
    painter.text(
        egui::pos2(label_rect.min.x, label_rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(11.0),
        text_dim,
    );

    // Track background
    painter.rect_filled(track_rect, 2.0, track_bg);

    // Value normalized to track
    let range_span = max - min;
    let value_t = if range_span.abs() > f32::EPSILON {
        (*value - min) / range_span
    } else {
        0.0
    };
    let default_t = if range_span.abs() > f32::EPSILON {
        (default_value - min) / range_span
    } else {
        0.5
    };

    let thumb_x = track_left + value_t * track_w;
    let center_x = track_left + default_t * track_w;

    // Fill from center to thumb
    if (value_t - default_t).abs() > 0.001 {
        let fill_left = thumb_x.min(center_x);
        let fill_right = thumb_x.max(center_x);
        let fill_rect = egui::Rect::from_min_max(
            egui::pos2(fill_left, track_y - 1.5),
            egui::pos2(fill_right, track_y + 1.5),
        );
        painter.rect_filled(fill_rect, 2.0, fill_color);
    }

    // Thumb: 10px white circle, 1.5px amber border, glow ring
    let thumb_center = egui::pos2(thumb_x, track_y);
    painter.circle_filled(thumb_center, 7.5, glow_color); // glow
    painter.circle_filled(thumb_center, 5.0, thumb_fill);
    painter.circle_stroke(thumb_center, 5.0, egui::Stroke::new(1.5, text_accent));

    // Value text (10px monospace, right-aligned)
    let is_default = (*value - default_value).abs() < 0.001;
    let value_text = format_fn(*value);
    let value_color = if is_default { text_dim } else { text_accent };
    let value_rect_right = rect.max.x;
    painter.text(
        egui::pos2(value_rect_right, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        value_text,
        egui::FontId::monospace(10.0),
        value_color,
    );

    ColorSliderResponse {
        changed,
        drag_started,
        drag_ended,
    }
}
