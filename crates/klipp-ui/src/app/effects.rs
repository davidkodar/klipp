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
        let Some(EditCommand::EditBuiltinEffect { clip_id, effect_index, old_effect, .. }) = pending else {
            self.state.active_history_mut().abandon_pending_drag();
            return;
        };
        let now = self.state.sequences[self.state.active_sequence_idx]
            .all_clips()
            .find(|c| c.id == clip_id)
            .and_then(|c| c.builtin_effects.get(effect_index).cloned());
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
                        clip.effects.push(klipp_plugin::AppliedEffect {
                            plugin_name: plugin_name.clone(),
                            enabled: true,
                            param_values: std::collections::HashMap::new(),
                            ..Default::default()
                        });
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::RemoveEffect { clip_id, effect_index } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if *effect_index < clip.effects.len() {
                            clip.effects.remove(*effect_index);
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
                        if let Some(effect) = clip.effects.get_mut(*effect_index) {
                            old_enabled = Some(effect.enabled);
                            effect.enabled = *enabled;
                        }
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
                        if from < clip.effects.len() && to < clip.effects.len() {
                            let effect = clip.effects.remove(from);
                            clip.effects.insert(to, effect);
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
                        if let Some(effect) = clip.effects.get_mut(*effect_index) {
                            effect.param_values.insert(param_name.clone(), value.clone());
                        }
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                true
            }

            // Built-in effects
            AppCommand::AddBuiltinEffect { clip_id, ref effect } => {
                let before = self.clip_effects_state(*clip_id);
                let mut is_speed_ramp = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.builtin_effects.push(effect.clone());
                        // Its enabled flag with it, where the clip keeps
                        // flags for the effects before it.
                        if clip.builtin_effects_enabled.len() + 1 == clip.builtin_effects.len() {
                            clip.builtin_effects_enabled.push(true);
                        }
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
                let before = self.clip_effects_state(*clip_id);
                let mut was_speed_ramp = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if *index < clip.builtin_effects.len() {
                            // Check if we're removing a SpeedRamp — need to reset speed
                            if matches!(clip.builtin_effects[*index], klipp_effects::BuiltinEffect::SpeedRamp { .. }) {
                                was_speed_ramp = true;
                            }
                            clip.builtin_effects.remove(*index);
                            // The enabled flags run parallel to the
                            // effects: leaving this one in place gave
                            // every effect after it its neighbour's flag.
                            if *index < clip.builtin_effects_enabled.len() {
                                clip.builtin_effects_enabled.remove(*index);
                            }
                        }
                        // Reset clip speed if SpeedRamp was removed and no other SpeedRamp remains
                        if was_speed_ramp && !clip.builtin_effects.iter().any(|e| matches!(e, klipp_effects::BuiltinEffect::SpeedRamp { .. })) {
                            clip.speed = 1.0;
                            let source_dur_secs = clip.source_duration().to_secs_f64();
                            let new_duration = klipp_core::RationalTime::from_secs_f64(source_dur_secs, clip.timeline_in.rate);
                            clip.timeline_out = clip.timeline_in + new_duration;
                        }
                        break;
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
                self.record_effects_change(*clip_id, before);
                self.sync_sequence_to_engine();
                if was_speed_ramp {
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                } else if self.state.playback != PlaybackState::Playing {
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                }
                true
            }
            AppCommand::ToggleBuiltinEffectEnabled { clip_id, index } => {
                let mut flipped = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        // Grow enabled vec if needed (default = all enabled)
                        while clip.builtin_effects_enabled.len() < clip.builtin_effects.len() {
                            clip.builtin_effects_enabled.push(true);
                        }
                        if let Some(val) = clip.builtin_effects_enabled.get_mut(*index) {
                            *val = !*val;
                            flipped = true;
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
                true
            }
            AppCommand::UpdateBuiltinEffect { clip_id, index, ref effect } => {
                let mut old_effect = None;
                let mut is_speed_ramp = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(fx) = clip.builtin_effects.get_mut(*index) {
                            old_effect = Some(fx.clone());
                            *fx = effect.clone();
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
                            if *effect_index < clip.builtin_effects.len() {
                                clip.builtin_effects.remove(*effect_index);
                                changed = true;
                            }
                            if *effect_index < clip.builtin_effects_enabled.len() {
                                clip.builtin_effects_enabled.remove(*effect_index);
                            }
                        } else if let Some(old) = old_effect {
                            if let Some(fx) = clip.builtin_effects.get_mut(*effect_index) {
                                changed = *fx != old;
                                *fx = old;
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
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        let from = *from_index;
                        let to = *to_index;
                        if from < clip.builtin_effects.len() && to < clip.builtin_effects.len() {
                            let fx = clip.builtin_effects.remove(from);
                            clip.builtin_effects.insert(to, fx);
                        }
                        break;
                    }
                }
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
                        clip.param_modifiers.entry(param.clone()).or_default().modifiers.push(modifier.clone());
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
                        if let Some(stack) = clip.param_modifiers.get_mut(param) {
                            if *modifier_index < stack.modifiers.len() {
                                stack.modifiers.remove(*modifier_index);
                            }
                            if stack.modifiers.is_empty() {
                                clip.param_modifiers.remove(param);
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
                        if let Some(stack) = clip.param_modifiers.get_mut(param) {
                            if let Some(m) = stack.modifiers.get_mut(*modifier_index) {
                                *m = modifier.clone();
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
            AppCommand::ToggleModifier { clip_id, ref param, modifier_index } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(stack) = clip.param_modifiers.get_mut(param) {
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
                        }
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
