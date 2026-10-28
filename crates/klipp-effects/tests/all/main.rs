//! Most of this crate's integration tests, built as one binary: one
//! link instead of one per file. Each file stays where it was and is a
//! module here. Files that time themselves or change process-wide state
//! stay their own binaries (the `[[test]]` list in Cargo.toml).

// Each file declares the helpers it shares (`mod common;`, a `#[path]`
// support module) as it did as a binary of its own, so they load once
// per file here.
#![allow(clippy::duplicate_mod)]

#[path = "../burn_and_whip_look.rs"]
mod burn_and_whip_look;
#[path = "../nv12_resident_pool.rs"]
mod nv12_resident_pool;
#[path = "../contrast_pivot_parity.rs"]
mod contrast_pivot_parity;
#[path = "../corner_pin.rs"]
mod corner_pin;
#[path = "../cpu_gpu_effect_parity.rs"]
mod cpu_gpu_effect_parity;
#[path = "../stabilise_motion.rs"]
mod stabilise_motion;
#[path = "../planar_gpu_parity.rs"]
mod planar_gpu_parity;
#[path = "../cpu_gpu_transition_parity.rs"]
mod cpu_gpu_transition_parity;
#[path = "../crop_feather.rs"]
mod crop_feather;
#[path = "../directional_blur_coverage.rs"]
mod directional_blur_coverage;
#[path = "../display_dither.rs"]
mod display_dither;
#[path = "../display_to_texture.rs"]
mod display_to_texture;
#[path = "../effect_definitions.rs"]
mod effect_definitions;
#[path = "../param_guard.rs"]
mod param_guard;
#[path = "../effects_hdr_behaviour.rs"]
mod effects_hdr_behaviour;
#[path = "../equalize_and_huesat_share_nothing.rs"]
mod equalize_and_huesat_share_nothing;
#[path = "../equalize_gpu.rs"]
mod equalize_gpu;
#[path = "../flat_dispatch_splits.rs"]
mod flat_dispatch_splits;
#[path = "../frame_upload_count.rs"]
mod frame_upload_count;
#[path = "../every_shader_is_validated.rs"]
mod every_shader_is_validated;
#[path = "../gpu_pipelines_build.rs"]
mod gpu_pipelines_build;
#[path = "../gpu_scopes.rs"]
mod gpu_scopes;
#[path = "../gpu_stack_parity.rs"]
mod gpu_stack_parity;
#[path = "../luma_key_effect.rs"]
mod luma_key_effect;
#[path = "../luma_key_gpu_parity.rs"]
mod luma_key_gpu_parity;
#[path = "../lut_node_cpu_gpu_parity.rs"]
mod lut_node_cpu_gpu_parity;
#[path = "../mask_gpu_parity.rs"]
mod mask_gpu_parity;
#[path = "../mask_mix.rs"]
mod mask_mix;
#[path = "../mixable_types.rs"]
mod mixable_types;
#[path = "../motion_warp_parity.rs"]
mod motion_warp_parity;
#[path = "../parity_coverage.rs"]
mod parity_coverage;
#[path = "../particles_look.rs"]
mod particles_look;
#[path = "../piece_transitions.rs"]
mod piece_transitions;
#[path = "../presentation.rs"]
mod presentation;
#[path = "../recorded_effects.rs"]
mod recorded_effects;
#[path = "../temporal_denoise_gpu.rs"]
mod temporal_denoise_gpu;
#[path = "../transitions_have_their_own_look.rs"]
mod transitions_have_their_own_look;
#[path = "../transitions_look_as_named.rs"]
mod transitions_look_as_named;
#[path = "../registry_coverage.rs"]
mod registry_coverage;
#[path = "../render_faults_1020.rs"]
mod render_faults_1020;
#[path = "../render_scale_units.rs"]
mod render_scale_units;
#[path = "../scopes_from_display_on_gpu.rs"]
mod scopes_from_display_on_gpu;
#[path = "../hdr_zones_gpu_parity.rs"]
mod hdr_zones_gpu_parity;
#[path = "../soft_clip_gpu_parity.rs"]
mod soft_clip_gpu_parity;
#[path = "../wgsl_camera_log_parity.rs"]
mod wgsl_camera_log_parity;
#[path = "../wgsl_yuv_levels.rs"]
mod wgsl_yuv_levels;
#[path = "../window_mix.rs"]
mod window_mix;

#[path = "../readback_diagnostics.rs"]
mod readback_diagnostics;

#[path = "../resident_alpha_masks.rs"]
mod resident_alpha_masks;

#[path = "../resident_title_styles.rs"]
mod resident_title_styles;

#[path = "../shared_warps.rs"]
mod shared_warps;
