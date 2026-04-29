//! MobileSAM — click-to-mask via TinyViT image encoder + SAM prompt
//! encoder + mask decoder. Pure-Rust through candle-transformers.
//!
//! ## Architecture (3 stages)
//!
//! 1. **Image encoder** (TinyViT, ~40 MB) — heavy, ~once per source
//!    frame. Outputs a `(1, 256, 64, 64)` image embedding.
//! 2. **Prompt encoder** — lightweight. Converts user clicks +
//!    boxes + scribbles into prompt embeddings.
//! 3. **Mask decoder** — lightweight. Combines image + prompt
//!    embeddings → 3-mask output (small/medium/large) with IoU
//!    predictions, scaled back to source resolution.
//!
//! Interactive use (click → mask in <100 ms) caches the image
//! embedding per frame and re-runs only stages 2-3 on each click.
//! `&mut self` in the trait carries this cache across `generate`
//! calls.
//!
//! ## Pre-process
//!
//! 1. Resize source frame to 1024×1024 with bilinear interpolation
//!    (SAM's standard input size).
//! 2. Apply ImageNet mean/std normalisation:
//!    `mean = [0.485, 0.456, 0.406]`, `std = [0.229, 0.224, 0.225]`.
//! 3. Layout `(1, 3, 1024, 1024)` f32, on whichever device candle
//!    selected (CPU / CUDA).
//!
//! ## Post-process
//!
//! Mask decoder outputs three candidate masks at 256×256 per click;
//! we sigmoid them, take the one with the highest IoU prediction
//! (or all three if `MaskCapabilities::AUTO_DETECT` mode), and
//! bilinear-resample to source resolution.
//!
//! ## Today's status (foundation commit)
//!
//! No candle dependency yet. `MobileSam::generate` returns
//! `MaskError::BackendInit("candle backend not enabled — rebuild
//! with --features ai-candle")` so the model is registry-visible
//! but inference fails actionably until the real candle path is
//! wired in the next commit.

use std::path::PathBuf;

use klipp_ml::{
    DevicePreference, MaskCapabilities, MaskError, MaskGenerator, MaskRequest, MaskResponse,
};

/// Stable id used in `assets/models.toml` and the registry.
pub const MOBILE_SAM_MODEL_ID: &str = "mobile_sam_v1";

/// User-facing display name.
pub const MOBILE_SAM_DISPLAY_NAME: &str = "MobileSAM";

/// SAM family input resolution. Both MobileSAM and SAM v1 expect
/// 1024×1024 — a single constant for both keeps the pre-process
/// helper reusable when SAM v1 ViT-B lands.
pub const SAM_INPUT_SIZE: u32 = 1024;

/// Builder for the MobileSAM generator.
///
/// Construction surface separate from the trait surface (kalosm
/// pattern) — keeps the runtime trait minimal while letting weights /
/// device options grow non-breakingly.
pub struct MobileSamBuilder {
    /// Path to the locally-cached TinyViT image encoder weights
    /// (.safetensors). MobileSAM ships two files: a TinyViT
    /// checkpoint for the encoder and a SAM-decoder checkpoint for
    /// stages 2-3. Some distributions bundle both into one .pt that
    /// candle's loader splits internally.
    weights_path: Option<PathBuf>,
    /// Device hint. `Cpu` forces CPU; `GpuPreferCpu` (default)
    /// tries CUDA first, falls back to CPU. `GpuRequired` errors
    /// out if no CUDA device is available.
    device: DevicePreference,
}

impl MobileSamBuilder {
    pub fn new() -> Self {
        Self {
            weights_path: None,
            device: DevicePreference::GpuPreferCpu,
        }
    }

    /// Path to the cached MobileSAM weights file. Resolved via
    /// [`klipp_ml::ModelCache`] from the manifest entry
    /// `mobile_sam_v1`.
    pub fn weights(mut self, path: PathBuf) -> Self {
        self.weights_path = Some(path);
        self
    }

    /// Override the device. `GpuPreferCpu` is the sensible default
    /// for users with NVIDIA hardware; `Cpu` for slower-but-portable.
    pub fn device(mut self, device: DevicePreference) -> Self {
        self.device = device;
        self
    }

    /// Materialise the generator. Today this never opens the
    /// candle session; session-init lands behind the `candle`
    /// feature flag.
    pub fn build(self) -> Result<MobileSam, MaskError> {
        Ok(MobileSam {
            weights_path: self.weights_path,
            device: self.device,
            #[cfg(feature = "candle")]
            session: None,
        })
    }
}

impl Default for MobileSamBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// MobileSAM mask generator. Implements [`MaskGenerator`] with
/// click-to-mask shape (POINT + BOX + SCRIBBLE).
pub struct MobileSam {
    weights_path: Option<PathBuf>,
    device: DevicePreference,
    /// Lazily-initialised candle pipeline. Built on first
    /// `generate` call — caching here keeps the user's "switch to
    /// MobileSAM in the model picker" gesture cheap (no session
    /// init until they actually run).
    #[cfg(feature = "candle")]
    session: Option<CandleSession>,
}

/// Internal candle pipeline state. Holds the loaded model, cached
/// image embedding, and the device. Stub today — real fields land
/// when the `candle` feature actually does something.
#[cfg(feature = "candle")]
struct CandleSession {
    // Real fields land here when the candle integration lands:
    //   device: candle_core::Device,
    //   model:  candle_transformers::models::segment_anything::sam::Sam,
    //   cached_embedding: Option<candle_core::Tensor>,
    //   cached_for_frame_hash: Option<u64>,
}

impl MaskGenerator for MobileSam {
    fn id(&self) -> &str {
        MOBILE_SAM_MODEL_ID
    }

    fn display_name(&self) -> &str {
        MOBILE_SAM_DISPLAY_NAME
    }

    fn capabilities(&self) -> MaskCapabilities {
        // SAM family supports point clicks, box prompts, and free
        // scribbles. AUTO_DETECT (no prompts) yields the
        // "everything mask" — useful for export but not the
        // primary interaction.
        MaskCapabilities::POINT_PROMPT
            | MaskCapabilities::BOX_PROMPT
            | MaskCapabilities::SCRIBBLE
            | MaskCapabilities::AUTO_DETECT
    }

    fn preferred_device(&self) -> DevicePreference {
        self.device
    }

    fn generate(&mut self, req: MaskRequest<'_>) -> Result<MaskResponse, MaskError> {
        if self.weights_path.is_none() {
            return Err(MaskError::BackendInit(
                "MobileSAM weights not provided — run model download first".into(),
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

        #[cfg(not(feature = "candle"))]
        {
            let _ = req;
            Err(MaskError::BackendInit(
                "MobileSAM candle backend not enabled — rebuild with --features ai-candle".into(),
            ))
        }

        #[cfg(feature = "candle")]
        {
            self.generate_with_candle(req)
        }
    }
}

#[cfg(feature = "candle")]
impl MobileSam {
    /// Real inference path. Stub for now — the candle `Sam` model
    /// load + image-encoder forward + prompt-encoder forward +
    /// mask-decoder forward all live here once the `candle` feature
    /// integration is fleshed out (next commit).
    fn generate_with_candle(
        &mut self,
        _req: MaskRequest<'_>,
    ) -> Result<MaskResponse, MaskError> {
        Err(MaskError::BackendInit(
            "MobileSAM candle session not yet implemented — coming in F-97-MOBILESAM phase 2".into(),
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
        let g = MobileSamBuilder::new().build().expect("build");
        assert_eq!(g.id(), MOBILE_SAM_MODEL_ID);
        assert_eq!(g.display_name(), MOBILE_SAM_DISPLAY_NAME);
    }

    #[test]
    fn capabilities_match_isolate_object_task() {
        let g = MobileSamBuilder::new().build().expect("build");
        let caps = g.capabilities();
        assert!(caps.contains(MaskCapabilities::POINT_PROMPT));
        assert!(caps.contains(MaskCapabilities::BOX_PROMPT));
        assert!(caps.contains(MaskCapabilities::SCRIBBLE));
        assert!(caps.contains(MaskCapabilities::AUTO_DETECT));
        // SAM v1 doesn't track across frames — that's SAM2's job.
        assert!(!caps.contains(MaskCapabilities::TEMPORAL));
    }

    #[test]
    fn generate_without_weights_returns_actionable_error() {
        let pixels = vec![0.0_f32; 4 * 4 * 4];
        let mut g = MobileSamBuilder::new().build().expect("build");
        let err = g
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
    #[cfg(not(feature = "candle"))]
    fn generate_without_candle_feature_says_rebuild_with_flag() {
        let pixels = vec![0.0_f32; 4 * 4 * 4];
        let mut g = MobileSamBuilder::new()
            .weights(PathBuf::from("/tmp/mobile_sam.safetensors"))
            .build()
            .expect("build");
        let err = g
            .generate(dummy_request(&pixels))
            .expect_err("candle feature off");
        match err {
            MaskError::BackendInit(msg) => {
                assert!(
                    msg.to_lowercase().contains("ai-candle")
                        || msg.to_lowercase().contains("feature"),
                    "error should mention the build flag: {msg}"
                );
            }
            other => panic!("expected BackendInit, got {other:?}"),
        }
    }

    #[test]
    fn shape_mismatch_on_zero_size_frame() {
        let pixels = Vec::<f32>::new();
        let req = MaskRequest {
            frame: InputFrame {
                width: 0,
                height: 0,
                linear_rgba: &pixels,
            },
            prompts: MaskPrompts::default(),
            temporal: None,
        };
        let mut g = MobileSamBuilder::new()
            .weights(PathBuf::from("/tmp/dummy"))
            .build()
            .expect("build");
        match g.generate(req).unwrap_err() {
            MaskError::ShapeMismatch { actual_w: 0, actual_h: 0, .. } => {}
            other => panic!("expected ShapeMismatch with 0x0 actual, got {other:?}"),
        }
    }

    #[test]
    fn shape_mismatch_on_short_pixel_buffer() {
        let pixels = vec![0.0_f32; 4]; // way too short for 4x4
        let req = MaskRequest {
            frame: InputFrame {
                width: 4,
                height: 4,
                linear_rgba: &pixels,
            },
            prompts: MaskPrompts::default(),
            temporal: None,
        };
        let mut g = MobileSamBuilder::new()
            .weights(PathBuf::from("/tmp/dummy"))
            .build()
            .expect("build");
        match g.generate(req).unwrap_err() {
            MaskError::ShapeMismatch { .. } => {}
            other => panic!("expected ShapeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn id_and_display_name_are_distinct() {
        let g = MobileSamBuilder::new().build().expect("build");
        assert_ne!(g.id(), g.display_name());
    }
}
