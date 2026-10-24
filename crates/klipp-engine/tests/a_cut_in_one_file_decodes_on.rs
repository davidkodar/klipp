//! Cuts within one file decode on, and a file's shots coming up are not
//! all opened at once (#2397). Played at 1080p from long-GOP HEVC, a
//! keyframe a second:
//!
//! - a strobe of 4-frame pieces, each four frames further on in its file,
//!   showed a third of its frames: each piece's preroll had the piece's
//!   160 ms to seek in. Now the playing decoder decodes on, and nothing
//!   is opened;
//! - the same strobe on a layer opened a stream for every piece; now one
//!   stream goes on from piece to piece;
//! - two files cut against each other on a layer held seven decoders and
//!   3.5 GB of the GPU's memory, every shot within the lead opened ahead;
//!   now one shot of each file ahead;
//! - one file over itself still takes two decoders.
//!
//! Its own binary: the counts of decoders open and the GPU memory read
//! are the process's.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 1920;
const H: u32 = 1080;
const FPS: u32 = 25;
const SECS: i64 = 20;
/// The share of a timeline's frames a pass must show.
const SHOWN: f64 = 0.95;
/// The GPU memory two files cut against each other on a layer may hold,
/// in MB: 1.97 GB measured, 3.5 GB with every shot opened ahead.
const ALTERNATION_MB: f64 = 2600.0;

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

/// A 20-second 10-bit HEVC test pattern with a keyframe a second, made
/// once; `None` without ffmpeg.
fn source(name: &str, pattern: &str) -> Option<PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-decodes-on-{name}-{W}x{H}.mp4"));
    if out.exists() {
        return Some(out);
    }
    let partial = out.with_extension(format!("{}.mp4", std::process::id()));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("{pattern}=s={W}x{H}:r={FPS}:d={SECS}")])
        .args(["-vf", "format=yuv420p10le", "-c:v", "libx265", "-preset", "ultrafast", "-x265-params", &format!("keyint={FPS}:min-keyint={FPS}:log-level=error")])
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

/// A shot: (file, timeline frame, length in frames, file frame).
type Shot = (usize, i64, i64, i64);

/// What one pass over a timeline did.
struct Pass {
    opened: u64,
    most_open: usize,
    handed_on: u64,
    shown: u64,
    frames: i64,
    vram_mb: Option<f64>,
}

/// Play `tracks` (top first) once from the start, over files `files`.
fn play(files: &[PathBuf], tracks: &[Vec<Shot>]) -> Pass {
    let fps = Fps::new(FPS, 1);
    let tc = move |f: i64| common::frame_tc(f, fps);
    let media: Vec<MediaId> = files.iter().map(|_| MediaId::new()).collect();
    let mut pool = MediaPool::default();
    for (id, path) in media.iter().zip(files) {
        pool.add(MediaItem {
            id: *id,
            path: path.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: W, height: H },
            ..MediaItem::new_synthetic_title(*id, path.display().to_string(), tc(FPS as i64 * SECS), fps)
        });
    }
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = tracks
        .iter()
        .enumerate()
        .map(|(i, shots)| {
            let mut t = Track::new(&format!("V{}", tracks.len() - i), TrackKind::Video);
            t.clips = shots
                .iter()
                .map(|&(m, at, len, from)| Clip { kind: ClipKind::Media { media_id: media[m] }, source_in: tc(from), source_out: tc(from + len), ..Clip::gap(tc(at), tc(at + len)) })
                .collect();
            t
        })
        .collect();
    let frames = tracks.iter().flatten().map(|s| s.1 + s.2).max().unwrap_or(0);

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
    let thread = std::thread::spawn(move || engine.run());
    let g = common::generation(&fb);
    cmd.send(AppCommand::SetPlayhead(tc(0))).unwrap();
    common::wait_new(&fb, g, Duration::from_secs(10));
    common::settle(&fb);

    let opened = klipp_engine::video_decoder::decoders_opened();
    let handed_on = klipp_engine::engine::streams_handed_on();
    klipp_engine::video_decoder::reset_most_decoders_open();
    let shown = common::generation(&fb);
    let mut vram_mb = own_vram_mb();
    let end = (frames - 1) as f64 / FPS as f64;
    let t0 = Instant::now();
    let mut read_at = Instant::now();
    cmd.send(AppCommand::Play).unwrap();
    while *furthest.lock().unwrap() < end && t0.elapsed() < Duration::from_secs_f64(3.0 * end + 10.0) {
        std::thread::sleep(Duration::from_millis(20));
        if read_at.elapsed() > Duration::from_millis(500) {
            vram_mb = vram_mb.zip(own_vram_mb()).map(|(a, b)| a.max(b));
            read_at = Instant::now();
        }
    }
    let pass = Pass {
        opened: klipp_engine::video_decoder::decoders_opened() - opened,
        most_open: klipp_engine::video_decoder::decoders_open().1,
        handed_on: klipp_engine::engine::streams_handed_on() - handed_on,
        shown: common::generation(&fb) - shown,
        frames,
        vram_mb,
    };
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    pass
}

fn shows_its_frames(name: &str, p: &Pass) {
    eprintln!("{name}: {} decoders opened, {} open at most, {} handed on, {}/{} frames shown, GPU {:?} MB", p.opened, p.most_open, p.handed_on, p.shown, p.frames, p.vram_mb);
    assert!(p.shown as f64 >= SHOWN * p.frames as f64, "{name}: showed {} of {} frames", p.shown, p.frames);
}

#[test]
fn cuts_in_one_file_decode_on_and_a_files_shots_open_one_ahead() {
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    let (Some(a), Some(b)) = (source("a", "testsrc2"), source("b", "testsrc")) else {
        assert!(!required, "GPU required, and no ffmpeg to make the sources");
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let files = [a, b];
    // 4-frame pieces, each four frames further on in the file, no gaps.
    let strobe: Vec<Shot> = (0..50).map(|k| (0, 4 * k, 4, 8 * k)).collect();

    let p = play(&files, std::slice::from_ref(&strobe));
    shows_its_frames("strobe", &p);
    assert_eq!(p.opened, 0, "the strobe's pieces opened decoders of their own");

    let p = play(&files, &[strobe, vec![(1, 0, 200, 0)]]);
    shows_its_frames("strobe on a layer", &p);
    assert!(p.handed_on >= 45, "only {} of the layer's 49 cuts went on with the stream before", p.handed_on);
    assert!(p.opened <= 1, "the layer's pieces opened {} streams", p.opened);

    // 12-frame shots of the two files cut against each other on a layer.
    let alternation: Vec<Shot> = (0..50).map(|i| if i % 2 == 0 { (0, 12 * i, 12, 12 * i) } else { (1, 12 * i, 12, 100 + 6 * i) }).collect();
    let p = play(&files, &[alternation, vec![(1, 0, 600, 0)]]);
    shows_its_frames("two files on a layer", &p);
    // The track below's, the shot on screen and one of each file ahead.
    assert!(p.most_open <= 4, "{} decoders open at once", p.most_open);
    if let Some(mb) = p.vram_mb {
        assert!(mb <= ALTERNATION_MB, "{mb:.0} MB on the GPU");
    } else {
        assert!(!required, "GPU required, and nvidia-smi gives no reading");
        eprintln!("UNVERIFIED: no nvidia-smi to read the GPU memory");
    }

    // One file over itself, at two times: a decoder each.
    let p = play(&files, &[vec![(0, 0, 300, 150)], vec![(0, 0, 300, 0)]]);
    shows_its_frames("one file over itself", &p);
    assert_eq!(p.most_open, 2, "one file over itself took {} decoders", p.most_open);
}
