//! A bound on how many of a kind of job run at once: a job takes a slot
//! before it starts its work and gives it back when the guard drops; the
//! rest wait their turn, in the order they asked.

use std::sync::{Condvar, Mutex};

/// A counted set of slots.
pub struct Slots {
    limit: usize,
    /// (slots taken, next ticket to hand out, ticket now served).
    state: Mutex<(usize, u64, u64)>,
    freed: Condvar,
}

/// A slot held: given back when dropped.
pub struct SlotGuard<'a> {
    slots: &'a Slots,
}

impl Slots {
    /// `limit` slots, at least one.
    pub const fn new(limit: usize) -> Self {
        Self { limit: if limit == 0 { 1 } else { limit }, state: Mutex::new((0, 0, 0)), freed: Condvar::new() }
    }

    /// One slot for each `per` cores, at least one: what heavy encodes,
    /// which use several cores each, can run together.
    pub fn per_cores(per: usize) -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        Self::new(cores / per.max(1))
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Wait for a slot, first come first served.
    pub fn acquire(&self) -> SlotGuard<'_> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let ticket = s.1;
        s.1 += 1;
        while s.0 >= self.limit || s.2 != ticket {
            s = self.freed.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        s.0 += 1;
        s.2 += 1;
        drop(s);
        // The next ticket may now be served too, while slots remain.
        self.freed.notify_all();
        SlotGuard { slots: self }
    }

    /// Slots taken now.
    pub fn in_use(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0
    }
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        let mut s = self.slots.state.lock().unwrap_or_else(|e| e.into_inner());
        s.0 -= 1;
        drop(s);
        self.slots.freed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Twelve jobs through two slots: never more than two at once, and
    /// every one runs.
    #[test]
    fn no_more_than_the_limit_run_together() {
        let slots = Arc::new(Slots::new(2));
        let (now, most, done) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let handles: Vec<_> = (0..12)
            .map(|_| {
                let (slots, now, most, done) = (slots.clone(), now.clone(), most.clone(), done.clone());
                std::thread::spawn(move || {
                    let _slot = slots.acquire();
                    let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    now.fetch_sub(1, Ordering::SeqCst);
                    done.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(done.load(Ordering::SeqCst), 12);
        assert!(most.load(Ordering::SeqCst) <= 2, "at most two at once: {}", most.load(Ordering::SeqCst));
        assert_eq!(slots.in_use(), 0);
    }

    #[test]
    fn there_is_always_a_slot() {
        assert_eq!(Slots::new(0).limit(), 1);
        assert!(Slots::per_cores(1_000_000).limit() >= 1);
    }
}
