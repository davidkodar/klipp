//! Free-track GUI edits are planned by the document before anything is
//! changed. Drag previews are restored by the panel before dispatch.

use klipp_core::{AppCommand, ClipId};
use klipp_document::{history::EditCommand, ops::{self, ClipRef, Edge, Op, Refusal}};
use klipp_media::MediaPool;
use klipp_timeline::Sequence;

/// Clip `id` (and its linked partner) taken out of a free track: the gap
/// closed (`ripple`, Delete) or left (Lift, #3060).
pub(crate) fn remove(seq: &Sequence, pool: &MediaPool, id: ClipId, ripple: bool) -> Result<EditCommand, Refusal> {
    let plan = ops::resolve(seq, pool, &[Op::Remove { clip_id: ClipRef::Id(id), ripple, linked: true }])?;
    let mut removed = vec![id];
    if let Some((ti, ci)) = seq.find_clip(id) {
        if let Some(partner) = seq.tracks[ti].clips[ci].linked_clip_id.filter(|p| seq.find_clip(*p).is_some()) {
            removed.push(partner);
        }
    }
    // Clear references only after planning succeeds, and record the
    // cleanup so undo restores bindings as well as the removed pair.
    let mut edits = Vec::new();
    for clip in seq.all_clips().filter(|c| !removed.contains(&c.id)) {
        let mut new = clip.clone();
        let mut changed = false;
        if new.matte_source.as_ref().is_some_and(|b| removed.contains(&b.source_clip_id)) {
            new.matte_source = None;
            changed = true;
        }
        if new.transform_link.as_ref().map(|l| l.target.referenced_clip_id()).is_some_and(|id| removed.contains(&id)) {
            new.transform_link = None;
            changed = true;
        }
        if new.linked_clip_id.is_some_and(|id| removed.contains(&id)) {
            new.linked_clip_id = None;
            changed = true;
        }
        if changed {
            edits.push(EditCommand::ReplaceClip { description: "Unlink", old: Box::new(clip.clone()), new: Box::new(new) });
        }
    }
    edits.extend(plan.steps.into_iter().map(|s| s.edit));
    Ok(EditCommand::Batch(edits))
}

pub(crate) fn trim(seq: &Sequence, pool: &MediaPool, cmd: &AppCommand) -> Result<Option<EditCommand>, String> {
    let AppCommand::TrimClip { clip_id, old_source_in, old_source_out, old_timeline_in, old_timeline_out,
        new_source_in, new_source_out, new_timeline_in, new_timeline_out, ripple, linked } = cmd else {
        return Err("not a trim request".into());
    };
    let old = [*old_source_in, *old_source_out, *old_timeline_in, *old_timeline_out];
    let new = [*new_source_in, *new_source_out, *new_timeline_in, *new_timeline_out];
    if old == new { return Ok(None); }
    let (ti, ci) = seq.find_clip(*clip_id).ok_or("the clip no longer exists")?;
    let clip = &seq.tracks[ti].clips[ci];
    if [clip.source_in, clip.source_out, clip.timeline_in, clip.timeline_out] != old {
        return Err("the clip changed during the trim; try again".into());
    }
    let (edge, target) = if new_source_in != old_source_in || new_timeline_in != old_timeline_in {
        (Edge::In, *old_timeline_in + (*new_source_in - *old_source_in))
    } else {
        (Edge::Out, *new_timeline_out)
    };
    ops::trim_at_linked(seq, pool, *clip_id, edge, target, *ripple, *linked)
        .map(Some).map_err(|r| r.why)
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Fps, RationalTime};
    use klipp_document::{apply::DocumentView, history::{CommandHistory, HistoryEntry}};
    use klipp_timeline::{Clip, MatteBinding, Track, TrackKind};

    fn tc(seconds: f64) -> RationalTime { RationalTime::from_secs_f64(seconds, 25) }

    fn fixture() -> (Sequence, MediaPool) {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/linked-trim.mov".into(), None);
        item.duration = tc(40.0);
        let id = pool.add(item);
        let mut video = Track::new("V1", TrackKind::Video);
        let mut audio = Track::new("A1", TrackKind::Audio);
        for n in 0..2 {
            let at = 2.0 + n as f64 * 4.0;
            let mut v = Clip::media(id, tc(at), tc(at + 4.0), tc(at + 8.0), tc(at + 12.0));
            // A separately recorded source: copying picture's absolute
            // source bounds would choose the wrong part of the sound.
            let mut a = Clip::media(id, tc(at), tc(at + 4.0), tc(at + 18.0), tc(at + 22.0));
            v.linked_clip_id = Some(a.id);
            a.linked_clip_id = Some(v.id);
            video.clips.push(v);
            audio.clips.push(a);
        }
        seq.tracks = vec![video, audio];
        (seq, pool)
    }

    fn command(c: &Clip, edge: Edge, delta: f64, ripple: bool) -> AppCommand {
        let d = tc(delta);
        let (si, so, ti, to) = match (edge, ripple) {
            (Edge::In, true) => (c.source_in + d, c.source_out, c.timeline_in, c.timeline_out - d),
            (Edge::In, false) => (c.source_in + d, c.source_out, c.timeline_in + d, c.timeline_out),
            (Edge::Out, _) => (c.source_in, c.source_out + d, c.timeline_in, c.timeline_out + d),
        };
        AppCommand::TrimClip { clip_id: c.id, old_source_in: c.source_in, old_source_out: c.source_out,
            old_timeline_in: c.timeline_in, old_timeline_out: c.timeline_out,
            new_source_in: si, new_source_out: so, new_timeline_in: ti, new_timeline_out: to, ripple, linked: true }
    }

    fn apply_and_round_trip(seq: Sequence, mut pool: MediaPool, edit: EditCommand) -> Sequence {
        let before = serde_json::to_value(&seq).unwrap();
        let mut fps = seq.fps;
        let mut sequences = vec![seq];
        let mut history = CommandHistory::new();
        history.begin_batch();
        DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(&edit);
        history.push(HistoryEntry { command: edit });
        history.end_batch_as("Edit");
        assert_eq!(history.undo_count(), 1);
        let after = serde_json::to_value(&sequences[0]).unwrap();
        // Saved timing and link identity also survive reloading.
        sequences[0] = serde_json::from_value(after.clone()).unwrap();
        let undo = history.undo().unwrap();
        DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_inverse(&undo.command);
        assert_eq!(serde_json::to_value(&sequences[0]).unwrap(), before);
        let redo = history.redo().unwrap();
        DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(&redo.command);
        assert_eq!(serde_json::to_value(&sequences[0]).unwrap(), after);
        sequences.remove(0)
    }

    #[test]
    fn gui_trims_preserve_linked_offsets_and_ripple_both_tracks_once() {
        for edge in [Edge::In, Edge::Out] {
            for ripple in [false, true] {
                for selected_track in 0..2 {
                    let (mut seq, pool) = fixture();
                    seq.tracks[1].clips[0].timeline_in -= tc(0.4);
                    seq.tracks[1].clips[0].source_in -= tc(0.4);
                    let cmd = command(&seq.tracks[selected_track].clips[0], edge, if edge == Edge::In { 1.0 } else { -1.0 }, ripple);
                    let edit = trim(&seq, &pool, &cmd).unwrap().unwrap();
                    let after = apply_and_round_trip(seq, pool, edit);
                    let (v, a) = (&after.tracks[0].clips, &after.tracks[1].clips);
                    assert_eq!(v[0].timeline_in - a[0].timeline_in, tc(0.4));
                    assert_eq!(a[0].source_in - v[0].source_in, tc(9.6));
                    assert_eq!(a[0].source_out - v[0].source_out, tc(10.0));
                    assert_eq!(v[1].timeline_in, tc(if ripple { 5.0 } else { 6.0 }));
                    assert_eq!(v[1].timeline_in, a[1].timeline_in);
                }
            }
        }
    }

    #[test]
    fn keyboard_trims_share_the_drag_planner() {
        for edge in [Edge::In, Edge::Out] {
            let (seq, pool) = fixture();
            let clip = &seq.tracks[0].clips[0];
            let target = if edge == Edge::In { tc(3.0) } else { tc(5.0) };
            let keyboard = ops::trim_at(&seq, &pool, clip.id, edge, target, true).unwrap();
            let drag = trim(&seq, &pool, &command(clip, edge, if edge == Edge::In { 1.0 } else { -1.0 }, true)).unwrap().unwrap();
            let a = apply_and_round_trip(seq.clone(), pool.clone(), keyboard);
            let b = apply_and_round_trip(seq, pool, drag);
            assert_eq!(serde_json::to_value(a).unwrap(), serde_json::to_value(b).unwrap());
        }
    }

    #[test]
    fn a_locked_or_exhausted_partner_refuses_the_whole_trim() {
        let (mut seq, pool) = fixture();
        let cmd = command(&seq.tracks[0].clips[0], Edge::Out, 1.0, true);
        seq.tracks[1].locked = true;
        let before = serde_json::to_value(&seq).unwrap();
        assert!(trim(&seq, &pool, &cmd).unwrap_err().contains("locked"));
        assert_eq!(serde_json::to_value(&seq).unwrap(), before);
        seq.tracks[1].locked = false;
        seq.tracks[1].clips[0].source_out = tc(40.0);
        assert!(trim(&seq, &pool, &cmd).unwrap_err().contains("file"));
    }

    #[test]
    fn gui_delete_plans_one_undoable_pair_removal() {
        for selected_track in 0..2 {
            let (mut seq, pool) = fixture();
            let removed_audio = seq.tracks[1].clips[0].id;
            seq.tracks[0].clips[1].matte_source = Some(MatteBinding::new(removed_audio));
            let id = seq.tracks[selected_track].clips[0].id;
            let edit = remove(&seq, &pool, id, true).unwrap();
            let after = apply_and_round_trip(seq, pool, edit);
            assert_eq!(after.tracks[0].clips.len(), 1);
            assert_eq!(after.tracks[1].clips.len(), 1);
            assert_eq!(after.tracks[0].clips[0].timeline_in, tc(2.0));
            assert_eq!(after.tracks[1].clips[0].timeline_in, tc(2.0));
            assert!(after.tracks[0].clips[0].matte_source.is_none());
        }
    }

    #[test]
    fn refused_deletes_leave_bindings_and_timing_untouched() {
        let (mut seq, pool) = fixture();
        let id = seq.tracks[0].clips[0].id;
        seq.tracks[0].clips[1].matte_source = Some(MatteBinding::new(id));
        seq.tracks[1].locked = true;
        let before = serde_json::to_value(&seq).unwrap();
        assert!(remove(&seq, &pool, id, true).unwrap_err().why.contains("locked"));
        assert_eq!(serde_json::to_value(&seq).unwrap(), before);
        seq.tracks[1].locked = false;
        seq.tracks[1].clips[0].timeline_in -= tc(0.4);
        assert!(remove(&seq, &pool, id, true).unwrap_err().why.contains("different timeline ranges"));
    }

    #[test]
    fn an_explicitly_unlinked_clip_can_still_be_edited_alone() {
        let (mut seq, pool) = fixture();
        seq.tracks[0].clips[0].linked_clip_id = None;
        seq.tracks[1].clips[0].linked_clip_id = None;
        seq.tracks[1].locked = true;
        let sound_before = serde_json::to_value(&seq.tracks[1]).unwrap();
        let id = seq.tracks[0].clips[0].id;
        let edit = trim(&seq, &pool, &command(&seq.tracks[0].clips[0], Edge::Out, -1.0, true)).unwrap().unwrap();
        let after = apply_and_round_trip(seq.clone(), pool.clone(), edit);
        assert_eq!(serde_json::to_value(&after.tracks[1]).unwrap(), sound_before);
        let edit = remove(&seq, &pool, id, true).unwrap();
        let after = apply_and_round_trip(seq, pool, edit);
        assert_eq!(serde_json::to_value(&after.tracks[1]).unwrap(), sound_before);
    }
}
