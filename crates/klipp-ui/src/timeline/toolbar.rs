use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::tooltip::styled_tooltip;
use klipp_core::AppCommand;

/// 34px timeline toolbar: edit tools, snap/skimming/marker toggles, role dots, zoom slider.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, cmds: &mut Vec<AppCommand>) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add_space(8.0);

        // ── Edit tools (26px, painted icons) ──
        #[allow(clippy::type_complexity)]
        let tool_defs: &[(klipp_core::Tool, &str, &str, fn(&egui::Painter, egui::Rect, egui::Color32))] = &[
            (klipp_core::Tool::Select, "Select Tool", "V", draw_tl_select_icon),
            (klipp_core::Tool::Blade, "Blade Tool", "B", draw_tl_blade_icon),
            (klipp_core::Tool::Select, "Trim Tool", "T", draw_tl_trim_icon),
            (klipp_core::Tool::Range, "Range Tool", "R", draw_tl_range_icon),
        ];
        for (tool, tip, key, draw_fn) in tool_defs {
            let active = state.active_tool == *tool;
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(26.0, 26.0),
                egui::Sense::click(),
            );
            let (bg, fg) = if active {
                (theme.accent, theme.bg_void)
            } else if resp.hovered() {
                (theme.bg_hover, theme.text_secondary)
            } else {
                (egui::Color32::TRANSPARENT, theme.text_dim)
            };
            ui.painter().rect_filled(rect, 5.0, bg);
            draw_fn(ui.painter(), rect, fg);
            styled_tooltip(&resp, theme, tip, Some(key));
            if resp.clicked() {
                state.active_tool = *tool;
            }
        }

        separator(ui, theme);

        // ── Snap toggle ──
        let snap_active = state.snap_enabled;
        let (snap_bg, snap_fg) = if snap_active {
            (theme.accent, theme.bg_void)
        } else {
            (egui::Color32::TRANSPARENT, theme.text_dim)
        };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::click());
        let bg = if resp.hovered() && !snap_active { theme.bg_hover } else { snap_bg };
        ui.painter().rect_filled(rect, 5.0, bg);
        draw_tl_snap_icon(ui.painter(), rect, snap_fg);
        styled_tooltip(&resp, theme, "Snap to Clips", Some("N"));
        if resp.clicked() { state.snap_enabled = !state.snap_enabled; }

        // ── Skimming toggle ──
        let skim_active = state.skimming_enabled;
        let (skim_bg, skim_fg) = if skim_active {
            (theme.accent, theme.bg_void)
        } else {
            (egui::Color32::TRANSPARENT, theme.text_dim)
        };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::click());
        let bg = if resp.hovered() && !skim_active { theme.bg_hover } else { skim_bg };
        ui.painter().rect_filled(rect, 5.0, bg);
        draw_tl_skim_icon(ui.painter(), rect, skim_fg);
        styled_tooltip(&resp, theme, "Skimming", Some("S"));
        if resp.clicked() {
            state.skimming_enabled = !state.skimming_enabled;
            let msg = if state.skimming_enabled { "Skimming On" } else { "Skimming Off" };
            state.toast.show(msg);
        }

        // ── Add Marker ──
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::click());
        let bg = if resp.hovered() { theme.bg_hover } else { egui::Color32::TRANSPARENT };
        ui.painter().rect_filled(rect, 5.0, bg);
        draw_tl_marker_icon(ui.painter(), rect, theme.text_dim);
        styled_tooltip(&resp, theme, "Add Marker", Some("M"));

        separator(ui, theme);

        // ── Role visibility dots ──
        let roles = [
            (theme.role_video, "Video role", true),
            (theme.role_dialogue, "Dialogue role", true),
            (theme.role_music, "Music role", true),
            (theme.role_sfx, "SFX role", true),
            (theme.role_broll, "B-Roll role", false),
        ];
        for (color, tip, visible) in &roles {
            let alpha = if *visible { 255u8 } else { 128 };
            let dot_color = egui::Color32::from_rgba_premultiplied(
                color.r(), color.g(), color.b(), alpha,
            );
            let (dot_rect, dot_resp) =
                ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::click());
            ui.painter().circle_filled(dot_rect.center(), 3.5, dot_color);
            if !visible {
                ui.painter().circle_stroke(dot_rect.center(), 3.5, egui::Stroke::new(1.0, *color));
            }
            styled_tooltip(&dot_resp, theme, tip, None);
        }

        // ── Zoom slider + Fit button (right-aligned) ──
        // Items in `right_to_left` are added from the RIGHT moving LEFT, so the
        // FIRST add becomes the rightmost on screen. Final visual order:
        //     [−] [slider] [+] [Fit]
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);

            // Fit button — first add → rightmost position on screen.
            let fit_resp = ui.add(
                egui::Button::new(
                    egui::RichText::new("Fit").size(9.0).color(theme.text_secondary),
                )
                .min_size(egui::vec2(26.0, 16.0))
                .rounding(3.0)
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(0.5, theme.border)),
            );
            styled_tooltip(&fit_resp, theme, "Fit timeline to window", Some("Shift+Z"));
            if fit_resp.clicked() {
                cmds.push(AppCommand::ZoomTimelineToFit);
            }
            ui.add_space(4.0);

            ui.label(egui::RichText::new("+").size(9.0).color(theme.text_dim));
            let mut zoom_t = ((state.timeline_zoom.ln() - 0.1f32.ln())
                / (2000.0f32.ln() - 0.1f32.ln())).clamp(0.0, 1.0);
            let slider = egui::Slider::new(&mut zoom_t, 0.0..=1.0).show_value(false);
            let resp = ui.add_sized([70.0, 16.0], slider);
            if resp.changed() {
                let new_zoom = (0.1f32.ln() + zoom_t * (2000.0f32.ln() - 0.1f32.ln())).exp();
                cmds.push(AppCommand::SetTimelineZoom { zoom: new_zoom, scroll_x: None });
            }
            ui.label(egui::RichText::new("\u{2212}").size(9.0).color(theme.text_dim));
        });
    });
}

fn separator(ui: &mut egui::Ui, theme: &KlippTheme) {
    ui.add_space(2.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 16.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme.border_light);
    ui.add_space(2.0);
}

// ── Painted icons for timeline toolbar (13x13 icon area inside 26x26 button) ──

fn icon_area(btn: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(btn.center(), egui::vec2(13.0, 13.0))
}

fn draw_tl_select_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    // Arrow cursor triangle
    let pts = vec![
        egui::pos2(ir.min.x + 1.0, ir.min.y),
        egui::pos2(ir.min.x + 1.0, ir.max.y - 1.0),
        egui::pos2(ir.min.x + 5.0, ir.max.y - 4.0),
        egui::pos2(ir.min.x + 10.0, ir.center().y + 2.0),
    ];
    painter.add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
}

fn draw_tl_blade_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3, c);
    // Diagonal razor line
    painter.line_segment([egui::pos2(ir.min.x + 1.0, ir.max.y - 1.0), egui::pos2(ir.max.x - 1.0, ir.min.y + 1.0)], s);
    // Small triangle tip
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(ir.max.x - 1.0, ir.min.y + 1.0),
        egui::pos2(ir.max.x - 4.0, ir.min.y + 2.0),
        egui::pos2(ir.max.x - 2.0, ir.min.y + 4.0),
    ], c, egui::Stroke::NONE));
}

fn draw_tl_trim_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3, c);
    // Two vertical bars with inward ticks
    painter.line_segment([egui::pos2(ir.min.x + 2.0, ir.min.y + 1.0), egui::pos2(ir.min.x + 2.0, ir.max.y - 1.0)], s);
    painter.line_segment([egui::pos2(ir.max.x - 2.0, ir.min.y + 1.0), egui::pos2(ir.max.x - 2.0, ir.max.y - 1.0)], s);
    painter.line_segment([egui::pos2(ir.min.x + 2.0, ir.center().y), egui::pos2(ir.min.x + 5.0, ir.center().y)], s);
    painter.line_segment([egui::pos2(ir.max.x - 5.0, ir.center().y), egui::pos2(ir.max.x - 2.0, ir.center().y)], s);
}

fn draw_tl_range_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3, c);
    // Left bracket bar
    painter.line_segment([egui::pos2(ir.min.x + 2.0, ir.min.y + 1.0), egui::pos2(ir.min.x + 2.0, ir.max.y - 1.0)], s);
    // Right bracket bar
    painter.line_segment([egui::pos2(ir.max.x - 2.0, ir.min.y + 1.0), egui::pos2(ir.max.x - 2.0, ir.max.y - 1.0)], s);
    // Horizontal connecting line
    painter.line_segment([egui::pos2(ir.min.x + 3.0, ir.center().y), egui::pos2(ir.max.x - 3.0, ir.center().y)], s);
    // Subtle fill
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(ir.min.x + 2.0, ir.min.y + 3.0),
            egui::pos2(ir.max.x - 2.0, ir.max.y - 3.0),
        ),
        0.0,
        egui::Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), 30),
    );
}

fn draw_tl_snap_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3, c);
    // Crosshair / magnet shape
    let cx = ir.center().x;
    let cy = ir.center().y;
    painter.line_segment([egui::pos2(cx, ir.min.y + 1.0), egui::pos2(cx, ir.max.y - 1.0)], s);
    painter.line_segment([egui::pos2(ir.min.x + 1.0, cy), egui::pos2(ir.min.x + 4.0, cy)], s);
    painter.line_segment([egui::pos2(ir.max.x - 4.0, cy), egui::pos2(ir.max.x - 1.0, cy)], s);
    painter.circle_stroke(egui::pos2(cx, cy), 3.0, s);
}

fn draw_tl_skim_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3, c);
    let cx = ir.center().x;
    // Vertical line with upward arrow
    painter.line_segment([egui::pos2(cx, ir.min.y + 4.0), egui::pos2(cx, ir.max.y - 1.0)], s);
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(cx, ir.min.y + 1.0),
        egui::pos2(cx - 3.0, ir.min.y + 5.0),
        egui::pos2(cx + 3.0, ir.min.y + 5.0),
    ], c, egui::Stroke::NONE));
}

fn draw_tl_marker_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let cx = ir.center().x;
    // Downward triangle (flag) + vertical line
    painter.line_segment(
        [egui::pos2(cx, ir.min.y + 1.0), egui::pos2(cx, ir.max.y - 1.0)],
        egui::Stroke::new(1.3, c),
    );
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(cx - 3.5, ir.min.y + 1.0),
        egui::pos2(cx + 3.5, ir.min.y + 1.0),
        egui::pos2(cx, ir.min.y + 5.0),
    ], c, egui::Stroke::NONE));
}
