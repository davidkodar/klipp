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

pub mod cache;
pub mod download;
pub mod manifest;
pub mod mask_generator;
pub mod registry;
pub mod stub;

pub use cache::{CacheError, CachedStatus, ModelCache, sha256_of_file};
pub use download::{
    DownloadError, NoOpProgress, ProgressSink, download_to_cache, write_and_verify,
};
pub use manifest::{
    DEFAULT_MANIFEST_VERSION, ManifestError, ModelEntry, ModelId, ModelKind, ModelManifest,
    ModelRuntime, PromptKinds, Source, load_effective, user_manifest_path,
};
pub use mask_generator::{
    BBox, ClickPrompt, BoxPrompt, Detection, DevicePreference, InputFrame, MaskBuffer,
    MaskCandidate, MaskCapabilities, MaskError, MaskGenerator, MaskPrompts, MaskRequest,
    MaskResponse, ObjectDetector, ScribblePrompt, TemporalHint, TrackerCapabilities,
    VideoTracker,
};
pub use registry::{MaskGeneratorFactory, MaskGeneratorGuard, ModelInfo, ModelRegistry};
pub use stub::{STUB_GENERATOR_ID, StubMaskGenerator};
