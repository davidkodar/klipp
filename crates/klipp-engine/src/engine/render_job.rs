//! Render In to Out and Render Selection (#924): the frames of a range
//! rendered into the render cache in the background, so a graded stretch
//! plays from the cache.
//!
//! Only frames the render cache can hold are rendered: one graded media
//! clip and nothing else on screen (`render_cache_key`'s rule). Their
//! pictures come from a decoder of the job's own, opened, sought and read
//! by a [`ClipStreams`] of capacity one, the same code that decodes an
//! overlay for playback, with the file and rotation playback would use.
//! So a frame rendered here is the frame playback would render, and the
//! playhead, the viewer and the playback decoders are left alone. One
//! frame is rendered per idle tick, and only while paused: playing, a
//! command, or a scrub comes first.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use klipp_core::{AppEvent, ClipId, RationalTime};

use super::streams::{ClipStreams, Want};
use super::PlaybackEngine;

/// How long one frame may take to arrive before it is passed over: a
/// file that cannot be read must not hold the job for ever.
const FRAME_PATIENCE: Duration = Duration::from_secs(5);

/// How often progress is sent to the UI.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// A range being rendered into the cache.
pub(crate) struct RenderJob {
    /// The frames still to render: timeline position (ticks) and the clip
    /// that fills it.
    pending: VecDeque<(i64, ClipId)>,
    total: usize,
    rendered: usize,
    passed_over: usize,
    /// The job's own decoder, following the clip of the next frame.
    streams: ClipStreams,
    /// When the frame at the front was first asked for.
    waiting_since: Option<Instant>,
    last_progress: Instant,
}

/// The frames of `from..to` (ticks, `ticks_per_frame` apart from 0) that
/// `cacheable` accepts and `cached` does not already hold.
pub(crate) fn frames_to_render(
    from: i64,
    to: i64,
    ticks_per_frame: i64,
    mut cacheable: impl FnMut(i64) -> Option<ClipId>,
    mut cached: impl FnMut(i64, ClipId) -> bool,
) -> VecDeque<(i64, ClipId)> {
    let step = ticks_per_frame.max(1);
    let first = from.max(0).div_euclid(step) * step;
    let first = if first < from { first + step } else { first };
    (0..)
        .map(|i| first + i * step)
        .take_while(|&t| t < to)
        .filter_map(|t| cacheable(t).map(|clip| (t, clip)))
        .filter(|&(t, clip)| !cached(t, clip))
        .collect()
}

/// The clip with `id`, wherever it is in `seq`.
fn clip_of(seq: &klipp_timeline::Sequence, id: ClipId) -> Option<&klipp_timeline::Clip> {
    let (track, index) = seq.find_clip(id)?;
    seq.tracks.get(track)?.clips.get(index)
}

impl PlaybackEngine {
    /// Start rendering `from..to` into the render cache, replacing a job
    /// under way.
    pub(super) fn start_render_range(&mut self, from: RationalTime, to: RationalTime) {
        self.render_job = None;
        if !self.render_cache_disk.is_enabled() {
            let _ = self.event_tx.send(AppEvent::RenderRangeFinished { rendered: 0, passed_over: 0, cancelled: false, note: Some("the render cache is turned off in Preferences".into()) });
            return;
        }
        let seq = std::sync::Arc::clone(&self.sequence);
        let seq = seq.read().unwrap_or_else(|e| e.into_inner());
        let rate = seq.fps.rate();
        let ticks_per_frame = i64::from(seq.fps.den).max(1);
        let (from, to) = (from.rescale_to(rate).value, to.rescale_to(rate).value);
        let mut hashes: std::collections::HashMap<ClipId, u64> = std::collections::HashMap::new();
        let pending = frames_to_render(
            from,
            to,
            ticks_per_frame,
            |t| {
                let at = RationalTime { value: t, rate };
                let layers = klipp_compose::active_layers(&seq, at);
                let [layer] = layers.as_slice() else { return None };
                let [clip] = layer.clips.as_slice() else { return None };
                let graded = clip.frames_cacheable();
                (!layer.transition && graded && clip.media_id().is_some() && !clip.is_reversed()).then_some(clip.id)
            },
            |t, clip| {
                let Some(c) = clip_of(&seq, clip) else { return true };
                let opacity = seq.tracks.iter().find(|tr| tr.clips.iter().any(|x| x.id == clip)).map(|tr| tr.opacity).unwrap_or(1.0);
                let hash = *hashes.entry(clip).or_insert_with(|| self.render_key_hash(&seq, c, opacity));
                self.render_cache_disk.cached_frames(clip, hash).any(|f| f == t)
            },
        );
        drop(seq);
        let total = pending.len();
        if total == 0 {
            let _ = self.event_tx.send(AppEvent::RenderRangeFinished { rendered: 0, passed_over: 0, cancelled: false, note: Some("nothing there needs rendering".into()) });
            return;
        }
        tracing::info!("Render range: {total} frame(s) to render");
        let _ = self.event_tx.send(AppEvent::RenderRangeProgress { done: 0, total });
        self.render_job = Some(RenderJob {
            pending,
            total,
            rendered: 0,
            passed_over: 0,
            streams: ClipStreams::new(),
            waiting_since: None,
            last_progress: Instant::now(),
        });
    }

    /// Stop the job under way, keeping what it rendered.
    pub(super) fn cancel_render_range(&mut self) {
        if let Some(job) = self.render_job.take() {
            let _ = self.event_tx.send(AppEvent::RenderRangeFinished { rendered: job.rendered, passed_over: job.passed_over, cancelled: true, note: None });
            self.update_render_cache_ranges();
        }
    }

    /// One step of the job: the next frame rendered once its picture is
    /// decoded. Returns whether a job is under way.
    pub(super) fn step_render_range(&mut self) -> bool {
        let Some(mut job) = self.render_job.take() else { return false };
        while let Some(&(t, clip_id)) = job.pending.front() {
            let found = {
                let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
                let at = RationalTime { value: t, rate: seq.fps.rate() };
                clip_of(&seq, clip_id).filter(|c| c.timeline_in <= at && at < c.timeline_out).and_then(|c| {
                    Some(Want {
                        clip: c.id,
                        media: c.media_id()?,
                        rotation: c.source_rotation,
                        source_secs: klipp_compose::render::source_secs(c, at, seq.fps.as_f64()),
                        reversed: false,
                        ahead: false,
                    })
                })
            };
            // The clip was edited away from the frame since the job began.
            let Some(want) = found else {
                job.pending.pop_front();
                job.passed_over += 1;
                continue;
            };
            let resolved = self.path_for_media(want.media).map(|path| {
                let rotation = self.effective_decode_rotation(want.media, &path, want.rotation);
                let cost = self.media_cost(want.media, &path);
                (path, rotation, cost)
            });
            let frame_secs = self.fps.frame_duration_secs();
            job.streams.sync(std::slice::from_ref(&want), frame_secs, &|_| resolved.clone());
            let waited = *job.waiting_since.get_or_insert_with(Instant::now);
            if job.streams.ready(frame_secs) {
                if let Some(held) = job.streams.frame(clip_id) {
                    self.prerender_frame_as(t, Some(clip_id), held);
                    job.rendered += 1;
                } else {
                    job.passed_over += 1;
                }
            } else if resolved.is_some() && waited.elapsed() < FRAME_PATIENCE {
                // Still decoding: the next idle tick looks again.
                break;
            } else {
                tracing::warn!("Render range: the frame at {t} of {clip_id:?} could not be decoded; passing it over");
                job.passed_over += 1;
            }
            job.pending.pop_front();
            job.waiting_since = None;
            break;
        }
        if job.pending.is_empty() {
            tracing::info!("Render range: done, {} rendered, {} passed over", job.rendered, job.passed_over);
            let _ = self.event_tx.send(AppEvent::RenderRangeFinished { rendered: job.rendered, passed_over: job.passed_over, cancelled: false, note: None });
            self.update_render_cache_ranges();
            (self.repaint_cb)();
            return false;
        }
        if job.last_progress.elapsed() >= PROGRESS_EVERY {
            job.last_progress = Instant::now();
            let _ = self.event_tx.send(AppEvent::RenderRangeProgress { done: job.total - job.pending.len(), total: job.total });
            self.update_render_cache_ranges();
            (self.repaint_cb)();
        }
        self.render_job = Some(job);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::frames_to_render;
    use klipp_core::ClipId;

    #[test]
    fn the_range_is_walked_a_frame_at_a_time_from_frame_boundaries() {
        let clip = ClipId::new();
        // NTSC: a frame is 1001 ticks. 500..5005 starts on the frame at 1001.
        let frames: Vec<i64> = frames_to_render(500, 5005, 1001, |_| Some(clip), |_, _| false).into_iter().map(|(t, _)| t).collect();
        assert_eq!(frames, vec![1001, 2002, 3003, 4004]);
        let frames: Vec<i64> = frames_to_render(0, 3003, 1001, |_| Some(clip), |_, _| false).into_iter().map(|(t, _)| t).collect();
        assert_eq!(frames, vec![0, 1001, 2002], "`to` is not included");
    }

    #[test]
    fn frames_the_cache_cannot_hold_or_already_holds_are_left_out() {
        let clip = ClipId::new();
        let frames: Vec<i64> = frames_to_render(0, 10, 1, |t| (t % 2 == 0).then_some(clip), |t, _| t == 4)
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(frames, vec![0, 2, 6, 8], "odd frames are not cacheable, 4 is cached");
    }
}
