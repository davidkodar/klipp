//! Undo for the timeline edits whose handlers change the active
//! sequence's tracks in place without recording it (#907): lift and
//! extract, close and insert gap, a clip enabled, soloed, linked or
//! grouped, a track added, locked, renamed or removed, a transition
//! changed or taken off. Each became an edit Ctrl+Z skipped, so the next
//! undo took back something older, out of order. Their tracks are kept
//! before the edit, and what changed is recorded as one named step: the
//! clips and transitions of the tracks whose clips changed, the one track
//! locked or renamed, the whole list when a track came or went. Undoing a
//! lift therefore leaves a track's mute or height, changed since, alone.

use crate::history::{EditCommand, TrackContents};
use klipp_timeline::Track;
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
        AppCommand::OverwriteEdit | AppCommand::InsertSourceAtPlayhead => "Overwrite",
        AppCommand::FitToFillEdit => "Fit to Fill",
        AppCommand::PlaceOnTopEdit => "Place on Top",
        AppCommand::ReplaceEdit => "Replace",
        AppCommand::RippleOverwriteEdit => "Ripple Overwrite",
        AppCommand::ExtractEdit => "Extract",
        AppCommand::CloseGap { .. } => "Close Gap",
        AppCommand::CloseAllGaps { .. } => "Close All Gaps",
        AppCommand::SetClipKeepPitch { .. } => "Keep Pitch",
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
        AppCommand::SetTrackSyncLock { on: true, .. } => "Sync Lock Track",
        AppCommand::SetTrackSyncLock { .. } => "Sync Unlock Track",
        AppCommand::SetTrackLanguage { .. } => "Track Language",
        AppCommand::SetTrackTargeted { on: true, .. } => "Target Track",
        AppCommand::SetTrackTargeted { .. } => "Untarget Track",
        AppCommand::SetTrackSourcePatch { .. } => "Patch Source",
        // Captions (#70): adding one was not undoable; a caption style,
        // with the captions it restyles, is one step.
        AppCommand::AddSubtitleAtPlayhead { .. } => "Add Caption",
        AppCommand::SaveCaptionStyle { apply_to_all: true, .. } => "Restyle Captions",
        AppCommand::SaveCaptionStyle { .. } => "Caption Style",
        AppCommand::ClearCaptionStyle { .. } => "Caption Style",
        AppCommand::AddVideoTrack
        | AppCommand::AddAudioTrack
        | AppCommand::AddSubtitleTrack { .. }
        | AppCommand::AddTrackAbove { .. }
        | AppCommand::AddTrackBelow { .. } => "Add Track",
        AppCommand::RemoveTrack { .. } => "Remove Track",
        // Removing a transition, its duration and its alignment record
        // themselves (through the document's transition ops).
        AppCommand::SetTransitionKind { .. } => "Transition Kind",
        AppCommand::SetTrackColourLabel { .. } => "Track Colour",
        AppCommand::MoveTrack { .. } => "Move Track",
        AppCommand::SplitToMono { .. } => "Split to Mono",
        AppCommand::SetTransitionParam { .. } => "Transition Settings",
        _ => return None,
    })
}

/// Whether repeats of this edit are one gesture, a value dragged, and
/// fold into the step before when they come close together.
fn is_gesture(label: &str) -> bool {
    matches!(label, "Transition Settings")
}

/// The active sequence's tracks before an edit, to record it afterwards.
pub struct Before {
    label: &'static str,
    sequence: klipp_core::SequenceId,
    /// The tracks kept: every one, or only `scope`'s.
    tracks: Vec<Track>,
    /// The one track an edit can change, when the command says which: a
    /// transition's settings, sent every frame of a drag, keep one track
    /// rather than all of them.
    scope: Option<usize>,
    /// The history's size before the edit: a handler that recorded its
    /// own step is not recorded twice.
    recorded: (u64, usize),
}

/// The tracks before `cmd`, when it is one of the edits recorded here.
pub fn before(state: &AppState, cmd: &AppCommand) -> Option<Before> {
    let label = label(cmd)?;
    let scope = match cmd {
        AppCommand::SetTransitionKind { track_index, .. } | AppCommand::SetTransitionParam { track_index, .. } => Some(*track_index),
        _ => None,
    };
    let mut before = before_labelled(state, label)?;
    if let Some(i) = scope {
        let seq = state.sequences.get(state.active_sequence_idx)?;
        before.tracks = vec![seq.tracks.get(i)?.clone()];
        before.scope = Some(i);
    }
    Some(before)
}

/// The tracks before an edit made outside a command (a track added from
/// a header's menu, a track removed with its clips from the dialog).
pub fn before_labelled(state: &AppState, label: &'static str) -> Option<Before> {
    let seq = state.sequences.get(state.active_sequence_idx)?;
    Some(Before { label, sequence: seq.id, tracks: seq.tracks.clone(), scope: None, recorded: history_size(state) })
}

/// Record the edit since `before` as one undo step, when it changed the
/// tracks: a refused edit, or one that changed nothing, records nothing.
/// A dragged value's changes in quick succession stay one step.
pub fn record(state: &mut AppState, before: Before) {
    let Some(seq) = state.sequences.get(state.active_sequence_idx) else { return };
    if seq.id != before.sequence || history_size(state) != before.recorded {
        return;
    }
    let edit = match before.scope {
        Some(i) => match seq.tracks.get(i) {
            Some(now) if now.id == before.tracks[0].id => edit_between(before.label, &before.tracks, std::slice::from_ref(now)).map(|e| at_index(e, i)),
            _ => None,
        },
        None => edit_between(before.label, &before.tracks, &seq.tracks),
    };
    let Some(edit) = edit else { return };
    let now = std::time::Instant::now();
    let continues = is_gesture(before.label)
        && state
            .tracks_undo_gesture
            .is_some_and(|(label, at)| label == before.label && now.duration_since(at) < GESTURE_GAP);
    state.tracks_undo_gesture = Some((before.label, now));
    if let (true, EditCommand::SetTrackContents { track_index, new, .. }) = (continues, &edit) {
        let merged = state.active_history_mut().merge_into_last(|last| match last {
            EditCommand::SetTrackContents { description, track_index: last_track, new: last_new, .. }
                if *description == before.label && last_track == track_index =>
            {
                *last_new = new.clone();
                true
            }
            _ => false,
        });
        if merged {
            return;
        }
    }
    state.push_history(edit);
}

/// The undo record taking `old` to `new`: the whole list when a track
/// came or went; else, for each track that changed, the track itself when
/// more than its clips and transitions changed, or just those.
fn edit_between(label: &'static str, old: &[Track], new: &[Track]) -> Option<EditCommand> {
    let ids = |tracks: &[Track]| tracks.iter().map(|t| t.id).collect::<Vec<_>>();
    if ids(old) != ids(new) {
        return Some(EditCommand::SetTracks { description: label, old: old.to_vec(), new: new.to_vec() });
    }
    let mut edits: Vec<EditCommand> = Vec::new();
    for (i, (o, n)) in old.iter().zip(new).enumerate() {
        if !same((&o.clips, &o.transitions), (&n.clips, &n.transitions)) || !same(bare(o), bare(n)) {
            edits.push(if same(bare(o), bare(n)) {
                EditCommand::SetTrackContents { description: label, track_index: i, old: Box::new(TrackContents::of(o)), new: Box::new(TrackContents::of(n)) }
            } else {
                EditCommand::SetTrack { description: label, track_index: i, old: Box::new(o.clone()), new: Box::new(n.clone()) }
            });
        }
    }
    match edits.len() {
        0 => None,
        1 => edits.pop(),
        _ => Some(EditCommand::Named { label: label.to_string(), edit: Box::new(EditCommand::Batch(edits)) }),
    }
}

/// How many steps the history has taken: pushes counted for autosave,
/// and the undo stack's depth.
fn history_size(state: &AppState) -> (u64, usize) {
    (state.history_ops_since_save, state.active_history().undo_count())
}

/// A one-track record made against a list of that one track, moved to
/// the track's real index.
fn at_index(edit: EditCommand, i: usize) -> EditCommand {
    match edit {
        EditCommand::SetTrackContents { description, old, new, .. } => EditCommand::SetTrackContents { description, track_index: i, old, new },
        EditCommand::SetTrack { description, old, new, .. } => EditCommand::SetTrack { description, track_index: i, old, new },
        other => other,
    }
}

/// A track without its clips and transitions: the rest of what it is.
fn bare(track: &Track) -> Track {
    Track { clips: Vec::new(), transitions: Vec::new(), ..track.clone() }
}

/// Whether two values are the same, field for field.
fn same(a: impl serde::Serialize, b: impl serde::Serialize) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
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
        let v1 = state.sequences[0].tracks.iter().position(|t| !t.clips.is_empty()).unwrap();
        let change = |state: &mut AppState, speed: f64, label: &'static str| {
            let before = before_labelled(state, label).unwrap();
            state.sequences[0].tracks[v1].clips[0].speed = speed;
            record(state, before);
        };
        change(&mut state, 1.1, "Transition Settings");
        change(&mut state, 1.2, "Transition Settings");
        change(&mut state, 1.3, "Transition Settings");
        assert_eq!(state.active_history_mut().undo_count(), 1, "one drag, one step");
        change(&mut state, 1.4, "Close Gap");
        change(&mut state, 1.5, "Close Gap");
        assert_eq!(state.active_history_mut().undo_count(), 3, "other edits are steps of their own");
    }

    #[test]
    fn each_edit_records_only_what_it_changed() {
        let mut state = state_with_a_clip();
        let v1 = state.sequences[0].tracks.iter().position(|t| !t.clips.is_empty()).unwrap();
        let top = |state: &mut AppState| state.active_history_mut().undo_entries().last().map(|e| e.command.clone());

        // A clip changed: that track's clips, so undoing it leaves the
        // track's mute, height or lock, changed since, alone.
        let before = before_labelled(&state, "Disable Clip").unwrap();
        state.sequences[0].tracks[v1].clips[0].enabled = false;
        record(&mut state, before);
        assert!(matches!(top(&mut state), Some(EditCommand::SetTrackContents { track_index, .. }) if track_index == v1));

        // A track locked: that track.
        let before = before_labelled(&state, "Lock Track").unwrap();
        state.sequences[0].tracks[0].locked = true;
        record(&mut state, before);
        assert!(matches!(top(&mut state), Some(EditCommand::SetTrack { track_index: 0, .. })));

        // A track added: the list.
        let before = before_labelled(&state, "Add Track").unwrap();
        state.sequences[0].tracks.push(klipp_timeline::Track::new("Video 9", klipp_timeline::TrackKind::Video));
        record(&mut state, before);
        assert!(matches!(top(&mut state), Some(EditCommand::SetTracks { .. })));
    }

    #[test]
    fn a_handler_that_recorded_its_own_step_is_not_recorded_twice() {
        let mut state = state_with_a_clip();
        let v1 = state.sequences[0].tracks.iter().position(|t| !t.clips.is_empty()).unwrap();
        let before = before_labelled(&state, "Close Gap").unwrap();
        let old = state.sequences[0].tracks[v1].clips[0].clone();
        state.sequences[0].tracks[v1].clips[0].enabled = false;
        let new = state.sequences[0].tracks[v1].clips[0].clone();
        state.push_history(EditCommand::ReplaceClip { description: "Its own", old: Box::new(old), new: Box::new(new) });
        record(&mut state, before);
        assert_eq!(state.active_history_mut().undo_count(), 1, "only the handler's own step");
    }

    #[test]
    fn transitions_that_record_themselves_are_not_named_here() {
        let t = klipp_core::TransitionId::new();
        assert!(label(&AppCommand::RemoveTransition { track_index: 0, transition_id: t }).is_none());
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
            AppCommand::SetTrackSyncLock { track_id: klipp_core::TrackId::new(), on: true },
            AppCommand::SetTrackLanguage { track_id: klipp_core::TrackId::new(), language: Some("de".into()) },
            AppCommand::SetTrackTargeted { track_id: klipp_core::TrackId::new(), on: false },
            AppCommand::SetTrackSourcePatch { track_id: klipp_core::TrackId::new(), on: true },
        ] {
            assert!(label(&cmd).is_some(), "{cmd:?}");
        }
        assert!(label(&AppCommand::Play).is_none());
    }
}
