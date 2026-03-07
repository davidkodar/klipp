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
        let elapsed = self.anchor_wall.lock().unwrap().elapsed();
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
        *self.anchor_wall.lock().unwrap() = Instant::now();
        self.anchor_pts_us.store(target_pts_us, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Pause: freeze the clock at the current position.
    pub fn pause(&self) {
        let current = self.current_pts_us();
        self.anchor_pts_us.store(current, Ordering::Release);
        *self.anchor_wall.lock().unwrap() = Instant::now();
        self.running.store(false, Ordering::Release);
    }

    /// Resume: start the clock ticking from the current anchor.
    pub fn resume(&self) {
        *self.anchor_wall.lock().unwrap() = Instant::now();
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
        *self.anchor_wall.lock().unwrap() = Instant::now();
        self.speed.store(speed.to_bits(), Ordering::Release);
    }
}
