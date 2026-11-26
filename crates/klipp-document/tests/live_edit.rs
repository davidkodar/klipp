//! The live edit a gesture shows (U2): however many frames it is shown
//! for, each in place of the last, a cancel gives the document back
//! exactly as it began, and what stays shown is the last frame's edit
//! made once. For each kind of edit a drag makes: trims, with and without
//! ripple and the linked sound, slips, slides and speed changes.

use klipp_core::RationalTime;
use klipp_document::apply::DocumentView;
use klipp_document::history::EditCommand;
use klipp_document::live::LiveEdit;
use klipp_document::ops::{resolve, Op};
use klipp_media::MediaPool;
use klipp_timeline::Sequence;
use serde_json::{json, Value};

const RATE: u32 = 24;

fn t(frames: i64) -> RationalTime {
    RationalTime::from_frames(frames, RATE)
}

/// Three 4 s clips of a picture with sound, end to end on V1 and A1.
fn fixture() -> (Vec<Sequence>, MediaPool) {
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::new(RATE, 1);
    let mut pool = MediaPool::new();
    let mut picture = klipp_media::MediaItem::synthetic_for_test("/tmp/live-edit.mov".into(), None);
    picture.duration = t(RATE as i64 * 60);
    picture.video_codec = "h264".into();
    picture.has_audio = true;
    let media = pool.add(picture);
    let mut fps = seq.fps;
    let mut seqs = vec![seq];
    for k in 0..3 {
        let op: Op = serde_json::from_value(json!({"op": "place", "media_id": media.0.to_string(), "track": 0, "at": {"secs": 4.0 * k as f64}, "source_in": 10.0 + 2.0 * k as f64, "source_out": 14.0 + 2.0 * k as f64})).unwrap();
        let plan = resolve(&seqs[0], &pool, &[op]).unwrap_or_else(|r| panic!("the fixture: {}", r.why));
        let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        for s in &plan.steps {
            view.apply_forward(&s.edit);
        }
    }
    (seqs, pool)
}

fn state(seqs: &[Sequence], pool: &MediaPool) -> Value {
    json!({"sequences": serde_json::to_value(seqs).unwrap(), "pool": serde_json::to_value(pool).unwrap()})
}

/// The edit of `op`, as the ops plan it on `seq`, all its steps as one.
fn planned(seq: &Sequence, pool: &MediaPool, op: Value) -> Option<EditCommand> {
    let op: Op = serde_json::from_value(op).unwrap();
    let plan = resolve(seq, pool, &[op]).ok()?;
    Some(EditCommand::Batch(plan.steps.into_iter().map(|s| s.edit).collect()))
}

/// A drag's frame `k` of `kind`, on the middle clip, from the start.
fn frame(kind: &str, seq: &Sequence, pool: &MediaPool, k: usize) -> Option<EditCommand> {
    let middle = seq.tracks[0].clips.iter().filter(|c| !c.kind.is_gap()).nth(1).unwrap();
    let id = middle.id.0.to_string();
    // Back and forth around where the gesture began, in whole frames.
    let swing = ((k as f64 * 0.37).sin() * 30.0).round() as i64;
    let secs = |frames: i64| t(frames).to_secs_f64();
    let edge_at = |base: RationalTime| secs((base.to_secs_f64() * RATE as f64).round() as i64 + swing);
    match kind {
        "trim out" => planned(seq, pool, json!({"op": "trim", "clip_id": id, "edge": "out", "to": {"secs": edge_at(middle.timeline_out)}, "ripple": false})),
        "trim out, ripple" => planned(seq, pool, json!({"op": "trim", "clip_id": id, "edge": "out", "to": {"secs": edge_at(middle.timeline_out)}, "ripple": true})),
        "trim in, ripple" => planned(seq, pool, json!({"op": "trim", "clip_id": id, "edge": "in", "to": {"secs": edge_at(middle.timeline_in)}, "ripple": true})),
        "trim out, picture alone" => {
            let target = RationalTime::from_secs_f64(edge_at(middle.timeline_out), RATE);
            klipp_document::ops::trim_at_linked(seq, pool, middle.id, klipp_document::ops::Edge::Out, target, false, false).ok()
        }
        "slip" => planned(seq, pool, json!({"op": "slip", "clip_id": id, "secs": secs(swing)})),
        "slide" => planned(seq, pool, json!({"op": "slide", "clip_id": id, "secs": secs(swing / 3)})),
        "retime" => planned(seq, pool, json!({"op": "retime", "clip_id": id, "speed": 1.0 + swing as f64 / 40.0, "ripple": true})),
        _ => unreachable!(),
    }
}

const KINDS: &[&str] = &["trim out", "trim out, ripple", "trim in, ripple", "trim out, picture alone", "slip", "slide", "retime"];

#[test]
fn shown_frames_cancel_to_the_start_and_leave_the_last_made_once() {
    for kind in KINDS {
        let (mut seqs, mut pool) = fixture();
        let start_seq = seqs[0].clone();
        let start_pool = pool.clone();
        let start = state(&seqs, &pool);
        let mut fps = seqs[0].fps;
        let mut live = LiveEdit::default();
        let mut last = None;
        let mut shown = 0;
        for k in 0..200 {
            let Some(edit) = frame(kind, &start_seq, &start_pool, k) else { continue };
            let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
            live.show(&mut view, edit.clone());
            last = Some(edit);
            shown += 1;
        }
        // An edge run into its neighbour is refused, as it is while dragging.
        assert!(shown > 60, "{kind}: the ops planned many frames ({shown} of 200)");
        // What is shown is the last frame's edit made once on the start.
        let (mut once, mut once_pool) = (vec![start_seq.clone()], start_pool.clone());
        let mut once_fps = once[0].fps;
        DocumentView { sequences: &mut once, active: 0, media_pool: &mut once_pool, fps: &mut once_fps }.apply_forward(last.as_ref().unwrap());
        assert!(state(&seqs, &pool) == state(&once, &once_pool), "{kind}: the last frame, as one edit made on the start");
        assert_ne!(state(&seqs, &pool), start, "{kind}: the frames changed the sequence");

        let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        live.cancel(&mut view);
        assert!(!live.is_live(), "{kind}: cancelled");
        assert!(state(&seqs, &pool) == start, "{kind}: cancelled, the document as it began");
    }
}

/// Ended, the edit stays applied and comes back to be recorded, once.
#[test]
fn an_ended_live_edit_stays_and_is_handed_over_once() {
    let (mut seqs, mut pool) = fixture();
    let start_seq = seqs[0].clone();
    let mut fps = seqs[0].fps;
    let mut live = LiveEdit::default();
    let edit = frame("trim out, ripple", &start_seq, &pool, 3).unwrap();
    live.show(&mut DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }, edit.clone());
    assert_eq!(live.sequence(), Some(start_seq.id));
    let shown = state(&seqs, &pool);
    let (id, ended) = live.end().expect("the edit shown");
    assert_eq!(id, start_seq.id);
    assert_eq!(format!("{ended:?}"), format!("{edit:?}"));
    assert!(live.end().is_none() && !live.is_live(), "handed over once");
    live.cancel(&mut DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps });
    assert!(state(&seqs, &pool) == shown, "a cancel after the end takes nothing back");
}

/// Shown on one sequence and then on another, the first is taken back
/// where it was shown.
#[test]
fn a_live_edit_moved_to_another_sequence_is_taken_back_on_its_own() {
    let (mut seqs, mut pool) = fixture();
    let mut second = seqs[0].clone();
    second.id = klipp_core::SequenceId::new();
    seqs.push(second);
    let start = state(&seqs, &pool);
    let mut fps = seqs[0].fps;
    let mut live = LiveEdit::default();
    let first = frame("slip", &seqs[0].clone(), &pool, 5).unwrap();
    live.show(&mut DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }, first);
    let other = frame("slip", &seqs[1].clone(), &pool, 9).unwrap();
    live.show(&mut DocumentView { sequences: &mut seqs, active: 1, media_pool: &mut pool, fps: &mut fps }, other);
    assert_eq!(serde_json::to_value(&seqs[0]).unwrap(), start["sequences"][0], "the first sequence back as it was");
    assert_eq!(live.sequence(), Some(seqs[1].id));
    live.cancel(&mut DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps });
    assert!(state(&seqs, &pool) == start, "cancelled on its own sequence, whichever is active");
}
