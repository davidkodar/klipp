//! The blade: a clip cut in two at a time, and the clip it is linked
//! to cut with it, as one gesture. The halves are linked crosswise —
//! left with left, right with right — and the recorded edits carry
//! the halves as linked, so a redo brings the pair back linked too.
//!
//! One rule for every caller, whichever kind of track the clips are
//! on: the cut must lie more than a frame inside a clip to cut it, and
//! a linked pair the cut runs through is cut together or not at all — a
//! pair like that across a lock is refused whole, so a lock never leaves
//! picture and sound out of step. A partner the cut misses is not cut
//! either way, so a lock over it refuses nothing.

use crate::history::EditCommand;
use klipp_core::{ClipId, Fps, RationalTime};
use klipp_timeline::magnetic::MagneticEditor;
use klipp_timeline::magnetic_commands::MagneticEditCommand;
use klipp_timeline::{Clip, Sequence};

/// Why a blade did not happen. Nothing changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BladeRefusal {
    /// The clip is gone, or the cut is not more than a frame inside it.
    /// A blade that close to an edge is meant for the edge, and a blade
    /// queued for a clip already cut through its partner lands on the
    /// new edge: neither is worth a word.
    NotInside,
    /// The clip's track is locked, and no unlocked partner is affected.
    Locked {
        /// The locked track's name.
        track: String,
    },
    /// The clip and the clip it is linked to would both be cut, and they
    /// lie either side of a lock.
    LockedAcrossPair {
        /// The locked track's name.
        locked: String,
        /// The other track's name.
        unlocked: String,
    },
}

impl BladeRefusal {
    /// What to tell the user, when there is something to tell.
    pub fn message(&self) -> Option<String> {
        match self {
            BladeRefusal::NotInside => None,
            BladeRefusal::Locked { track } => Some(format!("{track} is locked")),
            BladeRefusal::LockedAcrossPair { locked, unlocked } => Some(format!(
                "{locked} is locked and its clip is linked to one on {unlocked}: unlock {locked} or lock {unlocked} too"
            )),
        }
    }
}

/// Whether `cut` lies inside `clip` by more than a frame at `fps`: the
/// blade's edge rule, the same on a legacy and a magnetic track, so a
/// pair split across the two kinds is cut on both or on neither. The
/// frame is the sequence's, not a single tick of the clip's rate, which
/// at 29.97 is a thirtieth of the frame it stands for.
pub fn cut_is_inside(clip: &Clip, cut: RationalTime, fps: Fps) -> bool {
    let frame = klipp_timeline::one_frame(fps);
    cut > clip.timeline_in + frame && cut < clip.timeline_out - frame
}

fn overlaps(a: &Clip, b: &Clip) -> bool {
    a.timeline_in < b.timeline_out && a.timeline_out > b.timeline_in
}

/// How much of `a` lies over `b`, in seconds; zero when they do not meet.
fn overlap_secs(a: &Clip, b: &Clip) -> f64 {
    if !overlaps(a, b) {
        return 0.0;
    }
    let from = a.timeline_in.max(b.timeline_in);
    let to = a.timeline_out.min(b.timeline_out);
    (to - from).to_secs_f64()
}

/// Cut `clip_id` on `track_index` at `cut`, and the clip it is linked
/// to with it where the cut lies inside that clip too, the halves
/// linked crosswise. `magnetic(track)` says which tracks cut the
/// magnetic way. The edits are made on `seq` and returned in the order
/// made, one per clip cut and, for a partner left whole, its relink;
/// the caller records them as one entry. A partner left whole keeps
/// only the half that still lies over it, and each half keeps the
/// partner only while it lies over it.
pub fn blade_linked(
    seq: &mut Sequence,
    track_index: usize,
    clip_id: ClipId,
    cut: RationalTime,
    magnetic: impl Fn(usize) -> bool,
) -> Result<Vec<EditCommand>, BladeRefusal> {
    let fps = seq.fps;
    let clip = seq
        .tracks
        .get(track_index)
        .and_then(|t| t.find_clip(clip_id))
        .filter(|c| cut_is_inside(c, cut, fps))
        .cloned()
        .ok_or(BladeRefusal::NotInside)?;

    let partner = clip.linked_clip_id.and_then(|pid| seq.find_clip(pid)).map(|(pti, pci)| (pti, seq.tracks[pti].clips[pci].clone()));

    // Only a partner the cut lies inside would be split, and only a
    // split partner can put the gesture either side of a lock: sound
    // that ends before the cut — a J-cut's tail — would not change at
    // all, so a lock over it has nothing to protect.
    let cut_partner: Option<usize> = partner.as_ref().filter(|(_, p)| cut_is_inside(p, cut, fps)).map(|(pti, _)| *pti);
    let name = |ti: usize| seq.tracks[ti].name.clone();
    let locked = seq.tracks[track_index].locked;
    match (locked, cut_partner.map(|pti| seq.tracks[pti].locked)) {
        (true, Some(false)) => {
            return Err(BladeRefusal::LockedAcrossPair { locked: name(track_index), unlocked: name(cut_partner.unwrap()) });
        }
        (false, Some(true)) => {
            return Err(BladeRefusal::LockedAcrossPair { locked: name(cut_partner.unwrap()), unlocked: name(track_index) });
        }
        (true, _) => return Err(BladeRefusal::Locked { track: name(track_index) }),
        (false, _) => {}
    }

    let mut edits = Vec::new();
    let (mut edit, a, b) = cut_one(seq, track_index, clip_id, cut, magnetic(track_index)).ok_or(BladeRefusal::NotInside)?;

    match partner {
        Some((pti, p)) if cut_is_inside(&p, cut, fps) => {
            if let Some((mut p_edit, pa, pb)) = cut_one(seq, pti, p.id, cut, magnetic(pti)) {
                relink(seq, track_index, a, Some(pa));
                relink(seq, track_index, b, Some(pb));
                relink(seq, pti, pa, Some(a));
                relink(seq, pti, pb, Some(b));
                carry_halves(&mut edit, seq);
                carry_halves(&mut p_edit, seq);
                edits.push(edit);
                edits.push(p_edit);
                return Ok(edits);
            }
            // The partner would not cut after all: it stays whole.
            edits.extend(relink_whole_partner(seq, track_index, a, b, pti, &p, clip.id));
        }
        Some((pti, p)) => {
            edits.extend(relink_whole_partner(seq, track_index, a, b, pti, &p, clip.id));
        }
        None => {
            // A link to a clip that is gone is not carried into the halves.
            relink(seq, track_index, a, None);
            relink(seq, track_index, b, None);
        }
    }
    carry_halves(&mut edit, seq);
    edits.insert(0, edit);
    Ok(edits)
}

/// Cut one clip its track's way. Returns the edit and the halves' ids.
fn cut_one(seq: &mut Sequence, ti: usize, id: ClipId, cut: RationalTime, magnetic: bool) -> Option<(EditCommand, ClipId, ClipId)> {
    let fps = seq.fps;
    if magnetic {
        let result = MagneticEditor::blade(seq, id, cut)?;
        let MagneticEditCommand::Blade { left_clip, right_clip, .. } = &result.command else { return None };
        let (a, b) = (left_clip.id, right_clip.id);
        Some((EditCommand::Magnetic(result.command), a, b))
    } else {
        let track = seq.tracks.get_mut(ti)?;
        let position = track.clips.iter().position(|c| c.id == id)?;
        let original_clip = track.clips[position].clone();
        let (clip_a, clip_b) = track.blade_at(id, cut, fps)?;
        let (a, b) = (clip_a.id, clip_b.id);
        Some((EditCommand::LegacyBladeClip { track_index: ti, original_clip, clip_a, clip_b, position }, a, b))
    }
}

fn relink(seq: &mut Sequence, ti: usize, id: ClipId, to: Option<ClipId>) {
    if let Some(c) = seq.tracks[ti].find_clip_mut(id) {
        c.linked_clip_id = to;
    }
}

/// A partner the cut did not reach: each half keeps it only while it
/// lies over it, and the partner, if it pointed at the clip that was
/// cut, points at the half that lies over it most — or at nothing.
fn relink_whole_partner(seq: &mut Sequence, ti: usize, a: ClipId, b: ClipId, pti: usize, partner: &Clip, original: ClipId) -> Vec<EditCommand> {
    let mut best: Option<(ClipId, f64)> = None;
    for id in [a, b] {
        let Some(half) = seq.tracks[ti].find_clip(id) else { continue };
        let over = overlap_secs(half, partner);
        let keep = over > 0.0;
        if keep && best.is_none_or(|(_, o)| over > o) {
            best = Some((id, over));
        }
        relink(seq, ti, id, keep.then_some(partner.id));
    }
    if partner.linked_clip_id != Some(original) {
        return Vec::new();
    }
    let want = best.map(|(id, _)| id);
    if want == partner.linked_clip_id {
        return Vec::new();
    }
    relink(seq, pti, partner.id, want);
    let mut new = partner.clone();
    new.linked_clip_id = want;
    vec![EditCommand::ReplaceClip { description: "Blade Clip", old: Box::new(partner.clone()), new: Box::new(new) }]
}

/// Copy the halves as they now are — links mended — into the edit that
/// records the cut, so a redo re-inserts them linked.
fn carry_halves(edit: &mut EditCommand, seq: &Sequence) {
    let (ti, left, right) = match edit {
        EditCommand::LegacyBladeClip { track_index, clip_a, clip_b, .. } => (*track_index, clip_a, clip_b),
        EditCommand::Magnetic(MagneticEditCommand::Blade { track_index, left_clip, right_clip, .. }) => (*track_index, left_clip, right_clip),
        _ => return,
    };
    for half in [left, right] {
        if let Some(live) = seq.tracks[ti].find_clip(half.id) {
            *half = live.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::DocumentView;
    use crate::history::{CommandHistory, HistoryEntry};
    use klipp_core::MediaId;
    use klipp_media::MediaPool;
    use klipp_timeline::{Track, TrackKind, TrackRole};

    fn tc(f: i64) -> RationalTime {
        RationalTime::from_frames(f, 25)
    }
    fn clip(from: i64, to: i64) -> Clip {
        Clip::media(MediaId::new(), tc(from), tc(to), tc(0), tc(to - from))
    }

    /// V1 and A1, a clip on each spanning the given frames, linked both
    /// ways. Returns the sequence and the two clip ids.
    fn pair(v: (i64, i64), a: (i64, i64)) -> (Sequence, ClipId, ClipId) {
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        let mut v1 = Track::new("V1", TrackKind::Video);
        let mut a1 = Track::new("A1", TrackKind::Audio);
        v1.magnetic_role = TrackRole::Free;
        a1.magnetic_role = TrackRole::Free;
        let (mut vc, mut ac) = (clip(v.0, v.1), clip(a.0, a.1));
        vc.linked_clip_id = Some(ac.id);
        ac.linked_clip_id = Some(vc.id);
        let (vid, aid) = (vc.id, ac.id);
        v1.clips = vec![vc];
        a1.clips = vec![ac];
        seq.tracks = vec![v1, a1];
        (seq, vid, aid)
    }

    /// Each track's clips as (id, in, out, link), in frames.
    type Shape = Vec<Vec<(ClipId, i64, i64, Option<ClipId>)>>;
    fn shape(s: &Sequence) -> Shape {
        let f = |t: RationalTime| t.value * 25 / t.rate as i64;
        s.tracks.iter().map(|t| t.clips.iter().map(|c| (c.id, f(c.timeline_in), f(c.timeline_out), c.linked_clip_id)).collect()).collect()
    }

    /// The blade as the app records it: every edit in one batch.
    fn blade(seq: &mut Sequence, history: &mut CommandHistory, cut: i64, magnetic: bool) -> Result<usize, BladeRefusal> {
        let vid = seq.tracks[0].clips[0].id;
        let edits = blade_linked(seq, 0, vid, tc(cut), |_| magnetic)?;
        let n = edits.len();
        history.begin_batch();
        for e in edits {
            history.push(HistoryEntry { command: e });
        }
        history.end_batch();
        Ok(n)
    }

    fn undo(seq: &mut Sequence, history: &mut CommandHistory) {
        let entry = history.undo().expect("an entry to undo");
        let mut sequences = vec![seq.clone()];
        let (mut pool, mut fps) = (MediaPool::new(), seq.fps);
        DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_inverse(&entry.command);
        seq.tracks = sequences.remove(0).tracks;
    }

    fn redo(seq: &mut Sequence, history: &mut CommandHistory) {
        let entry = history.redo().expect("an entry to redo");
        let mut sequences = vec![seq.clone()];
        let (mut pool, mut fps) = (MediaPool::new(), seq.fps);
        DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(&entry.command);
        seq.tracks = sequences.remove(0).tracks;
    }

    /// The halves on each track, left then right, crosswise-linked.
    fn assert_cross_linked(seq: &Sequence, cut: i64) {
        let s = shape(seq);
        let (v, a) = (&s[0], &s[1]);
        assert_eq!(v.len(), 2, "video halves: {v:?}");
        assert_eq!(a.len(), 2, "audio halves: {a:?}");
        assert_eq!((v[0].2, v[1].1, a[0].2, a[1].1), (cut, cut, cut, cut), "the cut is at {cut}: {s:?}");
        assert_eq!(v[0].3, Some(a[0].0), "left video half links the left audio half: {s:?}");
        assert_eq!(a[0].3, Some(v[0].0), "left audio half links the left video half: {s:?}");
        assert_eq!(v[1].3, Some(a[1].0), "right video half links the right audio half: {s:?}");
        assert_eq!(a[1].3, Some(v[1].0), "right audio half links the right video half: {s:?}");
    }

    #[test]
    fn a_linked_pair_is_one_entry_that_undoes_and_redoes_with_its_links() {
        for magnetic in [false, true] {
            let (mut seq, _, _) = pair((0, 250), (0, 250));
            let before = shape(&seq);
            let mut history = CommandHistory::new();

            assert_eq!(blade(&mut seq, &mut history, 125, magnetic), Ok(2), "magnetic {magnetic}");
            assert_eq!(history.undo_count(), 1, "one entry for the pair");
            assert_eq!(history.undo_label(), Some(if magnetic { "Blade" } else { "Blade Clip" }), "the entry takes the blade's own name");
            assert_cross_linked(&seq, 125);
            let after = shape(&seq);

            undo(&mut seq, &mut history);
            assert_eq!(shape(&seq), before, "undo restores both clips with their links (magnetic {magnetic})");

            redo(&mut seq, &mut history);
            assert_eq!(shape(&seq), after, "redo reproduces the halves with their cross-links (magnetic {magnetic})");
            assert_cross_linked(&seq, 125);
        }
    }

    /// #429: the cut is not an end of the clip, so the blade leaves no
    /// fade at the join it makes — on either kind of track — and undo
    /// brings the clip's own fades back.
    #[test]
    fn a_blade_leaves_no_fade_at_the_join_it_makes() {
        for magnetic in [false, true] {
            let (mut seq, _, _) = pair((0, 250), (0, 250));
            for t in seq.tracks.iter_mut() {
                t.clips[0].fade_in_secs = 0.5;
                t.clips[0].fade_out_secs = 0.75;
            }
            let mut history = CommandHistory::new();
            blade(&mut seq, &mut history, 125, magnetic).unwrap();
            for t in &seq.tracks {
                assert_eq!((t.clips[0].fade_in_secs, t.clips[0].fade_out_secs), (0.5, 0.0), "the left half does not fade out at the cut (magnetic {magnetic})");
                assert_eq!((t.clips[1].fade_in_secs, t.clips[1].fade_out_secs), (0.0, 0.75), "the right half does not fade in at the cut (magnetic {magnetic})");
            }

            undo(&mut seq, &mut history);
            for t in &seq.tracks {
                assert_eq!(t.clips.len(), 1, "magnetic {magnetic}");
                assert_eq!((t.clips[0].fade_in_secs, t.clips[0].fade_out_secs), (0.5, 0.75), "undo restores the clip's own fades (magnetic {magnetic})");
            }
        }
    }

    /// A fade the cut leaves stranded on too short a half is shortened to
    /// it, on either kind of track, and undo brings the clip's own fades
    /// back at their full length. A 10 s clip fading in over 8 s and out
    /// over 9 s, cut in the middle, leaves 5 s of each.
    #[test]
    fn a_fade_too_long_for_its_half_is_shortened_to_it() {
        for magnetic in [false, true] {
            let (mut seq, _, _) = pair((0, 250), (0, 250));
            for t in seq.tracks.iter_mut() {
                t.clips[0].fade_in_secs = 8.0;
                t.clips[0].fade_out_secs = 9.0;
            }
            let mut history = CommandHistory::new();
            blade(&mut seq, &mut history, 125, magnetic).unwrap();
            for t in &seq.tracks {
                assert_eq!((t.clips[0].fade_in_secs, t.clips[0].fade_out_secs), (5.0, 0.0), "the left half fades in over its whole length (magnetic {magnetic})");
                assert_eq!((t.clips[1].fade_in_secs, t.clips[1].fade_out_secs), (0.0, 5.0), "the right half fades out over its whole length (magnetic {magnetic})");
            }

            undo(&mut seq, &mut history);
            for t in &seq.tracks {
                assert_eq!(t.clips.len(), 1, "magnetic {magnetic}");
                assert_eq!((t.clips[0].fade_in_secs, t.clips[0].fade_out_secs), (8.0, 9.0), "undo restores the fades at their own length (magnetic {magnetic})");
            }
        }
    }

    /// #406: the halves pair by position, not by an equal out-time, so a
    /// sound that runs on past its picture is still re-linked.
    #[test]
    fn a_longer_audio_tail_pairs_the_right_halves_too() {
        for magnetic in [false, true] {
            let (mut seq, _, _) = pair((0, 250), (0, 300));
            let mut history = CommandHistory::new();
            assert_eq!(blade(&mut seq, &mut history, 125, magnetic), Ok(2));
            assert_cross_linked(&seq, 125);
            let s = shape(&seq);
            assert_eq!((s[0][1].2, s[1][1].2), (250, 300), "the right halves keep their own tails: {s:?}");

            let after = s;
            undo(&mut seq, &mut history);
            redo(&mut seq, &mut history);
            assert_eq!(shape(&seq), after);
        }
    }

    /// #406: a partner the cut does not reach stays whole, linked to the
    /// half that lies over it; the half that does not is unlinked.
    #[test]
    fn a_partner_the_cut_misses_keeps_only_the_half_over_it() {
        for magnetic in [false, true] {
            // Sound under the head only: the left half keeps it.
            let (mut seq, _, aid) = pair((0, 250), (0, 100));
            let before = shape(&seq);
            let mut history = CommandHistory::new();
            blade(&mut seq, &mut history, 150, magnetic).unwrap();
            let s = shape(&seq);
            assert_eq!(s[0].len(), 2);
            assert_eq!(s[1].len(), 1, "the audio was not cut: {s:?}");
            assert_eq!(s[0][0].3, Some(aid), "the left half lies over the sound: {s:?}");
            assert_eq!(s[0][1].3, None, "the right half does not: {s:?}");
            assert_eq!(s[1][0].3, Some(s[0][0].0), "the sound points at the left half: {s:?}");
            let after = s;
            undo(&mut seq, &mut history);
            assert_eq!(shape(&seq), before);
            redo(&mut seq, &mut history);
            assert_eq!(shape(&seq), after);

            // Sound under the tail only: the right half keeps it, and the
            // sound follows it — on a magnetic track that means leaving
            // the left half, which kept the original id.
            let (mut seq, _, aid) = pair((0, 250), (200, 300));
            let mut history = CommandHistory::new();
            blade(&mut seq, &mut history, 125, magnetic).unwrap();
            let s = shape(&seq);
            assert_eq!(s[0][0].3, None, "{s:?}");
            assert_eq!(s[0][1].3, Some(aid), "{s:?}");
            assert_eq!(s[1][0].3, Some(s[0][1].0), "the sound points at the right half: {s:?}");
            assert_eq!(history.undo_count(), 1);
        }
    }

    /// #407: a pair across a lock is refused whole, whichever side is
    /// locked, and the toast names both tracks.
    #[test]
    fn a_pair_across_a_lock_is_refused_whole() {
        let (mut seq, vid, _) = pair((0, 250), (0, 250));
        let before = shape(&seq);
        seq.tracks[1].locked = true;
        let r = blade_linked(&mut seq, 0, vid, tc(125), |_| false).err();
        assert_eq!(r, Some(BladeRefusal::LockedAcrossPair { locked: "A1".into(), unlocked: "V1".into() }));
        assert_eq!(r.unwrap().message().as_deref(), Some("A1 is locked and its clip is linked to one on V1: unlock A1 or lock V1 too"));
        assert_eq!(shape(&seq), before, "nothing was cut");

        seq.tracks[1].locked = false;
        seq.tracks[0].locked = true;
        let r = blade_linked(&mut seq, 0, vid, tc(125), |_| true).err();
        assert_eq!(r, Some(BladeRefusal::LockedAcrossPair { locked: "V1".into(), unlocked: "A1".into() }));
        assert_eq!(shape(&seq), before);

        seq.tracks[1].locked = true;
        assert_eq!(blade_linked(&mut seq, 0, vid, tc(125), |_| false).err(), Some(BladeRefusal::Locked { track: "V1".into() }));
        assert_eq!(shape(&seq), before);
    }

    /// #435: a lock over a partner the cut does not reach protects
    /// nothing — that clip was never going to be cut — so the blade goes
    /// ahead on the clicked clip alone. V1 0-10 s over A1 0-4 s, a J-cut
    /// tail on a locked track, bladed at 6 s: the sound is nowhere near
    /// the cut.
    #[test]
    fn a_locked_partner_the_cut_misses_refuses_nothing() {
        for magnetic in [false, true] {
            let (mut seq, _, aid) = pair((0, 250), (0, 100));
            seq.tracks[1].locked = true;
            let mut history = CommandHistory::new();
            blade(&mut seq, &mut history, 150, magnetic).unwrap_or_else(|r| panic!("the blade went ahead (magnetic {magnetic}): {r:?}"));
            let s = shape(&seq);
            assert_eq!(s[0].len(), 2, "the picture was cut: {s:?}");
            assert_eq!(s[1].len(), 1, "the locked sound was not: {s:?}");
            assert_eq!((s[1][0].1, s[1][0].2), (0, 100), "and did not move: {s:?}");
            assert_eq!(s[0][0].3, Some(aid), "the left half still links the sound: {s:?}");
            assert_eq!(s[0][1].3, None, "the right half lies past it: {s:?}");
            assert_eq!(s[1][0].3, Some(s[0][0].0), "the sound links the half over it: {s:?}");

            // The clicked clip's own track locked is still a refusal, and
            // one about that track alone: no pair is being cut.
            let (mut seq, vid, _) = pair((0, 250), (0, 100));
            seq.tracks[0].locked = true;
            let before = shape(&seq);
            assert_eq!(
                blade_linked(&mut seq, 0, vid, tc(150), |_| magnetic).err(),
                Some(BladeRefusal::Locked { track: "V1".into() }),
                "magnetic {magnetic}"
            );
            assert_eq!(shape(&seq), before, "nothing was cut (magnetic {magnetic})");
        }
    }

    /// #407: the edge rule is the legacy blade's — more than a frame
    /// inside — on both kinds of track, so a pair across the two kinds
    /// is never cut on one side only.
    #[test]
    fn a_cut_within_a_frame_of_an_edge_is_refused_on_every_kind_of_track() {
        let (mut seq, vid, _) = pair((0, 250), (0, 250));
        let before = shape(&seq);
        // Video magnetic, audio legacy; the cut a frame from the head.
        let kinds = |ti: usize| ti == 0;
        assert_eq!(blade_linked(&mut seq, 0, vid, tc(1), kinds).err(), Some(BladeRefusal::NotInside));
        assert_eq!(blade_linked(&mut seq, 0, vid, tc(249), kinds).err(), Some(BladeRefusal::NotInside));
        assert_eq!(shape(&seq), before, "neither track was cut");
        assert!(blade_linked(&mut seq, 0, vid, tc(2), kinds).is_ok());
        assert_cross_linked(&seq, 2);
    }

    /// #434: the edge guard is a frame of the sequence's own fps. At
    /// 29.97 a frame is 1001 ticks of the 30000 rate, so a guard of one
    /// tick — about 33 microseconds — let a cut a frame from the head
    /// through, on both kinds of track.
    #[test]
    fn the_edge_guard_is_a_frame_wide_at_a_fractional_rate() {
        let f = |frames: i64| RationalTime { value: frames * 1001, rate: 30000 };
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::ntsc_29970();
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.magnetic_role = TrackRole::Free;
        let c = Clip::media(MediaId::new(), f(0), f(100), f(0), f(100));
        let id = c.id;
        v1.clips = vec![c.clone()];
        seq.tracks = vec![v1];

        assert!(!cut_is_inside(&c, f(1), seq.fps), "one frame from the head is meant for the head");
        assert!(!cut_is_inside(&c, f(99), seq.fps), "one frame from the tail is meant for the tail");
        assert!(cut_is_inside(&c, f(2), seq.fps), "two frames in is inside the clip");

        for magnetic in [false, true] {
            assert_eq!(blade_linked(&mut seq, 0, id, f(1), |_| magnetic).err(), Some(BladeRefusal::NotInside), "magnetic {magnetic}");
            assert_eq!(seq.tracks[0].clips.len(), 1, "nothing was cut (magnetic {magnetic})");
        }
        assert!(blade_linked(&mut seq, 0, id, f(2), |_| false).is_ok());
        assert_eq!(seq.tracks[0].clips.len(), 2, "two frames in cuts");
    }

    /// A stale blade of a partner's original id, queued before the pair
    /// was cut, changes nothing.
    #[test]
    fn a_blade_of_a_clip_already_cut_through_its_partner_is_a_noop() {
        for magnetic in [false, true] {
            let (mut seq, vid, aid) = pair((0, 250), (0, 250));
            blade_linked(&mut seq, 0, vid, tc(125), |_| magnetic).unwrap();
            let after = shape(&seq);
            assert_eq!(blade_linked(&mut seq, 1, aid, tc(125), |_| magnetic).err(), Some(BladeRefusal::NotInside));
            assert_eq!(shape(&seq), after);
        }
    }
}
