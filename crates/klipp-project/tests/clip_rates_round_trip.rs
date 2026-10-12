//! A clip's four times may carry different rates: a clip appended at its
//! media's rate keeps it, and a blade or a move at the sequence's rate
//! gives it times at that rate too. The project stores one rate per clip,
//! so every time must come back where it was, not be read at the wrong
//! rate.

use klipp_core::RationalTime;
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, ClipKind, Sequence};

fn secs(c: &Clip) -> [f64; 4] {
    [c.timeline_in, c.timeline_out, c.source_in, c.source_out].map(|t| t.to_secs_f64())
}

/// The sequence back from a real save and load, and the saved clips'
/// stored times as `[timeline_in, timeline_out, source_in, source_out,
/// rate]`.
fn save_and_load(name: &str, seq: Sequence) -> (Sequence, Vec<[i64; 5]>) {
    let dir = std::env::temp_dir().join(format!("klipp_clip_rates_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    let stored = written["sequences"][0]["tracks"][0]["clips"]
        .as_array()
        .expect("the track's clips")
        .iter()
        .map(|c| ["timeline_in", "timeline_out", "source_in", "source_out", "rate"].map(|k| c[k].as_i64().expect(k)))
        .collect();
    let (mut sequences, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (sequences.remove(0), stored)
}

fn round_trip(name: &str, seq: Sequence) -> Sequence {
    save_and_load(name, seq).0
}

/// A five-second clip at 30 ticks a second on a 24 fps sequence.
fn sequence_with_a_30_rate_clip() -> Sequence {
    let mut seq = Sequence::new_default();
    let at30 = |s: f64| RationalTime::from_secs_f64(s, 30);
    let solid = klipp_core::GeneratorData::SolidColor { color: [10, 10, 30, 255] };
    let mut clip = Clip { kind: ClipKind::Generator(solid), ..Clip::gap(at30(0.0), at30(5.0)) };
    clip.source_in = at30(0.0);
    clip.source_out = at30(5.0);
    seq.tracks[0].clips = vec![clip];
    seq
}

#[test]
fn a_bladed_clip_s_halves_keep_their_times() {
    let mut seq = sequence_with_a_30_rate_clip();
    let fps = seq.fps;
    let id = seq.tracks[0].clips[0].id;
    let cut = RationalTime::from_secs_f64(2.0, fps.rate());
    seq.tracks[0].blade_at(id, cut, fps).expect("the cut is inside the clip");
    let before: Vec<[f64; 4]> = seq.tracks[0].clips.iter().map(secs).collect();
    assert_eq!(before, vec![[0.0, 2.0, 0.0, 2.0], [2.0, 5.0, 2.0, 5.0]]);

    let back = round_trip("blade", seq);
    let after: Vec<[f64; 4]> = back.tracks[0].clips.iter().map(secs).collect();
    assert_eq!(after, before);
}

#[test]
fn a_moved_clip_keeps_its_times() {
    let mut seq = sequence_with_a_30_rate_clip();
    // A move as the timeline makes it: the new start at the sequence's
    // rate and the end a duration on, the source untouched.
    let clip = &mut seq.tracks[0].clips[0];
    let dur = clip.duration();
    clip.timeline_in = RationalTime::from_secs_f64(1.0, 24);
    clip.timeline_out = clip.timeline_in + dur;
    let before = secs(clip);
    assert_eq!(before, [1.0, 6.0, 0.0, 5.0]);

    let back = round_trip("move", seq);
    assert_eq!(secs(&back.tracks[0].clips[0]), before);
}

/// A clip whose times share a rate is stored at that rate with its own
/// values, as it always was: the common rate changes nothing for it.
#[test]
fn a_single_rate_clip_is_stored_as_before() {
    let mut seq = sequence_with_a_30_rate_clip();
    seq.tracks[0].clips[0].timeline_in = RationalTime { value: 30, rate: 30 };
    seq.tracks[0].clips[0].timeline_out = RationalTime { value: 180, rate: 30 };
    let (back, stored) = save_and_load("single", seq);
    assert_eq!(stored, vec![[30, 180, 0, 150, 30]]);
    assert_eq!(secs(&back.tracks[0].clips[0]), [1.0, 6.0, 0.0, 5.0]);
}
