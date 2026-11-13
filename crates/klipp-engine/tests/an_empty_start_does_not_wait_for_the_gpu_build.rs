//! The first frame of a start, no project open yet, is shown at once while
//! the GPU's effect pipelines are still compiling, and the engine goes on
//! taking commands. It waited for the whole compile, 2 s with the driver's
//! shader cache cold, on every start: the engine thread was parked behind
//! the build and the watchdog said it had stalled. Once built, the empty
//! frame is drawn the usual way, and looks the same.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::MediaPool;
use klipp_timeline::{Sequence, SharedSequence};

const W: u32 = 320;
const H: u32 = 180;
/// What the viewer's trail says of an empty frame shown during the build.
const SHOWN_WHILE_BUILDING: &str = "the GPU pipelines still building";

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 30)
}

fn shown_while_building(fb: &Mutex<FrameBuffer>) -> usize {
    fb.lock().unwrap().trail().iter().filter(|l| l.contains(SHOWN_WHILE_BUILDING)).count()
}

/// Moves the playhead to `secs` and waits up to `limit` for the frame.
fn frame_at(cmd: &crossbeam_channel::Sender<AppCommand>, fb: &Mutex<FrameBuffer>, secs: f64, limit: Duration) -> Option<Vec<u8>> {
    let since = common::generation(fb);
    cmd.send(AppCommand::SetPlayhead(tc(secs))).unwrap();
    common::wait_new(fb, since, limit)?;
    Some(fb.lock().unwrap().latest_frame().0.to_vec())
}

#[test]
fn an_empty_start_does_not_wait_for_the_gpu_build() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        let why = "no GPU: nothing is built";
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required: {why}");
        return eprintln!("skipping: {why}");
    }
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: W, height: H };
    let shared = Arc::new(SharedSequence::new(seq));
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    // A compile that lasts until the test lets it end.
    klipp_effects::gpu::hold_pipeline_build(true);
    let mut engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        shared, Arc::new(RwLock::new(MediaPool::default())), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    // As the app starts it.
    engine.prewarm_gpu();
    let thread = engine.spawn();

    let t = Instant::now();
    let first = frame_at(&cmd, &fb, 0.0, Duration::from_secs(2));
    let first_ms = t.elapsed().as_millis();
    let next = first.as_ref().and_then(|_| frame_at(&cmd, &fb, 1.0, Duration::from_secs(2)));
    let during = shown_while_building(&fb);
    klipp_effects::gpu::hold_pipeline_build(false);

    // Built, the empty frame is drawn the usual way again.
    let until = Instant::now() + Duration::from_secs(30);
    let mut usual = None;
    let mut at = 0.0;
    while usual.is_none() && Instant::now() < until {
        at = if at == 0.5 { 1.5 } else { 0.5 };
        let before = shown_while_building(&fb);
        let frame = frame_at(&cmd, &fb, at, Duration::from_secs(5));
        if shown_while_building(&fb) == before {
            usual = frame;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();

    eprintln!("first frame in {first_ms} ms, {during} shown while building");
    let first = first.expect("the first frame waited for the GPU pipelines' compile");
    let next = next.expect("the engine took no command while the GPU pipelines compiled");
    assert!(during >= 2, "shown while building: {during}");
    let usual = usual.expect("the empty frame was never drawn the usual way after the build");
    assert_eq!(first.len(), (W * H * 4) as usize);
    assert!(first == usual && next == usual, "the empty frame shown while building is not the one drawn after it");
}
