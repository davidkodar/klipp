//! A strobe plays at speed, every piece showing its own frame (#2376),
//! from the first play after the app starts (#2386). Its pieces are one
//! shot of one file cut into two-frame clips with two-frame gaps, the
//! file going on forward as the gaps do. A new decoder was opened and
//! seeked for each piece, which was not there by the time it was on
//! screen; then, with the playing decoder handed on across each gap, the
//! first gap's frame drawn on a cold GPU stack held the engine past the
//! next piece, and the strobe went black.
//!
//! Its own binary: the counters it reads are the process's.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;
/// Pieces of two frames with two-frame gaps, the source going on with
/// the timeline.
const PIECES: i64 = 9;

/// One test plays at a time: two engines decoding at once each fall
/// behind on time, and the counters read are the process's.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn centre(frame: &[u8]) -> u8 {
    let i = ((H as usize / 2) * W as usize + W as usize / 4) * 4;
    frame[i]
}

struct Running {
    cmd: Sender<AppCommand>,
    fb: Arc<Mutex<FrameBuffer>>,
    furthest: Arc<Mutex<f64>>,
    thread: JoinHandle<()>,
}

/// A fresh engine on the strobe: nothing drawn yet, its GPU stack cold;
/// `prewarm` false leaves it cold through the first gap too.
fn start(video: &std::path::Path, prewarm: bool) -> Running {
    let fps = Fps::new(common::FPS, 1);
    let tc = |f: i64| common::frame_tc(f, fps);
    let media = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id: media,
        path: video.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        ..MediaItem::new_synthetic_title(media, video.display().to_string(), tc(common::RAMP_N as i64), fps)
    });
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = (0..PIECES)
        .map(|k| Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(4 * k), source_out: tc(4 * k + 2), ..Clip::gap(tc(4 * k), tc(4 * k + 2)) })
        .collect();
    seq.tracks = vec![v1];
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
    let mut engine = engine;
    engine.set_prewarm_empty_stack(prewarm);
    let thread = std::thread::spawn(move || engine.run());
    Running { cmd, fb, furthest, thread }
}

fn stop(r: Running) {
    let _ = r.cmd.send(AppCommand::Pause);
    let _ = r.cmd.send(AppCommand::Quit);
    let _ = r.thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
}

/// Each piece's picture, parked on its first frame, on an engine of its
/// own; `None` without ffmpeg.
fn references(video: &std::path::Path) -> Vec<u8> {
    // The gaps are the GPU stack's frames: with no device (a driver
    // refusing new ones), this would test the CPU route and pass.
    if std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some() {
        assert!(klipp_effects::gpu_context::GpuContext::shared().is_some(), "GPU required");
    }
    let tc = |f: i64| common::frame_tc(f, Fps::new(common::FPS, 1));
    let r = start(video, true);
    let mut want = Vec::new();
    for k in 0..PIECES {
        let g = common::generation(&r.fb);
        r.cmd.send(AppCommand::SetPlayhead(tc(4 * k))).unwrap();
        common::wait_new(&r.fb, g, Duration::from_secs(10));
        common::settle(&r.fb);
        want.push(centre(r.fb.lock().unwrap().latest_frame().0));
    }
    stop(r);
    assert!(want.windows(2).all(|w| w[1] > w[0]), "the pieces are frames of a ramp: {want:?}");
    want
}

struct Played {
    /// Each frame shown while playing: the time it was shown for, its
    /// level.
    shown: Vec<(f64, u8)>,
    wall: f64,
    reached: f64,
    end: f64,
    handed: u64,
    cold: u64,
}

/// The strobe played from its start on a cold engine, as the first play
/// after the app starts: no gap drawn before.
fn played(video: &std::path::Path, prewarm: bool) -> Played {
    let fps = Fps::new(common::FPS, 1);
    let r = start(video, prewarm);
    let handed = klipp_engine::engine::decoders_handed_on();
    let (_, cold) = klipp_engine::engine::cut_openings();
    let g = common::generation(&r.fb);
    r.cmd.send(AppCommand::SetPlayhead(common::frame_tc(0, fps))).unwrap();
    common::wait_new(&r.fb, g, Duration::from_secs(10));
    common::settle(&r.fb);
    *r.furthest.lock().unwrap() = 0.0;
    let end = (4 * PIECES - 2) as f64 / fps.as_f64();
    let t0 = Instant::now();
    r.cmd.send(AppCommand::Play).unwrap();
    let mut shown: Vec<(f64, u8)> = Vec::new();
    let mut last = 0;
    while *r.furthest.lock().unwrap() < end && t0.elapsed() < Duration::from_secs(20) {
        {
            let fb = r.fb.lock().unwrap();
            let g = fb.latest_generation();
            if g != last {
                last = g;
                if let Some(at) = fb.latest_presented() {
                    shown.push((at.to_secs_f64(), centre(fb.latest_frame().0)));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let wall = t0.elapsed().as_secs_f64();
    let reached = *r.furthest.lock().unwrap();
    stop(r);
    Played {
        shown,
        wall,
        reached,
        end,
        handed: klipp_engine::engine::decoders_handed_on() - handed,
        cold: klipp_engine::engine::cut_openings().1 - cold,
    }
}

/// The pieces shown, each frame shown on a piece checked to be that
/// piece's (or the one after it in the ramp), never black.
fn pieces_shown(p: &Played, want: &[u8]) -> std::collections::BTreeSet<usize> {
    let frame = 1.0 / common::FPS as f64;
    let mut seen = std::collections::BTreeSet::new();
    for (at, level) in &p.shown {
        let k = (at / frame + 1e-6).floor() as i64;
        if k % 4 >= 2 || k / 4 >= PIECES {
            continue;
        }
        let piece = (k / 4) as usize;
        let next = want.get(piece + 1).copied().unwrap_or(255);
        assert!(*level + 2 >= want[piece] && *level <= next, "at {at:.3} s piece {piece} showed {level}, its frame is {}: {:?}", want[piece], p.shown);
        seen.insert(piece);
    }
    seen
}

#[test]
fn a_strobe_shows_every_piece_from_the_first_play() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(video) = common::ffv1_avi_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let want = references(&video);
    let p = played(&video, true);
    assert!(p.reached >= p.end, "playback reached only {:.2} s of {:.2}", p.reached, p.end);
    // At speed: the strobe plays in about its own length, not more than
    // twice that.
    assert!(p.wall < 2.0 * p.end + 0.5, "{:.2} s of strobe took {:.2} s to play", p.end, p.wall);
    let seen = pieces_shown(&p, &want);
    assert_eq!(seen.len(), PIECES as usize, "pieces shown {seen:?} of {PIECES}: {:?}", p.shown);
    assert!(p.handed >= PIECES as u64 - 1, "the decoder was not handed on across the gaps");
    assert_eq!(p.cold, 0, "a piece's file was opened again at its cut");
}

/// With the stack left cold, its first gap holds the engine past a
/// piece or two and that hand-over comes too late. The shot there is
/// opened as a seek opens it, and the next pieces wait while a decoder
/// opens (a few, as one takes longer to open than a piece lasts); then
/// every piece shows to the end, never black for good (#2386).
#[test]
fn a_late_hand_over_is_recovered_from() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(video) = common::ffv1_avi_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let want = references(&video);
    let p = played(&video, false);
    assert!(p.reached >= p.end, "playback reached only {:.2} s of {:.2}", p.reached, p.end);
    let seen = pieces_shown(&p, &want);
    // From some piece on, every one shows, and that piece comes no later
    // than the third from the end.
    let from = (0..PIECES as usize).rev().take_while(|k| seen.contains(k)).last().unwrap_or(PIECES as usize);
    assert!(from <= PIECES as usize - 3, "the strobe never came back: shown {seen:?}: {:?}", p.shown);
}
