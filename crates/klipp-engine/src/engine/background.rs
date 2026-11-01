//! Heavy frames drawn ahead while the person is idle, on the engine's
//! side (#3212): when to start and stop, and what to draw. See
//! [`crate::background_render`].

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use klipp_jobs::BackgroundJob;
use klipp_timeline::Sequence;

use super::{PlaybackEngine, PlaybackState};
use crate::background_render::{self as bg, Decision, State};
use crate::frame_cache::{FrameRequest, FramesTally};

/// How often the power supplies are read.
const POWER_EVERY: Duration = Duration::from_secs(30);

/// A run drawing heavy frames ahead.
pub(crate) struct Run {
    job: Arc<klipp_jobs::ThreadJob>,
    done: std::sync::mpsc::Receiver<FramesTally>,
    stop: Arc<AtomicBool>,
    total: usize,
}

/// A run let go is stopped and its thread waited for, the frame in hand
/// finished: an engine that has stopped leaves no job drawing (#3287).
impl Drop for Run {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.job.cancel();
        self.job.join();
    }
}

/// Drawing ahead's state on the engine.
pub(crate) struct Background {
    pub on: bool,
    pub delay: Duration,
    run: Option<Run>,
    /// The last input, edit or seek the engine saw.
    activity: Instant,
    seen: Option<(klipp_timeline::SequenceRevision, i64)>,
    battery: Option<(Instant, bool)>,
    /// What a run found the last time nothing changed since: every heavy
    /// frame drawn, or the cache full at that many bytes.
    finished: Option<Finished>,
    /// Input stopped the last run; it starts again after the idle time.
    stopped: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct Finished {
    revision: klipp_timeline::SequenceRevision,
    costs: u64,
    full_at: Option<u64>,
}

impl Default for Background {
    fn default() -> Self {
        Background { on: false, delay: bg::DEFAULT_DELAY, run: None, activity: Instant::now(), seen: None, battery: None, finished: None, stopped: false }
    }
}

/// Where the computer's power supplies are read.
fn power_root() -> std::path::PathBuf {
    std::path::PathBuf::from("/sys/class/power_supply")
}

impl PlaybackEngine {
    /// Record what the frame just shown cost, while playing: a frame from
    /// the frame cache costs nothing.
    ///
    /// A frame's cost is its whole time on the engine's thread, live or
    /// from the cache, so the two compare: a kept frame's goes to
    /// [`Self::kept_cost_ms`], not the log.
    pub(super) fn record_frame_cost(&mut self, total_ms: f32) {
        let from_cache = std::mem::take(&mut self.frame_from_cache);
        if self.state != PlaybackState::Playing {
            return;
        }
        if from_cache {
            let k = 1.0 / (self.kept_shown.min(29) + 1) as f32;
            self.kept_present_ms += (total_ms - self.kept_present_ms) * k;
            self.kept_shown += 1;
            return;
        }
        let fps = self.fps.as_f64();
        if fps <= 0.0 {
            return;
        }
        let rate = self.fps.rate();
        let step = i64::from(self.fps.den).max(1);
        let tick = self.playhead.rescale_to(rate).value.div_euclid(step) * step;
        self.cost_log.record(tick, total_ms, (1000.0 / fps) as f32);
    }

    /// What showing a kept frame costs, ms, once measured: its time on
    /// the engine's thread, or the reading thread's per frame where that
    /// is longer (it can keep only so far ahead).
    pub(super) fn kept_cost_ms(&self) -> Option<f32> {
        let read = crate::frame_cache::load_costs();
        let shown = (self.kept_shown > 0).then_some(self.kept_present_ms);
        let read = (read.frames > 0).then_some(read.total_ms);
        match (shown, read) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// The frames to draw ahead: those whose last playback was late, and
    /// every frame of a track whose measured cost is over half a frame's
    /// time, at the sequence's rate.
    pub(super) fn heavy_ticks(&self, seq: &Sequence) -> Vec<i64> {
        let rate = seq.fps.rate();
        let step = i64::from(seq.fps.den).max(1);
        let half = (500.0 / seq.fps.as_f64()) as f32;
        // Only where loading is quicker than drawing live, once known.
        let load = self.kept_cost_ms();
        let quicker = |ms: f32| load.is_none_or(|load| ms > load);
        let mut ticks: BTreeSet<i64> = self.cost_log.late().filter(|t| self.cost_log.get(*t).is_some_and(|c| quicker(c.ms))).collect();
        let forced = bg::forced_heavy();
        for track in seq.tracks.iter().filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text) && t.visible) {
            let ms = self.measured_once.get(&track.id).copied().or_else(|| crate::track_cost::cost(track.id).and_then(|c| c.gpu_ms));
            if !forced && ms.is_none_or(|ms| ms <= half || !quicker(ms)) {
                continue;
            }
            for clip in &track.clips {
                let (a, b) = (clip.timeline_in.rescale_to(rate).value, clip.timeline_out.rescale_to(rate).value);
                ticks.extend(super::frames_job::frames_between(a, b, step, rate).into_iter().map(|t| t.value));
            }
        }
        ticks.into_iter().collect()
    }

    /// Whether the frame at `tick` is cheap already: nothing on it, or
    /// every track on it frozen and its frozen frames current.
    fn frozen_or_empty(seq: &Sequence, tick: i64) -> bool {
        let at = klipp_core::RationalTime { value: tick, rate: seq.fps.rate() };
        let layers = klipp_compose::active_layers(seq, at);
        layers.iter().all(|l| {
            let Some(track) = seq.tracks.get(l.track).filter(|t| t.frozen) else { return false };
            crate::freeze::status(track.id).is_some_and(|s| {
                s.state() == crate::freeze::FreezeState::Current && !s.live.iter().chain(&s.stale).any(|(a, b)| (a.value..b.value).contains(&at.rescale_to(a.rate).value))
            })
        })
    }

    fn on_battery(&mut self) -> bool {
        match self.background.battery {
            Some((at, on)) if at.elapsed() < POWER_EVERY => on,
            _ => {
                let on = bg::on_battery(&power_root());
                self.background.battery = Some((Instant::now(), on));
                on
            }
        }
    }

    /// A tick of drawing ahead: started once the person has been idle for
    /// the delay, stopped on anything else, its end taken.
    pub(super) fn tick_background(&mut self) {
        let (seq, revision) = self.sequence.snapshot_with_revision();
        let playhead = self.playhead.rescale_to(seq.fps.rate()).value;
        if self.background.seen != Some((revision, playhead)) {
            self.background.activity = Instant::now();
            self.background.seen = Some((revision, playhead));
        }
        let mut since = self.background.activity.elapsed();
        let mut scrubbing = self.is_scrubbing;
        let mut playing = self.state == PlaybackState::Playing || self.source_state == PlaybackState::Playing;
        if let Some(fg) = &self.foreground_activity {
            since = since.min(fg.time_since_input());
            scrubbing |= fg.is_scrubbing();
            playing |= fg.is_playing();
        }
        let moment = bg::Moment {
            on: self.background.on,
            cache_on: self.frames_enabled,
            battery: self.background.on && self.on_battery(),
            playing,
            scrubbing,
            exporting: klipp_render::export_job::exports_running() > 0,
            freezing: crate::freeze::any_drawing(),
            rendering_range: self.frames_job.is_some(),
            since_activity: since,
            delay: self.background.delay,
        };
        let decision = bg::decide(&moment);
        let costs = self.cost_log.generation();
        if let Some(run) = &self.background.run {
            if let Ok(t) = run.done.try_recv() {
                let bytes = self.frames_store().lock().unwrap_or_else(|e| e.into_inner()).bytes(crate::frame_cache::NAMESPACE);
                if t.cancelled {
                    // Stopped between frames by the person's input, or
                    // by the engine: either way it starts again idle.
                    self.background.stopped = true;
                } else {
                    self.background.finished = Some(Finished { revision, costs, full_at: t.full.then_some(bytes) });
                }
                tracing::info!("Drawing ahead: {} drawn, {} current, {} failed{}{}", t.stored, t.current, t.failed, if t.full { ", the cache full" } else { "" }, if t.cancelled { ", stopped" } else { "" });
                self.background.run = None;
                self.update_render_cache_ranges();
            } else if decision != Decision::Run {
                run.stop.store(true, Ordering::Relaxed);
                run.job.cancel();
                self.background.stopped = matches!(decision, Decision::Busy | Decision::Waiting);
                // Its end is taken on a later tick, the frame in hand kept.
            } else {
                let done = (run.job.progress() * run.total as f32).round() as usize;
                bg::set_state(State::Drawing { done, total: run.total });
                return;
            }
        }
        if self.background.run.is_some() {
            bg::set_state(State::Stopped);
            return;
        }
        let bytes = || self.frames_store().lock().unwrap_or_else(|e| e.into_inner()).bytes(crate::frame_cache::NAMESPACE);
        let finished = self.background.finished.filter(|f| f.revision == revision && f.costs == costs && f.full_at.is_none_or(|at| bytes() >= at));
        let state = match (decision, finished) {
            (Decision::Off, _) => State::Off,
            (Decision::OnBattery, _) => State::OnBattery,
            (_, Some(f)) if f.full_at.is_some() => State::Full,
            (_, Some(_)) => State::Done,
            (Decision::Run, None) => {
                self.start_background(&seq, playhead, revision, costs);
                return;
            }
            _ if self.background.stopped => State::Stopped,
            _ => State::Waiting,
        };
        bg::set_state(state);
    }

    fn start_background(&mut self, seq: &Arc<Sequence>, playhead: i64, revision: klipp_timeline::SequenceRevision, costs: u64) {
        let step = i64::from(seq.fps.den).max(1);
        let heavy = self.heavy_ticks(seq);
        let ticks = bg::plan(&heavy, step, playhead, |t| Self::frozen_or_empty(seq, t));
        if ticks.is_empty() {
            self.background.finished = Some(Finished { revision, costs, full_at: None });
            bg::set_state(State::Done);
            return;
        }
        let rate = seq.fps.rate();
        let frames: Vec<_> = ticks.into_iter().map(|value| klipp_core::RationalTime { value, rate }).collect();
        let full = super::view::playback_canvas(None, (seq.resolution.width, seq.resolution.height));
        let sizes = super::view::prerender_sizes(full, crate::video_decoder::playback_quality_divisor()).into_iter().take(1).collect();
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner()).clone();
        let files: Arc<std::collections::HashMap<klipp_core::MediaId, klipp_render::MediaFile>> = Arc::new(pool.all().map(|i| (i.id, klipp_render::MediaFile::of(i))).collect());
        let working = crate::video_decoder::working_colorspace();
        let fps = seq.fps.as_f64();
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, fg, started) = (Arc::clone(&stop), self.foreground_activity.clone(), Instant::now());
        let asks: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || flag.load(Ordering::Relaxed) || fg.as_ref().is_some_and(|f| f.is_playing() || f.is_scrubbing() || f.time_since_input() < started.elapsed()));
        let total = frames.len();
        let req = FrameRequest { seq: Arc::clone(seq), frames, sizes, working, draw: self.draw_settings(), keying: self.frame_keying(), stop: Some(asks), evict: false };
        let (tx, done) = std::sync::mpsc::channel();
        let job = crate::frame_cache::spawn_frames(req, self.frames_store(), move || klipp_render::ExportFrameSource::shared(files, fps, working), move |t| {
            let _ = tx.send(t);
        });
        tracing::info!("Drawing ahead: {total} heavy frame(s), nearest the playhead first");
        self.background.stopped = false;
        self.background.run = Some(Run { job, done, stop, total });
        bg::set_state(State::Drawing { done: 0, total });
    }

    /// Stop drawing ahead now: a range asked for, or the cache cleared.
    pub(super) fn stop_background(&mut self) {
        if let Some(run) = self.background.run.take() {
            run.stop.store(true, Ordering::Relaxed);
            run.job.cancel();
        }
        self.background.finished = None;
    }

    pub(super) fn set_background_render(&mut self, on: bool, delay: Duration) {
        bg::set_summary(|m| {
            m.on = on;
            m.delay_secs = delay.as_secs_f32();
        });
        // Sent on every preferences save: said only when it changes.
        if (on, delay) != (self.background.on, self.background.delay) {
            tracing::info!("Drawing ahead while idle: {} after {:.1} s", if on { "on" } else { "off" }, delay.as_secs_f64());
        }
        self.background.on = on;
        self.background.delay = delay;
        if !on {
            self.stop_background();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::{Duration, Instant};

    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Track, TrackKind};

    use super::*;

    fn tc(frames: i64) -> klipp_core::Timecode {
        RationalTime::from_frames(frames, 24)
    }

    /// An engine, not running, on `tracks` at 24 fps.
    fn engine(tracks: Vec<Track>) -> PlaybackEngine {
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::film_24();
        seq.tracks = tracks;
        let (_cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        )
    }

    fn track(name: &str, from: i64, to: i64) -> Track {
        let mut t = Track::new(name, TrackKind::Video);
        t.clips = vec![Clip::media(MediaId::new(), tc(from), tc(to), tc(0), tc(to - from))];
        t
    }

    /// A frame played late is recorded at its frame, one on time is not
    /// heavy, and a frame shown from the cache or while parked isn't
    /// recorded at all.
    #[test]
    fn played_frames_record_their_cost_and_cached_ones_do_not() {
        let mut e = engine(vec![track("V1", 0, 48)]);
        e.state = PlaybackState::Playing;
        e.playhead = tc(5);
        e.record_frame_cost(60.0);
        e.playhead = tc(6);
        e.record_frame_cost(10.0);
        assert_eq!(e.cost_log.late().collect::<Vec<_>>(), vec![tc(5).value], "late at 24 fps is over 41.7 ms");
        assert!(e.cost_log.get(tc(6).value).is_some_and(|c| !c.late));
        e.playhead = tc(7);
        e.frame_from_cache = true;
        e.record_frame_cost(60.0);
        assert!(e.cost_log.get(tc(7).value).is_none(), "from the cache: costs nothing");
        assert!(!e.frame_from_cache, "the mark is taken");
        assert_eq!((e.kept_shown, e.kept_present_ms), (1, 60.0), "its time goes to the kept frames' cost");
        e.state = PlaybackState::Paused;
        e.playhead = tc(8);
        e.record_frame_cost(60.0);
        assert!(e.cost_log.get(tc(8).value).is_none(), "parked: not playback's cost");
    }

    /// A kept frame is handed over once the reading thread has it, never
    /// read on the engine's thread; and not at all where its frame drew
    /// live quicker than a kept frame loads.
    #[test]
    fn a_kept_frame_is_taken_when_read_and_only_where_loading_beats_live() {
        let mut e = engine(vec![track("V1", 0, 24)]);
        let root = std::env::temp_dir().join(format!("klipp-kept-take-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        e.freeze_folder = Some(root.clone());
        let seq = e.sequence.snapshot();
        let size = (16, 8);
        let at = e.frame_keying().place(&seq, tc(3), size);
        let pixels: Vec<f32> = (0..16 * 8).flat_map(|i| [i as f32 / 128.0, 0.5, 0.25, 1.0]).collect();
        e.frames_store().lock().unwrap().put_frame(crate::frame_cache::NAMESPACE, at, &pixels).unwrap();
        let take = |e: &mut PlaybackEngine| {
            let start = Instant::now();
            loop {
                if let Some(bytes) = e.kept_frame(&seq, tc(3), size) {
                    return Some(bytes);
                }
                if start.elapsed() > Duration::from_secs(2) {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        assert_eq!(take(&mut e).map(|b| b.len()), Some(16 * 8 * 4), "read ahead, then handed over");
        e.cost_log.record(tc(3).value, 5.0, 41.7);
        e.kept_shown = 1;
        e.kept_present_ms = 20.0;
        assert!(take(&mut e).is_none(), "drawn live in 5 ms, a kept frame costs 20: live");
        e.cost_log.record(tc(3).value, 50.0, 41.7);
        assert!(take(&mut e).is_some(), "drawn live in 50 ms: the kept frame");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Heavy: the frames played late, and every frame of a track measured
    /// at over half a frame's time; a track measured under it is not.
    #[test]
    fn heavy_frames_are_the_late_ones_and_a_costly_tracks() {
        let (v1, v2) = (track("V1", 0, 4), track("V2", 10, 13));
        let (light, costly) = (v1.id, v2.id);
        let mut e = engine(vec![v1, v2]);
        e.measured_once.insert(light, 15.0);
        e.measured_once.insert(costly, 25.0);
        e.cost_log.record(tc(2).value, 60.0, 41.7);
        let seq = e.sequence.snapshot();
        assert_eq!(e.heavy_ticks(&seq), [2, 10, 11, 12].map(|f| tc(f).value).to_vec());
        e.measured_once.insert(costly, 20.0);
        assert_eq!(e.heavy_ticks(&seq), vec![tc(2).value], "20 ms is under half of 41.7");
    }

    /// Drawing ahead leaves out a late frame, or a costly track's, where
    /// loading a kept frame costs as much as drawing it live: there the
    /// cache would only slow playback down.
    #[test]
    fn a_frame_quicker_live_than_loaded_is_not_drawn_ahead() {
        let v1 = track("V1", 10, 12);
        let costly = v1.id;
        let mut e = engine(vec![v1]);
        e.measured_once.insert(costly, 30.0);
        e.cost_log.record(tc(2).value, 50.0, 41.7);
        e.cost_log.record(tc(3).value, 90.0, 41.7);
        let seq = e.sequence.snapshot();
        assert_eq!(e.heavy_ticks(&seq), [2, 3, 10, 11].map(|f| tc(f).value).to_vec(), "loading not measured yet");
        e.kept_shown = 1;
        e.kept_present_ms = 60.0;
        assert!(e.kept_cost_ms().is_some_and(|c| c >= 60.0), "the kept frames shown, or the reading thread where slower");
        assert_eq!(e.heavy_ticks(&seq), vec![tc(3).value], "only the frame dearer live than the 60 ms a kept one costs");
    }

    /// An engine on its own thread stops and is joined when its handle is
    /// let go or joined, with no Quit sent (#3287): a test that fails
    /// before its Quit leaves no engine running past its end.
    #[test]
    fn a_running_engine_let_go_stops_and_is_joined() {
        // Its command sender held, as an owner's is: the loop runs on.
        let live = || {
            let (cmd, cmd_rx) = crossbeam_channel::unbounded();
            let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
            let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
            let e = PlaybackEngine::new(
                cmd_rx, ev_tx, fb(), fb(),
                Arc::new(klipp_timeline::SharedSequence::new(Sequence::new_default())), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
                Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
            );
            (cmd, e.spawn())
        };
        let start = Instant::now();
        let (_cmd, running) = live();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!running.is_finished(), "running until let go");
        assert!(running.join().is_ok(), "joined, no Quit sent");
        assert!(start.elapsed() < Duration::from_secs(5), "promptly");
        let (_cmd, running) = live();
        let stop = Arc::clone(&running.stop);
        let started = Instant::now();
        drop(running);
        assert!(stop.load(Ordering::Acquire) && started.elapsed() < Duration::from_secs(5), "dropping it stopped it and waited");
    }

    /// A decode thread still finishing past the engine's wait doesn't hold
    /// its end: the engine stops when the wait is up, the thread kept for
    /// later (#3287). The app's wait is a second, so a decoder hung at
    /// Quit doesn't hold the app open.
    #[test]
    fn a_decode_thread_past_the_wait_doesnt_hold_the_engines_end() {
        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let mut e = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::new(klipp_timeline::SharedSequence::new(Sequence::new_default())), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        e.set_straggler_wait(Duration::from_millis(50));
        let running = e.spawn();
        crate::video_decoder::keep_straggler(std::thread::spawn(|| std::thread::sleep(Duration::from_secs(3))));
        let start = Instant::now();
        cmd.send(klipp_core::AppCommand::Quit).unwrap();
        assert!(running.join().is_ok());
        assert!(start.elapsed() < Duration::from_secs(1), "stopped once its wait was up: {:?}", start.elapsed());

        // Run as the app runs it, its wait not set: a second.
        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let e = PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            Arc::new(klipp_timeline::SharedSequence::new(Sequence::new_default())), Arc::new(RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        let app = std::thread::spawn(move || e.run());
        crate::video_decoder::keep_straggler(std::thread::spawn(|| std::thread::sleep(Duration::from_secs(4))));
        let start = Instant::now();
        cmd.send(klipp_core::AppCommand::Quit).unwrap();
        assert!(app.join().is_ok());
        assert!(start.elapsed() < Duration::from_secs(2), "the app's engine waits a second: {:?}", start.elapsed());
    }

    /// A frame is cheap already with nothing on it, or with every track
    /// on it frozen and current there; a stale or live span isn't.
    #[test]
    fn frozen_current_and_empty_frames_need_no_drawing() {
        let mut v1 = track("V1", 0, 24);
        v1.frozen = true;
        let id = v1.id;
        let e = engine(vec![v1, track("V2", 12, 24)]);
        let seq = e.sequence.snapshot();
        let current = crate::freeze::FreezeStatus { frames: 24, tally: Some(crate::freeze::FreezeTally { stored: 24, ..Default::default() }), stale: vec![(tc(4), tc(6))], ..Default::default() };
        crate::freeze::set_statuses([(id, current)].into());
        assert!(PlaybackEngine::frozen_or_empty(&seq, tc(2).value), "frozen and current");
        assert!(!PlaybackEngine::frozen_or_empty(&seq, tc(5).value), "in a stale span");
        assert!(!PlaybackEngine::frozen_or_empty(&seq, tc(14).value), "another track on it, not frozen");
        assert!(PlaybackEngine::frozen_or_empty(&seq, tc(30).value), "nothing on it");
        let mut unfrozen = (*seq).clone();
        unfrozen.tracks[0].frozen = false;
        assert!(!PlaybackEngine::frozen_or_empty(&unfrozen, tc(2).value), "unfrozen: its frozen frames aren't played");
        crate::freeze::set_statuses([(id, crate::freeze::FreezeStatus { frames: 24, ..Default::default() })].into());
        assert!(!PlaybackEngine::frozen_or_empty(&seq, tc(2).value), "frozen, its frames not drawn yet");
        crate::freeze::set_statuses(Default::default());
    }
}
