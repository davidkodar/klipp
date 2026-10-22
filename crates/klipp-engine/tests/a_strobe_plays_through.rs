//! A strobe plays at speed, every piece showing its own frame (#2376).
//! Its pieces are one shot of one file cut into two-frame clips with
//! two-frame gaps, the file going on forward as the gaps do. A new
//! decoder was opened and seeked for each piece, which was not there by
//! the time it was on screen: playback crawled and showed black. Now the
//! playing decoder is handed on across each gap and decodes forward.
//!
//! Its own binary: the counters it reads are the process's.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;
/// Pieces of two frames with two-frame gaps, the source going on with
/// the timeline.
const PIECES: i64 = 9;

fn centre(frame: &[u8]) -> u8 {
    let i = ((H as usize / 2) * W as usize + W as usize / 4) * 4;
    frame[i]
}

#[test]
fn a_strobe_shows_every_piece_while_playing() {
    let Some(video) = common::ffv1_avi_ramp() else {
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
        ..MediaItem::new_synthetic_title(media, video.display().to_string(), tc(common::RAMP_N as i64), fps)
    });
    let pieces: Vec<Clip> = (0..PIECES)
        .map(|k| Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(4 * k), source_out: tc(4 * k + 2), ..Clip::gap(tc(4 * k), tc(4 * k + 2)) })
        .collect();
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = pieces;
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
    let perf = Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default()));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::clone(&perf),
    );
    let thread = std::thread::spawn(move || engine.run());

    // Each piece's picture, parked on its first frame.
    let mut want = Vec::new();
    for k in 0..PIECES {
        let g = common::generation(&fb);
        cmd.send(AppCommand::SetPlayhead(tc(4 * k))).unwrap();
        common::wait_new(&fb, g, Duration::from_secs(10));
        common::settle(&fb);
        want.push(centre(fb.lock().unwrap().latest_frame().0));
    }
    assert!(want.windows(2).all(|w| w[1] > w[0]), "the pieces are frames of a ramp: {want:?}");
    // A gap's frame too, so the stack that draws the gaps has drawn one
    // before playback asks it to on time.
    let g = common::generation(&fb);
    cmd.send(AppCommand::SetPlayhead(tc(2))).unwrap();
    common::wait_new(&fb, g, Duration::from_secs(10));
    common::settle(&fb);

    let handed = klipp_engine::engine::decoders_handed_on();
    let (_, cold) = klipp_engine::engine::cut_openings();
    cmd.send(AppCommand::SetPlayhead(tc(0))).unwrap();
    common::settle(&fb);
    *furthest.lock().unwrap() = 0.0;
    let end = (4 * PIECES - 2) as f64 / fps.as_f64();
    let t0 = Instant::now();
    cmd.send(AppCommand::Play).unwrap();
    // Every frame shown while playing, with the time it was shown for.
    let mut shown: Vec<(f64, u8)> = Vec::new();
    let mut last = 0;
    while *furthest.lock().unwrap() < end && t0.elapsed() < Duration::from_secs(20) {
        {
            let fb = fb.lock().unwrap();
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
    let _ = cmd.send(AppCommand::Pause);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    assert!(*furthest.lock().unwrap() >= end, "playback reached only {:.2} s of {end:.2}", *furthest.lock().unwrap());
    // At speed: the strobe plays in about its own length, not more than
    // twice that.
    assert!(wall < 2.0 * end + 0.5, "{end:.2} s of strobe took {wall:.2} s to play");
    // Each frame shown on a piece is that piece's (or the frame after it
    // in the ramp), never black.
    let frame = 1.0 / fps.as_f64();
    let mut on_pieces = 0;
    for (at, level) in &shown {
        let k = (at / frame).floor() as i64;
        if k % 4 >= 2 || k / 4 >= PIECES {
            continue;
        }
        on_pieces += 1;
        let piece = (k / 4) as usize;
        let next = want.get(piece + 1).copied().unwrap_or(255);
        assert!(*level + 2 >= want[piece] && *level <= next, "at {at:.3} s piece {piece} showed {level}, its frame is {}", want[piece]);
    }
    assert!(on_pieces >= PIECES as usize, "only {on_pieces} frames were shown on the pieces: {shown:?}");
    assert!(klipp_engine::engine::decoders_handed_on() - handed >= PIECES as u64 - 1, "the decoder was not handed on across the gaps");
    assert_eq!(klipp_engine::engine::cut_openings().1 - cold, 0, "a piece's file was opened again at its cut");
}
