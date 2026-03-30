use crate::icons::Icon;
use crate::theme::KlippTheme;
use crate::state::AppState;
use crate::widgets::scrubby_param::{scrubby_param_row, KeyframeState};
use klipp_core::AppCommand;
use klipp_timeline::Clip;

/// Empty state — no effects applied (shown for all selection states when no effects exist)
pub fn show_empty(ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>) {
    let available_h = ui.available_height();
    ui.add_space((available_h * 0.15).max(20.0));

    ui.vertical_centered(|ui| {
        // Centered + icon in a rounded square
        let icon_size = 40.0;
        let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(icon_size, icon_size), egui::Sense::hover());
        ui.painter().rect(icon_rect, 8.0, theme.bg_raised, egui::Stroke::new(1.0, theme.border));
        ui.painter().text(
            icon_rect.center(), egui::Align2::CENTER_CENTER,
            "+", egui::FontId::proportional(20.0), theme.text_dim,
        );

        ui.add_space(12.0);

        ui.label(
            egui::RichText::new("No effects applied")
                .size(14.0)
                .color(theme.text_primary),
        );

        ui.add_space(6.0);

        ui.label(
            egui::RichText::new("Add an effect from the Effects Browser or\nuse the button below")
                .size(11.0)
                .color(theme.text_dim),
        );

        ui.add_space(24.0);

        // "+ Add Effect..." button with "E" shortcut badge
        ui.horizontal(|ui| {
            let total_w = ui.available_width();
            let btn_w = total_w - 40.0;
            ui.add_space(10.0);

            let mut job = egui::text::LayoutJob::default();
            job.append("+  ", 0.0, egui::TextFormat {
                font_id: egui::FontId::proportional(12.0),
                color: theme.accent,
                ..Default::default()
            });
            job.append("Add Effect\u{2026}", 0.0, egui::TextFormat {
                font_id: egui::FontId::proportional(12.0),
                color: theme.text_secondary,
                ..Default::default()
            });

            if ui.add_sized(
                egui::vec2(btn_w, 30.0),
                egui::Button::new(job)
                    .fill(theme.bg_raised)
                    .stroke(egui::Stroke::new(1.0, theme.border))
                    .rounding(6.0),
            ).clicked() {
                cmds.push(AppCommand::ShowEffectsBrowser);
            }

            // "E" shortcut badge
            let (badge_rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            ui.painter().rect(badge_rect, 4.0, theme.bg_deep, egui::Stroke::new(0.5, theme.border));
            ui.painter().text(
                badge_rect.center(), egui::Align2::CENTER_CENTER,
                "E", egui::FontId::monospace(10.0), theme.text_dim,
            );
        });
    });

    ui.add_space(16.0);
}

pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
    show_add_builtin: &mut bool,
    show_add_ofx: &mut bool,
    clip_time_secs: f64,
) {
    let clip_id = clip.id;

    let has_any = !clip.builtin_effects.is_empty() || !clip.effects.is_empty();

    if !has_any {
        show_empty(ui, theme, cmds);
        return;
    }

    let pad = 14.0;
    let total_effects = clip.builtin_effects.len() + clip.effects.len();

    // ── "Drag to reorder" hint ──
    if total_effects > 1 {
        let hint_w = ui.available_width();
        let (hint_rect, _) = ui.allocate_exact_size(egui::vec2(hint_w, 20.0), egui::Sense::hover());
        let center = hint_rect.center();
        // Up/down arrows + text
        Icon::CaretUp.image(6.0).tint(theme.text_dim).paint_at(ui, egui::Rect::from_center_size(
            egui::pos2(center.x - 52.0, center.y - 4.0), egui::vec2(8.0, 8.0),
        ));
        Icon::CaretDown.image(6.0).tint(theme.text_dim).paint_at(ui, egui::Rect::from_center_size(
            egui::pos2(center.x - 52.0, center.y + 4.0), egui::vec2(8.0, 8.0),
        ));
        ui.painter().text(
            center, egui::Align2::CENTER_CENTER,
            "Drag to reorder", egui::FontId::proportional(10.0), theme.text_dim,
        );
        ui.add_space(2.0);
    }

    // ── Built-in effects ──
    let mut builtin_remove = None;
    let mut builtin_move_to: Option<(usize, usize)> = None; // (from, to)
    let builtin_reset: Option<(usize, klipp_effects::BuiltinEffect)> = None;
    let mut builtin_card_tops: Vec<f32> = Vec::new(); // y position of each card top
    let mut builtin_drag_info: Option<(usize, f32)> = None; // (dragged index, total_drag)

    let card_bg = theme.bg_raised;
    let card_header_bg = egui::Color32::from_rgb(
        (card_bg.r() as u16).saturating_sub(8) as u8,
        (card_bg.g() as u16).saturating_sub(8) as u8,
        (card_bg.b() as u16).saturating_sub(8) as u8,
    );
    let card_rounding = 6.0;
    let header_h = 32.0;
    let badge_green = egui::Color32::from_rgb(60, 160, 80);
    let badge_gray = egui::Color32::from_rgb(100, 110, 130);

    for (i, effect) in clip.builtin_effects.iter().enumerate() {
        let name = builtin_effect_name(effect);
        let header_id = ui.make_persistent_id(format!("bfx_{clip_id:?}_{i}"));
        let mut col_state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(), header_id, true,
        );
        let is_open = col_state.is_open();

        let card_w = ui.available_width();

        // ── Card header ──
        let (header_rect, header_resp) = ui.allocate_exact_size(
            egui::vec2(card_w, header_h), egui::Sense::click_and_drag(),
        );

        // Header background — top corners rounded, bottom only if collapsed
        let header_rounding = if is_open {
            egui::Rounding { nw: card_rounding, ne: card_rounding, sw: 0.0, se: 0.0 }
        } else {
            egui::Rounding::same(card_rounding)
        };
        ui.painter().rect_filled(header_rect, header_rounding, card_header_bg);
        // Border on header
        ui.painter().rect_stroke(header_rect, header_rounding, egui::Stroke::new(0.5, theme.border));

        // Collapse caret (SVG icon, not unicode)
        let caret_x = header_rect.min.x + pad;
        let caret_icon = if is_open { Icon::CaretDown } else { Icon::CaretRight };
        let caret_rect = egui::Rect::from_center_size(
            egui::pos2(caret_x + 5.0, header_rect.center().y),
            egui::vec2(10.0, 10.0),
        );
        caret_icon.image(10.0).tint(theme.text_dim).paint_at(ui, caret_rect);

        // Effect name (bold, 13px)
        let name_x = caret_x + 18.0;
        ui.painter().text(
            egui::pos2(name_x, header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            name,
            egui::FontId::proportional(13.0),
            theme.text_primary,
        );

        // "Built-in" badge — outlined pill, green
        let badge_text = "Built-in";
        let badge_font = egui::FontId::proportional(9.0);
        let badge_galley = ui.painter().layout_no_wrap(badge_text.to_string(), badge_font.clone(), badge_green);
        let badge_w = badge_galley.size().x + 10.0;
        let badge_h = 16.0;
        let name_galley = ui.painter().layout_no_wrap(name.to_string(), egui::FontId::proportional(13.0), theme.text_primary);
        let badge_x = name_x + name_galley.size().x + 8.0;
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(badge_x, header_rect.center().y - badge_h / 2.0),
            egui::vec2(badge_w, badge_h),
        );
        ui.painter().rect_stroke(badge_rect, badge_h / 2.0, egui::Stroke::new(0.8, badge_green));
        ui.painter().text(
            badge_rect.center(), egui::Align2::CENTER_CENTER,
            badge_text, badge_font, badge_green,
        );

        // Right side: × remove button
        let remove_x = header_rect.max.x - pad - 6.0;
        let remove_center = egui::pos2(remove_x, header_rect.center().y);
        let remove_rect = egui::Rect::from_center_size(remove_center, egui::vec2(18.0, 18.0));
        let remove_resp = ui.interact(remove_rect, ui.id().with(("bfx_rm", i)), egui::Sense::click());
        let remove_color = if remove_resp.hovered() { theme.text_primary } else { theme.text_dim };
        ui.painter().text(
            remove_center, egui::Align2::CENTER_CENTER,
            "\u{00d7}", egui::FontId::proportional(15.0), remove_color,
        );
        let remove_clicked = remove_resp.on_hover_text("Remove effect").clicked();
        if remove_clicked {
            builtin_remove = Some(i);
        }

        // Enable/disable toggle switch
        let is_enabled = clip.builtin_effects_enabled.get(i).copied().unwrap_or(true);
        let toggle_x = remove_x - 40.0;
        let toggle_w = 30.0;
        let toggle_h = 14.0;
        let toggle_rect = egui::Rect::from_min_size(
            egui::pos2(toggle_x - toggle_w / 2.0, header_rect.center().y - toggle_h / 2.0),
            egui::vec2(toggle_w, toggle_h),
        );
        let toggle_resp = ui.interact(toggle_rect, ui.id().with(("bfx_tog", i)), egui::Sense::click());
        let track_color = if is_enabled { theme.accent } else { egui::Color32::from_rgb(60, 60, 65) };
        ui.painter().rect_filled(toggle_rect, toggle_h / 2.0, track_color);
        let thumb_r = toggle_h / 2.0 - 1.0;
        let thumb_x = if is_enabled { toggle_rect.max.x - thumb_r - 2.0 } else { toggle_rect.min.x + thumb_r + 2.0 };
        ui.painter().circle_filled(egui::pos2(thumb_x, toggle_rect.center().y), thumb_r, egui::Color32::WHITE);
        let toggle_clicked = toggle_resp.on_hover_text(if is_enabled { "Disable effect" } else { "Enable effect" }).clicked();
        if toggle_clicked {
            cmds.push(AppCommand::ToggleBuiltinEffectEnabled { clip_id, index: i });
        }

        // Click header to toggle collapse (only if not on remove/toggle buttons)
        if header_resp.clicked() && !remove_clicked && !toggle_clicked {
            col_state.toggle(ui);
        }

        col_state.store(ui.ctx());

        // ── Card body ──
        if is_open {
            let body_frame = egui::Frame::none()
                .fill(card_bg)
                .stroke(egui::Stroke::new(0.5, theme.border))
                .rounding(egui::Rounding { nw: 0.0, ne: 0.0, sw: card_rounding, se: card_rounding })
                .inner_margin(egui::Margin { left: pad, right: pad, top: 8.0, bottom: 8.0 });
            body_frame.show(ui, |ui: &mut egui::Ui| {
                if let Some(updated) = show_builtin_effect_params(ui, theme, effect, clip_id, i, cmds, clip, clip_time_secs, state) {
                    cmds.push(AppCommand::UpdateBuiltinEffect {
                        clip_id,
                        index: i,
                        effect: updated,
                    });
                }
            });
        }

        // Track card position for drop indicator
        builtin_card_tops.push(header_rect.min.y);

        // Drag visual feedback + reorder
        if header_resp.dragged_by(egui::PointerButton::Primary) {
            ui.painter().rect_stroke(
                header_rect,
                if is_open { egui::Rounding { nw: card_rounding, ne: card_rounding, sw: 0.0, se: 0.0 } } else { egui::Rounding::same(card_rounding) },
                egui::Stroke::new(2.0, theme.accent),
            );
            let drag_dy = header_resp.drag_delta().y;
            let total_drag: f32 = ui.ctx().data_mut(|d| {
                let key = egui::Id::new("bfx_drag_accum").with(i);
                let accum: &mut f32 = d.get_temp_mut_or_default(key);
                *accum += drag_dy;
                *accum
            });
            builtin_drag_info = Some((i, total_drag));
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if header_resp.drag_stopped() {
            let total_drag: f32 = ui.ctx().data_mut(|d| {
                let key = egui::Id::new("bfx_drag_accum").with(i);
                let accum = d.get_temp::<f32>(key).unwrap_or(0.0);
                d.remove_temp::<f32>(key);
                accum
            });
            let slot_h = 40.0_f32;
            let steps = (total_drag.abs() / slot_h).floor() as usize;
            if steps > 0 {
                let max_idx = clip.builtin_effects.len() - 1;
                let target = if total_drag < 0.0 { i.saturating_sub(steps) } else { (i + steps).min(max_idx) };
                if target != i { builtin_move_to = Some((i, target)); }
            }
        }
        if header_resp.hovered() && !header_resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }

        ui.add_space(4.0);
    }

    if let Some(idx) = builtin_remove {
        cmds.push(AppCommand::RemoveBuiltinEffect { clip_id, index: idx });
    }
    if let Some((from, to)) = builtin_move_to {
        cmds.push(AppCommand::ReorderBuiltinEffect { clip_id, from_index: from, to_index: to });
    }
    if let Some((idx, default_effect)) = builtin_reset {
        cmds.push(AppCommand::UpdateBuiltinEffect { clip_id, index: idx, effect: default_effect });
    }

    // Draw builtin drop indicator at the target position
    if let Some((from_idx, total_drag)) = builtin_drag_info {
        let slot_h = 40.0_f32;
        let steps = (total_drag.abs() / slot_h).floor() as usize;
        if steps > 0 {
            let max_idx = clip.builtin_effects.len().saturating_sub(1);
            let target = if total_drag < 0.0 { from_idx.saturating_sub(steps) } else { (from_idx + steps).min(max_idx) };
            if target != from_idx {
                // Draw indicator line at target card position
                let line_y = if target < builtin_card_tops.len() {
                    if total_drag < 0.0 {
                        // Moving up: line above the target card
                        builtin_card_tops[target] - 2.0
                    } else {
                        // Moving down: line below the target card
                        if target + 1 < builtin_card_tops.len() {
                            builtin_card_tops[target + 1] - 2.0
                        } else {
                            // Last card — use current cursor position
                            ui.cursor().min.y - 4.0
                        }
                    }
                } else {
                    ui.cursor().min.y - 4.0
                };
                let x_range = if let Some(&_first_top) = builtin_card_tops.first() {
                    let w = ui.available_width();
                    (ui.min_rect().min.x)..=(ui.min_rect().min.x + w)
                } else {
                    0.0..=0.0
                };
                ui.painter().hline(x_range, line_y, egui::Stroke::new(2.5, theme.accent));
            }
        }
    }

    // ── OFX effects ──
    let mut ofx_remove = None;
    let mut ofx_move_to: Option<(usize, usize)> = None;

    for (i, effect) in clip.effects.iter().enumerate() {
        let display_name = effect.plugin_name
            .rsplit_once('.')
            .map(|(_, name)| name)
            .unwrap_or(&effect.plugin_name);

        let header_id = ui.make_persistent_id(format!("ofx_{clip_id:?}_{i}"));
        let mut col_state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(), header_id, true,
        );
        let is_open = col_state.is_open();
        let card_w = ui.available_width();

        // ── Card header ──
        let (header_rect, header_resp) = ui.allocate_exact_size(
            egui::vec2(card_w, header_h), egui::Sense::click_and_drag(),
        );

        let header_rounding = if is_open {
            egui::Rounding { nw: card_rounding, ne: card_rounding, sw: 0.0, se: 0.0 }
        } else {
            egui::Rounding::same(card_rounding)
        };
        ui.painter().rect_filled(header_rect, header_rounding, card_header_bg);
        ui.painter().rect_stroke(header_rect, header_rounding, egui::Stroke::new(0.5, theme.border));

        // Collapse caret (SVG icon)
        let caret_x = header_rect.min.x + pad;
        let caret_icon = if is_open { Icon::CaretDown } else { Icon::CaretRight };
        caret_icon.image(10.0).tint(theme.text_dim).paint_at(ui, egui::Rect::from_center_size(
            egui::pos2(caret_x + 5.0, header_rect.center().y), egui::vec2(10.0, 10.0),
        ));

        // Effect name
        let name_x = caret_x + 18.0;
        ui.painter().text(
            egui::pos2(name_x, header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            display_name,
            egui::FontId::proportional(13.0),
            theme.text_primary,
        );

        // "OFX" badge — outlined pill, gray
        let badge_text = "OFX";
        let badge_font = egui::FontId::proportional(9.0);
        let badge_galley = ui.painter().layout_no_wrap(badge_text.to_string(), badge_font.clone(), badge_gray);
        let badge_w = badge_galley.size().x + 10.0;
        let badge_h = 16.0;
        let name_galley = ui.painter().layout_no_wrap(display_name.to_string(), egui::FontId::proportional(13.0), theme.text_primary);
        let badge_x = name_x + name_galley.size().x + 8.0;
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(badge_x, header_rect.center().y - badge_h / 2.0),
            egui::vec2(badge_w, badge_h),
        );
        ui.painter().rect_stroke(badge_rect, badge_h / 2.0, egui::Stroke::new(0.8, badge_gray));
        ui.painter().text(
            badge_rect.center(), egui::Align2::CENTER_CENTER,
            badge_text, badge_font, badge_gray,
        );

        // Right side: × remove button
        let remove_x = header_rect.max.x - pad - 6.0;
        let remove_center = egui::pos2(remove_x, header_rect.center().y);
        let remove_rect = egui::Rect::from_center_size(remove_center, egui::vec2(18.0, 18.0));
        let remove_resp = ui.interact(remove_rect, ui.id().with(("ofx_rm", i)), egui::Sense::click());
        let remove_color = if remove_resp.hovered() { theme.text_primary } else { theme.text_dim };
        ui.painter().text(
            remove_center, egui::Align2::CENTER_CENTER,
            "\u{00d7}", egui::FontId::proportional(15.0), remove_color,
        );
        let remove_clicked = remove_resp.on_hover_text("Remove effect").clicked();
        if remove_clicked {
            ofx_remove = Some(i);
        }

        // Enable/disable toggle switch (orange when on, gray when off)
        let toggle_x = remove_x - 40.0;
        let toggle_center_y = header_rect.center().y;
        let toggle_w = 30.0;
        let toggle_h = 16.0;
        let toggle_rect = egui::Rect::from_min_size(
            egui::pos2(toggle_x - toggle_w / 2.0, toggle_center_y - toggle_h / 2.0),
            egui::vec2(toggle_w, toggle_h),
        );
        let toggle_resp = ui.interact(toggle_rect, ui.id().with(("ofx_tog", i)), egui::Sense::click());
        let enabled = effect.enabled;
        let track_color = if enabled { theme.accent } else { egui::Color32::from_rgb(80, 80, 90) };
        ui.painter().rect_filled(toggle_rect, toggle_h / 2.0, track_color);
        let thumb_x = if enabled { toggle_rect.max.x - toggle_h / 2.0 } else { toggle_rect.min.x + toggle_h / 2.0 };
        ui.painter().circle_filled(
            egui::pos2(thumb_x, toggle_center_y),
            toggle_h / 2.0 - 2.0,
            egui::Color32::WHITE,
        );
        let toggle_clicked = toggle_resp.on_hover_text(if enabled { "Disable" } else { "Enable" }).clicked();
        if toggle_clicked {
            cmds.push(AppCommand::SetEffectEnabled {
                clip_id,
                effect_index: i,
                enabled: !enabled,
            });
        }

        // Click header to toggle collapse
        if header_resp.clicked() && !remove_clicked && !toggle_clicked {
            col_state.toggle(ui);
        }

        col_state.store(ui.ctx());

        // ── Card body ──
        if is_open {
            let body_frame = egui::Frame::none()
                .fill(card_bg)
                .stroke(egui::Stroke::new(0.5, theme.border))
                .rounding(egui::Rounding { nw: 0.0, ne: 0.0, sw: card_rounding, se: card_rounding })
                .inner_margin(egui::Margin { left: pad, right: pad, top: 8.0, bottom: 8.0 });
            body_frame.show(ui, |ui: &mut egui::Ui| {
                // CPU-only notice for float-only plugins (CImg, keyers, etc.)
                let is_float_plugin = effect.plugin_name.contains("cimg")
                    || effect.plugin_name.contains("CImg")
                    || effect.plugin_name.contains("PIK")
                    || effect.plugin_name.contains("Keyer")
                    || effect.plugin_name.contains("IDistort")
                    || effect.plugin_name.contains("STMap")
                    || effect.plugin_name.contains("VectorToColor")
                    || effect.plugin_name.contains("ContactSheet");
                if is_float_plugin {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("CPU only").size(9.0).color(egui::Color32::from_rgb(220, 160, 60)).strong());
                        ui.label(egui::RichText::new("\u{2014} adjust while paused, apply on export").size(9.0).color(theme.text_dim));
                    });
                    ui.add_space(2.0);
                }

                // Render-only notice for temporal plugins
                let is_temporal = effect.plugin_name.contains("Retime")
                    || effect.plugin_name.contains("SlitScan")
                    || effect.plugin_name.contains("timeOffset")
                    || effect.plugin_name.contains("TimeOffset");
                if is_temporal {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Render only").size(9.0).color(egui::Color32::from_rgb(200, 100, 100)).strong());
                        ui.label(egui::RichText::new("\u{2014} not real-time, applied on export").size(9.0).color(theme.text_dim));
                    });
                    ui.add_space(2.0);
                }

                // Multi-view/stereo notice
                let is_stereo = effect.plugin_name.contains("anaglyph")
                    || effect.plugin_name.contains("Anaglyph")
                    || effect.plugin_name.contains("joinViews")
                    || effect.plugin_name.contains("JoinView")
                    || effect.plugin_name.contains("mixViews")
                    || effect.plugin_name.contains("MixView")
                    || effect.plugin_name.contains("oneView")
                    || effect.plugin_name.contains("OneView")
                    || effect.plugin_name.contains("sideBySide")
                    || effect.plugin_name.contains("SideBySide");
                if is_stereo {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Requires multi-view").size(9.0).color(egui::Color32::from_rgb(180, 80, 80)).strong());
                        ui.label(egui::RichText::new("\u{2014} stereo 3D not supported yet").size(9.0).color(theme.text_dim));
                    });
                    ui.add_space(2.0);
                }

                // Show parameter controls if plugin has declared params
                if let Some(plugin_info) = state.available_plugins.iter().find(|p| p.name == effect.plugin_name) {
                    for desc in &plugin_info.parameters {
                        if desc.secret {
                            continue;
                        }
                        match desc.param_type {
                            klipp_plugin::ParamType::Group | klipp_plugin::ParamType::Page => continue,
                            _ => {}
                        }
                        show_ofx_param(ui, theme, clip_id, i, effect, desc, cmds);
                    }
                    if plugin_info.parameters.is_empty() {
                        ui.label(
                            egui::RichText::new("No parameters")
                                .size(10.0)
                                .color(theme.text_dim),
                        );
                    }
                }
            });
        }

        // Drag visual feedback + reorder
        if header_resp.dragged_by(egui::PointerButton::Primary) {
            ui.painter().rect_stroke(
                header_rect,
                if is_open { egui::Rounding { nw: card_rounding, ne: card_rounding, sw: 0.0, se: 0.0 } } else { egui::Rounding::same(card_rounding) },
                egui::Stroke::new(2.0, theme.accent),
            );
            let drag_dy = header_resp.drag_delta().y;
            let total_drag: f32 = ui.ctx().data_mut(|d| {
                let key = egui::Id::new("ofx_drag_accum").with(i);
                let accum: &mut f32 = d.get_temp_mut_or_default(key);
                *accum += drag_dy;
                *accum
            });
            if total_drag < -15.0 && i > 0 {
                ui.painter().hline(header_rect.x_range(), header_rect.min.y - 2.0, egui::Stroke::new(2.0, theme.accent));
            } else if total_drag > 15.0 && i + 1 < clip.effects.len() {
                let line_y = if is_open { ui.cursor().min.y + 2.0 } else { header_rect.max.y + 2.0 };
                ui.painter().hline(header_rect.x_range(), line_y, egui::Stroke::new(2.0, theme.accent));
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if header_resp.drag_stopped() {
            let total_drag: f32 = ui.ctx().data_mut(|d| {
                let key = egui::Id::new("ofx_drag_accum").with(i);
                let accum = d.get_temp::<f32>(key).unwrap_or(0.0);
                d.remove_temp::<f32>(key);
                accum
            });
            let slot_h = 40.0_f32;
            let steps = (total_drag.abs() / slot_h).floor() as usize;
            if steps > 0 {
                let max_idx = clip.effects.len() - 1;
                let target = if total_drag < 0.0 {
                    i.saturating_sub(steps)
                } else {
                    (i + steps).min(max_idx)
                };
                if target != i {
                    ofx_move_to = Some((i, target));
                }
            }
        }
        if header_resp.hovered() && !header_resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }

        ui.add_space(4.0);
    }

    if let Some(idx) = ofx_remove {
        cmds.push(AppCommand::RemoveEffect { clip_id, effect_index: idx });
    }
    if let Some((from, to)) = ofx_move_to {
        cmds.push(AppCommand::ReorderEffect { clip_id, from_index: from, to_index: to });
    }

    // ── Add Effect button (dashed border, accent "+", "E" shortcut badge) ──
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        let total_w = ui.available_width();
        let btn_w = total_w - 36.0;
        ui.add_space(4.0);

        let mut job = egui::text::LayoutJob::default();
        job.append("+  ", 0.0, egui::TextFormat {
            font_id: egui::FontId::proportional(13.0),
            color: theme.accent,
            ..Default::default()
        });
        job.append("Add Effect\u{2026}", 0.0, egui::TextFormat {
            font_id: egui::FontId::proportional(12.0),
            color: theme.text_secondary,
            ..Default::default()
        });

        // Dashed border effect: transparent fill + dashed stroke (egui doesn't do dashed natively,
        // so we use a dotted-looking light stroke)
        let btn = egui::Button::new(job)
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::new(1.0, theme.border_light))
            .rounding(6.0);

        let resp = ui.add_sized(egui::vec2(btn_w, 32.0), btn);

        // Draw dashed overlay on the button rect
        let btn_rect = resp.rect;
        let dash_color = theme.border_light;
        // Horizontal dashes (top and bottom)
        let dash_len = 6.0;
        let gap_len = 4.0;
        for &y in &[btn_rect.min.y, btn_rect.max.y] {
            let mut x = btn_rect.min.x + card_rounding;
            while x < btn_rect.max.x - card_rounding {
                let end_x = (x + dash_len).min(btn_rect.max.x - card_rounding);
                ui.painter().line_segment(
                    [egui::pos2(x, y), egui::pos2(end_x, y)],
                    egui::Stroke::new(1.0, dash_color),
                );
                x += dash_len + gap_len;
            }
        }

        if resp.clicked() {
            cmds.push(AppCommand::ShowEffectsBrowser);
        }

        // "E" shortcut badge
        let (badge_rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
        ui.painter().rect(badge_rect, 4.0, theme.bg_deep, egui::Stroke::new(0.5, theme.border));
        ui.painter().text(
            badge_rect.center(), egui::Align2::CENTER_CENTER,
            "E", egui::FontId::monospace(10.0), theme.text_dim,
        );
    });

    // Inline add menus (kept for compatibility)
    if *show_add_builtin {
        ui.add_space(4.0);
        let effects = [
            ("Opacity", klipp_effects::BuiltinEffect::Opacity { value: 1.0 }),
            ("Gaussian Blur", klipp_effects::BuiltinEffect::GaussianBlur { radius: 0.0 }),
            ("Sharpen", klipp_effects::BuiltinEffect::Sharpen { amount: 0.0 }),
            ("Crop", klipp_effects::BuiltinEffect::Crop {
                left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0,
            }),
            ("Transform", klipp_effects::BuiltinEffect::Transform {
                position_x: 0.0, position_y: 0.0,
                scale_x: 1.0, scale_y: 1.0,
                rotation: 0.0,
                anchor_x: 0.5, anchor_y: 0.5,
            }),
            ("Chroma Key", klipp_effects::BuiltinEffect::ChromaKey {
                key_color: [0, 255, 0],
                tolerance: 0.2,
                spill_suppression: 0.1,
            }),
        ];
        for (label, effect) in &effects {
            if ui.small_button(*label).clicked() {
                cmds.push(AppCommand::AddBuiltinEffect {
                    clip_id,
                    effect: effect.clone(),
                });
                *show_add_builtin = false;
            }
        }
        ui.add_space(4.0);
        if ui.small_button("Cancel").clicked() {
            *show_add_builtin = false;
        }
    }

    if *show_add_ofx {
        ui.add_space(4.0);
        if state.available_plugins.is_empty() {
            ui.label(
                egui::RichText::new("No OFX plugins found")
                    .size(10.0)
                    .color(theme.text_dim),
            );
        }
        for plugin in &state.available_plugins {
            let display_name = plugin.name
                .rsplit_once('.')
                .map(|(_, name)| name)
                .unwrap_or(&plugin.name);
            let label = format!("{} v{}.{}", display_name, plugin.version.0, plugin.version.1);
            if ui.small_button(&label).clicked() {
                cmds.push(AppCommand::AddEffect {
                    clip_id,
                    plugin_name: plugin.name.clone(),
                });
                *show_add_ofx = false;
            }
        }
        ui.add_space(4.0);
        if ui.small_button("Cancel").clicked() {
            *show_add_ofx = false;
        }
    }
}

fn show_ofx_param(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    effect_index: usize,
    effect: &klipp_plugin::AppliedEffect,
    desc: &klipp_plugin::ParamDescriptor,
    cmds: &mut Vec<AppCommand>,
) {
    let current_value = effect.param_values.get(&desc.name);

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(&desc.label)
                .size(10.0)
                .color(egui::Color32::from_rgb(160, 160, 170)),
        );
        ui.add_space(4.0);

        match desc.param_type {
            klipp_plugin::ParamType::Double => {
                let mut v = current_value
                    .and_then(|cv| cv.as_double())
                    .or(match &desc.default { klipp_plugin::ParamValue::Double(d) => Some(*d), _ => None })
                    .unwrap_or(0.0) as f32;
                let dmin = desc.display_min.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Double(d) => Some(*d as f32), _ => None })
                    .unwrap_or(0.0);
                let dmax = desc.display_max.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Double(d) => Some(*d as f32), _ => None })
                    .unwrap_or(1.0);
                if ui.add(egui::Slider::new(&mut v, dmin..=dmax).step_by(0.01_f64)).changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id,
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Double(v as f64),
                    });
                }
            }
            klipp_plugin::ParamType::Int => {
                let mut v = current_value
                    .and_then(|cv| cv.as_int())
                    .or(match &desc.default { klipp_plugin::ParamValue::Int(i) => Some(*i), _ => None })
                    .unwrap_or(0);
                let imin = desc.min.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Int(i) => Some(*i), _ => None })
                    .unwrap_or(0);
                let imax = desc.max.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Int(i) => Some(*i), _ => None })
                    .unwrap_or(100);
                if ui.add(egui::DragValue::new(&mut v).range(imin..=imax).speed(1)).changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id,
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Int(v),
                    });
                }
            }
            klipp_plugin::ParamType::Bool => {
                let mut v = current_value
                    .and_then(|cv| cv.as_bool())
                    .or(match &desc.default { klipp_plugin::ParamValue::Bool(b) => Some(*b), _ => None })
                    .unwrap_or(false);
                if ui.checkbox(&mut v, "").changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id,
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Bool(v),
                    });
                }
            }
            klipp_plugin::ParamType::Choice => {
                let mut v = current_value
                    .and_then(|cv| cv.as_int())
                    .or(match &desc.default { klipp_plugin::ParamValue::Choice(c) => Some(*c), _ => None })
                    .unwrap_or(0);
                let selected_text = desc.choices.get(v as usize)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                egui::ComboBox::from_id_salt(egui::Id::new(("ofx_choice", clip_id, effect_index, &desc.name)))
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for (ci, choice) in desc.choices.iter().enumerate() {
                            if ui.selectable_value(&mut v, ci as i32, choice).changed() {
                                cmds.push(AppCommand::SetEffectParam {
                                    clip_id,
                                    effect_index,
                                    param_name: desc.name.clone(),
                                    value: klipp_plugin::OfxValue::Choice(v),
                                });
                            }
                        }
                    });
            }
            klipp_plugin::ParamType::Rgba => {
                let (r, g, b, a) = match current_value {
                    Some(klipp_plugin::OfxValue::Rgba(r, g, b, a)) => (*r, *g, *b, *a),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Rgba(r, g, b, a) => (*r, *g, *b, *a),
                        _ => (1.0, 1.0, 1.0, 1.0),
                    },
                };
                let mut color = egui::Color32::from_rgba_unmultiplied(
                    (r * 255.0) as u8, (g * 255.0) as u8,
                    (b * 255.0) as u8, (a * 255.0) as u8,
                );
                if ui.color_edit_button_srgba(&mut color).changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id,
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Rgba(
                            color.r() as f64 / 255.0,
                            color.g() as f64 / 255.0,
                            color.b() as f64 / 255.0,
                            color.a() as f64 / 255.0,
                        ),
                    });
                }
            }
            klipp_plugin::ParamType::Rgb => {
                let (r, g, b) = match current_value {
                    Some(klipp_plugin::OfxValue::Rgb(r, g, b)) => (*r, *g, *b),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Rgb(r, g, b) => (*r, *g, *b),
                        _ => (1.0, 1.0, 1.0),
                    },
                };
                let mut color = egui::Color32::from_rgb(
                    (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8,
                );
                if ui.color_edit_button_srgba(&mut color).changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id,
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Rgb(
                            color.r() as f64 / 255.0,
                            color.g() as f64 / 255.0,
                            color.b() as f64 / 255.0,
                        ),
                    });
                }
            }
            klipp_plugin::ParamType::Double2 => {
                let (mut v0, mut v1) = match current_value {
                    Some(klipp_plugin::OfxValue::Double2(a, b)) => (*a as f32, *b as f32),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Double2(a, b) => (*a as f32, *b as f32),
                        _ => (0.0, 0.0),
                    },
                };
                let dmin = desc.display_min.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Double2(a, _) => Some(*a as f32), klipp_plugin::ParamValue::Double(a) => Some(*a as f32), _ => None })
                    .unwrap_or(0.0);
                let dmax = desc.display_max.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Double2(a, _) => Some(*a as f32), klipp_plugin::ParamValue::Double(a) => Some(*a as f32), _ => None })
                    .unwrap_or(1000.0);
                ui.horizontal(|ui| {
                    let changed0 = ui.add(egui::DragValue::new(&mut v0).range(dmin..=dmax).speed(0.1).prefix("x: ")).changed();
                    let changed1 = ui.add(egui::DragValue::new(&mut v1).range(dmin..=dmax).speed(0.1).prefix("y: ")).changed();
                    if changed0 || changed1 {
                        cmds.push(AppCommand::SetEffectParam {
                            clip_id, effect_index,
                            param_name: desc.name.clone(),
                            value: klipp_plugin::OfxValue::Double2(v0 as f64, v1 as f64),
                        });
                    }
                });
            }
            klipp_plugin::ParamType::Double3 => {
                let (mut v0, mut v1, mut v2) = match current_value {
                    Some(klipp_plugin::OfxValue::Double3(a, b, c)) => (*a as f32, *b as f32, *c as f32),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Double3(a, b, c) => (*a as f32, *b as f32, *c as f32),
                        _ => (0.0, 0.0, 0.0),
                    },
                };
                ui.horizontal(|ui| {
                    let c0 = ui.add(egui::DragValue::new(&mut v0).speed(0.1).prefix("x: ")).changed();
                    let c1 = ui.add(egui::DragValue::new(&mut v1).speed(0.1).prefix("y: ")).changed();
                    let c2 = ui.add(egui::DragValue::new(&mut v2).speed(0.1).prefix("z: ")).changed();
                    if c0 || c1 || c2 {
                        cmds.push(AppCommand::SetEffectParam {
                            clip_id, effect_index,
                            param_name: desc.name.clone(),
                            value: klipp_plugin::OfxValue::Double3(v0 as f64, v1 as f64, v2 as f64),
                        });
                    }
                });
            }
            klipp_plugin::ParamType::Int2 => {
                let (mut v0, mut v1) = match current_value {
                    Some(klipp_plugin::OfxValue::Int2(a, b)) => (*a, *b),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Int2(a, b) => (*a, *b),
                        _ => (0, 0),
                    },
                };
                ui.horizontal(|ui| {
                    let c0 = ui.add(egui::DragValue::new(&mut v0).speed(1).prefix("x: ")).changed();
                    let c1 = ui.add(egui::DragValue::new(&mut v1).speed(1).prefix("y: ")).changed();
                    if c0 || c1 {
                        cmds.push(AppCommand::SetEffectParam {
                            clip_id, effect_index,
                            param_name: desc.name.clone(),
                            value: klipp_plugin::OfxValue::Int2(v0, v1),
                        });
                    }
                });
            }
            klipp_plugin::ParamType::Int3 => {
                let (mut v0, mut v1, mut v2) = match current_value {
                    Some(klipp_plugin::OfxValue::Int3(a, b, c)) => (*a, *b, *c),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Int3(a, b, c) => (*a, *b, *c),
                        _ => (0, 0, 0),
                    },
                };
                ui.horizontal(|ui| {
                    let c0 = ui.add(egui::DragValue::new(&mut v0).speed(1).prefix("x: ")).changed();
                    let c1 = ui.add(egui::DragValue::new(&mut v1).speed(1).prefix("y: ")).changed();
                    let c2 = ui.add(egui::DragValue::new(&mut v2).speed(1).prefix("z: ")).changed();
                    if c0 || c1 || c2 {
                        cmds.push(AppCommand::SetEffectParam {
                            clip_id, effect_index,
                            param_name: desc.name.clone(),
                            value: klipp_plugin::OfxValue::Int3(v0, v1, v2),
                        });
                    }
                });
            }
            klipp_plugin::ParamType::String => {
                let mut v = match current_value {
                    Some(klipp_plugin::OfxValue::String(s)) => s.clone(),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::String(s) => s.clone(),
                        _ => String::new(),
                    },
                };
                if ui.text_edit_singleline(&mut v).changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id, effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::String(v),
                    });
                }
            }
            _ => {
                ui.label(
                    egui::RichText::new(format!("{:?}", desc.param_type))
                        .size(10.0)
                        .color(egui::Color32::from_rgb(100, 100, 110)),
                );
            }
        }
    });
}

fn builtin_effect_name(effect: &klipp_effects::BuiltinEffect) -> &'static str {
    match effect {
        klipp_effects::BuiltinEffect::Opacity { .. } => "Opacity",
        klipp_effects::BuiltinEffect::GaussianBlur { .. } => "Gaussian Blur",
        klipp_effects::BuiltinEffect::Sharpen { .. } => "Sharpen",
        klipp_effects::BuiltinEffect::Crop { .. } => "Crop",
        klipp_effects::BuiltinEffect::Transform { .. } => "Transform",
        klipp_effects::BuiltinEffect::ChromaKey { .. } => "Chroma Key",
        klipp_effects::BuiltinEffect::Bloom { .. } => "Bloom",
        klipp_effects::BuiltinEffect::Glow { .. } => "Glow",
        klipp_effects::BuiltinEffect::LightRays { .. } => "Light Rays",
        klipp_effects::BuiltinEffect::Vignette { .. } => "Vignette",
        klipp_effects::BuiltinEffect::Diffusion { .. } => "Diffusion",
        klipp_effects::BuiltinEffect::LensFlare { .. } => "Lens Flare",
        klipp_effects::BuiltinEffect::LightWrap { .. } => "Light Wrap",
        klipp_effects::BuiltinEffect::AnamorphicStreak { .. } => "Anamorphic Streak",
        klipp_effects::BuiltinEffect::FilmHalation { .. } => "Film Halation",
        klipp_effects::BuiltinEffect::SpeedRamp { .. } => "Speed Ramp",
    }
}

/// Returns the default (identity) version of the same effect variant.
#[allow(dead_code)]
#[allow(dead_code)]
fn builtin_effect_default(effect: &klipp_effects::BuiltinEffect) -> klipp_effects::BuiltinEffect {
    use klipp_effects::BuiltinEffect;
    match effect {
        BuiltinEffect::Opacity { .. } => BuiltinEffect::Opacity { value: 1.0 },
        BuiltinEffect::GaussianBlur { .. } => BuiltinEffect::GaussianBlur { radius: 0.0 },
        BuiltinEffect::Sharpen { .. } => BuiltinEffect::Sharpen { amount: 0.0 },
        BuiltinEffect::Crop { .. } => BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0 },
        BuiltinEffect::Transform { .. } => BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
        },
        BuiltinEffect::ChromaKey { .. } => BuiltinEffect::ChromaKey {
            key_color: [0, 255, 0], tolerance: 0.2, spill_suppression: 0.1,
        },
        BuiltinEffect::Bloom { .. } => BuiltinEffect::Bloom {
            threshold: 0.7, radius: 20.0, intensity: 0.5,
        },
        BuiltinEffect::Glow { .. } => BuiltinEffect::Glow {
            radius: 30.0, intensity: 0.4,
        },
        BuiltinEffect::LightRays { .. } => BuiltinEffect::LightRays {
            center_x: 0.5, center_y: 0.3, threshold: 0.6, samples: 64, decay: 0.3, intensity: 0.5,
        },
        BuiltinEffect::Vignette { .. } => BuiltinEffect::Vignette {
            amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.5,
        },
        BuiltinEffect::Diffusion { .. } => BuiltinEffect::Diffusion {
            amount: 0.3, radius: 25.0, threshold: 0.5, desaturation: 0.2,
        },
        BuiltinEffect::LensFlare { .. } => BuiltinEffect::LensFlare {
            position_x: 0.7, position_y: 0.3, brightness: 0.6,
            flare_size: 0.15, ghost_count: 4, ghost_intensity: 0.3,
            chromatic_aberration: 0.4,
        },
        BuiltinEffect::LightWrap { .. } => BuiltinEffect::LightWrap {
            width: 15.0, intensity: 0.5, softness: 1.0,
        },
        BuiltinEffect::AnamorphicStreak { .. } => BuiltinEffect::AnamorphicStreak {
            threshold: 0.7, length: 80.0, intensity: 0.5, tint: 0.3,
        },
        BuiltinEffect::FilmHalation { .. } => BuiltinEffect::FilmHalation {
            threshold: 0.75, radius: 15.0, intensity: 0.3, hue: 0.2,
        },
        BuiltinEffect::SpeedRamp { .. } => BuiltinEffect::SpeedRamp {
            speed: 1.0, blend_mode: 1, ramp_curve: 0,
            ramp_start: 0.0, ramp_end: 1.0, reverse: false,
            freeze_at: 0.0, freeze: false,
        },
    }
}

#[allow(dead_code)]
#[allow(dead_code)]
fn builtin_effect_has_multi_params(effect: &klipp_effects::BuiltinEffect) -> bool {
    use klipp_effects::BuiltinEffect;
    matches!(effect, BuiltinEffect::Crop { .. } | BuiltinEffect::Transform { .. } | BuiltinEffect::ChromaKey { .. } | BuiltinEffect::Bloom { .. } | BuiltinEffect::LightRays { .. }
        | BuiltinEffect::Vignette { .. } | BuiltinEffect::Diffusion { .. } | BuiltinEffect::LensFlare { .. }
        | BuiltinEffect::LightWrap { .. } | BuiltinEffect::AnamorphicStreak { .. } | BuiltinEffect::FilmHalation { .. }
        | BuiltinEffect::SpeedRamp { .. })
}

#[allow(dead_code)]
#[allow(dead_code)]
fn builtin_effect_is_default(effect: &klipp_effects::BuiltinEffect) -> bool {
    let def = builtin_effect_default(effect);
    // Compare by debug string — simple and correct for all variants
    format!("{:?}", effect) == format!("{:?}", def)
}

/// Compute KeyframeState for a given effect param keyframe vec at a given clip-relative time.
fn effect_keyframe_state(clip: &Clip, effect_index: usize, param_name: &str, clip_time_secs: f64) -> KeyframeState {
    let key = format!("{}:{}", effect_index, param_name);
    let kfs = clip.effect_keyframes.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
    let kf_tolerance = 0.02;
    KeyframeState {
        has_keyframe_at_playhead: kfs.iter().any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance),
        has_any_keyframes: !kfs.is_empty(),
        prev_keyframe_time: kfs.iter()
            .filter(|k| k.time_secs < clip_time_secs - kf_tolerance)
            .last()
            .map(|k| k.time_secs),
        next_keyframe_time: kfs.iter()
            .find(|k| k.time_secs > clip_time_secs + kf_tolerance)
            .map(|k| k.time_secs),
        current_interpolation: kfs.iter()
            .find(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance)
            .map(|k| k.interpolation),
    }
}

/// Evaluate effect keyframes for a param, returning the keyframed value or the static default.
fn eval_effect_kf(clip: &Clip, effect_index: usize, param_name: &str, clip_time_secs: f64, default: f32) -> f32 {
    let key = format!("{}:{}", effect_index, param_name);
    if let Some(kfs) = clip.effect_keyframes.get(&key) {
        if !kfs.is_empty() {
            return klipp_core::keyframe::evaluate(kfs, clip_time_secs, default);
        }
    }
    default
}

/// Check if any keyframes exist for a given effect param.
fn has_effect_kfs(clip: &Clip, effect_index: usize, param_name: &str) -> bool {
    let key = format!("{}:{}", effect_index, param_name);
    clip.effect_keyframes.get(&key).map_or(false, |kfs| !kfs.is_empty())
}

fn show_builtin_effect_params(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    effect: &klipp_effects::BuiltinEffect,
    clip_id: klipp_core::ClipId,
    effect_index: usize,
    cmds: &mut Vec<AppCommand>,
    clip: &Clip,
    clip_time_secs: f64,
    state: &AppState,
) -> Option<klipp_effects::BuiltinEffect> {
    let mut updated = effect.clone();
    let mut changed = false;
    let mut any_drag_started = false;
    let mut any_drag_stopped = false;
    let drag_key = format!("{:?}:builtin:{}", clip_id, effect_index);
    let clip_timeline_in_secs = clip.timeline_in.to_secs_f64();
    let clip_rate = clip.timeline_in.rate;
    let kf_tolerance = 0.02;

    // ── Preset selector ──
    let effect_key = crate::effect_presets::builtin_effect_key(effect);
    let presets = state.preset_manager.presets_for(effect_key);
    if !presets.is_empty() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Preset").size(10.0).color(egui::Color32::from_rgb(160, 160, 170)));
            ui.add_space(4.0);
            let combo_id = ui.make_persistent_id(format!("preset_combo_{clip_id:?}_{effect_index}"));
            egui::ComboBox::from_id_salt(combo_id)
                .selected_text("Select preset...")
                .width(ui.available_width() - 80.0)
                .show_ui(ui, |ui| {
                    // Built-in presets
                    let builtins: Vec<_> = presets.iter().filter(|p| p.is_builtin).collect();
                    if !builtins.is_empty() {
                        for preset in &builtins {
                            let mut r = ui.selectable_label(false, &preset.name);
                            if !preset.description.is_empty() {
                                r = r.on_hover_text(&preset.description);
                            }
                            if r.clicked() {
                                if let Some(ref fx) = preset.builtin {
                                    cmds.push(AppCommand::UpdateBuiltinEffect {
                                        clip_id,
                                        index: effect_index,
                                        effect: fx.clone(),
                                    });
                                }
                            }
                        }
                    }
                    // Separator + user presets
                    let user_presets: Vec<_> = presets.iter().filter(|p| !p.is_builtin).collect();
                    if !user_presets.is_empty() {
                        if !builtins.is_empty() {
                            ui.separator();
                        }
                        for preset in &user_presets {
                            ui.horizontal(|ui| {
                                let mut r = ui.selectable_label(false, &preset.name);
                                if !preset.description.is_empty() {
                                    r = r.on_hover_text(&preset.description);
                                }
                                if r.clicked() {
                                    if let Some(ref fx) = preset.builtin {
                                        cmds.push(AppCommand::UpdateBuiltinEffect {
                                            clip_id,
                                            index: effect_index,
                                            effect: fx.clone(),
                                        });
                                    }
                                }
                                if ui.small_button("\u{00d7}").on_hover_text("Delete preset").clicked() {
                                    cmds.push(AppCommand::DeleteBuiltinEffectPreset {
                                        effect_key: preset.effect_key.clone(),
                                        name: preset.name.clone(),
                                    });
                                }
                            });
                        }
                    }
                });
            // Save button with popup
            let save_popup_id = ui.make_persistent_id(format!("save_preset_{clip_id:?}_{effect_index}"));
            let save_btn = ui.small_button("Save...");
            if save_btn.clicked() {
                ui.memory_mut(|m| m.toggle_popup(save_popup_id));
            }
            egui::popup_below_widget(ui, save_popup_id, &save_btn, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(200.0);
                ui.label(egui::RichText::new("Save Preset").size(11.0).strong());
                ui.add_space(4.0);
                let data_id = ui.make_persistent_id(format!("preset_name_buf_{clip_id:?}_{effect_index}"));
                let mut name_buf: String = ui.data_mut(|d| d.get_temp(data_id).unwrap_or_default());
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut name_buf);
                });
                ui.data_mut(|d| d.insert_temp(data_id, name_buf.clone()));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let name_valid = !name_buf.trim().is_empty();
                    if ui.add_enabled(name_valid, egui::Button::new("Save")).clicked() {
                        cmds.push(AppCommand::SaveBuiltinEffectPreset {
                            name: name_buf.trim().to_string(),
                            description: String::new(),
                            effect_key: effect_key.to_string(),
                            effect: effect.clone(),
                        });
                        ui.data_mut(|d| d.insert_temp::<String>(data_id, String::new()));
                        ui.memory_mut(|m| m.toggle_popup(save_popup_id));
                    }
                    if ui.button("Cancel").clicked() {
                        ui.data_mut(|d| d.insert_temp::<String>(data_id, String::new()));
                        ui.memory_mut(|m| m.toggle_popup(save_popup_id));
                    }
                });
            });
        });
        ui.add_space(4.0);
    } else {
        // No presets available, still show save button
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Preset").size(10.0).color(egui::Color32::from_rgb(160, 160, 170)));
            ui.add_space(4.0);
            let save_popup_id = ui.make_persistent_id(format!("save_preset_{clip_id:?}_{effect_index}"));
            let save_btn = ui.small_button("Save...");
            if save_btn.clicked() {
                ui.memory_mut(|m| m.toggle_popup(save_popup_id));
            }
            egui::popup_below_widget(ui, save_popup_id, &save_btn, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(200.0);
                ui.label(egui::RichText::new("Save Preset").size(11.0).strong());
                ui.add_space(4.0);
                let data_id = ui.make_persistent_id(format!("preset_name_buf_{clip_id:?}_{effect_index}"));
                let mut name_buf: String = ui.data_mut(|d| d.get_temp(data_id).unwrap_or_default());
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut name_buf);
                });
                ui.data_mut(|d| d.insert_temp(data_id, name_buf.clone()));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let name_valid = !name_buf.trim().is_empty();
                    if ui.add_enabled(name_valid, egui::Button::new("Save")).clicked() {
                        cmds.push(AppCommand::SaveBuiltinEffectPreset {
                            name: name_buf.trim().to_string(),
                            description: String::new(),
                            effect_key: effect_key.to_string(),
                            effect: effect.clone(),
                        });
                        ui.data_mut(|d| d.insert_temp::<String>(data_id, String::new()));
                        ui.memory_mut(|m| m.toggle_popup(save_popup_id));
                    }
                    if ui.button("Cancel").clicked() {
                        ui.data_mut(|d| d.insert_temp::<String>(data_id, String::new()));
                        ui.memory_mut(|m| m.toggle_popup(save_popup_id));
                    }
                });
            });
        });
        ui.add_space(4.0);
    }

    // Collect keyframe responses: (param_name, ScrubbyParamResponse, current_value)
    let mut kf_responses: Vec<(&str, crate::widgets::scrubby_param::ScrubbyParamResponse, f32)> = Vec::new();

    // Helper: process a scrubby response for drag coalescing
    macro_rules! track_scrubby {
        ($param_name:expr, $r:expr, $val:expr) => {{
            let r = $r;
            changed |= r.changed;
            if r.drag_started { any_drag_started = true; }
            if r.drag_stopped { any_drag_stopped = true; }
            kf_responses.push(($param_name, r, $val));
        }};
    }

    match &mut updated {
        klipp_effects::BuiltinEffect::Opacity { value } => {
            *value = eval_effect_kf(clip, effect_index, "value", clip_time_secs, *value);
            let ks = Some(effect_keyframe_state(clip, effect_index, "value", clip_time_secs));
            let r = scrubby_param_row(ui, "Opacity", value, 0.0..=1.0, 1.0, 0.005, 2, "", ks);
            track_scrubby!("value", r, *value);
        }
        klipp_effects::BuiltinEffect::GaussianBlur { radius } => {
            *radius = eval_effect_kf(clip, effect_index, "radius", clip_time_secs, *radius);
            let ks = Some(effect_keyframe_state(clip, effect_index, "radius", clip_time_secs));
            let r = scrubby_param_row(ui, "Radius", radius, 0.0..=100.0, 0.0, 0.5, 1, " px", ks);
            track_scrubby!("radius", r, *radius);
        }
        klipp_effects::BuiltinEffect::Sharpen { amount } => {
            *amount = eval_effect_kf(clip, effect_index, "amount", clip_time_secs, *amount);
            let ks = Some(effect_keyframe_state(clip, effect_index, "amount", clip_time_secs));
            let r = scrubby_param_row(ui, "Amount", amount, 0.0..=2.0, 0.5, 0.01, 2, "", ks);
            track_scrubby!("amount", r, *amount);
        }
        klipp_effects::BuiltinEffect::Crop { left, right, top, bottom, feather } => {
            *left = eval_effect_kf(clip, effect_index, "left", clip_time_secs, *left);
            *right = eval_effect_kf(clip, effect_index, "right", clip_time_secs, *right);
            *top = eval_effect_kf(clip, effect_index, "top", clip_time_secs, *top);
            *bottom = eval_effect_kf(clip, effect_index, "bottom", clip_time_secs, *bottom);
            *feather = eval_effect_kf(clip, effect_index, "feather", clip_time_secs, *feather);

            let ks_l = Some(effect_keyframe_state(clip, effect_index, "left", clip_time_secs));
            let ks_r = Some(effect_keyframe_state(clip, effect_index, "right", clip_time_secs));
            let ks_t = Some(effect_keyframe_state(clip, effect_index, "top", clip_time_secs));
            let ks_b = Some(effect_keyframe_state(clip, effect_index, "bottom", clip_time_secs));
            let ks_f = Some(effect_keyframe_state(clip, effect_index, "feather", clip_time_secs));

            let r_l = scrubby_param_row(ui, "Left", left, 0.0..=1.0, 0.0, 0.005, 3, "", ks_l);
            track_scrubby!("left", r_l, *left);
            let r_r = scrubby_param_row(ui, "Right", right, 0.0..=1.0, 0.0, 0.005, 3, "", ks_r);
            track_scrubby!("right", r_r, *right);
            let r_t = scrubby_param_row(ui, "Top", top, 0.0..=1.0, 0.0, 0.005, 3, "", ks_t);
            track_scrubby!("top", r_t, *top);
            let r_b = scrubby_param_row(ui, "Bottom", bottom, 0.0..=1.0, 0.0, 0.005, 3, "", ks_b);
            track_scrubby!("bottom", r_b, *bottom);
            let r_f = scrubby_param_row(ui, "Feather", feather, 0.0..=1.0, 0.0, 0.005, 3, "", ks_f);
            track_scrubby!("feather", r_f, *feather);
        }
        klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y } => {
            // Transform in effects tab: no keyframes (handled by video tab's dedicated keyframes)
            let r = scrubby_param_row(ui, "Pos X", position_x, -2000.0..=2000.0, 0.0, 1.0, 0, " px", None);
            track_scrubby!("", r, *position_x);
            let r = scrubby_param_row(ui, "Pos Y", position_y, -2000.0..=2000.0, 0.0, 1.0, 0, " px", None);
            track_scrubby!("", r, *position_y);
            let r = scrubby_param_row(ui, "Scale X", scale_x, 0.0..=4.0, 1.0, 0.01, 2, "\u{00d7}", None);
            track_scrubby!("", r, *scale_x);
            let r = scrubby_param_row(ui, "Scale Y", scale_y, 0.0..=4.0, 1.0, 0.01, 2, "\u{00d7}", None);
            track_scrubby!("", r, *scale_y);
            let r = scrubby_param_row(ui, "Rotation", rotation, -360.0..=360.0, 0.0, 0.5, 1, " \u{00b0}", None);
            track_scrubby!("", r, *rotation);
            let r = scrubby_param_row(ui, "Anchor X", anchor_x, 0.0..=1.0, 0.5, 0.005, 3, "", None);
            track_scrubby!("", r, *anchor_x);
            let r = scrubby_param_row(ui, "Anchor Y", anchor_y, 0.0..=1.0, 0.5, 0.005, 3, "", None);
            track_scrubby!("", r, *anchor_y);
        }
        klipp_effects::BuiltinEffect::ChromaKey { key_color, tolerance, spill_suppression } => {
            // Color picker stays as-is (not a scrubby candidate)
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Key Color").size(10.0));
                let mut color = egui::Color32::from_rgb(key_color[0], key_color[1], key_color[2]);
                if ui.color_edit_button_srgba(&mut color).changed() {
                    key_color[0] = color.r();
                    key_color[1] = color.g();
                    key_color[2] = color.b();
                    changed = true;
                }
            });
            *tolerance = eval_effect_kf(clip, effect_index, "tolerance", clip_time_secs, *tolerance);
            *spill_suppression = eval_effect_kf(clip, effect_index, "spill", clip_time_secs, *spill_suppression);

            let ks_t = Some(effect_keyframe_state(clip, effect_index, "tolerance", clip_time_secs));
            let ks_s = Some(effect_keyframe_state(clip, effect_index, "spill", clip_time_secs));

            let r_t = scrubby_param_row(ui, "Tolerance", tolerance, 0.0..=1.0, 0.2, 0.005, 3, "", ks_t);
            track_scrubby!("tolerance", r_t, *tolerance);
            let r_s = scrubby_param_row(ui, "Spill", spill_suppression, 0.0..=1.0, 0.1, 0.005, 3, "", ks_s);
            track_scrubby!("spill", r_s, *spill_suppression);
        }
        klipp_effects::BuiltinEffect::Bloom { threshold, radius, intensity } => {
            *threshold = eval_effect_kf(clip, effect_index, "threshold", clip_time_secs, *threshold);
            *radius = eval_effect_kf(clip, effect_index, "radius", clip_time_secs, *radius);
            *intensity = eval_effect_kf(clip, effect_index, "intensity", clip_time_secs, *intensity);

            let ks_th = Some(effect_keyframe_state(clip, effect_index, "threshold", clip_time_secs));
            let ks_r = Some(effect_keyframe_state(clip, effect_index, "radius", clip_time_secs));
            let ks_i = Some(effect_keyframe_state(clip, effect_index, "intensity", clip_time_secs));

            let r_th = scrubby_param_row(ui, "Threshold", threshold, 0.0..=1.0, 0.7, 0.005, 2, "", ks_th);
            track_scrubby!("threshold", r_th, *threshold);
            let r_r = scrubby_param_row(ui, "Radius", radius, 0.0..=100.0, 20.0, 0.5, 1, " px", ks_r);
            track_scrubby!("radius", r_r, *radius);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=2.0, 0.5, 0.01, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
        }
        klipp_effects::BuiltinEffect::Glow { radius, intensity } => {
            *radius = eval_effect_kf(clip, effect_index, "radius", clip_time_secs, *radius);
            *intensity = eval_effect_kf(clip, effect_index, "intensity", clip_time_secs, *intensity);

            let ks_r = Some(effect_keyframe_state(clip, effect_index, "radius", clip_time_secs));
            let ks_i = Some(effect_keyframe_state(clip, effect_index, "intensity", clip_time_secs));

            let r_r = scrubby_param_row(ui, "Radius", radius, 0.0..=100.0, 30.0, 0.5, 1, " px", ks_r);
            track_scrubby!("radius", r_r, *radius);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=2.0, 0.4, 0.01, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
        }
        klipp_effects::BuiltinEffect::LightRays { center_x, center_y, threshold, samples, decay, intensity } => {
            *center_x = eval_effect_kf(clip, effect_index, "center_x", clip_time_secs, *center_x);
            *center_y = eval_effect_kf(clip, effect_index, "center_y", clip_time_secs, *center_y);
            *threshold = eval_effect_kf(clip, effect_index, "threshold", clip_time_secs, *threshold);
            *decay = eval_effect_kf(clip, effect_index, "decay", clip_time_secs, *decay);
            *intensity = eval_effect_kf(clip, effect_index, "intensity", clip_time_secs, *intensity);

            let ks_cx = Some(effect_keyframe_state(clip, effect_index, "center_x", clip_time_secs));
            let ks_cy = Some(effect_keyframe_state(clip, effect_index, "center_y", clip_time_secs));
            let ks_th = Some(effect_keyframe_state(clip, effect_index, "threshold", clip_time_secs));
            let ks_d = Some(effect_keyframe_state(clip, effect_index, "decay", clip_time_secs));
            let ks_i = Some(effect_keyframe_state(clip, effect_index, "intensity", clip_time_secs));

            let r_cx = scrubby_param_row(ui, "Center X", center_x, 0.0..=1.0, 0.5, 0.005, 3, "", ks_cx);
            track_scrubby!("center_x", r_cx, *center_x);
            let r_cy = scrubby_param_row(ui, "Center Y", center_y, 0.0..=1.0, 0.3, 0.005, 3, "", ks_cy);
            track_scrubby!("center_y", r_cy, *center_y);
            let r_th = scrubby_param_row(ui, "Threshold", threshold, 0.0..=1.0, 0.6, 0.005, 2, "", ks_th);
            track_scrubby!("threshold", r_th, *threshold);
            // samples is u32, convert to f32 for slider then back
            let mut samples_f = *samples as f32;
            let ks_s = None; // samples not typically keyframed
            let r_s = scrubby_param_row(ui, "Samples", &mut samples_f, 4.0..=128.0, 64.0, 1.0, 0, "", ks_s);
            *samples = samples_f as u32;
            track_scrubby!("", r_s, samples_f);
            let r_d = scrubby_param_row(ui, "Decay", decay, 0.0..=1.0, 0.3, 0.005, 3, "", ks_d);
            track_scrubby!("decay", r_d, *decay);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=2.0, 0.5, 0.01, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
        }
        klipp_effects::BuiltinEffect::Vignette { amount, size, softness, roundness } => {
            *amount = eval_effect_kf(clip, effect_index, "amount", clip_time_secs, *amount);
            *size = eval_effect_kf(clip, effect_index, "size", clip_time_secs, *size);
            *softness = eval_effect_kf(clip, effect_index, "softness", clip_time_secs, *softness);
            *roundness = eval_effect_kf(clip, effect_index, "roundness", clip_time_secs, *roundness);

            let ks_a = Some(effect_keyframe_state(clip, effect_index, "amount", clip_time_secs));
            let ks_s = Some(effect_keyframe_state(clip, effect_index, "size", clip_time_secs));
            let ks_sf = Some(effect_keyframe_state(clip, effect_index, "softness", clip_time_secs));
            let ks_r = Some(effect_keyframe_state(clip, effect_index, "roundness", clip_time_secs));

            let r_a = scrubby_param_row(ui, "Amount", amount, -1.0..=1.0, -0.4, 0.005, 2, "", ks_a);
            track_scrubby!("amount", r_a, *amount);
            let r_s = scrubby_param_row(ui, "Size", size, 0.0..=2.0, 0.8, 0.01, 2, "", ks_s);
            track_scrubby!("size", r_s, *size);
            let r_sf = scrubby_param_row(ui, "Softness", softness, 0.0..=2.0, 0.5, 0.01, 2, "", ks_sf);
            track_scrubby!("softness", r_sf, *softness);
            let r_r = scrubby_param_row(ui, "Roundness", roundness, 0.0..=1.0, 0.5, 0.005, 2, "", ks_r);
            track_scrubby!("roundness", r_r, *roundness);
        }
        klipp_effects::BuiltinEffect::Diffusion { amount, radius, threshold, desaturation } => {
            *amount = eval_effect_kf(clip, effect_index, "amount", clip_time_secs, *amount);
            *radius = eval_effect_kf(clip, effect_index, "radius", clip_time_secs, *radius);
            *threshold = eval_effect_kf(clip, effect_index, "threshold", clip_time_secs, *threshold);
            *desaturation = eval_effect_kf(clip, effect_index, "desaturation", clip_time_secs, *desaturation);

            let ks_a = Some(effect_keyframe_state(clip, effect_index, "amount", clip_time_secs));
            let ks_r = Some(effect_keyframe_state(clip, effect_index, "radius", clip_time_secs));
            let ks_th = Some(effect_keyframe_state(clip, effect_index, "threshold", clip_time_secs));
            let ks_d = Some(effect_keyframe_state(clip, effect_index, "desaturation", clip_time_secs));

            let r_a = scrubby_param_row(ui, "Amount", amount, 0.0..=1.0, 0.3, 0.005, 2, "", ks_a);
            track_scrubby!("amount", r_a, *amount);
            let r_r = scrubby_param_row(ui, "Radius", radius, 0.0..=100.0, 25.0, 0.5, 1, " px", ks_r);
            track_scrubby!("radius", r_r, *radius);
            let r_th = scrubby_param_row(ui, "Threshold", threshold, 0.0..=1.0, 0.5, 0.005, 2, "", ks_th);
            track_scrubby!("threshold", r_th, *threshold);
            let r_d = scrubby_param_row(ui, "Desaturation", desaturation, 0.0..=1.0, 0.2, 0.005, 2, "", ks_d);
            track_scrubby!("desaturation", r_d, *desaturation);
        }
        klipp_effects::BuiltinEffect::LensFlare { position_x, position_y, brightness, flare_size, ghost_count, ghost_intensity, chromatic_aberration } => {
            *position_x = eval_effect_kf(clip, effect_index, "position_x", clip_time_secs, *position_x);
            *position_y = eval_effect_kf(clip, effect_index, "position_y", clip_time_secs, *position_y);
            *brightness = eval_effect_kf(clip, effect_index, "brightness", clip_time_secs, *brightness);
            *flare_size = eval_effect_kf(clip, effect_index, "flare_size", clip_time_secs, *flare_size);
            *ghost_intensity = eval_effect_kf(clip, effect_index, "ghost_intensity", clip_time_secs, *ghost_intensity);
            *chromatic_aberration = eval_effect_kf(clip, effect_index, "chromatic_aberration", clip_time_secs, *chromatic_aberration);

            let ks_px = Some(effect_keyframe_state(clip, effect_index, "position_x", clip_time_secs));
            let ks_py = Some(effect_keyframe_state(clip, effect_index, "position_y", clip_time_secs));
            let ks_b = Some(effect_keyframe_state(clip, effect_index, "brightness", clip_time_secs));
            let ks_fs = Some(effect_keyframe_state(clip, effect_index, "flare_size", clip_time_secs));
            let ks_gi = Some(effect_keyframe_state(clip, effect_index, "ghost_intensity", clip_time_secs));
            let ks_ca = Some(effect_keyframe_state(clip, effect_index, "chromatic_aberration", clip_time_secs));

            let r_px = scrubby_param_row(ui, "Position X", position_x, 0.0..=1.0, 0.7, 0.005, 3, "", ks_px);
            track_scrubby!("position_x", r_px, *position_x);
            let r_py = scrubby_param_row(ui, "Position Y", position_y, 0.0..=1.0, 0.3, 0.005, 3, "", ks_py);
            track_scrubby!("position_y", r_py, *position_y);
            let r_b = scrubby_param_row(ui, "Brightness", brightness, 0.0..=2.0, 0.6, 0.01, 2, "", ks_b);
            track_scrubby!("brightness", r_b, *brightness);
            let r_fs = scrubby_param_row(ui, "Flare Size", flare_size, 0.0..=1.0, 0.15, 0.005, 3, "", ks_fs);
            track_scrubby!("flare_size", r_fs, *flare_size);
            // ghost_count is u32, convert to f32 for slider then back
            let mut gc_f = *ghost_count as f32;
            let r_gc = scrubby_param_row(ui, "Ghost Count", &mut gc_f, 0.0..=8.0, 4.0, 1.0, 0, "", None);
            *ghost_count = gc_f as u32;
            track_scrubby!("", r_gc, gc_f);
            let r_gi = scrubby_param_row(ui, "Ghost Intensity", ghost_intensity, 0.0..=1.0, 0.3, 0.005, 2, "", ks_gi);
            track_scrubby!("ghost_intensity", r_gi, *ghost_intensity);
            let r_ca = scrubby_param_row(ui, "Chromatic Aberration", chromatic_aberration, 0.0..=1.0, 0.4, 0.005, 2, "", ks_ca);
            track_scrubby!("chromatic_aberration", r_ca, *chromatic_aberration);
        }
        klipp_effects::BuiltinEffect::LightWrap { width: wrap_w, intensity, softness } => {
            *wrap_w = eval_effect_kf(clip, effect_index, "width", clip_time_secs, *wrap_w);
            *intensity = eval_effect_kf(clip, effect_index, "intensity", clip_time_secs, *intensity);
            *softness = eval_effect_kf(clip, effect_index, "softness", clip_time_secs, *softness);

            let ks_w = Some(effect_keyframe_state(clip, effect_index, "width", clip_time_secs));
            let ks_i = Some(effect_keyframe_state(clip, effect_index, "intensity", clip_time_secs));
            let ks_s = Some(effect_keyframe_state(clip, effect_index, "softness", clip_time_secs));

            let r_w = scrubby_param_row(ui, "Width", wrap_w, 0.0..=100.0, 15.0, 0.5, 1, " px", ks_w);
            track_scrubby!("width", r_w, *wrap_w);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=1.0, 0.5, 0.005, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
            let r_s = scrubby_param_row(ui, "Softness", softness, 0.0..=2.0, 1.0, 0.01, 2, "", ks_s);
            track_scrubby!("softness", r_s, *softness);
        }
        klipp_effects::BuiltinEffect::AnamorphicStreak { threshold, length, intensity, tint } => {
            *threshold = eval_effect_kf(clip, effect_index, "threshold", clip_time_secs, *threshold);
            *length = eval_effect_kf(clip, effect_index, "length", clip_time_secs, *length);
            *intensity = eval_effect_kf(clip, effect_index, "intensity", clip_time_secs, *intensity);
            *tint = eval_effect_kf(clip, effect_index, "tint", clip_time_secs, *tint);

            let ks_th = Some(effect_keyframe_state(clip, effect_index, "threshold", clip_time_secs));
            let ks_l = Some(effect_keyframe_state(clip, effect_index, "length", clip_time_secs));
            let ks_i = Some(effect_keyframe_state(clip, effect_index, "intensity", clip_time_secs));
            let ks_t = Some(effect_keyframe_state(clip, effect_index, "tint", clip_time_secs));

            let r_th = scrubby_param_row(ui, "Threshold", threshold, 0.0..=1.0, 0.7, 0.005, 2, "", ks_th);
            track_scrubby!("threshold", r_th, *threshold);
            let r_l = scrubby_param_row(ui, "Length", length, 0.0..=200.0, 80.0, 1.0, 0, " px", ks_l);
            track_scrubby!("length", r_l, *length);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=2.0, 0.5, 0.01, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
            let r_t = scrubby_param_row(ui, "Tint", tint, 0.0..=1.0, 0.3, 0.005, 2, "", ks_t);
            track_scrubby!("tint", r_t, *tint);
        }
        klipp_effects::BuiltinEffect::FilmHalation { threshold, radius, intensity, hue } => {
            *threshold = eval_effect_kf(clip, effect_index, "threshold", clip_time_secs, *threshold);
            *radius = eval_effect_kf(clip, effect_index, "radius", clip_time_secs, *radius);
            *intensity = eval_effect_kf(clip, effect_index, "intensity", clip_time_secs, *intensity);
            *hue = eval_effect_kf(clip, effect_index, "hue", clip_time_secs, *hue);

            let ks_th = Some(effect_keyframe_state(clip, effect_index, "threshold", clip_time_secs));
            let ks_r = Some(effect_keyframe_state(clip, effect_index, "radius", clip_time_secs));
            let ks_i = Some(effect_keyframe_state(clip, effect_index, "intensity", clip_time_secs));
            let ks_h = Some(effect_keyframe_state(clip, effect_index, "hue", clip_time_secs));

            let r_th = scrubby_param_row(ui, "Threshold", threshold, 0.0..=1.0, 0.75, 0.005, 2, "", ks_th);
            track_scrubby!("threshold", r_th, *threshold);
            let r_r = scrubby_param_row(ui, "Radius", radius, 0.0..=50.0, 15.0, 0.5, 1, " px", ks_r);
            track_scrubby!("radius", r_r, *radius);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=1.0, 0.3, 0.005, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
            let r_h = scrubby_param_row(ui, "Hue", hue, 0.0..=1.0, 0.2, 0.005, 2, "", ks_h);
            track_scrubby!("hue", r_h, *hue);
        }
        klipp_effects::BuiltinEffect::SpeedRamp { speed, blend_mode, ramp_curve, ramp_start, ramp_end, reverse, freeze_at, freeze } => {
            *speed = eval_effect_kf(clip, effect_index, "speed", clip_time_secs, *speed);
            *ramp_start = eval_effect_kf(clip, effect_index, "ramp_start", clip_time_secs, *ramp_start);
            *ramp_end = eval_effect_kf(clip, effect_index, "ramp_end", clip_time_secs, *ramp_end);
            *freeze_at = eval_effect_kf(clip, effect_index, "freeze_at", clip_time_secs, *freeze_at);

            let ks_sp = Some(effect_keyframe_state(clip, effect_index, "speed", clip_time_secs));
            let ks_rs = Some(effect_keyframe_state(clip, effect_index, "ramp_start", clip_time_secs));
            let ks_re = Some(effect_keyframe_state(clip, effect_index, "ramp_end", clip_time_secs));

            let r_sp = scrubby_param_row(ui, "Speed", speed, 0.1..=4.0, 1.0, 0.05, 2, "\u{00d7}", ks_sp);
            track_scrubby!("speed", r_sp, *speed);

            // Blend Mode dropdown
            let blend_labels = ["Nearest", "Linear"];
            let blend_sel = (*blend_mode as usize).min(blend_labels.len() - 1);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Blend Mode").size(10.0));
                let combo_id = ui.make_persistent_id(format!("sr_blend_{clip_id:?}_{effect_index}"));
                egui::ComboBox::from_id_salt(combo_id)
                    .selected_text(blend_labels[blend_sel])
                    .show_ui(ui, |ui| {
                        for (i, label) in blend_labels.iter().enumerate() {
                            if ui.selectable_value(blend_mode, i as u32, *label).changed() {
                                changed = true;
                            }
                        }
                    });
            });

            // Ramp Curve dropdown
            let curve_labels = ["Constant", "Ease In", "Ease Out", "Ease In-Out", "S-Curve"];
            let curve_sel = (*ramp_curve as usize).min(curve_labels.len() - 1);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Ramp Curve").size(10.0));
                let combo_id = ui.make_persistent_id(format!("sr_curve_{clip_id:?}_{effect_index}"));
                egui::ComboBox::from_id_salt(combo_id)
                    .selected_text(curve_labels[curve_sel])
                    .show_ui(ui, |ui| {
                        for (i, label) in curve_labels.iter().enumerate() {
                            if ui.selectable_value(ramp_curve, i as u32, *label).changed() {
                                changed = true;
                            }
                        }
                    });
            });

            let r_rs = scrubby_param_row(ui, "Ramp Start", ramp_start, 0.0..=1.0, 0.0, 0.005, 3, "", ks_rs);
            track_scrubby!("ramp_start", r_rs, *ramp_start);
            let r_re = scrubby_param_row(ui, "Ramp End", ramp_end, 0.0..=1.0, 1.0, 0.005, 3, "", ks_re);
            track_scrubby!("ramp_end", r_re, *ramp_end);

            if ui.checkbox(reverse, "Reverse").changed() {
                changed = true;
            }
            if ui.checkbox(freeze, "Freeze Frame").changed() {
                changed = true;
            }
            if *freeze {
                let ks_fa = Some(effect_keyframe_state(clip, effect_index, "freeze_at", clip_time_secs));
                let r_fa = scrubby_param_row(ui, "Freeze At", freeze_at, 0.0..=1.0, 0.5, 0.005, 3, "", ks_fa);
                track_scrubby!("freeze_at", r_fa, *freeze_at);
            }
        }
    }

    // Emit begin_drag at the start of a drag gesture
    if any_drag_started {
        cmds.push(AppCommand::BeginEffectDrag {
            key: drag_key.clone(),
            clip_id,
            effect_index,
            old_effect: effect.clone(),
        });
    }

    // Emit end_drag when the drag gesture completes
    if any_drag_stopped {
        cmds.push(AppCommand::EndEffectDrag {
            key: drag_key,
            clip_id,
            effect_index,
            new_effect: updated.clone(),
        });
    }

    // Handle keyframe toggle/nav and auto-update for each param
    for (param_name, resp, value) in &kf_responses {
        if param_name.is_empty() {
            continue; // Transform/Opacity in effects tab -- no effect keyframes
        }

        // Auto-update keyframe at playhead when slider changes and keyframes exist
        if resp.changed && has_effect_kfs(clip, effect_index, param_name) {
            cmds.push(AppCommand::AddEffectKeyframe {
                clip_id,
                effect_index,
                param_name: param_name.to_string(),
                time_secs: clip_time_secs,
                value: *value,
            });
        }

        // Toggle keyframe on diamond click
        if resp.keyframe_toggled {
            let key = format!("{}:{}", effect_index, param_name);
            let has_at_playhead = clip.effect_keyframes.get(&key)
                .map_or(false, |kfs| kfs.iter().any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance));
            if has_at_playhead {
                cmds.push(AppCommand::RemoveEffectKeyframe {
                    clip_id,
                    effect_index,
                    param_name: param_name.to_string(),
                    time_secs: clip_time_secs,
                });
            } else {
                cmds.push(AppCommand::AddEffectKeyframe {
                    clip_id,
                    effect_index,
                    param_name: param_name.to_string(),
                    time_secs: clip_time_secs,
                    value: *value,
                });
            }
        }

        // Navigate to previous keyframe
        if resp.keyframe_nav_prev {
            let ks = effect_keyframe_state(clip, effect_index, param_name, clip_time_secs);
            if let Some(t) = ks.prev_keyframe_time {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
            }
        }

        // Navigate to next keyframe
        if resp.keyframe_nav_next {
            let ks = effect_keyframe_state(clip, effect_index, param_name, clip_time_secs);
            if let Some(t) = ks.next_keyframe_time {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
            }
        }
    }

    if changed { Some(updated) } else { None }
}
