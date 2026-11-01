//! The pieces of one shot a split left: neighbours on a track that carry
//! on from each other unbroken in the same file, so a grade given to one
//! can be offered to the rest.
use crate::{Clip, Sequence};
use klipp_core::ClipId;

/// Whether `b` carries on from `a` as one shot cut in two: right after it
/// on the timeline, the same file at the same speed, its source range
/// going on where `a`'s stops (from the far end when played reversed).
fn continues(a: &Clip, b: &Clip) -> bool {
    a.media_id().is_some()
        && a.media_id() == b.media_id()
        && a.timeline_out == b.timeline_in
        && a.speed == b.speed
        && a.speed_keyframes.is_empty()
        && b.speed_keyframes.is_empty()
        && a.is_reversed() == b.is_reversed()
        && if a.is_reversed() { b.source_out == a.source_in } else { b.source_in == a.source_out }
}

impl Sequence {
    /// The other pieces of `clip`'s shot with no grade of their own: its
    /// neighbours on its track that carry on from it unbroken, either side.
    pub fn ungraded_shot_pieces(&self, clip: ClipId) -> Vec<ClipId> {
        let Some(track) = self.tracks.iter().find(|t| t.clips.iter().any(|c| c.id == clip)) else { return Vec::new() };
        let mut clips: Vec<&Clip> = track.clips.iter().collect();
        clips.sort_by_key(|c| c.timeline_in);
        let Some(at) = clips.iter().position(|c| c.id == clip) else { return Vec::new() };
        let mut first = at;
        while first > 0 && continues(clips[first - 1], clips[first]) {
            first -= 1;
        }
        let mut last = at;
        while last + 1 < clips.len() && continues(clips[last], clips[last + 1]) {
            last += 1;
        }
        clips[first..=last].iter().filter(|c| c.id != clip && c.grade_is_identity()).map(|c| c.id).collect()
    }
}
