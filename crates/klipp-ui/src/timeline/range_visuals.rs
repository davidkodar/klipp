//! FCP-style range selection visuals for the timeline.
//!
//! All geometry constants, drawing functions, hit testing, and time formatting
//! for the in/out range selection live here. Both `timeline/mod.rs` (ruler area)
//! and `panels/timeline.rs` (clip area) call into this module.

use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::timeline::time_utils;

// ── Geometry constants ──

/// Visual width of handle bar (px).
pub const HANDLE_WIDTH: f32 = 6.0;
/// Hit-test width for grabbing handles (px, wider than visual).
pub const HANDLE_HIT_WIDTH: f32 = 12.0;
/// Radius of the lollipop circle cap above the ruler.
pub const LOLLIPOP_RADIUS: f32 = 5.0;
/// When in/out handles are closer than this, draw collapsed mode.
pub const COLLAPSED_THRESHOLD: f32 = 20.0;

// ── Alpha values ──

/// Ruler fill alpha (boosted from original 25 to be clearly visible).
const FILL_ALPHA: u8 = 55;
/// 1px accent line at top of ruler fill.
const TOP_EDGE_ALPHA: u8 = 180;
/// Amber overlay on clips within range.
const CLIP_OVERLAY_ALPHA: u8 = 30;
/// 2px accent line at top of clip overlay.
const CLIP_OVERLAY_EDGE_ALPHA: u8 = 140;
/// 1px vertical guide lines through track area.
const GUIDE_LINE_ALPHA: u8 = 40;

// ── High-level orchestrator ──

/// Draw all range selection visuals: ruler fill, handles, lollipops, timecode labels.
///
/// - `ruler_painter`: clipped to ruler_rect (for fill region)
/// - `parent_painter`: unclipped (for lollipop caps + timecode labels above ruler)
/// - `tracks_bottom`: y coordinate of the bottom of all track lanes
#[allow(clippy::too_many_arguments)]
pub fn draw_range(
    ruler_painter: &egui::Painter,
    parent_painter: &egui::Painter,
    ruler_rect: egui::Rect,
    tracks_bottom: f32,
    state: &AppState,
    theme: &KlippTheme,
    pps: f32,
    scroll_x: f32,
    loop_pulsing: bool,
) {
    let seq = state.sequence();
    let (in_pt, out_pt) = match (seq.in_point, seq.out_point) {
        (Some(i), Some(o)) => (Some(i), Some(o)),
        (Some(i), None) => (Some(i), None),
        (None, Some(o)) => (None, Some(o)),
        (None, None) => return,
    };

    let in_x = in_pt.map(|t| ruler_rect.left() + time_utils::time_to_x(t.to_secs_f64(), pps, scroll_x));
    let out_x = out_pt.map(|t| ruler_rect.left() + time_utils::time_to_x(t.to_secs_f64(), pps, scroll_x));

    // Ruler fill (only when both points set)
    if let (Some(ix), Some(ox)) = (in_x, out_x) {
        draw_ruler_fill(ruler_painter, ruler_rect, ix, ox, theme, loop_pulsing);
    }

    // Determine collapsed mode
    let collapsed = match (in_x, out_x) {
        (Some(ix), Some(ox)) => (ox - ix) < COLLAPSED_THRESHOLD,
        _ => false,
    };

    if collapsed {
        let ix = in_x.unwrap();
        let ox = out_x.unwrap();
        let dur_secs = match (in_pt, out_pt) {
            (Some(i), Some(o)) => (o - i).to_secs_f64(),
            _ => 0.0,
        };
        let dur_str = format_range_duration(dur_secs);
        draw_collapsed_handle(parent_painter, ix, ox, ruler_rect.top(), tracks_bottom, &dur_str, theme);
    } else {
        // Full handles
        if let Some(ix) = in_x {
            draw_handle(parent_painter, ix, true, ruler_rect.top(), tracks_bottom, theme);
            draw_lollipop(parent_painter, ix, true, ruler_rect.top(), theme);
            if let Some(t) = in_pt {
                let tc_str = format_handle_timecode(t.to_secs_f64(), state.timecode_display());
                draw_handle_timecode(parent_painter, ix, true, ruler_rect.top(), &tc_str, theme);
            }
        }
        if let Some(ox) = out_x {
            draw_handle(parent_painter, ox, false, ruler_rect.top(), tracks_bottom, theme);
            draw_lollipop(parent_painter, ox, false, ruler_rect.top(), theme);
            if let Some(t) = out_pt {
                let tc_str = format_handle_timecode(t.to_secs_f64(), state.timecode_display());
                draw_handle_timecode(parent_painter, ox, false, ruler_rect.top(), &tc_str, theme);
            }
        }
    }
}

// ── Sub-drawing functions ──

/// Amber fill region on the ruler between in and out x positions.
/// When `pulsing` is true (loop active + playing), animate alpha between 55 and 80.
fn draw_ruler_fill(
    painter: &egui::Painter,
    ruler_rect: egui::Rect,
    in_x: f32,
    out_x: f32,
    theme: &KlippTheme,
    pulsing: bool,
) {
    let x1 = in_x.max(ruler_rect.left());
    let x2 = out_x.min(ruler_rect.right());
    if x2 <= x1 {
        return;
    }

    let fill_rect = egui::Rect::from_min_max(
        egui::pos2(x1, ruler_rect.top()),
        egui::pos2(x2, ruler_rect.bottom()),
    );

    // Background fill — pulse alpha when loop is active + playing
    let fill_alpha = if pulsing {
        // Use wall time for smooth sine wave: ~1.5Hz pulse
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let t = (elapsed * 1.5 * std::f64::consts::TAU).sin() * 0.5 + 0.5; // 0..1
        (55.0 + t * 25.0) as u8 // 55..80
    } else {
        FILL_ALPHA
    };
    let c = theme.accent;
    painter.rect_filled(
        fill_rect,
        0.0,
        egui::Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), fill_alpha),
    );

    // 1px bright top-edge accent line
    painter.line_segment(
        [fill_rect.left_top(), fill_rect.right_top()],
        egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), TOP_EDGE_ALPHA),
        ),
    );
}

/// Draw a single handle bar (rounded rect spanning ruler through tracks).
/// `is_in`: true = in-handle (right edge at x), false = out-handle (left edge at x).
fn draw_handle(
    painter: &egui::Painter,
    x: f32,
    is_in: bool,
    ruler_top: f32,
    tracks_bottom: f32,
    theme: &KlippTheme,
) {
    let rect = if is_in {
        // In-handle: right edge at x
        egui::Rect::from_min_max(
            egui::pos2(x - HANDLE_WIDTH, ruler_top),
            egui::pos2(x, tracks_bottom),
        )
    } else {
        // Out-handle: left edge at x
        egui::Rect::from_min_max(
            egui::pos2(x, ruler_top),
            egui::pos2(x + HANDLE_WIDTH, tracks_bottom),
        )
    };
    painter.rect_filled(rect, 2.0, theme.accent);
}

/// Draw lollipop circle cap above the ruler.
fn draw_lollipop(
    painter: &egui::Painter,
    x: f32,
    is_in: bool,
    ruler_top: f32,
    theme: &KlippTheme,
) {
    let cx = if is_in {
        x - HANDLE_WIDTH / 2.0
    } else {
        x + HANDLE_WIDTH / 2.0
    };
    let cy = ruler_top - LOLLIPOP_RADIUS + 1.0;
    painter.circle_filled(egui::pos2(cx, cy), LOLLIPOP_RADIUS, theme.accent);
}

/// Draw timecode label above a handle cap.
fn draw_handle_timecode(
    painter: &egui::Painter,
    x: f32,
    is_in: bool,
    ruler_top: f32,
    timecode_str: &str,
    theme: &KlippTheme,
) {
    let cx = if is_in {
        x - HANDLE_WIDTH / 2.0
    } else {
        x + HANDLE_WIDTH / 2.0
    };
    let label_y = ruler_top - LOLLIPOP_RADIUS * 2.0 - 2.0;
    let align = if is_in {
        egui::Align2::RIGHT_BOTTOM
    } else {
        egui::Align2::LEFT_BOTTOM
    };

    // Background pill for readability
    let galley = painter.layout_no_wrap(
        timecode_str.to_string(),
        egui::FontId::monospace(10.0),
        theme.accent,
    );
    let text_w = galley.size().x;
    let text_h = galley.size().y;
    let pill_x = if is_in { cx - text_w - 4.0 } else { cx };
    let pill_rect = egui::Rect::from_min_size(
        egui::pos2(pill_x - 2.0, label_y - text_h - 2.0),
        egui::vec2(text_w + 4.0, text_h + 2.0),
    );
    painter.rect_filled(
        pill_rect,
        2.0,
        egui::Color32::from_rgba_unmultiplied(20, 20, 24, 220),
    );

    painter.text(
        egui::pos2(cx, label_y),
        align,
        timecode_str,
        egui::FontId::monospace(10.0),
        theme.accent,
    );
}

/// Draw collapsed handle when in/out points are very close (<20px apart).
/// Single 10px amber rect with one centered cap and duration label.
fn draw_collapsed_handle(
    painter: &egui::Painter,
    in_x: f32,
    out_x: f32,
    ruler_top: f32,
    tracks_bottom: f32,
    dur_str: &str,
    theme: &KlippTheme,
) {
    // Single bar spanning in_x to out_x (minimum 10px)
    let bar_left = in_x - 2.0;
    let bar_right = (out_x + 2.0).max(bar_left + 10.0);
    let rect = egui::Rect::from_min_max(
        egui::pos2(bar_left, ruler_top),
        egui::pos2(bar_right, tracks_bottom),
    );
    painter.rect_filled(rect, 2.0, theme.accent);

    // Centered cap
    let cx = (in_x + out_x) / 2.0;
    let cy = ruler_top - LOLLIPOP_RADIUS + 1.0;
    painter.circle_filled(egui::pos2(cx, cy), LOLLIPOP_RADIUS, theme.accent);

    // Duration label above
    let label_y = ruler_top - LOLLIPOP_RADIUS * 2.0 - 2.0;
    let galley = painter.layout_no_wrap(
        dur_str.to_string(),
        egui::FontId::monospace(10.0),
        theme.accent,
    );
    let pill_rect = egui::Rect::from_min_size(
        egui::pos2(cx - galley.size().x / 2.0 - 2.0, label_y - galley.size().y - 2.0),
        egui::vec2(galley.size().x + 4.0, galley.size().y + 2.0),
    );
    painter.rect_filled(
        pill_rect,
        2.0,
        egui::Color32::from_rgba_unmultiplied(20, 20, 24, 220),
    );
    painter.text(
        egui::pos2(cx, label_y),
        egui::Align2::CENTER_BOTTOM,
        dur_str,
        egui::FontId::monospace(10.0),
        theme.accent,
    );
}

/// Draw amber overlay on a clip region that falls within the in/out range.
/// Call this after drawing the clip body, passing the overlap rect.
pub fn draw_clip_overlay(painter: &egui::Painter, overlay_rect: egui::Rect, theme: &KlippTheme) {
    let c = theme.accent;
    // Semi-transparent amber fill
    painter.rect_filled(
        overlay_rect,
        0.0,
        egui::Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), CLIP_OVERLAY_ALPHA),
    );
    // 2px accent line at top edge
    painter.line_segment(
        [overlay_rect.left_top(), overlay_rect.right_top()],
        egui::Stroke::new(
            2.0_f32,
            egui::Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), CLIP_OVERLAY_EDGE_ALPHA),
        ),
    );
}

/// Draw 1px vertical guide line at an in or out point through the track area.
pub fn draw_guide_line(
    painter: &egui::Painter,
    x: f32,
    top: f32,
    bottom: f32,
    theme: &KlippTheme,
) {
    let c = theme.accent;
    painter.line_segment(
        [egui::pos2(x, top), egui::pos2(x, bottom)],
        egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), GUIDE_LINE_ALPHA),
        ),
    );
}

// ── Hit testing ──

/// Returns which handle the mouse is over, if any.
pub fn hit_test_handle(
    mouse_x: f32,
    in_x: Option<f32>,
    out_x: Option<f32>,
) -> Option<crate::state::RangeDragKind> {
    use crate::state::RangeDragKind;
    let half = HANDLE_HIT_WIDTH / 2.0;

    // Check in-handle (right edge at in_x, so center is in_x - HANDLE_WIDTH/2)
    if let Some(ix) = in_x {
        if mouse_x >= ix - HANDLE_HIT_WIDTH && mouse_x <= ix + half - HANDLE_WIDTH {
            return Some(RangeDragKind::InHandle);
        }
    }
    // Check out-handle (left edge at out_x, so center is out_x + HANDLE_WIDTH/2)
    if let Some(ox) = out_x {
        if mouse_x >= ox - half + HANDLE_WIDTH && mouse_x <= ox + HANDLE_HIT_WIDTH {
            return Some(RangeDragKind::OutHandle);
        }
    }
    None
}

/// Draw a snap line indicator (amber, full height, with timecode pill).
pub fn draw_snap_line(
    painter: &egui::Painter,
    x: f32,
    top: f32,
    bottom: f32,
    secs: f64,
    tc: klipp_core::TimecodeDisplay,
    theme: &KlippTheme,
) {
    let snap_color = theme.accent;
    // Thick amber line
    painter.line_segment(
        [egui::pos2(x, top), egui::pos2(x, bottom)],
        egui::Stroke::new(2.0_f32, snap_color),
    );
    // Glow
    let c = theme.accent;
    painter.line_segment(
        [egui::pos2(x, top), egui::pos2(x, bottom)],
        egui::Stroke::new(6.0_f32, egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 50)),
    );
    // Timecode pill label
    let label = format_handle_timecode(secs, tc);
    let label_pos = egui::pos2(x + 4.0, top + 3.0);
    let galley = painter.layout_no_wrap(
        label.clone(),
        egui::FontId::monospace(10.0),
        snap_color,
    );
    let pill_rect = egui::Rect::from_min_size(
        egui::pos2(label_pos.x - 2.0, label_pos.y - 1.0),
        egui::vec2(galley.size().x + 4.0, galley.size().y + 2.0),
    );
    painter.rect_filled(pill_rect, 2.0, egui::Color32::from_rgba_unmultiplied(20, 20, 24, 220));
    painter.text(
        label_pos,
        egui::Align2::LEFT_TOP,
        &label,
        egui::FontId::monospace(10.0),
        snap_color,
    );
}

// ── Time formatting ──

/// A position for a handle label, counted the way the sequence counts —
/// drop-frame included — and kept compact by dropping a zero hour.
pub fn format_handle_timecode(secs: f64, tc: klipp_core::TimecodeDisplay) -> String {
    let full = tc.position_secs(secs);
    // "00:01:23;04" reads as "01:23;04" in the space a handle has. The
    // sign, when there is one, stays in front.
    let (sign, rest) = match full.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", full.as_str()),
    };
    match rest.strip_prefix("00:") {
        Some(short) => format!("{sign}{short}"),
        None => full,
    }
}

/// Format range duration for display: "Xs" if < 60s, "M:SS" if < 1h, "H:MM:SS" otherwise.
pub fn format_range_duration(secs: f64) -> String {
    let s = secs.abs();
    if s < 60.0 {
        format!("{:.1}s", s)
    } else if s < 3600.0 {
        let m = (s / 60.0) as u32;
        let ss = (s % 60.0) as u32;
        format!("{}:{:02}", m, ss)
    } else {
        let h = (s / 3600.0) as u32;
        let m = ((s % 3600.0) / 60.0) as u32;
        let ss = (s % 60.0) as u32;
        format!("{}:{:02}:{:02}", h, m, ss)
    }
}
