//! The water under a reflection (#2795) is measured from the picture
//! below the reflected clip: on warm, blue and dark water it reads the
//! water's own colour, and its fingerprint holds while the footage under
//! it, its range and its waterline do, and changes when any does.

use klipp_core::*;
use klipp_media::MediaPool;
use klipp_render::snapshot::SequenceSnapshot;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

fn tc(s: f64) -> Timecode {
    Timecode::from_secs_f64(s, 25)
}

/// A clip reflected in calm water over water of `water`, on the track
/// below; its waterline at `waterline`.
fn scene(water: [u8; 3], waterline: f32) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    seq.resolution = Resolution { width: 320, height: 180 };
    let solid = |c: [u8; 3]| Clip { kind: ClipKind::Generator(GeneratorData::SolidColor { color: [c[0], c[1], c[2], 255] }), ..Clip::gap(tc(0.0), tc(2.0)) };
    let reflection = klipp_effects::reflection::surface("calm_water", waterline).unwrap();
    let layer = TestClip::of(solid([230, 230, 230])).effect(reflection).build();
    let mut top = Track::new("V2", TrackKind::Video);
    top.clips = vec![layer];
    let mut below = Track::new("V1", TrackKind::Video);
    below.clips = vec![solid(water)];
    seq.tracks = vec![top, below];
    seq
}

fn snapshot(seq: &Sequence) -> SequenceSnapshot {
    SequenceSnapshot::from_sequence(seq, &MediaPool::new(), &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).unwrap()
}

#[test]
fn the_water_is_read_from_the_picture_below() {
    // A warm sunset, blue daylight, a dark night.
    for water in [[205u8, 120, 60], [40, 105, 160], [14, 20, 34]] {
        let seq = scene(water, 0.6);
        let clip = seq.tracks[0].clips[0].id;
        let got = klipp_render::water::measure(&snapshot(&seq), clip).expect("water measured").srgb;
        let off = (0..3).map(|i| got[i].abs_diff(water[i])).max().unwrap();
        assert!(off <= 3, "water {water:?} read as {got:?}");
    }
}

#[test]
fn the_fingerprint_follows_what_is_under_the_reflection() {
    let seq = scene([40, 105, 160], 0.6);
    let clip = seq.tracks[0].clips[0].id;
    let key = klipp_render::water::footprint_key(&seq, clip).unwrap();
    assert_ne!(key, 0);
    assert_eq!(klipp_render::water::footprint_key(&seq.clone(), clip), Some(key), "the same footage, the same key");
    let moved = scene([40, 105, 160], 0.5);
    let moved_clip = moved.tracks[0].clips[0].id;
    // Same footage ids for the comparison: the clip's waterline alone moved.
    let mut lower = seq.clone();
    lower.tracks[0].clips[0] = { let mut c = moved.tracks[0].clips[0].clone(); c.id = clip; c };
    let _ = moved_clip;
    assert_ne!(klipp_render::water::footprint_key(&lower, clip), Some(key), "the waterline moved");
    let mut replaced = seq.clone();
    replaced.tracks[1].clips[0].id = klipp_core::ClipId::new();
    assert_ne!(klipp_render::water::footprint_key(&replaced, clip), Some(key), "the footage under it replaced");
}
