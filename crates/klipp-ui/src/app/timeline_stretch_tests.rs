//! Rate stretch on the timeline (#3055), driven through the timeline
//! panel with pointer events: the top corner of a clip's end is a grip
//! with its own cursor, and dragging it stretches; the edge below it
//! still trims; the stretched edge snaps to another track's edge exactly;
//! two selected clips with the same end stretch together.

use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId, RationalTime};
use klipp_timeline::Clip;

/// Points a second: a frame at 24 fps is about 2 points.
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
    cursor: egui::CursorIcon,
    texts: Vec<String>,
}

impl Timeline {
    fn new(state: AppState) -> Self {
        let mut old = crate::panels::timeline::TimelinePanel::default();
        old.embedded = true;
        let mut t = Timeline { ctx: egui::Context::default(), state, panel: Default::default(), old, time: 0.0, sent: Vec::new(), cursor: egui::CursorIcon::Default, texts: Vec::new() };
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
        let out = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                cmds = panel.show(ui, state, &theme, old, &kb, false);
            });
        });
        self.cursor = out.platform_output.cursor_icon;
        self.texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        }).collect();
        self.sent.extend(cmds);
    }

    fn track(&self, name: &str) -> usize {
        self.state.sequence().tracks.iter().position(|t| t.name == name).unwrap()
    }

    /// The track's drawn rect.
    fn rect(&self, track: usize) -> egui::Rect {
        self.old.frame_cache.track_rects.iter().find(|(i, _, _)| *i == track).map(|(_, _, r)| *r).expect("the track drawn")
    }

    fn x(&self, track: usize, secs: f64) -> f32 {
        self.rect(track).left() + secs as f32 * ZOOM - self.state.timeline_scroll_x
    }

    /// Press at `from`, move through `path` with the button held (a
    /// frame each), and let go at the last point, `m` held throughout.
    fn drag(&mut self, from: egui::Pos2, path: &[egui::Pos2], m: egui::Modifiers) {
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: m };
        self.frame(vec![egui::Event::PointerMoved(from)], m);
        self.frame(vec![b(from, true)], m);
        self.frame(vec![egui::Event::PointerMoved(from + egui::vec2(8.0, 0.0))], m);
        for p in path {
            self.frame(vec![egui::Event::PointerMoved(*p)], m);
        }
        let last = *path.last().unwrap();
        self.frame(vec![b(last, false)], m);
        self.frame(Vec::new(), m);
    }

    fn stretches(&self) -> Vec<(Vec<ClipId>, bool, f64)> {
        self.sent.iter().filter_map(|c| match c {
            AppCommand::StretchClip { clip_ids, at_end, to, .. } => Some((clip_ids.clone(), *at_end, to.to_secs_f64())),
            _ => None,
        }).collect()
    }
}

/// On Video 1 a 4 s clip of a 10 s file, 0–4 s; on Video 2 a clip
/// 0–8 s whose end is the snap target. The media is in the pool so the
/// stretch plans.
fn fixture() -> (AppState, ClipId, ClipId) {
    let mut state = AppState { snap_threshold_px: 10.0, playhead: t(30.0), ..Default::default() };
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/stretch-ui.mov".into(), None);
    item.duration = t(10.0);
    let media = state.media_pool.add(item);
    let seq = &mut state.sequences[0];
    seq.add_video_track("Video 2".into());
    let v1 = seq.tracks.iter().position(|t| t.name == "Video 1").unwrap();
    let v2 = seq.tracks.iter().position(|t| t.name == "Video 2").unwrap();
    let clip = Clip::media(media, t(0.0), t(4.0), t(0.0), t(4.0));
    let other = Clip::media(media, t(0.0), t(8.0), t(0.0), t(8.0));
    let ids = (clip.id, other.id);
    seq.tracks[v1].clips.push(clip);
    seq.tracks[v2].clips.push(other);
    (state, ids.0, ids.1)
}

/// The top corner of the clip's end shows the stretch cursor and its
/// hint; dragging it out to 8 s, near Video 2's end, snaps there and
/// commits a stretch to exactly 8 s, the clip put back for the commit.
#[test]
fn the_top_corner_stretches_and_snaps_to_another_tracks_edge() {
    let (state, clip, _) = fixture();
    let mut tl = Timeline::new(state);
    let v1 = tl.track("Video 1");
    let r = tl.rect(v1);
    let corner = egui::pos2(tl.x(v1, 4.0) - 8.0, r.top() + 6.0);
    tl.frame(vec![egui::Event::PointerMoved(corner)], egui::Modifiers::NONE);
    tl.frame(vec![egui::Event::PointerMoved(corner)], egui::Modifiers::NONE);
    assert_eq!(tl.cursor, egui::CursorIcon::ResizeColumn, "the stretch cursor over the grip");
    assert!(tl.texts.iter().any(|t| t == "Drag to change speed"), "the hint: {:?}", tl.texts);
    // Out to 3 points short of 8 s: the snap takes it to Video 2's end.
    let near = egui::pos2(tl.x(v1, 8.0) - 3.0 - 8.0, corner.y);
    tl.drag(corner, &[near], egui::Modifiers::NONE);
    let s = tl.stretches();
    assert_eq!(s.len(), 1, "one stretch: {:?}", tl.sent);
    assert_eq!((s[0].0.clone(), s[0].1), (vec![clip], true));
    assert!((s[0].2 - 8.0).abs() < 1e-9, "snapped to the other track's end exactly: {}", s[0].2);
    let c = tl.state.sequence().all_clips().find(|c| c.id == clip).unwrap();
    assert_eq!(c.timeline_out, t(4.0), "the preview put back for the commit");
}

/// The edge below the grip trims, as it always has.
#[test]
fn the_edge_below_the_grip_still_trims() {
    let (state, _, _) = fixture();
    let mut tl = Timeline::new(state);
    let v1 = tl.track("Video 1");
    let r = tl.rect(v1);
    let edge = egui::pos2(tl.x(v1, 4.0) - 4.0, r.bottom() - 8.0);
    tl.drag(edge, &[edge + egui::vec2(-40.0, 0.0)], egui::Modifiers::NONE);
    assert!(tl.stretches().is_empty(), "no stretch: {:?}", tl.sent);
    assert!(tl.sent.iter().any(|c| matches!(c, AppCommand::TrimClip { .. })), "a trim: {:?}", tl.sent);
}

/// Two selected clips whose ends are at the same time stretch together
/// from a grip of either.
#[test]
fn two_selected_clips_with_the_same_end_stretch_together() {
    let (mut state, clip, other) = fixture();
    let v2 = state.sequences[0].tracks.iter().position(|t| t.name == "Video 2").unwrap();
    state.sequences[0].tracks[v2].clips[0].timeline_out = t(4.0);
    state.sequences[0].tracks[v2].clips[0].source_out = t(4.0);
    state.selected_clip = Some(clip);
    state.selected_clips = vec![clip, other];
    let mut tl = Timeline::new(state);
    let v1 = tl.track("Video 1");
    let r = tl.rect(v1);
    let corner = egui::pos2(tl.x(v1, 4.0) - 8.0, r.top() + 6.0);
    tl.drag(corner, &[corner + egui::vec2(60.0, 0.0)], egui::Modifiers::NONE);
    let s = tl.stretches();
    assert_eq!(s.len(), 1, "{:?}", tl.sent);
    assert_eq!(s[0].0, vec![clip, other], "both, the dragged one first");
}

/// A trim past the end of the clip's media stops there and offers a
/// stretch: S (as the shortcuts pass it on) takes the same drag on as a
/// stretch to where the pointer goes; let go without it, the trim lands
/// at the media's end and the toast offers the stretch.
#[test]
fn a_trim_at_the_media_end_offers_a_stretch() {
    for take_it in [true, false] {
        let (mut state, clip, _) = fixture();
        // The clip shows the file's last 4 s: nothing after its end.
        let c = state.sequences[0].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == clip).unwrap();
        c.source_in = t(6.0);
        c.source_out = t(10.0);
        let mut tl = Timeline::new(state);
        let v1 = tl.track("Video 1");
        let r = tl.rect(v1);
        let edge = egui::pos2(tl.x(v1, 4.0) - 4.0, r.bottom() - 8.0);
        let m = egui::Modifiers::NONE;
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: m };
        tl.frame(vec![egui::Event::PointerMoved(edge)], m);
        tl.frame(vec![b(edge, true)], m);
        tl.frame(vec![egui::Event::PointerMoved(edge + egui::vec2(8.0, 0.0))], m);
        let past = egui::pos2(tl.x(v1, 6.0) - 4.0, edge.y);
        tl.frame(vec![egui::Event::PointerMoved(past)], m);
        assert!(tl.state.stretch_offered, "offered at the media's end");
        assert!(tl.texts.iter().any(|t| t == "Stretch instead (S)"), "{:?}", tl.texts);
        if take_it {
            tl.state.stretch_requested = true;
            tl.frame(vec![egui::Event::PointerMoved(past + egui::vec2(1.0, 0.0))], m);
            tl.frame(vec![egui::Event::PointerMoved(past)], m);
        }
        tl.frame(vec![b(past, false)], m);
        tl.frame(Vec::new(), m);
        let trims = tl.sent.iter().filter(|c| matches!(c, AppCommand::TrimClip { .. })).count();
        if take_it {
            let s = tl.stretches();
            assert_eq!((s.len(), trims), (1, 0), "the same drag a stretch: {:?}", tl.sent);
            assert!((s[0].2 - 6.0).abs() < 0.05, "to where the pointer went: {}", s[0].2);
        } else {
            assert_eq!((tl.stretches().len(), trims), (0, 1), "{:?}", tl.sent);
            assert_eq!(tl.state.toast.message_for_test(), Some("Reached the end of the media"));
        }
        assert!(!tl.state.stretch_offered, "the offer gone with the drag");
    }
}
