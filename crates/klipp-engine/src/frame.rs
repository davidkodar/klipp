//! Frame type — owned or borrowed view of a decoded video frame, with
//! explicit CPU vs GPU residency.
//!
//! Introduced in phase 2 of the end-to-end GPU-residency plan. In this
//! phase every producer still emits CPU-resident frames and every
//! consumer reads them from the CPU side, so the `Gpu*` variants are
//! placeholders. Phase 3 wires NVDEC → `GpuFrame`; phase 4 rewrites the
//! grade chain to consume `FrameRef` and keep pixels GPU-resident.
//!
//! The shape is deliberately minimal — owned for handoff across
//! boundaries, borrowed for in-place reads. Neither path clones unless
//! a caller explicitly calls `to_cpu()`.

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

/// GPU-resident frame handle. Placeholder in phase 2; phase 3 adds the
/// `wgpu::Texture` + `wgpu::TextureView` + metadata needed for the
/// grade chain to consume without a CPU round-trip.
///
/// Held `#[non_exhaustive]` so adding the phase-3 fields isn't a
/// breaking change for crates that match on `Frame`.
#[non_exhaustive]
pub struct GpuFrame {
    pub width: u32,
    pub height: u32,
}

#[non_exhaustive]
pub struct GpuFrameRef<'a> {
    pub width: u32,
    pub height: u32,
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
                _marker: std::marker::PhantomData,
            }),
        }
    }

    /// Collapse to CPU. Cheap on `Cpu` (move), expensive on `Gpu`
    /// (readback). Phase 3 fills in the Gpu case; until then it
    /// panics — any caller that reaches it has invariant confusion
    /// worth an explicit crash.
    pub fn into_cpu(self) -> CpuFrame {
        match self {
            Frame::Cpu(f) => f,
            Frame::Gpu(_) => {
                panic!("Frame::into_cpu on Gpu variant before phase 3 readback path exists");
            }
        }
    }
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

    #[test]
    #[should_panic(expected = "before phase 3 readback path exists")]
    fn into_cpu_panics_on_gpu_variant() {
        let f = Frame::Gpu(GpuFrame { width: 10, height: 10 });
        let _ = f.into_cpu();
    }
}
