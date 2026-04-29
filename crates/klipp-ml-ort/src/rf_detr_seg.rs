//! RF-DETR-Seg — auto-detect with per-instance segmentation. The
//! third F-97 model after BiRefNet (Remove Background) and
//! MobileSAM (Isolate Object).
//!
//! ## Architecture
//!
//! Real-time DEtection TRansformer (RF-DETR) is roboflow's
//! transformer-based detector that's a strict ONNX-Apache-licensed
//! alternative to Ultralytics YOLO (AGPL). The `-Seg` variant adds
//! a segmentation head that emits per-detection masks alongside
//! bounding boxes.
//!
//! Outputs (per the upstream ONNX export):
//!
//! 1. `boxes` — `(1, N, 4)` xyxy in normalised [0,1] image coords
//! 2. `scores` — `(1, N)` confidence per detection
//! 3. `labels` — `(1, N)` class id index into COCO-80
//! 4. `mask_coefficients` — `(1, N, 32)` per-detection mask
//!    coefficients
//! 5. `mask_prototypes` — `(1, 32, H, W)` shared mask prototypes
//!    where H, W ≈ model_input / 4
//!
//! Per-detection mask = sigmoid(sum_k(coeff_nk * proto_kij)) — a
//! 32-element linear combination of prototype masks weighted by the
//! detection's coefficients, then per-pixel sigmoid, then crop to
//! the detection's bbox.
//!
//! Each detection becomes one [`MaskCandidate`] with `bbox`, `label`
//! (looked up in [`COCO_LABELS`]), `confidence`, and `mask` set.
//! The AI Tools panel today only renders the first candidate; full
//! multi-detection visualisation (numbered bbox overlays + class
//! labels in the viewer) is a follow-up.
//!
//! ## Pre-process
//!
//! Resize to model input (typically 432×432 or 640×640 depending on
//! variant), normalise via ImageNet mean/std, layout NCHW f32.
//!
//! ## Today's status (foundation commit)
//!
//! No `ort` integration yet. `RfDetrSeg::generate` returns
//! `MaskError::BackendInit` with two distinguishable messages
//! (no-weights vs ort-not-wired) so the model is registry-visible
//! but inference fails actionably until the real ort session lands
//! in the next commit.

use std::path::PathBuf;

use klipp_ml::{
    DevicePreference, MaskCapabilities, MaskError, MaskGenerator, MaskRequest, MaskResponse,
};

/// Stable id used in `assets/models.toml` and the registry.
pub const RF_DETR_SEG_MODEL_ID: &str = "rf_detr_seg_v1";

/// User-facing display name.
pub const RF_DETR_SEG_DISPLAY_NAME: &str = "RF-DETR-Seg";

/// Model input resolution. RF-DETR variants ship at multiple sizes
/// (432, 560, 640, 800); the v1 we target is the 432² base. If a
/// future build wants a larger variant the constant moves to the
/// builder + manifest.
pub const RF_DETR_INPUT_SIZE: u32 = 432;

/// Number of mask prototypes in the segmentation head's output.
/// Pinned by RF-DETR-Seg's architecture; future variants might
/// differ. If a fail-shape mismatch happens it'll be on this dim.
pub const RF_DETR_PROTOTYPE_COUNT: usize = 32;

/// Builder for RF-DETR-Seg.
pub struct RfDetrSegBuilder {
    weights_path: Option<PathBuf>,
    device: DevicePreference,
    /// Score threshold for emitting a detection. 0.5 default; the
    /// builder + (future) panel UI let users dial it down for
    /// recall-heavy use cases (small objects in cluttered scenes).
    score_threshold: f32,
}

impl RfDetrSegBuilder {
    pub fn new() -> Self {
        Self {
            weights_path: None,
            device: DevicePreference::GpuPreferCpu,
            score_threshold: 0.5,
        }
    }

    pub fn weights(mut self, path: PathBuf) -> Self {
        self.weights_path = Some(path);
        self
    }

    pub fn device(mut self, device: DevicePreference) -> Self {
        self.device = device;
        self
    }

    /// Lower the threshold to surface low-confidence detections;
    /// 0.5 is a balanced default. RF-DETR's detections are
    /// Hungarian-matched at training time so the score distribution
    /// is sharper than two-stage detectors — 0.5 is a sensible cut.
    pub fn score_threshold(mut self, t: f32) -> Self {
        self.score_threshold = t.clamp(0.0, 1.0);
        self
    }

    pub fn build(self) -> Result<RfDetrSeg, MaskError> {
        Ok(RfDetrSeg {
            weights_path: self.weights_path,
            device: self.device,
            score_threshold: self.score_threshold,
            #[cfg(feature = "ort")]
            session: None,
        })
    }
}

impl Default for RfDetrSegBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct RfDetrSeg {
    weights_path: Option<PathBuf>,
    device: DevicePreference,
    #[allow(dead_code)] // used by the ort-feature inference path (next commit)
    score_threshold: f32,
    #[cfg(feature = "ort")]
    #[allow(dead_code)] // populated by the inference path (next commit)
    session: Option<ort::session::Session>,
}

impl MaskGenerator for RfDetrSeg {
    fn id(&self) -> &str {
        RF_DETR_SEG_MODEL_ID
    }

    fn display_name(&self) -> &str {
        RF_DETR_SEG_DISPLAY_NAME
    }

    fn capabilities(&self) -> MaskCapabilities {
        // RF-DETR is auto-detect — it consumes no user prompts.
        MaskCapabilities::AUTO_DETECT
    }

    fn preferred_device(&self) -> DevicePreference {
        self.device
    }

    fn generate(&mut self, req: MaskRequest<'_>) -> Result<MaskResponse, MaskError> {
        if self.weights_path.is_none() {
            return Err(MaskError::BackendInit(
                "RF-DETR-Seg weights not provided — run model download first".into(),
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
                "RF-DETR-Seg ort backend not enabled — rebuild with --features ai-ort".into(),
            ))
        }

        #[cfg(feature = "ort")]
        {
            let _ = req;
            // Real ONNX session integration lands in the next
            // commit. Pre-process + ort run + multi-output decode +
            // per-detection mask assembly is a substantial chunk of
            // code; foundation ships first so the registry plumbing
            // is reviewable in isolation (matches the BiRefNet +
            // MobileSAM rhythm).
            Err(MaskError::BackendInit(
                "RF-DETR-Seg session integration pending — body lands in F-97-RFDETR phase 2".into(),
            ))
        }
    }
}

/// COCO-80 class label list. Static so the registry doesn't allocate
/// on every call. Order matters — index in this array == `class_id`
/// in RF-DETR's output. Bundled here rather than via a runtime file
/// load so tests can pin label semantics without IO mocking.
pub const COCO_LABELS: &[&str] = &[
    "person", "bicycle", "car", "motorcycle", "airplane", "bus",
    "train", "truck", "boat", "traffic light", "fire hydrant",
    "stop sign", "parking meter", "bench", "bird", "cat", "dog",
    "horse", "sheep", "cow", "elephant", "bear", "zebra", "giraffe",
    "backpack", "umbrella", "handbag", "tie", "suitcase", "frisbee",
    "skis", "snowboard", "sports ball", "kite", "baseball bat",
    "baseball glove", "skateboard", "surfboard", "tennis racket",
    "bottle", "wine glass", "cup", "fork", "knife", "spoon", "bowl",
    "banana", "apple", "sandwich", "orange", "broccoli", "carrot",
    "hot dog", "pizza", "donut", "cake", "chair", "couch",
    "potted plant", "bed", "dining table", "toilet", "tv", "laptop",
    "mouse", "remote", "keyboard", "cell phone", "microwave", "oven",
    "toaster", "sink", "refrigerator", "book", "clock", "vase",
    "scissors", "teddy bear", "hair drier", "toothbrush",
];

/// Look up a class label by RF-DETR's `class_id`. Returns
/// `"unknown"` for ids outside the COCO-80 range — defensive in
/// case a future variant trains on a different class set and we
/// haven't updated [`COCO_LABELS`] to match yet.
pub fn coco_label(class_id: usize) -> &'static str {
    COCO_LABELS.get(class_id).copied().unwrap_or("unknown")
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
        let g = RfDetrSegBuilder::new().build().expect("build");
        assert_eq!(g.id(), RF_DETR_SEG_MODEL_ID);
        assert_eq!(g.display_name(), RF_DETR_SEG_DISPLAY_NAME);
    }

    #[test]
    fn capabilities_match_detect_objects_task() {
        let g = RfDetrSegBuilder::new().build().expect("build");
        let caps = g.capabilities();
        assert!(caps.contains(MaskCapabilities::AUTO_DETECT));
        // RF-DETR doesn't take user prompts.
        assert!(!caps.contains(MaskCapabilities::POINT_PROMPT));
        assert!(!caps.contains(MaskCapabilities::BOX_PROMPT));
        assert!(!caps.contains(MaskCapabilities::SCRIBBLE));
        assert!(!caps.contains(MaskCapabilities::TEMPORAL));
    }

    #[test]
    fn builder_score_threshold_clamps_to_unit_range() {
        // Out-of-range thresholds get clamped — UI sliders that
        // overshoot don't accidentally nuke detection output by
        // setting a > 1 threshold.
        let g = RfDetrSegBuilder::new()
            .score_threshold(2.5)
            .build()
            .expect("build");
        // Field is private, so we round-trip via behaviour: the
        // build succeeded means the value was accepted (not panicked).
        // The actual clamp is exercised below.
        let _ = g;
        assert_eq!(
            RfDetrSegBuilder::new().score_threshold(2.5).score_threshold,
            1.0,
        );
        assert_eq!(
            RfDetrSegBuilder::new()
                .score_threshold(-0.1)
                .score_threshold,
            0.0,
        );
        assert_eq!(
            RfDetrSegBuilder::new().score_threshold(0.3).score_threshold,
            0.3,
        );
    }

    #[test]
    fn generate_without_weights_returns_actionable_error() {
        let pixels = vec![0.0_f32; 4 * 4 * 4];
        let mut g = RfDetrSegBuilder::new().build().expect("build");
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
    #[cfg(not(feature = "ort"))]
    fn generate_without_ort_feature_says_rebuild_with_flag() {
        let pixels = vec![0.0_f32; 4 * 4 * 4];
        let mut g = RfDetrSegBuilder::new()
            .weights(PathBuf::from("/tmp/rf_detr_seg.onnx"))
            .build()
            .expect("build");
        let err = g
            .generate(dummy_request(&pixels))
            .expect_err("ort feature off");
        match err {
            MaskError::BackendInit(msg) => {
                assert!(
                    msg.to_lowercase().contains("ai-ort")
                        || msg.to_lowercase().contains("feature"),
                    "error should mention build flag: {msg}"
                );
            }
            other => panic!("expected BackendInit, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "ort")]
    fn generate_with_ort_feature_says_session_pending() {
        let pixels = vec![0.0_f32; 4 * 4 * 4];
        let mut g = RfDetrSegBuilder::new()
            .weights(PathBuf::from("/tmp/rf_detr_seg.onnx"))
            .build()
            .expect("build");
        let err = g
            .generate(dummy_request(&pixels))
            .expect_err("session not yet wired");
        match err {
            MaskError::BackendInit(msg) => {
                assert!(
                    msg.to_lowercase().contains("session")
                        || msg.to_lowercase().contains("pending")
                        || msg.to_lowercase().contains("phase 2"),
                    "error should mention session integration: {msg}"
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
        let mut g = RfDetrSegBuilder::new()
            .weights(PathBuf::from("/tmp/dummy"))
            .build()
            .expect("build");
        match g.generate(req).unwrap_err() {
            MaskError::ShapeMismatch { actual_w: 0, actual_h: 0, .. } => {}
            other => panic!("expected ShapeMismatch with 0x0, got {other:?}"),
        }
    }

    #[test]
    fn coco_labels_has_eighty_entries() {
        // Pinned — RF-DETR's class_id output indexes directly into
        // this array. Future variants trained on different class
        // sets need to either switch model-id or update this list.
        assert_eq!(COCO_LABELS.len(), 80);
    }

    #[test]
    fn coco_label_lookup_returns_known_classes() {
        assert_eq!(coco_label(0), "person");
        assert_eq!(coco_label(15), "cat");
        assert_eq!(coco_label(16), "dog");
        assert_eq!(coco_label(79), "toothbrush");
    }

    #[test]
    fn coco_label_lookup_handles_out_of_range_id() {
        // Defensive — a future RF-DETR variant trained on a custom
        // class set might emit ids beyond 79; we don't want the
        // panel to panic when rendering the label.
        assert_eq!(coco_label(80), "unknown");
        assert_eq!(coco_label(usize::MAX), "unknown");
    }

    #[test]
    fn id_and_display_name_are_distinct() {
        let g = RfDetrSegBuilder::new().build().expect("build");
        assert_ne!(g.id(), g.display_name());
    }
}
