//! Cooperative pause primitive for thread-based background jobs.
//!
//! A worker thread checks `wait_while_paused()` between work chunks.
//! When the scheduler calls `pause()`, the next check blocks the
//! worker on a condvar — no busy spin, no CPU wasted while paused.
//! `resume()` wakes it instantly.
//!
//! `cancel()` is a one-way flag: once set, `wait_while_paused`
//! returns immediately so the worker can observe `is_cancelled()` and
//! exit. Cancellation does NOT auto-resume — the worker is responsible
//! for unwinding cleanly.

use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
struct PauseState {
    paused: bool,
    cancelled: bool,
}

pub struct PauseToken {
    state: Mutex<PauseState>,
    cv: Condvar,
}

impl PauseToken {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PauseState::default()),
            cv: Condvar::new(),
        })
    }

    /// Mark the token as paused. Workers about to call
    /// `wait_while_paused` will block until `resume()` or `cancel()`.
    pub fn pause(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.paused = true;
        // No notify needed — pausing only blocks future waiters.
    }

    /// Wake any threads currently blocked in `wait_while_paused`.
    /// Idempotent: calling on a non-paused token is a no-op.
    pub fn resume(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.paused = false;
        self.cv.notify_all();
    }

    /// Permanently cancel the token. Wakes all waiters. Subsequent
    /// `wait_while_paused` calls return immediately. Workers should
    /// call `is_cancelled()` after waking to decide whether to exit.
    pub fn cancel(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.cancelled = true;
        s.paused = false;
        self.cv.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.cancelled
    }

    pub fn is_paused(&self) -> bool {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.paused
    }

    /// Block while the token is paused. Returns immediately if the
    /// token is unpaused or has been cancelled. Workers call this
    /// between work chunks so the scheduler can yield them to the
    /// foreground without burning CPU.
    pub fn wait_while_paused(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while s.paused && !s.cancelled {
            s = self.cv.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn fresh_token_does_not_block() {
        let t = PauseToken::new();
        let start = Instant::now();
        t.wait_while_paused();
        assert!(start.elapsed() < Duration::from_millis(10));
    }

    #[test]
    fn pause_blocks_resume_unblocks() {
        let token = PauseToken::new();
        token.pause();

        let counter = Arc::new(AtomicU32::new(0));
        let counter_w = Arc::clone(&counter);
        let token_w = Arc::clone(&token);
        let h = std::thread::spawn(move || {
            token_w.wait_while_paused();
            counter_w.store(1, Ordering::Relaxed);
        });

        // Worker should be blocked — counter stays 0 for a beat.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(counter.load(Ordering::Relaxed), 0,
            "worker must remain blocked while token is paused");

        token.resume();
        h.join().unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 1,
            "worker must run after resume");
    }

    #[test]
    fn cancel_releases_waiters() {
        let token = PauseToken::new();
        token.pause();

        let token_w = Arc::clone(&token);
        let h = std::thread::spawn(move || {
            token_w.wait_while_paused();
            // Worker observes cancellation and exits.
            assert!(token_w.is_cancelled());
        });

        std::thread::sleep(Duration::from_millis(20));
        token.cancel();
        // Join must not hang.
        h.join().unwrap();
        assert!(token.is_cancelled());
    }

    #[test]
    fn pause_resume_cycle_in_loop() {
        // The realistic worker pattern: loop running chunks with a
        // pause check between each. Pause + resume mid-flight should
        // not lose chunks.
        let token = PauseToken::new();
        let counter = Arc::new(AtomicU32::new(0));
        let counter_w = Arc::clone(&counter);
        let token_w = Arc::clone(&token);

        let h = std::thread::spawn(move || {
            for _ in 0..50 {
                token_w.wait_while_paused();
                if token_w.is_cancelled() { break; }
                counter_w.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(2));
            }
        });

        // Let it run a few chunks, pause, observe stop, resume.
        std::thread::sleep(Duration::from_millis(20));
        token.pause();
        let mid = counter.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(50));
        let after_pause = counter.load(Ordering::Relaxed);
        // At most one extra chunk could have been in flight when
        // pause was called (already past wait_while_paused for that
        // iter). So `after_pause - mid` is 0 or 1.
        assert!(after_pause - mid <= 1,
            "paused worker advanced too far: {mid} -> {after_pause}");
        token.resume();
        h.join().unwrap();
        // Worker completed all 50 chunks.
        assert_eq!(counter.load(Ordering::Relaxed), 50);
    }

    #[test]
    fn is_paused_reflects_state() {
        let t = PauseToken::new();
        assert!(!t.is_paused());
        t.pause();
        assert!(t.is_paused());
        t.resume();
        assert!(!t.is_paused());
    }
}
