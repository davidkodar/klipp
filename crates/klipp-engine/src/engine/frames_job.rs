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
const READ_AHEAD: i64 = 3;

/// A job drawing frames ahead, and how it ends.
pub(crate) struct FramesJob {
    job: Arc<klipp_jobs::ThreadJob>,
    done: std::sync::mpsc::Receiver<FramesTally>,
    total: usize,
    progress_at: Instant,
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
        Keying { media, environment: crate::freeze::environment(crate::video_decoder::working_colorspace(), self.use_proxies) }
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
    pub(super) fn viewer_shows_program_frame(&self) -> bool {
        !(self.in_node_param_edit
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
    pub(super) fn kept_frame(&mut self, seq: &Sequence, time: Timecode, size: (u32, u32)) -> Option<Arc<Vec<f32>>> {
        if !self.frames_enabled {
            return None;
        }
        let root = self.frames_root();
        if self.frames_store().lock().unwrap_or_else(|e| e.into_inner()).bytes(NAMESPACE) == 0 {
            return None;
        }
        let keying = self.frame_keying();
        let reader = self.frames_reader.get_or_insert_with(crate::frame_cache::FrameReader::new);
        if self.state == super::PlaybackState::Playing {
            let step = (i64::from(time.rate) * i64::from(seq.fps.den)).checked_div(i64::from(seq.fps.num)).unwrap_or(1).max(1);
            for k in 1..=READ_AHEAD {
                let next = RationalTime { value: time.value + step * k, rate: time.rate };
                reader.ask(&root, keying.place(seq, next, size));
            }
        }
        reader.take(&root, keying.place(seq, time, size))
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
        let req = FrameRequest { seq, frames, sizes, working, keying: self.frame_keying(), stop: None, evict: true };
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
    /// draws: each clip's share, and the spans over the ruler.
    pub(super) fn update_render_cache_ranges(&mut self) {
        let seq = self.sequence.snapshot();
        let size = crate::freeze::playback_size((seq.resolution.width, seq.resolution.height));
        let mut ticks: Vec<i64> = self.frames_store().lock().unwrap_or_else(|e| e.into_inner()).frames(NAMESPACE).into_iter().filter(|a| a.size == size).map(|a| a.tick).collect();
        ticks.sort_unstable();
        ticks.dedup();
        let ticks_per_frame = i64::from(seq.fps.den).max(1);
        let mut ranges = Vec::new();
        let mut status = Vec::new();
        for track in seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video) {
            for clip in &track.clips {
                let (a, b) = (clip.timeline_in.rescale_to(seq.fps.rate()).value, clip.timeline_out.rescale_to(seq.fps.rate()).value);
                let total = ((b - a) / ticks_per_frame).max(1) as u32;
                let inside: Vec<i64> = ticks.iter().copied().filter(|t| (a..b).contains(t)).collect();
                if !inside.is_empty() {
                    ranges.push((clip.id, inside.len() as u32, total));
                }
                status.extend(crate::texture::render_spans(a, b, &inside, ticks_per_frame));
            }
        }
        let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        fb.render_cache_ranges = ranges;
        fb.render_status = status;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_walked_a_frame_at_a_time_from_frame_boundaries() {
        let f = frames_between(1500, 6000, 1001, 30_000);
        assert_eq!(f.iter().map(|t| t.value).collect::<Vec<_>>(), vec![2002, 3003, 4004, 5005]);
        assert!(frames_between(10, 10, 1, 24).is_empty());
        assert_eq!(frames_between(0, 3, 1, 24).len(), 3);
    }
}
