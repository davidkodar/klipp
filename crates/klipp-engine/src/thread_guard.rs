//! Panic containment for long-running engine threads.
//!
//! Decoder, mixer, and feeder threads call into ffmpeg, cpal, wgpu,
//! and resampler code that can panic on malformed input or internal
//! invariant violations. Without containment, a panic on a background
//! thread unwinds silently — `thread::spawn` swallows the payload and
//! only surfaces it if someone calls `.join()`. Production paths here
//! either never join (detached stop) or join too late to log context,
//! so playback silently stalls and the user has no diagnostic.
//!
//! [`catch_panic`] wraps a thread body in `catch_unwind`, logs the
//! panic payload via `tracing::error!` tagged with the thread name,
//! and then lets the thread exit cleanly. It does not respawn — the
//! caller's supervisor (engine observing a disconnected channel,
//! stopped flag, or missing frames) decides recovery policy.
//!
//! [`catch_panic_once`] is the realtime-callback variant. cpal audio
//! callbacks fire ~90 times per second; if the callback body keeps
//! panicking, plain [`catch_panic`] would flood the log at that rate.
//! `catch_panic_once` takes a shared [`AtomicBool`] — the first panic
//! logs, every later one stays silent — so a broken invariant produces
//! one diagnostic instead of thousands.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

/// Run `body` with a panic guard. On panic, log the payload tagged
/// with `thread_name` and return normally. Non-panic paths return
/// unchanged.
pub fn catch_panic(thread_name: &str, body: impl FnOnce()) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(body)) {
        let msg = panic_payload_message(&payload);
        tracing::error!("thread '{thread_name}' panicked: {msg}");
    }
}

/// Realtime variant of [`catch_panic`]. Swallows every panic, but only
/// logs the first — subsequent panics flip no state and produce no log
/// line. Use for cpal / OS audio callbacks and any loop whose body can
/// fire hundreds of times per second, where log flooding on a
/// repeating panic would hide every other diagnostic.
pub fn catch_panic_once(thread_name: &str, logged: &AtomicBool, body: impl FnOnce()) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(body)) {
        if !logged.swap(true, Ordering::Relaxed) {
            let msg = panic_payload_message(&payload);
            tracing::error!(
                "callback '{thread_name}' panicked (logging only the first occurrence): {msg}",
            );
        }
    }
}

fn panic_payload_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catch_panic_runs_body_when_no_panic() {
        let mut flag = false;
        catch_panic("test-ok", || {
            flag = true;
        });
        assert!(flag, "body must run on the success path");
    }

    #[test]
    fn catch_panic_swallows_panic_and_returns() {
        // Before / after markers prove we returned from the guarded
        // block instead of unwinding out of it.
        let before = std::time::Instant::now();
        catch_panic("test-panic", || {
            panic!("boom");
        });
        let after = std::time::Instant::now();
        assert!(after >= before, "control flow must continue past the guard");
    }

    #[test]
    fn catch_panic_once_only_logs_first_panic() {
        // Logging itself is side-effectful and not readily observable
        // in this crate's test harness, so we test the flag side-effect
        // — the first panic should flip it, subsequent ones must not
        // panic through (still swallowed) and the flag stays true.
        let logged = AtomicBool::new(false);
        catch_panic_once("test-once", &logged, || panic!("first"));
        assert!(logged.load(Ordering::Relaxed), "first panic must flip the flag");
        catch_panic_once("test-once", &logged, || panic!("second"));
        assert!(
            logged.load(Ordering::Relaxed),
            "flag stays set on the second panic (no reset)",
        );
    }

    #[test]
    fn catch_panic_once_runs_body_when_no_panic() {
        let logged = AtomicBool::new(false);
        let mut flag = false;
        catch_panic_once("test-once-ok", &logged, || {
            flag = true;
        });
        assert!(flag, "body must run on the success path");
        assert!(
            !logged.load(Ordering::Relaxed),
            "logged flag must stay false when no panic occurred",
        );
    }
}
