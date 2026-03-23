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

    let canvas_width = ui.available_width().clamp(150.0, 400.0);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(canvas_width, height),
        egui::Sense::click_and_drag(),
    );

    let drag_started = response.drag_started();
    let drag_ended = response.drag_stopped();

    let painter = ui.painter_at(rect);
    let bg_color = egui::Color32::from_rgb(0x16, 0x16, 0x1a);
    let border_color = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);

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

    // Identity diagonal (thicker, brighter than grid — key reference line)
    painter.line_segment(
        [egui::pos2(rect.min.x, rect.max.y), egui::pos2(rect.max.x, rect.min.y)],
        egui::Stroke::new(1.0, egui::Color32::from_rgb(0x3a, 0x3a, 0x4a)),
    );

    // Build spline LUT for drawing
    let lut = klipp_color::curves::build_curve_lut(points);
    let curve_color = channel_color(channel);

    // Draw curve
    let mut curve_points = Vec::with_capacity(256);
    for (i, &lut_val) in lut.iter().enumerate() {
        let x = rect.min.x + (i as f32 / 255.0) * rect.width();
        let y = rect.max.y - (lut_val as f32 / 255.0) * rect.height();
        curve_points.push(egui::pos2(x, y));
    }
    for window in curve_points.windows(2) {
        painter.line_segment([window[0], window[1]], egui::Stroke::new(1.5, curve_color));
    }

    // Point interaction
    let hit_radius = 10.0;

    if let Some(pointer_pos) = response.interact_pointer_pos() {
        if response.dragged() {
            let mut closest_idx = None;
            let mut closest_dist = f32::MAX;
            for (i, pt) in points.iter().enumerate() {
                let px = rect.min.x + pt[0] * rect.width();
                let py = rect.max.y - pt[1] * rect.height();
                let dist = (egui::pos2(px, py) - pointer_pos).length();
                if dist < closest_dist {
                    closest_dist = dist;
                    closest_idx = Some(i);
                }
            }

            if let Some(idx) = closest_idx {
                if closest_dist < hit_radius || response.drag_started() {
                    let new_x = ((pointer_pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                    let new_y = ((rect.max.y - pointer_pos.y) / rect.height()).clamp(0.0, 1.0);

                    if idx > 0 && idx < points.len() - 1 {
                        let x_min = points[idx - 1][0] + 0.01;
                        let x_max = points[idx + 1][0] - 0.01;
                        points[idx][0] = new_x.clamp(x_min, x_max);
                    }
                    points[idx][1] = new_y;
                    changed = true;
                }
            }
        }
    }

    // Double-click: remove point (if not endpoint)
    if response.double_clicked() {
        if let Some(pointer_pos) = ui.input(|i| i.pointer.interact_pos()) {
            for i in (1..points.len().saturating_sub(1)).rev() {
                let px = rect.min.x + points[i][0] * rect.width();
                let py = rect.max.y - points[i][1] * rect.height();
                if (egui::pos2(px, py) - pointer_pos).length() < hit_radius {
                    points.remove(i);
                    changed = true;
                    break;
                }
            }
        }
    }

    // Single click: add point
    if response.clicked() && !changed {
        if let Some(pointer_pos) = response.interact_pointer_pos() {
            let new_x = ((pointer_pos.x - rect.min.x) / rect.width()).clamp(0.01, 0.99);
            let new_y = ((rect.max.y - pointer_pos.y) / rect.height()).clamp(0.0, 1.0);

            let near_point = points.iter().any(|pt| {
                let px = rect.min.x + pt[0] * rect.width();
                let py = rect.max.y - pt[1] * rect.height();
                (egui::pos2(px, py) - pointer_pos).length() < hit_radius
            });

            if !near_point {
                let insert_idx = points.iter().position(|pt| pt[0] > new_x).unwrap_or(points.len());
                points.insert(insert_idx, [new_x, new_y]);
                changed = true;
            }
        }
    }

    // Draw control points — HOLLOW ring style (dark fill + thick coloured stroke)
    for pt in points.iter() {
        let px = rect.min.x + pt[0] * rect.width();
        let py = rect.max.y - pt[1] * rect.height();
        let center = egui::pos2(px, py);
        painter.circle_filled(center, 4.0, egui::Color32::from_rgb(0x12, 0x12, 0x18));
        painter.circle_stroke(center, 4.0, egui::Stroke::new(2.0, curve_color));
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
