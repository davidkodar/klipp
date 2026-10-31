//! Panics a test injects into the running app (#3181), each once, over
//! the control socket's `inject_panic`: test hooks only, never in a
//! release build. Each says the guard it reaches is there and works.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// The next pass of a video decoder thread's loop panics: any decoder's
/// (`Some(None)`), or the one reading a file (`Some(Some(path))`, so a
/// test's panic is not taken by another test's decoder).
static DECODER: Mutex<Option<Option<PathBuf>>> = Mutex::new(None);
static DECODER_ARMED: AtomicBool = AtomicBool::new(false);

pub(crate) fn arm_decoder(path: Option<PathBuf>) {
    *DECODER.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
    DECODER_ARMED.store(true, Ordering::Release);
}

/// Whether the decoder reading `path` is to panic now, disarmed if so.
pub(crate) fn decoder_panics(path: &Path) -> bool {
    if !DECODER_ARMED.load(Ordering::Acquire) {
        return false;
    }
    let mut armed = DECODER.lock().unwrap_or_else(|e| e.into_inner());
    if armed.as_ref().is_some_and(|p| p.as_deref().is_none_or(|p| p == path)) {
        *armed = None;
        DECODER_ARMED.store(false, Ordering::Release);
        return true;
    }
    false
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
    (armed == 1 || armed == mixer) && MIX_THREAD.compare_exchange(armed, 0, Ordering::AcqRel, Ordering::Acquire).is_ok()
}

#[cfg(feature = "test-hooks")]
impl super::PlaybackEngine {
    /// Arm the panic at `at`; said in the log.
    pub(crate) fn inject_panic(&mut self, at: &str) {
        match at {
            "job" => {
                klipp_jobs::ThreadJob::spawn("inject-panic", klipp_jobs::JobPriority::Background, klipp_jobs::ResourceClass::Cpu, |_, _| panic!("a job panicked (a test's)"));
            }
            "decoder" => arm_decoder(None),
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
    use std::time::{Duration, Instant};

    use klipp_core::*;
    use klipp_media::{MediaItem, MediaPool};
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

    use crate::{FrameBuffer, PlaybackEngine};

    /// A 10-second 1080p25 test pattern, made once; `None` without ffmpeg.
    fn source() -> Option<std::path::PathBuf> {
        let out = std::env::temp_dir().join("klipp-decoder-panics-1080p25.mp4");
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
        std::thread::spawn(move || while ev_rx.recv().is_ok() {});
        let fb = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(1920, 1080))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        let mixer = Arc::as_ptr(&engine.mixer) as usize;
        let thread = std::thread::spawn(move || engine.run());
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
        let Some(path) = source() else {
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
        std::thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                if let AppEvent::PlayheadMoved(t) = ev {
                    *a.lock().unwrap() = t.to_secs_f64();
                }
            }
        });
        let fb = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(1920, 1080))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        let thread = std::thread::spawn(move || engine.run());
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
        super::arm_decoder(Some(path.clone()));
        let t0 = Instant::now();
        while super::DECODER_ARMED.load(std::sync::atomic::Ordering::Acquire) && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!super::DECODER_ARMED.load(std::sync::atomic::Ordering::Acquire), "the decoder did panic");
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
        let _ = cmd.send(AppCommand::Quit);
        let _ = thread.join();
        crate::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    }
}
