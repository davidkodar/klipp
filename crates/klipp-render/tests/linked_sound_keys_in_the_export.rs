//! A linked pair's level keys are on its picture, timed from the
//! picture's start. In the export they land where the picture puts them,
//! whenever the sound starts: a J cut's sound starts before its picture,
//! and the keys had landed that much early; an L cut's incoming sound
//! starts after it, and they landed that much late. A sound that starts
//! with its picture (a plain pair, an L cut's outgoing sound) is read as
//! it was.

use klipp_core::{ExportSettings, Keyframe, Timecode, WorkingColorspace};
use klipp_media::{MediaItem, MediaPool};
use klipp_render::audio_mixdown::levels_at;
use klipp_render::snapshot::{AudioClipSnapshot, SequenceSnapshot};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

fn tc(s: f64) -> Timecode {
    Timecode::from_secs_f64(s, 24)
}

fn key(t: f64, v: f32) -> Keyframe {
    Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

/// Picture A 0-2 s and B 2-4 s, B's level rising over its picture's
/// first second (0.2 at its start, full a second in), A's falling over
/// its second second; A's sound ends and B's starts at `cut` seconds:
/// 1.5 a J cut, 2.5 an L cut, 2 a plain pair.
fn pair(cut: f64) -> (Sequence, MediaPool) {
    let mut pool = MediaPool::new();
    let mut item = MediaItem::synthetic_for_test("/tmp/linked-sound-keys.wav".into(), None);
    item.has_audio = true;
    item.duration = tc(10.0);
    let id = pool.add(item);
    let mut a = Clip::media(id, tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let mut b = Clip::media(id, tc(2.0), tc(4.0), tc(5.0), tc(7.0));
    let mut a_sound = Clip::media(id, tc(0.0), tc(cut), tc(0.0), tc(cut));
    let mut b_sound = Clip::media(id, tc(cut), tc(4.0), tc(3.0 + cut), tc(7.0));
    a_sound.linked_clip_id = Some(a.id);
    a.linked_clip_id = Some(a_sound.id);
    b_sound.linked_clip_id = Some(b.id);
    b.linked_clip_id = Some(b_sound.id);
    b.volume_keyframes = vec![key(0.0, 0.2), key(1.0, 1.0)];
    a.volume_keyframes = vec![key(1.0, 1.0), key(2.0, 0.3)];
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![a, b];
    let mut s = Track::new("A1", TrackKind::Audio);
    s.clips = vec![a_sound, b_sound];
    let mut seq = Sequence::new_default();
    seq.tracks = vec![v, s];
    (seq, pool)
}

/// The sound starting at `start` seconds in the export snapshot of the
/// pair cut at `cut`.
fn sound_from(cut: f64, start: f64) -> (AudioClipSnapshot, f64) {
    let (seq, pool) = pair(cut);
    let snap = SequenceSnapshot::from_sequence(&seq, &pool, &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).unwrap();
    let fps = snap.fps;
    let sound = snap.audio_tracks.iter().flat_map(|t| t.clips.iter()).find(|c| (c.span_secs(fps).0 - start).abs() < 1.0 / fps).expect("the sound").clone();
    (sound, fps)
}

/// B's sound, from the cut.
fn b_sound(cut: f64) -> (AudioClipSnapshot, f64) {
    sound_from(cut, cut)
}

fn level(sound: &AudioClipSnapshot, fps: f64, t: f64) -> f32 {
    let start = sound.span_secs(fps).0;
    levels_at(&sound.shape, t - start, t, fps).0
}

#[test]
fn a_j_cuts_picture_keys_land_where_the_picture_puts_them() {
    let (sound, fps) = b_sound(1.5);
    assert!((sound.shape.settings_offset_secs - 0.5).abs() < 1e-9, "the picture starts half a second into the sound");
    let (early, at_cut, a_second_in) = (level(&sound, fps, 1.6), level(&sound, fps, 2.0), level(&sound, fps, 3.0));
    assert!((early - 0.2).abs() < 1e-3, "before the picture, its first key: {early:.3}");
    assert!((at_cut - 0.2).abs() < 1e-3, "at the cut the picture's first key, 0.2: {at_cut:.3}");
    assert!((a_second_in - 1.0).abs() < 1e-3, "a second into the picture, full: {a_second_in:.3}");
}

/// An L cut's incoming sound starts half a second after its picture:
/// its keys land at the picture's times too, not half a second late.
#[test]
fn an_l_cuts_incoming_keys_land_where_the_picture_puts_them() {
    let (sound, fps) = b_sound(2.5);
    assert!((sound.shape.settings_offset_secs + 0.5).abs() < 1e-9, "the picture starts half a second before the sound");
    let (at_start, a_second_in) = (level(&sound, fps, 2.5), level(&sound, fps, 3.0));
    let half_way = klipp_core::keyframe::evaluate_level(&[key(0.0, 0.2), key(1.0, 1.0)], 0.5, 1.0);
    assert!((at_start - half_way).abs() < 1e-3, "the sound comes in half way up the picture's ramp: {at_start:.3} vs {half_way:.3}");
    assert!((a_second_in - 1.0).abs() < 1e-3, "a second into the picture, full: {a_second_in:.3}");
}

/// A sound that starts with its picture, a plain pair's and an L cut's
/// outgoing one, is read exactly as before: its keys at the time into
/// the sound.
#[test]
fn a_sound_starting_with_its_picture_reads_its_keys_as_before() {
    for (cut, start) in [(2.5, 0.0), (2.0, 0.0), (2.0, 2.0)] {
        let (sound, fps) = sound_from(cut, start);
        assert_eq!(sound.shape.settings_offset_secs, 0.0, "cut at {cut} s, the sound from {start} s: no offset");
        assert!(!sound.shape.volume_keyframes.is_empty(), "keys to read");
        let end = sound.span_secs(fps).1;
        let mut t = start;
        while t < end {
            let before = klipp_core::keyframe::evaluate_level(&sound.shape.volume_keyframes, t - start, sound.shape.volume);
            assert_eq!(level(&sound, fps, t), before, "cut at {cut} s, the sound from {start} s, at {t:.2} s");
            t += 0.05;
        }
    }
}
