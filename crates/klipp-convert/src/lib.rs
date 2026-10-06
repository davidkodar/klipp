#![forbid(unsafe_code)]
//! The one-time conversion of Klipp projects to the node-graph format
//! (#997, phase 1): read-only preparation, and its dry run. It depends on
//! no app crate, and the application must never depend on it: it reads
//! and writes the files as they are on disk.
pub mod assemble;
pub mod canon;
pub mod document;
pub mod dry_run;
pub mod ids;
pub mod installed;
pub mod lift;
pub mod old_fields;
pub mod revision;
pub mod running;
pub mod scan;
pub mod splice;
pub mod to_v1;
pub mod to_v2;
pub mod v1;
pub mod verify;
pub use document::{Document, Picture, PictureField, StoreKind};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod to_v2_tests;

#[cfg(test)]
mod lift_tests;
