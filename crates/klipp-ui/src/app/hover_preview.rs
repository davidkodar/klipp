//! Hover preview (#3210): an entry hovered in a browser shown on the
//! viewer, on the selected clip (or the one under the playhead), without
//! being applied. Its click's commands are run, by the producers they are
//! applied through, on a copy of the sequence the engine draws, which the
//! engine shows in place of the document's (`SharedSequence::set_preview`)
//! until the pointer moves off, the document changes or playback stops.
//! Nothing of it reaches the document, its history or the render cache.

use super::*;
use klipp_graph_lift::ops::GraphOp;
use std::time::{Duration, Instant};

/// How long an entry is hovered before its preview goes up: a pointer
/// passing over a row of cards puts none up.
pub(crate) const DWELL: Duration = Duration::from_millis(80);

/// An entry a browser wants shown while it is hovered: its key (the entry
/// and the clip it goes on) and the commands a click on it would send.
#[derive(Debug, Clone)]
pub(crate) struct PreviewWant {
    pub key: String,
    pub cmds: Vec<AppCommand>,
}

/// The entry hovered: since when, whether its preview went up, and
/// whether it was taken down for good while still hovered (a playback
/// stop), not to go up again until the pointer comes back.
#[derive(Debug, Clone)]
pub(crate) struct HoverPreview {
    pub key: String,
    pub since: Instant,
    pub up: bool,
    pub ended: bool,
    /// Playing when it went up: a stop ends it.
    pub playing: bool,
}

/// What [`previewed`] reads beside the sequence: the media, and the
/// framing analysis a smooth cut is checked by.
pub(super) struct PreviewContext<'a> {
    pub pool: &'a klipp_media::MediaPool,
    pub analysis: &'a crate::transcript::TranscriptSource,
}

/// `seq` with `cmds` applied, as their click applies them: a clip's added
/// nodes through `graph_ops_edit`, its placed effects through
/// `apply_effect_edits`, a transition through `transition_edits`. With the
/// time to show it at: a transition's middle, where the playhead seldom
/// is; `None` for the frame parked. Or why they can't be shown so: a
/// command that is no picture edit, a speed change (it moves the clip's
/// frames, and the decoder with them), a refused edit.
pub(super) fn previewed(seq: &klipp_timeline::Sequence, cmds: &[AppCommand], ctx: &PreviewContext<'_>) -> Result<(klipp_timeline::Sequence, Option<klipp_core::Timecode>), String> {
    let mut out = seq.clone();
    let mut at = None;
    let ops: Vec<GraphOp> = cmds.iter().filter_map(path_commands::path_ops).flat_map(|(ops, _)| ops).collect();
    if !ops.is_empty() {
        let outcome = graph_ops::graph_ops_edit(&out, &ops).map_err(|(_, why)| why)?;
        let EditCommand::PictureEdit { clip_id, edit, .. } = outcome.edit else {
            return Err("it changes more than the clip's picture".into());
        };
        let clip = out.tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).ok_or("the clip isn't in the sequence")?;
        clip.restore_picture(&edit.after);
    }
    for cmd in cmds {
        match cmd {
            AppCommand::AddBuiltinEffect { clip_id, effect } => {
                if inspector_edit_host::is_ramp(effect) {
                    return Err("a speed change moves the clip's frames".into());
                }
                let effect = match effect {
                    klipp_effects::BuiltinEffect::Frei0r { type_id, params } => klipp_effects::BuiltinEffect::Frei0r { type_id: klipp_effects::frei0r_effect::canonical_type_id(type_id)?, params: params.clone() },
                    other => other.clone(),
                };
                let clip = out.tracks.iter_mut().find_map(|t| t.find_clip_mut(*clip_id)).ok_or("the clip isn't in the sequence")?;
                match clip.apply_effect_edits(&[klipp_timeline::EffectEdit::Add(&effect)]) {
                    Some(Ok(_)) => {}
                    Some(Err(why)) => return Err(why),
                    None => return Err("the clip can't take this edit through its picture".into()),
                }
            }
            AppCommand::AddTransition { track_index, clip_a_id, clip_b_id, kind_id, duration_frames } => {
                let made = transitions::transition_edits(&out, ctx.pool, ctx.analysis, *track_index, *clip_a_id, *clip_b_id, *kind_id, *duration_frames)?;
                // Its edits are of the sequence alone; the pool beside
                // them is a stand-in, never read and dropped after.
                let mut sequences = vec![out];
                let mut fps = sequences[0].fps;
                let mut scratch = klipp_media::MediaPool::default();
                let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut scratch, fps: &mut fps };
                for edit in &made.edits {
                    view.apply_forward(edit);
                }
                out = sequences.remove(0);
                let shown = out.tracks.get(*track_index).and_then(|t| t.transitions.iter().find(|x| x.clip_a_id == *clip_a_id && x.clip_b_id == *clip_b_id));
                let cut = out.tracks.get(*track_index).and_then(|t| t.find_clip(*clip_b_id)).map(|c| c.timeline_in);
                // Its middle, on a frame: the two pictures most mixed.
                at = shown.zip(cut).map(|(t, cut)| {
                    let (start, end) = t.span(cut);
                    klipp_core::RationalTime { value: (start.value + end.value) / 2, rate: start.rate }
                });
            }
            cmd if path_commands::path_ops(cmd).is_some() => {}
            _ => return Err("it isn't a change to the clip's picture".into()),
        }
    }
    Ok((out, at))
}

/// What a frame does to the preview, [`step`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Nothing,
    PutUp,
    TakeDown,
    /// The one up was skipped by the engine: say so on its entry.
    NoteSkipped,
}

/// A frame of the hover: `hovered` the key of the entry hovered now, if
/// any. The same entry hovered [`DWELL`] puts its preview up, another
/// takes the one up down and starts its own wait, none takes it down; a
/// playback stop takes it down until the pointer leaves the entry.
pub(crate) fn step(was: Option<HoverPreview>, hovered: Option<&str>, playing: bool, skipped: bool, now: Instant) -> (Option<HoverPreview>, Step) {
    match was {
        Some(mut h) if Some(h.key.as_str()) == hovered => {
            let step = if h.up && h.playing && !playing && !h.ended {
                h.ended = true;
                Step::TakeDown
            } else if !h.up && !h.ended && now.duration_since(h.since) >= DWELL {
                h.up = true;
                h.playing = playing;
                Step::PutUp
            } else if h.up && !h.ended && skipped {
                Step::NoteSkipped
            } else {
                Step::Nothing
            };
            (Some(h), step)
        }
        was => {
            let step = if was.is_some_and(|h| h.up && !h.ended) { Step::TakeDown } else { Step::Nothing };
            (hovered.map(|key| HoverPreview { key: key.to_owned(), since: now, up: false, ended: false, playing }), step)
        }
    }
}

impl KlippApp {
    /// Once a frame, after the browsers have drawn and the frame's
    /// commands are sent: [`step`], done.
    pub(super) fn update_hover_preview(&mut self) {
        let wanted = self.state.hover_preview_wanted.take();
        let playing = self.state.playback == PlaybackState::Playing;
        let skipped = self.frame_buffer.lock().is_ok_and(|fb| fb.preview_skipped);
        let (now, step) = step(self.state.hover_preview.take(), wanted.as_ref().map(|w| w.key.as_str()), playing, skipped, Instant::now());
        match step {
            Step::PutUp => self.put_preview_up(wanted.as_ref().expect("an entry hovered")),
            Step::TakeDown => self.take_preview_down(),
            Step::NoteSkipped => self.state.hover_preview_note = now.as_ref().map(|h| (h.key.clone(), "the GPU can't draw it here".into())),
            Step::Nothing => {}
        }
        self.state.hover_preview = now;
    }

    /// `want` on the viewer: its sequence made from the one the engine
    /// draws and offered for the revision it was made from.
    fn put_preview_up(&mut self, want: &PreviewWant) {
        let (shown, base) = self.sequence.snapshot_with_revision();
        let ctx = PreviewContext { pool: &self.state.media_pool, analysis: &self.transcript_source };
        match previewed(&shown, &want.cmds, &ctx) {
            Ok((seq, at)) => {
                self.state.hover_preview_note = None;
                if self.sequence.set_preview(base, Arc::new(seq)) {
                    self.send_commands(vec![AppCommand::RefreshPreview { at }]);
                }
            }
            Err(why) => self.state.hover_preview_note = Some((want.key.clone(), why)),
        }
    }

    fn take_preview_down(&mut self) {
        if self.sequence.clear_preview() {
            self.send_commands(vec![AppCommand::RefreshPreview { at: None }]);
        }
    }
}

/// An effect, look or frei0r card hovered in the Effects browser: what a
/// click on it would put on the clip a preview goes on (the selected one,
/// else the top picture under the playhead), wanted on the viewer, or the
/// reason noted when the click would be refused. Its key; `None` for what
/// has no picture to show (a sound effect, an OFX plug-in) or no clip.
pub(crate) fn want_effect(state: &mut AppState, name: &str, source: &crate::browser::effect_registry::EffectSource) -> Option<String> {
    use crate::browser::effect_registry::EffectSource;
    if !matches!(source, EffectSource::Builtin(_) | EffectSource::Look(_) | EffectSource::Frei0r(_)) {
        return None;
    }
    let clip = state.selected_clip.or_else(|| state.picture_under_playhead())?;
    let key = format!("effect:{name}:{}", clip.0);
    match crate::browser::effects_tab::effect_commands_in(state, source, clip) {
        Ok(cmds) => state.hover_preview_wanted = Some(PreviewWant { key: key.clone(), cmds }),
        Err(why) => state.hover_preview_note = Some((key.clone(), why)),
    }
    Some(key)
}

/// A transition card hovered in the Transitions browser: the transition
/// on the cut out of the clip a preview goes on, into the next clip on its
/// track, at the default length, as a double-click puts it there, wanted
/// on the viewer at its middle. Its key; `None` with no clip, the reason
/// noted when there is no cut after it.
pub(crate) fn want_transition(state: &mut AppState, spec: &klipp_core::TransitionSpec) -> Option<String> {
    let clip = state.selected_clip.or_else(|| state.picture_under_playhead())?;
    let key = format!("transition:{}:{}", spec.string_key, clip.0);
    let next = state.sequence().tracks.iter().enumerate().find_map(|(ti, t)| {
        let at = t.clips.iter().position(|c| c.id == clip)?;
        Some((ti, t.clips.get(at + 1).map(|c| c.id)))
    });
    match next {
        Some((track_index, Some(clip_b_id))) => {
            let cmds = vec![AppCommand::AddTransition { track_index, clip_a_id: clip, clip_b_id, kind_id: spec.kind_id, duration_frames: state.default_transition_frames }];
            state.hover_preview_wanted = Some(PreviewWant { key: key.clone(), cmds });
        }
        _ => state.hover_preview_note = Some((key.clone(), "the clip has no cut after it on its track".into())),
    }
    Some(key)
}

/// The tooltip line for the entry by `key` when it can't be previewed.
pub(crate) fn note_for(state: &AppState, key: &str) -> Option<String> {
    state.hover_preview_note.as_ref().filter(|(k, _)| k == key).map(|(_, why)| format!("Can't preview this one: {why}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_effects::BuiltinEffect;

    /// [`previewed`] with `state`'s media and no analysis: the sequence
    /// alone, and the time to show it at.
    fn shown(state: &AppState, cmds: &[AppCommand]) -> Result<(klipp_timeline::Sequence, Option<klipp_core::Timecode>), String> {
        let analysis = crate::transcript::TranscriptSource::default();
        previewed(state.sequence(), cmds, &PreviewContext { pool: &state.media_pool, analysis: &analysis })
    }

    fn timeline(clips: usize) -> (AppState, klipp_core::ClipId) {
        let mut state = AppState::default();
        let seq = &mut state.sequences[state.active_sequence_idx];
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        let tc = |f: i64| klipp_core::Timecode::from_frames(f, 24);
        track.clips = (0..clips as i64).map(|i| klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..klipp_timeline::Clip::gap(tc(i * 24), tc(i * 24 + 24)) }).collect();
        let first = track.clips[0].id;
        seq.tracks = vec![track];
        state.selected_clip = Some(first);
        (state, first)
    }

    /// #3210: the preview of an effect is the clip as applying it leaves
    /// it, and the sequence it was made from is as it was.
    #[test]
    fn a_previewed_effect_is_the_clip_as_applied() {
        for effect in [BuiltinEffect::GaussianBlur { radius: 12.0 }, BuiltinEffect::Opacity { value: 0.5 }] {
            let (state, clip) = timeline(3);
            let source = crate::browser::effect_registry::EffectSource::Builtin(effect.clone());
            let cmds = crate::browser::effects_tab::effect_commands_in(&state, &source, clip).expect("the commands");
            let before = serde_json::to_string(state.sequence()).unwrap();
            let (shown, _) = shown(&state, &cmds).expect("a preview");
            assert_eq!(serde_json::to_string(state.sequence()).unwrap(), before, "the sequence previewed from is unchanged");
            // Applied as the click applies them: path edits through the
            // state, a placed effect through the clip's effect list.
            let mut applying = AppState { sequences: state.sequences.clone(), active_sequence_idx: state.active_sequence_idx, ..Default::default() };
            for cmd in &cmds {
                match cmd {
                    AppCommand::AddBuiltinEffect { clip_id, effect } => {
                        let seq = &mut applying.sequences[applying.active_sequence_idx];
                        let c = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(*clip_id)).unwrap();
                        assert!(matches!(c.apply_effect_edits(&[klipp_timeline::EffectEdit::Add(effect)]), Some(Ok(_))));
                    }
                    cmd => {
                        let (ops, label) = path_commands::path_ops(cmd).expect("a path edit");
                        applying.apply_path_ops(&ops, label).expect("applied");
                    }
                }
            }
            let clip_json = |s: &klipp_timeline::Sequence| serde_json::to_value(&s.tracks[0].clips[0]).unwrap();
            assert_eq!(clip_json(&shown), clip_json(applying.sequence()), "{effect:?}: the previewed clip is the applied one");
        }
    }

    /// The hover's frames: an entry goes up only after its dwell, so a
    /// pointer run fast over fifty entries puts none up; moving to another
    /// takes the one up down; leaving takes it down; a playback stop takes
    /// it down for good while the entry stays hovered.
    #[test]
    fn a_preview_goes_up_after_its_dwell_and_down_when_left() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut h = None;
        let mut ups = 0;
        for i in 0..50u64 {
            let key = format!("effect:{i}");
            let (next, s) = step(h.take(), Some(&key), false, false, at(i));
            ups += usize::from(s == Step::PutUp);
            h = next;
        }
        assert_eq!(ups, 0, "fifty entries passed over in 50 ms put none up");
        let (h1, s) = step(h, Some("effect:a"), false, false, at(100));
        assert_eq!(s, Step::Nothing);
        let (h1, s) = step(h1, Some("effect:a"), false, false, at(100 + DWELL.as_millis() as u64));
        assert_eq!(s, Step::PutUp, "up after its dwell");
        let (h2, s) = step(h1.clone(), Some("effect:b"), false, false, at(300));
        assert_eq!(s, Step::TakeDown, "another entry takes it down");
        assert!(h2.as_ref().is_some_and(|h| h.key == "effect:b" && !h.up));
        let (none, s) = step(h1.clone(), None, false, false, at(300));
        assert_eq!((none.is_none(), s), (true, Step::TakeDown), "leaving takes it down");
        let (_, s) = step(h1.clone(), Some("effect:a"), false, true, at(300));
        assert_eq!(s, Step::NoteSkipped);
        // Up while playing, then stopped: down, and not up again.
        let (h3, _) = step(None, Some("effect:c"), true, false, at(0));
        let (h3, s) = step(h3, Some("effect:c"), true, false, at(200));
        assert_eq!(s, Step::PutUp);
        let (h3, s) = step(h3, Some("effect:c"), false, false, at(300));
        assert_eq!(s, Step::TakeDown, "a playback stop takes it down");
        let (_, s) = step(h3, Some("effect:c"), false, false, at(900));
        assert_eq!(s, Step::Nothing, "and it stays down while still hovered");
    }

    /// #3210 PR 2: a transition previewed is the one the Transitions
    /// browser puts on the cut, made by the same edits, and shown at its
    /// middle; the sequence previewed from is unchanged. A clip with no
    /// cut after it has nothing to preview.
    #[test]
    fn a_previewed_transition_is_the_one_applied_and_shown_at_its_middle() {
        let mut state = AppState::default();
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        // Two shots cut together, each from the middle of a ten-second
        // file: handles either side for the overlap.
        let mut shot = |from: f64| {
            let id = klipp_core::MediaId::new();
            let mut m = klipp_media::MediaItem::synthetic_for_test(std::path::PathBuf::from(format!("/media/{}.mp4", id.0)), None);
            m.id = id;
            m.duration = tc(10.0);
            state.media_pool.add(m);
            klipp_timeline::Clip::media(id, tc(from), tc(from + 2.0), tc(4.0), tc(6.0))
        };
        let (a, b) = (shot(0.0), shot(2.0));
        let (a_id, b_id) = (a.id, b.id);
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        track.clips = vec![a, b];
        state.sequences[state.active_sequence_idx].tracks = vec![track];
        state.selected_clip = Some(a_id);
        let spec = klipp_core::spec_for_kind(klipp_core::TransitionKind::CrossDissolve);
        let key = want_transition(&mut state, spec).expect("a clip to preview on");
        let want = state.hover_preview_wanted.take().expect("a preview wanted");
        assert_eq!(want.key, key);
        let before = serde_json::to_string(state.sequence()).unwrap();
        let (seq, at) = shown(&state, &want.cmds).expect("a preview");
        assert_eq!(serde_json::to_string(state.sequence()).unwrap(), before, "the sequence previewed from is unchanged");
        // The edits the browser's click applies, applied.
        let analysis = crate::transcript::TranscriptSource::default();
        let made = transitions::transition_edits(state.sequence(), &state.media_pool, &analysis, 0, a_id, b_id, spec.kind_id, state.default_transition_frames).expect("a transition");
        let mut sequences = vec![state.sequence().clone()];
        let (mut pool, mut fps) = (state.media_pool.clone(), sequences[0].fps);
        let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        for edit in &made.edits {
            view.apply_forward(edit);
        }
        let shape = |s: &klipp_timeline::Sequence| serde_json::to_value((&s.tracks[0].clips, s.tracks[0].transitions.iter().map(|t| (t.kind, t.duration, t.alignment, t.clip_a_id, t.clip_b_id)).collect::<Vec<_>>())).unwrap();
        assert_eq!(shape(&seq), shape(&sequences[0]), "the previewed cut is the one applied");
        let t = seq.tracks[0].transitions.first().expect("a transition on the cut");
        let (start, end) = t.span(seq.tracks[0].clips[1].timeline_in);
        let at = at.expect("a time to show it at");
        assert!(start < at && at < end && (at.value - (start.value + end.value) / 2).abs() <= 1, "shown at {at:?}, its span {start:?}..{end:?}");
        // The last clip: no cut after it.
        state.selected_clip = Some(b_id);
        let key = want_transition(&mut state, spec).expect("a clip");
        assert!(state.hover_preview_wanted.is_none() && note_for(&state, &key).is_some_and(|n| n.contains("no cut after it")));
    }

    #[test]
    fn a_speed_change_is_not_previewed() {
        let (state, clip) = timeline(1);
        let cmds = vec![AppCommand::AddBuiltinEffect { clip_id: clip, effect: BuiltinEffect::SpeedRamp { speed: 0.5, blend_mode: 2 } }];
        assert!(shown(&state, &cmds).unwrap_err().contains("speed"));
        assert!(shown(&state, &[AppCommand::Play]).is_err(), "nor anything but a picture edit");
    }

    /// The copy a preview is made on, measured on a long sequence: under
    /// half a frame at 60 fps (2.2 ms alone on the reference machine).
    #[test]
    fn a_preview_of_a_long_sequence_is_made_inside_a_frame() {
        let (state, clip) = timeline(2000);
        let source = crate::browser::effect_registry::EffectSource::Builtin(BuiltinEffect::GaussianBlur { radius: 12.0 });
        let cmds = crate::browser::effects_tab::effect_commands_in(&state, &source, clip).expect("the commands");
        let _ = shown(&state, &cmds).expect("a preview");
        // The fastest of twenty: the suite runs beside it, and a mean
        // measures the machine's load as much as the copy.
        let fastest = (0..20)
            .map(|_| {
                let t0 = Instant::now();
                std::hint::black_box(shown(&state, &cmds).expect("a preview"));
                t0.elapsed().as_secs_f64() * 1000.0
            })
            .fold(f64::INFINITY, f64::min);
        eprintln!("a preview of 2000 clips made in {fastest:.2} ms at the fastest");
        assert!(fastest < 8.0, "a preview of 2000 clips took {fastest:.2} ms at the fastest: half a 60 fps frame is 8.3");
    }
}
