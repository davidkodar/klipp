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

use std::panic::{catch_unwind, AssertUnwindSafe};

/// Run `body` with a panic guard. On panic, log the payload tagged
/// with `thread_name` and return normally. Non-panic paths return
/// unchanged.
pub fn catch_panic(thread_name: &str, body: impl FnOnce()) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(body)) {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        tracing::error!("thread '{thread_name}' panicked: {msg}");
    }
}
