//! FCP-style scrubby slider parameter row for the inspector.
//!
//! The core feature: dragging the **label** left/right changes the value.
//! This provides a compact, precise control without a traditional slider
//! track.

use crate::icons::Icon;

/// Preset ease curves applied as Bezier with specific tangent ratios.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EasePreset {
    /// Slow start, fast end — CSS ease-in (0.42, 0, 1, 1)
    EaseIn,
    /// Fast start, slow end — CSS ease-out (0, 0, 0.58, 1)
    EaseOut,
    /// Slow start and end — CSS ease-in-out (0.42, 0, 0.58, 1)
    EaseBoth,
}

impl EasePreset {
    /// Returns (tangent_out_ratio, tangent_in_ratio) where each is multiplied by the segment span.
    /// tangent_out is the out-tangent at the start of the segment (positive X direction).
    /// tangent_in is the in-tangent at the end of the segment (negative X direction).
    pub fn tangent_ratios(self) -> (f64, f64) {
        match self {
            EasePreset::EaseIn => (0.42, 0.0),
            EasePreset::EaseOut => (0.0, 0.42),
            EasePreset::EaseBoth => (0.42, 0.42),
        }
    }

    /// Compute the tangent values for a given keyframe index within a keyframe array.
    /// Returns (tangent_in, tangent_out) suitable for a SetTangent command.
    pub fn compute_tangents(self, keyframes: &[klipp_core::Keyframe], ki: usize) -> (Option<(f64, f32)>, Option<(f64, f32)>) {
        let (out_ratio, in_ratio) = self.tangent_ratios();
        let len = keyframes.len();
        if ki >= len {
            return (None, None);
        }
        let prev_t = if ki > 0 { keyframes[ki - 1].time_secs } else { keyframes[ki].time_secs - 1.0 };
        let next_t = if ki + 1 < len { keyframes[ki + 1].time_secs } else { keyframes[ki].time_secs + 1.0 };
        let span_out = (next_t - keyframes[ki].time_secs).abs();
        let span_in = (keyframes[ki].time_secs - prev_t).abs();
        (
            Some((-span_in * in_ratio, 0.0f32)),
            Some((span_out * out_ratio, 0.0f32)),
        )
    }
}

/// State passed to indicate keyframe status for the diamond button.
#[derive(Clone, Copy)]
pub struct KeyframeState {
    /// A keyframe exists at the current playhead time.
    pub has_keyframe_at_playhead: bool,
    /// The parameter has any keyframes at all.
    pub has_any_keyframes: bool,
    /// Time of the previous keyframe (relative to clip), if any.
    pub prev_keyframe_time: Option<f64>,
    /// Time of the next keyframe (relative to clip), if any.
    pub next_keyframe_time: Option<f64>,
    /// Interpolation type of the keyframe at the playhead (if any).
    pub current_interpolation: Option<klipp_core::Interpolation>,
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
    pub keyframe_set_interpolation: Option<klipp_core::Interpolation>, // right-click menu selected interpolation
    pub keyframe_ease_preset: Option<EasePreset>, // right-click menu selected ease preset
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
    let mut interp_changed: Option<klipp_core::Interpolation> = None;
    let mut ease_preset: Option<EasePreset> = None;
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
        // Reserve space: label takes left portion, value + reset + nav arrows + diamond on right.
        // The trailing cluster is rendered with `Layout::right_to_left` (see below) so it always
        // packs to the row's right edge — that's why the only thing we need to compute here is
        // the LABEL width. Subtracting all the right-cluster widget widths plus the actual
        // `item_spacing.x` between them keeps the row total under `available_width` and prevents
        // the parent Frame from auto-growing every row (which would compound row-by-row into a
        // pyramid of ever-wider rows).
        let available = ui.available_width();
        let value_width = 70.0;
        let icon_width = 18.0;
        let nav_arrow_width = 10.0; // per arrow
        let diamond_width = 16.0;
        let has_nav = keyframe_state.as_ref().is_some_and(|ks| ks.has_any_keyframes);
        let item_spacing = ui.spacing().item_spacing.x;
        let nav_total = if has_nav { nav_arrow_width * 2.0 } else { 0.0 };
        // Right cluster contains: value + reset + (prev arrow if has_nav) + diamond + (next arrow if has_nav).
        // That's 3 widgets without nav, 5 with nav. Spacings between them = (n-1) * item_spacing,
        // plus one item_spacing between the label and the cluster.
        let right_cluster_widget_count = if has_nav { 5 } else { 3 };
        let right_cluster_spacings = (right_cluster_widget_count as f32) * item_spacing;
        let label_width = (available
            - value_width - icon_width - nav_total - diamond_width
            - right_cluster_spacings
        ).max(40.0);

        // ---- LABEL (draggable -- this is the scrubby slider) ----
        // Use allocate_exact_size instead of Label to prevent text selection on drag.
        let (label_rect_inner, label_rect) = ui.allocate_exact_size(
            egui::vec2(label_width, 22.0),
            egui::Sense::click_and_drag(),
        );

        label_rect_out = label_rect_inner;

        // Paint the label text manually (no text selection possible)
        let label_color = if label_rect.hovered() || label_rect.dragged() {
            ui.visuals().widgets.hovered.fg_stroke.color
        } else {
            ui.visuals().widgets.inactive.fg_stroke.color
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

        // ---- TRAILING CLUSTER (right-to-left, anchored to row's right edge) ----
        // Visual order from left to right: value | reset | prev | diamond | next
        // Right-to-left layout means the FIRST widget added is RIGHTMOST, so we
        // add in reverse: next → diamond → prev → reset → value.
        // This keeps the diamond column lined up across rows regardless of how
        // wide the value string ("8" vs "0.500 dB" vs "1.00") renders.
        let is_keyframeable = keyframe_state.is_some();
        let mut diamond_btn_resp_opt: Option<egui::Response> = None;
        let mut reset_rect_inner: egui::Rect = egui::Rect::NOTHING;
        let mut reset_rect_resp: Option<egui::Response> = None;

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Next keyframe arrow (rightmost — only when keyframes exist)
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
                        ui.visuals().selection.stroke.color
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

            // Diamond button (left of next arrow)
            let diamond_btn_resp = ui.add(
                egui::Button::new("")
                    .frame(false)
                    .min_size(egui::vec2(diamond_width, 18.0))
                    .sense(if is_keyframeable { egui::Sense::click() } else { egui::Sense::hover() })
            );
            diamond_btn_resp_opt = Some(diamond_btn_resp);

            // Previous keyframe arrow (left of diamond)
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
                        ui.visuals().selection.stroke.color
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

            // Reset icon (left of prev arrow)
            let (rri, rr) = ui.allocate_exact_size(
                egui::vec2(icon_width, 22.0),
                egui::Sense::click(),
            );
            reset_rect_inner = rri;
            reset_rect_resp = Some(rr);

            // Drag value (leftmost in cluster)
            let dv = egui::DragValue::new(value)
                .speed(effective_speed)
                .range(range.clone())
                .fixed_decimals(decimals)
                .suffix(suffix);
            let dv_resp = ui.add(dv);
            dv_response = Some(dv_resp);
        });

        let diamond_btn_resp = diamond_btn_resp_opt.expect("diamond always allocated");
        let reset_rect = reset_rect_resp.expect("reset always allocated");

        // Check hover on the entire row including the reset area itself
        let row_is_hovered = label_rect.hovered() || label_rect.dragged()
            || dv_response.as_ref().is_some_and(|r| r.hovered())
            || reset_rect.hovered();

        // Show the reset icon if hovered (this frame or last frame, to prevent flicker)
        let show_reset = (row_is_hovered || was_row_hovered) && !is_at_default;
        if show_reset {
            let tint = if reset_rect.hovered() {
                ui.visuals().widgets.hovered.fg_stroke.color
            } else {
                // TODO: when scrubby_param_row gains a theme
                // parameter, replace with theme.overlay(80) so the
                // idle reset icon has correct contrast on Light
                // theme. Currently safe-leaving as polarity-neutral
                // translucent white because the alternative
                // (theme.overlay) requires plumbing theme through
                // every caller of this widely-reused widget.
                ui.visuals().widgets.inactive.fg_stroke.color
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

        // Determine diamond color, fill, and stroke width based on keyframe state.
        // Three visual states:
        //   1. At playhead: filled bright yellow diamond
        //   2. Has keyframes (not at playhead): hollow, full-alpha yellow, thicker stroke (1.8px)
        //   3. No keyframes: hollow, dim gray, thin stroke (1.2px)
        let (diamond_color, diamond_filled, diamond_stroke_width) = if let Some(ref ks) = keyframe_state {
            let hovered = diamond_btn_resp.hovered();
            if ks.has_keyframe_at_playhead {
                let c = if hovered { ui.visuals().selection.stroke.color.gamma_multiply(1.25) } else { ui.visuals().selection.stroke.color };
                (c, true, 0.0_f32) // filled — stroke width unused
            } else if ks.has_any_keyframes {
                // Full-alpha yellow with thicker stroke — clearly "active but not here"
                let c = if hovered { ui.visuals().selection.stroke.color.gamma_multiply(1.25) } else { ui.visuals().selection.stroke.color };
                (c, false, 1.8_f32)
            } else {
                let c = if hovered { egui::Color32::from_rgb(180, 180, 185) } else { egui::Color32::from_rgb(100, 100, 105) };
                (c, false, 1.2_f32)
            }
        } else {
            (egui::Color32::from_rgb(50, 50, 55), false, 1.2_f32)
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
            ui.painter().add(egui::Shape::closed_line(diamond_points, egui::Stroke::new(diamond_stroke_width, diamond_color)));
        }

        if let Some(ref ks) = keyframe_state {
            if !ks.has_any_keyframes {
                diamond_btn_resp.clone().on_hover_text("Click to enable keyframing");
            } else if ks.has_keyframe_at_playhead {
                diamond_btn_resp.clone().on_hover_text("Remove keyframe");
            } else {
                diamond_btn_resp.clone().on_hover_text("Add keyframe at playhead");
            }
        }

        if is_keyframeable && diamond_btn_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        if is_keyframeable && diamond_btn_resp.clicked() {
            keyframe_toggled = true;
        }

        // Right-click context menu on diamond for interpolation type
        if is_keyframeable && keyframe_state.as_ref().is_some_and(|ks| ks.has_keyframe_at_playhead) {
            let cur_interp = keyframe_state.as_ref().and_then(|ks| ks.current_interpolation);
            diamond_btn_resp.clone().context_menu(|ui| {
                ui.set_min_width(140.0);
                let check = |active: bool| if active { "  * " } else { "    " };
                let is_bezier = cur_interp == Some(klipp_core::Interpolation::Bezier);
                if ui.button(format!("{}Linear", check(cur_interp == Some(klipp_core::Interpolation::Linear)))).clicked() {
                    interp_changed = Some(klipp_core::Interpolation::Linear);
                    ui.close_menu();
                }
                if ui.button(format!("{}Bezier (Custom)", check(is_bezier))).clicked() {
                    interp_changed = Some(klipp_core::Interpolation::Bezier);
                    ui.close_menu();
                }
                if ui.button(format!("{}Ease In", check(false))).clicked() {
                    ease_preset = Some(EasePreset::EaseIn);
                    ui.close_menu();
                }
                if ui.button(format!("{}Ease Out", check(false))).clicked() {
                    ease_preset = Some(EasePreset::EaseOut);
                    ui.close_menu();
                }
                if ui.button(format!("{}Ease Both", check(false))).clicked() {
                    ease_preset = Some(EasePreset::EaseBoth);
                    ui.close_menu();
                }
                if ui.button(format!("{}Step (Hold)", check(cur_interp == Some(klipp_core::Interpolation::Step)))).clicked() {
                    interp_changed = Some(klipp_core::Interpolation::Step);
                    ui.close_menu();
                }
            });
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
        keyframe_set_interpolation: interp_changed,
        keyframe_ease_preset: ease_preset,
    }
}
