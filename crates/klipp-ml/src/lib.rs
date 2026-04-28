//! Local-only ML model handling for Klipp.
//!
//! This crate is the foundation for the SAM2 object-mask feature
//! (tracker entry F-97). Scope today: model manifest + secure
//! download + checksum verification + XDG cache management. Inference
//! itself (wonnx integration) builds on top, separately.
//!
//! # Trust model
//!
//! Model bytes are verified via SHA-256 against a manifest baked into
//! the Klipp binary at compile time (`include_str!`). The manifest
//! itself is source-controlled and code-reviewed; new model entries
//! require a Git PR. This matches Cargo's verification model — bundled
//! checksums + HTTPS + manual review of new dependencies — and is the
//! same trust posture users already accept for Rust crates.
//!
//! See `assets/models.toml` at the workspace root for the schema; see
//! the F-97 entry in CODE_AUDIT.html for the full design rationale
//! (trust chain, residual risks, why Option 2 over Option 3).
//!
//! # No-unsafe stance
//!
//! This crate writes zero `unsafe { ... }` blocks. The downloader is
//! `ureq`, the hash is `sha2` (RustCrypto), the parser is `toml` —
//! all pure-Rust + safe-API. Internal `unsafe` inside those crates is
//! audited upstream.

pub mod manifest;

pub use manifest::{
    DEFAULT_MANIFEST_VERSION, ManifestError, ModelEntry, ModelId, ModelManifest, Source,
    user_manifest_path,
};
