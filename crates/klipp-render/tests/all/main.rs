//! Most of this crate's integration tests, built as one binary: one
//! link instead of one per file. Each file stays where it was and is a
//! module here. Files that time themselves or change process-wide state
//! stay their own binaries (the `[[test]]` list in Cargo.toml).

// Each file declares the helpers it shares (`mod common;`, a `#[path]`
// support module) as it did as a binary of its own, so they load once
// per file here.
#![allow(clippy::duplicate_mod)]

#[path = "../clip_sources_reads_the_media.rs"]
mod clip_sources_reads_the_media;
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
#[path = "../hdr_export.rs"]
mod hdr_export;
#[path = "../layer_fx_goldens.rs"]
mod layer_fx_goldens;
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
