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

/// Every clip [`clips_in`] would take of the whole timeline but those
/// already `selected` (#925).
pub fn invert(seq: &Sequence, selected: &[ClipId], playhead: Timecode) -> Vec<ClipId> {
    clips_in(seq, None, SelectRange::All, playhead).into_iter().filter(|id| !selected.contains(id)).collect()
}

/// Every selectable clip using a media file one of `selected` uses (#925).
pub fn same_source(seq: &Sequence, selected: &[ClipId], playhead: Timecode) -> Vec<ClipId> {
    let media: Vec<klipp_core::MediaId> = seq.tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| selected.contains(&c.id)).filter_map(|c| c.media_id()).collect();
    let all = clips_in(seq, None, SelectRange::All, playhead);
    seq.tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| all.contains(&c.id) && c.media_id().is_some_and(|m| media.contains(&m))).map(|c| c.id).collect()
}

/// Every selectable clip with colour label `colour` (#925).
pub fn with_colour(seq: &Sequence, colour: u8, playhead: Timecode) -> Vec<ClipId> {
    let all = clips_in(seq, None, SelectRange::All, playhead);
    seq.tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| all.contains(&c.id) && c.colour_label == colour).map(|c| c.id).collect()
}

/// The empty stretches of a track between its clips, in order: what lies
/// between one clip's end and the next one's start, and any explicit gap
/// clip (#925). Before the first clip is not a gap.
pub fn gaps(track: &klipp_timeline::Track) -> Vec<(Timecode, Timecode)> {
    let mut clips: Vec<&klipp_timeline::Clip> = track.clips.iter().filter(|c| !matches!(c.kind, ClipKind::Gap)).collect();
    clips.sort_by_key(|c| c.timeline_in);
    let mut out: Vec<(Timecode, Timecode)> = clips.windows(2).filter(|w| w[1].timeline_in > w[0].timeline_out).map(|w| (w[0].timeline_out, w[1].timeline_in)).collect();
    out.extend(track.clips.iter().filter(|c| matches!(c.kind, ClipKind::Gap)).map(|c| (c.timeline_in, c.timeline_out)));
    out.sort();
    out.dedup();
    out
}

/// The gap of `track` that `at` falls in.
pub fn gap_at(track: &klipp_timeline::Track, at: Timecode) -> Option<(Timecode, Timecode)> {
    gaps(track).into_iter().find(|&(a, b)| a <= at && at < b)
}

/// Whether a file has a picture: a video stream or a still. A sound
/// file's cover art is not one (#3128).
pub fn has_picture(media: &klipp_media::MediaItem) -> bool {
    media.is_still || !media.video_codec.is_empty()
}

/// Whether a clip is sound only: on an audio track, or of a file with no
/// picture (#3128). Its inspector shows the sound's tabs, and the viewer
/// no transform handles for it.
pub fn is_sound_only(seq: &Sequence, pool: &klipp_media::MediaPool, clip_id: ClipId) -> bool {
    let Some((ti, ci)) = seq.find_clip(clip_id) else { return false };
    let clip = &seq.tracks[ti].clips[ci];
    seq.tracks[ti].kind == klipp_timeline::TrackKind::Audio || clip.media_id().and_then(|m| pool.get(&m)).is_some_and(|m| !has_picture(m))
}

/// The span a Set Range to Clip marks (#3127): the selected clip's, a
/// gap clip aside, else the first clip under the playhead.
pub fn range_to_clip(seq: &Sequence, selected: Option<klipp_core::ClipId>, playhead: Timecode) -> Option<(Timecode, Timecode)> {
    selected
        .and_then(|id| seq.all_clips().find(|c| c.id == id && !c.kind.is_gap()))
        .or_else(|| seq.tracks.iter().find_map(|t| t.clip_at_timecode(playhead).map(|(_, c)| c).filter(|c| !c.kind.is_gap())))
        .map(|c| (c.timeline_in, c.timeline_out))
}

/// The first gap starting after `after` on any unlocked track: its track
/// and its span (#925).
pub fn next_gap(seq: &Sequence, after: Timecode) -> Option<(usize, Timecode, Timecode)> {
    seq.tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.locked)
        .flat_map(|(i, t)| gaps(t).into_iter().map(move |(a, b)| (i, a, b)))
        .filter(|&(_, a, _)| a > after)
        .min_by_key(|&(_, a, _)| a)
}

/// Close every gap on `track`: each clip moves left to meet the one
/// before, explicit gap clips go, and the first clip stays where it is
/// (#925). Returns how many gaps were closed.
pub fn close_all_gaps(track: &mut klipp_timeline::Track) -> usize {
    let closed = gaps(track).len();
    track.clips.retain(|c| !matches!(c.kind, ClipKind::Gap));
    track.sort_clips();
    let mut cursor: Option<Timecode> = None;
    for c in track.clips.iter_mut() {
        if let Some(end) = cursor {
            if c.timeline_in > end {
                let shift = c.timeline_in - end;
                c.timeline_in -= shift;
                c.timeline_out -= shift;
            }
        }
        cursor = Some(cursor.map_or(c.timeline_out, |e: Timecode| e.max(c.timeline_out)));
    }
    closed
}

/// The track a clip is on, for the commands that act on "the selected
/// clip's track".
pub fn track_of(seq: &Sequence, clip: ClipId) -> Option<usize> {
    seq.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip))
}

#[cfg(test)]
mod tests {
    /// #3128: a clip on an audio track, or of a file with no picture
    /// (cover art aside), is sound only; a picture clip is not.
    #[test]
    fn a_clip_on_an_audio_track_or_of_a_sound_file_is_sound_only() {
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut pool = klipp_media::MediaPool::default();
        let mut song = klipp_media::MediaItem::synthetic_for_test("/tmp/song.mp3".into(), None);
        song.video_codec.clear();
        song.pixel_format = Some("yuvj420p".into());
        let mut film = klipp_media::MediaItem::synthetic_for_test("/tmp/film.mov".into(), None);
        film.video_codec = "h264".into();
        let (song, film) = (pool.add(song), pool.add(film));
        let mut seq = klipp_timeline::Sequence::new_default();
        let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let a = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        let picture = klipp_timeline::Clip::media(film, t(0.0), t(2.0), t(0.0), t(2.0));
        let its_sound = klipp_timeline::Clip::media(film, t(0.0), t(2.0), t(0.0), t(2.0));
        let music_on_video = klipp_timeline::Clip::media(song, t(2.0), t(4.0), t(0.0), t(2.0));
        let ids = (picture.id, its_sound.id, music_on_video.id);
        seq.tracks[v].clips = vec![picture, music_on_video];
        seq.tracks[a].clips = vec![its_sound];
        assert!(!super::is_sound_only(&seq, &pool, ids.0), "a picture");
        assert!(super::is_sound_only(&seq, &pool, ids.1), "the film's sound on an audio track");
        assert!(super::is_sound_only(&seq, &pool, ids.2), "a sound file, cover art and all, even on a video track");
        assert!(!super::has_picture(pool.get(&song).unwrap()));
    }

    /// #3127: Set Range to Clip marks the selected clip; a gap clip
    /// selected, or nothing, gives the clip under the playhead; nothing
    /// there, no range.
    #[test]
    fn set_range_to_clip_takes_the_selected_clip_else_the_one_at_the_playhead() {
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut seq = klipp_timeline::Sequence::new_default();
        let p = seq.primary_track_index().unwrap();
        let a = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let gap = klipp_timeline::Clip::gap(t(2.0), t(4.0));
        let b = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(4.0), t(7.0), t(0.0), t(3.0));
        let (a_id, gap_id) = (a.id, gap.id);
        seq.tracks[p].clips = vec![a, gap, b];
        assert_eq!(super::range_to_clip(&seq, Some(a_id), t(5.0)), Some((t(0.0), t(2.0))), "the selected clip");
        assert_eq!(super::range_to_clip(&seq, Some(gap_id), t(5.0)), Some((t(4.0), t(7.0))), "a gap selected: the clip at the playhead");
        assert_eq!(super::range_to_clip(&seq, None, t(1.0)), Some((t(0.0), t(2.0))));
        assert_eq!(super::range_to_clip(&seq, None, t(3.0)), None, "the playhead over the gap");
    }

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

    #[test]
    fn invert_same_source_and_colour_select_what_they_say() {
        let mut seq = timeline();
        let v1 = seq.tracks[0].clips.iter().filter(|c| !matches!(c.kind, ClipKind::Gap)).map(|c| c.id).collect::<Vec<_>>();
        let first = v1[0];
        let inverse = invert(&seq, &[first], tc(0.0));
        assert!(!inverse.contains(&first) && inverse.len() == 4, "the other two of V1 and both of A1; V2 is locked: {inverse:?}");
        // The A1 clip at 3 s uses V1's first clip's file.
        let media = seq.tracks[0].clips[0].media_id().unwrap();
        if let klipp_timeline::ClipKind::Media { media_id } = &mut seq.tracks[2].clips[1].kind {
            *media_id = media;
        }
        let same = same_source(&seq, &[first], tc(0.0));
        assert_eq!(same, vec![first, seq.tracks[2].clips[1].id]);
        seq.tracks[0].clips[1].colour_label = 3;
        assert_eq!(with_colour(&seq, 3, tc(0.0)), vec![seq.tracks[0].clips[1].id]);
    }

    #[test]
    fn gaps_are_found_selected_and_closed() {
        let mut seq = timeline();
        // V1: 0-2, 2-4, an explicit gap 4-5, 5-6. A1 moved to leave 3-4 empty.
        seq.tracks[2].clips[1].timeline_in = tc(4.0);
        seq.tracks[2].clips[1].timeline_out = tc(7.0);
        assert_eq!(gaps(&seq.tracks[0]), vec![(tc(4.0), tc(5.0))]);
        assert_eq!(gap_at(&seq.tracks[2], tc(3.5)), Some((tc(3.0), tc(4.0))));
        assert_eq!(gap_at(&seq.tracks[2], tc(1.0)), None);
        assert_eq!(next_gap(&seq, tc(0.0)), Some((2, tc(3.0), tc(4.0))), "the earliest, on A1");
        assert_eq!(next_gap(&seq, tc(3.5)), Some((0, tc(4.0), tc(5.0))));
        assert_eq!(close_all_gaps(&mut seq.tracks[0]), 1);
        let ends: Vec<f64> = seq.tracks[0].clips.iter().map(|c| c.timeline_out.to_secs_f64()).collect();
        assert_eq!(ends, vec![2.0, 4.0, 5.0], "the last clip moved up to 4-5");
    }
}

