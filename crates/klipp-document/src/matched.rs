//! Clips kept matched to a shot (#2236): a title, effect, adjustment or
//! sound laid over one shot starts and ends with it, and follows its
//! trims, rolls and moves.
//!
//! The follow is worked out where an edit is recorded, from the sequence
//! before and after it, and written as ordinary edits in the same undo
//! step: undo, redo and a socket's acked writes replay what happened, and
//! nothing is derived again. The clip moves through the move and
//! set-length ops, so a follow that would run over a neighbour or past
//! the clip's source is refused like any other; its link is then dropped
//! instead. So is the link of a clip whose shot was deleted, and of one
//! moved or trimmed on its own while its shot stayed where it was.

use klipp_core::ClipId;
use klipp_media::MediaPool;
use klipp_timeline::Sequence;

use crate::history::EditCommand;
use crate::ops::{self, Anchor, ClipRef, LengthKeep, Op};

/// The ops that give `clip_id` `shot_id`'s start and length, through the
/// move and set-length ops. `None` when it has them already, or either
/// is not in `seq`.
pub fn match_ops(seq: &Sequence, clip_id: ClipId, shot_id: ClipId) -> Option<Vec<Op>> {
    let clip = seq.all_clips().find(|c| c.id == clip_id)?;
    let shot = seq.all_clips().find(|c| c.id == shot_id)?;
    if (shot.timeline_in, shot.timeline_out) == (clip.timeline_in, clip.timeline_out) {
        return None;
    }
    let one = klipp_timeline::one_frame(seq.fps).value.max(1);
    let frames = ((shot.timeline_out - shot.timeline_in).value as f64 / one as f64).round() as i64;
    let to = Op::Move { clip_id: ClipRef::Id(clip_id), to: Anchor::Secs(shot.timeline_in.to_secs_f64()), track: None, linked: true };
    let length = Op::SetLength { clips: vec![ClipRef::Id(clip_id)], frames, keep: LengthKeep::Start, ripple: false };
    let (shot_len, clip_len) = (shot.timeline_out - shot.timeline_in, clip.timeline_out - clip.timeline_in);
    // Only what changes: set length refuses a clip already as long.
    Some(match (shot.timeline_in != clip.timeline_in, shot_len != clip_len) {
        (true, false) => vec![to],
        (false, _) => vec![length],
        // Shrunk before it moves, grown after, so it never passes through
        // a neighbour on the way.
        (true, true) if shot_len < clip_len => vec![length, to],
        (true, true) => vec![to, length],
    })
}

/// What an edit means for the clips kept matched: the edits that keep
/// them on their shots, and the links dropped, each with why.
#[derive(Debug, Default)]
pub struct Follow {
    pub edits: Vec<EditCommand>,
    pub dropped: Vec<(ClipId, String)>,
}

/// The follow after an edit took `before` to `after`: each clip kept
/// matched whose shot changed takes the shot's start and length again.
/// A link is dropped when the shot is gone, when the clip changed on its
/// own while its shot did not, or when the clip cannot follow.
pub fn follow(before: &Sequence, after: &Sequence, pool: &MediaPool) -> Follow {
    let mut out = Follow::default();
    // Links the edit made itself (Keep Matched) are not followed yet.
    let held_before = |clip: ClipId, shot: ClipId| before.all_clips().any(|c| c.id == clip && c.matched_to == Some(shot));
    let links: Vec<(ClipId, ClipId)> = after.all_clips().filter_map(|c| Some((c.id, c.matched_to?))).filter(|(c, s)| held_before(*c, *s)).collect();
    if links.is_empty() {
        return out;
    }
    let range = |seq: &Sequence, id: ClipId| seq.all_clips().find(|c| c.id == id).map(|c| (c.timeline_in, c.timeline_out));
    let mut sims = vec![after.clone()];
    let mut sim_pool = pool.clone();
    let mut sim_fps = after.fps;
    for (clip, shot) in links {
        let (shot_was, shot_is) = (range(before, shot), range(&sims[0], shot));
        let clip_moved = range(before, clip) != range(after, clip);
        let edits = match shot_is {
            None => Err("its shot was deleted".to_string()),
            // Moved or trimmed on its own, its shot where it was.
            Some(_) if clip_moved && shot_was == shot_is => Err("it was moved on its own".to_string()),
            Some(_) if shot_was == shot_is => continue,
            Some(_) => match match_ops(&sims[0], clip, shot) {
                None => continue,
                Some(ops) => ops::resolve(&sims[0], &sim_pool, &ops).map(|plan| plan.steps.into_iter().map(|s| s.edit).collect::<Vec<_>>()).map_err(|r| r.why),
            },
        };
        let edits = edits.unwrap_or_else(|why| {
            out.dropped.push((clip, why));
            vec![EditCommand::SetClipMatch { clip_id: clip, old: Some(shot), new: None }]
        });
        let mut view = crate::apply::DocumentView { sequences: &mut sims, active: 0, media_pool: &mut sim_pool, fps: &mut sim_fps };
        for e in &edits {
            view.apply_forward(e);
        }
        out.edits.extend(edits);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::{Clip, TrackKind};

    fn tc(s: f64) -> klipp_core::Timecode {
        klipp_core::RationalTime::from_secs_f64(s, 24)
    }

    /// A sequence with two shots side by side on the first video track and
    /// a title over the first, kept matched to it: (sequence, shot, next
    /// shot, title).
    fn matched() -> (Sequence, ClipId, ClipId, ClipId) {
        let mut seq = Sequence::new_default();
        seq.tracks.insert(0, klipp_timeline::Track::new("V2", TrackKind::Video));
        let v: Vec<usize> = seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video).map(|(i, _)| i).collect();
        let media = klipp_core::MediaId::new();
        let shot = Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        let next = Clip::media(media, tc(4.0), tc(8.0), tc(4.0), tc(8.0));
        let mut title = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        title.matched_to = Some(shot.id);
        let ids = (shot.id, next.id, title.id);
        seq.tracks[v[1]].clips = vec![shot, next];
        seq.tracks[v[0]].clips = vec![title];
        (seq, ids.0, ids.1, ids.2)
    }

    fn range(seq: &Sequence, id: ClipId) -> (f64, f64) {
        let c = seq.all_clips().find(|c| c.id == id).unwrap();
        (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64())
    }

    fn apply(seq: &Sequence, edits: &[EditCommand]) -> Sequence {
        let mut seqs = vec![seq.clone()];
        let (mut pool, mut fps) = (MediaPool::new(), seq.fps);
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        for e in edits {
            view.apply_forward(e);
        }
        seqs.remove(0)
    }

    /// A trimmed shot takes the clip matched to it along: the follow is
    /// ordinary edits, so undoing them puts it back.
    #[test]
    fn a_clip_follows_its_shot_trimmed() {
        let (before, shot, _, title) = matched();
        let mut after = before.clone();
        after.all_clips_mut().find(|c| c.id == shot).unwrap().timeline_out = tc(3.0);
        let f = follow(&before, &after, &MediaPool::new());
        assert!(f.dropped.is_empty(), "{:?}", f.dropped);
        let followed = apply(&after, &f.edits);
        assert_eq!(range(&followed, title), (0.0, 3.0));
        assert_eq!(followed.all_clips().find(|c| c.id == title).unwrap().matched_to, Some(shot), "still matched");
        assert!(follow(&after, &after, &MediaPool::new()).edits.is_empty(), "nothing changed, nothing follows");
    }

    /// A shot moved takes it along; one deleted, or the clip moved on its
    /// own, drops the link and leaves the clip where it is.
    #[test]
    fn a_clip_follows_its_shot_moved_and_lets_go_when_it_is_gone_or_moved_alone() {
        let (before, shot, next, title) = matched();
        // The shot and the one after it swap places.
        let mut after = before.clone();
        for c in after.all_clips_mut() {
            if c.id == shot {
                (c.timeline_in, c.timeline_out) = (tc(4.0), tc(8.0));
            } else if c.id == next {
                (c.timeline_in, c.timeline_out) = (tc(0.0), tc(4.0));
            }
        }
        let f = follow(&before, &after, &MediaPool::new());
        assert!(f.dropped.is_empty(), "{:?}", f.dropped);
        let followed = apply(&after, &f.edits);
        assert_eq!(range(&followed, title), (4.0, 8.0), "moved with its shot");

        let mut gone = before.clone();
        for t in &mut gone.tracks {
            t.clips.retain(|c| c.id != shot);
        }
        let f = follow(&before, &gone, &MediaPool::new());
        assert_eq!(f.dropped.len(), 1);
        let after_gone = apply(&gone, &f.edits);
        let t = after_gone.all_clips().find(|c| c.id == title).unwrap();
        assert_eq!((t.matched_to, range(&after_gone, title)), (None, (0.0, 4.0)), "link dropped, the clip left where it was");

        let mut alone = before.clone();
        let t = alone.all_clips_mut().find(|c| c.id == title).unwrap();
        (t.timeline_in, t.timeline_out) = (tc(1.0), tc(3.0));
        let f = follow(&before, &alone, &MediaPool::new());
        assert_eq!(f.dropped.iter().map(|d| d.0).collect::<Vec<_>>(), vec![title], "moved on its own");
        assert_eq!(range(&apply(&alone, &f.edits), title), (1.0, 3.0), "and left where it was put");
    }

    /// A follow the ops refuse, over a neighbour, drops the link instead.
    #[test]
    fn a_follow_that_would_run_over_a_neighbour_lets_go() {
        let (before, shot, _, title) = matched();
        let v = before.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == title)).unwrap();
        let mut before = before;
        before.tracks[v].clips.push(Clip::media(klipp_core::MediaId::new(), tc(4.0), tc(6.0), tc(0.0), tc(2.0)));
        let mut after = before.clone();
        after.all_clips_mut().find(|c| c.id == shot).unwrap().timeline_out = tc(5.0);
        let f = follow(&before, &after, &MediaPool::new());
        assert_eq!(f.dropped.iter().map(|d| d.0).collect::<Vec<_>>(), vec![title]);
        assert!(f.dropped[0].1.contains("run into"), "{}", f.dropped[0].1);
        let followed = apply(&after, &f.edits);
        assert_eq!(range(&followed, title), (0.0, 4.0), "left where it was");
        assert_eq!(followed.all_clips().find(|c| c.id == title).unwrap().matched_to, None);
    }
}
