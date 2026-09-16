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
//! This is the first step of the document owning what the UI used to:
//! the code moved here verbatim from `klipp-ui`, the UI calls it, and
//! the next steps route the control socket and an assistant through
//! the same path.

pub mod apply;
pub mod history;
