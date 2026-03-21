use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

struct TransitionDef {
    name: &'static str,
}

const TRANSITIONS: &[TransitionDef] = &[
    TransitionDef { name: "Cross Dissolve" },
    TransitionDef { name: "Dip to Black" },
    TransitionDef { name: "Push Left" },
    TransitionDef { name: "Zoom" },
    TransitionDef { name: "Film Dissolve" },
    TransitionDef { name: "Blur Dissolve" },
    TransitionDef { name: "Dip to White" },
    TransitionDef { name: "Flash" },
    TransitionDef { name: "Spin" },
    TransitionDef { name: "Light Leak" },
];

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
) -> Vec<AppCommand> {
    let cmds = Vec::new();
    let search_lower = search_query.to_lowercase();

    let filtered: Vec<_> = TRANSITIONS
        .iter()
        .filter(|t| search_lower.is_empty() || t.name.to_lowercase().contains(&search_lower))
        .collect();

    egui::ScrollArea::vertical().id_salt("browser_transitions_scroll").show(ui, |ui| {
        let avail_w = ui.available_width() - 16.0;
        let card_w = (avail_w - 6.0) / 2.0;

        egui::Grid::new("transitions_grid")
            .num_columns(2)
            .spacing([6.0, 6.0])
            .min_col_width(card_w)
            .show(ui, |ui| {
                for (i, tr) in filtered.iter().enumerate() {
                    let resp = transition_card(ui, theme, tr.name, card_w);
                    if resp.double_clicked() {
                        state.toast.show(format!(
                            "\"{}\" applied to selected cut point",
                            tr.name
                        ));
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
    });

    cmds
}

fn transition_card(ui: &mut egui::Ui, theme: &KlippTheme, name: &str, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, 68.0),
        egui::Sense::click(),
    );

    let hovered = resp.hovered();
    let border_color = if hovered { theme.accent_dim } else { theme.border };

    ui.painter().rect_filled(rect, 6.0, theme.bg_raised);
    ui.painter()
        .rect_stroke(rect, 6.0, egui::Stroke::new(1.0, border_color));

    // Preview area (48px)
    let preview_rect = egui::Rect::from_min_size(rect.min, egui::vec2(width, 48.0));
    ui.painter().rect_filled(
        preview_rect,
        egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 },
        egui::Color32::BLACK,
    );
    paint_transition_preview(ui.painter(), preview_rect, name, theme);

    // Transition name
    let name_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, rect.min.y + 48.0),
        egui::vec2(width, 20.0),
    );
    ui.painter().text(
        name_rect.center(),
        egui::Align2::CENTER_CENTER,
        name,
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );

    resp
}

fn paint_transition_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    name: &str,
    theme: &KlippTheme,
) {
    match name {
        "Cross Dissolve" => {
            let half = rect.width() / 2.0;
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(half, rect.height())),
                0.0,
                egui::Color32::from_rgba_premultiplied(60, 80, 120, 128),
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + half, rect.min.y),
                    egui::vec2(half, rect.height()),
                ),
                0.0,
                egui::Color32::from_rgba_premultiplied(120, 80, 60, 128),
            );
        }
        "Dip to Black" => {
            let third = rect.width() / 3.0;
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(third, rect.height())),
                0.0,
                egui::Color32::from_rgb(0x22, 0x22, 0x22),
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + third, rect.min.y),
                    egui::vec2(third, rect.height()),
                ),
                0.0,
                egui::Color32::BLACK,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + third * 2.0, rect.min.y),
                    egui::vec2(third, rect.height()),
                ),
                0.0,
                egui::Color32::from_rgb(0x22, 0x22, 0x22),
            );
        }
        "Push Left" => {
            // Arrow icon
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "\u{2192}",
                egui::FontId::proportional(20.0),
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 100),
            );
        }
        "Zoom" => {
            painter.rect_stroke(
                rect.shrink(12.0),
                1.0,
                egui::Stroke::new(1.5, egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)),
            );
        }
        "Dip to White" => {
            let third = rect.width() / 3.0;
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(third, rect.height())),
                0.0,
                egui::Color32::from_rgb(0x88, 0x88, 0x88),
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + third, rect.min.y),
                    egui::vec2(third, rect.height()),
                ),
                0.0,
                egui::Color32::WHITE,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + third * 2.0, rect.min.y),
                    egui::vec2(third, rect.height()),
                ),
                0.0,
                egui::Color32::from_rgb(0x88, 0x88, 0x88),
            );
        }
        "Flash" => {
            painter.circle_filled(
                rect.center(),
                rect.height() * 0.35,
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 200),
            );
        }
        "Spin" => {
            painter.circle_stroke(
                rect.center(),
                8.0,
                egui::Stroke::new(2.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)),
            );
        }
        "Light Leak" => {
            painter.circle_filled(
                egui::pos2(rect.min.x + rect.width() * 0.3, rect.center().y),
                rect.height() * 0.4,
                egui::Color32::from_rgba_premultiplied(255, 200, 80, 120),
            );
        }
        _ => {
            painter.rect_filled(rect, 0.0, theme.bg_panel);
        }
    }
}
