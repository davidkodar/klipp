//! `covers` over the socket (#3311): the covers and sizes listed, and an
//! export refused before anything is drawn when it can't be written.

use super::*;
use std::sync::{Arc, Mutex, RwLock};

fn context(covers: Vec<klipp_timeline::cover::Cover>) -> ControlContext {
    let mut sequence = klipp_timeline::Sequence::new_default();
    sequence.covers = covers;
    let (commands, _receiver) = crossbeam_channel::unbounded::<klipp_document::control::ControlEnvelope>();
    ControlContext {
        commands,
        shared: Arc::new(klipp_core::ControlShared::default()),
        wake_ui: Arc::new(|| {}),
        sequence: Arc::new(klipp_timeline::SharedSequence::new(sequence)),
        media_pool: Arc::new(RwLock::new(klipp_media::MediaPool::new())),
        frame_buffer: Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(8, 8))),
        perf_ring: klipp_engine::perf::shared(),
        log_path: std::env::temp_dir().join("klipp-covers-unused.log"),
        others: Default::default(),
    }
}

fn cover(name: &str) -> klipp_timeline::cover::Cover {
    klipp_timeline::cover::Cover { id: klipp_core::CoverId::new(), name: name.into(), at_secs: 1.0, size: "youtube".into(), crop: Default::default(), text: None, with_titles: false }
}

#[test]
fn covers_are_listed_and_an_export_that_cant_be_written_is_refused() {
    let ask = |ctx: &ControlContext, cover: Option<&str>, path: Option<&str>| respond(ctx, ControlCommand::Covers { cover: cover.map(str::to_string), path: path.map(str::to_string), propose: false, overwrite: false }, &mut None);
    let two = context(vec![cover("Hook"), cover("Calm")]);
    let listed = ask(&two, None, None).unwrap();
    assert_eq!(listed["covers"].as_array().map(Vec::len), Some(2), "{listed}");
    assert_eq!((listed["covers"][0]["name"].as_str(), listed["covers"][0]["size_name"].as_str()), (Some("Hook"), Some("YouTube thumbnail")));
    assert!(listed["sizes"].as_array().unwrap().iter().any(|s| s["id"] == "youtube" && s["max_bytes"] == 2_000_000), "{listed}");

    let why = |ctx: &ControlContext, cover: Option<&str>, path: &str| ask(ctx, cover, Some(path)).unwrap_err();
    assert!(why(&two, Some("Hook"), "cover.jpg").contains("absolute"));
    assert!(why(&two, Some("Hook"), "/tmp/cover.webp").contains(".png, .jpg or .jpeg"));
    assert!(why(&two, None, "/tmp/cover.jpg").contains("2 covers; give cover"));
    assert!(why(&two, Some("Nope"), "/tmp/cover.jpg").contains("no cover"));
    assert!(why(&context(Vec::new()), None, "/tmp/cover.png").contains("no covers"));
    // A file there already is kept unless overwrite is asked.
    let dir = std::env::temp_dir().join(format!("klipp-covers-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let there = dir.join("cover.jpg");
    std::fs::write(&there, b"mine").unwrap();
    assert!(why(&two, Some("Hook"), there.to_str().unwrap()).contains("send overwrite true"));
    assert_eq!(std::fs::read(&there).unwrap(), b"mine", "left as it was");
    assert!(why(&two, Some("Hook"), dir.to_str().unwrap()).contains("names no picture format"));
    let _ = std::fs::remove_dir_all(&dir);
}
