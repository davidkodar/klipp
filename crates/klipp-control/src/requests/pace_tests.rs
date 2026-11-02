//! `pace` over the socket (#3313): the Pace Strip's hints, read without
//! changing anything.

use super::*;
use std::sync::{Arc, RwLock};

#[test]
fn pace_lists_its_hints_and_the_destination_and_changes_nothing() {
    let mut sequence = klipp_timeline::Sequence::new_default();
    sequence.brief = Some(klipp_timeline::Brief { platform: "TikTok".into(), ..Default::default() });
    let time = |secs| klipp_core::Timecode::from_secs_f64(secs, 25);
    let words = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Read all of these words before they go".into(), ..Default::default() });
    let mut text = klipp_timeline::Track::new("Text 1", klipp_timeline::TrackKind::Text);
    text.clips.push(klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(words), ..klipp_timeline::Clip::gap(time(2.0), time(3.0)) });
    sequence.tracks.insert(0, text);
    let (commands, receiver) = crossbeam_channel::unbounded::<klipp_document::control::ControlEnvelope>();
    let ctx = ControlContext {
        commands,
        shared: Arc::new(klipp_core::ControlShared::default()),
        wake_ui: Arc::new(|| {}),
        sequence: Arc::new(klipp_timeline::SharedSequence::new(sequence)),
        media_pool: Arc::new(RwLock::new(klipp_media::MediaPool::new())),
        frame_buffer: Arc::new(std::sync::Mutex::new(klipp_engine::FrameBuffer::new(8, 8))),
        perf_ring: klipp_engine::perf::shared(),
        log_path: std::env::temp_dir().join("klipp-pace-unused.log"),
        others: Default::default(),
    };
    let reply = respond(&ctx, ControlCommand::Pace, &mut None).unwrap();
    assert_eq!(reply["destination"]["id"], "vertical", "{reply}");
    let marks = reply["marks"].as_array().unwrap();
    assert!(marks.iter().any(|m| m["kind"] == "brief_text" && m["from_secs"].as_f64() == Some(2.0)), "{reply}");
    assert_eq!(reply["count"].as_u64(), Some(marks.len() as u64));
    assert!(receiver.try_recv().is_err(), "nothing sent to the app");
}
