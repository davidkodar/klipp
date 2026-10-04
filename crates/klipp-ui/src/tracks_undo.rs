//! Undo for the timeline edits whose handlers change the active
//! sequence's tracks in place without recording it (#907): lift and
//! extract, close and insert gap, a clip enabled, soloed, linked or
//! grouped, a track added, locked, renamed or removed, a transition
//! changed or taken off. Each became an edit Ctrl+Z skipped, so the next
//! undo took back something older, out of order. Their tracks are kept
//! before the edit and recorded with the tracks after it as one named
//! step, [`EditCommand::SetTracks`].

use crate::history::EditCommand;
use crate::state::AppState;
use klipp_core::AppCommand;

/// How close together two changes of one dragged value must come to be
/// one undo step.
const GESTURE_GAP: std::time::Duration = std::time::Duration::from_millis(700);

/// The undo step's name for a timeline edit whose handler changes the
/// tracks without recording it, or `None` for one that records itself.
pub fn label(cmd: &AppCommand) -> Option<&'static str> {
    Some(match cmd {
        AppCommand::LiftEdit => "Lift",
        AppCommand::ExtractEdit => "Extract",
        AppCommand::CloseGap { .. } => "Close Gap",
        AppCommand::InsertGapOnTrack { .. } => "Insert Gap",
        AppCommand::SetClipEnabled { enabled: true, .. } => "Enable Clip",
        AppCommand::SetClipEnabled { .. } => "Disable Clip",
        AppCommand::SetClipSolo { .. } => "Solo Clip",
        AppCommand::UnlinkClip { .. } => "Unlink",
        AppCommand::LinkClips { .. } => "Link",
        AppCommand::GroupClips { .. } => "Group",
        AppCommand::UngroupClips { .. } | AppCommand::UngroupClip { .. } => "Ungroup",
        AppCommand::SetTrackLocked { locked: true, .. } => "Lock Track",
        AppCommand::SetTrackLocked { .. } => "Unlock Track",
        AppCommand::RenameTrack { .. } => "Rename Track",
        AppCommand::AddVideoTrack
        | AppCommand::AddAudioTrack
        | AppCommand::AddSubtitleTrack { .. }
        | AppCommand::AddTrackAbove { .. }
        | AppCommand::AddTrackBelow { .. } => "Add Track",
        AppCommand::RemoveTrack { .. } => "Remove Track",
        AppCommand::RemoveTransition { .. } => "Remove Transition",
        AppCommand::SetTransitionDuration { .. } => "Transition Duration",
        AppCommand::SetTransitionKind { .. } => "Transition Kind",
        AppCommand::SetTransitionParam { .. } => "Transition Settings",
        AppCommand::SetTransitionAlignment { .. } => "Transition Alignment",
        _ => return None,
    })
}

/// Whether repeats of this edit are one gesture, a value dragged, and
/// fold into the step before when they come close together.
fn is_gesture(label: &str) -> bool {
    matches!(label, "Transition Duration" | "Transition Settings")
}

/// The active sequence's tracks before an edit, to record it afterwards.
pub struct Before {
    label: &'static str,
    sequence: klipp_core::SequenceId,
    tracks: Vec<klipp_timeline::Track>,
}

/// The tracks before `cmd`, when it is one of the edits recorded here.
pub fn before(state: &AppState, cmd: &AppCommand) -> Option<Before> {
    let label = label(cmd)?;
    before_labelled(state, label)
}

/// The tracks before an edit made outside a command (a track added from
/// a header's menu, a track removed with its clips from the dialog).
pub fn before_labelled(state: &AppState, label: &'static str) -> Option<Before> {
    let seq = state.sequences.get(state.active_sequence_idx)?;
    Some(Before { label, sequence: seq.id, tracks: seq.tracks.clone() })
}

/// Record the edit since `before` as one undo step, when it changed the
/// tracks: a refused edit, or one that changed nothing, records nothing.
/// A dragged value's changes in quick succession stay one step.
pub fn record(state: &mut AppState, before: Before) {
    let Some(seq) = state.sequences.get(state.active_sequence_idx) else { return };
    if seq.id != before.sequence || same_tracks(&before.tracks, &seq.tracks) {
        return;
    }
    let new = seq.tracks.clone();
    let now = std::time::Instant::now();
    let continues = is_gesture(before.label)
        && state
            .tracks_undo_gesture
            .is_some_and(|(label, at)| label == before.label && now.duration_since(at) < GESTURE_GAP);
    state.tracks_undo_gesture = Some((before.label, now));
    if continues {
        let merged = state.active_history_mut().merge_into_last(|last| match last {
            EditCommand::SetTracks { description, new: last_new, .. } if *description == before.label => {
                *last_new = new.clone();
                true
            }
            _ => false,
        });
        if merged {
            return;
        }
    }
    state.push_history(EditCommand::SetTracks { description: before.label, old: before.tracks, new });
}

/// Whether two track lists are the same, field for field.
fn same_tracks(a: &[klipp_timeline::Track], b: &[klipp_timeline::Track]) -> bool {
    a.len() == b.len() && serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_a_clip() -> AppState {
        let mut state = AppState::default();
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        state.sequences[0].tracks[v1].clips.push(klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0)));
        state
    }

    fn top_label(state: &mut AppState) -> Option<String> {
        state.active_history_mut().undo_label().map(str::to_string)
    }

    #[test]
    fn an_edit_made_in_place_is_one_named_undo_step() {
        let mut state = state_with_a_clip();
        let clip = state.sequences[0].all_clips().next().unwrap().id;
        let cmd = AppCommand::SetClipEnabled { clip_id: clip, enabled: false };
        let before = before(&state, &cmd).expect("recorded here");
        state.sequences[0].all_clips_mut().next().unwrap().enabled = false;
        record(&mut state, before);
        assert_eq!(top_label(&mut state).as_deref(), Some("Disable Clip"));
        let count = state.active_history_mut().undo_count();
        assert_eq!(count, 1);
    }

    #[test]
    fn an_edit_that_changed_nothing_records_nothing() {
        let mut state = state_with_a_clip();
        let before = before(&state, &AppCommand::LiftEdit).unwrap();
        record(&mut state, before);
        assert_eq!(state.active_history_mut().undo_count(), 0, "a refused lift is no step");
    }

    #[test]
    fn a_dragged_value_is_one_step_and_a_separate_change_another() {
        let mut state = state_with_a_clip();
        let rename = |state: &mut AppState, name: &str, label: &'static str| {
            let before = before_labelled(state, label).unwrap();
            state.sequences[0].tracks[0].name = name.into();
            record(state, before);
        };
        rename(&mut state, "a", "Transition Settings");
        rename(&mut state, "b", "Transition Settings");
        rename(&mut state, "c", "Transition Settings");
        assert_eq!(state.active_history_mut().undo_count(), 1, "one drag, one step");
        rename(&mut state, "d", "Rename Track");
        rename(&mut state, "e", "Rename Track");
        assert_eq!(state.active_history_mut().undo_count(), 3, "renames are steps of their own");
    }

    #[test]
    fn every_edit_named_here_has_a_label() {
        let id = klipp_core::ClipId::new();
        for cmd in [
            AppCommand::LiftEdit,
            AppCommand::ExtractEdit,
            AppCommand::UnlinkClip { clip_id: id },
            AppCommand::AddVideoTrack,
            AppCommand::AddAudioTrack,
        ] {
            assert!(label(&cmd).is_some(), "{cmd:?}");
        }
        assert!(label(&AppCommand::Play).is_none());
    }
}
