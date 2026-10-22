//! The document: the edit history and the application of its commands
//! to the sequences and the media pool, with no UI and no engine in
//! the loop.
//!
//! `history` is the command set (`EditCommand`), the per-sequence
//! undo/redo stack (`CommandHistory`) and its batching; `apply` runs a
//! command forward or in reverse over a [`apply::DocumentView`] and
//! returns the [`apply::Effects`] the host must then perform — a
//! sequence pushed to the engine, a title re-rendered — so the
//! document never reaches into a window or a device.
//!
//! `ops` is intent in, geometry out: an edit said in editorial terms,
//! resolved against a copy of the document into the recorded commands
//! that make it, or refused with a reason; a plan is one undo step.
//! `control` carries a client's commands and plans to the host that
//! owns the document, and its acknowledgement back.
//!
//! This is the first step of the document owning what the UI used to:
//! the code moved here verbatim from `klipp-ui`, the UI calls it, and
//! the next steps route the control socket and an assistant through
//! the same path.

pub mod apply;
pub mod blade;
pub mod caption_translation;
pub mod control;
pub mod edit_plan;
pub mod find;
pub mod history;
pub mod generators;
pub mod ops;
pub mod qc;
pub mod qc_geometry;
pub mod qc_review;
pub mod review;
pub mod sound_design;
pub mod title_style;
