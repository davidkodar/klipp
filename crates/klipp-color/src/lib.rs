//! Colour grading, LUTs, scopes, and camera log decoders.
//!
//! The crate implements every colour-space operation Klipp exposes in
//! the inspector and node graph: exposure / contrast / saturation /
//! temperature / tint / lift-gamma-gain-offset / HSL qualifier /
//! shape masks / custom curves / 3D LUTs / colour warping. Each op has
//! a CPU path here; GPU implementations live in
//! [`klipp_effects::gpu`].
//!
//! # Known correctness limitation
//!
//! **The entire pipeline currently operates on 8-bit sRGB-encoded
//! bytes.** Exposure above ~+1 stop blows out midtones, the Rec.709
//! luma coefficients in saturation / qualifier are applied to
//! gamma-encoded values, and [`transforms::apply_input_transform`]
//! linearises camera log only to re-encode it to sRGB before the grade
//! — the IDT is defeated by its own wrapper. A future linear-light
//! pipeline (f16 or f32 intermediate plumbed through engine → render →
//! viewer) is the fundamental fix.
//!
//! # Camera log decoders ([`transforms`])
//!
//! Forward decoders for SLog2/3, LogC3/4, Apple Log (three-segment per
//! the 2023 whitepaper constants), PQ, HLG, N-Log, DJI D-Log, BMD Film
//! Gen5, and a few more. Constants are whitepaper-correct where
//! verified; Nikon N-Log and one or two others still want a reference
//! round-trip test.
//!
//! # LUTs
//!
//! [`lut`] supports `.cube`, `.csp`, `.3dl` parsers plus trilinear
//! (`apply_3d_lut`) and tetrahedral (`apply_3d_lut_tetrahedral`)
//! interpolation. Tetrahedral matches Resolve's built-in behaviour for
//! standard technical LUTs.
//!
//! # Scopes
//!
//! [`scopes`] computes waveform / vectorscope / parade / histogram on
//! CPU for small previews; [`klipp_effects::gpu_scopes`] does the
//! full-resolution GPU pass. Both use BT.709 coefficients on sRGB code
//! values, matching Resolve's non-linear scope convention.

pub mod color_match;
pub mod curves;
pub mod denoise;
pub mod huesat;
pub mod lut;
pub mod masks;
pub mod qualifier;
pub mod pipeline;
pub mod scopes;
pub mod transfer;
pub mod transforms;
pub mod warper;
pub mod wheels;

pub use lut::{apply_3d_lut, apply_3d_lut_tetrahedral, bake_grade_to_lut, generate_lut_preview, generate_lut_reference_image, load_cube, load_lut, parse_3dl, parse_csp, parse_cube, write_cube, LutData, LutError};
pub use pipeline::{ColorGradeParams, ColorPipeline};
pub use transforms::*;
pub use warper::apply_color_warper;
pub use klipp_core::ColorWarperParams;
pub use wheels::{generate_wheel_texture, hsv_to_rgb, wheel_offset_to_rgb};
