use crate::theme::KlippTheme;
use egui::{Color32, Painter, Pos2, Rect, Stroke};

const MIN_LABEL_SPACING_PX: f32 = 80.0;
const MIN_MINOR_TICK_PX: f32 = 6.0;

/// Choose the best major tick interval so labels are at least 80px apart.
///
/// Public so the snap layer can reuse the same tick-density logic for
/// `GridInterval::Adaptive`: when the user wants to snap to "whatever
/// the ruler is showing", both rendering and snap consult this fn at
/// the current pixels-per-second.
pub fn choose_major_interval(pixels_per_second: f32) -> f64 {
    const CANDIDATES: &[f64] = &[
        0.001,          // 1ms
        0.005,          // 5ms
        0.01,           // 10ms
        0.025,          // 25ms
        1.0 / 60.0,    // 1 frame @60fps
        1.0 / 50.0,    // 1 frame @50fps
        1.0 / 30.0,    // 1 frame @30fps
        1.0 / 25.0,    // 1 frame @25fps
        1.0 / 24.0,    // 1 frame @24fps
        0.1,            // 100ms
        0.2,            // 200ms
        0.5,            // 500ms
        1.0,            // 1s
        2.0,            // 2s
        5.0,            // 5s
        10.0,           // 10s
        15.0,           // 15s
        30.0,           // 30s
        60.0,           // 1min
        120.0,          // 2min
        300.0,          // 5min
        600.0,          // 10min
        1800.0,         // 30min
        3600.0,         // 1h
    ];

    for &interval in CANDIDATES {
        let px_between = interval as f32 * pixels_per_second;
        if px_between >= MIN_LABEL_SPACING_PX {
            return interval;
        }
    }
    3600.0
}

/// Choose minor tick subdivisions for a given major interval.
pub fn choose_minor_subdivisions(major_interval_secs: f64, pixels_per_second: f32) -> u32 {
    for &sub in &[10u32, 5, 4, 2] {
        let minor = major_interval_secs / sub as f64;
        if minor as f32 * pixels_per_second >= MIN_MINOR_TICK_PX {
            return sub;
        }
    }
    1
}

/// Format a time value as a ruler label.
fn format_time_label(time_secs: f64, major_interval_secs: f64) -> String {
    if major_interval_secs >= 3600.0 {
        let h = (time_secs / 3600.0) as u32;
        let m = ((time_secs % 3600.0) / 60.0) as u32;
        let s = (time_secs % 60.0) as u32;
        format!("{}:{:02}:{:02}", h, m, s)
    } else if major_interval_secs >= 1.0 {
        let m = (time_secs / 60.0) as u32;
        let s = (time_secs % 60.0) as u32;
        format!("{}:{:02}", m, s)
    } else if major_interval_secs >= 1.0 / 24.0 {
        let m = (time_secs / 60.0) as u32;
        let s = (time_secs % 60.0) as u32;
        let ms = ((time_secs * 1000.0) % 1000.0).round() as u32;
        if ms == 0 {
            format!("{}:{:02}", m, s)
        } else {
            format!("{}:{:02}.{:03}", m, s, ms)
        }
    } else {
        let ms = (time_secs * 1000.0).round() as u32;
        format!("{}ms", ms)
    }
}

/// Draw the adaptive timeline ruler.
///
/// - `painter`: clipped to `ruler_rect`
/// - `pps`: pixels per second (from `time_utils::pixels_per_second()`)
/// - `scroll_offset_px`: horizontal scroll in pixels
/// - `duration_secs`: total project duration
pub fn draw_ruler(
    painter: &Painter,
    ruler_rect: Rect,
    pps: f32,
    scroll_offset_px: f32,
    duration_secs: f64,
    theme: &KlippTheme,
) {
    // Background
    painter.rect_filled(ruler_rect, 0.0, theme.bg_panel);
    painter.line_segment(
        [
            Pos2::new(ruler_rect.min.x, ruler_rect.max.y),
            Pos2::new(ruler_rect.max.x, ruler_rect.max.y),
        ],
        Stroke::new(1.0_f32, theme.border),
    );

    let major_interval = choose_major_interval(pps);
    let subdivisions = choose_minor_subdivisions(major_interval, pps);
    let minor_interval = major_interval / subdivisions as f64;

    let visible_start = (scroll_offset_px / pps) as f64;
    let visible_end = ((scroll_offset_px + ruler_rect.width()) / pps) as f64;

    // Snap to nearest minor interval before visible range
    let first_tick = (visible_start / minor_interval).floor() * minor_interval;

    let major_tick_h = 9.0_f32;
    let minor_tick_h = 4.0_f32;
    let font_id = egui::FontId::monospace(10.0);
    let label_margin = 4.0_f32;

    let mut last_label_right = f32::NEG_INFINITY;
    let mut tick_time = first_tick;
    let mut iteration = 0;
    let max_ticks = 10_000;

    while tick_time <= visible_end.min(duration_secs + major_interval) {
        iteration += 1;
        if iteration > max_ticks {
            break;
        }

        let x = ruler_rect.min.x + (tick_time as f32 * pps) - scroll_offset_px;

        if x < ruler_rect.min.x - 1.0 {
            tick_time += minor_interval;
            continue;
        }
        if x > ruler_rect.max.x + 1.0 {
            break;
        }

        // Is this a major tick?
        let remainder = (tick_time / major_interval).rem_euclid(1.0);
        let is_major = !(1e-6..=(1.0 - 1e-6)).contains(&remainder);

        let tick_h = if is_major { major_tick_h } else { minor_tick_h };
        let tick_color = if is_major { theme.border_light } else { theme.border };

        painter.line_segment(
            [
                Pos2::new(x, ruler_rect.max.y - tick_h),
                Pos2::new(x, ruler_rect.max.y),
            ],
            Stroke::new(1.0_f32, tick_color),
        );

        // Label on major ticks with collision prevention
        if is_major {
            let label = format_time_label(tick_time, major_interval);
            let label_x = x + label_margin;
            let estimated_w = label.len() as f32 * 5.5;

            if label_x >= last_label_right + label_margin {
                painter.text(
                    Pos2::new(label_x, ruler_rect.min.y + 3.0),
                    egui::Align2::LEFT_TOP,
                    &label,
                    font_id.clone(),
                    theme.text_dim,
                );
                last_label_right = label_x + estimated_w;
            }
        }

        tick_time += minor_interval;
    }

    // "Beyond duration" amber tint
    let end_x = ruler_rect.min.x + (duration_secs as f32 * pps) - scroll_offset_px;
    if end_x < ruler_rect.max.x {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(end_x.max(ruler_rect.min.x), ruler_rect.min.y),
                ruler_rect.max,
            ),
            0.0,
            Color32::from_rgba_unmultiplied(220, 220, 228, 14),
        );
    }
}
