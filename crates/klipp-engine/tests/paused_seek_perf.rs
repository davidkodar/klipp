//! How long moving the playhead on a paused 4K HDR clip takes to show the
//! frame there — a click on the ruler near and far, and stepping a frame
//! at a time. A probe, not a pass/fail test: run with
//! `cargo test -p klipp-engine --test paused_seek_perf -- --ignored --nocapture`.
//!
//! The clip is 4K 10-bit PQ HEVC with a keyframe every second, graded with
//! an Exposure node; the viewer at full 4K. Each move is timed to the
//! first new frame and to the frame the viewer settles on.

mod common;

use std::time::{Duration, Instant};

use klipp_core::{AppCommand, RationalTime};

#[test]
#[ignore = "perf probe: run explicitly"]
fn paused_4k_pq_seek() {
    klipp_engine::video_decoder::set_max_playback_height(2160);
    let Some(src) = common::hevc_source("pq-hevc", 3840, 2160, common::PQ_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start(src, 3840, 2160);
    let tc = |f: i64| RationalTime::from_frames(f, common::FPS);

    // Time to the first new frame, and to the last one before the viewer
    // has been still for 400 ms.
    let timed = |label: &str, cmd: AppCommand| {
        let g = parked.generation();
        let t0 = Instant::now();
        parked.send(cmd);
        let Some(first) = parked.wait_new(g, Duration::from_secs(5)) else {
            eprintln!("{label:24} no frame");
            return;
        };
        let mut last = first;
        let mut gen = parked.generation();
        loop {
            match parked.wait_new(gen, Duration::from_millis(400)) {
                Some(_) => {
                    last = t0.elapsed();
                    gen = parked.generation();
                }
                None => break,
            }
        }
        eprintln!(
            "{label:24} first frame {:6.1} ms, settled {:6.1} ms",
            first.as_secs_f64() * 1000.0,
            last.as_secs_f64() * 1000.0,
        );
    };

    timed("click far (10 -> 40)", AppCommand::SetPlayhead(tc(40)));
    timed("click near (40 -> 43)", AppCommand::SetPlayhead(tc(43)));
    timed("click back (43 -> 12)", AppCommand::SetPlayhead(tc(12)));
    for i in 0..4 {
        timed(&format!("step forward #{i}"), AppCommand::SeekRelative(1));
    }
    for i in 0..4 {
        timed(&format!("step back #{i}"), AppCommand::SeekRelative(-1));
    }
    for s in parked.perf().iter().rev().take(4).rev() {
        eprintln!(
            "  engine: {}x{} total {:6.1} ms | decode {:5.1} grade {:5.1} composite {:5.1} encode {:5.1}",
            s.width, s.height, s.total_ms, s.decode_ms, s.grade_ms, s.composite_ms, s.reencode_ms,
        );
    }
}
