//! The look of a menu, as menus are drawn across the app (the node
//! editor's first, #1770): rows the menu's full width, no box round each
//! until hovered, the key on the right in a quiet colour.
//!
//! One row layout everywhere (#2632): a leading slot for an icon or a
//! toggle's check, the label after it, and the shortcut or a submenu's
//! arrow right-aligned in a column of their own. Every row starts its
//! label at [`LABEL_X`], whatever widget draws it: egui's own buttons
//! and submenu rows place their text after the button padding, which
//! the menu style sets to that column, and the rows here paint their
//! icon or check into the slot before it instead of beside the label.
//! A menu is as wide as its widest row; nothing wraps.

use crate::icons::Icon;

/// Where a menu row's slot starts, from the row's left edge.
const SLOT_X: f32 = 8.0;
/// The slot's width: an icon or a check.
const SLOT_W: f32 = 14.0;
/// Where every row's label starts, from the row's left edge: after the
/// slot and a gap. The shortcut column ends as far in from the right.
pub const LABEL_X: f32 = SLOT_X + SLOT_W + 6.0;

/// A menu drawn in its own popup frame (an `Area` the caller places).
pub fn menu_frame<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(4.0, 4.0)).show(ui, |ui| menu_body(ui, add))
}

/// A menu's rows inside a popup egui already framed (a `context_menu`'s).
pub fn menu_body<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let style = ui.style_mut();
    // The label column, for every row egui lays out itself: buttons,
    // selectable rows and submenu rows put their text after this.
    style.spacing.button_padding = egui::vec2(LABEL_X, 4.0);
    style.spacing.item_spacing.y = 1.0;
    // Framed rows with nothing drawn at rest, egui's own menu look: a
    // hovered row gets its band (#2621).
    style.visuals.button_frame = true;
    let w = &mut style.visuals.widgets;
    w.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    w.inactive.bg_stroke = egui::Stroke::NONE;
    w.hovered.bg_stroke = egui::Stroke::NONE;
    w.active.bg_stroke = egui::Stroke::NONE;
    // As wide as the widest row: a label never wraps under itself.
    style.wrap_mode = Some(egui::TextWrapMode::Extend);
    ui.set_min_width(200.0);
    ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), add).inner
}

/// A menu row with its key on the right.
pub fn menu_item(ui: &mut egui::Ui, label: &str, key: &str) -> egui::Response {
    ui.add(egui::Button::new(label).shortcut_text(key))
}

/// A menu row for one of a set of choices, shown selected while it is the
/// one in force.
pub fn menu_choice(ui: &mut egui::Ui, label: &str, key: &str, current: bool) -> egui::Response {
    ui.add(egui::Button::new(label).shortcut_text(key).selected(current))
}

/// A menu row for something on or off: a check in the slot while it is
/// on, the label where every row's is. Clicked, the caller flips it.
pub fn menu_toggle(ui: &mut egui::Ui, label: &str, key: &str, on: bool) -> egui::Response {
    let resp = menu_item(ui, label, key);
    if on {
        paint_in_slot(ui, &resp, Icon::Check);
    }
    resp
}

/// [`menu_toggle`] over a `bool` it flips when clicked.
pub fn menu_toggle_value(ui: &mut egui::Ui, value: &mut bool, label: &str, key: &str) -> egui::Response {
    let mut resp = menu_toggle(ui, label, key, *value);
    if resp.clicked() {
        *value = !*value;
        resp.mark_changed();
    }
    resp
}

/// A menu row with an icon in its slot.
pub fn menu_icon_item(ui: &mut egui::Ui, icon: Icon, label: &str, key: &str) -> egui::Response {
    let resp = menu_item(ui, label, key);
    paint_in_slot(ui, &resp, icon);
    resp
}

/// A quiet heading over a group of rows, its text in the label column.
pub fn menu_heading(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(LABEL_X);
        ui.label(egui::RichText::new(text).size(crate::theme::type_scale::CAPTION).weak())
    })
    .inner
}

/// `icon` in the slot of the row `resp` drew, in the row's text colour.
fn paint_in_slot(ui: &egui::Ui, resp: &egui::Response, icon: Icon) {
    let colour = ui.style().interact(resp).text_color();
    let slot = egui::Rect::from_center_size(egui::pos2(resp.rect.left() + SLOT_X + SLOT_W / 2.0, resp.rect.center().y), egui::vec2(SLOT_W, SLOT_W));
    icon.image(SLOT_W).tint(colour).paint_at(ui, slot);
}

/// Menus in the one row layout, opened from a widget or a submenu row.
pub trait MenuRows {
    /// `context_menu`, its rows in the menu style.
    fn context_menu_rows(&self, add: impl FnOnce(&mut egui::Ui)) -> Option<egui::InnerResponse<()>>;
}

impl MenuRows for egui::Response {
    fn context_menu_rows(&self, add: impl FnOnce(&mut egui::Ui)) -> Option<egui::InnerResponse<()>> {
        self.context_menu(|ui| menu_body(ui, add))
    }
}

/// A submenu in the one row layout.
pub trait SubmenuRows {
    /// `menu_button`, its rows in the menu style.
    fn menu_rows<R>(&mut self, title: impl Into<egui::WidgetText>, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<Option<R>>;
    /// [`SubmenuRows::menu_rows`] with `icon` in its row's slot.
    fn menu_rows_with_icon<R>(&mut self, icon: Icon, title: impl Into<egui::WidgetText>, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<Option<R>>;
}

impl SubmenuRows for egui::Ui {
    fn menu_rows<R>(&mut self, title: impl Into<egui::WidgetText>, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<Option<R>> {
        self.menu_button(title, |ui| menu_body(ui, add))
    }

    fn menu_rows_with_icon<R>(&mut self, icon: Icon, title: impl Into<egui::WidgetText>, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<Option<R>> {
        let inner = self.menu_rows(title, add);
        paint_in_slot(self, &inner.response, icon);
        inner
    }
}

/// What breaks the one row layout in the open menu that draws `anchor`
/// (#2632): a row whose text starts neither at the label column the
/// menu's rows share nor ends at its shortcut column, a label that
/// fakes the slot with a leading check or spaces, and text that wraps.
/// `shapes` is a frame's output with the menu open.
#[cfg(test)]
pub(crate) fn row_problems(shapes: &[egui::epaint::ClippedShape], anchor: &str) -> Vec<String> {
    let texts: Vec<(egui::Rect, &egui::epaint::TextShape)> = shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((s.clip_rect, t)),
            _ => None,
        })
        .collect();
    let Some(anchor_rect) = texts.iter().rev().find(|(_, t)| t.galley.text() == anchor).map(|(_, t)| egui::Rect::from_min_size(t.pos, t.galley.size())) else {
        return vec![format!("{anchor:?} is not drawn")];
    };
    // The menu is the smallest box drawn round the anchor that is taller
    // than a few rows: its popup frame. A hovered row's band is one row
    // high; a panel's background is larger.
    // A frame paints its shadow and box as one list of shapes.
    fn boxes(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
        match shape {
            egui::Shape::Rect(r) => out.push(r.rect),
            egui::Shape::Vec(v) => v.iter().for_each(|s| boxes(s, out)),
            _ => {}
        }
    }
    let mut all = Vec::new();
    for (i, s) in shapes.iter().enumerate() {
        let mut found = Vec::new();
        boxes(&s.shape, &mut found);
        all.extend(found.into_iter().map(|r| (i, r)));
    }
    let Some((first, menu)) = all
        .into_iter()
        .filter(|(_, r)| r.contains_rect(anchor_rect) && r.height() > 3.0 * anchor_rect.height())
        .min_by(|(_, a), (_, b)| a.area().total_cmp(&b.area()))
    else {
        return vec![format!("no menu frame round {anchor:?}")];
    };
    // The menu's own text: painted after its frame, so not what lies
    // under it in a panel.
    let rows: Vec<(String, egui::Rect, usize)> = shapes[first..]
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), egui::Rect::from_min_size(t.pos, t.galley.size()), t.galley.rows.len())),
            _ => None,
        })
        .filter(|(_, r, _)| menu.contains(r.center()))
        .collect();
    // The column most rows share, on each side.
    let mode = |xs: Vec<f32>| {
        let mut best = (0, 0.0);
        for x in &xs {
            let n = xs.iter().filter(|y| (*y - x).abs() < 0.75).count();
            if n > best.0 {
                best = (n, *x);
            }
        }
        best.1
    };
    let label_x = mode(rows.iter().map(|(_, r, _)| r.left()).collect());
    let key_x = mode(rows.iter().filter(|(_, r, _)| (r.left() - label_x).abs() >= 0.75).map(|(_, r, _)| r.right()).collect());
    let mut problems = Vec::new();
    for (text, rect, lines) in &rows {
        // A row's icon or check, in the slot before the label column.
        if rect.right() <= label_x + 0.5 {
            continue;
        }
        if *lines > 1 {
            problems.push(format!("{text:?} wraps onto {lines} lines"));
        }
        let at_label = (rect.left() - label_x).abs() < 0.75;
        let at_key = (rect.right() - key_x).abs() < 0.75;
        if !at_label && !at_key {
            problems.push(format!("{text:?} starts at {:.1}, not the label column {label_x:.1}, and ends at {:.1}, not the key column {key_x:.1}", rect.left(), rect.right()));
        }
        if at_label && !text.starts_with(|c: char| c.is_alphanumeric()) {
            problems.push(format!("{text:?} draws a check, icon or indent in its label, not the slot"));
        }
    }
    problems
}
