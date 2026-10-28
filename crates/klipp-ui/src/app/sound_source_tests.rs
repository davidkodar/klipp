//! A sound-only source laid by an overwrite (#2940): it goes to the audio
//! track it is sent to, or a free one, and the clip's sound on A1 is the
//! same, byte for byte, after as before. The overwrite and insert
//! commands take their audio track from `source_edit_track`, which is
//! `Sequence::sound_source_track_or_new` for a sound-only source.

use klipp_core::{MediaId, Timecode};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

fn t(s: f64) -> Timecode {
    Timecode::from_secs_f64(s, 24)
}

/// V1 a clip with its sound on A1 over 0..10 s, A2 empty.
fn edit() -> Sequence {
    let shot = MediaId::new();
    let mut picture = Clip::media(shot, t(0.0), t(10.0), t(0.0), t(10.0));
    let mut sound = Clip::media(shot, t(0.0), t(10.0), t(0.0), t(10.0));
    picture.linked_clip_id = Some(sound.id);
    sound.linked_clip_id = Some(picture.id);
    let mut seq = Sequence::new_default();
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![picture];
    let mut a1 = Track::new("A1", TrackKind::Audio);
    a1.clips = vec![sound];
    seq.tracks = vec![v1, a1, Track::new("A2", TrackKind::Audio)];
    seq
}

/// A 204 s piece of music overwritten from 2 s, as `.` lays it: its track,
/// and the sequence after.
fn overwrite_music(mut seq: Sequence) -> (usize, Sequence) {
    let music = Clip::media(MediaId::new(), t(2.0), t(206.0), t(0.0), t(204.0));
    let at = seq.sound_source_track_or_new(music.timeline_in, music.timeline_out);
    super::KlippApp::overwrite_insert_clip_on_track(&mut seq.tracks[at], music);
    (at, seq)
}

/// A1's clips as saved: its sound (its targeting is the badge's to change).
fn a1_bytes(seq: &Sequence) -> String {
    serde_json::to_string(&seq.tracks[1].clips).unwrap()
}

#[test]
fn a_sound_only_source_with_a2_targeted_lands_on_a2() {
    let mut seq = edit();
    let before = a1_bytes(&seq);
    seq.target_from_badge(2);
    let (at, after) = overwrite_music(seq);
    assert_eq!(at, 2, "on A2");
    assert_eq!(after.tracks[2].clips.len(), 1);
    assert_eq!(a1_bytes(&after), before, "A1's sound is untouched");
}

#[test]
fn a_sound_only_source_with_no_target_lands_on_a_free_track() {
    let seq = edit();
    let before = a1_bytes(&seq);
    let (at, after) = overwrite_music(seq);
    assert_eq!(at, 2, "the free A2, not A1");
    assert_eq!(a1_bytes(&after), before, "A1's sound is untouched");
    // Again: A2 is taken now, so a new track.
    let (again, after) = overwrite_music(after);
    assert_eq!((again, after.tracks.len()), (3, 4), "a new audio track");
    assert_eq!(a1_bytes(&after), before);
}
