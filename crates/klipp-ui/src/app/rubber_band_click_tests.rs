//! A clip's volume line on the timeline, driven through the timeline
//! panel with pointer events: a click on the line selects and leaves the
//! level; a drag moves the line by as far as the pointer went.

use crate::panels::timeline::{audio_param_value_to_y, audio_param_y_to_value};
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, AudioParam, ClipId, RationalTime};
use klipp_timeline::Clip;

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
    fn frame(&mut self, events: Vec<egui::Event>) {
        self.time += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 700.0))), time: Some(self.time), events, ..Default::default() };
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

    /// Press at `from`, move through `path` a frame each with the button
    /// held, and let go where the pointer ends.
    fn gesture(&mut self, from: egui::Pos2, path: &[egui::Pos2]) {
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: egui::Modifiers::NONE };
        self.frame(vec![egui::Event::PointerMoved(from)]);
        self.frame(vec![b(from, true)]);
        self.frame(Vec::new());
        for p in path {
            self.frame(vec![egui::Event::PointerMoved(*p)]);
        }
        self.frame(vec![b(*path.last().unwrap_or(&from), false)]);
        self.frame(Vec::new());
    }

    fn volumes(&self, clip: ClipId) -> Vec<f32> {
        self.sent.iter().filter_map(|c| match c {
            AppCommand::UpdateClipAudio { clip_id, volume, .. } if *clip_id == clip => Some(*volume),
            _ => None,
        }).collect()
    }
}

/// On Audio 1 an 8 s clip at volume 0.25, its line drawn flat.
fn timeline() -> (Timeline, ClipId) {
    let mut state = AppState { playhead: t(30.0), timeline_zoom: 50.0, timeline_scroll_x: 0.0, ..Default::default() };
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/rubber-band-ui.wav".into(), None);
    item.duration = t(10.0);
    let media = state.media_pool.add(item);
    let seq = &mut state.sequences[0];
    let a1 = seq.tracks.iter().position(|t| t.name == "Audio 1").unwrap();
    let mut clip = Clip::media(media, t(0.0), t(8.0), t(0.0), t(8.0));
    clip.volume = 0.25;
    let id = clip.id;
    seq.tracks[a1].clips.push(clip);
    let mut old = crate::panels::timeline::TimelinePanel::default();
    old.embedded = true;
    let mut tl = Timeline { ctx: egui::Context::default(), state, panel: Default::default(), old, time: 0.0, sent: Vec::new() };
    tl.frame(Vec::new());
    tl.frame(Vec::new());
    (tl, id)
}

/// A click 4 points off the line, close enough to take it, sets no
/// level: it used to set the level wherever the click landed, 0.25
/// becoming a little less on a click below the line.
#[test]
fn a_click_on_the_volume_line_leaves_the_level() {
    let (mut tl, clip) = timeline();
    let rect = *tl.old.frame_cache.clip_screen_rects.get(&clip).expect("the clip drawn");
    let line = audio_param_value_to_y(rect, AudioParam::Volume, 0.25);
    assert!(rect.y_range().contains(line), "the line inside the clip: {line} in {rect:?}");
    let at = egui::pos2(rect.left() + rect.width() * 0.4, line + 4.0);
    tl.gesture(at, &[]);
    assert!(tl.sent.iter().any(|c| matches!(c, AppCommand::BeginAudioDrag { clip_id, .. } if *clip_id == clip)), "the press took the line: {:?}", tl.sent);
    assert!(tl.volumes(clip).iter().all(|v| *v == 0.25), "a click keeps 0.25: {:?}", tl.volumes(clip));
    // A pointer that wobbles under the threshold is still a click.
    tl.gesture(at, &[at + egui::vec2(1.0, 2.0)]);
    assert!(tl.volumes(clip).iter().all(|v| *v == 0.25), "a wobble keeps 0.25: {:?}", tl.volumes(clip));
}

/// A drag 20 points up from 4 points under the line moves the line 20
/// points up from where it was, not from where the press landed.
#[test]
fn a_drag_moves_the_volume_line_by_the_pointers_travel() {
    let (mut tl, clip) = timeline();
    let rect = *tl.old.frame_cache.clip_screen_rects.get(&clip).expect("the clip drawn");
    let line = audio_param_value_to_y(rect, AudioParam::Volume, 0.25);
    let at = egui::pos2(rect.left() + rect.width() * 0.4, line + 4.0);
    tl.gesture(at, &[at - egui::vec2(0.0, 10.0), at - egui::vec2(0.0, 20.0)]);
    let want = audio_param_y_to_value(rect, AudioParam::Volume, line - 20.0);
    let got = *tl.volumes(clip).last().expect("the drag set a level");
    assert!(want > 0.25 && (got - want).abs() < 1e-4, "{got} for {want}");
}
