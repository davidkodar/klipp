/// FCP-style scrubby slider parameter row for the inspector.
///
/// The core feature: dragging the **label** left/right changes the value.
/// This provides a compact, precise control without a traditional slider track.

use crate::icons::Icon;

/// Returned by scrubby_param_row() so callers can check what happened.
pub struct ScrubbyParamResponse {
    pub changed: bool,          // value was modified (drag, label-drag, type, or reset)
    pub drag_started: bool,     // mouse-down on DragValue or label began a drag gesture
    pub drag_stopped: bool,     // mouse-up ended a drag gesture
    pub lost_focus: bool,       // text entry finished (Enter/Tab/click away after typing)
    pub reset_clicked: bool,    // user clicked the reset-to-default indicator
}

/// Renders a single inspector parameter row with FCP-style scrubby interaction.
///
/// Layout: [Label (draggable)] [range fill] [DragValue + suffix] [reset icon] [diamond stub]
///
/// Behaviour:
/// - Dragging the LABEL left/right changes the value (the "scrubby" part)
/// - Hovering the label shows resize cursor
/// - The DragValue is also independently draggable and double-click-to-type
/// - Hovering the row reveals a small reset-to-default icon (Phosphor ArrowCounterClockwise)
/// - A subtle range indicator bar shows where the value sits within its range
/// - Shift held during drag: 1/10 speed (fine, built into DragValue)
/// - Alt held during drag: 10x speed (coarse, applied via modifier check)
pub fn scrubby_param_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default: f32,
    speed: f64,
    decimals: usize,
    suffix: &str,
) -> ScrubbyParamResponse {
    let old_value = *value;
    let is_at_default = (*value - default).abs() < f32::EPSILON;

    // Read modifiers once for this frame
    let modifiers = ui.input(|i| i.modifiers);
    let effective_speed = if modifiers.alt {
        speed * 10.0   // Alt: coarse mode (10x)
    } else {
        speed
        // Shift: fine mode (1/10) is handled automatically by DragValue
    };

    let mut dv_response = None;
    let mut label_drag_started = false;
    let mut label_drag_stopped = false;
    let mut reset_clicked = false;

    // Use a persistent ID to track whether the row was hovered last frame,
    // preventing the reset button from flickering when the mouse moves to it.
    let row_id = ui.id().with(label).with("scrubby_row_hover");
    let was_row_hovered: bool = ui.ctx().data_mut(|d| *d.get_temp_mut_or_default(row_id));

    let mut label_rect_out = egui::Rect::NOTHING;

    let _row_response = ui.horizontal(|ui| {
        // Reserve space: label takes left portion, value + reset + diamond on right
        let available = ui.available_width();
        let value_width = 70.0;
        let icon_width = 18.0;
        let diamond_width = 16.0;
        let label_width = (available - value_width - icon_width - diamond_width - 12.0).max(40.0);

        // ---- LABEL (draggable -- this is the scrubby slider) ----
        // Use allocate_exact_size instead of Label to prevent text selection on drag.
        let (label_rect_inner, label_rect) = ui.allocate_exact_size(
            egui::vec2(label_width, 18.0),
            egui::Sense::click_and_drag(),
        );

        label_rect_out = label_rect_inner;

        // Paint the label text manually (no text selection possible)
        let label_color = if label_rect.hovered() || label_rect.dragged() {
            egui::Color32::from_rgb(190, 190, 195)
        } else {
            egui::Color32::from_rgb(160, 160, 165)
        };
        ui.painter().text(
            egui::pos2(label_rect_inner.left() + 4.0, label_rect_inner.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.0),
            label_color,
        );

        // Resize cursor on label hover
        if label_rect.hovered() || label_rect.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        // Apply label drag delta to value
        if label_rect.dragged() {
            let delta = label_rect.drag_delta().x as f64;
            let label_speed = if modifiers.shift_only() {
                effective_speed / 10.0   // Shift: fine
            } else {
                effective_speed
            };
            *value += (delta * label_speed) as f32;
            *value = value.clamp(*range.start(), *range.end());
        }
        label_drag_started = label_rect.drag_started();
        label_drag_stopped = label_rect.drag_stopped();

        // Click label to reset to default
        if label_rect.clicked() && !is_at_default {
            *value = default;
            reset_clicked = true;
        }

        // ---- DRAG VALUE (number, also draggable + double-click-to-type) ----
        let dv = egui::DragValue::new(value)
            .speed(effective_speed)
            .range(range.clone())
            .fixed_decimals(decimals)
            .suffix(suffix);

        let dv_resp = ui.add(dv);
        dv_response = Some(dv_resp);

        // ---- RESET ICON (always allocated, icon only painted when row hovered + non-default) ----
        let (reset_rect_inner, reset_rect) = ui.allocate_exact_size(
            egui::vec2(icon_width, 18.0),
            egui::Sense::click(),
        );

        // Check hover on the entire row including the reset area itself
        let row_is_hovered = label_rect.hovered() || label_rect.dragged()
            || dv_response.as_ref().map_or(false, |r| r.hovered())
            || reset_rect.hovered();

        // Show the reset icon if hovered (this frame or last frame, to prevent flicker)
        let show_reset = (row_is_hovered || was_row_hovered) && !is_at_default;
        if show_reset {
            let tint = if reset_rect.hovered() {
                egui::Color32::from_rgb(220, 220, 225)
            } else {
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 80)
            };
            let icon_rect = egui::Rect::from_center_size(reset_rect_inner.center(), egui::vec2(11.0, 11.0));
            Icon::ArrowCounterClockwise.image(11.0).tint(tint).paint_at(ui, icon_rect);

            if reset_rect.clicked() {
                *value = default;
                reset_clicked = true;
            }
        }

        // Store hover state for next frame (prevents reset button flicker)
        ui.ctx().data_mut(|d| d.insert_temp(row_id, row_is_hovered));

        // ---- KEYFRAME DIAMOND STUB (Phosphor Diamond, always grey -- no keyframing yet) ----
        let (diamond_rect_inner, _diamond_resp) = ui.allocate_exact_size(
            egui::vec2(diamond_width, 18.0),
            egui::Sense::hover(),
        );
        let diamond_tint = egui::Color32::from_rgba_premultiplied(255, 255, 255, 40);
        let diamond_icon_rect = egui::Rect::from_center_size(diamond_rect_inner.center(), egui::vec2(10.0, 10.0));
        Icon::Diamond.image(10.0).tint(diamond_tint).paint_at(ui, diamond_icon_rect);
    });

    // ---- RANGE INDICATOR (subtle fill bar under the label area only) ----
    let range_span = *range.end() - *range.start();
    if range_span > 0.0 && label_rect_out.width() > 0.0 {
        let t = ((*value - *range.start()) / range_span).clamp(0.0, 1.0);
        let bar_height = 2.0;
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(label_rect_out.left(), label_rect_out.bottom() - bar_height),
            egui::vec2(label_rect_out.width() * t, bar_height),
        );
        let bar_color = egui::Color32::from_rgba_premultiplied(255, 180, 50, 35);
        ui.painter().rect_filled(bar_rect, 1.0, bar_color);
    }

    let dv_resp = dv_response.unwrap();
    let value_changed = (*value - old_value).abs() > f32::EPSILON;

    ScrubbyParamResponse {
        changed: dv_resp.changed() || value_changed || reset_clicked,
        drag_started: dv_resp.drag_started() || label_drag_started,
        drag_stopped: dv_resp.drag_stopped() || label_drag_stopped,
        lost_focus: dv_resp.lost_focus(),
        reset_clicked,
    }
}
