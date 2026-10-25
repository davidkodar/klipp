//! The look of a menu, as menus are drawn across the app (the node
//! editor's first, #1770): rows the menu's full width, no box round each
//! until hovered, the key on the right in a quiet colour.

/// A menu drawn in its own popup frame (an `Area` the caller places).
pub fn menu_frame<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(4.0, 4.0)).show(ui, |ui| menu_body(ui, add))
}

/// A menu's rows inside a popup egui already framed (a `context_menu`'s).
pub fn menu_body<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let style = ui.style_mut();
    // No inset on the left: a plain row (an unframed button) has none,
    // and a submenu's row must start where it does.
    style.spacing.button_padding = egui::vec2(0.0, 4.0);
    style.spacing.item_spacing.y = 1.0;
    style.visuals.button_frame = false;
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
