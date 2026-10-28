//! A nest of scenes over the same shots keeps its GPU memory: each level
//! more costs a little, not a decoder of each file and a full frame of
//! the stack for every scene in it. A template saved from a placed
//! template, placed again, nests that way, every level on a track of its
//! own showing the same two files at the same time; each level more took
//! 1.5 GB at 1080p, two more decoders and a kept frame for every scene
//! inside it. Taken down to one level and built up again, the second
//! time takes no more than the first: what the levels held is reused,
//! not kept beside what comes next.
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
/// What a level more may cost: a frame of the stack is 33 MB at 1080p,
/// and the driver's pools grow in 64 MB steps.
const PER_LEVEL_MB: f64 = 200.0;
/// How far a second build of the same nest may read above the first: the
/// driver's 64 MB step.
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

/// `depth` levels as the template op nests them: a scene of the two
/// shots side by side; each level a scene holding the one below whole;
/// the top every level at once, a track each, with no shot of its own.
fn nest(depth: usize, left: MediaId, right: MediaId, fps: Fps) -> Sequence {
    let tc = |f: i64| common::frame_tc(f, fps);
    let scene = |tracks: Vec<Track>| {
        let mut s = Sequence::new_default();
        s.fps = fps;
        s.resolution = Resolution { width: W, height: H };
        s.tracks = tracks;
        s
    };
    let track = |c: Clip| {
        let mut t = Track::new("V", TrackKind::Video);
        t.clips = vec![c];
        t
    };
    let half = |m: MediaId, on_left: bool| {
        let c = Clip { kind: ClipKind::Media { media_id: m }, source_in: tc(0), source_out: tc(45), ..Clip::gap(tc(0), tc(45)) };
        klipp_timeline::test_clip::TestClip::of(c).effect(klipp_effects::BuiltinEffect::Crop { left: if on_left { 0.0 } else { 0.5 }, right: if on_left { 0.5 } else { 0.0 }, top: 0.0, bottom: 0.0, feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5 }).build()
    };
    let placed = |s: &Sequence| {
        let mut c = Clip::precomposition(s.id, tc(0), tc(45));
        c.source_out = tc(45);
        c
    };
    let mut levels = vec![scene(vec![track(half(left, true)), track(half(right, false))])];
    for k in 1..depth {
        let below = placed(&levels[k - 1]);
        levels.push(scene(vec![track(below)]));
    }
    let top = scene(levels.iter().rev().map(|s| track(placed(s))).collect());
    let mut all = levels;
    all.push(top.clone());
    klipp_timeline::flatten::flatten_sequence(&top, &all)
}

#[test]
fn a_nest_of_scenes_costs_little_a_level() {
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    if own_vram_mb().is_none() {
        assert!(!required, "GPU required, and nvidia-smi gives no reading");
        eprintln!("UNVERIFIED: no nvidia-smi to read the GPU memory");
        return;
    }
    let (Some(a), Some(b)) = (common::hevc_source("nest-vram-a", W, H, common::SDR_TAGS), common::hevc_source("nest-vram-b", W, H, common::SDR_TAGS)) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let mut pool = MediaPool::default();
    let mut add = |path: &std::path::Path| {
        let id = MediaId::new();
        pool.add(MediaItem {
            id,
            path: path.to_path_buf(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: W, height: H },
            ..MediaItem::new_synthetic_title(id, path.display().to_string(), common::frame_tc(50, fps), fps)
        });
        id
    };
    let (left, right) = (add(&a), add(&b));

    let shared = Arc::new(klipp_timeline::SharedSequence::new(nest(1, left, right, fps)));
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, _ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::clone(&shared), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = std::thread::spawn(move || engine.run());
    cmd.send(AppCommand::Pause).unwrap();

    // Each depth parked complete, at a frame of its own so it is drawn
    // again, then read once the memory has settled.
    let parked = |depth: usize, frame: i64| -> f64 {
        shared.replace(nest(depth, left, right, fps));
        let target = common::frame_tc(frame, fps);
        cmd.send(AppCommand::SeekTo(target)).unwrap();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(20) {
            let done = {
                let fb = fb.lock().unwrap();
                fb.latest_presented() == Some(target) && fb.latest_complete()
            };
            if done {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(fb.lock().unwrap().latest_complete(), "{depth} levels: no complete frame in {:?}", t0.elapsed());
        std::thread::sleep(Duration::from_millis(1500));
        let mb = own_vram_mb().unwrap_or(0.0);
        eprintln!("{depth} levels: {mb:.0} MB");
        mb
    };
    let one = parked(1, 20);
    let mut deepest = one;
    for depth in 2..=4 {
        deepest = parked(depth, 20 + depth as i64);
        let per_level = (deepest - one) / (depth - 1) as f64;
        assert!(per_level <= PER_LEVEL_MB, "{depth} levels take {deepest:.0} MB, {per_level:.0} MB a level over one level's {one:.0} MB; at most {PER_LEVEL_MB} MB a level");
    }
    // Down to one level and up again, by way of an undo and a redo.
    parked(1, 30);
    let again = parked(4, 31);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    assert!(again <= deepest + AGAIN_MB, "four levels built again take {again:.0} MB, {:.0} MB more than the first time's {deepest:.0} MB", again - deepest);
}
