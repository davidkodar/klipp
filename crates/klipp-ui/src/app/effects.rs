//! Effects: a clip's built-in and plugin effects added, changed and taken
//! off, each change one undo step (#826).

use super::*;

impl KlippApp {
    pub(super) fn handle_effects_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        let side = inspector_edit_host::EngineSideEffects {
            cmd_tx: &self.cmd_tx,
            sequence: &self.sequence,
            param_change_queue: &self.param_change_queue,
        };
        if (inspector_edit_host::InspectorEditHost { state: &mut self.state, side: &side }).apply(cmd) {
            return true;
        }
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
                // A plug-in's node is named by its place in the list, so the
                // ones after the removed one are renamed: a compare on one of
                // them follows it (M3 step 4; a built-in effect keeps its id).
                if let Some((cid, path)) = self.state.split_compare_node.clone() {
                    let ofx_node = |slot: usize| klipp_graph_lift::ids::entry_id(klipp_graph_lift::ids::RootKind::Clip, cid.0, "ofx", slot);
                    // Up to the list's length before the removal.
                    let before = self.state.sequences[self.state.active_sequence_idx].all_clips().find(|c| c.id == cid).map_or(0, |c| c.ofx_effects().len() + 1);
                    let slot = path.node().and_then(|n| (0..before).find(|k| ofx_node(*k) == n));
                    match slot {
                        // The compared plug-in itself: its slot's id now names
                        // the next one, so the compare ends here.
                        Some(k) if cid == *clip_id && k == *effect_index => {
                            self.state.split_compare_node = None;
                            let _ = self.cmd_tx.send(AppCommand::SetSplitCompareNode { clip_id: None, path: None });
                        }
                        Some(k) if cid == *clip_id && k > *effect_index => {
                            let moved = klipp_graph_format::path::NodePath::new(path.root, [ofx_node(k - 1)]);
                            self.state.split_compare_node = Some((cid, moved.clone()));
                            let _ = self.cmd_tx.send(AppCommand::SetSplitCompareNode { clip_id: Some(cid), path: Some(moved) });
                        }
                        _ => {}
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
    /// Each effect's id, so an undo or redo gives it back (#1373).
    pub(super) ids: Vec<klipp_timeline::EffectId>,
    pub(super) speed: f64,
    pub(super) timeline_out: klipp_core::RationalTime,
}

impl ClipEffectsState {
    pub(super) fn of(clip: &klipp_timeline::Clip) -> Self {
        Self {
            effects: clip.builtin_effects().to_vec(),
            enabled: clip.builtin_effects_enabled().to_vec(),
            ids: clip.effect_id_list(),
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
            old_ids: before.ids.clone(),
            new_ids: after.ids.clone(),
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

/// Drags whose release was never seen, closed on the state alone, so the
/// app's handlers and the path edits (M3 step 2) share one rule.
impl crate::state::AppState {
    /// The clip's picture fields now, on the active sequence.
    pub(crate) fn picture_of(&self, clip_id: klipp_core::ClipId) -> Option<klipp_timeline::ClipPictureFields> {
        self.sequences[self.active_sequence_idx].all_clips().find(|c| c.id == clip_id).map(|c| c.picture.clone())
    }

    /// Close an effect gesture whose release was never seen: as one
    /// undo entry from its "before" snapshot to the effect as it is
    /// now, or, when the effect has not moved (or the gesture is not
    /// an effect edit), by dropping it, so nothing bogus is pushed.
    pub(crate) fn close_open_drag(&mut self) {
        let pending = self.active_history().pending_old_entry_ref().map(|e| e.command.clone());
        // A drag on one of the sequence's own grades (M3 step 19): one step
        // from the grade the drag began on to the grade now.
        if let Some(EditCommand::SetRootGrade { description, root, old, .. }) = &pending {
            let new = self.sequences[self.active_sequence_idx].root_grade(*root).ok().flatten().cloned().map(Box::new);
            self.active_history_mut().end_drag(HistoryEntry { command: EditCommand::SetRootGrade { description, root: *root, old: old.clone(), new } });
            return;
        }
        if let Some(EditCommand::PictureEdit { clip_id, description, edit }) = &pending {
            match self.picture_of(*clip_id) {
                Some(after) => self.active_history_mut().end_drag(HistoryEntry {
                    command: EditCommand::PictureEdit { clip_id: *clip_id, description, edit: Box::new(klipp_timeline::PictureEdit { before: edit.before.clone(), after }) },
                }),
                None => self.active_history_mut().abandon_pending_drag(),
            }
            return;
        }
        let Some(EditCommand::EditBuiltinEffect { clip_id, effect_index, old_effect, .. }) = pending else {
            self.active_history_mut().abandon_pending_drag();
            return;
        };
        let now = self.sequences[self.active_sequence_idx]
            .all_clips()
            .find(|c| c.id == clip_id)
            .and_then(|c| c.builtin_effects().get(effect_index).cloned());
        match now {
            Some(new_effect) if new_effect != old_effect => {
                self.active_history_mut().end_drag(HistoryEntry {
                    command: EditCommand::EditBuiltinEffect { clip_id, effect_index, old_effect, new_effect },
                });
            }
            _ => self.active_history_mut().abandon_pending_drag(),
        }
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
        ClipEffectsState { effects, enabled, ids: Vec::new(), speed, timeline_out: klipp_core::RationalTime { value: 300, rate: 30 } }
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
/// The effect a `ParamId::Node` names on `clip`, and the name the effect
/// animates that key under (M3 step 2): its id is its node's, kept since M3
/// proposal 6. `None` when the node isn't one of this clip's effects.
fn effect_param_of(clip: &mut klipp_timeline::Clip, addr: &klipp_graph_format::path::NodePath, key: &klipp_graph_format::ParamKey) -> Option<(klipp_timeline::EffectId, String)> {
    if addr.root != (klipp_graph_format::path::Root::Clip { clip: clip.id.0 }) || addr.nodes.len() != 1 {
        return None;
    }
    let id = klipp_timeline::EffectId::of_node(addr.node()?);
    let len = clip.builtin_effects().len();
    let index = (0..len).find(|&i| clip.effect_id_at(i) == Some(id))?;
    let tag = serde_json::to_value(&clip.builtin_effects()[index]).ok()?.get("type")?.as_str()?.to_owned();
    Some((id, klipp_graph_lift::lift::keyframe_name(&tag, key.as_str()).to_owned()))
}

fn with_modifiers<R>(clip: &mut klipp_timeline::Clip, param: &klipp_core::ParamId, create: bool, f: impl FnOnce(&mut klipp_core::ModifierStack) -> R) -> Option<R> {
    match param {
        klipp_core::ParamId::Node { addr, key } => {
            let (id, name) = effect_param_of(clip, addr, key)?;
            if create {
                clip.edit_effect_modifiers(id, &name, f)
            } else {
                clip.edit_existing_effect_modifiers(id, &name, f)
            }
        }
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
        klipp_core::ParamId::Node { addr, key } => {
            if let Some((id, name)) = effect_param_of(clip, addr, key) {
                clip.remove_effect_modifiers(id, &name);
            }
        }
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

    /// A modifier added by node path lands on the stack the effect's index
    /// names (M3 step 2), and comes off by path too.
    #[test]
    fn a_modifier_by_node_path_is_the_effects() {
        let mut c = clip();
        c.push_effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 });
        let node = c.effect_id_at(0).unwrap().as_uuid();
        let by_path = ParamId::Node { addr: klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: c.id.0 }, [node]), key: "radius".into() };
        let by_index = ParamId::Effect { effect_index: 0, param_name: "radius".into() };
        with_modifiers(&mut c, &by_path, true, |s| s.modifiers.push(klipp_core::Modifier::Clamp { min: 0.0, max: 1.0, enabled: true }));
        assert_eq!(with_modifiers(&mut c, &by_index, false, |s| s.modifiers.len()), Some(1), "the effect's stack");
        remove_modifiers(&mut c, &by_path);
        assert_eq!(with_modifiers(&mut c, &by_index, false, |s| s.modifiers.len()), None, "gone by path");
        let elsewhere = ParamId::Node { addr: klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: uuid::Uuid::nil() }, [node]), key: "radius".into() };
        assert_eq!(with_modifiers(&mut c, &elsewhere, true, |s| s.modifiers.len()), None, "another clip's path names nothing here");
    }

}
