use crate::theme::KlippTheme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveChannel {
    Master,
    Red,
    Green,
    Blue,
}

/// What a curve's value means, which decides what "no change" looks
/// like and what the reference line is.
///
/// The RGB curves map an input level to an output level: identity is
/// the diagonal. The Hue/Sat curves are *relative* — the pipeline reads
/// them as `(v - 0.5) * 2`, a signed adjustment around a neutral
/// mid-line, and builds their LUT filled with 0.5. Drawing those on a
/// diagonal said that a curve doing nothing was rotating hue by a full
/// turn across the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveSpace {
    /// Input level to output level. Identity is the diagonal.
    Absolute,
    /// A signed adjustment about 0.5. Identity is flat across the middle.
    Relative,
}

impl CurveSpace {
    /// Where the reference line sits at input `t`.
    fn identity_at(self, t: f32) -> f32 {
        match self {
            CurveSpace::Absolute => t,
            CurveSpace::Relative => 0.5,
        }
    }
}

/// What the horizontal axis measures, drawn as a strip beneath the
/// canvas so the input is legible without a legend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveAxis {
    /// 0..360 degrees, drawn as the hue spectrum.
    Hue,
    /// Black to white.
    Luma,
    /// Grey to fully saturated.
    Saturation,
    /// A plain level, for the RGB curves.
    Level,
}

impl CurveAxis {
    /// The colour of the axis strip at `t`, or None for a plain axis.
    fn swatch(self, t: f32) -> Option<egui::Color32> {
        match self {
            CurveAxis::Level => None,
            CurveAxis::Hue => {
                let (r, g, b) = klipp_color::hsv_to_rgb(t * 360.0, 0.85, 0.95);
                Some(egui::Color32::from_rgb(
                    (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8,
                ))
            }
            CurveAxis::Luma => {
                let v = (t * 255.0) as u8;
                Some(egui::Color32::from_rgb(v, v, v))
            }
            CurveAxis::Saturation => {
                let (r, g, b) = klipp_color::hsv_to_rgb(20.0, t, 0.85);
                Some(egui::Color32::from_rgb(
                    (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8,
                ))
            }
        }
    }

    /// How an input position reads to a person.
    fn format_in(self, t: f32) -> String {
        match self {
            CurveAxis::Hue => format!("{:.0}\u{00B0}", t * 360.0),
            CurveAxis::Level | CurveAxis::Luma | CurveAxis::Saturation => format!("{t:.3}"),
        }
    }
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
    theme: &KlippTheme,
    channel: CurveChannel,
    points: &mut Vec<[f32; 2]>,
    height: f32,
    histogram_bins: Option<&[u32; 256]>,
) -> CurvesResponse {
    curves_editor_in(
        ui, theme, channel, points, height, histogram_bins,
        CurveSpace::Absolute, CurveAxis::Level,
    )
}

/// The editor, told what its numbers mean.
///
/// `space` decides what "no change" is: the diagonal for a level
/// mapping, a flat mid-line for a relative adjustment. `axis` draws the
/// input as a strip beneath the canvas so hue is visible as hue.
#[allow(clippy::too_many_arguments)]
pub fn curves_editor_in(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    channel: CurveChannel,
    points: &mut Vec<[f32; 2]>,
    height: f32,
    histogram_bins: Option<&[u32; 256]>,
    space: CurveSpace,
    axis: CurveAxis,
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
    let bg_color = theme.bg_panel_alt;
    let border_color = theme.border;

    // Background
    painter.rect_filled(rect, 4.0, bg_color);
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(0.5_f32, border_color));

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

    // The input axis, drawn as what it measures: hue as the spectrum,
    // luma as black-to-white, saturation as grey-to-colour. A curve
    // over hue is unreadable without it — you cannot tell which part
    // of the canvas is the oranges.
    if axis != CurveAxis::Level {
        let strip_h = 5.0_f32;
        let strip = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, rect.max.y - strip_h),
            rect.max,
        );
        let steps = 64;
        for i in 0..steps {
            let t0 = i as f32 / steps as f32;
            let t1 = (i + 1) as f32 / steps as f32;
            if let Some(c) = axis.swatch((t0 + t1) * 0.5) {
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(rect.min.x + t0 * rect.width(), strip.min.y),
                        egui::pos2(rect.min.x + t1 * rect.width(), strip.max.y),
                    ),
                    0.0,
                    c,
                );
            }
        }
    }

    // Numbers on the axis the strip only colours.
    //
    // The spectrum says which hue is where; it does not say where 180
    // is, and "shift the oranges by twenty degrees" needs the number.
    // Three marks is enough to read the scale without crowding a
    // ninety-pixel canvas.
    if axis != CurveAxis::Level {
        for t in [0.0_f32, 0.5, 1.0] {
            let label = axis.format_in(t);
            let x = rect.min.x + t * rect.width();
            let anchor = if t <= 0.0 {
                egui::Align2::LEFT_BOTTOM
            } else if t >= 1.0 {
                egui::Align2::RIGHT_BOTTOM
            } else {
                egui::Align2::CENTER_BOTTOM
            };
            painter.text(
                egui::pos2(x, rect.max.y - 7.0),
                anchor,
                label,
                egui::FontId::monospace(8.0),
                theme.overlay(120),
            );
        }
    }

    // Grid lines (25% intervals)
    let grid_color = theme.border_light;
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let x = rect.min.x + t * rect.width();
        let y = rect.min.y + t * rect.height();
        painter.line_segment(
            [egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)],
            egui::Stroke::new(0.5_f32, grid_color),
        );
        painter.line_segment(
            [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
            egui::Stroke::new(0.5_f32, grid_color),
        );
    }

    // The reference line: what this curve looks like doing nothing.
    // For a level mapping that is the diagonal; for a relative
    // adjustment it is flat across the middle, which is what the
    // pipeline fills these LUTs with and reads as `(v - 0.5) * 2`.
    match space {
        CurveSpace::Absolute => {
            painter.line_segment(
                [egui::pos2(rect.min.x, rect.max.y), egui::pos2(rect.max.x, rect.min.y)],
                egui::Stroke::new(1.2_f32, theme.overlay(50)),
            );
        }
        CurveSpace::Relative => {
            let y = rect.min.y + rect.height() * 0.5;
            painter.line_segment(
                [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
                egui::Stroke::new(1.2_f32, theme.overlay(70)),
            );
        }
    }

    // An untouched curve is left untouched.
    //
    // This used to seed the caller's own `Vec` with [0,0] and [1,1]
    // the moment the widget drew — and the LUT builder skips only an
    // *empty* curve, so merely opening the Hue/Sat tab turned "no
    // curve" into a full-range ramp on a relative curve whose identity
    // is flat. Nothing is written now until the user actually drags,
    // and what gets written then is this space's identity.
    let identity_pts: Vec<[f32; 2]> = match space {
        CurveSpace::Absolute => vec![[0.0, 0.0], [1.0, 1.0]],
        CurveSpace::Relative => vec![[0.0, 0.5], [1.0, 0.5]],
    };
    let drawing_identity = points.len() < 2;
    let draw_pts: &Vec<[f32; 2]> = if drawing_identity { &identity_pts } else { points };

    // Build spline LUT for drawing
    // Drawn from the table the grade reads: a hue curve's runs round
    // the circle.
    let lut = if axis == CurveAxis::Hue { klipp_color::curves::build_periodic_curve_lut(draw_pts) } else { klipp_color::curves::build_curve_lut(draw_pts) };
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
        let identity_y = rect.max.y - space.identity_at(t) * rect.height();
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
        painter.line_segment([window[0], window[1]], egui::Stroke::new(2.0_f32, curve_color));
    }

    // ── Point interaction — fast, locked drag, no re-search per frame ──
    let hit_radius = 12.0;

    // Use egui memory to persist which point is being dragged
    let drag_id = response.id.with("curve_drag");
    let mut dragging_idx: Option<usize> = ui.data(|d| d.get_temp(drag_id)).flatten();

    // First touch on an untouched curve writes this space's identity,
    // so the two endpoints that appear are the flat line the user was
    // looking at rather than a ramp they never asked for.
    if (response.drag_started() || response.clicked()) && points.len() < 2 {
        *points = identity_pts.clone();
        changed = true;
    }

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

    // ── The dragged point's values, while it is being dragged ──
    //
    // A curve point is an input level mapped to an output level, and
    // until now neither number was ever shown: you could place a point
    // precisely only by eye, and could not say afterwards what you had
    // set. Every grading application reads them out. Drawn only during
    // the drag, in the corner furthest from the point, so it never
    // covers the curve you are watching.
    if let Some(idx) = dragging_idx {
        if response.dragged() && idx < points.len() {
            let pt = points[idx];
            // In the axis's own units, and the output as what the
            // pipeline will do with it: a relative curve is read as
            // (v - 0.5) * 2, so it is a signed adjustment, not a level.
            let text = match space {
                CurveSpace::Absolute => {
                    format!("in {}   out {:.3}", axis.format_in(pt[0]), pt[1])
                }
                CurveSpace::Relative => {
                    format!("{}   {:+.0}%", axis.format_in(pt[0]), (pt[1] - 0.5) * 200.0)
                }
            };
            let font = egui::FontId::monospace(10.0);
            let galley = painter.layout_no_wrap(text, font, theme.text_primary);
            // Opposite corner from the point, so the readout and the
            // thing it describes are never in the same place.
            let pad = 6.0;
            let pos = egui::pos2(
                if pt[0] > 0.5 { rect.min.x + pad } else { rect.max.x - pad - galley.size().x },
                if pt[1] > 0.5 { rect.max.y - pad - galley.size().y } else { rect.min.y + pad },
            );
            painter.rect_filled(
                egui::Rect::from_min_size(pos, galley.size()).expand(3.0),
                3.0,
                theme.bg_panel.gamma_multiply(0.85),
            );
            painter.galley(pos, galley, theme.text_primary);
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
        painter.circle_filled(center, 5.0, theme.bg_deep);
        painter.circle_stroke(center, 5.0, egui::Stroke::new(1.5_f32, curve_color));
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
