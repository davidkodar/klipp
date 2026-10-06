//! Frame-sized buffers leased to the values of a run (#1089, phase 2b M2,
//! step 9b).
//!
//! The GPU chain kept its side values in three buffers of its own: the
//! picture a secondary or a mix keeps, a composite's second picture, and
//! the one matte. So two values of a kind could never be alive at once,
//! and a second matte flushed the chain. A run now leases a slot for
//! each such value when it is made and returns it after its last reader.
//!
//! The rules a lease keeps:
//! - Slots are kept across runs, keyed by size and use, so a steady state
//!   creates no buffers (and no bind groups for them).
//! - A slot the GPU wrote can be leased again in the same encoder: the
//!   passes are ordered.
//! - A slot written from the CPU (`queue.write_buffer`, which lands
//!   before the whole encoder, not in sequence with it) is held after
//!   its return until the encoder holding its readers has been submitted
//!   ([`SlotPool::submitted`]). Otherwise a later upload into it would
//!   decide what an earlier reader saw.
//! - At the end of a run every slot is free again, and a slot no run has
//!   leased for [`SlotPool::EVICT_AFTER_RUNS`] runs is dropped: sizes
//!   come and go (the viewer, thumbnails, an export at its delivery
//!   size), and at 4K one slot is about 127 MiB.
//! - At most `max_slots` slots exist at once, whatever their sizes; a
//!   lease past that is refused and the caller segments.
//!
//! In M2 the pool serves the new mask and mix values only. The chain's
//! own kept picture, composite input and matte buffers stay the legacy
//! paths' (decision D3), and no slot is ever one of them.
//!
//! The pool is generic over the buffer so its bookkeeping is tested
//! without a GPU; the pipeline's is a `SlotPool<wgpu::Buffer>`.

/// What a slot holds.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotUsage {
    /// A picture: linear RGBA.
    Image,
    /// A matte: a picture whose red channel is the coverage.
    Matte,
}

/// The size and use a slot was made for; a lease takes only a slot made
/// for the same.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotKey {
    pub width: u32,
    pub height: u32,
    pub usage: SlotUsage,
}

/// A slot of a [`SlotPool`].
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotId(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Free,
    Leased { cpu_written: bool },
    /// Returned, but written from the CPU for readers not yet submitted.
    Held,
}

struct Slot<B> {
    key: SlotKey,
    buf: B,
    state: State,
    /// The run that last leased it.
    last_run: u64,
}

/// Slots leased to a run's values; see the module's rules.
#[doc(hidden)]
pub struct SlotPool<B> {
    slots: Vec<Slot<B>>,
    leased: usize,
    peak: usize,
    created: u64,
    run: u64,
    max_slots: usize,
}

/// A lease the budget refused: the run would need more slots at once
/// than `max_slots`.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverBudget {
    pub max_slots: usize,
}

/// `make(key)`, unless the pool is at its budget with no free slot to
/// give up.
fn make_checked<B>(slots: &[Slot<B>], max_slots: usize, key: SlotKey, make: impl FnOnce(SlotKey) -> B) -> Result<B, OverBudget> {
    if slots.len() >= max_slots && !slots.iter().any(|s| s.state == State::Free) {
        return Err(OverBudget { max_slots });
    }
    Ok(make(key))
}

impl<B> SlotPool<B> {
    /// Runs a slot may go unleased before it is dropped.
    pub const EVICT_AFTER_RUNS: u64 = 240;

    /// A pool holding at most `max_slots` slots, of all sizes together.
    pub fn new(max_slots: usize) -> Self {
        Self { slots: Vec::new(), leased: 0, peak: 0, created: 0, run: 0, max_slots }
    }

    /// A free slot made for `key`, or a new one from `make`; refused when
    /// every slot the budget allows is leased.
    pub fn lease(&mut self, key: SlotKey, make: impl FnOnce(SlotKey) -> B) -> Result<SlotId, OverBudget> {
        let id = match self.slots.iter().position(|s| s.key == key && s.state == State::Free) {
            Some(i) => i,
            None => {
                let slot = Slot { key, buf: make_checked(&self.slots, self.max_slots, key, make)?, state: State::Free, last_run: self.run };
                self.created += 1;
                if self.slots.len() < self.max_slots {
                    self.slots.push(slot);
                    self.slots.len() - 1
                } else {
                    // At the budget: a free slot of another size makes room,
                    // replaced where it is, so the ids of leased slots stay.
                    let free = self.slots.iter().position(|s| s.state == State::Free).expect("make_checked found one");
                    self.slots[free] = slot;
                    free
                }
            }
        };
        let slot = &mut self.slots[id];
        slot.state = State::Leased { cpu_written: false };
        slot.last_run = self.run;
        self.leased += 1;
        self.peak = self.peak.max(self.leased);
        Ok(SlotId(id))
    }

    /// Say that `id` was written from the CPU, so its return waits for
    /// the next submit.
    pub fn mark_cpu_written(&mut self, id: SlotId) {
        if let State::Leased { cpu_written } = &mut self.slots[id.0].state {
            *cpu_written = true;
        }
    }

    /// Return `id` after its last reader.
    pub fn release(&mut self, id: SlotId) {
        let slot = &mut self.slots[id.0];
        if let State::Leased { cpu_written } = slot.state {
            slot.state = if cpu_written { State::Held } else { State::Free };
            self.leased -= 1;
        }
    }

    /// The encoder holding every reader so far has been submitted: slots
    /// held for it are free.
    pub fn submitted(&mut self) {
        for s in &mut self.slots {
            if s.state == State::Held {
                s.state = State::Free;
            }
        }
    }

    /// The run is over (its work submitted): every slot is free, and one
    /// unleased for [`Self::EVICT_AFTER_RUNS`] runs is dropped. Gives
    /// back the most slots leased at once during it.
    pub fn end_run(&mut self) -> usize {
        for s in &mut self.slots {
            s.state = State::Free;
        }
        self.leased = 0;
        let run = self.run;
        self.slots.retain(|s| run.saturating_sub(s.last_run) < Self::EVICT_AFTER_RUNS);
        self.run += 1;
        std::mem::take(&mut self.peak)
    }

    /// Slots held now, of all sizes.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn buffer(&self, id: SlotId) -> &B {
        &self.slots[id.0].buf
    }

    /// Slots made since the pool was made: flat in a steady state.
    pub fn created(&self) -> u64 {
        self.created
    }

    /// Slots leased now.
    pub fn leased(&self) -> usize {
        self.leased
    }

    /// Drop every slot not made for one of `keys` (a frame size change).
    pub fn retain_keys(&mut self, keys: &[SlotKey]) {
        debug_assert_eq!(self.leased, 0, "slots dropped while leased");
        self.slots.retain(|s| keys.contains(&s.key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMG: SlotKey = SlotKey { width: 4, height: 2, usage: SlotUsage::Image };
    const MATTE: SlotKey = SlotKey { width: 4, height: 2, usage: SlotUsage::Matte };

    fn pool() -> (SlotPool<u32>, impl FnMut(SlotKey) -> u32) {
        let mut n = 0;
        (SlotPool::new(16), move |_| {
            n += 1;
            n
        })
    }

    #[test]
    fn a_returned_slot_is_leased_again_and_a_steady_state_makes_nothing() {
        let (mut p, mut make) = pool();
        for _ in 0..5 {
            let a = p.lease(IMG, &mut make).unwrap();
            p.release(a);
            let b = p.lease(IMG, &mut make).unwrap();
            assert_eq!(a, b, "the GPU-written slot is reused in the same run");
            p.release(b);
            p.end_run();
        }
        assert_eq!(p.created(), 1);
    }

    #[test]
    fn values_alive_at_once_get_slots_of_their_own() {
        let (mut p, mut make) = pool();
        let m1 = p.lease(MATTE, &mut make).unwrap();
        let m2 = p.lease(MATTE, &mut make).unwrap();
        let img = p.lease(IMG, &mut make).unwrap();
        assert!(m1 != m2 && *p.buffer(m1) != *p.buffer(m2));
        assert_ne!(*p.buffer(img), *p.buffer(m1), "a different use is a different slot");
        assert_eq!(p.leased(), 3);
        p.release(m1);
        p.release(m2);
        p.release(img);
        assert_eq!(p.end_run(), 3, "the peak");
        assert_eq!(p.end_run(), 0, "and it starts over each run");
    }

    #[test]
    fn a_cpu_written_slot_waits_for_the_submit_before_it_is_leased_again() {
        let (mut p, mut make) = pool();
        let up = p.lease(MATTE, &mut make).unwrap();
        p.mark_cpu_written(up);
        p.release(up);
        let next = p.lease(MATTE, &mut make).unwrap();
        assert_ne!(up, next, "held until its readers are submitted");
        p.release(next);
        p.submitted();
        assert_eq!(p.lease(MATTE, &mut make).unwrap(), up, "free after the submit");
        assert_eq!(p.created(), 2);
    }

    #[test]
    fn sizes_held_in_turn_do_not_grow_the_pool() {
        let (mut p, mut make) = pool();
        let thumb = SlotKey { width: 2, height: 1, ..IMG };
        for _ in 0..50 {
            for key in [IMG, thumb] {
                let a = p.lease(key, &mut make).unwrap();
                let b = p.lease(key, &mut make).unwrap();
                p.release(a);
                p.release(b);
                p.end_run();
            }
        }
        assert_eq!(p.len(), 4, "two slots per size, kept");
        assert_eq!(p.created(), 4);
    }

    #[test]
    fn a_size_no_run_uses_is_dropped() {
        let (mut p, mut make) = pool();
        let old = SlotKey { width: 2, height: 1, ..IMG };
        let a = p.lease(old, &mut make).unwrap();
        p.release(a);
        p.end_run();
        for _ in 0..SlotPool::<u32>::EVICT_AFTER_RUNS {
            let b = p.lease(IMG, &mut make).unwrap();
            p.release(b);
            p.end_run();
        }
        assert_eq!(p.len(), 1, "only the size in use is left");
    }

    #[test]
    fn the_budget_counts_every_size_and_gives_up_free_slots_first() {
        let mut n = 0;
        let mut make = move |_| {
            n += 1;
            n
        };
        let mut p = SlotPool::new(2);
        let a = p.lease(IMG, &mut make).unwrap();
        let m = p.lease(MATTE, &mut make).unwrap();
        let other = SlotKey { width: 8, height: 4, ..IMG };
        assert_eq!(p.lease(other, &mut make), Err(OverBudget { max_slots: 2 }), "both leased: refused");
        p.release(a);
        let o = p.lease(other, &mut make).unwrap();
        assert_eq!(o, a, "the free slot is replaced where it was");
        assert_eq!(*p.buffer(m), 2, "and the leased one is untouched");
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn a_frame_size_change_drops_the_old_sizes() {
        let (mut p, mut make) = pool();
        let a = p.lease(IMG, &mut make).unwrap();
        p.release(a);
        p.end_run();
        let big = SlotKey { width: 8, height: 4, ..IMG };
        p.retain_keys(&[big]);
        p.lease(big, &mut make).unwrap();
        assert_eq!(p.created(), 2);
    }
}
