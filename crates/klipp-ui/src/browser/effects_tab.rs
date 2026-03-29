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

const CATEGORY_SIDEBAR_W: f32 = 110.0;

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
                ui.add_space(6.0);
                ui.spacing_mut().indent = 8.0; // left padding from window edge
                ui.indent("cat_indent", |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;

                // ── VIDEO section ──
                ui.label(
                    egui::RichText::new("VIDEO")
                        .size(10.0)
                        .strong()
                        .color(theme.text_secondary),
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
                    let count = state.effect_registry.filtered(*cat, "").len();

                    // Category row — entire row is clickable
                    let row_resp = ui.horizontal(|ui| {
                        let hovered = ui.rect_contains_pointer(ui.max_rect());
                        let text_color = if is_active {
                            theme.accent
                        } else if hovered {
                            theme.text_primary
                        } else {
                            theme.text_secondary
                        };
                        ui.add(egui::Label::new(egui::RichText::new(*label).size(11.0).color(text_color)).selectable(false));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(12.0);
                            let badge_color = if is_active {
                                egui::Color32::from_rgb(60, 60, 65)
                            } else {
                                egui::Color32::from_rgb(45, 45, 50)
                            };
                            let badge = egui::Button::new(
                                egui::RichText::new(format!("{}", count))
                                    .size(9.0)
                                    .color(theme.text_dim)
                            ).fill(badge_color).rounding(8.0).min_size(egui::vec2(24.0, 16.0));
                            ui.add(badge);
                        });
                    });
                    // Make the whole row clickable
                    let row_rect = row_resp.response.rect;
                    let click_resp = ui.interact(row_rect, ui.id().with(("cat_click", *cat as u8)), egui::Sense::click());

                    if is_active {
                        let bar_rect = egui::Rect::from_min_size(
                            egui::pos2(row_rect.min.x - 4.0, row_rect.min.y + 2.0),
                            egui::vec2(2.0, row_rect.height() - 4.0),
                        );
                        ui.painter().rect_filled(bar_rect, 1.0, theme.accent);
                    }

                    if click_resp.clicked() {
                        *active_category = *cat;
                    }
                }

                // ── Divider ──
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // ── AUDIO section ──
                ui.label(
                    egui::RichText::new("AUDIO")
                        .size(10.0)
                        .strong()
                        .color(theme.text_secondary),
                );
                ui.add_space(2.0);
                for label in ["EQ", "Dynamics", "Reverb"] {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(label).size(11.0).color(theme.text_dim));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(12.0);
                            let badge = egui::Button::new(
                                egui::RichText::new("0").size(9.0).color(egui::Color32::from_rgb(80, 80, 85))
                            ).fill(egui::Color32::from_rgb(40, 40, 45)).rounding(8.0).min_size(egui::vec2(24.0, 16.0));
                            ui.add(badge);
                        });
                    });
                }
                }); // close indent
            });
    });

    // ── Thumbnail grid ──
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(grid_rect), |ui| {
        ui.set_min_size(grid_rect.size());

        // ── Filter toggles ──
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let pill = |ui: &mut egui::Ui, label: &str, active: &mut bool, color: egui::Color32| {
                let bg = if *active { color } else { egui::Color32::from_rgb(50, 50, 55) };
                let text_color = if *active { egui::Color32::WHITE } else { egui::Color32::from_rgb(120, 120, 130) };
                let resp = ui.add(
                    egui::Button::new(egui::RichText::new(label).size(9.0).color(text_color))
                        .fill(bg)
                        .rounding(10.0)
                        .min_size(egui::vec2(0.0, 18.0)),
                );
                if resp.clicked() { *active = !*active; }
            };
            // Count effects by type
            let all_effects = state.effect_registry.filtered(*active_category, search_query);
            let builtin_count = all_effects.iter().filter(|fx| matches!(fx.source, crate::browser::effect_registry::EffectSource::Builtin(_))).count();
            let ofx_count = all_effects.iter().filter(|fx| matches!(fx.source, crate::browser::effect_registry::EffectSource::Ofx(_))).count();

            pill(ui, &format!("Built-in ({})", builtin_count), &mut state.effect_filter_builtin, egui::Color32::from_rgb(60, 150, 80));
            pill(ui, &format!("OFX ({})", ofx_count), &mut state.effect_filter_ofx, egui::Color32::from_rgb(80, 120, 200));
            pill(ui, "Hide unsupported", &mut state.effect_filter_hide_unsupported, egui::Color32::from_rgb(140, 70, 70));
        });
        ui.add_space(2.0);

        // Apply filters
        let filtered: Vec<&_> = state.effect_registry.filtered(*active_category, search_query)
            .into_iter()
            .filter(|fx| {
                let is_builtin = matches!(fx.source, crate::browser::effect_registry::EffectSource::Builtin(_));
                let is_ofx = matches!(fx.source, crate::browser::effect_registry::EffectSource::Ofx(_));

                // Source type filter
                if is_builtin && !state.effect_filter_builtin { return false; }
                if is_ofx && !state.effect_filter_ofx { return false; }

                // Hide unsupported (stereo, render-only)
                if state.effect_filter_hide_unsupported {
                    let id = match &fx.source {
                        crate::browser::effect_registry::EffectSource::Ofx(id) => id.as_str(),
                        _ => "",
                    };
                    if id.contains("anaglyph") || id.contains("joinView") || id.contains("mixView")
                        || id.contains("oneView") || id.contains("sideBySide") {
                        return false;
                    }
                }

                true
            })
            .collect();
        let count = filtered.len();

        // ── Header: count + view toggle ──
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} effects", count))
                    .size(10.0)
                    .color(theme.text_dim),
            );
        });
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .id_salt("browser_effects_scroll")
            .show(ui, |ui| {
                let grid_w = ui.available_width() - 8.0;
                let spacing = 6.0;
                let card_w = ((grid_w - spacing) / 2.0).floor();
                let preview_h = (card_w * 9.0 / 16.0).round();
                let card_h = preview_h + 36.0; // preview + name + category subtitle

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

                // Status bar at bottom
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("{count} items"))
                            .size(9.0)
                            .color(theme.text_dim),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Drag to timeline to apply")
                                .size(9.0)
                                .color(egui::Color32::from_rgb(90, 90, 100)),
                        );
                    });
                });
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

    // Source badge on the preview area (top-right corner)
    let (badge_text, badge_color) = match &fx.source {
        crate::browser::effect_registry::EffectSource::Ofx(_) => ("OFX", egui::Color32::from_rgba_unmultiplied(80, 120, 200, 200)),
        crate::browser::effect_registry::EffectSource::Builtin(_) => ("Built-in", egui::Color32::from_rgba_unmultiplied(60, 150, 80, 200)),
    };
    {
        let badge_font = egui::FontId::monospace(8.0);
        let badge_galley = painter.layout_no_wrap(badge_text.to_string(), badge_font, egui::Color32::WHITE);
        let badge_w = badge_galley.size().x + 6.0;
        let badge_h = badge_galley.size().y + 2.0;
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(card_rect.max.x - badge_w - 2.0, card_rect.min.y + 2.0),
            egui::vec2(badge_w, badge_h),
        );
        painter.rect_filled(badge_rect, 2.0, badge_color);
        painter.galley(
            egui::pos2(badge_rect.min.x + 3.0, badge_rect.min.y + 1.0),
            badge_galley,
            egui::Color32::WHITE,
        );
    }

    // Name + category below preview
    let name_y = card_rect.min.y + preview_h + 4.0;
    let name_galley = painter.layout(
        fx.name.clone(),
        egui::FontId::proportional(11.0),
        if hovered { theme.text_primary } else { theme.text_secondary },
        width - 6.0,
    );
    painter.galley(egui::pos2(card_rect.min.x + 3.0, name_y), name_galley.clone(), theme.text_secondary);

    // Category subtitle
    let cat_y = name_y + name_galley.size().y + 1.0;
    let cat_galley = painter.layout(
        fx.grouping.clone(),
        egui::FontId::proportional(9.0),
        theme.text_dim,
        width - 6.0,
    );
    painter.galley(egui::pos2(card_rect.min.x + 3.0, cat_y), cat_galley, theme.text_dim);

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
        // Limitation notices in tooltip
        let name = &fx.name;
        let plugin_id = match &fx.source {
            crate::browser::effect_registry::EffectSource::Ofx(id) => id.as_str(),
            _ => "",
        };
        if plugin_id.contains("cimg") || plugin_id.contains("CImg") || plugin_id.contains("PIK")
            || plugin_id.contains("Keyer") || plugin_id.contains("IDistort")
            || plugin_id.contains("STMap") || plugin_id.contains("VectorToColor")
        {
            ui.label(egui::RichText::new("CPU only — adjust while paused, apply on export").size(9.0).color(egui::Color32::from_rgb(220, 160, 60)));
        }
        if plugin_id.contains("Retime") || plugin_id.contains("SlitScan") || plugin_id.contains("timeOffset") {
            ui.label(egui::RichText::new("Render only — not real-time, applied on export").size(9.0).color(egui::Color32::from_rgb(200, 100, 100)));
        }
        if plugin_id.contains("anaglyph") || plugin_id.contains("joinView") || plugin_id.contains("mixView")
            || plugin_id.contains("oneView") || plugin_id.contains("sideBySide")
            || name.contains("Anaglyph") || name.contains("Join Views") || name.contains("Mix Views")
            || name.contains("One View") || name.contains("Side By Side")
        {
            ui.label(egui::RichText::new("Requires multi-view — stereo 3D not supported yet").size(9.0).color(egui::Color32::from_rgb(180, 80, 80)));
        }
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
