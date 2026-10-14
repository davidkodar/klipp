//! Colour grading, LUTs, scopes, and camera log decoders.
//!
//! The crate holds the colour maths behind the inspector and the node
//! graph: exposure / contrast / saturation / temperature / tint /
//! lift-gamma-gain-offset ([`linear`]), shape masks ([`masks`]),
//! keying ([`luma_key`], [`difference_key`]), custom curves
//! ([`curves`]) and 3D LUTs ([`lut`]). Colour warping lives with the
//! grade twins in `klipp_effects::cpu_grade`, beside its shader.
//! `klipp_effects` holds the GPU shaders for the same ops, and the
//! CPU grade twins that read a whole frame at a time.
//!
//! # The grade runs in linear light
//!
//! Frames are linear-f32 RGBA from decode to display: [`linear`] holds
//! the CPU grade ops, [`display`] the output transform that encodes a
//! finished frame for an SDR or HDR display, and the node graph's GPU
//! shaders work in the same space. [`transforms::log_to_linear`] and
//! [`camera_log`] decode a camera's log encoding into it, without
//! re-encoding afterwards.
//!
//! The working colourspace itself is a project setting
//! (`WorkingColorspace`): Rec.709 linear or ACEScg, with [`primaries`]
//! supplying the matrices between them and [`aces`] the ACES
//! transforms.
//!
//! # Camera log decoders ([`camera_log`])
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
//! standard technical LUTs. A project's global LUT is held by
//! [`pipeline::ColorPipeline`] and applied by `klipp_compose`;
//! `klipp_nodes::bake_graph_to_lut` bakes a clip's grade chain out to
//! a `.cube` via [`lut::write_cube`].
//!
//! # Scopes
//!
//! The scopes themselves are drawn on the GPU by
//! `klipp_effects::gpu_scopes`, at the frame's native size.
//! [`scopes`] holds what they measure *in*: [`scopes::ScopeBasis`]
//! carries the working space's own luminance row and Y'CbCr
//! construction, so a BT.2020 or ACEScg project is not read with
//! Rec.709's coefficients, plus the vectorscope's target boxes and the
//! histograms.

pub mod ofx;
pub mod measure;
pub mod aces;
pub mod camera_log;
pub mod chart;
pub mod color_match;
pub mod curves;
pub mod difference_key;
pub mod display;
pub mod false_colour;
pub mod gamut;
/// The primary grade's model, shared with the GPU grade in `klipp-effects`.
pub use klipp_grade_model as grade_model;
pub mod hdr;
pub mod hdr_view;
pub mod huesat;
pub mod linear;
pub mod luma_key;
pub mod lut;
pub mod masks;
pub mod primaries;
pub mod soft_clip;
pub mod pipeline;
pub mod scopes;
pub mod transfer;
pub mod transforms;
pub mod white_balance;
pub mod wheels;

pub use lut::{apply_3d_lut, apply_3d_lut_linear_ws, apply_3d_lut_linear_ws_with_overrides, apply_3d_lut_tetrahedral, generate_lut_preview, generate_lut_reference_image, lut_bracket, load_cube, load_lut, parse_3dl, parse_csp, parse_cube, write_cube, LutData, LutError};
pub use pipeline::ColorPipeline;
pub use transforms::*;
pub use klipp_core::ColorWarperParams;
pub use wheels::{generate_wheel_texture, hsv_to_rgb, printer_light, printer_points, rgb_to_wheel_offset, wheel_offset_to_rgb};
