//! Colour grading, LUTs, scopes, and camera log decoders.
//!
//! The crate holds the colour maths behind the inspector and the node
//! graph: exposure / contrast / saturation / temperature / tint /
//! lift-gamma-gain-offset ([`linear`]), shape masks ([`masks`]),
//! keying ([`luma_key`], [`difference_key`]), custom curves
//! ([`curves`]), 3D LUTs ([`lut`]) and colour warping ([`warper`]).
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
//! [`scopes`] computes waveform / vectorscope / parade / histogram on
//! CPU for small previews; `klipp_effects::gpu_scopes` does the
//! full-resolution GPU pass. Both use BT.709 coefficients on sRGB code
//! values, matching Resolve's non-linear scope convention.

pub mod measure;
pub mod aces;
pub mod camera_log;
pub mod color_match;
pub mod curves;
pub mod denoise;
pub mod difference_key;
pub mod display;
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
pub mod warper;
pub mod white_balance;
pub mod wheels;

pub use lut::{apply_3d_lut, apply_3d_lut_linear_ws, apply_3d_lut_linear_ws_with_overrides, apply_3d_lut_tetrahedral, generate_lut_preview, generate_lut_reference_image, load_cube, load_lut, parse_3dl, parse_csp, parse_cube, write_cube, LutData, LutError};
pub use pipeline::ColorPipeline;
pub use transforms::*;
pub use warper::apply_color_warper;
pub use klipp_core::ColorWarperParams;
pub use wheels::{generate_wheel_texture, hsv_to_rgb, rgb_to_wheel_offset, wheel_offset_to_rgb};
