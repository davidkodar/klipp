//! Where a frozen track plays live (#3207), marked on its lane: amber
//! hatching over frames not drawn yet or that couldn't be, dim hatching
//! over frames that read other tracks and always play live.

use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::timeline::time_utils;
use klipp_core::Timecode;
use klipp_timeline::TrackKind;

/// Freeze the selected clip's video track, or play it live again: what
/// Freeze Track's key and palette entry do. `None` with no clip selected
/// on a video track.
pub fn toggle_selected(state: &AppState) -> Option<klipp_core::AppCommand> {
    let seq = state.sequence();
    let (ti, _) = seq.find_clip(state.selected_clip?)?;
    let t = seq.tracks.get(ti).filter(|t| t.kind == TrackKind::Video)?;
    Some(klipp_core::AppCommand::SetTrackFrozen { track_id: t.id, frozen: !t.frozen })
}

/// The spans to hatch on track `id`'s lane: (stale, live).
pub fn spans(id: klipp_core::TrackId) -> Option<(Vec<(Timecode, Timecode)>, Vec<(Timecode, Timecode)>)> {
    klipp_engine::freeze::status(id).map(|s| (s.stale, s.live))
}

/// Hatch the frozen tracks' live spans over their lanes, `rects` as the
/// timeline laid them out this frame.
#[allow(clippy::too_many_arguments)]
pub fn draw(ui: &egui::Ui, tracks_rect: egui::Rect, rects: &[(usize, TrackKind, egui::Rect)], ruler_left: f32, state: &AppState, theme: &KlippTheme, pps: f32, scroll_x: f32) {
    let painter = ui.painter().with_clip_rect(tracks_rect);
    let seq = state.sequence();
    for (ti, kind, lane) in rects {
        let Some(track) = seq.tracks.get(*ti).filter(|t| t.frozen && *kind == TrackKind::Video) else { continue };
        let Some((stale, live)) = spans(track.id) else { continue };
        for (spans, colour) in [(&live, theme.text_dim), (&stale, theme.marker_warning)] {
            for (a, b) in spans {
                let x0 = ruler_left + time_utils::time_to_x(a.to_secs_f64(), pps, scroll_x);
                let x1 = ruler_left + time_utils::time_to_x(b.to_secs_f64(), pps, scroll_x);
                let r = egui::Rect::from_min_max(egui::pos2(x0.max(lane.left()), lane.top()), egui::pos2(x1.min(lane.right()), lane.bottom()));
                if r.width() <= 0.0 {
                    continue;
                }
                hatch(&painter, r, colour);
            }
        }
    }
}

/// Diagonal lines across `r`, faint, and a line along its foot.
fn hatch(painter: &egui::Painter, r: egui::Rect, colour: egui::Color32) {
    let faint = colour.gamma_multiply(0.35);
    let p = painter.with_clip_rect(r.intersect(painter.clip_rect()));
    let gap = 8.0;
    let mut x = r.left() - r.height();
    while x < r.right() {
        p.line_segment([egui::pos2(x, r.bottom()), egui::pos2(x + r.height(), r.top())], egui::Stroke::new(1.0_f32, faint));
        x += gap;
    }
    painter.rect_filled(egui::Rect::from_min_max(egui::pos2(r.left(), r.bottom() - 2.0), r.max), 0.0, colour.gamma_multiply(0.8));
}
