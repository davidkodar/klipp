//! Model manifest — the trust root for every downloadable ML asset
//! in Klipp.
//!
//! ## Sources
//!
//! - **Builtin** — `assets/models.toml` at the workspace root, baked
//!   into the binary via `include_str!`. Code-reviewed in Git;
//!   tampering requires repo write access.
//! - **User override** — `~/.config/klipp/models.toml` (XDG). Optional.
//!   Lets corporate / air-gap installs point at internal mirrors
//!   *without dropping the SHA-256 check* — the user manifest still
//!   carries hashes, and the downloader still rejects anything that
//!   doesn't match.
//!
//! When both exist, the user manifest wins per-model: an entry with a
//! given `id` in the user file replaces the builtin entry of the same
//! `id`. Models present in builtin but not user are unchanged. Models
//! present in user but not builtin are added.
//!
//! ## Schema versioning
//!
//! Every manifest carries `version = "1"`. Future schema changes bump
//! the version + add migration code here. Loading a manifest with an
//! unknown version is a hard error — better than silently
//! misinterpreting fields.

use std::collections::BTreeMap;
use std::path::PathBuf;

use bitflags::bitflags;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Current manifest schema version. Bumped when the on-disk format
/// changes in a way that requires migration.
pub const DEFAULT_MANIFEST_VERSION: &str = "1";

/// Stable identifier for a model entry. Used by code to look up a
/// specific model; user-facing label is `display_name`. Renaming
/// `display_name` is fine; renaming `id` breaks every saved project
/// that referenced it.
pub type ModelId = String;

/// What trait a model implements. Drives registry dispatch — a
/// `MaskGenerator` entry registers into the mask-generator side of the
/// registry, a `VideoTracker` into the tracker side, etc. The default
/// (`MaskGenerator`) matches every existing v1-shape manifest entry,
/// so back-compat is preserved when this field is absent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    #[default]
    MaskGenerator,
    VideoTracker,
    ObjectDetector,
    /// Speech to text with timestamps: the transcript analyser.
    Transcriber,
    /// Where text is in a picture (the OCR detector).
    TextDetector,
    /// What a line of text says (the OCR recogniser).
    TextRecognizer,
    /// A voice for narration: text to speech.
    Voice,
    /// Faces in a picture, with landmarks.
    FaceDetector,
    /// Speech from noise: the Voice Isolation audio effect.
    SpeechEnhancer,
    /// Where in a sound someone is speaking: what the transcriber runs
    /// on, so it is not asked to find words in traffic.
    VoiceActivity,
    /// Text from one language to another: a caption translation pair.
    Translator,
    /// A translation pair's vocabulary, read beside its model.
    TranslationVocabulary,
    /// What a sound is, by AudioSet's classes: what proposes a clip's
    /// audio role (#2216).
    SoundTagger,
}

/// Which backend crate runs the model. Lets the registry pick the
/// right runtime at materialisation time without hard-coding model
/// names. `Stub` is the back-compat default for the existing
/// placeholder manifest — it routes to `StubMaskGenerator` so legacy
/// entries keep working as the dev placeholder until release CI
/// populates real bytes + flips them to a real runtime.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelRuntime {
    /// `klipp-ml-candle` backend (pure-Rust, no FFI).
    Candle,
    /// `klipp-ml-ort` backend — wraps the ONNX Runtime C library via
    /// the `ort` crate. Requires `unsafe` FFI; opt-in via Cargo
    /// feature in later phases.
    Ort,
    /// `klipp-ml-tract` backend (Phase 2+, experimental).
    Tract,
    /// whisper.cpp through `whisper-rs`, a ggml model file; C++ via
    /// `unsafe` FFI, built from source.
    WhisperCpp,
    /// In-process stub used by `StubMaskGenerator` and tests. No
    /// real inference, no network, no weights on disk.
    #[default]
    Stub,
    /// The `piper` text-to-speech program, run as a separate process
    /// with the voice file; nothing of it is linked in.
    Piper,
}

impl ModelRuntime {
    /// `true` for runtimes that wrap a C / C++ library via `unsafe`
    /// FFI bindings. The validator emits a warn-level log when one
    /// of these is reachable from the default build, so a user / CI
    /// can audit which manifests force a non-default trust boundary.
    pub fn requires_unsafe_ffi(self) -> bool {
        matches!(self, Self::Ort | Self::WhisperCpp)
    }
}

bitflags! {
    /// Which prompt categories a model accepts. Mirrors
    /// `MaskCapabilities` from `mask_generator.rs` but lives at the
    /// manifest layer so the UI can pre-load the right affordances
    /// *before* the model has been materialised (the registry only
    /// instantiates a model on first use; UI needs a capability hint
    /// at picker-render time, before any inference has run).
    ///
    /// Keep flag names + values in sync with `MaskCapabilities` —
    /// this is the on-disk hint, that is the runtime truth. A future
    /// validation pass can cross-check the two once a model is
    /// instantiated.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct PromptKinds: u32 {
        const POINT       = 1 << 0;
        const BOX         = 1 << 1;
        const SCRIBBLE    = 1 << 2;
        const TEXT        = 1 << 3;
        const AUTO_DETECT = 1 << 4;
        const TEMPORAL    = 1 << 5;
    }
}

impl PromptKinds {
    /// Render to the manifest string form: lowercase flag names
    /// joined with ` | `. Empty bitset renders as `""`. We hand-roll
    /// this rather than relying on bitflags' built-in serde feature
    /// so the on-disk text matches the casing humans actually want
    /// to read in `assets/models.toml` (`"point | box"`, not
    /// `"POINT | BOX"`).
    fn to_manifest_string(self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut parts: Vec<&'static str> = Vec::with_capacity(6);
        if self.contains(Self::POINT) {
            parts.push("point");
        }
        if self.contains(Self::BOX) {
            parts.push("box");
        }
        if self.contains(Self::SCRIBBLE) {
            parts.push("scribble");
        }
        if self.contains(Self::TEXT) {
            parts.push("text");
        }
        if self.contains(Self::AUTO_DETECT) {
            parts.push("auto_detect");
        }
        if self.contains(Self::TEMPORAL) {
            parts.push("temporal");
        }
        parts.join(" | ")
    }

    /// Parse the manifest string form back into a bitset. Whitespace
    /// around tokens + the separator is forgiving; unknown tokens are
    /// rejected with a parse error so a typo doesn't silently strip a
    /// capability.
    fn from_manifest_string(s: &str) -> Result<Self, String> {
        let mut out = Self::empty();
        for raw in s.split('|') {
            let token = raw.trim();
            if token.is_empty() {
                continue;
            }
            let bit = match token {
                "point" => Self::POINT,
                "box" => Self::BOX,
                "scribble" => Self::SCRIBBLE,
                "text" => Self::TEXT,
                "auto_detect" => Self::AUTO_DETECT,
                "temporal" => Self::TEMPORAL,
                other => {
                    return Err(format!(
                        "unknown prompt-kind {other:?} (valid: point | box | scribble | text | auto_detect | temporal)"
                    ));
                }
            };
            out |= bit;
        }
        Ok(out)
    }
}

impl Serialize for PromptKinds {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.to_manifest_string())
    }
}

impl<'de> Deserialize<'de> for PromptKinds {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Self::from_manifest_string(&s).map_err(serde::de::Error::custom)
    }
}

fn default_prompt_kinds() -> PromptKinds {
    PromptKinds::empty()
}

/// One downloadable model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelEntry {
    /// Stable identifier, code-facing (e.g. `"sam2_1_tiny"`).
    pub id: ModelId,
    /// User-facing label (e.g. `"SAM2.1 Tiny"`).
    pub display_name: String,
    /// File size in bytes — surfaced in the consent dialog and used
    /// as a sanity guard at HEAD-request time before committing to
    /// the download.
    pub size_bytes: u64,
    /// SHA-256 of the file, lowercase hex (no `0x`, no whitespace).
    /// THIS is the security boundary. The URL in `sources` is
    /// authoritative only insofar as the bytes it serves match this.
    pub sha256: String,
    /// SPDX license identifier (e.g. `"Apache-2.0"`). Disclosed to
    /// the user in the consent dialog.
    pub license: String,
    /// Attribution string (e.g. `"Meta AI Research"`). Disclosed to
    /// the user in the consent dialog.
    pub attribution: String,
    /// Ordered list of download URLs. The downloader tries each in
    /// order; first to succeed *and* match the SHA-256 wins. All
    /// entries must be `https://`.
    pub sources: Vec<Source>,
    /// Which trait this entry implements. Defaults to
    /// `MaskGenerator` so v1-shape manifests (without this field)
    /// keep parsing — older SAM2 placeholders were mask generators.
    #[serde(default)]
    pub kind: ModelKind,
    /// Which backend crate runs the model. Defaults to `Stub` so
    /// v1-shape manifests route to `StubMaskGenerator` rather than
    /// silently picking up a real (but unconfigured) runtime.
    #[serde(default)]
    pub runtime: ModelRuntime,
    /// Capability hint: which prompt kinds the model accepts. Lets
    /// the UI pre-load the right affordances before instantiation.
    /// Defaults to empty — a v1-shape entry advertises no prompt
    /// capabilities until its manifest is upgraded.
    #[serde(default = "default_prompt_kinds")]
    pub prompts: PromptKinds,
}

/// One download source. Wrapped in a struct (not a bare `String`) so
/// future per-source metadata (auth headers, mirror priority hints,
/// rate-limit notes) can land without breaking the schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub url: String,
}

/// A parsed model manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelManifest {
    pub version: String,
    #[serde(default, rename = "model")]
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to parse model manifest: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("unsupported manifest version {0:?} (expected {expected:?})", expected = DEFAULT_MANIFEST_VERSION)]
    UnsupportedVersion(String),
    #[error("manifest validation failed: {0}")]
    Invalid(String),
    #[error("io error reading user manifest: {0}")]
    Io(#[from] std::io::Error),
}

/// True the first time `id` is seen in this process.
fn first_notice(id: &ModelId) -> bool {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<ModelId>>> = std::sync::OnceLock::new();
    SEEN.get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert(id.clone()))
        .unwrap_or(false)
}

#[cfg(test)]
mod notice_tests {
    use super::*;

    #[test]
    fn a_model_is_noticed_once() {
        let id: ModelId = "notice_test_model".to_string();
        assert!(first_notice(&id));
        assert!(!first_notice(&id));
    }
}

impl ModelManifest {
    /// Parse a manifest from a TOML string. Validates schema version
    /// and per-entry invariants (non-empty id, https-only sources,
    /// 64-char lowercase-hex SHA-256, etc.) before returning.
    pub fn parse(toml_str: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(toml_str)?;
        if manifest.version != DEFAULT_MANIFEST_VERSION {
            return Err(ManifestError::UnsupportedVersion(manifest.version));
        }
        manifest.validate()?;
        Ok(manifest)
    }

    /// Run schema-level validation. Cheap, called from `parse`. Catches
    /// the kinds of mistakes that would otherwise surface as a
    /// confusing download failure later.
    pub fn validate(&self) -> Result<(), ManifestError> {
        let mut seen_ids = std::collections::HashSet::new();
        for entry in &self.models {
            if entry.id.is_empty() {
                return Err(ManifestError::Invalid("model id is empty".into()));
            }
            if !seen_ids.insert(entry.id.clone()) {
                return Err(ManifestError::Invalid(format!(
                    "duplicate model id: {}",
                    entry.id
                )));
            }
            if entry.sources.is_empty() {
                return Err(ManifestError::Invalid(format!(
                    "model {} has no sources",
                    entry.id
                )));
            }
            for src in &entry.sources {
                if !src.url.starts_with("https://") {
                    return Err(ManifestError::Invalid(format!(
                        "model {} source must be https://, got {:?}",
                        entry.id, src.url,
                    )));
                }
            }
            if entry.sha256.len() != 64
                || !entry.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            {
                return Err(ManifestError::Invalid(format!(
                    "model {} sha256 must be 64 lowercase hex chars, got {:?}",
                    entry.id, entry.sha256,
                )));
            }
            // `size_bytes == 0` is allowed as a placeholder marker
            // (used by the unfilled-by-release-CI shipping manifest)
            // — the downloader rejects 0 at pre-flight time so a
            // user never actually fetches an unverified model. We
            // don't reject it at parse time because that would
            // break `cargo test` against the placeholder manifest
            // before the release pipeline ever runs.
            if entry.display_name.is_empty() {
                return Err(ManifestError::Invalid(format!(
                    "model {} display_name is empty",
                    entry.id,
                )));
            }
            if entry.license.is_empty() {
                return Err(ManifestError::Invalid(format!(
                    "model {} license is empty",
                    entry.id,
                )));
            }
            if entry.runtime.requires_unsafe_ffi() && first_notice(&entry.id) {
                // Informational only at this layer. Later phases gate
                // FFI runtimes behind a Cargo feature; this log lets
                // the user / CI see that the manifest assumes the
                // opt-in build before the build itself fails.
                //
                // Once per model per process, at debug: this layer
                // cannot tell whether the runtime was built in, and the
                // manifest is validated on every load, which every
                // analyser does for every file, so a warning here put
                // a dozen lines in the log per imported clip.
                tracing::debug!(
                    model_id = %entry.id,
                    runtime = ?entry.runtime,
                    "model entry requires an unsafe-FFI runtime; ensure the corresponding backend feature is enabled at build time"
                );
            }
        }
        Ok(())
    }

    /// Look up a model by its stable id.
    pub fn get(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Merge a user-override manifest on top of this one. User entries
    /// win per-id; user-only entries are appended; builtin-only
    /// entries are unchanged. The user manifest is validated against
    /// the same rules — wrong-version / wrong-shape user files fail
    /// before the merge.
    pub fn merged_with(mut self, user: ModelManifest) -> Self {
        let mut by_id: BTreeMap<ModelId, ModelEntry> = self
            .models
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        for entry in user.models {
            by_id.insert(entry.id.clone(), entry);
        }
        self.models = by_id.into_values().collect();
        self.models.sort_by(|a, b| a.id.cmp(&b.id));
        self
    }
}

/// Resolved path of the optional user-override manifest. `None` when
/// the platform's config dir can't be determined (rare; happens on
/// some headless CI environments).
pub fn user_manifest_path() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("klipp").join("models.toml"))
}

/// Builtin manifest, baked into the binary at compile time. The
/// `include_str!` is the trust root: changing the bytes here requires
/// a Git PR, which is reviewed.
pub const BUILTIN_MANIFEST: &str = include_str!("../../../assets/models.toml");

/// Load the effective manifest: builtin merged with the user override
/// at `~/.config/klipp/models.toml` if present. Returns the builtin
/// alone when no user override exists. Errors propagate from either
/// parser; the user file is never silently ignored on parse failure
/// (a syntax mistake there should surface, not silently drop the
/// overrides).
pub fn load_effective() -> Result<ModelManifest, ManifestError> {
    let builtin = ModelManifest::parse(BUILTIN_MANIFEST)?;
    let Some(user_path) = user_manifest_path() else {
        return Ok(builtin);
    };
    if !user_path.exists() {
        return Ok(builtin);
    }
    let user_str = std::fs::read_to_string(&user_path)?;
    let user = ModelManifest::parse(&user_str)?;
    Ok(builtin.merged_with(user))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest_str() -> &'static str {
        r#"version = "1"

[[model]]
id = "sam2_1_tiny"
display_name = "SAM2.1 Tiny"
size_bytes = 162_435_072
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
license = "Apache-2.0"
attribution = "Meta AI Research"
sources = [
    { url = "https://example.com/sam2.1_hiera_tiny.onnx" },
]
"#
    }

    #[test]
    fn parses_valid_manifest() {
        let m = ModelManifest::parse(valid_manifest_str()).expect("valid");
        assert_eq!(m.version, "1");
        assert_eq!(m.models.len(), 1);
        assert_eq!(m.models[0].id, "sam2_1_tiny");
    }

    #[test]
    fn rejects_unknown_version() {
        let s = r#"version = "99"
"#;
        let err = ModelManifest::parse(s).expect_err("should fail");
        match err {
            ManifestError::UnsupportedVersion(v) => assert_eq!(v, "99"),
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn rejects_http_source() {
        let s = r#"version = "1"

[[model]]
id = "x"
display_name = "X"
size_bytes = 1
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
license = "Apache-2.0"
attribution = "x"
sources = [
    { url = "http://example.com/insecure" },
]
"#;
        let err = ModelManifest::parse(s).expect_err("http should be rejected");
        assert!(matches!(err, ManifestError::Invalid(_)),
            "expected Invalid, got {err:?}");
    }

    #[test]
    fn rejects_uppercase_hex_sha() {
        let s = r#"version = "1"

[[model]]
id = "x"
display_name = "X"
size_bytes = 1
sha256 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
license = "Apache-2.0"
attribution = "x"
sources = [
    { url = "https://example.com/x" },
]
"#;
        let err = ModelManifest::parse(s).expect_err("uppercase hex should be rejected");
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn rejects_short_sha() {
        let s = r#"version = "1"

[[model]]
id = "x"
display_name = "X"
size_bytes = 1
sha256 = "abc"
license = "Apache-2.0"
attribution = "x"
sources = [
    { url = "https://example.com/x" },
]
"#;
        let err = ModelManifest::parse(s).expect_err("short sha should be rejected");
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn allows_zero_size_at_parse_time_as_placeholder_marker() {
        // size_bytes = 0 is the "unfilled by release CI yet"
        // placeholder used in the shipping manifest before release
        // CI populates it. parse() must accept it; the downloader's
        // pre-flight check rejects it at runtime so a user never
        // actually downloads an unverified model.
        let s = r#"version = "1"

[[model]]
id = "x"
display_name = "X"
size_bytes = 0
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
license = "Apache-2.0"
attribution = "x"
sources = [
    { url = "https://example.com/x" },
]
"#;
        let m = ModelManifest::parse(s).expect("placeholder size accepted at parse");
        assert_eq!(m.models[0].size_bytes, 0);
    }

    #[test]
    fn rejects_no_sources() {
        let s = r#"version = "1"

[[model]]
id = "x"
display_name = "X"
size_bytes = 1
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
license = "Apache-2.0"
attribution = "x"
sources = []
"#;
        let err = ModelManifest::parse(s).expect_err("empty sources should be rejected");
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn rejects_duplicate_id() {
        let s = r#"version = "1"

[[model]]
id = "x"
display_name = "X"
size_bytes = 1
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
license = "Apache-2.0"
attribution = "x"
sources = [{ url = "https://example.com/x" }]

[[model]]
id = "x"
display_name = "X again"
size_bytes = 2
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
license = "Apache-2.0"
attribution = "x"
sources = [{ url = "https://example.com/x2" }]
"#;
        let err = ModelManifest::parse(s).expect_err("duplicate id should be rejected");
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn user_override_wins_per_id() {
        let builtin = ModelManifest::parse(valid_manifest_str()).unwrap();
        let user = ModelManifest::parse(
            r#"version = "1"

[[model]]
id = "sam2_1_tiny"
display_name = "SAM2.1 Tiny (corp mirror)"
size_bytes = 162_435_072
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
license = "Apache-2.0"
attribution = "Meta AI Research"
sources = [{ url = "https://internal-mirror.corp/sam2_1_tiny.onnx" }]
"#,
        )
        .unwrap();
        let merged = builtin.merged_with(user);
        let entry = merged.get("sam2_1_tiny").expect("present");
        assert_eq!(entry.display_name, "SAM2.1 Tiny (corp mirror)");
        assert_eq!(entry.sources[0].url, "https://internal-mirror.corp/sam2_1_tiny.onnx");
    }

    #[test]
    fn user_only_entry_appended() {
        let builtin = ModelManifest::parse(valid_manifest_str()).unwrap();
        let user = ModelManifest::parse(
            r#"version = "1"

[[model]]
id = "experimental_model"
display_name = "Experimental"
size_bytes = 1024
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
license = "Apache-2.0"
attribution = "Lab"
sources = [{ url = "https://lab.example.com/m.onnx" }]
"#,
        )
        .unwrap();
        let merged = builtin.merged_with(user);
        assert!(merged.get("sam2_1_tiny").is_some(), "builtin preserved");
        assert!(merged.get("experimental_model").is_some(), "user-only added");
    }

    #[test]
    fn builtin_manifest_parses_and_validates() {
        // The shipped manifest must parse cleanly. If a future
        // edit to assets/models.toml introduces a typo or breaks a
        // schema invariant, this test catches it before release.
        let m = ModelManifest::parse(BUILTIN_MANIFEST)
            .expect("builtin manifest must parse");
        assert!(!m.models.is_empty(),
            "builtin manifest should ship with at least one model entry");
    }

    #[test]
    fn user_manifest_path_is_under_config_dir() {
        // Don't require a config dir to exist (test sandboxes vary).
        // Just verify the API gives back a path under whatever
        // config dir does resolve.
        if let Some(p) = user_manifest_path() {
            let s = p.to_string_lossy();
            assert!(s.ends_with("klipp/models.toml"),
                "user manifest path should end with klipp/models.toml, got {s}");
        }
    }

    // ─── kind / runtime / prompts schema extensions ──────────────────

    #[test]
    fn v1_shape_manifest_applies_defaults_for_new_fields() {
        // The original v1 manifest has no kind / runtime / prompts
        // fields. They must be filled in with sensible defaults so
        // shipping manifests keep parsing across schema upgrades.
        let m = ModelManifest::parse(valid_manifest_str()).expect("v1 parses");
        let entry = &m.models[0];
        assert_eq!(entry.kind, ModelKind::MaskGenerator,
            "default kind should be MaskGenerator");
        assert_eq!(entry.runtime, ModelRuntime::Stub,
            "default runtime should be Stub (back-compat for placeholder manifests)");
        assert!(entry.prompts.is_empty(),
            "default prompts should be empty (no advertised capabilities)");
    }

    #[test]
    fn manifest_with_new_fields_round_trips() {
        let src = r#"version = "1"

[[model]]
id = "sam2_1_tiny"
display_name = "SAM2.1 Tiny"
size_bytes = 1
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
license = "Apache-2.0"
attribution = "Meta AI Research"
sources = [{ url = "https://example.com/x.onnx" }]
kind = "mask_generator"
runtime = "ort"
prompts = "point | box | scribble | temporal"
"#;
        let parsed = ModelManifest::parse(src).expect("parse with new fields");
        let entry = &parsed.models[0];
        assert_eq!(entry.kind, ModelKind::MaskGenerator);
        assert_eq!(entry.runtime, ModelRuntime::Ort);
        assert_eq!(
            entry.prompts,
            PromptKinds::POINT | PromptKinds::BOX | PromptKinds::SCRIBBLE | PromptKinds::TEMPORAL,
        );

        // Round-trip via serialize → parse and ensure equality.
        let toml_out = toml::to_string(&parsed).expect("serialize");
        let reparsed = ModelManifest::parse(&toml_out).expect("reparse");
        assert_eq!(parsed, reparsed,
            "manifest must round-trip through TOML serialise + parse");
    }

    #[test]
    fn prompt_kinds_compose_with_or() {
        let pk = PromptKinds::POINT | PromptKinds::BOX | PromptKinds::TEMPORAL;
        assert!(pk.contains(PromptKinds::POINT));
        assert!(pk.contains(PromptKinds::BOX));
        assert!(pk.contains(PromptKinds::TEMPORAL));
        assert!(!pk.contains(PromptKinds::TEXT));
        assert!(!pk.contains(PromptKinds::AUTO_DETECT));
        assert!(!pk.contains(PromptKinds::SCRIBBLE));
    }

    #[test]
    fn prompt_kinds_serde_round_trip() {
        // Container struct so we exercise the on-disk string form
        // exactly as it'll appear inside `[[model]]`.
        #[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
        struct Wrap {
            prompts: PromptKinds,
        }

        let original = Wrap {
            prompts: PromptKinds::POINT
                | PromptKinds::BOX
                | PromptKinds::SCRIBBLE
                | PromptKinds::TEMPORAL,
        };
        let s = toml::to_string(&original).expect("serialize");
        // Spot-check the on-disk form is the lowercase pipe-joined
        // string the manifest format documents.
        assert!(
            s.contains("prompts = \"point | box | scribble | temporal\""),
            "expected lowercase pipe-joined form in TOML, got: {s}"
        );
        let back: Wrap = toml::from_str(&s).expect("deserialize");
        assert_eq!(original, back);

        // Empty round-trips too.
        let empty = Wrap { prompts: PromptKinds::empty() };
        let s = toml::to_string(&empty).expect("serialize empty");
        let back: Wrap = toml::from_str(&s).expect("deserialize empty");
        assert_eq!(empty, back);

        // Whitespace-tolerant on parse.
        let parsed: Wrap =
            toml::from_str("prompts = \"  point|box  |  temporal  \"").expect("forgiving parse");
        assert_eq!(
            parsed.prompts,
            PromptKinds::POINT | PromptKinds::BOX | PromptKinds::TEMPORAL,
        );
    }

    #[test]
    fn prompt_kinds_rejects_unknown_token() {
        // Container with serde — same shape as inside the manifest.
        #[derive(Deserialize, Debug)]
        #[allow(dead_code)]
        struct Wrap {
            prompts: PromptKinds,
        }
        let err = toml::from_str::<Wrap>("prompts = \"point | bogus\"").expect_err("unknown token");
        let msg = err.to_string();
        assert!(msg.contains("bogus"), "error should mention the offending token: {msg}");
    }

    #[test]
    fn shipping_manifest_does_not_advertise_unavailable_sam2_models() {
        let m = ModelManifest::parse(BUILTIN_MANIFEST).expect("builtin parses");
        assert!(!m.models.is_empty(), "builtin should have entries");
        assert!(m.models.iter().all(|entry| !entry.id.starts_with("sam2")));
    }

    #[test]
    fn ort_runtime_marked_as_unsafe_ffi() {
        assert!(ModelRuntime::Ort.requires_unsafe_ffi());
        assert!(!ModelRuntime::Candle.requires_unsafe_ffi());
        assert!(!ModelRuntime::Tract.requires_unsafe_ffi());
        assert!(!ModelRuntime::Stub.requires_unsafe_ffi());
    }
}
