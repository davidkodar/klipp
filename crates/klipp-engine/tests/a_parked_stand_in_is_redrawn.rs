//! A parked frame drawn from a clip's skim stand-in is redrawn from the
//! original, and the frame buffer says when that is still to come: where
//! the parked frame was judged, and whether a redraw is pending there. A
//! frame grab waits on it; it used to hand back the stand-in's frame, and
//! an HDR clip's stand-in is tone-mapped its own way, so grabs came back
//! brighter and paler than the export.
//!
//! The stand-in here is red and the original green, so which file a frame
//! came from is plain. Needs the ffmpeg CLI to write the sources; skips
//! without it.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

fn tc(frame: i64) -> Timecode {
    common::frame_tc(frame, Fps::new(common::FPS, 1))
}

/// Two seconds of one colour, `None` without ffmpeg.
fn solid(name: &str, colour: &str) -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-stand-in-{name}-{W}x{H}.mkv"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mkv");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("color=c={colour}:s={W}x{H}:r={}:d=2", common::FPS)])
        .args(["-vf", "format=yuv420p", "-c:v", "ffv1"])
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

/// Whether `rgba` is mostly green rather than red.
fn green(rgba: &[u8]) -> bool {
    let (mut r, mut g) = (0u64, 0u64);
    for p in rgba.chunks(4) {
        r += p[0] as u64;
        g += p[1] as u64;
    }
    g > r * 2
}

#[test]
fn a_parked_stand_in_frame_is_redrawn_and_said_to_be() {
    let (Some(original), Some(stand_in)) = (solid("original", "0x00c000"), solid("stand-in", "0xc00000")) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    // Every clip from its stand-in, as heavy footage plays.
    klipp_engine::video_decoder::set_optimised_playback_mode(2);
    let fps = Fps::new(common::FPS, 1);
    let mut pool = MediaPool::default();
    let id = MediaId::new();
    pool.add(MediaItem {
        id,
        path: original.clone(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        skim_file: Some(stand_in),
        ..MediaItem::new_synthetic_title(id, original.display().to_string(), tc(50), fps)
    });
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(50), ..Clip::gap(tc(0), tc(50)) }];
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![v1];

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, _events) = crossbeam_channel::unbounded::<AppEvent>();
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let _thread = engine.spawn();
    cmd.send(AppCommand::Pause).unwrap();

    // Sampled until the frame at `frame` is the original's: every time the
    // buffer says no redraw is coming there, the frame it shows there is
    // the original's. Whether it got there within ten seconds.
    let settles_green = |frame: i64| -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            {
                let mut fb = fb.lock().unwrap();
                fb.swap_pending();
                let here = fb.latest_presented() == Some(tc(frame));
                let is_green = green(fb.latest_frame().0);
                if fb.original_redraw == Some((tc(frame), false)) && here {
                    assert!(is_green, "at {frame} the buffer says no redraw is coming over the stand-in's frame");
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    };

    for frame in [10, 30, 10] {
        cmd.send(AppCommand::SeekTo(tc(frame))).unwrap();
        assert!(settles_green(frame), "the parked frame at {frame} is redrawn from the original and said to be");
    }
    klipp_engine::video_decoder::set_optimised_playback_mode(0);
}
