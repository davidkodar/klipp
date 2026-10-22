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
    /// Number of authored keys within this displayed frame window.
    pub keys_in_frame: usize,
    /// Time of the previous keyframe (relative to clip), if any.
    pub prev_keyframe_time: Option<f64>,
    /// Time of the next keyframe (relative to clip), if any.
    pub next_keyframe_time: Option<f64>,
    /// Interpolation type of the keyframe at the playhead (if any).
    pub current_interpolation: Option<klipp_core::Interpolation>,
}

/// How wide a previous or next key arrow is: a target a finger of the
/// pointer can find.
const NAV_ARROW_W: f32 = 16.0;

/// A previous or next key arrow, [`NAV_ARROW_W`] wide: dim, still seen,
/// and inert with no key that way; its tip says which. Whether it was
/// clicked.
fn nav_arrow(ui: &mut egui::Ui, has_key: bool, icon: Icon) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(NAV_ARROW_W, 18.0), if has_key { egui::Sense::click() } else { egui::Sense::hover() });
    let next = matches!(icon, Icon::CaretRight);
    let tip = match (next, has_key) {
        (true, true) => "Next key",
        (false, true) => "Previous key",
        (true, false) => "No key after the playhead",
        (false, false) => "No key before the playhead",
    };
    let resp = resp.on_hover_text(tip);
    let colour = if !has_key {
        egui::Color32::from_rgb(95, 95, 100)
    } else if resp.hovered() {
        ui.visuals().selection.stroke.color
    } else {
        egui::Color32::from_rgb(140, 140, 145)
    };
    icon.image(8.0).tint(colour).paint_at(ui, egui::Rect::from_center_size(rect.center(), egui::vec2(8.0, 8.0)));
    if has_key && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    has_key && resp.clicked()
}

/// The keyframe diamond over `resp`'s rect, and its hover text. Three
/// looks: a key at the playhead, filled; keys elsewhere, hollow and
/// bright with a thicker stroke; no keys, hollow and dim. `None` (a
/// parameter that can't be keyed) draws a dark outline.
fn paint_diamond(ui: &egui::Ui, resp: &egui::Response, keyframe_state: Option<&KeyframeState>, keys: Option<usize>) {
    let (colour, filled, stroke_width) = if let Some(ks) = keyframe_state {
        let hovered = resp.hovered();
        if ks.has_keyframe_at_playhead {
            let c = if hovered { ui.visuals().selection.stroke.color.gamma_multiply(1.25) } else { ui.visuals().selection.stroke.color };
            (c, true, 0.0_f32)
        } else if ks.has_any_keyframes {
            // Animated, between keys: the accent's outline, thin enough to
            // read as hollow beside a key's filled diamond.
            let c = if hovered { ui.visuals().selection.stroke.color.gamma_multiply(1.25) } else { ui.visuals().selection.stroke.color };
            (c, false, 1.1_f32)
        } else {
            let c = if hovered { egui::Color32::from_rgb(180, 180, 185) } else { egui::Color32::from_rgb(100, 100, 105) };
            (c, false, 1.2_f32)
        }
    } else {
        (egui::Color32::from_rgb(50, 50, 55), false, 1.2_f32)
    };
    let center = resp.rect.center();
    let s = 4.5;
    let points = vec![egui::pos2(center.x, center.y - s), egui::pos2(center.x + s, center.y), egui::pos2(center.x, center.y + s), egui::pos2(center.x - s, center.y)];
    if filled {
        ui.painter().add(egui::Shape::convex_polygon(points, colour, egui::Stroke::NONE));
    } else {
        ui.painter().add(egui::Shape::closed_line(points, egui::Stroke::new(stroke_width, colour)));
    }
    if let Some(ks) = keyframe_state {
        // A row that knows its track says what the diamond and arrows do (#2296).
        let all = keys.and_then(|n| crate::param_focus::diamond_hover(n, ks.has_keyframe_at_playhead));
        if let Some(text) = all {
            resp.clone().on_hover_text(text);
        } else if ks.keys_in_frame > 1 {
            resp.clone().on_hover_text(format!("{} keys in this frame. This diamond edits or removes the nearest key; use the curve editor to select the others.", ks.keys_in_frame));
        } else if !ks.has_any_keyframes {
            resp.clone().on_hover_text("Click to enable keyframing");
        } else if ks.has_keyframe_at_playhead {
            resp.clone().on_hover_text("Remove keyframe");
        } else {
            resp.clone().on_hover_text("Add keyframe at playhead");
        }
    }
}

impl EasePreset {
    /// The side of a key this preset slows, as a key edited by path takes it.
    pub fn key_ease(self) -> klipp_core::KeyEase {
        match self {
            EasePreset::EaseIn => klipp_core::KeyEase::In,
            EasePreset::EaseOut => klipp_core::KeyEase::Out,
            EasePreset::EaseBoth => klipp_core::KeyEase::Both,
        }
    }
}

/// What a key's menu picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMenuPick {
    Interpolation(klipp_core::Interpolation),
    Ease(EasePreset),
}

/// A key's right-click menu, in the app's menu look: how the move leaves
/// the key (Linear, the eases, a custom Bezier, a hold), the one in force
/// shown selected. What was picked, closing the menu.
pub fn key_menu(ui: &mut egui::Ui, current: Option<klipp_core::Interpolation>) -> Option<KeyMenuPick> {
    use klipp_core::Interpolation;
    super::menu::menu_body(ui, |ui| {
        let mut pick = None;
        if super::menu::menu_choice(ui, "Linear", "", current == Some(Interpolation::Linear)).clicked() {
            pick = Some(KeyMenuPick::Interpolation(Interpolation::Linear));
        }
        for (label, preset) in [("Ease In", EasePreset::EaseIn), ("Ease Out", EasePreset::EaseOut), ("Ease Both", EasePreset::EaseBoth)] {
            if super::menu::menu_item(ui, label, "").clicked() {
                pick = Some(KeyMenuPick::Ease(preset));
            }
        }
        // The named curves (#2274), on the segment leaving the key.
        for (family, eases) in klipp_core::keyframe::Ease::PRESETS {
            ui.menu_button(family, |ui| {
                for (ease, label) in eases.into_iter().zip(["In", "Out", "In-Out"]) {
                    if super::menu::menu_choice(ui, label, "", current == Some(Interpolation::Eased(ease))).clicked() {
                        pick = Some(KeyMenuPick::Interpolation(Interpolation::Eased(ease)));
                    }
                }
            });
        }
        if super::menu::menu_choice(ui, "Bezier (Custom)", "", current == Some(Interpolation::Bezier)).clicked() {
            pick = Some(KeyMenuPick::Interpolation(Interpolation::Bezier));
        }
        if super::menu::menu_choice(ui, "Step (Hold)", "", current == Some(Interpolation::Step)).clicked() {
            pick = Some(KeyMenuPick::Interpolation(Interpolation::Step));
        }
        if pick.is_some() {
            ui.close_menu();
        }
        pick
    })
}

/// A keyframe diamond button 16 points wide, drawn as a number row's is
/// (filled on a key, an accent outline between keys, dim with none), its
/// hover saying how many of `keys` there are when given: for a control
/// that draws its own row (#2296).
pub fn diamond_button(ui: &mut egui::Ui, ks: &KeyframeState, keys: Option<usize>) -> egui::Response {
    let resp = ui.add(egui::Button::new("").frame(false).min_size(egui::vec2(16.0, 18.0)).sense(egui::Sense::click()));
    paint_diamond(ui, &resp, Some(ks), keys);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// `ui`'s value fields tinted with the accent, as an animated row's are.
pub fn tint_animated(ui: &mut egui::Ui) {
    let accent = ui.visuals().selection.stroke.color;
    let w = &mut ui.visuals_mut().widgets;
    for state in [&mut w.inactive, &mut w.hovered, &mut w.active] {
        state.bg_stroke = egui::Stroke::new(1.0_f32, accent.gamma_multiply(0.55));
        state.weak_bg_fill = state.weak_bg_fill.lerp_to_gamma(accent, 0.12);
    }
}

/// What a key strip did this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct KeyStrip {
    /// The diamond was clicked: add a key at the playhead, or take it away.
    pub toggled: bool,
    pub prev: bool,
    pub next: bool,
    /// The key at the playhead's menu picked this.
    pub pick: Option<KeyMenuPick>,
}

/// A control's key strip, as a number row's: the diamond, filled on a key
/// at the playhead, with the previous and next key arrows either side once
/// there are keys, and the key's menu on a right click of the diamond.
/// Laid out right to left from where `ui` places it.
pub fn key_strip(ui: &mut egui::Ui, ks: KeyframeState) -> KeyStrip {
    let mut out = KeyStrip::default();
    // The row's track, when the caller named it (#2296).
    let tag = crate::param_focus::take_tag(ui);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if ks.has_any_keyframes {
            out.next = nav_arrow(ui, ks.next_keyframe_time.is_some(), Icon::CaretRight);
        }
        let diamond = ui.add(egui::Button::new("").frame(false).min_size(egui::vec2(16.0, 18.0)).sense(egui::Sense::click()));
        paint_diamond(ui, &diamond, Some(&ks), tag.as_ref().map(|t| t.keys()));
        if diamond.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        out.toggled = diamond.clicked();
        if ks.has_keyframe_at_playhead {
            diamond.context_menu(|ui| out.pick = key_menu(ui, ks.current_interpolation));
        }
        if ks.has_any_keyframes {
            out.prev = nav_arrow(ui, ks.prev_keyframe_time.is_some(), Icon::CaretLeft);
        }
    });
    // The whole row the controls end, lit when its track is in focus.
    if let Some(tag) = &tag {
        crate::param_focus::row_drawn(ui, tag, ui.min_rect(), out.toggled);
    }
    out
}

/// What a switch or choice row's key controls did this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct KeyControls {
    /// The diamond was clicked: add a key at the playhead, or take it away.
    pub toggled: bool,
    pub prev: bool,
    pub next: bool,
}

/// A switch or choice row's key controls at the right edge, as a number
/// row draws them: the diamond, with the previous and next key arrows
/// either side of it once the parameter has keys. Its keys are steps, so
/// there is no interpolation menu.
pub fn key_controls(ui: &mut egui::Ui, ks: KeyframeState) -> KeyControls {
    let mut out = KeyControls::default();
    // The row's track, when the caller named it (#2296).
    let tag = crate::param_focus::take_tag(ui);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ks.has_any_keyframes {
            out.next = nav_arrow(ui, ks.next_keyframe_time.is_some(), Icon::CaretRight);
        }
        let diamond = ui.add(egui::Button::new("").frame(false).min_size(egui::vec2(16.0, 18.0)).sense(egui::Sense::click()));
        paint_diamond(ui, &diamond, Some(&ks), tag.as_ref().map(|t| t.keys()));
        if diamond.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        out.toggled = diamond.clicked();
        if ks.has_any_keyframes {
            out.prev = nav_arrow(ui, ks.prev_keyframe_time.is_some(), Icon::CaretLeft);
        }
    });
    // The whole row the controls end, lit when its track is in focus.
    if let Some(tag) = &tag {
        crate::param_focus::row_drawn(ui, tag, ui.min_rect(), out.toggled);
    }
    out
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
    scrubby_row(ui, RowLook::default(), label, value, range, default, speed, decimals, suffix, keyframe_state, None)
}

/// How a scrubby row is drawn: its label's size, colour and inset, and
/// the row's height. The default is the widget's own compact look; a
/// tab drawn in the inspector's row style passes its own.
#[derive(Clone, Copy, Debug)]
pub struct RowLook {
    pub label_size: f32,
    /// `None` for the visuals' inactive text colour.
    pub label_colour: Option<egui::Color32>,
    /// Space before the label inside its drag area.
    pub label_inset: f32,
    pub height: f32,
}

impl Default for RowLook {
    fn default() -> Self {
        Self { label_size: 11.0, label_colour: None, label_inset: 4.0, height: 22.0 }
    }
}

/// [`scrubby_param_row`] drawn with `look`; it behaves the same.
#[allow(clippy::too_many_arguments)]
pub fn scrubby_param_row_look(
    ui: &mut egui::Ui,
    look: RowLook,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default: f32,
    speed: f64,
    decimals: usize,
    suffix: &str,
    keyframe_state: Option<KeyframeState>,
) -> ScrubbyParamResponse {
    scrubby_row(ui, look, label, value, range, default, speed, decimals, suffix, keyframe_state, None)
}

/// [`scrubby_param_row`] whose drags (of the label or the number) move the
/// value by `drag(value, dx_pixels)` instead of its speed, unrounded: a
/// grade row's rule (`grade_drag`), the old panel's own.
#[allow(clippy::too_many_arguments)]
pub fn scrubby_param_row_dragged(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default: f32,
    speed: f64,
    decimals: usize,
    suffix: &str,
    keyframe_state: Option<KeyframeState>,
    drag: &dyn Fn(f32, f32) -> f32,
) -> ScrubbyParamResponse {
    // `speed` is the field's keyboard nudge; a drag goes by `drag`.
    scrubby_row(ui, RowLook::default(), label, value, range, default, speed, decimals, suffix, keyframe_state, Some(drag))
}

#[allow(clippy::too_many_arguments)]
fn scrubby_row(
    ui: &mut egui::Ui,
    look: RowLook,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default: f32,
    speed: f64,
    decimals: usize,
    suffix: &str,
    keyframe_state: Option<KeyframeState>,
    drag: Option<&dyn Fn(f32, f32) -> f32>,
) -> ScrubbyParamResponse {
    #[cfg(any(test, feature = "row-recorder"))]
    super::row_recorder::numeric(ui.id().with(label).with("scrubby_row_hover"), "scrubby", label, *value, &range, Some(default), Some(speed), Some(decimals), suffix, keyframe_state);
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
    // The row's track, when the caller named it (#2296).
    let tag = crate::param_focus::take_tag(ui);
    let mut row_clicked = false;

    let row_response = ui.horizontal(|ui| {
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
        let nav_arrow_width = NAV_ARROW_W; // per arrow
        let diamond_width = 16.0;
        // Every keyable row keeps the arrows' room, animated or not, so the
        // values and diamonds of a column line up.
        let has_nav = keyframe_state.is_some();
        let animated = keyframe_state.as_ref().is_some_and(|ks| ks.has_any_keyframes);
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
            egui::vec2(label_width, look.height),
            egui::Sense::click_and_drag(),
        );

        label_rect_out = label_rect_inner;

        // Paint the label text manually (no text selection possible)
        let label_color = if label_rect.hovered() || label_rect.dragged() {
            ui.visuals().widgets.hovered.fg_stroke.color
        } else {
            look.label_colour.unwrap_or(ui.visuals().widgets.inactive.fg_stroke.color)
        };
        ui.painter().text(
            egui::pos2(label_rect_inner.left() + look.label_inset, label_rect_inner.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(look.label_size),
            label_color,
        );

        // Resize cursor on label hover
        if label_rect.hovered() || label_rect.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        // Apply label drag delta to value
        if label_rect.dragged() {
            // Shift drags a tenth as fast, Alt ten times, by either rule.
            let pace = if modifiers.shift_only() { 0.1 } else if modifiers.alt { 10.0 } else { 1.0 };
            match drag {
                Some(drag) => *value = drag(*value, label_rect.drag_delta().x * pace),
                None => {
                    let delta = label_rect.drag_delta().x as f64;
                    let label_speed = if modifiers.shift_only() {
                        effective_speed / 10.0   // Shift: fine
                    } else {
                        effective_speed
                    };
                    *value += (delta * label_speed) as f32;
                }
            }
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
                if !ks.has_any_keyframes {
                    ui.allocate_exact_size(egui::vec2(nav_arrow_width, 18.0), egui::Sense::hover());
                } else if nav_arrow(ui, ks.next_keyframe_time.is_some(), Icon::CaretRight) {
                    nav_next = true;
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
                if !ks.has_any_keyframes {
                    ui.allocate_exact_size(egui::vec2(nav_arrow_width, 18.0), egui::Sense::hover());
                } else if nav_arrow(ui, ks.prev_keyframe_time.is_some(), Icon::CaretLeft) {
                    nav_prev = true;
                }
            }

            // Reset icon (left of prev arrow)
            let (rri, rr) = ui.allocate_exact_size(
                egui::vec2(icon_width, look.height),
                egui::Sense::click(),
            );
            reset_rect_inner = rri;
            reset_rect_resp = Some(rr);

            // Drag value (leftmost in cluster)
            let before_drag = *value;
            // A value that moves is tinted, so the row reads as animated
            // without its diamond.
            if animated {
                tint_animated(ui);
            }
            let dv = egui::DragValue::new(value)
                .speed(effective_speed)
                .range(range.clone())
                .fixed_decimals(decimals)
                .suffix(suffix);
            let dv_resp = ui.add(dv);
            // A drag rule moves the value itself, unrounded; the field's
            // own drag (at speed 0) is overridden, its typing kept.
            if let Some(drag) = drag.filter(|_| dv_resp.dragged()) {
                let pace = if modifiers.shift_only() { 0.1 } else if modifiers.alt { 10.0 } else { 1.0 };
                *value = drag(before_drag, dv_resp.drag_delta().x * pace).clamp(*range.start(), *range.end());
            }
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

        paint_diamond(ui, &diamond_btn_resp, keyframe_state.as_ref(), tag.as_ref().map(|t| t.keys()));
        row_clicked = label_rect.clicked() || diamond_btn_resp.clicked();

        if is_keyframeable && diamond_btn_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        if is_keyframeable && diamond_btn_resp.clicked() {
            keyframe_toggled = true;
        }

        // Right-click context menu on diamond for interpolation type
        if is_keyframeable && keyframe_state.as_ref().is_some_and(|ks| ks.has_keyframe_at_playhead) {
            let cur_interp = keyframe_state.as_ref().and_then(|ks| ks.current_interpolation);
            diamond_btn_resp.clone().context_menu(|ui| match key_menu(ui, cur_interp) {
                Some(KeyMenuPick::Interpolation(i)) => interp_changed = Some(i),
                Some(KeyMenuPick::Ease(e)) => ease_preset = Some(e),
                None => {}
            });
        }

    });

    if let Some(tag) = &tag {
        crate::param_focus::row_drawn(ui, tag, row_response.response.rect, row_clicked);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An animated row and a plain one, one under the other: the value
    /// field and the diamond sit at the same place across in both, the
    /// plain row keeping the room the animated one's arrows take (#2296).
    #[test]
    fn animated_and_plain_rows_line_up() {
        let ctx = egui::Context::default();
        crate::icons::install(&ctx);
        let state = |any| KeyframeState { has_keyframe_at_playhead: false, has_any_keyframes: any, keys_in_frame: 0, prev_keyframe_time: any.then_some(0.5), next_keyframe_time: None, current_interpolation: None };
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0))), ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let (mut a, mut b) = (1.25_f32, 2.5_f32);
                scrubby_param_row(ui, "Moving", &mut a, 0.0..=4.0, 1.0, 0.01, 2, "", Some(state(true)));
                scrubby_param_row(ui, "Still", &mut b, 0.0..=4.0, 1.0, 0.01, 2, "", Some(state(false)));
            });
        });
        let right_of = |text: &str| out.shapes.iter().find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == text => Some(t.pos.x + t.galley.size().x),
            _ => None,
        }).unwrap_or_else(|| panic!("{text} drawn"));
        assert!((right_of("1.25") - right_of("2.50")).abs() < 0.5, "values end at {} and {}", right_of("1.25"), right_of("2.50"));
        // The diamonds: four-point shapes, one per row, at one x.
        let diamonds: Vec<f32> = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Path(p) if p.points.len() == 4 && p.closed => Some((p.points[0].x + p.points[2].x) / 2.0),
            _ => None,
        }).collect();
        assert_eq!(diamonds.len(), 2, "{diamonds:?}");
        assert!((diamonds[0] - diamonds[1]).abs() < 0.5, "{diamonds:?}");
    }
}
