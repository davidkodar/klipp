//! A title layer's handles in the viewer (#1714): its box moved,
//! resized and turned, and where a text on an arc starts. Each drag is
//! one undo step: the steps between `BeginTitleObjectDrag` and
//! `EndTitleObjectDrag` record nothing of their own.
//!
//! On the state, so the app's handlers and a test drive the same code.

use super::*;

/// The history key a drag on `clip_id`'s layers is open under.
pub(super) fn drag_key(clip_id: ClipId) -> String {
    format!("title_object:{}", clip_id.0)
}

pub(super) fn snapshot_title(state: &AppState, clip_id: ClipId) -> Option<klipp_timeline::TitleData> {
    state.sequences[state.active_sequence_idx].tracks.iter()
        .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
        .and_then(|c| c.title_data())
        .cloned()
}

/// Records a title edit from `old` to the clip's title now, one step,
/// unless a handle drag on the clip is open: that drag's end is its step.
/// Keystrokes in a typing burst merge into one step too.
pub(super) fn push_title_undo(state: &mut AppState, clip_id: ClipId, description: &'static str, old: klipp_timeline::TitleData) {
    if state.active_history().is_dragging(&drag_key(clip_id)) {
        return;
    }
    let Some(new) = snapshot_title(state, clip_id) else { return };
    if old == new {
        return;
    }
    if description == "Edit Title Text" && state.active_history_mut().try_coalesce_title_text(clip_id, &old, &new) {
        return;
    }
    state.active_history_mut().push(crate::history::HistoryEntry {
        command: crate::history::EditCommand::EditTitle { clip_id, description, old_data: old, new_data: new },
    });
    if description == "Edit Title Text" {
        state.active_history_mut().note_title_text_edit(clip_id);
    }
}

/// Applies a handle command to the state. Returns the clip whose title
/// changed, for the caller to raster and sync; `None` for a command that
/// changed no title, or one that is not a handle's.
pub(super) fn apply(state: &mut AppState, cmd: &AppCommand) -> Option<ClipId> {
    match cmd {
        AppCommand::BeginTitleObjectDrag { clip_id } => {
            let data = snapshot_title(state, *clip_id)?;
            state.active_history_mut().begin_drag(drag_key(*clip_id), title_entry(*clip_id, data.clone(), data));
            None
        }
        AppCommand::EndTitleObjectDrag { clip_id } => {
            if !state.active_history().is_dragging(&drag_key(*clip_id)) {
                return None;
            }
            let before = state.active_history().pending_old_entry_ref().and_then(|e| match &e.command {
                crate::history::EditCommand::EditTitle { old_data, .. } => Some(old_data.clone()),
                _ => None,
            });
            let now = snapshot_title(state, *clip_id);
            match now {
                // A press on a handle that moved nothing is no step.
                Some(now) if before.as_ref() != Some(&now) => {
                    state.active_history_mut().end_drag(title_entry(*clip_id, now.clone(), now));
                }
                _ => state.active_history_mut().abandon_pending_drag(),
            }
            None
        }
        AppCommand::SetTitleObjectTransform { clip_id, object_id, center_x, center_y, size_x, size_y, rotation, anchor_x, anchor_y } => {
            edit_object(state, *clip_id, *object_id, "Move Title Object", |o| {
                // Fields one by one, so the layer's keys survive a drag:
                // it sets the values that hold without them.
                o.transform.center = klipp_timeline::TitlePosition { x: *center_x, y: *center_y };
                o.transform.size = klipp_timeline::TitlePosition { x: *size_x, y: *size_y };
                o.transform.rotation = *rotation;
                o.transform.anchor_norm = klipp_timeline::TitlePosition { x: *anchor_x, y: *anchor_y };
            })
        }
        AppCommand::SetTitleObjectTransformAndFont { clip_id, object_id, center_x, center_y, size_x, size_y, rotation, anchor_x, anchor_y, font_size } => {
            edit_object(state, *clip_id, *object_id, "Resize Title Object", |o| {
                o.transform.center = klipp_timeline::TitlePosition { x: *center_x, y: *center_y };
                o.transform.size = klipp_timeline::TitlePosition { x: *size_x, y: *size_y };
                o.transform.rotation = *rotation;
                o.transform.anchor_norm = klipp_timeline::TitlePosition { x: *anchor_x, y: *anchor_y };
                if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut o.content {
                    tb.font_size = *font_size;
                }
            })
        }
        AppCommand::SetTitleArcStart { clip_id, object_id, start_angle } => {
            edit_object(state, *clip_id, *object_id, "Title Text Path", |o| {
                if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut o.content {
                    if let Some(klipp_timeline::TextPath::Arc { start_angle: a, .. }) = &mut tb.text_path {
                        *a = *start_angle;
                    }
                }
            })
        }
        _ => None,
    }
}

fn title_entry(clip_id: ClipId, old_data: klipp_timeline::TitleData, new_data: klipp_timeline::TitleData) -> crate::history::HistoryEntry {
    crate::history::HistoryEntry {
        command: crate::history::EditCommand::EditTitle { clip_id, description: "Move Title Object", old_data, new_data },
    }
}

fn edit_object(
    state: &mut AppState,
    clip_id: ClipId,
    object_id: u32,
    description: &'static str,
    edit: impl FnOnce(&mut klipp_timeline::TitleObject),
) -> Option<ClipId> {
    let old = snapshot_title(state, clip_id)?;
    let target = klipp_timeline::TitleObjectId(object_id);
    let o = state.sequences[state.active_sequence_idx]
        .tracks
        .iter_mut()
        .find_map(|t| t.clips.iter_mut().find(|c| c.id == clip_id))
        .and_then(|c| match &mut c.kind {
            klipp_timeline::ClipKind::Title(td) => td.object_mut(target),
            _ => None,
        })?;
    edit(o);
    push_title_undo(state, clip_id, description, old);
    state.dirty = true;
    Some(clip_id)
}

#[cfg(test)]
mod tests {
    use super::super::viewer_harness::Viewer;
    use super::*;
    use klipp_timeline::{ObjectTransform, TextBox, TitleData, TitleObject, TitleObjectContent, TitlePosition};

    /// The viewer with a title selected on a text track at the playhead,
    /// its layers `layers`, the first selected, and the Select tool.
    fn viewer(layers: &[(&str, (f32, f32), f32)]) -> (Viewer, ClipId) {
        let mut v = Viewer::new();
        v.state.active_tool = Tool::Select;
        let mut td = TitleData { objects: Vec::new(), ..TitleData::default() };
        for (text, (x, y), rotation) in layers {
            let mut t = klipp_timeline::default_text_transform();
            t.center = TitlePosition { x: *x, y: *y };
            t.rotation = *rotation;
            td.add_object(TitleObjectContent::TextBox(TextBox { text: text.to_string(), font_size: 72.0, ..Default::default() }), t);
        }
        let first = td.objects[0].id;
        let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut clip = klipp_timeline::Clip::gap(at(0.0), at(5.0));
        clip.kind = klipp_timeline::ClipKind::Title(td);
        let id = clip.id;
        let mut track = klipp_timeline::Track::new("Text 1", klipp_timeline::TrackKind::Text);
        track.clips.push(clip);
        v.state.sequences[0].tracks.insert(0, track);
        v.state.selected_clip = Some(id);
        v.state.selected_clips = vec![id];
        v.state.selected_title_object = Some((id, first));
        v.frame(Vec::new());
        (v, id)
    }

    fn title(v: &Viewer, id: ClipId) -> TitleData {
        v.state.sequences[0].all_clips().find(|c| c.id == id).unwrap().title_data().unwrap().clone()
    }

    fn layer(v: &Viewer, id: ClipId, n: usize) -> TitleObject {
        title(v, id).objects[n].clone()
    }

    /// Layer `n` as the editor draws it, hugging its text.
    fn shown(v: &Viewer, id: ClipId, n: usize) -> TitleObject {
        let res = v.state.sequence().resolution;
        let td = title(v, id);
        crate::panels::viewer::shown_title_layer(&td, (res.width, res.height), td.objects[n].id).unwrap()
    }

    fn screen(v: &Viewer, x: f32, y: f32) -> egui::Pos2 {
        let r = v.image_rect();
        r.min + egui::vec2(x * r.width(), y * r.height())
    }

    /// A shown layer's corners on screen: top left, top right, bottom
    /// right, bottom left, turned with it.
    fn corners(v: &Viewer, o: &TitleObject) -> [egui::Pos2; 4] {
        let r = v.image_rect();
        let c = screen(v, o.transform.center.x, o.transform.center.y);
        let half = egui::vec2(o.transform.size.x * r.width(), o.transform.size.y * r.height()) * 0.5;
        let (sin, cos) = o.transform.rotation.to_radians().sin_cos();
        let turn = |x: f32, y: f32| c + egui::vec2(x * cos - y * sin, x * sin + y * cos);
        [turn(-half.x, -half.y), turn(half.x, -half.y), turn(half.x, half.y), turn(-half.x, half.y)]
    }

    fn steps(v: &Viewer) -> usize {
        v.state.active_history().undo_count()
    }

    fn undo(v: &mut Viewer) {
        let entry = v.state.active_history_mut().undo().expect("a step");
        let mut doc = klipp_document::apply::DocumentView {
            sequences: &mut v.state.sequences,
            active: v.state.active_sequence_idx,
            media_pool: &mut v.state.media_pool,
            fps: &mut v.state.fps,
        };
        doc.apply_inverse(&entry.command);
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// The title clip `id` placed by keys on its own lanes (#2014), held
    /// over the clip: `lanes` of `(param, value)`, positions in frame
    /// pixels.
    fn place(v: &mut Viewer, id: ClipId, lanes: &[(&str, f32)]) {
        let seq = &mut v.state.sequences[0];
        let clip = seq.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == id).unwrap();
        let mut b = klipp_timeline::test_clip::TestClip::of(clip.clone());
        for (param, value) in lanes {
            b = b.lane(param, 0.0, *value).lane(param, 5.0, *value);
        }
        *clip = b.build();
        v.frame(Vec::new());
    }

    /// The frame's size, pixels.
    fn frame_size(v: &Viewer) -> (f32, f32) {
        let r = v.state.sequence().resolution;
        (r.width as f32, r.height as f32)
    }

    /// Where layer `n`'s body is grabbed on screen with its canvas
    /// unplaced: a quarter of its box right of its middle, clear of the
    /// handle at the middle.
    fn middle(v: &Viewer, id: ClipId, n: usize) -> egui::Pos2 {
        let seen = shown(v, id, n);
        let c = corners(v, &seen);
        c[0].lerp(c[2], 0.5) + (c[1] - c[0]) * 0.25
    }

    /// #2032: a title in the middle of the frame, selected, is moved by a
    /// drag from its middle, where the hyperzoom focal handle sits: the
    /// handle is there only while the clip's HYPERZOOM section is open
    /// in the inspector. The control: with it open, the same drag moves
    /// the focal point and not the title.
    #[test]
    fn a_drag_from_a_selected_titles_middle_moves_the_title() {
        let (mut v, id) = viewer(&[("Hello", (0.5, 0.5), 0.0)]);
        // The clip selected, none of its layers: the middle is its body,
        // not a layer's anchor.
        v.state.selected_title_object = None;
        v.frame(Vec::new());
        let w = v.image_rect().width();
        let at = v.image_rect().center();
        v.modifiers = egui::Modifiers::ALT;
        v.drag(at, at + egui::vec2(40.0, 0.0));
        let c = layer(&v, id, 0).transform.center;
        assert!(close(c.x, 0.5 + 40.0 / w) && close(c.y, 0.5), "the title moved: {c:?}");
        assert!(close(crate::hyperzoom::read_hyperzoom_scratch(&v.ctx, id).focal_x, 0.5), "and the focal point did not");

        let (mut v, id) = viewer(&[("Hello", (0.5, 0.5), 0.0)]);
        v.state.selected_title_object = None;
        crate::hyperzoom::keep_section_shown(&v.ctx, id);
        v.frame(Vec::new());
        let w = v.image_rect().width();
        let at = v.image_rect().center();
        v.modifiers = egui::Modifiers::ALT;
        v.drag(at, at + egui::vec2(40.0, 0.0));
        assert!(close(layer(&v, id, 0).transform.center.x, 0.5), "the title stayed");
        let fx = crate::hyperzoom::read_hyperzoom_scratch(&v.ctx, id).focal_x;
        assert!((fx - (0.5 + 40.0 / w)).abs() < 0.01, "the focal point moved with the pointer: {fx}");
    }

    // The layers below stand off the frame's centre, where a selected
    // clip's hyperzoom focal handle would take the press while its
    // section is open.

    /// #2014: a title composed to the right is edited where it is drawn:
    /// its layer is grabbed there and moved as far as the pointer goes,
    /// and where it would be unmoved there is nothing to grab.
    #[test]
    fn a_composed_titles_layer_is_grabbed_where_it_is_drawn() {
        let (mut v, id) = viewer(&[("Hello", (0.3, 0.5), 0.0)]);
        let (fw, _) = frame_size(&v);
        place(&mut v, id, &[("pos_x", 0.4 * fw)]);
        let w = v.image_rect().width();
        let unmoved = middle(&v, id, 0);
        let drawn = unmoved + egui::vec2(0.4 * w, 0.0);
        // Where it would be, unmoved: nothing to grab.
        v.modifiers = egui::Modifiers::ALT;
        v.drag(unmoved, unmoved + egui::vec2(30.0, 0.0));
        assert!(close(layer(&v, id, 0).transform.center.x, 0.3), "nothing moved where the title isn't");
        // Where it is drawn: moved with the pointer.
        v.drag(drawn, drawn + egui::vec2(40.0, 0.0));
        let c = layer(&v, id, 0).transform.center;
        assert!(close(c.x, 0.3 + 40.0 / w) && close(c.y, 0.5), "{c:?}");
    }

    /// #2014: on a title scaled to half, a drag moves its layer twice as
    /// far in the title's own space; on one turned a quarter, a drag to
    /// the right moves it up the title's canvas.
    #[test]
    fn a_drag_follows_a_scaled_or_turned_title() {
        let (mut v, id) = viewer(&[("Hello", (0.3, 0.5), 0.0)]);
        place(&mut v, id, &[("scale", 0.5)]);
        let w = v.image_rect().width();
        let centre = v.image_rect().center();
        // Scaled to half about the frame's centre.
        let at = centre + (middle(&v, id, 0) - centre) * 0.5;
        v.modifiers = egui::Modifiers::ALT;
        v.drag(at, at + egui::vec2(20.0, 0.0));
        let c = layer(&v, id, 0).transform.center;
        assert!(close(c.x, 0.3 + 40.0 / w) && close(c.y, 0.5), "twice as far: {c:?}");

        let (mut v, id) = viewer(&[("Hello", (0.3, 0.5), 0.0)]);
        place(&mut v, id, &[("rotation", 90.0)]);
        let (fw, fh) = frame_size(&v);
        let w = v.image_rect().width();
        let centre = v.image_rect().center();
        // Turned a quarter, clockwise on screen, about the frame's centre.
        let d = middle(&v, id, 0) - centre;
        let at = centre + egui::vec2(-d.y, d.x);
        v.modifiers = egui::Modifiers::ALT;
        v.drag(at, at + egui::vec2(40.0, 0.0));
        let c = layer(&v, id, 0).transform.center;
        assert!(close(c.x, 0.3) && close(c.y, 0.5 - 40.0 / w * fw / fh), "up the canvas: {c:?}");
    }

    /// #2014: on a title tilted in perspective, or scaled to nothing, the
    /// canvas has nothing to grab: no handle where the title isn't.
    #[test]
    fn a_tilted_or_vanished_title_has_no_handles() {
        for lanes in [&[("tilt_x", 25.0)][..], &[("scale", 0.0)][..]] {
            let (mut v, id) = viewer(&[("Hello", (0.3, 0.5), 0.0)]);
            place(&mut v, id, lanes);
            let before = title(&v, id);
            let at = middle(&v, id, 0);
            v.modifiers = egui::Modifiers::ALT;
            v.drag(at, at + egui::vec2(40.0, 0.0));
            assert_eq!(title(&v, id), before, "{lanes:?}: nothing moved");
            assert_eq!(steps(&v), 0, "{lanes:?}: no step");
        }
        // The control: unplaced, the same press moves the layer.
        let (mut v, id) = viewer(&[("Hello", (0.3, 0.5), 0.0)]);
        let at = middle(&v, id, 0);
        v.modifiers = egui::Modifiers::ALT;
        v.drag(at, at + egui::vec2(40.0, 0.0));
        assert!(!close(layer(&v, id, 0).transform.center.x, 0.3), "the control moved");
    }

    /// #2014: a layer of a composed title snaps to its own canvas's centre
    /// line, where the placed title draws that line, not to the frame's.
    #[test]
    fn a_composed_titles_layer_snaps_to_its_own_canvas() {
        let (mut v, id) = viewer(&[("Hello", (0.48, 0.5), 0.0)]);
        let (fw, _) = frame_size(&v);
        place(&mut v, id, &[("pos_x", 0.2 * fw)]);
        let w = v.image_rect().width();
        let at = middle(&v, id, 0) + egui::vec2(0.2 * w, 0.0);
        v.modifiers = egui::Modifiers::NONE;
        // A little short of the canvas's centre: snapped onto it.
        v.drag(at, at + egui::vec2(0.018 * w, 0.0));
        assert!(close(layer(&v, id, 0).transform.center.x, 0.5), "{:?}", layer(&v, id, 0).transform.center);
    }

    /// An edge handle resizes the box from the box as it is seen (the
    /// layout box behind it is the whole frame): the right edge moves
    /// with the pointer and the left stays. The drag is one undo step,
    /// and undoing it puts the title back.
    #[test]
    fn an_edge_resizes_the_box_as_seen_in_one_step() {
        let (mut v, id) = viewer(&[("Hello", (0.3, 0.35), 0.0)]);
        let before = title(&v, id);
        let seen = shown(&v, id, 0);
        let c = corners(&v, &seen);
        v.modifiers = egui::Modifiers::ALT;
        let edge = c[1].lerp(c[2], 0.5);
        v.drag(edge, edge + egui::vec2(80.0, 0.0));
        let t = layer(&v, id, 0).transform;
        let w = v.image_rect().width();
        assert!(close(t.size.x, seen.transform.size.x + 80.0 / w), "{:?} from {:?}: {:?}", t.size, seen.transform.size, v.sent);
        assert!(close(t.center.x, 0.3 + 40.0 / w), "{:?}", t.center);
        assert!(close(t.size.y, seen.transform.size.y) && close(t.center.y, 0.35));
        assert_eq!(steps(&v), 1, "one step for the drag");
        undo(&mut v);
        assert_eq!(title(&v, id), before);
    }

    /// A corner keeps the box's proportions and scales the type with it.
    #[test]
    fn a_corner_scales_the_box_and_its_type() {
        let (mut v, id) = viewer(&[("Hello", (0.3, 0.35), 0.0)]);
        let seen = shown(&v, id, 0);
        let c = corners(&v, &seen);
        v.modifiers = egui::Modifiers::ALT;
        v.drag(c[2], c[2] + egui::vec2(60.0, 10.0));
        let o = layer(&v, id, 0);
        let k = o.transform.size.x / seen.transform.size.x;
        assert!(k > 1.2 && close(o.transform.size.y / seen.transform.size.y, k), "{:?}", o.transform.size);
        let TitleObjectContent::TextBox(tb) = &o.content else { unreachable!() };
        assert!((tb.font_size - 72.0 * k).abs() < 0.01, "{} for {k}", tb.font_size);
        assert_eq!(steps(&v), 1);
    }

    /// The rotation handle turns the box by the angle the pointer goes
    /// round its centre, as seen on screen, and a turn ending near a
    /// right angle lands on it.
    #[test]
    fn the_rotation_handle_turns_the_box() {
        let (mut v, id) = viewer(&[("Hello", (0.4, 0.45), 0.0)]);
        let seen = shown(&v, id, 0);
        let c = corners(&v, &seen);
        let centre = screen(&v, 0.4, 0.45);
        let stalk = c[0].lerp(c[1], 0.5) + egui::vec2(0.0, -24.0);
        let reach = centre.distance(stalk);
        // A quarter turn and a degree and a half: it lands on 90.
        v.drag(stalk, centre + egui::vec2(reach, reach * 1.5_f32.to_radians().tan()));
        let t = layer(&v, id, 0).transform;
        assert_eq!(t.rotation, 90.0, "{:?}", v.sent);
        assert!(close(t.center.x, 0.4) && close(t.size.x, 1.0), "a turn leaves the box: {t:?}");
        assert_eq!(steps(&v), 1);
    }

    /// A turned box resizes along its own sides: its right edge, turned
    /// a quarter clockwise, faces down, and dragging it down widens the
    /// box while its centre goes half as far.
    #[test]
    fn a_turned_box_resizes_along_its_sides() {
        let (mut v, id) = viewer(&[("Hello", (0.4, 0.4), 90.0)]);
        let seen = shown(&v, id, 0);
        let c = corners(&v, &seen);
        v.modifiers = egui::Modifiers::ALT;
        let edge = c[1].lerp(c[2], 0.5);
        v.drag(edge, edge + egui::vec2(0.0, 40.0));
        let t = layer(&v, id, 0).transform;
        let r = v.image_rect();
        assert!(close(t.size.x, seen.transform.size.x + 40.0 / r.width()), "{:?} from {:?}", t.size, seen.transform.size);
        assert!(close(t.center.y, 0.4 + 20.0 / r.height()) && close(t.center.x, 0.4), "{:?}", t.center);
        assert_eq!(t.rotation, 90.0);
    }

    /// Moved to within a few points of the frame's centre line, the
    /// layer lands on it; with Alt held the same move does not snap.
    #[test]
    fn a_move_snaps_to_the_centre_line_unless_alt() {
        for alt in [false, true] {
            let (mut v, id) = viewer(&[("Hello", (0.3, 0.35), 0.0)]);
            let seen = shown(&v, id, 0);
            let w = v.image_rect().width();
            let grab = screen(&v, 0.3, 0.35) + egui::vec2(seen.transform.size.x * w * 0.25, 0.0);
            if alt {
                v.modifiers = egui::Modifiers::ALT;
            }
            v.drag(grab, grab + egui::vec2(0.2 * w + 3.0, 0.0));
            let t = layer(&v, id, 0).transform;
            if alt {
                assert!(close(t.center.x, 0.5 + 3.0 / w), "free: {:?}", t.center);
            } else {
                assert_eq!(t.center.x, 0.5, "snapped: {:?}", v.sent);
            }
            assert!(close(t.size.x, 1.0), "a move leaves the layout box");
            assert_eq!(steps(&v), 1);
        }
    }

    /// An edge drawn to within a few points of another layer's edge
    /// lands on it.
    #[test]
    fn an_edge_snaps_to_another_layers_edge() {
        let (mut v, id) = viewer(&[("Hello", (0.25, 0.35), 0.0), ("World", (0.7, 0.35), 0.0)]);
        let seen = shown(&v, id, 0);
        let other = shown(&v, id, 1);
        let other_left = other.transform.center.x - other.transform.size.x * 0.5;
        let right = seen.transform.center.x + seen.transform.size.x * 0.5;
        let c = corners(&v, &seen);
        let w = v.image_rect().width();
        let edge = c[1].lerp(c[2], 0.5);
        v.drag(edge, edge + egui::vec2((other_left - right) * w - 3.0, 0.0));
        let t = layer(&v, id, 0).transform;
        let left = seen.transform.center.x - seen.transform.size.x * 0.5;
        assert!(close(t.center.x + t.size.x * 0.5, other_left), "right edge {} to {other_left}", t.center.x + t.size.x * 0.5);
        assert!(close(t.center.x - t.size.x * 0.5, left), "the left edge stays");
        assert_eq!(steps(&v), 1);
    }

    /// The path-start handle of a text on an arc sets where the arc
    /// starts, from the angle the pointer is at about the layer's centre.
    #[test]
    fn the_path_start_handle_turns_the_arcs_start() {
        let (mut v, id) = viewer(&[("Round and round", (0.5, 0.5), 0.0)]);
        {
            let clip = v.state.sequences[0].tracks[0].find_clip_mut(id).unwrap();
            let klipp_timeline::ClipKind::Title(td) = &mut clip.kind else { unreachable!() };
            let TitleObjectContent::TextBox(tb) = &mut td.objects[0].content else { unreachable!() };
            tb.text_path = Some(klipp_timeline::TextPath::Arc { radius: 300.0, start_angle: 180.0, sweep: 120.0 });
        }
        v.frame(Vec::new());
        let res = v.state.sequence().resolution;
        let (fw, fh) = (res.width as f32, res.height as f32);
        let on_arc = |v: &Viewer, deg: f32| {
            let a = deg.to_radians();
            screen(v, 0.5 + 300.0 * a.cos() / fw, 0.5 + 300.0 * a.sin() / fh)
        };
        let (from, to) = (on_arc(&v, 180.0), on_arc(&v, 30.0));
        v.drag(from, to);
        let o = layer(&v, id, 0);
        let TitleObjectContent::TextBox(tb) = &o.content else { unreachable!() };
        let Some(klipp_timeline::TextPath::Arc { start_angle, radius, sweep }) = tb.text_path else { panic!("still an arc") };
        assert!((start_angle - 30.0).abs() < 0.5, "{start_angle}: {:?}", v.sent);
        assert_eq!((radius, sweep), (300.0, 120.0));
        assert_eq!(o.transform, ObjectTransform { center: TitlePosition { x: 0.5, y: 0.5 }, ..klipp_timeline::default_text_transform() });
        assert_eq!(steps(&v), 1);
    }
}
