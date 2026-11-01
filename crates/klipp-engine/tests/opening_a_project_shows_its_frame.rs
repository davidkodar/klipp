//! #2664: a project opened shows the frame at its playhead, with nothing
//! touched after. The engine is told as the app tells it on an open: the
//! old project dropped, the new sequence and media put in place, the
//! frame buffer cleared, the media loaded, and a seek to the playhead.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

/// The mean of a published frame's first channel.
fn mean(bytes: &[u8]) -> f64 {
    let px = bytes.chunks_exact(4);
    let n = px.len().max(1) as f64;
    px.map(|p| p[0] as f64).sum::<f64>() / n
}

/// A project of one clip of `src`, from its source frame `from`.
fn project(src: &std::path::Path, (w, h): (u32, u32), from: i64, fps: Fps) -> (Sequence, MediaPool, MediaId) {
    let id = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id,
        path: src.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: w, height: h },
        ..MediaItem::new_synthetic_title(id, src.display().to_string(), common::frame_tc(50, fps), fps)
    });
    let tc = |f| common::frame_tc(f, fps);
    let mut seq = Sequence::new_default();
    (seq.fps, seq.resolution) = (fps, Resolution { width: w, height: h });
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(from), source_out: tc(from + 10), ..Clip::gap(tc(0), tc(10)) }];
    seq.tracks = vec![v1];
    (seq, pool, id)
}

/// Opened twice, at start and over the same project parked at its start:
/// a picture each time, not black.
fn opens_onto_a_picture(src: &std::path::Path, size: (u32, u32), from: i64) {
    let fps = Fps::new(common::FPS, 1);
    // As the app starts: an empty sequence and pool.
    let shared = Arc::new(klipp_timeline::SharedSequence::new(Sequence::new_default()));
    let pool_shared = Arc::new(RwLock::new(MediaPool::default()));
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(size.0, size.1)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(size.0, size.1))),
        Arc::clone(&shared), Arc::clone(&pool_shared), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();
    std::thread::sleep(Duration::from_millis(300));

    for open in 0..2 {
        let (seq, pool, media) = project(src, size, from, fps);
        let before = common::generation(&fb);
        // `reset_project_state`, then `do_open_project_inner`.
        cmd.send(AppCommand::Stop).unwrap();
        cmd.send(AppCommand::ResetEngine).unwrap();
        shared.replace(seq);
        *pool_shared.write().unwrap() = pool;
        cmd.send(AppCommand::Stop).unwrap();
        fb.lock().unwrap().clear();
        cmd.send(AppCommand::LoadMediaFile { path: src.to_path_buf(), media_id: media }).unwrap();
        cmd.send(AppCommand::SeekTo(common::frame_tc(0, fps))).unwrap();
        // A picture, not the blank the reset leaves: waited for, not timed.
        let latest = || {
            let fb = fb.lock().unwrap();
            let (bytes, w, h, g) = fb.latest_frame();
            (bytes.to_vec(), w, h, g)
        };
        let until = Instant::now() + Duration::from_secs(20);
        while { let (_, w, _, g) = latest(); w == 0 || g == before } && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        common::settle(&fb);
        let (bytes, w, h, g) = latest();
        assert!(g != before, "open {open}: no frame after the open");
        assert!(w > 0 && h > 0, "open {open}: no picture after the open, only the blank");
        let m = mean(&bytes);
        assert!(m > 30.0, "open {open}: the viewer is black (mean {m:.1})");
    }

    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
}

#[test]
fn an_opened_project_shows_the_frame_at_its_playhead() {
    let Some(src) = common::ramp("opened", &["-c:v", "ffv1"], "avi") else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    opens_onto_a_picture(&src, (common::RAMP_W, common::RAMP_H), 25);
}

/// A 4K long-GOP file, whose first frame takes longer than a seek waits.
#[test]
fn an_opened_4k_project_shows_the_frame_at_its_playhead() {
    let Some(src) = common::hevc_source("opened-4k", 3840, 2160, "color_primaries=bt709:color_trc=bt709:colorspace=bt709") else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    opens_onto_a_picture(&src, (3840, 2160), 0);
}
