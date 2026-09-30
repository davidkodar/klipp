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

/// The favourite star on a Library card, as the Effects tab draws it:
/// top-left of the picture in a grid, at the right edge of a list row.
/// Shown while the item is a favourite or the card is hovered; the
/// card's right-click menu offers the same toggle through
/// [`favourite_menu_item`]. True when clicked.
pub fn favourite_star(ui: &mut egui::Ui, theme: &KlippTheme, card: &egui::Response, grid: bool, is_fav: bool, id: impl std::hash::Hash) -> bool {
    let star_rect = if grid {
        egui::Rect::from_min_size(card.rect.min + egui::vec2(6.0, 6.0), egui::vec2(22.0, 22.0))
    } else {
        egui::Rect::from_min_size(egui::pos2(card.rect.max.x - 28.0, card.rect.center().y - 11.0), egui::vec2(22.0, 22.0))
    };
    let star = ui.interact(star_rect, ui.id().with(("fav_star", id)), egui::Sense::click());
    if is_fav || card.hovered() || star.hovered() {
        if grid {
            let alpha = if is_fav { 180 } else { 130 };
            ui.painter().rect_filled(star_rect, 4.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, alpha));
        }
        let colour = if is_fav || star.hovered() {
            egui::Color32::from_rgb(255, 215, 106)
        } else if grid {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)
        } else {
            theme.text_dim
        };
        let glyph = if is_fav { "\u{2605}" } else { "\u{2606}" };
        ui.painter().text(star_rect.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(13.0), colour);
    }
    star.on_hover_text(if is_fav { "Remove from favourites" } else { "Add to favourites" }).clicked()
}

/// The favourites entry of a card's right-click menu. True when chosen.
pub fn favourite_menu_item(ui: &mut egui::Ui, is_fav: bool) -> bool {
    let chosen = ui.button(if is_fav { "Remove from Favourites" } else { "Add to Favourites" }).clicked();
    if chosen {
        ui.close_menu();
    }
    chosen
}

/// Add `key` to a favourites list, or take it out.
pub fn toggle_favourite(favourites: &mut Vec<String>, key: &str) {
    match favourites.iter().position(|k| k == key) {
        Some(i) => {
            favourites.remove(i);
        }
        None => favourites.push(key.to_string()),
    }
}

/// A browser tab's own lists, shown in place of a category: the
/// starred items, persisted in the preferences, and the ones added to
/// the timeline this session, newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryList {
    Favourites,
    Recent,
}

/// What was added to the timeline from each Library tab this session,
/// newest first. On the app state, since a drop onto the timeline adds
/// it there, away from the browser.
#[derive(Debug, Clone, Default)]
pub struct LibraryRecent {
    /// By the transition registry's `string_key`.
    pub transitions: Vec<String>,
    /// By template name.
    pub titles: Vec<String>,
    /// By generator name.
    pub generators: Vec<String>,
}

/// How many items a Recent list keeps.
pub const MAX_RECENT: usize = 12;

/// Put `key` at the front of a most-recent-first list, once.
pub fn push_recent(recent: &mut Vec<String>, key: &str) {
    recent.retain(|k| k != key);
    recent.insert(0, key.to_string());
    recent.truncate(MAX_RECENT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_is_newest_first_once_and_capped() {
        let mut r = Vec::new();
        for i in 0..20 {
            push_recent(&mut r, &format!("t{i}"));
        }
        assert_eq!(r.len(), MAX_RECENT);
        assert_eq!(r[0], "t19");
        push_recent(&mut r, "t12");
        assert_eq!(r[0], "t12");
        assert_eq!(r.iter().filter(|k| *k == "t12").count(), 1);
    }

    #[test]
    fn a_favourite_toggles() {
        let mut f = vec!["a".to_string()];
        toggle_favourite(&mut f, "b");
        toggle_favourite(&mut f, "a");
        assert_eq!(f, vec!["b".to_string()]);
    }
}
