//! Playback runs to the end of the timeline, not to the end of the last
//! shot. An end card on a text track after the last picture clip, or
//! music running on, was never reached: the engine followed the picture
//! track, and with no next clip there it stopped (#542).

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, TitleData, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;

struct Engine {
    cmd: crossbeam_channel::Sender<AppCommand>,
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
            Arc::new(RwLock::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
        );
        std::thread::spawn(move || engine.run());
        Engine { cmd, moves, paused }
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

/// A shot to 0.8 s, and an end card on a text track to 2.0 s: playback
/// shows the card to its end and stops there.
#[test]
fn playback_runs_on_past_the_last_shot_to_the_end_card() {
    let Some(src) = common::ffv1_avi_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let id = MediaId::new();
    let tc = |f| common::frame_tc(f, fps);
    let card = Clip { kind: ClipKind::Title(TitleData::default()), ..Clip::gap(tc(10), tc(50)) };
    let seq = sequence(vec![track("Text 1", TrackKind::Text, vec![card]), track("V1", TrackKind::Video, vec![shot(id, 0, 20, fps)])], fps);
    let e = Engine::start(seq, pool_of(id, &src, fps));

    let (moves, stopped) = e.play_from(0.2, fps, Duration::from_secs(6));
    let reached = furthest(&moves);
    assert!(reached > 1.9, "playback stopped at {reached:.2} s, before the end card's end at 2.0 s");
    assert!(stopped, "playback did not stop at the end of the timeline");
    assert!(reached <= 2.0 + 1e-6, "played past the end: {reached:.2} s");

    // Started inside the card, after the last shot: it plays from there.
    let (moves, stopped) = e.play_from(1.2, fps, Duration::from_secs(4));
    let reached = furthest(&moves);
    assert!(reached > 1.9, "from 1.2 s playback reached only {reached:.2} s");
    assert!(stopped, "from 1.2 s playback did not stop at the end");
}

/// Two shots with an empty stretch between them: the stretch is played
/// through in its own time, and the second shot takes over after it.
#[test]
fn a_gap_between_shots_is_played_through() {
    let Some(src) = common::ffv1_avi_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let id = MediaId::new();
    let seq = sequence(vec![track("V1", TrackKind::Video, vec![shot(id, 0, 15, fps), shot(id, 25, 40, fps)])], fps);
    let e = Engine::start(seq, pool_of(id, &src, fps));

    let (moves, stopped) = e.play_from(0.2, fps, Duration::from_secs(6));
    assert!(stopped, "playback did not stop at the end");
    assert!(furthest(&moves) > 1.5, "the second shot was not played: reached {:.2} s", furthest(&moves));
    // In the gap (0.6 s to 1.0 s) the playhead moves through it rather
    // than jumping from its start to its end.
    let in_gap: Vec<_> = moves.iter().filter(|m| m.1 > 0.65 && m.1 < 0.95).collect();
    assert!(!in_gap.is_empty(), "the playhead jumped over the gap: {:?}", moves.iter().map(|m| (m.1 * 100.0).round() / 100.0).collect::<Vec<_>>());
    let spent = in_gap.last().unwrap().0.duration_since(in_gap.first().unwrap().0);
    assert!(spent > Duration::from_millis(150), "the gap went by in {spent:?}");
}

/// After the last shot on V1, a picture on V2 (an image, a closing
/// shot on its own track) takes over when the playhead reaches it and
/// plays to its end.
#[test]
fn a_picture_on_another_track_after_the_last_shot_is_played() {
    let Some(src) = common::ffv1_avi_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let id = MediaId::new();
    let seq = sequence(vec![track("V2", TrackKind::Video, vec![shot(id, 25, 45, fps)]), track("V1", TrackKind::Video, vec![shot(id, 0, 15, fps)])], fps);
    let e = Engine::start(seq, pool_of(id, &src, fps));

    let (moves, stopped) = e.play_from(0.2, fps, Duration::from_secs(6));
    let reached = furthest(&moves);
    assert!(reached > 1.7, "the picture on V2 (1.0-1.8 s) was not played: reached {reached:.2} s");
    assert!(stopped, "playback did not stop at the end");
}

/// Looping over a timeline that ends on an end card: each pass runs to
/// the card's end and wraps back to the shot, which plays again. The
/// loop leaves clock-only for the shot it lands on.
#[test]
fn a_loop_through_the_end_card_wraps_back_to_the_shot() {
    let Some(src) = common::ffv1_avi_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let id = MediaId::new();
    let tc = |f| common::frame_tc(f, fps);
    let card = Clip { kind: ClipKind::Title(TitleData::default()), ..Clip::gap(tc(10), tc(50)) };
    let seq = sequence(vec![track("Text 1", TrackKind::Text, vec![card]), track("V1", TrackKind::Video, vec![shot(id, 0, 20, fps)])], fps);
    let e = Engine::start(seq, pool_of(id, &src, fps));
    e.cmd.send(AppCommand::SetLoopPlayback(true)).unwrap();

    let (moves, stopped) = e.play_from(0.2, fps, Duration::from_secs(6));
    assert!(!stopped, "a looping playback paused");
    // Count the wraps: the playhead falling back from the card's end to
    // the shot at the start.
    let wraps = moves.windows(2).filter(|w| w[0].1 > 1.8 && w[1].1 < 0.4).count();
    assert!(wraps >= 2, "{wraps} wraps in 6 s of a 2 s loop");
    // After a wrap the shot plays: the playhead moves on through it.
    let after_wrap = moves.windows(2).position(|w| w[0].1 > 1.8 && w[1].1 < 0.4).unwrap();
    assert!(moves[after_wrap + 1..].iter().any(|m| m.1 > 0.5 && m.1 < 0.8), "the shot did not play after the wrap");
}
