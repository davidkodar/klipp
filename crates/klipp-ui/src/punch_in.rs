//! Recording to picture (#114): where a take starts and lands. Arm an
//! audio track, set an In (or leave the playhead where the line goes),
//! press Record: playback rolls from a pre-roll before the In, the take
//! is captured from the roll, and on stop it lands on the armed track at
//! the In, its pre-roll kept as a handle before the clip's start. With an
//! Out set too, recording stops there. A take over an earlier one goes on
//! a track of its own below, so the takes stack to choose between.
//!
//! Pure, and tested; the app runs it from StartRecording, StopRecording
//! and the loaded take.

use klipp_core::{MediaId, Timecode, TrackId};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

/// A recording's place on the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Punch {
    pub track_id: TrackId,
    /// Where playback rolls from, and the take's first sample.
    pub roll_from: Timecode,
    /// Where the take lands.
    pub punch_in: Timecode,
    /// Where recording stops by itself, if an Out is set.
    pub punch_out: Option<Timecode>,
}

/// The punch for a recording on `track_id`: at the sequence's In when it
/// has one (and its Out, when that is after it), else at the playhead,
/// rolling `pre_roll_secs` before, never before the start.
pub fn plan(seq: &Sequence, track_id: TrackId, playhead: Timecode, pre_roll_secs: f64) -> Punch {
    let punch_in = seq.in_point.unwrap_or(playhead);
    let punch_out = seq.in_point.and(seq.out_point).filter(|out| *out > punch_in);
    let roll = (punch_in.to_secs_f64() - pre_roll_secs.max(0.0)).max(0.0);
    Punch { track_id, roll_from: Timecode::from_secs_f64(roll, punch_in.rate), punch_in, punch_out }
}

/// Where a take goes: on the armed track, or a new track to be made at
/// `index` because the armed one has something there already.
#[derive(Debug, Clone)]
pub enum TakeTrack {
    Armed(usize),
    New { index: usize, track: Box<Track> },
}

/// The take `media_id`, `take_secs` long, as a clip and where it goes.
/// `None` when the armed track is gone or the take ends before the In.
pub fn place(seq: &Sequence, punch: &Punch, media_id: MediaId, take_secs: f64) -> Option<(TakeTrack, Clip)> {
    let ti = seq.tracks.iter().position(|t| t.id == punch.track_id)?;
    let rate = punch.punch_in.rate;
    let handle = punch.punch_in.to_secs_f64() - punch.roll_from.to_secs_f64();
    let mut end = take_secs;
    if let Some(out) = punch.punch_out {
        end = end.min(out.to_secs_f64() - punch.roll_from.to_secs_f64());
    }
    if end <= handle + 1e-3 {
        return None;
    }
    let source_in = Timecode::from_secs_f64(handle, rate);
    let source_out = Timecode::from_secs_f64(end, rate);
    let timeline_out = Timecode::from_secs_f64(punch.punch_in.to_secs_f64() + end - handle, rate);
    let clip = Clip::media(media_id, punch.punch_in, timeline_out, source_in, source_out);
    let armed = &seq.tracks[ti];
    let taken = armed.clips.iter().any(|c| c.timeline_in < clip.timeline_out && c.timeline_out > clip.timeline_in);
    if !taken {
        return Some((TakeTrack::Armed(ti), clip));
    }
    // Takes stack below the armed track: "Voice Take 2", "Voice Take 3".
    let stem = format!("{} Take", armed.name);
    let n = seq.tracks.iter().filter(|t| t.name.starts_with(&stem)).count() + 2;
    let below = (ti + 1..seq.tracks.len()).take_while(|&i| seq.tracks[i].name.starts_with(&stem)).last().unwrap_or(ti) + 1;
    Some((TakeTrack::New { index: below, track: Box::new(Track::new(&format!("{stem} {n}"), TrackKind::Audio)) }, clip))
}

/// The folder a take is written to: a saved project's own Recordings
/// folder, else `cache`'s recordings. An unsaved project's path is the
/// placeholder "Untitled", which is no folder: a take there would land
/// beside wherever the app was started.
pub fn take_dir(project_path: Option<&std::path::Path>, cache: &std::path::Path) -> std::path::PathBuf {
    match project_path {
        Some(dir) if dir.is_absolute() && dir.is_dir() => dir.join("Recordings"),
        _ => cache.join("recordings"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;

    fn t(secs: f64) -> Timecode {
        RationalTime::from_secs_f64(secs, 25)
    }

    fn seq() -> (Sequence, TrackId) {
        let mut seq = Sequence::new_default();
        seq.tracks.clear();
        seq.tracks.push(Track::new("Video 1", TrackKind::Video));
        let voice = Track::new("Voice", TrackKind::Audio);
        let id = voice.id;
        seq.tracks.push(voice);
        (seq, id)
    }

    #[test]
    fn the_punch_is_at_the_in_with_a_pre_roll_or_at_the_playhead() {
        let (mut s, id) = seq();
        let p = plan(&s, id, t(10.0), 2.0);
        assert_eq!((p.roll_from, p.punch_in, p.punch_out), (t(8.0), t(10.0), None), "the playhead, no Out");
        assert_eq!(plan(&s, id, t(1.0), 2.0).roll_from, t(0.0), "never before the start");
        s.in_point = Some(t(20.0));
        s.out_point = Some(t(25.0));
        let p = plan(&s, id, t(3.0), 2.0);
        assert_eq!((p.roll_from, p.punch_in, p.punch_out), (t(18.0), t(20.0), Some(t(25.0))), "In and Out");
    }

    #[test]
    fn a_take_lands_at_the_in_with_its_pre_roll_as_a_handle() {
        let (s, id) = seq();
        let p = Punch { track_id: id, roll_from: t(8.0), punch_in: t(10.0), punch_out: None };
        let media = MediaId::new();
        let (track, clip) = place(&s, &p, media, 7.0).unwrap();
        assert!(matches!(track, TakeTrack::Armed(1)));
        assert_eq!((clip.timeline_in, clip.timeline_out), (t(10.0), t(15.0)));
        assert_eq!((clip.source_in, clip.source_out), (t(2.0), t(7.0)), "the two seconds of roll before the In are a handle");
        // Stopped during the pre-roll: nothing to place.
        assert!(place(&s, &p, media, 1.5).is_none());
        // An Out cuts the take there.
        let p = Punch { punch_out: Some(t(12.0)), ..p };
        let (_, clip) = place(&s, &p, media, 7.0).unwrap();
        assert_eq!(clip.timeline_out, t(12.0));
    }

    #[test]
    fn a_take_over_another_stacks_on_a_track_below() {
        let (mut s, id) = seq();
        let p = Punch { track_id: id, roll_from: t(8.0), punch_in: t(10.0), punch_out: None };
        let (_, first) = place(&s, &p, MediaId::new(), 7.0).unwrap();
        s.tracks[1].clips.push(first);
        let Some((TakeTrack::New { index, track }, _)) = place(&s, &p, MediaId::new(), 6.0) else { panic!("a new track") };
        assert_eq!((index, track.name.as_str(), track.kind), (2, "Voice Take 2", TrackKind::Audio));
        s.tracks.insert(index, *track);
        let Some((TakeTrack::New { index, track }, _)) = place(&s, &p, MediaId::new(), 6.0) else { panic!() };
        assert_eq!((index, track.name.as_str()), (3, "Voice Take 3"), "below the takes before it");
    }

    #[test]
    fn an_unsaved_projects_takes_go_to_the_cache() {
        let cache = std::env::temp_dir().join("klipp-take-dir-cache");
        assert_eq!(take_dir(None, &cache), cache.join("recordings"));
        assert_eq!(take_dir(Some(std::path::Path::new("Untitled")), &cache), cache.join("recordings"), "the placeholder is no folder");
        let saved = std::env::temp_dir();
        assert_eq!(take_dir(Some(&saved), &cache), saved.join("Recordings"));
    }
}
