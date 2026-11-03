//! A project closing clears the Source viewer's frame as it does the
//! Program viewer's (#3457): the frame belongs to the old project's media.

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::AppCommand;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::MediaPool;
use klipp_timeline::Sequence;

#[test]
fn a_reset_leaves_the_source_viewer_empty() {
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let source = Arc::new(Mutex::new(FrameBuffer::new(2, 2)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::new(Mutex::new(FrameBuffer::new(2, 2))), Arc::clone(&source),
        Arc::new(klipp_timeline::SharedSequence::new(Sequence::new_default())), Arc::new(RwLock::new(MediaPool::default())), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();
    source.lock().unwrap().write_frame(vec![200; 16], 2, 2);
    assert_eq!(source.lock().unwrap().latest_frame().1, 2);

    cmd.send(AppCommand::ResetEngine).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while source.lock().unwrap().latest_frame().1 != 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    let shown = source.lock().unwrap().latest_frame().1;
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    assert_eq!(shown, 0, "the old project's frame is still in the Source viewer");
}
