//! Editing a freeform mask's path in the viewer (#74): the geometry the
//! Mask tool's gestures need, free of egui so it can be tested on its own.
//!
//! Points are in frame fractions; a handle is an offset from its point.
//! Lengths and directions are compared in frame widths both ways
//! (`aspect` is the frame's height over its width), where a turn is a
//! turn on the picture.

use klipp_core::mask_path::BezierPoint;

type P = (f32, f32);

fn lerp(a: P, b: P, t: f32) -> P {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// The cubic from point `i` to the next, its four control points.
fn segment(points: &[BezierPoint], i: usize) -> [P; 4] {
    let (a, b) = (&points[i], &points[(i + 1) % points.len()]);
    [a.pos(), a.out_handle(), b.in_handle(), b.pos()]
}

fn cubic_at(c: &[P; 4], t: f32) -> P {
    let (ab, bc, cd) = (lerp(c[0], c[1], t), lerp(c[1], c[2], t), lerp(c[2], c[3], t));
    lerp(lerp(ab, bc, t), lerp(bc, cd, t), t)
}

pub use klipp_core::mask_path::split_segment;

/// The point on the path nearest `at`, as (segment, `t` along it, its
/// position, its distance in frame widths), searched by sampling each
/// segment and refining about the best sample. `None` for no segment.
pub fn nearest_on_path(points: &[BezierPoint], at: P, aspect: f32) -> Option<(usize, f32, P, f32)> {
    if points.len() < 2 {
        return None;
    }
    let dist = |p: P| ((p.0 - at.0).powi(2) + ((p.1 - at.1) * aspect).powi(2)).sqrt();
    let mut best: Option<(usize, f32, P, f32)> = None;
    for seg in 0..points.len() {
        let c = segment(points, seg);
        const STEPS: usize = 32;
        let mut seg_best = (0.0f32, f32::MAX);
        for k in 0..=STEPS {
            let t = k as f32 / STEPS as f32;
            let d = dist(cubic_at(&c, t));
            if d < seg_best.1 {
                seg_best = (t, d);
            }
        }
        // Halving steps about the best sample.
        let mut step = 1.0 / STEPS as f32;
        let mut t = seg_best.0;
        for _ in 0..12 {
            step *= 0.5;
            for cand in [t - step, t + step] {
                let cand = cand.clamp(0.0, 1.0);
                let d = dist(cubic_at(&c, cand));
                if d < seg_best.1 {
                    seg_best = (cand, d);
                    t = cand;
                }
            }
        }
        if best.is_none_or(|b| seg_best.1 < b.3) {
            best = Some((seg, seg_best.0, cubic_at(&c, seg_best.0), seg_best.1));
        }
    }
    best
}

/// Whether `at` is inside the closed path, as its fill has it.
pub fn inside(points: &[BezierPoint], at: P, aspect: f32) -> bool {
    let outline = klipp_core::mask_path::flatten(points, 1e-3, aspect);
    let n = outline.len();
    if n < 3 {
        return false;
    }
    let mut winding = 0i32;
    for i in 0..n {
        let (a, b) = (outline[i], outline[(i + 1) % n]);
        let cross = (b.0 - a.0) * (at.1 - a.1) - (b.1 - a.1) * (at.0 - a.0);
        if a.1 <= at.1 {
            if b.1 > at.1 && cross > 0.0 {
                winding += 1;
            }
        } else if b.1 <= at.1 && cross < 0.0 {
            winding -= 1;
        }
    }
    winding != 0
}

/// Point `i` made smooth: handles along the line from the point before
/// it to the one after, each a third of the way to its neighbour, as a
/// curve drawn through the three would run.
pub fn make_smooth(points: &[BezierPoint], i: usize, aspect: f32) -> BezierPoint {
    let n = points.len();
    let p = points[i];
    if n < 3 {
        return BezierPoint { smooth: true, ..p };
    }
    let (prev, next) = (points[(i + n - 1) % n].pos(), points[(i + 1) % n].pos());
    // In widths, so the line is the one seen on the picture.
    let w = |q: P| (q.0, q.1 * aspect);
    let (pw, a, b) = (w(p.pos()), w(prev), w(next));
    let dir = (b.0 - a.0, b.1 - a.1);
    let len = (dir.0 * dir.0 + dir.1 * dir.1).sqrt();
    if len < 1e-6 {
        return BezierPoint { smooth: true, ..p };
    }
    let u = (dir.0 / len, dir.1 / len);
    let to = |q: P| ((q.0 - pw.0).powi(2) + (q.1 - pw.1).powi(2)).sqrt() / 3.0;
    let (li, lo) = (to(a), to(b));
    BezierPoint { in_dx: -u.0 * li, in_dy: -u.1 * li / aspect, out_dx: u.0 * lo, out_dy: u.1 * lo / aspect, smooth: true, ..p }
}

/// Point `p` made a corner: no handles.
pub fn make_corner(p: BezierPoint) -> BezierPoint {
    BezierPoint::corner(p.x, p.y)
}

/// Point `p` with its `out` (else in) handle's end dragged to `end`. A
/// smooth point keeps its other handle in line, at its own length;
/// `break_smooth` (Alt) makes it a corner whose handles move apart.
pub fn drag_handle(p: BezierPoint, out: bool, end: P, break_smooth: bool, aspect: f32) -> BezierPoint {
    let (dx, dy) = (end.0 - p.x, end.1 - p.y);
    let mut q = p;
    if out {
        q.out_dx = dx;
        q.out_dy = dy;
    } else {
        q.in_dx = dx;
        q.in_dy = dy;
    }
    if break_smooth {
        q.smooth = false;
        return q;
    }
    if p.smooth {
        let (ox, oy) = if out { (p.in_dx, p.in_dy) } else { (p.out_dx, p.out_dy) };
        let other_len = (ox * ox + (oy * aspect).powi(2)).sqrt();
        let len = (dx * dx + (dy * aspect).powi(2)).sqrt();
        if len > 1e-6 {
            let (ux, uy) = (-dx / len, -dy * aspect / len);
            let (nx, ny) = (ux * other_len, uy * other_len / aspect);
            if out {
                q.in_dx = nx;
                q.in_dy = ny;
            } else {
                q.out_dx = nx;
                q.out_dy = ny;
            }
        }
    }
    q
}

/// `points` with those in `selected` moved by (`dx`, `dy`), their
/// handles with them.
pub fn move_points(points: &[BezierPoint], selected: &std::collections::BTreeSet<usize>, dx: f32, dy: f32) -> Vec<BezierPoint> {
    points
        .iter()
        .enumerate()
        .map(|(i, p)| if selected.contains(&i) { BezierPoint { x: p.x + dx, y: p.y + dy, ..*p } } else { *p })
        .collect()
}

pub use klipp_core::mask_path::without_points as delete_points;

/// The points inside the rectangle from `a` to `b`.
pub fn in_marquee(points: &[BezierPoint], a: P, b: P) -> std::collections::BTreeSet<usize> {
    let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
    let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
    points.iter().enumerate().filter(|(_, p)| p.x >= x0 && p.x <= x1 && p.y >= y0 && p.y <= y1).map(|(i, _)| i).collect()
}

/// A point the pen puts down at `at`, its handles pulled to `drag` (the
/// out handle there, the in handle opposite, as the pen draws): a corner
/// when there was no drag.
pub fn pen_point(at: P, drag: Option<P>) -> BezierPoint {
    match drag {
        Some(d) if (d.0 - at.0).abs() > 1e-5 || (d.1 - at.1).abs() > 1e-5 => {
            let (dx, dy) = (d.0 - at.0, d.1 - at.1);
            BezierPoint { x: at.0, y: at.1, out_dx: dx, out_dy: dy, in_dx: -dx, in_dy: -dy, smooth: true }
        }
        _ => BezierPoint::corner(at.0, at.1),
    }
}


// ── The Mask tool in the viewer ─────────────────────────────────────────

/// What the Mask tool is doing (#74): kept on the app state between
/// frames, never saved.
#[derive(Debug, Default, Clone)]
pub struct MaskEdit {
    /// The freeform mask whose points are being edited: its Mask node,
    /// and its place in the node's stack.
    pub mask: Option<(klipp_nodes::NodeId, usize)>,
    /// Its selected points.
    pub selected: std::collections::BTreeSet<usize>,
    /// A shape being drawn with the pen, not a mask until it is closed.
    pub pen: Vec<BezierPoint>,
    /// A press in progress.
    pub gesture: Option<Gesture>,
    /// A drawn shape was just closed: its points are selected once it
    /// is in the stack.
    pub select_new: Option<usize>,
}

impl MaskEdit {
    /// Whether the tool has something Delete, Enter or Escape act on:
    /// selected points, or a shape being drawn.
    pub fn has_keys(&self) -> bool {
        !self.selected.is_empty() || !self.pen.is_empty()
    }
}

/// A press the Mask tool is following, in frame fractions as the shape
/// was drawn.
#[derive(Debug, Clone)]
pub enum Gesture {
    /// The pen putting a point down at `at`, its handle pulled to `to`.
    Pen { at: P, to: P },
    /// The selected points moved from `start`.
    Points { start: P, initial: Vec<BezierPoint>, began: bool },
    /// A handle of point `index` dragged (`out`, else in).
    Handle { index: usize, out: bool, initial: Vec<BezierPoint>, began: bool },
    /// The whole shape moved from `start`.
    Shape { start: P, initial: Vec<BezierPoint>, began: bool },
    /// A marquee from `start` to `now`.
    Marquee { start: P, now: P, add: bool },
}

/// How a mask's points go to the screen and back: through the window's
/// tracked motion and its plane map, as the rest of the overlay draws it.
struct View {
    rect: egui::Rect,
    motion: Option<klipp_core::WindowMotion>,
    pivot: P,
    /// Now to as drawn (a perspective track's), and back.
    back: Option<klipp_core::PlaneMap>,
    ahead: Option<klipp_core::PlaneMap>,
    aspect: f32,
}

impl View {
    fn plain(rect: egui::Rect) -> Self {
        Self { rect, motion: None, pivot: (0.5, 0.5), back: None, ahead: None, aspect: rect.height() / rect.width().max(1.0) }
    }

    fn of(rect: egui::Rect, mask: &klipp_core::Mask, time_secs: f64) -> Self {
        let mut v = Self::plain(rect);
        v.motion = mask.track_at(time_secs);
        v.pivot = mask.evaluated_at(time_secs).shape.pivot();
        v.back = mask.evaluated_in(time_secs, v.aspect).keyframes.warp;
        v.ahead = v.back.and_then(|w| w.inverse());
        v
    }

    fn screen(&self, p: P) -> egui::Pos2 {
        let mut q = p;
        // Moved as `MaskShape::moved` moves the shape's points, about its
        // pivot.
        if let Some(m) = self.motion {
            let (sin, cos) = m.rotation.to_radians().sin_cos();
            let (ux, uy) = (p.0 - self.pivot.0, (p.1 - self.pivot.1) * self.aspect);
            q = (self.pivot.0 + m.dx + m.scale * (ux * cos - uy * sin), self.pivot.1 + m.dy + m.scale * (ux * sin + uy * cos) / self.aspect);
        }
        if let Some(a) = self.ahead {
            q = a.apply(q).unwrap_or(q);
        }
        egui::pos2(self.rect.min.x + q.0 * self.rect.width(), self.rect.min.y + q.1 * self.rect.height())
    }

    fn frame(&self, s: egui::Pos2) -> P {
        let mut n = ((s.x - self.rect.min.x) / self.rect.width(), (s.y - self.rect.min.y) / self.rect.height());
        if let Some(b) = self.back {
            n = b.apply(n).unwrap_or(n);
        }
        if let Some(m) = self.motion {
            n = m.undo(n, self.pivot, self.aspect);
        }
        n
    }
}

/// The overlay's colours from the theme (#74): every mark drawn twice,
/// a dark stroke under a light one, so it reads on bright footage and
/// dark alike.
struct Ink {
    under: egui::Color32,
    line: egui::Color32,
    edited: egui::Color32,
    selected: egui::Color32,
    point: egui::Color32,
}

impl Ink {
    fn of(theme: &crate::theme::KlippTheme) -> Self {
        Self {
            under: egui::Color32::from_black_alpha(170),
            line: theme.text_primary,
            edited: theme.accent,
            selected: theme.accent,
            point: theme.bg_deep,
        }
    }

    fn polyline(&self, painter: &egui::Painter, pts: Vec<egui::Pos2>, closed: bool, colour: egui::Color32) {
        let under = egui::Stroke::new(3.0_f32, self.under);
        let over = egui::Stroke::new(1.4_f32, colour);
        if closed {
            painter.add(egui::Shape::closed_line(pts.clone(), under));
            painter.add(egui::Shape::closed_line(pts, over));
        } else {
            painter.add(egui::Shape::line(pts.clone(), under));
            painter.add(egui::Shape::line(pts, over));
        }
    }

    fn handle_line(&self, painter: &egui::Painter, a: egui::Pos2, b: egui::Pos2) {
        painter.line_segment([a, b], egui::Stroke::new(2.5_f32, self.under));
        painter.line_segment([a, b], egui::Stroke::new(1.0_f32, self.line));
    }

    fn point(&self, painter: &egui::Painter, at: egui::Pos2, selected: bool) {
        let r = egui::Rect::from_center_size(at, egui::vec2(7.0, 7.0));
        painter.rect_filled(r.expand(1.5), 1.0, self.under);
        painter.rect_filled(r, 1.0, if selected { self.selected } else { self.point });
        painter.rect_stroke(r, 1.0, egui::Stroke::new(1.0_f32, self.line));
    }

    fn handle_end(&self, painter: &egui::Painter, at: egui::Pos2) {
        painter.circle_filled(at, 4.5, self.under);
        painter.circle_filled(at, 3.0, self.line);
    }

    fn insert_marker(&self, painter: &egui::Painter, at: egui::Pos2) {
        painter.circle_filled(at, 7.0, self.under);
        painter.circle_stroke(at, 5.5, egui::Stroke::new(1.4_f32, self.edited));
        let s = egui::Stroke::new(1.4_f32, self.line);
        painter.line_segment([at - egui::vec2(3.0, 0.0), at + egui::vec2(3.0, 0.0)], s);
        painter.line_segment([at - egui::vec2(0.0, 3.0), at + egui::vec2(0.0, 3.0)], s);
    }
}

/// How near, in screen pixels, a press must be to take a point, a handle
/// or the curve.
const HIT_PX: f32 = 8.0;

/// The grade's Mask node the overlay edits on `clip`: the selected one,
/// else the first, with its masks.
fn mask_node(state: &crate::state::AppState, clip: klipp_core::ClipId) -> Option<(klipp_nodes::NodeId, klipp_core::MaskStack)> {
    use klipp_timeline::GradeAccess;
    let clip = state.sequence().tracks.iter().find_map(|t| t.find_clip(clip))?;
    let grade = clip.grade();
    let is_mask = |n: &&klipp_nodes::Node| n.kind == klipp_nodes::NodeKind::Mask;
    let node = state.selected_node_id.and_then(|id| grade.nodes.iter().filter(is_mask).find(|n| n.id == id)).or_else(|| grade.nodes.iter().find(is_mask))?;
    Some((node.id, node.params.masks.clone().unwrap_or_default()))
}

fn is_freeform(mask: &klipp_core::Mask) -> bool {
    matches!(mask.shape, klipp_core::MaskShape::Bezier { .. })
}


/// The Mask tool (#74), drawn over the picture in `rect` and following
/// the pointer and, while the viewer has the keys, Delete, Enter and
/// Escape: draw a shape with the pen, select points (Shift adds, a drag
/// on empty picture draws a marquee), drag them, their handles (Alt
/// breaks a smooth point) or the shape, Alt-click a point to make it
/// smooth or a corner, double-click the curve to add a point there.
/// Every edit is one undo step.
pub(crate) fn mask_tool(ui: &mut egui::Ui, state: &mut crate::state::AppState, rect: egui::Rect, cmds: &mut Vec<klipp_core::AppCommand>, theme: &crate::theme::KlippTheme) {
    use klipp_core::AppCommand;
    let Some(clip_id) = state.selected_clip else {
        state.mask_edit = MaskEdit::default();
        return;
    };
    // The viewer let go of the keys (a press in another panel): its
    // point selection and any shape being drawn go with them, so Delete
    // there is never taken for these points, nor these for its (#74).
    if state.active_panel != Some(crate::state::PanelKind::Viewer) && state.mask_edit.has_keys() {
        state.mask_edit.selected.clear();
        state.mask_edit.pen.clear();
        state.mask_edit.gesture = None;
    }
    let node = mask_node(state, clip_id);
    let time = state.sequence().tracks.iter().find_map(|t| t.find_clip(clip_id)).map_or(0.0, |c| (state.playhead_smooth_secs - c.timeline_in.to_secs_f64()).max(0.0));
    // A shape key is written at the canonical playhead, as the mask
    // list's keys are, and is the one there within half a frame.
    let key_time = state.sequence().tracks.iter().find_map(|t| t.find_clip(clip_id)).map_or(0.0, |c| (state.playhead.to_secs_f64() - c.timeline_in.to_secs_f64()).max(0.0));
    let key_tolerance = crate::inspector::keyframe_selection::frame_tolerance(state.sequence());
    let ink = Ink::of(theme);
    let painter = ui.painter_at(rect);
    let edit = &mut state.mask_edit;

    // A shape just closed is selected once it is in the stack.
    if let (Some(at), Some((node_id, masks))) = (edit.select_new, &node) {
        if let Some(points) = masks.masks.get(at).and_then(|m| m.path_at(time)) {
            edit.mask = Some((*node_id, at));
            edit.selected = (0..points.len()).collect();
            edit.select_new = None;
        }
    }
    // The edited mask still a freeform one on this node, unlocked.
    let edited = edit.mask.and_then(|(id, i)| {
        let (node_id, masks) = node.as_ref()?;
        (*node_id == id).then_some(())?;
        let m = masks.masks.get(i)?;
        (m.enabled && !m.locked && is_freeform(m)).then_some(i)
    });
    if edited.is_none() {
        edit.mask = None;
        edit.selected.clear();
    }

    let resp = ui.interact(rect, ui.id().with("mask_tool"), egui::Sense::click_and_drag());
    let (pressed, released, pointer, mods, double) = ui.input(|i| {
        (i.pointer.primary_pressed(), i.pointer.primary_released(), i.pointer.interact_pos(), i.modifiers, i.pointer.button_double_clicked(egui::PointerButton::Primary))
    });
    if pressed && resp.hovered() {
        state.active_panel = Some(crate::state::PanelKind::Viewer);
    }
    let edit = &mut state.mask_edit;
    let plain = View::plain(rect);

    // Every freeform mask, the edited one in the accent with its points.
    let mut hit_point: Option<(usize, usize)> = None;
    let mut hit_handle: Option<(usize, bool)> = None;
    let mut hit_inside: Option<usize> = None;
    let mut near_curve: Option<(usize, f32, egui::Pos2)> = None;
    if let Some((_, masks)) = &node {
        for (i, m) in masks.masks.iter().enumerate() {
            // As its shape keys have it now (#74).
            let Some(points) = m.path_at(time) else { continue };
            let points = &points;
            if !m.enabled || points.len() < 2 {
                continue;
            }
            let view = View::of(rect, m, time);
            let outline: Vec<egui::Pos2> = klipp_core::mask_path::flatten(points, 0.5 / rect.width().max(1.0), view.aspect).into_iter().map(|p| view.screen(p)).collect();
            let this = edited == Some(i);
            ink.polyline(&painter, outline, true, if this { ink.edited } else { ink.line.gamma_multiply(if m.locked { 0.5 } else { 1.0 }) });
            if m.locked {
                continue;
            }
            let pointer_frame = pointer.map(|s| view.frame(s));
            if this {
                // Handles of the selected points and their neighbours.
                let n = points.len();
                let shown: std::collections::BTreeSet<usize> = edit.selected.iter().flat_map(|&k| [k, (k + 1) % n, (k + n - 1) % n]).collect();
                for &k in &shown {
                    let p = points[k];
                    for (out, end) in [(false, p.in_handle()), (true, p.out_handle())] {
                        if end == p.pos() {
                            continue;
                        }
                        let (a, b) = (view.screen(p.pos()), view.screen(end));
                        ink.handle_line(&painter, a, b);
                        ink.handle_end(&painter, b);
                        if pointer.is_some_and(|s| s.distance(b) <= HIT_PX) && hit_handle.is_none() {
                            hit_handle = Some((k, out));
                        }
                    }
                }
            }
            for (k, p) in points.iter().enumerate() {
                let at = view.screen(p.pos());
                ink.point(&painter, at, this && edit.selected.contains(&k));
                if pointer.is_some_and(|s| s.distance(at) <= HIT_PX) && hit_point.is_none() {
                    hit_point = Some((i, k));
                }
            }
            if let Some(f) = pointer_frame {
                if this {
                    if let Some((seg, t, on, _)) = nearest_on_path(points, f, view.aspect) {
                        let s = view.screen(on);
                        if pointer.is_some_and(|p| p.distance(s) <= HIT_PX) {
                            near_curve = Some((seg, t, s));
                        }
                    }
                }
                if hit_inside.is_none() && inside(points, f, view.aspect) {
                    hit_inside = Some(i);
                }
            }
        }
    }
    if hit_point.is_none() && hit_handle.is_none() && edit.gesture.is_none() {
        if let Some((_, _, s)) = near_curve {
            ink.insert_marker(&painter, s);
        }
    }

    let masks_of = |node: &Option<(klipp_nodes::NodeId, klipp_core::MaskStack)>| node.as_ref().map(|(_, m)| m.clone()).unwrap_or_default();
    // An edit of mask `i`, written whole.
    let set_mask = |cmds: &mut Vec<AppCommand>, node: &Option<(klipp_nodes::NodeId, klipp_core::MaskStack)>, i: usize, edit: &dyn Fn(&klipp_core::Mask) -> Option<klipp_core::Mask>| {
        if let Some((node_id, masks)) = node {
            let mut masks = masks.clone();
            if let Some(m) = masks.masks.get_mut(i).and_then(|m| edit(m)) {
                masks.masks[i] = m;
                cmds.push(AppCommand::SetNodeMasks { clip_id, node_id: *node_id, masks });
            }
        }
    };
    // Its points now: the shape key at the playhead when the shape is
    // keyed (one added there when there is none), else the shape (#74).
    let set_points = |cmds: &mut Vec<AppCommand>, node: &Option<(klipp_nodes::NodeId, klipp_core::MaskStack)>, i: usize, points: Vec<BezierPoint>| {
        set_mask(cmds, node, i, &|m| Some(m.with_path_at(key_time, points.clone(), key_tolerance)));
    };
    let view_of = |i: usize| node.as_ref().and_then(|(_, m)| m.masks.get(i)).map_or(View::plain(rect), |m| View::of(rect, m, time));

    // A press: the pen, a handle, a point, the curve, the shape, or the
    // empty picture.
    if pressed && resp.hovered() {
        if let Some(s) = pointer {
            let f = plain.frame(s);
            if !edit.pen.is_empty() {
                let first = plain.screen(edit.pen[0].pos());
                if edit.pen.len() >= 3 && s.distance(first) <= HIT_PX {
                    close_pen(edit, &node, clip_id, cmds);
                } else {
                    edit.gesture = Some(Gesture::Pen { at: f, to: f });
                }
            } else if let (Some((k, out)), Some(i)) = (hit_handle, edited) {
                let initial = masks_of(&node).masks.get(i).and_then(|m| m.path_at(time)).unwrap_or_default();
                edit.gesture = Some(Gesture::Handle { index: k, out, initial, began: false });
            } else if let Some((i, k)) = hit_point {
                let points = masks_of(&node).masks.get(i).and_then(|m| m.path_at(time)).unwrap_or_default();
                if edit.mask.map(|(_, j)| j) != Some(i) {
                    edit.selected.clear();
                }
                edit.mask = node.as_ref().map(|(id, _)| (*id, i));
                if mods.alt {
                    // Alt-click: smooth or corner.
                    let mut changed = points.clone();
                    let aspect = view_of(i).aspect;
                    changed[k] = if points[k].is_corner() { make_smooth(&points, k, aspect) } else { make_corner(points[k]) };
                    set_points(cmds, &node, i, changed);
                    edit.selected = [k].into();
                } else {
                    if mods.shift {
                        if !edit.selected.remove(&k) {
                            edit.selected.insert(k);
                        }
                    } else if !edit.selected.contains(&k) {
                        edit.selected = [k].into();
                    }
                    let start = view_of(i).frame(s);
                    edit.gesture = Some(Gesture::Points { start, initial: points, began: false });
                }
            } else if let Some(i) = hit_inside.filter(|_| near_curve.is_none()) {
                let points = masks_of(&node).masks.get(i).and_then(|m| m.path_at(time)).unwrap_or_default();
                if edit.mask.map(|(_, j)| j) != Some(i) {
                    edit.selected.clear();
                }
                edit.mask = node.as_ref().map(|(id, _)| (*id, i));
                edit.gesture = Some(Gesture::Shape { start: view_of(i).frame(s), initial: points, began: false });
            } else if near_curve.is_some() {
                // On the curve, short of a double-click: nothing yet.
            } else if !edit.selected.is_empty() {
                edit.gesture = Some(Gesture::Marquee { start: f, now: f, add: mods.shift });
            } else {
                edit.mask = None;
                edit.gesture = Some(Gesture::Pen { at: f, to: f });
            }
        }
    }

    // A double-click on the curve: a point added there (egui tells of it
    // as the second press lets go).
    if let (true, Some((seg, t, _)), Some(i), None) = (double && resp.hovered(), near_curve, edited, hit_point) {
        // In every shape key alike, so each keeps as many points.
        set_mask(cmds, &node, i, &|m| Some(m.with_point_split(seg, t)));
        edit.selected = [seg + 1].into();
        edit.gesture = None;
    }

    // The press followed.
    if let (Some(gesture), Some(s)) = (edit.gesture.as_mut(), pointer) {
        let mask_i = edit.mask.map(|(_, i)| i);
        match gesture {
            Gesture::Pen { to, .. } => *to = plain.frame(s),
            Gesture::Marquee { now, .. } => *now = plain.frame(s),
            Gesture::Points { start, initial, began } => {
                if let Some(i) = mask_i {
                    let f = view_of(i).frame(s);
                    let (dx, dy) = (f.0 - start.0, f.1 - start.1);
                    if dx != 0.0 || dy != 0.0 {
                        if !*began {
                            cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                            *began = true;
                        }
                        set_points(cmds, &node, i, move_points(initial, &edit.selected, dx, dy));
                    }
                }
            }
            Gesture::Shape { start, initial, began } => {
                if let Some(i) = mask_i {
                    let f = view_of(i).frame(s);
                    let (dx, dy) = (f.0 - start.0, f.1 - start.1);
                    if dx != 0.0 || dy != 0.0 {
                        if !*began {
                            cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                            *began = true;
                        }
                        let all: std::collections::BTreeSet<usize> = (0..initial.len()).collect();
                        set_points(cmds, &node, i, move_points(initial, &all, dx, dy));
                    }
                }
            }
            Gesture::Handle { index, out, initial, began } => {
                if let Some(i) = mask_i {
                    let v = view_of(i);
                    let end = v.frame(s);
                    if let Some(p) = initial.get(*index) {
                        let moved = drag_handle(*p, *out, end, mods.alt, v.aspect);
                        if moved != *p {
                            if !*began {
                                cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                *began = true;
                            }
                            let mut points = initial.clone();
                            points[*index] = moved;
                            set_points(cmds, &node, i, points);
                        }
                    }
                }
            }
        }
    }
    if released {
        match edit.gesture.take() {
            Some(Gesture::Pen { at, to }) => {
                let drag = (plain.screen(to).distance(plain.screen(at)) > 3.0).then_some(to);
                edit.pen.push(pen_point(at, drag));
            }
            Some(Gesture::Marquee { start, now, add }) => {
                if let Some(points) = edit.mask.and_then(|(_, i)| masks_of(&node).masks.get(i).and_then(|m| m.path_at(time))) {
                    let picked = in_marquee(&points, start, now);
                    if add {
                        edit.selected.extend(picked);
                    } else {
                        edit.selected = picked;
                    }
                }
            }
            Some(Gesture::Points { began: true, .. } | Gesture::Shape { began: true, .. } | Gesture::Handle { began: true, .. }) => {
                cmds.push(AppCommand::EndNodeParamEdit { clip_id });
            }
            _ => {}
        }
    }

    // The keys, while the viewer has them (see `AppState::mask_keys_owned`).
    if state.active_panel == Some(crate::state::PanelKind::Viewer) && state.mask_edit.has_keys() {
        let edit = &mut state.mask_edit;
        let (delete, enter, escape) = ui.input(|i| (i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace), i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        if !edit.pen.is_empty() {
            if enter && edit.pen.len() >= 3 {
                close_pen(edit, &node, clip_id, cmds);
            } else if escape {
                edit.pen.clear();
                edit.gesture = None;
            } else if delete {
                edit.pen.pop();
            }
        } else if escape {
            edit.selected.clear();
        } else if delete {
            if let Some(i) = edit.mask.map(|(_, i)| i) {
                // From every shape key alike.
                let selected = edit.selected.clone();
                set_mask(cmds, &node, i, &|m| m.without_path_points(&selected));
                edit.selected.clear();
            }
        }
    }

    // The pen's shape so far, the rubber band to the pointer, and the
    // first point ringed when a click there would close it.
    let edit = &state.mask_edit;
    if !edit.pen.is_empty() {
        let mut pts = edit.pen.clone();
        if let Some(Gesture::Pen { at, to }) = &edit.gesture {
            let drag = (plain.screen(*to).distance(plain.screen(*at)) > 3.0).then_some(*to);
            pts.push(pen_point(*at, drag));
        } else if let Some(s) = pointer.filter(|s| rect.contains(*s)) {
            pts.push(BezierPoint::from(plain.frame(s)));
        }
        let mut line = klipp_core::mask_path::flatten(&pts, 0.5 / rect.width().max(1.0), plain.aspect);
        // Open: the closing side isn't drawn.
        if let Some(last) = pts.last() {
            let end = line.iter().position(|p| *p == last.pos()).unwrap_or(line.len() - 1);
            line.truncate(end + 1);
        }
        ink.polyline(&painter, line.into_iter().map(|p| plain.screen(p)).collect(), false, ink.edited);
        for (k, p) in edit.pen.iter().enumerate() {
            ink.point(&painter, plain.screen(p.pos()), k == 0);
        }
        if edit.pen.len() >= 3 && pointer.is_some_and(|s| s.distance(plain.screen(edit.pen[0].pos())) <= HIT_PX) {
            painter.circle_stroke(plain.screen(edit.pen[0].pos()), 8.0, egui::Stroke::new(1.5_f32, ink.edited));
        }
    }
    if let Some(Gesture::Marquee { start, now, .. }) = &edit.gesture {
        let r = egui::Rect::from_two_pos(plain.screen(*start), plain.screen(*now));
        painter.rect_filled(r, 0.0, ink.edited.gamma_multiply(0.12));
        painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, ink.edited));
    }
    // What a click does now.
    let hint = if !edit.pen.is_empty() {
        "Click to add points, drag to pull out a curve. Click the first point or press Enter to close, Esc to cancel"
    } else if edit.mask.is_some() {
        "Drag points or handles. Shift adds to the selection, Alt-click makes a point smooth or a corner, double-click the curve to add a point, Delete removes"
    } else {
        "Click to start a mask's points, or click a mask's point to edit it"
    };
    let galley = painter.layout_no_wrap(hint.into(), egui::FontId::proportional(11.0), ink.line);
    let at = rect.min + egui::vec2(8.0, 8.0);
    painter.rect_filled(egui::Rect::from_min_size(at, galley.size()).expand(4.0), 3.0, ink.under);
    painter.galley(at, galley, ink.line);
}

/// The pen's shape made a mask: added to the Mask node the overlay
/// shows, or to a new one; its points selected once it is there.
fn close_pen(edit: &mut MaskEdit, node: &Option<(klipp_nodes::NodeId, klipp_core::MaskStack)>, clip_id: klipp_core::ClipId, cmds: &mut Vec<klipp_core::AppCommand>) {
    use klipp_core::AppCommand;
    let shape = klipp_core::MaskShape::Bezier { points: std::mem::take(&mut edit.pen) };
    edit.gesture = None;
    edit.selected.clear();
    match node {
        Some((node_id, masks)) => {
            let mut masks = masks.clone();
            masks.masks.push(klipp_core::Mask { shape, ..klipp_core::Mask::default() });
            edit.select_new = Some(masks.masks.len() - 1);
            cmds.push(AppCommand::SetNodeMasks { clip_id, node_id: *node_id, masks });
        }
        None => {
            edit.select_new = Some(0);
            cmds.push(AppCommand::AddMaskShape { clip_id, shape });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn square() -> Vec<BezierPoint> {
        [(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)].into_iter().map(BezierPoint::from).collect()
    }

    fn curved() -> Vec<BezierPoint> {
        let mut p = square();
        p[0].out_dx = 0.2;
        p[0].out_dy = -0.1;
        p[1].in_dx = -0.2;
        p[1].in_dy = -0.15;
        p
    }

    fn close(a: P, b: P) -> bool {
        (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4
    }

    /// Splitting a curve leaves its shape alone: the outline before and
    /// after cover the same places, and the new point is on the curve.
    #[test]
    fn a_split_keeps_the_shape() {
        let before = curved();
        let after = split_segment(&before, 0, 0.3);
        assert_eq!(after.len(), 5);
        let c = segment(&before, 0);
        assert!(close(after[1].pos(), cubic_at(&c, 0.3)));
        assert!(after[1].smooth);
        for t in [0.1, 0.25, 0.5, 0.9] {
            let on = cubic_at(&c, t);
            let (_, _, _, d) = nearest_on_path(&after, on, 1.0).unwrap();
            assert!(d < 1e-4, "{t}: {d}");
        }
        // A straight side splits into corners.
        let straight = split_segment(&square(), 2, 0.5);
        assert!(straight[3].is_corner() && close(straight[3].pos(), (0.5, 0.8)));
    }

    /// The nearest point on the path to a click near an edge is on that
    /// edge, at the right place along it.
    #[test]
    fn the_nearest_point_is_on_the_right_edge() {
        let (seg, t, p, d) = nearest_on_path(&square(), (0.5, 0.82), 1.0).unwrap();
        assert_eq!(seg, 2);
        assert!((t - 0.5).abs() < 1e-3 && close(p, (0.5, 0.8)) && (d - 0.02).abs() < 1e-4);
    }

    /// A smooth point keeps its handles in line as one is dragged; Alt
    /// breaks it into a corner whose other handle stays.
    #[test]
    fn a_smooth_handle_drags_its_partner_and_alt_breaks_it() {
        let p = BezierPoint { x: 0.5, y: 0.5, in_dx: -0.1, in_dy: 0.0, out_dx: 0.1, out_dy: 0.0, smooth: true };
        let q = drag_handle(p, true, (0.5, 0.3), false, 1.0);
        assert!(close((q.out_dx, q.out_dy), (0.0, -0.2)));
        assert!(close((q.in_dx, q.in_dy), (0.0, 0.1)), "in line, its own length: {q:?}");
        let r = drag_handle(p, true, (0.5, 0.3), true, 1.0);
        assert!(!r.smooth && close((r.in_dx, r.in_dy), (-0.1, 0.0)));
    }

    /// A point made smooth gets handles along its neighbours' line; made
    /// a corner, none.
    #[test]
    fn smooth_and_corner_points() {
        let s = make_smooth(&square(), 1, 1.0);
        assert!(s.smooth);
        // Along the line from (0.2,0.2) to (0.8,0.8).
        assert!((s.out_dx - s.out_dy).abs() < 1e-5 && s.out_dy > 0.0 && (s.in_dx - s.in_dy).abs() < 1e-5 && s.in_dy < 0.0);
        assert!(make_corner(s).is_corner());
    }

    /// Points move with their handles; deleting keeps at least three.
    #[test]
    fn move_and_delete() {
        let sel: BTreeSet<usize> = [0, 1].into();
        let moved = move_points(&curved(), &sel, 0.05, -0.05);
        assert!(close(moved[0].pos(), (0.25, 0.15)) && close(moved[0].out_handle(), (0.45, 0.05)));
        assert!(close(moved[2].pos(), (0.8, 0.8)));
        assert_eq!(delete_points(&square(), &[3].into()).unwrap().len(), 3);
        assert!(delete_points(&square(), &sel).is_none(), "two left is no shape");
    }

    /// Inside and outside the filled path, the marquee, and the pen.
    #[test]
    fn inside_marquee_and_pen() {
        assert!(inside(&square(), (0.5, 0.5), 1.0));
        assert!(!inside(&square(), (0.9, 0.5), 1.0));
        assert_eq!(in_marquee(&square(), (0.7, 0.1), (0.9, 0.9)), BTreeSet::from([1, 2]));
        assert!(pen_point((0.3, 0.3), None).is_corner());
        let p = pen_point((0.3, 0.3), Some((0.4, 0.3)));
        assert!(p.smooth && close(p.out_handle(), (0.4, 0.3)) && close(p.in_handle(), (0.2, 0.3)));
    }
}
