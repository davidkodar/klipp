//! Insert edit on free (non-magnetic) tracks.
//!
//! The track-based NLE's insert, as Premiere, Resolve and Avid do it: at
//! the edit point every unlocked track is cut, and everything from that
//! point on moves right by the inserted clip's length. Tracks stay in sync
//! with each other *because* they all move; a clip straddling the point is
//! split, and the part after the cut still exists — it now sits after the
//! inserted clip, source position intact. A locked track is left alone,
//! which is the opt-out those NLEs offer through sync lock, auto-select
//! and track patching.
//!
//! Before this, the legacy path rippled one track only — the first of the
//! target's kind — and shifted a straddling clip whole instead of
//! splitting it, so an insert into V1 with audio on A1 and an overlay on
//! V2 put all three out of step with each other, and none of it was
//! recorded for undo. The magnetic path (`magnetic::insert_primary`) was
//! right; this brings the free-track path level with it.
//!
//! The operation returns an [`InsertEdit`] holding everything needed to
//! undo it exactly and redo it identically — the split halves with their
//! final ids and links, the old and new bounds of every moved clip, every
//! re-pointed transition and every changed link — so the app's history
//! stores the record and calls back in.

use klipp_core::{ClipId, RationalTime, Timecode, TransitionId};

use crate::sequence::{Clip, Sequence};

/// A clip that straddled the edit point and was split there.
#[derive(Debug, Clone)]
pub struct Split {
    pub track_index: usize,
    /// Where the original sat in the track's clip list.
    pub position: usize,
    pub original: Clip,
    /// The two halves as they are once the edit is complete — after the
    /// right half has moved and the links have been settled — so a redo
    /// re-inserts them as they were.
    pub left: Clip,
    pub right: Clip,
}

/// A clip that was already on the timeline at or after the edit point and
/// moved right by the inserted length.
#[derive(Debug, Clone)]
pub struct Shifted {
    pub track_index: usize,
    pub id: ClipId,
    pub old_in: RationalTime,
    pub old_out: RationalTime,
    pub new_in: RationalTime,
    pub new_out: RationalTime,
}

/// A transition that was attached to a clip that got split, re-pointed at
/// the half it now belongs to.
#[derive(Debug, Clone)]
pub struct RetargetedTransition {
    pub track_index: usize,
    pub id: TransitionId,
    pub old: (ClipId, ClipId),
    pub new: (ClipId, ClipId),
}

/// A surviving clip whose `linked_clip_id` changed because its partner was
/// split (the halves themselves are recorded whole in [`Split`]).
#[derive(Debug, Clone)]
pub struct Relinked {
    pub track_index: usize,
    pub id: ClipId,
    pub old: Option<ClipId>,
    pub new: Option<ClipId>,
}

/// Everything one insert edit did, for undo and redo.
#[derive(Debug, Clone)]
pub struct InsertEdit {
    /// The track the clip went on.
    pub track_index: usize,
    /// The inserted clip, at its timeline position.
    pub clip: Clip,
    pub splits: Vec<Split>,
    pub shifted: Vec<Shifted>,
    pub transitions: Vec<RetargetedTransition>,
    pub relinked: Vec<Relinked>,
}

impl Sequence {
    /// Insert `clip` on track `track_index` at `at`, rippling every
    /// unlocked track. `None` if the target track is locked or does not
    /// exist — nothing is changed in that case.
    pub fn insert_edit(&mut self, track_index: usize, mut clip: Clip, at: Timecode) -> Option<InsertEdit> {
        if self.tracks.get(track_index)?.locked {
            return None;
        }
        let dur = clip.timeline_out - clip.timeline_in;
        clip.timeline_in = at;
        clip.timeline_out = at + dur;

        let mut splits = Vec::new();
        let mut shifted = Vec::new();
        let mut transitions = Vec::new();

        for (ti, track) in self.tracks.iter_mut().enumerate() {
            if track.locked {
                continue;
            }
            // Split whatever straddles the point. A well-formed track has
            // at most one such clip; overlapping clips get one split each.
            let straddling: Vec<ClipId> = track
                .clips
                .iter()
                .filter(|c| c.timeline_in < at && at < c.timeline_out)
                .map(|c| c.id)
                .collect();
            let mut halves = Vec::new();
            for id in straddling {
                let Some(position) = track.clips.iter().position(|c| c.id == id) else { continue };
                let original = track.clips[position].clone();
                let Some((left, right)) = track.split_at(id, at) else { continue };
                // A transition into the clip's head now leads into the
                // left half; one out of its tail now leaves the right.
                for t in &mut track.transitions {
                    if t.clip_a_id == id || t.clip_b_id == id {
                        let old = (t.clip_a_id, t.clip_b_id);
                        if t.clip_b_id == id {
                            t.clip_b_id = left.id;
                        }
                        if t.clip_a_id == id {
                            t.clip_a_id = right.id;
                        }
                        transitions.push(RetargetedTransition { track_index: ti, id: t.id, old, new: (t.clip_a_id, t.clip_b_id) });
                    }
                }
                halves.push((left.id, right.id));
                splits.push(Split { track_index: ti, position, original, left, right });
            }
            // Everything from the point on moves right — the right halves
            // included, but they are recorded as halves, not as shifts.
            for c in &mut track.clips {
                if c.timeline_in >= at {
                    let (old_in, old_out) = (c.timeline_in, c.timeline_out);
                    c.timeline_in += dur;
                    c.timeline_out += dur;
                    if !halves.iter().any(|(_, r)| *r == c.id) {
                        shifted.push(Shifted { track_index: ti, id: c.id, old_in, old_out, new_in: c.timeline_in, new_out: c.timeline_out });
                    }
                }
            }
        }

        let relinked = self.settle_links_after_splits(&splits);

        // Record the halves as they finally are: moved, and linked.
        for s in &mut splits {
            let track = &self.tracks[s.track_index];
            if let Some(l) = track.find_clip(s.left.id) {
                s.left = l.clone();
            }
            if let Some(r) = track.find_clip(s.right.id) {
                s.right = r.clone();
            }
        }

        let track = &mut self.tracks[track_index];
        track.clips.push(clip.clone());
        track.sort_clips();

        Some(InsertEdit { track_index, clip, splits, shifted, transitions, relinked })
    }

    /// A split clip's halves inherit its `linked_clip_id`, which names a
    /// partner that may itself have been split, or may be whole but still
    /// pointing at an id that no longer exists. Settle both cases: halves
    /// of a split pair link across (left to left, right to right); a whole
    /// partner links to the half it shares time with, and the other half
    /// lets go of it.
    fn settle_links_after_splits(&mut self, splits: &[Split]) -> Vec<Relinked> {
        let mut relinked = Vec::new();
        for s in splits {
            let Some(partner_id) = s.original.linked_clip_id else { continue };
            if let Some(p) = splits.iter().find(|o| o.original.id == partner_id) {
                // Both split at the same point: pair the halves. The
                // partner's own iteration sets its side.
                if let Some(l) = self.tracks[s.track_index].find_clip_mut(s.left.id) {
                    l.linked_clip_id = Some(p.left.id);
                }
                if let Some(r) = self.tracks[s.track_index].find_clip_mut(s.right.id) {
                    r.linked_clip_id = Some(p.right.id);
                }
                continue;
            }
            // The partner survived whole (it did not straddle the point:
            // it lies entirely before, or moved entirely after). It keeps
            // the half on its side of the cut.
            let Some((pt, partner)) = self
                .tracks
                .iter()
                .enumerate()
                .find_map(|(ti, t)| t.find_clip(partner_id).map(|c| (ti, c)))
            else {
                continue;
            };
            let partner_before_cut = partner.timeline_in < s.left.timeline_out;
            let (keeps, lets_go) = if partner_before_cut { (s.left.id, s.right.id) } else { (s.right.id, s.left.id) };
            let old = partner.linked_clip_id;
            if let Some(p) = self.tracks[pt].find_clip_mut(partner_id) {
                p.linked_clip_id = Some(keeps);
            }
            relinked.push(Relinked { track_index: pt, id: partner_id, old, new: Some(keeps) });
            if let Some(c) = self.tracks[s.track_index].find_clip_mut(lets_go) {
                c.linked_clip_id = None;
            }
        }
        relinked
    }

    /// Put the sequence back exactly as it was before `edit`.
    pub fn undo_insert_edit(&mut self, edit: &InsertEdit) {
        if let Some(track) = self.tracks.get_mut(edit.track_index) {
            track.remove_clip(edit.clip.id);
        }
        for r in &edit.relinked {
            if let Some(c) = self.tracks.get_mut(r.track_index).and_then(|t| t.find_clip_mut(r.id)) {
                c.linked_clip_id = r.old;
            }
        }
        for s in &edit.shifted {
            if let Some(c) = self.tracks.get_mut(s.track_index).and_then(|t| t.find_clip_mut(s.id)) {
                c.timeline_in = s.old_in;
                c.timeline_out = s.old_out;
            }
        }
        for t in &edit.transitions {
            if let Some(tr) = self.tracks.get_mut(t.track_index).and_then(|tk| tk.transitions.iter_mut().find(|x| x.id == t.id)) {
                tr.clip_a_id = t.old.0;
                tr.clip_b_id = t.old.1;
            }
        }
        for s in edit.splits.iter().rev() {
            if let Some(track) = self.tracks.get_mut(s.track_index) {
                track.clips.retain(|c| c.id != s.left.id && c.id != s.right.id);
                let pos = s.position.min(track.clips.len());
                track.clips.insert(pos, s.original.clone());
            }
        }
        for track in &mut self.tracks {
            track.sort_clips();
        }
    }

    /// Apply `edit` again, reproducing the same ids and positions.
    pub fn redo_insert_edit(&mut self, edit: &InsertEdit) {
        for s in &edit.splits {
            if let Some(track) = self.tracks.get_mut(s.track_index) {
                track.remove_clip(s.original.id);
                track.clips.push(s.left.clone());
                track.clips.push(s.right.clone());
            }
        }
        for t in &edit.transitions {
            if let Some(tr) = self.tracks.get_mut(t.track_index).and_then(|tk| tk.transitions.iter_mut().find(|x| x.id == t.id)) {
                tr.clip_a_id = t.new.0;
                tr.clip_b_id = t.new.1;
            }
        }
        for s in &edit.shifted {
            if let Some(c) = self.tracks.get_mut(s.track_index).and_then(|t| t.find_clip_mut(s.id)) {
                c.timeline_in = s.new_in;
                c.timeline_out = s.new_out;
            }
        }
        for r in &edit.relinked {
            if let Some(c) = self.tracks.get_mut(r.track_index).and_then(|t| t.find_clip_mut(r.id)) {
                c.linked_clip_id = r.new;
            }
        }
        if let Some(track) = self.tracks.get_mut(edit.track_index) {
            track.clips.push(edit.clip.clone());
        }
        for track in &mut self.tracks {
            track.sort_clips();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sequence::{Track, TrackKind, Transition};
    use klipp_core::{MediaId, TransitionAlignment, TransitionKind, TransitionUserParams};

    const FPS: u32 = 25;

    fn tc(f: i64) -> Timecode {
        RationalTime::from_frames(f, FPS)
    }

    fn clip(from: i64, to: i64) -> Clip {
        Clip::media(MediaId::new(), tc(from), tc(to), tc(0), tc(to - from))
    }

    /// A linked pair: the same span on V1 and A1.
    fn linked(from: i64, to: i64) -> (Clip, Clip) {
        let mut v = clip(from, to);
        let mut a = clip(from, to);
        v.linked_clip_id = Some(a.id);
        a.linked_clip_id = Some(v.id);
        (v, a)
    }

    fn seq(tracks: Vec<(TrackKind, Vec<Clip>)>) -> Sequence {
        let mut s = Sequence::new_default();
        s.fps = klipp_core::Fps::new(FPS, 1);
        s.tracks = tracks
            .into_iter()
            .enumerate()
            .map(|(i, (kind, clips))| {
                let mut t = Track::new(&format!("{kind:?} {i}"), kind);
                t.clips = clips;
                t
            })
            .collect();
        s
    }

    /// The observable shape of a sequence: for every clip on every track,
    /// its bounds, source range and link. `Clip` has no `PartialEq`, and
    /// this is what an undo has to put back.
    type Shape = Vec<Vec<(ClipId, i64, i64, i64, i64, Option<ClipId>)>>;
    fn shape(s: &Sequence) -> Shape {
        let f = |t: RationalTime| t.value * FPS as i64 / t.rate as i64;
        s.tracks
            .iter()
            .map(|t| t.clips.iter().map(|c| (c.id, f(c.timeline_in), f(c.timeline_out), f(c.source_in), f(c.source_out), c.linked_clip_id)).collect())
            .collect()
    }
    fn bounds(s: &Sequence, ti: usize) -> Vec<(i64, i64)> {
        shape(s)[ti].iter().map(|c| (c.1, c.2)).collect()
    }

    #[test]
    fn every_unlocked_track_is_cut_and_rippled_so_they_stay_in_sync() {
        // V1: a clip across the edit point. A1: its audio. V2: an overlay
        // that starts after the point.
        let (v, a) = linked(0, 100);
        let overlay = clip(60, 80);
        let mut s = seq(vec![(TrackKind::Video, vec![overlay]), (TrackKind::Video, vec![v]), (TrackKind::Audio, vec![a])]);

        let e = s.insert_edit(1, clip(0, 10), tc(40)).expect("insert");

        // V1: left half, the insert, right half — with the right half's
        // source picking up exactly where the left's leaves off.
        assert_eq!(bounds(&s, 1), vec![(0, 40), (40, 50), (50, 110)]);
        let v1 = &shape(&s)[1];
        assert_eq!((v1[0].3, v1[0].4), (0, 40), "left half keeps the head of the source");
        assert_eq!((v1[2].3, v1[2].4), (40, 100), "right half continues the source where the cut was");
        // A1 was cut and moved by the same amount: still in sync with V1.
        assert_eq!(bounds(&s, 2), vec![(0, 40), (50, 110)]);
        // V2's overlay, entirely after the point, moved with everything else.
        assert_eq!(bounds(&s, 0), vec![(70, 90)]);
        assert_eq!(e.splits.len(), 2);
        assert_eq!(e.shifted.len(), 1, "only the overlay was a plain shift; the right halves are recorded as halves");
    }

    #[test]
    fn a_locked_track_is_neither_cut_nor_moved() {
        let mut s = seq(vec![(TrackKind::Video, vec![clip(0, 100)]), (TrackKind::Video, vec![clip(0, 100)])]);
        s.tracks[1].locked = true;
        let before = bounds(&s, 1);

        s.insert_edit(0, clip(0, 10), tc(40)).expect("insert");

        assert_eq!(bounds(&s, 0), vec![(0, 40), (40, 50), (50, 110)]);
        assert_eq!(bounds(&s, 1), before, "the locked track changed");
    }

    #[test]
    fn a_locked_target_track_refuses_the_edit_and_changes_nothing() {
        let mut s = seq(vec![(TrackKind::Video, vec![clip(0, 100)]), (TrackKind::Video, vec![clip(0, 100)])]);
        s.tracks[0].locked = true;
        let before = shape(&s);
        assert!(s.insert_edit(0, clip(0, 10), tc(40)).is_none());
        assert_eq!(shape(&s), before);
    }

    #[test]
    fn at_a_clip_boundary_nothing_is_split() {
        let mut s = seq(vec![(TrackKind::Video, vec![clip(0, 40), clip(40, 100)])]);
        let e = s.insert_edit(0, clip(0, 10), tc(40)).expect("insert");
        assert!(e.splits.is_empty());
        assert_eq!(bounds(&s, 0), vec![(0, 40), (40, 50), (50, 110)]);
    }

    #[test]
    fn one_frame_into_a_clip_still_splits_it() {
        // The blade tool refuses a cut within a frame of an edge, which is
        // a click-tolerance rule. An insert lands where the playhead is.
        let mut s = seq(vec![(TrackKind::Video, vec![clip(0, 100)])]);
        s.insert_edit(0, clip(0, 10), tc(1)).expect("insert");
        assert_eq!(bounds(&s, 0), vec![(0, 1), (1, 11), (11, 110)]);
    }

    #[test]
    fn split_linked_pairs_are_linked_half_to_half() {
        let (v, a) = linked(0, 100);
        let mut s = seq(vec![(TrackKind::Video, vec![v]), (TrackKind::Audio, vec![a])]);
        s.insert_edit(0, clip(0, 10), tc(40)).expect("insert");
        let (v1, a1) = (&shape(&s)[0], &shape(&s)[1]);
        // v1: [left, insert, right]; a1: [left, right]
        assert_eq!(v1[0].5, Some(a1[0].0), "video left half links to audio left half");
        assert_eq!(a1[0].5, Some(v1[0].0));
        assert_eq!(v1[2].5, Some(a1[1].0), "video right half links to audio right half");
        assert_eq!(a1[1].5, Some(v1[2].0));
    }

    #[test]
    fn a_whole_partner_keeps_the_half_on_its_side_of_the_cut() {
        // Video 0..100, its audio trimmed to 0..30: the audio lies wholly
        // before the cut at 40, so it stays linked to the left half and the
        // right half lets go of it.
        let (v, mut a) = linked(0, 100);
        a.timeline_out = tc(30);
        a.source_out = tc(30);
        let a_id = a.id;
        let mut s = seq(vec![(TrackKind::Video, vec![v]), (TrackKind::Audio, vec![a])]);
        let e = s.insert_edit(0, clip(0, 10), tc(40)).expect("insert");
        let v1 = &shape(&s)[0];
        assert_eq!(v1[0].5, Some(a_id), "left half keeps the audio");
        assert_eq!(v1[2].5, None, "right half has no partner");
        assert_eq!(s.tracks[1].find_clip(a_id).unwrap().linked_clip_id, Some(v1[0].0), "the audio now names the left half");
        assert_eq!(e.relinked.len(), 1);
    }

    #[test]
    fn a_transition_follows_the_half_it_belongs_to() {
        let (x, y) = (clip(0, 50), clip(50, 100));
        let (x_id, y_id) = (x.id, y.id);
        let mut s = seq(vec![(TrackKind::Video, vec![x, y])]);
        s.tracks[0].transitions.push(Transition {
            id: TransitionId::new(),
            kind: TransitionKind::CrossDissolve,
            duration: tc(10),
            clip_a_id: x_id,
            clip_b_id: y_id,
            user_params: TransitionUserParams::default(),
            alignment: TransitionAlignment::Center,
        });
        // Cut inside x, well before the transition.
        let e = s.insert_edit(0, clip(0, 10), tc(20)).expect("insert");
        let right = e.splits[0].right.id;
        let t = &s.tracks[0].transitions[0];
        assert_eq!((t.clip_a_id, t.clip_b_id), (right, y_id), "the dissolve out of x now leaves x's right half");
        assert_eq!(e.transitions.len(), 1);
    }

    #[test]
    fn undo_restores_the_sequence_exactly_and_redo_reproduces_the_edit() {
        let (v, a) = linked(0, 100);
        let (v2, a2) = linked(100, 150);
        let overlay = clip(60, 80);
        let (v_id, v2_id) = (v.id, v2.id);
        let mut s = seq(vec![(TrackKind::Video, vec![overlay]), (TrackKind::Video, vec![v, v2]), (TrackKind::Audio, vec![a, a2])]);
        s.tracks[1].transitions.push(Transition {
            id: TransitionId::new(),
            kind: TransitionKind::CrossDissolve,
            duration: tc(10),
            clip_a_id: v_id,
            clip_b_id: v2_id,
            user_params: TransitionUserParams::default(),
            alignment: TransitionAlignment::Center,
        });
        let before = shape(&s);
        let transitions_before: Vec<_> = s.tracks[1].transitions.iter().map(|t| (t.clip_a_id, t.clip_b_id)).collect();

        let e = s.insert_edit(1, clip(0, 10), tc(40)).expect("insert");
        let after = shape(&s);
        let transitions_after: Vec<_> = s.tracks[1].transitions.iter().map(|t| (t.clip_a_id, t.clip_b_id)).collect();
        assert_ne!(before, after);

        s.undo_insert_edit(&e);
        assert_eq!(shape(&s), before, "undo did not restore the sequence");
        let t: Vec<_> = s.tracks[1].transitions.iter().map(|t| (t.clip_a_id, t.clip_b_id)).collect();
        assert_eq!(t, transitions_before, "undo did not restore the transition");

        s.redo_insert_edit(&e);
        assert_eq!(shape(&s), after, "redo did not reproduce the edit, ids included");
        let t: Vec<_> = s.tracks[1].transitions.iter().map(|t| (t.clip_a_id, t.clip_b_id)).collect();
        assert_eq!(t, transitions_after);

        s.undo_insert_edit(&e);
        assert_eq!(shape(&s), before, "a second undo diverged");
    }

    #[test]
    fn keyframes_and_markers_stay_with_their_half() {
        let mut c = clip(0, 100);
        klipp_core::keyframe::upsert_keyframe(&mut c.view_mut().opacity_keyframes.0, 0.4, 0.2);
        klipp_core::keyframe::upsert_keyframe(&mut c.view_mut().opacity_keyframes.0, 2.4, 0.8);
        let mut s = seq(vec![(TrackKind::Video, vec![c])]);
        s.insert_edit(0, clip(0, 10), tc(40)).expect("insert");
        let (left, right) = (&s.tracks[0].clips[0], &s.tracks[0].clips[2]);
        assert_eq!(left.opacity_keyframes().len(), 1);
        assert_eq!(right.opacity_keyframes().len(), 1);
        // The right half's keyframe is re-timed from its own start.
        assert!((right.picture.view().opacity_keyframes.0[0].time_secs - (2.4 - 1.6)).abs() < 1e-9);
    }
}
