//! Most of this crate's integration tests, built as one binary: one
//! link instead of one per file. Each file stays where it was and is a
//! module here. Files that time themselves or change process-wide state
//! stay their own binaries (the `[[test]]` list in Cargo.toml).

// Each file declares the helpers it shares (`mod common;`, a `#[path]`
// support module) as it did as a binary of its own, so they load once
// per file here.
#![allow(clippy::duplicate_mod)]

#[path = "../alpha_output.rs"]
mod alpha_output;
#[path = "../chain_counters.rs"]
mod chain_counters;
#[path = "../chain_thumbnails.rs"]
mod chain_thumbnails;
#[path = "../composite_gpu_parity.rs"]
mod composite_gpu_parity;
#[path = "../contrast_pivot_encoding.rs"]
mod contrast_pivot_encoding;
#[path = "../difference_key_node.rs"]
mod difference_key_node;
#[path = "../blend_on_chain.rs"]
mod blend_on_chain;
#[path = "../denoise_on_chain.rs"]
mod denoise_on_chain;
#[path = "../effect_motion.rs"]
mod effect_motion;
#[path = "../effect_node_parity.rs"]
mod effect_node_parity;
#[path = "../effect_nodes.rs"]
mod effect_nodes;
#[path = "../effect_window_on_gpu.rs"]
mod effect_window_on_gpu;
#[path = "../fault_hook_feature.rs"]
mod fault_hook_feature;
#[path = "../gated_correction.rs"]
mod gated_correction;
#[path = "../gpu_keys.rs"]
mod gpu_keys;
#[path = "../grain_denoise_parity.rs"]
mod grain_denoise_parity;
#[path = "../held_outputs.rs"]
mod held_outputs;
#[path = "../key_matte_parity.rs"]
mod key_matte_parity;
#[path = "../key_mixer.rs"]
mod key_mixer;
#[path = "../lut_node_tags_parity.rs"]
mod lut_node_tags_parity;
#[path = "../mask_cutout_gpu_parity.rs"]
mod mask_cutout_gpu_parity;
#[path = "../mask_mix_node.rs"]
mod mask_mix_node;
#[path = "../masked_corrector.rs"]
mod masked_corrector;
#[path = "../moving_mask_playback.rs"]
mod moving_mask_playback;
#[path = "../neutral_corrector.rs"]
mod neutral_corrector;
#[path = "../nothing_chosen_selects_everything.rs"]
mod nothing_chosen_selects_everything;
#[path = "../outside_node.rs"]
mod outside_node;
#[path = "../presentation.rs"]
mod presentation;
#[path = "../registry_coverage.rs"]
mod registry_coverage;
#[path = "../hdr_zones_node.rs"]
mod hdr_zones_node;
#[path = "../scene_to_display_node.rs"]
mod scene_to_display_node;
#[path = "../wireless_loop.rs"]
mod wireless_loop;
#[path = "../zero_mix.rs"]
mod zero_mix;
