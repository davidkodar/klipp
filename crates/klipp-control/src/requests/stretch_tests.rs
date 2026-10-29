//! #3055: `stretch` sent as the assistant sends it, an apply op: a 4 s
//! clip stretched to 8 s plays at half speed, its linked sound with it;
//! into the clip after it, refused without ripple and moved with it; past
//! the speed limits and on a locked track, refused, saying why. The host
//! applies each edit as the app does.

use super::*;
use std::sync::{Arc, Mutex, RwLock};

fn tc(s: f64) -> klipp_core::Timecode {
    klipp_core::Timecode::from_secs_f64(s, 25)
}

/// A 4 s clip with its linked sound, and a clip from 6 s.
fn context() -> (ControlContext, klipp_core::ClipId, klipp_core::ClipId, klipp_core::ClipId) {
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test(std::env::temp_dir().join(format!("klipp-stretch-{}.mov", uuid::Uuid::new_v4())), None);
    item.duration = tc(10.0);
    item.has_audio = true;
    let media = pool.add(item);
    let mut seq = klipp_timeline::Sequence::new_default();
    seq.fps = klipp_core::Fps::new(25, 1);
    let mut v = klipp_timeline::Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let mut a = klipp_timeline::Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    v.linked_clip_id = Some(a.id);
    a.linked_clip_id = Some(v.id);
    let next = klipp_timeline::Clip::media(media, tc(6.0), tc(9.0), tc(5.0), tc(8.0));
    let ids = (v.id, a.id, next.id);
    let mut vt = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    vt.clips = vec![v, next];
    let mut at = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
    at.clips = vec![a];
    seq.tracks = vec![vt, at];
    let shared = Arc::new(klipp_core::ControlShared::default());
    let seq = Arc::new(klipp_timeline::SharedSequence::new(seq));
    let pool = Arc::new(RwLock::new(pool));
    let (commands, receiver) = crossbeam_channel::unbounded::<klipp_document::control::ControlEnvelope>();
    let (host_shared, host_seq, host_pool) = (shared.clone(), seq.clone(), pool.clone());
    let wake_ui = Arc::new(move || {
        while let Ok(envelope) = receiver.try_recv() {
            if !envelope.edits.is_empty() {
                let mut seqs = vec![(*host_seq.snapshot()).clone()];
                let mut p = host_pool.read().unwrap().clone();
                let mut fps = seqs[0].fps;
                let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
                for e in &envelope.edits {
                    view.apply_forward(e);
                }
                host_seq.replace(seqs.pop().unwrap());
                host_shared.bump_revision();
            }
            if let Some(ack) = envelope.ack {
                ack.send(klipp_document::control::ControlAck { revision: host_shared.revision(), refused: None, refused_op: None, renamed: Vec::new(), ungated: Vec::new(), picture_refused: None }).unwrap();
            }
        }
    });
    let ctx = ControlContext {
        commands,
        shared,
        wake_ui,
        sequence: seq,
        media_pool: pool,
        frame_buffer: Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(16, 16))),
        perf_ring: klipp_engine::perf::shared(),
        log_path: std::env::temp_dir().join("klipp-stretch-unused.log"),
        others: Default::default(),
    };
    (ctx, ids.0, ids.1, ids.2)
}

fn apply(ctx: &ControlContext, op: serde_json::Value) -> Result<serde_json::Value, String> {
    let op: klipp_document::ops::Op = serde_json::from_value(op).map_err(|e| e.to_string())?;
    crate::handle(ctx, ControlCommand::Apply { ops: vec![op], expected_revision: None, expected_project_identity: None, label: None, detail: false }, &mut None)
}

fn clip(ctx: &ControlContext, id: klipp_core::ClipId) -> klipp_timeline::Clip {
    ctx.sequence.snapshot().all_clips().find(|c| c.id == id).cloned().expect("the clip")
}

#[test]
fn stretch_over_the_socket_does_what_the_timeline_does_and_says_why_not() {
    let (ctx, v, a, next) = context();
    let id = v.0.to_string();
    // Into the clip after it: refused, nothing changed.
    let why = apply(&ctx, serde_json::json!({ "op": "stretch", "clip_id": id, "end_at_secs": 8.0 })).unwrap_err();
    assert!(why.contains("would run into"), "{why}");
    assert_eq!(clip(&ctx, v).timeline_out, tc(4.0));
    // With ripple: twice as long at half speed, its sound with it, the
    // next clip moved by the change.
    let reply = apply(&ctx, serde_json::json!({ "op": "stretch", "clip_id": id, "end_at_secs": 8.0, "ripple": true })).unwrap();
    let (cv, ca) = (clip(&ctx, v), clip(&ctx, a));
    assert_eq!((cv.timeline_out.to_secs_f64(), cv.source_out.to_secs_f64()), (8.0, 4.0), "{reply}");
    assert!((cv.speed - 0.5).abs() < 1e-12 && (ca.speed - 0.5).abs() < 1e-12 && ca.keep_pitch);
    assert_eq!(ca.timeline_out.to_secs_f64(), 8.0);
    assert_eq!(clip(&ctx, next).timeline_in.to_secs_f64(), 10.0);
    assert!(reply["text"].as_str().unwrap_or_default().contains("50.0%"), "the reply says the speed: {reply}");
    // Past the limits: refused with what it would need.
    let why = apply(&ctx, serde_json::json!({ "op": "stretch", "clip_id": id, "to_secs": 0.1, "ripple": true })).unwrap_err();
    assert!(why.contains("speed range") && why.contains('%'), "{why}");
    // Two targets: refused.
    let why = apply(&ctx, serde_json::json!({ "op": "stretch", "clip_id": id, "to_secs": 5.0, "speed": 2.0 })).unwrap_err();
    assert!(why.contains("one of"), "{why}");
    // On a locked track: refused.
    let mut locked = (*ctx.sequence.snapshot()).clone();
    locked.tracks[1].locked = true;
    ctx.sequence.replace(locked);
    let why = apply(&ctx, serde_json::json!({ "op": "stretch", "clip_id": id, "to_secs": 9.0, "ripple": true })).unwrap_err();
    assert!(why.contains("locked"), "{why}");
}
