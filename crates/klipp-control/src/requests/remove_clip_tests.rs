//! `remove_clip` over the socket: a lift by default, as the `remove` op
//! is, and a ripple only when asked. It closed the gap on every track,
//! moving later cards and backdrops on upper tracks.

use super::*;
use std::sync::{Arc, Mutex, RwLock};

/// Two clips on a video track, and what the app was sent.
fn context() -> (ControlContext, Vec<klipp_core::ClipId>, Arc<Mutex<Vec<AppCommand>>>) {
    let mut sequence = klipp_timeline::Sequence::new_default();
    sequence.tracks.clear();
    let time = |frames| klipp_core::Timecode::from_frames(frames, 24);
    let mut track = klipp_timeline::Track::new("Video 2", klipp_timeline::TrackKind::Video);
    let mut pool = klipp_media::MediaPool::new();
    let media = pool.add(klipp_media::MediaItem::synthetic_for_test("/nonexistent/card.png".into(), None));
    for start in [0, 96] {
        track.clips.push(klipp_timeline::Clip::media(media, time(start), time(start + 48), time(0), time(48)));
    }
    let ids = track.clips.iter().map(|c| c.id).collect();
    sequence.tracks.push(track);
    let sent = Arc::new(Mutex::new(Vec::new()));
    let (commands, receiver) = crossbeam_channel::unbounded::<klipp_document::control::ControlEnvelope>();
    let got = sent.clone();
    let wake_ui = Arc::new(move || {
        while let Ok(envelope) = receiver.try_recv() {
            got.lock().unwrap().extend(envelope.commands);
            if let Some(ack) = envelope.ack {
                ack.send(klipp_document::control::ControlAck { revision: 1, refused: None, refused_op: None, renamed: Vec::new(), ungated: Vec::new(), picture_refused: None }).unwrap();
            }
        }
    });
    let ctx = ControlContext {
        commands, shared: Arc::new(klipp_core::ControlShared::default()), wake_ui,
        sequence: Arc::new(klipp_timeline::SharedSequence::new(sequence)),
        media_pool: Arc::new(RwLock::new(pool)), frame_buffer: Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(8, 8))),
        perf_ring: klipp_engine::perf::shared(), log_path: std::env::temp_dir().join("unused.log"), others: Default::default(),
    };
    (ctx, ids, sent)
}

fn removed(line: &str) -> Vec<(Vec<klipp_core::ClipId>, bool)> {
    let (ctx, ids, sent) = context();
    let command: ControlCommand = serde_json::from_str(&line.replace("ID", &ids[0].to_string())).unwrap();
    respond(&ctx, command, &mut None).unwrap();
    (ctx.wake_ui)();
    let out: Vec<(Vec<klipp_core::ClipId>, bool)> = sent.lock().unwrap().iter().filter_map(|c| match c {
        AppCommand::RemoveClips { clip_ids, lift } => Some((clip_ids.clone(), *lift)),
        AppCommand::RemoveClip { clip_id } => Some((vec![*clip_id], false)),
        _ => None,
    }).collect();
    assert!(out.iter().all(|(c, _)| c == &vec![ids[0]]), "{out:?}");
    out
}

#[test]
fn remove_clip_lifts_unless_asked_to_ripple() {
    assert_eq!(removed(r#"{"method":"remove_clip","clip_id":"ID"}"#).iter().map(|r| r.1).collect::<Vec<_>>(), vec![true], "a plain remove leaves later clips in place");
    assert_eq!(removed(r#"{"method":"remove_clip","clip_id":"ID","ripple":false}"#).iter().map(|r| r.1).collect::<Vec<_>>(), vec![true]);
    assert_eq!(removed(r#"{"method":"remove_clip","clip_id":"ID","ripple":true}"#).iter().map(|r| r.1).collect::<Vec<_>>(), vec![false], "ripple closes the gap");
}
