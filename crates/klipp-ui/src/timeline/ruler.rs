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

/// Format a time value as a ruler label. The time is first put on the
/// nearest millisecond: a tick worked out a hair short of a second
/// (1.9999999 s) is that second, not the one before.
fn format_time_label(time_secs: f64, major_interval_secs: f64) -> String {
    let time_secs = (time_secs * 1000.0).round() / 1000.0;
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

/// The ruler's ticks across `width` points scrolled `scroll_px` in at
/// `pps` points a second, up to a major tick past `duration_secs`: each
/// tick's time, with its label when it is a major one. Each is its index
/// times the minor interval, never a running sum, which drifts: 0.2 s
/// added ten times is 1.9999999 s, and the 2 s tick read 0:01.
fn ticks(pps: f32, scroll_px: f32, width: f32, duration_secs: f64) -> Vec<(f64, Option<String>)> {
    let major_interval = choose_major_interval(pps);
    let subdivisions = i64::from(choose_minor_subdivisions(major_interval, pps));
    let minor_interval = major_interval / subdivisions as f64;
    let visible_start = (scroll_px / pps) as f64;
    let visible_end = ((scroll_px + width) / pps) as f64;
    let first_index = (visible_start / minor_interval).floor() as i64;
    (first_index..first_index + 10_000)
        .map(|index| (index, index as f64 * minor_interval))
        .take_while(|(_, t)| *t <= visible_end.min(duration_secs + major_interval))
        .map(|(index, t)| (t, (index.rem_euclid(subdivisions) == 0).then(|| format_time_label(t, major_interval))))
        .collect()
}

/// The render status strip along the ruler's top (#924): where graded
/// clips are in the render cache and ready to play, and where they still
/// need rendering. A span to render is drawn over a cached one, so a
/// moment shows ready only when every graded clip there is.
pub fn draw_render_status(
    painter: &Painter,
    ruler_rect: Rect,
    pps: f32,
    scroll_offset_px: f32,
    spans: &[klipp_engine::texture::RenderSpan],
    ticks_per_sec: f64,
    theme: &KlippTheme,
) {
    if spans.is_empty() || ticks_per_sec <= 0.0 {
        return;
    }
    // Along the top: the foot is where the marker flags point (#946 review).
    let strip = Rect::from_min_max(ruler_rect.min, Pos2::new(ruler_rect.max.x, ruler_rect.min.y + 3.0));
    let x_of = |ticks: i64| ruler_rect.min.x + (ticks as f64 / ticks_per_sec) as f32 * pps - scroll_offset_px;
    for cached in [true, false] {
        let colour = if cached { theme.marker_success } else { theme.marker_error };
        for span in spans.iter().filter(|s| s.cached == cached) {
            let (x1, x2) = (x_of(span.from).max(strip.min.x), x_of(span.to).min(strip.max.x));
            if x2 > x1 {
                painter.rect_filled(Rect::from_min_max(Pos2::new(x1, strip.min.y), Pos2::new(x2, strip.max.y)), 0.0, colour);
            }
        }
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

    let major_tick_h = 9.0_f32;
    let minor_tick_h = 4.0_f32;
    let font_id = egui::FontId::monospace(10.0);
    let label_margin = 4.0_f32;

    let mut last_label_right = f32::NEG_INFINITY;
    for (tick_time, label) in ticks(pps, scroll_offset_px, ruler_rect.width(), duration_secs) {
        let is_major = label.is_some();
        let x = ruler_rect.min.x + (tick_time as f32 * pps) - scroll_offset_px;

        if x < ruler_rect.min.x - 1.0 {
            continue;
        }
        if x > ruler_rect.max.x + 1.0 {
            break;
        }

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
        if let Some(label) = label {
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

#[cfg(test)]
mod tick_tests {
    use super::*;

    /// Every major tick's label is its own time, at every zoom the
    /// ruler picks intervals for: from tenths of a second apart to
    /// minutes. A running sum of the minor interval put the 2 s tick at
    /// 1.9999999 s, labelled 0:01.
    #[test]
    fn each_major_label_reads_its_own_time() {
        for pps in [3.0_f32, 12.0, 25.0, 40.0, 55.0, 80.0, 120.0, 400.0, 900.0] {
            let major = choose_major_interval(pps);
            for (t, label) in ticks(pps, 0.0, 1600.0, 600.0) {
                let Some(label) = label else { continue };
                let n = (t / major).round();
                assert!((t - n * major).abs() < 1e-9, "{pps} pps: a major tick off its interval at {t}");
                assert_eq!(label, format_time_label(n * major + 1e-12, major), "{pps} pps: the tick at {t} s");
                if major >= 1.0 {
                    let secs = (n * major).round() as u64;
                    assert_eq!(label, format!("{}:{:02}", secs / 60, secs % 60), "{pps} pps");
                }
            }
        }
        assert_eq!(format_time_label(1.999_999_9, 2.0), "0:02");
        assert_eq!(format_time_label(59.999_999, 1.0), "1:00");
    }
}
