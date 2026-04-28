//! `BiRefNet_lite` — first user-facing AI feature for Klipp.
//!
//! Architecture (Swin-Tiny backbone, single-input single-output mask)
//! is stateless and slots into Klipp's per-frame effects pipeline
//! without recurrent state. Model weights are MIT-licensed
//! (huggingface.co/onnx-community/BiRefNet_lite-ONNX), 214 MB fp32 /
//! ~110 MB INT8.
//!
//! Pre-process (per the upstream reference implementation):
//!
//! 1. Resize the input frame to 1024×1024 with bilinear interpolation.
//! 2. Normalise per-channel using ImageNet stats —
//!    mean = [0.485, 0.456, 0.406], std = [0.229, 0.224, 0.225].
//! 3. Layout NCHW (1, 3, 1024, 1024) f32.
//!
//! Post-process: single-channel logits → sigmoid → alpha mask,
//! resampled back to the source frame resolution.
//!
//! ## Today's status (foundation commit)
//!
//! No `ort` dependency yet — `generate` returns
//! `MaskError::BackendInit("ort backend not yet wired")` so the model
//! is visible in the registry but inference fails with an actionable
//! message. The full pre-process + ORT session + post-process body
//! lands when `ort = "=2.0.0-rc.12"` joins the dep tree (next
//! session).

use std::path::PathBuf;

use klipp_ml::{
    DevicePreference, MaskCapabilities, MaskError, MaskGenerator, MaskRequest, MaskResponse,
};

/// Stable id used throughout `assets/models.toml` and the registry.
pub const BIREFNET_LITE_MODEL_ID: &str = "birefnet_lite_v1";

/// User-facing display name for the picker.
pub const BIREFNET_LITE_DISPLAY_NAME: &str = "BiRefNet Lite";

/// Builder for the BiRefNet_lite generator.
///
/// Construction surface kept separate from the trait surface
/// (kalosm pattern) — adding a builder field is non-breaking and
/// keeps the runtime trait minimal.
pub struct BiRefNetLiteBuilder {
    weights_path: Option<PathBuf>,
    device: DevicePreference,
}

impl BiRefNetLiteBuilder {
    pub fn new() -> Self {
        Self {
            weights_path: None,
            // BiRefNet runs comfortably on CPU at 1024² but benefits
            // hugely from GPU. Prefer GPU, fall back gracefully.
            device: DevicePreference::GpuPreferCpu,
        }
    }

    /// Path to the locally-cached `.onnx` weights file. Typically
    /// resolved via [`klipp_ml::ModelCache`] from the manifest entry
    /// `birefnet_lite_v1`.
    pub fn weights(mut self, path: PathBuf) -> Self {
        self.weights_path = Some(path);
        self
    }

    /// Override the device. Default is `Auto` — the backend picks
    /// CUDA if available, falls back to CPU otherwise.
    pub fn device(mut self, device: DevicePreference) -> Self {
        self.device = device;
        self
    }

    /// Materialise the generator. Today this never opens the ORT
    /// session; the session-init lands behind the `ort` feature flag
    /// in a follow-up.
    pub fn build(self) -> Result<BiRefNetLite, MaskError> {
        Ok(BiRefNetLite {
            weights_path: self.weights_path,
            device: self.device,
        })
    }
}

impl Default for BiRefNetLiteBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// BiRefNet_lite mask generator. Implements [`MaskGenerator`] with
/// `RemoveBackground`-shaped capabilities (auto-detect only — there
/// are no clicks or boxes for matting).
pub struct BiRefNetLite {
    /// Path to the locally cached weights. `None` until the cache
    /// resolves, in which case `generate` errors out.
    weights_path: Option<PathBuf>,
    /// User-requested device. `Auto` lets the backend pick.
    device: DevicePreference,
}

impl MaskGenerator for BiRefNetLite {
    fn id(&self) -> &str {
        BIREFNET_LITE_MODEL_ID
    }

    fn display_name(&self) -> &str {
        BIREFNET_LITE_DISPLAY_NAME
    }

    fn capabilities(&self) -> MaskCapabilities {
        // Background matting needs no prompts — the entire frame is
        // the input. AUTO_DETECT is the matching capability.
        MaskCapabilities::AUTO_DETECT
    }

    fn preferred_device(&self) -> DevicePreference {
        self.device
    }

    fn generate(&mut self, _req: MaskRequest<'_>) -> Result<MaskResponse, MaskError> {
        if self.weights_path.is_none() {
            return Err(MaskError::BackendInit(
                "BiRefNet_lite weights not provided — run model download first".into(),
            ));
        }
        // Real path lands when the `ort` feature flag is wired and
        // the prebuilt-binary distribution story is locked in.
        Err(MaskError::BackendInit(
            "BiRefNet_lite ort backend not yet wired — enable --features ai-ort in a future build".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_ml::{InputFrame, MaskPrompts};

    fn dummy_request<'a>(pixels: &'a [f32]) -> MaskRequest<'a> {
        MaskRequest {
            frame: InputFrame {
                width: 4,
                height: 4,
                linear_rgba: pixels,
            },
            prompts: MaskPrompts::default(),
            temporal: None,
        }
    }

    #[test]
    fn builder_default_constructs_without_weights() {
        let gen = BiRefNetLiteBuilder::new().build().expect("build");
        assert_eq!(gen.id(), BIREFNET_LITE_MODEL_ID);
        assert_eq!(gen.display_name(), BIREFNET_LITE_DISPLAY_NAME);
        assert!(gen
            .capabilities()
            .contains(MaskCapabilities::AUTO_DETECT));
    }

    #[test]
    fn builder_accepts_weights_path() {
        let gen = BiRefNetLiteBuilder::new()
            .weights(PathBuf::from("/tmp/birefnet.onnx"))
            .build()
            .expect("build");
        assert!(gen.weights_path.is_some());
    }

    #[test]
    fn generate_without_weights_returns_actionable_error() {
        let pixels = vec![0.0_f32; 4 * 4 * 4];
        let mut gen = BiRefNetLiteBuilder::new().build().expect("build");
        let err = gen
            .generate(dummy_request(&pixels))
            .expect_err("must error without weights");
        match err {
            MaskError::BackendInit(msg) => {
                assert!(
                    msg.to_lowercase().contains("weights"),
                    "error should mention weights: {msg}"
                );
            }
            other => panic!("expected BackendInit, got {other:?}"),
        }
    }

    #[test]
    fn generate_with_weights_currently_says_backend_unwired() {
        // Until the `ort` feature lands, even a "valid" weights path
        // returns BackendInit — but with a different message so users
        // can tell their weights resolved correctly.
        let pixels = vec![0.0_f32; 4 * 4 * 4];
        let mut gen = BiRefNetLiteBuilder::new()
            .weights(PathBuf::from("/tmp/birefnet.onnx"))
            .build()
            .expect("build");
        let err = gen
            .generate(dummy_request(&pixels))
            .expect_err("ort backend not wired");
        match err {
            MaskError::BackendInit(msg) => {
                assert!(
                    msg.to_lowercase().contains("ort"),
                    "error should mention ort: {msg}"
                );
            }
            other => panic!("expected BackendInit, got {other:?}"),
        }
    }

    #[test]
    fn capabilities_match_remove_background_task() {
        let gen = BiRefNetLiteBuilder::new().build().expect("build");
        let caps = gen.capabilities();
        // BiRefNet is auto-detect only; no clicks, boxes, scribbles.
        assert!(caps.contains(MaskCapabilities::AUTO_DETECT));
        assert!(!caps.contains(MaskCapabilities::POINT_PROMPT));
        assert!(!caps.contains(MaskCapabilities::BOX_PROMPT));
        assert!(!caps.contains(MaskCapabilities::SCRIBBLE));
        assert!(!caps.contains(MaskCapabilities::TEMPORAL));
    }
}
