//! Cache Manager modal dialog (feature #78).
//!
//! Mirrors Kdenlive's cache manager: pie chart showing category proportions
//! on the left, per-category size + file count + clear button on the right,
//! footer with Refresh / Clear All / Close.
//!
//! Categories and their on-disk paths come from `crate::cache_manager::
//! scan_cache_categories`. The dialog is purely a view over
//! `AppState::cache_categories` — dispatch `RefreshCacheManager` to re-scan.
//!
//! Arc drawing: egui has no native arc primitive, so each pie slice is an
//! N-step fan (triangle strip expressed as a `Shape::convex_polygon` with
//! one centre vertex + points sampled along the arc).

use klipp_core::AppCommand;

const PIE_STEPS_PER_SLICE: usize = 32;
const PIE_DIAMETER: f32 = 160.0;
const LEFT_COL_WIDTH: f32 = 200.0;

pub fn show_cache_manager_dialog(
    ctx: &egui::Context,
    state: &mut crate::state::AppState,
    _prefs: &crate::prefs::Preferences,
) -> Vec<AppCommand> {
    let mut cmds: Vec<AppCommand> = Vec::new();
    if !state.show_cache_manager {
        return cmds;
    }

    let mut open = state.show_cache_manager;
    let mut close_requested = false;

    egui::Window::new("Cache Manager")
        .collapsible(false)
        .resizable(false)
        .default_size([560.0, 420.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // ── Body: pie on left, list on right ──
            ui.horizontal_top(|ui| {
                // LEFT — pie + total
                ui.allocate_ui_with_layout(
                    egui::vec2(LEFT_COL_WIDTH, 320.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        draw_pie_chart(ui, &state.cache_categories);
                        ui.add_space(8.0);
                        let total: u64 = state
                            .cache_categories
                            .iter()
                            .map(|c| c.size_bytes)
                            .sum();
                        ui.label(
                            egui::RichText::new(format!(
                                "Total: {}",
                                crate::cache_manager::format_cache_size(total)
                            ))
                            .strong(),
                        );
                    },
                );

                ui.add_space(12.0);

                // RIGHT — category list
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for cat in state.cache_categories.clone() {
                                render_category_row(ui, &cat, &mut cmds);
                                ui.separator();
                            }
                            if state.cache_categories.is_empty() {
                                ui.label(
                                    egui::RichText::new(
                                        "No cache data — click Refresh.",
                                    )
                                    .italics()
                                    .weak(),
                                );
                            }
                        });
                });
            });

            ui.add_space(4.0);
            ui.separator();

            // ── Footer ──
            ui.horizontal(|ui| {
                if ui.button("Refresh").clicked() {
                    cmds.push(AppCommand::RefreshCacheManager);
                }
                if ui.button("Clear All").clicked() {
                    for cat in &state.cache_categories {
                        if cat.size_bytes > 0 {
                            cmds.push(AppCommand::ClearCacheCategory {
                                path: cat.path.clone(),
                            });
                        }
                    }
                }
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui.button("Close").clicked() {
                            close_requested = true;
                        }
                    },
                );
            });
        });

    if !open || close_requested {
        state.show_cache_manager = false;
    }
    cmds
}

fn render_category_row(
    ui: &mut egui::Ui,
    cat: &crate::cache_manager::CacheCategory,
    cmds: &mut Vec<AppCommand>,
) {
    ui.horizontal(|ui| {
        // Colour swatch
        let (swatch_rect, _) =
            ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
        ui.painter().rect_filled(swatch_rect, 3.0, cat.color);
        ui.painter().rect_stroke(
            swatch_rect,
            3.0,
            egui::Stroke::new(
                1.0,
                egui::Color32::from_black_alpha(60),
            ),
        );
        ui.add_space(6.0);

        // Name + size/count stacked
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(cat.name).strong());
            let subtitle = if cat.size_bytes == 0 {
                "Empty".to_string()
            } else {
                format!(
                    "{} \u{00B7} {} files",
                    crate::cache_manager::format_cache_size(cat.size_bytes),
                    cat.file_count
                )
            };
            ui.label(
                egui::RichText::new(subtitle)
                    .small()
                    .color(egui::Color32::from_gray(170)),
            );
        });

        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let btn = egui::Button::new("Clear");
                let resp = if cat.size_bytes > 0 {
                    ui.add(btn)
                } else {
                    ui.add_enabled(false, btn)
                };
                if resp.clicked() {
                    cmds.push(AppCommand::ClearCacheCategory {
                        path: cat.path.clone(),
                    });
                }
            },
        );
    });
}

/// Draws a pie chart allocated into a square 160×160 region. If the total
/// size across categories is zero, draws a grey placeholder circle labelled
/// "No cache". Otherwise draws one slice per non-empty category, coloured
/// by `CacheCategory::color`.
fn draw_pie_chart(
    ui: &mut egui::Ui,
    cats: &[crate::cache_manager::CacheCategory],
) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(PIE_DIAMETER, PIE_DIAMETER),
        egui::Sense::hover(),
    );
    let centre = rect.center();
    let radius = PIE_DIAMETER * 0.5 - 4.0;
    let bg = egui::Color32::from_gray(58);
    let stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(30));

    let total: u64 = cats.iter().map(|c| c.size_bytes).sum();
    if total == 0 {
        // Placeholder — full grey circle + centre label.
        let pts = circle_points(centre, radius, 64);
        ui.painter().add(egui::Shape::convex_polygon(pts, bg, stroke));
        ui.painter().text(
            centre,
            egui::Align2::CENTER_CENTER,
            "No cache",
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(190),
        );
        return;
    }

    // Draw slices for each non-empty category, starting from -PI/2 (top of
    // the circle) and sweeping clockwise for natural "12 o'clock start".
    let mut start_angle = -std::f32::consts::FRAC_PI_2;
    for cat in cats {
        if cat.size_bytes == 0 {
            continue;
        }
        let fraction = cat.size_bytes as f32 / total as f32;
        let sweep = fraction * std::f32::consts::TAU;
        let end_angle = start_angle + sweep;

        let pts = pie_slice_points(centre, radius, start_angle, end_angle);
        ui.painter().add(egui::Shape::convex_polygon(pts, cat.color, stroke));

        start_angle = end_angle;
    }
}

fn pie_slice_points(
    centre: egui::Pos2,
    radius: f32,
    start_angle: f32,
    end_angle: f32,
) -> Vec<egui::Pos2> {
    let mut pts = Vec::with_capacity(PIE_STEPS_PER_SLICE + 2);
    pts.push(centre);
    let steps = PIE_STEPS_PER_SLICE;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let a = start_angle + (end_angle - start_angle) * t;
        pts.push(egui::pos2(
            centre.x + radius * a.cos(),
            centre.y + radius * a.sin(),
        ));
    }
    pts
}

fn circle_points(centre: egui::Pos2, radius: f32, steps: usize) -> Vec<egui::Pos2> {
    let mut pts = Vec::with_capacity(steps);
    for i in 0..steps {
        let a = (i as f32 / steps as f32) * std::f32::consts::TAU;
        pts.push(egui::pos2(
            centre.x + radius * a.cos(),
            centre.y + radius * a.sin(),
        ));
    }
    pts
}
