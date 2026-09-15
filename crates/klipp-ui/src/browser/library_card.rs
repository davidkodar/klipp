//! One card for every Library tab: the picture, a name row and a
//! caption row on the same field, border and hover as a media card.

use crate::theme::KlippTheme;

/// Height of the name and caption rows under the picture.
pub const CAPTION_H: f32 = 36.0;

/// Paint a Library card into `rect`. `paint_preview` draws the picture
/// into the area above the caption rows, clipped to it.
#[allow(clippy::too_many_arguments)]
pub fn library_card(
    ui: &egui::Ui,
    rect: egui::Rect,
    name: &str,
    caption: &str,
    selected: bool,
    hovered: bool,
    theme: &KlippTheme,
    paint_preview: impl FnOnce(&egui::Painter, egui::Rect),
) -> egui::Rect {
    let rounding = egui::Rounding::same(crate::theme::radius::PANEL);
    let painter = ui.painter().clone();
    painter.rect_filled(rect, rounding, theme.bg_panel_alt);
    let preview = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), (rect.height() - CAPTION_H).max(1.0)));
    paint_preview(&painter.with_clip_rect(preview), preview);

    let (stroke_w, stroke_colour) = if selected {
        (1.5_f32, theme.accent)
    } else if hovered {
        (1.0_f32, theme.border_light)
    } else {
        (1.0_f32, theme.border)
    };
    painter.rect_stroke(rect, rounding, egui::Stroke::new(stroke_w, stroke_colour));

    let name_font = egui::FontId::proportional(crate::theme::type_scale::BODY);
    let caption_font = egui::FontId::proportional(crate::theme::type_scale::CAPTION);
    let max_w = rect.width() - 12.0;
    let x = rect.min.x + 6.0;
    let name_y = preview.max.y + 5.0;
    painter.text(
        egui::pos2(x, name_y),
        egui::Align2::LEFT_TOP,
        fit(&painter, name, &name_font, max_w),
        name_font,
        theme.text_primary,
    );
    painter.text(
        egui::pos2(x, name_y + crate::theme::type_scale::BODY + 4.0),
        egui::Align2::LEFT_TOP,
        fit(&painter, caption, &caption_font, max_w),
        caption_font,
        theme.text_dim,
    );
    preview
}

/// The text cut to one line of `max_w`, with an ellipsis.
pub fn fit(painter: &egui::Painter, text: &str, font: &egui::FontId, max_w: f32) -> String {
    let width = |t: &str| painter.layout_no_wrap(t.to_string(), font.clone(), egui::Color32::WHITE).size().x;
    if width(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut n = chars.len();
    while n > 1 {
        n -= 1;
        let candidate: String = chars[..n].iter().collect::<String>() + "\u{2026}";
        if width(&candidate) <= max_w {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}
