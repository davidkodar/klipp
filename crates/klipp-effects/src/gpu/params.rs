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
    /// A storage table per slot ([`Self::TABLE_BYTES`] each, at the slot's
    /// own offset), written with its params: what a pass reads besides
    /// them that a uniform can't hold (Posterize's band edges, #1443).
    tables: wgpu::Buffer,
    /// What each slot's table holds, by its caller's key: a claim whose
    /// slot already holds that table writes nothing (a frame after frame
    /// of one Posterize uploads its ladder once).
    table_keys: Vec<Option<u64>>,
    next: usize,
}

impl ParamSlots {
    /// `len` buffers of `size` bytes each, usable as uniforms.
    pub fn new(device: &wgpu::Device, len: usize, size: u64, label: &str) -> Self {
        let bufs: Vec<wgpu::Buffer> = (0..len)
            .map(|i| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("{label}_{i}")),
                    size,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        let tables = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label}_tables")),
            size: Self::TABLE_BYTES * len.max(1) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { table_keys: vec![None; bufs.len()], bufs, tables, next: 0 }
    }

    /// The bytes of one slot's table: 512 floats, a multiple of every
    /// storage offset alignment wgpu allows (256 at most).
    pub const TABLE_BYTES: u64 = 2048;

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

    /// Claim the next slot, writing `data` into it and the table `key`
    /// names into its table (at most [`Self::TABLE_BYTES`]), unless the
    /// slot holds that table already, when `table` isn't even made: its
    /// index, or `None` when the ring is used up. Only these claims write
    /// tables, each into its own slot's, so a table kept there is the one
    /// a pass of an earlier frame read.
    pub fn claim_with_table<T: bytemuck::Pod>(&mut self, queue: &wgpu::Queue, data: &T, key: u64, table: impl FnOnce() -> Vec<f32>) -> Option<usize> {
        let slot = self.claim(queue, data)?;
        if self.table_keys[slot] != Some(key) {
            let table = table();
            let bytes: &[u8] = bytemuck::cast_slice(&table);
            debug_assert!(bytes.len() as u64 <= Self::TABLE_BYTES);
            queue.write_buffer(&self.tables, slot as u64 * Self::TABLE_BYTES, bytes);
            self.table_keys[slot] = Some(key);
        }
        Some(slot)
    }

    /// Slot `slot`'s table, as a binding.
    pub fn table(&self, slot: usize) -> wgpu::BufferBinding<'_> {
        wgpu::BufferBinding { buffer: &self.tables, offset: slot as u64 * Self::TABLE_BYTES, size: std::num::NonZeroU64::new(Self::TABLE_BYTES) }
    }

    /// The buffer of slot `slot`.
    pub fn buffer(&self, slot: usize) -> &wgpu::Buffer {
        &self.bufs[slot]
    }
}
