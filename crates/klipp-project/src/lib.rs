//! Project persistence and external-format import / export.
//!
//! This crate owns the `.klipp` on-disk format (via [`save::save_project`]
//! / [`load::load_project`]), the media-consolidation pipeline
//! ([`consolidate`]), grade-still PNG storage ([`grade_stills`]),
//! workspace (multi-project folder) management ([`workspace`]), and the
//! three external-format importers (Kdenlive/MLT, FCPXML, OTIO) plus
//! the MLT exporter.
//!
//! # Trust boundary
//!
//! * **Trusted input** — our own `.klipp` files. Written by
//!   `save`, so paths, FPS, resolutions are known-good. The load path
//!   uses [`path_util::resolve_media_path`] (no traversal check; we
//!   trust ourselves).
//! * **Untrusted input** — MLT / FCPXML / OTIO. External editors or
//!   hand-crafted files can contain any garbage. All such paths route
//!   through [`path_util::safe_resolve`] (rejects `..` that escape the
//!   project dir) and all numeric fields through
//!   [`path_util::sanitize_fps`] / [`path_util::sanitize_resolution`].
//!
//! # Robustness primitives ([`path_util`])
//!
//! * [`path_util::atomic_write`] — write to `.<file>.tmp.<pid>.<nanos>` then
//!   rename. Collision-proof even under concurrent saves.
//! * [`path_util::read_to_string_capped`] — size-checked file
//!   reads (64 MiB default cap) so a 10 GB crafted `.fcpxml` can't OOM
//!   the process before parsing even begins.
//! * [`path_util::parse_xml_safe`] — roxmltree parse with `allow_dtd: false` and
//!   a 1M-node limit, defending against entity-bomb DoS.
//!
//! # Consolidate
//!
//! [`consolidate::consolidate_project`] copies every referenced media
//! file into a single portable folder. Symlinks are explicitly
//! rejected (a hostile project shouldn't exfiltrate `~/.ssh/id_rsa`
//! during consolidation).

pub mod metadata_sheet;
pub mod consolidate;
mod format;
pub mod fcpxml;
pub mod fcpxml_import;
pub mod grade_stills;
mod load;
pub mod mlt_export;
pub mod mlt_import;
pub mod otio_import;
pub mod path_util;
mod save;
pub mod workspace;

pub use grade_stills::{delete_grade_still_file, load_grade_stills, save_freeze_frame_png, save_grade_stills, GradeStillData, GradeStillMeta};
pub use load::{load_looks, load_project};
pub use save::{iso8601_now, save_looks, save_project, save_project_full, save_project_with_color, save_project_with_color_and_backups, ColorSettings};
