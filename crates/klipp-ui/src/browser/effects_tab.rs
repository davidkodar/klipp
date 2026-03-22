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

const CATEGORY_SIDEBAR_W: f32 = 90.0;

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
    active_category: &mut EffectCategory,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let avail = ui.available_rect_before_wrap();

    // ── Split: category sidebar (left) | thumbnail grid (right) ──
    let sidebar_w = CATEGORY_SIDEBAR_W.min(avail.width() * 0.35);
    let sidebar_rect = egui::Rect::from_min_size(
        avail.min,
        egui::vec2(sidebar_w, avail.height()),
    );
    let grid_rect = egui::Rect::from_min_max(
        egui::pos2(avail.min.x + sidebar_w, avail.min.y),
        avail.max,
    );

    // ── Category sidebar ──
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_rect), |ui| {
        ui.set_min_size(sidebar_rect.size());

        // Subtle right border
        ui.painter().vline(
            sidebar_rect.max.x,
            sidebar_rect.y_range(),
            egui::Stroke::new(1.0, theme.border),
        );

        egui::ScrollArea::vertical()
            .id_salt("effects_category_scroll")
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("VIDEO")
                        .size(9.0)
                        .strong()
                        .color(theme.text_dim),
                );
                ui.add_space(2.0);

                let categories = [
                    (EffectCategory::All, "All"),
                    (EffectCategory::Blur, "Blur"),
                    (EffectCategory::Color, "Color"),
                    (EffectCategory::Stylize, "Stylize"),
                    (EffectCategory::Keying, "Keying"),
                    (EffectCategory::Distort, "Distort"),
                    (EffectCategory::Light, "Light"),
                ];

                for (cat, label) in &categories {
                    let is_active = *active_category == *cat;
                    let text_color = if is_active { theme.text_primary } else { theme.text_secondary };

                    let resp = ui.add(
                        egui::Label::new(
                            egui::RichText::new(*label)
                                .size(11.0)
                                .color(text_color)
                        )
                        .selectable(false)
                        .sense(egui::Sense::click()),
                    );

                    // Highlight bar for active category
                    if is_active {
                        let bar_rect = egui::Rect::from_min_size(
                            egui::pos2(resp.rect.min.x - 2.0, resp.rect.min.y),
                            egui::vec2(2.0, resp.rect.height()),
                        );
                        ui.painter().rect_filled(bar_rect, 1.0, theme.accent);
                    }

                    if resp.clicked() {
                        *active_category = *cat;
                    }
                }
            });
    });

    // ── Thumbnail grid ──
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(grid_rect), |ui| {
        ui.set_min_size(grid_rect.size());

        let filtered = state.effect_registry.filtered(*active_category, search_query);
        let count = filtered.len();

        egui::ScrollArea::vertical()
            .id_salt("browser_effects_scroll")
            .show(ui, |ui| {
                let grid_w = ui.available_width() - 8.0;
                let spacing = 4.0;
                let card_w = ((grid_w - spacing) / 2.0).floor();
                let preview_h = (card_w * 9.0 / 16.0).round(); // 16:9 aspect
                let card_h = preview_h + 22.0; // preview + name label

                egui::Grid::new("effects_grid")
                    .num_columns(2)
                    .spacing([spacing, spacing])
                    .min_col_width(card_w)
                    .show(ui, |ui| {
                        for (i, fx) in filtered.iter().enumerate() {
                            let thumb = state.effect_thumbnails.get_texture(&fx.name);
                            let is_pending = state.effect_thumbnails.is_ofx_pending(&fx.name);
                            let resp = effect_card(ui, theme, fx, card_w, card_h, preview_h, thumb, is_pending);

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
                                            state.toast.show(format!("\"{}\" applied", fx.name));
                                        }
                                        EffectSource::Ofx(plugin_name) => {
                                            cmds.push(AppCommand::AddEffect {
                                                clip_id,
                                                plugin_name: plugin_name.clone(),
                                            });
                                            state.toast.show(format!("\"{}\" applied", fx.name));
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

                // Item count at bottom
                if count > 0 {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(format!("{count} items"))
                                .size(9.0)
                                .color(theme.text_dim),
                        );
                    });
                }
            });
    });

    cmds
}

/// FCP-style effect card: large thumbnail + name below. Clean, no badges.
fn effect_card(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    fx: &crate::browser::effect_registry::EffectEntry,
    width: f32,
    card_h: f32,
    preview_h: f32,
    thumbnail: Option<&egui::TextureHandle>,
    is_pending: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, card_h),
        egui::Sense::click_and_drag(),
    );

    let hovered = resp.hovered();
    let card_rect = rect;
    let painter = ui.painter();

    // Preview area (top)
    let preview_rect = egui::Rect::from_min_size(card_rect.min, egui::vec2(width, preview_h));
    let rounding = egui::Rounding::same(4.0);

    // Hover highlight: subtle bright border
    if hovered {
        let highlight = preview_rect.expand(1.5);
        painter.rect_stroke(highlight, rounding, egui::Stroke::new(1.5, theme.accent_dim));
    }

    // Dark background for preview
    painter.rect_filled(preview_rect, rounding, theme.bg_panel);

    if let Some(tex) = thumbnail {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        // Clip the image to rounded rect
        painter.rect_filled(preview_rect, rounding, egui::Color32::BLACK);
        painter.image(tex.id(), preview_rect, uv, egui::Color32::WHITE);
    } else {
        paint_effect_preview(painter, preview_rect.shrink(1.0), fx, theme);

        if is_pending {
            let center = preview_rect.center();
            let t = ui.input(|i| i.time) as f32;
            let angle = t * 4.0;
            for i in 0..8 {
                let a = angle + i as f32 * std::f32::consts::TAU / 8.0;
                let alpha = (255.0 * (1.0 - i as f32 / 8.0)) as u8;
                let dot = egui::pos2(center.x + a.cos() * 6.0, center.y + a.sin() * 6.0);
                painter.circle_filled(dot, 1.5, egui::Color32::from_white_alpha(alpha));
            }
            ui.ctx().request_repaint();
        }
    }

    // Name below preview — FCP style: just the name, no badges
    let name_y = card_rect.min.y + preview_h + 2.0;
    let name_galley = painter.layout(
        fx.name.clone(),
        egui::FontId::proportional(10.0),
        if hovered { theme.text_primary } else { theme.text_secondary },
        width - 2.0,
    );
    painter.galley(egui::pos2(card_rect.min.x + 1.0, name_y), name_galley, theme.text_secondary);

    // Tooltip with full info
    resp.clone().on_hover_ui(|ui| {
        ui.label(egui::RichText::new(&fx.name).strong().size(12.0));
        if !fx.grouping.is_empty() {
            ui.label(egui::RichText::new(&fx.grouping).size(10.0).color(theme.text_dim));
        }
        ui.label(
            egui::RichText::new(fx.source_label)
                .size(9.0)
                .color(theme.text_dim),
        );
        ui.label(egui::RichText::new("Double-click to apply").size(9.0).color(egui::Color32::from_rgb(100, 100, 110)));
    });

    resp
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
