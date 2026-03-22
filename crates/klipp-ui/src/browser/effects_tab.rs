use crate::browser::effect_registry::EffectSource;
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

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
    active_category: &mut EffectCategory,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();

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

    // Get filtered effects from registry
    let filtered = state.effect_registry.filtered(*active_category, search_query);

    // 2-column grid
    egui::ScrollArea::vertical()
        .id_salt("browser_effects_scroll")
        .show(ui, |ui| {
            let avail_w = ui.available_width() - 16.0;
            let card_w = (avail_w - 6.0) / 2.0;

            egui::Grid::new("effects_grid")
                .num_columns(2)
                .spacing([6.0, 6.0])
                .min_col_width(card_w)
                .show(ui, |ui| {
                    for (i, fx) in filtered.iter().enumerate() {
                        let thumb = state.effect_thumbnails.get_texture(&fx.name);
                        let is_pending = state.effect_thumbnails.is_ofx_pending(&fx.name);
                        let resp = effect_card(ui, theme, fx, card_w, thumb, is_pending);

                        // On hover: trigger OFX render if not already cached
                        if resp.hovered() {
                            if let EffectSource::Ofx(ref plugin_id) = fx.source {
                                if thumb.is_none() && !is_pending {
                                    if let Some(ref_frame) = &state.effect_registry.reference_frame {
                                        state.effect_thumbnails.request_ofx_render(
                                            &fx.name,
                                            plugin_id,
                                            ref_frame,
                                        );
                                    }
                                }
                            }
                        }

                        if resp.double_clicked() {
                            if let Some(clip_id) = state.selected_clip {
                                match &fx.source {
                                    EffectSource::Builtin(effect) => {
                                        cmds.push(AppCommand::AddBuiltinEffect {
                                            clip_id,
                                            effect: effect.clone(),
                                        });
                                        state.toast.show(format!(
                                            "\"{}\" applied (Built-in)",
                                            fx.name
                                        ));
                                    }
                                    EffectSource::Ofx(plugin_name) => {
                                        cmds.push(AppCommand::AddEffect {
                                            clip_id,
                                            plugin_name: plugin_name.clone(),
                                        });
                                        state.toast.show(format!(
                                            "\"{}\" applied (OFX)",
                                            fx.name
                                        ));
                                    }
                                }
                            } else {
                                state.toast.show("Select a clip first".to_string());
                            }
                        }

                        if resp.drag_started() {
                            let (is_builtin, plugin_id) = match &fx.source {
                                EffectSource::Builtin(_) => (true, String::new()),
                                EffectSource::Ofx(id) => (false, id.clone()),
                            };
                            state.drag_drop.begin(DragPayload::Effect {
                                name: fx.name.clone(),
                                is_builtin,
                                plugin_id,
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

fn effect_card(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    fx: &crate::browser::effect_registry::EffectEntry,
    width: f32,
    thumbnail: Option<&egui::TextureHandle>,
    is_pending: bool,
) -> egui::Response {
    let card_h = 72.0;
    let preview_h = 48.0;
    let info_h = card_h - preview_h;

    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, card_h),
        egui::Sense::click_and_drag(),
    );

    let hovered = resp.hovered();
    let border_color = if hovered { theme.accent_dim } else { theme.border };
    let card_rect = if hovered {
        rect.translate(egui::vec2(0.0, -1.0))
    } else {
        rect
    };

    let painter = ui.painter();

    // Hover shadow
    if hovered {
        let shadow_rect = card_rect.expand(2.0).translate(egui::vec2(0.0, 2.0));
        painter.rect_filled(shadow_rect, 8.0, egui::Color32::from_black_alpha(40));
    }

    // Card background
    painter.rect_filled(card_rect, 6.0, theme.bg_raised);
    painter.rect_stroke(card_rect, 6.0, egui::Stroke::new(if hovered { 1.5 } else { 1.0 }, border_color));

    // Preview area
    let preview_rect = egui::Rect::from_min_size(card_rect.min, egui::vec2(width, preview_h));
    let preview_rounding = egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 };
    painter.rect_filled(preview_rect, preview_rounding, theme.bg_panel);

    if let Some(tex) = thumbnail {
        // Draw real thumbnail texture
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(tex.id(), preview_rect.shrink(1.0), uv, egui::Color32::WHITE);
    } else {
        // Fallback: category-colored gradient placeholder
        paint_effect_preview(painter, preview_rect.shrink(2.0), fx, theme);

        if is_pending {
            // Spinner overlay while OFX render is in progress
            let center = preview_rect.center();
            let t = ui.input(|i| i.time) as f32;
            let angle = t * 4.0; // radians per second
            let r = 6.0;
            for i in 0..8 {
                let a = angle + i as f32 * std::f32::consts::TAU / 8.0;
                let alpha = (255.0 * (1.0 - i as f32 / 8.0)) as u8;
                let dot = egui::pos2(
                    center.x + a.cos() * r,
                    center.y + a.sin() * r,
                );
                painter.circle_filled(dot, 1.5, egui::Color32::from_white_alpha(alpha));
            }
            ui.ctx().request_repaint(); // keep animating
        } else if matches!(fx.source, EffectSource::Ofx(_)) {
            // "Hover to preview" hint for OFX effects without thumbnails
            painter.text(
                preview_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Hover to preview",
                egui::FontId::proportional(8.0),
                egui::Color32::from_white_alpha(60),
            );
        }
    }

    // Category dot (top-left corner of preview)
    let cat_color = category_color(fx.category, theme);
    painter.circle_filled(
        egui::pos2(preview_rect.min.x + 8.0, preview_rect.min.y + 8.0),
        3.0,
        cat_color,
    );

    // Info area
    let info_rect = egui::Rect::from_min_size(
        egui::pos2(card_rect.min.x, card_rect.min.y + preview_h),
        egui::vec2(width, info_h),
    );

    // Effect name (left)
    painter.text(
        egui::pos2(info_rect.min.x + 6.0, info_rect.center().y),
        egui::Align2::LEFT_CENTER,
        &fx.name,
        egui::FontId::proportional(10.0),
        theme.text_secondary,
    );

    // Source badge (right)
    let badge_color = match &fx.source {
        EffectSource::Builtin(_) => theme.accent_dim,
        EffectSource::Ofx(_) => egui::Color32::from_rgb(0xe0, 0x80, 0x20),
    };
    let badge_text = fx.source_label;
    let badge_galley = painter.layout_no_wrap(
        badge_text.to_string(),
        egui::FontId::proportional(8.0),
        badge_color,
    );
    let badge_w = badge_galley.size().x + 8.0;
    let badge_rect = egui::Rect::from_min_size(
        egui::pos2(info_rect.max.x - badge_w - 4.0, info_rect.center().y - 7.0),
        egui::vec2(badge_w, 14.0),
    );
    painter.rect_filled(badge_rect, 3.0, badge_color.linear_multiply(0.15));
    painter.rect_stroke(badge_rect, 3.0, egui::Stroke::new(0.5, badge_color.linear_multiply(0.4)));
    painter.text(
        badge_rect.center(),
        egui::Align2::CENTER_CENTER,
        badge_text,
        egui::FontId::proportional(8.0),
        badge_color,
    );

    // Tooltip on hover
    let cat_name = category_name(fx.category);
    let grouping_display = if fx.grouping.is_empty() { cat_name.to_string() } else { fx.grouping.clone() };
    resp.clone().on_hover_ui(|ui| {
        ui.label(egui::RichText::new(&fx.name).strong().size(12.0));
        ui.horizontal(|ui| {
            let dot = category_color(fx.category, theme);
            let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().circle_filled(dot_rect.center(), 3.5, dot);
            ui.label(egui::RichText::new(&grouping_display).size(10.0).color(egui::Color32::from_rgb(160, 160, 170)));
            ui.label(egui::RichText::new("\u{2022}").size(10.0).color(egui::Color32::from_rgb(80, 80, 90)));
            ui.label(egui::RichText::new(fx.source_label).size(10.0).color(badge_color));
        });
        ui.label(egui::RichText::new("Double-click to apply").size(9.0).color(egui::Color32::from_rgb(100, 100, 110)));
    });

    resp
}

fn category_name(cat: EffectCategory) -> &'static str {
    match cat {
        EffectCategory::All => "All",
        EffectCategory::Blur => "Blur",
        EffectCategory::Color => "Color",
        EffectCategory::Stylize => "Stylize",
        EffectCategory::Keying => "Keying",
        EffectCategory::Distort => "Distort",
        EffectCategory::Light => "Light",
    }
}

fn category_color(cat: EffectCategory, theme: &KlippTheme) -> egui::Color32 {
    match cat {
        EffectCategory::All => theme.text_dim,
        EffectCategory::Blur => egui::Color32::from_rgb(0x4a, 0x9a, 0xde),
        EffectCategory::Color => theme.accent,
        EffectCategory::Stylize => egui::Color32::from_rgb(0xa0, 0x4a, 0xda),
        EffectCategory::Keying => theme.role_dialogue,
        EffectCategory::Distort => egui::Color32::from_rgb(0xe8, 0x50, 0x20),
        EffectCategory::Light => egui::Color32::from_rgb(0xf0, 0xd0, 0x60),
    }
}

fn paint_effect_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    fx: &crate::browser::effect_registry::EffectEntry,
    theme: &KlippTheme,
) {
    // Simple visual preview based on category (placeholder until real thumbnails in Phase C)
    let cat_color = category_color(fx.category, theme);
    let dim = cat_color.linear_multiply(0.3);

    match fx.category {
        EffectCategory::Blur => {
            for i in 0..5 {
                let y = rect.min.y + i as f32 * (rect.height() / 5.0);
                let alpha = 60 + i * 30;
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.min.x, y),
                        egui::vec2(rect.width(), rect.height() / 5.0 - 1.0),
                    ),
                    0.0,
                    cat_color.linear_multiply(alpha as f32 / 255.0),
                );
            }
        }
        EffectCategory::Color => {
            painter.rect_filled(rect, 0.0, dim);
            let half = egui::Rect::from_min_size(
                egui::pos2(rect.center().x, rect.min.y),
                egui::vec2(rect.width() / 2.0, rect.height()),
            );
            painter.rect_filled(half, 0.0, cat_color.linear_multiply(0.2));
        }
        EffectCategory::Keying => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x0a, 0x30, 0x0a));
        }
        EffectCategory::Light => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x10, 0x10, 0x10));
            painter.circle_filled(
                rect.center(),
                rect.height() * 0.3,
                cat_color.linear_multiply(0.4),
            );
        }
        EffectCategory::Distort => {
            painter.rect_filled(rect, 0.0, dim);
            // Diagonal lines
            for i in 0..6 {
                let x = rect.min.x + (i as f32 / 5.0) * rect.width();
                painter.line_segment(
                    [egui::pos2(x, rect.min.y), egui::pos2(x + 10.0, rect.max.y)],
                    egui::Stroke::new(1.0, cat_color.linear_multiply(0.3)),
                );
            }
        }
        _ => {
            painter.rect_filled(rect, 0.0, dim);
        }
    }
}
