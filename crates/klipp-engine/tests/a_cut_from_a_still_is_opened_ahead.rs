//! The shot after a still is opened before the cut, not at it. A still's
//! file is one frame, held to its clip's end (#978); the look-ahead that
//! opens the next shot ran only as frames were taken, so behind a still it
//! never ran and the next shot's file was opened at the cut, the picture
//! freezing while it opened and then jumping (#2097). On the still's own
//! track the next shot comes from its preroll; on another track, from a
//! stream opened before it came up, which then becomes the primary
//! decoder rather than the file being opened a second time.
//!
//! Its own binary: the counters it reads are the process's.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;

/// A one-frame PNG. `None` without ffmpeg.
fn still_png() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-still-{W}x{H}.png"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "png");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:d=1"), "-frames:v", "1"])
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

fn item(id: MediaId, path: &std::path::Path, is_still: bool, fps: Fps) -> MediaItem {
    MediaItem {
        id,
        path: path.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        is_still,
        ..MediaItem::new_synthetic_title(id, path.display().to_string(), common::frame_tc(if is_still { 1 } else { common::RAMP_N as i64 }, fps), fps)
    }
}

fn clip(id: MediaId, from: i64, to: i64, fps: Fps) -> Clip {
    let tc = |f| common::frame_tc(f, fps);
    Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(to - from), ..Clip::gap(tc(from), tc(to)) }
}

fn track(name: &str, clips: Vec<Clip>) -> Track {
    let mut t = Track::new(name, TrackKind::Video);
    t.clips = clips;
    t
}

/// Play `tracks` from 0.2 s until the playhead passes 3.5 s, past the cut
/// at 3 s.
fn play(tracks: Vec<Track>, pool: MediaPool, fps: Fps) {
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = tracks;
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let furthest = Arc::new(Mutex::new(0.0f64));
    let f = Arc::clone(&furthest);
    std::thread::spawn(move || {
        while let Ok(ev) = ev_rx.recv() {
            if let AppEvent::PlayheadMoved(tc) = ev {
                let mut f = f.lock().unwrap();
                *f = f.max(tc.to_secs_f64());
            }
        }
    });
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();
    cmd.send(AppCommand::SetPlayhead(common::frame_tc(5, fps))).unwrap();
    common::settle(&fb);
    *furthest.lock().unwrap() = 0.0;
    cmd.send(AppCommand::Play).unwrap();
    // Waited on, not timed: a loaded machine gets there later.
    let t0 = Instant::now();
    while *furthest.lock().unwrap() < 3.5 && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let reached = *furthest.lock().unwrap();
    let _ = cmd.send(AppCommand::Pause);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    assert!(reached >= 3.5, "playback reached only {reached:.2} s");
}

#[test]
fn the_shot_after_a_still_is_opened_before_the_cut() {
    let (Some(png), Some(video)) = (still_png(), common::ffv1_avi_ramp()) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let (still, shot) = (MediaId::new(), MediaId::new());
    let pool = || {
        let mut pool = MediaPool::default();
        pool.add(item(still, &png, true, fps));
        pool.add(item(shot, &video, false, fps));
        pool
    };

    // Three seconds of the still, longer than the look-ahead, and then
    // the shot, on one track: the shot is the still's track's next, taken
    // from its preroll.
    let (warm, cold) = klipp_engine::engine::cut_openings();
    play(vec![track("V1", vec![clip(still, 0, 75, fps), clip(shot, 75, 100, fps)])], pool(), fps);
    let (warm2, cold2) = klipp_engine::engine::cut_openings();
    assert_eq!((warm2 - warm, cold2 - cold), (1, 0), "the cut from the still opened the next shot at the cut, not from its preroll");

    // The still on V2, the shot on V1 after it: the still's track runs
    // out, and the shot's stream, opened before it came up, becomes the
    // primary decoder. Its file is opened once: not again by a seek at the
    // cut.
    let late = klipp_engine::engine::streams_opened_on_screen();
    let taken = klipp_engine::engine::stream_take_overs();
    let (_, sought) = klipp_engine::engine::gap_hand_overs();
    play(vec![track("V2", vec![clip(still, 0, 75, fps)]), track("V1", vec![clip(shot, 75, 100, fps)])], pool(), fps);
    assert_eq!(klipp_engine::engine::streams_opened_on_screen() - late, 0, "the shot under the still opened its stream at the cut, not before");
    assert_eq!(klipp_engine::engine::stream_take_overs() - taken, 1, "the shot's stream was not taken over as the primary");
    assert_eq!(klipp_engine::engine::gap_hand_overs().1 - sought, 0, "the shot's file was opened again by a seek at the cut");
}
