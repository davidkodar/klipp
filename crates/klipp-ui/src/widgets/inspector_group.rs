//! Inspector group chrome — primitives for the redesigned Title inspector.
//!
//! Three pubs:
//! - `inspector_group_header`: collapsible row with title + status indicator
//!   (count badge for toggle-list groups, plain dot for single-state groups).
//! - `inspector_subgroup_card`: bordered card with a toggle pill that doubles
//!   as the disclosure (toggle on = expanded, toggle off = collapsed header).
//! - `quick_preset_button`: small button with active-state styling for
//!   Position presets (Center / Lower 3rd / Top).
//!
//! Each is theme-tokened — accent for ON state, `bg_raised` for badge fill,
//! `text_dim` for summary chips. The themed toggle inside the subcard uses
//! `theme.accent` rather than the existing `widgets::toggle` widget's hardcoded
//! green; both coexist (the existing one is still used by audio FX panels).

use crate::icons::Icon;
use crate::theme::KlippTheme;
use egui::{
    Align, Color32, FontId, Frame, Layout, Margin, Rect, Response, RichText, Rounding, Sense,
    Stroke, Ui, pos2, vec2,
};

/// Status indicator shown at the right of a group header. `count` renders as
/// a numeric pill; `active` renders as a dot. Effects + Typography use both
/// (count is the number of enabled subtoggles); other groups use only `active`.
#[derive(Clone, Copy, Debug, Default)]
pub struct GroupStatus {
    pub count: Option<u32>,
    pub active: bool,
}

/// Collapsible group header. Caller owns the `is_open` state — this only
/// renders the row and reports clicks. Body rendering is the caller's job
/// (typically gated on `is_open`).
pub fn inspector_group_header(
    ui: &mut Ui,
    title: &str,
    is_open: bool,
    status: GroupStatus,
    theme: &KlippTheme,
) -> Response {
    let w = ui.available_width();
    let header_h = 36.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, header_h), Sense::click());

    if !ui.is_rect_visible(rect) {
        return resp;
    }

    // Bottom hairline separator.
    ui.painter().hline(
        rect.min.x..=rect.max.x,
        rect.max.y - 0.5,
        Stroke::new(1.0, theme.border),
    );

    // Hover background (subtle).
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, Rounding::ZERO, theme.bg_hover);
    }

    // Chevron (left).
    let icon = if is_open {
        Icon::CaretDown
    } else {
        Icon::CaretRight
    };
    let icon_size = 12.0;
    let icon_rect = Rect::from_center_size(
        pos2(rect.min.x + 16.0, rect.center().y),
        vec2(icon_size, icon_size),
    );
    icon.image(icon_size)
        .tint(theme.text_secondary)
        .paint_at(ui, icon_rect);

    // Title.
    ui.painter().text(
        pos2(rect.min.x + 36.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        FontId::proportional(14.0),
        theme.text_primary,
    );

    // Right-side status: optional count badge then optional accent dot.
    let mut x_cursor = rect.max.x - 16.0;

    if status.active {
        let r = 4.0;
        ui.painter()
            .circle_filled(pos2(x_cursor - r, rect.center().y), r, theme.accent);
        x_cursor -= r * 2.0 + 8.0;
    }

    if let Some(n) = status.count {
        let txt = n.to_string();
        // Approx width: 8px per digit + 14px horizontal padding.
        let badge_w = 14.0 + 8.0 * txt.len() as f32;
        let badge_h = 18.0;
        let badge_rect = Rect::from_min_size(
            pos2(x_cursor - badge_w, rect.center().y - badge_h / 2.0),
            vec2(badge_w, badge_h),
        );
        ui.painter()
            .rect_filled(badge_rect, Rounding::same(9.0), theme.bg_raised);
        ui.painter().text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            txt,
            FontId::proportional(11.0),
            theme.text_primary,
        );
    }

    resp
}

/// Themed toggle pill — same shape as `widgets::toggle::toggle` but uses
/// `theme.accent` for the on state. Returns the response so callers can detect
/// the click (mutates `*on` in place).
fn themed_toggle_ui(ui: &mut Ui, on: &mut bool, theme: &KlippTheme) -> Response {
    let desired = vec2(36.0, 20.0);
    let (rect, mut resp) = ui.allocate_exact_size(desired, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }

    if !ui.is_rect_visible(rect) {
        return resp;
    }

    let how_on = ui.ctx().animate_bool_with_time(resp.id, *on, 0.15);
    let bg = lerp_color(theme.bg_raised, theme.accent, how_on);
    let rounding = rect.height() / 2.0;
    ui.painter()
        .rect_filled(rect, Rounding::same(rounding), bg);
    ui.painter().rect_stroke(
        rect,
        Rounding::same(rounding),
        Stroke::new(1.0, theme.border),
    );

    // Knob.
    let cr = rect.height() / 2.0 - 2.0;
    let cx = egui::lerp(rect.left() + cr + 3.0..=rect.right() - cr - 3.0, how_on);
    ui.painter()
        .circle_filled(pos2(cx, rect.center().y), cr, Color32::WHITE);

    resp
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| -> u8 {
        ((x as f32) + ((y as f32) - (x as f32)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_premultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

/// Subcard for an effect inside the Effects group. Toggle pill on the left
/// IS the disclosure: when `*enabled` is true the header shows the summary
/// chip and the body is rendered via `add_body`; when false only the header
/// row is shown. Soft-disable: caller's data is preserved across toggle.
///
/// Returns `Some(R)` from the body closure when expanded, `None` when collapsed.
pub fn inspector_subgroup_card<R>(
    ui: &mut Ui,
    title: &str,
    enabled: &mut bool,
    summary_chip: Option<&str>,
    theme: &KlippTheme,
    add_body: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let frame = Frame::none()
        .fill(theme.bg_panel_alt)
        .stroke(Stroke::new(1.0, theme.border))
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::same(0.0));

    let mut body_result: Option<R> = None;

    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());

        // Header row — fixed height, horizontal layout.
        let header_h = 40.0;
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), header_h),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add_space(12.0);
                themed_toggle_ui(ui, enabled, theme);
                ui.add_space(10.0);

                let label_color = if *enabled {
                    theme.text_primary
                } else {
                    theme.text_secondary
                };
                ui.label(
                    RichText::new(title)
                        .size(13.5)
                        .color(label_color)
                        .strong(),
                );

                // Summary chip — right-aligned, only when enabled.
                if *enabled {
                    if let Some(chip) = summary_chip {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(12.0);
                            ui.label(
                                RichText::new(chip)
                                    .size(11.5)
                                    .color(theme.text_dim),
                            );
                        });
                    }
                }
            },
        );

        // Body — only when enabled. Generous top padding so the
        // first param row has breathing room under the header,
        // matching the reference's interior rhythm.
        if *enabled {
            let body_frame = Frame::none().inner_margin(Margin {
                left: 12.0,
                right: 12.0,
                top: 8.0,
                bottom: 12.0,
            });
            let inner = body_frame.show(ui, |ui| add_body(ui));
            body_result = Some(inner.inner);
        }
    });

    body_result
}

/// Position-preset button (Center / Lower 3rd / Top under the Position block).
/// Active styling: orange accent outline + accent_subtle fill + accent text.
/// Inactive: panel-alt bg + border + secondary text.
///
/// Caller controls `active` — typically by tracking the most-recently-clicked
/// preset and verifying X/Y are still within ±0.005 of the preset values.
pub fn quick_preset_button(
    ui: &mut Ui,
    label: &str,
    active: bool,
    theme: &KlippTheme,
) -> Response {
    let h = 30.0;
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());

    if !ui.is_rect_visible(rect) {
        return resp;
    }

    let (bg, stroke_color, text_color) = if active {
        (theme.accent_subtle, theme.accent, theme.accent)
    } else if resp.hovered() {
        (theme.bg_hover, theme.border_light, theme.text_primary)
    } else {
        (theme.bg_panel_alt, theme.border, theme.text_secondary)
    };

    ui.painter().rect(
        rect,
        Rounding::same(4.0),
        bg,
        Stroke::new(1.0, stroke_color),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        text_color,
    );

    resp
}
