//! The inspector's rows, one look for every tab: a compact row with the
//! label on the left in the secondary text colour and the value on the
//! right, no rule between rows. A value is either a control (a switch, a
//! button, a field) or plainly a fact, and colour carries meaning only:
//! the accent for what can be changed, green and red for a status.

use egui::{vec2, Align, Color32, Layout, RichText, Ui};
use klipp_core::AppCommand;

use crate::theme::KlippTheme;

/// A row's height.
pub const ROW_H: f32 = 26.0;
/// The space either side of a row's contents.
pub const ROW_PAD: f32 = 14.0;
/// The size of a row's text.
pub const TEXT: f32 = 12.5;

/// A label on the left and `value` on the right, no rule between rows.
pub fn row(ui: &mut Ui, theme: &KlippTheme, label: &str, value: impl FnOnce(&mut Ui)) {
    row_padded(ui, theme, label, ROW_PAD, value);
}

/// [`row`] with `pad` either side instead of the row's own, for a row
/// inside a frame that already keeps that margin.
pub fn row_padded(ui: &mut Ui, theme: &KlippTheme, label: &str, pad: f32, value: impl FnOnce(&mut Ui)) {
    let w = ui.available_width();
    ui.allocate_ui_with_layout(vec2(w, ROW_H), Layout::left_to_right(Align::Center), |ui| {
        ui.add_space(pad);
        ui.label(RichText::new(label).size(TEXT).color(theme.text_secondary));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(pad);
            value(ui);
        });
    });
}

/// A fact: its value as plain text, cut short with the whole of it on
/// hover when the row is too narrow for it.
pub fn text_row(ui: &mut Ui, theme: &KlippTheme, label: &str, value: impl Into<String>) {
    colour_row(ui, theme, label, value, theme.text_primary);
}

/// A timecode or other figure that reads best in fixed-width digits.
pub fn mono_row(ui: &mut Ui, theme: &KlippTheme, label: &str, value: impl Into<String>) {
    let value = value.into();
    row(ui, theme, label, |ui| {
        ui.add(egui::Label::new(RichText::new(value).monospace().size(TEXT).color(theme.text_primary)).truncate());
    });
}

/// A fact in a colour that means something: a status (green, red), or
/// the secondary colour for an absence ("None", "Not set").
pub fn colour_row(ui: &mut Ui, theme: &KlippTheme, label: &str, value: impl Into<String>, colour: Color32) {
    let value = value.into();
    row(ui, theme, label, |ui| {
        ui.add(egui::Label::new(RichText::new(value).size(TEXT).color(colour)).truncate());
    });
}

/// A line of text across the row's width, wrapped: a path, a note.
pub fn note(ui: &mut Ui, theme: &KlippTheme, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(ROW_PAD);
        ui.set_max_width(ui.available_width() - ROW_PAD);
        ui.add(egui::Label::new(RichText::new(text).size(11.0).color(theme.text_secondary)).wrap());
    });
}

/// An In or Out point: its timecode with a clear mark, or "Not set".
pub fn point_row(ui: &mut Ui, theme: &KlippTheme, label: &str, tc: Option<String>, cmds: &mut Vec<AppCommand>, clear: AppCommand) {
    row(ui, theme, label, |ui| match tc {
        Some(tc) => {
            if ui.add(egui::Button::new(RichText::new("\u{2715}").size(11.0)).frame(false)).on_hover_text(format!("Clear the {label} point")).clicked() {
                cmds.push(clear);
            }
            ui.label(RichText::new(tc).monospace().size(TEXT).color(theme.text_primary));
        }
        None => {
            ui.label(RichText::new("Not set").size(TEXT).color(theme.text_secondary));
        }
    });
}

/// A compact switch in the theme's colours.
pub fn switch(ui: &mut Ui, theme: &KlippTheme, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 18.0), egui::Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, on, 0.12);
    let track = if on { theme.accent } else { theme.bg_raised };
    ui.painter().rect_filled(rect, 9.0, track);
    let r = 7.0;
    let x = egui::lerp(rect.left() + r + 2.0..=rect.right() - r - 2.0, t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), r, if on { theme.text_on_accent } else { theme.text_secondary });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Helpers for drawing a view in a test and reading back its text.
#[cfg(test)]
pub(crate) mod test_frame {
    use super::*;

    /// One frame of `draw` in a 360 px wide panel, with `events`: the
    /// text it drew and where, and the commands it sent.
    pub fn frame(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        mut draw: impl FnMut(&mut Ui, &KlippTheme, &mut Vec<AppCommand>),
    ) -> (Vec<(String, egui::Pos2)>, Vec<AppCommand>) {
        let mut cmds = Vec::new();
        let theme = KlippTheme::default();
        let out = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(360.0, 6000.0))), events, ..Default::default() },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| draw(ui, &theme, &mut cmds));
            },
        );
        let texts = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)),
                _ => None,
            })
            .collect();
        (texts, cmds)
    }

    /// A press and release at `at`, over two frames.
    pub fn click(
        ctx: &egui::Context,
        at: egui::Pos2,
        mut draw: impl FnMut(&mut Ui, &KlippTheme, &mut Vec<AppCommand>),
    ) -> Vec<AppCommand> {
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = frame(ctx, vec![egui::Event::PointerMoved(at), press(true)], &mut draw).1;
        cmds.extend(frame(ctx, vec![press(false)], &mut draw).1);
        cmds
    }
}
