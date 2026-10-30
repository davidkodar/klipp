//! A decoder dropped while its GPU open runs leaves no thread behind that
//! the wait before exit does not cover. The open runs on a thread of its
//! own (#2394), and that thread was not counted: once its decoder stopped
//! waiting for it, it went on creating a CUDA context while `exit()` tore
//! the process down, and the process aborted after every test had passed.
//!
//! Its own binary: the stall it injects, and the shunning of the GPU that
//! follows a stalled open, are the process's.

mod common;

use std::time::{Duration, Instant};

use klipp_engine::video_decoder::{self, VideoDecoder};

/// Threads of this process still opening a GPU decoder.
fn opening_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .map(|tasks| {
            tasks
                .flatten()
                .filter(|t| std::fs::read_to_string(t.path().join("comm")).is_ok_and(|c| c.trim() == "klipp-hw-open"))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn a_decoder_dropped_while_opening_is_waited_for() {
    let Some(src) = common::hevc_source("dropped-opening", 640, 360, common::SDR_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };

    // Dropped as soon as it is made, again and again: the wait returns
    // only once each GPU open is over.
    for round in 0..10 {
        drop(VideoDecoder::open(&src, 0).expect("open"));
        assert!(video_decoder::wait_for_decode_threads(Duration::from_secs(10)), "round {round}: decode threads still running");
        assert_eq!(opening_threads(), 0, "round {round}: a GPU open outlived the wait");
    }

    // An open that stalls past the limit is left behind by its decoder,
    // which goes on in software. Dropping that decoder ends its own
    // thread at once, but the wait holds on until the open does.
    video_decoder::stall_next_hw_open(Duration::from_millis(4500));
    let dec = VideoDecoder::open(&src, 0).expect("open");
    dec.resume();
    let start = Instant::now();
    while video_decoder::hw_opens_timed_out() == 0 {
        assert!(start.elapsed() < Duration::from_secs(10), "the stalled open never ran out of time");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(dec);
    assert_eq!(opening_threads(), 1, "the stalled open is still running");
    assert!(video_decoder::wait_for_decode_threads(Duration::from_secs(15)), "decode threads still running");
    assert_eq!(opening_threads(), 0, "the wait returned while the stalled open still ran");
}
