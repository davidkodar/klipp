//! Transitions: added, changed and taken off between clips (#826).

use super::*;

impl KlippApp {
    pub(super) fn handle_transition_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::AddTransition { track_index, clip_a_id, clip_b_id, kind_id, duration_frames } => {
                // Through the document's transition op, as the assistant
                // adds one: the two clips must meet at a cut with handles
                // for the overlap, it is one undo step, and on an audio
                // track it is a crossfade. A picture transition crossfades
                // the linked sound under it where it can, rather than
                // dipping it to silence at the cut.
                let seq = self.state.sequence();
                let next_is_b = seq.tracks.get(*track_index).is_some_and(|t| {
                    t.clips.iter().position(|c| c.id == *clip_a_id).and_then(|i| t.clips.get(i + 1)).map(|c| c.id) == Some(*clip_b_id)
                });
                if !next_is_b {
                    self.state.toast.show("A transition goes between two clips next to each other on one track".to_string());
                    return true;
                }
                let Some(spec) = klipp_core::spec_for_kind_id(*kind_id) else { return true };
                let secs = *duration_frames as f64 / seq.fps.as_f64();
                let op = klipp_document::ops::Op::Transition {
                    clip_id: klipp_document::ops::ClipRef::Id(*clip_a_id),
                    kind: spec.string_key.to_string(),
                    secs,
                    remove: false,
                };
                match klipp_document::ops::resolve(seq, &self.state.media_pool, std::slice::from_ref(&op)) {
                    Ok(plan) => {
                        let edits: Vec<EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                        self.apply_edits_as_one(&edits, "Add Transition");
                        crate::browser::library_card::push_recent(&mut self.state.library_recent.transitions, spec.string_key);
                    }
                    Err(r) => self.state.toast.show(format!("No transition: {}. {}", r.why, capitalise(&r.hint))),
                }
                true
            }
            AppCommand::RemoveTransition { track_index, transition_id } => {
                let seq = self.state.sequence();
                let Some(track) = seq.tracks.get(*track_index) else { return true };
                let Some(transition) = track.transitions.iter().find(|t| t.id == *transition_id).copied() else { return true };
                let fades = |t: &klipp_timeline::Track, tr: &klipp_timeline::Transition| {
                    let out_a = t.find_clip(tr.clip_a_id).map_or(0.0, |c| c.fade_out_secs);
                    let in_b = t.find_clip(tr.clip_b_id).map_or(0.0, |c| c.fade_in_secs);
                    (out_a, in_b)
                };
                let (old_fade_out_a, old_fade_in_b) = fades(track, &transition);
                let mut edits = vec![EditCommand::RemoveTransition { track_index: *track_index, transition, old_fade_out_a, old_fade_in_b }];
                // A picture transition's crossfade of its linked sound
                // goes with it, as it came with it.
                if track.kind != klipp_timeline::TrackKind::Audio {
                    let linked = |id| seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id).and_then(|c| c.linked_clip_id);
                    if let (Some(la), Some(lb)) = (linked(transition.clip_a_id), linked(transition.clip_b_id)) {
                        for (ti, t) in seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Audio && !t.locked) {
                            if let Some(sound) = t.transitions.iter().find(|x| x.clip_a_id == la && x.clip_b_id == lb).copied() {
                                let (a, b) = fades(t, &sound);
                                edits.push(EditCommand::RemoveTransition { track_index: ti, transition: sound, old_fade_out_a: a, old_fade_in_b: b });
                            }
                        }
                    }
                }
                if self.state.selected_transition == Some((*track_index, *transition_id)) {
                    self.state.selected_transition = None;
                }
                self.apply_edits_as_one(&edits, "Remove Transition");
                true
            }
            AppCommand::SetTransitionDuration { track_index, transition_id, duration_frames } => {
                let seq = self.state.sequence();
                let fps = seq.fps;
                // Frames of the sequence, as ticks at its rate: a frame is
                // `den` of them, one only at a whole-number rate.
                let duration = klipp_core::RationalTime { value: i64::from(*duration_frames) * i64::from(fps.den), rate: fps.num };
                let Some(t) = seq.tracks.get(*track_index).and_then(|t| t.transitions.iter().find(|t| t.id == *transition_id)) else { return true };
                // The same time at another rate is no edit.
                if t.duration.to_secs_f64() != duration.to_secs_f64() {
                    self.reshape_transition(*track_index, *transition_id, duration, t.alignment);
                }
                true
            }
            AppCommand::SetTransitionKind { track_index, transition_id, kind_id } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    if let Some(t) = track.transitions.iter_mut().find(|t| t.id == *transition_id) {
                        t.kind = klipp_engine::transition::transition_kind_from_id(*kind_id);
                    }
                }
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                true
            }
            AppCommand::SetTransitionParam { track_index, transition_id, user_params } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    if let Some(t) = track.transitions.iter_mut().find(|t| t.id == *transition_id) {
                        t.user_params = *user_params;
                    }
                }
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                true
            }
            AppCommand::SetTransitionAlignment { track_index, transition_id, alignment } => {
                let seq = self.state.sequence();
                let Some(t) = seq.tracks.get(*track_index).and_then(|t| t.transitions.iter().find(|t| t.id == *transition_id)) else { return true };
                if t.alignment != *alignment {
                    self.reshape_transition(*track_index, *transition_id, t.duration, *alignment);
                }
                true
            }
            _ => false,
        }
    }
}
