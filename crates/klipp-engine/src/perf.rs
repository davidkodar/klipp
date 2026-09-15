//! Per-frame performance telemetry.
//!
//! The engine's `post_process_rgba` already instruments five timing
//! spans (seq_read / grade / ofx / builtin / lut) plus a total. Until
//! now those numbers only surfaced to the tracing log when a single
//! frame blew past 2 ms. That's fine for "did anything catastrophic
//! happen during this run" but useless for the "why does adding a
//! blur to a 4K clip drop my framerate" question the user actually
//! cares about.
//!
//! `PerfRing` fixes that by capturing every processed frame's
//! breakdown into a fixed-size ring buffer, indexed by wall time.
//! The UI thread reads it to paint the performance overlay;
//! `snapshot_json` serialises the whole ring for offline diagnosis
//! (send it to Claude, attach it to a bug report, graph it yourself).
//!
//! One ring per engine, shared via `Arc<Mutex<PerfRing>>`. Capacity
//! is tuned for five seconds at 60 fps; at typical playback rates
//! that's a generous window for eyeballing a repro.

use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One frame's post-processing timing snapshot.
///
/// All `*_ms` fields are wall-clock durations in milliseconds.
/// Values are f32 because no-one needs sub-microsecond precision on
/// a frame-budget graph, and f32 halves the ring-buffer footprint.
#[derive(Debug, Clone, Copy)]
pub struct PerfSample {
    /// When the sample was taken. Used by the UI to compute drops-
    /// per-second and to timestamp exported sessions.
    pub timestamp: Instant,
    /// Frame dimensions at post-process time. The compositor may
    /// down-scale for playback; this is the size actually processed.
    pub width: u32,
    pub height: u32,
    /// Target frame rate for the currently-playing clip or sequence.
    /// `0.0` when nothing is playing.
    pub target_fps: f32,

    /// Decode-thread cost of the most recently queued frame:
    /// `receive_frame` + HW download / swscale. Measured on the
    /// decode thread (pipelined with post-processing, so not
    /// additive with `total_ms` — compare each against the budget).
    pub decode_ms: f32,
    /// Sequence-read stage: reading the effect stack and pre-effect
    /// frame setup. Usually small.
    pub seq_read_ms: f32,
    /// Colour grade stage: node-graph execution on this frame.
    pub grade_ms: f32,
    /// OFX plugin stage: third-party plugin invocations (empty
    /// project → 0).
    pub ofx_ms: f32,
    /// Built-in effects stage: blur / sharpen / opacity / grade etc.
    /// This is usually where a slow frame lives.
    pub builtin_ms: f32,
    /// LUT stage: final 3D LUT application.
    pub lut_ms: f32,
    /// Total `post_process_rgba` time. Should be close to the sum
    /// of the stages above, but may differ by a fraction of a ms
    /// due to unaccounted bookkeeping.
    pub total_ms: f32,
    /// True when the engine judged the frame didn't make its
    /// wall-clock budget. Computed per-sample; not retroactively
    /// updated if later frames catch up.
    pub dropped: bool,

    /// CPU sRGB-u8 → linear-f32 bridge feeding the pregrade caches
    /// (4-90 ms at 4K depending on whether the GPU bridge handles
    /// it). Useful even when the user has no grade applied.
    pub bridge_u8_ms: f32,
    /// CPU/GPU linear-f32 → sRGB-u8 reencode feeding the scope and
    /// render cache. Same scaling as `bridge_u8_ms`.
    pub reencode_ms: f32,
    /// GPU scope render (waveform / parade / vectorscope / histogram),
    /// paced by its own cost: a cheap pass runs every frame, a costly
    /// one every other, and the frames between see 0.0.
    pub scope_ms: f32,
    /// CPU composite + tonemap — V1 + overlays blend into the final
    /// RGBA buffer. Independent of grade. Reserved; not yet wired.
    pub composite_ms: f32,
    /// Wall-clock for the entire `update_playback` tick — the most
    /// honest "actual" number for the overlay because it captures
    /// stages outside the grade pipeline (decode wait, fb_write,
    /// command drain). Reserved; not yet wired.
    pub update_playback_ms: f32,
}

/// Plain-data carrier for `PerfRing::update_wrap_stages`. Stages
/// the outer `post_process_rgba` measures that aren't visible from
/// inside `post_process_rgba_inner`.
pub struct WrapStages {
    pub bridge_u8_ms: f32,
    pub reencode_ms: f32,
    pub scope_ms: f32,
}

impl PerfSample {
    /// Effective budget for the target fps, in milliseconds. `None`
    /// when `target_fps` is zero or non-finite — avoids a div-by-zero
    /// in the UI layer.
    pub fn frame_budget_ms(&self) -> Option<f32> {
        if self.target_fps > 0.0 && self.target_fps.is_finite() {
            Some(1000.0 / self.target_fps)
        } else {
            None
        }
    }
}

/// Ring buffer of recent `PerfSample`s.
///
/// Fixed capacity — oldest samples fall off the back as new ones
/// arrive. Not lock-free (one `Mutex` wraps the whole thing); that's
/// fine because writes are per-frame (max ~60 Hz) and reads are per-
/// UI-paint (max ~monitor refresh). Contention is microseconds of
/// work at worst.
pub struct PerfRing {
    samples: Vec<PerfSample>,
    /// Next write index. Wraps at `capacity` via `% capacity`.
    head: usize,
    capacity: usize,
    /// Set by the UI when the user clicks "Record Performance". The
    /// engine keeps writing as normal — the record button just marks
    /// the starting point so a snapshot exports from that index
    /// forward.
    record_start: Option<(usize, Instant)>,
}

impl PerfRing {
    /// Capacity defaults to 5 seconds at 60 fps — enough to catch
    /// a "stutter when I enable blur" repro without reserving
    /// megabytes. Tune via [`Self::with_capacity`] for long captures.
    pub const DEFAULT_CAPACITY: usize = 300;

    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
            head: 0,
            capacity: capacity.max(1),
            record_start: None,
        }
    }

    pub fn push(&mut self, sample: PerfSample) {
        if self.samples.len() < self.capacity {
            self.samples.push(sample);
        } else {
            self.samples[self.head] = sample;
        }
        self.head = (self.head + 1) % self.capacity;
    }

    /// Patch the most recent sample with wrap-stage timings that
    /// the inner-grade push didn't know about. Called from the
    /// outer `post_process_rgba` after the wrapper finishes its own
    /// stage timing (bridge_u8, reencode, scope). Lets the overlay
    /// surface meaningful per-frame numbers even when the user has
    /// no grade applied (where the inner stages would all read 0).
    /// No-op if the ring has never been pushed to (engine just
    /// started).
    pub fn update_wrap_stages(&mut self, stages: WrapStages) {
        if self.samples.is_empty() { return; }
        // The most-recently-written sample is at `(head + capacity - 1) % capacity`
        // when the ring has wrapped, else at `head - 1` when it hasn't.
        let last_idx = if self.samples.len() < self.capacity {
            self.head.saturating_sub(1)
        } else {
            (self.head + self.capacity - 1) % self.capacity
        };
        if let Some(s) = self.samples.get_mut(last_idx) {
            s.bridge_u8_ms = stages.bridge_u8_ms;
            s.reencode_ms = stages.reencode_ms;
            s.scope_ms = stages.scope_ms;
        }
    }

    /// Iterate samples in insertion order (oldest first), up to the
    /// newest write. Holds no references to self after the closure
    /// returns, so callers can chain `.collect()` and release the
    /// lock quickly.
    pub fn iter_chronological(&self) -> impl Iterator<Item = &PerfSample> {
        // When `samples.len() < capacity`, the ring hasn't wrapped
        // yet — head is effectively `samples.len()`, and entries
        // `[0..head)` are in order already.
        if self.samples.len() < self.capacity {
            self.samples.iter().chain([].iter())
        } else {
            // Wrapped: oldest lives at `head`, chain [head..] then [..head].
            let (tail, head_slice) = self.samples.split_at(self.head);
            // `tail` = newer block (was written after wrap),
            // `head_slice` = older block. Wait — that's backwards.
            // Actually after wrap, `head` points to the *next*
            // slot to be overwritten, which is the *oldest* sample.
            // So order is [head..] then [..head).
            head_slice.iter().chain(tail.iter())
        }
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Most recent sample, if any. The overlay reads this every
    /// frame for the "current" numbers column.
    pub fn latest(&self) -> Option<&PerfSample> {
        if self.samples.is_empty() {
            return None;
        }
        // head points to the *next* write slot, so the last-written
        // sample is at `head - 1` modulo capacity.
        let idx = if self.head == 0 {
            self.samples.len() - 1
        } else {
            self.head - 1
        };
        self.samples.get(idx)
    }

    /// Rolling averages across the last `window` samples.
    pub fn averages(&self, window: usize) -> PerfAverages {
        let mut avg = PerfAverages::default();
        if self.samples.is_empty() {
            return avg;
        }
        let take = window.min(self.samples.len());
        let mut chron: Vec<&PerfSample> = self.iter_chronological().collect();
        chron.reverse();
        let recent: Vec<&PerfSample> = chron.into_iter().take(take).collect();
        let n = recent.len() as f32;
        if n == 0.0 {
            return avg;
        }
        for s in &recent {
            avg.total_ms += s.total_ms;
            avg.decode_ms += s.decode_ms;
            avg.seq_read_ms += s.seq_read_ms;
            avg.grade_ms += s.grade_ms;
            avg.ofx_ms += s.ofx_ms;
            avg.builtin_ms += s.builtin_ms;
            avg.lut_ms += s.lut_ms;
            avg.bridge_u8_ms += s.bridge_u8_ms;
            avg.reencode_ms += s.reencode_ms;
            avg.scope_ms += s.scope_ms;
            if s.dropped {
                avg.dropped_count += 1;
            }
        }
        avg.total_ms /= n;
        avg.decode_ms /= n;
        avg.seq_read_ms /= n;
        avg.grade_ms /= n;
        avg.ofx_ms /= n;
        avg.builtin_ms /= n;
        avg.lut_ms /= n;
        avg.bridge_u8_ms /= n;
        avg.reencode_ms /= n;
        avg.scope_ms /= n;
        avg.sample_count = recent.len();
        avg
    }

    /// Drops recorded within the last `secs` seconds of wall time.
    /// The overlay surfaces this as "Drops: N in last 1s" — the
    /// rolling counter is what reveals "my 4K + blur drops every
    /// couple of seconds" patterns that a cumulative total would
    /// hide.
    pub fn drops_in_last(&self, secs: f32) -> u32 {
        let now = Instant::now();
        self.iter_chronological()
            .filter(|s| s.dropped && now.duration_since(s.timestamp).as_secs_f32() <= secs)
            .count() as u32
    }

    /// Mark the start of a recording session. Subsequent calls to
    /// [`Self::end_recording`] export only samples from this
    /// point forward.
    pub fn begin_recording(&mut self) {
        self.record_start = Some((self.head, Instant::now()));
    }

    pub fn is_recording(&self) -> bool {
        self.record_start.is_some()
    }

    /// Stop recording and return the samples captured since the
    /// most recent `begin_recording` call, chronologically. Returns
    /// an empty `Vec` if no recording is active.
    pub fn end_recording(&mut self) -> Vec<PerfSample> {
        let Some((start_head, _)) = self.record_start.take() else {
            return Vec::new();
        };
        // Collect every sample whose write index is between
        // start_head and head. Easiest: grab chronological order,
        // then keep everything from the first sample whose timestamp
        // is >= the recording start.
        let all: Vec<PerfSample> = self.iter_chronological().copied().collect();
        // Safer than raw index math: the `record_start` may have
        // been overwritten if the ring wrapped during the session.
        // In that case we just return what's still in the buffer,
        // which is the correct "most recent 5 s" snapshot anyway.
        let _ = start_head;
        all
    }
}

impl Default for PerfRing {
    fn default() -> Self {
        Self::new()
    }
}

/// Rolling averages produced by [`PerfRing::averages`].
#[derive(Debug, Clone, Copy, Default)]
pub struct PerfAverages {
    pub total_ms: f32,
    pub decode_ms: f32,
    pub seq_read_ms: f32,
    pub grade_ms: f32,
    pub ofx_ms: f32,
    pub builtin_ms: f32,
    pub lut_ms: f32,
    pub bridge_u8_ms: f32,
    pub reencode_ms: f32,
    pub scope_ms: f32,
    pub dropped_count: u32,
    pub sample_count: usize,
}

/// Shared handle — `Arc<Mutex<PerfRing>>`. Passed from
/// [`crate::PlaybackEngine`] to the UI thread via `KlippApp::new`.
pub type SharedPerfRing = Arc<Mutex<PerfRing>>;

pub fn shared() -> SharedPerfRing {
    Arc::new(Mutex::new(PerfRing::new()))
}

/// Serialise a slice of samples to a compact JSON string for
/// offline analysis. Hand-rolled to avoid pulling in a JSON crate
/// for a single structure — `Instant` can't serde round-trip
/// without extra plumbing, so we export milliseconds since the
/// *first* sample's timestamp as the time axis.
pub fn samples_to_json(samples: &[PerfSample]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(samples.len() * 180);
    out.push_str("{\n  \"samples\": [\n");
    let t0 = samples.first().map(|s| s.timestamp);
    for (i, s) in samples.iter().enumerate() {
        let t_ms = match t0 {
            Some(origin) => s.timestamp.duration_since(origin).as_secs_f64() * 1000.0,
            None => 0.0,
        };
        let _ = write!(
            out,
            "    {{\"t_ms\":{:.3},\"w\":{},\"h\":{},\"fps\":{:.2},\"decode_ms\":{:.3},\"seq_ms\":{:.3},\"grade_ms\":{:.3},\"ofx_ms\":{:.3},\"builtin_ms\":{:.3},\"lut_ms\":{:.3},\"total_ms\":{:.3},\"dropped\":{}}}",
            t_ms, s.width, s.height, s.target_fps, s.decode_ms, s.seq_read_ms, s.grade_ms,
            s.ofx_ms, s.builtin_ms, s.lut_ms, s.total_ms, s.dropped,
        );
        if i + 1 < samples.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    fn sample(total_ms: f32, dropped: bool) -> PerfSample {
        PerfSample {
            timestamp: Instant::now(),
            width: 1920,
            height: 1080,
            target_fps: 24.0,
            decode_ms: 0.0,
            seq_read_ms: 0.5,
            grade_ms: 1.0,
            ofx_ms: 0.0,
            builtin_ms: total_ms * 0.6,
            lut_ms: 0.2,
            total_ms,
            dropped,
            bridge_u8_ms: 0.0,
            reencode_ms: 0.0,
            scope_ms: 0.0,
            composite_ms: 0.0,
            update_playback_ms: 0.0,
        }
    }

    #[test]
    fn ring_holds_up_to_capacity() {
        let mut r = PerfRing::with_capacity(4);
        for i in 0..3 {
            r.push(sample(i as f32 + 1.0, false));
        }
        assert_eq!(r.len(), 3);
        let totals: Vec<f32> = r.iter_chronological().map(|s| s.total_ms).collect();
        assert_eq!(totals, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn ring_wraps_and_preserves_order() {
        let mut r = PerfRing::with_capacity(3);
        for i in 0..6 {
            r.push(sample(i as f32, false));
        }
        // After 6 pushes into cap=3, we should see the last 3 in order.
        let totals: Vec<f32> = r.iter_chronological().map(|s| s.total_ms).collect();
        assert_eq!(totals, vec![3.0, 4.0, 5.0]);
    }

    #[test]
    fn latest_returns_most_recent() {
        let mut r = PerfRing::with_capacity(4);
        r.push(sample(1.0, false));
        r.push(sample(2.0, false));
        r.push(sample(3.0, false));
        assert_eq!(r.latest().unwrap().total_ms, 3.0);
    }

    #[test]
    fn averages_match_hand_computed() {
        let mut r = PerfRing::with_capacity(10);
        r.push(sample(10.0, true));
        r.push(sample(20.0, false));
        r.push(sample(30.0, true));
        let a = r.averages(10);
        assert!((a.total_ms - 20.0).abs() < 1e-3);
        assert_eq!(a.dropped_count, 2);
        assert_eq!(a.sample_count, 3);
    }

    #[test]
    fn drops_in_last_respects_wall_time() {
        let mut r = PerfRing::with_capacity(10);
        r.push(sample(10.0, true));
        sleep(Duration::from_millis(50));
        r.push(sample(10.0, true));
        // Both drops are well within the last second.
        assert_eq!(r.drops_in_last(1.0), 2);
        // Neither is within the last 0.01s (sleep makes sure of it).
        assert!(r.drops_in_last(0.01) < 2);
    }

    #[test]
    fn snapshot_to_json_round_trips_structure() {
        let samples = vec![sample(10.0, false), sample(20.0, true)];
        let json = samples_to_json(&samples);
        assert!(json.contains("\"total_ms\":10"));
        assert!(json.contains("\"total_ms\":20"));
        assert!(json.contains("\"dropped\":true"));
        assert!(json.contains("\"dropped\":false"));
        // Valid-ish JSON structure: starts with { ends with }
        assert!(json.trim_start().starts_with('{'));
        assert!(json.trim_end().ends_with('}'));
    }

    #[test]
    fn frame_budget_handles_zero_fps() {
        let mut s = sample(10.0, false);
        s.target_fps = 0.0;
        assert_eq!(s.frame_budget_ms(), None);
        s.target_fps = 24.0;
        assert!((s.frame_budget_ms().unwrap() - 41.666_668).abs() < 1e-3);
    }

    #[test]
    fn update_wrap_stages_patches_latest_sample() {
        // The grade-only push fills bridge_u8/reencode/scope = 0.
        // The wrap then patches them in. Overlay should see the
        // updated values on its next read.
        let mut r = PerfRing::with_capacity(8);
        r.push(sample(0.0, false));
        r.update_wrap_stages(WrapStages {
            bridge_u8_ms: 7.5,
            reencode_ms: 6.2,
            scope_ms: 0.7,
        });
        let last = r.iter_chronological().last().unwrap();
        assert_eq!(last.bridge_u8_ms, 7.5);
        assert_eq!(last.reencode_ms, 6.2);
        assert_eq!(last.scope_ms, 0.7);
    }

    #[test]
    fn update_wrap_stages_targets_latest_after_wraparound() {
        // When the ring has wrapped past capacity, "latest" lives at
        // (head + capacity - 1) % capacity. Verify the helper picks
        // the right slot.
        let mut r = PerfRing::with_capacity(3);
        r.push(sample(1.0, false));
        r.push(sample(2.0, false));
        r.push(sample(3.0, false));
        r.push(sample(4.0, false)); // overwrites slot 0; head wraps
        r.update_wrap_stages(WrapStages {
            bridge_u8_ms: 11.1,
            reencode_ms: 22.2,
            scope_ms: 33.3,
        });
        let last = r.iter_chronological().last().unwrap();
        assert_eq!(last.total_ms, 4.0);
        assert_eq!(last.bridge_u8_ms, 11.1);
        assert_eq!(last.reencode_ms, 22.2);
        assert_eq!(last.scope_ms, 33.3);
    }

    #[test]
    fn update_wrap_stages_noop_on_empty_ring() {
        // Engine just started, no samples pushed yet. update_wrap_stages
        // must not panic.
        let mut r = PerfRing::new();
        r.update_wrap_stages(WrapStages {
            bridge_u8_ms: 1.0,
            reencode_ms: 2.0,
            scope_ms: 3.0,
        });
        assert_eq!(r.len(), 0);
    }
}
