//! Syncing clips on the timeline (#925): the selected clips moved so they
//! line up with one of them, the anchor, by their files' timecodes or by
//! their first clip markers. Pure; the app moves each clip with MoveClip,
//! which takes its linked partner along.

use klipp_core::{ClipId, Timecode};
use klipp_media::MediaPool;
use klipp_timeline::Sequence;

/// The moves that line `selected` up with `anchor`: each clip and where it
/// starts, and the clips that could not be placed with why. A clip whose
/// linked partner is also selected moves once, with it.
pub type Moves = (Vec<(ClipId, Timecode)>, Vec<String>);

fn others(seq: &Sequence, selected: &[ClipId], anchor: ClipId) -> Vec<ClipId> {
    let clip = |id: ClipId| seq.find_clip(id).map(|(t, c)| &seq.tracks[t].clips[c]);
    let partner = |id: ClipId| clip(id).and_then(|c| c.linked_clip_id);
    let mut out: Vec<ClipId> = Vec::new();
    for &id in selected {
        if id == anchor || partner(anchor) == Some(id) || out.contains(&id) || partner(id).is_some_and(|p| out.contains(&p)) {
            continue;
        }
        if clip(id).is_some() {
            out.push(id);
        }
    }
    out
}

/// Line the selected clips up by their files' start timecodes: the same
/// moment of the shoot under each, counted from each clip's in point.
pub fn by_timecode(seq: &Sequence, pool: &MediaPool, selected: &[ClipId], anchor: ClipId) -> Result<Moves, String> {
    let clock = |id: ClipId| -> Result<(f64, f64, String), String> {
        let (t, c) = seq.find_clip(id).ok_or("a clip is not on the timeline")?;
        let clip = &seq.tracks[t].clips[c];
        let item = clip.media_id().and_then(|m| pool.get(&m)).ok_or("a clip has no media")?;
        if (clip.speed - 1.0).abs() > 1e-6 {
            return Err(format!("{} is retimed", item.filename));
        }
        let tc = item.shot.start_timecode.as_deref().ok_or_else(|| format!("{} has no start timecode", item.filename))?;
        let fps = if item.fps.num == 0 { seq.fps } else { item.fps };
        let start = klipp_core::sync::timecode_to_secs(tc, fps).ok_or_else(|| format!("{}'s timecode {tc} cannot be read", item.filename))?;
        Ok((start + clip.source_in.to_secs_f64(), clip.timeline_in.to_secs_f64(), item.filename.clone()))
    };
    let (anchor_at, anchor_in, _) = clock(anchor)?;
    let mut moves = Vec::new();
    let mut skipped = Vec::new();
    for id in others(seq, selected, anchor) {
        match clock(id) {
            Ok((at, _, name)) => {
                let new_in = anchor_in + (at - anchor_at);
                if new_in < 0.0 {
                    skipped.push(format!("{name} would start before the timeline"));
                } else {
                    moves.push((id, Timecode::from_secs_f64(new_in, seq.fps.rate())));
                }
            }
            Err(why) => skipped.push(why),
        }
    }
    Ok((moves, skipped))
}

/// Line the selected clips up by their first clip markers: each clip's
/// marker where the anchor's is.
pub fn by_markers(seq: &Sequence, selected: &[ClipId], anchor: ClipId) -> Result<Moves, String> {
    // A clip marker's time is from the clip's source in, at its speed.
    let marker = |id: ClipId| -> Option<(f64, f64)> {
        let (t, c) = seq.find_clip(id)?;
        let clip = &seq.tracks[t].clips[c];
        let speed = if clip.speed.abs() > 1e-6 { clip.speed.abs() } else { 1.0 };
        let first = clip.clip_markers.iter().map(|m| m.time_secs).fold(f64::INFINITY, f64::min);
        first.is_finite().then(|| (clip.timeline_in.to_secs_f64(), first / speed))
    };
    let (anchor_in, anchor_offset) = marker(anchor).ok_or("the anchor clip has no clip marker")?;
    let at = anchor_in + anchor_offset;
    let mut moves = Vec::new();
    let mut skipped = Vec::new();
    for id in others(seq, selected, anchor) {
        match marker(id) {
            Some((_, offset)) if at - offset >= 0.0 => moves.push((id, Timecode::from_secs_f64(at - offset, seq.fps.rate()))),
            Some(_) => skipped.push("a clip would start before the timeline".to_string()),
            None => skipped.push("a clip has no clip marker".to_string()),
        }
    }
    Ok((moves, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_media::MediaItem;
    use klipp_timeline::{Clip, ClipMarker, Track, TrackKind};

    fn t(secs: f64) -> Timecode {
        Timecode::from_secs_f64(secs, 24)
    }

    fn setup() -> (Sequence, MediaPool, ClipId, ClipId, ClipId) {
        let mut seq = Sequence::new_default();
        seq.tracks.clear();
        let mut pool = MediaPool::new();
        let mut a = MediaItem::synthetic_for_test("/tmp/cam_a.mov".into(), None);
        a.shot.start_timecode = Some("10:00:00:00".into());
        let mut b = MediaItem::synthetic_for_test("/tmp/cam_b.mov".into(), None);
        b.shot.start_timecode = Some("10:00:04:00".into());
        let (a, b) = (pool.add(a), pool.add(b));
        let mut ca = Clip::media(a, t(0.0), t(10.0), t(0.0), t(10.0));
        ca.clip_markers.push(ClipMarker { time_secs: 3.0, name: "clap".into(), color: klipp_core::MarkerColor::Red, comment: String::new() });
        let mut cb = Clip::media(b, t(20.0), t(30.0), t(1.0), t(11.0));
        cb.clip_markers.push(ClipMarker { time_secs: 1.0, name: "clap".into(), color: klipp_core::MarkerColor::Red, comment: String::new() });
        let mut sound = Clip::media(b, t(20.0), t(30.0), t(1.0), t(11.0));
        cb.linked_clip_id = Some(sound.id);
        sound.linked_clip_id = Some(cb.id);
        let ids = (ca.id, cb.id, sound.id);
        let mut v1 = Track::new("Video 1", TrackKind::Video);
        v1.clips.push(ca);
        let mut v2 = Track::new("Video 2", TrackKind::Video);
        v2.clips.push(cb);
        let mut a1 = Track::new("Audio 1", TrackKind::Audio);
        a1.clips.push(sound);
        seq.tracks = vec![v2, v1, a1];
        (seq, pool, ids.0, ids.1, ids.2)
    }

    #[test]
    fn timecode_puts_the_same_moment_under_each_clip() {
        // B's file starts 4 s after A's, and its clip uses it from 1 s: its
        // in point is 5 s into A's clock, so it starts at 5 s.
        let (seq, pool, a, b, sound) = setup();
        let (moves, skipped) = by_timecode(&seq, &pool, &[a, b, sound], a).unwrap();
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(moves.len(), 1, "the linked pair moves once");
        assert_eq!(moves[0].0, b);
        assert!((moves[0].1.to_secs_f64() - 5.0).abs() < 1e-6, "{}", moves[0].1.to_secs_f64());
    }

    #[test]
    fn markers_meet_where_the_anchors_is() {
        // A's marker is 3 s into it, at 3 s; B's is 1 s into it, so B starts at 2 s.
        let (seq, _, a, b, _) = setup();
        let (moves, _) = by_markers(&seq, &[a, b], a).unwrap();
        assert!((moves[0].1.to_secs_f64() - 2.0).abs() < 1e-6, "{}", moves[0].1.to_secs_f64());
    }

    #[test]
    fn what_cannot_be_placed_is_said() {
        let (mut seq, mut pool, a, b, _) = setup();
        let bm = seq.tracks[0].clips[0].media_id().unwrap();
        pool.get_mut(&bm).unwrap().shot.start_timecode = None;
        let (moves, skipped) = by_timecode(&seq, &pool, &[a, b], a).unwrap();
        assert!(moves.is_empty());
        assert!(skipped[0].contains("no start timecode"), "{skipped:?}");
        seq.tracks[1].clips[0].clip_markers.clear();
        assert!(by_markers(&seq, &[a, b], a).is_err(), "the anchor has no marker");
    }
}
