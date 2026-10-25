//! Cut, Copy, Paste and Duplicate of a selection (#845): every selected
//! clip with its linked partner and the transitions between them, kept
//! on their tracks at their offsets, pasted over what is there or
//! inserted with every track cut and moved along, into this sequence or
//! another, undone in one step that gives back what the paste covered.

use klipp_core::{ClipId, Timecode};
use crate::history::{EditCommand, TrackContents};
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind, Transition};
use std::collections::{BTreeSet, HashMap, HashSet};

/// One copied clip and where it came from.
#[derive(Debug, Clone)]
pub struct CopiedClip {
    pub clip: Clip,
    /// Its track's index, and the kind of track, for a sequence whose
    /// tracks are not the same.
    pub track: usize,
    pub kind: TrackKind,
}

/// What Cut and Copy put on the clipboard.
#[derive(Debug, Clone, Default)]
pub struct ClipboardContent {
    /// In timeline order, then track order.
    pub clips: Vec<CopiedClip>,
    /// Transitions whose two clips were both copied, with their track.
    pub transitions: Vec<(Transition, usize)>,
    /// The colour labels on the copied clips' nodes (#1753), which a
    /// paste puts on the copies' nodes.
    pub labels: Vec<klipp_timeline::NodeLabel>,
}

impl ClipboardContent {
    /// From the earliest start to the latest end.
    pub fn span(&self) -> Timecode {
        let start = self.clips.iter().map(|c| c.clip.timeline_in).min();
        let end = self.clips.iter().map(|c| c.clip.timeline_out).max();
        match (start, end) {
            (Some(s), Some(e)) => e - s,
            _ => Timecode::zero(),
        }
    }
}

/// The clips `ids` with their linked partners and the transitions between
/// them; `None` when none of them is on the timeline. Gaps are left out.
pub fn copy(seq: &Sequence, ids: &[ClipId]) -> Option<ClipboardContent> {
    let mut wanted: HashSet<ClipId> = HashSet::new();
    for &id in ids {
        if let Some((ti, ci)) = seq.find_clip(id) {
            let clip = &seq.tracks[ti].clips[ci];
            wanted.insert(id);
            if let Some(partner) = clip.linked_clip_id {
                wanted.insert(partner);
            }
        }
    }
    let mut clips: Vec<CopiedClip> = Vec::new();
    for (ti, track) in seq.tracks.iter().enumerate() {
        for clip in &track.clips {
            if wanted.contains(&clip.id) && !matches!(clip.kind, ClipKind::Gap) {
                clips.push(CopiedClip { clip: clip.clone(), track: ti, kind: track.kind });
            }
        }
    }
    if clips.is_empty() {
        return None;
    }
    clips.sort_by_key(|c| (c.clip.timeline_in, c.track));
    let copied: HashSet<ClipId> = clips.iter().map(|c| c.clip.id).collect();
    let transitions = seq
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(ti, t)| t.transitions.iter().map(move |tr| (tr, ti)))
        .filter(|(tr, _)| copied.contains(&tr.clip_a_id) && copied.contains(&tr.clip_b_id))
        .map(|(tr, ti)| (*tr, ti))
        .collect();
    let labels = seq.node_labels.iter().filter(|l| l.node.root_clip().is_some_and(|c| copied.contains(&ClipId(c)))).cloned().collect();
    Some(ClipboardContent { clips, transitions, labels })
}

/// What lies in `seq` from `from` to `to` on every track, as Copy would
/// take it (#82): each clip there, one that straddles an end cut at it,
/// with the transitions between them. `None` when nothing is there.
pub fn copy_range(seq: &Sequence, from: Timecode, to: Timecode) -> Option<ClipboardContent> {
    if to <= from {
        return None;
    }
    let mut cut = seq.clone();
    for track in &mut cut.tracks {
        for at in [from, to] {
            if let Some(id) = track.clips.iter().find(|c| c.timeline_in < at && at < c.timeline_out).map(|c| c.id) {
                track.split_at(id, at);
            }
        }
    }
    let inside: Vec<ClipId> = cut.tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| c.timeline_in >= from && c.timeline_out <= to).map(|c| c.id).collect();
    let mut cb = copy(&cut, &inside)?;
    // Only what lies inside: a partner of a clip inside that reaches out
    // of the range is not taken.
    cb.clips.retain(|c| c.clip.timeline_in >= from && c.clip.timeline_out <= to);
    (!cb.clips.is_empty()).then_some(cb)
}

/// Whether a paste lays the clips over what is there, or cuts every
/// unlocked track at the playhead and moves everything after it along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteMode {
    Overwrite,
    Insert,
}

/// What a paste made: the sequence after it, the tracks it changed, the
/// new clips, and how many copied clips found no track to go on.
pub struct Pasted {
    pub sequence: Sequence,
    pub touched: BTreeSet<usize>,
    pub ids: Vec<ClipId>,
    pub skipped: usize,
    /// A Paste Insert that could not cut the tracks to make room (its
    /// track locked): nothing was pasted, rather than an overwrite in its
    /// place.
    pub no_room: bool,
}

/// The track a copied clip goes on in `seq`: its own, when it is still a
/// track of its kind, unlocked and targeted; else the first such track
/// (#915); with no targeted track of its kind, its own if unlocked, else
/// the first unlocked track of its kind; else none.
fn target_track(seq: &Sequence, copied: &CopiedClip) -> Option<usize> {
    let usable = |ti: usize| seq.tracks.get(ti).is_some_and(|t| t.kind == copied.kind && !t.locked);
    let targeted = |ti: usize| usable(ti) && seq.is_targeted(ti);
    if targeted(copied.track) {
        return Some(copied.track);
    }
    (0..seq.tracks.len())
        .find(|&ti| targeted(ti))
        .or_else(|| usable(copied.track).then_some(copied.track))
        .or_else(|| (0..seq.tracks.len()).find(|&ti| usable(ti)))
}

/// `cb`, copied from `source`, made ready to go into `target` as a carry
/// between stacked timelines puts it (#82): with `linked` off, only the
/// clips `ids` themselves, as a move takes them; onto `track`, the
/// topmost of that track's kind there and the others of its kind as far
/// below it as they were (any that would fall past the last track left
/// where the paste puts them).
pub fn carried(source: &Sequence, target: &Sequence, mut cb: ClipboardContent, ids: &[ClipId], track: Option<usize>, linked: bool) -> ClipboardContent {
    if !linked {
        cb.clips.retain(|c| ids.contains(&c.clip.id));
    }
    if let Some(kind) = track.and_then(|t| target.tracks.get(t)).map(|t| t.kind) {
        let kind_of = |i: usize| source.tracks.get(i).map(|t| t.kind);
        if let Some(top) = cb.clips.iter().filter(|c| kind_of(c.track) == Some(kind)).map(|c| c.track).min() {
            let to = track.unwrap_or(top);
            for c in cb.clips.iter_mut().filter(|c| kind_of(c.track) == Some(kind)) {
                let moved = to + (c.track - top);
                if target.tracks.get(moved).is_some_and(|t| t.kind == kind) {
                    c.track = moved;
                }
            }
        }
    }
    cb
}

/// `cb` pasted into `seq` at `at`: new ids, each clip at its own offset
/// from the first, a linked pair linked again and to nothing else, what
/// belongs to the original alone left behind.
pub fn paste(seq: &Sequence, cb: &ClipboardContent, at: Timecode, mode: PasteMode) -> Pasted {
    paste_with(seq, cb, at, mode, target_track)
}

/// `cb` pasted as [`paste`] does, but each clip onto its own track: the
/// one it was dropped on (#2549), not the targeted one. A clip whose
/// track is gone, of another kind or locked is skipped.
pub fn paste_onto(seq: &Sequence, cb: &ClipboardContent, at: Timecode, mode: PasteMode) -> Pasted {
    paste_with(seq, cb, at, mode, |seq, copied| {
        seq.tracks.get(copied.track).is_some_and(|t| t.kind == copied.kind && !t.locked).then_some(copied.track)
    })
}

fn paste_with(seq: &Sequence, cb: &ClipboardContent, at: Timecode, mode: PasteMode, target: impl Fn(&Sequence, &CopiedClip) -> Option<usize>) -> Pasted {
    let mut out = seq.clone();
    let mut touched = BTreeSet::new();
    let first = cb.clips.iter().map(|c| c.clip.timeline_in).min().unwrap_or(at);
    let new_ids: HashMap<ClipId, ClipId> = cb.clips.iter().map(|c| (c.clip.id, ClipId::new())).collect();
    let targets: Vec<Option<usize>> = cb.clips.iter().map(|c| target(&out, c)).collect();
    if mode == PasteMode::Insert {
        // Room for the whole paste: a gap of its length inserted on the
        // first track it goes to cuts every unlocked track at the
        // playhead and moves what follows, as an insert edit does; the
        // gap itself is then taken out again.
        if let Some(ti) = targets.iter().flatten().next().copied() {
            let span = cb.span();
            if span > Timecode::zero() {
                let gap = Clip::gap(at, at + span);
                let gap_id = gap.id;
                if out.insert_edit(ti, gap, at).is_none() {
                    return Pasted { sequence: seq.clone(), touched: BTreeSet::new(), ids: Vec::new(), skipped: 0, no_room: true };
                }
                out.tracks[ti].remove_clip(gap_id);
                touched.extend(out.tracks.iter().enumerate().filter(|(_, t)| !t.locked).map(|(i, _)| i));
            }
        }
    }
    let mut ids = Vec::new();
    let mut skipped = 0;
    for (copied, target) in cb.clips.iter().zip(&targets) {
        let Some(ti) = *target else {
            skipped += 1;
            continue;
        };
        let clip = &copied.clip;
        let timeline_in = at + (clip.timeline_in - first);
        let mut pasted = Clip {
            id: new_ids[&clip.id],
            timeline_in,
            timeline_out: timeline_in + clip.duration(),
            linked_clip_id: clip.linked_clip_id.and_then(|l| new_ids.get(&l).copied()),
            linked_title_asset_id: None,
            group_id: None,
            magnetic_anchor: None,
            transform_link: None,
            track_points: Vec::new(),
            // The keys are in file time: a pasted copy keeps the track,
            // and a pinned copy pasted with its host follows the copy.
            surfaces: clip.surfaces.clone(),
            surface_pin: clip.surface_pin.map(|p| klipp_timeline::SurfacePin { host: new_ids.get(&p.host).copied().unwrap_or(p.host), ..p }),
            precomp: None,
            baked_own: None,
            ..clip.clone()
        };
        // A grade group is the sequence's: in another sequence the clip
        // is in none (#237).
        if pasted.grade_group.is_some_and(|g| out.grade_group(g).is_none()) {
            pasted.grade_group = None;
        }
        ids.push(pasted.id);
        // The copied clip's node labels on the copy's nodes (#1753).
        out.node_labels.extend(klipp_timeline::carry_node_labels(&cb.labels, clip, &pasted));
        overwrite_insert_clip_on_track(&mut out.tracks[ti], pasted);
        touched.insert(ti);
    }
    // The transitions between pasted clips, on the track their clips went to.
    for (tr, _) in &cb.transitions {
        let (Some(&a), Some(&b)) = (new_ids.get(&tr.clip_a_id), new_ids.get(&tr.clip_b_id)) else { continue };
        let Some(ti) = out.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == a) && t.clips.iter().any(|c| c.id == b)) else { continue };
        out.tracks[ti].transitions.push(Transition { id: klipp_core::TransitionId::new(), clip_a_id: a, clip_b_id: b, ..*tr });
    }
    Pasted { sequence: out, touched, ids, skipped, no_room: false }
}

/// The undo records that take `before` to `after` on the `touched` tracks.
pub fn track_edits(before: &Sequence, after: &Sequence, touched: &BTreeSet<usize>, description: &'static str) -> Vec<EditCommand> {
    touched
        .iter()
        .filter_map(|&ti| {
            let (old, new) = (before.tracks.get(ti)?, after.tracks.get(ti)?);
            Some(EditCommand::SetTrackContents {
                description,
                track_index: ti,
                old: Box::new(TrackContents::of(old)),
                new: Box::new(TrackContents::of(new)),
            })
        })
        .collect()
}

/// The clips a Cut, Copy or Duplicate of `clip` takes: the selection when
/// `clip` is part of one of several clips, else `clip` alone.
pub fn selection_for(clip: ClipId, primary: Option<ClipId>, selected: &[ClipId]) -> Vec<ClipId> {
    let mut sel: Vec<ClipId> = Vec::new();
    for id in selected.iter().copied().chain(primary) {
        if !sel.contains(&id) {
            sel.push(id);
        }
    }
    if sel.len() > 1 && sel.contains(&clip) { sel } else { vec![clip] }
}

/// A sequence's content revision (#2385): a hash of all it holds, so any
/// change to it, by whatever path, changes it. A plan that copied from a
/// sequence records it, and the copy lands only while it still holds.
pub fn content_revision(seq: &Sequence) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(seq).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// Overwrite-insert a clip onto a track at a given position.
/// Trims or removes any overlapping clips in the range [timeline_in, timeline_out].
pub fn overwrite_insert_clip_on_track(
    track: &mut klipp_timeline::Track,
    new_clip: klipp_timeline::Clip,
) {
    let new_in = new_clip.timeline_in;
    let new_out = new_clip.timeline_out;

    // Collect indices of clips to modify (process in reverse to preserve indices)
    let mut to_remove = Vec::new();
    let mut to_trim_left = Vec::new();  // trim in-point forward (clip extends past new_out)
    let mut to_trim_right = Vec::new(); // trim out-point back (clip starts before new_in)
    let mut to_split = Vec::new();      // clip fully contains new range — needs split

    for (i, clip) in track.clips.iter().enumerate() {
        let c_in = clip.timeline_in;
        let c_out = clip.timeline_out;

        // No overlap
        if c_out <= new_in || c_in >= new_out {
            continue;
        }

        if c_in >= new_in && c_out <= new_out {
            // Fully covered — remove
            to_remove.push(i);
        } else if c_in < new_in && c_out > new_out {
            // Clip contains the new range — split into two
            to_split.push(i);
        } else if c_in < new_in && c_out > new_in {
            // Overlaps from left — trim right edge back
            to_trim_right.push(i);
        } else if c_in < new_out && c_out > new_out {
            // Overlaps from right — trim left edge forward
            to_trim_left.push(i);
        }
    }

    // Handle splits first (creates a new clip)
    for &i in to_split.iter().rev() {
        let clip = track.clips[i].clone();
        let left_dur = new_in - clip.timeline_in;
        let right_offset = new_out - clip.timeline_in;

        // Left portion: keep original in, trim out to new_in
        track.clips[i].timeline_out = new_in;
        track.clips[i].source_out = clip.source_in + left_dur;
        // A glide is the whole clip's move; the pieces keep their keys (#611).
        track.clips[i].frame_glide = None;
        track.clips[i].keep_angle_cuts_on_piece(0.0, left_dur.to_secs_f64());

        // Right portion: new clip starting at new_out. The clip's own
        // copy, picture and all; what belongs to the clip alone is not.
        let mut right_clip = clip.clone();
        right_clip.id = klipp_core::ClipId::new();
        (right_clip.timeline_in, right_clip.source_in) = (new_out, clip.source_in + right_offset);
        right_clip.magnetic_anchor = None;
        right_clip.frame_glide = None;
        right_clip.matched_to = None;
        right_clip.transform_link = None;
        right_clip.track_points = Vec::new();
        right_clip.grade_flag = Default::default();
        right_clip.precomp = None;
        right_clip.baked_own = None;
        right_clip.keep_angle_cuts_on_piece(right_offset.to_secs_f64(), (clip.timeline_out - clip.timeline_in).to_secs_f64());
        track.clips.insert(i + 1, right_clip);
    }

    // Trim left edges forward (clip starts before new_out, ends after)
    for &i in &to_trim_left {
        let clip = &track.clips[i];
        let trim_amount = new_out - clip.timeline_in;
        track.clips[i].source_in += trim_amount;
        track.clips[i].timeline_in = new_out;
        track.clips[i].keep_angle_cuts_on_picture(trim_amount.to_secs_f64());
    }

    // Trim right edges back (clip starts before new_in, ends after)
    for &i in &to_trim_right {
        let clip = &track.clips[i];
        let trim_amount = clip.timeline_out - new_in;
        track.clips[i].source_out -= trim_amount;
        track.clips[i].timeline_out = new_in;
    }

    // Remove fully covered clips (reverse order)
    for &i in to_remove.iter().rev() {
        track.clips.remove(i);
    }

    // Insert the new clip
    track.clips.push(new_clip);
    track.sort_clips();
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Track, TransitionKind};

    fn tc(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 25)
    }

    fn clip(from: f64, to: f64) -> Clip {
        Clip::media(MediaId::new(), tc(from), tc(to), tc(0.0), tc(to - from))
    }

    /// V1: a 0-2, b 2-4 with a transition between them, c 6-8. A1: the
    /// sound of a, linked to it, 0-2; and d 4-6.
    fn timeline() -> (Sequence, [ClipId; 5]) {
        let mut seq = Sequence::new_default();
        let (mut a, b, c) = (clip(0.0, 2.0), clip(2.0, 4.0), clip(6.0, 8.0));
        let (mut sa, d) = (clip(0.0, 2.0), clip(4.0, 6.0));
        a.linked_clip_id = Some(sa.id);
        sa.linked_clip_id = Some(a.id);
        let ids = [a.id, b.id, c.id, sa.id, d.id];
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.transitions = vec![Transition { id: klipp_core::TransitionId::new(), kind: TransitionKind::CrossDissolve, duration: tc(0.5), clip_a_id: a.id, clip_b_id: b.id, user_params: Default::default(), alignment: Default::default() }];
        v1.clips = vec![a, b, c];
        let mut a1 = Track::new("A1", TrackKind::Audio);
        a1.clips = vec![sa, d];
        seq.tracks = vec![v1, a1];
        (seq, ids)
    }

    /// An overwrite paste into a multicam primary leaves each angle cut on
    /// the picture it cuts: the piece after the paste counts its cuts from
    /// its own start, and one whose head is pasted over from its new start.
    /// The angle at every moment outside the paste is the one it was.
    #[test]
    fn an_overwrite_paste_keeps_each_angle_at_its_moment() {
        let g = klipp_core::GroupId::new();
        let angle = |n: u8| {
            let mut c = clip(0.0, 10.0);
            (c.multicam_group, c.multicam_angle) = (Some(g), n);
            c
        };
        let mut primary = angle(0);
        primary.multicam_cuts = vec![(2.0, 1), (4.0, 0), (6.0, 1)];
        let mut seq = Sequence::new_default();
        seq.tracks = [primary, angle(1)].into_iter().map(|c| Track { clips: vec![c], ..Track::new("V", TrackKind::Video) }).collect();
        let at = |s: &Sequence, xs: &[f64]| xs.iter().map(|x| s.multicam_at(None, tc(*x)).map(|m| m.active)).collect::<Vec<_>>();
        for (from, to, outside) in [(3.0, 5.0, vec![1.0, 2.5, 5.5, 6.5, 9.0]), (0.0, 3.0, vec![3.5, 4.5, 6.5, 9.0])] {
            let mut pasted = seq.clone();
            for track in &mut pasted.tracks {
                overwrite_insert_clip_on_track(track, clip(from, to));
            }
            assert_eq!(at(&pasted, &outside), at(&seq, &outside), "pasted over {from}-{to} s");
        }
    }

    /// #1753: a copied clip's node labels go with it, onto the paste's
    /// nodes, in another sequence too: a grade node's under the copy's own
    /// grade group. Another clip's labels stay behind.
    #[test]
    fn a_paste_carries_the_copied_clips_node_labels() {
        use klipp_graph_format::path::{NodePath, Root};
        use klipp_timeline::{grade_group_id, GradeAccess, NodeLabel};
        let (mut seq, [a, b, ..]) = timeline();
        let (ti, ci) = seq.find_clip(a).unwrap();
        let node = seq.tracks[ti].clips[ci].edit_grade(|g| g.ensure_corrector());
        let on = |clip: ClipId| NodePath::new(Root::Clip { clip: clip.0 }, [grade_group_id(clip), node]);
        seq.node_labels = vec![NodeLabel { node: on(a), colour: 2 }, NodeLabel { node: on(b), colour: 3 }];
        let cb = copy(&seq, &[a]).unwrap();
        for into in [seq.clone(), Sequence::new_default()] {
            let pasted = paste(&into, &cb, tc(10.0), PasteMode::Overwrite);
            let copy = pasted.ids.iter().copied().find(|id| pasted.sequence.tracks.iter().any(|t| t.kind == TrackKind::Video && t.find_clip(*id).is_some())).unwrap();
            let added: Vec<_> = pasted.sequence.node_labels.iter().filter(|l| !into.node_labels.contains(l)).cloned().collect();
            assert_eq!(added, vec![NodeLabel { node: on(copy), colour: 2 }]);
            assert!(pasted.sequence.holds_node(&on(copy)), "the label names the copy's node");
        }
    }

    /// #82: a range of a selects sequence is what lies in it on every
    /// track, a clip straddling an end cut there, and it goes into a cut
    /// as an insert that moves everything after it along.
    #[test]
    fn a_range_is_copied_across_tracks_and_inserted() {
        let (selects, [a, b, c, sa, d]) = timeline();
        // 1 s to 5 s: the back half of a and of its sound, all of b, the
        // front half of d; nothing of c.
        let cb = copy_range(&selects, tc(1.0), tc(5.0)).unwrap();
        let spans: Vec<(usize, f64, f64)> = cb.clips.iter().map(|x| (x.track, x.clip.timeline_in.to_secs_f64(), x.clip.timeline_out.to_secs_f64())).collect();
        assert_eq!(spans, vec![(0, 1.0, 2.0), (1, 1.0, 2.0), (0, 2.0, 4.0), (1, 4.0, 5.0)], "{spans:?}");
        assert!(cb.clips.iter().all(|x| ![a, sa, d, c].contains(&x.clip.id)), "the cut pieces are new clips");
        assert!(cb.clips.iter().any(|x| x.clip.id == b), "b whole");
        // The back half of a starts a second into its source.
        assert_eq!(cb.clips[0].clip.source_in.to_secs_f64(), 1.0);
        assert!(copy_range(&selects, tc(4.0), tc(4.0)).is_none() && copy_range(&selects, tc(20.0), tc(30.0)).is_none());
        // Into a cut of one 10 s clip, at 3 s: it is cut there and what
        // follows moves along by the 4 s range.
        let mut cut = Sequence::new_default();
        let long = clip(0.0, 10.0);
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![long];
        cut.tracks = vec![v1, Track::new("A1", TrackKind::Audio)];
        let p = paste(&cut, &cb, tc(3.0), PasteMode::Insert);
        assert_eq!(p.ids.len(), 4);
        let end = p.sequence.tracks[0].clips.iter().map(|x| x.timeline_out.to_secs_f64()).fold(0.0, f64::max);
        assert_eq!(end, 14.0, "the cut grew by the range");
        let starts: Vec<f64> = p.ids.iter().filter_map(|id| p.sequence.all_clips().find(|x| x.id == *id)).map(|x| x.timeline_in.to_secs_f64()).collect();
        assert_eq!(starts, vec![3.0, 3.0, 4.0, 6.0], "each at its offset from the range's start");
    }

    #[test]
    fn a_selection_is_copied_with_its_partners_and_its_transitions() {
        let (seq, [a, b, _, sa, _]) = timeline();
        let cb = copy(&seq, &[a, b]).unwrap();
        let got: Vec<(ClipId, usize)> = cb.clips.iter().map(|c| (c.clip.id, c.track)).collect();
        assert_eq!(got, vec![(a, 0), (sa, 1), (b, 0)], "a's sound comes with it, in time order");
        assert_eq!(cb.transitions.len(), 1, "the dissolve between a and b");
        assert_eq!(cb.span(), tc(4.0));
        assert!(copy(&seq, &[ClipId::new()]).is_none());
    }

    #[test]
    fn a_paste_keeps_its_layout_links_and_transitions_and_undoes_whole() {
        let (seq, [a, b, c, ..]) = timeline();
        let cb = copy(&seq, &[a, b]).unwrap();
        // Over c and the empty space after it, at 6 s.
        let p = paste(&seq, &cb, tc(6.0), PasteMode::Overwrite);
        assert_eq!(p.ids.len(), 3);
        assert_eq!(p.skipped, 0);
        let v1 = &p.sequence.tracks[0];
        assert!(v1.clips.iter().all(|x| x.id != c), "c was under the paste");
        let pasted: Vec<&Clip> = p.ids.iter().filter_map(|id| p.sequence.all_clips().find(|x| x.id == *id)).collect();
        let starts: Vec<f64> = pasted.iter().map(|x| x.timeline_in.to_secs_f64()).collect();
        assert_eq!(starts, vec![6.0, 6.0, 8.0], "each at its own offset");
        let (pa, psa) = (pasted[0], pasted[1]);
        assert_eq!((pa.linked_clip_id, psa.linked_clip_id), (Some(psa.id), Some(pa.id)), "linked to each other, not to the originals");
        assert_eq!(v1.transitions.len(), 2, "the dissolve came too");
        // Undo gives c back.
        let edits = track_edits(&seq, &p.sequence, &p.touched, "Paste");
        let mut seqs = vec![p.sequence.clone()];
        let mut pool = klipp_media::MediaPool::default();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        for e in edits.iter().rev() {
            view.apply_inverse(e);
        }
        assert!(view.sequences[0].tracks[0].clips.iter().any(|x| x.id == c), "c is back");
        assert_eq!(view.sequences[0].all_clips().count(), seq.all_clips().count());
    }

    #[test]
    fn a_paste_insert_moves_everything_after_the_playhead_along() {
        let (seq, [a, b, c, _, d]) = timeline();
        let cb = copy(&seq, &[b]).unwrap();
        // Insert b's copy (2 s) at 6 s: c on V1 moves to 8 s; d on A1 ends
        // at 6, so it stays.
        let p = paste(&seq, &cb, tc(6.0), PasteMode::Insert);
        let at = |id: ClipId| p.sequence.all_clips().find(|x| x.id == id).map(|x| x.timeline_in.to_secs_f64());
        assert_eq!(at(c), Some(8.0));
        assert_eq!(at(d), Some(4.0));
        assert_eq!(at(a), Some(0.0));
        assert_eq!(p.sequence.all_clips().filter(|x| matches!(x.kind, ClipKind::Gap)).count(), 0, "the room left no gap clip behind");
        assert_eq!(at(p.ids[0]), Some(6.0));
    }

    /// #853 review: a Paste Insert goes only onto an unlocked track of its
    /// kind; with none it pastes nothing. (`no_room` guards the insert
    /// itself refusing, which the choice of an unlocked track rules out
    /// today, so that it can never quietly become an overwrite.)
    #[test]
    fn a_paste_insert_needs_an_unlocked_track_of_its_kind() {
        let (mut seq, [_, b, ..]) = timeline();
        let cb = copy(&seq, &[b]).unwrap();
        // V1 locked, A1 free: b's copy finds no unlocked picture track.
        seq.tracks[0].locked = true;
        let p = paste(&seq, &cb, tc(6.0), PasteMode::Insert);
        assert!(p.ids.is_empty());
        // A second picture track, unlocked, takes it, but a locked one in
        // the insert's way is refused.
        let mut v2 = klipp_timeline::Track::new("V2", TrackKind::Video);
        v2.locked = false;
        seq.tracks.push(v2);
        assert!(!paste(&seq, &cb, tc(6.0), PasteMode::Insert).no_room, "an unlocked picture track takes the insert");
    }

    /// #915: a paste goes onto a targeted track, its own when that is one.
    #[test]
    fn a_paste_goes_onto_a_targeted_track() {
        let (mut seq, [_, b, ..]) = timeline();
        seq.tracks.insert(0, Track::new("V2", TrackKind::Video));
        let cb = copy(&seq, &[b]).unwrap();
        let track_of = |p: &Pasted| p.sequence.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == p.ids[0]));
        assert_eq!(track_of(&paste(&seq, &cb, tc(10.0), PasteMode::Overwrite)), Some(1), "all targeted: its own track");
        seq.tracks[1].targeted = false;
        assert_eq!(track_of(&paste(&seq, &cb, tc(10.0), PasteMode::Overwrite)), Some(0), "its own untargeted: the targeted V2");
        seq.tracks[0].targeted = false;
        assert_eq!(track_of(&paste(&seq, &cb, tc(10.0), PasteMode::Overwrite)), Some(1), "no picture track targeted: its own again");
    }

    #[test]
    fn into_another_sequence_each_clip_finds_a_track_of_its_kind() {
        let (seq, [a, ..]) = timeline();
        let cb = copy(&seq, &[a]).unwrap();
        // A sequence with its audio track first and its video second.
        let mut other = Sequence::new_default();
        other.tracks = vec![Track::new("A", TrackKind::Audio), Track::new("V", TrackKind::Video)];
        let p = paste(&other, &cb, tc(0.0), PasteMode::Overwrite);
        assert_eq!(p.skipped, 0);
        let kind_of = |id: ClipId| p.sequence.tracks.iter().find(|t| t.clips.iter().any(|c| c.id == id)).map(|t| t.kind);
        assert_eq!(kind_of(p.ids[0]), Some(TrackKind::Video));
        assert_eq!(kind_of(p.ids[1]), Some(TrackKind::Audio));
        // No audio track at all: the sound is skipped and counted.
        let mut picture_only = Sequence::new_default();
        picture_only.tracks = vec![Track::new("V", TrackKind::Video)];
        assert_eq!(paste(&picture_only, &cb, tc(0.0), PasteMode::Overwrite).skipped, 1);
    }

    #[test]
    fn the_selection_is_taken_only_when_the_clip_is_part_of_it() {
        let (x, y, z) = (ClipId::new(), ClipId::new(), ClipId::new());
        assert_eq!(selection_for(x, Some(x), &[x, y]), vec![x, y]);
        assert_eq!(selection_for(z, Some(x), &[x, y]), vec![z]);
        assert_eq!(selection_for(x, Some(x), &[]), vec![x]);
    }
}

#[cfg(test)]
mod carried_tests {
    use super::*;
    use klipp_timeline::{Clip, Track, TrackKind};

    fn t(s: f64) -> Timecode {
        Timecode::from_secs_f64(s, 25)
    }

    /// A carry onto a track puts the topmost clip of that kind there; with
    /// Linked off it takes the clip alone, not its sound.
    #[test]
    fn a_carry_goes_onto_its_track_and_alone_when_unlinked() {
        let media = klipp_core::MediaId::new();
        let mut v = Clip::media(media, t(0.0), t(2.0), t(0.0), t(2.0));
        let mut a = Clip::media(media, t(0.0), t(2.0), t(0.0), t(2.0));
        v.linked_clip_id = Some(a.id);
        a.linked_clip_id = Some(v.id);
        let vid = v.id;
        let mut source = Sequence::new_default();
        source.tracks = vec![Track::new("V1", TrackKind::Video), Track::new("A1", TrackKind::Audio)];
        source.tracks[0].clips = vec![v];
        source.tracks[1].clips = vec![a];
        let mut target = Sequence::new_default();
        target.tracks = vec![Track::new("V2", TrackKind::Video), Track::new("V1", TrackKind::Video), Track::new("A1", TrackKind::Audio)];
        let cb = copy(&source, &[vid]).unwrap();
        let both = carried(&source, &target, cb.clone(), &[vid], Some(1), true);
        assert_eq!(both.clips.len(), 2, "with its sound");
        assert_eq!(both.clips.iter().find(|c| c.clip.id == vid).unwrap().track, 1, "onto the track let go over");
        let alone = carried(&source, &target, cb, &[vid], Some(0), false);
        assert_eq!(alone.clips.iter().map(|c| (c.clip.id, c.track)).collect::<Vec<_>>(), vec![(vid, 0)]);
    }
}
