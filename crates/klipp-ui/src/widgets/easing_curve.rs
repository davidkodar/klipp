//! A motion's own easing curve (#1511): the curve from rest to arrival
//! drawn in a small square, its two handles dragged to shape it.

use egui::{pos2, vec2, Pos2, Rect, Response, Sense, Stroke, Ui};
use klipp_core::title_motion::CubicBezier;

use crate::theme::KlippTheme;

/// How far above arrival and below rest the square shows, as a share of
/// the travel, so an overshooting curve stays in view.
const ROOM: f32 = 0.25;

/// The square's point for time `x` and progress `y`.
fn to_screen(rect: Rect, x: f32, y: f32) -> Pos2 {
    let span = 1.0 + 2.0 * ROOM;
    pos2(rect.left() + x * rect.width(), rect.bottom() - (y + ROOM) / span * rect.height())
}

/// The time and progress at a point in the square.
fn from_screen(rect: Rect, p: Pos2) -> (f32, f32) {
    let span = 1.0 + 2.0 * ROOM;
    ((p.x - rect.left()) / rect.width(), (rect.bottom() - p.y) / rect.height() * span - ROOM)
}

/// Handle `which` (0 the first, 1 the second) moved to time `x` and
/// progress `y`: the time kept within 0..1, the progress within the
/// square.
pub fn set_handle(curve: &mut CubicBezier, which: usize, x: f32, y: f32) {
    let (x, y) = (x.clamp(0.0, 1.0), y.clamp(-ROOM, 1.0 + ROOM));
    if which == 0 {
        (curve.x1, curve.y1) = (x, y);
    } else {
        (curve.x2, curve.y2) = (x, y);
    }
}

/// Draw `curve` and let its handles be dragged; the response is changed
/// while one moves.
pub fn easing_curve(ui: &mut Ui, theme: &KlippTheme, curve: &mut CubicBezier) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(132.0, 100.0), Sense::hover());
    let painter = ui.painter_at(rect.expand(6.0));
    painter.rect_filled(rect, crate::theme::radius::PANEL, theme.bg_panel_alt);
    let rest = to_screen(rect, 0.0, 0.0);
    let arrival = to_screen(rect, 1.0, 1.0);
    for y in [0.0, 1.0] {
        painter.line_segment([to_screen(rect, 0.0, y), to_screen(rect, 1.0, y)], Stroke::new(1.0_f32, theme.border));
    }
    let points: Vec<Pos2> = (0..=48).map(|i| {
        let t = i as f32 / 48.0;
        to_screen(rect, t, curve.apply(t))
    }).collect();
    painter.add(egui::Shape::line(points, Stroke::new(2.0_f32, theme.accent)));
    for (which, anchor) in [(0, rest), (1, arrival)] {
        let (x, y) = if which == 0 { (curve.x1, curve.y1) } else { (curve.x2, curve.y2) };
        let at = to_screen(rect, x, y);
        let handle = ui.interact(Rect::from_center_size(at, vec2(12.0, 12.0)), response.id.with(which), Sense::drag());
        if handle.dragged() {
            if let Some(p) = handle.interact_pointer_pos() {
                let (x, y) = from_screen(rect, p);
                set_handle(curve, which, x, y);
                response.mark_changed();
            }
        }
        let colour = if handle.hovered() || handle.dragged() { theme.text_primary } else { theme.text_secondary };
        painter.line_segment([anchor, at], Stroke::new(1.0_f32, colour));
        painter.circle_filled(at, 4.0, colour);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A point in the square reads back as the time and progress it
    /// was drawn for, the room above and below included.
    #[test]
    fn the_square_maps_both_ways() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(132.0, 100.0));
        for (x, y) in [(0.0, 0.0), (1.0, 1.0), (0.3, 1.2), (0.7, -0.2)] {
            let (bx, by) = from_screen(rect, to_screen(rect, x, y));
            assert!((bx - x).abs() < 1e-5 && (by - y).abs() < 1e-5, "{x} {y} -> {bx} {by}");
        }
    }

    /// A handle dragged past the square's sides keeps its time within
    /// 0..1, so the curve still has one progress for each time.
    #[test]
    fn a_handle_keeps_its_time_within_the_motion() {
        let mut c = CubicBezier::default();
        set_handle(&mut c, 0, -0.5, 3.0);
        set_handle(&mut c, 1, 1.5, -3.0);
        assert_eq!((c.x1, c.y1, c.x2, c.y2), (0.0, 1.0 + ROOM, 1.0, -ROOM));
    }
}
