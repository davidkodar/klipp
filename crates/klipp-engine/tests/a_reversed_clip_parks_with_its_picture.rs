//! #3217: a reversed clip parks with its picture at any speed. Parked at
//! 1 s, a 4K clip reversed at exactly a quarter showed a black frame,
//! with any blend, where a quarter and a hundredth either side, and every
//! other speed tried, showed the picture.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 3840;
const H: u32 = 2160;
const FPS: u32 = 25;

/// Three seconds of 4K test pattern, every frame a keyframe. `None`
/// without ffmpeg.
fn uhd() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-uhd-testsrc2-intra-3s.mp4");
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r={FPS}:d=3")])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "1", "-pix_fmt", "yuv420p"])
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

/// The mean of the engine's parked frame at `secs` of the clip reversed
/// at `speed`, `len` seconds long.
fn parked_mean(path: &std::path::Path, speed: f64, len: f64, secs: f64) -> f64 {
    let fps = Fps::new(FPS, 1);
    let tc = |s: f64| common::frame_tc((s * f64::from(FPS)).round() as i64, fps);
    let media = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id: media,
        path: path.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        is_still: false,
        has_audio: false,
        fps,
        ..MediaItem::new_synthetic_title(media, path.display().to_string(), tc(3.0), fps)
    });
    let mut clip = Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0.0), source_out: tc(3.0), ..Clip::gap(tc(0.0), tc(len)) };
    clip.speed = speed;
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![clip];
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![v1];
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
    let g = common::generation(&fb);
    cmd.send(AppCommand::SetPlayhead(tc(secs))).unwrap();
    assert!(common::wait_new(&fb, g, Duration::from_secs(30)).is_some(), "no frame was drawn at {secs} s");
    common::settle(&fb);
    let mean = {
        let fb = fb.lock().unwrap();
        let (data, _, _, _) = fb.latest_frame();
        data.chunks_exact(4).map(|p| (u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])) as f64 / 3.0).sum::<f64>() / (data.len() / 4) as f64
    };
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    mean
}

#[test]
fn a_reversed_clip_parks_with_its_picture_at_a_quarter() {
    let Some(path) = uhd() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    for (speed, len) in [(-0.25, 12.0), (-0.24, 12.0), (-0.5, 6.0)] {
        let mean = parked_mean(&path, speed, len, 1.0);
        eprintln!("reversed at {speed}, {len} s long, parked at 1 s: mean {mean:.1}");
        assert!(mean > 20.0, "reversed at {speed}, parked at 1 s, the frame is black (mean {mean:.1})");
    }
}
