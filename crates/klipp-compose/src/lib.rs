//! One definition of how a timeline frame is rendered.
//!
//! The viewer (klipp-engine) and export (klipp-render) used to render
//! frames with separate code that disagreed: different effect orders,
//! different track stacking, transitions graded or not, opacity applied
//! once or twice, log footage graded scene-linear or not. This crate is
//! where those rules live, once; both sides call it.

pub mod eval;
pub mod inspect;
pub mod synth;
pub mod render;
pub mod resolve;
pub mod selfcheck;

pub use render::{
    LinearFrameProviderFn,
    active_layers, frame_snapshot, ActiveLayer, DisplayDelivery, FrameSource, GlobalLut, GpuPixels, Inspect, InspectAt, Inspection, RenderOptions,
    RenderStats, Renderer, EFFECTS_BLENDED_ON_CPU, EFFECTS_ON_CPU, EFFECTS_ON_CPU_KERNELS, EFFECT_MASKS_UNAVAILABLE, GPU_STACK_DECLINES, PLAN_EFFECT_CLIPS, PLAN_EFFECT_FALLBACKS,
    SourceFrame, SourcePixels, frame_index, frame_blends, over_u8_straight, gpu_composite_enabled};
