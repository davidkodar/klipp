//! CMX 3600 EDL import (#925): an edit decision list read back into a
//! sequence, its events matched to media already in the pool.
//!
//! The parser is pure ([`parse`]); [`build`] lays the events out as clips,
//! asking the caller which media an event names. An event's media is the
//! pool item named by its `* FROM CLIP NAME` comment, else the one whose
//! reel (as [`crate::fcpxml::export_edl`] names reels) matches the event's.
//! Source timecodes are taken relative to the file's own start timecode
//! when it has one and they lie past it (a conform EDL), and from zero
//! otherwise (what Klipp's own export writes). Cuts are imported; a
//! dissolve or wipe is imported as a cut and listed in the warnings, as is
//! any event whose media is not in the pool.

use klipp_core::{Fps, MediaId, Timecode};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

/// One event of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub number: u32,
    pub reel: String,
    /// "V", "A", "A1".."A4", "AA", "B" (picture and sound).
    pub channel: String,
    /// "C" for a cut; "D", "W…", "K…" for a transition.
    pub transition: String,
    /// Source and record timecodes, in frames at the list's rate.
    pub src_in: i64,
    pub src_out: i64,
    pub rec_in: i64,
    pub rec_out: i64,
    /// The `* FROM CLIP NAME` comment after it, if any.
    pub clip_name: Option<String>,
}

/// A parsed list: its title, whether its timecodes are drop-frame, and
/// its events.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Edl {
    pub title: Option<String>,
    pub drop_frame: bool,
    pub events: Vec<Event>,
}

/// Frames from "hh:mm:ss:ff" (or ";" before the frames for drop-frame) at
/// `fps`.
fn frames_of(tc: &str, fps: Fps) -> Option<i64> {
    let secs = klipp_core::sync::timecode_to_secs(tc, fps)?;
    Some((secs * fps.as_f64()).round() as i64)
}

/// Read a CMX 3600 list at `fps`. Lines it does not understand are passed
/// over; an event line needs its eight fields.
pub fn parse(text: &str, fps: Fps) -> Edl {
    let mut edl = Edl::default();
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(title) = line.strip_prefix("TITLE:") {
            edl.title = Some(title.trim().to_string());
            continue;
        }
        if let Some(fcm) = line.strip_prefix("FCM:") {
            edl.drop_frame = fcm.trim().eq_ignore_ascii_case("DROP FRAME");
            continue;
        }
        if let Some(name) = line.trim_start().strip_prefix("* FROM CLIP NAME:") {
            if let Some(last) = edl.events.last_mut() {
                last.clip_name = Some(name.trim().to_string());
            }
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let Some(number) = f.first().and_then(|n| n.parse::<u32>().ok()) else { continue };
        // num reel channel transition [duration] src_in src_out rec_in rec_out
        if f.len() < 8 {
            continue;
        }
        let tcs: Vec<i64> = f[f.len() - 4..].iter().filter_map(|tc| frames_of(tc, fps)).collect();
        let [src_in, src_out, rec_in, rec_out] = tcs.as_slice() else { continue };
        edl.events.push(Event {
            number,
            reel: f[1].to_string(),
            channel: f[2].to_string(),
            transition: f[3].to_string(),
            src_in: *src_in,
            src_out: *src_out,
            rec_in: *rec_in,
            rec_out: *rec_out,
            clip_name: None,
        });
    }
    edl
}

/// The media an event is matched to, with that media's start timecode in
/// frames when it has one.
pub struct Matched {
    pub media_id: MediaId,
    pub start_frames: Option<i64>,
}

/// Lay `edl` out as a sequence named `name` at `fps`, `find` naming each
/// event's media. Returns the sequence and the warnings: events without
/// media, and transitions imported as cuts.
pub fn build(edl: &Edl, name: &str, fps: Fps, mut find: impl FnMut(&Event) -> Option<Matched>) -> (Sequence, Vec<String>) {
    let mut seq = Sequence::new_default();
    seq.name = name.to_string();
    seq.fps = fps;
    seq.tracks.clear();
    let mut warnings = Vec::new();
    let tc = |frames: i64| Timecode { value: frames * i64::from(fps.den), rate: fps.num };
    for e in &edl.events {
        let Some(m) = find(e) else {
            warnings.push(format!("Event {:03}: no media in the pool for reel {}{}", e.number, e.reel, e.clip_name.as_deref().map(|n| format!(" ({n})")).unwrap_or_default()));
            continue;
        };
        if e.transition != "C" {
            warnings.push(format!("Event {:03}: transition {} imported as a cut", e.number, e.transition));
        }
        // A conform list counts source frames from the file's timecode.
        let base = m.start_frames.filter(|&start| e.src_in >= start).unwrap_or(0);
        let (src_in, src_out) = (e.src_in - base, e.src_out - base);
        if e.rec_out <= e.rec_in || src_out <= src_in {
            warnings.push(format!("Event {:03}: no length, left out", e.number));
            continue;
        }
        let channels: Vec<(TrackKind, usize)> = match e.channel.as_str() {
            "V" => vec![(TrackKind::Video, 0)],
            "B" => vec![(TrackKind::Video, 0), (TrackKind::Audio, 0)],
            "AA" | "A12" => vec![(TrackKind::Audio, 0), (TrackKind::Audio, 1)],
            other => {
                let n = other.strip_prefix('A').and_then(|n| if n.is_empty() { Some(1) } else { n.parse::<usize>().ok() }).unwrap_or(1);
                if n > MAX_AUDIO_CHANNEL {
                    warnings.push(format!("Event {:03}: audio channel {n} is past the {MAX_AUDIO_CHANNEL} tracks an import makes, left out", e.number));
                    continue;
                }
                vec![(TrackKind::Audio, n.saturating_sub(1))]
            }
        };
        for (kind, index) in channels {
            let track = track_for(&mut seq, kind, index);
            let clip = Clip::media(m.media_id, tc(e.rec_in), tc(e.rec_out), tc(src_in), tc(src_out));
            let at = seq.tracks[track].clips.partition_point(|c| c.timeline_in < clip.timeline_in);
            seq.tracks[track].clips.insert(at, clip);
        }
    }
    seq.renumber_tracks();
    (seq, warnings)
}

/// The most audio tracks an EDL import makes: an event on a channel past
/// it is left out with a warning.
pub const MAX_AUDIO_CHANNEL: usize = 64;

/// The `index`th track of `kind`, made as needed: picture tracks above
/// the sound ones.
fn track_for(seq: &mut Sequence, kind: TrackKind, index: usize) -> usize {
    loop {
        let found: Vec<usize> = seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == kind).map(|(i, _)| i).collect();
        if let Some(&i) = found.get(index) {
            return i;
        }
        let name = seq.next_track_name(kind);
        let at = match kind {
            TrackKind::Video => seq.tracks.iter().position(|t| t.kind != TrackKind::Video).unwrap_or(seq.tracks.len()),
            _ => seq.tracks.len(),
        };
        seq.tracks.insert(at, Track::new(&name, kind));
    }
}

/// Match an event to media in `pool`: by its clip name (the file's name),
/// then by its reel against each item's reel as the export names it.
pub fn match_in_pool(pool: &MediaPool, e: &Event, fps: Fps) -> Option<Matched> {
    let by_name = e.clip_name.as_deref().and_then(|n| pool.all().find(|m| m.filename == n || m.path.file_name().is_some_and(|f| f.to_string_lossy() == n)));
    let by_reel = || {
        pool.all().find(|m| {
            let reel = m.reel().map(str::to_string).filter(|r| !r.trim().is_empty()).unwrap_or_else(|| m.path.file_stem().unwrap_or_default().to_string_lossy().into_owned());
            crate::fcpxml::sanitise_reel(&reel) == e.reel
        })
    };
    let item = by_name.or_else(by_reel)?;
    let start_frames = item.shot.start_timecode.as_deref().and_then(|tc| frames_of(tc, if item.fps.num == 0 { fps } else { item.fps }));
    Some(Matched { media_id: item.id, start_frames })
}

#[cfg(test)]
mod tests {

    /// An event on an audio channel past [`MAX_AUDIO_CHANNEL`] is left out
    /// with a warning; no track is made for it.
    #[test]
    fn a_channel_past_the_cap_makes_no_tracks() {
        let list = "TITLE: X\nFCM: NON-DROP FRAME\n\n\
001  A001C001 A999999999 C        00:00:01:00 00:00:03:00 00:00:00:00 00:00:02:00\n\
002  A001C001 A64   C        00:00:01:00 00:00:03:00 00:00:02:00 00:00:04:00\n";
        let edl = parse(list, fps());
        let m = MediaId::new();
        let (seq, warnings) = build(&edl, "X", fps(), |_| Some(Matched { media_id: m, start_frames: None }));
        assert!(warnings.iter().any(|w| w.contains("Event 001") && w.contains("left out")), "{warnings:?}");
        let audio = seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio).count();
        assert_eq!(audio, 64, "channel 64 is made; 999999999 is not");
    }

    use super::*;

    const LIST: &str = "TITLE: Scene 1\nFCM: NON-DROP FRAME\n\n\
001  A001C001 V     C        00:00:01:00 00:00:03:00 00:00:00:00 00:00:02:00\n\
* FROM CLIP NAME: A001C001.mov\n\n\
002  A001C002 V     D    012 01:00:10:00 01:00:12:00 00:00:02:00 00:00:04:00\n\
002  A001C002 A1    C        01:00:10:00 01:00:12:00 00:00:02:00 00:00:04:00\n\
003  ZZZZ     V     C        00:00:00:00 00:00:01:00 00:00:04:00 00:00:05:00\n";

    fn fps() -> Fps {
        Fps { num: 25, den: 1 }
    }

    #[test]
    fn events_their_timecodes_and_clip_names_are_read() {
        let edl = parse(LIST, fps());
        assert_eq!(edl.title.as_deref(), Some("Scene 1"));
        assert!(!edl.drop_frame);
        assert_eq!(edl.events.len(), 4);
        let first = &edl.events[0];
        assert_eq!((first.number, first.reel.as_str(), first.channel.as_str(), first.transition.as_str()), (1, "A001C001", "V", "C"));
        assert_eq!((first.src_in, first.src_out, first.rec_in, first.rec_out), (25, 75, 0, 50));
        assert_eq!(first.clip_name.as_deref(), Some("A001C001.mov"));
        let dissolve = &edl.events[1];
        assert_eq!((dissolve.transition.as_str(), dissolve.src_in), ("D", 90250), "the duration field is passed over");
    }

    #[test]
    fn the_sequence_is_built_with_conform_timecodes_and_warnings() {
        let edl = parse(LIST, fps());
        let (a, b) = (MediaId::new(), MediaId::new());
        let (seq, warnings) = build(&edl, "Scene 1", fps(), |e| match e.reel.as_str() {
            "A001C001" => Some(Matched { media_id: a, start_frames: None }),
            // The second file starts at 01:00:10:00, so its events count from there.
            "A001C002" => Some(Matched { media_id: b, start_frames: Some(90250) }),
            _ => None,
        });
        let v: Vec<&Clip> = seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).flat_map(|t| t.clips.iter()).collect();
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].source_in.value, v[0].timeline_in.value), (25, 0), "from zero: Klipp's own export");
        assert_eq!((v[1].source_in.value, v[1].source_out.value, v[1].timeline_in.value), (0, 50, 50), "relative to the file's timecode");
        assert_eq!(seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio).flat_map(|t| t.clips.iter()).count(), 1);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("002") && w.contains("as a cut")));
        assert!(warnings.iter().any(|w| w.contains("003") && w.contains("ZZZZ")));
    }
}
