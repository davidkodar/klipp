//! Most of this crate's integration tests, built as one binary: one
//! link instead of one per file. Each file stays where it was and is a
//! module here. Files that time themselves or change process-wide state
//! stay their own binaries (the `[[test]]` list in Cargo.toml).

// Each file declares the helpers it shares (`mod common;`, a `#[path]`
// support module) as it did as a binary of its own, so they load once
// per file here.
#![allow(clippy::duplicate_mod)]

#[path = "../ab_wipe.rs"]
mod ab_wipe;
#[path = "../channel_view.rs"]
mod channel_view;
#[path = "../clip_below_render.rs"]
mod clip_below_render;
#[path = "../colour_effects_render.rs"]
mod colour_effects_render;
#[path = "../corner_pin_keys.rs"]
mod corner_pin_keys;
#[path = "../disabled_transform.rs"]
mod disabled_transform;
#[path = "../domain_refusal_render.rs"]
mod domain_refusal_render;
#[path = "../effect_keyframes.rs"]
mod effect_keyframes;
#[path = "../effect_node_anim.rs"]
mod effect_node_anim;
#[path = "../effect_render_scale.rs"]
mod effect_render_scale;
#[path = "../export_another_aspect.rs"]
mod export_another_aspect;
#[path = "../export_gpu_path_agrees.rs"]
mod export_gpu_path_agrees;
#[path = "../gates_keyed_off_the_frame.rs"]
mod gates_keyed_off_the_frame;
#[path = "../golden_corpus.rs"]
mod golden_corpus;
#[path = "../gpu_composite.rs"]
mod gpu_composite;
#[path = "../grade_alpha_output.rs"]
mod grade_alpha_output;
#[path = "../grade_compound_render.rs"]
mod grade_compound_render;
#[path = "../surface_shade_render.rs"]
mod surface_shade_render;
#[path = "../reflection_render.rs"]
mod reflection_render;
#[path = "../grade_cpu_gpu_parity.rs"]
mod grade_cpu_gpu_parity;
#[path = "../grade_keys_render.rs"]
mod grade_keys_render;
#[path = "../grade_mix.rs"]
mod grade_mix;
#[path = "../grade_render_scale.rs"]
mod grade_render_scale;
#[path = "../held_outputs_parked.rs"]
mod held_outputs_parked;
#[path = "../inspect_by_path.rs"]
mod inspect_by_path;
#[path = "../key_mixer_render.rs"]
mod key_mixer_render;
#[path = "../legacy_route_gone.rs"]
mod legacy_route_gone;
#[path = "../masked_effects_render.rs"]
mod masked_effects_render;
#[path = "../masked_grade_seeded.rs"]
mod masked_grade_seeded;
#[path = "../media_source_render.rs"]
mod media_source_render;
#[path = "../merge_on_the_stack.rs"]
mod merge_on_the_stack;
#[path = "../merge_branch.rs"]
mod merge_branch;
#[path = "../merge_render.rs"]
mod merge_render;
#[path = "../ofx_runs_after_the_grade.rs"]
mod ofx_runs_after_the_grade;
#[path = "../opacity_multiplies.rs"]
mod opacity_multiplies;
#[path = "../picture_transform_render.rs"]
mod picture_transform_render;
#[path = "../published_producer_masks.rs"]
mod published_producer_masks;
#[path = "../render_counters.rs"]
mod render_counters;
#[path = "../render_path_selfcheck.rs"]
mod render_path_selfcheck;
#[path = "../render_rules.rs"]
mod render_rules;
#[path = "../reroute_render.rs"]
mod reroute_render;
#[path = "../resolve_parity.rs"]
mod resolve_parity;
#[path = "../resolved_placement.rs"]
mod resolved_placement;
#[path = "../scale_aspect.rs"]
mod scale_aspect;
#[path = "../side_mattes_render.rs"]
mod side_mattes_render;
#[path = "../speed_ramp_retime.rs"]
mod speed_ramp_retime;
#[path = "../stack_effects_plan.rs"]
mod stack_effects_plan;
#[path = "../clean_frame_plate.rs"]
mod clean_frame_plate;
#[path = "../surface_pin.rs"]
mod surface_pin;
#[path = "../surface_pin_node.rs"]
mod surface_pin_node;
#[path = "../temporal_denoise_at_the_source.rs"]
mod temporal_denoise_at_the_source;
#[path = "../temporal_denoise_gpu.rs"]
mod temporal_denoise_gpu;
#[path = "../temporal_reader_size.rs"]
mod temporal_reader_size;
#[path = "../title_blend_mask_stack.rs"]
mod title_blend_mask_stack;
#[path = "../title_effects_spread.rs"]
mod title_effects_spread;
#[path = "../title_pinned_to_surface.rs"]
mod title_pinned_to_surface;
#[path = "../title_templates_gpu.rs"]
mod title_templates_gpu;
#[path = "../title_image_gpu.rs"]
mod title_image_gpu;
#[path = "../title_keyed_position_gpu.rs"]
mod title_keyed_position_gpu;
#[path = "../title_source_render.rs"]
mod title_source_render;
#[path = "../title_still_glyph_route.rs"]
mod title_still_glyph_route;
#[path = "../title_layers_kept.rs"]
mod title_layers_kept;
#[path = "../title_loops_placed_gpu.rs"]
mod title_loops_placed_gpu;
#[path = "../title_metal_gpu.rs"]
mod title_metal_gpu;
#[path = "../title_contrast_protect.rs"]
mod title_contrast_protect;
#[path = "../template_placed_gpu.rs"]
mod template_placed_gpu;
#[path = "../animated_caption_gpu.rs"]
mod animated_caption_gpu;
#[path = "../title_moving_styles_gpu.rs"]
mod title_moving_styles_gpu;
#[path = "../title_routes_f16.rs"]
mod title_routes_f16;
#[path = "../truth_lanes_render.rs"]
mod truth_lanes_render;
#[path = "../vector_glow_parity.rs"]
mod vector_glow_parity;
#[path = "../view_any_node.rs"]
mod view_any_node;
#[path = "../wide_gamut_scene_export.rs"]
mod wide_gamut_scene_export;
#[path = "../a_grade_of_the_wrong_size.rs"]
mod a_grade_of_the_wrong_size;

#[path = "../animated_generators_gpu.rs"]
mod animated_generators_gpu;
#[path = "../still_layers_kept.rs"]
mod still_layers_kept;
#[path = "../living_gradient_gpu.rs"]
mod living_gradient_gpu;
