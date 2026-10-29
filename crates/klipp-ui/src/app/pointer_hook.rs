//! The control socket's `pointer` test hook (#3056): pointer steps played
//! into the window's input one a frame, as a mouse's events reach egui,
//! so a harness's drag goes through the code a person's hand does.

use klipp_core::pointer::{PointerAction, PointerAt, PointerButton, PointerStep};
use std::collections::VecDeque;

/// The steps to play and where the pointer is.
#[derive(Default)]
pub(crate) struct PointerPlayer {
    steps: VecDeque<PointerStep>,
    /// Frames the front step has played.
    played: u32,
    at: egui::Pos2,
    /// The keys held with a button the steps pressed and haven't let go:
    /// they stay held between requests, as a hand holds them through a
    /// drag, instead of the real keyboard's (none) taking over.
    held: Option<egui::Modifiers>,
}

impl PointerPlayer {
    pub(crate) fn queue(&mut self, steps: &[PointerStep]) {
        self.steps.extend(steps.iter().cloned());
    }

    /// This frame's events from the front step, added to `raw`; a
    /// timeline point resolved by `timeline_point` (seconds, track).
    pub(crate) fn play(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput, timeline_point: impl Fn(f64, usize) -> Option<egui::Pos2>) {
        let Some(step) = self.steps.front().cloned() else {
            if let Some(m) = self.held {
                raw.modifiers = m;
            }
            return;
        };
        let first = self.played == 0;
        if first {
            self.at = match step.at {
                Some(PointerAt::Screen { x, y }) => egui::pos2(x, y),
                Some(PointerAt::Timeline { secs, track }) => timeline_point(secs, track).unwrap_or_else(|| {
                    tracing::warn!("pointer hook: no track {track} drawn; the pointer stays");
                    self.at
                }),
                Some(PointerAt::Offset { dx, dy }) => self.at + egui::vec2(dx, dy),
                None => self.at,
            };
        }
        let m = step.modifiers;
        let modifiers = egui::Modifiers { alt: m.alt, ctrl: m.ctrl, shift: m.shift, mac_cmd: false, command: m.ctrl };
        raw.modifiers = modifiers;
        let button = match step.button {
            PointerButton::Primary => egui::PointerButton::Primary,
            PointerButton::Secondary => egui::PointerButton::Secondary,
            PointerButton::Middle => egui::PointerButton::Middle,
        };
        match step.action {
            PointerAction::Press => self.held = Some(modifiers),
            PointerAction::Release | PointerAction::Click => self.held = None,
            _ if self.held.is_some() => self.held = Some(modifiers),
            _ => {}
        }
        let pos = self.at;
        raw.events.push(egui::Event::PointerMoved(pos));
        let press = |pressed| egui::Event::PointerButton { pos, button, pressed, modifiers };
        let frames = step.frames.unwrap_or(1).max(if matches!(step.action, PointerAction::Click | PointerAction::Key) { 2 } else { 1 });
        let key = step.key.as_deref().and_then(|name| {
            let k = egui::Key::from_name(name);
            if k.is_none() {
                tracing::warn!("pointer hook: no key {name:?}");
            }
            k
        });
        let key_event = |pressed| key.map(|key| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
        match step.action {
            PointerAction::Press if first => raw.events.push(press(true)),
            PointerAction::Release if first => raw.events.push(press(false)),
            PointerAction::Click if first => raw.events.push(press(true)),
            PointerAction::Click if self.played == 1 => raw.events.push(press(false)),
            PointerAction::Scroll if first => raw.events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(step.scroll[0], step.scroll[1]), modifiers }),
            PointerAction::KeyPress | PointerAction::Key if first => raw.events.extend(key_event(true)),
            PointerAction::KeyRelease if first => raw.events.extend(key_event(false)),
            PointerAction::Key if self.played == 1 => raw.events.extend(key_event(false)),
            PointerAction::Text if first => raw.events.extend(step.text.clone().map(egui::Event::Text)),
            _ => {}
        }
        self.played += 1;
        if self.played >= frames {
            self.steps.pop_front();
            self.played = 0;
        }
        ctx.request_repaint();
    }
}

/// Where `secs` on timeline track `track` was drawn last frame: at the
/// middle of the track's height.
pub(crate) fn timeline_point(timeline: &crate::panels::timeline::TimelinePanel, state: &crate::state::AppState, secs: f64, track: usize) -> Option<egui::Pos2> {
    let (_, _, r) = timeline.frame_cache.track_rects.iter().find(|(i, _, _)| *i == track)?;
    Some(egui::pos2(r.left() + secs as f32 * state.timeline_zoom - state.timeline_scroll_x, r.center().y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(p: &mut PointerPlayer, ctx: &egui::Context) -> egui::RawInput {
        let mut raw = egui::RawInput::default();
        p.play(ctx, &mut raw, |secs, track| Some(egui::pos2(100.0 + secs as f32 * 10.0, 50.0 + track as f32 * 40.0)));
        raw
    }

    /// Steps play one a frame, each where it says: a press on a timeline
    /// point, a move by an offset held two frames with Shift, a release
    /// where the pointer is, a click as a press and a release a frame
    /// apart, a scroll; then nothing, the real input left alone.
    #[test]
    fn steps_play_one_a_frame() {
        let steps: Vec<PointerStep> = serde_json::from_value(serde_json::json!([
            {"action": "press", "at": {"timeline": {"secs": 2.0, "track": 1}}},
            {"at": {"offset": {"dx": 30.0, "dy": 0.0}}, "modifiers": {"shift": true}, "frames": 2},
            {"action": "release"},
            {"action": "click", "at": {"screen": {"x": 5.0, "y": 6.0}}},
            {"action": "scroll", "scroll": [0.0, -40.0]}
        ]))
        .unwrap();
        let ctx = egui::Context::default();
        let mut p = PointerPlayer::default();
        p.queue(&steps);
        let start = egui::pos2(120.0, 90.0);
        let raws: Vec<egui::RawInput> = (0..8).map(|_| frame(&mut p, &ctx)).collect();
        let pressed = |r: &egui::RawInput| r.events.iter().find_map(|e| match e { egui::Event::PointerButton { pos, pressed, .. } => Some((*pos, *pressed)), _ => None });
        assert_eq!(pressed(&raws[0]), Some((start, true)));
        for r in &raws[1..3] {
            assert_eq!(r.events, [egui::Event::PointerMoved(start + egui::vec2(30.0, 0.0))]);
            assert!(r.modifiers.shift);
        }
        assert_eq!(pressed(&raws[3]), Some((start + egui::vec2(30.0, 0.0), false)));
        assert!(!raws[3].modifiers.shift);
        assert_eq!(pressed(&raws[4]), Some((egui::pos2(5.0, 6.0), true)));
        assert_eq!(pressed(&raws[5]), Some((egui::pos2(5.0, 6.0), false)));
        assert!(raws[6].events.iter().any(|e| matches!(e, egui::Event::MouseWheel { delta, .. } if *delta == egui::vec2(0.0, -40.0))));
        assert!(raws[7].events.is_empty(), "nothing left: {:?}", raws[7].events);
    }

    /// A key is pressed and let go a frame apart with its modifiers held,
    /// and text is typed as one event; an unknown key name sends nothing.
    #[test]
    fn keys_and_text_play_as_a_keyboard_does() {
        let steps: Vec<PointerStep> = serde_json::from_value(serde_json::json!([
            {"action": "key", "key": "Delete", "modifiers": {"shift": true}},
            {"action": "text", "text": "Hi"},
            {"action": "key_press", "key": "Nope"}
        ]))
        .unwrap();
        let ctx = egui::Context::default();
        let mut p = PointerPlayer::default();
        p.queue(&steps);
        let raws: Vec<egui::RawInput> = (0..4).map(|_| frame(&mut p, &ctx)).collect();
        let key = |r: &egui::RawInput| r.events.iter().find_map(|e| match e { egui::Event::Key { key, pressed, modifiers, .. } => Some((*key, *pressed, modifiers.shift)), _ => None });
        assert_eq!(key(&raws[0]), Some((egui::Key::Delete, true, true)));
        assert_eq!(key(&raws[1]), Some((egui::Key::Delete, false, true)));
        assert!(raws[2].events.contains(&egui::Event::Text("Hi".into())));
        assert_eq!(key(&raws[3]), None, "no such key");
    }

    /// A button pressed with Shift and not yet let go keeps Shift held in
    /// the frames after the steps run out; let go, the real keyboard's
    /// keys come back.
    #[test]
    fn keys_stay_held_with_a_held_button() {
        let ctx = egui::Context::default();
        let mut p = PointerPlayer::default();
        let steps = |v| serde_json::from_value::<Vec<PointerStep>>(v).unwrap();
        p.queue(&steps(serde_json::json!([{"action": "press", "at": {"screen": {"x": 1.0, "y": 1.0}}, "modifiers": {"shift": true}}])));
        let _ = frame(&mut p, &ctx);
        assert!(frame(&mut p, &ctx).modifiers.shift, "held between requests");
        p.queue(&steps(serde_json::json!([{"action": "release"}])));
        let _ = frame(&mut p, &ctx);
        assert!(!frame(&mut p, &ctx).modifiers.shift, "let go");
    }
}
