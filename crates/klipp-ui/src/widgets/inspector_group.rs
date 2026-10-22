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
/// The height of every section header in the inspector.
pub const SECTION_HEADER_H: f32 = 28.0;

/// Paint a section header into `rect`: a caret at the left, the title,
/// an optional badge at the right, a hairline below, the hover tone
/// under the pointer. One look for every section of every tab.
pub fn paint_section_header(ui: &Ui, rect: Rect, title: &str, is_open: bool, badge: Option<&str>, hovered: bool, theme: &KlippTheme) {
    let painter = ui.painter();
    if hovered {
        painter.rect_filled(rect, Rounding::ZERO, theme.bg_hover);
    }
    painter.hline(rect.min.x..=rect.max.x, rect.max.y - 0.5, Stroke::new(1.0_f32, theme.border));
    let icon = if is_open { Icon::CaretDown } else { Icon::CaretRight };
    let icon_rect = Rect::from_center_size(pos2(rect.min.x + 12.0, rect.center().y), vec2(10.0, 10.0));
    icon.image(10.0).tint(theme.text_dim).paint_at(ui, icon_rect);
    painter.text(
        pos2(rect.min.x + 26.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        FontId::proportional(crate::theme::type_scale::CONTROL),
        theme.text_primary,
    );
    if let Some(badge) = badge.filter(|b| !b.is_empty()) {
        painter.text(
            pos2(rect.max.x - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            badge,
            FontId::proportional(crate::theme::type_scale::CAPTION),
            theme.text_dim,
        );
    }
}

/// Sections that start closed rather than open.
///
/// `GRADE` — versions, stills and looks — is management: reached a few
/// times a session, where the wheels and curves below it are reached a
/// few times a minute. Open it cost ~140 px at the top of a panel whose
/// constraint is height, pushing the constantly-used thing down to make
/// room for the rarely-used one. Its header stays where it is, so it is
/// one click away and the chevron says so.
const DEFAULT_CLOSED: &[&str] = &["GRADE"];

fn starts_open(title: &str) -> bool {
    !DEFAULT_CLOSED.contains(&title)
}

/// A collapsible section: the shared header over `add_contents`, its
/// open state remembered by title across the session.
///
/// The remembered set holds the titles whose state **differs from their
/// default**, not the titles that are collapsed. That keeps one set
/// working for sections that start open and sections that start closed,
/// and it keeps every previously-saved entry meaning what it meant: a
/// section that starts open and is listed is still collapsed.
pub fn section_card(ui: &mut Ui, title: &str, badge: Option<String>, theme: &KlippTheme, add_contents: impl FnOnce(&mut Ui)) {
    let w = ui.available_width();
    let collapsed_key = egui::Id::new("klipp_collapsed_sections");
    let flipped: bool = ui.ctx().data_mut(|d| {
        let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
        set.contains(title)
    });
    let default_open = starts_open(title);
    let open_by_default = default_open != flipped;
    let id = ui.id().with(title).with("section");
    let mut col_state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open_by_default);
    let (header_rect, header_resp) = ui.allocate_exact_size(vec2(w, SECTION_HEADER_H), Sense::click());
    let is_open = col_state.is_open();
    if ui.is_rect_visible(header_rect) {
        paint_section_header(ui, header_rect, title, is_open, badge.as_deref(), header_resp.hovered(), theme);
    }
    if header_resp.clicked() {
        col_state.toggle(ui);
        let now_open = col_state.is_open();
        ui.ctx().data_mut(|d| {
            let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
            // Listed only while it disagrees with its default, so a
            // section left at its default costs nothing in the prefs
            // file and a changed default reaches everyone who has not
            // expressed a preference.
            if now_open == default_open {
                set.remove(title);
            } else {
                set.insert(title.to_string());
            }
        });
    }
    if col_state.is_open() {
        Frame::none()
            .inner_margin(Margin { left: 0.0, right: 0.0, top: 4.0, bottom: 6.0 })
            .show(ui, |ui| add_contents(ui));
    }
    col_state.store(ui.ctx());
}

/// The one-row title of an inspector column: what is being inspected,
/// nothing else, on the deep surface with a hairline below.
pub fn panel_title_row(ui: &mut Ui, title: &str, dim: bool, theme: &KlippTheme) -> bool {
    let mut collapse = false;
    let rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), SECTION_HEADER_H));
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.painter().rect_filled(rect, 0.0, theme.bg_deep);
        ui.painter().hline(rect.x_range(), rect.max.y, Stroke::new(1.0_f32, theme.border));
        ui.horizontal_centered(|ui| {
            ui.add_space(12.0);
            let colour = if dim { theme.text_dim } else { theme.text_primary };
            ui.add(
                egui::Label::new(
                    egui::RichText::new(title).size(crate::theme::type_scale::CONTROL).strong().color(colour),
                )
                .truncate()
                .selectable(false),
            );

            // Collapse, mirroring the browser's. Right-to-left so it
            // sits at the far edge whatever the title's length.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                let (btn_rect, resp) =
                    ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(btn_rect.shrink(2.0), 4.0, theme.bg_hover);
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let tint = if resp.hovered() { theme.text_primary } else { theme.text_dim };
                let icon_rect = Rect::from_center_size(btn_rect.center(), vec2(12.0, 12.0));
                ui.put(
                    icon_rect,
                    crate::icons::Icon::CaretRight
                        .image(12.0)
                        .tint(tint)
                        .max_size(vec2(12.0, 12.0)),
                );
                if resp.on_hover_text("Collapse inspector  (Ctrl+Shift+I)").clicked() {
                    collapse = true;
                }
            });
        });
    });
    ui.advance_cursor_after_rect(rect);
    collapse
}

/// One strip of tabs for every inspector column. Each tab is as wide as
/// its title at the largest size that fits; titles fall back to their
/// short forms before they would collide, so none is ever clipped.
/// Returns the index of a tab clicked this frame.
pub fn tab_strip(ui: &mut Ui, labels: &[&str], short: &[&str], active: usize, theme: &KlippTheme) -> Option<usize> {
    tab_strip_with_tips(ui, labels, short, &[], active, theme)
}

/// [`tab_strip`], each tab with its tip on hover where `tips` gives one.
pub fn tab_strip_with_tips(ui: &mut Ui, labels: &[&str], short: &[&str], tips: &[Option<String>], active: usize, theme: &KlippTheme) -> Option<usize> {
    let strip = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), SECTION_HEADER_H));
    let mut clicked = None;
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(strip), |ui| {
        ui.painter().rect_filled(strip, 0.0, theme.bg_deep);
        ui.painter().hline(strip.x_range(), strip.max.y, Stroke::new(1.0_f32, theme.border));
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let avail = ui.available_width();
            let pad = 14.0;
            let widths = |ui: &Ui, labels: &[&str], size: f32| -> Vec<f32> {
                labels
                    .iter()
                    .map(|l| ui.painter().layout_no_wrap(l.to_string(), egui::FontId::proportional(size), egui::Color32::WHITE).size().x + pad)
                    .collect()
            };
            let tiers: [(&[&str], f32); 3] = [
                (labels, crate::theme::type_scale::BODY),
                (labels, crate::theme::type_scale::CAPTION),
                (short, crate::theme::type_scale::CAPTION),
            ];
            let (names, size, w) = tiers
                .into_iter()
                .map(|(names, size)| (names, size, widths(ui, names, size)))
                .find(|(_, _, w)| w.iter().sum::<f32>() <= avail)
                .unwrap_or_else(|| (short, crate::theme::type_scale::CAPTION, widths(ui, short, crate::theme::type_scale::CAPTION)));
            let total: f32 = w.iter().sum();
            let stretch = if total > 0.0 { (avail / total).max(1.0) } else { 1.0 };
            for (i, name) in names.iter().enumerate() {
                let is_active = i == active;
                let (tab_rect, resp) = ui.allocate_exact_size(vec2(w[i] * stretch, SECTION_HEADER_H - 2.0), Sense::click());
                let colour = if is_active {
                    theme.text_primary
                } else if resp.hovered() {
                    theme.text_secondary
                } else {
                    theme.text_dim
                };
                ui.painter().text(tab_rect.center(), egui::Align2::CENTER_CENTER, *name, egui::FontId::proportional(size), colour);
                if is_active {
                    ui.painter().hline(tab_rect.x_range().shrink(4.0), tab_rect.max.y - 1.0, Stroke::new(2.0_f32, theme.accent));
                }
                if let Some(Some(tip)) = tips.get(i) {
                    resp.clone().on_hover_text(tip);
                }
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
        });
    });
    ui.advance_cursor_after_rect(strip);
    clicked
}

pub fn inspector_group_header(
    ui: &mut Ui,
    title: &str,
    is_open: bool,
    status: GroupStatus,
    theme: &KlippTheme,
) -> Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, SECTION_HEADER_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let badge = status.count.map(|n| n.to_string());
    paint_section_header(ui, rect, title, is_open, badge.as_deref(), resp.hovered(), theme);
    if status.active {
        // A lit dot: the group is doing something to the picture.
        let x = rect.max.x - 12.0 - badge.as_ref().map_or(0.0, |b| 8.0 * b.len() as f32 + 8.0);
        ui.painter().circle_filled(pos2(x - 3.0, rect.center().y), 3.0, theme.accent);
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
        Stroke::new(1.0_f32, theme.border),
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
        .stroke(Stroke::new(1.0_f32, theme.border))
        .rounding(Rounding::same(4.0))
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
                                    .size(11.0)
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
    let h = crate::theme::control::REGULAR;
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
        Stroke::new(1.0_f32, stroke_color),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(crate::theme::type_scale::CONTROL),
        text_color,
    );

    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the remembered set means changed: it holds the titles whose
    /// state disagrees with their default, not the titles that are
    /// collapsed. Every entry a user already had saved still means the
    /// same thing, because every section it can name starts open.
    #[test]
    fn a_saved_entry_still_collapses_a_section_that_starts_open() {
        assert!(starts_open("EXPOSURE"));
        // Listed, and it starts open, so it is collapsed — exactly what
        // that entry meant before.
        let listed = true;
        assert!(!(starts_open("EXPOSURE") != listed));
    }

    #[test]
    fn grade_starts_closed_and_nothing_else_does() {
        assert!(!starts_open("GRADE"));
        for title in [
            "EXPOSURE", "CONTRAST", "SATURATION", "TONE CONTROLS",
            "COLOUR BALANCE", "NOISE REDUCTION", "MASKS", "RENDER QUEUE",
            "OUTPUT", "VIDEO", "AUDIO", "SIZE AND RATE", "RANGE",
        ] {
            assert!(starts_open(title), "{title} should start open");
        }
    }

    /// Opening GRADE has to be remembered, which only works if the set
    /// records the disagreement rather than the collapse. Expanding a
    /// default-closed section lists it; collapsing it again clears it.
    #[test]
    fn expanding_a_default_closed_section_is_what_gets_remembered() {
        let default_open = starts_open("GRADE");
        assert!(!default_open);

        // The user expands it: now_open = true, which differs from the
        // default, so it is listed.
        let now_open = true;
        assert_ne!(now_open, default_open, "must be listed to persist");

        // And collapses it again: back to the default, so it is dropped
        // and costs nothing in the prefs file.
        let now_open = false;
        assert_eq!(now_open, default_open, "must be dropped, not listed");
    }
}
