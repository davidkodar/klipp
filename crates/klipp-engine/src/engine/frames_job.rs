//! Whole frames drawn ahead, on the engine's side (#3212): where they are
//! kept, shown in place of composing while their key holds, and drawn by
//! Render In to Out and Render Selection in a background job of their own.
//! See [`crate::frame_cache`].

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use klipp_core::{AppEvent, RationalTime, Timecode};
use klipp_jobs::BackgroundJob;
use klipp_timeline::Sequence;

use super::PlaybackEngine;
use crate::frame_cache::{FrameRequest, FramesTally, Keying, NAMESPACE};

/// How often a job's progress is sent to the UI.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// Frames read ahead while playing.
const READ_AHEAD: i64 = 4;

/// A job drawing frames ahead, and how it ends.
pub(crate) struct FramesJob {
    job: Arc<klipp_jobs::ThreadJob>,
    done: std::sync::mpsc::Receiver<FramesTally>,
    total: usize,
    progress_at: Instant,
}

/// A job let go is cancelled and its thread waited for, the frame in hand
/// finished: an engine that has stopped leaves no job drawing (#3287).
impl Drop for FramesJob {
    fn drop(&mut self) {
        self.job.cancel();
        self.job.join();
    }
}

/// The frames from `from` to `to` (ticks at the sequence's rate) a frame
/// apart, from the first frame at or after `from`.
pub(crate) fn frames_between(from: i64, to: i64, ticks_per_frame: i64, rate: u32) -> Vec<Timecode> {
    let step = ticks_per_frame.max(1);
    let first = from.max(0).div_euclid(step) * step;
    let first = if first < from { first + step } else { first };
    (0..).map(|i| first + i * step).take_while(|&t| t < to).map(|t| RationalTime { value: t, rate }).collect()
}

impl PlaybackEngine {
    /// Where whole frames are kept: the project's cache, or without a saved
    /// project the user's.
    pub(super) fn frames_root(&self) -> std::path::PathBuf {
        self.freeze_folder.clone().unwrap_or_else(|| dirs::cache_dir().unwrap_or_default().join("klipp").join("render"))
    }

    pub(super) fn frames_store(&self) -> Arc<Mutex<crate::cache_store::CacheStore>> {
        crate::cache_store::shared_at(&self.frames_root(), u64::from(self.cache_budget_gb) << 30)
    }

    /// What keys a whole frame now: the files' identities, and the
    /// environment as playback draws.
    pub(super) fn frame_keying(&mut self) -> Keying {
        let media = match &self.freeze_media {
            Some(m) => Arc::clone(m),
            None => {
                let m = Arc::new(crate::freeze::media_identities(&self.media_pool.read().unwrap_or_else(|e| e.into_inner())));
                self.freeze_media = Some(Arc::clone(&m));
                m
            }
        };
        Keying { media, environment: crate::freeze::environment(crate::video_decoder::working_colorspace(), self.use_proxies, self.draw_settings()) }
    }

    /// How the viewer draws past the working space, as frames kept for it
    /// are drawn and keyed.
    pub(super) fn draw_settings(&self) -> crate::freeze::DrawSettings {
        crate::freeze::DrawSettings::current(self.color_pipeline.use_tetrahedral)
    }

    /// Whether the viewer shows the program frame, the one frames are
    /// kept of: not a node soloed, one channel, a matte, the HDR picture,
    /// the grade bypassed, a compare split, one clip measured alone, a
    /// node's input sampled or its thumbnails drawn, nor a grade dragged
    /// (each frame of the drag is new). Anything else is drawn live, as
    /// the frame kept would show the program frame in its place. Also
    /// while the inspector's matte tile is up, which reads each frame's
    /// matte from its composite.
    ///
    /// Frames drawn ahead don't ask: they draw the program frame on their
    /// own renderer, whatever the viewer shows.
    ///
    /// Nor while a Timeline LUT is on (#3265): frames are drawn ahead
    /// without it, and one kept showed the picture without the LUT. They
    /// stay right for when it is off again.
    pub(super) fn viewer_shows_program_frame(&self) -> bool {
        !(self.in_node_param_edit
            || self.color_pipeline.applies()
            || self.solo.is_some()
            || self.view_channel != klipp_core::ViewChannel::Picture
            || self.matte_preview
            || self.hdr_monitor.renders_hdr()
            || self.bypass_color_grade
            || self.compare_stabilization
            || self.compare_effect_active
            || self.measure_only_clip.is_some()
            || self.node_sample.is_some()
            || self.inspector_matte_preview_target.is_some()
            || (self.state != super::PlaybackState::Playing && !self.visible_node_thumbs.is_empty()))
    }

    /// The whole frame at `time` at `size`, when it is kept under its key:
    /// read ahead while playing. `None` with nothing kept, or turned off.
    ///
    /// Its display bytes, made on the reading thread: nothing is read,
    /// unpacked or encoded here. Not taken where drawing it live is as
    /// quick as loading it ([`crate::background_render::prefer_kept`]).
    pub(super) fn kept_frame(&mut self, seq: &Sequence, time: Timecode, size: (u32, u32)) -> Option<Vec<u8>> {
        if !self.frames_enabled {
            return None;
        }
        let root = self.frames_root();
        if self.frames_store().lock().unwrap_or_else(|e| e.into_inner()).bytes(NAMESPACE) == 0 {
            return None;
        }
        let rate = seq.fps.rate();
        let frame = i64::from(seq.fps.den).max(1);
        let tick = time.rescale_to(rate).value.div_euclid(frame) * frame;
        if !crate::background_render::prefer_kept(self.cost_log.get(tick), self.kept_cost_ms()) {
            return None;
        }
        let keying = self.frame_keying();
        let encode = crate::frame_cache::DisplayEncode { tone: self.tone_map_op, working: crate::video_decoder::working_colorspace(), transfer: self.sdr_transfer };
        let reader = self.frames_reader.get_or_insert_with(crate::frame_cache::FrameReader::new);
        // Playing, the frames ahead; parked, this one and its neighbours,
        // for the next step either way.
        let ahead: Vec<i64> = if self.state == super::PlaybackState::Playing { (0..=READ_AHEAD).collect() } else { vec![0, 1, -1] };
        for k in ahead {
            let at = RationalTime { value: tick + frame * k, rate };
            if at.value >= 0 {
                reader.ask(&root, keying.place(seq, at, size), encode);
            }
        }
        reader.take(keying.place(seq, RationalTime { value: tick, rate }, size), encode)
    }

    /// Render `from..to` ahead into the frame cache, replacing a job under
    /// way: every frame there, at the size playback draws it and the size
    /// a parked frame is drawn at, in a background job of its own.
    pub(super) fn start_render_range(&mut self, from: RationalTime, to: RationalTime) {
        self.cancel_frames_job(false);
        self.stop_background();
        if !self.frames_enabled {
            let _ = self.event_tx.send(AppEvent::RenderRangeFinished { rendered: 0, passed_over: 0, cancelled: false, note: Some("the render cache is turned off in Preferences".into()) });
            return;
        }
        let seq = self.sequence.snapshot();
        let rate = seq.fps.rate();
        let frames = frames_between(from.rescale_to(rate).value, to.rescale_to(rate).value, i64::from(seq.fps.den), rate);
        if frames.is_empty() {
            let _ = self.event_tx.send(AppEvent::RenderRangeFinished { rendered: 0, passed_over: 0, cancelled: false, note: Some("nothing there to render".into()) });
            return;
        }
        let full = super::view::playback_canvas(None, (seq.resolution.width, seq.resolution.height));
        let sizes = super::view::prerender_sizes(full, crate::video_decoder::playback_quality_divisor());
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner()).clone();
        let files: Arc<std::collections::HashMap<klipp_core::MediaId, klipp_render::MediaFile>> = Arc::new(pool.all().map(|i| (i.id, klipp_render::MediaFile::of(i))).collect());
        let working = crate::video_decoder::working_colorspace();
        let fps = seq.fps.as_f64();
        let total = frames.len();
        let req = FrameRequest { seq, frames, sizes, working, draw: self.draw_settings(), keying: self.frame_keying(), stop: None, evict: true };
        let (tx, done) = std::sync::mpsc::channel();
        let job = crate::frame_cache::spawn_frames(req, self.frames_store(), move || klipp_render::ExportFrameSource::shared(files, fps, working), move |t| {
            let _ = tx.send(t);
        });
        tracing::info!("Render range: {total} frame(s) ahead");
        let _ = self.event_tx.send(AppEvent::RenderRangeProgress { done: 0, total });
        self.frames_job = Some(FramesJob { job, done, total, progress_at: Instant::now() });
    }

    /// Stop the job under way, keeping what it drew; `say` reports it.
    pub(super) fn cancel_frames_job(&mut self, say: bool) {
        if let Some(j) = self.frames_job.take() {
            j.job.cancel();
            if say {
                let done = (j.job.progress() * j.total as f32).round() as usize;
                let _ = self.event_tx.send(AppEvent::RenderRangeFinished { rendered: done, passed_over: 0, cancelled: true, note: None });
            }
            self.update_render_cache_ranges();
        }
    }

    pub(super) fn cancel_render_range(&mut self) {
        self.cancel_frames_job(true);
    }

    /// A tick of a job under way: its progress sent, its end reported.
    pub(super) fn poll_frames_job(&mut self) {
        let Some(j) = &mut self.frames_job else { return };
        if let Ok(t) = j.done.try_recv() {
            let note = if t.full { Some("the cache is full: a larger Max cache size in Preferences holds more".into()) } else { None };
            let _ = self.event_tx.send(AppEvent::RenderRangeFinished { rendered: t.stored + t.current, passed_over: t.failed, cancelled: t.cancelled, note });
            self.frames_job = None;
            self.update_render_cache_ranges();
            return;
        }
        if j.progress_at.elapsed() >= PROGRESS_EVERY {
            j.progress_at = Instant::now();
            let done = (j.job.progress() * j.total as f32).round() as usize;
            let _ = self.event_tx.send(AppEvent::RenderRangeProgress { done, total: j.total });
        }
    }

    /// Clear the frames kept from `from` to `to`, at every key and size.
    pub(super) fn clear_render_cache_range(&mut self, from: RationalTime, to: RationalTime) {
        let rate = self.sequence.snapshot().fps.rate();
        let removed = self.frames_store().lock().unwrap_or_else(|e| e.into_inner()).remove_ticks(NAMESPACE, from.rescale_to(rate).value..to.rescale_to(rate).value);
        tracing::info!("Render cache: cleared {removed} frame(s) from {:.2}s to {:.2}s", from.to_secs_f64(), to.to_secs_f64());
        self.update_render_cache_ranges();
        (self.repaint_cb)();
    }

    /// Every frame kept let go, and the files an older render cache left
    /// beside them.
    pub(super) fn clear_render_cache_all(&mut self) {
        self.stop_background();
        self.frames_store().lock().unwrap_or_else(|e| e.into_inner()).remove_namespace(NAMESPACE);
        let old = dirs::cache_dir().unwrap_or_default().join("klipp").join("render");
        for e in std::fs::read_dir(&old).into_iter().flatten().flatten() {
            if e.path().extension().is_some_and(|x| x == "lz4") && e.file_type().is_ok_and(|t| t.is_file()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        tracing::info!("Render cache: cleared");
        self.update_render_cache_ranges();
        (self.repaint_cb)();
    }

    /// What the timeline shows of the frames kept at the size playback
    /// draws (#3212): each clip's share, and the spans over the ruler,
    /// rendered, heavy or stale. Worked out on a thread of its own, as a
    /// stored frame's current key costs a hash of the frame; again only
    /// when the sequence, the frames kept or the costs change.
    pub(super) fn update_render_cache_ranges(&mut self) {
        let (seq, revision) = self.sequence.snapshot_with_revision();
        let size = crate::freeze::playback_size((seq.resolution.width, seq.resolution.height));
        let (all, bytes) = {
            let store = self.frames_store();
            let store = store.lock().unwrap_or_else(|e| e.into_inner());
            (store.frames(NAMESPACE), store.bytes(NAMESPACE))
        };
        crate::background_render::set_summary(|m| {
            m.frames = all.len();
            m.bytes = bytes;
        });
        let inputs = (revision, all.len(), bytes, self.cost_log.generation());
        if self.status_inputs == Some(inputs) {
            return;
        }
        self.status_inputs = Some(inputs);
        let kept: Vec<crate::cache_store::FrameAt> = all.into_iter().filter(|a| a.size == size).collect();
        let heavy = self.heavy_ticks(&seq);
        let keying = self.frame_keying();
        let generation = STATUS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let fb = Arc::clone(&self.frame_buffer);
        let worker = self.status_worker.get_or_insert_with(|| spawn_status_worker(fb, Arc::default()));
        let _ = worker.send(StatusAsk { seq, kept, heavy, keying, size, generation });
    }
}

/// What the ruler's status is worked out from, at one moment.
pub(crate) struct StatusAsk {
    seq: Arc<Sequence>,
    kept: Vec<crate::cache_store::FrameAt>,
    heavy: Vec<i64>,
    keying: Keying,
    size: (u32, u32),
    generation: u64,
}

/// The one thread that works out the ruler's status: a pass at a time,
/// each from the latest ask, the asks that came meanwhile skipped. On a
/// large cache a pass can outlast a second; a thread an ask would run
/// them side by side.
/// `passes` counts the passes made, for a test to count its own worker's.
fn spawn_status_worker(fb: Arc<Mutex<crate::texture::FrameBuffer>>, passes: Arc<std::sync::atomic::AtomicU64>) -> std::sync::mpsc::Sender<StatusAsk> {
    let (tx, rx) = std::sync::mpsc::channel::<StatusAsk>();
    let _ = std::thread::Builder::new().name("klipp-render-status".into()).spawn(move || {
        while let Ok(first) = rx.recv() {
            let ask = rx.try_iter().last().unwrap_or(first);
            let (status, ranges) = render_status(&ask.seq, &ask.kept, &ask.heavy, &ask.keying, ask.size);
            passes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if STATUS_GENERATION.load(std::sync::atomic::Ordering::Relaxed) == ask.generation {
                let mut fb = fb.lock().unwrap_or_else(|e| e.into_inner());
                fb.render_status = status;
                fb.render_cache_ranges = ranges;
            }
        }
    });
    tx
}

/// The last status asked for: an older one finishing later is dropped.
static STATUS_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How the timeline stands in the frame cache: `kept` (frames at `size`)
/// rendered where kept under the frame's current key, stale where kept
/// under another only, and the `heavy` frames not kept; with each video
/// clip's share of frames rendered.
pub(crate) fn render_status(seq: &Sequence, kept: &[crate::cache_store::FrameAt], heavy: &[i64], keying: &Keying, size: (u32, u32)) -> (Vec<crate::texture::RenderSpan>, Vec<(klipp_core::ClipId, u32, u32)>) {
    use crate::texture::SpanStatus;
    let rate = seq.fps.rate();
    let step = i64::from(seq.fps.den).max(1);
    let end = seq.total_duration().rescale_to(rate).value;
    let mut current = std::collections::HashMap::new();
    let mut by_tick: std::collections::BTreeMap<i64, SpanStatus> = std::collections::BTreeMap::new();
    for at in kept.iter().filter(|a| (0..end).contains(&a.tick)) {
        let key = *current.entry(at.tick).or_insert_with(|| keying.place(seq, RationalTime { value: at.tick, rate }, size).key);
        let status = if at.key == key { SpanStatus::Rendered } else { SpanStatus::Stale };
        let slot = by_tick.entry(at.tick).or_insert(status);
        if status == SpanStatus::Rendered {
            *slot = status;
        }
    }
    for &t in heavy {
        by_tick.entry(t).or_insert(SpanStatus::Heavy);
    }
    let rendered: Vec<i64> = by_tick.iter().filter(|(_, s)| **s == SpanStatus::Rendered).map(|(t, _)| *t).collect();
    let mut ranges = Vec::new();
    for track in seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video) {
        for clip in &track.clips {
            let (a, b) = (clip.timeline_in.rescale_to(rate).value, clip.timeline_out.rescale_to(rate).value);
            let inside = rendered.iter().filter(|t| (a..b).contains(*t)).count() as u32;
            if inside > 0 {
                ranges.push((clip.id, inside, ((b - a) / step).max(1) as u32));
            }
        }
    }
    let frames: Vec<(i64, SpanStatus)> = by_tick.into_iter().collect();
    (crate::texture::status_spans(&frames, step), ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #3265: while a Timeline LUT is on, the viewer draws live, never a
    /// kept frame: frames are drawn ahead without the LUT, and one kept
    /// showed the picture without it. Switched on with no LUT loaded, it
    /// applies nothing, and kept frames are shown.
    #[test]
    fn a_timeline_lut_on_shows_no_kept_frame() {
        let sequence = Arc::new(klipp_timeline::SharedSequence::new(Sequence::new_default()));
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded();
        let fb = || Arc::new(Mutex::new(crate::texture::FrameBuffer::new(16, 16)));
        let mut engine = crate::PlaybackEngine::new(
            cmd_rx, ev_tx, fb(), fb(),
            sequence, Arc::new(std::sync::RwLock::new(klipp_media::MediaPool::new())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(crate::perf::PerfRing::default())),
        );
        assert!(engine.viewer_shows_program_frame(), "nothing on: kept frames may show");
        engine.color_pipeline.enabled = true;
        assert!(engine.viewer_shows_program_frame(), "on with no LUT loaded applies nothing");
        engine.color_pipeline.global_lut = Some(klipp_color::lut::LutData::identity(2));
        assert!(!engine.viewer_shows_program_frame(), "a Timeline LUT on: drawn live");
        engine.color_pipeline.enabled = false;
        assert!(engine.viewer_shows_program_frame(), "switched off again");
    }

    /// A frame kept under its current key reads rendered, one kept under
    /// another only stale, a heavy frame not kept heavy, and nothing past
    /// the sequence's end; a clip's share counts its rendered frames.
    #[test]
    fn kept_frames_read_rendered_or_stale_by_their_key_and_heavy_ones_not_kept_heavy() {
        use crate::texture::{RenderSpan, SpanStatus};
        let tc = |f: i64| RationalTime::from_frames(f, 24);
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        track.clips = vec![klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0), tc(10), tc(0), tc(10))];
        let clip = track.clips[0].id;
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::film_24();
        seq.tracks = vec![track];
        let keying = Keying { media: Arc::default(), environment: 1 };
        let size = (8, 8);
        let now = |f: i64| keying.place(&seq, tc(f), size);
        let old = |f: i64| crate::cache_store::FrameAt { key: now(f).key ^ 1, ..now(f) };
        // 0-2 current, 3 kept twice (current wins), 4-5 old only, 12 past the end.
        let kept = [now(0), now(1), now(2), old(3), now(3), old(4), old(5), now(12)];
        let heavy: Vec<i64> = (4..8).map(|f| tc(f).value).collect();
        let (spans, ranges) = render_status(&seq, &kept, &heavy, &keying, size);
        let span = |a: i64, b: i64, status| RenderSpan { from: tc(a).value, to: tc(b).value, status };
        assert_eq!(spans, vec![span(0, 4, SpanStatus::Rendered), span(4, 6, SpanStatus::Stale), span(6, 8, SpanStatus::Heavy)]);
        assert_eq!(ranges, vec![(clip, 4, 10)]);
    }

    /// The status is worked out a pass at a time, from the latest ask: 50
    /// asks while a pass is held up make two passes, not 50 side by side,
    /// and the ruler shows the last.
    #[test]
    fn the_status_worker_takes_the_latest_ask_and_skips_the_rest() {
        let tc = |f: i64| RationalTime::from_frames(f, 24);
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        track.clips = vec![klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0), tc(60), tc(0), tc(60))];
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::film_24();
        seq.tracks = vec![track];
        let seq = Arc::new(seq);
        let keying = || Keying { media: Arc::default(), environment: 1 };
        let size = (8, 8);
        let fb = Arc::new(Mutex::new(crate::texture::FrameBuffer::new(4, 4)));
        let passes: Arc<std::sync::atomic::AtomicU64> = Arc::default();
        let worker = spawn_status_worker(Arc::clone(&fb), Arc::clone(&passes));
        let ask = |n: i64| {
            let kept = (0..n).map(|f| keying().place(&seq, tc(f), size)).collect();
            let generation = STATUS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            worker.send(StatusAsk { seq: Arc::clone(&seq), kept, heavy: vec![], keying: keying(), size, generation }).unwrap();
        };
        // The first pass held up writing its result, then 49 more asks.
        let held = fb.lock().unwrap();
        ask(1);
        let start = std::time::Instant::now();
        while passes.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            assert!(start.elapsed() < std::time::Duration::from_secs(5), "the first pass made");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        for n in 2..=50 {
            ask(n);
        }
        drop(held);
        let start = std::time::Instant::now();
        let last = crate::texture::RenderSpan { from: 0, to: tc(50).value, status: crate::texture::SpanStatus::Rendered };
        while fb.lock().unwrap().render_status != vec![last] {
            assert!(start.elapsed() < std::time::Duration::from_secs(5), "the last ask shown: {:?}", fb.lock().unwrap().render_status);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let passes = passes.load(std::sync::atomic::Ordering::Relaxed);
        assert!(passes <= 2, "{passes} passes for 50 asks");
    }

    #[test]
    fn a_range_is_walked_a_frame_at_a_time_from_frame_boundaries() {
        let f = frames_between(1500, 6000, 1001, 30_000);
        assert_eq!(f.iter().map(|t| t.value).collect::<Vec<_>>(), vec![2002, 3003, 4004, 5005]);
        assert!(frames_between(10, 10, 1, 24).is_empty());
        assert_eq!(frames_between(0, 3, 1, 24).len(), 3);
    }
}
