//! A drag's clip moves as the document's own moves (#3060).

use klipp_core::ClipId;
use klipp_document::ops::{Anchor, ClipRef, Op, TrackRef};

/// A drag's moves as the document's moves (#3060), the clip moving into
/// the space taken first so none meets another on the way: the latest
/// first when they move later, the earliest first when earlier.
pub fn drag_move_ops(seq: &klipp_timeline::Sequence, moves: &[(ClipId, klipp_core::Timecode, Option<usize>)], linked: bool) -> Vec<Op> {
    let fps = seq.fps.as_f64();
    let mut moves: Vec<(ClipId, klipp_core::Timecode, Option<usize>, f64)> = moves
        .iter()
        .filter_map(|&(id, to, track)| seq.all_clips().find(|c| c.id == id).map(|c| (id, to, track, c.timeline_in.to_secs_f64())))
        .collect();
    let later = moves.first().is_some_and(|m| m.1.to_secs_f64() > m.3);
    moves.sort_by(|a, b| a.3.total_cmp(&b.3));
    if later {
        moves.reverse();
    }
    moves
        .into_iter()
        .map(|(id, to, track, _)| Op::Move { clip_id: ClipRef::Id(id), to: Anchor::Frame((to.to_secs_f64() * fps).round() as i64), track: track.map(TrackRef::Index), linked })
        .collect()
}
