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

/// A 23.976 fps original whose frame N is grey level 16 + 3N, so the
/// frame a picture came from can be read off it. `None` without ffmpeg.
fn counted_23976() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-stand-in-counted-23976-{W}x{H}.mkv"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mkv");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("nullsrc=s={W}x{H}:r=24000/1001:d=3")])
        .args(["-vf", "format=gray,geq=lum='16+3*N',format=yuv420p", "-c:v", "ffv1"])
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

/// The original's redraw over a parked stand-in is the file frame nearest
/// the playhead, as playback and export take it: a 23.976 fps clip on a
/// 25 fps timeline was parked on the frame on show at the time, a file
/// frame behind the export wherever the time fell past a frame's middle.
#[test]
fn a_parked_redraw_is_the_nearest_file_frame() {
    let (Some(original), Some(stand_in)) = (counted_23976(), solid("stand-in", "0xc00000")) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    klipp_engine::video_decoder::set_optimised_playback_mode(2);
    let fps = Fps::new(25, 1);
    let at = |f: i64| common::frame_tc(f, fps);
    let mut pool = MediaPool::default();
    let id = MediaId::new();
    pool.add(MediaItem {
        id,
        path: original.clone(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        skim_file: Some(stand_in),
        ..MediaItem::new_synthetic_title(id, original.display().to_string(), at(75), Fps::new(24000, 1001))
    });
    // One second into the file: timeline frame f shows source time
    // 1 + f/25, file frame (1 + f/25) x 23.976.
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip { kind: ClipKind::Media { media_id: id }, source_in: at(25), source_out: at(75), ..Clip::gap(at(0), at(50)) }];
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

    // The grey of the original's redraw parked at timeline frame `frame`.
    let parked_grey = |frame: i64| -> f64 {
        cmd.send(AppCommand::SeekTo(at(frame))).unwrap();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            {
                let mut fb = fb.lock().unwrap();
                fb.swap_pending();
                if fb.original_redraw == Some((at(frame), false)) && fb.latest_presented() == Some(at(frame)) {
                    let px = fb.latest_frame().0;
                    let (r, g) = px.chunks(4).fold((0u64, 0u64), |(r, g), p| (r + p[0] as u64, g + p[1] as u64));
                    assert!(g * 2 > r, "the original's grey, not the red stand-in");
                    return r as f64 / (px.len() / 4) as f64;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("no redraw from the original at {frame}");
    };
    let file_frame = |f: i64| (1.0 + f as f64 / 25.0) * 24000.0 / 1001.0;
    // Frames 14 and 19 fall short of a file frame's middle (37.40, 42.20):
    // nearest and on-show agree there, and they set the scale.
    let (g37, g42) = (parked_grey(14), parked_grey(19));
    let per_frame = (g42 - g37) / 5.0;
    assert!(per_frame > 1.0, "grey steps with the frame: {g37:.1} at 37, {g42:.1} at 42");
    for frame in [5, 10, 1] {
        let want = file_frame(frame).round();
        assert_ne!(want, file_frame(frame).floor(), "a time past a frame's middle");
        let got = 37.0 + (parked_grey(frame) - g37) / per_frame;
        assert!((got - want).abs() < 0.4, "timeline frame {frame} (file frame {:.2}): parked on file frame {got:.2}, the nearest is {want}", file_frame(frame));
    }
    klipp_engine::video_decoder::set_optimised_playback_mode(0);
}
