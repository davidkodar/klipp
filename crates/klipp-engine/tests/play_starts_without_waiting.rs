//! Play waits for its first picture without holding the engine. The
//! clock and the sound start together once a frame near the playhead is
//! decoded, so the sound never runs ahead of the picture; meanwhile the
//! engine goes on ticking. At a slowed shot of a long-GOP 1440p file the
//! first frame took longer than the 1.5 s the engine used to wait for it
//! on its own thread, every press of Play.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

/// The engine's tick count is the process's: one play at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

struct Engine {
    cmd: crossbeam_channel::Sender<AppCommand>,
    thread: Option<klipp_engine::RunningEngine>,
    /// Every report, in the engine's order: playheads and starts.
    heard: Arc<Mutex<Vec<(Instant, Heard)>>>,
    shown: Arc<Mutex<FrameBuffer>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Heard {
    Moved(f64),
    Started,
}

impl Engine {
    fn start(seq: Sequence, pool: MediaPool, w: u32, h: u32) -> Self {
        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let hd = Arc::clone(&heard);
        std::thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                let h = match ev {
                    AppEvent::PlayheadMoved(tc) => Heard::Moved(tc.to_secs_f64()),
                    AppEvent::PlaybackStarted => Heard::Started,
                    _ => continue,
                };
                hd.lock().unwrap().push((Instant::now(), h));
            }
        });
        let shown = Arc::new(Mutex::new(FrameBuffer::new(w, h)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&shown), Arc::new(Mutex::new(FrameBuffer::new(w, h))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
        );
        Engine { cmd, thread: Some(engine.spawn()), heard, shown }
    }

    /// Wait for the frame at `secs` on screen, whole.
    fn wait_shown(&self, secs: f64, fps: f64) {
        let t0 = Instant::now();
        let on_screen = || {
            let fb = self.shown.lock().unwrap();
            fb.shown_presented().is_some_and(|at| (at.to_secs_f64() - secs).abs() < 0.5 / fps) && fb.latest_complete()
        };
        while t0.elapsed() < Duration::from_secs(30) && !on_screen() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(on_screen(), "the seek to {secs} s never put its frame on screen");
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.cmd.send(AppCommand::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
            klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
        }
    }
}

/// One play and pause at the clip's start, before what is measured: the
/// GPU's first-use costs (its pipelines, a fresh process's shader cache)
/// are the app's start-up's, not Play's.
fn warm_up(e: &Engine, fps: f64) {
    e.cmd.send(AppCommand::SetPlayhead(RationalTime::from_frames(0, fps.round() as u32))).unwrap();
    e.wait_shown(0.0, fps);
    let from = e.heard.lock().unwrap().len();
    e.cmd.send(AppCommand::Play).unwrap();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(20) && !e.heard.lock().unwrap().iter().skip(from).any(|(_, h)| *h == Heard::Started) {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    e.cmd.send(AppCommand::Pause).unwrap();
    std::thread::sleep(Duration::from_millis(100));
}

/// What one press of Play showed.
struct Played {
    /// The longest the engine went without a tick, from Play until
    /// playback started.
    longest_tick_gap: Duration,
    /// From Play to PlaybackStarted.
    start_after: Duration,
    /// The first playhead reported once playback started.
    first_report: f64,
    /// From the start (the clock and sound running) to the first picture
    /// near where Play was pressed on screen; zero when it was there
    /// before.
    picture_after_start: Duration,
    /// The playheads reported once it started, in order.
    reports: Vec<f64>,
}

/// When Play is pressed after the seek to where it plays from.
#[derive(Clone, Copy, PartialEq)]
enum After {
    /// The seek's frame is on screen.
    Shown,
    /// The seek has said where it landed, its frame still decoding: a
    /// paused seek that missed its frame.
    Landed,
    /// Right behind it, in the same batch.
    Sent,
}

/// Seek to `at`, press Play when `after` says, and watch the engine's
/// ticks and reports for `watch`.
fn play(e: &Engine, at: f64, fps: f64, after: After, watch: Duration) -> Played {
    let heard_before = e.heard.lock().unwrap().len();
    e.cmd.send(AppCommand::SetPlayhead(RationalTime::from_secs_f64(at, fps.round() as u32))).unwrap();
    match after {
        After::Shown => e.wait_shown(at, fps),
        After::Landed => {
            let t0 = Instant::now();
            let landed = || e.heard.lock().unwrap().iter().skip(heard_before).any(|(_, h)| matches!(h, Heard::Moved(p) if (p - at).abs() < 0.5 / fps));
            while t0.elapsed() < Duration::from_secs(30) && !landed() {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(landed(), "the seek to {at} s never said it landed");
        }
        After::Sent => {}
    }
    let from = e.heard.lock().unwrap().len().max(heard_before);
    let t0 = Instant::now();
    e.cmd.send(AppCommand::Play).unwrap();
    let started = || e.heard.lock().unwrap().iter().skip(from).any(|(_, h)| *h == Heard::Started);
    let mut longest = Duration::ZERO;
    let (mut last_tick, mut last_change) = (klipp_engine::ENGINE_TICK.load(std::sync::atomic::Ordering::Relaxed), Instant::now());
    while t0.elapsed() < Duration::from_secs(10) && !started() {
        let tick = klipp_engine::ENGINE_TICK.load(std::sync::atomic::Ordering::Relaxed);
        if tick != last_tick {
            longest = longest.max(last_change.elapsed());
            (last_tick, last_change) = (tick, Instant::now());
        }
        std::thread::sleep(Duration::from_micros(500));
    }
    longest = longest.max(last_change.elapsed());
    assert!(started(), "playback never started");
    let started_at = e.heard.lock().unwrap().iter().skip(from).find(|(_, h)| *h == Heard::Started).map(|(t, _)| *t).expect("started");
    let near = || e.shown.lock().unwrap().shown_presented().is_some_and(|p| (p.to_secs_f64() - at).abs() < 0.5);
    let t1 = Instant::now();
    while t1.elapsed() < Duration::from_secs(5) && !near() {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(near(), "the picture near {at} s never came");
    let picture_after_start = Instant::now().saturating_duration_since(started_at);
    std::thread::sleep(watch);
    let _ = e.cmd.send(AppCommand::Pause);
    let heard = e.heard.lock().unwrap().clone();
    let at_start = heard.iter().skip(from).position(|(_, h)| *h == Heard::Started).expect("started") + from;
    let start_after = heard[at_start].0.duration_since(t0);
    let reports: Vec<f64> = heard[at_start..].iter().filter_map(|(_, h)| if let Heard::Moved(p) = h { Some(*p) } else { None }).collect();
    Played { longest_tick_gap: longest, start_after, first_report: *reports.first().expect("a report after the start"), picture_after_start, reports }
}

fn assert_started_well(p: &Played, at: f64, what: &str) {
    assert!(
        p.longest_tick_gap < Duration::from_millis(200),
        "{what}: the engine stood still {:?} while Play waited for its first picture (started after {:?})",
        p.longest_tick_gap, p.start_after,
    );
    // The clock and the sound start with the picture, not before it: the
    // picture is up within a few ticks of the start.
    assert!(
        p.picture_after_start < Duration::from_millis(150),
        "{what}: the clock and sound started {:?} before the first picture: the sound ran ahead of it",
        p.picture_after_start,
    );
    // And the first report is where Play was pressed, not as far on as
    // the wait was long.
    assert!(
        (p.first_report - at).abs() < 0.25,
        "{what}: the first report after the start was {:.3} s, Play was at {at:.3} s: the clock ran ahead of the picture",
        p.first_report,
    );
    let back = p.reports.windows(2).position(|w| w[1] < w[0] - 1e-6);
    assert!(back.is_none(), "{what}: the playhead stepped back after report {back:?}: {:?}", p.reports);
}

/// `secs` of 1440p H.264 at 62.5 fps with a keyframe every 8 s, as a
/// camera's slow-motion file: the first frame after a seek late in a
/// group of pictures is a long decode away.
fn long_gop_slowmo() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-play-gate-slowmo-1440p.mp4");
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=2560x1440:r=125/2:d=10"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-g", "500", "-keyint_min", "500", "-sc_threshold", "0"])
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

fn item(id: MediaId, path: &std::path::Path, w: u32, h: u32, fps: Fps, secs: f64) -> MediaItem {
    MediaItem {
        id,
        path: path.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: w, height: h },
        ..MediaItem::new_synthetic_title(id, path.display().to_string(), RationalTime::from_secs_f64(secs, fps.num), fps)
    }
}

fn one_clip(clip: Clip, w: u32, h: u32, fps: Fps) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: w, height: h };
    let mut t = Track::new("V1", TrackKind::Video);
    t.clips = vec![clip];
    seq.tracks = vec![t];
    seq
}

/// A slowed long-GOP shot, played from late in its group of pictures as
/// soon as the seek there has landed, its frame still decoding (a paused
/// seek misses it): the engine ticks on while it decodes, and the clock
/// and sound start with it.
#[test]
fn play_at_a_slowed_long_gop_shot_does_not_hold_the_engine() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(src) = long_gop_slowmo() else {
        eprintln!("skipping: no ffmpeg with libx264");
        return;
    };
    let fps = Fps::new(24, 1);
    let (w, h) = (2560, 1440);
    let id = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(item(id, &src, w, h, Fps::new(125, 2), 10.0));
    // 9.9 s of source at 2.05×: 4.83 s on the timeline.
    let tc = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut clip = Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0.0), source_out: tc(9.9), ..Clip::gap(tc(0.0), tc(9.9 / 2.05)) };
    clip.speed = 2.05;
    let e = Engine::start(one_clip(clip, w, h, fps), pool, w, h);
    warm_up(&e, 24.0);
    // Source 7.8 s: 7.8 s past the only earlier keyframe.
    let at = (7.8 / 2.05 * 24.0_f64).round() / 24.0;
    let p = play(&e, at, 24.0, After::Landed, Duration::from_millis(400));
    assert_started_well(&p, at, "slowed long-GOP shot");
}

/// A plain all-intra clip: Play starts at once, from where it was.
#[test]
fn play_at_a_normal_clip_starts_where_it_was() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(src) = common::ffv1_avi_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let (w, h) = (common::RAMP_W, common::RAMP_H);
    let id = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(item(id, &src, w, h, fps, common::RAMP_N as f64 / common::FPS as f64));
    let ftc = |f| common::frame_tc(f, fps);
    let clip = Clip { kind: ClipKind::Media { media_id: id }, source_in: ftc(0), source_out: ftc(40), ..Clip::gap(ftc(0), ftc(40)) };
    let e = Engine::start(one_clip(clip, w, h, fps), pool, w, h);
    warm_up(&e, common::FPS as f64);
    let p = play(&e, 0.2, common::FPS as f64, After::Shown, Duration::from_millis(300));
    assert_started_well(&p, 0.2, "normal clip");
}

/// Play sent right behind a seek, before its frame is on screen: the
/// seek's own decode is the first picture, and the start waits for it
/// without holding the engine.
#[test]
fn play_right_after_a_seek_starts_at_the_seek() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(src) = long_gop_slowmo() else {
        eprintln!("skipping: no ffmpeg with libx264");
        return;
    };
    let fps = Fps::new(24, 1);
    let (w, h) = (2560, 1440);
    let id = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(item(id, &src, w, h, Fps::new(125, 2), 10.0));
    let tc = |s: f64| RationalTime::from_secs_f64(s, 24);
    let clip = Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0.0), source_out: tc(9.9), ..Clip::gap(tc(0.0), tc(9.9)) };
    let e = Engine::start(one_clip(clip, w, h, fps), pool, w, h);
    // Warmed and parked at the start, so Play is from a paused engine.
    warm_up(&e, 24.0);
    let at = 7.5;
    let p = play(&e, at, 24.0, After::Sent, Duration::from_millis(400));
    assert_started_well(&p, at, "play right after a seek");
}
