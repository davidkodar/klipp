//! A clip dropped where a drag let it go, along its track or onto another
//! of its kind: laid over what is there, as an editor's overwrite does,
//! its linked partner with it (#3467). Set straight onto a track, a clip
//! dropped over another left the two over each other.

use crate::history::EditCommand;
use crate::state::AppState;
use klipp_core::{ClipId, Timecode};

/// Whether the track `clip_id` is on is locked.
fn track_is_locked(state: &AppState, clip_id: &ClipId) -> bool {
    state.sequence().tracks.iter().find(|t| t.clips.iter().any(|c| c.id == *clip_id)).is_some_and(|t| t.locked)
}

/// The clip `id`, already where it was dropped on track `ti`, laid over
/// what else is there: what it covers out, what it overlaps trimmed, one
/// it falls inside split. The edits that do so, for the same undo step.
fn lay_over(state: &mut AppState, ti: usize, id: ClipId) -> Vec<EditCommand> {
    let seq = state.sequence();
    let Some(clip) = seq.tracks.get(ti).and_then(|t| t.find_clip(id)).cloned() else { return Vec::new() };
    let edits = klipp_document::clipboard::lay_over_edits(seq, ti, &clip);
    apply(state, &edits);
    edits
}

/// The sound linked to each clip a drop on track `ti` cut (as `before`
/// had it), cut the same way: a picture covered whole takes its sound
/// off, one trimmed trims it, one split in two splits it, each piece of
/// sound linked to its piece of picture. The edits, made on the state, for
/// the same undo step. A drop over a clip with its sound under it cut the
/// picture and left the sound whole: the two ran 93 frames apart. Only
/// sound in step with its picture is cut; `moved` (the dropped clip) and
/// its own sound are the move's.
fn mirror_onto_linked_sound(state: &mut AppState, before: &klipp_timeline::Sequence, ti: usize, moved: ClipId) -> Vec<EditCommand> {
    use std::cmp::Ordering::Equal;
    let after = state.sequence().clone();
    let (Some(was), Some(now)) = (before.tracks.get(ti), after.tracks.get(ti)) else { return Vec::new() };
    let known: std::collections::HashSet<ClipId> = was.clips.iter().map(|c| c.id).collect();
    let mut edits = Vec::new();
    for c in was.clips.iter().filter(|c| c.id != moved && !c.kind.is_gap()) {
        let Some(pid) = c.linked_clip_id else { continue };
        let Some((pt, pi)) = before.find_clip(pid) else { continue };
        let p = &before.tracks[pt].clips[pi];
        if pt == ti || p.timeline_in.cmp(&c.timeline_in) != Equal || p.timeline_out.cmp(&c.timeline_out) != Equal {
            continue;
        }
        // What is left of `c`: its own id, and pieces cut from it (they
        // keep its link).
        let pieces: Vec<&klipp_timeline::Clip> = now.clips.iter().filter(|x| x.id == c.id || (!known.contains(&x.id) && x.linked_clip_id == Some(pid))).collect();
        let untouched = pieces.len() == 1 && pieces[0].id == c.id && pieces[0].timeline_in.cmp(&c.timeline_in) == Equal && pieces[0].timeline_out.cmp(&c.timeline_out) == Equal && pieces[0].source_in == c.source_in;
        if untouched {
            continue;
        }
        // The sound for a piece of picture: its span, its source moved as
        // the picture's was.
        let shaped = |piece: &klipp_timeline::Clip| {
            let mut s = p.clone();
            s.timeline_in = piece.timeline_in;
            s.timeline_out = piece.timeline_out;
            s.source_in = p.source_in + (piece.source_in - c.source_in);
            s.source_out = s.source_in + (piece.source_out - piece.source_in);
            s.linked_clip_id = Some(piece.id);
            s
        };
        let keeps_id = pieces.iter().any(|x| x.id == c.id);
        if !keeps_id {
            edits.push(EditCommand::LegacyRemoveClip { clip: p.clone(), track_index: pt, position: pi });
        }
        for piece in pieces {
            if piece.id == c.id {
                edits.push(EditCommand::ReplaceClip { description: "Overwrite", old: Box::new(p.clone()), new: Box::new(shaped(piece)) });
            } else {
                let mut sound = shaped(piece);
                sound.id = ClipId::new();
                let mut relinked = piece.clone();
                relinked.linked_clip_id = Some(sound.id);
                edits.push(EditCommand::LegacyAppendClip { clip: sound, track_index: pt });
                edits.push(EditCommand::ReplaceClip { description: "Overwrite", old: Box::new(piece.clone()), new: Box::new(relinked) });
            }
        }
    }
    apply(state, &edits);
    edits
}

/// `edits` made on the state, so it is what the history holds: a piece
/// split off is the history's own, which undo then takes off. Laid over
/// apart from its edits, the piece had another id, and undo left it.
fn apply(state: &mut AppState, edits: &[EditCommand]) {
    let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps };
    for e in edits {
        view.apply_forward(e);
    }
}

/// The linked partner `lid` moved by `delta` on its own track, laid over
/// what is there (#3467).
pub(super) fn partner_over(state: &mut AppState, lid: ClipId, delta: Timecode) -> Option<Vec<EditCommand>> {
    let seq = state.sequence();
    let (ti, ci) = seq.find_clip(lid)?;
    let mut moved = seq.tracks[ti].clips[ci].clone();
    moved.timeline_in += delta;
    moved.timeline_out += delta;
    let edits = klipp_document::clipboard::lay_over_edits(seq, ti, &moved);
    apply(state, &edits);
    Some(edits)
}

/// `edit` in the history, with `more` after it as one undo step when
/// there is any.
fn push_with(state: &mut AppState, edit: EditCommand, more: Vec<EditCommand>) {
    if more.is_empty() {
        state.push_history(edit);
        return;
    }
    let batching = state.active_history_mut().is_batching();
    if !batching {
        state.active_history_mut().begin_batch();
    }
    state.push_history(edit);
    for e in more {
        state.push_history(e);
    }
    if !batching {
        state.active_history_mut().end_batch_as("Move Clip");
    }
}

/// `clip_id` moved along its track to start at `new_timeline_in`, laid
/// over what is there; its linked partner and its group by the same
/// amount.
pub(super) fn move_clip(state: &mut AppState, clip_id: ClipId, new_timeline_in: Timecode) {
    // Snapshot the link/group info and compute the delta before
    // we start mutating. The linked partner (set up by
    // ImportMediaLinked for A/V pairs) must follow the primary
    // by the same delta so linked clips stay phase-locked.
    let (group_id, linked_id, delta) = {
        let clip = state.sequence().tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == clip_id);
        match clip {
            // The Linked switch off moves the clip alone (#916).
            Some(c) => (c.group_id, c.linked_clip_id.filter(|_| state.linked_selection), new_timeline_in - c.timeline_in),
            None => (None, None, klipp_core::RationalTime { value: 0, rate: 1 }),
        }
    };

    let before = state.sequence().clone();
    // Move primary clip and capture its old/new bounds.
    let mut primary_history: Option<(Timecode, Timecode, Timecode, Timecode)> = None;
    let mut on_track = None;
    if let Some((ti, track)) = state.sequence_mut().tracks.iter_mut().enumerate().find(|(_, t)| t.clips.iter().any(|c| c.id == clip_id)) {
        if let Some(clip) = track.find_clip_mut(clip_id) {
            let old_in = clip.timeline_in;
            let old_out = clip.timeline_out;
            let dur = clip.duration();
            clip.timeline_in = new_timeline_in;
            clip.timeline_out = new_timeline_in + dur;
            primary_history = Some((old_in, old_out, clip.timeline_in, clip.timeline_out));
        }
        track.sort_clips();
        on_track = Some(ti);
    }
    let cover = match (on_track, delta.value != 0) {
        (Some(ti), true) => {
            let mut edits = lay_over(state, ti, clip_id);
            edits.extend(mirror_onto_linked_sound(state, &before, ti, clip_id));
            edits
        }
        _ => Vec::new(),
    };

    // Move the linked partner by the same delta — but skip if
    // it's on a locked track, or if it's already in the same
    // group (group propagation below would double-move it).
    let partner_in_same_group = match (linked_id, group_id) {
        (Some(lid), Some(gid)) => state.sequence().tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == lid).is_some_and(|c| c.group_id == Some(gid)),
        _ => false,
    };
    let partner_edits = linked_id.filter(|lid| !partner_in_same_group && !track_is_locked(state, lid) && delta.value != 0).and_then(|lid| partner_over(state, lid, delta));

    if let Some((old_in, old_out, new_in, new_out)) = primary_history {
        let move_clip = EditCommand::LegacyMoveClip { clip_id, old_timeline_in: old_in, old_timeline_out: old_out, new_timeline_in: new_in, new_timeline_out: new_out, linked_partner: None };
        push_with(state, move_clip, cover.into_iter().chain(partner_edits.into_iter().flatten()).collect());
    }

    // Propagate move to grouped clips (skip primary AND the
    // already-moved linked partner to avoid double-moving).
    if let Some(gid) = group_id {
        if delta.value != 0 {
            for track in &mut state.sequence_mut().tracks {
                for clip in &mut track.clips {
                    if clip.group_id == Some(gid) && clip.id != clip_id && Some(clip.id) != linked_id {
                        clip.timeline_in += delta;
                        clip.timeline_out += delta;
                    }
                }
                track.sort_clips();
            }
        }
    }
}

/// `clip_id` moved from track `from` onto track `to`, starting at
/// `new_in`, laid over what is there; its linked partner by the same
/// amount on its own track. Whether it moved.
pub(super) fn move_to_track(state: &mut AppState, clip_id: ClipId, new_in: Timecode, from: usize, to: usize) -> bool {
    let Some(clip) = state.sequence().tracks.get(from).and_then(|t| t.find_clip(clip_id)).cloned() else { return false };
    if state.sequence().tracks.get(to).is_none() {
        return false;
    }
    let old_in = clip.timeline_in;
    let old_out = clip.timeline_out;
    let dur = clip.duration();
    let linked_id = clip.linked_clip_id;
    let delta = new_in - old_in;
    let before = state.sequence().clone();
    let mut moved_clip = clip.clone();
    moved_clip.timeline_in = new_in;
    moved_clip.timeline_out = new_in + dur;
    state.sequence_mut().tracks[from].remove_clip(clip_id);
    let dst_track = &mut state.sequence_mut().tracks[to];
    dst_track.clips.push(moved_clip.clone());
    dst_track.sort_clips();
    let mut cover = lay_over(state, to, clip_id);
    cover.extend(mirror_onto_linked_sound(state, &before, to, clip_id));

    // Shift the linked partner on its own track by the
    // same delta (A/V partners live on different track
    // kinds, so the partner doesn't cross tracks — only
    // its timeline bounds move).
    let partner_edits = linked_id.filter(|lid| !track_is_locked(state, lid) && delta.value != 0).and_then(|lid| partner_over(state, lid, delta));

    let move_clip = EditCommand::LegacyMoveClipToTrack { clip: moved_clip, old_timeline_in: old_in, old_timeline_out: old_out, new_timeline_in: new_in, from_track: from, to_track: to, linked_partner: None };
    push_with(state, move_clip, cover.into_iter().chain(partner_edits.into_iter().flatten()).collect());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::{Clip, TrackKind};

    fn t(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    fn clip(a: f64, b: f64) -> Clip {
        Clip::media(klipp_core::MediaId::new(), t(a), t(b), t(0.0), t(b - a))
    }

    fn undo(state: &mut AppState) {
        let entry = state.active_history_mut().undo().expect("a step to undo");
        let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps };
        view.apply_inverse(&entry.command);
    }

    fn redo(state: &mut AppState) {
        let entry = state.active_history_mut().redo().expect("a step to redo");
        let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps };
        view.apply_forward(&entry.command);
    }

    /// Each track's clips in seconds, in time order.
    fn spans(state: &AppState) -> Vec<Vec<(f64, f64)>> {
        state.sequence().tracks.iter().map(|tr| tr.clips.iter().map(|c| (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64())).collect()).collect()
    }

    fn no_overlaps(state: &AppState) -> bool {
        spans(state).iter().all(|tr| tr.windows(2).all(|w| w[0].1 <= w[1].0 + 1e-9))
    }

    /// V1 (the first video track) with a 0-5 s picture linked to its sound
    /// on A1, and V2 holding a 10-20 s clip; their indices.
    fn timeline() -> (AppState, usize, usize, ClipId) {
        let mut state = AppState::default();
        let seq = state.sequence_mut();
        seq.add_video_track("V2".into());
        let v1 = seq.tracks.iter().position(|t| t.kind == TrackKind::Video && t.name != "V2").unwrap();
        let v2 = seq.tracks.iter().position(|t| t.name == "V2").unwrap();
        let a1 = seq.tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
        let (mut pic, mut sound) = (clip(0.0, 5.0), clip(0.0, 5.0));
        pic.linked_clip_id = Some(sound.id);
        sound.linked_clip_id = Some(pic.id);
        let id = pic.id;
        seq.tracks[v1].clips.push(pic);
        seq.tracks[a1].clips.push(sound);
        seq.tracks[v2].clips.push(clip(10.0, 20.0));
        state.linked_selection = true;
        (state, v1, v2, id)
    }

    /// V1 primary with three clips end to end, A 0-4, B 4-8 (no sound) and
    /// C 8-`c_end` with its sound linked on A1; their ids and indices.
    fn primary_line(c_end: f64) -> (AppState, usize, usize, [ClipId; 3], ClipId) {
        let mut state = AppState::default();
        let seq = state.sequence_mut();
        let v1 = seq.primary_track_index().expect("a primary track");
        let a1 = seq.tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
        let (a, b) = (clip(0.0, 4.0), clip(4.0, 8.0));
        let (mut c, mut sound) = (clip(8.0, c_end), clip(8.0, c_end));
        c.linked_clip_id = Some(sound.id);
        sound.linked_clip_id = Some(c.id);
        let ids = [a.id, b.id, c.id];
        let sid = sound.id;
        seq.tracks[v1].clips = vec![a, b, c];
        seq.tracks[a1].clips = vec![sound];
        state.linked_selection = true;
        (state, v1, a1, ids, sid)
    }

    /// Each picture on `v1` linked to a sound on `a1` runs with it: same
    /// span, same source.
    fn in_sync(state: &AppState, v1: usize, a1: usize) -> bool {
        let seq = state.sequence();
        seq.tracks[v1].clips.iter().filter_map(|p| p.linked_clip_id.map(|s| (p, s))).all(|(p, s)| {
            seq.tracks[a1].find_clip(s).is_some_and(|s| s.timeline_in == p.timeline_in && s.timeline_out == p.timeline_out && s.source_in == p.source_in && s.linked_clip_id == Some(p.id))
        })
    }

    /// An overwrite-move on the primary track over a clip with its sound
    /// under it: the moved clip lands where it was dropped, a gap holds its
    /// place, and what it covers of the clip there goes from the picture
    /// and the sound alike; one undo puts it all back. It slid back to
    /// where it was, the clip it covered lost its start, and that clip's
    /// sound stayed whole, 93 frames out of step.
    #[test]
    fn an_overwrite_move_over_a_linked_clip_cuts_its_sound_with_it() {
        let (mut state, v1, a1, [_, b, c], sound) = primary_line(12.0);
        let before = spans(&state);
        move_clip(&mut state, b, t(7.75));
        let seq = state.sequence();
        let pic = |id| seq.tracks[v1].find_clip(id).map(|c| (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64(), c.source_in.to_secs_f64()));
        assert_eq!(pic(b), Some((7.75, 11.75, 0.0)), "B where it was dropped");
        assert_eq!(pic(c), Some((11.75, 12.0, 3.75)), "C's picture loses what B covers");
        let s = seq.tracks[a1].find_clip(sound).map(|c| (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64(), c.source_in.to_secs_f64()));
        assert_eq!(s, Some((11.75, 12.0, 3.75)), "and so does its sound");
        assert!(seq.tracks[v1].clips.iter().any(|x| x.kind.is_gap() && x.timeline_in == t(4.0) && x.timeline_out == t(7.75)), "a gap holds B's place: {:?}", spans(&state)[v1]);
        assert!(in_sync(&state, v1, a1) && no_overlaps(&state), "{:?}", spans(&state));
        let after = spans(&state);
        undo(&mut state);
        assert_eq!(spans(&state), before, "one undo");
        redo(&mut state);
        assert_eq!(spans(&state), after, "redone");
    }

    /// Dropped inside a longer linked clip: the clip splits round it and
    /// so does its sound, each piece of sound linked to its piece of
    /// picture, in step.
    #[test]
    fn an_overwrite_move_inside_a_linked_clip_splits_its_sound_too() {
        let (mut state, v1, a1, [_, b, _], _) = primary_line(20.0);
        move_clip(&mut state, b, t(12.0));
        assert_eq!(spans(&state)[a1], vec![(8.0, 12.0), (16.0, 20.0)], "the sound round the drop");
        assert!(in_sync(&state, v1, a1) && no_overlaps(&state), "{:?}", spans(&state));
        undo(&mut state);
        assert_eq!(spans(&state)[a1], vec![(8.0, 20.0)]);
        assert!(in_sync(&state, v1, a1));
    }

    /// A clip dragged onto V2 over the clip there is laid over it, as its
    /// sound is on A1: the clip there split round it, nothing over
    /// anything; undo puts every clip back, redo lays it again. Set
    /// straight on the track, the two were over each other.
    #[test]
    fn a_clip_dropped_onto_a_clip_on_another_track_is_laid_over_it() {
        let (mut state, v1, v2, id) = timeline();
        let before = spans(&state);
        assert!(move_to_track(&mut state, id, t(12.0), v1, v2));
        assert!(no_overlaps(&state), "{:?}", spans(&state));
        assert_eq!(spans(&state)[v2], vec![(10.0, 12.0), (12.0, 17.0), (17.0, 20.0)], "split round the clip dropped");
        let after = spans(&state);
        undo(&mut state);
        assert_eq!(spans(&state), before, "undone");
        redo(&mut state);
        assert_eq!(spans(&state), after, "redone");
    }

    /// A linked sound moved with its picture to inside a longer sound
    /// splits it, and undo takes the piece split off away again: laid
    /// over apart from its edits, the piece had another id and stayed.
    #[test]
    fn undo_takes_off_the_piece_a_moved_sound_split_off() {
        let (mut state, v1, _, id) = timeline();
        let a1 = state.sequence().tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
        state.sequence_mut().tracks[a1].clips.push(clip(10.0, 30.0));
        let before = spans(&state);
        move_clip(&mut state, id, t(15.0));
        assert!(no_overlaps(&state), "{:?}", spans(&state));
        assert_eq!(spans(&state)[a1], vec![(10.0, 15.0), (15.0, 20.0), (20.0, 30.0)], "split round the sound");
        let _ = v1;
        undo(&mut state);
        assert_eq!(spans(&state), before, "undone");
    }

    /// Along its own track the same: moved over the next clip, it is laid
    /// over that clip's head.
    #[test]
    fn a_clip_moved_onto_the_next_clip_on_its_track_is_laid_over_it() {
        let (mut state, _, v2, _) = timeline();
        let next = clip(0.0, 4.0);
        let id = next.id;
        state.sequence_mut().tracks[v2].clips.insert(0, next);
        let before = spans(&state);
        move_clip(&mut state, id, t(8.0));
        assert!(no_overlaps(&state), "{:?}", spans(&state));
        assert_eq!(spans(&state)[v2], vec![(8.0, 12.0), (12.0, 20.0)], "the next clip's head under it taken off");
        let after = spans(&state);
        undo(&mut state);
        assert_eq!(spans(&state), before, "undone");
        redo(&mut state);
        assert_eq!(spans(&state), after, "redone");
    }
}
