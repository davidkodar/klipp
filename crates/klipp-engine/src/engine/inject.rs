//! Panics a test injects into the running app (#3181), each once, over
//! the control socket's `inject_panic`: test hooks only, never in a
//! release build. Each says the guard it reaches is there and works.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Video decoders whose loop panics: any decoder's (`path` `None`), or the
/// one reading a file, so a test's panic is not taken by another test's
/// decoder; once, or each time it runs (a test's, until it disarms).
struct ArmedDecoder {
    path: Option<PathBuf>,
    every_time: bool,
    panics: u64,
}
static DECODERS: Mutex<Vec<ArmedDecoder>> = Mutex::new(Vec::new());
static DECODER_ARMED: AtomicBool = AtomicBool::new(false);
/// A mix thread that ends each time it starts (a test's, until it
/// disarms).
static MIX_STICKY: AtomicBool = AtomicBool::new(false);
/// Mix thread panics given out, for a test to count its own.
static MIX_PANICS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Frame renders still to panic (a test's), and the engine's, by its
/// frame buffer's address (0: any engine's), so a test's panic is not
/// taken by another test's engine.
static PRESENT: AtomicUsize = AtomicUsize::new(0);
static PRESENT_ENGINE: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn arm_present(engine: usize, frames: usize) {
    PRESENT_ENGINE.store(engine, Ordering::Release);
    PRESENT.store(frames, Ordering::Release);
}

/// Whether this frame's render, `engine`'s, is to panic, one fewer to go
/// if so.
pub(crate) fn present_panics(engine: usize) -> bool {
    let armed = PRESENT_ENGINE.load(Ordering::Acquire);
    (armed == 0 || armed == engine) && PRESENT.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1)).is_ok()
}

pub(crate) fn arm_decoder(path: Option<PathBuf>, every_time: bool) {
    let mut armed = DECODERS.lock().unwrap_or_else(|e| e.into_inner());
    armed.retain(|a| a.path != path);
    armed.push(ArmedDecoder { path, every_time, panics: 0 });
    DECODER_ARMED.store(true, Ordering::Release);
}

/// Whether the decoder reading `path` is to panic now; one armed once is
/// disarmed, one armed every time is counted.
pub(crate) fn decoder_panics(path: &Path) -> bool {
    if !DECODER_ARMED.load(Ordering::Acquire) {
        return false;
    }
    let mut armed = DECODERS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(i) = armed.iter().position(|a| a.path.as_deref().is_none_or(|p| p == path)) else { return false };
    if armed[i].every_time {
        armed[i].panics += 1;
    } else {
        armed.remove(i);
    }
    DECODER_ARMED.store(!armed.is_empty(), Ordering::Release);
    true
}

/// Whether a decoder of `path` is still armed, and how often one armed
/// every time has panicked.
#[cfg(test)]
fn armed_decoder(path: &Path) -> Option<u64> {
    DECODERS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|a| a.path.as_deref() == Some(path)).map(|a| a.panics)
}

#[cfg(test)]
fn disarm_decoder(path: &Path) {
    let mut armed = DECODERS.lock().unwrap_or_else(|e| e.into_inner());
    armed.retain(|a| a.path.as_deref() != Some(path));
    DECODER_ARMED.store(!armed.is_empty(), Ordering::Release);
}
/// The mix thread panics outside its block's catch, and ends: any mix
/// thread's (1), or the one mixing with a mixer, by its address (so a
/// test's panic is not taken by another test's thread). 0: none.
static MIX_THREAD: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn arm_mix_thread(mixer: Option<usize>) {
    MIX_THREAD.store(mixer.unwrap_or(1), Ordering::Release);
}

/// Whether the mix thread mixing with `mixer` is to panic now, disarmed
/// if so.
pub(crate) fn mix_thread_panics(mixer: usize) -> bool {
    let armed = MIX_THREAD.load(Ordering::Acquire);
    if armed != 1 && armed != mixer {
        return false;
    }
    let panics = MIX_STICKY.load(Ordering::Acquire) || MIX_THREAD.compare_exchange(armed, 0, Ordering::AcqRel, Ordering::Acquire).is_ok();
    if panics {
        MIX_PANICS.fetch_add(1, Ordering::Relaxed);
    }
    panics
}

#[cfg(feature = "test-hooks")]
impl super::PlaybackEngine {
    /// Arm the panic at `at`; said in the log.
    pub(crate) fn inject_panic(&mut self, at: &str) {
        match at {
            "job" => {
                klipp_jobs::ThreadJob::spawn("inject-panic", klipp_jobs::JobPriority::Background, klipp_jobs::ResourceClass::Cpu, |_, _| panic!("a job panicked (a test's)"));
            }
            "decoder" => arm_decoder(None, false),
            "present" => arm_present(0, 1),
            "mix_thread" => arm_mix_thread(None),
            "mix" => crate::mix_thread::lock_mixer(&self.mixer).panic_in_mix = true,
            "effect" => {
                let mut m = crate::mix_thread::lock_mixer(&self.mixer);
                let Some(track) = m.track_inserts.iter().position(Option::is_some) else {
                    tracing::warn!("inject_panic: no track has effects to panic");
                    return;
                };
                m.panic_in_inserts = Some(track);
            }
            "plugin" => klipp_frei0r::PANIC_NEXT_UPDATE.store(true, Ordering::Release),
            _ => {
                tracing::warn!("inject_panic: no place {at:?}");
                return;
            }
        }
        tracing::warn!("inject_panic: {at} armed");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, RwLock};
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use klipp_core::*;
    use klipp_media::{MediaItem, MediaPool};
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

    use crate::{FrameBuffer, PlaybackEngine};

    /// A 10-second 1080p25 test pattern, made once; `None` without ffmpeg.
    fn source(name: &str) -> Option<std::path::PathBuf> {
        let out = std::env::temp_dir().join(format!("klipp-decoder-panics-{name}-1080p25.mp4"));
        if out.exists() {
            return Some(out);
        }
        let partial = out.with_extension(format!("{}.mp4", std::process::id()));
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=25:d=10", "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
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

    /// A 10-second 1080p25 test pattern with a tone, made once; `None`
    /// without ffmpeg.
    fn source_with_sound() -> Option<std::path::PathBuf> {
        let out = std::env::temp_dir().join("klipp-mix-thread-panics-1080p25-tone.mp4");
        if out.exists() {
            return Some(out);
        }
        let partial = out.with_extension(format!("{}.mp4", std::process::id()));
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=25:d=10", "-f", "lavfi", "-i", "sine=frequency=440:duration=10:sample_rate=48000"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac", "2", "-shortest"])
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

    /// #3181: a mix thread that ends mid-playback (a panic past its
    /// block's catch) is made again while playing, with the sound from
    /// the playhead. It was made again only when a new sound started, and
    /// playing on stayed silent until then.
    #[test]
    fn a_mix_thread_that_ends_while_playing_is_made_again() {
        let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
        let Some(path) = source_with_sound() else {
            assert!(!required, "GPU required, and no ffmpeg to make the source");
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let fps = Fps::new(25, 1);
        let tc = |f: i64| RationalTime { value: f * fps.den as i64, rate: fps.num };
        let media = MediaId::new();
        let mut pool = MediaPool::default();
        let mut item = MediaItem::synthetic_for_test(path.clone(), None);
        item.id = media;
        item.fps = fps;
        item.duration = tc(250);
        item.sample_rate = Some(48_000);
        item.audio_channels = Some(2);
        pool.add(item);
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        seq.resolution = Resolution { width: 1920, height: 1080 };
        let clip = || Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0), source_out: tc(250), ..Clip::gap(tc(0), tc(250)) };
        let mut video = Track::new("V1", TrackKind::Video);
        video.clips = vec![clip()];
        let mut audio = Track::new("A1", TrackKind::Audio);
        audio.clips = vec![clip()];
        seq.tracks = vec![video, audio];

        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
        let gave_up = Arc::new(Mutex::new(Vec::<String>::new()));
        let plays = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (g, p) = (Arc::clone(&gave_up), Arc::clone(&plays));
        std::thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                match ev {
                    AppEvent::PlaybackGaveUp { what } => g.lock().unwrap().push(what),
                    AppEvent::PlaybackStarted => {
                        p.fetch_add(1, std::sync::atomic::Ordering::Release);
                    }
                    _ => {}
                }
            }
        });
        let fb = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(1920, 1080))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        let mixer = Arc::as_ptr(&engine.mixer) as usize;
        let thread = engine.spawn();
        cmd.send(AppCommand::SetPlayhead(tc(0))).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        cmd.send(AppCommand::Play).unwrap();
        std::thread::sleep(Duration::from_secs(1));

        let started = crate::mix_thread::mix_threads_started();
        super::arm_mix_thread(Some(mixer));
        let t0 = Instant::now();
        while super::MIX_THREAD.load(std::sync::atomic::Ordering::Acquire) != 0 && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(super::MIX_THREAD.load(std::sync::atomic::Ordering::Acquire), 0, "the mix thread did panic");
        let t0 = Instant::now();
        while crate::mix_thread::mix_threads_started() == started && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(crate::mix_thread::mix_threads_started() > started, "a mix thread is made again while playing");

        // One that ends every time it starts is made again three times,
        // then left until the person plays again.
        //
        // Waited on, not slept on (#3289): the Play forgives the panics
        // so far, and under load a Play the engine had not yet read when
        // the sticky panic was armed forgave the first of its restarts,
        // which made it again twice too often. So the Play is in hand —
        // this engine said it started, which it says after forgiving —
        // before the panic is armed, and the count is read once the person
        // was told it was left.
        let played = plays.load(std::sync::atomic::Ordering::Acquire);
        cmd.send(AppCommand::Pause).unwrap();
        cmd.send(AppCommand::Play).unwrap();
        let t0 = Instant::now();
        while plays.load(std::sync::atomic::Ordering::Acquire) == played && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(plays.load(std::sync::atomic::Ordering::Acquire) > played, "the Play was read");
        let panics = || super::MIX_PANICS.load(std::sync::atomic::Ordering::Relaxed);
        let before = panics();
        super::MIX_STICKY.store(true, std::sync::atomic::Ordering::Release);
        super::arm_mix_thread(Some(mixer));
        let t0 = Instant::now();
        while gave_up.lock().unwrap().is_empty() && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let at_two = panics();
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(panics(), at_two, "it stops being made again");
        let made = at_two - before;
        assert!(made >= 2 && made <= crate::engine::PANIC_RESTARTS as u64 + 1, "made again {made} times");
        assert_eq!(*gave_up.lock().unwrap(), ["The sound"], "and the person is told, once");
        super::MIX_STICKY.store(false, std::sync::atomic::Ordering::Release);
        super::MIX_THREAD.store(0, std::sync::atomic::Ordering::Release);
        cmd.send(AppCommand::Pause).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let started = crate::mix_thread::mix_threads_started();
        cmd.send(AppCommand::Play).unwrap();
        let t0 = Instant::now();
        while crate::mix_thread::mix_threads_started() == started && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(crate::mix_thread::mix_threads_started() > started, "playing again makes it again");
        let _ = cmd.send(AppCommand::Quit);
        let _ = thread.join();
        crate::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    }

    /// A frame whose render panics is left and the engine goes on: two
    /// parked frames panic in their render, and the next park is drawn.
    /// Before, a paused regrade that panicked (a layer of another size
    /// under a reflection, "mid > len") ended the engine thread, and the
    /// picture stayed frozen until the app was restarted.
    #[test]
    fn a_frame_whose_render_panics_is_left_and_the_engine_goes_on() {
        let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
        let Some(path) = source("present") else {
            assert!(!required, "GPU required, and no ffmpeg to make the source");
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let fps = Fps::new(25, 1);
        let tc = |f: i64| RationalTime { value: f * fps.den as i64, rate: fps.num };
        let media = MediaId::new();
        let mut pool = MediaPool::default();
        pool.add(MediaItem {
            id: media,
            path: path.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: 1920, height: 1080 },
            ..MediaItem::new_synthetic_title(media, path.display().to_string(), tc(250), fps)
        });
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        seq.resolution = Resolution { width: 1920, height: 1080 };
        let mut track = Track::new("V1", TrackKind::Video);
        track.clips = vec![Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0), source_out: tc(250), ..Clip::gap(tc(0), tc(250)) }];
        seq.tracks = vec![track];
        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
        std::thread::spawn(move || while ev_rx.recv().is_ok() {});
        let fb = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(1920, 1080))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        let thread = engine.spawn();
        let generation = || fb.lock().unwrap().latest_generation();
        let drawn_after = |g: u64, limit: Duration| {
            let t0 = Instant::now();
            while generation() == g && t0.elapsed() < limit {
                std::thread::sleep(Duration::from_millis(10));
            }
            generation() != g
        };
        let g = generation();
        cmd.send(AppCommand::SetPlayhead(tc(10))).unwrap();
        assert!(drawn_after(g, Duration::from_secs(30)), "the first park is drawn");
        std::thread::sleep(Duration::from_millis(300));

        super::arm_present(Arc::as_ptr(&fb) as usize, 2);
        cmd.send(AppCommand::SetPlayhead(tc(20))).unwrap();
        let t0 = Instant::now();
        while super::PRESENT.load(Ordering::Acquire) > 0 && t0.elapsed() < Duration::from_secs(10) {
            cmd.send(AppCommand::SetPlayhead(tc(20 + t0.elapsed().as_millis() as i64 / 100 % 5))).unwrap();
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(super::PRESENT.load(Ordering::Acquire), 0, "both frames' renders panicked");
        let g = generation();
        cmd.send(AppCommand::SetPlayhead(tc(40))).unwrap();
        assert!(drawn_after(g, Duration::from_secs(30)), "the engine draws the next park after frames whose render panicked");
        let _ = cmd.send(AppCommand::Quit);
        let _ = thread.join();
        crate::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    }

    /// #3181: a video decoder thread that panics mid-playback is caught
    /// by its guard, and the engine opens the file again where the
    /// playhead is: the playhead goes on and new frames are shown. It used
    /// to stop the picture, and every seek after it, until a restart.
    #[test]
    fn a_decoder_that_panics_while_playing_is_opened_again() {
        let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
        let Some(path) = source("reopen") else {
            assert!(!required, "GPU required, and no ffmpeg to make the source");
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let fps = Fps::new(25, 1);
        let tc = |f: i64| RationalTime { value: f * fps.den as i64, rate: fps.num };
        let media = MediaId::new();
        let mut pool = MediaPool::default();
        pool.add(MediaItem {
            id: media,
            path: path.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: 1920, height: 1080 },
            ..MediaItem::new_synthetic_title(media, path.display().to_string(), tc(250), fps)
        });
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        seq.resolution = Resolution { width: 1920, height: 1080 };
        let mut track = Track::new("V1", TrackKind::Video);
        track.clips = vec![Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0), source_out: tc(250), ..Clip::gap(tc(0), tc(250)) }];
        seq.tracks = vec![track];

        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
        let at = Arc::new(Mutex::new(0.0f64));
        let a = Arc::clone(&at);
        let gave_up = Arc::new(Mutex::new(Vec::<String>::new()));
        let g = Arc::clone(&gave_up);
        std::thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                match ev {
                    AppEvent::PlayheadMoved(t) => *a.lock().unwrap() = t.to_secs_f64(),
                    AppEvent::PlaybackGaveUp { what } => g.lock().unwrap().push(what),
                    _ => {}
                }
            }
        });
        let fb = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(1920, 1080))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        let thread = engine.spawn();
        let playhead = || *at.lock().unwrap();
        let wait_past = |secs: f64, limit: Duration| {
            let t0 = Instant::now();
            while playhead() < secs && t0.elapsed() < limit {
                std::thread::sleep(Duration::from_millis(10));
            }
            playhead()
        };
        cmd.send(AppCommand::SetPlayhead(tc(0))).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        cmd.send(AppCommand::Play).unwrap();
        assert!(wait_past(1.0, Duration::from_secs(10)) >= 1.0, "it plays");

        let opened = crate::video_decoder::decoders_opened();
        super::arm_decoder(Some(path.clone()), false);
        let t0 = Instant::now();
        while super::armed_decoder(&path).is_some() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(super::armed_decoder(&path).is_none(), "the decoder did panic");
        let then = playhead();
        let shown = fb.lock().unwrap().latest_generation();
        let reached = wait_past(then + 2.0, Duration::from_secs(10));
        assert!(reached >= then + 2.0, "the playhead goes on after the panic: {then:.2} s, then {reached:.2} s");
        // A second of frames, given time: a loaded machine shows them later.
        let t0 = Instant::now();
        while fb.lock().unwrap().latest_generation() <= shown + 25 && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(fb.lock().unwrap().latest_generation() > shown + 25, "and new frames are shown");
        assert!(crate::video_decoder::decoders_opened() > opened, "from a decoder opened again");

        // One that panics every time it runs is opened again three times,
        // then left; the playhead goes on, and playing again opens it.
        cmd.send(AppCommand::Pause).unwrap();
        cmd.send(AppCommand::Play).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let panics = || super::armed_decoder(&path).unwrap_or(0);
        super::arm_decoder(Some(path.clone()), true);
        let before = panics();
        let then = playhead();
        std::thread::sleep(Duration::from_secs(2));
        let at_two = panics();
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(panics(), at_two, "it stops being opened again");
        let reopened = at_two - before;
        assert!(reopened >= 2 && reopened <= crate::engine::PANIC_RESTARTS as u64 + 1, "opened {reopened} times");
        assert_eq!(*gave_up.lock().unwrap(), ["The picture"], "and the person is told, once");
        // A refresh's seek (an edit's, an undo's: SetPlayhead, not the
        // person's) does not open it again, nor say so again.
        let given_up_at = panics();
        for _ in 0..3 {
            cmd.send(AppCommand::SetPlayhead(tc((playhead() * 25.0) as i64))).unwrap();
            std::thread::sleep(Duration::from_millis(300));
        }
        assert_eq!(panics(), given_up_at, "a refresh's seek did not open it again");
        assert_eq!(gave_up.lock().unwrap().len(), 1, "nor say so again");

        assert!(playhead() > then + 1.0, "the playhead goes on: {then:.2} s, then {:.2} s", playhead());
        super::disarm_decoder(&path);
        cmd.send(AppCommand::Pause).unwrap();
        // Back at 1 s, a refresh's seek, which forgives nothing: on a
        // loaded machine the waits above can run the playhead to the
        // clip's end, where playing again has no frames to show.
        cmd.send(AppCommand::SetPlayhead(tc(25))).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let shown = fb.lock().unwrap().latest_generation();
        cmd.send(AppCommand::Play).unwrap();
        let t0 = Instant::now();
        while fb.lock().unwrap().latest_generation() <= shown + 25 && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(fb.lock().unwrap().latest_generation() > shown + 25, "playing again shows frames (playhead at {:.2} s)", playhead());
        let _ = cmd.send(AppCommand::Quit);
        let _ = thread.join();
        crate::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    }

    /// #3181: the decoder's tries are each clip's: a second clip whose
    /// decoder panics too gets its own three and is said of its own. They
    /// carried over, and the second clip got none and no word.
    #[test]
    fn a_second_clip_that_panics_gets_its_own_tries() {
        let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
        let (Some(a), Some(b)) = (source("first-clip"), source("second-clip")) else {
            assert!(!required, "GPU required, and no ffmpeg to make the sources");
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let fps = Fps::new(25, 1);
        let tc = |f: i64| RationalTime { value: f * fps.den as i64, rate: fps.num };
        let mut pool = MediaPool::default();
        let mut track = Track::new("V1", TrackKind::Video);
        for (i, path) in [&a, &b].into_iter().enumerate() {
            let media = MediaId::new();
            pool.add(MediaItem {
                id: media,
                path: path.clone(),
                kind: klipp_media::MediaItemKind::File,
                resolution: Resolution { width: 1920, height: 1080 },
                ..MediaItem::new_synthetic_title(media, path.display().to_string(), tc(250), fps)
            });
            let at = 125 * i as i64;
            track.clips.push(Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0), source_out: tc(125), ..Clip::gap(tc(at), tc(at + 125)) });
        }
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        seq.resolution = Resolution { width: 1920, height: 1080 };
        seq.tracks = vec![track];

        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
        let gave_up = Arc::new(Mutex::new(Vec::<String>::new()));
        let g = Arc::clone(&gave_up);
        std::thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                if let AppEvent::PlaybackGaveUp { what } = ev {
                    g.lock().unwrap().push(what);
                }
            }
        });
        let fb = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(1920, 1080))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        let thread = engine.spawn();
        let wait_for = |n: usize| {
            let t0 = Instant::now();
            while gave_up.lock().unwrap().len() < n && t0.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(20));
            }
            gave_up.lock().unwrap().len()
        };
        super::arm_decoder(Some(a.clone()), true);
        super::arm_decoder(Some(b.clone()), true);
        cmd.send(AppCommand::SetPlayhead(tc(0))).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        cmd.send(AppCommand::Play).unwrap();
        assert_eq!(wait_for(1), 1, "the first clip is left off, and said");
        let b_before = super::armed_decoder(&b).unwrap_or(0);
        // On to the second clip by a refresh's seek, which forgives
        // nothing: what it gets is the second clip's own.
        cmd.send(AppCommand::SetPlayhead(tc(150))).unwrap();
        assert_eq!(wait_for(2), 2, "the second clip is left off, and said of its own");
        let tries = super::armed_decoder(&b).unwrap_or(0) - b_before;
        assert!(tries >= 2, "the second clip had tries of its own: {tries}");
        super::disarm_decoder(&a);
        super::disarm_decoder(&b);
        let _ = cmd.send(AppCommand::Quit);
        let _ = thread.join();
        crate::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    }
}
