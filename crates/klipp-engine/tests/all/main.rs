//! Most of this crate's integration tests, built as one binary: one
//! link instead of one per file. Each file stays where it was and is a
//! module here. Files that time themselves or change process-wide state
//! stay their own binaries (the `[[test]]` list in Cargo.toml).

// Each file declares the helpers it shares (`mod common;`, a `#[path]`
// support module) as it did as a binary of its own, so they load once
// per file here.
#![allow(clippy::duplicate_mod)]

#[path = "../a_large_jpeg_still_shows.rs"]
mod a_large_jpeg_still_shows;
#[path = "../a_live_drag_reaches.rs"]
mod a_live_drag_reaches;
#[path = "../a_parked_step_keeps_its_neighbours.rs"]
mod a_parked_step_keeps_its_neighbours;
#[path = "../a_still_plays_under_moving_layers.rs"]
mod a_still_plays_under_moving_layers;
#[path = "../cache_bytes_are_display_bytes.rs"]
mod cache_bytes_are_display_bytes;
#[path = "../chroma_reconstruction.rs"]
mod chroma_reconstruction;
#[path = "../clip_serde_rich.rs"]
mod clip_serde_rich;
#[path = "../clip_serde_shape.rs"]
mod clip_serde_shape;
#[path = "../decode_downscale.rs"]
mod decode_downscale;
#[path = "../direct_present_identity.rs"]
mod direct_present_identity;
#[path = "../display_yuv_cpu_matches_gpu.rs"]
mod display_yuv_cpu_matches_gpu;
#[path = "../filmstrip_says_what_it_could_not_make.rs"]
mod filmstrip_says_what_it_could_not_make;
#[path = "../gpu_chain_prime_identity.rs"]
mod gpu_chain_prime_identity;
#[path = "../gpu_context_shared_device.rs"]
mod gpu_context_shared_device;
#[path = "../gpu_rgba64_to_linear_bridge.rs"]
mod gpu_rgba64_to_linear_bridge;
#[path = "../gpu_u8_to_linear_bridge.rs"]
mod gpu_u8_to_linear_bridge;
#[path = "../grade_to_texture_identity.rs"]
mod grade_to_texture_identity;
#[path = "../hdr_gpu_graded_path.rs"]
mod hdr_gpu_graded_path;
#[path = "../hdr_grade_no_crush.rs"]
mod hdr_grade_no_crush;
#[path = "../hdr_monitoring.rs"]
mod hdr_monitoring;
#[path = "../hdr_p010_split.rs"]
mod hdr_p010_split;
#[path = "../log_nv12_split.rs"]
mod log_nv12_split;
#[path = "../nv12_gpu_frame_identity.rs"]
mod nv12_gpu_frame_identity;
#[path = "../nv12_linear_output.rs"]
mod nv12_linear_output;
#[path = "../p010_gpu_acescg_parity.rs"]
mod p010_gpu_acescg_parity;
#[path = "../p010_gpu_frame_identity.rs"]
mod p010_gpu_frame_identity;
#[path = "../p010_linear_output.rs"]
mod p010_linear_output;
#[path = "../parked_drag_with_title.rs"]
mod parked_drag_with_title;
#[path = "../parked_drag_matches_full_render.rs"]
mod parked_drag_matches_full_render;
#[path = "../paused_encode_cost.rs"]
mod paused_encode_cost;
#[path = "../paused_regrade_perf.rs"]
mod paused_regrade_perf;
#[path = "../scope_basis.rs"]
mod scope_basis;
#[path = "../seek_after_end_of_file.rs"]
mod seek_after_end_of_file;
#[path = "../sequential_decoder_walks_every_frame.rs"]
mod sequential_decoder_walks_every_frame;
#[path = "../sw_decode_parity.rs"]
mod sw_decode_parity;
#[path = "../viewer_and_export_make_one_asset.rs"]
mod viewer_and_export_make_one_asset;
#[path = "../yuv420p10_cpu_matches_gpu.rs"]
mod yuv420p10_cpu_matches_gpu;
