//! A clip slowed to 50% plays at the timeline's rate (#2108): parked on
//! it and played, the playhead moved twice as fast as the timeline runs,
//! because the seek that parked there gave the clock the clip's speed and
//! nobody gave it to the clock.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

/// Four seconds at 60 fps, a keyframe each second. `None` without ffmpeg.
fn sixty() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-retimed-{W}x{H}-60.mp4"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r=60:d=4")])
        .args(["-c:v", "libx264", "-g", "60", "-pix_fmt", "yuv420p"])
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

#[test]
fn a_clip_at_half_speed_plays_at_the_timelines_rate() {
    let Some(path) = sixty() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(30, 1);
    let tc = |s: f64| RationalTime::from_secs_f64(s, fps.rate());
    let id = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id,
        path: path.clone(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        is_still: false,
        has_audio: false,
        fps: Fps::new(60, 1),
        ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(4.0), Fps::new(60, 1))
    });
    // Four seconds of source over eight of timeline.
    let mut clip = Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0.0), source_out: tc(4.0), ..Clip::gap(tc(0.0), tc(8.0)) };
    clip.speed = 0.5;
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let moves: Arc<Mutex<Vec<(Instant, f64)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&moves);
    std::thread::spawn(move || {
        while let Ok(ev) = ev_rx.recv() {
            if let AppEvent::PlayheadMoved(t) = ev {
                seen.lock().unwrap().push((Instant::now(), t.to_secs_f64()));
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
    cmd.send(AppCommand::SetPlayhead(tc(1.0))).unwrap();
    common::settle(&fb);
    std::thread::sleep(Duration::from_millis(300));
    cmd.send(AppCommand::Play).unwrap();
    let t0 = Instant::now();
    std::thread::sleep(Duration::from_millis(2500));
    let _ = cmd.send(AppCommand::Pause);
    let after: Vec<(Instant, f64)> = moves.lock().unwrap().iter().copied().filter(|(t, _)| *t >= t0).collect();
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    let (Some(first), Some(last)) = (after.iter().find(|m| m.1 > 1.0 + 0.5 / 30.0), after.last()) else { panic!("the playhead never moved: {after:?}") };
    let rate = (last.1 - first.1) / last.0.duration_since(first.0).as_secs_f64().max(1e-3);
    // Waited on, not timed tightly: a loaded machine plays slower, never
    // faster. Twice the rate was the bug.
    assert!(rate < 1.25, "the playhead runs at {rate:.2}x the timeline's rate over a clip at 50%");
    assert!(rate > 0.5, "the playhead hardly moves: {rate:.2}x");
}
