//! Most of this crate's integration tests, built as one binary: one
//! link instead of one per file. Each file stays where it was and is a
//! module here. Files that time themselves or change process-wide state
//! stay their own binaries (the `[[test]]` list in Cargo.toml).

// Each file declares the helpers it shares (`mod common;`, a `#[path]`
// support module) as it did as a binary of its own, so they load once
// per file here.
#![allow(clippy::duplicate_mod)]

#[path = "../an_export_without_gpu_planes_composites_on_the_gpu.rs"]
mod an_export_without_gpu_planes_composites_on_the_gpu;
#[path = "../a_timeline_frame_at_full_size.rs"]
mod a_timeline_frame_at_full_size;
#[path = "../audio_only_export.rs"]
mod audio_only_export;
#[path = "../captions_608_export.rs"]
mod captions_608_export;
#[path = "../clip_sources_reads_the_media.rs"]
mod clip_sources_reads_the_media;
#[path = "../banding_hint.rs"]
mod banding_hint;
#[path = "../codec_true_peak.rs"]
mod codec_true_peak;
#[path = "../track_effects_run_on_the_bus.rs"]
mod track_effects_run_on_the_bus;
#[path = "../export_status_timings.rs"]
mod export_status_timings;
#[path = "../export_audio_plays_the_whole_source.rs"]
mod export_audio_plays_the_whole_source;
#[path = "../export_frames_shared.rs"]
mod export_frames_shared;
#[path = "../export_mastering_peak.rs"]
mod export_mastering_peak;
#[path = "../export_odt_round_trip.rs"]
mod export_odt_round_trip;
#[path = "../export_refuses_a_missing_analysis.rs"]
mod export_refuses_a_missing_analysis;
#[path = "../frame_blend_reads_the_stream.rs"]
mod frame_blend_reads_the_stream;
#[path = "../frame_colour_tags.rs"]
mod frame_colour_tags;
#[path = "../gif_export.rs"]
mod gif_export;
#[path = "../gradient_grain_export.rs"]
mod gradient_grain_export;
#[path = "../graded_still.rs"]
mod graded_still;
#[path = "../hdr10plus_export.rs"]
mod hdr10plus_export;
#[path = "../hdr_export.rs"]
mod hdr_export;
#[path = "../keyer_op_matches_the_app.rs"]
mod keyer_op_matches_the_app;
#[path = "../layer_fx_goldens.rs"]
mod layer_fx_goldens;
#[path = "../normalize_clips.rs"]
mod normalize_clips;
#[path = "../odd_size_export.rs"]
mod odd_size_export;
#[path = "../pixel_check_fractional.rs"]
mod pixel_check_fractional;
#[path = "../precomposition_sound.rs"]
mod precomposition_sound;
#[path = "../reverse_export.rs"]
mod reverse_export;
#[path = "../temporal_export_size.rs"]
mod temporal_export_size;
#[path = "../title_authoring_export.rs"]
mod title_authoring_export;
#[path = "../title_composed_export.rs"]
mod title_composed_export;
#[path = "../title_fit_export.rs"]
mod title_fit_export;
#[path = "../pan_inside_a_card.rs"]
mod pan_inside_a_card;
#[path = "../frames_survive_save.rs"]
mod frames_survive_save;
#[path = "../scene_layers.rs"]
mod scene_layers;
#[path = "../secondaries_change_the_picture.rs"]
mod secondaries_change_the_picture;
#[path = "../place_by_sync.rs"]
mod place_by_sync;
#[path = "../tnr_export_cost.rs"]
mod tnr_export_cost;
#[path = "../tnr_neighbours_kept.rs"]
mod tnr_neighbours_kept;
#[path = "../tnr_on_the_chain.rs"]
mod tnr_on_the_chain;
