//! Keyframes: video, effect and speed keyframes added, moved and taken off,
//! and their drags as one undo step (#826).

use super::*;

impl KlippApp {
    pub(super) fn handle_keyframe_commands(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        self.handle_keyframe_drag_commands(cmd, needs_refresh)
            || self.handle_speed_keyframe_commands(cmd, needs_refresh)
            || self.handle_effect_keyframe_commands(cmd, needs_refresh)
            || self.handle_audio_commands(cmd, needs_refresh)
            || self.handle_video_keyframe_commands(cmd, needs_refresh)
    }

    fn handle_video_keyframe_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        let side = inspector_edit_host::EngineSideEffects {
            cmd_tx: &self.cmd_tx,
            sequence: &self.sequence,
            param_change_queue: &self.param_change_queue,
        };
        (inspector_edit_host::InspectorEditHost { state: &mut self.state, side: &side }).apply_video_keyframes(cmd)
    }

    fn handle_effect_keyframe_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        let side = inspector_edit_host::EngineSideEffects {
            cmd_tx: &self.cmd_tx,
            sequence: &self.sequence,
            param_change_queue: &self.param_change_queue,
        };
        if (inspector_edit_host::InspectorEditHost { state: &mut self.state, side: &side }).apply_effect_keyframes(cmd) {
            return true;
        }
        match cmd {
                AppCommand::AddAudioEffectKeyframe { clip_id, effect_index, param_name, time_secs, value } => {
                    let key = format!("{}:{}", effect_index, param_name);
                    // Same drag key the BeginAudioChainDrag /
                    // EndAudioChainDrag pair uses — when a slider drag
                    // is active, the auto-update path
                    // (`dispatch_audio_kf_response`) fires
                    // AddAudioEffectKeyframe per drag tick. Without
                    // this is_dragging gate every tick was pushing a
                    // separate EditKeyframes entry, replicating the
                    // 60 Hz "Auto saved" spam on the kf'd-slider path
                    // (the static-slider path is already gated via
                    // UpdateAudioEffectParam). The Begin/End pair
                    // already snapshots the full clip state including
                    // audio_effect_keyframes, so undo rolls back the
                    // entire drag in one step.
                    let drag_key = format!("audio_chain:{}:{}:{}", clip_id.0, effect_index, param_name);
                    let is_dragging = self.state.active_history().is_dragging(&drag_key);
                    let old_state = if !is_dragging {
                        self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip)
                    } else {
                        None
                    };
                    // `__bypass` defaults to Step interpolation (it's an
                    // on/off toggle); other params default to Linear.
                    let default_interp = if param_name == "__bypass" {
                        klipp_core::Interpolation::Step
                    } else {
                        klipp_core::Interpolation::Linear
                    };
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = clip.audio_effect_keyframes.entry(key.clone()).or_default();
                            klipp_core::keyframe::upsert_keyframe_with_interpolation(keyframes, *time_secs, *value, default_interp);
                            break;
                        }
                    }
                    if !is_dragging {
                        if let Some(old_snap) = old_state {
                            let new_snap = self.state.sequences[self.state.active_sequence_idx].all_clips()
                                .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                            if let Some(new_snap) = new_snap {
                                self.state.push_history(EditCommand::EditKeyframes {
                                        clip_id: *clip_id,
                                        linked_clip_id: None,
                                        description: "Add Audio Effect Keyframe",
                                        old_state: old_snap,
                                        new_state: new_snap,
                                        old_linked_state: None,
                                        new_linked_state: None,
                                    });
                            }
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::RemoveAudioEffectKeyframe { clip_id, effect_index, param_name, time_secs } => {
                    let key = format!("{}:{}", effect_index, param_name);
                    // See AddAudioEffectKeyframe above for the
                    // is_dragging contract — diamond toggles fire this
                    // path during a drag too (when scrubby_param's
                    // auto-update detects the user re-crossed an
                    // existing kf at the playhead and the value is
                    // being removed in place). Skip the per-tick push
                    // and let the Begin/End wrap own the history.
                    let drag_key = format!("audio_chain:{}:{}:{}", clip_id.0, effect_index, param_name);
                    let is_dragging = self.state.active_history().is_dragging(&drag_key);
                    let old_state = if !is_dragging {
                        self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip)
                    } else {
                        None
                    };
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(keyframes) = clip.audio_effect_keyframes.get_mut(&key) {
                                klipp_core::keyframe::remove_keyframe_at(keyframes, *time_secs);
                                if keyframes.is_empty() {
                                    clip.audio_effect_keyframes.remove(&key);
                                }
                            }
                            break;
                        }
                    }
                    if !is_dragging {
                        if let Some(old_snap) = old_state {
                            let new_snap = self.state.sequences[self.state.active_sequence_idx].all_clips()
                                .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                            if let Some(new_snap) = new_snap {
                                self.state.push_history(EditCommand::EditKeyframes {
                                        clip_id: *clip_id,
                                        linked_clip_id: None,
                                        description: "Remove Audio Effect Keyframe",
                                        old_state: old_snap,
                                        new_state: new_snap,
                                        old_linked_state: None,
                                        new_linked_state: None,
                                    });
                            }
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::MoveAudioEffectKeyframe { .. } => {
                    audio_effect_key_write(&mut self.state, cmd);
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::SetAudioEffectKeyframeInterpolation { clip_id, effect_index, param_name, keyframe_index, interpolation } => {
                    let key = format!("{}:{}", effect_index, param_name);
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(keyframes) = clip.audio_effect_keyframes.get_mut(&key) {
                                klipp_core::keyframe::set_interpolation_at(keyframes, *keyframe_index, *interpolation);
                            }
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        let new_snap = self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        if let Some(new_snap) = new_snap {
                            self.state.push_history(EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Set Audio Keyframe Interpolation",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::SetAudioEffectKeyframeTangent { .. } => {
                    audio_effect_key_write(&mut self.state, cmd);
                    // Tangent edits coalesce through the existing drag
                    // pipeline (BeginDrag / EndKeyframeDrag) so no
                    // separate history push here — matches the
                    // SetAudioKeyframeTangent contract.
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::CopyAudioEffectKeyframes { clip_id, effect_index, param_name } => {
                    if let Some(clip) = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id)
                    {
                        let key = format!("{effect_index}:{param_name}");
                        let kfs = clip.audio_effect_keyframes.get(&key).cloned().unwrap_or_default();
                        self.state.keyframe_clipboard = Some(crate::state::KeyframeClipboard {
                            keyframes: kfs,
                            source_duration_secs: clip.duration().to_secs_f64(),
                        });
                    }
                    true
                }
                AppCommand::PasteAudioEffectKeyframes { clip_id, effect_index, param_name } => {
                    if let Some(ref cb) = self.state.keyframe_clipboard.clone() {
                        let old_state = self.state.sequences[self.state.active_sequence_idx]
                            .all_clips().find(|c| c.id == *clip_id)
                            .map(crate::history::AudioChainSnapshot::from_clip);
                        let target_dur = self.state.sequences[self.state.active_sequence_idx]
                            .all_clips().find(|c| c.id == *clip_id).map(|c| c.duration().to_secs_f64());
                        if let Some(target_dur) = target_dur {
                            let scale = if cb.source_duration_secs > 0.0 { target_dur / cb.source_duration_secs } else { 1.0 };
                            let mut new_kfs: Vec<klipp_core::Keyframe> = cb.keyframes.iter().map(|kf| {
                                let mut k = kf.clone();
                                k.time_secs *= scale;
                                k
                            }).collect();
                            new_kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            let key = format!("{effect_index}:{param_name}");
                            for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                                if let Some(clip) = track.find_clip_mut(*clip_id) {
                                    clip.audio_effect_keyframes.insert(key.clone(), new_kfs);
                                    break;
                                }
                            }
                            self.push_audio_chain_history(*clip_id, "Paste Audio Effect Keyframes", old_state);
                            self.sync_sequence_to_engine();
                            let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::ReverseAudioEffectKeyframes { clip_id, effect_index, param_name } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id)
                        .map(crate::history::AudioChainSnapshot::from_clip);
                    let key = format!("{effect_index}:{param_name}");
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(keyframes) = clip.audio_effect_keyframes.get_mut(&key) {
                                if keyframes.len() >= 2 {
                                    let max_time = keyframes.last().map(|k| k.time_secs).unwrap_or(0.0);
                                    for kf in keyframes.iter_mut() {
                                        kf.time_secs = max_time - kf.time_secs;
                                        let old_in = kf.tangent_in;
                                        let old_out = kf.tangent_out;
                                        kf.tangent_in = (-old_out.0, old_out.1);
                                        kf.tangent_out = (-old_in.0, old_in.1);
                                    }
                                    keyframes.reverse();
                                    keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                                }
                            }
                            break;
                        }
                    }
                    self.push_audio_chain_history(*clip_id, "Reverse Audio Effect Keyframes", old_state);
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::ScaleAudioEffectKeyframeTiming { clip_id, effect_index, param_name, factor } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id)
                        .map(crate::history::AudioChainSnapshot::from_clip);
                    let key = format!("{effect_index}:{param_name}");
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let clip_dur = clip.duration().to_secs_f64();
                            if let Some(keyframes) = clip.audio_effect_keyframes.get_mut(&key) {
                                for kf in keyframes.iter_mut() {
                                    kf.time_secs *= factor;
                                    kf.tangent_in.0 *= *factor;
                                    kf.tangent_out.0 *= *factor;
                                }
                                keyframes.retain(|kf| kf.time_secs >= 0.0 && kf.time_secs <= clip_dur);
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            }
                            break;
                        }
                    }
                    self.push_audio_chain_history(*clip_id, "Scale Audio Effect Keyframe Timing", old_state);
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::InsertAudioEffectKeyframeBump { clip_id, effect_index, param_name, center_time, bump_value, base_value, duration_secs, ramp_secs } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id)
                        .map(crate::history::AudioChainSnapshot::from_clip);
                    let key = format!("{effect_index}:{param_name}");
                    let bump_kfs: Vec<(f64, f32)> = vec![
                        (center_time - duration_secs / 2.0 - ramp_secs, *base_value),
                        (center_time - duration_secs / 2.0, *bump_value),
                        (center_time + duration_secs / 2.0, *bump_value),
                        (center_time + duration_secs / 2.0 + ramp_secs, *base_value),
                    ];
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let clip_dur = clip.duration().to_secs_f64();
                            let keyframes = clip.audio_effect_keyframes.entry(key.clone()).or_default();
                            for &(t, v) in &bump_kfs {
                                if t >= 0.0 && t <= clip_dur {
                                    keyframes.push(klipp_core::Keyframe {
                                        time_secs: t,
                                        value: v,
                                        interpolation: klipp_core::Interpolation::Linear,
                                        tangent_out: (0.0, 0.0),
                                        tangent_in: (0.0, 0.0),
                                        handles: Default::default(),
                                    });
                                }
                            }
                            keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            break;
                        }
                    }
                    self.push_audio_chain_history(*clip_id, "Insert Audio Effect Keyframe Bump", old_state);
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
            _ => false,
        }
    }

    fn handle_speed_keyframe_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                AppCommand::SetClipSpeed { clip_id, speed, ripple } => {
                    // A negative speed plays the clip backward (#617). The
                    // magnitude is clamped to [0.1, 16.0] — zero would stall
                    // playback and keeps timeline duration math well-defined.
                    let speed = if *speed < 0.0 {
                        -(speed.abs().clamp(0.1, 16.0))
                    } else {
                        speed.clamp(0.1, 16.0)
                    };
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let Some(clip) = seq.all_clips().find(|c| c.id == *clip_id) else { return true };
                    // Reverse is a media clip's: a compound or a title has
                    // no file of its own to read backward.
                    if speed < 0.0 && !matches!(clip.kind, klipp_timeline::ClipKind::Media { .. }) {
                        self.state.toast.show("Only a clip of a media file can play in reverse".to_string());
                        return true;
                    }
                    // As the retime op does it: the linked sound with it, and
                    // the clips after them rippled, or a refusal rather than
                    // a clip laid over the next one (#639).
                    match klipp_document::ops::retime_at(seq, &self.state.media_pool, *clip_id, speed, *ripple) {
                        Ok((what, edit)) => {
                            tracing::info!("{what}");
                            self.apply_edits_as_one(&[edit], "Clip Speed");
                        }
                        Err(r) => self.state.toast.show(format!("Speed unchanged: {}. {}", r.why, r.hint)),
                    }
                    true
                }
                AppCommand::SetClipInterpolation { clip_id, interpolation } => {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    match klipp_document::ops::interpolation_at(seq, &self.state.media_pool, *clip_id, interpolation) {
                        Ok(Some((what, edit))) => {
                            tracing::info!("{what}");
                            self.apply_edits_as_one(&[edit], "In-between Frames");
                        }
                        Ok(None) => {}
                        Err(r) => self.state.toast.show(format!("In-between frames unchanged: {}. {}", r.why, r.hint)),
                    }
                    true
                }
                AppCommand::AddSpeedKeyframe { clip_id, time_secs, speed } => {
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some((ti, ci)) = seq.find_clip(*clip_id) {
                        let clip = &mut seq.tracks[ti].clips[ci];
                        clip.speed_keyframes.push(klipp_core::Keyframe {
                            time_secs: *time_secs,
                            value: *speed as f32,
                            interpolation: klipp_core::keyframe::Interpolation::Linear,
                            tangent_out: (0.0, 0.0),
                            tangent_in: (0.0, 0.0),
                            handles: Default::default(),
                        });
                        clip.speed_keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                        recalc_timeline_out_from_speed(clip);
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    true
                }
                AppCommand::RemoveSpeedKeyframe { clip_id, index } => {
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some((ti, ci)) = seq.find_clip(*clip_id) {
                        let clip = &mut seq.tracks[ti].clips[ci];
                        if *index < clip.speed_keyframes.len() {
                            clip.speed_keyframes.remove(*index);
                        }
                        recalc_timeline_out_from_speed(clip);
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    true
                }
                AppCommand::UpdateSpeedKeyframe { clip_id, index, time_secs, speed } => {
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some((ti, ci)) = seq.find_clip(*clip_id) {
                        let clip = &mut seq.tracks[ti].clips[ci];
                        if *index < clip.speed_keyframes.len() {
                            clip.speed_keyframes[*index].time_secs = *time_secs;
                            clip.speed_keyframes[*index].value = *speed as f32;
                            clip.speed_keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                        }
                        recalc_timeline_out_from_speed(clip);
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    true
                }
                AppCommand::SetSpeedKeyframes { clip_id, keyframes } => {
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some((ti, ci)) = seq.find_clip(*clip_id) {
                        let clip = &mut seq.tracks[ti].clips[ci];
                        clip.speed_keyframes = keyframes.clone();
                        recalc_timeline_out_from_speed(clip);
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    true
                }
                AppCommand::BeginSpeedKeyframeDrag { ref key, clip_id } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    if let Some(old_state) = old_state {
                        self.state.active_history_mut().begin_drag(
                            key.clone(),
                            HistoryEntry {
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Move Speed Keyframe",
                                    old_state: old_state.clone(),
                                    new_state: old_state,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            },
                        );
                    }
                    true
                }
                AppCommand::EndSpeedKeyframeDrag { ref key, clip_id } => {
                    let new_state = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    if let Some(new_state) = new_state {
                        self.state.active_history_mut().end_drag(HistoryEntry {
                            command: EditCommand::EditKeyframes {
                                clip_id: *clip_id,
                                linked_clip_id: None,
                                description: "Move Speed Keyframe",
                                old_state: new_state.clone(), // placeholder, combine_old_new merges
                                new_state,
                                old_linked_state: None,
                                new_linked_state: None,
                            },
                        });
                    }
                    let _ = key;
                    true
                }
            _ => false,
        }
    }

    fn handle_keyframe_drag_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        let side = inspector_edit_host::EngineSideEffects { cmd_tx: &self.cmd_tx, sequence: &self.sequence, param_change_queue: &self.param_change_queue };
        (inspector_edit_host::InspectorEditHost { state: &mut self.state, side: &side }).apply_keyframe_drag(cmd)
    }
}

/// Recompute `clip.timeline_out` so that its visible length on the timeline
/// matches the time it takes the speed curve to exhaust the clip's source range.
/// Uses the constant `clip.speed` when there are no keyframes; integrates the
/// curve otherwise. Called from the speed-keyframe command handlers so the UI
/// and engine see a consistent clip length after a ramp edit.
pub(super) fn recalc_timeline_out_from_speed(clip: &mut klipp_timeline::Clip) {
    let source_dur_secs = clip.source_duration().to_secs_f64();
    if source_dur_secs <= 0.0 {
        return;
    }
    let rate = clip.timeline_in.rate;
    let new_dur_secs = klipp_timeline::effective_timeline_duration_for_keyframes(
        &clip.speed_keyframes,
        source_dur_secs,
        rate as f64,
        clip.speed,
    );
    let new_duration = klipp_core::RationalTime::from_secs_f64(new_dur_secs, rate);
    clip.timeline_out = clip.timeline_in + new_duration;
}

/// A sound effect key moved, or its tangents set, on the clip the command
/// names: the keys' write alone, which records no step (the drag that
/// holds it does). Shared by the app's handlers and the tests that drive
/// the timeline and the curve editor (#1990).
pub(super) fn audio_effect_key_write(state: &mut crate::state::AppState, cmd: &AppCommand) -> bool {
    let (clip_id, effect_index, param_name, keyframe_index) = match cmd {
        AppCommand::MoveAudioEffectKeyframe { clip_id, effect_index, param_name, keyframe_index, .. }
        | AppCommand::SetAudioEffectKeyframeTangent { clip_id, effect_index, param_name, keyframe_index, .. } => (clip_id, effect_index, param_name, keyframe_index),
        _ => return false,
    };
    let key = format!("{}:{}", effect_index, param_name);
    for track in &mut state.sequences[state.active_sequence_idx].tracks {
        if let Some(clip) = track.find_clip_mut(*clip_id) {
            if let Some(keyframes) = clip.audio_effect_keyframes.get_mut(&key) {
                match cmd {
                    AppCommand::MoveAudioEffectKeyframe { new_time_secs, new_value, .. } => {
                        if let Some(kf) = keyframes.get_mut(*keyframe_index) {
                            kf.time_secs = *new_time_secs;
                            kf.value = *new_value;
                        }
                        keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                    }
                    AppCommand::SetAudioEffectKeyframeTangent { tangent_in, tangent_out, handles, .. } => {
                        klipp_core::keyframe::edit_handles(keyframes, *keyframe_index, *tangent_in, *tangent_out, *handles);
                    }
                    _ => {}
                }
            }
            return true;
        }
    }
    true
}
