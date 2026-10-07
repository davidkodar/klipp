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
        ui.painter().rect(icon_rect, 8.0, theme.bg_raised, egui::Stroke::new(1.0_f32, theme.border));
        ui.painter().text(
            icon_rect.center(), egui::Align2::CENTER_CENTER,
            "+", egui::FontId::proportional(16.0), theme.text_dim,
        );

        ui.add_space(12.0);

        ui.label(
            egui::RichText::new("No effects applied")
                .size(13.0)
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
                    .stroke(egui::Stroke::new(1.0_f32, theme.border))
                    .rounding(4.0),
            ).clicked() {
                cmds.push(AppCommand::ShowEffectsBrowser);
            }

            // "E" shortcut badge
            let (badge_rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            ui.painter().rect(badge_rect, 4.0, theme.bg_deep, egui::Stroke::new(0.5_f32, theme.border));
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

    let has_any = !clip.builtin_effects().is_empty() || !clip.ofx_effects().is_empty();

    if !has_any {
        show_empty(ui, theme, cmds);
        return;
    }

    let pad = 14.0;
    let total_effects = clip.builtin_effects().len() + clip.ofx_effects().len();

    // ── "Drag to reorder" hint ──
    show_drag_hint(ui, theme, total_effects);

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
    let badge_green = theme.marker_success;
    let badge_gray = theme.text_dim;

    for (i, effect) in clip.builtin_effects().iter().enumerate() {
        let name = builtin_effect_name(effect).into_owned();
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
        // Border on header — shrink by 0.25 so the 0.5px centered stroke fits
        // entirely inside header_rect. Prevents the stroke's outer half (0.25px)
        // from bleeding left of inspector_rect.min.x into the adjacent resizer.
        ui.painter().rect_stroke(header_rect.shrink(0.25), header_rounding, egui::Stroke::new(0.5_f32, theme.border));

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
            &name,
            egui::FontId::proportional(13.0),
            theme.text_primary,
        );

        // "Built-in" badge — outlined pill, green
        let badge_text = "Built-in";
        let badge_font = egui::FontId::proportional(10.0);
        let badge_galley = ui.painter().layout_no_wrap(badge_text.to_string(), badge_font.clone(), badge_green);
        let badge_w = badge_galley.size().x + 10.0;
        let badge_h = 16.0;
        let name_galley = ui.painter().layout_no_wrap(name.to_string(), egui::FontId::proportional(13.0), theme.text_primary);
        let badge_x = name_x + name_galley.size().x + 8.0;
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(badge_x, header_rect.center().y - badge_h / 2.0),
            egui::vec2(badge_w, badge_h),
        );
        ui.painter().rect_stroke(badge_rect, badge_h / 2.0, egui::Stroke::new(0.8_f32, badge_green));
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
            "\u{00d7}", egui::FontId::proportional(13.0), remove_color,
        );
        let remove_clicked = remove_resp.on_hover_text("Remove effect").clicked();
        if remove_clicked {
            builtin_remove = Some(i);
        }

        // Enable/disable eye-icon toggle. Matches the track-visibility icon
        // in the timeline (panels/timeline.rs:2639): Icon::Eye when enabled,
        // Icon::EyeSlash when disabled, with a muted neutral tint that dims
        // further when off.
        let is_enabled = clip.builtin_effects_enabled().get(i).copied().unwrap_or(true);
        let toggle_rect = egui::Rect::from_min_size(
            egui::pos2(remove_x - 26.0, header_rect.center().y - 8.0),
            egui::vec2(16.0, 16.0),
        );
        let toggle_resp = ui
            .interact(toggle_rect, ui.id().with(("bfx_tog", i)), egui::Sense::click())
            .on_hover_text(if is_enabled { "Disable effect" } else { "Enable effect" });
        let eye_icon = if is_enabled { Icon::Eye } else { Icon::EyeSlash };
        let tint = if is_enabled {
            theme.text_primary
        } else {
            theme.text_dim
        };
        eye_icon.image(14.0).tint(tint).paint_at(ui, toggle_rect);
        let toggle_clicked = toggle_resp.clicked();
        if toggle_clicked {
            cmds.push(AppCommand::ToggleBuiltinEffectEnabled { clip_id, index: i });
        }

        // Split-compare (A/B) toggle — sits left of the eye icon. When active
        // for this effect, the viewer shows a draggable split divider with the
        // effect bypassed on the left and applied on the right.
        let compare_active = matches!(
            state.split_compare_effect,
            Some((cid, idx, false)) if cid == clip_id && idx == i
        );
        let compare_rect = egui::Rect::from_min_size(
            egui::pos2(remove_x - 48.0, header_rect.center().y - 8.0),
            egui::vec2(16.0, 16.0),
        );
        let compare_resp = ui
            .interact(compare_rect, ui.id().with(("bfx_cmp", i)), egui::Sense::click())
            .on_hover_text(if compare_active { "Stop A/B compare" } else { "A/B compare: bypass vs applied" });
        let compare_tint = if compare_active {
            theme.accent_alt
        } else if compare_resp.hovered() {
            theme.text_primary
        } else {
            theme.text_dim
        };
        ui.painter().text(
            compare_rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{25D0}",
            egui::FontId::proportional(13.0),
            compare_tint,
        );
        let compare_clicked = compare_resp.clicked();
        if compare_clicked {
            if compare_active {
                cmds.push(AppCommand::SetSplitCompareEffect {
                    clip_id: None,
                    effect_index: 0,
                    is_ofx: false,
                });
            } else {
                cmds.push(AppCommand::SetSplitCompareEffect {
                    clip_id: Some(clip_id),
                    effect_index: i,
                    is_ofx: false,
                });
            }
        }

        // Click header to toggle collapse (only if not on remove/toggle buttons)
        if header_resp.clicked() && !remove_clicked && !toggle_clicked && !compare_clicked {
            col_state.toggle(ui);
        }

        col_state.store(ui.ctx());

        // ── Card body ──
        if is_open {
            let body_frame = egui::Frame::none()
                .fill(card_bg)
                .stroke(egui::Stroke::new(0.5_f32, theme.border))
                .rounding(egui::Rounding { nw: 0.0, ne: 0.0, sw: card_rounding, se: card_rounding })
                .inner_margin(egui::Margin { left: pad, right: pad, top: 8.0, bottom: 8.0 });
            body_frame.show(ui, |ui: &mut egui::Ui| {
                // When disabled, dim and lock the params so the visual state
                // matches the effect's actual bypassed behaviour on-engine.
                ui.add_enabled_ui(is_enabled, |ui| {
                    if let Some(updated) = show_builtin_effect_params(ui, theme, effect, clip_id, i, cmds, clip, clip_time_secs, state) {
                        cmds.push(AppCommand::UpdateBuiltinEffect {
                            clip_id,
                            index: i,
                            effect: updated,
                        });
                    }
                });
            });
        }

        // Track card position for drop indicator
        builtin_card_tops.push(header_rect.min.y);

        // Drag visual feedback + reorder
        if header_resp.dragged_by(egui::PointerButton::Primary) {
            ui.painter().rect_stroke(
                header_rect,
                if is_open { egui::Rounding { nw: card_rounding, ne: card_rounding, sw: 0.0, se: 0.0 } } else { egui::Rounding::same(card_rounding) },
                egui::Stroke::new(2.0_f32, theme.accent),
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
                let max_idx = clip.builtin_effects().len() - 1;
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
            let max_idx = clip.builtin_effects().len().saturating_sub(1);
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
                ui.painter().hline(x_range, line_y, egui::Stroke::new(2.5_f32, theme.accent));
            }
        }
    }

    // ── OFX effects ──
    let mut ofx_remove = None;
    let mut ofx_move_to: Option<(usize, usize)> = None;

    for (i, effect) in clip.ofx_effects().iter().enumerate() {
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
        ui.painter().rect_stroke(header_rect.shrink(0.25), header_rounding, egui::Stroke::new(0.5_f32, theme.border));

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
        let badge_font = egui::FontId::proportional(10.0);
        let badge_galley = ui.painter().layout_no_wrap(badge_text.to_string(), badge_font.clone(), badge_gray);
        let badge_w = badge_galley.size().x + 10.0;
        let badge_h = 16.0;
        let name_galley = ui.painter().layout_no_wrap(display_name.to_string(), egui::FontId::proportional(13.0), theme.text_primary);
        let badge_x = name_x + name_galley.size().x + 8.0;
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(badge_x, header_rect.center().y - badge_h / 2.0),
            egui::vec2(badge_w, badge_h),
        );
        ui.painter().rect_stroke(badge_rect, badge_h / 2.0, egui::Stroke::new(0.8_f32, badge_gray));
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
            "\u{00d7}", egui::FontId::proportional(13.0), remove_color,
        );
        let remove_clicked = remove_resp.on_hover_text("Remove effect").clicked();
        if remove_clicked {
            ofx_remove = Some(i);
        }

        // Enable/disable eye-icon toggle — same style as the builtin path.
        let enabled = effect.enabled;
        let toggle_rect = egui::Rect::from_min_size(
            egui::pos2(remove_x - 26.0, header_rect.center().y - 8.0),
            egui::vec2(16.0, 16.0),
        );
        let toggle_resp = ui
            .interact(toggle_rect, ui.id().with(("ofx_tog", i)), egui::Sense::click())
            .on_hover_text(if enabled { "Disable effect" } else { "Enable effect" });
        let eye_icon = if enabled { Icon::Eye } else { Icon::EyeSlash };
        let tint = if enabled {
            theme.text_primary
        } else {
            theme.text_dim
        };
        eye_icon.image(14.0).tint(tint).paint_at(ui, toggle_rect);
        let toggle_clicked = toggle_resp.clicked();
        if toggle_clicked {
            cmds.push(AppCommand::SetEffectEnabled {
                clip_id,
                effect_index: i,
                enabled: !enabled,
            });
        }

        // Split-compare (A/B) toggle — mirrors the builtin path.
        let compare_active = matches!(
            state.split_compare_effect,
            Some((cid, idx, true)) if cid == clip_id && idx == i
        );
        let compare_rect = egui::Rect::from_min_size(
            egui::pos2(remove_x - 48.0, header_rect.center().y - 8.0),
            egui::vec2(16.0, 16.0),
        );
        let compare_resp = ui
            .interact(compare_rect, ui.id().with(("ofx_cmp", i)), egui::Sense::click())
            .on_hover_text(if compare_active { "Stop A/B compare" } else { "A/B compare: bypass vs applied" });
        let compare_tint = if compare_active {
            theme.accent_alt
        } else if compare_resp.hovered() {
            theme.text_primary
        } else {
            theme.text_dim
        };
        ui.painter().text(
            compare_rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{25D0}",
            egui::FontId::proportional(13.0),
            compare_tint,
        );
        let compare_clicked = compare_resp.clicked();
        if compare_clicked {
            if compare_active {
                cmds.push(AppCommand::SetSplitCompareEffect {
                    clip_id: None,
                    effect_index: 0,
                    is_ofx: false,
                });
            } else {
                cmds.push(AppCommand::SetSplitCompareEffect {
                    clip_id: Some(clip_id),
                    effect_index: i,
                    is_ofx: true,
                });
            }
        }

        // Click header to toggle collapse
        if header_resp.clicked() && !remove_clicked && !toggle_clicked && !compare_clicked {
            col_state.toggle(ui);
        }

        col_state.store(ui.ctx());

        // ── Card body ──
        if is_open {
            let body_frame = egui::Frame::none()
                .fill(card_bg)
                .stroke(egui::Stroke::new(0.5_f32, theme.border))
                .rounding(egui::Rounding { nw: 0.0, ne: 0.0, sw: card_rounding, se: card_rounding })
                .inner_margin(egui::Margin { left: pad, right: pad, top: 8.0, bottom: 8.0 });
            body_frame.show(ui, |ui: &mut egui::Ui| {
                // When disabled, dim and lock the params — mirrors the
                // builtin path for consistency.
                ui.add_enabled_ui(enabled, |ui| {
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
                        ui.label(egui::RichText::new("CPU only").size(10.0).color(theme.marker_warning).strong());
                        ui.label(egui::RichText::new("\u{2014} adjust while paused, apply on export").size(10.0).color(theme.text_dim));
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
                        ui.label(egui::RichText::new("Render only").size(10.0).color(theme.marker_error).strong());
                        ui.label(egui::RichText::new("\u{2014} not real-time, applied on export").size(10.0).color(theme.text_dim));
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
                        ui.label(egui::RichText::new("Requires multi-view").size(10.0).color(theme.marker_error).strong());
                        ui.label(egui::RichText::new("\u{2014} stereo 3D not supported yet").size(10.0).color(theme.text_dim));
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
                }); // end ui.add_enabled_ui
            });
        }

        // Drag visual feedback + reorder
        if header_resp.dragged_by(egui::PointerButton::Primary) {
            ui.painter().rect_stroke(
                header_rect,
                if is_open { egui::Rounding { nw: card_rounding, ne: card_rounding, sw: 0.0, se: 0.0 } } else { egui::Rounding::same(card_rounding) },
                egui::Stroke::new(2.0_f32, theme.accent),
            );
            let drag_dy = header_resp.drag_delta().y;
            let total_drag: f32 = ui.ctx().data_mut(|d| {
                let key = egui::Id::new("ofx_drag_accum").with(i);
                let accum: &mut f32 = d.get_temp_mut_or_default(key);
                *accum += drag_dy;
                *accum
            });
            if total_drag < -15.0 && i > 0 {
                ui.painter().hline(header_rect.x_range(), header_rect.min.y - 2.0, egui::Stroke::new(2.0_f32, theme.accent));
            } else if total_drag > 15.0 && i + 1 < clip.ofx_effects().len() {
                let line_y = if is_open { ui.cursor().min.y + 2.0 } else { header_rect.max.y + 2.0 };
                ui.painter().hline(header_rect.x_range(), line_y, egui::Stroke::new(2.0_f32, theme.accent));
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
                let max_idx = clip.ofx_effects().len() - 1;
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
    show_add_effect_button(ui, theme, clip, cmds);

    // Inline add menus (kept for compatibility)
    show_preset_selectors(ui, theme, state, clip, show_add_builtin, show_add_ofx, cmds);
}


/// The widest crop feather the inspector offers, in the sequence's pixels:
/// a soft edge across a fifth of a 1080-line frame's height.
const CROP_FEATHER_MAX_PX: f32 = 200.0;

fn show_preset_selectors(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    clip: &Clip,
    show_add_builtin: &mut bool,
    show_add_ofx: &mut bool,
    cmds: &mut Vec<AppCommand>,
) {
    let clip_id = clip.id;
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
                anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
                focal_ref: None }),
            ("Chroma Key", klipp_effects::BuiltinEffect::ChromaKey {
                key_color: [0, 255, 0],
                tolerance: 0.2,
                soft_edge: 0.1,
                spill_method: klipp_effects::SpillMethod::ChannelClamp,
                spill_strength: 0.6,
            }),
            ("Corner Pin", klipp_effects::corner_pin::still()),
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

fn show_add_effect_button(ui: &mut egui::Ui, theme: &KlippTheme, _clip: &Clip, cmds: &mut Vec<AppCommand>) {
    let card_rounding = 6.0;
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
            .stroke(egui::Stroke::new(1.0_f32, theme.border_light))
            .rounding(4.0);

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
                    egui::Stroke::new(1.0_f32, dash_color),
                );
                x += dash_len + gap_len;
            }
        }

        if resp.clicked() {
            cmds.push(AppCommand::ShowEffectsBrowser);
        }

        // "E" shortcut badge
        let (badge_rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
        ui.painter().rect(badge_rect, 4.0, theme.bg_deep, egui::Stroke::new(0.5_f32, theme.border));
        ui.painter().text(
            badge_rect.center(), egui::Align2::CENTER_CENTER,
            "E", egui::FontId::monospace(10.0), theme.text_dim,
        );
    });
}

fn show_drag_hint(ui: &mut egui::Ui, theme: &KlippTheme, total_effects: usize) {
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
}

fn show_ofx_param(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
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
                .color(theme.text_dim),
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
                        .color(theme.text_dim),
                );
            }
        }
    });
}

fn builtin_effect_name(effect: &klipp_effects::BuiltinEffect) -> std::borrow::Cow<'static, str> {
    use std::borrow::Cow;
    match effect {
        klipp_effects::BuiltinEffect::ColorFade { .. } => Cow::Borrowed("Colour Fade"),
        klipp_effects::BuiltinEffect::Opacity { .. } => Cow::Borrowed("Opacity"),
        klipp_effects::BuiltinEffect::GaussianBlur { .. } => Cow::Borrowed("Gaussian Blur"),
        klipp_effects::BuiltinEffect::RoundedCorners { .. } => Cow::Borrowed("Rounded Corners"),
        klipp_effects::BuiltinEffect::DropShadow { .. } => Cow::Borrowed("Drop Shadow"),
        klipp_effects::BuiltinEffect::OuterGlow { .. } => Cow::Borrowed("Outer Glow"),
        klipp_effects::BuiltinEffect::FillBars { .. } => Cow::Borrowed("Fill Bars"),
        klipp_effects::BuiltinEffect::Sharpen { .. } => Cow::Borrowed("Sharpen"),
        klipp_effects::BuiltinEffect::Crop { .. } => Cow::Borrowed("Crop"),
        klipp_effects::BuiltinEffect::Transform { .. } => Cow::Borrowed("Transform"),
        klipp_effects::BuiltinEffect::CornerPin { .. } => Cow::Borrowed("Corner Pin"),
        klipp_effects::BuiltinEffect::ChromaKey { .. } => Cow::Borrowed("Chroma Key"),
        klipp_effects::BuiltinEffect::Bloom { .. } => Cow::Borrowed("Bloom"),
        klipp_effects::BuiltinEffect::Glow { .. } => Cow::Borrowed("Glow"),
        klipp_effects::BuiltinEffect::LightRays { .. } => Cow::Borrowed("Light Rays"),
        klipp_effects::BuiltinEffect::Vignette { .. } => Cow::Borrowed("Vignette"),
        klipp_effects::BuiltinEffect::Diffusion { .. } => Cow::Borrowed("Diffusion"),
        klipp_effects::BuiltinEffect::LensFlare { .. } => Cow::Borrowed("Lens Flare"),
        klipp_effects::BuiltinEffect::LightWrap { .. } => Cow::Borrowed("Light Wrap"),
        klipp_effects::BuiltinEffect::AnamorphicStreak { .. } => Cow::Borrowed("Anamorphic Streak"),
        klipp_effects::BuiltinEffect::FilmHalation { .. } => Cow::Borrowed("Film Halation"),
        klipp_effects::BuiltinEffect::Invert { .. } => Cow::Borrowed("Invert"),
        klipp_effects::BuiltinEffect::Posterize { .. } => Cow::Borrowed("Posterize"),
        klipp_effects::BuiltinEffect::Mosaic { .. } => Cow::Borrowed("Mosaic"),
        klipp_effects::BuiltinEffect::EdgeDetect { .. } => Cow::Borrowed("Edge Detect"),
        klipp_effects::BuiltinEffect::ChannelMixer { .. } => Cow::Borrowed("Channel Mixer"),
        klipp_effects::BuiltinEffect::DirectionalBlur { .. } => Cow::Borrowed("Directional Blur"),
        klipp_effects::BuiltinEffect::Emboss { .. } => Cow::Borrowed("Emboss"),
        klipp_effects::BuiltinEffect::Mirror { .. } => Cow::Borrowed("Mirror"),
        klipp_effects::BuiltinEffect::LensDistortion { .. } => Cow::Borrowed("Lens Distortion"),
        klipp_effects::BuiltinEffect::Dilate { .. } => Cow::Borrowed("Dilate"),
        klipp_effects::BuiltinEffect::Erode { .. } => Cow::Borrowed("Erode"),
        klipp_effects::BuiltinEffect::Denoise { .. } => Cow::Borrowed("Denoise"),
        klipp_effects::BuiltinEffect::Gamma { .. } => Cow::Borrowed("Gamma"),
        klipp_effects::BuiltinEffect::HSVAdjust { .. } => Cow::Borrowed("HSV Adjust"),
        klipp_effects::BuiltinEffect::NoiseGenerator { .. } => Cow::Borrowed("Noise"),
        klipp_effects::BuiltinEffect::FilmGrain { .. } => Cow::Borrowed("Film Grain"),
        klipp_effects::BuiltinEffect::TiltShift { .. } => Cow::Borrowed("Tilt-Shift"),
        klipp_effects::BuiltinEffect::FocusBlur { .. } => Cow::Borrowed("Focus Blur"),
        klipp_effects::BuiltinEffect::OldFilm { .. } => Cow::Borrowed("Old Film"),
        klipp_effects::BuiltinEffect::Vhs { .. } => Cow::Borrowed("VHS"),
        klipp_effects::BuiltinEffect::MedianFilter { .. } => Cow::Borrowed("Median Filter"),
        klipp_effects::BuiltinEffect::RadialBlur { .. } => Cow::Borrowed("Radial Blur"),
        klipp_effects::BuiltinEffect::Levels { .. } => Cow::Borrowed("Levels"),
        klipp_effects::BuiltinEffect::Equalize { .. } => Cow::Borrowed("Equalize"),
        klipp_effects::BuiltinEffect::Letterbox { .. } => Cow::Borrowed("Letterbox"),
        klipp_effects::BuiltinEffect::ChromaticAberration { .. } => Cow::Borrowed("Chromatic Aberration"),
        klipp_effects::BuiltinEffect::RollingShutterCorrection { .. } => Cow::Borrowed("Rolling Shutter Correction"),
        klipp_effects::BuiltinEffect::TemporalDenoise { .. } => Cow::Borrowed("Temporal Noise Reduction"),
        klipp_effects::BuiltinEffect::Deflicker { .. } => Cow::Borrowed("Deflicker"),
        klipp_effects::BuiltinEffect::VideoStabilization { .. } => Cow::Borrowed("Video Stabilization"),
        klipp_effects::BuiltinEffect::SpeedRamp { .. } => Cow::Borrowed("Speed Ramp"),
        klipp_effects::BuiltinEffect::Vibrance { .. } => Cow::Borrowed("Vibrance"),
        klipp_effects::BuiltinEffect::Frei0r { type_id, .. } => {
            // Resolve the plugin's actual display name from the runtime
            // registry; fall back to "frei0r" if the plugin isn't
            // installed (covers project-doctor "missing plugin" listings).
            klipp_effects::frei0r_effect::lookup(type_id)
                .map(|info| Cow::Owned(info.name))
                .unwrap_or(Cow::Borrowed("frei0r"))
        }
    }
}

/// Returns the default (identity) version of the same effect variant.
#[allow(dead_code)]
fn builtin_effect_default(effect: &klipp_effects::BuiltinEffect) -> klipp_effects::BuiltinEffect {
    use klipp_effects::BuiltinEffect;
    match effect {
        BuiltinEffect::ColorFade { mode, .. } => BuiltinEffect::ColorFade {
            color: [0, 0, 0, 255], duration_secs: 1.0, mode: *mode, progress: 0.0,
        },
        BuiltinEffect::Opacity { .. } => BuiltinEffect::Opacity { value: 1.0 },
        BuiltinEffect::GaussianBlur { .. } => BuiltinEffect::GaussianBlur { radius: 0.0 },
        BuiltinEffect::RoundedCorners { .. } => BuiltinEffect::RoundedCorners { radius: 0.0, feather: 0.0 },
        BuiltinEffect::DropShadow { .. } => BuiltinEffect::DropShadow {
            offset_x: 0.0, offset_y: 0.0, blur: 0.0, color: [0, 0, 0], opacity: 0.0,
        },
        BuiltinEffect::OuterGlow { .. } => BuiltinEffect::OuterGlow { radius: 0.0, intensity: 0.0, color: [255, 255, 255] },
        // No setting of it fills nothing; this is where it starts.
        BuiltinEffect::FillBars { .. } => BuiltinEffect::FillBars { blur: 0.05, zoom: 1.0, darken: 0.25 },
        BuiltinEffect::Sharpen { .. } => BuiltinEffect::Sharpen { amount: 0.0 },
        BuiltinEffect::Crop { .. } => BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0 },
        BuiltinEffect::Transform { .. } => BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None },
        BuiltinEffect::ChromaKey { .. } => BuiltinEffect::ChromaKey {
            key_color: [0, 255, 0], tolerance: 0.2, soft_edge: 0.1,
            spill_method: klipp_effects::SpillMethod::ChannelClamp, spill_strength: 0.6,
        },
        BuiltinEffect::CornerPin { .. } => klipp_effects::corner_pin::still(),
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
        BuiltinEffect::Invert { .. } => BuiltinEffect::Invert { mix: 1.0 },
        BuiltinEffect::Posterize { .. } => BuiltinEffect::Posterize { levels: 8.0 },
        BuiltinEffect::Mosaic { .. } => BuiltinEffect::Mosaic { block_size: 10.0 },
        BuiltinEffect::EdgeDetect { .. } => BuiltinEffect::EdgeDetect { strength: 1.0, mix: 1.0 },
        BuiltinEffect::ChannelMixer { .. } => BuiltinEffect::ChannelMixer {
            red_red: 1.0, red_green: 0.0, red_blue: 0.0,
            green_red: 0.0, green_green: 1.0, green_blue: 0.0,
            blue_red: 0.0, blue_green: 0.0, blue_blue: 1.0,
        },
        BuiltinEffect::DirectionalBlur { .. } => BuiltinEffect::DirectionalBlur {
            angle: 0.0, distance: 20.0, samples: 8,
        },
        BuiltinEffect::Emboss { .. } => BuiltinEffect::Emboss {
            angle: 135.0, strength: 1.0, mix: 1.0,
        },
        BuiltinEffect::Mirror { .. } => BuiltinEffect::Mirror {
            horizontal: 1, vertical: 0,
        },
        BuiltinEffect::LensDistortion { .. } => BuiltinEffect::LensDistortion {
            distortion: 0.0, scale: 1.0,
        },
        BuiltinEffect::Dilate { .. } => BuiltinEffect::Dilate { radius: 1 },
        BuiltinEffect::Erode { .. } => BuiltinEffect::Erode { radius: 1 },
        BuiltinEffect::Denoise { .. } => BuiltinEffect::Denoise {
            spatial_sigma: 5.0, color_sigma: 0.1, iterations: 1,
        },
        BuiltinEffect::Gamma { .. } => BuiltinEffect::Gamma { gamma: 1.0 },
        BuiltinEffect::HSVAdjust { .. } => BuiltinEffect::HSVAdjust {
            hue_shift: 0.0, saturation: 1.0, value: 1.0,
        },
        BuiltinEffect::NoiseGenerator { .. } => BuiltinEffect::NoiseGenerator {
            amount: 0.1, monochrome: 0, seed: 0,
        },
        BuiltinEffect::FilmGrain { .. } => BuiltinEffect::FilmGrain {
            amount: 0.3, size: 1.5, color: 0, shadows: 0.6, seed: 0,
        },
        BuiltinEffect::TiltShift { .. } => BuiltinEffect::TiltShift {
            center: 0.5, width: 0.3, softness: 0.2, radius: 12.0,
        },
        BuiltinEffect::FocusBlur { .. } => BuiltinEffect::FocusBlur { radius: 8.0, vertical: true, band: 0.3 },
        BuiltinEffect::OldFilm { .. } => BuiltinEffect::OldFilm {
            flicker: 0.4, scratches: 0.5, dust: 0.5, weave: 0.5, sepia: 0.6, seed: 0,
        },
        BuiltinEffect::Vhs { .. } => BuiltinEffect::Vhs {
            tracking: 0.5, bleed: 0.5, noise: 0.4, scanlines: 0.5, seed: 0,
        },
        BuiltinEffect::MedianFilter { .. } => BuiltinEffect::MedianFilter { radius: 1 },
        BuiltinEffect::RadialBlur { .. } => BuiltinEffect::RadialBlur {
            center_x: 0.5, center_y: 0.5, amount: 10.0, samples: 12, mode: 0,
        },
        BuiltinEffect::Levels { .. } => BuiltinEffect::Levels {
            in_black: 0.0, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0,
        },
        BuiltinEffect::Equalize { .. } => BuiltinEffect::Equalize { strength: 1.0 },
        BuiltinEffect::Letterbox { .. } => BuiltinEffect::Letterbox {
            aspect_ratio: 2.39, bar_color: 0, opacity: 1.0,
        },
        BuiltinEffect::ChromaticAberration { .. } => BuiltinEffect::ChromaticAberration {
            red_shift: 2.0, blue_shift: -2.0, center_x: 0.5, center_y: 0.5,
        },
        BuiltinEffect::RollingShutterCorrection { .. } => BuiltinEffect::RollingShutterCorrection {
            strength: 0.5, readout_ratio: 0.5,
        },
        BuiltinEffect::TemporalDenoise { .. } => BuiltinEffect::TemporalDenoise { strength: 0.5, threshold: 0.02 },
        BuiltinEffect::Deflicker { .. } => BuiltinEffect::Deflicker { strength: 0.8, window_secs: 1.0, gain: 1.0 },
        BuiltinEffect::VideoStabilization { .. } => BuiltinEffect::VideoStabilization {
            smoothing: 0.5, crop_ratio: 0.05, zoom: true,
        },
        BuiltinEffect::SpeedRamp { .. } => BuiltinEffect::SpeedRamp {
            speed: 1.0, blend_mode: 1, ramp_curve: 0,
            ramp_start: 0.0, ramp_end: 1.0, reverse: false,
            freeze_at: 0.0, freeze: false,
        },
        klipp_effects::BuiltinEffect::Vibrance { .. } => {
            klipp_effects::BuiltinEffect::Vibrance { amount: 0.0 }
        },
        klipp_effects::BuiltinEffect::Frei0r { type_id, .. } => {
            // "Default" for a frei0r effect = same plugin, fresh
            // default param values from the registry. If the plugin
            // disappeared since the project was saved, fall through to
            // an empty params list (the lookup-failure path in
            // `frei0r_effect::apply` will passthrough).
            klipp_effects::BuiltinEffect::Frei0r {
                type_id: type_id.clone(),
                params: klipp_effects::frei0r_effect::default_params(type_id)
                    .unwrap_or_default(),
            }
        },
    }
}

#[allow(dead_code)]
fn builtin_effect_has_multi_params(effect: &klipp_effects::BuiltinEffect) -> bool {
    use klipp_effects::BuiltinEffect;
    matches!(effect, BuiltinEffect::Crop { .. } | BuiltinEffect::Transform { .. } | BuiltinEffect::ChromaKey { .. } | BuiltinEffect::Bloom { .. } | BuiltinEffect::LightRays { .. }
        | BuiltinEffect::Vignette { .. } | BuiltinEffect::Diffusion { .. } | BuiltinEffect::LensFlare { .. }
        | BuiltinEffect::LightWrap { .. } | BuiltinEffect::AnamorphicStreak { .. } | BuiltinEffect::FilmHalation { .. }
        | BuiltinEffect::EdgeDetect { .. } | BuiltinEffect::ChannelMixer { .. } | BuiltinEffect::DirectionalBlur { .. }
        | BuiltinEffect::Emboss { .. } | BuiltinEffect::Mirror { .. } | BuiltinEffect::LensDistortion { .. }
        | BuiltinEffect::Denoise { .. } | BuiltinEffect::RollingShutterCorrection { .. } | BuiltinEffect::VideoStabilization { .. } | BuiltinEffect::SpeedRamp { .. }
        | BuiltinEffect::Deflicker { .. } | BuiltinEffect::TemporalDenoise { .. }
        | BuiltinEffect::HSVAdjust { .. } | BuiltinEffect::NoiseGenerator { .. }
        | BuiltinEffect::RadialBlur { .. } | BuiltinEffect::Levels { .. }
        | BuiltinEffect::Letterbox { .. } | BuiltinEffect::ChromaticAberration { .. }
        | BuiltinEffect::FilmGrain { .. } | BuiltinEffect::TiltShift { .. }
        | BuiltinEffect::OldFilm { .. } | BuiltinEffect::Vhs { .. })
}

#[allow(dead_code)]
fn builtin_effect_is_default(effect: &klipp_effects::BuiltinEffect) -> bool {
    let def = builtin_effect_default(effect);
    // Compare by debug string — simple and correct for all variants
    format!("{:?}", effect) == format!("{:?}", def)
}

/// Compute KeyframeState for a given effect param keyframe vec at a given clip-relative time.
fn effect_keyframe_state(clip: &Clip, effect_index: usize, param_name: &str, clip_time_secs: f64, keyframe_tolerance: f64) -> KeyframeState {
    let key = format!("{}:{}", effect_index, param_name);
    let kfs = clip.effect_keyframes().get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
    crate::inspector::keyframe_selection::state(kfs, clip_time_secs, keyframe_tolerance)
}

/// The keyframe state of a parameter the render animates; `None`, so no
/// diamond is drawn, for one it does not.
fn keyframe_state_if(animatable: &[&str], clip: &Clip, effect_index: usize, param_name: &str, clip_time_secs: f64, keyframe_tolerance: f64) -> Option<KeyframeState> {
    animatable
        .contains(&param_name)
        .then(|| effect_keyframe_state(clip, effect_index, param_name, clip_time_secs, keyframe_tolerance))
}

/// Evaluate effect keyframes for a param, returning the keyframed value or the static default.
fn eval_effect_kf(clip: &Clip, effect_index: usize, param_name: &str, clip_time_secs: f64, default: f32, tolerance: f64) -> f32 {
    let key = format!("{}:{}", effect_index, param_name);
    if let Some(kfs) = clip.effect_keyframes().get(&key) {
        if !kfs.is_empty() {
            return crate::inspector::keyframe_selection::editable_value(kfs, clip_time_secs, default, tolerance);
        }
    }
    default
}

/// Check if any keyframes exist for a given effect param.
fn has_effect_kfs(clip: &Clip, effect_index: usize, param_name: &str) -> bool {
    let key = format!("{}:{}", effect_index, param_name);
    clip.effect_keyframes().get(&key).is_some_and(|kfs| !kfs.is_empty())
}

fn show_builtin_effect_params(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    effect: &klipp_effects::BuiltinEffect,
    clip_id: klipp_core::ClipId,
    effect_index: usize,
    cmds: &mut Vec<AppCommand>,
    clip: &Clip,
    clip_time_secs: f64,
    state: &AppState,
) -> Option<klipp_effects::BuiltinEffect> {
    #[cfg(any(test, feature = "row-recorder"))]
    let _recording_scope = crate::widgets::row_recorder::scope(crate::effect_presets::builtin_effect_key(effect));
    let keyframe_tolerance = crate::inspector::keyframe_selection::frame_tolerance(state.sequence());
    let mut updated = effect.clone();
    let mut changed = false;
    let mut any_drag_started = false;
    let mut any_drag_stopped = false;
    let drag_key = format!("{:?}:builtin:{}", clip_id, effect_index);
    let clip_timeline_in_secs = clip.timeline_in.to_secs_f64();
    let clip_rate = clip.timeline_in.rate;
    let kf_tolerance = keyframe_tolerance;

    // ── Preset selector ──
    let effect_key = crate::effect_presets::builtin_effect_key(effect);
    let presets = state.preset_manager.presets_for(effect_key);
    if !presets.is_empty() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Preset").size(10.0).color(theme.text_dim));
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
            ui.label(egui::RichText::new("Preset").size(10.0).color(theme.text_dim));
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
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::parameter($param_name);
            changed |= r.changed;
            if r.drag_started { any_drag_started = true; }
            if r.drag_stopped { any_drag_stopped = true; }
            kf_responses.push(($param_name, r, $val));
        }};
    }

    // Keyframed parameters show their value at the playhead. The names
    // are the render's own list, so a diamond only appears on a
    // parameter whose keys the picture follows (#992).
    let animatable = updated.animatable_params();
    updated.animate(&mut |name, v| eval_effect_kf(clip, effect_index, name, clip_time_secs, v, keyframe_tolerance));

    match &mut updated {
        klipp_effects::BuiltinEffect::ColorFade { color, duration_secs, mode, .. } => {
            // Mode picker (In / Out)
            ui.horizontal(|ui| {
                ui.label("Mode");
                let mut m = *mode;
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", "mode", "Mode", *mode);
                egui::ComboBox::from_id_salt(("color_fade_mode", effect_index))
                    .selected_text(match m {
                        klipp_effects::ColorFadeMode::In => "Fade From",
                        klipp_effects::ColorFadeMode::Out => "Fade To",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut m, klipp_effects::ColorFadeMode::In, "Fade From").clicked();
                        if ui.selectable_value(&mut m, klipp_effects::ColorFadeMode::Out, "Fade To").clicked() {}
                    });
                if m != *mode { *mode = m; changed = true; }
            });
            // Colour picker
            ui.horizontal(|ui| {
                ui.label("Colour");
                let mut c = egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(ui.next_auto_id(), "colour", "color", "Colour", *color);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    *color = [c.r(), c.g(), c.b(), c.a()];
                    changed = true;
                }
            });
            // Duration
            ui.horizontal(|ui| {
                ui.label("Duration");
                #[cfg(any(test, feature = "row-recorder"))]
                {
                    crate::widgets::row_recorder::numeric(ui.next_auto_id(), "drag_value", "Duration", *duration_secs, &(0.05..=10.0), None, Some(0.05), Some(2), " s", None);
                    crate::widgets::row_recorder::parameter("duration_secs");
                }
                if ui.add(egui::DragValue::new(duration_secs)
                    .range(0.05..=10.0)
                    .speed(0.05)
                    .suffix(" s")
                    .fixed_decimals(2)
                ).changed() {
                    changed = true;
                }
            });
        }
        klipp_effects::BuiltinEffect::Opacity { value } => {
            let ks = keyframe_state_if(&animatable, clip, effect_index, "value", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Opacity", value, 0.0..=1.0, 1.0, 0.005, 2, "", ks);
            track_scrubby!("value", r, *value);
        }
        klipp_effects::BuiltinEffect::GaussianBlur { radius } => {
            let ks = keyframe_state_if(&animatable, clip, effect_index, "radius", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Radius", radius, 0.0..=100.0, 0.0, 0.5, 1, " px", ks);
            track_scrubby!("radius", r, *radius);
        }
        klipp_effects::BuiltinEffect::RoundedCorners { radius, feather } => {
            let ks_r = keyframe_state_if(&animatable, clip, effect_index, "radius", clip_time_secs, keyframe_tolerance);
            let ks_f = keyframe_state_if(&animatable, clip, effect_index, "feather", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Radius", radius, 0.0..=0.5, 0.0, 0.002, 3, "", ks_r);
            track_scrubby!("radius", r, *radius);
            let r = scrubby_param_row(ui, "Feather", feather, 0.0..=0.1, 0.0, 0.001, 3, "", ks_f);
            track_scrubby!("feather", r, *feather);
        }
        klipp_effects::BuiltinEffect::DropShadow { offset_x, offset_y, blur, color, opacity } => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Colour").size(10.0));
                let mut c = egui::Color32::from_rgb(color[0], color[1], color[2]);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(ui.next_auto_id(), "colour", "color", "Colour", *color);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    *color = [c.r(), c.g(), c.b()];
                    changed = true;
                }
            });
            let ks_x = keyframe_state_if(&animatable, clip, effect_index, "offset_x", clip_time_secs, keyframe_tolerance);
            let ks_y = keyframe_state_if(&animatable, clip, effect_index, "offset_y", clip_time_secs, keyframe_tolerance);
            let ks_b = keyframe_state_if(&animatable, clip, effect_index, "blur", clip_time_secs, keyframe_tolerance);
            let ks_o = keyframe_state_if(&animatable, clip, effect_index, "opacity", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Right", offset_x, -0.2..=0.2, 0.0, 0.001, 3, "", ks_x);
            track_scrubby!("offset_x", r, *offset_x);
            let r = scrubby_param_row(ui, "Down", offset_y, -0.2..=0.2, 0.0, 0.001, 3, "", ks_y);
            track_scrubby!("offset_y", r, *offset_y);
            let r = scrubby_param_row(ui, "Blur", blur, 0.0..=0.2, 0.0, 0.001, 3, "", ks_b);
            track_scrubby!("blur", r, *blur);
            let r = scrubby_param_row(ui, "Opacity", opacity, 0.0..=1.0, 0.0, 0.005, 3, "", ks_o);
            track_scrubby!("opacity", r, *opacity);
        }
        klipp_effects::BuiltinEffect::OuterGlow { radius, intensity, color } => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Colour").size(10.0));
                let mut c = egui::Color32::from_rgb(color[0], color[1], color[2]);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(ui.next_auto_id(), "colour", "color", "Colour", *color);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    *color = [c.r(), c.g(), c.b()];
                    changed = true;
                }
            });
            let ks_r = keyframe_state_if(&animatable, clip, effect_index, "radius", clip_time_secs, keyframe_tolerance);
            let ks_i = keyframe_state_if(&animatable, clip, effect_index, "intensity", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Radius", radius, 0.0..=0.3, 0.0, 0.001, 3, "", ks_r);
            track_scrubby!("radius", r, *radius);
            let r = scrubby_param_row(ui, "Intensity", intensity, 0.0..=1.0, 0.0, 0.005, 3, "", ks_i);
            track_scrubby!("intensity", r, *intensity);
        }
        klipp_effects::BuiltinEffect::FillBars { blur, zoom, darken } => {
            let ks_b = keyframe_state_if(&animatable, clip, effect_index, "blur", clip_time_secs, keyframe_tolerance);
            let ks_z = keyframe_state_if(&animatable, clip, effect_index, "zoom", clip_time_secs, keyframe_tolerance);
            let ks_d = keyframe_state_if(&animatable, clip, effect_index, "darken", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Blur", blur, 0.0..=0.2, 0.05, 0.001, 3, "", ks_b);
            track_scrubby!("blur", r, *blur);
            let r = scrubby_param_row(ui, "Zoom", zoom, 1.0..=3.0, 1.0, 0.005, 2, "", ks_z);
            track_scrubby!("zoom", r, *zoom);
            let r = scrubby_param_row(ui, "Darken", darken, 0.0..=1.0, 0.25, 0.005, 2, "", ks_d);
            track_scrubby!("darken", r, *darken);
        }
        klipp_effects::BuiltinEffect::Sharpen { amount } => {
            let ks = keyframe_state_if(&animatable, clip, effect_index, "amount", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Amount", amount, 0.0..=2.0, 0.5, 0.01, 2, "", ks);
            track_scrubby!("amount", r, *amount);
        }
        klipp_effects::BuiltinEffect::Crop { left, right, top, bottom, feather } => {

            let ks_l = keyframe_state_if(&animatable, clip, effect_index, "left", clip_time_secs, keyframe_tolerance);
            let ks_r = keyframe_state_if(&animatable, clip, effect_index, "right", clip_time_secs, keyframe_tolerance);
            let ks_t = keyframe_state_if(&animatable, clip, effect_index, "top", clip_time_secs, keyframe_tolerance);
            let ks_b = keyframe_state_if(&animatable, clip, effect_index, "bottom", clip_time_secs, keyframe_tolerance);
            let ks_f = keyframe_state_if(&animatable, clip, effect_index, "feather", clip_time_secs, keyframe_tolerance);

            let r_l = scrubby_param_row(ui, "Left", left, 0.0..=1.0, 0.0, 0.005, 3, "", ks_l);
            track_scrubby!("left", r_l, *left);
            let r_r = scrubby_param_row(ui, "Right", right, 0.0..=1.0, 0.0, 0.005, 3, "", ks_r);
            track_scrubby!("right", r_r, *right);
            let r_t = scrubby_param_row(ui, "Top", top, 0.0..=1.0, 0.0, 0.005, 3, "", ks_t);
            track_scrubby!("top", r_t, *top);
            let r_b = scrubby_param_row(ui, "Bottom", bottom, 0.0..=1.0, 0.0, 0.005, 3, "", ks_b);
            track_scrubby!("bottom", r_b, *bottom);
            // In pixels of the sequence's frame, as the crop measures it: the
            // slider ran 0..1, so no feather could be wider than a pixel
            // (#1030).
            let r_f = scrubby_param_row(ui, "Feather", feather, 0.0..=CROP_FEATHER_MAX_PX, 0.0, 0.5, 1, " px", ks_f);
            track_scrubby!("feather", r_f, *feather);
        }
        klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y, .. } => {
            // Transform in effects tab: no keyframes (handled by video tab's dedicated keyframes)
            let r = scrubby_param_row(ui, "Pos X", position_x, -2000.0..=2000.0, 0.0, 1.0, 0, " px", None);
            track_scrubby!("", r, *position_x);
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::parameter("position_x");
            let r = scrubby_param_row(ui, "Pos Y", position_y, -2000.0..=2000.0, 0.0, 1.0, 0, " px", None);
            track_scrubby!("", r, *position_y);
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::parameter("position_y");
            let r = scrubby_param_row(ui, "Scale X", scale_x, 0.0..=4.0, 1.0, 0.01, 2, "\u{00d7}", None);
            track_scrubby!("", r, *scale_x);
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::parameter("scale_x");
            let r = scrubby_param_row(ui, "Scale Y", scale_y, 0.0..=4.0, 1.0, 0.01, 2, "\u{00d7}", None);
            track_scrubby!("", r, *scale_y);
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::parameter("scale_y");
            let r = scrubby_param_row(ui, "Rotation", rotation, -360.0..=360.0, 0.0, 0.5, 1, " \u{00b0}", None);
            track_scrubby!("", r, *rotation);
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::parameter("rotation");
            let r = scrubby_param_row(ui, "Anchor X", anchor_x, 0.0..=1.0, 0.5, 0.005, 3, "", None);
            track_scrubby!("", r, *anchor_x);
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::parameter("anchor_x");
            let r = scrubby_param_row(ui, "Anchor Y", anchor_y, 0.0..=1.0, 0.5, 0.005, 3, "", None);
            track_scrubby!("", r, *anchor_y);
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::parameter("anchor_y");
        }
        klipp_effects::BuiltinEffect::CornerPin { tl, tr, br, bl } => {
            // Eight rows, one per coordinate, each keyed under its own
            // name; the viewer's handles drag the same four corners.
            let labels = ["Top Left", "Top Right", "Bottom Right", "Bottom Left"];
            for ((corner, names), (label, still)) in [tl, tr, br, bl]
                .into_iter()
                .zip(klipp_effects::corner_pin::PARAMS)
                .zip(labels.iter().zip(klipp_effects::corner_pin::STILL))
            {
                for axis in 0..2 {
                    let name = names[axis];
                    let ks = keyframe_state_if(&animatable, clip, effect_index, name, clip_time_secs, keyframe_tolerance);
                    let row = format!("{label} {}", if axis == 0 { "X" } else { "Y" });
                    let r = scrubby_param_row(ui, &row, &mut corner[axis], -1.0..=2.0, still[axis], 0.002, 3, "", ks);
                    track_scrubby!(name, r, corner[axis]);
                }
            }
        }
        klipp_effects::BuiltinEffect::ChromaKey { key_color, tolerance, soft_edge, spill_method, spill_strength } => {
            // Color picker stays as-is (not a scrubby candidate)
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Key Colour").size(10.0));
                let mut color = egui::Color32::from_rgb(key_color[0], key_color[1], key_color[2]);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(ui.next_auto_id(), "colour", "key_color", "Key Colour", *key_color);
                if ui.color_edit_button_srgba(&mut color).changed() {
                    key_color[0] = color.r();
                    key_color[1] = color.g();
                    key_color[2] = color.b();
                    changed = true;
                }
            });
            // Keyframe key "spill" continues to address the soft-edge field
            // (it's what existing user automations are saved against, even
            // though the field was renamed). New "spill_strength" key drives
            // the real spill knob.

            let ks_t = keyframe_state_if(&animatable, clip, effect_index, "tolerance", clip_time_secs, keyframe_tolerance);
            let ks_se = keyframe_state_if(&animatable, clip, effect_index, "spill", clip_time_secs, keyframe_tolerance);
            let ks_ss = keyframe_state_if(&animatable, clip, effect_index, "spill_strength", clip_time_secs, keyframe_tolerance);

            let r_t = scrubby_param_row(ui, "Tolerance", tolerance, 0.0..=1.0, 0.2, 0.005, 3, "", ks_t);
            track_scrubby!("tolerance", r_t, *tolerance);
            let r_se = scrubby_param_row(ui, "Soft Edge", soft_edge, 0.0..=1.0, 0.1, 0.005, 3, "", ks_se);
            track_scrubby!("spill", r_se, *soft_edge);

            // Spill suppression — Method dropdown + Strength slider.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Spill Method").size(10.0));
                let prev = *spill_method;
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", "spill_method", "Spill Method", *spill_method);
                egui::ComboBox::from_id_salt(("chroma_key_spill_method", effect_index))
                    .selected_text(match spill_method {
                        klipp_effects::SpillMethod::None => "None",
                        klipp_effects::SpillMethod::ChannelClamp => "Channel Clamp",
                        klipp_effects::SpillMethod::Hawk => "Hawk",
                    })
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(matches!(spill_method, klipp_effects::SpillMethod::None), "None").clicked() {
                            *spill_method = klipp_effects::SpillMethod::None;
                        }
                        if ui.selectable_label(matches!(spill_method, klipp_effects::SpillMethod::ChannelClamp), "Channel Clamp").clicked() {
                            *spill_method = klipp_effects::SpillMethod::ChannelClamp;
                        }
                        if ui.selectable_label(matches!(spill_method, klipp_effects::SpillMethod::Hawk), "Hawk").clicked() {
                            *spill_method = klipp_effects::SpillMethod::Hawk;
                        }
                    });
                if *spill_method != prev {
                    changed = true;
                }
            });
            let r_ss = scrubby_param_row(ui, "Spill Strength", spill_strength, 0.0..=1.0, 0.6, 0.005, 3, "", ks_ss);
            track_scrubby!("spill_strength", r_ss, *spill_strength);
        }
        klipp_effects::BuiltinEffect::Bloom { threshold, radius, intensity } => {

            let ks_th = keyframe_state_if(&animatable, clip, effect_index, "threshold", clip_time_secs, keyframe_tolerance);
            let ks_r = keyframe_state_if(&animatable, clip, effect_index, "radius", clip_time_secs, keyframe_tolerance);
            let ks_i = keyframe_state_if(&animatable, clip, effect_index, "intensity", clip_time_secs, keyframe_tolerance);

            let r_th = scrubby_param_row(ui, "Threshold", threshold, 0.0..=1.0, 0.7, 0.005, 2, "", ks_th);
            track_scrubby!("threshold", r_th, *threshold);
            let r_r = scrubby_param_row(ui, "Radius", radius, 0.0..=100.0, 20.0, 0.5, 1, " px", ks_r);
            track_scrubby!("radius", r_r, *radius);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=2.0, 0.5, 0.01, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
        }
        klipp_effects::BuiltinEffect::Glow { radius, intensity } => {

            let ks_r = keyframe_state_if(&animatable, clip, effect_index, "radius", clip_time_secs, keyframe_tolerance);
            let ks_i = keyframe_state_if(&animatable, clip, effect_index, "intensity", clip_time_secs, keyframe_tolerance);

            let r_r = scrubby_param_row(ui, "Radius", radius, 0.0..=100.0, 30.0, 0.5, 1, " px", ks_r);
            track_scrubby!("radius", r_r, *radius);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=2.0, 0.4, 0.01, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
        }
        klipp_effects::BuiltinEffect::LightRays { center_x, center_y, threshold, samples, decay, intensity } => {

            let ks_cx = keyframe_state_if(&animatable, clip, effect_index, "center_x", clip_time_secs, keyframe_tolerance);
            let ks_cy = keyframe_state_if(&animatable, clip, effect_index, "center_y", clip_time_secs, keyframe_tolerance);
            let ks_th = keyframe_state_if(&animatable, clip, effect_index, "threshold", clip_time_secs, keyframe_tolerance);
            let ks_d = keyframe_state_if(&animatable, clip, effect_index, "decay", clip_time_secs, keyframe_tolerance);
            let ks_i = keyframe_state_if(&animatable, clip, effect_index, "intensity", clip_time_secs, keyframe_tolerance);

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
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::parameter("samples");
            let r_d = scrubby_param_row(ui, "Decay", decay, 0.0..=1.0, 0.3, 0.005, 3, "", ks_d);
            track_scrubby!("decay", r_d, *decay);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=2.0, 0.5, 0.01, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
        }
        klipp_effects::BuiltinEffect::Vignette { amount, size, softness, roundness } => {

            let ks_a = keyframe_state_if(&animatable, clip, effect_index, "amount", clip_time_secs, keyframe_tolerance);
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "size", clip_time_secs, keyframe_tolerance);
            let ks_sf = keyframe_state_if(&animatable, clip, effect_index, "softness", clip_time_secs, keyframe_tolerance);
            let ks_r = keyframe_state_if(&animatable, clip, effect_index, "roundness", clip_time_secs, keyframe_tolerance);

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

            let ks_a = keyframe_state_if(&animatable, clip, effect_index, "amount", clip_time_secs, keyframe_tolerance);
            let ks_r = keyframe_state_if(&animatable, clip, effect_index, "radius", clip_time_secs, keyframe_tolerance);
            let ks_th = keyframe_state_if(&animatable, clip, effect_index, "threshold", clip_time_secs, keyframe_tolerance);
            let ks_d = keyframe_state_if(&animatable, clip, effect_index, "desaturation", clip_time_secs, keyframe_tolerance);

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

            let ks_px = keyframe_state_if(&animatable, clip, effect_index, "position_x", clip_time_secs, keyframe_tolerance);
            let ks_py = keyframe_state_if(&animatable, clip, effect_index, "position_y", clip_time_secs, keyframe_tolerance);
            let ks_b = keyframe_state_if(&animatable, clip, effect_index, "brightness", clip_time_secs, keyframe_tolerance);
            let ks_fs = keyframe_state_if(&animatable, clip, effect_index, "flare_size", clip_time_secs, keyframe_tolerance);
            let ks_gi = keyframe_state_if(&animatable, clip, effect_index, "ghost_intensity", clip_time_secs, keyframe_tolerance);
            let ks_ca = keyframe_state_if(&animatable, clip, effect_index, "chromatic_aberration", clip_time_secs, keyframe_tolerance);

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
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::parameter("ghost_count");
            let r_gi = scrubby_param_row(ui, "Ghost Intensity", ghost_intensity, 0.0..=1.0, 0.3, 0.005, 2, "", ks_gi);
            track_scrubby!("ghost_intensity", r_gi, *ghost_intensity);
            let r_ca = scrubby_param_row(ui, "Chromatic Aberration", chromatic_aberration, 0.0..=1.0, 0.4, 0.005, 2, "", ks_ca);
            track_scrubby!("chromatic_aberration", r_ca, *chromatic_aberration);
        }
        klipp_effects::BuiltinEffect::LightWrap { width: wrap_w, intensity, softness } => {

            let ks_w = keyframe_state_if(&animatable, clip, effect_index, "width", clip_time_secs, keyframe_tolerance);
            let ks_i = keyframe_state_if(&animatable, clip, effect_index, "intensity", clip_time_secs, keyframe_tolerance);
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "softness", clip_time_secs, keyframe_tolerance);

            let r_w = scrubby_param_row(ui, "Width", wrap_w, 0.0..=100.0, 15.0, 0.5, 1, " px", ks_w);
            track_scrubby!("width", r_w, *wrap_w);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=1.0, 0.5, 0.005, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
            let r_s = scrubby_param_row(ui, "Softness", softness, 0.0..=2.0, 1.0, 0.01, 2, "", ks_s);
            track_scrubby!("softness", r_s, *softness);
        }
        klipp_effects::BuiltinEffect::AnamorphicStreak { threshold, length, intensity, tint } => {

            let ks_th = keyframe_state_if(&animatable, clip, effect_index, "threshold", clip_time_secs, keyframe_tolerance);
            let ks_l = keyframe_state_if(&animatable, clip, effect_index, "length", clip_time_secs, keyframe_tolerance);
            let ks_i = keyframe_state_if(&animatable, clip, effect_index, "intensity", clip_time_secs, keyframe_tolerance);
            let ks_t = keyframe_state_if(&animatable, clip, effect_index, "tint", clip_time_secs, keyframe_tolerance);

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

            let ks_th = keyframe_state_if(&animatable, clip, effect_index, "threshold", clip_time_secs, keyframe_tolerance);
            let ks_r = keyframe_state_if(&animatable, clip, effect_index, "radius", clip_time_secs, keyframe_tolerance);
            let ks_i = keyframe_state_if(&animatable, clip, effect_index, "intensity", clip_time_secs, keyframe_tolerance);
            let ks_h = keyframe_state_if(&animatable, clip, effect_index, "hue", clip_time_secs, keyframe_tolerance);

            let r_th = scrubby_param_row(ui, "Threshold", threshold, 0.0..=1.0, 0.75, 0.005, 2, "", ks_th);
            track_scrubby!("threshold", r_th, *threshold);
            let r_r = scrubby_param_row(ui, "Radius", radius, 0.0..=50.0, 15.0, 0.5, 1, " px", ks_r);
            track_scrubby!("radius", r_r, *radius);
            let r_i = scrubby_param_row(ui, "Intensity", intensity, 0.0..=1.0, 0.3, 0.005, 2, "", ks_i);
            track_scrubby!("intensity", r_i, *intensity);
            let r_h = scrubby_param_row(ui, "Hue", hue, 0.0..=1.0, 0.2, 0.005, 2, "", ks_h);
            track_scrubby!("hue", r_h, *hue);
        }
        klipp_effects::BuiltinEffect::RollingShutterCorrection { strength, readout_ratio } => {
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "strength", clip_time_secs, keyframe_tolerance);
            let ks_r = keyframe_state_if(&animatable, clip, effect_index, "readout_ratio", clip_time_secs, keyframe_tolerance);
            let r_s = scrubby_param_row(ui, "Strength", strength, 0.0..=1.0, 0.5, 0.005, 2, "", ks_s);
            track_scrubby!("strength", r_s, *strength);
            let r_r = scrubby_param_row(ui, "Readout Ratio", readout_ratio, 0.0..=1.0, 0.5, 0.005, 2, "", ks_r);
            track_scrubby!("readout_ratio", r_r, *readout_ratio);
        }
        klipp_effects::BuiltinEffect::TemporalDenoise { strength, threshold } => {
            ui.label(
                egui::RichText::new(
                    "Runs on the source, before the grade — where it sits in this list makes no difference.",
                )
                .size(10.0)
                .color(theme.text_dim),
            );
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "strength", clip_time_secs, keyframe_tolerance);
            let r_s = scrubby_param_row(ui, "Strength", strength, 0.0..=1.0, 0.5, 0.01, 2, "", ks_s);
            track_scrubby!("strength", r_s, *strength);
            let ks_t = keyframe_state_if(&animatable, clip, effect_index, "threshold", clip_time_secs, keyframe_tolerance);
            let r_t = scrubby_param_row(ui, "Threshold", threshold, 0.001..=0.2, 0.02, 0.001, 3, "", ks_t);
            track_scrubby!("threshold", r_t, *threshold);
            ui.label(
                egui::RichText::new(
                    "How far a pixel may change between frames and still count as the same pixel. Too high and moving subjects ghost; too low and nothing is cleaned.",
                )
                .size(10.0)
                .color(theme.text_dim),
            );
            ui.label(
                egui::RichText::new(
                    "A clip using this is rendered on the CPU rather than the GPU: the frames it averages against are decoded there.",
                )
                .size(10.0)
                .color(theme.text_dim),
            );
        }
        klipp_effects::BuiltinEffect::Deflicker { strength, window_secs, .. } => {
            let has_data = clip.deflicker_data.is_some();
            let is_analyzing = state.deflicker_in_progress.contains(&clip_id);

            if is_analyzing {
                let progress = state.deflicker_progress.get(&clip_id).copied().unwrap_or(0.0);
                ui.add(egui::ProgressBar::new(progress).text(format!("Measuring {:.0}%", progress * 100.0)));
                if ui.small_button("Cancel").clicked() {
                    cmds.push(AppCommand::CancelDeflicker { clip_id });
                }
            } else if has_data {
                ui.horizontal(|ui| {
                    ui.colored_label(theme.marker_success, "Analysis complete");
                    if ui.small_button("Re-analyse").clicked() {
                        cmds.push(AppCommand::AnalyzeDeflicker { clip_id });
                    }
                });
                // What it found, so "nothing happened" is answerable.
                if let Some(gains) = clip.deflicker_gains.as_ref() {
                    let worst = gains
                        .gains
                        .iter()
                        .map(|g| (g - 1.0).abs())
                        .fold(0.0f32, f32::max);
                    let (colour, text) = if worst < 0.01 {
                        (theme.text_dim, "No flicker found — the shot is already steady".to_string())
                    } else {
                        (
                            theme.marker_success,
                            format!("Flicker found: up to {:.1}% per frame", worst * 100.0),
                        )
                    };
                    ui.colored_label(colour, egui::RichText::new(text).size(11.0));
                }
            } else {
                ui.colored_label(theme.text_dim, egui::RichText::new("Not analysed — the clip's light has to be measured first").size(11.0));
                if ui.button("Analyse").clicked() {
                    cmds.push(AppCommand::AnalyzeDeflicker { clip_id });
                }
            }

            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "strength", clip_time_secs, keyframe_tolerance);
            let r_s = scrubby_param_row(ui, "Strength", strength, 0.0..=1.0, 0.8, 0.01, 2, "", ks_s);
            track_scrubby!("strength", r_s, *strength);
            let ks_w = keyframe_state_if(&animatable, clip, effect_index, "window_secs", clip_time_secs, keyframe_tolerance);
            let r_w = scrubby_param_row(ui, "Window", window_secs, 0.2..=4.0, 1.0, 0.05, 2, " s", ks_w);
            let window_moved = r_w.changed;
            track_scrubby!("window_secs", r_w, *window_secs);
            if window_moved {
                // The measurement stands; only the smoothing over it
                // has to be redone.
                cmds.push(AppCommand::RegainDeflicker { clip_id });
                changed = true;
            }
            ui.label(
                egui::RichText::new(
                    "Variation faster than the window is flicker and goes; slower is the shot's own brightness and stays.",
                )
                .size(10.0)
                .color(theme.text_dim),
            );
        }
        klipp_effects::BuiltinEffect::VideoStabilization { smoothing, crop_ratio, zoom } => {
            // Analysis status
            let has_data = clip.stabilization_data.is_some();
            let is_analyzing = state.stabilization_in_progress.contains(&clip_id);

            if is_analyzing {
                let progress = state.stabilization_progress.get(&clip_id).copied().unwrap_or(0.0);
                ui.add(egui::ProgressBar::new(progress).text(format!("Analyzing {:.0}%", progress * 100.0)));
                if ui.small_button("Cancel").clicked() {
                    cmds.push(AppCommand::CancelStabilization { clip_id });
                }
            } else if has_data {
                ui.horizontal(|ui| {
                    ui.colored_label(theme.marker_success, "Analysis complete");
                    if ui.small_button("Re-analyze").clicked() {
                        cmds.push(AppCommand::AnalyzeStabilization { clip_id });
                    }
                    let compare_on = state.stabilization_compare_active;
                    let compare_label = if compare_on { "Exit Compare" } else { "Compare" };
                    let compare_btn = ui.small_button(compare_label)
                        .on_hover_text("Split-view raw vs stabilized. Drag the divider on the viewer to compare.");
                    if compare_btn.clicked() {
                        cmds.push(AppCommand::ToggleStabilizationCompare);
                    }
                });
                // Quality metric: how much shake the smoothing actually removed.
                if let Some(smoothed) = clip.smoothed_trajectory.as_ref() {
                    let pct = smoothed.jitter_reduction_pct();
                    let (label_color, headline) = if smoothed.raw_jitter_px < 0.25 {
                        // Source was already smooth — nothing to remove.
                        (theme.text_dim, "Source already smooth — minimal shake detected".to_string())
                    } else if pct >= 60.0 {
                        (theme.marker_success, format!("Shake reduced by {pct:.0}%"))
                    } else if pct >= 25.0 {
                        (theme.marker_warning, format!("Shake reduced by {pct:.0}%"))
                    } else {
                        (theme.marker_error, format!("Shake reduced by {pct:.0}%"))
                    };
                    ui.colored_label(label_color, egui::RichText::new(headline).size(11.0));
                    ui.label(
                        egui::RichText::new(format!(
                            "Jitter: {:.1} → {:.1} px/frame",
                            smoothed.raw_jitter_px, smoothed.smoothed_jitter_px
                        ))
                        .size(10.0)
                        .color(theme.text_dim),
                    );

                    // Trajectory graph: raw camera tx (jagged) vs smoothed tx (clean).
                    // Visualizes what stabilization is actually doing.
                    if let Some(data) = clip.stabilization_data.as_ref() {
                        let raw: Vec<f32> = data.trajectory.iter().map(|p| p.tx as f32).collect();
                        let smooth: Vec<f32> = smoothed.smoothed.iter().map(|p| p.tx as f32).collect();
                        if raw.len() >= 2 && smooth.len() == raw.len() {
                            let avail = ui.available_width();
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(avail, 64.0),
                                egui::Sense::hover(),
                            );
                            let p = ui.painter_at(rect);
                            // Background
                            p.rect_filled(rect, 2.0, theme.bg_deep);
                            p.rect_stroke(rect, 2.0, egui::Stroke::new(0.5_f32, theme.border));

                            // Y range covers both lines with small padding.
                            let mut lo = f32::INFINITY;
                            let mut hi = f32::NEG_INFINITY;
                            for &v in raw.iter().chain(smooth.iter()) {
                                if v < lo { lo = v; }
                                if v > hi { hi = v; }
                            }
                            let span = (hi - lo).max(1.0);
                            let pad_y = 4.0;
                            let inner = rect.shrink2(egui::vec2(4.0, pad_y));

                            let to_pt = |i: usize, v: f32| -> egui::Pos2 {
                                let x = inner.left() + (i as f32 / (raw.len() - 1) as f32) * inner.width();
                                let t = (v - lo) / span;
                                let y = inner.bottom() - t * inner.height();
                                egui::pos2(x, y)
                            };

                            // Zero/baseline reference line at the smoothed midpoint.
                            let mid = (lo + hi) * 0.5;
                            let mid_y = inner.bottom() - ((mid - lo) / span) * inner.height();
                            p.line_segment(
                                [egui::pos2(inner.left(), mid_y), egui::pos2(inner.right(), mid_y)],
                                egui::Stroke::new(0.5_f32, theme.border),
                            );

                            let raw_pts: Vec<egui::Pos2> = raw.iter().enumerate().map(|(i, &v)| to_pt(i, v)).collect();
                            let smooth_pts: Vec<egui::Pos2> = smooth.iter().enumerate().map(|(i, &v)| to_pt(i, v)).collect();
                            // Raw trajectory: dim, thin
                            p.add(egui::Shape::line(raw_pts, egui::Stroke::new(1.0_f32, theme.text_dim)));
                            // Smoothed trajectory: bright accent, thicker
                            p.add(egui::Shape::line(smooth_pts, egui::Stroke::new(1.6_f32, theme.accent)));

                            // Tiny legend
                            p.text(
                                egui::pos2(rect.left() + 6.0, rect.top() + 4.0),
                                egui::Align2::LEFT_TOP,
                                "raw",
                                egui::FontId::proportional(8.5),
                                theme.text_dim,
                            );
                            p.text(
                                egui::pos2(rect.left() + 28.0, rect.top() + 4.0),
                                egui::Align2::LEFT_TOP,
                                "smoothed",
                                egui::FontId::proportional(8.5),
                                theme.accent,
                            );
                        }
                    }
                }
            } else {
                if ui.button("Analyze Clip").clicked() {
                    cmds.push(AppCommand::AnalyzeStabilization { clip_id });
                }
            }

            ui.add_space(4.0);

            // Parameter sliders
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "smoothing", clip_time_secs, keyframe_tolerance);
            let ks_c = keyframe_state_if(&animatable, clip, effect_index, "crop_ratio", clip_time_secs, keyframe_tolerance);
            let r_s = scrubby_param_row(ui, "Smoothing", smoothing, 0.0..=1.0, 0.5, 0.005, 2, "", ks_s);
            let smoothing_changed = r_s.changed;
            track_scrubby!("smoothing", r_s, *smoothing);
            // Smoothing slider drives live re-smooth: cheap (microseconds), no decode.
            // Only fires when there's analysis data to re-smooth against.
            if smoothing_changed && clip.stabilization_data.is_some() {
                cmds.push(AppCommand::ResmoothStabilization { clip_id });
                changed = true;
            }
            let r_c = scrubby_param_row(ui, "Crop", crop_ratio, 0.0..=0.2, 0.05, 0.002, 3, "", ks_c);
            track_scrubby!("crop_ratio", r_c, *crop_ratio);

            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "zoom", "Auto-zoom to fill", *zoom);
            if ui.checkbox(zoom, "Auto-zoom to fill").changed() {
                changed = true;
            }
        }
        klipp_effects::BuiltinEffect::SpeedRamp { speed, blend_mode, ramp_curve, ramp_start, ramp_end, reverse, freeze_at, freeze } => {

            let ks_sp = keyframe_state_if(&animatable, clip, effect_index, "speed", clip_time_secs, keyframe_tolerance);
            let ks_rs = keyframe_state_if(&animatable, clip, effect_index, "ramp_start", clip_time_secs, keyframe_tolerance);
            let ks_re = keyframe_state_if(&animatable, clip, effect_index, "ramp_end", clip_time_secs, keyframe_tolerance);

            let r_sp = scrubby_param_row(ui, "Speed", speed, 0.1..=4.0, 1.0, 0.05, 2, "\u{00d7}", ks_sp);
            track_scrubby!("speed", r_sp, *speed);

            // Blend Mode dropdown
            let blend_labels = ["Nearest", "Linear"];
            let blend_sel = (*blend_mode as usize).min(blend_labels.len() - 1);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Blend Mode").size(10.0));
                let combo_id = ui.make_persistent_id(format!("sr_blend_{clip_id:?}_{effect_index}"));
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(combo_id, "choice", "blend_mode", "Blend Mode", *blend_mode);
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
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(combo_id, "choice", "ramp_curve", "Ramp Curve", *ramp_curve);
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

            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "reverse", "Reverse", *reverse);
            if ui.checkbox(reverse, "Reverse").changed() {
                changed = true;
            }
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "freeze", "Freeze Frame", *freeze);
            if ui.checkbox(freeze, "Freeze Frame").changed() {
                changed = true;
            }
            if *freeze {
                let ks_fa = keyframe_state_if(&animatable, clip, effect_index, "freeze_at", clip_time_secs, keyframe_tolerance);
                let r_fa = scrubby_param_row(ui, "Freeze At", freeze_at, 0.0..=1.0, 0.5, 0.005, 3, "", ks_fa);
                track_scrubby!("freeze_at", r_fa, *freeze_at);
            }
        }
        klipp_effects::BuiltinEffect::Vibrance { amount } => {
            let ks = keyframe_state_if(&animatable, clip, effect_index, "amount", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Amount", amount, -1.0..=2.0, 0.5, 0.01, 2, "", ks);
            track_scrubby!("amount", r, *amount);
        }
        klipp_effects::BuiltinEffect::Invert { mix } => {
            let ks = keyframe_state_if(&animatable, clip, effect_index, "mix", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Mix", mix, 0.0..=1.0, 1.0, 0.005, 2, "", ks);
            track_scrubby!("mix", r, *mix);
        }
        klipp_effects::BuiltinEffect::Posterize { levels } => {
            let ks = keyframe_state_if(&animatable, clip, effect_index, "levels", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Levels", levels, 2.0..=256.0, 8.0, 1.0, 0, "", ks);
            track_scrubby!("levels", r, *levels);
        }
        klipp_effects::BuiltinEffect::Mosaic { block_size } => {
            let ks = keyframe_state_if(&animatable, clip, effect_index, "block_size", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Block Size", block_size, 1.0..=100.0, 10.0, 0.5, 1, " px", ks);
            track_scrubby!("block_size", r, *block_size);
        }
        klipp_effects::BuiltinEffect::EdgeDetect { strength, mix } => {
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "strength", clip_time_secs, keyframe_tolerance);
            let ks_m = keyframe_state_if(&animatable, clip, effect_index, "mix", clip_time_secs, keyframe_tolerance);
            let r_s = scrubby_param_row(ui, "Strength", strength, 0.0..=5.0, 1.0, 0.05, 2, "", ks_s);
            track_scrubby!("strength", r_s, *strength);
            let r_m = scrubby_param_row(ui, "Mix", mix, 0.0..=1.0, 1.0, 0.005, 2, "", ks_m);
            track_scrubby!("mix", r_m, *mix);
        }
        klipp_effects::BuiltinEffect::ChannelMixer {
            red_red, red_green, red_blue,
            green_red, green_green, green_blue,
            blue_red, blue_green, blue_blue,
        } => {

            ui.label(egui::RichText::new("Red Output").size(10.0).strong());
            let ks = keyframe_state_if(&animatable, clip, effect_index, "red_red", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Red", red_red, -1.0..=2.0, 1.0, 0.01, 2, "", ks);
            track_scrubby!("red_red", r, *red_red);
            let ks = keyframe_state_if(&animatable, clip, effect_index, "red_green", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Green", red_green, -1.0..=2.0, 0.0, 0.01, 2, "", ks);
            track_scrubby!("red_green", r, *red_green);
            let ks = keyframe_state_if(&animatable, clip, effect_index, "red_blue", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Blue", red_blue, -1.0..=2.0, 0.0, 0.01, 2, "", ks);
            track_scrubby!("red_blue", r, *red_blue);

            ui.label(egui::RichText::new("Green Output").size(10.0).strong());
            let ks = keyframe_state_if(&animatable, clip, effect_index, "green_red", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Red", green_red, -1.0..=2.0, 0.0, 0.01, 2, "", ks);
            track_scrubby!("green_red", r, *green_red);
            let ks = keyframe_state_if(&animatable, clip, effect_index, "green_green", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Green", green_green, -1.0..=2.0, 1.0, 0.01, 2, "", ks);
            track_scrubby!("green_green", r, *green_green);
            let ks = keyframe_state_if(&animatable, clip, effect_index, "green_blue", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Blue", green_blue, -1.0..=2.0, 0.0, 0.01, 2, "", ks);
            track_scrubby!("green_blue", r, *green_blue);

            ui.label(egui::RichText::new("Blue Output").size(10.0).strong());
            let ks = keyframe_state_if(&animatable, clip, effect_index, "blue_red", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Red", blue_red, -1.0..=2.0, 0.0, 0.01, 2, "", ks);
            track_scrubby!("blue_red", r, *blue_red);
            let ks = keyframe_state_if(&animatable, clip, effect_index, "blue_green", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Green", blue_green, -1.0..=2.0, 0.0, 0.01, 2, "", ks);
            track_scrubby!("blue_green", r, *blue_green);
            let ks = keyframe_state_if(&animatable, clip, effect_index, "blue_blue", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Blue", blue_blue, -1.0..=2.0, 1.0, 0.01, 2, "", ks);
            track_scrubby!("blue_blue", r, *blue_blue);
        }
        klipp_effects::BuiltinEffect::DirectionalBlur { angle, distance, samples } => {
            let ks_a = keyframe_state_if(&animatable, clip, effect_index, "angle", clip_time_secs, keyframe_tolerance);
            let ks_d = keyframe_state_if(&animatable, clip, effect_index, "distance", clip_time_secs, keyframe_tolerance);
            let r_a = scrubby_param_row(ui, "Angle", angle, 0.0..=360.0, 0.0, 1.0, 1, "\u{00b0}", ks_a);
            track_scrubby!("angle", r_a, *angle);
            let r_d = scrubby_param_row(ui, "Distance", distance, 0.0..=100.0, 20.0, 0.5, 1, " px", ks_d);
            track_scrubby!("distance", r_d, *distance);
            // Samples: integer, use a simple slider
            let mut s_f = *samples as f32;
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "samples", clip_time_secs, keyframe_tolerance);
            let r_s = scrubby_param_row(ui, "Samples", &mut s_f, 3.0..=32.0, 8.0, 1.0, 0, "", ks_s);
            *samples = s_f as u32;
            track_scrubby!("samples", r_s, s_f);
        }
        klipp_effects::BuiltinEffect::Emboss { angle, strength, mix } => {
            let ks_a = keyframe_state_if(&animatable, clip, effect_index, "angle", clip_time_secs, keyframe_tolerance);
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "strength", clip_time_secs, keyframe_tolerance);
            let ks_m = keyframe_state_if(&animatable, clip, effect_index, "mix", clip_time_secs, keyframe_tolerance);
            let r_a = scrubby_param_row(ui, "Angle", angle, 0.0..=360.0, 135.0, 1.0, 1, "\u{00b0}", ks_a);
            track_scrubby!("angle", r_a, *angle);
            let r_s = scrubby_param_row(ui, "Strength", strength, 0.0..=5.0, 1.0, 0.05, 2, "", ks_s);
            track_scrubby!("strength", r_s, *strength);
            let r_m = scrubby_param_row(ui, "Mix", mix, 0.0..=1.0, 1.0, 0.005, 2, "", ks_m);
            track_scrubby!("mix", r_m, *mix);
        }
        klipp_effects::BuiltinEffect::Mirror { horizontal, vertical } => {
            let mut h_bool = *horizontal != 0;
            let mut v_bool = *vertical != 0;
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "horizontal", "Horizontal", h_bool);
            if ui.checkbox(&mut h_bool, "Horizontal").changed() {
                *horizontal = if h_bool { 1 } else { 0 };
                changed = true;
            }
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "vertical", "Vertical", v_bool);
            if ui.checkbox(&mut v_bool, "Vertical").changed() {
                *vertical = if v_bool { 1 } else { 0 };
                changed = true;
            }
        }
        klipp_effects::BuiltinEffect::LensDistortion { distortion, scale } => {
            let ks_d = keyframe_state_if(&animatable, clip, effect_index, "distortion", clip_time_secs, keyframe_tolerance);
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "scale", clip_time_secs, keyframe_tolerance);
            let r_d = scrubby_param_row(ui, "Distortion", distortion, -1.0..=1.0, 0.0, 0.01, 2, "", ks_d);
            track_scrubby!("distortion", r_d, *distortion);
            let r_s = scrubby_param_row(ui, "Scale", scale, 0.5..=2.0, 1.0, 0.01, 2, "", ks_s);
            track_scrubby!("scale", r_s, *scale);
        }
        klipp_effects::BuiltinEffect::Dilate { radius } => {
            let mut r_f = *radius as f32;
            let ks = keyframe_state_if(&animatable, clip, effect_index, "radius", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Radius", &mut r_f, 1.0..=10.0, 1.0, 1.0, 0, " px", ks);
            *radius = r_f as u32;
            track_scrubby!("radius", r, r_f);
        }
        klipp_effects::BuiltinEffect::Erode { radius } => {
            let mut r_f = *radius as f32;
            let ks = keyframe_state_if(&animatable, clip, effect_index, "radius", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Radius", &mut r_f, 1.0..=10.0, 1.0, 1.0, 0, " px", ks);
            *radius = r_f as u32;
            track_scrubby!("radius", r, r_f);
        }
        klipp_effects::BuiltinEffect::Denoise { spatial_sigma, color_sigma, iterations } => {
            let ks_sp = keyframe_state_if(&animatable, clip, effect_index, "spatial_sigma", clip_time_secs, keyframe_tolerance);
            let ks_co = keyframe_state_if(&animatable, clip, effect_index, "color_sigma", clip_time_secs, keyframe_tolerance);
            let r_sp = scrubby_param_row(ui, "Spatial Sigma", spatial_sigma, 1.0..=20.0, 5.0, 0.5, 1, "", ks_sp);
            track_scrubby!("spatial_sigma", r_sp, *spatial_sigma);
            let r_co = scrubby_param_row(ui, "Colour Sigma", color_sigma, 0.01..=1.0, 0.1, 0.01, 2, "", ks_co);
            track_scrubby!("color_sigma", r_co, *color_sigma);
            let mut iter_f = *iterations as f32;
            let ks_it = keyframe_state_if(&animatable, clip, effect_index, "iterations", clip_time_secs, keyframe_tolerance);
            let r_it = scrubby_param_row(ui, "Iterations", &mut iter_f, 1.0..=3.0, 1.0, 1.0, 0, "", ks_it);
            *iterations = iter_f as u32;
            track_scrubby!("iterations", r_it, iter_f);
        }
        klipp_effects::BuiltinEffect::Gamma { gamma } => {
            let ks = keyframe_state_if(&animatable, clip, effect_index, "gamma", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Gamma", gamma, 0.1..=5.0, 1.0, 0.05, 2, "", ks);
            track_scrubby!("gamma", r, *gamma);
        }
        klipp_effects::BuiltinEffect::HSVAdjust { hue_shift, saturation, value } => {
            let ks_h = keyframe_state_if(&animatable, clip, effect_index, "hue_shift", clip_time_secs, keyframe_tolerance);
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "saturation", clip_time_secs, keyframe_tolerance);
            let ks_v = keyframe_state_if(&animatable, clip, effect_index, "value", clip_time_secs, keyframe_tolerance);
            let r_h = scrubby_param_row(ui, "Hue Shift", hue_shift, -180.0..=180.0, 0.0, 1.0, 1, "\u{00b0}", ks_h);
            track_scrubby!("hue_shift", r_h, *hue_shift);
            let r_s = scrubby_param_row(ui, "Saturation", saturation, 0.0..=3.0, 1.0, 0.01, 2, "", ks_s);
            track_scrubby!("saturation", r_s, *saturation);
            let r_v = scrubby_param_row(ui, "Value", value, 0.0..=3.0, 1.0, 0.01, 2, "", ks_v);
            track_scrubby!("value", r_v, *value);
        }
        klipp_effects::BuiltinEffect::NoiseGenerator { amount, monochrome, seed } => {
            let ks_a = keyframe_state_if(&animatable, clip, effect_index, "amount", clip_time_secs, keyframe_tolerance);
            let r_a = scrubby_param_row(ui, "Amount", amount, 0.0..=1.0, 0.1, 0.005, 3, "", ks_a);
            track_scrubby!("amount", r_a, *amount);
            let mut mono_f = *monochrome as f32;
            #[cfg(any(test, feature = "row-recorder"))]
            {
                crate::widgets::row_recorder::numeric(ui.next_auto_id(), "slider", "Monochrome", mono_f, &(0.0..=1.0), None, None, None, "", None);
                crate::widgets::row_recorder::parameter("monochrome");
            }
            let r_m = ui.add(egui::Slider::new(&mut mono_f, 0.0..=1.0).step_by(1.0_f64).text("Monochrome"));
            *monochrome = mono_f as u32;
            if r_m.drag_started() { any_drag_started = true; }
            if r_m.drag_stopped() { any_drag_stopped = true; }
            changed |= r_m.changed();
            let mut seed_f = *seed as f32;
            #[cfg(any(test, feature = "row-recorder"))]
            {
                crate::widgets::row_recorder::numeric(ui.next_auto_id(), "slider", "Seed", seed_f, &(0.0..=10000.0), None, None, None, "", None);
                crate::widgets::row_recorder::parameter("seed");
            }
            let r_s = ui.add(egui::Slider::new(&mut seed_f, 0.0..=10000.0).text("Seed"));
            *seed = seed_f as u32;
            if r_s.drag_started() { any_drag_started = true; }
            if r_s.drag_stopped() { any_drag_stopped = true; }
            changed |= r_s.changed();
        }
        klipp_effects::BuiltinEffect::FilmGrain { amount, size, color, shadows, .. } => {
            let ks_a = keyframe_state_if(&animatable, clip, effect_index, "amount", clip_time_secs, keyframe_tolerance);
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "size", clip_time_secs, keyframe_tolerance);
            let ks_h = keyframe_state_if(&animatable, clip, effect_index, "shadows", clip_time_secs, keyframe_tolerance);
            let r_a = scrubby_param_row(ui, "Amount", amount, 0.0..=1.0, 0.3, 0.005, 3, "", ks_a);
            track_scrubby!("amount", r_a, *amount);
            let r_s = scrubby_param_row(ui, "Size", size, 0.5..=6.0, 1.5, 0.02, 2, " px", ks_s);
            track_scrubby!("size", r_s, *size);
            let r_h = scrubby_param_row(ui, "Shadows", shadows, 0.0..=1.0, 0.6, 0.005, 2, "", ks_h);
            track_scrubby!("shadows", r_h, *shadows);
            let mut col = *color != 0;
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "color", "Colour grain", col);
            let r_c = ui.checkbox(&mut col, "Colour grain");
            *color = u32::from(col);
            changed |= r_c.changed();
        }
        klipp_effects::BuiltinEffect::FocusBlur { .. } => {
            // Put on a title layer by the compositor from its roll's focus
            // band; set there, in the Title tab.
            ui.label(egui::RichText::new("Set by the roll's focus band in the Title tab").size(11.0).color(theme.text_dim));
        }
        klipp_effects::BuiltinEffect::TiltShift { center, width, softness, radius } => {
            let ks_c = keyframe_state_if(&animatable, clip, effect_index, "center", clip_time_secs, keyframe_tolerance);
            let ks_w = keyframe_state_if(&animatable, clip, effect_index, "width", clip_time_secs, keyframe_tolerance);
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "softness", clip_time_secs, keyframe_tolerance);
            let ks_r = keyframe_state_if(&animatable, clip, effect_index, "radius", clip_time_secs, keyframe_tolerance);
            let r_c = scrubby_param_row(ui, "Focus", center, 0.0..=1.0, 0.5, 0.005, 2, "", ks_c);
            track_scrubby!("center", r_c, *center);
            let r_w = scrubby_param_row(ui, "Band", width, 0.0..=1.0, 0.3, 0.005, 2, "", ks_w);
            track_scrubby!("width", r_w, *width);
            let r_s = scrubby_param_row(ui, "Softness", softness, 0.01..=0.5, 0.2, 0.005, 2, "", ks_s);
            track_scrubby!("softness", r_s, *softness);
            let r_r = scrubby_param_row(ui, "Blur", radius, 0.0..=60.0, 12.0, 0.2, 1, " px", ks_r);
            track_scrubby!("radius", r_r, *radius);
        }
        klipp_effects::BuiltinEffect::OldFilm { flicker, scratches, dust, weave, sepia, .. } => {
            let ks_f = keyframe_state_if(&animatable, clip, effect_index, "flicker", clip_time_secs, keyframe_tolerance);
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "scratches", clip_time_secs, keyframe_tolerance);
            let ks_d = keyframe_state_if(&animatable, clip, effect_index, "dust", clip_time_secs, keyframe_tolerance);
            let ks_w = keyframe_state_if(&animatable, clip, effect_index, "weave", clip_time_secs, keyframe_tolerance);
            let ks_p = keyframe_state_if(&animatable, clip, effect_index, "sepia", clip_time_secs, keyframe_tolerance);
            let r_f = scrubby_param_row(ui, "Flicker", flicker, 0.0..=1.0, 0.4, 0.005, 2, "", ks_f);
            track_scrubby!("flicker", r_f, *flicker);
            let r_s = scrubby_param_row(ui, "Scratches", scratches, 0.0..=1.0, 0.5, 0.005, 2, "", ks_s);
            track_scrubby!("scratches", r_s, *scratches);
            let r_d = scrubby_param_row(ui, "Dust", dust, 0.0..=1.0, 0.5, 0.005, 2, "", ks_d);
            track_scrubby!("dust", r_d, *dust);
            let r_w = scrubby_param_row(ui, "Gate weave", weave, 0.0..=1.0, 0.5, 0.005, 2, "", ks_w);
            track_scrubby!("weave", r_w, *weave);
            let r_p = scrubby_param_row(ui, "Sepia", sepia, 0.0..=1.0, 0.6, 0.005, 2, "", ks_p);
            track_scrubby!("sepia", r_p, *sepia);
        }
        klipp_effects::BuiltinEffect::Vhs { tracking, bleed, noise, scanlines, .. } => {
            let ks_t = keyframe_state_if(&animatable, clip, effect_index, "tracking", clip_time_secs, keyframe_tolerance);
            let ks_b = keyframe_state_if(&animatable, clip, effect_index, "bleed", clip_time_secs, keyframe_tolerance);
            let ks_n = keyframe_state_if(&animatable, clip, effect_index, "noise", clip_time_secs, keyframe_tolerance);
            let ks_s = keyframe_state_if(&animatable, clip, effect_index, "scanlines", clip_time_secs, keyframe_tolerance);
            let r_t = scrubby_param_row(ui, "Tracking", tracking, 0.0..=1.0, 0.5, 0.005, 2, "", ks_t);
            track_scrubby!("tracking", r_t, *tracking);
            let r_b = scrubby_param_row(ui, "Chroma bleed", bleed, 0.0..=1.0, 0.5, 0.005, 2, "", ks_b);
            track_scrubby!("bleed", r_b, *bleed);
            let r_n = scrubby_param_row(ui, "Noise", noise, 0.0..=1.0, 0.4, 0.005, 2, "", ks_n);
            track_scrubby!("noise", r_n, *noise);
            let r_s = scrubby_param_row(ui, "Scanlines", scanlines, 0.0..=1.0, 0.5, 0.005, 2, "", ks_s);
            track_scrubby!("scanlines", r_s, *scanlines);
        }
        klipp_effects::BuiltinEffect::MedianFilter { radius } => {
            let mut r_f = *radius as f32;
            let r = scrubby_param_row(ui, "Radius", &mut r_f, 1.0..=5.0, 1.0, 1.0, 0, " px", None);
            *radius = r_f as u32;
            track_scrubby!("radius", r, r_f);
        }
        klipp_effects::BuiltinEffect::RadialBlur { center_x, center_y, amount, samples, mode } => {
            let ks_cx = keyframe_state_if(&animatable, clip, effect_index, "center_x", clip_time_secs, keyframe_tolerance);
            let ks_cy = keyframe_state_if(&animatable, clip, effect_index, "center_y", clip_time_secs, keyframe_tolerance);
            let ks_a = keyframe_state_if(&animatable, clip, effect_index, "amount", clip_time_secs, keyframe_tolerance);
            let r_cx = scrubby_param_row(ui, "Center X", center_x, 0.0..=1.0, 0.5, 0.005, 3, "", ks_cx);
            track_scrubby!("center_x", r_cx, *center_x);
            let r_cy = scrubby_param_row(ui, "Center Y", center_y, 0.0..=1.0, 0.5, 0.005, 3, "", ks_cy);
            track_scrubby!("center_y", r_cy, *center_y);
            let r_a = scrubby_param_row(ui, "Amount", amount, 0.0..=50.0, 10.0, 0.5, 1, "", ks_a);
            track_scrubby!("amount", r_a, *amount);
            let mut s_f = *samples as f32;
            #[cfg(any(test, feature = "row-recorder"))]
            {
                crate::widgets::row_recorder::numeric(ui.next_auto_id(), "slider", "Samples", s_f, &(4.0..=32.0), None, None, None, "", None);
                crate::widgets::row_recorder::parameter("samples");
            }
            let r_s = ui.add(egui::Slider::new(&mut s_f, 4.0..=32.0).text("Samples"));
            *samples = s_f as u32;
            if r_s.drag_started() { any_drag_started = true; }
            if r_s.drag_stopped() { any_drag_stopped = true; }
            changed |= r_s.changed();
            // Mode dropdown
            let mode_labels = ["Zoom", "Spin"];
            let mode_sel = (*mode as usize).min(mode_labels.len() - 1);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Mode").size(10.0));
                let combo_id = ui.make_persistent_id(format!("rb_mode_{clip_id:?}_{effect_index}"));
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(combo_id, "choice", "mode", "Mode", *mode);
                egui::ComboBox::from_id_salt(combo_id)
                    .selected_text(mode_labels[mode_sel])
                    .show_ui(ui, |ui| {
                        for (i, label) in mode_labels.iter().enumerate() {
                            if ui.selectable_value(mode, i as u32, *label).changed() {
                                changed = true;
                            }
                        }
                    });
            });
        }
        klipp_effects::BuiltinEffect::Levels { in_black, in_white, gamma, out_black, out_white } => {
            let ks_ib = keyframe_state_if(&animatable, clip, effect_index, "in_black", clip_time_secs, keyframe_tolerance);
            let ks_iw = keyframe_state_if(&animatable, clip, effect_index, "in_white", clip_time_secs, keyframe_tolerance);
            let ks_g = keyframe_state_if(&animatable, clip, effect_index, "gamma", clip_time_secs, keyframe_tolerance);
            let ks_ob = keyframe_state_if(&animatable, clip, effect_index, "out_black", clip_time_secs, keyframe_tolerance);
            let ks_ow = keyframe_state_if(&animatable, clip, effect_index, "out_white", clip_time_secs, keyframe_tolerance);
            let r_ib = scrubby_param_row(ui, "In Black", in_black, 0.0..=1.0, 0.0, 0.005, 3, "", ks_ib);
            track_scrubby!("in_black", r_ib, *in_black);
            let r_iw = scrubby_param_row(ui, "In White", in_white, 0.0..=1.0, 1.0, 0.005, 3, "", ks_iw);
            track_scrubby!("in_white", r_iw, *in_white);
            let r_g = scrubby_param_row(ui, "Gamma", gamma, 0.1..=5.0, 1.0, 0.05, 2, "", ks_g);
            track_scrubby!("gamma", r_g, *gamma);
            let r_ob = scrubby_param_row(ui, "Out Black", out_black, 0.0..=1.0, 0.0, 0.005, 3, "", ks_ob);
            track_scrubby!("out_black", r_ob, *out_black);
            let r_ow = scrubby_param_row(ui, "Out White", out_white, 0.0..=1.0, 1.0, 0.005, 3, "", ks_ow);
            track_scrubby!("out_white", r_ow, *out_white);
        }
        klipp_effects::BuiltinEffect::Equalize { strength } => {
            let ks = keyframe_state_if(&animatable, clip, effect_index, "strength", clip_time_secs, keyframe_tolerance);
            let r = scrubby_param_row(ui, "Strength", strength, 0.0..=1.0, 1.0, 0.005, 2, "", ks);
            track_scrubby!("strength", r, *strength);
        }
        klipp_effects::BuiltinEffect::Letterbox { aspect_ratio, bar_color, opacity } => {
            let ks_ar = keyframe_state_if(&animatable, clip, effect_index, "aspect_ratio", clip_time_secs, keyframe_tolerance);
            let ks_op = keyframe_state_if(&animatable, clip, effect_index, "opacity", clip_time_secs, keyframe_tolerance);
            let r_ar = scrubby_param_row(ui, "Aspect Ratio", aspect_ratio, 0.5..=4.0, 2.39, 0.01, 2, ":1", ks_ar);
            track_scrubby!("aspect_ratio", r_ar, *aspect_ratio);
            // Bar color toggle
            let mut bc_f = *bar_color as f32;
            #[cfg(any(test, feature = "row-recorder"))]
            {
                crate::widgets::row_recorder::numeric(ui.next_auto_id(), "slider", "Bar Colour (0=Black, 1=White)", bc_f, &(0.0..=1.0), None, None, None, "", None);
                crate::widgets::row_recorder::parameter("bar_color");
            }
            let r_bc = ui.add(egui::Slider::new(&mut bc_f, 0.0..=1.0).step_by(1.0_f64).text("Bar Colour (0=Black, 1=White)"));
            *bar_color = bc_f as u32;
            if r_bc.drag_started() { any_drag_started = true; }
            if r_bc.drag_stopped() { any_drag_stopped = true; }
            changed |= r_bc.changed();
            let r_op = scrubby_param_row(ui, "Opacity", opacity, 0.0..=1.0, 1.0, 0.005, 2, "", ks_op);
            track_scrubby!("opacity", r_op, *opacity);
        }
        klipp_effects::BuiltinEffect::ChromaticAberration { red_shift, blue_shift, center_x, center_y } => {
            let ks_rs = keyframe_state_if(&animatable, clip, effect_index, "red_shift", clip_time_secs, keyframe_tolerance);
            let ks_bs = keyframe_state_if(&animatable, clip, effect_index, "blue_shift", clip_time_secs, keyframe_tolerance);
            let ks_cx = keyframe_state_if(&animatable, clip, effect_index, "center_x", clip_time_secs, keyframe_tolerance);
            let ks_cy = keyframe_state_if(&animatable, clip, effect_index, "center_y", clip_time_secs, keyframe_tolerance);
            let r_rs = scrubby_param_row(ui, "Red Shift", red_shift, -20.0..=20.0, 2.0, 0.1, 1, " px", ks_rs);
            track_scrubby!("red_shift", r_rs, *red_shift);
            let r_bs = scrubby_param_row(ui, "Blue Shift", blue_shift, -20.0..=20.0, -2.0, 0.1, 1, " px", ks_bs);
            track_scrubby!("blue_shift", r_bs, *blue_shift);
            let r_cx = scrubby_param_row(ui, "Center X", center_x, 0.0..=1.0, 0.5, 0.005, 3, "", ks_cx);
            track_scrubby!("center_x", r_cx, *center_x);
            let r_cy = scrubby_param_row(ui, "Center Y", center_y, 0.0..=1.0, 0.5, 0.005, 3, "", ks_cy);
            track_scrubby!("center_y", r_cy, *center_y);
        }
        klipp_effects::BuiltinEffect::Frei0r { type_id, params } => {
            // Resolution-warning banner: surfaces above the params when
            // the project frame size doesn't fit frei0r's spec
            // constraints for THIS plugin (8-aligned, ≤2048 unless
            // tilable). Temporal plugins (delay0r, baltan) refuse 4K
            // even with tiling on. Pulled from the live registry so a
            // missing-plugin clip falls through quietly.
            let plugin_info = klipp_effects::frei0r_effect::lookup(type_id);
            let resolution = state.sequence().resolution;
            let project_w = resolution.width;
            let project_h = resolution.height;
            if let Some(ref info) = plugin_info {
                if !klipp_effects::frei0r_effect::supports_resolution(
                    &info.name,
                    project_w,
                    project_h,
                ) {
                    let why = if !project_w.is_multiple_of(8) || !project_h.is_multiple_of(8) {
                        format!(
                            "frei0r requires 8-aligned dimensions; project is {}×{}",
                            project_w, project_h
                        )
                    } else {
                        format!(
                            "{} doesn't support {}×{} (frei0r {} can't tile temporal effects above 2048)",
                            info.name, project_w, project_h, info.name
                        )
                    };
                    ui.colored_label(theme.marker_warning, format!("⚠ {why}"));
                    ui.add_space(4.0);
                }
            } else {
                ui.colored_label(
                    theme.marker_warning,
                    format!("⚠ Plugin not installed: {}", type_id),
                );
                ui.add_space(4.0);
            }

            // Per-param widgets — typed by the plugin's declared kind.
            // We read declared param info from the registry; if the
            // plugin disappeared, fall through to passthrough (no
            // widgets).
            let Some(plugin_info) = plugin_info else {
                return if changed { Some(updated) } else { None };
            };

            for (i, param_info) in plugin_info.params.iter().enumerate() {
                // Lazily extend params Vec if a plugin grew between
                // saves (defensive — plugins shouldn't change shape
                // across versions, but covers the case cleanly).
                while params.len() <= i {
                    params.push(klipp_frei0r::Frei0rValue::default_for(
                        plugin_info.params[params.len()].kind,
                    ));
                    changed = true;
                }
                let value = &mut params[i];
                ui.horizontal(|ui| {
                    ui.label(&param_info.name);
                    match (param_info.kind, value) {
                        (klipp_frei0r::ParamKind::Bool, klipp_frei0r::Frei0rValue::Bool(b)) => {
                            if ui.checkbox(b, "").changed() {
                                changed = true;
                            }
                        }
                        (
                            klipp_frei0r::ParamKind::Double,
                            klipp_frei0r::Frei0rValue::Double(d),
                        ) => {
                            // Spec: f0r_param_double range is [0, 1].
                            if ui.add(egui::Slider::new(d, 0.0..=1.0)).changed() {
                                changed = true;
                            }
                        }
                        (
                            klipp_frei0r::ParamKind::Color,
                            klipp_frei0r::Frei0rValue::Color(rgb),
                        ) => {
                            let mut color = [
                                (rgb[0].clamp(0.0, 1.0) * 255.0) as u8,
                                (rgb[1].clamp(0.0, 1.0) * 255.0) as u8,
                                (rgb[2].clamp(0.0, 1.0) * 255.0) as u8,
                            ];
                            if ui.color_edit_button_srgb(&mut color).changed() {
                                rgb[0] = color[0] as f32 / 255.0;
                                rgb[1] = color[1] as f32 / 255.0;
                                rgb[2] = color[2] as f32 / 255.0;
                                changed = true;
                            }
                        }
                        (
                            klipp_frei0r::ParamKind::Position,
                            klipp_frei0r::Frei0rValue::Position(xy),
                        ) => {
                            ui.label("X");
                            if ui
                                .add(egui::Slider::new(&mut xy[0], 0.0..=1.0))
                                .changed()
                            {
                                changed = true;
                            }
                            ui.label("Y");
                            if ui
                                .add(egui::Slider::new(&mut xy[1], 0.0..=1.0))
                                .changed()
                            {
                                changed = true;
                            }
                        }
                        (
                            klipp_frei0r::ParamKind::String,
                            klipp_frei0r::Frei0rValue::String(s),
                        ) => {
                            if ui.text_edit_singleline(s).changed() {
                                changed = true;
                            }
                        }
                        _ => {
                            // Type/value mismatch — render a clear
                            // disabled hint instead of silently
                            // ignoring the row.
                            ui.colored_label(theme.text_dim, "(unsupported param type)");
                        }
                    }
                });
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
        if param_name.is_empty() || !animatable.contains(param_name) {
            continue; // Transform in the effects tab, or not animated by the render
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
            let has_at_playhead = clip.effect_keyframes().get(&key)
                .is_some_and(|kfs| crate::inspector::keyframe_selection::nearest(kfs, clip_time_secs, kf_tolerance).is_some());
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
            let ks = effect_keyframe_state(clip, effect_index, param_name, clip_time_secs, keyframe_tolerance);
            if let Some(t) = ks.prev_keyframe_time {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
            }
        }

        // Navigate to next keyframe
        if resp.keyframe_nav_next {
            let ks = effect_keyframe_state(clip, effect_index, param_name, clip_time_secs, keyframe_tolerance);
            if let Some(t) = ks.next_keyframe_time {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
            }
        }
    }

    if changed { Some(updated) } else { None }
}
