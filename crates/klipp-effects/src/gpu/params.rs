//! Uniform slots for passes recorded into one encoder.
//!
//! wgpu orders `queue.write_buffer` before the *next* submit, not in
//! sequence with the passes recorded around it. Recording
//! `write(params A); pass A; write(params B); pass B` into one encoder
//! and submitting once would have both passes read B. So each recorded
//! pass claims a buffer of its own from a ring, and the ring may only be
//! reset once everything recorded against it has been submitted.

/// A ring of uniform buffers, one per pass recorded since the last
/// [`Self::reset`]. The chain's ring is one (its bind-group pools are
/// indexed by slot); an effect run's is another, so neither takes the
/// other's slots.
#[doc(hidden)]
pub struct ParamSlots {
    bufs: Vec<wgpu::Buffer>,
    next: usize,
}

impl ParamSlots {
    /// `len` buffers of `size` bytes each, usable as uniforms.
    pub(crate) fn new(device: &wgpu::Device, len: usize, size: u64, label: &str) -> Self {
        let bufs = (0..len)
            .map(|i| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("{label}_{i}")),
                    size,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        Self { bufs, next: 0 }
    }

    /// How many slots the ring has.
    pub fn len(&self) -> usize {
        self.bufs.len()
    }

    /// Whether the ring has no slots at all.
    pub fn is_empty(&self) -> bool {
        self.bufs.is_empty()
    }

    /// Slots left before the work recorded against the ring must be
    /// submitted and the ring reset.
    pub fn remaining(&self) -> usize {
        self.bufs.len() - self.next
    }

    /// Start over. Only once everything recorded against the slots has
    /// been submitted: a pass still waiting would read what the next
    /// claim writes.
    pub fn reset(&mut self) {
        self.next = 0;
    }

    /// The next slot's index, or `None` when the ring is used up.
    pub fn claim_index(&mut self) -> Option<usize> {
        (self.next < self.bufs.len()).then(|| {
            self.next += 1;
            self.next - 1
        })
    }

    /// Claim the next slot and write `data` into it: its index, or
    /// `None` (nothing written) when the ring is used up.
    pub fn claim<T: bytemuck::Pod>(&mut self, queue: &wgpu::Queue, data: &T) -> Option<usize> {
        let slot = self.claim_index()?;
        queue.write_buffer(&self.bufs[slot], 0, bytemuck::bytes_of(data));
        Some(slot)
    }

    /// The buffer of slot `slot`.
    pub fn buffer(&self, slot: usize) -> &wgpu::Buffer {
        &self.bufs[slot]
    }
}
