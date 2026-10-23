//! A clip shows its own media, and no other track's (#2138). The picture
//! track's shot ends into a gap and the shot after the gap is prerolled;
//! in the gap another track becomes the picture track, and its second
//! clip, at the same index as the prerolled shot, cuts in. The preroll
//! was known by its index alone, so the other track's shot was swapped
//! in for that clip: in the Stress Test a tile of colour bars showed a
//! still frame of a dog from forty seconds later.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 160;
const H: u32 = 90;
const FPS: u32 = 25;

/// Seven seconds of one colour. `None` without ffmpeg.
fn solid(colour: &str) -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-solid-{colour}-{W}x{H}.mkv"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mkv");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("color=c={colour}:s={W}x{H}:r={FPS}:d=7"), "-c:v", "ffv1"])
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
fn a_cut_on_another_track_never_takes_the_prerolled_shot() {
    let fps = Fps::new(FPS, 1);
    let tc = |s: f64| common::frame_tc((s * f64::from(FPS)).round() as i64, fps);
    let mut pool = MediaPool::default();
    let mut media = |colour: &str| -> Option<MediaId> {
        let path = solid(colour)?;
        let id = MediaId::new();
        pool.add(MediaItem {
            id,
            path: path.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: W, height: H },
            ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(7.0), fps)
        });
        Some(id)
    };
    let (Some(red), Some(blue), Some(green), Some(yellow)) = (media("red"), media("blue"), media("green"), media("yellow")) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let shot = |m: MediaId, from: f64, to: f64| Clip { kind: ClipKind::Media { media_id: m }, source_in: tc(0.0), source_out: tc(to - from), ..Clip::gap(tc(from), tc(to)) };
    // The picture track: red, a gap, then blue far off, prerolled from
    // red's last moments as clip 1 of this track.
    let mut a = Track::new("V1", TrackKind::Video);
    a.clips = vec![shot(red, 0.0, 1.0), shot(blue, 5.0, 6.0)];
    // In red's gap this track takes the picture over, and its clip 1,
    // yellow, cuts in at 2.5 s: alone on screen until 4 s.
    let mut b = Track::new("V2", TrackKind::Video);
    b.clips = vec![shot(green, 0.5, 2.5), shot(yellow, 2.5, 4.0)];
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![a, b];

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let playhead = Arc::new(Mutex::new(0.0f64));
    let seen = Arc::clone(&playhead);
    std::thread::spawn(move || {
        while let Ok(ev) = ev_rx.recv() {
            if let AppEvent::PlayheadMoved(t) = ev {
                *seen.lock().unwrap() = t.to_secs_f64();
            }
        }
    });
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = std::thread::spawn(move || engine.run());

    cmd.send(AppCommand::SetPlayhead(tc(0.0))).unwrap();
    common::settle(&fb);
    cmd.send(AppCommand::Play).unwrap();
    // Played across the cut to yellow, waited on rather than timed.
    let t0 = Instant::now();
    while *playhead.lock().unwrap() < 3.2 && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let reached = *playhead.lock().unwrap();
    cmd.send(AppCommand::Pause).unwrap();
    common::settle(&fb);
    let centre = || {
        let fb = fb.lock().unwrap();
        let (data, w, h, _) = fb.latest_frame();
        let i = ((h / 2 * w + w / 2) * 4) as usize;
        [data[i], data[i + 1], data[i + 2]]
    };
    // Yellow: red and green up, blue down. The prerolled shot was blue.
    let yellow = |px: [u8; 3]| px[0] > 180 && px[1] > 180 && px[2] < 80;
    // Until the parked frame shows yellow, however busy the machine: a
    // quiet moment can come while the shot before it is still held, its
    // stream not yet open (#2222). A wrong shot stays wrong.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut px = centre();
    while !yellow(px) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        px = centre();
    }
    let at = *playhead.lock().unwrap();
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    assert!(reached >= 3.2, "playback never reached the yellow shot: at {reached} s");
    assert!((2.6..4.0).contains(&at), "parked at {at} s, not on the yellow shot");
    assert!(yellow(px), "at {at} s the yellow shot shows {px:?}, not yellow");
}
