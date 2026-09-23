//! Frame-aware UI selection, separate from exact authored timestamp identity.
use klipp_core::{AppCommand, AudioParam, Keyframe, TitlePositionAxis, VideoParam};
use klipp_timeline::{Clip, Sequence, TitleObjectContent, TitleObjectId};

/// Inspector and K operate on the same canonical playhead, not the smooth
/// pointer/playback position between displayed frames.
pub(super) fn clip_time(state: &crate::state::AppState, clip: &Clip) -> f64 {
    (state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0)
}

/// Pick the closest key within half a sequence frame. Equal distances prefer
/// the earlier entry in the sorted track. Never move the stored timestamp.
pub(crate) fn nearest(keys: &[Keyframe], time: f64, tolerance: f64) -> Option<usize> {
    keys.iter()
        .enumerate()
        .filter(|(_, k)| (k.time_secs - time).abs() <= tolerance)
        .min_by(|(_, a), (_, b)| {
            (a.time_secs - time)
                .abs()
                .total_cmp(&(b.time_secs - time).abs())
        })
        .map(|(i, _)| i)
}

/// Selection and navigation share the displayed frame window. A rounded
/// off-grid key is current, so previous/next must not select it again.
pub(super) fn state(
    keys: &[Keyframe],
    time: f64,
    tolerance: f64,
) -> crate::widgets::scrubby_param::KeyframeState {
    let current = nearest(keys, time, tolerance);
    crate::widgets::scrubby_param::KeyframeState {
        has_keyframe_at_playhead: current.is_some(),
        has_any_keyframes: !keys.is_empty(),
        current_interpolation: current.map(|i| keys[i].interpolation),
        prev_keyframe_time: keys
            .iter()
            .rfind(|k| k.time_secs < time - tolerance)
            .map(|k| k.time_secs),
        next_keyframe_time: keys
            .iter()
            .find(|k| k.time_secs > time + tolerance)
            .map(|k| k.time_secs),
    }
}

/// Inspector commands use a displayed frame, while the document mutation uses
/// precise timestamp identity. Resolve only inspector add/remove commands here;
/// agent-authored timestamps and curve moves retain their original precision.
pub(super) fn resolve_command(sequence: &Sequence, command: &mut AppCommand) {
    resolve_with(command, 0.5 / sequence.fps.as_f64(), |id| {
        sequence
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .find(|c| c.id == id)
    });
}

fn resolve_with<'a>(
    command: &mut AppCommand,
    tolerance: f64,
    clip_for: impl Fn(klipp_core::ClipId) -> Option<&'a Clip>,
) {
    use AppCommand::*;
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
            let keys = match param {
                VideoParam::Opacity => &c.opacity_keyframes,
                VideoParam::PositionX => &c.transform_pos_x_keyframes,
                VideoParam::PositionY => &c.transform_pos_y_keyframes,
                VideoParam::Scale => &c.transform_scale_keyframes,
                VideoParam::Rotation => &c.transform_rotation_keyframes,
                VideoParam::TiltX => &c.transform_tilt_x_keyframes,
                VideoParam::TiltY => &c.transform_tilt_y_keyframes,
                VideoParam::MatteFeather => &c.matte_feather_keyframes,
                VideoParam::MatteChoke => &c.matte_choke_keyframes,
            };
            (keys, time_secs)
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
            (
                c.effect_keyframes
                    .get(&format!("{effect_index}:{param_name}"))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                time_secs,
            )
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
            let TitleObjectContent::TextBox(t) = &o.content else {
                return;
            };
            (t.effect_keyframes_for(param_key).unwrap_or(&[]), time_secs)
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
            })
            .collect()
    }

    fn fixture() -> Sequence {
        let mut seq = Sequence::new_default();
        let mut c = Clip::gap(
            Timecode::from_secs_f64(0.0, 24),
            Timecode::from_secs_f64(5.0, 24),
        );
        c.opacity_keyframes = keys(&[0.7, 1.0]);
        c.volume_keyframes = c.opacity_keyframes.clone();
        c.effect_keyframes
            .insert("0:radius".into(), keys(&[0.7, 1.0]));
        c.audio_effect_keyframes
            .insert("0:gain".into(), keys(&[0.7, 1.0]));
        let mut title = TitleData::default();
        title.objects[0].transform.center_x_keyframes = keys(&[0.7, 1.0]);
        let TitleObjectContent::TextBox(text) = &mut title.objects[0].content else {
            unreachable!()
        };
        text.glows.push(klipp_timeline::TextGlow {
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
            c.opacity_keyframes = keys(&[0.0, dt, 2.0 * dt]);
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
            let track = &mut seq.tracks[0].clips[0].opacity_keyframes;
            klipp_core::keyframe::remove_keyframe_at(track, time_secs);
            assert_eq!(*track, keys(&[0.0, 2.0 * dt]));
        }
    }

    #[test]
    fn nearby_subframe_keys_select_the_closest_without_retiming_others() {
        let mut seq = fixture();
        seq.tracks[0].clips[0].opacity_keyframes = keys(&[0.695, 0.707]);
        let c = &seq.tracks[0].clips[0];
        let mut cmd = AppCommand::AddVideoKeyframe {
            clip_id: c.id,
            param: VideoParam::Opacity,
            time_secs: 17.0 / 24.0,
            value: 0.9,
        };
        assert_eq!(
            nearest(&c.opacity_keyframes, 17.0 / 24.0, 0.5 / 24.0),
            Some(1)
        );
        resolve_command(&seq, &mut cmd);
        let AppCommand::AddVideoKeyframe {
            time_secs, value, ..
        } = cmd
        else {
            unreachable!()
        };
        let track = &mut seq.tracks[0].clips[0].opacity_keyframes;
        klipp_core::keyframe::upsert_keyframe(track, time_secs, value);
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
        seq.tracks[0].clips[0].opacity_keyframes = keys(&[halfway]);
        for frame in [0.0, 1.0 / 32.0] {
            let clip = &seq.tracks[0].clips[0];
            assert_eq!(nearest(&clip.opacity_keyframes, frame, halfway), Some(0));
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
}
