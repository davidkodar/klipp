use crate::theme::KlippTheme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveChannel {
    Master,
    Red,
    Green,
    Blue,
}

pub struct CurvesResponse {
    pub changed: bool,
    pub drag_started: bool,
    pub drag_ended: bool,
}

/// Channel color for curves.
pub fn channel_color(channel: CurveChannel) -> egui::Color32 {
    match channel {
        CurveChannel::Master => egui::Color32::from_rgb(0xe8, 0xa0, 0x20), // amber per mockup
        CurveChannel::Red => egui::Color32::from_rgb(0xc0, 0x70, 0x70),
        CurveChannel::Green => egui::Color32::from_rgb(0x70, 0xb0, 0x70),
        CurveChannel::Blue => egui::Color32::from_rgb(0x70, 0x70, 0xc0),
    }
}

/// Interactive curves editor widget with optional histogram background.
/// `height` controls the canvas height. `histogram_bins` is optional 256-entry histogram data.
pub fn curves_editor_sized(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    channel: CurveChannel,
    points: &mut Vec<[f32; 2]>,
    height: f32,
    histogram_bins: Option<&[u32; 256]>,
) -> CurvesResponse {
    let mut changed = false;

    let canvas_width = ui.available_width().clamp(100.0, 800.0);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(canvas_width, height),
        egui::Sense::click_and_drag(),
    );

    let drag_started = response.drag_started();
    let drag_ended = response.drag_stopped();

    let painter = ui.painter_at(rect);
    let bg_color = egui::Color32::from_rgb(0x1e, 0x1e, 0x22);
    let border_color = egui::Color32::from_rgb(0x34, 0x34, 0x3c);

    // Background
    painter.rect_filled(rect, 4.0, bg_color);
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(0.5, border_color));

    // Histogram background (if provided)
    if let Some(bins) = histogram_bins {
        let max_count = bins.iter().max().copied().unwrap_or(1) as f32;
        let curve_col = channel_color(channel);
        let hist_alpha = 35u8; // subtle, per spec
        let hist_color = egui::Color32::from_rgba_unmultiplied(
            curve_col.r(), curve_col.g(), curve_col.b(), hist_alpha,
        );
        let bin_w = rect.width() / 256.0;
        for (i, &count) in bins.iter().enumerate() {
            if count == 0 { continue; }
            let h = (count as f32 / max_count) * rect.height() * 0.85;
            let x = rect.min.x + i as f32 * bin_w;
            let bar = egui::Rect::from_min_max(
                egui::pos2(x, rect.max.y - h),
                egui::pos2(x + bin_w, rect.max.y),
            );
            painter.rect_filled(bar, 0.0, hist_color);
        }
    }

    // Grid lines (25% intervals)
    let grid_color = egui::Color32::from_rgb(0x2c, 0x2c, 0x38);
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let x = rect.min.x + t * rect.width();
        let y = rect.min.y + t * rect.height();
        painter.line_segment(
            [egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)],
            egui::Stroke::new(0.5, grid_color),
        );
        painter.line_segment(
            [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
            egui::Stroke::new(0.5, grid_color),
        );
    }

    // Identity diagonal — the reference "shadow" curve that's always visible
    // This is the unmodified straight line so user can see deviation
    painter.line_segment(
        [egui::pos2(rect.min.x, rect.max.y), egui::pos2(rect.max.x, rect.min.y)],
        egui::Stroke::new(1.2, egui::Color32::from_rgba_premultiplied(255, 255, 255, 50)),
    );

    // Ensure minimum identity endpoints exist — empty curves get [0,0] and [1,1]
    if points.len() < 2 {
        points.clear();
        points.push([0.0, 0.0]);
        points.push([1.0, 1.0]);
    }

    // Build spline LUT for drawing
    let lut = klipp_color::curves::build_curve_lut(points);
    let curve_color = channel_color(channel);

    // Draw filled areas on BOTH sides of the curve vs the identity diagonal.
    // Above identity = brighter fill, Below identity = darker fill.
    // This shows the deviation from neutral — standard curve editor behavior.
    let mut curve_points = Vec::with_capacity(256);
    let px_step = (rect.width() / 256.0).max(1.0);
    let fill_above = egui::Color32::from_rgba_premultiplied(
        (curve_color.r() as u16 * 2 / 3) as u8,
        (curve_color.g() as u16 * 2 / 3) as u8,
        (curve_color.b() as u16 * 2 / 3) as u8,
        10,
    );
    let fill_below = egui::Color32::from_rgba_premultiplied(
        curve_color.r() / 4, curve_color.g() / 4, curve_color.b() / 4, 8,
    );

    for (i, &lut_val) in lut.iter().enumerate() {
        let t = i as f32 / 255.0;
        let x = rect.min.x + t * rect.width();
        let curve_y = rect.max.y - (lut_val as f32 / 255.0) * rect.height();
        let identity_y = rect.max.y - t * rect.height(); // diagonal reference
        curve_points.push(egui::pos2(x, curve_y));

        // Fill between curve and identity diagonal
        if (curve_y - identity_y).abs() > 1.0 {
            let (top, bottom, color) = if curve_y < identity_y {
                // Curve above identity (brighter)
                (curve_y, identity_y, fill_above)
            } else {
                // Curve below identity (darker)
                (identity_y, curve_y, fill_below)
            };
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x, top), egui::pos2(x + px_step, bottom)),
                0.0, color,
            );
        }
    }

    // Draw curve line on top
    for window in curve_points.windows(2) {
        painter.line_segment([window[0], window[1]], egui::Stroke::new(2.0, curve_color));
    }

    // ── Point interaction — fast, locked drag, no re-search per frame ──
    let hit_radius = 12.0;

    // Use egui memory to persist which point is being dragged
    let drag_id = response.id.with("curve_drag");
    let mut dragging_idx: Option<usize> = ui.data(|d| d.get_temp(drag_id)).flatten();

    if response.drag_started() {
        // Lock onto the closest point at drag start
        if let Some(pos) = response.interact_pointer_pos() {
            let mut best_idx = None;
            let mut best_dist = f32::MAX;
            for (i, pt) in points.iter().enumerate() {
                let px = rect.min.x + pt[0] * rect.width();
                let py = rect.max.y - pt[1] * rect.height();
                let dist = (egui::pos2(px, py) - pos).length();
                if dist < best_dist {
                    best_dist = dist;
                    best_idx = Some(i);
                }
            }
            if best_dist < hit_radius {
                dragging_idx = best_idx;
            } else {
                // Clicked empty area during drag start — add a new point and drag it
                let new_x = ((pos.x - rect.min.x) / rect.width()).clamp(0.01, 0.99);
                let new_y = ((rect.max.y - pos.y) / rect.height()).clamp(0.0, 1.0);
                let insert_idx = points.iter().position(|pt| pt[0] > new_x).unwrap_or(points.len());
                points.insert(insert_idx, [new_x, new_y]);
                dragging_idx = Some(insert_idx);
                changed = true;
            }
            ui.data_mut(|d| d.insert_temp(drag_id, dragging_idx));
        }
    }

    // Continue dragging the locked point
    if response.dragged() {
        if let Some(idx) = dragging_idx {
            if idx < points.len() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let new_x = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                    let new_y = ((rect.max.y - pos.y) / rect.height()).clamp(0.0, 1.0);

                    // Constrain X between neighbors (don't cross)
                    if idx > 0 && idx < points.len() - 1 {
                        let x_min = points[idx - 1][0] + 0.005;
                        let x_max = points[idx + 1][0] - 0.005;
                        points[idx][0] = new_x.clamp(x_min, x_max);
                    }
                    // Endpoints: only move Y
                    points[idx][1] = new_y;
                    changed = true;
                }
            }
        }
    }

    if response.drag_stopped() {
        ui.data_mut(|d| d.insert_temp::<Option<usize>>(drag_id, None));
    }

    // Double-click: remove point (if not endpoint)
    if response.double_clicked() {
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
            for i in (1..points.len().saturating_sub(1)).rev() {
                let px = rect.min.x + points[i][0] * rect.width();
                let py = rect.max.y - points[i][1] * rect.height();
                if (egui::pos2(px, py) - pos).length() < hit_radius {
                    points.remove(i);
                    changed = true;
                    break;
                }
            }
        }
    }

    // Draw control points — hollow ring style matching the reference
    // Larger circle (5px), dark fill, 2px colored stroke
    for pt in points.iter() {
        let px = rect.min.x + pt[0] * rect.width();
        let py = rect.max.y - pt[1] * rect.height();
        let center = egui::pos2(px, py);
        painter.circle_filled(center, 5.0, egui::Color32::from_rgb(0x14, 0x14, 0x1a));
        painter.circle_stroke(center, 5.0, egui::Stroke::new(1.5, curve_color));
    }

    CurvesResponse {
        changed,
        drag_started,
        drag_ended,
    }
}

/// Legacy: curves editor with default square size, no histogram.
pub fn curves_editor(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    channel: CurveChannel,
    points: &mut Vec<[f32; 2]>,
) -> CurvesResponse {
    let size = ui.available_width().clamp(150.0, 300.0);
    curves_editor_sized(ui, theme, channel, points, size, None)
}
