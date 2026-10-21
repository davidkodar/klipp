//! Blocking effects-pipeline readbacks, separate from engine frame conversion.
use std::sync::Mutex;
use std::time::Instant;

/// Cumulative attempts. Bytes count successful downloads; time includes queue
/// submission, waiting and CPU copy-out, not an isolated GPU execution time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ReadbackStats {
    pub count: u64,
    pub bytes: u64,
    pub total_us: u64,
    pub failures: u64,
}

#[derive(Default)]
pub(super) struct Counters(Mutex<ReadbackStats>);
static TOTAL: Counters = Counters(Mutex::new(ReadbackStats {
    count: 0,
    bytes: 0,
    total_us: 0,
    failures: 0,
}));

/// Process totals for blocking effects-pipeline downloads (including title
/// alpha masks). Excludes engine frame conversions, async export staging and
/// other GPU subsystems. These totals must not be added to frame render time.
pub fn readback_stats() -> ReadbackStats {
    TOTAL.snapshot()
}

thread_local! {
    static THREAD: Counters = Counters::default();
}

/// [`readback_stats`] for the calling thread alone. Blocking downloads run
/// on the thread that asks for them, so tests running side by side can
/// count their own without seeing each other's.
pub fn thread_readback_stats() -> ReadbackStats {
    THREAD.with(Counters::snapshot)
}

impl Counters {
    pub(super) fn snapshot(&self) -> ReadbackStats {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn record(&self, bytes: u64, us: u64, succeeded: bool) {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        s.count += 1;
        s.total_us += us;
        if succeeded {
            s.bytes += bytes;
        } else {
            s.failures += 1;
        }
    }
    pub(super) fn begin(&self, bytes: u64) -> Attempt<'_> {
        Attempt {
            counters: self,
            bytes,
            start: Instant::now(),
            succeeded: false,
        }
    }
}

pub(super) struct Attempt<'a> {
    counters: &'a Counters,
    bytes: u64,
    start: Instant,
    pub(super) succeeded: bool,
}
impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        let us = self.start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        self.counters.record(self.bytes, us, self.succeeded);
        TOTAL.record(self.bytes, us, self.succeeded);
        THREAD.with(|t| t.record(self.bytes, us, self.succeeded));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_attempts_do_not_claim_transferred_bytes() {
        let c = Counters::default();
        {
            let mut attempt = c.begin(128);
            attempt.succeeded = true;
        }
        {
            let _failed = c.begin(256);
        }
        let s = c.snapshot();
        assert_eq!((s.count, s.bytes, s.failures), (2, 128, 1));
    }
}
