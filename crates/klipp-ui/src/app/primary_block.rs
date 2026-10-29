//! A selection dragged on the gapless primary track (#3060): its primary
//! clips move as one block by the magnetic editor's own edits, the rest
//! of the selection by the drag's offset on their tracks, all one undo
//! step that undoes and redoes to the exact document.
//!
//! In Insert, the block's clips leave gap clips where they were and go
//! in one after another at the slot the drag showed, what is there split
//! and what follows rippled on. In Overwrite, each leaves a gap clip and
//! overwrites what is at its start moved by the offset. Gap clips left
//! after the track's last clip are taken off, so the timeline doesn't
//! grow an empty tail.

use crate::history::EditCommand;
use crate::state::AppState;
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_timeline::magnetic::MagneticEditor;

/// `block` (primary clips and where each started) moved by `delta`, in
/// Insert or `overwrite`; `others` (clips off the primary and where each
/// goes) moved as the document moves them. Refused, nothing changed,
/// when the others' moves can't be made.
pub(crate) fn move_block(state: &mut AppState, block: &[(ClipId, Timecode)], others: &[(ClipId, Timecode, Option<usize>)], delta: RationalTime, overwrite: bool) -> Result<(), String> {
    let Some(p) = state.sequence().primary_track_index() else { return Err("no primary track".into()) };
    let ops = crate::timeline::moves::drag_move_ops(state.sequence(), others, state.linked_selection);
    let other_edits: Vec<EditCommand> = if ops.is_empty() {
        Vec::new()
    } else {
        klipp_document::ops::resolve(state.sequence(), &state.media_pool, &ops).map_err(|r| r.why)?.steps.into_iter().map(|s| s.edit).collect()
    };
    let mut block: Vec<(ClipId, Timecode)> = block.iter().copied().filter(|(id, _)| state.sequence().tracks[p].find_clip(*id).is_some()).collect();
    block.sort_by_key(|(_, at)| at.value);
    if block.is_empty() || delta.value == 0 {
        return Ok(());
    }
    let clips: Vec<klipp_timeline::Clip> = block.iter().filter_map(|(id, _)| state.sequence().tracks[p].find_clip(*id).cloned()).collect();
    let before = state.sequence().clone();
    let own_batch = !state.active_history_mut().is_batching();
    if own_batch {
        state.active_history_mut().begin_batch();
    }
    let magnetic = |state: &mut AppState, command| state.push_history(EditCommand::Magnetic(command));
    for (id, _) in &block {
        let r = MagneticEditor::lift(state.sequence_mut(), *id);
        magnetic(state, r.command);
    }
    let zero = |at: Timecode| if at.value < 0 { RationalTime { value: 0, rate: at.rate } } else { at };
    if overwrite {
        for (clip, (_, at)) in clips.into_iter().zip(&block) {
            let r = MagneticEditor::overwrite_primary(state.sequence_mut(), clip, zero(*at + delta));
            magnetic(state, r.command);
        }
    } else {
        let mut at = zero(block[0].1 + delta);
        for clip in clips {
            let len = clip.duration();
            let r = MagneticEditor::insert_primary(state.sequence_mut(), clip, at, None, Some(&klipp_timeline::magnetic::PLACED));
            magnetic(state, r.command);
            at += len;
        }
    }
    // Gap clips after the last clip: an empty tail, taken off.
    while let Some(last) = state.sequence().tracks[p].clips.last().filter(|c| c.kind.is_gap()).cloned() {
        let position = state.sequence().tracks[p].clips.len() - 1;
        state.sequence_mut().tracks[p].clips.pop();
        state.push_history(EditCommand::LegacyRemoveClip { clip: last, track_index: p, position });
    }
    if state.linked_selection {
        for f in MagneticEditor::follow_linked_partners(&before, state.sequence_mut()) {
            state.push_history(EditCommand::LegacyMoveClip { clip_id: f.clip_id, old_timeline_in: f.old_in, old_timeline_out: f.old_out, new_timeline_in: f.new_in, new_timeline_out: f.new_out, linked_partner: None });
        }
    }
    for e in other_edits {
        let _ = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps }.apply_forward(&e);
        state.push_history(e);
    }
    if own_batch {
        state.active_history_mut().end_batch_as("Move Clips");
    }
    state.dirty = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::Clip;

    fn t(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    /// The persona's track, gapless: 0-4, 14-18, 18-22, then A 26-29,
    /// B 29-32 and C 34-37, gap clips between; a clip on V2 selected too.
    fn state() -> (AppState, Vec<ClipId>, ClipId) {
        let mut state = AppState::default();
        state.sequences[0].add_video_track("V2".into());
        let p = state.sequence().primary_track_index().unwrap();
        let v2 = state.sequence().tracks.iter().position(|t| t.name == "V2").unwrap();
        let c = |a: f64, b: f64| Clip::media(klipp_core::MediaId::new(), t(a), t(b), t(0.0), t(b - a));
        let clips: Vec<Clip> = [(0.0, 4.0), (14.0, 18.0), (18.0, 22.0), (26.0, 29.0), (29.0, 32.0), (34.0, 37.0)].iter().map(|&(a, b)| c(a, b)).collect();
        let ids = clips.iter().map(|c| c.id).collect();
        state.sequences[0].tracks[p].clips = clips;
        state.sequences[0].fill_primary_holes();
        let free = c(10.0, 11.0);
        let fid = free.id;
        state.sequences[0].tracks[v2].clips.push(free);
        (state, ids, fid)
    }

    fn spans(state: &AppState) -> Vec<(bool, f64, f64)> {
        let p = state.sequence().primary_track_index().unwrap();
        state.sequence().tracks[p].clips.iter().map(|c| (c.kind.is_gap(), c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64())).collect()
    }

    fn undo(state: &mut AppState) {
        let entry = state.active_history_mut().undo().expect("a step");
        let _ = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps }.apply_inverse(&entry.command);
    }

    fn redo(state: &mut AppState) {
        let entry = state.active_history_mut().redo().expect("a step");
        let _ = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps }.apply_forward(&entry.command);
    }

    /// A, B and C moved 2 s later, with the V2 clip. In Insert they go in
    /// one after another at 28 s, what was after rippled on, their old
    /// places gap clips; in Overwrite each lands 2 s later over what was
    /// there. Both: the primary gapless, no empty tail, the V2 clip 2 s
    /// later, and one undo and one redo to the exact documents.
    #[test]
    fn a_block_moves_in_insert_and_overwrite_and_undoes_exactly() {
        for overwrite in [false, true] {
            let (mut state, ids, fid) = state();
            let original = serde_json::to_value(&state.sequences).unwrap();
            let block = [(ids[3], t(26.0)), (ids[4], t(29.0)), (ids[5], t(34.0))];
            move_block(&mut state, &block, &[(fid, t(12.0), None)], t(2.0), overwrite).unwrap();
            assert!(state.sequence().primary_holes().is_none(), "overwrite {overwrite}: gapless");
            #[cfg(debug_assertions)]
            state.sequence().assert_primary_gapless();
            let real: Vec<(f64, f64)> = spans(&state).into_iter().filter(|s| !s.0).map(|s| (s.1, s.2)).collect();
            let want: &[(f64, f64)] = if overwrite {
                &[(0.0, 4.0), (14.0, 18.0), (18.0, 22.0), (28.0, 31.0), (31.0, 34.0), (36.0, 39.0)]
            } else {
                &[(0.0, 4.0), (14.0, 18.0), (18.0, 22.0), (28.0, 31.0), (31.0, 34.0), (34.0, 37.0)]
            };
            assert_eq!(real, want, "overwrite {overwrite}");
            assert!(!spans(&state).last().unwrap().0, "overwrite {overwrite}: no empty tail");
            let p = state.sequence().primary_track_index().unwrap();
            let order: Vec<ClipId> = state.sequence().tracks[p].clips.iter().filter(|c| !c.kind.is_gap()).map(|c| c.id).collect();
            assert_eq!(order, ids, "overwrite {overwrite}: the clips in their order, none lost");
            let free = state.sequence().all_clips().find(|c| c.id == fid).unwrap();
            assert_eq!(free.timeline_in, t(12.0), "overwrite {overwrite}: the V2 clip by the offset");
            let moved = serde_json::to_value(&state.sequences).unwrap();
            undo(&mut state);
            assert!(serde_json::to_value(&state.sequences).unwrap() == original, "overwrite {overwrite}: one undo, the exact document");
            redo(&mut state);
            assert!(serde_json::to_value(&state.sequences).unwrap() == moved, "overwrite {overwrite}: one redo, the moved document");
        }
    }
}
