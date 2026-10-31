//! What Klipp holds on the GPU, by holder, and what it makes there call
//! by call (#3174): a driver that runs out of memory took the GPU stack
//! away where Klipp could not read the GPU's free memory, and only the
//! reversed window's frames were counted.
//!
//! Held bytes are what each cache keeps now, reported by its owner as it
//! changes ([`Gauge`]); made bytes are what was created fresh for one
//! call and let go after it, counted as it is made. Relaxed atomics, read
//! by the perf report and the memory report test. With the
//! `gpu-memory-report` feature, wgpu's own totals for the device are
//! read beside them: what it has allocated for everyone, so what no
//! holder here owns (frames in flight, buffers let go and not yet freed)
//! is the difference.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// A cache that keeps GPU memory across frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// Decoded frames kept for reuse by the NV12/P010 converters (4 bytes
    /// a pixel, and 4 more of codes for HDR).
    DecodedFrames,
    /// The effect chain's working buffers, `fx_a`/`b`/`c`, the matte and
    /// the read-back staging (16 bytes a pixel each).
    ChainBuffers,
    /// The effect chain's pool slots (16 bytes a pixel each).
    Slots,
    /// The compositing stack's layers and accumulator.
    StackLayers,
    /// Titles' resident sprites.
    Sprites,
    /// Optical flow's kept pairs and its drawn frame.
    FlowPairs,
    /// Neighbour frames for a frame blend or a temporal denoise, decoded
    /// to linear at the chain's size and kept for reuse.
    Neighbours,
}

/// Memory made for one call and let go after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Made {
    /// A frame converted to linear light into a buffer of its own.
    ConvertToLinear,
    /// A decoded frame made past the converters' pool, so not kept.
    DecodedPastPool,
    /// A new optical-flow pair's frames, pyramids and fields.
    FlowPair,
    /// Optical flow's uniform buffers, one a dispatch.
    FlowUniforms,
    /// A neighbour frame decoded to linear (fitted to the chain's size)
    /// or uploaded, for a frame blend or a temporal denoise.
    Neighbour,
}

const HELD: [Held; 7] = [Held::DecodedFrames, Held::ChainBuffers, Held::Slots, Held::StackLayers, Held::Sprites, Held::FlowPairs, Held::Neighbours];
const MADE: [Made; 5] = [Made::ConvertToLinear, Made::DecodedPastPool, Made::FlowPair, Made::FlowUniforms, Made::Neighbour];

static HELD_BYTES: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
/// Per cache: how many buffers, how many of them in use (held by a reader
/// or leased) rather than kept for reuse, the bytes of one (the last
/// reported), and its bytes a pixel (0 where it isn't a picture).
static HELD_COUNT: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
static HELD_IN_USE: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
static HELD_EACH: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
static HELD_BPP: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
static MADE_BYTES: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
static MADE_COUNT: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

impl Held {
    fn index(self) -> usize {
        HELD.iter().position(|h| *h == self).expect("listed")
    }

    pub fn name(self) -> &'static str {
        match self {
            Held::DecodedFrames => "decoded_frames",
            Held::ChainBuffers => "chain_buffers",
            Held::Slots => "slots",
            Held::StackLayers => "stack_layers",
            Held::Sprites => "sprites",
            Held::FlowPairs => "flow_pairs",
            Held::Neighbours => "neighbours",
        }
    }
}

impl Made {
    fn index(self) -> usize {
        MADE.iter().position(|m| *m == self).expect("listed")
    }

    pub fn name(self) -> &'static str {
        match self {
            Made::ConvertToLinear => "convert_to_linear",
            Made::DecodedPastPool => "decoded_past_pool",
            Made::FlowPair => "flow_pair",
            Made::FlowUniforms => "flow_uniforms",
            Made::Neighbour => "neighbour",
        }
    }
}

/// What an owner holds of one cache (#3174): how many buffers, how many of
/// them in use rather than kept for reuse, the bytes of one, and the bytes
/// a pixel of what they hold (0 where it isn't a picture).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Holding {
    pub count: u64,
    pub in_use: u64,
    pub each: u64,
    pub bytes_per_pixel: u64,
}

/// One owner's share of a [`Held`] total: what it reported last, taken
/// back out when it reports again or goes, so owners of one kind add up.
#[derive(Debug)]
pub struct Gauge {
    held: Held,
    last: u64,
    count: u64,
    in_use: u64,
}

/// `slot` moved from `old` to `new`.
fn shift(slot: &AtomicU64, old: u64, new: u64) {
    if new >= old {
        slot.fetch_add(new - old, Relaxed);
    } else {
        slot.fetch_sub(old - new, Relaxed);
    }
}

impl Gauge {
    pub const fn new(held: Held) -> Self {
        Self { held, last: 0, count: 0, in_use: 0 }
    }

    /// The owner holds `bytes` now.
    pub fn report(&mut self, bytes: u64) {
        shift(&HELD_BYTES[self.held.index()], self.last, bytes);
        self.last = bytes;
    }

    /// The owner holds `bytes` now, as `holding` says.
    pub fn report_holding(&mut self, bytes: u64, holding: Holding) {
        self.report(bytes);
        let i = self.held.index();
        shift(&HELD_COUNT[i], self.count, holding.count);
        shift(&HELD_IN_USE[i], self.in_use, holding.in_use);
        (self.count, self.in_use) = (holding.count, holding.in_use);
        if holding.count > 0 {
            HELD_EACH[i].store(holding.each, Relaxed);
            HELD_BPP[i].store(holding.bytes_per_pixel, Relaxed);
        }
    }
}

impl Drop for Gauge {
    fn drop(&mut self) {
        self.report_holding(0, Holding::default());
    }
}

/// The size of the frame decoded onto the GPU last, `width << 32 | height`.
static DECODED_FRAME: AtomicU64 = AtomicU64::new(0);

/// A frame of `width` × `height` was decoded onto the GPU: what the held
/// frames' size is, beside their bytes.
pub fn note_decoded_frame(width: u32, height: u32) {
    DECODED_FRAME.store((u64::from(width) << 32) | u64::from(height), Relaxed);
}

/// `bytes` made fresh for one call of `made`.
pub fn made(made: Made, bytes: u64) {
    MADE_BYTES[made.index()].fetch_add(bytes, Relaxed);
    MADE_COUNT[made.index()].fetch_add(1, Relaxed);
}

/// wgpu's own totals for the device (with `gpu-memory-report`): bytes it
/// has allocated for buffers and textures, and its memory allocations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Driver {
    pub buffer_bytes: u64,
    pub texture_bytes: u64,
    pub allocations: u64,
}

/// One cache in a [`Snapshot`]: its bytes, its buffers and how many are in
/// use, the bytes of one and its pixels (`each / bytes_per_pixel`), and for
/// decoded frames the size of the last one made, width and height.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct HeldNow {
    pub name: &'static str,
    pub bytes: u64,
    pub count: u64,
    pub in_use: u64,
    pub each: u64,
    pub bytes_per_pixel: u64,
    pub size: Option<(u32, u32)>,
}

impl HeldNow {
    /// Pixels in one of its buffers, where it holds pictures.
    pub fn pixels_each(&self) -> Option<u64> {
        (self.bytes_per_pixel > 0).then(|| self.each / self.bytes_per_pixel)
    }
}

/// What is held and made now. What no cache owns, frames in flight and
/// buffers let go and not yet freed by the driver, is the driver's total
/// less `held_total`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Snapshot {
    /// Each cache.
    pub held: Vec<HeldNow>,
    /// What one-call allocations have made since the start: by name, the
    /// bytes and how many.
    pub made: Vec<(&'static str, u64, u64)>,
    /// The held bytes added up.
    pub held_total: u64,
    /// The size of the frame decoded onto the GPU last, width and height.
    pub decoded_frame: Option<(u32, u32)>,
    /// wgpu's totals, where the build counts them.
    pub driver: Option<Driver>,
}

/// What Klipp holds and has made on the GPU now.
pub fn snapshot() -> Snapshot {
    let frame = DECODED_FRAME.load(Relaxed);
    let decoded_frame = (frame != 0).then_some(((frame >> 32) as u32, frame as u32));
    let held: Vec<HeldNow> = HELD
        .iter()
        .map(|h| {
            let i = h.index();
            HeldNow {
                name: h.name(),
                bytes: HELD_BYTES[i].load(Relaxed),
                count: HELD_COUNT[i].load(Relaxed),
                in_use: HELD_IN_USE[i].load(Relaxed),
                each: HELD_EACH[i].load(Relaxed),
                bytes_per_pixel: HELD_BPP[i].load(Relaxed),
                size: decoded_frame.filter(|_| *h == Held::DecodedFrames),
            }
        })
        .collect();
    let made = MADE.iter().map(|m| (m.name(), MADE_BYTES[m.index()].load(Relaxed), MADE_COUNT[m.index()].load(Relaxed))).collect();
    let held_total = held.iter().map(|h| h.bytes).sum();
    Snapshot { held, made, held_total, decoded_frame, driver: driver() }
}

#[cfg(feature = "gpu-memory-report")]
fn driver() -> Option<Driver> {
    let ctx = crate::gpu_context::GpuContext::shared()?;
    let hal = ctx.device.get_internal_counters().hal;
    let read = |c: isize| u64::try_from(c).unwrap_or(0);
    Some(Driver { buffer_bytes: read(hal.buffer_memory.read()), texture_bytes: read(hal.texture_memory.read()), allocations: read(hal.memory_allocations.read()) })
}

#[cfg(not(feature = "gpu-memory-report"))]
fn driver() -> Option<Driver> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #3174: two owners of one kind add up, a report replaces the
    /// owner's last, and an owner gone takes its share out; one-call
    /// allocations add their bytes and count.
    #[test]
    fn gauges_add_up_by_owner_and_made_counts() {
        let before = snapshot();
        let held = |s: &Snapshot| s.held.iter().find(|h| h.name == "sprites").unwrap().bytes;
        let mut a = Gauge::new(Held::Sprites);
        let mut b = Gauge::new(Held::Sprites);
        a.report(100);
        b.report(50);
        assert_eq!(held(&snapshot()) - held(&before), 150);
        a.report(30);
        assert_eq!(held(&snapshot()) - held(&before), 80);
        drop(b);
        assert_eq!(held(&snapshot()) - held(&before), 30);
        drop(a);
        assert_eq!(held(&snapshot()), held(&before));
        // A holding: counts add up by owner, the size of one is the last's.
        let count = |s: &Snapshot| s.held.iter().find(|h| h.name == "flow_pairs").map(|h| (h.count, h.in_use)).unwrap();
        let was = count(&snapshot());
        let mut c = Gauge::new(Held::FlowPairs);
        c.report_holding(300, Holding { count: 3, in_use: 1, each: 100, bytes_per_pixel: 4 });
        let now = snapshot();
        assert_eq!((count(&now).0 - was.0, count(&now).1 - was.1), (3, 1));
        assert_eq!(now.held.iter().find(|h| h.name == "flow_pairs").unwrap().pixels_each(), Some(25));
        drop(c);
        assert_eq!(count(&snapshot()), was);
        made(Made::FlowUniforms, 16);
        made(Made::FlowUniforms, 16);
        let m = snapshot().made.into_iter().find(|(n, _, _)| *n == "flow_uniforms").unwrap();
        let was = before.made.into_iter().find(|(n, _, _)| *n == "flow_uniforms").unwrap();
        assert_eq!((m.1 - was.1, m.2 - was.2), (32, 2));
    }
}
