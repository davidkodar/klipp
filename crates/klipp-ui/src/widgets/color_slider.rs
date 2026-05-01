use crate::theme::KlippTheme;
use std::ops::RangeInclusive;

pub struct ColorSliderResponse {
    pub changed: bool,
    pub drag_started: bool,
    pub drag_ended: bool,
}

/// Shared state for numeric input editing across all sliders.
#[derive(Default)]
pub struct SliderEditState {
    /// Which slider is currently in edit mode (by ID string).
    pub editing_id: Option<String>,
    /// Text buffer for the edit field.
    pub edit_buf: String,
}

/// Center-origin slider for color grading parameters.
/// Fill extends from the center-point (where `default_value` sits) toward the thumb.
/// Click the value label to type an exact number (requires `edit_state` and `slider_id`).
pub fn color_slider(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    default_value: f32,
    format_fn: fn(f32) -> String,
) -> ColorSliderResponse {
    color_slider_with_edit(ui, theme, label, value, range, default_value, format_fn, None)
}

/// Like `color_slider` but with optional numeric edit support.
/// Pass `Some((slider_id, edit_state))` to enable click-to-type on the value.
#[allow(clippy::too_many_arguments)]
pub fn color_slider_with_edit(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    default_value: f32,
    format_fn: fn(f32) -> String,
    edit: Option<(&str, &mut SliderEditState)>,
) -> ColorSliderResponse {
    let min = *range.start();
    let max = *range.end();

    // Layout: 82px label | flexible track (max 220px) | 44px value
    let available_width = ui.available_width();
    let label_w = 82.0;
    let value_w = 44.0;
    let track_w = (available_width - label_w - value_w - 16.0).clamp(40.0, 220.0);
    let total_w = label_w + track_w + value_w + 16.0;
    let row_height = 16.0;

    // Check if this slider is in edit mode
    let is_editing = edit.as_ref().is_some_and(|(id, es)| {
        es.editing_id.as_deref() == Some(*id)
    });

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(total_w, row_height),
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

    // Handle track input (only when not editing)
    let mut changed = false;
    if !is_editing {
        if response.dragged() || response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                // Only handle clicks on the track area, not the value area
                if pos.x >= track_left && pos.x <= track_left + track_w {
                    let t = ((pos.x - track_left) / track_w).clamp(0.0, 1.0);
                    *value = min + t * (max - min);
                    changed = true;
                }
            }
        }

        // Double-click on track resets to default
        if response.double_clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                if pos.x < track_left + track_w + 4.0 {
                    *value = default_value;
                    changed = true;
                }
            }
        }
    }

    let painter = ui.painter_at(rect);

    // Colors
    let text_dim = theme.text_dim;
    let text_accent = theme.accent;
    let track_bg = theme.bg_raised;
    let fill_color = text_accent;
    let thumb_fill = theme.text_primary;
    let glow_color = theme.accent_glow;

    // Label (11px, left-aligned)
    painter.text(
        egui::pos2(rect.min.x, rect.center().y),
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

    // Thumb
    let thumb_center = egui::pos2(thumb_x, track_y);
    painter.circle_filled(thumb_center, 6.0, glow_color);
    painter.circle_filled(thumb_center, 4.5, thumb_fill);
    painter.circle_stroke(thumb_center, 4.5, egui::Stroke::new(1.5_f32, text_accent));

    // Value display / edit area
    let value_rect = egui::Rect::from_min_size(
        egui::pos2(rect.max.x - value_w, rect.min.y),
        egui::vec2(value_w, row_height),
    );

    if is_editing {
        // ── Inline text edit mode ──
        if let Some((_id, edit_state)) = edit {
            let edit_rect = egui::Rect::from_min_size(
                egui::pos2(value_rect.min.x, value_rect.min.y + 1.0),
                egui::vec2(value_w, row_height - 2.0),
            );

            // Background for edit field
            painter.rect_filled(edit_rect, 2.0, theme.bg_deep);
            painter.rect_stroke(edit_rect, 2.0, egui::Stroke::new(1.0_f32, text_accent));

            let edit_resp = ui.allocate_new_ui(
                egui::UiBuilder::new().max_rect(edit_rect),
                |ui| {
                    let te = egui::TextEdit::singleline(&mut edit_state.edit_buf)
                        .font(egui::FontId::monospace(9.0))
                        .text_color(text_accent)
                        .desired_width(value_w - 4.0)
                        .margin(egui::Margin::symmetric(2.0, 0.0))
                        .frame(false);
                    let r = ui.add(te);
                    // Auto-focus on first frame
                    if !r.has_focus() {
                        r.request_focus();
                    }
                    r
                },
            ).inner;

            // Enter: commit value
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Ok(parsed) = edit_state.edit_buf.trim().parse::<f32>() {
                    *value = parsed.clamp(min, max);
                    changed = true;
                }
                edit_state.editing_id = None;
            }
            // Escape: cancel
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                edit_state.editing_id = None;
            }
            // Click elsewhere: commit
            if edit_resp.lost_focus() && !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                if let Ok(parsed) = edit_state.edit_buf.trim().parse::<f32>() {
                    *value = parsed.clamp(min, max);
                    changed = true;
                }
                edit_state.editing_id = None;
            }
        }
    } else {
        // ── Normal value display ──
        let is_default = (*value - default_value).abs() < 0.001;
        let value_text = format_fn(*value);
        let value_color = if is_default { text_dim } else { text_accent };

        painter.text(
            egui::pos2(value_rect.max.x, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            &value_text,
            egui::FontId::monospace(10.0),
            value_color,
        );

        // Value area interaction: click → edit mode, drag up/down → scrub value
        if let Some((id, edit_state)) = edit {
            let val_resp = ui.allocate_rect(value_rect, egui::Sense::click_and_drag());

            // Drag up/down on value to scrub (FCP-style)
            if val_resp.dragged() {
                let delta_y = val_resp.drag_delta().y;
                // Drag up = increase, down = decrease. Sensitivity: 1px = 0.5% of range
                let step = -delta_y * range_span * 0.005;
                *value = (*value + step).clamp(min, max);
                changed = true;
                // Set cursor to vertical resize to indicate scrub mode
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            }

            if val_resp.drag_started() {
                // Treat scrub start as drag_started for undo
                // (drag_started/drag_ended from the main response won't fire here)
            }

            // Click (released without drag) → enter text edit mode
            if val_resp.clicked() {
                edit_state.editing_id = Some(id.to_string());
                edit_state.edit_buf = format_fn(*value);
            }

            // Double-click on value → reset to default
            if val_resp.double_clicked() {
                *value = default_value;
                changed = true;
            }
        }
    }

    ColorSliderResponse {
        changed,
        drag_started,
        drag_ended,
    }
}
