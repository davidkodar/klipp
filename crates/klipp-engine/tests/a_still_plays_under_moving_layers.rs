//! A still image on the primary track plays at the frame rate when
//! something else on screen moves: its file is one frame, then its end of
//! stream, which waits until the clip's end (#978), and nothing was
//! presented in between. A moving background, titles, clips entering over
//! it all stood still until the still's clip ended, then jumped (#2097).

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

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

#[test]
fn a_still_over_a_moving_gradient_is_presented_every_frame() {
    let Some(png) = still_png() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let tc = |f| common::frame_tc(f, fps);
    let id = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id,
        path: png.clone(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        is_still: true,
        ..MediaItem::new_synthetic_title(id, png.display().to_string(), tc(1), fps)
    });
    // Ten seconds of the still over a gradient that drifts.
    let still = Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(250), ..Clip::gap(tc(0), tc(250)) };
    let gradient = Clip {
        kind: ClipKind::Generator(GeneratorData::LivingGradient { a: [12, 23, 32, 255], b: [29, 60, 65, 255], angle_deg: 25.0, drift_deg_per_sec: 90.0, glow: 0.18, grain: 0.0 }),
        source_in: tc(0),
        source_out: tc(250),
        ..Clip::gap(tc(0), tc(250))
    };
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    let mut top = Track::new("V2", TrackKind::Video);
    top.clips = vec![still];
    let mut under = Track::new("V1", TrackKind::Video);
    under.clips = vec![gradient];
    seq.tracks = vec![top, under];

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();

    cmd.send(AppCommand::SetPlayhead(tc(2))).unwrap();
    common::settle(&fb);
    cmd.send(AppCommand::Play).unwrap();
    // The still's one frame is shown first; count from the frame after.
    let start = common::generation(&fb);
    let first = common::wait_new(&fb, start, Duration::from_secs(30));
    // Waited on, not timed: a loaded machine shows fewer, later. Held,
    // the picture showed none until the clip's end, ten seconds on.
    const WANTED: u64 = 12;
    let from = common::generation(&fb);
    let t0 = Instant::now();
    while common::generation(&fb) - from < WANTED && t0.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let presented = common::generation(&fb) - from;
    let spent = t0.elapsed();
    let _ = cmd.send(AppCommand::Pause);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    assert!(first.is_some(), "playing showed nothing");
    assert!(presented >= WANTED, "{presented} frames presented in {spent:?} over a still");
}
