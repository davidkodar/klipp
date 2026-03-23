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

/// Interactive curves editor widget.
/// Shows a grid canvas with a spline curve and draggable control points.
pub fn curves_editor(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    channel: CurveChannel,
    points: &mut Vec<[f32; 2]>,
) -> CurvesResponse {
    let mut changed = false;

    let canvas_size = ui.available_width().clamp(150.0, 300.0);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(canvas_size, canvas_size),
        egui::Sense::click_and_drag(),
    );

    let drag_started = response.drag_started();
    let drag_ended = response.drag_stopped();

    let painter = ui.painter_at(rect);

    // Background
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(0x0e, 0x0e, 0x12));
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(0.5, egui::Color32::from_rgb(0x2a, 0x2a, 0x32)));

    // Grid lines (25% intervals)
    let grid_color = egui::Color32::from_rgb(0x1a, 0x1a, 0x22);
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

    // Identity diagonal
    painter.line_segment(
        [egui::pos2(rect.min.x, rect.max.y), egui::pos2(rect.max.x, rect.min.y)],
        egui::Stroke::new(0.5, egui::Color32::from_rgb(0x22, 0x22, 0x2a)),
    );

    // Build spline LUT for drawing
    let lut = klipp_color::curves::build_curve_lut(points);

    // Draw curve
    let curve_color = match channel {
        CurveChannel::Master => egui::Color32::from_rgb(0xcc, 0xcc, 0xcc),
        CurveChannel::Red => egui::Color32::from_rgb(0xe0, 0x60, 0x60),
        CurveChannel::Green => egui::Color32::from_rgb(0x60, 0xc0, 0x60),
        CurveChannel::Blue => egui::Color32::from_rgb(0x60, 0x70, 0xe0),
    };

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
    let point_radius = 4.0;
    let hit_radius = 10.0;

    // Find point being dragged or clicked
    if let Some(pointer_pos) = response.interact_pointer_pos() {
        if response.dragged() {
            // Find closest point and drag it
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

                    // Interior points: clamp x between neighbors. Endpoints: y only.
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

    // Single click on empty area: add point
    if response.clicked() && !changed {
        if let Some(pointer_pos) = response.interact_pointer_pos() {
            let new_x = ((pointer_pos.x - rect.min.x) / rect.width()).clamp(0.01, 0.99);
            let new_y = ((rect.max.y - pointer_pos.y) / rect.height()).clamp(0.0, 1.0);

            // Check if click is far from any existing point
            let near_point = points.iter().any(|pt| {
                let px = rect.min.x + pt[0] * rect.width();
                let py = rect.max.y - pt[1] * rect.height();
                (egui::pos2(px, py) - pointer_pos).length() < hit_radius
            });

            if !near_point {
                // Insert in sorted order
                let insert_idx = points.iter().position(|pt| pt[0] > new_x).unwrap_or(points.len());
                points.insert(insert_idx, [new_x, new_y]);
                changed = true;
            }
        }
    }

    // Draw control points
    for pt in points.iter() {
        let px = rect.min.x + pt[0] * rect.width();
        let py = rect.max.y - pt[1] * rect.height();
        let center = egui::pos2(px, py);
        painter.circle_filled(center, point_radius, curve_color);
        painter.circle_stroke(center, point_radius, egui::Stroke::new(1.0, egui::Color32::from_rgb(0x0e, 0x0e, 0x12)));
    }

    CurvesResponse {
        changed,
        drag_started,
        drag_ended,
    }
}
