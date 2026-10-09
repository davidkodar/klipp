//! Production inspector edits without a window or engine (M3 step 1c).
//! The live adapter forwards side effects inline; tests record the same
//! calls at the same points, without copying the command handlers.

use super::*;
use super::effects::{ClipEffectsState, effects_change_edits, fold_drag_into_add, is_add_of_effect};

pub(super) trait InspectorSideEffects {
    fn sync_sequence(&self, state: &AppState);
    fn send(&self, command: AppCommand);
}

pub(super) struct EngineSideEffects<'a> {
    pub cmd_tx: &'a klipp_engine::commands::EngineSender,
    pub sequence: &'a klipp_timeline::SharedSequence,
    pub param_change_queue: &'a ArrayQueue<ParamChange>,
}

impl InspectorSideEffects for EngineSideEffects<'_> {
    fn sync_sequence(&self, state: &AppState) {
        engine_sync::sync_sequence(state, self.sequence, self.param_change_queue);
    }

    fn send(&self, command: AppCommand) {
        let _ = self.cmd_tx.send(command);
    }
}

pub(super) struct InspectorEditHost<'a> {
    pub state: &'a mut AppState,
    pub side: &'a dyn InspectorSideEffects,
}

impl InspectorEditHost<'_> {
    /// A key drag's history (`BeginKeyframeDrag` / `EndKeyframeDrag`): the
    /// clip's keys at its start, one step at its end, as the timeline's
    /// lanes and the curve editor's clip lanes hold their drags. Moved here
    /// unchanged from the app's keyframe dispatch, which calls this.
    pub(super) fn apply_keyframe_drag(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
                AppCommand::BeginKeyframeDrag { ref key, clip_id } => {
                    // Capture keyframe snapshot at drag start for undo coalescing
                    let mut linked_partner_id = None;
                    let mut old_snap = None;
                    let mut old_linked_snap = None;
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id = c.linked_clip_id;
                            old_snap = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_snap = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    if let Some(old_state) = old_snap {
                        self.state.active_history_mut().begin_drag(
                            key.clone(),
                            HistoryEntry {
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id,
                                    description: "Move Keyframe",
                                    old_state: old_state.clone(),
                                    new_state: old_state, // placeholder, replaced at end_drag
                                    old_linked_state: old_linked_snap.clone(),
                                    new_linked_state: old_linked_snap,
                                },
                            },
                        );
                    }
                    true
                }
                AppCommand::EndKeyframeDrag { ref key, clip_id } => {
                    let _ = key;
                    // Read the old state from the pending entry
                    let (old_state, linked_clip_id, old_linked_state, description) = self.state.active_history().pending_old_entry_ref()
                        .and_then(|e| match &e.command {
                            EditCommand::EditKeyframes { old_state, linked_clip_id, old_linked_state, description, .. } =>
                                Some((old_state.clone(), *linked_clip_id, old_linked_state.clone(), *description)),
                            _ => None,
                        })
                        .unwrap_or_else(|| {
                            // Fallback: capture current state (no-op undo)
                            let snap = self.state.sequences[self.state.active_sequence_idx].all_clips()
                                .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip)
                                .unwrap_or_else(KeyframeSnapshot::empty);
                            (snap.clone(), None, None, "Move Keyframe")
                        });
                    // Capture current (post-drag) state
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let new_state = seq.all_clips().find(|c| c.id == *clip_id)
                        .map(KeyframeSnapshot::from_clip)
                        .unwrap_or_else(|| old_state.clone());
                    let new_linked_state = linked_clip_id.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                    self.state.active_history_mut().end_drag(HistoryEntry {
                        command: EditCommand::EditKeyframes {
                            clip_id: *clip_id,
                            linked_clip_id,
                            description,
                            old_state,
                            new_state,
                            old_linked_state,
                            new_linked_state,
                        },
                    });
                    true
                }
            _ => false,
        }
    }

    /// The production video keyframe handlers (a clip's opacity, placement
    /// and matte keys, their timing, bumps, Ken Burns and clearing),
    /// shared with the windowless drivers: the curve editor's Video tracks
    /// and the Video tab's diamonds send them. Moved here unchanged from the
    /// app's keyframe dispatch, which calls this.
    pub(super) fn apply_video_keyframes(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
                AppCommand::AddVideoKeyframe { clip_id, param, time_secs, value } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.edit_video_param_keyframes(*param, |keyframes| {
                                klipp_core::keyframe::upsert_keyframe(keyframes, *time_secs, *value);
                            });
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
                                    description: "Add Video Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::SetVideoKeyframeInterpolation { clip_id, param, keyframe_index, interpolation } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.edit_video_param_keyframes(*param, |keyframes| {
                                let ki = *keyframe_index;
                                let len = keyframes.len();
                                if ki < len {
                                    keyframes[ki].interpolation = *interpolation;
                                }
                                if ki > 0 {
                                    keyframes[ki - 1].interpolation = *interpolation;
                                }
                                if *interpolation == klipp_core::Interpolation::Bezier {
                                    for idx in [ki.saturating_sub(1), ki] {
                                        if idx >= len { continue; }
                                        let prev_t = if idx > 0 { keyframes[idx - 1].time_secs } else { keyframes[idx].time_secs - 1.0 };
                                        let next_t = if idx + 1 < len { keyframes[idx + 1].time_secs } else { keyframes[idx].time_secs + 1.0 };
                                        let span_out = (next_t - keyframes[idx].time_secs).abs() * 0.33;
                                        let span_in = (keyframes[idx].time_secs - prev_t).abs() * 0.33;
                                        let kf = &mut keyframes[idx];
                                        if kf.tangent_out == (0.0, 0.0) || kf.tangent_out.0.abs() < 0.5 {
                                            kf.tangent_out = (span_out, 0.0);
                                        }
                                        if kf.tangent_in == (0.0, 0.0) || kf.tangent_in.0.abs() < 0.5 {
                                            kf.tangent_in = (-span_in, 0.0);
                                        }
                                    }
                                }
                            });
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
                                    description: "Set Keyframe Interpolation",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::SetVideoKeyframeTangent { clip_id, param, keyframe_index, tangent_in, tangent_out } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.edit_video_param_keyframes(*param, |keyframes| {
                                if let Some(kf) = keyframes.get_mut(*keyframe_index) {
                                    if let Some(ti) = tangent_in {
                                        kf.tangent_in = *ti;
                                    }
                                    if let Some(to) = tangent_out {
                                        kf.tangent_out = *to;
                                    }
                                }
                            });
                            break;
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::RemoveVideoKeyframe { clip_id, param, time_secs } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.edit_video_param_keyframes(*param, |keyframes| {
                                klipp_core::keyframe::remove_keyframe_at(keyframes, *time_secs);
                            });
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
                                    description: "Remove Video Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::MoveVideoKeyframe { clip_id, param, index, new_time_secs, new_value } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.edit_video_param_keyframes(*param, |keyframes| {
                                if let Some(kf) = keyframes.get_mut(*index) {
                                    kf.time_secs = *new_time_secs;
                                    kf.value = *new_value;
                                }
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            });
                            break;
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::ScaleVideoKeyframeTiming { clip_id, param, factor } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let clip_dur = clip.duration().to_secs_f64();
                            clip.edit_video_param_keyframes(*param, |keyframes| {
                                for kf in keyframes.iter_mut() {
                                    kf.time_secs *= factor;
                                    kf.tangent_in.0 *= *factor;
                                    kf.tangent_out.0 *= *factor;
                                }
                                keyframes.retain(|kf| kf.time_secs >= 0.0 && kf.time_secs <= clip_dur);
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            });
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
                                    description: "Scale Video Keyframe Timing",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::InsertVideoKeyframeBump { clip_id, param, center_time, bump_value, base_value, duration_secs, ramp_secs } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    let bump_kfs: Vec<(f64, f32)> = vec![
                        (center_time - duration_secs / 2.0 - ramp_secs, *base_value),
                        (center_time - duration_secs / 2.0, *bump_value),
                        (center_time + duration_secs / 2.0, *bump_value),
                        (center_time + duration_secs / 2.0 + ramp_secs, *base_value),
                    ];
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let clip_dur = clip.duration().to_secs_f64();
                            clip.edit_video_param_keyframes(*param, |keyframes| {
                                for &(t, v) in &bump_kfs {
                                    if t >= 0.0 && t <= clip_dur {
                                        keyframes.push(klipp_core::Keyframe {
                                            time_secs: t,
                                            value: v,
                                            interpolation: klipp_core::Interpolation::Linear,
                                            tangent_out: (0.0, 0.0),
                                            tangent_in: (0.0, 0.0),
                                        });
                                    }
                                }
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            });
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
                                    description: "Insert Video Keyframe Bump",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SetPlayhead(self.state.playhead));
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::ApplyKenBurns { clip_id, start_scale, end_scale, start_pos_x, start_pos_y, end_pos_x, end_pos_y } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    // The keys are the placement's own, and a clip always has
                    // a placement: no Transform to add (M3 step 13).
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let duration = clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64();
                            let ease_t = 0.42 * duration;

                            // Clear existing transform keyframes
                            clip.edit_transform_scale_keyframes(|v| v.clear());
                            clip.edit_transform_pos_x_keyframes(|v| v.clear());
                            clip.edit_transform_pos_y_keyframes(|v| v.clear());

                            // Scale keyframes
                            clip.edit_transform_scale_keyframes(|v| v.push(klipp_core::Keyframe {
                                time_secs: 0.0, value: *start_scale,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (ease_t, 0.0), tangent_in: (0.0, 0.0),
                            }));
                            clip.edit_transform_scale_keyframes(|v| v.push(klipp_core::Keyframe {
                                time_secs: duration, value: *end_scale,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (0.0, 0.0), tangent_in: (-ease_t, 0.0),
                            }));

                            // Position X keyframes
                            clip.edit_transform_pos_x_keyframes(|v| v.push(klipp_core::Keyframe {
                                time_secs: 0.0, value: *start_pos_x,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (ease_t, 0.0), tangent_in: (0.0, 0.0),
                            }));
                            clip.edit_transform_pos_x_keyframes(|v| v.push(klipp_core::Keyframe {
                                time_secs: duration, value: *end_pos_x,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (0.0, 0.0), tangent_in: (-ease_t, 0.0),
                            }));

                            // Position Y keyframes
                            clip.edit_transform_pos_y_keyframes(|v| v.push(klipp_core::Keyframe {
                                time_secs: 0.0, value: *start_pos_y,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (ease_t, 0.0), tangent_in: (0.0, 0.0),
                            }));
                            clip.edit_transform_pos_y_keyframes(|v| v.push(klipp_core::Keyframe {
                                time_secs: duration, value: *end_pos_y,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (0.0, 0.0), tangent_in: (-ease_t, 0.0),
                            }));
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
                                    description: "Apply Ken Burns",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::ClearTransformKeyframes { clip_id } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.edit_transform_scale_keyframes(|v| v.clear());
                            clip.edit_transform_pos_x_keyframes(|v| v.clear());
                            clip.edit_transform_pos_y_keyframes(|v| v.clear());
                            clip.edit_transform_rotation_keyframes(|v| v.clear());
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
                                    description: "Clear Transform Keyframes",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::ClearVideoParamKeyframes { clip_id, param } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            match param {
                                klipp_core::VideoParam::Opacity => clip.edit_opacity_keyframes(|v| v.clear()),
                                klipp_core::VideoParam::PositionX => clip.edit_transform_pos_x_keyframes(|v| v.clear()),
                                klipp_core::VideoParam::PositionY => clip.edit_transform_pos_y_keyframes(|v| v.clear()),
                                klipp_core::VideoParam::Scale => clip.edit_transform_scale_keyframes(|v| v.clear()),
                                klipp_core::VideoParam::Rotation => clip.edit_transform_rotation_keyframes(|v| v.clear()),
                                klipp_core::VideoParam::TiltX => clip.edit_transform_tilt_x_keyframes(|v| v.clear()),
                                klipp_core::VideoParam::TiltY => clip.edit_transform_tilt_y_keyframes(|v| v.clear()),
                                klipp_core::VideoParam::MatteFeather => clip.matte_feather_keyframes.clear(),
                                klipp_core::VideoParam::MatteChoke => clip.matte_choke_keyframes.clear(),
                            }
                            break;
                        }
                    }
                    true
                }
                AppCommand::CopyVideoKeyframes { clip_id, param } => {
                    if let Some(clip) = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id)
                    {
                        let kfs = clip.video_param_keyframes(*param);
                        self.state.keyframe_clipboard = Some(crate::state::KeyframeClipboard {
                            keyframes: kfs.to_vec(),
                            source_duration_secs: clip.duration().to_secs_f64(),
                        });
                    }
                    true
                }
                AppCommand::PasteVideoKeyframes { clip_id, param } => {
                    if let Some(ref cb) = self.state.keyframe_clipboard.clone() {
                        let old_state = self.state.sequences[self.state.active_sequence_idx]
                            .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
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
                            for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                                if let Some(clip) = track.find_clip_mut(*clip_id) {
                                    clip.edit_video_param_keyframes(*param, |target| *target = new_kfs);
                                    break;
                                }
                            }
                            if let Some(old_snap) = old_state {
                                let new_snap = self.state.sequences[self.state.active_sequence_idx]
                                    .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                                if let Some(new_snap) = new_snap {
                                    self.state.push_history(EditCommand::EditKeyframes {
                                            clip_id: *clip_id,
                                            linked_clip_id: None,
                                            description: "Paste Video Keyframes",
                                            old_state: old_snap,
                                            new_state: new_snap,
                                            old_linked_state: None,
                                            new_linked_state: None,
                                        });
                                }
                            }
                            self.side.sync_sequence(self.state);
                            self.side.send(AppCommand::SequenceUpdated);
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::ReverseVideoKeyframes { clip_id, param } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.edit_video_param_keyframes(*param, |keyframes| {
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
                            });
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        let new_snap = self.state.sequences[self.state.active_sequence_idx]
                            .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        if let Some(new_snap) = new_snap {
                            self.state.push_history(EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Reverse Video Keyframes",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
            _ => false,
        }
    }

    /// The production builtin-effect key handlers, shared with the
    /// windowless control driver. Audio and Video lanes keep their own
    /// dispatch; this extraction preserves the live history and side effects.
    pub(super) fn apply_effect_keyframes(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
                AppCommand::AddEffectKeyframe { clip_id, effect_index, param_name, time_secs, value } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            // An index past the list names no effect: no track
                            // is started there.
                            if let Some(id) = clip.effect_id_at(*effect_index) {
                                clip.edit_effect_keyframes_of(id, param_name, |keyframes| {
                                    klipp_core::keyframe::upsert_keyframe(keyframes, *time_secs, *value);
                                });
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
                                    description: "Add Effect Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::RemoveEffectKeyframe { clip_id, effect_index, param_name, time_secs } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(id) = clip.effect_id_at(*effect_index) {
                                let emptied = clip.edit_existing_effect_keyframes(id, param_name, |keyframes| {
                                    klipp_core::keyframe::remove_keyframe_at(keyframes, *time_secs);
                                    keyframes.is_empty()
                                });
                                if emptied == Some(true) {
                                    clip.remove_effect_keyframes(id, param_name);
                                }
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
                                    description: "Remove Effect Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::MoveEffectKeyframe { clip_id, effect_index, param_name, keyframe_index, new_time_secs, new_value } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(id) = clip.effect_id_at(*effect_index) {
                                clip.edit_existing_effect_keyframes(id, param_name, |keyframes| {
                                    if let Some(kf) = keyframes.get_mut(*keyframe_index) {
                                        kf.time_secs = *new_time_secs;
                                        kf.value = *new_value;
                                    }
                                    keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                                });
                            }
                            break;
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::SetEffectKeyframeInterpolation { clip_id, effect_index, param_name, keyframe_index, interpolation } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(id) = clip.effect_id_at(*effect_index) {
                                clip.edit_existing_effect_keyframes(id, param_name, |keyframes| {
                                    klipp_core::keyframe::set_interpolation_at(keyframes, *keyframe_index, *interpolation);
                                });
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
                                    description: "Set Keyframe Interpolation",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.side.sync_sequence(self.state);
                    self.side.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
            _ => false,
        }
    }

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
            self.side.send(AppCommand::SequenceUpdated);
        }
    }

    /// The clip's picture fields now, for a drag's ends.
    pub(super) fn picture_now(&self, clip_id: klipp_core::ClipId) -> Option<klipp_timeline::ClipPictureFields> {
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
    pub(super) fn close_open_effect_drag(&mut self) {
        self.state.close_open_drag();
    }

    /// Apply the existing built-in effect commands. The app's dispatch
    /// boundary still closes unrelated path drags before this call.
    pub(super) fn apply(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            // Built-in effects
            AppCommand::AddBuiltinEffect { clip_id, ref effect } => {
                if self.effect_edit_by_ops(*clip_id, klipp_timeline::EffectEdit::Add(effect), "Add Effect", true).is_some() {
                    self.side.sync_sequence(self.state);
                    if self.state.playback != PlaybackState::Playing {
                        self.side.send(AppCommand::SequenceUpdated);
                    }
                    return true;
                }
                let before = self.clip_effects_state(*clip_id);
                let mut is_speed_ramp = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.push_effect(effect.clone());
                        // SpeedRamp: apply speed change on add
                        if let klipp_effects::BuiltinEffect::SpeedRamp { speed, .. } = effect {
                            is_speed_ramp = true;
                            let new_speed = (*speed as f64).clamp(0.1, 16.0);
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
                self.side.sync_sequence(self.state);
                if is_speed_ramp {
                    self.side.send(AppCommand::SeekTo(self.state.playhead));
                } else if self.state.playback != PlaybackState::Playing {
                    self.side.send(AppCommand::SequenceUpdated);
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
                if removed && by_ops.is_none() {
                    self.record_clip_replaced("Remove Effect", before);
                }
                self.side.sync_sequence(self.state);
                if was_speed_ramp {
                    self.side.send(AppCommand::SeekTo(self.state.playhead));
                } else if self.state.playback != PlaybackState::Playing {
                    self.side.send(AppCommand::SequenceUpdated);
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
                        self.side.sync_sequence(self.state);
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
                self.side.sync_sequence(self.state);
                self.refresh_paused_frame();
                true
            }
            AppCommand::UpdateBuiltinEffect { clip_id, index, ref effect } => {
                // A drag's steps record nothing; its end records the drag.
                let drag_key = format!("{:?}:builtin:{}", clip_id, index);
                let history = self.state.active_history();
                let dragging = history.is_dragging(&drag_key) || history.is_dragging_builtin_effect(*clip_id, *index) || history.is_dragging_picture(*clip_id);
                if self.effect_edit_by_ops(*clip_id, klipp_timeline::EffectEdit::Update(*index, effect), "Edit Effect", !dragging).is_some() {
                    self.side.sync_sequence(self.state);
                    if self.state.playback != PlaybackState::Playing {
                        self.side.send(AppCommand::SequenceUpdated);
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
                        if let klipp_effects::BuiltinEffect::SpeedRamp { speed, .. } = effect {
                            is_speed_ramp = true;
                            let new_speed = (*speed as f64).clamp(0.1, 16.0);
                            clip.speed = new_speed;
                            // Adjust timeline_out exactly like SetClipSpeed
                            let source_dur_secs = clip.source_duration().to_secs_f64();
                            let new_dur_secs = source_dur_secs / new_speed;
                            let new_duration = klipp_core::RationalTime::from_secs_f64(new_dur_secs, clip.timeline_in.rate);
                            clip.timeline_out = clip.timeline_in + new_duration;
                            tracing::debug!("SpeedRamp: speed={} → clip.speed={} dur={}s", speed, new_speed, new_dur_secs);
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
                self.side.sync_sequence(self.state);
                if is_speed_ramp {
                    // Force engine to restart decoder at new speed
                    self.side.send(AppCommand::SeekTo(self.state.playhead));
                } else if self.state.playback != PlaybackState::Playing {
                    self.side.send(AppCommand::SequenceUpdated);
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
                    self.side.sync_sequence(self.state);
                    if self.state.playback != PlaybackState::Playing {
                        self.side.send(AppCommand::SequenceUpdated);
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
                    self.side.sync_sequence(self.state);
                    if self.state.playback != PlaybackState::Playing {
                        self.side.send(AppCommand::SequenceUpdated);
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
            AppCommand::ReorderBuiltinEffect { clip_id, from_index, to_index } => {
                match self.effect_edit_by_ops(*clip_id, klipp_timeline::EffectEdit::Move { from: *from_index, to: *to_index }, "Reorder Effects", true) {
                    Some(true) => {
                        self.side.sync_sequence(self.state);
                        if self.state.playback != PlaybackState::Playing {
                            self.side.send(AppCommand::SeekTo(self.state.playhead));
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
                self.side.sync_sequence(self.state);
                if self.state.playback != PlaybackState::Playing {
                    self.side.send(AppCommand::SeekTo(self.state.playhead));
                }
                true
            }

            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use klipp_effects::BuiltinEffect as E;

    #[derive(Debug)]
    #[allow(clippy::large_enum_variant)] // a test's record, kept as it reads
    enum Side {
        Sync { picture: klipp_timeline::LiftedPicture, speed: f64, end: klipp_core::RationalTime },
        Command(AppCommand),
    }

    #[derive(Default)]
    struct Recorder(RefCell<Vec<Side>>);
    impl InspectorSideEffects for Recorder {
        fn sync_sequence(&self, state: &AppState) {
            let clip = state.sequences[state.active_sequence_idx].all_clips().next().unwrap();
            self.0.borrow_mut().push(Side::Sync { picture: (*clip.picture_v2().unwrap()).clone(), speed: clip.speed, end: clip.timeline_out });
        }
        fn send(&self, command: AppCommand) { self.0.borrow_mut().push(Side::Command(command)); }
    }

    fn fixture(effects: &[E]) -> (AppState, klipp_core::ClipId) {
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        for effect in effects { clip.push_effect(effect.clone()); }
        let id = clip.id;
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips.push(clip);
        (state, id)
    }
    fn clip(state: &AppState) -> &klipp_timeline::Clip { state.sequences[0].all_clips().next().unwrap() }
    fn run(state: &mut AppState, side: &dyn InspectorSideEffects, command: AppCommand) {
        state.before_dispatch(&command);
        assert!(InspectorEditHost { state, side }.apply(&command));
    }
    fn history(state: &mut AppState, forward: bool) {
        let command = if forward { state.active_history_mut().redo() } else { state.active_history_mut().undo() }.unwrap().command;
        let mut doc = klipp_document::apply::DocumentView {
            sequences: &mut state.sequences, active: state.active_sequence_idx,
            media_pool: &mut state.media_pool, fps: &mut state.fps,
        };
        if forward { doc.apply_forward(&command); } else { doc.apply_inverse(&command); }
    }

    #[test]
    fn the_production_effect_drag_is_one_exact_undo_step_without_a_window() {
        let (mut state, id) = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        let before = clip(&state).picture_v2().unwrap();
        let count = state.active_history().undo_count();
        let side = Recorder::default();
        let key = format!("{id:?}:builtin:0");
        run(&mut state, &side, AppCommand::BeginEffectDrag { key: key.clone(), clip_id: id, effect_index: 0, old_effect: E::GaussianBlur { radius: 3.0 } });
        for radius in [4.0, 5.0, 6.0] {
            run(&mut state, &side, AppCommand::UpdateBuiltinEffect { clip_id: id, index: 0, effect: E::GaussianBlur { radius } });
            assert_eq!(state.active_history().undo_count(), count, "drag ticks create no separate undo steps");
        }
        run(&mut state, &side, AppCommand::EndEffectDrag { key, clip_id: id, effect_index: 0, new_effect: E::GaussianBlur { radius: 6.0 } });
        assert_eq!(state.active_history().undo_count(), count + 1);
        let after = clip(&state).picture_v2().unwrap();
        assert_ne!(after, before);
        assert_eq!(clip(&state).builtin_effects(), &[E::GaussianBlur { radius: 6.0 }]);
        let events = side.0.borrow();
        assert_eq!(events.len(), 6);
        for pair in events.chunks_exact(2) {
            assert!(matches!(&pair[0], Side::Sync { .. }));
            assert!(matches!(&pair[1], Side::Command(AppCommand::SequenceUpdated)));
        }
        assert!(matches!(&events[4], Side::Sync { picture, .. } if picture == after.as_ref()));
        history(&mut state, false);
        assert_eq!(clip(&state).picture_v2().unwrap(), before);
        history(&mut state, true);
        assert_eq!(clip(&state).picture_v2().unwrap(), after);
    }

    #[test]
    fn speed_ramp_add_and_remove_keep_length_history_and_seek_order() {
        let (mut state, id) = fixture(&[]);
        let original = clip(&state).clone();
        let side = Recorder::default();
        let ramp = E::SpeedRamp { speed: 2.0, blend_mode: 0 };
        run(&mut state, &side, AppCommand::AddBuiltinEffect { clip_id: id, effect: ramp });
        assert_eq!(state.active_history().undo_count(), 1);
        assert_eq!(clip(&state).speed, 2.0);
        assert_eq!(clip(&state).timeline_out.to_secs_f64(), 2.0);
        let with_ramp = clip(&state).clone();
        {
            let events = side.0.borrow();
            assert!(matches!(events.as_slice(), [Side::Sync { speed: 2.0, end, .. }, Side::Command(AppCommand::SeekTo(_))] if end.to_secs_f64() == 2.0));
        }
        run(&mut state, &side, AppCommand::RemoveBuiltinEffect { clip_id: id, index: 0 });
        assert_eq!(state.active_history().undo_count(), 2);
        assert_eq!(clip(&state).speed, original.speed);
        assert_eq!(clip(&state).timeline_out, original.timeline_out);
        history(&mut state, false);
        assert_eq!(clip(&state).picture_v2().unwrap(), with_ramp.picture_v2().unwrap());
        assert_eq!((clip(&state).speed, clip(&state).timeline_out), (with_ramp.speed, with_ramp.timeline_out));
        history(&mut state, false);
        assert_eq!(clip(&state).picture_v2().unwrap(), original.picture_v2().unwrap());
        assert_eq!((clip(&state).speed, clip(&state).timeline_out), (original.speed, original.timeline_out));
    }

    #[test]
    fn live_adapter_updates_the_engine_mirror_and_clears_old_param_writes() {
        let (mut state, id) = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        let sequence = klipp_timeline::SharedSequence::new(state.sequences[0].clone());
        let queue = ArrayQueue::new(4);
        queue.push(ParamChange { clip_id: id, node_id: uuid::Uuid::nil(), param: "stale".into(), value: 1.0 }).unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        let sender = klipp_engine::commands::EngineSender::new(tx);
        let side = EngineSideEffects { cmd_tx: &sender, sequence: &sequence, param_change_queue: &queue };
        run(&mut state, &side, AppCommand::UpdateBuiltinEffect { clip_id: id, index: 0, effect: E::GaussianBlur { radius: 8.0 } });
        assert!(queue.is_empty());
        let snapshot = sequence.snapshot();
        let mirrored = snapshot.all_clips().find(|c| c.id == id).unwrap();
        assert_eq!(mirrored.picture_v2().unwrap(), clip(&state).picture_v2().unwrap());
        assert!(matches!(rx.try_recv(), Ok(AppCommand::SequenceUpdated)));
        assert!(rx.try_recv().is_err());
        assert!(!InspectorEditHost { state: &mut state, side: &side }.apply(&AppCommand::Play));
        assert!(rx.try_recv().is_err(), "unhandled commands produce no side effects");
    }
}
