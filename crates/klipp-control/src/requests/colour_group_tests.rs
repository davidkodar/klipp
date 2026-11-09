use super::*;
use std::sync::{Arc, Mutex, RwLock};

fn context() -> ControlContext {
    let root = std::env::temp_dir().join(format!("klipp-group-advice-{}", uuid::Uuid::new_v4()));
    let mut pool = klipp_media::MediaPool::new();
    let media = pool.add(klipp_media::MediaItem::synthetic_for_test(root.join("scene.mp4"), None));
    let mut sequence = klipp_timeline::Sequence::new_default();
    sequence.tracks.clear();
    let time = |frames| klipp_core::Timecode::from_frames(frames, 24);
    let mut track = klipp_timeline::Track::new("Footage", klipp_timeline::TrackKind::Video);
    for start in [0, 48] {
        track.clips.push(klipp_timeline::Clip::media(media, time(start), time(start + 48), time(0), time(48)));
    }
    sequence.tracks.push(track);
    let fb = Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(8, 8)));
    let (commands, receiver) = crossbeam_channel::unbounded::<klipp_document::control::ControlEnvelope>();
    let frame_buffer = fb.clone();
    // Exercise the real request and frame-read path with a synchronous host.
    // The unique, nonexistent media path cannot load a user's analysis cache.
    let wake_ui = Arc::new(move || {
        while let Ok(envelope) = receiver.try_recv() {
            for command in envelope.commands {
                if let AppCommand::SeekTo(at) = command {
                    let mut frame = frame_buffer.lock().unwrap();
                    frame.write_frame([90, 110, 130, 255].repeat(64), 8, 8);
                    frame.set_presented(at, true);
                }
            }
            if let Some(ack) = envelope.ack {
                ack.send(klipp_document::control::ControlAck {
                    revision: 0, refused: None, refused_op: None,
                    renamed: Vec::new(), ungated: Vec::new(), picture_refused: None,
                }).unwrap();
            }
        }
    });
    ControlContext {
        commands, shared: Arc::new(klipp_core::ControlShared::default()), wake_ui,
        sequence: Arc::new(klipp_timeline::SharedSequence::new(sequence)),
        media_pool: Arc::new(RwLock::new(pool)), frame_buffer: fb,
        perf_ring: klipp_engine::perf::shared(), log_path: root.join("unused.log"), others: Default::default(),
    }
}

fn preview(ctx: &ControlContext) -> serde_json::Value {
    respond(ctx, ControlCommand::GradeToTarget {
        clip_id: None, clips: Vec::new(), look_if_unbriefed: None, apply: false, rounds: None, force: true, strength: None,
    }, &mut None).unwrap()
}

#[test]
fn grouped_scene_does_not_repeat_grouping_advice() {
    let ctx = context();
    {
        let mut seq = ctx.sequence.write().unwrap();
        // Membership already exists even before a grade is added to the group.
        let group = klipp_timeline::GradeGroup::new("One scene");
        for clip in &mut seq.tracks[0].clips { clip.grade_group = Some(group.id); }
        seq.grade_groups.push(group);
    }
    let reply = preview(&ctx);
    assert_eq!(reply["shots"].as_array().unwrap().len(), 2);
    assert!(reply["scene_groups"].as_array().unwrap().is_empty(), "already grouped: {reply}");
    assert!(!reply["text"].as_str().unwrap().contains("Grade them together"));
}

#[test]
fn incomplete_or_missing_scene_groups_keep_the_advice() {
    let ctx = context();
    let (a, b) = (klipp_timeline::GradeGroup::new("A"), klipp_timeline::GradeGroup::new("B"));
    let (a_id, b_id) = (a.id, b.id);
    ctx.sequence.write().unwrap().grade_groups = vec![a, b];
    let dangling = klipp_core::GradeGroupId::new();
    for memberships in [[None, None], [Some(a_id), None], [Some(a_id), Some(b_id)], [Some(dangling), Some(dangling)]] {
        {
            let mut seq = ctx.sequence.write().unwrap();
            for (clip, group) in seq.tracks[0].clips.iter_mut().zip(memberships) { clip.grade_group = group; }
        }
        let reply = preview(&ctx);
        assert_eq!(reply["scene_groups"].as_array().unwrap().len(), 1, "{memberships:?}: {reply}");
        assert!(reply["text"].as_str().unwrap().contains("Grade them together"));
    }
}

/// Grade to Look reads frames across the shot and grades from the middle
/// one in lightness, seldom the playhead's: its reply says so, so a time
/// that is not the playhead's does not read as a mistake.
#[test]
fn grade_to_look_says_which_frame_it_read_and_why() {
    let ctx = context();
    let reply = preview(&ctx);
    let text = reply["text"].as_str().unwrap();
    assert!(text.contains("the middle in lightness of 3 frames across the shot"), "{text}");
}

/// With no face sizes in the clip's analysis Balance Skin reads the
/// clip's middle, and says that, not "where the largest face is".
#[test]
fn balance_skin_says_which_frame_it_read_and_why() {
    let ctx = context();
    let clip = ctx.sequence.read().unwrap().tracks[0].clips[1].id.0.to_string();
    let err = respond(&ctx, ControlCommand::SkinBalance { clip_id: clip, apply: false }, &mut None).unwrap_err();
    assert!(err.contains("read at 3.00 s, the clip's middle") && !err.contains("largest"), "{err}");
}
