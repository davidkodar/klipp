//! What the GPU parity suites compare against, and what they do without a
//! GPU (#2462).
//!
//! A `Renderer` runs its grades and effects on the GPU when it has one, so
//! a frame from `render` on it is the GPU-assisted route, not the CPU
//! reference. [`CpuReference`] renders with the GPU off, then turns it back
//! on, for a renderer that had it on. [`no_gpu`] is a GPU route that could
//! not be taken: a skip, unless `KLIPP_TEST_REQUIRE_GPU=1` asks for the GPU,
//! when it fails.

use klipp_compose::{FrameSource, Inspect, Inspection, RenderOptions, Renderer};
use klipp_core::Timecode;

pub trait CpuReference {
    /// `render` with the GPU off: the CPU renderer's own frame.
    fn cpu_render(&mut self, seq: &klipp_timeline::Sequence, time: Timecode, width: u32, height: u32, src: &mut dyn FrameSource, opts: RenderOptions) -> Vec<f32>;
    /// `render_inspected` with the GPU off.
    fn cpu_render_inspected(&mut self, seq: &klipp_timeline::Sequence, time: Timecode, width: u32, height: u32, src: &mut dyn FrameSource, opts: RenderOptions, inspect: Inspect) -> (Vec<f32>, Inspection);
}

impl CpuReference for Renderer {
    fn cpu_render(&mut self, seq: &klipp_timeline::Sequence, time: Timecode, width: u32, height: u32, src: &mut dyn FrameSource, opts: RenderOptions) -> Vec<f32> {
        self.cpu_render_inspected(seq, time, width, height, src, opts, Inspect::default()).0
    }

    fn cpu_render_inspected(&mut self, seq: &klipp_timeline::Sequence, time: Timecode, width: u32, height: u32, src: &mut dyn FrameSource, opts: RenderOptions, inspect: Inspect) -> (Vec<f32>, Inspection) {
        self.set_gpu_enabled(false);
        let out = self.render_inspected(seq, time, width, height, src, opts, inspect);
        self.set_gpu_enabled(true);
        out
    }
}

/// A GPU route this test could not take, `why`: a skip, or a failure when
/// `KLIPP_TEST_REQUIRE_GPU=1` asks for the GPU.
pub fn no_gpu(why: &str) {
    assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required: {why}");
    eprintln!("skipping: {why}");
}
