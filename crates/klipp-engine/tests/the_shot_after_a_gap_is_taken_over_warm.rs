//! The shot after a gap between shots is taken over from a decoder
//! opened during the gap, as a cut between adjacent shots is, not with a
//! seek; a gap of a frame is cut across (#546).

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;

struct Engine {
    cmd: crossbeam_channel::Sender<AppCommand>,
    /// The engine's thread, joined when the test is done: left running,
    /// it was still freeing its GPU objects when the test binary exited
    /// and the driver was torn down under it, a SIGSEGV (#567).
    thread: Option<std::thread::JoinHandle<()>>,
    /// Every playhead the engine reported, and when.
    moves: Arc<Mutex<Vec<(Instant, f64)>>>,
    paused: Arc<Mutex<Option<Instant>>>,
}

impl Engine {
    fn start(seq: Sequence, pool: MediaPool) -> Self {
        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
        let moves = Arc::new(Mutex::new(Vec::new()));
        let paused = Arc::new(Mutex::new(None));
        let (m, p) = (Arc::clone(&moves), Arc::clone(&paused));
        std::thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                match ev {
                    AppEvent::PlayheadMoved(tc) => m.lock().unwrap().push((Instant::now(), tc.to_secs_f64())),
                    AppEvent::PlaybackPaused => *p.lock().unwrap() = Some(Instant::now()),
                    _ => {}
                }
            }
        });
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::new(Mutex::new(FrameBuffer::new(W, H))), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
        );
        let thread = Some(std::thread::spawn(move || engine.run()));
        Engine { cmd, thread, moves, paused }
    }

    /// Seek to `secs`, let it land, then play; wait until playback
    /// pauses by itself or `limit` passes. Returns the playheads
    /// reported after Play, and whether it paused by itself.
    fn play_from(&self, secs: f64, fps: Fps, limit: Duration) -> (Vec<(Instant, f64)>, bool) {
        self.cmd.send(AppCommand::SetPlayhead(RationalTime::from_secs_f64(secs, fps.rate()))).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        *self.paused.lock().unwrap() = None;
        let t0 = Instant::now();
        self.cmd.send(AppCommand::Play).unwrap();
        while t0.elapsed() < limit && self.paused.lock().unwrap().is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
        let stopped = self.paused.lock().unwrap().is_some();
        let _ = self.cmd.send(AppCommand::Pause);
        let moves = self.moves.lock().unwrap().iter().copied().filter(|(t, _)| *t >= t0).collect();
        (moves, stopped)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.cmd.send(AppCommand::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn pool_of(id: MediaId, src: &std::path::Path, fps: Fps) -> MediaPool {
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id,
        path: src.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        ..MediaItem::new_synthetic_title(id, src.display().to_string(), common::frame_tc(common::RAMP_N as i64, fps), fps)
    });
    pool
}

fn shot(id: MediaId, from: i64, to: i64, fps: Fps) -> Clip {
    let tc = |f| common::frame_tc(f, fps);
    Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(to - from), ..Clip::gap(tc(from), tc(to)) }
}

fn sequence(tracks: Vec<Track>, fps: Fps) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = tracks;
    seq
}

fn track(name: &str, kind: TrackKind, clips: Vec<Clip>) -> Track {
    let mut t = Track::new(name, kind);
    t.clips = clips;
    t
}

fn furthest(moves: &[(Instant, f64)]) -> f64 {
    moves.iter().map(|m| m.1).fold(0.0, f64::max)
}

/// Two shots with 0.4 s of nothing between: the second is taken over
/// from its preroll. Then with one frame between: cut across, no gap
/// played at all. Both play to the end.
#[test]
fn the_shot_after_a_gap_is_taken_over_warm() {
    let Some(src) = common::ffv1_avi_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let id = MediaId::new();

    let seq = sequence(vec![track("V1", TrackKind::Video, vec![shot(id, 0, 15, fps), shot(id, 25, 40, fps)])], fps);
    let e = Engine::start(seq, pool_of(id, &src, fps));
    let (warm, cold) = klipp_engine::engine::gap_hand_overs();
    let (moves, stopped) = e.play_from(0.2, fps, Duration::from_secs(6));
    assert!(stopped && furthest(&moves) > 1.5, "played to {:.2} s", furthest(&moves));
    let (warm2, cold2) = klipp_engine::engine::gap_hand_overs();
    assert_eq!((warm2 - warm, cold2 - cold), (1, 0), "the shot after the gap came from its preroll, not a seek");
    // Taken over a tick late at most, and anchored where the sound is:
    // the playhead never steps back to the shot's start.
    let back = moves.windows(2).find(|w| w[1].1 < w[0].1 - 1e-6);
    assert!(back.is_none(), "the playhead stepped back: {back:?}");
    drop(e);

    let seq = sequence(vec![track("V1", TrackKind::Video, vec![shot(id, 0, 15, fps), shot(id, 16, 30, fps)])], fps);
    let e = Engine::start(seq, pool_of(id, &src, fps));
    let (moves, stopped) = e.play_from(0.2, fps, Duration::from_secs(6));
    assert!(stopped && furthest(&moves) > 1.1, "played to {:.2} s", furthest(&moves));
    assert_eq!(klipp_engine::engine::gap_hand_overs(), (warm2, cold2), "a frame's gap is cut across, never played as one");
}
