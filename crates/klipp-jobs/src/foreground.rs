//! Shared atomic state describing foreground (user-facing) activity.
//!
//! The engine flips `is_playing` on play/pause. The UI bumps
//! `last_input_at_us` on every input event. Background jobs read
//! [`ForegroundActivity::is_user_busy`] to decide whether to yield.
//!
//! All state is lock-free atomics — readable from anywhere without
//! contention. Single source of truth so scope refresh, prefetch,
//! cache eviction, and every BackgroundJob agree on "is the user busy
//! right now".

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct ForegroundActivity {
    /// Microseconds since `epoch` of the most recent user input event
    /// (mouse/key/scroll). 0 means "no input yet". Stored relative to
    /// a monotonic anchor so we don't have to deal with system-clock
    /// adjustments mid-session.
    last_input_at_us: AtomicU64,
    /// Anchor for the monotonic clock. All `last_input_at_us` values
    /// are microseconds relative to this Instant.
    #[allow(dead_code)]
    epoch: Instant,
    /// True when the engine's playback state is `Playing`.
    is_playing: AtomicBool,
    /// True when the user is actively scrubbing (timeline drag, slider
    /// drag, transport jog). Distinct from `is_playing` because a
    /// scrub can happen while the engine is paused.
    is_scrubbing: AtomicBool,
}

impl ForegroundActivity {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            last_input_at_us: AtomicU64::new(0),
            epoch: Instant::now(),
            is_playing: AtomicBool::new(false),
            is_scrubbing: AtomicBool::new(false),
        })
    }

    /// Bump the "last input at" timestamp to now. Called from the UI
    /// thread on every mouse / keyboard / scroll event.
    pub fn touch_input(&self) {
        // Store `elapsed_us + 1` so the value is always non-zero —
        // 0 is the sentinel for "no input recorded yet". Without
        // the +1, calling `touch_input` immediately after `new()`
        // would store a 0-microsecond elapsed which then reads back
        // as Duration::MAX (the "never touched" branch).
        let us = self.epoch.elapsed().as_micros() as u64;
        self.last_input_at_us.store(us.saturating_add(1), Ordering::Relaxed);
    }

    pub fn set_playing(&self, playing: bool) {
        self.is_playing.store(playing, Ordering::Relaxed);
    }

    pub fn set_scrubbing(&self, scrubbing: bool) {
        self.is_scrubbing.store(scrubbing, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing.load(Ordering::Relaxed)
    }

    pub fn is_scrubbing(&self) -> bool {
        self.is_scrubbing.load(Ordering::Relaxed)
    }

    /// How long since the most recent input event. Returns
    /// [`Duration::MAX`] if no input has ever been recorded.
    pub fn time_since_input(&self) -> Duration {
        // 0 is the sentinel for "never recorded". `touch_input`
        // stores `elapsed_us + 1` so a real input always reads back
        // as ≥ 1.
        let stored = self.last_input_at_us.load(Ordering::Relaxed);
        if stored == 0 {
            return Duration::MAX;
        }
        let last_us = stored - 1; // undo the touch_input bias
        let now_us = self.epoch.elapsed().as_micros() as u64;
        Duration::from_micros(now_us.saturating_sub(last_us))
    }

    /// True iff the user is doing anything that demands foreground
    /// resources — playing, scrubbing, or having interacted within
    /// `idle_window`.
    pub fn is_user_busy(&self, idle_window: Duration) -> bool {
        self.is_playing()
            || self.is_scrubbing()
            || self.time_since_input() < idle_window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_activity_reports_idle() {
        let fg = ForegroundActivity::new();
        assert!(!fg.is_playing());
        assert!(!fg.is_scrubbing());
        assert_eq!(fg.time_since_input(), Duration::MAX);
        assert!(!fg.is_user_busy(Duration::from_secs(2)));
    }

    #[test]
    fn touch_input_makes_user_busy_within_window() {
        let fg = ForegroundActivity::new();
        fg.touch_input();
        // Within 2s of touch_input, user is busy.
        assert!(fg.is_user_busy(Duration::from_secs(2)));
        // With a 0-window, even a fresh touch isn't "busy".
        assert!(!fg.is_user_busy(Duration::ZERO));
    }

    #[test]
    fn playing_implies_busy_regardless_of_input() {
        let fg = ForegroundActivity::new();
        fg.set_playing(true);
        // No input ever, but playing means busy.
        assert!(fg.is_user_busy(Duration::from_secs(2)));
        fg.set_playing(false);
        assert!(!fg.is_user_busy(Duration::from_secs(2)));
    }

    #[test]
    fn scrubbing_implies_busy_regardless_of_input() {
        let fg = ForegroundActivity::new();
        fg.set_scrubbing(true);
        assert!(fg.is_user_busy(Duration::from_secs(2)));
        fg.set_scrubbing(false);
        assert!(!fg.is_user_busy(Duration::from_secs(2)));
    }

    #[test]
    fn time_since_input_advances() {
        let fg = ForegroundActivity::new();
        fg.touch_input();
        let t1 = fg.time_since_input();
        std::thread::sleep(Duration::from_millis(10));
        let t2 = fg.time_since_input();
        assert!(t2 > t1, "time_since_input should grow ({t2:?} > {t1:?})");
    }

    #[test]
    fn shared_arc_visible_across_threads() {
        let fg = ForegroundActivity::new();
        let fg_clone = Arc::clone(&fg);
        let h = std::thread::spawn(move || {
            fg_clone.set_playing(true);
        });
        h.join().unwrap();
        assert!(fg.is_playing());
    }
}
