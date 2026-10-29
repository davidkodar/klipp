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
}

impl Timeline {
    fn new(state: AppState) -> Self {
        let mut old = crate::panels::timeline::TimelinePanel::default();
        old.embedded = true;
        let mut t = Timeline { ctx: egui::Context::default(), state, panel: Default::default(), old, time: 0.0, sent: Vec::new() };
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
        let _ = self.ctx.run(input, |ctx| {
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
/// second after the first) comes 4 points short of the target's in. A
/// selected clip that stays where it is isn't a target either.
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
    assert_eq!(moved_to(&tl.sent), [(t(2.0), None)]);
}
