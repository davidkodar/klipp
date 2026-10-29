//! Clips taken out of the timeline, one or a selection (#3060): Delete
//! closes the gap, Lift leaves it, on every track. Several clips are one
//! undo step, as one is.

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

/// `ids` taken out, as one undo step named `label`: lifted (`lift`, the
/// gap left) or deleted (the gap closed). A clip already gone with a
/// linked partner taken before it, or on a locked track, is passed over.
/// Refused whole, before anything changes, when a clip off the magnetic
/// tracks can't go (its linked partner locked).
pub(crate) fn remove_clips(state: &mut AppState, ids: &[ClipId], lift: bool, label: &str) -> Result<usize, String> {
    for &id in ids {
        let Some((ti, _)) = state.sequence().find_clip(id) else { continue };
        if !state.sequence().tracks[ti].locked && !magnetic(state, ti) {
            crate::clip_edits::remove(state.sequence(), &state.media_pool, id, !lift).map_err(|r| r.why)?;
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
            if let Ok(edit) = crate::clip_edits::remove(state.sequence(), &state.media_pool, id, !lift) {
                apply(state, edit);
            }
            continue;
        }
        // A clip pointing at this one as its matte or transform link
        // loses the binding, so a stale id never outlives its target: as
        // edits in the step, so the undo gives it back.
        let unlinks: Vec<EditCommand> = state.sequence().all_clips()
            .filter_map(|c| {
                let matte = c.matte_source.as_ref().is_some_and(|b| b.source_clip_id == id);
                let link = c.transform_link.as_ref().is_some_and(|tl| tl.target.referenced_clip_id() == id);
                (matte || link).then(|| {
                    let mut new = c.clone();
                    if matte {
                        new.matte_source = None;
                    }
                    if link {
                        new.transform_link = None;
                    }
                    EditCommand::ReplaceClip { description: "Unlink", old: Box::new(c.clone()), new: Box::new(new) }
                })
            })
            .collect();
        for e in unlinks {
            apply(state, e);
        }
        if lift {
            let result = klipp_timeline::magnetic::MagneticEditor::lift(state.sequence_mut(), id);
            state.push_history(EditCommand::Magnetic(result.command));
            continue;
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
    /// and one on a free video track (1–3 s), selected; left alone, one
    /// after them on each (4–6 s on the primary, 4–5 s on the free track).
    fn state() -> (AppState, [ClipId; 5]) {
        let mut state = AppState { magnetic_timeline_enabled: true, ..Default::default() };
        let seq = &mut state.sequences[0];
        seq.add_video_track("V2".into());
        let primary = seq.tracks.iter().position(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary).unwrap();
        let free = seq.tracks.iter().position(|t| t.name == "V2").unwrap();
        assert_eq!(seq.tracks[free].kind, TrackKind::Video);
        let c = |a: f64, b: f64| Clip::media(klipp_core::MediaId::new(), t(a), t(b), t(0.0), t(b - a));
        let (p1, p2, p3, f1, f2) = (c(0.0, 2.0), c(2.0, 4.0), c(4.0, 6.0), c(1.0, 3.0), c(4.0, 5.0));
        let ids = [p1.id, p2.id, f1.id, p3.id, f2.id];
        seq.tracks[primary].clips.extend([p1, p2, p3]);
        seq.tracks[free].clips.extend([f1, f2]);
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
    /// each track closing up (the primary's clip left behind moves to 0 s,
    /// the free track's to 2 s), in one undo step that puts all three back
    /// where they were.
    #[test]
    fn delete_takes_every_selected_clip_in_one_step() {
        let (mut state, ids) = state();
        let before = spans(&state);
        let sel = selected(&state);
        assert_eq!(remove_clips(&mut state, &sel, false, "Delete Clips"), Ok(3));
        assert_eq!(spans(&state), [(ids[3], 0, 48), (ids[4], 48, 72)]);
        assert!(state.selected_clips.is_empty() && state.selected_clip.is_none());
        undo(&mut state);
        assert_eq!(spans(&state), before);
        assert!(state.active_history_mut().undo().is_none(), "one step");
    }

    /// Lift with the same selection takes all three and leaves their
    /// gaps on both tracks: the clips left behind stay at 4 s. One undo
    /// step.
    #[test]
    fn lift_takes_every_selected_clip_and_leaves_the_gaps() {
        let (mut state, ids) = state();
        let before = spans(&state);
        let sel = selected(&state);
        assert_eq!(remove_clips(&mut state, &sel, true, "Lift Clips"), Ok(3));
        let left: Vec<_> = spans(&state).into_iter().filter(|s| !state.sequence().all_clips().any(|c| c.id == s.0 && c.kind.is_gap())).collect();
        let mut want = vec![(ids[3], 96, 144), (ids[4], 96, 120)];
        want.sort_by_key(|x| (x.1, x.0 .0));
        assert_eq!(left, want);
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

    /// On a primary track with gaps, deleting or lifting selected clips,
    /// one of them the matte of a clip on another track, is one undo step
    /// that puts the whole document back as it was: the gaps, the other
    /// clips, and the matte binding the removal cleared.
    #[test]
    fn a_removal_undoes_to_the_exact_document() {
        for lift in [false, true] {
            let mut state = AppState { magnetic_timeline_enabled: true, ..Default::default() };
            let seq = &mut state.sequences[0];
            seq.add_video_track("V2".into());
            let primary = seq.tracks.iter().position(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary).unwrap();
            let free = seq.tracks.iter().position(|t| t.name == "V2").unwrap();
            let c = |a: f64, b: f64| Clip::media(klipp_core::MediaId::new(), t(a), t(b), t(0.0), t(b - a));
            // The primary is gapless: its gaps are gap clips.
            let prim: Vec<Clip> = [(0.0, 4.0), (14.0, 18.0), (26.0, 29.0), (34.0, 37.0)].iter().map(|&(a, b)| c(a, b)).collect();
            let ids: Vec<ClipId> = prim.iter().map(|c| c.id).collect();
            let gaps = [(4.0, 14.0), (18.0, 26.0), (29.0, 34.0)].map(|(a, b)| Clip::gap(t(a), t(b)));
            let mut prim: Vec<Clip> = prim.into_iter().chain(gaps).collect();
            prim.sort_by_key(|c| c.timeline_in.value);
            let mut matted = c(26.0, 29.0);
            matted.matte_source = Some(klipp_timeline::MatteBinding::new(ids[2]));
            seq.tracks[primary].clips = prim;
            seq.tracks[free].clips.push(matted);
            let before = serde_json::to_value(&state.sequences).unwrap();
            assert_eq!(remove_clips(&mut state, &[ids[1], ids[2]], lift, "Remove"), Ok(2));
            let after = serde_json::to_value(&state.sequences).unwrap();
            assert!(state.sequence().all_clips().all(|c| c.matte_source.is_none()), "lift {lift}: the binding to a removed clip is cleared");
            undo(&mut state);
            assert!(serde_json::to_value(&state.sequences).unwrap() == before, "lift {lift}: one undo puts the whole document back");
            let entry = state.active_history_mut().redo().expect("the step again");
            let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps };
            let _ = view.apply_forward(&entry.command);
            assert!(serde_json::to_value(&state.sequences).unwrap() == after, "lift {lift}: one redo as the removal left it");
        }
    }

    /// The Premiere persona's crash (#3060): on a gapless primary, a clip
    /// lifted (a gap clip in its place), the last clip nudged later by
    /// the document's move, then a clip deleted: the nudge's hole is a
    /// gap clip, so the delete's magnetic edit finds the primary gapless
    /// (no debug panic, nothing packed), and undoing all three steps
    /// gives the document back exactly.
    #[test]
    fn a_lift_a_nudge_and_a_delete_keep_the_primary_gapless() {
        let mut state = AppState { magnetic_timeline_enabled: true, ..Default::default() };
        let p = state.sequence().tracks.iter().position(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary).unwrap();
        let c = |a: f64, b: f64| Clip::media(klipp_core::MediaId::new(), t(a), t(b), t(0.0), t(b - a));
        let clips: Vec<Clip> = [(0.0, 2.0), (2.0, 4.0), (4.0, 6.0), (6.0, 8.0)].iter().map(|&(a, b)| c(a, b)).collect();
        let ids: Vec<ClipId> = clips.iter().map(|c| c.id).collect();
        state.sequences[0].tracks[p].clips = clips;
        let original = serde_json::to_value(&state.sequences).unwrap();
        remove_clips(&mut state, &[ids[0]], true, "Lift").unwrap();
        // Alt+. on the last clip, ten frames: the document's move.
        let ops = crate::timeline::moves::drag_move_ops(state.sequence(), &[(ids[3], t(6.0 + 10.0 / 24.0), None)], true);
        let plan = klipp_document::ops::resolve(state.sequence(), &state.media_pool, &ops).expect("the nudge");
        let edits: Vec<EditCommand> = plan.steps.into_iter().map(|s| s.edit).collect();
        for e in &edits {
            let _ = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: 0, media_pool: &mut state.media_pool, fps: &mut state.fps }.apply_forward(e);
        }
        state.push_edits_as_one(&edits, "Nudge");
        assert!(state.sequence().primary_holes().is_none(), "the nudge's hole is a gap clip");
        remove_clips(&mut state, &[ids[1]], false, "Delete").unwrap();
        state.sequence().assert_primary_gapless();
        for _ in 0..3 {
            undo(&mut state);
        }
        assert!(serde_json::to_value(&state.sequences).unwrap() == original, "three undos give it back exactly");
    }

    /// An edit the app applies itself (not a planned op), leaving a hole
    /// on the primary, gets its gap clip when it is recorded: in the same
    /// step, so one undo takes both away.
    #[test]
    fn a_recorded_edit_that_leaves_a_hole_gets_its_gap_clip() {
        let mut state = AppState::default();
        let p = state.sequence().tracks.iter().position(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary).unwrap();
        let c = |a: f64, b: f64| Clip::media(klipp_core::MediaId::new(), t(a), t(b), t(0.0), t(b - a));
        state.sequences[0].tracks[p].clips = vec![c(0.0, 2.0), c(2.0, 4.0)];
        let original = serde_json::to_value(&state.sequences).unwrap();
        let old = state.sequences[0].tracks[p].clips[0].clone();
        let mut new = old.clone();
        new.timeline_out = t(1.5);
        new.source_out = t(1.5);
        state.sequences[0].tracks[p].clips[0] = new.clone();
        state.push_history(EditCommand::ReplaceClip { description: "Trim", old: Box::new(old), new: Box::new(new) });
        assert!(state.sequence().tracks[p].clips.iter().any(|c| c.kind.is_gap() && c.timeline_in == t(1.5) && c.timeline_out == t(2.0)), "a gap clip 1.5-2 s");
        undo(&mut state);
        assert!(serde_json::to_value(&state.sequences).unwrap() == original);
    }
}
