//! The primary track stays gapless (#3060): an op that leaves a stretch
//! of it uncovered (a place past its end, a removal or a trim that
//! doesn't ripple, a move of its last clip) fills that stretch with a gap
//! clip in the same step, so no later magnetic edit packs the track
//! without a word; the undo takes the gap clip away with the edit.

use klipp_core::{MediaId, RationalTime};
use klipp_document::apply::DocumentView;
use klipp_document::ops::{resolve, Op};
use klipp_timeline::{Clip, Sequence, TrackRole};

fn t(secs: f64) -> RationalTime {
    RationalTime::from_secs_f64(secs, 24)
}

/// A sequence whose primary holds 0-2, 2-4 and 4-6 s, and a pool with
/// one long item for the place op.
fn fixture() -> (Sequence, klipp_media::MediaPool, MediaId, [klipp_core::ClipId; 3]) {
    let mut seq = Sequence::new_default();
    let p = seq.tracks.iter().position(|t| t.magnetic_role == TrackRole::Primary).unwrap();
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/gapless.mov".into(), None);
    item.duration = t(60.0);
    let media = pool.add(item);
    let clips: Vec<Clip> = (0..3).map(|i| Clip::media(media, t(2.0 * i as f64), t(2.0 * i as f64 + 2.0), t(0.0), t(2.0))).collect();
    let ids = [clips[0].id, clips[1].id, clips[2].id];
    seq.tracks[p].clips = clips;
    (seq, pool, media, ids)
}

/// `op` applied to the fixture; the primary is gapless after it, and its
/// undo puts the sequence back exactly.
fn holds(name: &str, seq: Sequence, pool: klipp_media::MediaPool, op: serde_json::Value) -> Sequence {
    let op: Op = serde_json::from_value(op).unwrap_or_else(|e| panic!("{name}: {e}"));
    let plan = resolve(&seq, &pool, &[op]).unwrap_or_else(|r| panic!("{name}: {}", r.why));
    let (mut seqs, mut p, mut fps) = (vec![seq.clone()], pool, seq.fps);
    let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
    for s in &plan.steps {
        let _ = view.apply_forward(&s.edit);
    }
    let after = view.sequences[0].clone();
    assert!(after.primary_holes().is_none(), "{name}: a hole left on the primary: {:?}", after.primary_holes());
    #[cfg(debug_assertions)]
    after.assert_primary_gapless();
    for s in plan.steps.iter().rev() {
        let _ = view.apply_inverse(&s.edit);
    }
    assert!(serde_json::to_value(&view.sequences[0]).unwrap() == serde_json::to_value(&seq).unwrap(), "{name}: the undo puts it back");
    after
}

#[test]
fn every_op_that_leaves_a_hole_on_the_primary_fills_it() {
    let (seq, pool, media, ids) = fixture();
    let p = seq.primary_track_index().unwrap();
    let id = |i: usize| ids[i].0.to_string();
    let placed = holds("place past the end", seq.clone(), pool.clone(), serde_json::json!({ "op": "place", "media_id": media.0.to_string(), "track": p, "at": { "secs": 9.0 }, "source_in": 0.0, "source_out": 1.0, "with_audio": false }));
    assert!(placed.tracks[p].clips.iter().any(|c| c.kind.is_gap() && c.timeline_in == t(6.0) && c.timeline_out == t(9.0)), "a gap clip 6-9 s");
    let removed = holds("remove without ripple", seq.clone(), pool.clone(), serde_json::json!({ "op": "remove", "clip_id": id(1), "ripple": false }));
    assert!(removed.tracks[p].clips.iter().any(|c| c.kind.is_gap() && c.timeline_in == t(2.0) && c.timeline_out == t(4.0)), "a gap clip where it was");
    holds("move the last clip later", seq.clone(), pool.clone(), serde_json::json!({ "op": "move", "clip_id": id(2), "to": { "secs": 5.0 } }));
    holds("trim an out without ripple", seq.clone(), pool.clone(), serde_json::json!({ "op": "trim", "clip_id": id(1), "edge": "out", "to": { "secs": 3.5 } }));
    let trimmed = holds("trim an in without ripple", seq.clone(), pool.clone(), serde_json::json!({ "op": "trim", "clip_id": id(0), "edge": "in", "to": { "secs": 0.5 } }));
    assert!(trimmed.tracks[p].clips.iter().any(|c| c.kind.is_gap() && c.timeline_in.to_secs_f64() == 0.0 && c.timeline_out == t(0.5)), "a gap clip of half a second, not rounded");
}

/// The fixture with its middle clip taken out: 0-2 s, a gap clip 2-4 s,
/// 4-6 s.
fn with_a_gap() -> (Sequence, klipp_media::MediaPool, [klipp_core::ClipId; 3]) {
    let (mut seq, pool, _, ids) = fixture();
    let p = seq.primary_track_index().unwrap();
    seq.tracks[p].clips.retain(|c| c.id != ids[1]);
    seq.fill_primary_holes();
    (seq, pool, ids)
}

/// A gap clip is room, not a shot: the first clip slowed into the gap
/// after it, by a stretch or a retime without ripple, takes the gap's
/// room and moves nothing; the rest of the gap stays a gap clip; the
/// sequence is as long as it was. A stretch past the gap into the next
/// shot is still refused.
#[test]
fn a_clip_slowed_into_a_gap_clip_takes_its_room() {
    let (seq, pool, ids) = with_a_gap();
    let p = seq.primary_track_index().unwrap();
    let length = seq.total_duration();
    let id = |i: usize| ids[i].0.to_string();
    let span = |s: &Sequence, i: usize| s.tracks[p].clips.iter().find(|c| c.id == ids[i]).map(|c| (c.timeline_in, c.timeline_out)).unwrap();
    let gaps = |s: &Sequence| s.tracks[p].clips.iter().filter(|c| c.kind.is_gap()).map(|c| (c.timeline_in, c.timeline_out)).collect::<Vec<_>>();

    let part = holds("stretch into part of the gap", seq.clone(), pool.clone(), serde_json::json!({ "op": "stretch", "clip_id": id(0), "end_at_secs": 3.0 }));
    assert_eq!(span(&part, 0), (t(0.0), t(3.0)), "the clip ends at 3 s");
    assert_eq!(gaps(&part), vec![(t(3.0), t(4.0))], "a gap clip for what is left of the room");
    assert_eq!(span(&part, 2), (t(4.0), t(6.0)), "the next shot stays");
    assert_eq!(part.total_duration(), length);

    let whole = holds("stretch over the whole gap", seq.clone(), pool.clone(), serde_json::json!({ "op": "stretch", "clip_id": id(0), "end_at_secs": 4.0 }));
    assert_eq!(span(&whole, 0), (t(0.0), t(4.0)));
    assert!(gaps(&whole).is_empty(), "no gap left: {:?}", gaps(&whole));
    assert_eq!(span(&whole, 2), (t(4.0), t(6.0)));

    let retimed = holds("retime at half speed into the gap", seq.clone(), pool.clone(), serde_json::json!({ "op": "retime", "clip_id": id(0), "speed": 0.5 }));
    assert_eq!(span(&retimed, 0), (t(0.0), t(4.0)), "half speed, twice as long, over the gap");
    assert_eq!(span(&retimed, 2), (t(4.0), t(6.0)));
    assert_eq!(retimed.total_duration(), length);

    for op in [serde_json::json!({ "op": "stretch", "clip_id": id(0), "end_at_secs": 5.0 }), serde_json::json!({ "op": "retime", "clip_id": id(0), "speed": 0.25 })] {
        let op: Op = serde_json::from_value(op).unwrap();
        let Err(refused) = resolve(&seq, &pool, &[op]) else { panic!("past the gap into the next shot is refused") };
        assert!(refused.why.contains("would run"), "{}", refused.why);
        assert!(!refused.why.contains(" s on track"), "the track said with each clip, not again after the time: {}", refused.why);
    }
}

/// #3115: a retime fitted to fill runs over the gap clip to the next
/// shot, not to the gap clip's start; `length_secs` and `fit_range` set
/// the clip's length and work out the speed; two ways at once are
/// refused.
#[test]
fn a_retime_fits_to_the_next_shot_a_length_or_the_marked_range() {
    let (mut seq, pool, ids) = with_a_gap();
    let p = seq.primary_track_index().unwrap();
    let id = |i: usize| ids[i].0.to_string();
    let first = |s: &Sequence| s.tracks[p].clips.iter().find(|c| c.id == ids[0]).cloned().unwrap();

    let fitted = holds("fit to fill over the gap clip", seq.clone(), pool.clone(), serde_json::json!({ "op": "retime", "clip_id": id(0), "fit": true }));
    assert_eq!((first(&fitted).timeline_in, first(&fitted).timeline_out), (t(0.0), t(4.0)), "to the next shot at 4 s");
    assert!((first(&fitted).speed - 0.5).abs() < 1e-9, "2 s of source over 4 s: {}", first(&fitted).speed);

    let long = holds("a length", seq.clone(), pool.clone(), serde_json::json!({ "op": "retime", "clip_id": id(0), "length_secs": 3.0 }));
    assert_eq!(first(&long).timeline_out, t(3.0));
    assert!((first(&long).speed - 2.0 / 3.0).abs() < 1e-9, "{}", first(&long).speed);

    seq.in_point = Some(t(1.0));
    seq.out_point = Some(t(4.5));
    let ranged = holds("the marked range's length", seq.clone(), pool.clone(), serde_json::json!({ "op": "retime", "clip_id": id(0), "fit_range": true, "ripple": true }));
    assert_eq!(first(&ranged).timeline_out, t(3.5), "3.5 s, the range's length, from where it starts");

    for (op, why) in [
        (serde_json::json!({ "op": "retime", "clip_id": id(0), "fit": true, "length_secs": 3.0 }), "more than one"),
        (serde_json::json!({ "op": "retime", "clip_id": id(0), "length_secs": 0.0 }), "no length"),
    ] {
        let op: Op = serde_json::from_value(op).unwrap();
        let Err(refused) = resolve(&seq, &pool, &[op]) else { panic!("{why}: refused") };
        assert!(refused.why.contains(why), "{}", refused.why);
    }
    seq.in_point = None;
    let op: Op = serde_json::from_value(serde_json::json!({ "op": "retime", "clip_id": id(0), "fit_range": true })).unwrap();
    let Err(refused) = resolve(&seq, &pool, &[op]) else { panic!("no range: refused") };
    assert!(refused.why.contains("no marked range"), "{}", refused.why);
}

/// #3116: a generator placed over a gap clip takes its room, whole or in
/// part, the rest staying a gap clip; over a shot it is still refused.
#[test]
fn a_generator_fills_a_gap_clip() {
    let (seq, pool, ids) = with_a_gap();
    let p = seq.primary_track_index().unwrap();
    let black = serde_json::json!({ "SolidColor": { "color": [0, 0, 0, 255] } });
    let kinds = |s: &Sequence| s.tracks[p].clips.iter().map(|c| (c.kind.is_gap(), matches!(c.kind, klipp_timeline::ClipKind::Generator(_)), c.timeline_in, c.timeline_out)).collect::<Vec<_>>();

    let whole = holds("fill the whole gap", seq.clone(), pool.clone(), serde_json::json!({ "op": "generator", "data": black, "at": { "secs": 2.0 }, "end": { "secs": 4.0 }, "track": p }));
    assert!(kinds(&whole).contains(&(false, true, t(2.0), t(4.0))), "{:?}", kinds(&whole));
    assert!(!kinds(&whole).iter().any(|k| k.0), "no gap clip left: {:?}", kinds(&whole));
    assert_eq!(whole.tracks[p].clips.iter().find(|c| c.id == ids[2]).map(|c| c.timeline_in), Some(t(4.0)), "the next shot stays");

    let part = holds("fill part of the gap", seq.clone(), pool.clone(), serde_json::json!({ "op": "generator", "data": black, "at": { "secs": 2.0 }, "end": { "secs": 3.0 }, "track": p }));
    assert!(kinds(&part).contains(&(false, true, t(2.0), t(3.0))) && kinds(&part).contains(&(true, false, t(3.0), t(4.0))), "{:?}", kinds(&part));

    let op: Op = serde_json::from_value(serde_json::json!({ "op": "generator", "data": black, "at": { "secs": 2.0 }, "end": { "secs": 5.0 }, "track": p })).unwrap();
    let Err(refused) = resolve(&seq, &pool, &[op]) else { panic!("over the next shot: refused") };
    assert!(refused.why.contains("overlap a clip"), "{}", refused.why);
}
