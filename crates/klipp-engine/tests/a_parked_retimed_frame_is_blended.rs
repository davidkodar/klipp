//! #3216: a slowed clip parked between two of its frames shows its
//! blend every time it is parked, forward and reversed: the frame drawn
//! by optical flow or a cross-fade, not the nearest source frame. Parked
//! again and again, it was blended in some runs and not in others.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use klipp_core::*;
use klipp_effects::BuiltinEffect;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 3840;
const H: u32 = 2160;
const FPS: u32 = 25;

/// Three seconds of 4K panning across still, blurred noise, every frame
/// a keyframe: the frames either side of a time differ everywhere, and
/// flow follows them. `None` without ffmpeg.
fn pan() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-uhd-noise-pan-intra-3s.mp4");
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "nullsrc=s=4800x2700:d=1,geq=lum='random(1)*255':cb=128:cr=128,gblur=sigma=3"])
        .args(["-vf", &format!("loop=-1:1:0,fps={FPS},crop={W}:{H}:x='40+t*90':y='30+t*40',trim=duration=3")])
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

/// The engine's parked frame at 1 s of the clip slowed to `speed` with
/// `blend_mode` (0 the nearest frame, 1 a cross-fade, 2 optical flow).
fn parked(path: &std::path::Path, speed: f64, blend_mode: u32) -> Vec<u8> {
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
    // At 0.3 over ten seconds, at 1 s the source is halfway between two
    // frames: at 0.3 s forward, at 2.7 s reversed.
    let mut slow = Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0.0), source_out: tc(3.0), ..Clip::gap(tc(0.0), tc(10.0)) };
    slow.speed = speed;
    let slow = TestClip::of(slow).effect(BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode }).build();
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![slow];
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
    let thread = std::thread::spawn(move || engine.run());
    let g = common::generation(&fb);
    cmd.send(AppCommand::SetPlayhead(tc(1.0))).unwrap();
    assert!(common::wait_new(&fb, g, Duration::from_secs(30)).is_some(), "no frame was drawn at 1 s");
    common::settle(&fb);
    let frame = fb.lock().unwrap().latest_frame().0.to_vec();
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    frame
}

/// Mean difference over red, green and blue, in levels.
fn mean_diff(a: &[u8], b: &[u8]) -> f64 {
    let (sum, n) = a.chunks_exact(4).zip(b.chunks_exact(4)).fold((0u64, 0u64), |(s, n), (p, q)| (s + (0..3).map(|c| u64::from(p[c].abs_diff(q[c]))).sum::<u64>(), n + 3));
    sum as f64 / n.max(1) as f64
}

#[test]
fn a_slowed_clip_parked_between_frames_is_blended_every_time() {
    let Some(path) = pan() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let mut unblended = Vec::new();
    for speed in [0.3, -0.3] {
        let nearest = parked(&path, speed, 0);
        for run in 0..4 {
            for mode in [1, 2] {
                let off = mean_diff(&parked(&path, speed, mode), &nearest);
                eprintln!("at {speed}, run {run}, blend mode {mode}: {off:.2} levels from the nearest frame");
                if off < 1.0 {
                    unblended.push(format!("at {speed}, run {run}, blend mode {mode} ({off:.2} levels)"));
                }
            }
        }
    }
    assert!(unblended.is_empty(), "parked without its blend: {}", unblended.join("; "));
}
