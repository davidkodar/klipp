//! The sequence's markers on the ruler (#909): a flag for each, a shaded
//! band for a range marker, a faint line down through the tracks, and a
//! tooltip with the marker's kind, label and note. A click jumps to the
//! marker, a double-click opens it in the Markers panel to edit, and a drag
//! moves it, as one undo step when it is let go. They were only drawn by
//! the timeline the app no longer shows, so a marker added with M could be
//! listed but never seen.

use crate::state::AppState;
use crate::timeline::time_utils;
use klipp_core::{AppCommand, MarkerId};

/// A marker being dragged and where it would land, kept between frames.
fn drag_id() -> egui::Id {
    egui::Id::new("timeline_marker_drag")
}

/// Draw the markers over `ruler_rect` and take the clicks and drags on
/// them. Their lines through the tracks are [`draw_lines`], drawn after
/// the tracks so the clips do not cover them.
pub fn show(
    ui: &mut egui::Ui,
    ruler_rect: egui::Rect,
    state: &mut AppState,
    pps: f32,
    scroll_x: f32,
    cmds: &mut Vec<AppCommand>,
) {
    let painter = ui.painter().with_clip_rect(ruler_rect);
    let dragging: Option<(MarkerId, f64)> = ui.data(|d| d.get_temp(drag_id()));
    let x_of = |secs: f64| ruler_rect.left() + time_utils::time_to_x(secs, pps, scroll_x);
    let markers: Vec<klipp_timeline::Marker> = state.sequence().markers.clone();
    for marker in &markers {
        let secs = match dragging {
            Some((id, at)) if id == marker.id => at,
            _ => marker.position.to_secs_f64(),
        };
        let x = x_of(secs);
        if x < ruler_rect.left() - 8.0 || x > ruler_rect.right() + 8.0 {
            continue;
        }
        let (r, g, b) = marker.color.to_rgb();
        let color = egui::Color32::from_rgb(r, g, b);
        if let Some(dur) = marker.duration {
            let x_end = x_of(secs + dur.to_secs_f64()).min(ruler_rect.right());
            if x_end > x {
                let band = egui::Rect::from_min_max(egui::pos2(x.max(ruler_rect.left()), ruler_rect.top()), egui::pos2(x_end, ruler_rect.bottom()));
                painter.rect_filled(band, 0.0, egui::Color32::from_rgba_unmultiplied(r, g, b, 50));
                painter.line_segment(
                    [egui::pos2(x_end, ruler_rect.top()), egui::pos2(x_end, ruler_rect.bottom())],
                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(r, g, b, 160)),
                );
            }
        }
        let (top, bot, half_w) = (ruler_rect.bottom() - 11.0, ruler_rect.bottom() - 1.0, 5.0);
        painter.add(egui::Shape::convex_polygon(
            vec![egui::pos2(x - half_w, top), egui::pos2(x + half_w, top), egui::pos2(x, bot)],
            color,
            egui::Stroke::NONE,
        ));

        let flag = egui::Rect::from_min_max(egui::pos2(x - half_w - 2.0, top - 2.0), egui::pos2(x + half_w + 2.0, bot + 1.0));
        let resp = ui.interact(flag, ui.id().with(("timeline_marker", marker.id.0)), egui::Sense::click_and_drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        let resp = resp.on_hover_text(tooltip(marker));
        if resp.double_clicked() {
            state.edit_marker_request = Some(marker.id);
            state.show_markers_panel = true;
        } else if resp.clicked() {
            cmds.push(AppCommand::JumpToMarker { marker_id: marker.id });
        }
        if resp.dragged() {
            if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                let at = time_utils::x_to_time(pos.x - ruler_rect.left(), pps, scroll_x).max(0.0);
                ui.data_mut(|d| d.insert_temp(drag_id(), (marker.id, at)));
            }
        }
        if resp.drag_stopped() {
            if let Some((id, at)) = ui.data_mut(|d| d.remove_temp::<(MarkerId, f64)>(drag_id())) {
                if id == marker.id {
                    let position = klipp_core::RationalTime::from_secs_f64(at, state.fps.rate());
                    if position != marker.position {
                        cmds.push(AppCommand::MoveMarker { marker_id: id, position });
                    }
                }
            }
        }
    }
}

/// A faint line down through the tracks at each marker, so a cut can be
/// lined up on it.
pub fn draw_lines(ui: &egui::Ui, tracks_rect: egui::Rect, ruler_left: f32, state: &AppState, pps: f32, scroll_x: f32) {
    let painter = ui.painter().with_clip_rect(tracks_rect);
    let dragging: Option<(MarkerId, f64)> = ui.data(|d| d.get_temp(drag_id()));
    for marker in &state.sequence().markers {
        let secs = match dragging {
            Some((id, at)) if id == marker.id => at,
            _ => marker.position.to_secs_f64(),
        };
        let x = ruler_left + time_utils::time_to_x(secs, pps, scroll_x);
        let (r, g, b) = marker.color.to_rgb();
        painter.line_segment(
            [egui::pos2(x, tracks_rect.top()), egui::pos2(x, tracks_rect.bottom())],
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(r, g, b, 110)),
        );
    }
}

/// What a marker's tooltip says: its kind and label, its length when it
/// is a range, and its note.
pub fn tooltip(marker: &klipp_timeline::Marker) -> String {
    let label = if marker.label.is_empty() { "(no label)".to_string() } else { marker.label.clone() };
    let mut lines = vec![if marker.kind != klipp_core::MarkerKind::None { format!("{} \u{2014} {label}", marker.kind.label()) } else { label }];
    if let Some(d) = marker.duration {
        lines.push(format!("{:.2} s long", d.to_secs_f64()));
    }
    if !marker.comment.is_empty() {
        lines.push(marker.comment.clone());
    }
    lines.push("Click to jump, double-click to edit, drag to move".into());
    lines.join("\n")
}
