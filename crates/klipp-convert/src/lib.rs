#![forbid(unsafe_code)]
//! Standalone, read-only preparation for project conversion.
//! The application must never depend on this crate.
pub mod document;
pub mod v1;
pub use document::{Document, Picture, PictureField, StoreKind};
