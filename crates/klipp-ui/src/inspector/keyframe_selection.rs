//! Frame-aware UI selection, separate from exact authored timestamp identity.
use klipp_core::{AppCommand, AudioParam, Keyframe, TitlePositionAxis};
use klipp_timeline::{Clip, Sequence, TitleObjectId};

/// Inspector and K operate on the same canonical playhead, not the smooth
/// pointer/playback position between displayed frames.
pub(super) fn clip_time(state: &crate::state::AppState, clip: &Clip) -> f64 {
    (state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0)
}

/// The canonical clip time and selection window travel together through all
/// mask inspector pages. Callers supply the canonical playhead, not the
/// smooth scrub position.
#[derive(Clone, Copy)]
pub(crate) struct FrameSelection {
    pub time_secs: f64,
    pub tolerance: f64,
    /// Where the clip starts on the timeline: a key's clip time from it is
    /// the timeline time a caret seeks to.
    pub clip_start: klipp_core::Timecode,
}

impl FrameSelection {
    pub fn new(playhead: klipp_core::Timecode, clip_start: klipp_core::Timecode, tolerance: f64) -> Self {
        Self { time_secs: (playhead.to_secs_f64() - clip_start.to_secs_f64()).max(0.0), tolerance, clip_start }
    }

    /// Seek to the clip's `clip_secs`.
    pub fn seek_to(&self, clip_secs: f64) -> klipp_core::AppCommand {
        klipp_core::AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(self.clip_start.to_secs_f64() + clip_secs, self.clip_start.rate))
    }
}

/// Half a displayed sequence frame; storage identity remains a separate rule.
pub(crate) fn frame_tolerance(sequence: &Sequence) -> f64 {
    0.5 / sequence.fps.as_f64()
}

/// Timeline clicks select an existing key in the displayed frame, or create
/// a frame-aligned key. Exact curve-editor operations do not call this.
pub(crate) fn timeline_key_time(keys: &[Keyframe], time: f64, fps: f64) -> f64 {
    let frame = (time * fps).round() / fps;
    nearest(keys, frame, 0.5 / fps).map_or(frame, |i| keys[i].time_secs)
}

fn in_window(key_time: f64, time: f64, tolerance: f64) -> bool {
    (key_time - time).abs() <= tolerance + klipp_core::keyframe::KEYFRAME_TIME_EPSILON
}

/// Pick the closest key within half a sequence frame. Equal distances prefer
/// the earlier entry in the sorted track. Never move the stored timestamp.
/// Endpoints are inclusive with rounding slack: a halfway key is selectable
/// from either adjacent frame, rather than unreachable from both.
pub(crate) fn nearest(keys: &[Keyframe], time: f64, tolerance: f64) -> Option<usize> {
    keys.iter()
        .enumerate()
        .filter(|(_, k)| in_window(k.time_secs, time, tolerance))
        .min_by(|(_, a), (_, b)| {
            (a.time_secs - time)
                .abs()
                .total_cmp(&(b.time_secs - time).abs())
        })
        .map(|(i, _)| i)
}

/// Selection and navigation share the displayed frame window. A rounded
/// off-grid key is current, so previous/next must not select it again.
pub(crate) fn state(
    keys: &[Keyframe],
    time: f64,
    tolerance: f64,
) -> crate::widgets::scrubby_param::KeyframeState {
    let current = nearest(keys, time, tolerance);
    crate::widgets::scrubby_param::KeyframeState {
        has_keyframe_at_playhead: current.is_some(),
        has_any_keyframes: !keys.is_empty(),
        keys_in_frame: keys.iter().filter(|k| in_window(k.time_secs, time, tolerance)).count(),
        current_interpolation: current.map(|i| keys[i].interpolation),
        prev_keyframe_time: keys
            .iter()
            .rfind(|k| k.time_secs < time && !in_window(k.time_secs, time, tolerance))
            .map(|k| k.time_secs),
        next_keyframe_time: keys
            .iter()
            .find(|k| k.time_secs > time && !in_window(k.time_secs, time, tolerance))
            .map(|k| k.time_secs),
    }
}

/// Inspector commands use a displayed frame, while the document mutation uses
/// precise timestamp identity. Resolve only inspector add/remove commands here;
/// agent-authored timestamps and curve moves retain their original precision.
pub(crate) fn resolve_command(sequence: &Sequence, command: &mut AppCommand) {
    resolve_with(command, frame_tolerance(sequence), |id| {
        sequence
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .find(|c| c.id == id)
    });
}

/// `command` resolved as [`resolve_command`] does, against `clip` alone:
/// for a view drawing one clip outside the inspector (the grade view on the
/// Colour page), with its frame's tolerance.
pub(crate) fn resolve_for_clip(clip: &Clip, tolerance: f64, command: &mut AppCommand) {
    resolve_with(command, tolerance, |id| (id == clip.id).then_some(clip));
}

/// `command` resolved as [`resolve_command`] does, for a key named by
/// path on `grade`, one of the sequence's own grades (the timeline grade, a
/// group stage: M3 step 19), whose node keys are its v1 nodes' tracks: a
/// key added or taken off within `tolerance` of a stored one is that key.
pub(crate) fn resolve_for_grade(grade: &klipp_nodes::NodeGraph, tolerance: f64, command: &mut AppCommand) {
    let keys = |addr: &klipp_graph_format::path::NodePath, key: &klipp_graph_format::ParamKey| -> Vec<Keyframe> {
        // At its path's depth: inside a compound too (#1391).
        let level = addr.nodes[..addr.nodes.len().saturating_sub(1)].iter().try_fold(grade, |g, id| {
            g.nodes.iter().find(|n| n.id == *id && n.kind == klipp_nodes::NodeKind::Compound)?.params.compound_subgraph.as_ref()
        });
        let node = addr.nodes.last().and_then(|id| level?.nodes.iter().find(|n| n.id == *id));
        node.and_then(|n| n.keyframes.tracks.iter().find(|t| t.param_name == key.as_str())).map(|t| t.keyframes.clone()).unwrap_or_default()
    };
    match command {
        AppCommand::AddKeyAt { addr, key, keyframe } => {
            let Some(t) = keyframe.get("time_secs").and_then(serde_json::Value::as_f64) else { return };
            let stored = keys(addr, key);
            if let Some(i) = nearest(&stored, t, tolerance) {
                keyframe["time_secs"] = serde_json::json!(stored[i].time_secs);
            }
        }
        AppCommand::RemoveKeyAt { addr, key, time_secs } | AppCommand::SetInterpKeyAt { addr, key, time_secs, .. } | AppCommand::EaseKeyAt { addr, key, time_secs, .. } => {
            let stored = keys(addr, key);
            if let Some(i) = nearest(&stored, *time_secs, tolerance) {
                *time_secs = stored[i].time_secs;
            }
        }
        _ => {}
    }
}

fn resolve_with<'a>(
    command: &mut AppCommand,
    tolerance: f64,
    clip_for: impl Fn(klipp_core::ClipId) -> Option<&'a Clip>,
) {
    use AppCommand::*;
    // A key added by path carries its time in its own record (M3 step 3).
    if let AddKeyAt { addr, key, keyframe } = command {
        let Some(c) = addr.root_clip().and_then(|id| clip_for(klipp_core::ClipId(id))) else { return };
        let (Some(state), Some(t)) = (c.node_key_state(addr, key), keyframe.get("time_secs").and_then(serde_json::Value::as_f64)) else { return };
        if let Some(i) = nearest(&state.keys, t, tolerance) {
            keyframe["time_secs"] = serde_json::json!(state.keys[i].time_secs);
        }
        return;
    }
    // A node's keys, read by path into this, for the arm that does.
    let node_keys: Vec<Keyframe>;
    // A placement or opacity lane's keys, read from the picture.
    let lane_keys: Vec<Keyframe>;
    let (keys, time): (&[Keyframe], &mut f64) = match command {
        AddVideoKeyframe {
            clip_id,
            param,
            time_secs,
            ..
        }
        | RemoveVideoKeyframe {
            clip_id,
            param,
            time_secs,
        } => {
            let Some(c) = clip_for(*clip_id) else { return };
            // The lane as the picture holds it (M4 step 12c).
            lane_keys = crate::inspector::placement_by_path::param_keys(c, *param);
            (&lane_keys, time_secs)
        }
        AddAudioKeyframe {
            clip_id,
            param,
            time_secs,
            ..
        }
        | RemoveAudioKeyframe {
            clip_id,
            param,
            time_secs,
        } => {
            let Some(c) = clip_for(*clip_id) else { return };
            (
                match param {
                    AudioParam::Volume => &c.volume_keyframes,
                    AudioParam::Pan => &c.pan_keyframes,
                    AudioParam::Gain => &c.gain_keyframes,
                },
                time_secs,
            )
        }
        AddEffectKeyframe {
            clip_id,
            effect_index,
            param_name,
            time_secs,
            ..
        }
        | RemoveEffectKeyframe {
            clip_id,
            effect_index,
            param_name,
            time_secs,
        } => {
            let Some(c) = clip_for(*clip_id) else { return };
            (c.effect_param_keyframes(*effect_index, param_name), time_secs)
        }
        // A key removed, or its interpolation set, by path: the node's
        // keys, by path and key.
        RemoveKeyAt { addr, key, time_secs } | SetInterpKeyAt { addr, key, time_secs, .. } | EaseKeyAt { addr, key, time_secs, .. } => {
            let Some(c) = addr.root_clip().and_then(|id| clip_for(klipp_core::ClipId(id))) else { return };
            node_keys = c.node_key_state(addr, key).map(|s| s.keys).unwrap_or_default();
            (&node_keys, time_secs)
        }
        AddAudioEffectKeyframe {
            clip_id,
            effect_index,
            param_name,
            time_secs,
            ..
        }
        | RemoveAudioEffectKeyframe {
            clip_id,
            effect_index,
            param_name,
            time_secs,
        } => {
            let Some(c) = clip_for(*clip_id) else { return };
            (
                c.audio_effect_keyframes
                    .get(&format!("{effect_index}:{param_name}"))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                time_secs,
            )
        }
        AddTitleObjectPositionKeyframe {
            clip_id,
            object_id,
            axis,
            time_secs,
            ..
        }
        | RemoveTitleObjectPositionKeyframe {
            clip_id,
            object_id,
            axis,
            time_secs,
        } => {
            let Some(o) = clip_for(*clip_id)
                .and_then(Clip::title_data)
                .and_then(|t| t.object(TitleObjectId(*object_id)))
            else {
                return;
            };
            (
                match axis {
                    TitlePositionAxis::X => &o.transform.center_x_keyframes,
                    TitlePositionAxis::Y => &o.transform.center_y_keyframes,
                },
                time_secs,
            )
        }
        AddTitleObjectEffectKeyframe {
            clip_id,
            object_id,
            param_key,
            time_secs,
            ..
        }
        | RemoveTitleObjectEffectKeyframe {
            clip_id,
            object_id,
            param_key,
            time_secs,
        } => {
            let Some(o) = clip_for(*clip_id)
                .and_then(Clip::title_data)
                .and_then(|t| t.object(TitleObjectId(*object_id)))
            else {
                return;
            };
            // The object's: the stack's params live on any layer, the
            // text's own on its text box, and the object knows both.
            (o.effect_keyframes_for(param_key).unwrap_or(&[]), time_secs)
        }
        _ => return,
    };
    if let Some(i) = nearest(keys, *time, tolerance) {
        *time = keys[i].time_secs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::VideoParam;
    use klipp_core::{Fps, Interpolation, Timecode};
    use klipp_timeline::{ClipKind, TitleData};

    fn keys(times: &[f64]) -> Vec<Keyframe> {
        times
            .iter()
            .map(|&time_secs| Keyframe {
                time_secs,
                value: 0.5,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0),
                tangent_out: (0.0, 0.0),
                handles: Default::default(),
            })
            .collect()
    }

    fn fixture() -> Sequence {
        let mut seq = Sequence::new_default();
        let mut c = Clip::gap(
            Timecode::from_secs_f64(0.0, 24),
            Timecode::from_secs_f64(5.0, 24),
        );
        c.edit_opacity_keyframes(|v| *v = keys(&[0.7, 1.0]));
        c.volume_keyframes = c.opacity_keyframes().to_vec();
        c.edit_effect_keyframes(|m| m.insert("0:radius".into(), keys(&[0.7, 1.0])));
        c.audio_effect_keyframes
            .insert("0:gain".into(), keys(&[0.7, 1.0]));
        let mut title = TitleData::default();
        title.objects[0].transform.center_x_keyframes = keys(&[0.7, 1.0]);
        title.objects[0].fx_mut().glows.push(klipp_timeline::TextGlow {
            radius_keyframes: keys(&[0.7, 1.0]),
            enabled: true,
            color: [255; 4],
            radius: 10.0,
            intensity: 0.8,
            intensity_keyframes: vec![],
        });
        c.kind = ClipKind::Title(title);
        seq.tracks[0].clips.push(c);
        seq
    }

    #[test]
    fn off_grid_inspector_edits_resolve_to_the_stored_key() {
        let seq = fixture();
        let c = &seq.tracks[0].clips[0];
        let time = Timecode::from_secs_f64(0.7, 24).to_secs_f64();
        assert!((time - 0.7).abs() > 0.001);
        let clip_id = c.id;
        let object_id = c.title_data().unwrap().objects[0].id.0;
        let mut commands = [
            AppCommand::AddVideoKeyframe {
                clip_id,
                param: VideoParam::Opacity,
                time_secs: time,
                value: 0.9,
            },
            AppCommand::RemoveAudioKeyframe {
                clip_id,
                param: AudioParam::Volume,
                time_secs: time,
            },
            AppCommand::AddEffectKeyframe {
                clip_id,
                effect_index: 0,
                param_name: "radius".into(),
                time_secs: time,
                value: 0.9,
            },
            AppCommand::RemoveAudioEffectKeyframe {
                clip_id,
                effect_index: 0,
                param_name: "gain".into(),
                time_secs: time,
            },
            AppCommand::AddTitleObjectPositionKeyframe {
                clip_id,
                object_id,
                axis: TitlePositionAxis::X,
                time_secs: time,
                value: 0.9,
            },
            AppCommand::RemoveTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: "glow.radius".into(),
                time_secs: time,
            },
        ];
        for cmd in &mut commands {
            resolve_command(&seq, cmd);
            let time_secs = match cmd {
                AppCommand::AddVideoKeyframe { time_secs, .. }
                | AppCommand::RemoveAudioKeyframe { time_secs, .. }
                | AppCommand::AddEffectKeyframe { time_secs, .. }
                | AppCommand::RemoveAudioEffectKeyframe { time_secs, .. }
                | AppCommand::RemoveTitleObjectEffectKeyframe { time_secs, .. }
                | AppCommand::AddTitleObjectPositionKeyframe { time_secs, .. } => *time_secs,
                _ => unreachable!(),
            };
            assert_eq!(time_secs, 0.7);
            let mut track = keys(&[0.7, 1.0]);
            klipp_core::keyframe::upsert_keyframe(&mut track, time_secs, 0.9);
            assert_eq!(
                track.len(),
                2,
                "an edit must not create an off-grid duplicate"
            );
            klipp_core::keyframe::remove_keyframe_at(&mut track, time_secs);
            assert_eq!(track, keys(&[1.0]));
        }
    }

    #[test]
    fn inspector_uses_canonical_playhead_despite_smooth_scrubbing() {
        let state = crate::state::AppState {
            playhead: Timecode::from_secs_f64(3.0, 30),
            playhead_smooth_secs: 3.009,
            ..Default::default()
        };
        let c = Clip::gap(
            Timecode::from_secs_f64(0.0, 30),
            Timecode::from_secs_f64(5.0, 30),
        );
        assert_eq!(clip_time(&state, &c), 3.0);
        let track = keys(&[3.0, 3.009]);
        assert_eq!(nearest(&track, clip_time(&state, &c), 0.5 / 30.0), Some(0));
    }

    #[test]
    fn dense_frames_and_fractional_rates_select_and_remove_only_one_key() {
        for fps in [
            Fps { num: 60, den: 1 },
            Fps { num: 120, den: 1 },
            Fps {
                num: 60000,
                den: 1001,
            },
        ] {
            let dt = 1.0 / fps.as_f64();
            let mut seq = fixture();
            seq.fps = fps;
            let c = &mut seq.tracks[0].clips[0];
            c.edit_opacity_keyframes(|v| *v = keys(&[0.0, dt, 2.0 * dt]));
            let mut cmd = AppCommand::RemoveVideoKeyframe {
                clip_id: c.id,
                param: VideoParam::Opacity,
                time_secs: dt + dt * 0.1,
            };
            resolve_command(&seq, &mut cmd);
            let AppCommand::RemoveVideoKeyframe { time_secs, .. } = cmd else {
                unreachable!()
            };
            assert_eq!(time_secs, dt);
            let clip = &mut seq.tracks[0].clips[0];
            clip.edit_opacity_keyframes(|track| klipp_core::keyframe::remove_keyframe_at(track, time_secs));
            assert_eq!(clip.opacity_keyframes(), keys(&[0.0, 2.0 * dt]));
        }
    }

    #[test]
    fn nearby_subframe_keys_select_the_closest_without_retiming_others() {
        let mut seq = fixture();
        seq.tracks[0].clips[0].edit_opacity_keyframes(|v| *v = keys(&[0.695, 0.707]));
        let c = &seq.tracks[0].clips[0];
        let mut cmd = AppCommand::AddVideoKeyframe {
            clip_id: c.id,
            param: VideoParam::Opacity,
            time_secs: 17.0 / 24.0,
            value: 0.9,
        };
        assert_eq!(
            nearest(c.opacity_keyframes(), 17.0 / 24.0, 0.5 / 24.0),
            Some(1)
        );
        resolve_command(&seq, &mut cmd);
        let AppCommand::AddVideoKeyframe {
            time_secs, value, ..
        } = cmd
        else {
            unreachable!()
        };
        let clip = &mut seq.tracks[0].clips[0];
        clip.edit_opacity_keyframes(|track| klipp_core::keyframe::upsert_keyframe(track, time_secs, value));
        let track = clip.opacity_keyframes();
        assert_eq!(track.len(), 2);
        assert_eq!(track[0], keys(&[0.695])[0]);
        assert_eq!(track[1].time_secs, 0.707);
        assert_eq!(track[1].value, 0.9);
    }
    #[test]
    fn navigation_does_not_reselect_the_current_off_grid_key() {
        let track = keys(&[0.5, 0.7, 1.0]);
        let current = state(&track, 17.0 / 24.0, 0.5 / 24.0);
        assert!(current.has_keyframe_at_playhead);
        assert_eq!(current.prev_keyframe_time, Some(0.5));
        assert_eq!(current.next_keyframe_time, Some(1.0));
        for fps in [60.0, 120.0, 60000.0 / 1001.0] {
            let dt = 1.0 / fps;
            let current = state(&keys(&[0.0, dt, 2.0 * dt]), dt, 0.5 / fps);
            assert_eq!(current.prev_keyframe_time, Some(0.0));
            assert_eq!(current.next_keyframe_time, Some(2.0 * dt));
        }
    }
    #[test]
    fn a_key_exactly_halfway_between_frames_remains_selectable() {
        // Binary-exact times expose a gap hidden by decimal rounding when
        // both frame windows exclude their endpoints.
        let mut seq = fixture();
        seq.fps = Fps { num: 32, den: 1 };
        let halfway = 1.0 / 64.0;
        seq.tracks[0].clips[0].edit_opacity_keyframes(|v| *v = keys(&[halfway]));
        for frame in [0.0, 1.0 / 32.0] {
            let clip = &seq.tracks[0].clips[0];
            assert_eq!(nearest(clip.opacity_keyframes(), frame, halfway), Some(0));
            let mut cmd = AppCommand::RemoveVideoKeyframe {
                clip_id: clip.id,
                param: VideoParam::Opacity,
                time_secs: frame,
            };
            resolve_command(&seq, &mut cmd);
            assert!(
                matches!(cmd, AppCommand::RemoveVideoKeyframe { time_secs, .. } if time_secs == halfway)
            );
        }
    }
    #[test]
    fn decimal_half_frame_boundaries_have_no_selection_navigation_gap() {
        for fps in [24.0, 30.0, 60.0, 120.0, 30000.0 / 1001.0] {
            let tol = 0.5 / fps;
            for frame in 0..2000 {
                let t = frame as f64 / fps;
                for k in [t - tol, t + tol] {
                    let track = keys(&[k]);
                    assert_eq!(nearest(&track, t, tol), Some(0), "{fps} fps, frame {frame}");
                    let selection = state(&track, t, tol);
                    assert!(selection.has_keyframe_at_playhead);
                    assert_eq!(selection.prev_keyframe_time, None);
                    assert_eq!(selection.next_keyframe_time, None);
                }
                let track = keys(&[t - tol - 2e-6, t + tol + 2e-6]);
                let selection = state(&track, t, tol);
                assert!(!selection.has_keyframe_at_playhead);
                assert_eq!(selection.prev_keyframe_time, Some(track[0].time_secs));
                assert_eq!(selection.next_keyframe_time, Some(track[1].time_secs));
            }
        }
    }

    #[test]
    fn subframe_keys_are_counted_instead_of_silently_hidden() {
        let track = keys(&(0..20).map(|i| i as f64 / 120.0).collect::<Vec<_>>());
        let selection = state(&track, 1.0 / 24.0, 0.5 / 24.0);
        assert_eq!(selection.keys_in_frame, 5);
        assert_eq!(nearest(&track, 1.0 / 24.0, 0.5 / 24.0), Some(5));
        assert_eq!(selection.prev_keyframe_time, Some(2.0 / 120.0));
        assert_eq!(selection.next_keyframe_time, Some(8.0 / 120.0));
    }

    #[test]
    fn editing_an_off_grid_bezier_key_preserves_its_curve_shape() {
        let mut seq = fixture();
        let clip = &mut seq.tracks[0].clips[0];
        clip.edit_opacity_keyframes(|k| {
            k[0].interpolation = Interpolation::Bezier;
            k[0].tangent_in = (-0.2, -0.3);
            k[0].tangent_out = (0.1, 0.4);
        });
        let mut expected = clip.opacity_keyframes().to_vec();
        expected[0].value = 0.9;
        let mut cmd = AppCommand::AddVideoKeyframe { clip_id: clip.id, param: VideoParam::Opacity, time_secs: 17.0 / 24.0, value: 0.9 };
        resolve_command(&seq, &mut cmd);
        let AppCommand::AddVideoKeyframe { time_secs, value, .. } = cmd else { unreachable!() };
        seq.tracks[0].clips[0].edit_opacity_keyframes(|v| klipp_core::keyframe::upsert_keyframe(v, time_secs, value));
        assert_eq!(seq.tracks[0].clips[0].opacity_keyframes(), expected);
    }

    #[test]
    fn mask_frame_context_uses_canonical_clip_time_and_sequence_window() {
        for (rate, stored) in [(24, 0.7), (60, 0.707)] {
            let mut seq = fixture();
            seq.fps = Fps::new(rate, 1);
            let clip_start = Timecode::from_secs_f64(2.0, rate);
            let playhead = Timecode::from_secs_f64(2.0 + stored, rate);
            let frame = FrameSelection::new(playhead, clip_start, frame_tolerance(&seq));
            let track = keys(&[stored, 1.0]);
            let selected = nearest(&track, frame.time_secs, frame.tolerance).unwrap();
            assert_eq!(track[selected].time_secs, stored);
            assert!((frame.time_secs - stored).abs() > 1e-3, "old node mask window must miss this fixture");
            let mut edited = track.clone();
            klipp_core::keyframe::remove_keyframe_at(&mut edited, track[selected].time_secs);
            assert_eq!(edited, keys(&[1.0]));
        }
    }

}

/// Controls editing an in-frame key display its stored value, not the
/// interpolated value at a different timestamp. Non-key frames evaluate normally.
pub(crate) fn editable_value(keys: &[Keyframe], time: f64, default: f32, tolerance: f64) -> f32 {
    nearest(keys, time, tolerance).map_or_else(|| klipp_core::keyframe::evaluate(keys,time,default), |i| keys[i].value)
}

#[cfg(test)]
mod edit_value_tests {
    use super::*;
    #[test]
    fn dragging_an_off_grid_key_starts_at_its_value() {
        let mut keys = Vec::new();
        klipp_core::keyframe::upsert_keyframe(&mut keys,0.7,0.0);
        klipp_core::keyframe::upsert_keyframe(&mut keys,1.0,100.0);
        let time = 17.0/24.0;
        let value = editable_value(&keys,time,0.0,0.5/24.0);
        assert_eq!(value,0.0);
        let stored = keys[nearest(&keys,time,0.5/24.0).unwrap()].time_secs;
        klipp_core::keyframe::upsert_keyframe(&mut keys,stored,value+1.0);
        assert_eq!(editable_value(&keys,time,0.0,0.5/24.0),1.0);
        assert_eq!(keys[1].value,100.0);
    }
}

#[cfg(test)]
mod timeline_click_tests {
    #[test]
    fn clicks_select_existing_keys_or_insert_on_frames() {
        let mut keys=vec![];klipp_core::keyframe::upsert_keyframe(&mut keys,1.0,0.0);
        assert_eq!(super::timeline_key_time(&keys,1.004,120.0),1.0);
        assert_eq!(super::timeline_key_time(&[],1.004,24.0),1.0);
        let mut off_grid=vec![];klipp_core::keyframe::upsert_keyframe(&mut off_grid,0.707,0.0);
        assert_eq!(super::timeline_key_time(&off_grid,0.700,24.0),0.707);
    }
}
