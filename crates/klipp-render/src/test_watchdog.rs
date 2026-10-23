//! A limit on how long a test that opens decoders, encoders or the GPU
//! may run (#2372). The lib tests once sat for 1h49m, every thread
//! waiting: a thread stuck in the GPU driver is not one a timeout or a
//! panic can stop, and the tests behind it wait on its locks (the serial
//! lock, the shared GPU device's). Past the limit the watchdog names the
//! test and aborts the run, so it fails, and the core dump the abort
//! leaves holds every thread's stack.

use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::Duration;

/// Far past what any of these tests takes on a loaded machine: the
/// whole binary runs in about ten seconds.
const LIMIT: Duration = Duration::from_secs(180);

/// Watches the test that started it, until dropped.
pub(crate) struct Watchdog {
    _done: Sender<()>,
}

impl Watchdog {
    /// Watches the running test, named by its thread as the test harness
    /// names it.
    pub(crate) fn start() -> Self {
        Self::start_with(LIMIT, |test| {
            eprintln!(
                "{test} is still running after {} s: stuck in a decoder, an encoder or the GPU driver (#2372). \
                 Aborting the run; the core dump holds every thread's stack (coredumpctl debug).",
                LIMIT.as_secs()
            );
            std::process::abort();
        })
    }

    fn start_with(limit: Duration, stuck: fn(&str)) -> Self {
        let test = std::thread::current().name().unwrap_or("a test").to_string();
        let (done, wait) = channel::<()>();
        let watched = test.clone();
        let spawned = std::thread::Builder::new().name("test watchdog".into()).spawn(move || {
            // Dropped, the guard's sender ends the wait as disconnected.
            if let Err(RecvTimeoutError::Timeout) = wait.recv_timeout(limit) {
                stuck(&watched);
            }
        });
        if let Err(e) = spawned {
            eprintln!("{test} runs unwatched: {e}");
        }
        Self { _done: done }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    static STUCK: AtomicBool = AtomicBool::new(false);
    static FINISHED: AtomicBool = AtomicBool::new(false);

    /// A test past its limit is reported, by its own name.
    #[test]
    fn a_test_past_its_limit_is_reported() {
        let _watch = Watchdog::start_with(Duration::from_millis(20), |test| {
            assert!(test.ends_with("a_test_past_its_limit_is_reported"), "{test}");
            STUCK.store(true, Ordering::SeqCst);
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !STUCK.load(Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline, "the watchdog never fired");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// One that ends in time is not.
    #[test]
    fn a_test_that_ends_is_let_go() {
        drop(Watchdog::start_with(Duration::from_millis(200), |_| FINISHED.store(true, Ordering::SeqCst)));
        std::thread::sleep(Duration::from_millis(400));
        assert!(!FINISHED.load(Ordering::SeqCst), "the watchdog fired for a test that had ended");
    }
}
