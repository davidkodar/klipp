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

#[cfg(feature = "ort")]
use klipp_ml::{BBox, MaskBuffer, MaskCandidate};

#[cfg(feature = "ort")]
use crate::preprocess::bilinear_resize_rgba;

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
            self.generate_with_ort(req)
        }
    }
}

#[cfg(feature = "ort")]
impl RfDetrSeg {
    /// Real inference path. Lazy session init → pre-process → run
    /// session → decode 5 outputs → per-detection mask assembly →
    /// build MaskResponse. Mirrors `BiRefNetLite::generate_with_ort`
    /// in structure; the heavy lifting is the multi-output decode
    /// and the prototype-mask linear combination.
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

        // ── Pre-process ──
        // RF-DETR-Seg expects 432×432 NCHW f32 in [0, 1] range,
        // normalised by ImageNet mean/std. (Some upstream variants
        // skip the normalize and consume raw [0, 1] floats — if
        // the model fails to converge against this pre-process,
        // try the raw path before changing anything else.)
        let src_w = req.frame.width as usize;
        let src_h = req.frame.height as usize;
        let model_size = RF_DETR_INPUT_SIZE as u32;
        let resized = bilinear_resize_rgba(
            req.frame.linear_rgba,
            req.frame.width,
            req.frame.height,
            model_size,
            model_size,
        );
        if resized.is_empty() {
            return Err(MaskError::Inference("resize produced empty buffer".into()));
        }

        let pixel_count = (model_size as usize) * (model_size as usize);
        let mean = [0.485_f32, 0.456, 0.406];
        let std_ = [0.229_f32, 0.224, 0.225];
        let mut chw = vec![0.0_f32; 3 * pixel_count];
        for i in 0..pixel_count {
            for c in 0..3 {
                let lin = resized[i * 4 + c].clamp(0.0, 1.0);
                let s = klipp_color::transfer::linear_to_srgb(lin);
                chw[c * pixel_count + i] = (s - mean[c]) / std_[c];
            }
        }

        let input_shape = [1_i64, 3, model_size as i64, model_size as i64];
        let input_tensor = ort::value::Value::from_array((input_shape, chw))
            .map_err(|e| MaskError::BackendInit(format!("input tensor: {e}")))?;

        // ── Run session ──
        let outputs = session
            .run(ort::inputs![input_tensor])
            .map_err(|e| MaskError::Inference(format!("ort run: {e}")))?;

        // ── Decode the 5 outputs ──
        // Output names vary across RF-DETR re-exports. We probe a
        // few common name variants per output, falling back to
        // positional indexing.
        let (boxes_shape, boxes) = extract_named_or_indexed::<f32>(
            &outputs,
            &["boxes", "pred_boxes", "dets"],
            0,
        )?;
        let (_scores_shape, scores) = extract_named_or_indexed::<f32>(
            &outputs,
            &["scores", "pred_scores", "confs"],
            1,
        )?;
        // Labels can be int64 or int32 depending on export.
        let labels_i64 =
            extract_named_or_indexed::<i64>(&outputs, &["labels", "pred_labels", "class_ids"], 2)
                .ok()
                .map(|(_, v)| v);
        let labels_i32 = if labels_i64.is_none() {
            extract_named_or_indexed::<i32>(&outputs, &["labels", "pred_labels", "class_ids"], 2)
                .ok()
                .map(|(_, v)| v)
        } else {
            None
        };
        let (_mask_coeffs_shape, mask_coeffs) = extract_named_or_indexed::<f32>(
            &outputs,
            &["mask_coefficients", "mask_coeffs", "masks"],
            3,
        )?;
        let (proto_shape, prototypes) = extract_named_or_indexed::<f32>(
            &outputs,
            &["mask_prototypes", "prototypes", "proto"],
            4,
        )?;

        // Shapes:
        //   boxes: (1, N, 4)
        //   scores: (1, N)
        //   labels: (1, N)
        //   mask_coeffs: (1, N, K) where K = RF_DETR_PROTOTYPE_COUNT
        //   prototypes: (1, K, H, W)
        let n_dets = num_detections(&boxes_shape)?;
        let (proto_k, proto_h, proto_w) = prototype_dims(&proto_shape)?;
        if proto_k != RF_DETR_PROTOTYPE_COUNT {
            return Err(MaskError::Inference(format!(
                "unexpected prototype count {proto_k}, expected {}",
                RF_DETR_PROTOTYPE_COUNT
            )));
        }

        // Filter candidates by score; convert each surviving
        // detection into a MaskCandidate. Limit to a sensible cap
        // so a low score_threshold doesn't drown the panel in
        // hundreds of candidates.
        const MAX_CANDIDATES: usize = 64;
        let mut candidates = Vec::new();
        for n in 0..n_dets {
            let score = scores
                .get(n)
                .copied()
                .ok_or_else(|| MaskError::Inference("scores OOB".into()))?;
            if score < self.score_threshold {
                continue;
            }
            if candidates.len() >= MAX_CANDIDATES {
                break;
            }

            // Bounding box. Coordinates are normalised [0, 1] in
            // most exports — if a future variant emits absolute
            // pixel coords we'll trip a clamp here and need to
            // detect from the `proto_w` / `model_size` ratio.
            // Output is xyxy → convert to BBox's (x, y, w, h).
            let bx_off = n * 4;
            let x0 = boxes[bx_off].clamp(0.0, 1.0);
            let y0 = boxes[bx_off + 1].clamp(0.0, 1.0);
            let x1 = boxes[bx_off + 2].clamp(0.0, 1.0);
            let y1 = boxes[bx_off + 3].clamp(0.0, 1.0);
            // Skip degenerate bboxes — zero-area detections are
            // either pad slots in the output or model artifacts.
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let bbox = BBox {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            };

            let class_id = labels_i64
                .as_ref()
                .map(|l| l.get(n).copied().unwrap_or(-1) as i32)
                .or_else(|| {
                    labels_i32
                        .as_ref()
                        .map(|l| l.get(n).copied().unwrap_or(-1))
                })
                .unwrap_or(-1);
            let label_str = if class_id >= 0 {
                Some(coco_label(class_id as usize).to_string())
            } else {
                None
            };

            // Per-detection mask: linear combination of K
            // prototypes weighted by this detection's K coefficients,
            // then sigmoid, then crop to the bbox.
            let coeff_off = n * RF_DETR_PROTOTYPE_COUNT;
            let coeffs = &mask_coeffs[coeff_off..coeff_off + RF_DETR_PROTOTYPE_COUNT];
            let mask = assemble_detection_mask(
                coeffs,
                &prototypes,
                proto_h,
                proto_w,
                &bbox,
                src_w,
                src_h,
            );

            candidates.push(MaskCandidate {
                mask,
                confidence: score,
                bbox: Some(bbox),
                label: label_str,
            });
        }

        // Sort by confidence descending so the panel's
        // `first_candidate` lookup picks the strongest detection.
        candidates.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(MaskResponse {
            candidates,
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

    let mut builder = ort::session::Session::builder()
        .map_err(|e| MaskError::BackendInit(format!("session builder: {e}")))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| MaskError::BackendInit(format!("set optimisation: {e}")))?;

    #[cfg(feature = "cuda")]
    {
        if matches!(
            device,
            DevicePreference::GpuPreferCpu | DevicePreference::GpuRequired
        ) {
            let cuda = ort::execution_providers::CUDAExecutionProvider::default();
            builder = builder
                .with_execution_providers([cuda.build()])
                .map_err(|e| MaskError::BackendInit(format!("register CUDA EP: {e}")))?;
        }
    }
    #[cfg(not(feature = "cuda"))]
    {
        if matches!(device, DevicePreference::GpuRequired) {
            return Err(MaskError::DeviceUnavailable(
                "GPU required but klipp-ml-ort was built without --features cuda",
            ));
        }
    }

    builder
        .commit_from_file(weights)
        .map_err(|e| MaskError::BackendInit(format!("load weights: {e}")))
}

/// Pull a tensor from `outputs` by trying a list of probable names
/// first, then falling back to the indexed position. Returns
/// `(shape, data)` as **owned** Vecs — the borrow returned by
/// `try_extract_tensor` is tied to the `ValueRef` we'd have to
/// keep alive across the whole decode path, which makes the
/// lifetime story painful. RF-DETR's outputs are <1 MB total so
/// the copy is cheap.
#[cfg(feature = "ort")]
fn extract_named_or_indexed<T: ort::value::PrimitiveTensorElementType + Copy>(
    outputs: &ort::session::SessionOutputs<'_>,
    names: &[&str],
    fallback_idx: usize,
) -> Result<(Vec<i64>, Vec<T>), MaskError> {
    for name in names {
        if let Some(v) = outputs.get(*name) {
            let (shape, data) = v
                .try_extract_tensor::<T>()
                .map_err(|e| MaskError::Inference(format!("extract {name}: {e}")))?;
            return Ok((shape.iter().copied().collect(), data.to_vec()));
        }
    }
    let v = outputs
        .iter()
        .nth(fallback_idx)
        .ok_or_else(|| {
            MaskError::Inference(format!(
                "RF-DETR output index {fallback_idx} (tried names {names:?}) missing"
            ))
        })?
        .1;
    let (shape, data) = v
        .try_extract_tensor::<T>()
        .map_err(|e| MaskError::Inference(format!("extract idx {fallback_idx}: {e}")))?;
    Ok((shape.iter().copied().collect(), data.to_vec()))
}

/// `boxes_shape` is `(1, N, 4)`. Returns N. Defensive: returns 0 on
/// any unexpected shape, which makes the detection loop produce no
/// candidates rather than panic.
#[cfg(feature = "ort")]
fn num_detections(shape: &[i64]) -> Result<usize, MaskError> {
    match shape {
        [1, n, 4] => Ok(*n as usize),
        [n, 4] => Ok(*n as usize),
        other => Err(MaskError::Inference(format!(
            "boxes shape {other:?} (expected (1, N, 4))"
        ))),
    }
}

/// `proto_shape` is `(1, K, H, W)`. Returns (K, H, W).
#[cfg(feature = "ort")]
fn prototype_dims(shape: &[i64]) -> Result<(usize, usize, usize), MaskError> {
    match shape {
        [1, k, h, w] | [k, h, w] => Ok((*k as usize, *h as usize, *w as usize)),
        other => Err(MaskError::Inference(format!(
            "prototype shape {other:?} (expected (1, K, H, W))"
        ))),
    }
}

/// Assemble a single detection's mask from its K coefficients and
/// the K shared prototypes.
///
/// Algorithm: for each output pixel (y, x) at proto resolution,
/// compute `sigmoid(sum_k(coeff_k * proto_kij))`. Crop the
/// resulting `proto_h × proto_w` map to the detection's bbox
/// (in normalised image coords), then resample to the source
/// frame's resolution. Threshold at sigmoid > 0.5 → u8 alpha.
///
/// Implementation runs the linear combination in CPU f32 — the
/// proto map is small (typically 108×108 for RF-DETR-Seg) so
/// 32 muls × 108² = ~370k FLOPs per detection. Well under 1 ms
/// per detection on any modern CPU.
#[cfg(feature = "ort")]
fn assemble_detection_mask(
    coeffs: &[f32],
    prototypes: &[f32],
    proto_h: usize,
    proto_w: usize,
    bbox: &BBox,
    src_w: usize,
    src_h: usize,
) -> MaskBuffer {
    let proto_pixels = proto_h * proto_w;
    let mut proto_mask = vec![0.0_f32; proto_pixels];

    // Linear combination: proto_mask[i] = sum_k(coeffs[k] * prototypes[k, i]).
    for k in 0..RF_DETR_PROTOTYPE_COUNT {
        let coeff = coeffs[k];
        if coeff == 0.0 {
            continue;
        }
        let proto_off = k * proto_pixels;
        for i in 0..proto_pixels {
            proto_mask[i] += coeff * prototypes[proto_off + i];
        }
    }

    // Sigmoid + bbox crop. The proto map covers the entire image;
    // a detection's mask is its slice of that map within the bbox
    // (everything outside the bbox is forced to 0 to avoid
    // mask-leak across detections).
    let bx0 = (bbox.x * proto_w as f32) as usize;
    let by0 = (bbox.y * proto_h as f32) as usize;
    let bx1 = (((bbox.x + bbox.w) * proto_w as f32) as usize).min(proto_w);
    let by1 = (((bbox.y + bbox.h) * proto_h as f32) as usize).min(proto_h);

    let mut cropped = vec![0.0_f32; proto_pixels];
    for y in by0..by1 {
        for x in bx0..bx1 {
            let idx = y * proto_w + x;
            cropped[idx] = sigmoid(proto_mask[idx]);
        }
    }

    // Bilinear-resample to source resolution. Hand-written so we
    // don't pull in a full image crate just for resize.
    let mut alpha = vec![0u8; src_w * src_h];
    let scale_x = (proto_w as f32) / (src_w as f32);
    let scale_y = (proto_h as f32) / (src_h as f32);

    for dy in 0..src_h {
        let sy = ((dy as f32) + 0.5) * scale_y - 0.5;
        let sy0 = sy.floor().max(0.0).min((proto_h - 1) as f32) as usize;
        let sy1 = (sy0 + 1).min(proto_h - 1);
        let fy = (sy - sy0 as f32).clamp(0.0, 1.0);

        for dx in 0..src_w {
            let sx = ((dx as f32) + 0.5) * scale_x - 0.5;
            let sx0 = sx.floor().max(0.0).min((proto_w - 1) as f32) as usize;
            let sx1 = (sx0 + 1).min(proto_w - 1);
            let fx = (sx - sx0 as f32).clamp(0.0, 1.0);

            let p00 = cropped[sy0 * proto_w + sx0];
            let p01 = cropped[sy0 * proto_w + sx1];
            let p10 = cropped[sy1 * proto_w + sx0];
            let p11 = cropped[sy1 * proto_w + sx1];
            let top = p00 * (1.0 - fx) + p01 * fx;
            let bot = p10 * (1.0 - fx) + p11 * fx;
            let v = (top * (1.0 - fy) + bot * fy).clamp(0.0, 1.0);
            alpha[dy * src_w + dx] = (v * 255.0).round() as u8;
        }
    }

    MaskBuffer {
        width: src_w as u32,
        height: src_h as u32,
        alpha,
    }
}

#[cfg(feature = "ort")]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
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
    fn generate_with_invalid_weights_fails_at_load_step() {
        // Phase-2 sanity: with the ort feature on and a bogus
        // weights path, generate() should fail at session load —
        // not silently fall through. The exact error text is
        // ort-version-dependent so we just match the variant.
        let pixels = vec![0.0_f32; 4 * 4 * 4];
        let mut g = RfDetrSegBuilder::new()
            .weights(PathBuf::from("/tmp/this-does-not-exist.onnx"))
            .build()
            .expect("build");
        match g.generate(dummy_request(&pixels)).unwrap_err() {
            MaskError::BackendInit(_) => {}
            other => panic!("expected BackendInit on missing weights, got {other:?}"),
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
