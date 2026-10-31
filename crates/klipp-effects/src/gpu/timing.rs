//! How long the GPU takes over parts of a frame (#3207), by timestamps
//! the queue writes as it reaches them: what each track costs the GPU.
//!
//! One frame is timed at a time. Each mark hands the queue what was
//! recorded before it and writes its timestamp in a submit of its own, so
//! marks fall between whole pieces of work in the order the queue runs
//! them, whatever encoders that work used. The times are resolved at the
//! frame's end and read back a frame or more later, never waited for.
//! On a device without timestamp queries nothing is timed.

use std::sync::{Arc, Mutex};

use super::GpuEffectsPipeline;

/// The most marks a timed frame takes; past it, marks are left out.
const MARKS: u32 = 256;

/// The features timing needs: timestamps, and written between passes.
pub const FEATURES: wgpu::Features = wgpu::Features::TIMESTAMP_QUERY.union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);

enum Reading {
    /// Resolved into the read buffer: `n` marks, not yet asked for.
    Copied(u32),
    /// Asked for: mapped once the flag says so.
    Mapping(u32, Arc<Mutex<Option<bool>>>),
}

pub(crate) struct Timing {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    /// Nanoseconds a timestamp tick is.
    period: f64,
    /// Marks written in the frame being timed, `None` outside one.
    used: Option<u32>,
    reading: Option<Reading>,
}

impl GpuEffectsPipeline {
    /// Whether the device times the GPU's work.
    pub fn timing_available(&self) -> bool {
        self.device.features().contains(FEATURES)
    }

    /// Start timing this frame: `false` on a device without timestamps,
    /// or while the last timed frame's times are still to be read.
    pub fn time_begin(&self) -> bool {
        if !self.timing_available() {
            return false;
        }
        let mut t = self.timing.borrow_mut();
        let t = t.get_or_insert_with(|| {
            let bytes = u64::from(MARKS) * 8;
            Timing {
                set: self.device.create_query_set(&wgpu::QuerySetDescriptor { label: Some("klipp-timing"), ty: wgpu::QueryType::Timestamp, count: MARKS }),
                resolve: self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("klipp-timing-resolve"), size: bytes, usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false }),
                read: self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("klipp-timing-read"), size: bytes, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }),
                period: f64::from(self.queue.get_timestamp_period()),
                used: None,
                reading: None,
            }
        });
        if t.reading.is_some() || t.used.is_some() {
            return false;
        }
        t.used = Some(0);
        true
    }

    /// A timestamp where the queue is now, after everything recorded so
    /// far: its index, `None` outside a timed frame or past [`MARKS`].
    pub fn time_mark(&self) -> Option<u32> {
        let mut t = self.timing.borrow_mut();
        let t = t.as_mut()?;
        let i = t.used.filter(|i| *i < MARKS)?;
        self.flush();
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("klipp-timing-mark") });
        enc.write_timestamp(&t.set, i);
        self.submit_now(enc);
        t.used = Some(i + 1);
        Some(i)
    }

    /// End the timed frame: its marks resolved and copied to be read.
    pub fn time_end(&self) {
        let mut t = self.timing.borrow_mut();
        let Some(t) = t.as_mut() else { return };
        let Some(n) = t.used.take() else { return };
        if n == 0 {
            return;
        }
        self.flush();
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("klipp-timing-resolve") });
        enc.resolve_query_set(&t.set, 0..n, &t.resolve, 0);
        enc.copy_buffer_to_buffer(&t.resolve, 0, &t.read, 0, u64::from(n) * 8);
        self.submit_now(enc);
        t.reading = Some(Reading::Copied(n));
    }

    /// The last timed frame's marks, nanoseconds from the first, once the
    /// GPU has them: `None` until then. Never waits.
    pub fn take_times(&self) -> Option<Vec<f64>> {
        let mut guard = self.timing.borrow_mut();
        let t = guard.as_mut()?;
        match t.reading.take()? {
            Reading::Copied(n) => {
                let flag: Arc<Mutex<Option<bool>>> = Arc::default();
                let set = Arc::clone(&flag);
                t.read.slice(..u64::from(n) * 8).map_async(wgpu::MapMode::Read, move |r| {
                    *set.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.is_ok());
                });
                t.reading = Some(Reading::Mapping(n, flag));
                None
            }
            Reading::Mapping(n, flag) => {
                self.device.poll(wgpu::Maintain::Poll);
                let mapped = *flag.lock().unwrap_or_else(|e| e.into_inner());
                match mapped {
                    None => {
                        t.reading = Some(Reading::Mapping(n, flag));
                        None
                    }
                    Some(false) => None,
                    Some(true) => {
                        let ticks: Vec<u64> = {
                            let view = t.read.slice(..u64::from(n) * 8).get_mapped_range();
                            view.chunks_exact(8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8]))).collect()
                        };
                        t.read.unmap();
                        let first = ticks.first().copied().unwrap_or(0);
                        Some(ticks.iter().map(|k| k.saturating_sub(first) as f64 * t.period).collect())
                    }
                }
            }
        }
    }
}
