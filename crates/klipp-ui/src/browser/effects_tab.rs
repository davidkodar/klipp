use crate::drag_drop::DragPayload;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectCategory {
    All,
    Blur,
    Color,
    Stylize,
    Keying,
    Distort,
    Light,
}

struct EffectDef {
    name: &'static str,
    category: EffectCategory,
}

const EFFECTS: &[EffectDef] = &[
    EffectDef { name: "Gaussian Blur", category: EffectCategory::Blur },
    EffectDef { name: "Motion Blur", category: EffectCategory::Blur },
    EffectDef { name: "Sharpen", category: EffectCategory::Blur },
    EffectDef { name: "Defocus", category: EffectCategory::Blur },
    EffectDef { name: "Color Wheels", category: EffectCategory::Color },
    EffectDef { name: "LUT Import", category: EffectCategory::Color },
    EffectDef { name: "Tint", category: EffectCategory::Color },
    EffectDef { name: "Vibrance", category: EffectCategory::Color },
    EffectDef { name: "Vignette", category: EffectCategory::Stylize },
    EffectDef { name: "Film Grain", category: EffectCategory::Stylize },
    EffectDef { name: "Glow", category: EffectCategory::Stylize },
    EffectDef { name: "Pixelate", category: EffectCategory::Stylize },
    EffectDef { name: "Chroma Key", category: EffectCategory::Keying },
    EffectDef { name: "Luma Key", category: EffectCategory::Keying },
    EffectDef { name: "Fisheye", category: EffectCategory::Distort },
    EffectDef { name: "Chromatic Ab.", category: EffectCategory::Distort },
    EffectDef { name: "Lens Flare", category: EffectCategory::Light },
    EffectDef { name: "Light Leak", category: EffectCategory::Light },
];

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
    active_category: &mut EffectCategory,
) -> Vec<AppCommand> {
    let cmds = Vec::new();

    // Category filter pills
    ui.horizontal_wrapped(|ui| {
        ui.add_space(6.0);
        ui.spacing_mut().item_spacing = egui::vec2(3.0, 2.0);
        let categories = [
            (EffectCategory::All, "All", theme.text_dim),
            (EffectCategory::Blur, "Blur", egui::Color32::from_rgb(0x4a, 0x9a, 0xde)),
            (EffectCategory::Color, "Color", theme.accent),
            (EffectCategory::Stylize, "Stylize", egui::Color32::from_rgb(0xa0, 0x4a, 0xda)),
            (EffectCategory::Keying, "Keying", theme.role_dialogue),
            (EffectCategory::Distort, "Distort", egui::Color32::from_rgb(0xe8, 0x50, 0x20)),
            (EffectCategory::Light, "Light", egui::Color32::from_rgb(0xf0, 0xd0, 0x60)),
        ];

        for (cat, label, dot_color) in &categories {
            let is_active = *active_category == *cat;
            let (bg, fg) = if is_active {
                (theme.accent_subtle, theme.accent)
            } else {
                (egui::Color32::TRANSPARENT, theme.text_secondary)
            };

            let resp = ui.add(
                egui::Button::new(egui::RichText::new(*label).size(11.0).color(fg))
                    .fill(bg)
                    .rounding(4.0),
            );

            // Draw dot before text (approximate)
            let dot_pos = egui::pos2(resp.rect.left() + 4.0, resp.rect.center().y);
            ui.painter().circle_filled(dot_pos, 2.5, *dot_color);

            if resp.clicked() {
                *active_category = *cat;
            }
        }
    });

    ui.add_space(2.0);
    ui.painter().hline(
        ui.available_rect_before_wrap().x_range(),
        ui.cursor().min.y,
        egui::Stroke::new(1.0, theme.border),
    );
    ui.add_space(2.0);

    // Filter effects
    let search_lower = search_query.to_lowercase();
    let filtered: Vec<_> = EFFECTS
        .iter()
        .filter(|e| {
            (*active_category == EffectCategory::All || e.category == *active_category)
                && (search_lower.is_empty()
                    || e.name.to_lowercase().contains(&search_lower))
        })
        .collect();

    // 2-column grid
    egui::ScrollArea::vertical().id_salt("browser_effects_scroll").show(ui, |ui| {
        let avail_w = ui.available_width() - 16.0;
        let card_w = (avail_w - 6.0) / 2.0;

        egui::Grid::new("effects_grid")
            .num_columns(2)
            .spacing([6.0, 6.0])
            .min_col_width(card_w)
            .show(ui, |ui| {
                for (i, fx) in filtered.iter().enumerate() {
                    let resp = effect_card(ui, theme, fx.name, card_w);
                    if resp.double_clicked() {
                        state.toast.show(format!(
                            "\"{}\" applied to selected clip",
                            fx.name
                        ));
                    }
                    if resp.drag_started() {
                        state.drag_drop.begin(DragPayload::Effect {
                            name: fx.name.to_string(),
                        });
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
    });

    cmds
}

fn effect_card(ui: &mut egui::Ui, theme: &KlippTheme, name: &str, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, 72.0),
        egui::Sense::click_and_drag(),
    );

    let hovered = resp.hovered();
    let border_color = if hovered { theme.accent_dim } else { theme.border };
    let card_rect = if hovered {
        rect.translate(egui::vec2(0.0, -1.0))
    } else {
        rect
    };

    // Card background
    ui.painter()
        .rect_filled(card_rect, 6.0, theme.bg_raised);
    ui.painter()
        .rect_stroke(card_rect, 6.0, egui::Stroke::new(1.0, border_color));

    // Preview area (top 52px)
    let preview_rect = egui::Rect::from_min_size(card_rect.min, egui::vec2(width, 52.0));
    ui.painter()
        .rect_filled(preview_rect, egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 }, theme.bg_panel);

    // Simple preview gradient based on name
    let preview_inner = preview_rect.shrink(2.0);
    paint_effect_preview(ui.painter(), preview_inner, name, theme);

    // Effect name
    let name_rect = egui::Rect::from_min_size(
        egui::pos2(card_rect.min.x, card_rect.min.y + 52.0),
        egui::vec2(width, 20.0),
    );
    ui.painter().text(
        name_rect.center(),
        egui::Align2::CENTER_CENTER,
        name,
        egui::FontId::proportional(10.0),
        theme.text_secondary,
    );

    resp
}

fn paint_effect_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    name: &str,
    theme: &KlippTheme,
) {
    // Simple visual previews per category
    match name {
        "Gaussian Blur" | "Motion Blur" | "Defocus" | "Sharpen" => {
            // Horizontal gradient bars
            for i in 0..5 {
                let y = rect.min.y + i as f32 * (rect.height() / 5.0);
                let alpha = 80 + i * 30;
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.min.x, y),
                        egui::vec2(rect.width(), rect.height() / 5.0 - 1.0),
                    ),
                    0.0,
                    egui::Color32::from_rgba_premultiplied(0x4a, 0x9a, 0xde, alpha as u8),
                );
            }
        }
        "Color Wheels" | "LUT Import" | "Tint" | "Vibrance" => {
            // Diagonal gradient
            let mesh = egui::Mesh::default();
            let _ = mesh; // Simplified: just fill with a gradient
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x1a, 0x3a, 0x5c));
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.center().x, rect.min.y),
                    egui::vec2(rect.width() / 2.0, rect.height()),
                ),
                0.0,
                egui::Color32::from_rgb(0x5c, 0x3a, 0x1a),
            );
        }
        "Chroma Key" | "Luma Key" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x0a, 0x3a, 0x0a));
        }
        "Vignette" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x18, 0x18, 0x18));
            painter.circle_filled(rect.center(), rect.height() * 0.4, egui::Color32::from_rgb(0x30, 0x30, 0x30));
        }
        "Glow" | "Light Leak" | "Lens Flare" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x10, 0x10, 0x10));
            painter.circle_filled(
                rect.center(),
                rect.height() * 0.3,
                egui::Color32::from_rgba_premultiplied(232, 160, 32, 100),
            );
        }
        "Film Grain" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x20, 0x1e, 0x1a));
        }
        "Pixelate" => {
            let cell = 8.0;
            let cols = (rect.width() / cell) as u32;
            let rows = (rect.height() / cell) as u32;
            for r in 0..rows {
                for c in 0..cols {
                    let v = ((r * 37 + c * 53) % 60 + 20) as u8;
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(rect.min.x + c as f32 * cell, rect.min.y + r as f32 * cell),
                            egui::vec2(cell - 0.5, cell - 0.5),
                        ),
                        0.0,
                        egui::Color32::from_rgb(v, v, v),
                    );
                }
            }
        }
        _ => {
            painter.rect_filled(rect, 0.0, theme.bg_panel);
        }
    }
}
