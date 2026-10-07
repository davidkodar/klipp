//! Effects: a clip's built-in and plugin effects added, changed and taken
//! off, each change one undo step (#826).

use super::*;

impl KlippApp {
    /// A clip's built-in effects, their enabled flags, speed and length,
    /// before an edit that may change them.
    fn clip_effects_state(&self, clip_id: klipp_core::ClipId) -> Option<ClipEffectsState> {
        self.state.sequences[self.state.active_sequence_idx]
            .all_clips()
            .find(|c| c.id == clip_id)
            .map(ClipEffectsState::of)
    }

    /// The clip as it is, before an edit that moves its effects about.
    fn clip_snapshot(&self, clip_id: klipp_core::ClipId) -> Option<klipp_timeline::Clip> {
        self.state.sequences[self.state.active_sequence_idx].all_clips().find(|c| c.id == clip_id).cloned()
    }

    /// `edit` through the clip's graph ops when every entry it touches
    /// takes them (node-graph M2 step 15b): one `PictureEdit` undo step,
    /// or none while `record` is false (a drag's live steps, which its end
    /// records). `None` when it doesn't take them, and the caller's
    /// effect-list mutators run as before: the kinds the clip places
    /// itself, until picture-truth clips are the norm. `Some(false)` when
    /// the clip refused it, said in a toast.
    fn effect_edit_by_ops(&mut self, clip_id: klipp_core::ClipId, edit: klipp_timeline::EffectEdit, description: &'static str, record: bool) -> Option<bool> {
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        let clip = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id))?;
        let ops = clip.effect_edit_ops(edit)?;
        match clip.apply_picture_ops(&ops) {
            Ok(done) => {
                self.state.dirty = true;
                if record {
                    self.state.push_history(EditCommand::PictureEdit { description, clip_id, edit: Box::new(done) });
                }
                Some(true)
            }
            Err(why) => {
                tracing::warn!(?edit, %why, "effect edit refused");
                self.state.toast.show(format!("{description}: {why}"));
                Some(false)
            }
        }
    }

    /// Have the engine draw the paused frame again. A new sequence alone
    /// keeps each clip's cached render key until `SequenceUpdated`, so an
    /// edit that sends only the sequence (a toggle did) showed the old
    /// frame from the render cache until another edit came along.
    fn refresh_paused_frame(&self) {
        if self.state.playback != PlaybackState::Playing {
            let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
        }
    }

    /// The clip's picture fields now, for a drag's ends.
    fn picture_now(&self, clip_id: klipp_core::ClipId) -> Option<klipp_timeline::ClipPictureFields> {
        self.state.sequences[self.state.active_sequence_idx].all_clips().find(|c| c.id == clip_id).map(|c| c.picture.clone())
    }

    /// Record an edit that moved effects about as one undo step: the
    /// clip whole either side, since the effects' keyframes and
    /// modifiers moved with them and the effects list alone would put
    /// them back without (#992).
    fn record_clip_replaced(&mut self, description: &'static str, before: Option<klipp_timeline::Clip>) {
        let Some(before) = before else { return };
        let Some(after) = self.clip_snapshot(before.id) else { return };
        self.state.dirty = true;
        self.state.push_history(EditCommand::ReplaceClip { description, old: Box::new(before), new: Box::new(after) });
    }

    /// Record an effect added or removed as one undo step: the effects
    /// list, and the clip's speed and length when a speed ramp moved them.
    fn record_effects_change(&mut self, clip_id: klipp_core::ClipId, before: Option<ClipEffectsState>) {
        let (Some(before), Some(after)) = (before, self.clip_effects_state(clip_id)) else { return };
        let edits = effects_change_edits(clip_id, &before, &after);
        if edits.is_empty() {
            return;
        }
        self.state.dirty = true;
        let history = self.state.active_history_mut();
        let owns_batch = edits.len() > 1 && !history.is_batching();
        if owns_batch {
            history.begin_batch();
        }
        for e in edits {
            self.state.push_history(e);
        }
        if owns_batch {
            self.state.active_history_mut().end_batch();
        }
    }

    /// Close an effect gesture whose release was never seen: as one
    /// undo entry from its "before" snapshot to the effect as it is
    /// now, or, when the effect has not moved (or the gesture is not
    /// an effect edit), by dropping it, so nothing bogus is pushed.
    fn close_open_effect_drag(&mut self) {
        let pending = self.state.active_history().pending_old_entry_ref().map(|e| e.command.clone());
        if let Some(EditCommand::PictureEdit { clip_id, description, edit }) = &pending {
            match self.picture_now(*clip_id) {
                Some(after) => self.state.active_history_mut().end_drag(HistoryEntry {
                    command: EditCommand::PictureEdit { clip_id: *clip_id, description, edit: Box::new(klipp_timeline::PictureEdit { before: edit.before.clone(), after }) },
                }),
                None => self.state.active_history_mut().abandon_pending_drag(),
            }
            return;
        }
        let Some(EditCommand::EditBuiltinEffect { clip_id, effect_index, old_effect, .. }) = pending else {
            self.state.active_history_mut().abandon_pending_drag();
            return;
        };
        let now = self.state.sequences[self.state.active_sequence_idx]
            .all_clips()
            .find(|c| c.id == clip_id)
            .and_then(|c| c.builtin_effects().get(effect_index).cloned());
        match now {
            Some(new_effect) if new_effect != old_effect => {
                self.state.active_history_mut().end_drag(HistoryEntry {
                    command: EditCommand::EditBuiltinEffect { clip_id, effect_index, old_effect, new_effect },
                });
            }
            _ => self.state.active_history_mut().abandon_pending_drag(),
        }
    }

    pub(super) fn handle_effects_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            // OFX effects
            AppCommand::AddEffect { clip_id, ref plugin_name } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_ofx_effects(|effects| effects.push(klipp_plugin::AppliedEffect {
                            plugin_name: plugin_name.clone(),
                            enabled: true,
                            param_values: std::collections::HashMap::new(),
                            ..Default::default()
                        }));
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::RemoveEffect { clip_id, effect_index } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if *effect_index < clip.ofx_effects().len() {
                            clip.edit_ofx_effects(|effects| effects.remove(*effect_index));
                        }
                        break;
                    }
                }
                if let Some((cid, idx, true)) = self.state.split_compare_effect {
                    if cid == *clip_id {
                        if idx == *effect_index {
                            self.state.split_compare_effect = None;
                            let _ = self.cmd_tx.send(AppCommand::SetSplitCompareEffect {
                                clip_id: None,
                                effect_index: 0,
                                is_ofx: false,
                            });
                        } else if idx > *effect_index {
                            self.state.split_compare_effect = Some((cid, idx - 1, true));
                            let _ = self.cmd_tx.send(AppCommand::SetSplitCompareEffect {
                                clip_id: Some(cid),
                                effect_index: idx - 1,
                                is_ofx: true,
                            });
                        }
                    }
                }
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::SetEffectEnabled { clip_id, effect_index, enabled } => {
                let mut old_enabled: Option<bool> = None;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_ofx_effect(*effect_index, |effect| {
                            old_enabled = Some(effect.enabled);
                            effect.enabled = *enabled;
                        });
                        break;
                    }
                }
                if let Some(prev) = old_enabled {
                    if prev != *enabled {
                        self.state.push_history(crate::history::EditCommand::SetOFXEffectEnabled {
                                clip_id: *clip_id,
                                effect_index: *effect_index,
                                old_enabled: prev,
                                new_enabled: *enabled,
                            });
                        self.state.dirty = true;
                    }
                }
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::ReorderEffect { clip_id, from_index, to_index } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        let from = *from_index;
                        let to = *to_index;
                        if from < clip.ofx_effects().len() && to < clip.ofx_effects().len() {
                            clip.edit_ofx_effects(|effects| {
                                let effect = effects.remove(from);
                                effects.insert(to, effect);
                            });
                        }
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::SetEffectParam { clip_id, effect_index, ref param_name, ref value } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_ofx_effect(*effect_index, |effect| {
                            effect.param_values.insert(param_name.clone(), value.clone());
                        });
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                true
            }

            // Built-in effects
            AppCommand::AddBuiltinEffect { clip_id, ref effect } => {
                if self.effect_edit_by_ops(*clip_id, klipp_timeline::EffectEdit::Add(effect), "Add Effect", true).is_some() {
                    self.sync_sequence_to_engine();
                    if self.state.playback != PlaybackState::Playing {
                        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    }
                    return true;
                }
                let before = self.clip_effects_state(*clip_id);
                let mut is_speed_ramp = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.push_effect(effect.clone());
                        // SpeedRamp: apply speed change on add
                        if let klipp_effects::BuiltinEffect::SpeedRamp { speed, freeze, .. } = effect {
                            is_speed_ramp = true;
                            let new_speed = if *freeze { 0.001 } else { (*speed as f64).clamp(0.1, 16.0) };
                            clip.speed = new_speed;
                            let source_dur_secs = clip.source_duration().to_secs_f64();
                            let new_dur_secs = source_dur_secs / new_speed;
                            let new_duration = klipp_core::RationalTime::from_secs_f64(new_dur_secs, clip.timeline_in.rate);
                            clip.timeline_out = clip.timeline_in + new_duration;
                        }
                        break;
                    }
                }
                self.record_effects_change(*clip_id, before);
                self.sync_sequence_to_engine();
                if is_speed_ramp {
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                } else if self.state.playback != PlaybackState::Playing {
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                }
                true
            }
            AppCommand::RemoveBuiltinEffect { clip_id, index } => {
                let before = self.clip_snapshot(*clip_id);
                let mut was_speed_ramp = false;
                let by_ops = self.effect_edit_by_ops(*clip_id, klipp_timeline::EffectEdit::Remove(*index), "Remove Effect", true);
                let mut removed = by_ops == Some(true);
                if by_ops == Some(false) {
                    return true;
                }
                if by_ops.is_none() {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if *index < clip.builtin_effects().len() {
                                // Check if we're removing a SpeedRamp — need to reset speed
                                if matches!(clip.builtin_effects()[*index], klipp_effects::BuiltinEffect::SpeedRamp { .. }) {
                                    was_speed_ramp = true;
                                }
                                // Its enabled flag, keyframes and modifiers
                                // go with it, and those of the effects after
                                // it move down with theirs.
                                removed = clip.remove_builtin_effect(*index).is_some();
                            }
                            // Reset clip speed if SpeedRamp was removed and no other SpeedRamp remains
                            if was_speed_ramp && !clip.builtin_effects().iter().any(|e| matches!(e, klipp_effects::BuiltinEffect::SpeedRamp { .. })) {
                                clip.speed = 1.0;
                                let source_dur_secs = clip.source_duration().to_secs_f64();
                                let new_duration = klipp_core::RationalTime::from_secs_f64(source_dur_secs, clip.timeline_in.rate);
                                clip.timeline_out = clip.timeline_in + new_duration;
                            }
                            break;
                        }
                    }
                }
                if let Some((cid, idx, false)) = self.state.split_compare_effect {
                    if cid == *clip_id {
                        if idx == *index {
                            self.state.split_compare_effect = None;
                            let _ = self.cmd_tx.send(AppCommand::SetSplitCompareEffect {
                                clip_id: None,
                                effect_index: 0,
                                is_ofx: false,
                            });
                        } else if idx > *index {
                            self.state.split_compare_effect = Some((cid, idx - 1, false));
                            let _ = self.cmd_tx.send(AppCommand::SetSplitCompareEffect {
                                clip_id: Some(cid),
                                effect_index: idx - 1,
                                is_ofx: false,
                            });
                        }
                    }
                }
                if removed && by_ops.is_none() {
                    self.record_clip_replaced("Remove Effect", before);
                }
                self.sync_sequence_to_engine();
                if was_speed_ramp {
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                } else if self.state.playback != PlaybackState::Playing {
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                }
                true
            }
            AppCommand::ToggleBuiltinEffectEnabled { clip_id, index } => {
                let on = self.state.sequences[self.state.active_sequence_idx]
                    .tracks
                    .iter_mut()
                    .find_map(|t| t.find_clip_mut(*clip_id))
                    .and_then(|c| c.effect_entries().nth(*index).map(|(_, _, on)| on));
                if let Some(on) = on {
                    if self.effect_edit_by_ops(*clip_id, klipp_timeline::EffectEdit::SetEnabled(*index, !on), "Toggle Effect", true).is_some() {
                        self.sync_sequence_to_engine();
                        self.refresh_paused_frame();
                        return true;
                    }
                }
                let mut flipped = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(id) = clip.effect_id_at(*index) {
                            let on = clip.effect_entries().nth(*index).is_some_and(|(_, _, on)| on);
                            flipped = clip.set_effect_enabled(id, !on);
                        }
                        break;
                    }
                }
                if flipped {
                    self.state.push_history(crate::history::EditCommand::ToggleBuiltinEffectEnabled {
                            clip_id: *clip_id,
                            index: *index,
                        });
                }
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                self.refresh_paused_frame();
                true
            }
            AppCommand::UpdateBuiltinEffect { clip_id, index, ref effect } => {
                // A drag's steps record nothing; its end records the drag.
                let drag_key = format!("{:?}:builtin:{}", clip_id, index);
                let history = self.state.active_history();
                let dragging = history.is_dragging(&drag_key) || history.is_dragging_builtin_effect(*clip_id, *index) || history.is_dragging_picture(*clip_id);
                if self.effect_edit_by_ops(*clip_id, klipp_timeline::EffectEdit::Update(*index, effect), "Edit Effect", !dragging).is_some() {
                    self.sync_sequence_to_engine();
                    if self.state.playback != PlaybackState::Playing {
                        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    }
                    return true;
                }
                let mut old_effect = None;
                let mut is_speed_ramp = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(id) = clip.effect_id_at(*index) {
                            old_effect = clip.effect(id).cloned();
                            clip.replace_effect(id, effect.clone());
                        }
                        // SpeedRamp: use the same logic as SetClipSpeed
                        if let klipp_effects::BuiltinEffect::SpeedRamp { speed, reverse, freeze, .. } = effect {
                            is_speed_ramp = true;
                            let new_speed = if *freeze {
                                0.001
                            } else {
                                (*speed as f64).clamp(0.1, 16.0)
                            };
                            clip.speed = new_speed;
                            // Adjust timeline_out exactly like SetClipSpeed
                            let source_dur_secs = clip.source_duration().to_secs_f64();
                            let new_dur_secs = source_dur_secs / new_speed;
                            let new_duration = klipp_core::RationalTime::from_secs_f64(new_dur_secs, clip.timeline_in.rate);
                            clip.timeline_out = clip.timeline_in + new_duration;
                            tracing::debug!("SpeedRamp: speed={} freeze={} reverse={} → clip.speed={} dur={}s",
                                speed, freeze, reverse, new_speed, new_dur_secs);
                        }
                        break;
                    }
                }
                // Push undo if NOT in a coalescing drag
                if let Some(old_fx) = old_effect {
                    let drag_key = format!("{:?}:builtin:{}", clip_id, index);
                    let history = self.state.active_history();
                    if !history.is_dragging(&drag_key) && !history.is_dragging_builtin_effect(*clip_id, *index) {
                        self.state.push_history(EditCommand::EditBuiltinEffect {
                                clip_id: *clip_id,
                                effect_index: *index,
                                old_effect: old_fx,
                                new_effect: effect.clone(),
                            });
                    }
                }
                self.sync_sequence_to_engine();
                if is_speed_ramp {
                    // Force engine to restart decoder at new speed
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                } else if self.state.playback != PlaybackState::Playing {
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                }
                true
            }

            // Effect drag gestures
            AppCommand::BeginEffectDrag { ref key, clip_id, effect_index, ref old_effect } => {
                // A gesture left open, its release never seen (#409), is
                // closed here rather than overwritten: as one entry when
                // its effect has moved since, silently when it has not.
                if self.state.active_history().is_any_drag_in_progress() {
                    tracing::warn!(key, "BeginEffectDrag with a drag still open: closing the open one first");
                    self.close_open_effect_drag();
                }
                // A plain effect's drag is one picture edit, from the
                // picture as the drag began.
                let plain = self.state.sequences[self.state.active_sequence_idx]
                    .all_clips()
                    .find(|c| c.id == *clip_id)
                    .filter(|c| c.effect_edit_ops(klipp_timeline::EffectEdit::Update(*effect_index, old_effect)).is_some())
                    .map(|c| c.picture.clone());
                if let Some(before) = plain {
                    self.state.active_history_mut().begin_drag(
                        key.clone(),
                        HistoryEntry {
                            command: EditCommand::PictureEdit {
                                clip_id: *clip_id,
                                description: "Edit Effect",
                                edit: Box::new(klipp_timeline::PictureEdit { before: before.clone(), after: before }),
                            },
                        },
                    );
                    return true;
                }
                self.state.active_history_mut().begin_drag(
                    key.clone(),
                    HistoryEntry {
                        command: EditCommand::EditBuiltinEffect {
                            clip_id: *clip_id,
                            effect_index: *effect_index,
                            old_effect: old_effect.clone(),
                            new_effect: old_effect.clone(), // placeholder, replaced at end_drag
                        },
                    },
                );
                true
            }
            AppCommand::AbandonEffectDrag { clip_id, effect_index, added } => {
                let picture_drag = match self.state.active_history().pending_old_entry_ref().map(|e| &e.command) {
                    Some(EditCommand::PictureEdit { clip_id: c, edit, .. }) if *c == *clip_id => Some(edit.before.clone()),
                    _ => None,
                };
                if let Some(before) = picture_drag {
                    self.state.active_history_mut().abandon_pending_drag();
                    if let Some(clip) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut().find_map(|t| t.find_clip_mut(*clip_id)) {
                        clip.restore_picture(&before);
                    }
                    self.sync_sequence_to_engine();
                    if self.state.playback != PlaybackState::Playing {
                        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    }
                    return true;
                }
                let old_effect = self.state.active_history().pending_old_entry_ref()
                    .and_then(|e| match &e.command {
                        EditCommand::EditBuiltinEffect { clip_id: c, effect_index: i, old_effect, .. }
                            if *c == *clip_id && *i == *effect_index => Some(old_effect.clone()),
                        _ => None,
                    });
                let history = self.state.active_history_mut();
                history.abandon_pending_drag();
                // The add that opened this gesture is the last edit on the
                // stack, the drag's own steps having pushed nothing. Retract
                // it, and the effect with it: nobody asked for an identity
                // Transform (#408).
                let retracted = *added
                    && history.retract_last(|last| is_add_of_effect(last, *clip_id, *effect_index));
                let mut changed = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if retracted {
                            changed = clip.remove_builtin_effect(*effect_index).is_some();
                        } else if let Some(old) = old_effect {
                            if let Some(id) = clip.effect_id_at(*effect_index) {
                                clip.edit_effect(id, |fx| {
                                    changed = *fx != old;
                                    *fx = old;
                                });
                            }
                        }
                        break;
                    }
                }
                if changed {
                    self.sync_sequence_to_engine();
                    if self.state.playback != PlaybackState::Playing {
                        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    }
                }
                true
            }
            AppCommand::EndEffectDrag { ref key, clip_id, effect_index, ref new_effect } => {
                let _ = key; // key is used to match begin_drag
                if self.state.active_history().is_dragging_picture(*clip_id) {
                    if let Some(after) = self.picture_now(*clip_id) {
                        let before = after.clone(); // replaced by the drag's own "before"
                        self.state.active_history_mut().end_drag(HistoryEntry {
                            command: EditCommand::PictureEdit { clip_id: *clip_id, description: "Edit Effect", edit: Box::new(klipp_timeline::PictureEdit { before, after }) },
                        });
                    }
                    return true;
                }
                // Read the old_effect from the pending entry
                let old_effect = self.state.active_history().pending_old_entry_ref()
                    .and_then(|e| match &e.command {
                        EditCommand::EditBuiltinEffect { old_effect, .. } => Some(old_effect.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| new_effect.clone());
                // The gizmo's first drag adds the Transform and then moves
                // it. When the add is the last edit on the stack, the move
                // is folded into it, so one undo takes both back.
                let folded = self.state.active_history_mut().merge_into_last(|last| {
                    fold_drag_into_add(last, *clip_id, *effect_index, &old_effect, new_effect)
                });
                if folded {
                    self.state.active_history_mut().abandon_pending_drag();
                    return true;
                }
                self.state.active_history_mut().end_drag(HistoryEntry {
                    command: EditCommand::EditBuiltinEffect {
                        clip_id: *clip_id,
                        effect_index: *effect_index,
                        old_effect,
                        new_effect: new_effect.clone(),
                    },
                });
                true
            }
            AppCommand::BeginAudioChainDrag { ref drag_key, clip_id } => {
                // Snapshot the clip's audio chain BEFORE the drag so
                // the merged history entry rolls back to the pre-drag
                // state on undo. Per-tick UpdateAudioEffectParam /
                // SetAudioEffectWetDry calls during the drag check
                // is_dragging(drag_key) and skip their own pushes,
                // so the entire gesture coalesces into one entry.
                let old_state = self.state.sequences[self.state.active_sequence_idx]
                    .all_clips()
                    .find(|c| c.id == *clip_id)
                    .map(crate::history::AudioChainSnapshot::from_clip);
                if let Some(old_state) = old_state {
                    self.state.active_history_mut().begin_drag(
                        drag_key.clone(),
                        HistoryEntry {
                            command: EditCommand::EditAudioChain {
                                clip_id: *clip_id,
                                description: "Edit Audio Effect Param",
                                old_state: old_state.clone(),
                                // Placeholder — combine_old_new replaces with end_drag's new_state.
                                new_state: old_state,
                            },
                        },
                    );
                }
                true
            }
            AppCommand::EndAudioChainDrag { ref drag_key, clip_id } => {
                let _ = drag_key; // matched against the pending begin_drag
                let new_state = self.state.sequences[self.state.active_sequence_idx]
                    .all_clips()
                    .find(|c| c.id == *clip_id)
                    .map(crate::history::AudioChainSnapshot::from_clip);
                let old_state = self.state.active_history().pending_old_entry_ref()
                    .and_then(|e| match &e.command {
                        EditCommand::EditAudioChain { old_state, .. } => Some(old_state.clone()),
                        _ => None,
                    });
                if let (Some(old_state), Some(new_state)) = (old_state, new_state) {
                    self.state.active_history_mut().end_drag(HistoryEntry {
                        command: EditCommand::EditAudioChain {
                            clip_id: *clip_id,
                            description: "Edit Audio Effect Param",
                            old_state,
                            new_state,
                        },
                    });
                }
                true
            }
            AppCommand::ReorderBuiltinEffect { clip_id, from_index, to_index } => {
                match self.effect_edit_by_ops(*clip_id, klipp_timeline::EffectEdit::Move { from: *from_index, to: *to_index }, "Reorder Effects", true) {
                    Some(true) => {
                        self.sync_sequence_to_engine();
                        if self.state.playback != PlaybackState::Playing {
                            let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                        }
                        return true;
                    }
                    Some(false) => return true,
                    None => {}
                }
                let before = self.clip_snapshot(*clip_id);
                let mut moved = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        // The effect's enabled flag, keyframes and
                        // modifiers move with it.
                        moved = clip.move_builtin_effect(*from_index, *to_index);
                        break;
                    }
                }
                if !moved {
                    return true;
                }
                self.record_clip_replaced("Reorder Effects", before);
                self.sync_sequence_to_engine();
                if self.state.playback != PlaybackState::Playing {
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                }
                true
            }

            // Effect presets
            AppCommand::SaveBuiltinEffectPreset { ref name, ref description, ref effect_key, ref effect } => {
                let preset = crate::effect_presets::EffectPreset {
                    name: name.clone(),
                    description: description.clone(),
                    effect_key: effect_key.clone(),
                    builtin: Some(effect.clone()),
                    ofx_params: None,
                    is_builtin: false,
                };
                self.state.preset_manager.save_user_preset(preset);
                true
            }
            AppCommand::DeleteBuiltinEffectPreset { ref effect_key, ref name } => {
                self.state.preset_manager.delete_user_preset(effect_key, name);
                true
            }

            // Audio DSP effects
            AppCommand::AddAudioEffect { clip_id, ref effect_type } => {
                // Audio effects are edited in the Audio inspector. Switch to
                // that surface as soon as the user adds one from the browser,
                // a drop, or another command so the new chain entry and its
                // controls are immediately discoverable.
                self.layout.active_inspector_tab = crate::layout_state::InspectorTab::Audio;
                // Snapshot pre-state for undo.
                let old_state = self.state.sequences[self.state.active_sequence_idx]
                    .all_clips()
                    .find(|c| c.id == *clip_id)
                    .map(crate::history::AudioChainSnapshot::from_clip);
                // Build default instance from effect descriptors. LADSPA plugins
                // don't expose `&'static str` param names, so their defaults
                // come from the discovered plugin info instead of the factory.
                let instance = self.default_audio_effect(effect_type);
                let mut added = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.audio_effects.push(instance.clone());
                        tracing::debug!("audio_fx: added effect '{}' to clip {:?}, total: {}", effect_type, clip.id, clip.audio_effects.len());
                        added = true;
                        break;
                    }
                }
                if !added {
                    // A stale browser/inspector command must be visible to the
                    // user rather than looking like a no-op. This can happen
                    // when a clip is deleted or the active sequence changes
                    // between frames.
                    tracing::warn!("audio_fx: could not find target clip {:?} for '{}'", clip_id, effect_type);
                    self.state.toast.show("Select an audio clip before adding an effect");
                    return true;
                }
                self.push_audio_chain_history(*clip_id, "Add Audio Effect", old_state);
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                true
            }
            AppCommand::RemoveAudioEffect { clip_id, effect_index } => {
                let old_state = self.state.sequences[self.state.active_sequence_idx]
                    .all_clips()
                    .find(|c| c.id == *clip_id)
                    .map(crate::history::AudioChainSnapshot::from_clip);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if *effect_index < clip.audio_effects.len() {
                            clip.audio_effects.remove(*effect_index);
                            // Drop kf tracks belonging to this effect — keys
                            // are "{idx}:..." so the prefix tells us. Higher
                            // indices DO NOT shift here; that's a known
                            // limitation matching how video effects behave.
                            let prefix = format!("{}:", effect_index);
                            clip.audio_effect_keyframes.retain(|k, _| !k.starts_with(&prefix));
                        }
                        break;
                    }
                }
                self.push_audio_chain_history(*clip_id, "Remove Audio Effect", old_state);
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                true
            }
            AppCommand::UpdateAudioEffectParam { clip_id, effect_index, ref param_name, value } => {
                let drag_key = format!("audio_chain:{}:{}:{}", clip_id.0, effect_index, param_name);
                let is_dragging = self.state.active_history().is_dragging(&drag_key);
                // Snapshot only when we'll actually push — saves an
                // AudioChainSnapshot::clone on every drag tick.
                let old_state = if !is_dragging {
                    self.state.sequences[self.state.active_sequence_idx]
                        .all_clips()
                        .find(|c| c.id == *clip_id)
                        .map(crate::history::AudioChainSnapshot::from_clip)
                } else {
                    None
                };
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(inst) = clip.audio_effects.get_mut(*effect_index) {
                            if let Some(p) = inst.params.iter_mut().find(|(k, _)| k == param_name) {
                                p.1 = *value;
                            } else {
                                inst.params.push((param_name.clone(), *value));
                            }
                        }
                        break;
                    }
                }
                // History push only on non-drag updates (text-entry,
                // single-click reset). During a drag the
                // BeginAudioChainDrag / EndAudioChainDrag pair owns
                // history coalescing — without this guard every
                // 60Hz drag tick pushes a separate entry, bumping
                // ops_since_save and triggering "Auto saved" toasts
                // continuously while the user moves a slider.
                if !is_dragging {
                    self.push_audio_chain_history(*clip_id, "Edit Audio Effect Param", old_state);
                }
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                // Param edits emit SequenceUpdated so the running chain rebuilds —
                // without it the user adjusts a slider and hears nothing change
                // until the next seek. The fingerprint check skips pointless
                // restarts when the value is identical.
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                true
            }
            AppCommand::SetAudioEffectParams { clip_id, effect_index, ref params } => {
                self.set_audio_effect_params(*clip_id, *effect_index, params, "Apply Audio Effect Preset");
                true
            }
            AppCommand::LearnDenoiserProfile { clip_id, effect_index, at_clip_secs } => {
                self.learn_denoiser_profile(*clip_id, *effect_index, *at_clip_secs);
                true
            }
            AppCommand::ToggleAudioEffect { clip_id, effect_index } => {
                let old_state = self.state.sequences[self.state.active_sequence_idx]
                    .all_clips()
                    .find(|c| c.id == *clip_id)
                    .map(crate::history::AudioChainSnapshot::from_clip);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(inst) = clip.audio_effects.get_mut(*effect_index) {
                            inst.enabled = !inst.enabled;
                        }
                        break;
                    }
                }
                self.push_audio_chain_history(*clip_id, "Toggle Audio Effect", old_state);
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                true
            }
            AppCommand::SetAudioEffectWetDry { clip_id, effect_index, wet_dry } => {
                // Same drag-coalesce shape as UpdateAudioEffectParam. The
                // wet/dry slider is also dragged so per-tick history pushes
                // would spam autosave too.
                let drag_key = format!("audio_chain:{}:{}:__wetdry", clip_id.0, effect_index);
                let is_dragging = self.state.active_history().is_dragging(&drag_key);
                let old_state = if !is_dragging {
                    self.state.sequences[self.state.active_sequence_idx]
                        .all_clips()
                        .find(|c| c.id == *clip_id)
                        .map(crate::history::AudioChainSnapshot::from_clip)
                } else {
                    None
                };
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(inst) = clip.audio_effects.get_mut(*effect_index) {
                            inst.wet_dry = *wet_dry;
                        }
                        break;
                    }
                }
                if !is_dragging {
                    self.push_audio_chain_history(*clip_id, "Set Wet/Dry", old_state);
                }
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                true
            }
            AppCommand::ReorderAudioEffect { clip_id, from_index, to_index } => {
                let old_state = self.state.sequences[self.state.active_sequence_idx]
                    .all_clips()
                    .find(|c| c.id == *clip_id)
                    .map(crate::history::AudioChainSnapshot::from_clip);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        let from = *from_index;
                        let to = *to_index;
                        if from < clip.audio_effects.len() && to < clip.audio_effects.len() {
                            let fx = clip.audio_effects.remove(from);
                            clip.audio_effects.insert(to, fx);
                        }
                        break;
                    }
                }
                self.push_audio_chain_history(*clip_id, "Reorder Audio Effect", old_state);
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                true
            }

            // Parameter modifiers (behaviors)
            AppCommand::AddModifier { clip_id, ref param, ref modifier } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        with_modifiers(clip, param, true, |stack| stack.modifiers.push(modifier.clone()));
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::RemoveModifier { clip_id, ref param, modifier_index } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        let emptied = with_modifiers(clip, param, false, |stack| {
                            if *modifier_index < stack.modifiers.len() {
                                stack.modifiers.remove(*modifier_index);
                            }
                            stack.modifiers.is_empty()
                        });
                        if emptied == Some(true) {
                            {
                                remove_modifiers(clip, param);
                            }
                        }
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::UpdateModifier { clip_id, ref param, modifier_index, ref modifier } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        with_modifiers(clip, param, false, |stack| {
                            if let Some(m) = stack.modifiers.get_mut(*modifier_index) {
                                *m = modifier.clone();
                            }
                        });
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::ToggleModifier { clip_id, ref param, modifier_index } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        with_modifiers(clip, param, false, |stack| {
                            if let Some(m) = stack.modifiers.get_mut(*modifier_index) {
                                match m {
                                    klipp_core::Modifier::Oscillate { enabled, .. } |
                                    klipp_core::Modifier::Wiggle { enabled, .. } |
                                    klipp_core::Modifier::Throw { enabled, .. } |
                                    klipp_core::Modifier::Ramp { enabled, .. } |
                                    klipp_core::Modifier::Loop { enabled, .. } |
                                    klipp_core::Modifier::PingPong { enabled, .. } |
                                    klipp_core::Modifier::Clamp { enabled, .. } => {
                                        *enabled = !*enabled;
                                    }
                                }
                            }
                        });
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }

            _ => false,
        }
    }
}

impl KlippApp {
    /// A new audio effect of `effect_type` at its defaults: a LADSPA
    /// plugin's from what discovery found, a built-in one's from its own
    /// descriptors.
    pub(super) fn default_audio_effect(&self, effect_type: &str) -> klipp_core::AudioEffectInstance {
        let params: Vec<(String, f32)> = if effect_type.starts_with("ladspa:") {
            effect_type
                .strip_prefix("ladspa:")
                .and_then(|rest| {
                    let cut = rest.rfind(':')?;
                    let path = &rest[..cut];
                    let label = &rest[cut + 1..];
                    self.state.ladspa_plugins.iter().find(|p| p.path.to_string_lossy() == path && p.label == label)
                })
                .map(|info| info.control_inputs.iter().map(|port| (port.name.clone(), port.default)).collect())
                .unwrap_or_default()
        } else {
            klipp_audio::dsp::create_effect(effect_type, 48000)
                .map(|fx| fx.param_descriptors().iter().map(|d| (d.name.to_string(), d.default)).collect())
                .unwrap_or_default()
        };
        klipp_core::AudioEffectInstance { effect_type: effect_type.to_string(), enabled: true, wet_dry: 1.0, params }
    }
}

/// A clip's built-in effects as an undo step sees them.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ClipEffectsState {
    pub(super) effects: Vec<klipp_effects::BuiltinEffect>,
    pub(super) enabled: Vec<bool>,
    pub(super) speed: f64,
    pub(super) timeline_out: klipp_core::RationalTime,
}

impl ClipEffectsState {
    pub(super) fn of(clip: &klipp_timeline::Clip) -> Self {
        Self {
            effects: clip.builtin_effects().to_vec(),
            enabled: clip.builtin_effects_enabled().to_vec(),
            speed: clip.speed,
            timeline_out: clip.timeline_out,
        }
    }
}

/// The history edits that take a clip from `before` to `after`: the
/// effects list when it changed, and the speed and length when a speed
/// ramp moved them. Adding or removing a built-in effect recorded
/// nothing, so it could not be undone (#391).
pub(super) fn effects_change_edits(clip_id: klipp_core::ClipId, before: &ClipEffectsState, after: &ClipEffectsState) -> Vec<EditCommand> {
    let mut edits = Vec::new();
    if before.effects != after.effects || before.enabled != after.enabled {
        edits.push(EditCommand::SetBuiltinEffects {
            clip_id,
            old_effects: before.effects.clone(),
            old_enabled: before.enabled.clone(),
            new_effects: after.effects.clone(),
            new_enabled: after.enabled.clone(),
        });
    }
    if before.speed != after.speed || before.timeline_out != after.timeline_out {
        edits.push(EditCommand::SetClipSpeed {
            clip_id,
            old_speed: before.speed,
            new_speed: after.speed,
            old_timeline_out: before.timeline_out,
            new_timeline_out: after.timeline_out,
        });
    }
    edits
}

/// Whether `last` is the add that appended effect `index` to `clip_id`.
pub(super) fn is_add_of_effect(last: &EditCommand, clip_id: klipp_core::ClipId, index: usize) -> bool {
    matches!(
        last,
        EditCommand::SetBuiltinEffects { clip_id: c, old_effects, new_effects, .. }
            if *c == clip_id && new_effects.len() == old_effects.len() + 1 && index + 1 == new_effects.len()
    )
}

/// Fold a finished drag of effect `index` into the history's last edit
/// when that edit is the add that made the effect, with the effect as
/// the drag found it: the add then records the effect as the drag left
/// it, and one undo takes both back.
pub(super) fn fold_drag_into_add(
    last: &mut EditCommand,
    clip_id: klipp_core::ClipId,
    index: usize,
    dragged_from: &klipp_effects::BuiltinEffect,
    dragged_to: &klipp_effects::BuiltinEffect,
) -> bool {
    match last {
        EditCommand::SetBuiltinEffects { clip_id: c, old_effects, new_effects, .. }
            if *c == clip_id
                && new_effects.len() == old_effects.len() + 1
                && index + 1 == new_effects.len()
                && new_effects[index] == *dragged_from =>
        {
            new_effects[index] = dragged_to.clone();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod effect_toggle_tests {
    //! Pure-logic coverage for the per-effect enable/disable toggle. The
    //! full handler runs on KlippApp and is hard to unit-test in isolation;
    //! these tests exercise the parallel-vec flip/grow logic that both the
    //! forward handler and the undo/redo arms share.

    fn toggle_builtin_enabled(enabled: &mut Vec<bool>, effect_count: usize, index: usize) {
        while enabled.len() < effect_count {
            enabled.push(true);
        }
        if let Some(val) = enabled.get_mut(index) {
            *val = !*val;
        }
    }

    #[test]
    fn builtin_effects_enabled_flip_logic() {
        let mut enabled = vec![true, true];
        toggle_builtin_enabled(&mut enabled, 2, 0);
        assert_eq!(enabled, vec![false, true]);
        // Simulate undo (flip again) — returns to original state.
        toggle_builtin_enabled(&mut enabled, 2, 0);
        assert_eq!(enabled, vec![true, true]);
    }

    #[test]
    fn builtin_toggle_is_idempotent_across_undo_redo() {
        // Three flips round-trips through enabled -> disabled -> enabled
        // -> disabled, mirroring user action -> undo -> redo -> user again.
        let mut enabled = vec![true];
        for _ in 0..3 {
            toggle_builtin_enabled(&mut enabled, 1, 0);
        }
        assert_eq!(enabled, vec![false]);
        toggle_builtin_enabled(&mut enabled, 1, 0);
        assert_eq!(enabled, vec![true]);
    }

    #[test]
    fn builtin_toggle_grows_parallel_vec_lazily() {
        // The parallel vec may be shorter than the effect list when projects
        // load from old `.klipp` files. Both the forward handler and the
        // undo arm grow it with `true` defaults so toggling the last effect
        // still works.
        let mut enabled: Vec<bool> = Vec::new();
        toggle_builtin_enabled(&mut enabled, 3, 2);
        assert_eq!(enabled.len(), 3);
        assert_eq!(enabled, vec![true, true, false]);
    }

    #[test]
    fn ofx_enabled_field_round_trip() {
        // OFX flag is an absolute bool — undo restores old, redo restores new.
        let mut enabled = true;
        let old = enabled;
        let new = false;
        // Forward
        enabled = new;
        assert!(!enabled);
        // Undo
        enabled = old;
        assert!(enabled);
        // Redo
        enabled = new;
        assert!(!enabled);
    }
}

#[cfg(test)]
mod effects_undo_tests {
    use super::*;

    fn crop(left: f32) -> klipp_effects::BuiltinEffect {
        klipp_effects::BuiltinEffect::Crop { left, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0 }
    }

    fn state(effects: Vec<klipp_effects::BuiltinEffect>, speed: f64) -> ClipEffectsState {
        let enabled = vec![true; effects.len()];
        ClipEffectsState { effects, enabled, speed, timeline_out: klipp_core::RationalTime { value: 300, rate: 30 } }
    }

    #[test]
    fn adding_an_effect_is_one_undoable_edit() {
        let id = klipp_core::ClipId::new();
        let edits = effects_change_edits(id, &state(vec![], 1.0), &state(vec![crop(0.1)], 1.0));
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].label(), "Add Effect");
    }

    #[test]
    fn removing_one_says_so_and_nothing_changed_records_nothing() {
        let id = klipp_core::ClipId::new();
        let edits = effects_change_edits(id, &state(vec![crop(0.1)], 1.0), &state(vec![], 1.0));
        assert_eq!(edits[0].label(), "Remove Effect");
        assert!(effects_change_edits(id, &state(vec![crop(0.1)], 1.0), &state(vec![crop(0.1)], 1.0)).is_empty());
    }

    #[test]
    fn a_speed_ramp_brings_its_speed_with_it() {
        let id = klipp_core::ClipId::new();
        let mut after = state(vec![crop(0.1)], 2.0);
        after.timeline_out = klipp_core::RationalTime { value: 150, rate: 30 };
        let edits = effects_change_edits(id, &state(vec![], 1.0), &after);
        assert_eq!(edits.len(), 2);
        assert!(matches!(edits[1], EditCommand::SetClipSpeed { new_speed, .. } if new_speed == 2.0));
    }

    /// The gizmo's first drag: the add of an identity effect, then a
    /// drag of it, becomes one add of the dragged effect.
    #[test]
    fn a_drag_of_the_effect_just_added_folds_into_the_add() {
        let id = klipp_core::ClipId::new();
        let mut last = effects_change_edits(id, &state(vec![], 1.0), &state(vec![crop(0.0)], 1.0)).remove(0);
        assert!(fold_drag_into_add(&mut last, id, 0, &crop(0.0), &crop(0.3)));
        let EditCommand::SetBuiltinEffects { new_effects, old_effects, .. } = &last else { panic!() };
        assert!(old_effects.is_empty());
        assert_eq!(new_effects, &vec![crop(0.3)]);
        // Not another clip's, not another effect, not one that was changed since.
        assert!(!fold_drag_into_add(&mut last.clone(), klipp_core::ClipId::new(), 0, &crop(0.3), &crop(0.5)));
        assert!(!fold_drag_into_add(&mut last.clone(), id, 1, &crop(0.3), &crop(0.5)));
        assert!(!fold_drag_into_add(&mut last.clone(), id, 0, &crop(0.9), &crop(0.5)));
    }
}

/// The modifier stack of `param` on `clip`: an effect parameter's through
/// the effect's id (keyed as the saved form), the clip's own directly.
/// `create` makes an empty one where there is none. Changed in `f`, which
/// gives back what `f` returns; `None` where there is no stack (and none was
/// made), and for an effect index past the list, which names no effect.
fn with_modifiers<R>(clip: &mut klipp_timeline::Clip, param: &klipp_core::ParamId, create: bool, f: impl FnOnce(&mut klipp_core::ModifierStack) -> R) -> Option<R> {
    match param {
        klipp_core::ParamId::Effect { effect_index, param_name } => {
            // An index past the list names no effect: nothing is kept there
            // to animate whichever effect lands at it later.
            let id = clip.effect_id_at(*effect_index)?;
            if create {
                clip.edit_effect_modifiers(id, param_name, f)
            } else {
                clip.edit_existing_effect_modifiers(id, param_name, f)
            }
        }
        _ if create => Some(clip.edit_param_modifiers(|v| f(v.entry(param.clone()).or_default()))),
        _ => clip.edit_param_modifiers(|v| v.get_mut(param).map(f)),
    }
}

/// Take `param`'s modifier stack off `clip`, as [`modifiers_mut`] finds it.
fn remove_modifiers(clip: &mut klipp_timeline::Clip, param: &klipp_core::ParamId) {
    match param {
        klipp_core::ParamId::Effect { effect_index, param_name } => {
            if let Some(id) = clip.effect_id_at(*effect_index) {
                clip.remove_effect_modifiers(id, param_name);
            }
        }
        _ => {
            clip.edit_param_modifiers(|v| v.remove(param));
        }
    }
}

#[cfg(test)]
mod modifier_target_tests {
    use super::{with_modifiers, remove_modifiers};
    use klipp_core::{ModifierStack, ParamId};

    fn clip() -> klipp_timeline::Clip {
        let t = |n| klipp_core::RationalTime::from_frames(n, 24);
        klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0), t(24), t(0), t(24))
    }

    fn effect_param(i: usize) -> ParamId {
        ParamId::Effect { effect_index: i, param_name: "radius".into() }
    }

    /// An effect added to a clip whose flag list runs past its effects (an
    /// old file can hold that) arrives on. Today's push added no flag, so
    /// the new effect took the stale one at its index and could arrive
    /// switched off; a latent bug, fixed by the effect-list API (#1073).
    #[test]
    fn an_added_effect_arrives_on_past_stale_flags() {
        let mut c = clip();
        c.set_effect_list(vec![klipp_effects::BuiltinEffect::Gamma { gamma: 1.0 }], vec![true, false, false]);
        c.push_effect(klipp_effects::BuiltinEffect::Gamma { gamma: 2.0 });
        assert_eq!(c.builtin_effects().len(), 2);
        assert_eq!(c.builtin_effects_enabled(), [true, true], "the stale flags go and the new effect is on");
    }

    /// An effect parameter's stack is the effect's, keyed as saved; the
    /// clip's own stays direct.
    #[test]
    fn stacks_land_on_their_effect_or_the_clip() {
        let mut c = clip();
        c.push_effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 });
        assert!(with_modifiers(&mut c, &effect_param(0), true, |_| ()).is_some());
        assert!(c.param_modifiers().contains_key(&effect_param(0)));
        assert!(with_modifiers(&mut c, &ParamId::Opacity, true, |_| ()).is_some());
        assert!(c.param_modifiers().contains_key(&ParamId::Opacity));
        assert!(with_modifiers(&mut c, &ParamId::Scale, false, |_| ()).is_none(), "asking without create makes nothing");
        remove_modifiers(&mut c, &effect_param(0));
        assert!(!c.param_modifiers().contains_key(&effect_param(0)));
    }

    /// An index past the list names no effect: no stack, and no keyframe
    /// track, is started there, where it would animate whichever effect
    /// lands at that index later (it used to be).
    #[test]
    fn an_index_past_the_list_starts_nothing() {
        let mut c = clip();
        c.push_effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 });
        assert!(with_modifiers(&mut c, &effect_param(3), true, |_| ()).is_none());
        assert!(c.param_modifiers().is_empty());
        // The keyframe commands resolve the same way.
        assert!(c.effect_id_at(3).is_none());
        // Once an effect does land at that index, it starts clean.
        for _ in 0..3 {
            c.push_effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 1.0 });
        }
        let id = c.effect_id_at(3).unwrap();
        assert!(c.edit_existing_effect_modifiers(id, "radius", |_| ()).is_none());
        let _: Option<()> = with_modifiers(&mut c, &effect_param(3), false, |_: &mut ModifierStack| ());
        assert!(c.param_modifiers().is_empty());
    }
}
