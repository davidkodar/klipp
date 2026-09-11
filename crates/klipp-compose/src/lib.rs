//! One definition of how a timeline frame is rendered.
//!
//! The viewer (klipp-engine) and export (klipp-render) used to render
//! frames with separate code that disagreed: different effect orders,
//! different track stacking, transitions graded or not, opacity applied
//! once or twice, log footage graded scene-linear or not. This crate is
//! where those rules live, once; both sides call it.

pub mod eval;
pub mod synth;
pub mod render;

pub use render::{FrameSource, GlobalLut, RenderOptions, Renderer, SourceFrame, SourcePixels};
