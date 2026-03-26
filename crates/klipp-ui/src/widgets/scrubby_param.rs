/// FCP-style scrubby slider parameter row for the inspector.
///
/// The core feature: dragging the **label** left/right changes the value.
/// This provides a compact, precise control without a traditional slider track.

use crate::icons::Icon;

/// State passed to indicate keyframe status for the diamond button.
pub struct KeyframeState {
    /// A keyframe exists at the current playhead time.
    pub has_keyframe_at_playhead: bool,
    /// The parameter has any keyframes at all.
    pub has_any_keyframes: bool,
    /// Time of the previous keyframe (relative to clip), if any.
    pub prev_keyframe_time: Option<f64>,
    /// Time of the next keyframe (relative to clip), if any.
    pub next_keyframe_time: Option<f64>,
}

/// Returned by scrubby_param_row() so callers can check what happened.
pub struct ScrubbyParamResponse {
    pub changed: bool,          // value was modified (drag, label-drag, type, or reset)
    pub drag_started: bool,     // mouse-down on DragValue or label began a drag gesture
    pub drag_stopped: bool,     // mouse-up ended a drag gesture
    pub lost_focus: bool,       // text entry finished (Enter/Tab/click away after typing)
    pub reset_clicked: bool,    // user clicked the reset-to-default indicator
    pub keyframe_toggled: bool, // user clicked the diamond to add/remove keyframe
    pub keyframe_nav_prev: bool, // user clicked previous keyframe button
    pub keyframe_nav_next: bool, // user clicked next keyframe button
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
    keyframe_state: Option<KeyframeState>,
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
    let mut keyframe_toggled = false;
    let mut nav_prev = false;
    let mut nav_next = false;

    // Use a persistent ID to track whether the row was hovered last frame,
    // preventing the reset button from flickering when the mouse moves to it.
    let row_id = ui.id().with(label).with("scrubby_row_hover");
    let was_row_hovered: bool = ui.ctx().data_mut(|d| *d.get_temp_mut_or_default(row_id));

    let mut label_rect_out = egui::Rect::NOTHING;

    let _row_response = ui.horizontal(|ui| {
        // Reserve space: label takes left portion, value + reset + nav arrows + diamond on right
        let available = ui.available_width();
        let value_width = 70.0;
        let icon_width = 18.0;
        let nav_arrow_width = 10.0; // per arrow
        let diamond_width = 16.0;
        let has_nav = keyframe_state.as_ref().map_or(false, |ks| ks.has_any_keyframes);
        let nav_total = if has_nav { nav_arrow_width * 2.0 } else { 0.0 };
        let label_width = (available - value_width - icon_width - nav_total - diamond_width - 12.0).max(40.0);

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

        // ---- KEYFRAME NAV + DIAMOND ----
        let is_keyframeable = keyframe_state.is_some();

        // Previous keyframe arrow (only when keyframes exist and prev is available)
        if let Some(ref ks) = keyframe_state {
            if ks.has_any_keyframes {
                let has_prev = ks.prev_keyframe_time.is_some();
                let (prev_rect, prev_resp) = ui.allocate_exact_size(
                    egui::vec2(nav_arrow_width, 18.0),
                    if has_prev { egui::Sense::click() } else { egui::Sense::hover() },
                );
                let arrow_color = if !has_prev {
                    egui::Color32::from_rgb(60, 60, 65)
                } else if prev_resp.hovered() {
                    egui::Color32::from_rgb(240, 180, 40)
                } else {
                    egui::Color32::from_rgb(140, 140, 145)
                };
                let icon_rect = egui::Rect::from_center_size(prev_rect.center(), egui::vec2(8.0, 8.0));
                Icon::CaretLeft.image(8.0).tint(arrow_color).paint_at(ui, icon_rect);
                if has_prev && prev_resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if has_prev && prev_resp.clicked() {
                    nav_prev = true;
                }
            }
        }

        // Diamond button
        let diamond_btn_resp = ui.add(
            egui::Button::new("")
                .frame(false)
                .min_size(egui::vec2(diamond_width, 18.0))
                .sense(if is_keyframeable { egui::Sense::click() } else { egui::Sense::hover() })
        );

        // Determine diamond color and fill based on keyframe state
        let (diamond_color, diamond_filled) = if let Some(ref ks) = keyframe_state {
            let hovered = diamond_btn_resp.hovered();
            if ks.has_keyframe_at_playhead {
                let c = if hovered { egui::Color32::from_rgb(255, 200, 60) } else { egui::Color32::from_rgb(240, 180, 40) };
                (c, true)
            } else if ks.has_any_keyframes {
                let c = if hovered { egui::Color32::from_rgb(240, 180, 40) } else { egui::Color32::from_rgba_premultiplied(240, 180, 40, 140) };
                (c, false)
            } else {
                let c = if hovered { egui::Color32::from_rgb(180, 180, 185) } else { egui::Color32::from_rgb(100, 100, 105) };
                (c, false)
            }
        } else {
            (egui::Color32::from_rgb(50, 50, 55), false)
        };

        // Draw diamond shape over the button rect
        let center = diamond_btn_resp.rect.center();
        let s = 4.5;
        let diamond_points = vec![
            egui::pos2(center.x, center.y - s),
            egui::pos2(center.x + s, center.y),
            egui::pos2(center.x, center.y + s),
            egui::pos2(center.x - s, center.y),
        ];
        if diamond_filled {
            ui.painter().add(egui::Shape::convex_polygon(diamond_points, diamond_color, egui::Stroke::NONE));
        } else {
            ui.painter().add(egui::Shape::closed_line(diamond_points, egui::Stroke::new(1.2, diamond_color)));
        }

        if is_keyframeable && diamond_btn_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        if is_keyframeable && diamond_btn_resp.clicked() {
            keyframe_toggled = true;
        }

        // Next keyframe arrow (only when keyframes exist and next is available)
        if let Some(ref ks) = keyframe_state {
            if ks.has_any_keyframes {
                let has_next = ks.next_keyframe_time.is_some();
                let (next_rect, next_resp) = ui.allocate_exact_size(
                    egui::vec2(nav_arrow_width, 18.0),
                    if has_next { egui::Sense::click() } else { egui::Sense::hover() },
                );
                let arrow_color = if !has_next {
                    egui::Color32::from_rgb(60, 60, 65)
                } else if next_resp.hovered() {
                    egui::Color32::from_rgb(240, 180, 40)
                } else {
                    egui::Color32::from_rgb(140, 140, 145)
                };
                let icon_rect = egui::Rect::from_center_size(next_rect.center(), egui::vec2(8.0, 8.0));
                Icon::CaretRight.image(8.0).tint(arrow_color).paint_at(ui, icon_rect);
                if has_next && next_resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if has_next && next_resp.clicked() {
                    nav_next = true;
                }
            }
        }
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
        keyframe_toggled,
        keyframe_nav_prev: nav_prev,
        keyframe_nav_next: nav_next,
    }
}
