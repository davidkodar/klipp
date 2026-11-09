//! Frame Glide in the viewer (#611): the Start and End framings drawn
//! over the clip's whole picture, moved by dragging inside one and sized
//! by dragging a corner. A drag is one edit, sent when it is let go.
//!
//! Both framings keep the sequence's aspect (they are the output's
//! shape), and stay inside the frame so the moving picture always fills
//! it. Start is drawn solid and End dashed, each named, so the two are
//! told apart without their colours.

use klipp_core::frame_glide::{FrameGlide, GlideFrame};
use klipp_core::{AppCommand, ClipId};

use crate::state::AppState;

/// Which framing a drag holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Start,
    End,
}

/// What a drag does to its framing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Grip {
    /// Moves it; `from` is where the pointer took it.
    Move { from: egui::Pos2 },
    /// Sizes it about the opposite corner, `0..4` clockwise from top left.
    Corner(usize),
}

/// A drag in progress, and the glide as it stands with it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlideDrag {
    pub clip_id: ClipId,
    pub which: End,
    pub grip: Grip,
    pub before: FrameGlide,
    pub glide: FrameGlide,
}

/// A framing on screen, over `frame` (the frame as the viewer draws it).
pub fn to_screen(f: GlideFrame, frame: egui::Rect) -> egui::Rect {
    let (w, h) = (frame.width(), frame.height());
    egui::Rect::from_center_size(frame.center() + egui::vec2(f.x * w, f.y * h), egui::vec2(f.width * w, f.width * h))
}

/// `f` held to a framing the frame can fill: no narrower than the
/// smallest push, no wider than the frame, and inside it.
pub fn clamp(f: GlideFrame) -> GlideFrame {
    let width = f.width.clamp(GlideFrame::MIN_WIDTH, 1.0);
    let room = (1.0 - width) / 2.0;
    GlideFrame { x: f.x.clamp(-room, room), y: f.y.clamp(-room, room), width }
}

/// `f` moved by `delta` screen points over `frame`.
pub fn moved(f: GlideFrame, delta: egui::Vec2, frame: egui::Rect) -> GlideFrame {
    clamp(GlideFrame { x: f.x + delta.x / frame.width(), y: f.y + delta.y / frame.height(), ..f })
}

/// `f` sized by its `corner` dragged to `to`, about the opposite corner,
/// keeping the frame's aspect.
pub fn resized(f: GlideFrame, corner: usize, to: egui::Pos2, frame: egui::Rect) -> GlideFrame {
    let r = to_screen(f, frame);
    let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()];
    let anchor = corners[(corner + 2) % 4];
    let (w, h) = (frame.width(), frame.height());
    let width = ((to.x - anchor.x).abs() / w).max((to.y - anchor.y).abs() / h).clamp(GlideFrame::MIN_WIDTH, 1.0);
    // The new framing grows from the anchor toward the dragged corner.
    let sx = if corners[corner].x >= anchor.x { 1.0 } else { -1.0 };
    let sy = if corners[corner].y >= anchor.y { 1.0 } else { -1.0 };
    let centre = anchor + egui::vec2(sx * width * w / 2.0, sy * width * h / 2.0);
    clamp(GlideFrame { x: (centre.x - frame.center().x) / w, y: (centre.y - frame.center().y) / h, width })
}

/// Which framing and grip a press at `p` takes: a corner of either first,
/// End before Start where they overlap, then the inside of either.
pub fn grip_at(glide: &FrameGlide, p: egui::Pos2, frame: egui::Rect, corner_reach: f32) -> Option<(End, Grip)> {
    let order = [(End::End, glide.end), (End::Start, glide.start)];
    for (which, f) in order {
        let r = to_screen(f, frame);
        let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()];
        if let Some(c) = corners.iter().position(|c| (*c - p).length() <= corner_reach) {
            return Some((which, Grip::Corner(c)));
        }
    }
    order.iter().find(|(_, f)| to_screen(*f, frame).contains(p)).map(|(which, _)| (*which, Grip::Move { from: p }))
}

/// The glide with `which` framing replaced.
fn with(glide: FrameGlide, which: End, f: GlideFrame) -> FrameGlide {
    match which {
        End::Start => FrameGlide { start: f, ..glide },
        End::End => FrameGlide { end: f, ..glide },
    }
}

/// Draw the tool over `frame` and run its drags. Escape leaves the mode.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, frame: egui::Rect, cmds: &mut Vec<AppCommand>) {
    let Some(clip_id) = state.frame_glide_clip else { return };
    let Some(clip) = state.sequence().all_clips().find(|c| c.id == clip_id) else {
        cmds.push(AppCommand::SetFrameGlideMode(None));
        return;
    };
    let stored = clip.frame_glide.unwrap_or_default();
    let mut glide = state.frame_glide_drag.filter(|d| d.clip_id == clip_id).map_or(stored, |d| d.glide);

    let (pointer, pressed, down, released, escape) = ui.input(|i| {
        (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released(), i.key_pressed(egui::Key::Escape))
    });
    if escape {
        state.frame_glide_drag = None;
        cmds.push(AppCommand::SetFrameGlideMode(None));
        return;
    }
    // The tool has the viewer to itself: nothing under it takes the drag.
    let response = ui.interact(frame, ui.id().with("frame_glide_tool"), egui::Sense::click_and_drag());
    // Done here too: the inspector's moves down as the clip gains keys, away
    // from a pointer on its way to it. Read from the pointer, as the drags
    // are: what the viewer draws after the tool would take a widget's click.
    let done_at = done_rect(frame);
    let over_done = pointer.is_some_and(|p| done_at.contains(p));
    // A fixed id: the viewer's own ids move with what it draws, between a
    // press and its release.
    let held = egui::Id::new("frame_glide_done_held");
    if pressed {
        ui.ctx().data_mut(|d| d.insert_temp(held, over_done));
    }
    if released {
        let pressed_on_done = ui.ctx().data_mut(|d| d.remove_temp::<bool>(held)).unwrap_or(false);
        if pressed_on_done && over_done && state.frame_glide_drag.is_none() {
            cmds.push(AppCommand::SetFrameGlideMode(None));
        }
    }
    if let Some(p) = pointer {
        if pressed && response.hovered() && !done_at.contains(p) && state.frame_glide_drag.is_none() {
            if let Some((which, grip)) = grip_at(&glide, p, frame, 12.0) {
                state.frame_glide_drag = Some(GlideDrag { clip_id, which, grip, before: glide, glide });
            }
        }
        if down {
            if let Some(drag) = state.frame_glide_drag.as_mut() {
                let f = match drag.which {
                    End::Start => drag.before.start,
                    End::End => drag.before.end,
                };
                let f = match drag.grip {
                    Grip::Move { from } => moved(f, p - from, frame),
                    Grip::Corner(c) => resized(f, c, p, frame),
                };
                drag.glide = with(drag.before, drag.which, f);
                glide = drag.glide;
            }
        }
    }
    if released {
        if let Some(drag) = state.frame_glide_drag.take() {
            if drag.glide != drag.before {
                cmds.push(AppCommand::SetFrameGlide { clip_id, glide: Some(drag.glide), replace_animation: false });
            }
        }
    }

    let painter = ui.painter().with_clip_rect(frame.expand(2.0));
    // The picture outside both framings dimmed a little: what the move
    // never shows.
    painter.rect_filled(frame, 0.0, egui::Color32::from_black_alpha(40));
    let start_colour = egui::Color32::from_rgb(110, 210, 140);
    let end_colour = egui::Color32::from_rgb(245, 170, 80);
    for (which, f, colour) in [(End::Start, glide.start, start_colour), (End::End, glide.end, end_colour)] {
        let r = to_screen(f, frame);
        let stroke = egui::Stroke::new(2.0_f32, colour);
        let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()];
        for i in 0..4 {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            if which == End::Start {
                painter.line_segment([a, b], stroke);
            } else {
                painter.extend(egui::Shape::dashed_line(&[a, b], stroke, 8.0, 5.0));
            }
        }
        for c in corners {
            painter.rect_filled(egui::Rect::from_center_size(c, egui::vec2(9.0, 9.0)), 1.0, colour);
        }
        let label = if which == End::Start { "Start" } else { "End" };
        let at = if which == End::Start { r.left_top() + egui::vec2(6.0, 6.0) } else { r.right_bottom() - egui::vec2(6.0, 6.0) };
        let align = if which == End::Start { egui::Align2::LEFT_TOP } else { egui::Align2::RIGHT_BOTTOM };
        let galley = painter.layout_no_wrap(label.to_string(), egui::FontId::proportional(13.0), egui::Color32::BLACK);
        let bg = align.anchor_size(at, galley.size()).expand(3.0);
        painter.rect_filled(bg, 3.0, colour);
        painter.galley(bg.min + egui::vec2(3.0, 3.0), galley, egui::Color32::BLACK);
    }
    painter.text(
        frame.center_bottom() - egui::vec2(0.0, 10.0),
        egui::Align2::CENTER_BOTTOM,
        "Drag to move \u{00b7} corner to size \u{00b7} Esc: done",
        egui::FontId::proportional(12.0),
        egui::Color32::from_white_alpha(220),
    );
    painter.rect_filled(done_at, 3.0, if over_done { egui::Color32::from_gray(70) } else { egui::Color32::from_black_alpha(190) });
    painter.text(done_at.center(), egui::Align2::CENTER_CENTER, "Done", egui::FontId::proportional(13.0), egui::Color32::WHITE);
    if state.frame_glide_drag.is_some() {
        ui.ctx().request_repaint();
    }
}

/// The tool's Done button, at the top of the frame.
fn done_rect(frame: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(frame.center_top() + egui::vec2(0.0, 22.0), egui::vec2(64.0, 24.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(640.0, 360.0))
    }

    #[test]
    fn a_framing_is_drawn_where_it_is() {
        let r = to_screen(GlideFrame::FULL, frame());
        assert_eq!(r, frame());
        let half = to_screen(GlideFrame { x: 0.25, y: 0.0, width: 0.5 }, frame());
        assert_eq!((half.min.x, half.max.x, half.height()), (420.0, 740.0, 180.0), "right half, the frame's shape");
    }

    #[test]
    fn a_framing_stays_inside_the_frame() {
        let f = moved(GlideFrame { x: 0.0, y: 0.0, width: 0.5 }, egui::vec2(10_000.0, 0.0), frame());
        assert_eq!(f.x, 0.25, "stopped at the right edge");
        assert_eq!(clamp(GlideFrame { x: 0.0, y: 0.0, width: 3.0 }).width, 1.0);
        assert_eq!(clamp(GlideFrame { x: 0.0, y: 0.0, width: 0.0 }).width, GlideFrame::MIN_WIDTH);
    }

    /// A corner sizes the framing about the opposite one, keeping the shape.
    #[test]
    fn a_corner_sizes_about_the_opposite_corner() {
        let full = GlideFrame::FULL;
        // Top-left corner dragged to the frame's centre: the bottom-right
        // quarter's diagonal, so the framing is the bottom-right half-size.
        let f = resized(full, 0, frame().center(), frame());
        assert!((f.width - 0.5).abs() < 1e-5 && (f.x - 0.25).abs() < 1e-5 && (f.y - 0.25).abs() < 1e-5, "{f:?}");
        let r = to_screen(f, frame());
        assert!((r.max - frame().max).length() < 1e-3, "the opposite corner stayed put");
    }

    #[test]
    fn a_press_takes_the_framing_under_it() {
        let glide = FrameGlide { start: GlideFrame::FULL, end: GlideFrame { x: 0.0, y: 0.0, width: 0.5 }, ..FrameGlide::default() };
        assert_eq!(grip_at(&glide, frame().center(), frame(), 12.0).map(|g| g.0), Some(End::End), "End over Start");
        assert_eq!(grip_at(&glide, frame().left_top() + egui::vec2(3.0, 3.0), frame(), 12.0), Some((End::Start, Grip::Corner(0))));
        assert_eq!(grip_at(&glide, frame().left_top() + egui::vec2(40.0, 40.0), frame(), 12.0).map(|g| g.0), Some(End::Start));
        assert_eq!(grip_at(&glide, egui::pos2(0.0, 0.0), frame(), 12.0), None);
    }

    /// The Done button on the viewer ends the tool on a click, and the press
    /// on it starts no drag of the framing under it.
    #[test]
    fn done_on_the_viewer_ends_the_tool_and_drags_nothing() {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let id = clip.id;
        let mut state = AppState::default();
        let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        state.sequences[0].tracks[v1].clips.push(clip);
        state.frame_glide_clip = Some(id);
        let ctx = egui::Context::default();
        let at = done_rect(frame()).center();
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut sent = Vec::new();
        for events in [Vec::new(), vec![egui::Event::PointerMoved(at)], vec![press(true)], vec![press(false)], Vec::new()] {
            let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 500.0))), events, ..Default::default() };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show(ui, &mut state, frame(), &mut sent));
            });
            assert!(state.frame_glide_drag.is_none(), "no drag from the press on it");
        }
        assert!(sent.iter().any(|c| matches!(c, AppCommand::SetFrameGlideMode(None))), "{sent:?}");
        assert!(!sent.iter().any(|c| matches!(c, AppCommand::SetFrameGlide { .. })), "nothing written");
    }
}
