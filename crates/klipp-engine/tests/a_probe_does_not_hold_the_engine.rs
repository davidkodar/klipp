//! Reading a file's streams when it is loaded is not the engine's to wait
//! for. Opening a project loads each of its files again, and the engine
//! thread read them one after another before anything else: a project of
//! 93 files held it 12–19 s, the viewer and the sound with it. A seek
//! asked for after the loads is answered first; the loads still come
//! back, each once, in the order they were asked for.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::MediaPool;

#[derive(Debug, Clone, PartialEq)]
enum Heard {
    Loaded(MediaId),
    Moved,
}

#[test]
fn a_seek_after_many_loads_is_answered_first() {
    let Some(src) = common::source("probe-order", 160, 90, common::SDR_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&heard);
    std::thread::spawn(move || {
        while let Ok(ev) = ev_rx.recv() {
            match ev {
                AppEvent::MediaLoaded(p) => h.lock().unwrap().push(Heard::Loaded(p.media_id)),
                AppEvent::PlayheadMoved(_) => h.lock().unwrap().push(Heard::Moved),
                _ => {}
            }
        }
    });
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::new(Mutex::new(FrameBuffer::new(160, 90))), Arc::new(Mutex::new(FrameBuffer::new(160, 90))),
        Arc::new(klipp_timeline::SharedSequence::new(klipp_timeline::Sequence::new_default())), Arc::new(RwLock::new(MediaPool::default())), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );

    // All in one batch, as an open sends them: the loads, then the seek.
    let ids: Vec<MediaId> = (0..30).map(|_| MediaId::new()).collect();
    for id in &ids {
        cmd.send(AppCommand::LoadMediaFile { path: src.clone(), media_id: *id }).unwrap();
    }
    cmd.send(AppCommand::SetPlayhead(RationalTime::from_frames(1, 25))).unwrap();
    let running = engine.spawn();

    let all_in = || heard.lock().unwrap().iter().filter(|h| matches!(h, Heard::Loaded(_))).count() == ids.len();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(60) && !all_in() {
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = cmd.send(AppCommand::Quit);
    let _ = running.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    let heard = heard.lock().unwrap().clone();
    let loaded: Vec<MediaId> = heard.iter().filter_map(|h| if let Heard::Loaded(id) = h { Some(*id) } else { None }).collect();
    assert_eq!(loaded, ids, "every file loaded once, in the order asked");
    let first_move = heard.iter().position(|h| *h == Heard::Moved).expect("the seek was answered");
    let first_load = heard.iter().position(|h| matches!(h, Heard::Loaded(_))).expect("a file was loaded");
    assert!(
        first_move < first_load,
        "the seek waited for {first_move} file load(s) to be read on the engine's thread: {heard:?}"
    );
}
