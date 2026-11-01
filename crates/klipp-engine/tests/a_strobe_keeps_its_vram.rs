//! A strobe played over and over keeps its GPU memory bounded (#2395): a
//! strobe of one file whose two-frame pieces each open a stream of their
//! own, the per-piece path that filled 16 GB of VRAM before #2382, played
//! eight times. The memory this process holds on the GPU once a pass has
//! settled stays where the first pass left it, and goes back near nothing
//! once the engine has quit and its decoders have closed.
//!
//! Read per process from `nvidia-smi`, so other work on the GPU does not
//! count; without it (another vendor, no driver tools) unverified, and a
//! failure with `KLIPP_TEST_REQUIRE_GPU=1`. Its own binary: the memory
//! read is the process's.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 1920;
const H: u32 = 1080;
const PIECES: i64 = 12;
const PASSES: usize = 8;
/// How far the settled memory may move from the first pass's: the
/// driver's pools grow in 64 MB steps.
const DRIFT_MB: f64 = 128.0;
/// What may stay on the GPU once the engine has quit: the CUDA context
/// and the shared device's own, some 60 MB.
const LEFT_MB: f64 = 256.0;

/// This process's GPU memory in MB, `None` without `nvidia-smi`.
fn own_vram_mb() -> Option<f64> {
    let o = std::process::Command::new("nvidia-smi").args(["--query-compute-apps=pid,used_memory", "--format=csv,noheader,nounits"]).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let me = std::process::id().to_string();
    Some(String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| {
        let mut f = l.split(',').map(str::trim);
        (f.next()? == me).then(|| f.next()?.parse::<f64>().ok()).flatten()
    }).sum())
}

#[test]
fn a_strobe_played_over_and_over_keeps_its_vram() {
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    if own_vram_mb().is_none() {
        assert!(!required, "GPU required, and nvidia-smi gives no reading");
        eprintln!("UNVERIFIED: no nvidia-smi to read the GPU memory");
        return;
    }
    let Some(video) = common::hevc_source("strobe-vram", W, H, common::SDR_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let tc = |f: i64| common::frame_tc(f, fps);
    let media = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id: media,
        path: video.clone(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        ..MediaItem::new_synthetic_title(media, video.display().to_string(), tc(50), fps)
    });
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    // The pieces over the file whole, from places in it out of order: no
    // piece follows on from the one before, so each opens its own stream.
    let mut pieces = Track::new("V2", TrackKind::Video);
    pieces.clips = (0..PIECES)
        .map(|k| {
            let s = (k * 17) % 46;
            Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(s), source_out: tc(s + 2), ..Clip::gap(tc(4 * k), tc(4 * k + 2)) }
        })
        .collect();
    let mut under = Track::new("V1", TrackKind::Video);
    under.clips = vec![Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0), source_out: tc(50), ..Clip::gap(tc(0), tc(50)) }];
    seq.tracks = vec![pieces, under];

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let furthest = Arc::new(Mutex::new(0.0f64));
    let f = Arc::clone(&furthest);
    std::thread::spawn(move || {
        while let Ok(ev) = ev_rx.recv() {
            if let AppEvent::PlayheadMoved(tc) = ev {
                let mut f = f.lock().unwrap();
                *f = f.max(tc.to_secs_f64());
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
    let opened_before = klipp_engine::engine::streams_opened();
    let end = (4 * PIECES - 2) as f64 / fps.as_f64();
    let mut settled: Vec<f64> = Vec::new();
    for _ in 0..PASSES {
        let g = common::generation(&fb);
        cmd.send(AppCommand::SetPlayhead(tc(0))).unwrap();
        common::wait_new(&fb, g, Duration::from_secs(10));
        common::settle(&fb);
        *furthest.lock().unwrap() = 0.0;
        let t0 = Instant::now();
        cmd.send(AppCommand::Play).unwrap();
        while *furthest.lock().unwrap() < end && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(20));
        }
        cmd.send(AppCommand::Pause).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        settled.push(own_vram_mb().unwrap_or(0.0));
    }
    let opened = klipp_engine::engine::streams_opened() - opened_before;
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    std::thread::sleep(Duration::from_secs(1));
    let left = own_vram_mb().unwrap_or(0.0);
    eprintln!("settled after each pass {settled:?} MB, {opened} streams opened, {left:.0} MB left after quitting");
    // The per-piece path ran: a stream for each piece, every pass.
    assert!(opened >= (PIECES as u64) * PASSES as u64 / 2, "only {opened} streams opened: the pieces did not each take their own");
    let first = settled[0];
    let most = settled.iter().cloned().fold(0.0, f64::max);
    assert!(most - first <= DRIFT_MB, "the GPU memory grew pass by pass, {first:.0} to {most:.0} MB: {settled:?}");
    assert!(left <= LEFT_MB, "{left:.0} MB still on the GPU after the engine quit");
}
