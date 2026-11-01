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
    thread: Option<klipp_engine::RunningEngine>,
    /// Every playhead the engine reported, and when.
    moves: Arc<Mutex<Vec<(Instant, f64)>>>,
    /// When each PlaybackPaused came.
    paused: Arc<Mutex<Vec<Instant>>>,
    /// How many playheads had been reported when each PlaybackStarted
    /// came: the engine's own order, which no clock reorders (#1806).
    started: Arc<Mutex<Vec<usize>>>,
    /// The frame the engine shows, to wait on a seek's frame.
    shown: Arc<Mutex<FrameBuffer>>,
}

impl Engine {
    fn start(seq: Sequence, pool: MediaPool) -> Self {
        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
        let moves = Arc::new(Mutex::new(Vec::new()));
        let paused = Arc::new(Mutex::new(Vec::new()));
        let started = Arc::new(Mutex::new(Vec::new()));
        let (m, p, st) = (Arc::clone(&moves), Arc::clone(&paused), Arc::clone(&started));
        std::thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                match ev {
                    AppEvent::PlayheadMoved(tc) => m.lock().unwrap().push((Instant::now(), tc.to_secs_f64())),
                    AppEvent::PlaybackPaused => p.lock().unwrap().push(Instant::now()),
                    AppEvent::PlaybackStarted => {
                        let n = m.lock().unwrap().len();
                        st.lock().unwrap().push(n);
                    }
                    _ => {}
                }
            }
        });
        let shown = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&shown), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
        );
        let thread = Some(engine.spawn());
        Engine { cmd, thread, moves, paused, started, shown }
    }

    /// Seek to `secs`, wait for it to land, then play; wait until playback
    /// pauses by itself or `limit` passes. Returns the playheads reported
    /// once playback had started, in the engine's order, and whether it
    /// paused by itself.
    ///
    /// Waited on, never timed: a loaded machine is slower, not wrong. The
    /// seek's own pause can come late; a pause counts only once the
    /// playhead has moved on from where playback started.
    fn play_from(&self, secs: f64, fps: Fps, limit: Duration) -> (Vec<(Instant, f64)>, bool) {
        let asked = Instant::now();
        self.cmd.send(AppCommand::SetPlayhead(RationalTime::from_secs_f64(secs, fps.rate()))).unwrap();
        let landed = |m: &[(Instant, f64)]| m.iter().any(|(t, p)| *t >= asked && (p - secs).abs() < 0.5 / fps.as_f64());
        while asked.elapsed() < Duration::from_secs(30) && !landed(&self.moves.lock().unwrap()) {
            std::thread::sleep(Duration::from_millis(5));
        }
        // And its frame on screen, whole: a seek reports again as its
        // frame arrives (#1806). This waited for 200 ms of quiet, a guess
        // a loaded machine could outlast.
        let on_screen = || {
            let fb = self.shown.lock().unwrap();
            fb.shown_presented().is_some_and(|at| (at.to_secs_f64() - secs).abs() < 0.5 / fps.as_f64()) && fb.latest_complete()
        };
        while asked.elapsed() < Duration::from_secs(30) && !on_screen() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(on_screen(), "the seek to {secs} s never put its frame on screen");
        let starts_before = self.started.lock().unwrap().len();
        let t0 = Instant::now();
        self.cmd.send(AppCommand::Play).unwrap();
        let advanced = |m: &[(Instant, f64)]| m.iter().find(|(t, p)| *t >= t0 && *p > secs + 0.5 / fps.as_f64()).map(|m| m.0);
        let stopped = || advanced(&self.moves.lock().unwrap()).is_some_and(|from| self.paused.lock().unwrap().iter().any(|p| *p >= from));
        while t0.elapsed() < limit && !stopped() {
            std::thread::sleep(Duration::from_millis(20));
        }
        let stopped = stopped();
        let _ = self.cmd.send(AppCommand::Pause);
        // From the engine's PlaybackStarted on, by its own order: a report
        // the seek sent before it but that was timed after Play (the
        // listener is a thread too) is not playback's (#1806).
        let from = self.started.lock().unwrap().get(starts_before).copied().unwrap_or(usize::MAX);
        let moves = self.moves.lock().unwrap().iter().skip(from).copied().collect();
        (moves, stopped)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.cmd.send(AppCommand::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
            // A decode thread still in ffmpeg's teardown at exit crashes the
            // process (CUDA's context destroyed under it).
            klipp_engine::video_decoder::wait_for_decode_threads(std::time::Duration::from_secs(10));
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
    let (moves, stopped) = e.play_from(0.2, fps, Duration::from_secs(60));
    assert!(stopped && furthest(&moves) > 1.5, "played to {:.2} s", furthest(&moves));
    let (warm2, cold2) = klipp_engine::engine::gap_hand_overs();
    assert_eq!((warm2 - warm, cold2 - cold), (1, 0), "the shot after the gap came from its preroll, not a seek");
    // Taken over a tick late at most, and anchored where the sound is:
    // the playhead never steps back, not even at the end (every report is
    // clamped to it, the stop's included).
    let back = moves.windows(2).position(|w| w[1].1 < w[0].1 - 1e-6);
    // Every report, ms from Play, so a failure says where (#1806).
    let reports = || {
        let t0 = moves.first().map(|m| m.0);
        moves.iter().map(|(t, p)| format!("{:.0} ms {p:.3}", t0.map_or(0.0, |s| t.duration_since(s).as_secs_f64() * 1000.0))).collect::<Vec<_>>()
    };
    assert!(back.is_none(), "the playhead stepped back after report {back:?}: {:?}", reports());
    drop(e);

    let seq = sequence(vec![track("V1", TrackKind::Video, vec![shot(id, 0, 15, fps), shot(id, 16, 30, fps)])], fps);
    let e = Engine::start(seq, pool_of(id, &src, fps));
    let (moves, stopped) = e.play_from(0.2, fps, Duration::from_secs(60));
    assert!(stopped && furthest(&moves) > 1.1, "played to {:.2} s", furthest(&moves));
    assert_eq!(klipp_engine::engine::gap_hand_overs(), (warm2, cold2), "a frame's gap is cut across, never played as one");
}
