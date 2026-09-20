//! The strip a collapsed side panel leaves behind.
//!
//! A panel that collapses to nothing can only be brought back from a
//! menu, and a menu item is a thing you have to already know about. So
//! collapsing leaves a rail: the panel's own tab icons, stacked, plus a
//! chevron pointing the way back. Clicking an icon reopens the panel
//! *on that tab*, so the rail is a shortcut and not just an undo —
//! collapsed LIBRARY, click MEDIA, and you are in the media pool.
//!
//! This is the pattern every editor that gets it right uses, under
//! different names: Resolve's edge strips, VS Code's activity bar. The
//! shared idea is that the way back is visible from where you are.

use crate::icons::Icon;
use crate::theme::KlippTheme;

/// What a rail entry shows. Whichever the panel's own tab row uses:
/// the browser's tabs are icons, the inspector's are short words, and
/// the rail borrows each rather than inventing a third vocabulary.
pub enum RailGlyph<'a> {
    Icon(Icon),
    /// A short label — the inspector's own `VID` / `AUD` / `COL`.
    Text(&'a str),
}

/// One entry on the rail: a glyph, a tooltip, and what it selects.
pub struct RailItem<'a> {
    pub glyph: RailGlyph<'a>,
    /// Named in the tooltip, and what the tab row calls it.
    pub label: &'a str,
    /// Whether this is the tab the panel will open on with no other input.
    pub active: bool,
}

/// What the rail was asked to do this frame.
pub enum RailAction {
    None,
    /// Reopen the panel, selecting the item at this index.
    Open(usize),
}

/// Which edge the rail lives on, which decides where the chevron points.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RailSide {
    Left,
    Right,
}

/// Draw a collapsed panel's rail into `rect`.
///
/// `rect` is the full height of the panel area and [`crate::theme::COLLAPSED_RAIL_W`] wide.
pub fn show(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    theme: &KlippTheme,
    side: RailSide,
    items: &[RailItem<'_>],
    tooltip_noun: &str,
) -> RailAction {
    let mut action = RailAction::None;

    ui.painter().rect_filled(rect, 0.0, theme.bg_panel);

    // A hairline on the inner edge, so the rail reads as the edge of a
    // panel rather than as a gap in the window.
    let seam_x = match side {
        RailSide::Left => rect.max.x - 0.5,
        RailSide::Right => rect.min.x + 0.5,
    };
    ui.painter().line_segment(
        [egui::pos2(seam_x, rect.min.y), egui::pos2(seam_x, rect.max.y)],
        egui::Stroke::new(1.0_f32, theme.border),
    );

    let icon_h = 30.0;
    let pad = 6.0;
    let mut y = rect.min.y + pad;

    // The chevron first: it points outward-to-inward, i.e. the direction
    // the panel will grow when clicked.
    let chevron = match side {
        RailSide::Left => Icon::CaretRight,
        RailSide::Right => Icon::CaretLeft,
    };
    let chev_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, y),
        egui::vec2(rect.width(), icon_h),
    );
    let chev_resp = ui.interact(
        chev_rect,
        ui.id().with(("rail_chevron", tooltip_noun)),
        egui::Sense::click(),
    );
    if chev_resp.hovered() {
        ui.painter().rect_filled(chev_rect.shrink(3.0), 4.0, theme.bg_hover);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let chev_tint = if chev_resp.hovered() { theme.text_primary } else { theme.text_secondary };
    put_icon(ui, chev_rect, chevron, 13.0, chev_tint);
    let chev_resp = chev_resp.on_hover_text(format!("Show {tooltip_noun}"));
    if chev_resp.clicked() {
        // No index: reopen on whatever tab was last active.
        action = RailAction::Open(usize::MAX);
    }

    y += icon_h + pad;
    ui.painter().line_segment(
        [egui::pos2(rect.min.x + 7.0, y), egui::pos2(rect.max.x - 7.0, y)],
        egui::Stroke::new(1.0_f32, theme.border),
    );
    y += pad;

    for (i, item) in items.iter().enumerate() {
        let item_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, y),
            egui::vec2(rect.width(), icon_h),
        );
        let resp = ui.interact(
            item_rect,
            ui.id().with(("rail_item", tooltip_noun, i)),
            egui::Sense::click(),
        );

        if item.active {
            // The active tab keeps its accent bar on the outer edge, so
            // a glance at the rail says where you will land.
            let bar_x = match side {
                RailSide::Left => rect.min.x,
                RailSide::Right => rect.max.x - 2.0,
            };
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::pos2(bar_x, item_rect.min.y + 4.0), egui::vec2(2.0, icon_h - 8.0)),
                1.0,
                theme.accent,
            );
        }
        if resp.hovered() {
            ui.painter().rect_filled(item_rect.shrink(3.0), 4.0, theme.bg_hover);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let colour = if item.active {
            theme.text_accent
        } else if resp.hovered() {
            theme.text_primary
        } else {
            theme.text_dim
        };
        match &item.glyph {
            RailGlyph::Icon(icon) => put_icon(ui, item_rect, *icon, 15.0, colour),
            RailGlyph::Text(t) => {
                ui.painter().text(
                    item_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    t,
                    egui::FontId::proportional(crate::theme::type_scale::CAPTION),
                    colour,
                );
            }
        }

        let resp = resp.on_hover_text(item.label);
        if resp.clicked() {
            action = RailAction::Open(i);
        }

        y += icon_h + 2.0;
    }

    action
}

/// Centre an icon in `rect` at `size`, tinted. The rail draws every
/// glyph through here so spacing cannot drift between the chevron and
/// the tabs below it.
fn put_icon(ui: &mut egui::Ui, rect: egui::Rect, icon: Icon, size: f32, tint: egui::Color32) {
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(size, size));
    ui.put(
        icon_rect,
        icon.image(size).tint(tint).max_size(egui::vec2(size, size)),
    );
}
