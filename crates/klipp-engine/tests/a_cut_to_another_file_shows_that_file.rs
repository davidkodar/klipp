//! A cut to a clip of another file shows that file from its first frame.
//! The next clip's decoder is opened ahead of the cut on a worker thread
//! (#572), where it used to be opened on the engine thread: the cut takes
//! it when it has arrived, waits for it when it is still opening, and
//! either way plays the new file, not more of the old one.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;

fn item(id: MediaId, src: &std::path::Path, fps: Fps) -> MediaItem {
    MediaItem {
        id,
        path: src.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        ..MediaItem::new_synthetic_title(id, src.display().to_string(), common::frame_tc(common::RAMP_N as i64, fps), fps)
    }
}

/// Timeline frames `from..to` showing source frames from `source_in`.
fn shot(id: MediaId, from: i64, to: i64, source_in: i64, fps: Fps) -> Clip {
    let tc = |f| common::frame_tc(f, fps);
    Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(source_in), source_out: tc(source_in + to - from), ..Clip::gap(tc(from), tc(to)) }
}

/// The mean of a published frame's first channel.
fn mean(bytes: &[u8]) -> f64 {
    let px = bytes.chunks_exact(4);
    let n = px.len().max(1) as f64;
    px.map(|p| p[0] as f64).sum::<f64>() / n
}

/// File A's bright end (ramp frames 15–34) to file B's dark start (0–19),
/// cut at 0.8 s. Every frame published after the cut is darker than every
/// frame before it: B took over at the cut, where more of A would have
/// gone on getting brighter.
#[test]
fn a_cut_to_another_file_shows_that_file_from_its_first_frame() {
    let (Some(a), Some(b)) = (
        common::ramp("cut-a", &["-c:v", "ffv1"], "avi"),
        common::ramp("cut-b", &["-c:v", "ffv1"], "avi"),
    ) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let (ia, ib) = (MediaId::new(), MediaId::new());
    let mut pool = MediaPool::default();
    pool.add(item(ia, &a, fps));
    pool.add(item(ib, &b, fps));
    let mut seq = Sequence::new_default();
    (seq.fps, seq.resolution) = (fps, Resolution { width: W, height: H });
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![shot(ia, 0, 20, 15, fps), shot(ib, 20, 40, 0, fps)];
    seq.tracks = vec![v1];
    let cut = 20.0 / common::FPS as f64;

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

    cmd.send(AppCommand::SetPlayhead(common::frame_tc(2, fps))).unwrap();
    // The parked frame waited for, not timed: a loaded machine is slower,
    // not wrong.
    let parked = common::frame_tc(2, fps).to_secs_f64();
    let by = Instant::now() + Duration::from_secs(30);
    while Instant::now() < by && !fb.lock().unwrap().latest_presented().is_some_and(|t| (t.to_secs_f64() - parked).abs() < 0.5 / fps.as_f64()) {
        std::thread::sleep(Duration::from_millis(5));
    }
    cmd.send(AppCommand::Play).unwrap();
    // Every frame published, by where on the timeline it is.
    let mut seen: Vec<(f64, f64)> = Vec::new();
    let mut last_gen = 0;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(60) && seen.last().is_none_or(|s| s.0 < cut + 0.5) {
        {
            let fb = fb.lock().unwrap();
            let (bytes, _, _, generation) = fb.latest_frame();
            if generation != last_gen && !bytes.is_empty() {
                last_gen = generation;
                if let Some(at) = fb.latest_presented() {
                    seen.push((at.to_secs_f64(), mean(bytes)));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = cmd.send(AppCommand::Pause);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    // A decode thread still in ffmpeg's teardown at exit crashes the
    // process (CUDA's context destroyed under it).
    klipp_engine::video_decoder::wait_for_decode_threads(std::time::Duration::from_secs(10));

    let before: Vec<f64> = seen.iter().filter(|s| s.0 < cut - 1e-6).map(|s| s.1).collect();
    let after: Vec<f64> = seen.iter().filter(|s| s.0 >= cut - 1e-6).map(|s| s.1).collect();
    assert!(!before.is_empty() && !after.is_empty(), "playback did not cross the cut: {seen:?}");
    let darkest_before = before.iter().copied().fold(f64::MAX, f64::min);
    let brightest_after = after.iter().copied().fold(0.0, f64::max);
    assert!(
        brightest_after < darkest_before,
        "after the cut the picture is not file B's dark start (brightest {brightest_after:.1} vs A's darkest {darkest_before:.1}): {seen:?}"
    );
}
