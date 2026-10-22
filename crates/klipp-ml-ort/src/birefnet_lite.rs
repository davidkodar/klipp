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
//! ## Running it
//!
//! With the crate's `ort` feature on, `generate` runs a real ONNX
//! Runtime session, built on first use and held for later calls; the
//! `cuda` feature asks ort for the CUDA execution provider, otherwise
//! it runs on CPU. Without the feature the pre- and post-process are
//! still compiled — they are plain CPU code — and `generate` returns
//! `MaskError::BackendInit`, so the model stays visible in the
//! registry and fails with something a user can act on.

use std::path::PathBuf;

use klipp_ml::{
    DevicePreference, MaskCapabilities, MaskError, MaskGenerator, MaskRequest, MaskResponse,
};

#[cfg(feature = "ort")]
use klipp_ml::MaskCandidate;

#[cfg(feature = "ort")]
use crate::{
    postprocess::postprocess,
    preprocess::{preprocess, BIREFNET_INPUT_SIZE},
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

    /// Materialise the generator. The ORT session is created lazily
    /// on the first `generate` call so an unused builder costs
    /// nothing.
    pub fn build(self) -> Result<BiRefNetLite, MaskError> {
        Ok(BiRefNetLite {
            weights_path: self.weights_path,
            device: self.device,
            #[cfg(feature = "ort")]
            session: None,
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
    /// Lazily-initialised ORT session. Built on the first call to
    /// `generate` so construction is cheap (the user might have
    /// switched models before any inference fired). Behind the `ort`
    /// feature; the no-feature build keeps the field absent so the
    /// crate links without any ort symbols.
    #[cfg(feature = "ort")]
    session: Option<ort::session::Session>,
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

    fn generate(&mut self, req: MaskRequest<'_>) -> Result<MaskResponse, MaskError> {
        if self.weights_path.is_none() {
            return Err(MaskError::BackendInit(
                "BiRefNet_lite weights not provided — run model download first".into(),
            ));
        }
        if req.frame.width == 0 || req.frame.height == 0 {
            return Err(MaskError::ShapeMismatch {
                expected_w: 1,
                expected_h: 1,
                actual_w: req.frame.width,
                actual_h: req.frame.height,
            });
        }
        let expected_pixels = (req.frame.width as usize) * (req.frame.height as usize) * 4;
        if req.frame.linear_rgba.len() < expected_pixels {
            return Err(MaskError::ShapeMismatch {
                expected_w: req.frame.width,
                expected_h: req.frame.height,
                actual_w: req.frame.width,
                actual_h: req.frame.height,
            });
        }

        #[cfg(not(feature = "ort"))]
        {
            let _ = req;
            Err(MaskError::BackendInit(
                "BiRefNet_lite ort backend not enabled — rebuild with --features ai-ort".into(),
            ))
        }

        #[cfg(feature = "ort")]
        {
            self.generate_with_ort(req)
        }
    }
}

#[cfg(feature = "ort")]
impl BiRefNetLite {
    /// Real inference path. Pre-process → bind input → run session →
    /// post-process. Splits out from `generate` so the no-feature
    /// build doesn't compile this body at all.
    fn generate_with_ort(
        &mut self,
        req: MaskRequest<'_>,
    ) -> Result<MaskResponse, MaskError> {
        let started = std::time::Instant::now();
        let weights = self
            .weights_path
            .clone()
            .ok_or_else(|| MaskError::BackendInit("weights path missing".into()))?;

        if self.session.is_none() {
            self.session = Some(build_session(&weights, self.device)?);
        }
        let session = self
            .session
            .as_mut()
            .expect("session built above");

        let pre = preprocess(&req.frame);
        debug_assert_eq!(pre.width, BIREFNET_INPUT_SIZE);
        debug_assert_eq!(pre.height, BIREFNET_INPUT_SIZE);

        // Build the input tensor (NCHW, 1×3×1024×1024) from the
        // pre-processed planar buffer. ort 2.0's `Value::from_array`
        // takes a shape + flat slice; ownership is moved in.
        let input_shape = [1_i64, 3, pre.height as i64, pre.width as i64];
        let input_tensor =
            ort::value::Value::from_array((input_shape, pre.data))
                .map_err(|e| MaskError::BackendInit(format!("input tensor: {e}")))?;

        let outputs = session
            .run(ort::inputs![input_tensor])
            .map_err(|e| MaskError::Inference(format!("ort run: {e}")))?;

        // BiRefNet returns a single output (a logits map). Extract
        // the first tensor; `try_extract_tensor` returns
        // (&Shape, &[f32]) where Shape derefs to &[i64].
        let first_output = outputs
            .iter()
            .next()
            .ok_or_else(|| MaskError::Inference("ort returned no outputs".into()))?
            .1;
        let (out_shape, out_data) = first_output
            .try_extract_tensor::<f32>()
            .map_err(|e| MaskError::Inference(format!("extract output: {e}")))?;

        // Output shape is typically (1, 1, H, W). Some exports drop
        // the leading channel dim. Pick H, W defensively.
        let dims: &[i64] = out_shape;
        let (out_h, out_w) = match dims {
            [1, 1, h, w] | [1, h, w] => (*h as u32, *w as u32),
            other => {
                return Err(MaskError::Inference(format!(
                    "unexpected output shape {other:?} (expected (1,1,H,W) or (1,H,W))"
                )));
            }
        };

        let mask = postprocess(out_data, out_w, out_h, req.frame.width, req.frame.height);

        Ok(MaskResponse {
            candidates: vec![MaskCandidate {
                mask,
                confidence: 1.0,
                bbox: None,
                label: Some("foreground".into()),
            }],
            inference_ms: started.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(feature = "ort")]
fn build_session(
    weights: &std::path::Path,
    device: DevicePreference,
) -> Result<ort::session::Session, MaskError> {
    use ort::session::builder::GraphOptimizationLevel;

    // Before touching ort at all: a missing dylib deadlocks its lazy
    // init rather than erroring. See `ensure_runtime_loadable`.
    crate::runtime_resolve::ensure_runtime_loadable().map_err(MaskError::BackendInit)?;

    let mut builder = ort::session::Session::builder()
        .map_err(|e| MaskError::BackendInit(format!("session builder: {e}")))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| MaskError::BackendInit(format!("set optimisation: {e}")))?;

    // On the GPU where it loads, else the CPU, what it got recorded
    // (#2250).
    let model = weights.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    builder = crate::provider::on_device(builder, device, &model).map_err(MaskError::BackendInit)?;

    builder
        .commit_from_file(weights)
        .map_err(|e| MaskError::BackendInit(format!("load weights: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_ml::{InputFrame, MaskPrompts, MaskTunables};

    fn dummy_request<'a>(pixels: &'a [f32]) -> MaskRequest<'a> {
        MaskRequest {
            frame: InputFrame {
                width: 4,
                height: 4,
                linear_rgba: pixels,
            },
            prompts: MaskPrompts::default(),
            temporal: None,
            tunables: MaskTunables::default(),
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
    #[cfg(not(feature = "ort"))]
    fn generate_without_ort_feature_says_rebuild_with_flag() {
        // Without the `ort` feature, even a "valid" weights path
        // returns BackendInit — but with a different message so users
        // can tell their weights resolved correctly and the only
        // thing missing is the build flag.
        let pixels = vec![0.0_f32; 4 * 4 * 4];
        let mut gen = BiRefNetLiteBuilder::new()
            .weights(PathBuf::from("/tmp/birefnet.onnx"))
            .build()
            .expect("build");
        let err = gen
            .generate(dummy_request(&pixels))
            .expect_err("ort feature off");
        match err {
            MaskError::BackendInit(msg) => {
                assert!(
                    msg.to_lowercase().contains("ai-ort")
                        || msg.to_lowercase().contains("feature"),
                    "error should mention the build flag: {msg}"
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
