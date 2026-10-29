//! Clips taken out of the timeline, one or a selection (#3060): Delete
//! closes the gap on a magnetic track, Lift leaves it; off the magnetic
//! tracks a removal always leaves its gap. Several clips are one undo
//! step, as one is.

use crate::history::EditCommand;
use crate::state::AppState;
use klipp_core::ClipId;

/// Whether track `ti` closes up when a clip leaves it.
fn magnetic(state: &AppState, ti: usize) -> bool {
    state.magnetic_timeline_enabled
        && matches!(
            state.sequence().tracks.get(ti).map(|t| &t.magnetic_role),
            Some(klipp_timeline::TrackRole::Primary | klipp_timeline::TrackRole::Connected | klipp_timeline::TrackRole::ConnectedStoryline)
        )
}

/// The selected clips (the primary selection with the rest), each once.
pub(crate) fn selected(state: &AppState) -> Vec<ClipId> {
    let mut ids = state.selected_clips.clone();
    if let Some(id) = state.selected_clip.filter(|id| !ids.contains(id)) {
        ids.push(id);
    }
    ids
}

fn apply(state: &mut AppState, edit: EditCommand) {
    let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps };
    let _ = view.apply_forward(&edit);
    state.push_history(edit);
}

/// `ids` taken out, as one undo step named `label`: lifted (`lift`, a gap
/// left on a magnetic track) or deleted. A clip already gone with a
/// linked partner taken before it, or on a locked track, is passed over.
/// Refused whole, before anything changes, when a clip off the magnetic
/// tracks can't go (its linked partner locked).
pub(crate) fn remove_clips(state: &mut AppState, ids: &[ClipId], lift: bool, label: &str) -> Result<usize, String> {
    for &id in ids {
        let Some((ti, _)) = state.sequence().find_clip(id) else { continue };
        if !state.sequence().tracks[ti].locked && !magnetic(state, ti) {
            crate::clip_edits::remove(state.sequence(), &state.media_pool, id).map_err(|r| r.why)?;
        }
    }
    let own_batch = !state.active_history_mut().is_batching();
    if own_batch {
        state.active_history_mut().begin_batch();
    }
    let mut removed = 0;
    for &id in ids {
        let Some((ti, _)) = state.sequence().find_clip(id) else { continue };
        if state.sequence().tracks[ti].locked {
            continue;
        }
        removed += 1;
        if !magnetic(state, ti) {
            if let Ok(edit) = crate::clip_edits::remove(state.sequence(), &state.media_pool, id) {
                apply(state, edit);
            }
            continue;
        }
        if lift {
            let result = klipp_timeline::magnetic::MagneticEditor::lift(state.sequence_mut(), id);
            state.push_history(EditCommand::Magnetic(result.command));
            continue;
        }
        // A clip pointing at this one as its matte or transform link
        // loses the binding, so a stale id never outlives its target.
        for c in state.sequence_mut().all_clips_mut() {
            if c.matte_source.as_ref().is_some_and(|b| b.source_clip_id == id) {
                c.matte_source = None;
            }
            if c.transform_link.as_ref().is_some_and(|tl| tl.target.referenced_clip_id() == id) {
                c.transform_link = None;
            }
        }
        // A transition joining the clip (or its linked partner, which the
        // ripple delete takes too) goes with it (#979).
        let gone: Vec<ClipId> = std::iter::once(id).chain(state.sequence().find_clip(id).and_then(|(ti, ci)| state.sequence().tracks[ti].clips[ci].linked_clip_id)).collect();
        for e in klipp_document::ops::transitions_removed_with(state.sequence(), &gone) {
            apply(state, e);
        }
        let result = klipp_timeline::magnetic::MagneticEditor::ripple_delete(state.sequence_mut(), id);
        state.push_history(EditCommand::Magnetic(result.command));
    }
    if own_batch {
        state.active_history_mut().end_batch_as(label);
    }
    let here = |state: &AppState, id: ClipId| state.sequence().find_clip(id).is_some();
    if state.selected_clip.is_some_and(|id| !here(state, id)) {
        state.selected_clip = None;
    }
    let keep: Vec<ClipId> = state.selected_clips.iter().copied().filter(|&id| here(state, id)).collect();
    state.selected_clips = keep;
    state.dirty = true;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::{Clip, TrackKind};

    fn t(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    /// Three clips on two tracks: two on the primary (0–2, 2–4 s, packed)
    /// and one on a free video track (1–3 s), plus one left alone.
    fn state() -> (AppState, [ClipId; 4]) {
        let mut state = AppState { magnetic_timeline_enabled: true, ..Default::default() };
        let seq = &mut state.sequences[0];
        seq.add_video_track("V2".into());
        let primary = seq.tracks.iter().position(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary).unwrap();
        let free = seq.tracks.iter().position(|t| t.name == "V2").unwrap();
        assert_eq!(seq.tracks[free].kind, TrackKind::Video);
        let c = |a: f64, b: f64| Clip::media(klipp_core::MediaId::new(), t(a), t(b), t(0.0), t(b - a));
        let (p1, p2, p3, f1) = (c(0.0, 2.0), c(2.0, 4.0), c(4.0, 6.0), c(1.0, 3.0));
        let ids = [p1.id, p2.id, f1.id, p3.id];
        seq.tracks[primary].clips.extend([p1, p2, p3]);
        seq.tracks[free].clips.push(f1);
        state.selected_clips = vec![ids[0], ids[2]];
        state.selected_clip = Some(ids[1]);
        (state, ids)
    }

    fn undo(state: &mut AppState) {
        let entry = state.active_history_mut().undo().expect("a step to undo");
        let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps };
        view.apply_inverse(&entry.command);
    }

    fn spans(state: &AppState) -> Vec<(ClipId, i64, i64)> {
        let mut v: Vec<_> = state.sequence().all_clips().map(|c| (c.id, c.timeline_in.value, c.timeline_out.value)).collect();
        v.sort_by_key(|x| (x.1, x.0 .0));
        v
    }

    /// Delete with three selected clips on two tracks takes all three,
    /// the primary closing up (the clip left behind moves to 0 s), in one
    /// undo step that puts all three back where they were.
    #[test]
    fn delete_takes_every_selected_clip_in_one_step() {
        let (mut state, ids) = state();
        let before = spans(&state);
        let sel = selected(&state);
        assert_eq!(remove_clips(&mut state, &sel, false, "Delete Clips"), Ok(3));
        assert_eq!(spans(&state), [(ids[3], 0, 48)]);
        assert!(state.selected_clips.is_empty() && state.selected_clip.is_none());
        undo(&mut state);
        assert_eq!(spans(&state), before);
        assert!(state.active_history_mut().undo().is_none(), "one step");
    }

    /// Lift with the same selection takes all three and leaves their
    /// gaps: the clip left behind stays at 4 s. One undo step.
    #[test]
    fn lift_takes_every_selected_clip_and_leaves_the_gaps() {
        let (mut state, ids) = state();
        let before = spans(&state);
        let sel = selected(&state);
        assert_eq!(remove_clips(&mut state, &sel, true, "Lift Clips"), Ok(3));
        let left: Vec<_> = spans(&state).into_iter().filter(|s| !state.sequence().all_clips().any(|c| c.id == s.0 && c.kind.is_gap())).collect();
        assert_eq!(left, [(ids[3], 96, 144)]);
        undo(&mut state);
        assert_eq!(spans(&state), before);
        assert!(state.active_history_mut().undo().is_none(), "one step");
    }

    /// Delete sends the selection's removal when several clips are
    /// selected, and the one clip's as before when one is.
    #[test]
    fn delete_asks_for_the_whole_selection() {
        let (mut state, ids) = state();
        let cmds = crate::app::shortcuts::delete_selection(&mut state);
        assert!(matches!(&cmds[..], [klipp_core::AppCommand::RemoveClips { clip_ids, lift: false }] if clip_ids.len() == 3), "{cmds:?}");
        state.selected_clips.clear();
        let cmds = crate::app::shortcuts::delete_selection(&mut state);
        assert!(matches!(&cmds[..], [klipp_core::AppCommand::RemoveClip { clip_id }] if *clip_id == ids[1]), "{cmds:?}");
    }
}
