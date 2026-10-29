//! Clip moves on the timeline snap and land where they showed (#3056):
//! driven through the timeline panel with pointer events.

use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId, RationalTime};
use klipp_timeline::{Clip, TrackKind};

/// Points a second: a frame at 24 fps is about 2 points, finer than
/// the 10-point snap distance.
const ZOOM: f32 = 50.0;

fn t(secs: f64) -> RationalTime {
    RationalTime::from_secs_f64(secs, 24)
}

struct Timeline {
    ctx: egui::Context,
    state: AppState,
    panel: crate::timeline::NewTimelinePanel,
    old: crate::panels::timeline::TimelinePanel,
    time: f64,
    sent: Vec<AppCommand>,
    last: egui::FullOutput,
}

impl Timeline {
    fn new(state: AppState) -> Self {
        let mut old = crate::panels::timeline::TimelinePanel::default();
        old.embedded = true;
        let mut t = Timeline { ctx: egui::Context::default(), state, panel: Default::default(), old, time: 0.0, sent: Vec::new(), last: Default::default() };
        t.state.timeline_zoom = ZOOM;
        t.state.timeline_scroll_x = 0.0;
        t.frame(Vec::new(), egui::Modifiers::NONE);
        t
    }

    fn frame(&mut self, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
        self.time += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 700.0))), time: Some(self.time), events, modifiers, ..Default::default() };
        let (theme, kb) = (KlippTheme::default(), crate::keybindings::KeyBindings::default());
        let (state, panel, old) = (&mut self.state, &mut self.panel, &mut self.old);
        let mut cmds = Vec::new();
        self.last = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                cmds = panel.show(ui, state, &theme, old, &kb, false);
            });
        });
        self.sent.extend(cmds);
    }

    fn track(&self, name: &str) -> usize {
        self.state.sequence().tracks.iter().position(|t| t.name == name).unwrap()
    }

    /// A point `secs` into the timeline, on the middle of track `track`.
    fn at(&self, track: usize, secs: f64) -> egui::Pos2 {
        let r = self.old.frame_cache.track_rects.iter().find(|(i, _, _)| *i == track).map(|(_, _, r)| *r).expect("the track drawn");
        egui::pos2(r.left() + secs as f32 * ZOOM - self.state.timeline_scroll_x, r.center().y)
    }

    /// Press at `from`, move through `path` with the button held (a frame
    /// each), and let go where the last point is, in a frame of its own;
    /// `m` held once the drag has begun.
    fn drag(&mut self, from: egui::Pos2, path: &[egui::Pos2], m: egui::Modifiers) {
        let none = egui::Modifiers::NONE;
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: m };
        self.frame(vec![egui::Event::PointerMoved(from)], none);
        self.frame(vec![egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: none }], none);
        self.frame(vec![egui::Event::PointerMoved(from + egui::vec2(8.0, 0.0))], none);
        for p in path {
            self.frame(vec![egui::Event::PointerMoved(*p)], m);
        }
        let last = *path.last().unwrap();
        self.frame(vec![b(last, false)], m);
        self.frame(Vec::new(), m);
    }

    fn snap_line(&self) -> Option<f32> {
        self.old.frame_cache.snap_line_x.map(|(x, _)| x)
    }
}

/// Three video tracks: Video 1 the primary, with a clip 5–7 s whose in
/// is the target; Video 2 and Video 3 free, a clip 0–2 s on Video 2.
fn three_tracks() -> (AppState, ClipId, ClipId) {
    let mut state = AppState { snap_threshold_px: 10.0, playhead: t(30.0), ..Default::default() };
    let seq = &mut state.sequences[0];
    seq.add_video_track("Video 2".into());
    seq.add_video_track("Video 3".into());
    let v1 = seq.tracks.iter().position(|t| t.name == "Video 1").unwrap();
    let v2 = seq.tracks.iter().position(|t| t.name == "Video 2").unwrap();
    assert_eq!(seq.tracks[v1].magnetic_role, klipp_timeline::TrackRole::Primary);
    let target = Clip::media(klipp_core::MediaId::new(), t(5.0), t(7.0), t(0.0), t(2.0));
    let moved = Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    let (target_id, moved_id) = (target.id, moved.id);
    seq.tracks[v1].clips.push(target);
    seq.tracks[v2].clips.push(moved);
    assert!(seq.tracks.iter().all(|t| t.kind != TrackKind::Video || t.name.starts_with("Video")));
    (state, moved_id, target_id)
}

fn moved_to(sent: &[AppCommand]) -> Vec<(RationalTime, Option<usize>)> {
    sent.iter()
        .filter_map(|c| match c {
            AppCommand::MoveClip { new_timeline_in, .. } => Some((*new_timeline_in, None)),
            AppCommand::MoveClipToTrack { new_timeline_in, to_track, .. } => Some((*new_timeline_in, Some(*to_track))),
            _ => None,
        })
        .collect()
}

/// A clip moved on its own track so its out comes 4 points short of
/// another track's clip's in snaps there, shows the snap line on it while
/// dragging, and lands on that frame: the out at 5 s, the in at 3 s. The
/// drop used to be worked out again from the pointer, 4 points off.
#[test]
fn a_moved_clip_lands_on_the_snap_it_showed() {
    let (state, moved, _) = three_tracks();
    let mut tl = Timeline::new(state);
    let v2 = tl.track("Video 2");
    let grab = tl.at(v2, 1.0);
    let drop = grab + egui::vec2(3.0 * ZOOM - 4.0, 0.0);
    let held = [grab + egui::vec2(300.0, 0.0), drop - egui::vec2(1.0, 0.0), drop];
    let m = egui::Modifiers::NONE;
    let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: m };
    tl.frame(vec![egui::Event::PointerMoved(grab)], m);
    tl.frame(vec![b(grab, true)], m);
    for p in held {
        tl.frame(vec![egui::Event::PointerMoved(p)], m);
    }
    let line = tl.snap_line().expect("a snap line while dragging");
    assert!((line - tl.at(v2, 5.0).x).abs() < 0.5, "the line on the target's in: {line}");
    tl.frame(vec![b(drop, false)], m);
    tl.frame(Vec::new(), m);
    assert_eq!(moved_to(&tl.sent), [(t(3.0), None)], "moved {moved:?}");
}

/// Dropped on another free track, the same: it lands on the snap.
#[test]
fn a_clip_moved_to_another_track_lands_on_the_snap() {
    let (state, _, _) = three_tracks();
    let mut tl = Timeline::new(state);
    let (v2, v3) = (tl.track("Video 2"), tl.track("Video 3"));
    let grab = tl.at(v2, 1.0);
    let drop = tl.at(v3, 1.0) + egui::vec2(3.0 * ZOOM + 4.0, 0.0);
    tl.drag(grab, &[grab + egui::vec2(200.0, 0.0), drop], egui::Modifiers::NONE);
    assert_eq!(moved_to(&tl.sent), [(t(3.0), Some(v3))]);
}

/// Shift held, nothing snaps: it lands where the pointer put it, on the
/// nearest frame. 12 points short of the snap is 2.76 s, frame 66.24,
/// so frame 66.
#[test]
fn shift_places_a_clip_where_the_pointer_puts_it() {
    let (state, _, _) = three_tracks();
    let mut tl = Timeline::new(state);
    let v2 = tl.track("Video 2");
    let grab = tl.at(v2, 1.0);
    let drop = grab + egui::vec2(3.0 * ZOOM - 12.0, 0.0);
    tl.drag(grab, &[grab + egui::vec2(200.0, 0.0), drop], egui::Modifiers::SHIFT);
    assert_eq!(moved_to(&tl.sent), [(RationalTime { value: 66, rate: 24 }, None)]);
    assert_eq!(tl.snap_line(), None);
}

/// Once snapped, moving back and forth within one and a half times the
/// distance keeps the snap; it lets go past that.
#[test]
fn a_snapped_move_holds_until_past_one_and_a_half_times_the_distance() {
    let (state, _, _) = three_tracks();
    let mut tl = Timeline::new(state);
    let v2 = tl.track("Video 2");
    let grab = tl.at(v2, 1.0);
    let snapped = grab + egui::vec2(3.0 * ZOOM, 0.0);
    let m = egui::Modifiers::NONE;
    let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: m };
    tl.frame(vec![egui::Event::PointerMoved(grab)], m);
    tl.frame(vec![b(grab, true)], m);
    tl.frame(vec![egui::Event::PointerMoved(grab + egui::vec2(200.0, 0.0))], m);
    tl.frame(vec![egui::Event::PointerMoved(snapped + egui::vec2(-3.0, 0.0))], m);
    let line = tl.snap_line().expect("snapped");
    // A frame is about 2 points: 14 points is frame 7, 14.6 points.
    for dx in [12.0, -13.0, 14.0, -12.0] {
        tl.frame(vec![egui::Event::PointerMoved(snapped + egui::vec2(dx, 0.0))], m);
        assert_eq!(tl.snap_line(), Some(line), "held at {dx} points");
    }
    tl.frame(vec![egui::Event::PointerMoved(snapped + egui::vec2(17.0, 0.0))], m);
    assert_eq!(tl.snap_line(), None, "let go past 15 points");
    tl.frame(vec![b(snapped + egui::vec2(17.0, 0.0), false)], m);
}

/// On the primary track a moved clip goes into the slot its insert line
/// showed. Three clips packed 0–2, 2–4, 4–6 s; the first dragged so its
/// in comes 4 points short of 4 s snaps there and goes between the other
/// two. Worked out again from the pointer, at about 4.9 s, it used to go
/// after the last.
#[test]
fn a_clip_moved_on_the_primary_track_goes_where_its_line_showed() {
    let (mut state, _, _) = three_tracks();
    state.edit_mode = klipp_timeline::magnetic::EditMode::Insert;
    let seq = &mut state.sequences[0];
    let v1 = seq.tracks.iter().position(|t| t.name == "Video 1").unwrap();
    let clips: Vec<Clip> = (0..3).map(|i| Clip::media(klipp_core::MediaId::new(), t(2.0 * i as f64), t(2.0 * i as f64 + 2.0), t(0.0), t(2.0))).collect();
    let ids: Vec<ClipId> = clips.iter().map(|c| c.id).collect();
    seq.tracks[v1].clips = clips;
    let mut tl = Timeline::new(state);
    let grab = tl.at(v1, 1.0);
    let drop = grab + egui::vec2(4.0 * ZOOM - 4.0, 0.0);
    tl.drag(grab, &[grab + egui::vec2(100.0, 0.0), drop], egui::Modifiers::NONE);
    let order: Vec<ClipId> = tl.state.sequence().tracks[v1].clips.iter().map(|c| c.id).collect();
    assert_eq!(order, [ids[1], ids[0], ids[2]]);
}

/// A marker is a target; a clip's linked partner, which moves with it,
/// isn't: the partner's old in, 4 points from the drop, doesn't hold it.
#[test]
fn markers_snap_a_move_and_a_linked_partner_does_not() {
    let (mut state, moved, _) = three_tracks();
    state.sequences[0].markers.push(klipp_timeline::Marker {
        id: klipp_core::MarkerId::new(), position: t(10.0), label: "m".into(), color: klipp_core::MarkerColor::Red,
        duration: None, comment: String::new(), kind: Default::default(), review: None,
    });
    let mut tl = Timeline::new(state);
    let v2 = tl.track("Video 2");
    let grab = tl.at(v2, 1.0);
    let drop = grab + egui::vec2(10.0 * ZOOM + 4.0, 0.0);
    tl.drag(grab, &[grab + egui::vec2(200.0, 0.0), drop], egui::Modifiers::NONE);
    assert_eq!(moved_to(&tl.sent), [(t(10.0), None)], "on the marker");

    // A partner on Video 3 at 0.5–2.5 s, linked: dragging the clip 6
    // points past 0.5 s, its in finds no target there.
    let (mut state, moved2, _) = three_tracks();
    let v3 = state.sequence().tracks.iter().position(|t| t.name == "Video 3").unwrap();
    let mut partner = Clip::media(klipp_core::MediaId::new(), t(0.5), t(2.5), t(0.0), t(2.0));
    partner.linked_clip_id = Some(moved2);
    let pid = partner.id;
    state.sequences[0].tracks[v3].clips.push(partner);
    for c in state.sequences[0].all_clips_mut() {
        if c.id == moved2 {
            c.linked_clip_id = Some(pid);
        }
    }
    let mut tl = Timeline::new(state);
    let grab = tl.at(v2, 1.0);
    tl.drag(grab, &[grab + egui::vec2(60.0, 0.0), grab + egui::vec2(0.5 * ZOOM + 6.0, 0.0)], egui::Modifiers::NONE);
    assert_eq!(moved_to(&tl.sent), [(RationalTime::from_secs_f64(0.62, 24), None)], "not to the partner's 0.5 s {moved:?}");
}

/// Two grouped clips move as one, and the group snaps by whichever of
/// its edges is nearest a target: here the second clip's out (it ends a
/// second after the first) comes 4 points short of the target's in.
#[test]
fn a_group_snaps_by_its_nearest_edge() {
    let (mut state, moved, _) = three_tracks();
    let v3 = state.sequence().tracks.iter().position(|t| t.name == "Video 3").unwrap();
    let g = klipp_core::GroupId::new();
    let mut second = Clip::media(klipp_core::MediaId::new(), t(1.0), t(3.0), t(0.0), t(2.0));
    second.group_id = Some(g);
    let second_id = second.id;
    state.sequences[0].tracks[v3].clips.push(second);
    for c in state.sequences[0].all_clips_mut() {
        if c.id == moved {
            c.group_id = Some(g);
        }
    }
    state.selected_clips = vec![moved, second_id];
    let mut tl = Timeline::new(state);
    let v2 = tl.track("Video 2");
    let grab = tl.at(v2, 1.0);
    // The second clip's out, 3 s, to 5 s less 4 points: the first's in
    // to 2 s less 4 points, nowhere near a target of its own.
    let drop = grab + egui::vec2(2.0 * ZOOM - 4.0, 0.0);
    tl.drag(grab, &[grab + egui::vec2(200.0, 0.0), drop], egui::Modifiers::NONE);
    // Both selected, so both go as the selection's move (#3060).
    let mut got = moves(&tl.sent);
    got.sort_by_key(|m| m.1.value);
    assert_eq!(got, [(moved, t(2.0), None), (second_id, t(3.0), None)]);
}

fn marker(at: f64) -> klipp_timeline::Marker {
    klipp_timeline::Marker {
        id: klipp_core::MarkerId::new(), position: t(at), label: "m".into(), color: klipp_core::MarkerColor::Red,
        duration: None, comment: String::new(), kind: Default::default(), review: None,
    }
}

/// Clips on Video 2 at `spans`, each with a second of source to spare
/// either side, and a marker at 2.5 s.
fn clips_on_video_2(spans: &[(f64, f64)]) -> (AppState, Vec<ClipId>) {
    let (mut state, moved, _) = three_tracks();
    let seq = &mut state.sequences[0];
    let v2 = seq.tracks.iter().position(|t| t.name == "Video 2").unwrap();
    seq.tracks[v2].clips.retain(|c| c.id != moved);
    let clips: Vec<Clip> = spans.iter().map(|&(a, b)| Clip::media(klipp_core::MediaId::new(), t(a), t(b), t(1.0), t(1.0 + b - a))).collect();
    let ids = clips.iter().map(|c| c.id).collect();
    seq.tracks[v2].clips = clips;
    seq.markers.push(marker(2.5));
    (state, ids)
}

fn span(state: &AppState, id: ClipId) -> (RationalTime, RationalTime) {
    let c = state.sequence().all_clips().find(|c| c.id == id).unwrap();
    (c.timeline_in, c.timeline_out)
}

/// A roll of the cut between two clips snaps to a marker as a move does:
/// let go 4 points short of it, the cut is on it. It used to snap to the
/// playhead only, at its own 8 points.
#[test]
fn a_roll_snaps_to_a_marker() {
    let (state, ids) = clips_on_video_2(&[(0.0, 2.0), (2.0, 4.0)]);
    let mut tl = Timeline::new(state);
    let v2 = tl.track("Video 2");
    let cut = tl.at(v2, 2.0);
    tl.drag(cut, &[cut + egui::vec2(15.0, 0.0), tl.at(v2, 2.5) - egui::vec2(4.0, 0.0)], egui::Modifiers::NONE);
    let rolled = tl.sent.iter().find_map(|c| match c {
        AppCommand::RollTrimClip { clip_a_id, new_a_timeline_out, new_b_timeline_in, .. } if *clip_a_id == ids[0] => Some((*new_a_timeline_out, *new_b_timeline_in)),
        _ => None,
    });
    assert_eq!(rolled, Some((t(2.5), t(2.5))));
}

/// A slide (the Trim tool, Shift at the press) snaps its clip's edge
/// with Shift still held: its in, 4 points short of the marker, lands on
/// it, the neighbours following. Ctrl held, it goes where the pointer is.
#[test]
fn a_slide_snaps_with_shift_held_and_ctrl_lets_it_go_free() {
    for (during, want) in [(egui::Modifiers::SHIFT, t(2.5)), (egui::Modifiers::SHIFT | egui::Modifiers::COMMAND, RationalTime { value: 58, rate: 24 })] {
        let (mut state, ids) = clips_on_video_2(&[(0.0, 2.0), (2.0, 3.0), (3.0, 5.0)]);
        state.active_tool = klipp_core::Tool::Trim;
        let mut tl = Timeline::new(state);
        let v2 = tl.track("Video 2");
        let grab = tl.at(v2, 2.5);
        let shift = egui::Modifiers::SHIFT;
        let b = |p, d, m| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: m };
        tl.frame(vec![egui::Event::PointerMoved(grab)], shift);
        tl.frame(vec![b(grab, true, shift)], shift);
        tl.frame(vec![egui::Event::PointerMoved(grab + egui::vec2(8.0, 0.0))], shift);
        let to = grab + egui::vec2(0.5 * ZOOM - 4.0, 0.0);
        tl.frame(vec![egui::Event::PointerMoved(to)], during);
        tl.frame(vec![b(to, false, during)], during);
        tl.frame(Vec::new(), egui::Modifiers::NONE);
        assert_eq!(span(&tl.state, ids[1]).0, want, "{during:?}");
        assert_eq!(span(&tl.state, ids[0]).1, want);
    }
}

/// The Range tool's moving end snaps to a clip edge, 4 points off; with
/// Shift held it stays where the pointer is, on the nearest frame.
#[test]
fn the_range_tool_snaps_and_shift_does_not() {
    for (m, want) in [(egui::Modifiers::NONE, t(5.0)), (egui::Modifiers::SHIFT, RationalTime { value: 122, rate: 24 })] {
        let (mut state, _, _) = three_tracks();
        state.active_tool = klipp_core::Tool::Range;
        let mut tl = Timeline::new(state);
        let v3 = tl.track("Video 3");
        let from = tl.at(v3, 8.0);
        tl.drag(from, &[from - egui::vec2(60.0, 0.0), tl.at(v3, 5.0) + egui::vec2(4.0, 0.0)], m);
        let last_in = tl.sent.iter().rev().find_map(|c| match c {
            AppCommand::SetSequenceInPoint(at) => Some(*at),
            _ => None,
        });
        assert_eq!(last_in, Some(want), "{m:?}");
    }
}

/// A clip or a title let go on the timeline lands on a marker within the
/// distance; with Shift, on the nearest frame, rounded, not cut down.
#[test]
fn drops_snap_to_markers_and_round_to_a_frame() {
    let (mut state, _, _) = three_tracks();
    state.sequences[0].markers.push(marker(10.0));
    let drop = |secs, shift| crate::panels::timeline::TimelinePanel::drop_time_for_test(&state, secs, ZOOM, shift);
    assert_eq!(drop(10.0 + 4.0 / ZOOM as f64, false), t(10.0));
    assert_eq!(drop(12.0 + 0.7 / 24.0, true), RationalTime { value: 289, rate: 24 });
}

/// The blade cuts on a snap within the distance, as its line shows: a
/// click 4 points from the Out point cuts on it (the line used to miss a
/// snap to the Out that the cut made); with Shift, at the pointer.
#[test]
fn the_blade_cuts_where_its_line_shows() {
    for (m, want) in [(egui::Modifiers::NONE, t(1.5)), (egui::Modifiers::SHIFT, RationalTime { value: 38, rate: 24 })] {
        let (mut state, _, _) = three_tracks();
        state.sequences[0].out_point = Some(t(1.5));
        state.active_tool = klipp_core::Tool::Blade;
        let mut tl = Timeline::new(state);
        let v2 = tl.track("Video 2");
        let at = tl.at(v2, 1.5) + egui::vec2(4.0, 0.0);
        tl.frame(vec![egui::Event::PointerMoved(at)], m);
        for pressed in [true, false] {
            tl.frame(vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: m }], m);
        }
        let cut = tl.sent.iter().find_map(|c| match c {
            AppCommand::BladeClip { cut_at, .. } => Some(*cut_at),
            _ => None,
        });
        assert_eq!(cut, Some(want), "{m:?}");
    }
}

/// Free video tracks Video 4, Video 3 and Video 2 (top to bottom) over
/// the primary: clips at 0–1 s on Video 4, 0.5–1.5 s on Video 3 and
/// 3–4 s on Video 4, all selected. Snapping off, so the drag is exact.
fn selection() -> (AppState, [ClipId; 3]) {
    let (mut state, moved, _) = three_tracks();
    state.snap.master = false;
    let seq = &mut state.sequences[0];
    seq.add_video_track("Video 4".into());
    for t in &mut seq.tracks {
        t.clips.retain(|c| c.id != moved);
    }
    let at = |seq: &klipp_timeline::Sequence, n: &str| seq.tracks.iter().position(|t| t.name == n).unwrap();
    let (v4, v3) = (at(seq, "Video 4"), at(seq, "Video 3"));
    let c = |a: f64, b: f64| Clip::media(klipp_core::MediaId::new(), t(a), t(b), t(0.0), t(b - a));
    let (a, b, d) = (c(0.0, 1.0), c(0.5, 1.5), c(3.0, 4.0));
    let ids = [a.id, b.id, d.id];
    seq.tracks[v4].clips.extend([a, d]);
    seq.tracks[v3].clips.push(b);
    state.selected_clips = ids.to_vec();
    state.selected_clip = Some(ids[0]);
    (state, ids)
}

fn moves(sent: &[AppCommand]) -> Vec<(ClipId, RationalTime, Option<usize>)> {
    sent.iter().find_map(|c| match c {
        AppCommand::MoveClips { moves } => Some(moves.clone()),
        _ => None,
    }).unwrap_or_default()
}

/// Three selected clips on two tracks, one dragged 2 s later: all three
/// move 2 s, frame-exact, and the document's moves put them there as
/// one undo step that puts all three back.
#[test]
fn a_selection_drags_together_as_one_step() {
    let (state, ids) = selection();
    let mut tl = Timeline::new(state);
    let v4 = tl.track("Video 4");
    let grab = tl.at(v4, 0.5);
    tl.drag(grab, &[grab + egui::vec2(40.0, 0.0), grab + egui::vec2(2.0 * ZOOM, 0.0)], egui::Modifiers::NONE);
    let mut got = moves(&tl.sent);
    got.sort_by_key(|m| m.1.value);
    assert_eq!(got, [(ids[0], t(2.0), None), (ids[1], t(2.5), None), (ids[2], t(5.0), None)]);

    let state = &mut tl.state;
    let before: Vec<_> = ids.iter().map(|&id| span(state, id)).collect();
    let ops = crate::timeline::moves::drag_move_ops(state.sequence(), &got, true);
    let plan = klipp_document::ops::resolve(state.sequence(), &state.media_pool, &ops).expect("the moves resolve");
    let edits: Vec<crate::history::EditCommand> = plan.steps.into_iter().map(|s| s.edit).collect();
    {
        let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps };
        for e in &edits {
            let _ = view.apply_forward(e);
        }
    }
    state.push_edits_as_one(&edits, "Move Clips");
    assert_eq!(ids.iter().map(|&id| span(state, id).0).collect::<Vec<_>>(), [t(2.0), t(2.5), t(5.0)]);
    let entry = state.active_history_mut().undo().expect("a step");
    let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps };
    view.apply_inverse(&entry.command);
    assert_eq!(ids.iter().map(|&id| span(state, id)).collect::<Vec<_>>(), before);
    assert!(state.active_history_mut().undo().is_none(), "one step");
}

/// Dragged down a track, a selection on Video 4 and Video 3 goes to
/// Video 3 and Video 2, each one track of its kind lower; with Video 2
/// locked the drag is refused and nothing is sent.
#[test]
fn a_selection_drags_across_tracks_or_is_refused_whole() {
    for locked in [false, true] {
        let (mut state, ids) = selection();
        let v2 = state.sequence().tracks.iter().position(|t| t.name == "Video 2").unwrap();
        state.sequences[0].tracks[v2].locked = locked;
        let mut tl = Timeline::new(state);
        let (v4, v3) = (tl.track("Video 4"), tl.track("Video 3"));
        let grab = tl.at(v4, 0.5);
        let to = tl.at(v3, 0.5);
        tl.drag(grab, &[grab + egui::vec2(0.0, 20.0), to], egui::Modifiers::NONE);
        if locked {
            assert!(moves(&tl.sent).is_empty(), "refused: {:?}", moves(&tl.sent));
            continue;
        }
        let mut got = moves(&tl.sent);
        got.sort_by_key(|m| m.1.value);
        assert_eq!(got, [(ids[0], t(0.0), Some(v3)), (ids[1], t(0.5), Some(v2)), (ids[2], t(3.0), Some(v3))]);
        // The document takes them in an order where none lands on a clip
        // still waiting to leave (the Video 3 one goes down first).
        let ops = crate::timeline::moves::drag_move_ops(tl.state.sequence(), &got, true);
        let plan = klipp_document::ops::resolve(tl.state.sequence(), &tl.state.media_pool, &ops).expect("the moves resolve");
        assert_eq!(plan.steps.len(), 3);
    }
}

/// The persona repro (#3060): Video 1, the primary, holds clips at 0-4,
/// 14-18, 18-22, then A 26-29, B 29-32 and C 34-37, gap clips between (as
/// a project with bare gaps reads once opened). A, B and C selected, A
/// dragged 2 s later. While dragging each has its ghost where it goes: in
/// Insert one after another from 28 s, in Overwrite each 2 s on. Let go,
/// the block is sent as one move; applied, nothing is lost, the primary
/// is gapless, and one undo and one redo give the exact documents. The
/// drag used to close every gap on the track outside the undo step, and
/// in Overwrite cut one clip and lose two on undo.
#[test]
fn a_selection_on_the_primary_moves_as_a_block_and_undoes_exactly() {
    for (mode, ghosts_at, lands) in [
        (klipp_timeline::magnetic::EditMode::Insert, [28, 31, 34], [(28.0, 31.0), (31.0, 34.0), (34.0, 37.0)]),
        (klipp_timeline::magnetic::EditMode::Overwrite, [28, 31, 36], [(28.0, 31.0), (31.0, 34.0), (36.0, 39.0)]),
    ] {
        let (mut state, _, _) = three_tracks();
        state.snap.master = false;
        state.edit_mode = mode;
        let v1 = state.sequence().tracks.iter().position(|t| t.name == "Video 1").unwrap();
        let c = |a: f64, b: f64| Clip::media(klipp_core::MediaId::new(), t(a), t(b), t(0.0), t(b - a));
        let clips: Vec<Clip> = [(0.0, 4.0), (14.0, 18.0), (18.0, 22.0), (26.0, 29.0), (29.0, 32.0), (34.0, 37.0)].iter().map(|&(a, b)| c(a, b)).collect();
        let ids: Vec<ClipId> = clips.iter().map(|c| c.id).collect();
        for t in &mut state.sequences[0].tracks {
            t.clips.clear();
        }
        state.sequences[0].tracks[v1].clips = clips;
        state.sequences[0].fill_primary_holes();
        state.selected_clips = ids[3..].to_vec();
        state.selected_clip = Some(ids[3]);
        let original = serde_json::to_value(&state.sequences).unwrap();
        // Zoomed out so the whole track is drawn.
        let mut tl = Timeline::new(state);
        tl.state.timeline_zoom = 30.0;
        tl.frame(Vec::new(), egui::Modifiers::NONE);
        let r = tl.old.frame_cache.track_rects.iter().find(|(i, _, _)| *i == v1).unwrap().2;
        let grab = egui::pos2(r.left() + 27.5 * 30.0, r.center().y);
        let m = egui::Modifiers::NONE;
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: m };
        tl.frame(vec![egui::Event::PointerMoved(grab)], m);
        tl.frame(vec![b(grab, true)], m);
        tl.frame(vec![egui::Event::PointerMoved(grab + egui::vec2(10.0, 0.0))], m);
        let to = grab + egui::vec2(2.0 * 30.0, 0.0);
        tl.frame(vec![egui::Event::PointerMoved(to)], m);
        let mut ghosts: Vec<i32> = tl.last.shapes.iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Rect(g) if g.fill.a() == 128 && r.contains(g.rect.center()) => Some(((g.rect.left() - r.left()) / 30.0).round() as i32),
                _ => None,
            })
            .collect();
        ghosts.sort();
        assert_eq!(ghosts, ghosts_at, "{mode:?}: a ghost per selected clip where it goes (seconds)");
        tl.frame(vec![b(to, false)], m);
        tl.frame(Vec::new(), m);
        let sent = tl.sent.iter().find_map(|c| match c {
            AppCommand::MovePrimaryBlock { block, others, delta, overwrite } => Some((block.clone(), others.clone(), *delta, *overwrite)),
            _ => None,
        });
        let Some((block, others, delta, overwrite)) = sent else { panic!("{mode:?}: the block is sent") };
        assert_eq!(block, [(ids[3], t(26.0)), (ids[4], t(29.0)), (ids[5], t(34.0))], "{mode:?}");
        assert!(others.is_empty() && delta == t(2.0));
        assert_eq!(overwrite, mode == klipp_timeline::magnetic::EditMode::Overwrite);

        // As the app applies it.
        let state = &mut tl.state;
        super::primary_block::move_block(state, &block, &others, delta, overwrite).unwrap();
        assert!(state.sequence().primary_holes().is_none(), "{mode:?}: the primary is gapless");
        let real: Vec<(ClipId, f64, f64)> = state.sequence().tracks[v1].clips.iter().filter(|c| !c.kind.is_gap()).map(|c| (c.id, c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64())).collect();
        assert_eq!(real.iter().map(|r| r.0).collect::<Vec<_>>(), ids, "{mode:?}: every clip kept, in its order");
        assert_eq!(real[3..].iter().map(|r| (r.1, r.2)).collect::<Vec<_>>(), lands, "{mode:?}: the block where its ghosts showed");
        assert_eq!(real[..3].iter().map(|r| (r.1, r.2)).collect::<Vec<_>>(), [(0.0, 4.0), (14.0, 18.0), (18.0, 22.0)], "{mode:?}: the clips before it unmoved");
        let moved = serde_json::to_value(&state.sequences).unwrap();
        let entry = state.active_history_mut().undo().expect("a step");
        let _ = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps }.apply_inverse(&entry.command);
        assert!(serde_json::to_value(&state.sequences).unwrap() == original, "{mode:?}: one undo puts the whole document back");
        let entry = state.active_history_mut().redo().expect("the step again");
        let _ = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps }.apply_forward(&entry.command);
        assert!(serde_json::to_value(&state.sequences).unwrap() == moved, "{mode:?}: one redo as the move left it");
    }
}


/// Escape during a drag cancels it (#3060): a trim pulled out a second
/// and a move dragged two on a free track, and a trim of a primary clip
/// with its linked sound and gap clips round it, each Escaped before it
/// is let go: the tracks are exactly as before (the partner and the gap
/// clips included), nothing is sent, and no history entry is made.
#[test]
fn escape_cancels_a_trim_and_a_move() {
    for (case, trim) in [("free trim", true), ("free move", false), ("primary trim", true)] {
        let (mut state, ids) = clips_on_video_2(&[(0.0, 2.0)]);
        let (track_name, at) = if case == "primary trim" {
            // A primary clip 4-6 s between gap clips, its sound linked.
            let p = state.sequence().primary_track_index().unwrap();
            let a1 = state.sequence().tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
            let media = klipp_core::MediaId::new();
            let mut picture = Clip::media(media, t(4.0), t(6.0), t(1.0), t(3.0));
            let mut sound = Clip::media(media, t(4.0), t(6.0), t(1.0), t(3.0));
            picture.linked_clip_id = Some(sound.id);
            sound.linked_clip_id = Some(picture.id);
            state.sequences[0].tracks[p].clips = vec![picture];
            state.sequences[0].tracks[a1].clips = vec![sound];
            state.sequences[0].tracks[p].clips.push(Clip::media(klipp_core::MediaId::new(), t(8.0), t(9.0), t(0.0), t(1.0)));
            state.sequences[0].fill_primary_holes();
            ("Video 1", 6.0)
        } else {
            ("Video 2", if trim { 2.0 } else { 1.0 })
        };
        let mut tl = Timeline::new(state);
        let track = tl.track(track_name);
        let before = serde_json::to_value(&tl.state.sequence().tracks).unwrap();
        let grab = if trim { tl.at(track, at) - egui::vec2(3.0, 0.0) } else { tl.at(track, at) };
        let m = egui::Modifiers::NONE;
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: m };
        tl.frame(vec![egui::Event::PointerMoved(grab)], m);
        tl.frame(vec![b(grab, true)], m);
        tl.frame(vec![egui::Event::PointerMoved(grab + egui::vec2(10.0, 0.0))], m);
        let to = grab + egui::vec2(if trim { ZOOM } else { 2.0 * ZOOM }, 0.0);
        tl.frame(vec![egui::Event::PointerMoved(to)], m);
        if trim {
            assert!(serde_json::to_value(&tl.state.sequence().tracks).unwrap() != before, "{case}: the trim is under way");
        }
        tl.sent.clear();
        tl.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: m }], m);
        tl.frame(vec![egui::Event::PointerMoved(to + egui::vec2(5.0, 0.0))], m);
        tl.frame(vec![b(to, false)], m);
        tl.frame(Vec::new(), m);
        let _ = ids;
        assert!(serde_json::to_value(&tl.state.sequence().tracks).unwrap() == before, "{case}: the tracks exactly as they were");
        assert!(!tl.sent.iter().any(|c| matches!(c, AppCommand::MoveClip { .. } | AppCommand::MoveClips { .. } | AppCommand::MoveClipToTrack { .. } | AppCommand::TrimClip { .. } | AppCommand::MovePrimaryBlock { .. })), "{case}: nothing sent: {:?}", tl.sent);
        assert!(tl.state.active_history_mut().undo().is_none(), "{case}: no history entry");
    }
}
