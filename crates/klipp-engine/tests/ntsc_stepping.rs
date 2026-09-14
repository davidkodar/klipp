//! Stepping and jumping by frames moves whole frames at 29.97 fps.
//!
//! A timeline position counts ticks of `fps.num` a second, so at 29.97 fps
//! (30000/1001) a frame is 1001 ticks. The engine added the UI's frame
//! counts to ticks directly: a step back moved 1/1001 of a frame and
//! showed the frame it was already on, and the ten-second jump buttons
//! moved less than a frame. Integer rates hid it — there a tick is a frame.

mod common;

use std::time::Duration;

use klipp_core::{AppCommand, Fps};

fn settle(parked: &common::Parked) {
    let g = parked.generation();
    parked.settle_after(g, Duration::from_secs(5));
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
    let mut shown = parked.frame();
    for (cmd, want) in [
        (AppCommand::SeekRelative(-1), 9.0),
        (AppCommand::SeekRelative(-1), 8.0),
        (AppCommand::SeekRelative(1), 9.0),
        (AppCommand::SeekRelative(1), 10.0),
        (AppCommand::SeekRelative(-5), 5.0),
        (AppCommand::SeekRelative(12), 17.0),
    ] {
        let label = format!("{cmd:?}");
        parked.send(cmd);
        settle(&parked);
        let got = at(&parked);
        eprintln!("{label}: at frame {got:.3}, want {want}");
        assert!((got - want).abs() < 0.01, "{label}: the playhead is at frame {got:.3}, not {want}");
        let now = parked.frame();
        assert_ne!(now, shown, "{label}: the picture did not change");
        shown = now;
    }
}
