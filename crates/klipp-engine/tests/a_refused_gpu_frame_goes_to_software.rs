//! A stream whose hardware frames the GPU refuses goes on in software
//! (#2394). A driver out of channels or memory refused the transfer of
//! every frame a hardware decoder made; each was dropped, and the stream
//! went on decoding into nothing, the picture empty for good. Now, after
//! a few refused in a row, the stream is decoded in software from the
//! first frame refused, and frames keep coming.
//!
//! Its own binary: the refusal it injects and the count it reads are the
//! process's.

mod common;

use std::time::{Duration, Instant};

use klipp_engine::video_decoder::{self, VideoDecoder};

/// The frames the decoder hands over within `wait` after the first.
fn drain(dec: &VideoDecoder, wait: Duration) -> Vec<f64> {
    // Up to 20 s for the first frame, then `wait` for the rest.
    let first_by = Instant::now() + Duration::from_secs(20);
    let mut deadline = None;
    let mut out = Vec::new();
    while deadline.map_or(Instant::now() < first_by, |d| Instant::now() < d) {
        if deadline.is_none() && !out.is_empty() {
            deadline = Some(Instant::now() + wait);
        }
        {
            let mut q = dec.frame_queue().lock().unwrap();
            while let Some(f) = q.pop() {
                if !f.is_eof {
                    out.push(f.pts_secs());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    out
}

#[test]
fn refused_hardware_frames_go_on_in_software() {
    let Some(src) = common::hevc_source("refused", 640, 360, common::SDR_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_writer(std::io::stderr).try_init();
    // The GPU refuses every hardware frame from the first.
    video_decoder::refuse_next_hw_frames(u32::MAX);
    let dec = VideoDecoder::open(&src, 0).expect("open");
    dec.resume();
    let frames = drain(&dec, Duration::from_millis(600));
    video_decoder::refuse_next_hw_frames(0);
    if video_decoder::streams_moved_to_software() == 0 && !dec.hw_active.load(std::sync::atomic::Ordering::Relaxed) {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required: the stream did not open in hardware");
        eprintln!("skipping: no hardware decode here");
        return;
    }
    assert!(!frames.is_empty(), "no frame came after the GPU refused them");
    assert!(frames.windows(2).all(|w| w[1] > w[0]), "the frames go on forward: {frames:?}");
    assert!(frames[0] < 0.05, "it went on from the first frame it refused, none lost: {frames:?}");
    assert_eq!(video_decoder::streams_moved_to_software(), 1, "the stream went on in software once");
    assert!(!dec.hw_active.load(std::sync::atomic::Ordering::Relaxed), "and says it is no longer on the GPU");
}
