//! Local-only ML model handling for Klipp.
//!
//! Two jobs. It manages the models themselves — the manifest, the
//! download, the SHA-256 check and the XDG cache — and it defines the
//! traits inference backends implement ([`mask_generator`]) together
//! with the [`registry`] that hands a caller a generator by id. The
//! backends live in their own crates (`klipp-ml-ort`,
//! `klipp-ml-candle`), so nothing here depends on an inference
//! runtime.
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
//! See `assets/models.toml` at the workspace root for the schema.
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
    download_to_cache, download_url_uncertified, head_content_length, write_and_compute,
    write_and_verify, DownloadError, NoOpProgress, ProgressSink,
};
pub use manifest::{
    DEFAULT_MANIFEST_VERSION, ManifestError, ModelEntry, ModelId, ModelKind, ModelManifest,
    ModelRuntime, PromptKinds, Source, load_effective, user_manifest_path,
};
pub use mask_generator::{
    BBox, ClickPrompt, BoxPrompt, Detection, DevicePreference, InputFrame, MaskBuffer,
    MaskCandidate, MaskCapabilities, MaskError, MaskGenerator, MaskPrompts, MaskRequest,
    MaskResponse, MaskTunables, ObjectDetector, ScribblePrompt, TemporalHint,
    TrackerCapabilities, VideoTracker,
};
pub use registry::{MaskGeneratorFactory, MaskGeneratorGuard, ModelInfo, ModelRegistry};
pub use stub::{STUB_GENERATOR_ID, StubMaskGenerator};
