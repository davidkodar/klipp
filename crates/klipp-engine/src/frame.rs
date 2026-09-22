//! Frame type — owned or borrowed view of a decoded video frame, with
//! explicit CPU vs GPU residency.
//!
//! Introduced in phase 2 of the end-to-end GPU-residency plan. Phase
//! 3a (this revision) expands `GpuFrame` with real state — a
//! `wgpu::Buffer` holding RGBA8, plus the device + queue required to
//! read it back — and implements `Frame::into_cpu()` as a true
//! map-async readback. Producers (phase 3b's decoder wiring) build a
//! `GpuFrame` and hand it across threads; any consumer that still
//! needs CPU data calls `into_cpu()` at the boundary.
//!
//! The shape is deliberately minimal — owned for handoff across
//! boundaries, borrowed for in-place reads. Neither path clones unless
//! a caller explicitly calls `into_cpu()`.

/// Owned decoded frame.
pub enum Frame {
    Cpu(CpuFrame),
    Gpu(GpuFrame),
}

/// Borrowed view of a decoded frame. Phase 3+ callers that only need
/// to read pixels should prefer this to `Frame` — no ownership move,
/// no allocation.
pub enum FrameRef<'a> {
    Cpu(CpuFrameRef<'a>),
    Gpu(GpuFrameRef<'a>),
}

/// CPU-resident RGBA8 frame.
#[derive(Clone)]
pub struct CpuFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub struct CpuFrameRef<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// GPU-resident frame handle.
///
/// Carries the output of a GPU shader (e.g. NV12→RGBA, P010→RGBA) as a
/// `wgpu::Buffer` of linear-order RGBA8 pixels, together with the
/// `Arc<wgpu::Device>` and `Arc<wgpu::Queue>` required to read it
/// back. `Send + Sync` via the Arc-wrapped wgpu handles, so a decode
/// thread can build one and push it to the engine.
///
/// Consumers that need CPU pixels flatten the buffer with
/// `Frame::into_cpu()`, or expand it to linear floats with
/// [`crate::types::realise_linear_from_gpu`]. The GPU render path
/// reads it in place and never does either.
#[non_exhaustive]
pub struct GpuFrame {
    /// Device the buffer was allocated on. All readbacks must use
    /// this same device — wgpu buffers are not portable between
    /// devices.
    pub device: std::sync::Arc<wgpu::Device>,
    /// Queue for submitting the readback copy command.
    pub queue: std::sync::Arc<wgpu::Queue>,
    /// RGBA8 pixels in row-major order. Must have `COPY_SRC` usage so
    /// `into_cpu()` can copy into a fresh staging buffer. Callers
    /// that intend to feed this directly into the grade chain in
    /// phase 4 can additionally flag it `STORAGE`.
    pub rgba_buffer: std::sync::Arc<wgpu::Buffer>,
    pub width: u32,
    pub height: u32,
}

#[non_exhaustive]
pub struct GpuFrameRef<'a> {
    pub width: u32,
    pub height: u32,
    /// Borrow of the underlying RGBA8 buffer. Phase 4's grade chain
    /// binds this directly as a shader input.
    pub rgba_buffer: &'a wgpu::Buffer,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl Frame {
    pub fn width(&self) -> u32 {
        match self {
            Frame::Cpu(f) => f.width,
            Frame::Gpu(f) => f.width,
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            Frame::Cpu(f) => f.height,
            Frame::Gpu(f) => f.height,
        }
    }

    pub fn as_ref(&self) -> FrameRef<'_> {
        match self {
            Frame::Cpu(f) => FrameRef::Cpu(CpuFrameRef {
                data: &f.data,
                width: f.width,
                height: f.height,
            }),
            Frame::Gpu(f) => FrameRef::Gpu(GpuFrameRef {
                width: f.width,
                height: f.height,
                rgba_buffer: &f.rgba_buffer,
                _marker: std::marker::PhantomData,
            }),
        }
    }

    /// Collapse to CPU. Zero-cost move on `Cpu`; on `Gpu` it triggers
    /// a `map_async` + `device.poll(Wait)` readback of the underlying
    /// `wgpu::Buffer`. The readback cost is intrinsic to consuming
    /// GPU-resident pixels from the CPU — phase 4 eliminates it by
    /// letting the grade chain consume `GpuFrame` directly.
    pub fn into_cpu(self) -> CpuFrame {
        match self {
            Frame::Cpu(f) => f,
            Frame::Gpu(f) => gpu_frame_into_cpu(f),
        }
    }
}

fn gpu_frame_into_cpu(f: GpuFrame) -> CpuFrame {
    let byte_size = (f.width as u64) * (f.height as u64) * 4;
    // Staging buffer — mappable, same size as the source RGBA buffer.
    // Allocated per readback for now; a pooled staging ring is a
    // phase-4+ optimisation once the GPU path is actually used.
    let staging = f.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("frame_into_cpu_staging"),
        size: byte_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = f.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("frame_into_cpu"),
    });
    encoder.copy_buffer_to_buffer(&f.rgba_buffer, 0, &staging, 0, byte_size);
    f.queue.submit(std::iter::once(encoder.finish()));

    let slice = staging.slice(..byte_size);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    klipp_effects::gpu::mark_gpu_stage(klipp_effects::gpu::GpuStage::FrameIntoCpu);
    // Non-blocking poll loop instead of `Maintain::Wait` — see
    // `klipp_effects::gpu::poll_until_mapped` for why Wait deadlocks
    // on a UI-shared device. Falls back to a zero-filled buffer if
    // the map times out.
    let data = match klipp_effects::gpu::poll_until_mapped(&f.device, &rx) {
        Ok(Ok(())) => {
            let mapped = slice.get_mapped_range();
            let v = mapped.to_vec();
            drop(mapped);
            staging.unmap();
            v
        }
        other => {
            tracing::warn!(
                "Frame::into_cpu readback failed ({other:?}); returning zeros"
            );
            vec![0u8; byte_size as usize]
        }
    };
    klipp_effects::gpu::mark_gpu_stage(klipp_effects::gpu::GpuStage::Idle);

    CpuFrame { data, width: f.width, height: f.height }
}

impl<'a> FrameRef<'a> {
    pub fn width(&self) -> u32 {
        match self {
            FrameRef::Cpu(f) => f.width,
            FrameRef::Gpu(f) => f.width,
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            FrameRef::Cpu(f) => f.height,
            FrameRef::Gpu(f) => f.height,
        }
    }
}

impl CpuFrame {
    pub fn new(data: Vec<u8>, width: u32, height: u32) -> Self {
        Self { data, width, height }
    }
}

impl GpuFrame {
    /// Build a `GpuFrame` from a caller-owned `wgpu::Buffer`. The
    /// buffer must hold width * height * 4 bytes of RGBA8 pixels
    /// (row-major) and have `COPY_SRC` usage so `Frame::into_cpu()`
    /// can read it back. Producers that intend to feed the buffer
    /// directly into the grade chain in phase 4 should additionally
    /// flag it `STORAGE`.
    ///
    /// Held behind this constructor because the struct is
    /// `#[non_exhaustive]` — the field list will grow in phase 4.
    pub fn new(
        device: std::sync::Arc<wgpu::Device>,
        queue: std::sync::Arc<wgpu::Queue>,
        rgba_buffer: std::sync::Arc<wgpu::Buffer>,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            device,
            queue,
            rgba_buffer,
            width,
            height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_frame_width_height_match_construction() {
        let f = Frame::Cpu(CpuFrame::new(vec![0; 16], 2, 2));
        assert_eq!(f.width(), 2);
        assert_eq!(f.height(), 2);
    }

    #[test]
    fn frame_as_ref_returns_matching_variant() {
        let f = Frame::Cpu(CpuFrame::new(vec![1, 2, 3, 4], 1, 1));
        match f.as_ref() {
            FrameRef::Cpu(r) => {
                assert_eq!(r.width, 1);
                assert_eq!(r.height, 1);
                assert_eq!(r.data, &[1, 2, 3, 4]);
            }
            FrameRef::Gpu(_) => panic!("expected Cpu variant"),
        }
    }

    #[test]
    fn into_cpu_is_identity_on_cpu_variant() {
        let f = Frame::Cpu(CpuFrame::new(vec![9; 4], 1, 1));
        let cpu = f.into_cpu();
        assert_eq!(cpu.width, 1);
        assert_eq!(cpu.height, 1);
        assert_eq!(cpu.data, vec![9; 4]);
    }

    // NOTE: GpuFrame construction now requires a live wgpu::Device +
    // wgpu::Queue + wgpu::Buffer. A full readback round-trip test
    // lives in the integration suite; unit-testing the Gpu variant
    // in place would bring up a headless wgpu device on every test
    // invocation (slow + hardware-dependent).
}
