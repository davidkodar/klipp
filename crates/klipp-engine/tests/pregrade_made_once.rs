//! The ungraded picture of a paused frame is made once, not once per
//! viewing.
//!
//! Every paused frame keeps an ungraded picture — the source of the scope
//! preview while a slider is dragged. For an HDR frame, making it costs a
//! decode of its codes, a tone map and an sRGB encode: 20-30 ms at 4K. It
//! was remade every time a frame was shown again from either cache, so
//! stepping back and forth over the same frames paid it over and over.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use klipp_core::AppCommand;
use klipp_engine::PREGRADE_DISPLAYS_MADE;

#[test]
fn a_frame_shown_again_does_not_remake_its_ungraded_picture() {
    let Some(src) = common::source("pq", 640, 360, common::PQ_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start(src, 640, 360);
    let step = |n: i64| {
        let g = parked.generation();
        parked.send(AppCommand::SeekRelative(n));
        parked.wait_new(g, Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(250));
    };

    // Walk forward over three frames, then back over them, then forward
    // again: the frames are all in the caches by then.
    let made_new = |before: u64| PREGRADE_DISPLAYS_MADE.load(Ordering::Relaxed) - before;
    let start = PREGRADE_DISPLAYS_MADE.load(Ordering::Relaxed);
    for n in [1, 1, 1] {
        step(n);
    }
    let walked = made_new(start);
    eprintln!("three new frames: {walked} ungraded pictures made");
    assert!(walked <= 3, "three frames made {walked} ungraded pictures");

    let seen = PREGRADE_DISPLAYS_MADE.load(Ordering::Relaxed);
    for n in [-1, -1, -1, 1, 1, 1] {
        step(n);
    }
    let again = made_new(seen);
    eprintln!("six steps over frames already seen: {again} ungraded pictures made");
    assert_eq!(again, 0, "frames already seen made {again} more ungraded pictures");
}
