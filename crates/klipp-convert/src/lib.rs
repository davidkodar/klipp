#![forbid(unsafe_code)]
//! The one-time conversion of Klipp projects to the node-graph format
//! (#997, phase 1): read-only preparation, and its dry run. It depends on
//! no app crate, and the application must never depend on it: it reads
//! and writes the files as they are on disk.
pub mod document;
pub mod running;
pub mod scan;
pub mod v1;
pub use document::{Document, Picture, PictureField, StoreKind};
