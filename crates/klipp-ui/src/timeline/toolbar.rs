use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::tooltip::styled_tooltip;
use klipp_core::AppCommand;
use klipp_timeline::magnetic::EditMode;

/// The timeline's own toolbar: everything that changes how an edit on
/// the timeline behaves lives here and nowhere else. Left to right:
/// the edit tools, the insert / overwrite mode, snap and skimming, the
/// marker button; on the right the zoom.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, cmds: &mut Vec<AppCommand>) {
    let btn = crate::theme::control::REGULAR;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add_space(8.0);

        // ── Edit tools ──
        #[allow(clippy::type_complexity)]
        let tool_defs: &[(klipp_core::Tool, &str, &str, fn(&egui::Painter, egui::Rect, egui::Color32))] = &[
            (klipp_core::Tool::Select, "Select Tool", "V", draw_tl_select_icon),
            (klipp_core::Tool::Trim, "Trim Tool", "T", draw_tl_trim_icon),
            (klipp_core::Tool::Blade, "Blade Tool", "B", draw_tl_blade_icon),
            (klipp_core::Tool::Range, "Range Tool", "R", draw_tl_range_icon),
        ];
        for (tool, tip, key, draw_fn) in tool_defs {
            let active = state.active_tool == *tool;
            let resp = icon_button(ui, theme, btn, active, *draw_fn);
            styled_tooltip(&resp, theme, tip, Some(key));
            if resp.clicked() {
                cmds.push(AppCommand::SetActiveTool(*tool));
            }
        }

        separator(ui, theme);

        // ── Insert / Overwrite: a two-segment switch, the active mode lit ──
        {
            let active = if state.edit_mode == EditMode::Insert { 0 } else { 1 };
            let before = ui.cursor().min;
            if let Some(i) = crate::widgets::segmented::segmented(
                ui, &["INS", "OVR"], active, theme,
                egui::FontId::monospace(crate::theme::type_scale::CAPTION), crate::theme::control::COMPACT,
            ) {
                state.edit_mode = if i == 0 { EditMode::Insert } else { EditMode::Overwrite };
            }
            let tip = match state.edit_mode {
                EditMode::Insert => "Edit Mode: Insert",
                EditMode::Overwrite => "Edit Mode: Overwrite",
            };
            let rect = egui::Rect::from_min_max(before, ui.cursor().min + egui::vec2(0.0, crate::theme::control::COMPACT));
            let hover = ui.interact(rect, ui.id().with("edit_mode_tip"), egui::Sense::hover());
            styled_tooltip(&hover, theme, tip, Some("W"));
        }

        separator(ui, theme);

        // ── Snap (left-click = master; right-click = per-target menu) ──
        // Dispatches through AppCommand so the central handler can
        // persist state.snap → layout.snap on every flip (and so the
        // four snap-toggle surfaces — toolbar, N shortcut, palette,
        // View menu — stay in sync without parallel mutation paths).
        let snap_active = state.snap.master;
        let resp = icon_button(ui, theme, btn, snap_active, draw_tl_snap_icon);
        // A small chevron in the corner hints at the per-target menu.
        let chevron = if snap_active { theme.accent } else { theme.text_dim };
        ui.painter().text(
            egui::pos2(resp.rect.right() - 4.0, resp.rect.bottom() - 6.0),
            egui::Align2::RIGHT_BOTTOM,
            "\u{25BE}",
            egui::FontId::proportional(10.0),
            chevron,
        );
        styled_tooltip(&resp, theme, "Snap (right-click for per-target options)", Some("N"));
        if resp.clicked() {
            cmds.push(AppCommand::SetSnapMaster(!state.snap.master));
        }
        resp.context_menu(|ui| {
            use klipp_core::SnapTargetKind;
            let cfg = state.snap;
            ui.set_min_width(180.0);
            let master_label = if cfg.master { "\u{2714} Snap (master)" } else { "    Snap (master)" };
            if ui.button(master_label).clicked() {
                cmds.push(AppCommand::SetSnapMaster(!cfg.master));
                ui.close_menu();
            }
            ui.separator();
            ui.add_enabled_ui(cfg.master, |ui| {
                let rows: &[(SnapTargetKind, &str, bool)] = &[
                    (SnapTargetKind::ClipEdge, "Snap to clips", cfg.clips),
                    (SnapTargetKind::Playhead, "Snap to playhead", cfg.playhead),
                    (SnapTargetKind::Marker, "Snap to markers", cfg.markers),
                    (SnapTargetKind::InPoint, "Snap to in/out", cfg.in_out),
                    (SnapTargetKind::Grid, "Snap to grid", cfg.grid),
                ];
                for &(target, label, enabled) in rows {
                    let row_label = if enabled { format!("\u{2714} {label}") } else { format!("    {label}") };
                    if ui.button(row_label).clicked() {
                        cmds.push(AppCommand::SetSnapTarget { target, enabled: !enabled });
                        ui.close_menu();
                    }
                }
            });
        });

        // ── Skimming ──
        let resp = icon_button(ui, theme, btn, state.skimming_enabled, draw_tl_skim_icon);
        styled_tooltip(&resp, theme, "Skimming", Some("S"));
        if resp.clicked() {
            state.skimming_enabled = !state.skimming_enabled;
            let msg = if state.skimming_enabled { "Skimming On" } else { "Skimming Off" };
            state.toast.show(msg);
        }

        separator(ui, theme);

        // ── Add marker ──
        let resp = icon_button(ui, theme, btn, false, draw_tl_marker_icon);
        styled_tooltip(&resp, theme, "Add Marker", Some("M"));
        if resp.clicked() {
            cmds.push(AppCommand::AddMarker {
                position: state.playhead,
                label: String::new(),
                color: klipp_core::MarkerColor::Blue,
            });
        }

        // ── Zoom (right-aligned): [−] [slider] [+] [Fit] ──
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            let fit_resp = ui.add(egui::Button::new("Fit").min_size(egui::vec2(36.0, crate::theme::control::COMPACT)));
            styled_tooltip(&fit_resp, theme, "Fit timeline to window", Some("Shift+Z"));
            if fit_resp.clicked() {
                cmds.push(AppCommand::ZoomTimelineToFit);
            }
            ui.add_space(4.0);

            ui.label(egui::RichText::new("+").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
            let mut zoom_t = ((state.timeline_zoom.ln() - 0.1f32.ln())
                / (2000.0f32.ln() - 0.1f32.ln())).clamp(0.0, 1.0);
            let slider = egui::Slider::new(&mut zoom_t, 0.0..=1.0).show_value(false);
            let resp = ui.add_sized([90.0, 16.0], slider);
            if resp.changed() {
                let new_zoom = (0.1f32.ln() + zoom_t * (2000.0f32.ln() - 0.1f32.ln())).exp();
                cmds.push(AppCommand::SetTimelineZoom { zoom: new_zoom, scroll_x: None });
            }
            ui.label(egui::RichText::new("\u{2212}").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
        });
    });
}

/// A square glyph button: a tinted field and the accent on the glyph
/// when active, the hover tone under the pointer, nothing otherwise.
fn icon_button(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    size: f32,
    active: bool,
    draw: impl FnOnce(&egui::Painter, egui::Rect, egui::Color32),
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let (bg, fg) = if active {
        (theme.accent_subtle, theme.accent)
    } else if resp.hovered() {
        (theme.bg_hover, theme.text_primary)
    } else {
        (egui::Color32::TRANSPARENT, theme.text_secondary)
    };
    ui.painter().rect_filled(rect, crate::theme::radius::CONTROL, bg);
    draw(ui.painter(), rect, fg);
    resp
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
    let s = egui::Stroke::new(1.3_f32, c);
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
    let s = egui::Stroke::new(1.3_f32, c);
    // Two vertical bars with inward ticks
    painter.line_segment([egui::pos2(ir.min.x + 2.0, ir.min.y + 1.0), egui::pos2(ir.min.x + 2.0, ir.max.y - 1.0)], s);
    painter.line_segment([egui::pos2(ir.max.x - 2.0, ir.min.y + 1.0), egui::pos2(ir.max.x - 2.0, ir.max.y - 1.0)], s);
    painter.line_segment([egui::pos2(ir.min.x + 2.0, ir.center().y), egui::pos2(ir.min.x + 5.0, ir.center().y)], s);
    painter.line_segment([egui::pos2(ir.max.x - 5.0, ir.center().y), egui::pos2(ir.max.x - 2.0, ir.center().y)], s);
}

fn draw_tl_range_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3_f32, c);
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
    let s = egui::Stroke::new(1.3_f32, c);
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
    let s = egui::Stroke::new(1.3_f32, c);
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
        egui::Stroke::new(1.3_f32, c),
    );
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(cx - 3.5, ir.min.y + 1.0),
        egui::pos2(cx + 3.5, ir.min.y + 1.0),
        egui::pos2(cx, ir.min.y + 5.0),
    ], c, egui::Stroke::NONE));
}
