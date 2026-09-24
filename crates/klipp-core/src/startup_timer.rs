//! One-shot timer for startup performance instrumentation.
//!
//! Wrap an init step in a `LogTimer` to print its elapsed wall time on Drop.
//! All entries are tagged with the `STARTUP_PERF` prefix so they can be greppable
//! out of `klipp-debug.log`:
//!
//! ```ignore
//! use klipp_core::startup_timer::LogTimer;
//!
//! fn init() {
//!     let _t = LogTimer::new("PlaybackEngine::new");
//!     // ...heavy work...
//! } // logs: "STARTUP_PERF PlaybackEngine::new                3220.4 ms"
//! ```
//!
//! `T0` captures the moment the timer module is first touched (typically the
//! start of `main`). Each entry also prints `[T+x.xxx s]` so the absolute
//! offset from process start is visible in addition to the per-step elapsed.
//!
//! This is intended only for one-shot startup paths — do **not** use it in
//! per-frame hot loops.

use std::sync::OnceLock;
use std::time::Instant;
use tracing::info;

/// Process-start anchor — set on first access.
static T0: OnceLock<Instant> = OnceLock::new();

/// Absolute timestamp of "process start" for the startup phase.
/// Lazily initialized on first call so callers don't need an explicit `init()`.
pub fn process_start() -> Instant {
    *T0.get_or_init(Instant::now)
}

/// Force-initialize the process-start anchor to "now".
/// Call this from the very top of `main()` to make `T+x.xxx s` offsets meaningful.
pub fn mark_process_start() {
    let _ = T0.set(Instant::now());
}

/// Lightweight one-shot timer. Prints elapsed wall time on Drop.
///
/// The label is `'static` so we never allocate or format dynamic strings on
/// the startup path. If you need a dynamic label, use `LogTimer::with_owned`.
pub struct LogTimer {
    label: &'static str,
    start: Instant,
}

impl LogTimer {
    pub fn new(label: &'static str) -> Self {
        // Touch the anchor so the first timer also seeds T0.
        let _ = process_start();
        Self {
            label,
            start: Instant::now(),
        }
    }

    /// Stop the timer early and return the elapsed wall time without printing.
    /// Intended for cases where the caller wants to fold this into a larger
    /// computation. Most callers should just rely on the `Drop` impl.
    pub fn elapsed_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
}

impl Drop for LogTimer {
    fn drop(&mut self) {
        let elapsed_ms = self.start.elapsed().as_secs_f64() * 1000.0;
        let t_offset = process_start().elapsed().as_secs_f64();
        info!(
            "STARTUP_PERF [T+{:>6.3}s] {:<48} {:>9.1} ms",
            t_offset, self.label, elapsed_ms
        );
    }
}

/// Print a one-shot marker line (no elapsed time, just the absolute T+x.xxx offset).
/// Useful for "phase boundary" markers like "main entered" or "first frame painted".
pub fn mark(label: &'static str) {
    let t_offset = process_start().elapsed().as_secs_f64();
    info!(
        "STARTUP_PERF [T+{:>6.3}s] {:<48}     -- mark --",
        t_offset, label
    );
}
