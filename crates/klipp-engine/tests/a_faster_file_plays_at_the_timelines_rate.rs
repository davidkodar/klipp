//! A file at a higher rate than the timeline plays at the timeline's
//! rate (#2556): a 30 fps file on a 23.976 timeline showed 26.5 to 28
//! pictures a second, each of the file's frames published at its own
//! time. Each picture is now one timeline frame, the file's frame nearest
//! to that frame's time, as export renders it.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

/// Six seconds at 30 fps, a keyframe each second. `None` without ffmpeg.
fn thirty() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-faster-file-{W}x{H}-30.mp4"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r=30:d=6")])
        .args(["-c:v", "libx264", "-g", "30", "-pix_fmt", "yuv420p"])
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
fn a_thirty_fps_file_shows_one_picture_per_timeline_frame() {
    let Some(path) = thirty() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(24000, 1001);
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
        fps: Fps::new(30, 1),
        ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(6.0), Fps::new(30, 1))
    });
    let clip = Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0.0), source_out: tc(6.0), ..Clip::gap(tc(0.0), tc(6.0)) };
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];

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
    // Parked on a timeline frame, the twelfth.
    cmd.send(AppCommand::SetPlayhead(RationalTime { value: 12 * 1001, rate: fps.rate() })).unwrap();
    common::settle(&fb);
    std::thread::sleep(Duration::from_millis(300));
    cmd.send(AppCommand::Play).unwrap();
    // The pictures shown, sampled as they come.
    let mut shown: Vec<RationalTime> = Vec::new();
    let end = Instant::now() + Duration::from_millis(2600);
    while Instant::now() < end {
        if let Some(at) = fb.lock().unwrap().latest_presented() {
            if shown.last() != Some(&at) {
                shown.push(at);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let rate = fb.lock().unwrap().presented_fps(Instant::now(), Duration::from_millis(1500));
    let _ = cmd.send(AppCommand::Pause);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    let rate = rate.expect("pictures were shown");
    // A loaded machine shows fewer, never more: more than the timeline's
    // 23.976 a second was the file's frames shown as they came.
    assert!(rate <= 24.5, "{rate:.1} pictures a second on a 23.976 timeline");
    assert!(rate > 10.0, "playback hardly moved: {rate:.1} a second");
    // Each picture is a timeline frame's.
    let frame = |t: &RationalTime| t.to_secs_f64() * fps.as_f64();
    let off: Vec<f64> = shown.iter().map(frame).filter(|f| (f - f.round()).abs() > 1e-3).collect();
    assert!(off.is_empty(), "pictures between timeline frames: {off:?}");
}
