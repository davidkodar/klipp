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

pub mod lock;
pub mod recovery;
pub mod metadata_sheet;
pub mod brand_kit;
pub mod consolidate;
pub mod collect_verify;
pub mod collect_report;
pub mod collect_analysis;
pub mod project_fonts;
pub mod consolidate_trim;
mod format;
pub mod fcpxml;
pub mod fcpxml_import;
pub mod xmeml_import;
pub mod edl_import;
pub mod grade_stills;
pub mod made_media;
mod load;
pub mod migrate_ids;
pub mod mlt_export;
mod not_carried;
pub mod mlt_import;
pub mod import_report;
pub mod otio_export;
pub mod otio_tool;
pub mod otio_import;
pub mod path_util;
mod save;
mod unique_ids;
pub mod template;
pub mod template_pack;
pub mod workspace;

pub use grade_stills::{adopt_still, adopt_temp_stills, delete_grade_still_file, is_temp_still, load_grade_stills, save_freeze_frame_png, save_grade_stills, temp_still_roots, GradeStillData, GradeStillMeta, StillAdoption, StillSource};
/// The newest project format this Klipp reads and writes (#994): the
/// `format_revision` a save writes, and the highest a load accepts. It
/// goes up only when a change to the file would be *misread* by a build
/// that knows the previous revision (data moved or removed, not merely
/// added), and that change's PR raises it and says why. 2 is the node-graph
/// format (#997, #1089): the pictures in `graph_v2`, read back into this
/// model through `klipp_graph_lift::unlift` and written through
/// `revision2`. 3 reads each effect's node id as stored, so an effect
/// keeps its id through a reorder and a save (`revision3`); a revision-2
/// reader would rename them. 4 is picture revision 4 (M4 step F): its
/// stored forms change for #1391, #1406, #1409, #1386 and the plug-in
/// nodes, so a revision-3 reader would misread them. 5 keeps a tracked
/// surface's keys as maps from its reference corners, with the tracker's
/// confidence (#2754): their corners are gone, so a revision-4 reader
/// would find no track. A file from before it is refused, naming the tool
/// that converts it.
pub const FORMAT_REVISION: u32 = 5;

/// The side stores' revision (a Look, the stills index, a colour preset):
/// they hold no effects, so they stay at 2 while the project is at 3.
pub const SIDE_STORE_REVISION: u32 = 2;

/// Which form of the project file a load reads or a save writes: revision 2,
/// the app's, or today's v1 form, kept only for the converter's proofs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileForm {
    Revision2,
    #[cfg(feature = "v1-oracle")]
    V1,
}

pub use load::{load_looks, load_project, load_project_strict, migrate_graph, side_store_revision_refusal};
#[cfg(feature = "v1-oracle")]
pub use load::{load_looks_v1, load_project_v1};
#[cfg(feature = "v1-oracle")]
pub use save::{save_looks_v1, save_project_v1};
#[cfg(feature = "v1-oracle")]
pub use grade_stills::{load_grade_stills_v1, save_grade_stills_v1};
pub use made_media::{adopt_made_media, MediaAdoption};
pub use save::{iso8601_now, media_items_save_alike, project_digest, save_looks, save_project, save_project_full, save_project_with_color, save_project_with_color_and_backups, ColorSettings};
