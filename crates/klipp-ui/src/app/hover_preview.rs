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
/// and the clip it goes on) and the commands a click on it would send, or
/// the clip it would place; the clip's time a scrub runs over, where the
/// entry knows it; and where the pointer is across the card, 0 to 1.
#[derive(Debug, Clone, Default)]
pub(crate) struct PreviewWant {
    pub key: String,
    pub cmds: Vec<AppCommand>,
    pub place: Option<Place>,
    pub span: Option<(klipp_core::Timecode, klipp_core::Timecode)>,
    pub scrub: Option<f32>,
}

/// A clip a card places rather than an edit of one: a title or a
/// generator, put where a double-click on its card puts it.
#[derive(Debug, Clone)]
pub(crate) enum Place {
    Title(Box<klipp_timeline::TitleData>),
    Generator(klipp_core::GeneratorData),
}

/// Where the pointer is across a hovered card, 0 at its left edge to 1 at
/// its right: what a scrub over the card follows.
pub(crate) fn across(resp: &egui::Response) -> Option<f32> {
    let p = resp.hover_pos()?;
    Some(((p.x - resp.rect.min.x) / resp.rect.width().max(1.0)).clamp(0.0, 1.0))
}

/// How far across a card the pointer moves before it scrubs: a pointer
/// come to rest on it shows the frame the preview opens on.
const SCRUB_START: f32 = 0.03;

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
    /// The time a scrub over the card runs over, once it is up.
    pub span: Option<(klipp_core::Timecode, klipp_core::Timecode)>,
    /// Where the pointer came onto the card, and the scrub last shown.
    pub entry: Option<f32>,
    pub scrubbed: Option<f32>,
}

/// A [`previewed`] sequence: it, the time to show it at (`None` the frame
/// parked), and the time a scrub over its card runs over.
pub(super) struct Shown {
    pub seq: klipp_timeline::Sequence,
    pub at: Option<klipp_core::Timecode>,
    pub span: Option<(klipp_core::Timecode, klipp_core::Timecode)>,
}

/// What [`previewed`] reads beside the sequence: the media, and the
/// framing analysis a smooth cut is checked by.
pub(super) struct PreviewContext<'a> {
    pub pool: &'a klipp_media::MediaPool,
    pub analysis: &'a crate::transcript::TranscriptSource,
    /// Where a placed title or generator goes, as its click places it.
    pub playhead: klipp_core::Timecode,
}

/// `seq` with `want` applied, as its click applies it: a clip's added
/// nodes through `graph_ops_edit`, its placed effects through
/// `apply_effect_edits`, a transition through `transition_edits`, a title
/// or generator placed through `place_title` and `add_title_clip` or
/// `add_generator_clip` on a scratch state holding the copy. With the time
/// to show it at: a transition's middle, where the playhead seldom is, a
/// placed clip where its click parks the playhead; `None` for the frame
/// parked. Or why it can't be shown so: a command that is no picture
/// edit, a speed change (it moves the clip's frames, and the decoder with
/// them), a refused edit.
pub(super) fn previewed(seq: &klipp_timeline::Sequence, want: &PreviewWant, ctx: &PreviewContext<'_>) -> Result<Shown, String> {
    let cmds = &want.cmds;
    let mut out = seq.clone();
    let mut at = None;
    let mut span = want.span;
    if let Some(place) = &want.place {
        let mut scratch = AppState { sequences: vec![out], active_sequence_idx: 0, playhead: ctx.playhead, ..Default::default() };
        let (clip, park) = match place {
            Place::Generator(data) => {
                let (clip, _, park) = timeline_edit::add_generator_clip(&mut scratch, None, data);
                (clip, park)
            }
            Place::Title(data) => {
                let AppCommand::CreateTitleClip { track_index, timeline_in, duration_frames } = crate::browser::titles_tab::place_title(&mut scratch, (**data).clone()) else {
                    return Err("a title is placed by its own command".into());
                };
                let (clip, rest) = titles::add_title_clip(&mut scratch, track_index, timeline_in, duration_frames);
                (clip, rest.unwrap_or(ctx.playhead))
            }
        };
        out = scratch.sequences.remove(0);
        span = out.all_clips().find(|c| c.id == clip).map(|c| (c.timeline_in, c.timeline_out));
        at = Some(park);
    }
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
                span = shown.zip(cut).map(|(t, cut)| t.span(cut));
                at = span.map(|(start, end)| klipp_core::RationalTime { value: (start.value + end.value) / 2, rate: start.rate });
            }
            // A LUT preset as the clip's look (#3255): the look op's edit,
            // as its click applies it.
            AppCommand::ApplyClipLut { clip_id, lut_path, name } => {
                let op = crate::browser::effect_registry::lut_look_op(*clip_id, lut_path, name);
                let plan = klipp_document::ops::resolve(&out, ctx.pool, &[op]).map_err(|r| r.why)?;
                let mut sequences = vec![out];
                let mut fps = sequences[0].fps;
                let mut scratch = klipp_media::MediaPool::default();
                let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut scratch, fps: &mut fps };
                for step in &plan.steps {
                    view.apply_forward(&step.edit);
                }
                out = sequences.remove(0);
            }
            cmd if path_commands::path_ops(cmd).is_some() => {}
            _ => return Err("it isn't a change to the clip's picture".into()),
        }
    }
    Ok(Shown { seq: out, at, span })
}

/// What a frame does to the preview, [`step`] says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Step {
    Nothing,
    PutUp,
    TakeDown,
    /// The one up was skipped by the engine: say so on its entry.
    NoteSkipped,
    /// Show the time this far across the card's span.
    Scrub(f32),
}

/// A frame of the hover: `hovered` the key of the entry hovered now and
/// where the pointer is across it, if any. The same entry hovered
/// [`DWELL`] puts its preview up, another takes the one up down and starts
/// its own wait, none takes it down; a playback stop takes it down until
/// the pointer leaves the entry. Up and parked, the pointer moved across
/// the card from where it came on scrubs.
pub(crate) fn step(was: Option<HoverPreview>, hovered: Option<(&str, Option<f32>)>, playing: bool, skipped: bool, now: Instant) -> (Option<HoverPreview>, Step) {
    match was {
        Some(mut h) if hovered.is_some_and(|(key, _)| key == h.key) => {
            let across = hovered.and_then(|(_, x)| x);
            let moved = |x: f32| h.scrubbed.is_some() || h.entry.is_some_and(|e| (x - e).abs() > SCRUB_START);
            let step = if h.up && h.playing && !playing && !h.ended {
                h.ended = true;
                Step::TakeDown
            } else if !h.up && !h.ended && now.duration_since(h.since) >= DWELL {
                h.up = true;
                h.playing = playing;
                Step::PutUp
            } else if let Some(x) = across.filter(|&x| h.up && !h.ended && !playing && moved(x) && h.scrubbed != Some(x)) {
                h.scrubbed = Some(x);
                Step::Scrub(x)
            } else if h.up && !h.ended && skipped {
                Step::NoteSkipped
            } else {
                Step::Nothing
            };
            (Some(h), step)
        }
        was => {
            let step = if was.is_some_and(|h| h.up && !h.ended) { Step::TakeDown } else { Step::Nothing };
            let next = hovered.map(|(key, x)| HoverPreview { key: key.to_owned(), since: now, up: false, ended: false, playing, span: None, entry: x, scrubbed: None });
            (next, step)
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
        let (mut now, step) = step(self.state.hover_preview.take(), wanted.as_ref().map(|w| (w.key.as_str(), w.scrub)), playing, skipped, Instant::now());
        match step {
            Step::PutUp => {
                let span = self.put_preview_up(wanted.as_ref().expect("an entry hovered"));
                if let Some(h) = now.as_mut() {
                    h.span = span;
                }
            }
            Step::TakeDown => self.take_preview_down(),
            Step::NoteSkipped => self.state.hover_preview_note = now.as_ref().map(|h| (h.key.clone(), "the GPU can't draw it here".into())),
            Step::Scrub(x) => {
                if let Some((start, end)) = now.as_ref().and_then(|h| h.span) {
                    let at = klipp_core::RationalTime { value: start.value + ((end.value - start.value) as f64 * f64::from(x)).round() as i64, rate: start.rate };
                    self.send_commands(vec![AppCommand::RefreshPreview { at: Some(at.min(end - klipp_core::RationalTime { value: 1, rate: end.rate })) }]);
                }
            }
            Step::Nothing => {}
        }
        self.state.hover_preview = now;
    }

    /// `want` on the viewer: its sequence made from the one the engine
    /// draws and offered for the revision it was made from. The time a
    /// scrub over its card runs over.
    fn put_preview_up(&mut self, want: &PreviewWant) -> Option<(klipp_core::Timecode, klipp_core::Timecode)> {
        let (shown, base) = self.sequence.snapshot_with_revision();
        let ctx = PreviewContext { pool: &self.state.media_pool, analysis: &self.transcript_source, playhead: self.state.playhead };
        match previewed(&shown, want, &ctx) {
            Ok(Shown { seq, at, span }) => {
                self.state.hover_preview_note = None;
                if self.sequence.set_preview(base, Arc::new(seq)) {
                    self.send_commands(vec![AppCommand::RefreshPreview { at }]);
                }
                span
            }
            Err(why) => {
                self.state.hover_preview_note = Some((want.key.clone(), why));
                None
            }
        }
    }

    fn take_preview_down(&mut self) {
        if self.sequence.clear_preview() {
            self.send_commands(vec![AppCommand::RefreshPreview { at: None }]);
        }
    }
}

impl KlippApp {
    /// `ShowPreview` and `ClearPreview` (#3210): an entry named over the
    /// control socket shown as its card's hover shows it, at the playhead
    /// and at once, for `frame_at`; refused with why in
    /// `preview_command_refused`, which the control bridge answers with.
    pub(super) fn handle_preview_commands(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::ShowPreview { entry, clip_id } => {
                self.state.preview_command_refused = self.show_named_preview(entry, *clip_id).err();
                true
            }
            AppCommand::ClearPreview => {
                self.take_preview_down();
                self.state.hover_preview = None;
                true
            }
            _ => false,
        }
    }

    fn show_named_preview(&mut self, entry: &klipp_core::PreviewEntry, clip_id: Option<klipp_core::ClipId>) -> Result<(), String> {
        let want = want_named(&mut self.state, entry, clip_id)?;
        // At the playhead, where frame_at has put it: the time asked for,
        // not the entry's own (a transition's middle).
        let (shown, base) = self.sequence.snapshot_with_revision();
        let ctx = PreviewContext { pool: &self.state.media_pool, analysis: &self.transcript_source, playhead: self.state.playhead };
        let Shown { seq, .. } = previewed(&shown, &want, &ctx)?;
        if !self.sequence.set_preview(base, Arc::new(seq)) {
            return Err("the timeline changed while it was made: ask again".into());
        }
        self.send_commands(vec![AppCommand::RefreshPreview { at: None }]);
        Ok(())
    }
}

/// The [`PreviewWant`] of `entry` named as its browser names it (any
/// case), on `clip_id` when given: as its card's hover would want it. Or
/// why there is none: no such entry, no clip to show it on, or the reason
/// its card would give.
pub(crate) fn want_named(state: &mut AppState, entry: &klipp_core::PreviewEntry, clip_id: Option<klipp_core::ClipId>) -> Result<PreviewWant, String> {
    use klipp_core::PreviewEntry as E;
    state.hover_preview_note = None;
    state.hover_preview_wanted = None;
    let no_clip = "no clip to show it on: select one, or give clip_id";
    let key = match entry {
        E::Effect(name) => {
            let source = state.effect_registry.effects.iter().find(|e| e.name.eq_ignore_ascii_case(name)).map(|e| e.source.clone()).ok_or_else(|| format!("no effect or look named {name:?} in the Effects browser"))?;
            want_effect_on(state, name, &source, clip_id, None).ok_or(no_clip)?
        }
        E::Transition(name) => {
            let spec = klipp_core::TRANSITION_SPECS.iter().find(|s| s.display_name.eq_ignore_ascii_case(name) || s.string_key.eq_ignore_ascii_case(name)).ok_or_else(|| format!("no transition named {name:?}"))?;
            want_transition_on(state, spec, clip_id, None).ok_or(no_clip)?
        }
        E::Title(name) => {
            let data = match klipp_timeline::title_library::TITLES.iter().find(|t| t.name.eq_ignore_ascii_case(name)) {
                Some(t) => crate::browser::titles_tab::Entry::Builtin(t).data(),
                None => {
                    let user = state.title_templates.templates().find(|t| t.name.eq_ignore_ascii_case(name)).ok_or_else(|| format!("no title template named {name:?}"))?;
                    crate::browser::titles_tab::Entry::User(user).data()
                }
            };
            want_placed(state, &format!("title:{name}"), Place::Title(Box::new(data)), None)
        }
        E::Generator(name) => {
            let tmpl = crate::browser::generators_tab::generators().iter().find(|g| g.name.as_str().eq_ignore_ascii_case(name)).ok_or_else(|| format!("no generator named {name:?}"))?;
            want_placed(state, &format!("generator:{name}"), Place::Generator(tmpl.data.clone()), None)
        }
    };
    state.hover_preview_wanted.take().ok_or_else(|| note_for(state, &key).map_or_else(|| "it can't be previewed".into(), |n| n.trim_start_matches("Can't preview this one: ").to_owned()))
}

/// An effect, look or frei0r card hovered in the Effects browser: what a
/// click on it would put on the clip a preview goes on (the selected one,
/// else the top picture under the playhead), wanted on the viewer, or the
/// reason noted when the click would be refused. Its key; `None` for what
/// has no picture to show (a sound effect, an OFX plug-in) or no clip.
pub(crate) fn want_effect(state: &mut AppState, name: &str, source: &crate::browser::effect_registry::EffectSource, across: Option<f32>) -> Option<String> {
    want_effect_on(state, name, source, None, across)
}

/// [`want_effect`] on `clip` when given.
pub(crate) fn want_effect_on(state: &mut AppState, name: &str, source: &crate::browser::effect_registry::EffectSource, clip: Option<klipp_core::ClipId>, across: Option<f32>) -> Option<String> {
    use crate::browser::effect_registry::EffectSource;
    if !matches!(source, EffectSource::Builtin(_) | EffectSource::Look(_) | EffectSource::Frei0r(_) | EffectSource::LutLook(_)) {
        return None;
    }
    let clip = clip.or(state.selected_clip).or_else(|| state.picture_under_playhead())?;
    let key = format!("effect:{name}:{}", clip.0);
    match crate::browser::effects_tab::effect_commands_in(state, source, clip) {
        Ok(cmds) => {
            let span = state.sequence().all_clips().find(|c| c.id == clip).map(|c| (c.timeline_in, c.timeline_out));
            state.hover_preview_wanted = Some(PreviewWant { key: key.clone(), cmds, span, scrub: across, ..Default::default() });
        }
        Err(why) => state.hover_preview_note = Some((key.clone(), why)),
    }
    Some(key)
}

/// A transition card hovered in the Transitions browser: the transition
/// on the cut out of the clip a preview goes on, into the next clip on its
/// track, at the default length, as a double-click puts it there, wanted
/// on the viewer at its middle. Its key; `None` with no clip, the reason
/// noted when there is no cut after it.
pub(crate) fn want_transition(state: &mut AppState, spec: &klipp_core::TransitionSpec, across: Option<f32>) -> Option<String> {
    want_transition_on(state, spec, None, across)
}

/// [`want_transition`] on the cut out of `clip` when given.
pub(crate) fn want_transition_on(state: &mut AppState, spec: &klipp_core::TransitionSpec, clip: Option<klipp_core::ClipId>, across: Option<f32>) -> Option<String> {
    let clip = clip.or(state.selected_clip).or_else(|| state.picture_under_playhead())?;
    let key = format!("transition:{}:{}", spec.string_key, clip.0);
    let next = state.sequence().tracks.iter().enumerate().find_map(|(ti, t)| {
        let at = t.clips.iter().position(|c| c.id == clip)?;
        Some((ti, t.clips.get(at + 1).map(|c| c.id)))
    });
    match next {
        Some((track_index, Some(clip_b_id))) => {
            let cmds = vec![AppCommand::AddTransition { track_index, clip_a_id: clip, clip_b_id, kind_id: spec.kind_id, duration_frames: state.default_transition_frames }];
            state.hover_preview_wanted = Some(PreviewWant { key: key.clone(), cmds, scrub: across, ..Default::default() });
        }
        _ => state.hover_preview_note = Some((key.clone(), "the clip has no cut after it on its track".into())),
    }
    Some(key)
}

/// A title or generator card hovered: the clip its double-click would place
/// at the playhead, wanted on the viewer where that click would show it.
/// Its key.
pub(crate) fn want_placed(state: &mut AppState, name: &str, place: Place, across: Option<f32>) -> String {
    let key = format!("place:{name}");
    state.hover_preview_wanted = Some(PreviewWant { key: key.clone(), place: Some(place), scrub: across, ..Default::default() });
    key
}

/// The tooltip line for the entry by `key` when it can't be previewed.
pub(crate) fn note_for(state: &AppState, key: &str) -> Option<String> {
    state.hover_preview_note.as_ref().filter(|(k, _)| k == key).map(|(_, why)| format!("Can't preview this one: {why}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_effects::BuiltinEffect;
    use klipp_timeline::GradeAccess;

    /// [`previewed`] with `state`'s media and no analysis: the sequence
    /// alone, and the time to show it at.
    fn shown(state: &AppState, cmds: &[AppCommand]) -> Result<(klipp_timeline::Sequence, Option<klipp_core::Timecode>), String> {
        shown_want(state, &PreviewWant { cmds: cmds.to_vec(), ..Default::default() }).map(|s| (s.seq, s.at))
    }

    fn shown_want(state: &AppState, want: &PreviewWant) -> Result<Shown, String> {
        let analysis = crate::transcript::TranscriptSource::default();
        previewed(state.sequence(), want, &PreviewContext { pool: &state.media_pool, analysis: &analysis, playhead: state.playhead })
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

    /// #3255: a LUT preset hovered is the clip with that LUT as its look,
    /// as its click's look op makes it, and the sequence previewed from is
    /// as it was.
    #[test]
    fn a_previewed_lut_preset_is_the_clips_look_as_applied() {
        let Some(base) = klipp_core::assets::bundled_luts_dir() else {
            eprintln!("skipping: no bundled LUTs");
            return;
        };
        let path = crate::browser::effect_registry::lut_files(&base.join("film-bw")).into_iter().next().expect("a black-and-white preset");
        let (state, clip) = timeline(2);
        let source = crate::browser::effect_registry::EffectSource::LutLook(path.clone());
        let cmds = crate::browser::effects_tab::effect_commands_in(&state, &source, clip).expect("the commands");
        assert!(matches!(&cmds[..], [AppCommand::ApplyClipLut { clip_id, lut_path, .. }] if *clip_id == clip && *lut_path == path), "{cmds:?}");
        let before = serde_json::to_string(state.sequence()).unwrap();
        let (shown, _) = shown(&state, &cmds).expect("a preview");
        assert_eq!(serde_json::to_string(state.sequence()).unwrap(), before, "the sequence previewed from is unchanged");
        let lut_of = |s: &klipp_timeline::Sequence| s.all_clips().find(|c| c.id == clip).and_then(|c| c.grade().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Lut3D).and_then(|n| n.params.lut_path.clone())).map(|p| klipp_core::assets::resolve_lut_path(&p.to_string_lossy(), Some(base)));
        assert_eq!(lut_of(&shown), Some(path.clone()), "the LUT in the clip's grade");
        let op = klipp_document::ops::Op::Look { clip_id: Some(klipp_document::ops::ClipRef::Id(clip)), on: None, lut_path: path.display().to_string(), name: None, opacity: None, remove: false };
        let plan = klipp_document::ops::resolve(state.sequence(), &state.media_pool, &[op]).unwrap();
        let mut seqs = vec![state.sequence().clone()];
        let (mut pool, mut fps) = (klipp_media::MediaPool::default(), seqs[0].fps);
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        for step in &plan.steps {
            view.apply_forward(&step.edit);
        }
        // Node for node, by kind and settings: the ids are made new each time.
        let grade = |s: &klipp_timeline::Sequence| s.all_clips().find(|c| c.id == clip).unwrap().grade().nodes.iter().map(|n| (n.kind, serde_json::to_value(&n.params).unwrap())).collect::<Vec<_>>();
        assert_eq!(grade(&shown), grade(&seqs[0]), "the previewed grade is the applied one");
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
            let (next, s) = step(h.take(), Some((&key, None)), false, false, at(i));
            ups += usize::from(s == Step::PutUp);
            h = next;
        }
        assert_eq!(ups, 0, "fifty entries passed over in 50 ms put none up");
        let (h1, s) = step(h, Some(("effect:a", None)), false, false, at(100));
        assert_eq!(s, Step::Nothing);
        let (h1, s) = step(h1, Some(("effect:a", None)), false, false, at(100 + DWELL.as_millis() as u64));
        assert_eq!(s, Step::PutUp, "up after its dwell");
        let (h2, s) = step(h1.clone(), Some(("effect:b", None)), false, false, at(300));
        assert_eq!(s, Step::TakeDown, "another entry takes it down");
        assert!(h2.as_ref().is_some_and(|h| h.key == "effect:b" && !h.up));
        let (none, s) = step(h1.clone(), None, false, false, at(300));
        assert_eq!((none.is_none(), s), (true, Step::TakeDown), "leaving takes it down");
        let (_, s) = step(h1.clone(), Some(("effect:a", None)), false, true, at(300));
        assert_eq!(s, Step::NoteSkipped);
        // Up while playing, then stopped: down, and not up again.
        let (h3, _) = step(None, Some(("effect:c", None)), true, false, at(0));
        let (h3, s) = step(h3, Some(("effect:c", None)), true, false, at(200));
        assert_eq!(s, Step::PutUp);
        let (h3, s) = step(h3, Some(("effect:c", None)), false, false, at(300));
        assert_eq!(s, Step::TakeDown, "a playback stop takes it down");
        let (_, s) = step(h3, Some(("effect:c", None)), false, false, at(900));
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
        let key = want_transition(&mut state, spec, None).expect("a clip to preview on");
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
        let key = want_transition(&mut state, spec, None).expect("a clip");
        assert!(state.hover_preview_wanted.is_none() && note_for(&state, &key).is_some_and(|n| n.contains("no cut after it")));
    }

    /// A scrub over the card once up: the pointer resting where it came on
    /// shows the frame the preview opened on; moved across, it shows the
    /// time that far through; a twitch of less than 3% is no scrub; and
    /// none while playing.
    #[test]
    fn a_preview_up_scrubs_as_the_pointer_moves_across_its_card() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let (h, _) = step(None, Some(("effect:a", Some(0.4))), false, false, at(0));
        let (h, s) = step(h, Some(("effect:a", Some(0.41))), false, false, at(100));
        assert_eq!(s, Step::PutUp);
        let (h, s) = step(h, Some(("effect:a", Some(0.42))), false, false, at(120));
        assert_eq!(s, Step::Nothing, "a twitch is no scrub");
        let (h, s) = step(h, Some(("effect:a", Some(0.7))), false, false, at(140));
        assert_eq!(s, Step::Scrub(0.7));
        let (h, s) = step(h, Some(("effect:a", Some(0.7))), false, false, at(160));
        assert_eq!(s, Step::Nothing, "the same place again shows nothing new");
        let (_, s) = step(h, Some(("effect:a", Some(0.71))), false, false, at(180));
        assert_eq!(s, Step::Scrub(0.71), "once scrubbing, every move");
        let (h, _) = step(None, Some(("effect:p", Some(0.1))), true, false, at(0));
        let (h, _) = step(h, Some(("effect:p", Some(0.1))), true, false, at(100));
        let (_, s) = step(h, Some(("effect:p", Some(0.9))), true, false, at(140));
        assert_eq!(s, Step::Nothing, "no scrub while playing");
    }

    /// #3210 PR 3: a title or generator previewed is placed as its click
    /// places it, at the playhead, by the same functions, and shown where
    /// the click parks the playhead; its span is the clip's, for a scrub.
    #[test]
    fn a_placed_title_or_generator_is_the_one_its_click_places() {
        let (mut state, _) = timeline(2);
        state.playhead = klipp_core::Timecode::from_frames(30, 24);
        let title = klipp_timeline::TitleData::default();
        let gen = klipp_core::GeneratorData::SolidColor { color: [200, 40, 40, 255] };
        for place in [Place::Title(Box::new(title.clone())), Place::Generator(gen.clone())] {
            let want = PreviewWant { key: "place".into(), place: Some(place.clone()), ..Default::default() };
            let before = serde_json::to_string(state.sequence()).unwrap();
            let shown = shown_want(&state, &want).expect("a preview");
            assert_eq!(serde_json::to_string(state.sequence()).unwrap(), before, "the sequence previewed from is unchanged");
            // Placed by the click's own functions, on a state of its own.
            let mut clicked = AppState { sequences: vec![state.sequence().clone()], active_sequence_idx: 0, playhead: state.playhead, ..Default::default() };
            let (clip, park) = match &place {
                Place::Generator(data) => {
                    let (clip, _, park) = timeline_edit::add_generator_clip(&mut clicked, None, data);
                    (clip, park)
                }
                Place::Title(data) => {
                    let AppCommand::CreateTitleClip { track_index, timeline_in, duration_frames } = crate::browser::titles_tab::place_title(&mut clicked, (**data).clone()) else { unreachable!() };
                    let (clip, rest) = titles::add_title_clip(&mut clicked, track_index, timeline_in, duration_frames);
                    (clip, rest.unwrap_or(state.playhead))
                }
            };
            // The clips' ids are new each time: compared without them.
            let shape = |s: &klipp_timeline::Sequence| -> serde_json::Value {
                let mut v = serde_json::to_value(s.tracks.iter().map(|t| (&t.kind, t.clips.iter().map(|c| (c.timeline_in, c.timeline_out, serde_json::to_value(&c.kind).unwrap())).collect::<Vec<_>>())).collect::<Vec<_>>()).unwrap();
                v.take()
            };
            assert_eq!(shape(&shown.seq), shape(&clicked.sequences[0]), "the previewed sequence is the clicked one");
            let placed = clicked.sequences[0].all_clips().find(|c| c.id == clip).unwrap();
            assert_eq!((shown.at, shown.span), (Some(park), Some((placed.timeline_in, placed.timeline_out))), "shown where the click parks, over the clip's time");
        }
    }

    /// #3210 PR 4: an entry named as frame_at names it (in any case) is
    /// wanted as hovering its card wants it, on the clip given, else the
    /// selected one; and each way it can't be is said.
    #[test]
    fn a_named_entry_is_wanted_as_its_card_wants_it() {
        use klipp_core::PreviewEntry as E;
        let (mut state, first) = timeline(2);
        let second = state.sequence().tracks[0].clips[1].id;
        let blur = state.effect_registry.effects.iter().find(|e| matches!(e.source, crate::browser::effect_registry::EffectSource::Builtin(klipp_effects::BuiltinEffect::GaussianBlur { .. }))).map(|e| e.name.clone()).expect("the blur in the browser");
        let want = want_named(&mut state, &E::Effect(blur.to_uppercase()), Some(second)).expect("the blur, by its name in capitals");
        assert!(want.key.contains(&second.0.to_string()) && !want.cmds.is_empty(), "on the clip given: {}", want.key);
        let want = want_named(&mut state, &E::Effect(blur.clone()), None).expect("the blur");
        assert!(want.key.contains(&first.0.to_string()), "else the selected clip: {}", want.key);
        assert!(want_named(&mut state, &E::Effect("No Such Effect".into()), None).unwrap_err().contains("no effect"));
        let gen = crate::browser::generators_tab::generators()[0].name.as_str().to_owned();
        assert!(want_named(&mut state, &E::Generator(gen), None).is_ok_and(|w| matches!(w.place, Some(Place::Generator(_)))));
        let title = klipp_timeline::title_library::TITLES[0].name.to_string();
        assert!(want_named(&mut state, &E::Title(title), None).is_ok_and(|w| matches!(w.place, Some(Place::Title(_)))));
        let why = want_named(&mut state, &E::Transition("cross dissolve".into()), Some(second)).unwrap_err();
        assert!(why.contains("no cut after it"), "the card's reason: {why}");
        state.selected_clip = None;
        state.playhead = klipp_core::Timecode::from_frames(24 * 1000, 24);
        assert!(want_named(&mut state, &E::Effect(blur), None).unwrap_err().contains("no clip"), "nothing selected and nothing under the playhead");
    }

    #[test]
    fn a_speed_change_is_not_previewed() {
        let (state, clip) = timeline(1);
        let cmds = vec![AppCommand::AddBuiltinEffect { clip_id: clip, effect: BuiltinEffect::SpeedRamp { speed: 0.5, blend_mode: 2 } }];
        assert!(shown(&state, &cmds).unwrap_err().contains("speed"));
        assert!(shown(&state, &[AppCommand::Play]).is_err(), "nor anything but a picture edit");
    }

    /// The copy a preview is made on, measured on a long sequence: under
    /// half a frame at 60 fps (2.0 ms, and 4.2 ms for a title placed, alone
    /// on the reference machine). A probe: beside the whole suite the
    /// same copy took 12 ms.
    #[test]
    #[ignore = "perf probe: run explicitly, alone"]
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
        // A title placed, on a scratch state holding the copy.
        let want = PreviewWant { place: Some(Place::Title(Box::default())), ..Default::default() };
        let placed = (0..20)
            .map(|_| {
                let t0 = Instant::now();
                std::hint::black_box(shown_want(&state, &want).expect("a preview"));
                t0.elapsed().as_secs_f64() * 1000.0
            })
            .fold(f64::INFINITY, f64::min);
        eprintln!("a title placed over 2000 clips previewed in {placed:.2} ms at the fastest");
        assert!(placed < 8.0, "a title placed over 2000 clips took {placed:.2} ms at the fastest");
    }
}
