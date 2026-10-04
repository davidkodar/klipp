//! Which clips a selection command takes (#844): every clip of a track,
//! or of the timeline, or those after or before the playhead. Shared by
//! the Edit menu, the track header's menu and the key bindings, so they
//! cannot disagree about what "forward" means.

use klipp_core::{ClipId, SelectRange, Timecode};
use klipp_timeline::{ClipKind, Sequence};

/// The clips `range` takes of track `track` (every track when `None`),
/// in track order and then time. Gaps are not clips to select, and a
/// locked track's clips are left out: whatever comes next (a move, an
/// effect, a delete) would be refused on them.
pub fn clips_in(seq: &Sequence, track: Option<usize>, range: SelectRange, playhead: Timecode) -> Vec<ClipId> {
    seq.tracks
        .iter()
        .enumerate()
        .filter(|(ti, t)| track.is_none_or(|want| want == *ti) && !t.locked)
        .flat_map(|(_, t)| t.clips.iter())
        .filter(|c| !matches!(c.kind, ClipKind::Gap))
        .filter(|c| match range {
            SelectRange::All => true,
            SelectRange::Forward => c.timeline_in >= playhead,
            SelectRange::Backward => c.timeline_out <= playhead,
        })
        .map(|c| c.id)
        .collect()
}

/// The track a clip is on, for the commands that act on "the selected
/// clip's track".
pub fn track_of(seq: &Sequence, clip: ClipId) -> Option<usize> {
    seq.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Track, TrackKind};

    fn tc(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    fn clip(from: f64, to: f64) -> Clip {
        Clip::media(MediaId::new(), tc(from), tc(to), tc(0.0), tc(to - from))
    }

    /// Two picture tracks of three clips (0-2, 2-4, 4-6 s), the second
    /// locked, and a gap on the first.
    fn timeline() -> Sequence {
        let mut seq = Sequence::new_default();
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![clip(0.0, 2.0), clip(2.0, 4.0), Clip::gap(tc(4.0), tc(5.0)), clip(5.0, 6.0)];
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.clips = vec![clip(0.0, 2.0), clip(2.0, 4.0)];
        v2.locked = true;
        let mut a1 = Track::new("A1", TrackKind::Audio);
        a1.clips = vec![clip(0.0, 3.0), clip(3.0, 6.0)];
        seq.tracks = vec![v1, v2, a1];
        seq
    }

    #[test]
    fn a_track_its_clips_and_none_of_its_gaps() {
        let seq = timeline();
        let ids = |t: usize, k: &[usize]| k.iter().map(|&i| seq.tracks[t].clips[i].id).collect::<Vec<_>>();
        assert_eq!(clips_in(&seq, Some(0), SelectRange::All, tc(0.0)), ids(0, &[0, 1, 3]), "the gap is not a clip");
        assert!(clips_in(&seq, Some(1), SelectRange::All, tc(0.0)).is_empty(), "a locked track gives nothing");
        assert_eq!(clips_in(&seq, None, SelectRange::All, tc(0.0)).len(), 5, "every unlocked clip of the timeline");
    }

    #[test]
    fn forward_and_backward_by_the_playhead() {
        let seq = timeline();
        let ids = |t: usize, k: &[usize]| k.iter().map(|&i| seq.tracks[t].clips[i].id).collect::<Vec<_>>();
        // At 2 s: on V1 the clip starting there and after it; the clip
        // ending there and before it.
        assert_eq!(clips_in(&seq, Some(0), SelectRange::Forward, tc(2.0)), ids(0, &[1, 3]));
        assert_eq!(clips_in(&seq, Some(0), SelectRange::Backward, tc(2.0)), ids(0, &[0]));
        // A clip the playhead is inside is in neither.
        assert_eq!(clips_in(&seq, Some(2), SelectRange::Forward, tc(2.0)), ids(2, &[1]));
        assert!(clips_in(&seq, Some(2), SelectRange::Backward, tc(2.0)).is_empty());
        // On every track, still without the locked one.
        assert_eq!(clips_in(&seq, None, SelectRange::Forward, tc(2.0)), [ids(0, &[1, 3]), ids(2, &[1])].concat());
        assert_eq!(track_of(&seq, seq.tracks[2].clips[1].id), Some(2));
    }
}
