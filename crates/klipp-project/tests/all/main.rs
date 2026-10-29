//! Most of this crate's integration tests, built as one binary: one
//! link instead of one per file. Each file stays where it was and is a
//! module here. Files that time themselves or change process-wide state
//! stay their own binaries (the `[[test]]` list in Cargo.toml).

// Each file declares the helpers it shares (`mod common;`, a `#[path]`
// support module) as it did as a binary of its own, so they load once
// per file here.
#![allow(clippy::duplicate_mod)]

#[path = "../auto_move_save.rs"]
mod auto_move_save;
#[path = "../rich_round_trip.rs"]
mod rich_round_trip;
#[path = "../autosave_restore_paths.rs"]
mod autosave_restore_paths;
#[path = "../crash_recovery_open.rs"]
mod crash_recovery_open;
#[path = "../brief_round_trip.rs"]
mod brief_round_trip;
#[path = "../awaiting_refit_round_trip.rs"]
mod awaiting_refit_round_trip;
#[path = "../matched_clip_round_trip.rs"]
mod matched_clip_round_trip;
#[path = "../review_markers_round_trip.rs"]
mod review_markers_round_trip;
#[path = "../camera_keys_round_trip.rs"]
mod camera_keys_round_trip;
#[path = "../camera_round_trip.rs"]
mod camera_round_trip;
#[path = "../canvas_metadata.rs"]
mod canvas_metadata;
#[path = "../clip_below_round_trip.rs"]
mod clip_below_round_trip;
#[path = "../clip_label_round_trip.rs"]
mod clip_label_round_trip;
#[path = "../clip_rates_round_trip.rs"]
mod clip_rates_round_trip;
#[path = "../consolidate_integration.rs"]
mod consolidate_integration;
#[path = "../colour_effects_round_trip.rs"]
mod colour_effects_round_trip;
#[path = "../corner_pin_round_trip.rs"]
mod corner_pin_round_trip;
#[path = "../dangling_transition_heals.rs"]
mod dangling_transition_heals;
#[path = "../drop_frame_round_trip.rs"]
mod drop_frame_round_trip;
#[path = "../edit_plan_round_trip.rs"]
mod edit_plan_round_trip;
#[path = "../frame_glide_round_trip.rs"]
mod frame_glide_round_trip;
#[path = "../edit_planner_corpus.rs"]
mod edit_planner_corpus;
#[path = "../edits_survive_save_load.rs"]
mod edits_survive_save_load;
#[path = "../effect_list_round_trip.rs"]
mod effect_list_round_trip;
#[path = "../grade_compound_round_trip.rs"]
mod grade_compound_round_trip;
#[path = "../key_invert_round_trip.rs"]
mod key_invert_round_trip;
#[path = "../key_mixer_round_trip.rs"]
mod key_mixer_round_trip;
#[path = "../layer_fx_format.rs"]
mod layer_fx_format;
#[path = "../log_format_override.rs"]
mod log_format_override;
#[path = "../looks_each_saved.rs"]
mod looks_each_saved;
#[path = "../manifest_order.rs"]
mod manifest_order;
#[path = "../listed_media_offline.rs"]
mod listed_media_offline;
#[path = "../media_kind_round_trip.rs"]
mod media_kind_round_trip;
#[path = "../media_outside_the_folder.rs"]
mod media_outside_the_folder;
#[path = "../media_source_round_trip.rs"]
mod media_source_round_trip;
#[path = "../media_state_round_trip.rs"]
mod media_state_round_trip;
#[path = "../merge_round_trip.rs"]
mod merge_round_trip;
#[path = "../newer_format_refused.rs"]
mod newer_format_refused;
#[path = "../node_labels_round_trip.rs"]
mod node_labels_round_trip;
#[path = "../picture_cache_keys.rs"]
mod picture_cache_keys;
#[path = "../picture_ops_corpus.rs"]
mod picture_ops_corpus;
#[path = "../picture_truth_round_trip.rs"]
mod picture_truth_round_trip;
#[path = "../plugin_eye_round_trip.rs"]
mod plugin_eye_round_trip;
#[path = "../precomposition_round_trip.rs"]
mod precomposition_round_trip;
#[path = "../published_port_refused.rs"]
mod published_port_refused;
#[path = "../refused_clip_is_named.rs"]
mod refused_clip_is_named;
#[path = "../reroute_round_trip.rs"]
mod reroute_round_trip;
#[path = "../save_is_one_generation.rs"]
mod save_is_one_generation;
#[path = "../grade_alpha_round_trip.rs"]
mod grade_alpha_round_trip;
#[path = "../grade_version_mix.rs"]
mod grade_version_mix;
#[path = "../track_grade_save.rs"]
mod track_grade_save;
#[path = "../title_template_fields.rs"]
mod title_template_fields;
#[path = "../title_blend_mask_format.rs"]
mod title_blend_mask_format;
#[path = "../sequence_grade_mix.rs"]
mod sequence_grade_mix;
#[path = "../side_stores_kept.rs"]
mod side_stores_kept;
#[path = "../side_stores_revision2.rs"]
mod side_stores_revision2;
#[path = "../step_f_saves.rs"]
mod step_f_saves;
#[path = "../stored_lut_path.rs"]
mod stored_lut_path;
#[path = "../subtitle_source_round_trip.rs"]
mod subtitle_source_round_trip;
#[path = "../surface_round_trip.rs"]
mod surface_round_trip;
#[path = "../texture_fill_path.rs"]
mod texture_fill_path;
#[path = "../title_nits_round_trip.rs"]
mod title_nits_round_trip;
#[path = "../track_automation_round_trip.rs"]
mod track_automation_round_trip;
#[path = "../track_effects_round_trip.rs"]
mod track_effects_round_trip;
#[path = "../track_language_round_trip.rs"]
mod track_language_round_trip;
#[path = "../unknown_effect_refused.rs"]
mod unknown_effect_refused;
#[path = "../unknown_node_refused.rs"]
mod unknown_node_refused;
#[path = "../vector_path.rs"]
mod vector_path;
#[path = "../working_colorspace_round_trip.rs"]
mod working_colorspace_round_trip;
#[path = "../zero_mix_round_trip.rs"]
mod zero_mix_round_trip;
#[path = "../old_ids_open.rs"]
mod old_ids_open;
