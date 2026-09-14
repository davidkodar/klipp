//! Which seek a decoded frame belongs to.
//!
//! A seek is a command on a channel: the decode thread clears its queue when
//! it takes one, but a frame converted just before can still land after the
//! clear. Consumers used to guess at those from the timestamp — "more than a
//! second from the target, so probably stale" — which cannot tell a stale
//! frame from a legitimately distant one, and in the other direction let a
//! frame up to two seconds from the seek target pass as the frame at it.
//!
//! Every frame now carries the epoch of the seek it was decoded under, so
//! the question is answered rather than guessed. These tests pin the
//! contract that answer rests on.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use klipp_engine::video_decoder::VideoDecoder;

const W: u32 = 320;
const H: u32 = 180;
const FPS: u32 = 25;
/// Ten seconds, so a seek has somewhere far to go.
const FRAMES: u32 = 250;

/// One builder at a time, so concurrent tests wait rather than race.
fn source_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn source() -> Option<PathBuf> {
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        return None;
    }
    let _building = source_lock();
    let out = std::env::temp_dir().join(format!("klipp-seek-epoch-{W}x{H}-{FRAMES}.mkv"));
    if out.exists() {
        return Some(out);
    }
    // Per-process, per-thread: the tests in this binary run concurrently and
    // both want this source, so a shared partial name had one rename the
    // other's half-written file into place — or find its own gone and skip,
    // which reads as a pass.
    let partial = out.with_extension(format!(
        "partial-{}-{:?}.mkv",
        std::process::id(),
        std::thread::current().id(),
    ));
    // Every frame a different green, so a frame can be identified on sight
    // if a failure ever needs chasing.
    let filter = format!(
        "color=c=black:s={W}x{H}:r={FPS}:d={},format=rgb24,geq=r='40':g='mod(N,250)':b='180'",
        FRAMES / FPS,
    );
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &filter])
        .args(["-c:v", "ffv1"])
        .arg(&partial)
        .status()
        .ok()?
        .success();
    if !ok {
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    std::fs::rename(&partial, &out).ok()?;
    Some(out)
}

/// Frames the decoder hands over within `wait`, with their epochs, and
/// at least one of them — `wait` is how long to keep collecting *after*
/// the first frame arrives, not a budget for it to appear in.
///
/// The distinction is the difference between a reliable test and a
/// flaky one. A fixed window assumes the decode thread gets scheduled
/// and finishes a frame inside it, which is true when this file runs on
/// its own and not always true during a full workspace run, where every
/// other test binary is competing for the same cores and GPU. It failed
/// exactly once that way — "the decoder produced nothing to test with" —
/// which says nothing about epochs, the thing under test.
fn drain(dec: &VideoDecoder, wait: Duration) -> Vec<(f64, u64)> {
    let first_frame_by = Instant::now() + Duration::from_secs(20);
    let mut deadline = Instant::now() + wait;
    let mut out = Vec::new();
    while Instant::now() < deadline || (out.is_empty() && Instant::now() < first_frame_by) {
        // Once something arrives, collect for the caller's window from
        // that point, so the frames-after-a-seek cases still see a run
        // of frames rather than just the first.
        if out.is_empty() {
            deadline = Instant::now() + wait;
        }
        {
            let mut q = dec.frame_queue().lock().unwrap();
            while let Some(f) = q.front() {
                let (pts, epoch, eof) = (f.pts_secs(), f.seek_epoch, f.is_eof);
                q.pop();
                if !eof {
                    out.push((pts, epoch));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    out
}

#[test]
fn a_seek_claims_an_epoch_and_its_frames_carry_it() {
    let Some(src) = source() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let dec = VideoDecoder::open(&src, 0).expect("open");
    assert_eq!(dec.requested_seek(), 0, "no seek asked for yet");

    // Decoding from the start: epoch 0, the frames of no seek at all.
    dec.resume();
    let first = drain(&dec, Duration::from_millis(400));
    assert!(!first.is_empty(), "the decoder produced nothing to test with");
    assert!(
        first.iter().all(|(_, e)| *e == 0),
        "frames before any seek carry epoch 0, got {:?}",
        first.iter().map(|(_, e)| *e).collect::<Vec<_>>(),
    );

    // Each seek claims the next epoch, and says which it claimed.
    for (i, secs) in [8.0_f64, 2.0, 5.0].into_iter().enumerate() {
        let claimed = dec.seek(secs);
        assert_eq!(claimed, i as u64 + 1, "seeks claim epochs in order");
        assert_eq!(dec.requested_seek(), claimed, "the decoder reports the newest seek");
        let frames = drain(&dec, Duration::from_millis(500));
        // Whatever arrives, nothing from this seek may be stamped older
        // than it — that stamp is what tells a consumer the frame is stale.
        for (pts, epoch) in &frames {
            assert!(
                *epoch <= claimed,
                "a frame at {pts:.3}s claims epoch {epoch}, newer than the seek asked for",
            );
        }
        // The seek's *landing* is at the target. Not its later frames: once
        // landed, the decoder runs forward freely, and 500 ms of decoding a
        // small source covers the rest of the file — those frames sit far
        // from the target and are correctly stamped with this seek.
        let mine: Vec<f64> = frames.iter().filter(|(_, e)| *e == claimed).map(|(p, _)| *p).collect();
        if let Some(&landing) = mine.iter().min_by(|a, b| a.total_cmp(b)) {
            // A seek lands on the keyframe at or before the target, and this
            // source is all-intra, so it lands within a frame of it.
            assert!(
                (landing - secs).abs() < 0.1,
                "seek to {secs}s landed at {landing:.3}s",
            );
        }
    }
}

/// The frames in flight when a seek is taken keep the older epoch, so a
/// consumer can drop them. Without this, the only evidence a frame was stale
/// was its timestamp — and a stale frame's timestamp can sit anywhere,
/// including right next to the target.
///
/// Opportunistic: whether a frame is in flight at the moment the decode
/// thread takes the seek is a race, so a run that sees none proves nothing
/// and says so. It has been observed — a seek to 8 s arriving while frames
/// at 6.92 to 7.04 s were in flight, all four correctly left on the older
/// epoch. The 7.04 s one is the case that matters: at 0.96 s from the
/// target it sat inside the one-second window the primary path used to
/// accept, so it would have been taken for the frame at 8 s, 24 frames out.
#[test]
fn frames_from_before_a_seek_stay_on_the_older_epoch() {
    let Some(src) = source() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let dec = VideoDecoder::open(&src, 0).expect("open");
    dec.resume();

    // Let the queue fill from the head of the file, then seek away without
    // draining: everything already queued belongs to the epoch before it.
    let filled_by = Instant::now() + Duration::from_secs(20);
    while dec.frame_queue().lock().unwrap().front().is_none() && Instant::now() < filled_by {
        std::thread::sleep(Duration::from_millis(5));
    }
    let claimed = dec.seek(9.0);

    let frames = drain(&dec, Duration::from_millis(600));
    assert!(!frames.is_empty(), "nothing arrived to test with");
    let (old, new): (Vec<_>, Vec<_>) = frames.iter().partition(|(_, e)| *e < claimed);
    if old.is_empty() {
        eprintln!(
            "no frame was in flight when the seek was taken — this run proves \
             nothing about stale frames, only that the seek's own are marked",
        );
    } else {
        eprintln!("{} frame(s) from before the seek, {} after", old.len(), new.len());
    }
    // Every frame from the older epoch is from the head of the file, not
    // near the seek target — which is exactly the frame a timestamp-based
    // guess would have had trouble with had it landed near 9 s.
    for (pts, epoch) in &old {
        assert!(*epoch < claimed, "partition");
        assert!(
            *pts < 8.0,
            "a frame at {pts:.3}s carries the pre-seek epoch but sits at the target",
        );
    }
    // And the seek's own frames are where it was sent.
    for (pts, _) in &new {
        assert!(
            *pts >= 8.0,
            "a frame at {pts:.3}s carries the epoch of a seek to 9.0s",
        );
    }
}
