//! Stepping and jumping by frames moves whole frames at 29.97 fps.
//!
//! A timeline position counts ticks of `fps.num` a second, so at 29.97 fps
//! (30000/1001) a frame is 1001 ticks. The engine added the UI's frame
//! counts to ticks directly: a step back moved 1/1001 of a frame and
//! showed the frame it was already on, and the ten-second jump buttons
//! moved less than a frame. Integer rates hid it — there a tick is a frame.

mod common;

use std::time::{Duration, Instant};

use klipp_core::{AppCommand, Fps};

/// Send `cmd`, wait for the playhead to report `want` (thirty seconds at the
/// most), then for the picture to settle. Waiting on quiet alone raced on
/// a loaded machine: a re-render of the old frame counted as the seek's
/// frame, the buffer went quiet before the engine took the seek, and the
/// test read the old playhead. A wrong frame still fails, after the wait.
fn seek(parked: &common::Parked, cmd: AppCommand, want: f64) {
    let g = parked.generation();
    parked.send(cmd);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(30) && !parked.frame_number().is_some_and(|f| (f - want).abs() < 0.01) {
        std::thread::sleep(Duration::from_millis(1));
    }
    parked.settle_after(g, Duration::from_secs(30));
}

#[test]
fn steps_and_jumps_move_whole_frames_at_29_97() {
    let fps = Fps::new(30000, 1001);
    let Some(src) = common::hevc_source_at("sdr-2997", 640, 360, common::SDR_TAGS, fps) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start_at(src, 640, 360, fps, true);
    let at = |parked: &common::Parked| parked.frame_number().expect("no playhead reported");

    eprintln!("parked at frame {:?}", parked.frame_number());
    // What the viewer shows: its bytes, or, for a frame presented on the
    // GPU without them (zero-copy, which a paused step may take or not
    // as the machine's load goes), how many such frames there have been.
    // Two GPU frames both read as no bytes, and compared the same.
    let picture = |parked: &common::Parked| (parked.frame(), parked.gpu_frames_and_generation().0);
    let mut shown = picture(&parked);
    for (cmd, want) in [
        (AppCommand::SeekRelative(-1), 9.0),
        (AppCommand::SeekRelative(-1), 8.0),
        (AppCommand::SeekRelative(1), 9.0),
        (AppCommand::SeekRelative(1), 10.0),
        (AppCommand::SeekRelative(-5), 5.0),
        (AppCommand::SeekRelative(12), 17.0),
    ] {
        let label = format!("{cmd:?}");
        seek(&parked, cmd, want);
        let got = at(&parked);
        eprintln!("{label}: at frame {got:.3}, want {want}");
        assert!((got - want).abs() < 0.01, "{label}: the playhead is at frame {got:.3}, not {want}");
        let now = picture(&parked);
        assert_ne!(now, shown, "{label}: the picture did not change");
        shown = now;
    }
}
