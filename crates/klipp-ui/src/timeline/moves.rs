//! A drag's clip moves as the document's own moves (#3060).

use klipp_core::ClipId;
use klipp_document::ops::{Anchor, ClipRef, Op, TrackRef};

/// A drag's moves as the document's moves (#3060), each clip moving
/// into space already left so none meets another on the way: across
/// tracks, the clips on the track furthest along go first (the lowest
/// first when they move down); along a track, the latest first when
/// they move later, the earliest first when earlier.
pub fn drag_move_ops(seq: &klipp_timeline::Sequence, moves: &[(ClipId, klipp_core::Timecode, Option<usize>)], linked: bool) -> Vec<Op> {
    let fps = seq.fps.as_f64();
    // (id, to, track, start, its track now)
    let mut moves: Vec<(ClipId, klipp_core::Timecode, Option<usize>, f64, usize)> = moves
        .iter()
        .filter_map(|&(id, to, track)| {
            let (ti, ci) = seq.find_clip(id)?;
            Some((id, to, track, seq.tracks[ti].clips[ci].timeline_in.to_secs_f64(), ti))
        })
        .collect();
    let later = moves.first().is_some_and(|m| m.1.to_secs_f64() > m.3);
    let down = moves.iter().any(|m| m.2.is_some_and(|t| t > m.4));
    moves.sort_by(|a, b| {
        let along = if down { b.4.cmp(&a.4) } else { a.4.cmp(&b.4) };
        let time = if later { b.3.total_cmp(&a.3) } else { a.3.total_cmp(&b.3) };
        along.then(time)
    });
    moves
        .into_iter()
        .map(|(id, to, track, _, _)| Op::Move { clip_id: ClipRef::Id(id), to: Anchor::Frame((to.to_secs_f64() * fps).round() as i64), track: track.map(TrackRef::Index), linked })
        .collect()
}
