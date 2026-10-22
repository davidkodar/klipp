//! A hardware decoder that will not open goes on in software (#2394). A
//! driver refusing new work blocked a decoder's open, or its first
//! frame, for good: a test waited seven minutes for one. Now the open
//! and first frame have a time limit, after which the stream decodes in
//! software; for a while after, new streams skip the GPU at once.
//!
//! Its own binary: the stall it injects and the count it reads are the
//! process's.

mod common;

use std::time::{Duration, Instant};

use klipp_engine::video_decoder::{self, VideoDecoder};

/// How long `dec` took to hand over its first frame, if it did within 20 s.
fn first_frame(dec: &VideoDecoder) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        if dec.frame_queue().lock().unwrap().pop().is_some_and(|f| !f.is_eof) {
            return Some(start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

#[test]
fn a_stalled_gpu_open_goes_on_in_software() {
    let Some(src) = common::hevc_source("stalled", 640, 360, common::SDR_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_writer(std::io::stderr).try_init();
    let gpu_required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some();

    // An open in time hands over a working decoder, on the GPU where
    // there is one.
    video_decoder::stall_next_hw_open(Duration::ZERO);
    let dec = VideoDecoder::open(&src, 0).expect("open");
    dec.resume();
    assert!(first_frame(&dec).is_some(), "no frame from a decoder opened in time");
    assert!(!gpu_required || dec.hw_active.load(std::sync::atomic::Ordering::Relaxed), "GPU required: the stream did not open in hardware");
    drop(dec);
    assert_eq!(video_decoder::hw_opens_timed_out(), 0, "an open in time is not counted");

    // An open that stalls past the limit: frames come in software, soon
    // after the limit and long before the stall ends.
    video_decoder::stall_next_hw_open(Duration::from_secs(10));
    let dec = VideoDecoder::open(&src, 0).expect("open");
    dec.resume();
    let took = first_frame(&dec).expect("no frame after the GPU open stalled");
    assert!(took < Duration::from_secs(6), "the first frame came {took:?} after the open, past the limit and its margin");
    assert_eq!(video_decoder::hw_opens_timed_out(), 1, "the stalled open is counted");
    assert!(!dec.hw_active.load(std::sync::atomic::Ordering::Relaxed), "and the stream says it is not on the GPU");
    drop(dec);

    // The next stream skips the GPU without waiting.
    let dec = VideoDecoder::open(&src, 0).expect("open");
    dec.resume();
    let took = first_frame(&dec).expect("no frame from the stream after");
    assert!(took < Duration::from_secs(2), "the stream after waited {took:?}");
    assert!(!dec.hw_active.load(std::sync::atomic::Ordering::Relaxed), "the stream after stays off the GPU");
    assert_eq!(video_decoder::hw_opens_timed_out(), 1, "and does not try it again");
}
