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

/// The render status strip along the ruler's top (#3212): green where
/// frames are rendered and play from the cache, amber where they are
/// heavy to play and not rendered, and dashed where what was rendered
/// has changed since (stale). Light frames not rendered show nothing.
pub fn draw_render_status(
    painter: &Painter,
    ruler_rect: Rect,
    pps: f32,
    scroll_offset_px: f32,
    spans: &[klipp_engine::texture::RenderSpan],
    ticks_per_sec: f64,
    theme: &KlippTheme,
) {
    use klipp_engine::texture::SpanStatus;
    if spans.is_empty() || ticks_per_sec <= 0.0 {
        return;
    }
    // Along the top: the foot is where the marker flags point (#946 review).
    let strip = Rect::from_min_max(ruler_rect.min, Pos2::new(ruler_rect.max.x, ruler_rect.min.y + 3.0));
    let x_of = |ticks: i64| ruler_rect.min.x + (ticks as f64 / ticks_per_sec) as f32 * pps - scroll_offset_px;
    for span in spans {
        let (x1, x2) = (x_of(span.from).max(strip.min.x), x_of(span.to).min(strip.max.x));
        if x2 <= x1 {
            continue;
        }
        let at = |a: f32, b: f32| Rect::from_min_max(Pos2::new(a, strip.min.y), Pos2::new(b, strip.max.y));
        match span.status {
            SpanStatus::Rendered => {
                painter.rect_filled(at(x1, x2), 0.0, theme.marker_success);
            }
            SpanStatus::Heavy => {
                painter.rect_filled(at(x1, x2), 0.0, theme.marker_warning);
            }
            SpanStatus::Stale => {
                // Dashes from the span's own start, so they keep still as
                // the timeline scrolls.
                let start = x_of(span.from);
                let first = ((x1 - start) / STALE_DASH_PERIOD).floor();
                let mut x = start + first * STALE_DASH_PERIOD;
                while x < x2 {
                    let (a, b) = (x.max(x1), (x + STALE_DASH).min(x2));
                    if b > a {
                        painter.rect_filled(at(a, b), 0.0, theme.marker_success.gamma_multiply(0.6));
                    }
                    x += STALE_DASH_PERIOD;
                }
            }
        }
    }
}

/// A stale span's dashes: this long, this far apart start to start.
const STALE_DASH: f32 = 4.0;
const STALE_DASH_PERIOD: f32 = 7.0;

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

#[cfg(test)]
mod render_status_tests {
    use klipp_engine::texture::{RenderSpan, SpanStatus};

    use super::*;

    /// The strip's fills over `spans`, at 100 px a second and 100 ticks a
    /// second: each fill's left, right and colour.
    fn fills(spans: &[RenderSpan], theme: &KlippTheme) -> Vec<(f32, f32, Color32)> {
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            draw_render_status(&painter, Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 20.0)), 100.0, 0.0, spans, 100.0, theme);
        });
        out.shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Rect(r) => Some((r.rect.left(), r.rect.right(), r.fill)),
                _ => None,
            })
            .collect()
    }

    /// Rendered frames read green, heavy ones amber, stale ones as dashes
    /// with gaps; a frame neither shows nothing.
    #[test]
    fn rendered_heavy_and_stale_each_read_their_own_way() {
        let theme = KlippTheme::default();
        let span = |from, to, status| RenderSpan { from, to, status };
        let got = fills(&[span(0, 100, SpanStatus::Rendered), span(200, 300, SpanStatus::Heavy), span(400, 500, SpanStatus::Stale)], &theme);
        assert!(got.contains(&(0.0, 100.0, theme.marker_success)), "{got:?}");
        assert!(got.contains(&(200.0, 300.0, theme.marker_warning)), "{got:?}");
        let dashes: Vec<_> = got.iter().filter(|(a, _, _)| *a >= 400.0).collect();
        assert!(dashes.len() >= 10 && dashes.iter().all(|(a, b, c)| b - a <= STALE_DASH && *c != theme.marker_success), "dashes, dimmer: {dashes:?}");
        assert!(!got.iter().any(|(a, b, _)| *b > 100.0 && *a < 200.0), "nothing between them");
    }
}
