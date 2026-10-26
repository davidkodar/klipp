#![forbid(unsafe_code)]
//! The one-time conversion of Klipp projects to the node-graph format
//! (#997, phase 1): read-only preparation, and its dry run. It depends on
//! no app crate, and the application must never depend on it: it reads
//! and writes the files as they are on disk.
pub mod adapter;
pub mod assemble;
pub mod document;
pub mod dry_run;
pub mod installed;
pub mod run;
pub mod running;
pub mod scan;
pub mod step3;
pub mod step4;
pub mod step5;
pub mod verify;
/// The pure conversion, in its own crate so the app can use it.
pub use klipp_graph_lift::{canon, delift, ids, lift, old_fields, ops, paths, revision, revision3, splice, to_v1, to_v2, unlift, v1};
pub mod revision2;
pub use document::{Document, Picture, PictureField, StoreKind};

#[cfg(test)]
mod tests;
