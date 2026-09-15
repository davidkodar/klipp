//! The segmented control: several labels in one field, the active one
//! lit. One look for the workspace switch, the edit mode, the viewer
//! choice and the scope mode.

use crate::theme::KlippTheme;
use egui::{FontId, Pos2, Rect, Response, Sense, Ui, Vec2};

const PAD: f32 = 12.0;

/// The size a segmented control of these labels wants.
pub fn segmented_size(ui: &Ui, labels: &[&str], font: &FontId, h: f32) -> Vec2 {
    let w: f32 = labels
        .iter()
        .map(|l| ui.painter().layout_no_wrap(l.to_string(), font.clone(), egui::Color32::WHITE).size().x + PAD * 2.0)
        .sum();
    Vec2::new(w + 4.0, h)
}

/// Lay the control out at the cursor. Returns the segment clicked.
pub fn segmented(ui: &mut Ui, labels: &[&str], active: usize, theme: &KlippTheme, font: FontId, h: f32) -> Option<usize> {
    let size = segmented_size(ui, labels, &font, h);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    segmented_in(ui, rect, labels, active, theme, font)
}

/// Draw the control into `rect`. Returns the segment clicked.
pub fn segmented_in(ui: &mut Ui, rect: Rect, labels: &[&str], active: usize, theme: &KlippTheme, font: FontId) -> Option<usize> {
    let resp: Response = ui.allocate_rect(rect, Sense::click());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, crate::theme::radius::PANEL, theme.bg_panel);
    painter.rect_stroke(rect, crate::theme::radius::PANEL, egui::Stroke::new(1.0_f32, theme.border));
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| painter.layout_no_wrap(l.to_string(), font.clone(), egui::Color32::WHITE).size().x + PAD * 2.0)
        .collect();
    let natural: f32 = widths.iter().sum();
    let stretch = if natural > 0.0 { ((rect.width() - 4.0) / natural).max(1.0) } else { 1.0 };
    let pointer: Option<Pos2> = ui.input(|i| i.pointer.interact_pos());
    let mut x = rect.min.x + 2.0;
    let mut clicked = None;
    for (i, label) in labels.iter().enumerate() {
        let w = widths[i] * stretch;
        let seg = Rect::from_min_size(Pos2::new(x, rect.min.y + 2.0), Vec2::new(w, rect.height() - 4.0));
        let is_active = i == active;
        let under = pointer.is_some_and(|p| seg.contains(p));
        if is_active {
            painter.rect_filled(seg, crate::theme::radius::CONTROL, theme.accent_subtle);
        } else if under && resp.hovered() {
            painter.rect_filled(seg, crate::theme::radius::CONTROL, theme.bg_hover);
        }
        let colour = if is_active { theme.accent } else if under { theme.text_primary } else { theme.text_secondary };
        painter.text(seg.center(), egui::Align2::CENTER_CENTER, *label, font.clone(), colour);
        if resp.clicked() && under {
            clicked = Some(i);
        }
        x += w;
    }
    clicked
}
