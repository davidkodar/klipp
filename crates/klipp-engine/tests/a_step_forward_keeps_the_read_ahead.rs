//! A seek a few frames ahead of the last decoded frame is answered by
//! decoding on, and the frames the decoder had already read ahead —
//! exactly a scrub's next positions — stay queued, restamped for the new
//! seek. A seek backwards, or far ahead, goes through the container and
//! starts the queue afresh.
//!
//! Every seek used to seek the container and flush, so each scrub step
//! decoded the whole group of pictures again from its keyframe and the
//! viewer showed 7 to 10 positions a second however fast the mouse moved.

mod common;

use klipp_engine::video_decoder::VideoDecoder;
use std::time::{Duration, Instant};

/// Wait until the queue holds at least `n` frames at or after `secs`
/// stamped with `epoch`, or give up after two seconds.
fn wait_for_frames(dec: &VideoDecoder, secs: f64, epoch: u64, n: usize) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        let q = dec.frame_queue().lock().unwrap();
        if q.len() >= n && q.oldest_pts_secs().is_some_and(|p| p >= secs - 0.05) && q.front().is_some_and(|f| f.seek_epoch == epoch) {
            return true;
        }
        drop(q);
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn a_step_forward_keeps_the_read_ahead_and_a_seek_back_does_not() {
    let Some(path) = common::hevc_ramp() else { return };
    let dec = VideoDecoder::open(&path, 0).expect("open the ramp");
    let e1 = dec.seek(1.0);
    dec.resume();
    // The decoder may deliver from a frame before 1.0 s (its skip
    // tolerance), so wait for enough frames that at least six lie past
    // the step's target whatever the first one is.
    assert!(wait_for_frames(&dec, 1.0, e1, 12), "the decoder never filled its queue after the first seek");

    // Two frames on at 25 fps: the queue is kept from there, under the
    // new epoch, the moment the seek is requested.
    let e2 = dec.seek(1.08);
    {
        let q = dec.frame_queue().lock().unwrap();
        assert!(q.len() >= 6, "the read-ahead was thrown away: {} queued", q.len());
        let front = q.front().expect("a queued frame");
        assert!((front.pts_secs() - 1.08).abs() < 0.03, "queue front at {:.3}s, not the new target", front.pts_secs());
        assert_eq!(front.seek_epoch, e2, "the kept frames carry the new epoch");
        assert!(e2 > e1);
    }

    // Back to before anything queued: those frames have gone by, so the
    // container is sought and the queue starts over.
    let e3 = dec.seek(0.5);
    // Nothing from before is kept under the new epoch. The queue need not
    // be empty when we look: the decode thread may already have put the
    // seek's first frame there, or a frame it was decoding for the old
    // epoch, which the engine drops as stale. Asserting emptiness raced
    // it and refused unrelated pushes on a loaded machine (#490).
    // Under one lock: every frame is looked at, the stale ones dropped
    // and the seek's own put back in order, so the checks below still
    // see its first frame.
    {
        let mut q = dec.frame_queue().lock().unwrap();
        let mut fresh = Vec::new();
        while let Some(f) = q.pop() {
            if f.seek_epoch >= e3 {
                assert!((f.pts_secs() - 0.5).abs() < 0.1, "a seek back kept a frame at {:.3}s under its new epoch", f.pts_secs());
                fresh.push(f);
            }
        }
        for f in fresh {
            q.push(f);
        }
    }
    assert!(wait_for_frames(&dec, 0.5, e3, 1), "no frame arrived for the seek back");
    let front_pts = dec.frame_queue().lock().unwrap().front().unwrap().pts_secs();
    assert!((front_pts - 0.5).abs() < 0.03, "after the seek back the queue starts at {front_pts:.3}s");
}
