//! Several keys selected and edited together (#1958), driven with pointer
//! events as the timeline's lanes and the curve editor draw them, every
//! command applied as the app's dispatch applies it: a key's picture lane
//! by path, a mask's on its grade, the rest by the windowless host.

use super::inspector_edit_host::{InspectorEditHost, InspectorSideEffects};
use crate::mask_tracks::MaskTrack;
use crate::panels::curve_editor::{CurveEditorPanel, CurveParam};
use crate::state::{AppState, KeyframeParamKind};
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId, Keyframe, Mask, MaskShape, MaskStack, RationalTime, VideoParam};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{GradeAccess, TrackKind};

fn key(t: f64, v: f32) -> Keyframe {
    Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

/// A 4 s clip on the first video track, its media in the pool: its x
/// position keyed at 1 s (10) and 2 s (20), its opacity at 3 s (0.5),
/// and a Mask node whose "Face" circle has its radius keyed at 0.5 s,
/// 1.5 s and 3 s.
fn state() -> (AppState, ClipId, klipp_nodes::NodeId) {
    let mut state = AppState::default();
    let t = |s| RationalTime::from_secs_f64(s, 24);
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/keys.mov".into(), None);
    item.duration = t(60.0);
    let media = state.media_pool.add(item);
    let mut clip = klipp_timeline::Clip::media(media, t(0.0), t(4.0), t(0.0), t(4.0));
    let mut face = Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.2 }, name: "Face".into(), ..Mask::default() };
    face.keyframes.radius = vec![key(0.5, 0.2), key(1.5, 0.3), key(3.0, 0.25)];
    let node = clip.edit_grade(|g| {
        let (node, _) = g.ensure_mask_cutout();
        g.nodes.iter_mut().find(|n| n.id == node).unwrap().params.masks = Some(MaskStack { masks: vec![face] });
        node
    });
    let clip = TestClip::of(clip).lane("pos_x", 1.0, 10.0).lane("pos_x", 2.0, 20.0).lane("opacity", 3.0, 0.5).build();
    let id = clip.id;
    let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
    state.sequences[0].tracks[v1].clips.push(clip);
    state.selected_clip = Some(id);
    (state, id, node)
}

fn clip(state: &AppState, id: ClipId) -> klipp_timeline::Clip {
    state.sequences[0].all_clips().find(|c| c.id == id).unwrap().clone()
}

/// A track's keys as `(time, value)`.
fn keys(state: &AppState, id: ClipId, kind: &KeyframeParamKind) -> Vec<(f64, f32)> {
    crate::key_selection::track_keys(&clip(state, id), kind).iter().map(|k| (k.time_secs, k.value)).collect()
}

fn radius(node: klipp_nodes::NodeId) -> KeyframeParamKind {
    KeyframeParamKind::Mask(MaskTrack { node, mask: 0, param: "radius".into() })
}

struct Sink;
impl InspectorSideEffects for Sink {
    fn sync_sequence(&self, _state: &AppState) {}
    fn send(&self, _command: AppCommand) {}
}

/// `cmds` applied as the app's dispatch applies them; any no handler
/// here took, named.
fn apply(state: &mut AppState, cmds: &[AppCommand]) -> Vec<String> {
    let mut unapplied = Vec::new();
    for cmd in cmds {
        state.before_dispatch(cmd);
        match cmd {
            AppCommand::BeginHistoryBatch => state.active_history_mut().begin_batch(),
            AppCommand::EndHistoryBatch => state.active_history_mut().end_batch(),
            // What the panels show: no clip changes.
            AppCommand::SelectClip(id) => state.selected_clip = *id,
            AppCommand::SetVideoRubberBandParam(_) | AppCommand::SequenceUpdated | AppCommand::BrowserSkimAudioStart { .. } => {}
            _ if super::colour::mask_key_edit(state, cmd, true).is_some() => {}
            _ if super::keyframes::audio_effect_key_write(state, cmd) => {}
            _ if state.path_command(cmd).is_some() => {}
            _ => {
                let mut host = InspectorEditHost { state: &mut *state, side: &Sink };
                if !host.apply_effect_keyframes(cmd) && !host.apply_video_keyframes(cmd) && !host.apply_keyframe_drag(cmd) && !host.apply(cmd) {
                    unapplied.push(format!("{cmd:?}"));
                }
            }
        }
    }
    unapplied
}

/// Undo, as the app's document path applies it.
fn undo(state: &mut AppState) {
    let entry = state.active_history_mut().undo().expect("a step to undo");
    let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps };
    view.apply_inverse(&entry.command);
}

/// Every key diamond a lane drew, at its centre, left to right: the
/// four-point shapes `half` pixels across.
fn diamonds(shapes: &[egui::epaint::ClippedShape], half: f32) -> Vec<egui::Pos2> {
    fn walk(shape: &egui::Shape, half: f32, out: &mut Vec<egui::Pos2>) {
        match shape {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, half, out)),
            egui::Shape::Path(p) if p.points.len() == 4 && p.closed => {
                let [a, b, c, d] = [p.points[0], p.points[1], p.points[2], p.points[3]];
                if (a.x - c.x).abs() < 0.01 && (b.y - d.y).abs() < 0.01 && ((b.x - a.x) - half).abs() < 0.01 {
                    out.push(egui::pos2(a.x, b.y));
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for s in shapes {
        walk(&s.shape, half, &mut out);
    }
    out.sort_by(|a, b| a.x.total_cmp(&b.x));
    out.dedup_by(|a, b| a.distance(*b) < 0.5);
    out
}

/// The timeline, drawn as the app draws it, its commands applied.
struct Timeline {
    ctx: egui::Context,
    state: AppState,
    panel: crate::timeline::NewTimelinePanel,
    old: crate::panels::timeline::TimelinePanel,
    time: f64,
    shapes: Vec<egui::epaint::ClippedShape>,
    sent: Vec<AppCommand>,
    unapplied: Vec<String>,
}

impl Timeline {
    fn new(state: AppState) -> Self {
        let mut old = crate::panels::timeline::TimelinePanel::default();
        old.embedded = true;
        let mut t = Timeline { ctx: egui::Context::default(), state, panel: Default::default(), old, time: 0.0, shapes: Vec::new(), sent: Vec::new(), unapplied: Vec::new() };
        t.state.timeline_zoom = 200.0;
        t.state.timeline_scroll_x = 0.0;
        t.frame(Vec::new(), egui::Modifiers::NONE);
        t
    }

    fn frame(&mut self, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
        self.time += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 600.0))), time: Some(self.time), events, modifiers, ..Default::default() };
        let (theme, kb) = (KlippTheme::default(), crate::keybindings::KeyBindings::default());
        let (state, panel, old) = (&mut self.state, &mut self.panel, &mut self.old);
        let mut cmds = Vec::new();
        let out = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                cmds = panel.show(ui, state, &theme, old, &kb, false);
            });
        });
        self.unapplied.extend(apply(&mut self.state, &cmds));
        self.sent.extend(cmds);
        self.shapes = out.shapes;
    }

    /// The video band's key diamonds now.
    fn keys(&mut self) -> Vec<egui::Pos2> {
        self.frame(Vec::new(), egui::Modifiers::NONE);
        let mut found = diamonds(&self.shapes, 8.0);
        found.extend(diamonds(&self.shapes, 9.0));
        found.sort_by(|a, b| a.x.total_cmp(&b.x));
        found
    }

    fn click(&mut self, at: egui::Pos2, modifiers: egui::Modifiers) {
        // Apart from the last click, as a person's are: never a double.
        self.time += 1.0;
        self.frame(vec![egui::Event::PointerMoved(at)], modifiers);
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers }], modifiers);
        }
        self.frame(Vec::new(), egui::Modifiers::NONE);
    }

    fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
        let m = egui::Modifiers::NONE;
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: m };
        self.frame(vec![egui::Event::PointerMoved(from)], m);
        self.frame(vec![b(from, true)], m);
        // Past egui's drag threshold, still on the key.
        self.frame(vec![egui::Event::PointerMoved(from + (to - from).normalized() * 7.0)], m);
        for i in 1..=5 {
            self.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 5.0))], m);
        }
        self.frame(vec![egui::Event::PointerMoved(to), b(to, false)], m);
        self.frame(Vec::new(), m);
    }
}

/// On the timeline's lanes, Shift-click adds keys across two tracks (x
/// position, then opacity), each ringed; a drag of one moves all three in
/// time by the same amount, the clip left where it is, as one undo step
/// that undo takes back exactly.
#[test]
fn shift_click_on_the_lanes_selects_across_tracks_and_drags_them_as_one_step() {
    let (state, id, _) = state();
    let mut t = Timeline::new(state);
    let (x, o) = (KeyframeParamKind::Video(VideoParam::PositionX), KeyframeParamKind::Video(VideoParam::Opacity));
    t.state.video_rubber_band_param = VideoParam::PositionX;
    let pos = t.keys();
    assert_eq!(pos.len(), 2, "the x position's two keys drawn: {pos:?}");
    t.click(pos[0], egui::Modifiers::NONE);
    t.click(pos[1], egui::Modifiers::SHIFT);
    assert_eq!(t.state.key_selection.len(), 2, "{:?}", t.state.key_selection);
    assert_eq!(t.state.selected_clip, Some(id), "a Shift-click on a key leaves the clip selected");
    t.state.video_rubber_band_param = VideoParam::Opacity;
    let op = t.keys();
    assert_eq!(op.len(), 1, "the opacity's key drawn: {op:?}");
    t.click(op[0], egui::Modifiers::SHIFT);
    assert_eq!(t.state.key_selection.len(), 3, "a key on another track added: {:?}", t.state.key_selection);
    assert!(t.state.key_selection.holds(id, &o, 3.0) && t.state.key_selection.holds(id, &x, 1.0) && t.state.key_selection.holds(id, &x, 2.0));

    let (before, at, steps) = (clip(&t.state, id).picture_v2().unwrap(), clip(&t.state, id).timeline_in, t.state.active_history().undo_count());
    // Half a second earlier: 100 px at 200 px a second.
    let from = t.keys()[0];
    t.drag(from, from - egui::vec2(100.0, 0.0));
    assert!(t.unapplied.is_empty(), "{:?}", t.unapplied);
    let near = |a: &[(f64, f32)], b: &[(f64, f32)]| a.len() == b.len() && a.iter().zip(b).all(|(p, q)| (p.0 - q.0).abs() < 0.03 && (p.1 - q.1).abs() < 1e-4);
    assert!(near(&keys(&t.state, id, &o), &[(2.5, 0.5)]), "{:?}", keys(&t.state, id, &o));
    assert!(near(&keys(&t.state, id, &x), &[(0.5, 10.0), (1.5, 20.0)]), "the x position's keys moved with it: {:?}", keys(&t.state, id, &x));
    assert_eq!(clip(&t.state, id).timeline_in, at, "the clip stays where it is");
    assert_eq!(t.state.active_history().undo_count(), steps + 1, "one step for the drag");
    assert_eq!(t.state.key_selection.len(), 3, "the keys stay selected where they are now");
    undo(&mut t.state);
    assert_eq!(clip(&t.state, id).picture_v2().unwrap(), before, "undo takes the whole drag back");
}

/// A plain click on a key of several selects it alone; on another clip's
/// key, the selection starts afresh there.
#[test]
fn a_plain_click_or_another_clip_starts_afresh() {
    let (mut state, id, _) = state();
    let mut other = clip(&state, id);
    other.id = ClipId::new();
    let t = |s| RationalTime::from_secs_f64(s, 24);
    (other.timeline_in, other.timeline_out) = (t(5.0), t(9.0));
    let other_id = other.id;
    state.sequences[0].tracks.iter_mut().find(|t| t.kind == TrackKind::Video).unwrap().clips.push(other);
    let x = KeyframeParamKind::Video(VideoParam::PositionX);
    let c = clip(&state, id);
    crate::key_selection::click(&mut state, &c, &x, 0, false);
    crate::key_selection::click(&mut state, &c, &x, 1, true);
    assert_eq!(state.key_selection.len(), 2);
    crate::key_selection::click(&mut state, &c, &x, 1, false);
    assert_eq!(state.key_selection.len(), 1, "a plain click: that key alone");
    crate::key_selection::click(&mut state, &c, &x, 0, true);
    let o = clip(&state, other_id);
    crate::key_selection::click(&mut state, &o, &x, 0, true);
    assert_eq!(state.key_selection.clip_id, Some(other_id));
    assert_eq!(state.key_selection.len(), 1, "another clip's key: afresh, Shift or not");
    assert_eq!(state.selected_keyframe.as_ref().map(|s| s.clip_id), Some(other_id));
}

/// A drag that would put a selected key on an unselected one's time on
/// its track moves nothing; the next clear time moves them all.
#[test]
fn a_drag_onto_an_unselected_key_is_refused() {
    let (mut state, id, node) = state();
    let r = radius(node);
    let c = clip(&state, id);
    crate::key_selection::click(&mut state, &c, &r, 0, false);
    crate::key_selection::click(&mut state, &c, &r, 1, true);
    let begin = crate::key_selection::begin_drag(&mut state, &c, None);
    apply(&mut state, &begin);
    // 1.5 s onto the key at 3 s.
    let c = clip(&state, id);
    assert!(crate::key_selection::drag_to(&mut state, &c, 1.5, 0.0).is_empty(), "onto an unselected key: nothing");
    let moved = crate::key_selection::drag_to(&mut state, &c, 1.0, 0.0);
    assert_eq!(moved.len(), 2, "{moved:?}");
    apply(&mut state, &moved);
    let end = crate::key_selection::end_drag(&mut state);
    apply(&mut state, &end);
    let times: Vec<f64> = keys(&state, id, &r).iter().map(|k| k.0).collect();
    assert_eq!(times, vec![1.5, 2.5, 3.0]);
}

/// The curve editor, drawn as the app draws it, its commands applied.
struct Editor {
    state: AppState,
    clip: ClipId,
    node: klipp_nodes::NodeId,
    panel: CurveEditorPanel,
    ctx: egui::Context,
    clock: f64,
    texts: Vec<String>,
}

impl Editor {
    fn new() -> Self {
        let (state, clip, node) = state();
        Self::showing(state, clip, node, CurveParam::Mask { track: MaskTrack { node, mask: 0, param: "radius".into() }, name: "Face \u{00B7} Radius".into() })
    }

    fn showing(state: AppState, clip: ClipId, node: klipp_nodes::NodeId, param: CurveParam) -> Self {
        let ctx = egui::Context::default();
        crate::icons::install(&ctx);
        let mut panel = CurveEditorPanel::new();
        panel.show_track(param);
        let mut e = Editor { state, clip, node, panel, ctx, clock: 0.0, texts: Vec::new() };
        e.frame(Vec::new(), egui::Modifiers::NONE);
        e.frame(Vec::new(), egui::Modifiers::NONE);
        e
    }

    fn frame(&mut self, events: Vec<egui::Event>, modifiers: egui::Modifiers) -> Vec<(String, egui::Pos2)> {
        self.clock += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 500.0))), time: Some(self.clock), events, modifiers, ..Default::default() };
        let theme = KlippTheme::default();
        let mut cmds = Vec::new();
        let (state, panel) = (&mut self.state, &mut self.panel);
        let out = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                cmds.extend(panel.show(ui, state, &theme));
                crate::panels::timeline::TimelinePanel::show_keyframe_context_menu(ui, state, &mut cmds);
            });
        });
        let unapplied = apply(&mut self.state, &cmds);
        assert!(unapplied.is_empty(), "{unapplied:?}");
        let drawn: Vec<(String, egui::Pos2)> = out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect();
        self.texts = drawn.iter().map(|(t, _)| t.clone()).collect();
        drawn
    }

    fn at(&self, t: f64, v: f32) -> egui::Pos2 {
        self.panel.screen_at(t, v).expect("the graph drawn")
    }

    fn press(&mut self, at: egui::Pos2, button: egui::PointerButton, modifiers: egui::Modifiers) {
        self.frame(vec![egui::Event::PointerMoved(at)], modifiers);
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerButton { pos: at, button, pressed, modifiers }], modifiers);
        }
        self.frame(Vec::new(), egui::Modifiers::NONE);
    }

    fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
        let m = egui::Modifiers::NONE;
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: m };
        self.frame(vec![egui::Event::PointerMoved(from), b(from, true)], m);
        self.frame(vec![egui::Event::PointerMoved(from + (to - from).normalized() * 7.0)], m);
        for i in 1..=5 {
            self.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 5.0))], m);
        }
        self.frame(vec![egui::Event::PointerMoved(to), b(to, false)], m);
        self.frame(Vec::new(), m);
    }

    fn radius(&self) -> Vec<(f64, f32)> {
        keys(&self.state, self.clip, &radius(self.node))
    }
}

/// A box drawn over the graph selects the keys inside it, and the editor
/// says how many; a selected key dragged moves them all, as one step.
#[test]
fn a_box_in_the_curve_editor_selects_and_its_keys_drag_together() {
    let mut e = Editor::new();
    assert!(!e.texts.iter().any(|t| t.ends_with(" keys")), "{:?}", e.texts);
    // Round the keys at 0.5 s and 1.5 s, not the one at 3 s.
    let (a, b) = (e.at(0.5, 0.2), e.at(1.5, 0.3));
    let (from, to) = (egui::pos2(a.x - 20.0, a.y.max(b.y) + 20.0), egui::pos2(b.x + 20.0, a.y.min(b.y) - 20.0));
    e.drag(from, to);
    let r = radius(e.node);
    assert_eq!(e.state.key_selection.len(), 2, "{:?}", e.state.key_selection);
    assert!(e.state.key_selection.holds(e.clip, &r, 0.5) && e.state.key_selection.holds(e.clip, &r, 1.5));
    e.frame(Vec::new(), egui::Modifiers::NONE);
    assert!(e.texts.iter().any(|t| t == "2 keys"), "the count shown: {:?}", e.texts);

    let steps = e.state.active_history().undo_count();
    let before = e.radius();
    let (from, to) = (e.at(1.5, 0.3), e.at(2.0, 0.3));
    e.drag(from, to);
    let now = e.radius();
    assert!((now[0].0 - 1.0).abs() < 0.05 && (now[1].0 - 2.0).abs() < 0.05 && now[2] == (3.0, 0.25), "both moved, the third not: {now:?}");
    assert_eq!(e.state.active_history().undo_count(), steps + 1, "one step");
    undo(&mut e.state);
    assert_eq!(e.radius(), before, "undo takes it back exactly");
}

/// With several keys selected, the key menu's choice goes to each, one
/// step; Delete takes every one off, one step.
#[test]
fn the_menu_and_delete_act_on_every_selected_key() {
    let mut e = Editor::new();
    let r = radius(e.node);
    let c = clip(&e.state, e.clip);
    crate::key_selection::click(&mut e.state, &c, &r, 0, false);
    let at = e.at(1.5, 0.3);
    e.press(at, egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    assert_eq!(e.state.key_selection.len(), 2, "Shift-click in the editor adds: {:?}", e.state.key_selection);
    let steps = e.state.active_history().undo_count();
    e.press(at, egui::PointerButton::Secondary, egui::Modifiers::NONE);
    let hold = e.frame(Vec::new(), egui::Modifiers::NONE).into_iter().rev().find(|(t, _)| t.trim() == "Hold").expect("the menu's Hold").1;
    e.press(hold, egui::PointerButton::Primary, egui::Modifiers::NONE);
    let interp: Vec<_> = crate::key_selection::track_keys(&clip(&e.state, e.clip), &r).iter().map(|k| k.interpolation).collect();
    assert_eq!(&interp[..2], &[klipp_core::Interpolation::Step; 2], "{interp:?}");
    assert_eq!(e.state.active_history().undo_count(), steps + 1, "one step for both");

    let all = crate::key_selection::delete_all(&e.state).expect("several to delete");
    apply(&mut e.state, &all);
    assert_eq!(e.radius(), vec![(3.0, 0.25)], "both taken off");
    assert_eq!(e.state.active_history().undo_count(), steps + 2, "one step for both");
}

/// A sound key's move records no step of its own: the selection's moves
/// of them are held in a keyframe drag of the clip's keys, which does.
#[test]
fn sound_keys_move_inside_a_keyframe_drag() {
    let (mut state, id, _) = state();
    let v = KeyframeParamKind::Audio(klipp_core::AudioParam::Volume);
    for track in &mut state.sequences[0].tracks {
        if let Some(c) = track.find_clip_mut(id) {
            c.volume_keyframes = vec![key(1.0, 0.5), key(2.0, 1.0)];
        }
    }
    let c = clip(&state, id);
    crate::key_selection::click(&mut state, &c, &v, 0, false);
    crate::key_selection::click(&mut state, &c, &v, 1, true);
    crate::key_selection::begin_drag(&mut state, &c, None);
    let moved = crate::key_selection::drag_to(&mut state, &c, 0.5, 0.0);
    let kinds: Vec<&str> = moved.iter().map(|c| match c {
        AppCommand::BeginKeyframeDrag { .. } => "begin",
        AppCommand::MoveAudioKeyframe { .. } => "move",
        AppCommand::EndKeyframeDrag { .. } => "end",
        _ => "other",
    }).collect();
    assert_eq!(kinds, ["begin", "move", "move", "end"], "{moved:?}");
}

/// The fixture with a 4 s clip on the first sound track too, its media in
/// the pool, carrying a Gain effect whose `gain_db` is keyed at 1 s (0)
/// and 2 s (6), shown on the sound band; that clip selected.
fn fx_state() -> (AppState, ClipId, klipp_nodes::NodeId) {
    let (mut state, _, node) = state();
    let t = |s| RationalTime::from_secs_f64(s, 24);
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/fx.wav".into(), None);
    item.duration = t(60.0);
    let media = state.media_pool.add(item);
    let mut clip = klipp_timeline::Clip::media(media, t(0.0), t(4.0), t(0.0), t(4.0));
    clip.audio_effects = vec![klipp_core::AudioEffectInstance { effect_type: "gain".into(), enabled: true, wet_dry: 1.0, params: vec![("gain_db".into(), 0.0)] }];
    clip.audio_effect_keyframes.insert("0:gain_db".into(), vec![key(1.0, 0.0), key(2.0, 6.0)]);
    let id = clip.id;
    let a1 = state.sequences[0].tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
    state.sequences[0].tracks[a1].clips.push(clip);
    state.selected_clip = Some(id);
    state.rubber_band_param = crate::state::RubberBandParam::EffectParam { effect_index: 0, param_name: "gain_db".into() };
    (state, id, node)
}

fn gain() -> KeyframeParamKind {
    KeyframeParamKind::AudioEffect { effect_index: 0, param_name: "gain_db".into() }
}

/// A sound effect's key on the timeline's band, pressed and dragged,
/// moves while the button is held, and the drag is one undo step that
/// undo takes back exactly (#1990).
#[test]
fn an_effect_key_drags_on_the_band_as_one_step() {
    let (state, id, _) = fx_state();
    let mut t = Timeline::new(state);
    t.frame(Vec::new(), egui::Modifiers::NONE);
    let mut found = diamonds(&t.shapes, 4.0);
    found.extend(diamonds(&t.shapes, 5.5));
    found.sort_by(|a, b| a.x.total_cmp(&b.x));
    assert_eq!(found.len(), 2, "the band's two keys drawn: {found:?}");
    let before = keys(&t.state, id, &gain());
    let steps = t.state.active_history().undo_count();
    // Half a second later: 100 px at 200 px a second.
    t.drag(found[0], found[0] + egui::vec2(100.0, 0.0));
    assert!(t.unapplied.is_empty(), "{:?}", t.unapplied);
    let now = keys(&t.state, id, &gain());
    assert!((now[0].0 - 1.5).abs() < 0.03 && now[1] == (2.0, 6.0), "the key moved while held: {now:?}");
    assert_eq!(t.state.active_history().undo_count(), steps + 1, "one step for the drag");
    undo(&mut t.state);
    assert_eq!(keys(&t.state, id, &gain()), before, "undo takes it back exactly");
}

/// A key dragged past its neighbour stays the one dragged: its index
/// follows it as the keys sort (#1990).
#[test]
fn an_effect_key_dragged_past_another_stays_itself() {
    let (state, id, _) = fx_state();
    let mut t = Timeline::new(state);
    t.frame(Vec::new(), egui::Modifiers::NONE);
    let mut found = diamonds(&t.shapes, 4.0);
    found.extend(diamonds(&t.shapes, 5.5));
    found.sort_by(|a, b| a.x.total_cmp(&b.x));
    // From 1 s to 2.6 s, past the key at 2 s, and on to 2.8 s.
    let from = found[0];
    let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: egui::Modifiers::NONE };
    let m = egui::Modifiers::NONE;
    t.frame(vec![egui::Event::PointerMoved(from)], m);
    t.frame(vec![b(from, true)], m);
    for dx in [7.0, 100.0, 320.0, 360.0] {
        t.frame(vec![egui::Event::PointerMoved(from + egui::vec2(dx, 0.0))], m);
    }
    t.frame(vec![egui::Event::PointerMoved(from + egui::vec2(360.0, 0.0)), b(from + egui::vec2(360.0, 0.0), false)], m);
    let now = keys(&t.state, id, &gain());
    assert!(now[0] == (2.0, 6.0) && (now[1].0 - 2.8).abs() < 0.03 && now[1].1.abs() < 1e-3, "the key dragged went on past, its neighbour left: {now:?}");
}

/// The curve editor moves a sound effect's key too, one step.
#[test]
fn the_curve_editor_moves_an_effect_key_as_one_step() {
    let (state, id, node) = fx_state();
    let mut e = Editor::showing(state, id, node, CurveParam::AudioEffect("0:gain_db".into()));
    let before = keys(&e.state, id, &gain());
    let steps = e.state.active_history().undo_count();
    let (from, to) = (e.at(1.0, 0.0), e.at(1.5, 0.0));
    e.drag(from, to);
    let now = keys(&e.state, id, &gain());
    assert!((now[0].0 - 1.5).abs() < 0.05 && now[1] == (2.0, 6.0), "{now:?}");
    assert_eq!(e.state.active_history().undo_count(), steps + 1, "one step for the drag");
    undo(&mut e.state);
    assert_eq!(keys(&e.state, id, &gain()), before);
}

/// A key dragged on a lane moves the key, never the clip under it: one
/// key, several, or a sound effect's (#1990).
#[test]
fn a_key_drag_leaves_the_clip_where_it_is() {
    let (state, id, _) = state();
    let mut t = Timeline::new(state);
    t.state.video_rubber_band_param = VideoParam::PositionX;
    let at = clip(&t.state, id).timeline_in;
    let pos = t.keys();
    t.drag(pos[0], pos[0] + egui::vec2(60.0, 0.0));
    assert!(t.sent.iter().all(|c| !matches!(c, AppCommand::MoveClip { .. } | AppCommand::MoveClipToTrack { .. })), "a clip move sent: {:?}", t.sent.iter().filter(|c| matches!(c, AppCommand::MoveClip { .. })).collect::<Vec<_>>());
    assert_eq!(clip(&t.state, id).timeline_in, at);
    let x = keys(&t.state, id, &KeyframeParamKind::Video(VideoParam::PositionX));
    assert!((x[0].0 - 1.3).abs() < 0.03, "the key moved: {x:?}");
}

/// On a sound track too, where a moved clip is sent as a move: a
/// volume key's drag and a sound effect key's drag leave the clip.
#[test]
fn a_sound_key_drag_leaves_the_clip_where_it_is() {
    for effect in [false, true] {
        let (mut state, id, _) = fx_state();
        if !effect {
            state.rubber_band_param = crate::state::RubberBandParam::ClipParam(klipp_core::AudioParam::Volume);
            for track in &mut state.sequences[0].tracks {
                if let Some(c) = track.find_clip_mut(id) {
                    c.volume_keyframes = vec![key(1.0, 0.5), key(2.0, 1.0)];
                }
            }
        }
        let mut t = Timeline::new(state);
        t.frame(Vec::new(), egui::Modifiers::NONE);
        let mut found: Vec<egui::Pos2> = [4.0, 5.5, 8.0, 9.0].iter().flat_map(|h| diamonds(&t.shapes, *h)).collect();
        found.sort_by(|a, b| a.x.total_cmp(&b.x));
        assert!(found.len() >= 2, "effect {effect}: {found:?}");
        t.sent.clear();
        t.drag(found[0], found[0] + egui::vec2(60.0, 0.0));
        assert!(t.sent.iter().all(|c| !matches!(c, AppCommand::MoveClip { .. } | AppCommand::MoveClipToTrack { .. })), "effect {effect}: a clip move sent");
        assert_eq!(clip(&t.state, id).timeline_in, RationalTime::from_secs_f64(0.0, 24), "effect {effect}");
    }
}

/// The dope sheet (#2240): every keyed track of the clip a row, named as
/// the curves name it; a key clicked, another Shift-clicked on another
/// row, and one of them dragged, both move by the same time, one undo
/// step; a box across rows selects what it holds; a row's name
/// double-clicked opens its curve.
#[test]
fn the_dope_sheet_moves_keys_on_several_rows_together() {
    let mut e = Editor::new();
    let drawn = e.frame(Vec::new(), egui::Modifiers::NONE);
    let tab = drawn.iter().find(|(t, _)| t == "Dope Sheet").expect("the Dope Sheet tab").1;
    e.press(tab, egui::PointerButton::Primary, egui::Modifiers::NONE);
    assert!(e.panel.showing_sheet());
    e.frame(Vec::new(), egui::Modifiers::NONE);
    for name in ["Opacity", "Position X", "Face \u{00B7} Radius"] {
        assert!(e.texts.iter().any(|t| t == name), "a row for {name}: {:?}", e.texts);
    }
    // Rows: Opacity (3 s), Position X (1 s, 2 s), the mask's radius.
    let (opacity, pos_x) = (KeyframeParamKind::Video(VideoParam::Opacity), KeyframeParamKind::Video(VideoParam::PositionX));
    let a = e.panel.sheet_at(1, 1.0).unwrap();
    e.press(a, egui::PointerButton::Primary, egui::Modifiers::NONE);
    let b = e.panel.sheet_at(0, 3.0).unwrap();
    e.press(b, egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    assert_eq!(e.state.key_selection.len(), 2, "two keys on two rows");
    let undo_before = e.state.active_history().undo_count();
    let to = e.panel.sheet_at(1, 1.5).unwrap();
    e.drag(a, to);
    assert_eq!(keys(&e.state, e.clip, &pos_x), vec![(1.5, 10.0), (2.0, 20.0)], "the key dragged");
    assert_eq!(keys(&e.state, e.clip, &opacity), vec![(3.5, 0.5)], "and the other selected one, by the same time");
    assert_eq!(e.state.active_history().undo_count(), undo_before + 1, "one undo step");
    // A box over the first two rows from 0 s to 1.75 s: the x key at 1.5 s.
    let (from, until) = (e.panel.sheet_at(0, 0.05).unwrap(), e.panel.sheet_at(1, 1.75).unwrap());
    e.drag(from - egui::vec2(0.0, 8.0), until + egui::vec2(0.0, 8.0));
    assert_eq!(e.state.key_selection.len(), 1, "{:?}", e.state.key_selection);
    // A row's name double-clicked: its curve.
    let drawn = e.frame(Vec::new(), egui::Modifiers::NONE);
    // The row's name, under the header's own list of curves.
    let name = drawn.iter().filter(|(t, _)| t == "Position X").map(|(_, p)| *p).max_by(|a, b| a.y.total_cmp(&b.y)).unwrap();
    e.frame(vec![egui::Event::PointerMoved(name)], egui::Modifiers::NONE);
    for _ in 0..2 {
        for pressed in [true, false] {
            e.frame(vec![egui::Event::PointerButton { pos: name, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], egui::Modifiers::NONE);
        }
    }
    e.frame(Vec::new(), egui::Modifiers::NONE);
    assert!(!e.panel.showing_sheet(), "the curves, for Position X");
}

/// #2279: a key dragged in the curve editor lands on the playhead when
/// near it (even off a frame), else on a whole frame, and with Shift
/// where it is let go.
#[test]
fn a_dragged_key_snaps_to_the_playhead_and_to_frames() {
    let (state, clip, node) = state();
    let mut e = Editor::showing(state, clip, node, CurveParam::Video(VideoParam::PositionX));
    let pos_x = KeyframeParamKind::Video(VideoParam::PositionX);
    let fps = e.state.sequence().fps.as_f64();
    // The playhead at 1.6 s; the key at 2 s (20) dragged to just past it.
    e.state.playhead = klipp_core::Timecode::from_secs_f64(1.6, e.state.sequence().fps.rate());
    e.frame(Vec::new(), egui::Modifiers::NONE);
    let playhead = e.state.playhead.to_secs_f64();
    let (from, near) = (e.at(2.0, 20.0), e.at(playhead + 0.004, 20.0));
    e.drag(from, near);
    assert!(keys(&e.state, e.clip, &pos_x).iter().any(|(t, _)| (t - playhead).abs() < 1e-6), "on the playhead: {:?}", keys(&e.state, e.clip, &pos_x));
    // Away from it and the other key: a whole frame.
    let (from, away) = (e.at(playhead, 20.0), e.at(3.13, 20.0));
    e.drag(from, away);
    let moved = keys(&e.state, e.clip, &pos_x).into_iter().map(|(t, _)| t).find(|t| (t - 3.13).abs() < 0.05).expect("the key near 3.13 s");
    assert!(((moved * fps) - (moved * fps).round()).abs() < 1e-6, "on a frame: {moved}");
}

/// #2278: the key tools. A key at the playhead takes the curve's value
/// there; a selected key copied and pasted at the playhead lands there
/// with its value.
#[test]
fn the_key_tools_add_a_key_at_the_playhead_and_paste_copied_keys() {
    let (state, clip, node) = state();
    let mut e = Editor::showing(state, clip, node, CurveParam::Video(VideoParam::PositionX));
    let pos_x = KeyframeParamKind::Video(VideoParam::PositionX);
    let rate = e.state.sequence().fps.rate();
    let button = |e: &mut Editor, label: &str| -> egui::Pos2 {
        let drawn = e.frame(Vec::new(), egui::Modifiers::NONE);
        drawn.iter().find(|(t, _)| t == label).unwrap_or_else(|| panic!("a {label} button: {:?}", e.texts)).1
    };
    // A key at the playhead, 3 s: the curve holds 20 there.
    e.state.playhead = klipp_core::Timecode::from_secs_f64(3.0, rate);
    let add = button(&mut e, "\u{25C6}");
    e.press(add, egui::PointerButton::Primary, egui::Modifiers::NONE);
    assert_eq!(keys(&e.state, e.clip, &pos_x), vec![(1.0, 10.0), (2.0, 20.0), (3.0, 20.0)]);
    // The key at 1 s selected, copied, and pasted at 3.5 s.
    let first = e.at(1.0, 10.0);
    e.press(first, egui::PointerButton::Primary, egui::Modifiers::NONE);
    let copy = button(&mut e, "Copy");
    e.press(copy, egui::PointerButton::Primary, egui::Modifiers::NONE);
    e.state.playhead = klipp_core::Timecode::from_secs_f64(3.5, rate);
    let paste = button(&mut e, "Paste");
    e.press(paste, egui::PointerButton::Primary, egui::Modifiers::NONE);
    // (A click on a key nudges its value by the pointer's rounding: #2285.)
    assert!(keys(&e.state, e.clip, &pos_x).iter().any(|(t, v)| (t - 3.5).abs() < 1e-9 && (v - 10.0).abs() < 1e-2), "{:?}", keys(&e.state, e.clip, &pos_x));
}
