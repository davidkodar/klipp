//! A/V master clock: single authority for playback time.
//!
//! Both the video decoder loop and the audio callback slave to
//! [`MasterClock`]'s `now_pts_us()`. Neither thread drives time forward
//! — that's exclusively done by wall-clock interpolation from
//! `anchor_wall` using the configured `speed`. Audio advancing or
//! video arriving never changes the clock.
//!
//! # State fields
//!
//! * `anchor_pts_us` — the clock's anchor in source PTS (microseconds).
//! * `anchor_wall` — the wall-clock `Instant` at the anchor.
//! * `speed` — playback-rate multiplier (stored as f64 bits in a
//!   `u64` because `AtomicF64` isn't stable). 1.0 = realtime, 2.0 = 2×.
//! * `running` — false while paused. Interpolation stops; PTS
//!   returned from `now_pts_us()` freezes at the anchor value.
//! * `generation` — bumped on every seek. Decoders and the mix
//!   thread stamp every produced frame/sample with the generation
//!   at decode time; mismatches at playback time identify stale
//!   frames that must be dropped instead of shown.
//!
//! # Ordering invariants (CRITICAL)
//!
//! Operations on the clock have a strict sequence that every seek /
//! pause / resume path must follow. Get it wrong and you leak
//! frames from the previous anchor into the new one (A/V desync
//! for ~1 s) or the audio callback snapshots mid-update and plays
//! half-a-second of glitched samples.
//!
//! ## Seek
//!
//! 1. **Pause the clock** — `pause()`. Prevents downstream threads
//!    from interpolating past the seek target while the anchor is
//!    being updated.
//! 2. **Flush audio ring buffer** — stale samples from the old
//!    anchor must not reach cpal. The mix thread also clears its
//!    own staging buffers here.
//! 3. **Seek decoders** — video and audio `avformat_seek_file` +
//!    decoder.flush(). Generation bump happens next so any
//!    post-flush frame they emit is stamped with the new gen.
//! 4. **Set anchor + bump generation** — `set_anchor_pts_us()`
//!    atomically writes the new PTS and increments generation.
//! 5. **Flush audio ring buffer AGAIN** — covers the case where
//!    the audio callback drained samples between step 2 and 4.
//! 6. **Resume the clock** — `resume()`. Wall-clock interpolation
//!    restarts from the new anchor.
//!
//! ## Pause
//!
//! Just `pause()`; no flush required — samples already in the ring
//! are correct and will resume correctly.
//!
//! ## Resume
//!
//! Refresh `anchor_wall` to `Instant::now()` (so interpolation
//! restarts from the current wall time rather than accumulating
//! the pause duration) and set `running = true`.
//!
//! # Scrub
//!
//! Timeline scrubs write directly to `playhead_smooth_secs` and
//! update the anchor without going through the full seek dance
//! above — they're expected to produce visible tearing/flicker
//! during the drag, which is what users want for tactile scrub.
//! See [`crate::engine`] for the scrub-begin / scrub-end handoff
//! that re-syncs to the final position on release.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// External monotonic master clock — the single source of truth for A/V timing.
///
/// Neither audio nor video drives this clock. Only seek(), pause(), and resume()
/// modify it. Both audio and video slave to it independently.
pub struct MasterClock {
    /// PTS at the last anchor point (microseconds).
    anchor_pts_us: AtomicU64,
    /// Wall-clock time when the anchor was set.
    anchor_wall: Mutex<Instant>,
    /// Playback speed (stored as f64 bits).
    speed: AtomicU64,
    /// True if the clock is running.
    running: AtomicBool,
    /// Incremented on every seek — used to detect stale frames/samples.
    generation: AtomicU64,
}

impl Default for MasterClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MasterClock {
    pub fn new() -> Self {
        Self {
            anchor_pts_us: AtomicU64::new(0),
            anchor_wall: Mutex::new(Instant::now()),
            speed: AtomicU64::new(1.0_f64.to_bits()),
            running: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        }
    }

    /// Get the current playback position in microseconds.
    pub fn current_pts_us(&self) -> u64 {
        if !self.running.load(Ordering::Acquire) {
            return self.anchor_pts_us.load(Ordering::Acquire);
        }
        let elapsed = self.anchor_wall.lock().unwrap_or_else(|e| e.into_inner()).elapsed();
        let speed = f64::from_bits(self.speed.load(Ordering::Acquire));
        let anchor = self.anchor_pts_us.load(Ordering::Acquire);
        anchor + (elapsed.as_micros() as f64 * speed) as u64
    }

    /// Get the current playback position in seconds (f64).
    pub fn current_secs(&self) -> f64 {
        self.current_pts_us() as f64 / 1_000_000.0
    }

    /// Seek: set the clock to a new position and bump generation.
    pub fn seek(&self, target_pts_us: u64) {
        *self.anchor_wall.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.anchor_pts_us.store(target_pts_us, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Pause: freeze the clock at the current position.
    pub fn pause(&self) {
        let current = self.current_pts_us();
        self.anchor_pts_us.store(current, Ordering::Release);
        *self.anchor_wall.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.running.store(false, Ordering::Release);
    }

    /// Resume: start the clock ticking from the current anchor.
    pub fn resume(&self) {
        *self.anchor_wall.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.running.store(true, Ordering::Release);
    }

    /// Get the current seek generation counter.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Check if the clock is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Set playback speed (1.0 = normal).
    pub fn set_speed(&self, speed: f64) {
        // Freeze at current position first, then change speed
        let current = self.current_pts_us();
        self.anchor_pts_us.store(current, Ordering::Release);
        *self.anchor_wall.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.speed.store(speed.to_bits(), Ordering::Release);
    }
}
