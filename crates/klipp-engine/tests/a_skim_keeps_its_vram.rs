//! Skimming across a timeline keeps its GPU memory bounded (#2945): ten
//! clips of ten files skimmed end to end open at most a few skim
//! decoders, each holding its frames on the GPU, and their memory goes
//! back once the skim ends; a second skim keeps no more than the first.
//!
//! Read per process from `nvidia-smi`, so other work on the GPU does not
//! count; without it unverified, and a failure with
//! `KLIPP_TEST_REQUIRE_GPU=1`. Its own binary: the memory read is the
//! process's.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 1920;
const H: u32 = 1080;
const CLIPS: i64 = 10;
/// The most the skim may hold over what the engine held before it: four
/// skim decoders' frames on the GPU (some 450 MB each at 1080p, #2935)
/// and the stack's buffers for a 1080p frame.
const SKIMMING_MB: f64 = 2400.0;
/// What may stay once a skim has ended: what the renderer sets up on its
/// first frame and every later one reuses, the GPU stack's frame-sized
/// buffers kept at the largest frame drawn, 258 MB measured at 1080p the
/// same for one clip as for ten (the decoders alone go back to where they
/// were). The decoders' frames and the held node outputs are let go.
/// Two of the driver's 64 MB steps over that: what is held after a skim
/// measured 795-859 MB run to run, and the engine before it 537 or 731
/// MB as the stack's buffers were made before the skim or by it.
const LEFT_MB: f64 = 258.0 + 2.0 * 64.0;
/// How far a second skim may leave memory above the first: the driver's
/// 64 MB step.
const AGAIN_MB: f64 = 64.0;

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
fn skimming_ten_clips_holds_a_few_decoders_and_lets_them_go() {
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    if own_vram_mb().is_none() {
        assert!(!required, "GPU required, and nvidia-smi gives no reading");
        eprintln!("UNVERIFIED: no nvidia-smi to read the GPU memory");
        return;
    }
    let fps = Fps::new(common::FPS, 1);
    let tc = |f: i64| common::frame_tc(f, fps);
    let mut pool = MediaPool::default();
    let mut v1 = Track::new("V1", TrackKind::Video);
    for k in 0..CLIPS {
        let Some(path) = common::hevc_source(&format!("skim-vram-{k}"), W, H, common::SDR_TAGS) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let id = MediaId::new();
        pool.add(MediaItem {
            id,
            path: path.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: W, height: H },
            skim_file: Some(path.clone()),
            ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(50), fps)
        });
        v1.clips.push(Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(25), ..Clip::gap(tc(25 * k), tc(25 * (k + 1))) });
    }
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![v1];

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, events) = crossbeam_channel::unbounded::<AppEvent>();
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = std::thread::spawn(move || engine.run());
    cmd.send(AppCommand::Pause).unwrap();
    cmd.send(AppCommand::SeekTo(tc(0))).unwrap();
    common::settle(&fb);
    std::thread::sleep(Duration::from_millis(1500));
    let before = own_vram_mb().unwrap_or(0.0);

    // End to end and back, a position every five frames, as a drag goes;
    // twice, the second time to see that the first left nothing it adds to.
    let mut drawn = 0;
    let mut most = before;
    let there: Vec<i64> = (0..25 * CLIPS).step_by(5).collect();
    let positions: Vec<i64> = there.iter().chain(there.iter().rev()).copied().collect();
    let mut afters = Vec::new();
    for _ in 0..2 {
        cmd.send(AppCommand::BeginScrub).unwrap();
        for (n, f) in positions.iter().enumerate() {
            cmd.send(AppCommand::SkimTimeline { time: tc(*f) }).unwrap();
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(10) {
                if let Ok(AppEvent::TimelineSkimShown { composited, .. }) = events.recv_timeout(Duration::from_millis(50)) {
                    drawn += usize::from(composited);
                    break;
                }
            }
            if n % 10 == 0 {
                most = most.max(own_vram_mb().unwrap_or(0.0));
            }
        }
        most = most.max(own_vram_mb().unwrap_or(0.0));
        cmd.send(AppCommand::EndScrub).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        afters.push(own_vram_mb().unwrap_or(0.0));
    }
    let after = afters[0];
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    eprintln!("before {before:.0} MB, most while skimming {most:.0} MB, after each skim {afters:?} MB; {drawn} of {} drawn", 2 * positions.len());
    assert!(drawn * 10 >= 2 * positions.len() * 9, "only {drawn} of {} skims were drawn", 2 * positions.len());
    assert!(most - before <= SKIMMING_MB, "skimming took {:.0} MB over the {before:.0} MB before it; at most {SKIMMING_MB}", most - before);
    assert!(after - before <= LEFT_MB, "{:.0} MB is still held after the skim ended (before {before:.0}, most {most:.0})", after - before);
    assert!(afters[1] <= afters[0] + AGAIN_MB, "a second skim left {:.0} MB more than the first", afters[1] - afters[0]);
}
