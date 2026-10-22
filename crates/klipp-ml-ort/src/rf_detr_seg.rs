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
//! 1. `boxes` — `(1, N, 4)` xyxy in normalised `[0,1]` image coords
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
//! ## Running it
//!
//! With the crate's `ort` feature on, `generate` runs a real ONNX
//! Runtime session, built on first use and held for later calls.
//! Without it the pre- and post-process still compile and `generate`
//! returns `MaskError::BackendInit`, distinguishing "no weights" from
//! "no backend" so the failure says which it is.

use std::path::PathBuf;

use klipp_ml::{
    DevicePreference, MaskCapabilities, MaskError, MaskGenerator, MaskRequest, MaskResponse,
};

#[cfg(feature = "ort")]
use klipp_ml::{BBox, MaskBuffer, MaskCandidate};

#[cfg(feature = "ort")]
use crate::preprocess::bilinear_resize_rgba;

/// Stable id used in `assets/models.toml` and the registry — preview tier
/// (RFDETRSegPreview, ~134 MB ONNX, fast but noisier classifier).
pub const RF_DETR_SEG_MODEL_ID: &str = "rf_detr_seg_v1";

/// User-facing display name for the preview tier.
pub const RF_DETR_SEG_DISPLAY_NAME: &str = "RF-DETR-Seg (Preview)";

/// Stable id for the medium tier (RFDETRSegMedium, ~140 MB ONNX,
/// genuinely accurate). Same architecture + 3-output layout as the
/// preview — only the weights differ — so a single `RfDetrSeg`
/// implementation backs both, parameterised by `weights_path` +
/// reported id/display_name.
pub const RF_DETR_SEG_MEDIUM_MODEL_ID: &str = "rf_detr_seg_medium_v1";

/// User-facing display name for the medium tier.
pub const RF_DETR_SEG_MEDIUM_DISPLAY_NAME: &str = "RF-DETR-Seg (Medium)";

/// Model input resolution. RF-DETR variants ship at multiple sizes
/// (432, 560, 640, 800); the v1 we target is the 432² base. If a
/// future build wants a larger variant the constant moves to the
/// builder + manifest.
pub const RF_DETR_INPUT_SIZE: u32 = 432;

/// Number of mask prototypes in the segmentation head's output.
/// Pinned by RF-DETR-Seg's architecture; future variants might
/// differ. If a fail-shape mismatch happens it'll be on this dim.
pub const RF_DETR_PROTOTYPE_COUNT: usize = 32;

/// Default overlap threshold for non-maximum-suppression. Compared
/// against `max(IoU, IoMin)` where `IoMin = intersection /
/// min(area_a, area_b)`. The IoMin term catches the "small box
/// fully inside a larger same-class box" case that pure IoU misses
/// — common with noisy detectors that emit nested duplicates.
/// 0.5 is the COCO-standard IoU cut; the IoMin term makes this
/// effectively stricter for nested cases.
pub const RF_DETR_DEFAULT_NMS_IOU_THRESHOLD: f32 = 0.5;

/// Builder for RF-DETR-Seg.
pub struct RfDetrSegBuilder {
    weights_path: Option<PathBuf>,
    device: DevicePreference,
    /// Score threshold for emitting a detection. 0.5 default; the
    /// builder + (future) panel UI let users dial it down for
    /// recall-heavy use cases (small objects in cluttered scenes).
    score_threshold: f32,
    /// IoU threshold for class-aware non-maximum-suppression. Two
    /// same-class detections with IoU > this value get collapsed to
    /// the higher-confidence one. Set to 1.0+ to disable NMS
    /// entirely (useful for debugging the raw model output).
    nms_iou_threshold: f32,
    /// Stable model id reported by `MaskGenerator::id`. Defaults to
    /// the preview tier; set via `model_id` to register the medium
    /// (or future large/x-large) tier without forking the impl.
    model_id: &'static str,
    /// User-facing display name reported by `MaskGenerator::display_name`.
    display_name: &'static str,
}

impl RfDetrSegBuilder {
    pub fn new() -> Self {
        Self {
            weights_path: None,
            device: DevicePreference::GpuPreferCpu,
            score_threshold: 0.5,
            nms_iou_threshold: RF_DETR_DEFAULT_NMS_IOU_THRESHOLD,
            model_id: RF_DETR_SEG_MODEL_ID,
            display_name: RF_DETR_SEG_DISPLAY_NAME,
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

    /// Override the reported model id + display name. Used to
    /// register a different tier (e.g. medium) without duplicating
    /// the impl — the architecture and 3-output decode are identical
    /// across RF-DETR-Seg variants, only the weights and naming
    /// differ.
    pub fn model_id(mut self, id: &'static str, display_name: &'static str) -> Self {
        self.model_id = id;
        self.display_name = display_name;
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

    /// IoU threshold for class-aware non-maximum-suppression. Pass
    /// 1.0+ to disable NMS entirely (raw model output, useful for
    /// debugging duplicate-detection patterns).
    pub fn nms_iou_threshold(mut self, t: f32) -> Self {
        self.nms_iou_threshold = t.clamp(0.0, f32::INFINITY);
        self
    }

    pub fn build(self) -> Result<RfDetrSeg, MaskError> {
        Ok(RfDetrSeg {
            weights_path: self.weights_path,
            device: self.device,
            score_threshold: self.score_threshold,
            nms_iou_threshold: self.nms_iou_threshold,
            model_id: self.model_id,
            display_name: self.display_name,
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
    #[allow(dead_code)] // used by the ort-feature inference path
    score_threshold: f32,
    #[allow(dead_code)] // used by the ort-feature inference path
    nms_iou_threshold: f32,
    model_id: &'static str,
    display_name: &'static str,
    #[cfg(feature = "ort")]
    #[allow(dead_code)] // populated by the inference path
    session: Option<ort::session::Session>,
}

impl MaskGenerator for RfDetrSeg {
    fn id(&self) -> &str {
        self.model_id
    }

    fn display_name(&self) -> &str {
        self.display_name
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

        // Per-call override fallback to construction defaults. UI
        // sliders write into req.tunables; the builder values stay as
        // the floor when no override is set. Both are clamped on
        // intake so a malformed value can't propagate further.
        let score_threshold = req
            .tunables
            .score_threshold
            .map(|t| t.clamp(0.0, 1.0))
            .unwrap_or(self.score_threshold);
        let nms_iou_threshold = req
            .tunables
            .nms_iou_threshold
            .map(|t| t.clamp(0.0, f32::INFINITY))
            .unwrap_or(self.nms_iou_threshold);

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
        let model_size = RF_DETR_INPUT_SIZE;
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

        // ── Decode the 3 outputs ──
        // RF-DETR-Seg-Preview's actual ONNX export (verified via
        // probe_outputs example):
        //   dets: (1, 200, 4) f32 — bbox cxcywh, normalised [0, 1]
        //   labels: (1, 200, 91) f32 — class logits for COCO-91
        //     (DETR convention; class 90 is the "no object" slot)
        //   <auto-named>: (1, 200, 108, 108) f32 — per-detection
        //     mask logits, already assembled (no prototype combo)
        // Output #2 has an auto-generated numeric name from the
        // exporter; we fall back to positional indexing for it.
        let (dets_shape, dets) = extract_named_or_indexed::<f32>(
            &outputs,
            &["dets", "boxes", "pred_boxes"],
            0,
        )?;
        let (labels_shape, labels_logits) = extract_named_or_indexed::<f32>(
            &outputs,
            &["labels", "pred_logits", "scores"],
            1,
        )?;
        let (masks_shape, mask_logits) = extract_named_or_indexed::<f32>(
            &outputs,
            &["masks", "mask_logits", "pred_masks"],
            2,
        )?;

        let n_dets = match dets_shape.as_slice() {
            [1, n, 4] | [n, 4] => *n as usize,
            other => {
                return Err(MaskError::Inference(format!(
                    "dets shape {other:?} (expected (1, N, 4))"
                )));
            }
        };
        let n_classes = match labels_shape.as_slice() {
            [1, _n, c] | [_n, c] => *c as usize,
            other => {
                return Err(MaskError::Inference(format!(
                    "labels shape {other:?} (expected (1, N, C))"
                )));
            }
        };
        let (mask_h, mask_w) = match masks_shape.as_slice() {
            [1, _n, h, w] | [_n, h, w] => (*h as usize, *w as usize),
            other => {
                return Err(MaskError::Inference(format!(
                    "masks shape {other:?} (expected (1, N, H, W))"
                )));
            }
        };
        let mask_pixels = mask_h * mask_w;

        // With 91 logits the classes are COCO's own category ids
        // (1 to 90, with gaps) and slot 0 is unused; with 80 they are
        // contiguous and the last slot is DETR's "no object".
        let no_object_class = if n_classes == 91 { 0 } else { n_classes.saturating_sub(1) };

        // Filter candidates by score; convert each surviving
        // detection into a MaskCandidate. Cap so a low threshold
        // doesn't drown the panel.
        const MAX_CANDIDATES: usize = 64;
        let mut candidates = Vec::new();
        for n in 0..n_dets {
            // argmax + softmax over this detection's class logits
            let class_off = n * n_classes;
            let class_slice = &labels_logits[class_off..class_off + n_classes];
            let (class_id, _) = argmax(class_slice);
            if class_id == no_object_class {
                continue;
            }
            let score = softmax_at(class_slice, class_id);
            if score < score_threshold {
                continue;
            }
            if candidates.len() >= MAX_CANDIDATES {
                break;
            }

            // RF-DETR's bboxes are cxcywh in normalised coords.
            // Convert to BBox's (x, y, w, h) which is (top-left,
            // size). Defensive clamp so out-of-frame predictions
            // don't trip downstream resize asserts.
            let bx_off = n * 4;
            let cx = dets[bx_off].clamp(0.0, 1.0);
            let cy = dets[bx_off + 1].clamp(0.0, 1.0);
            let bw = dets[bx_off + 2].clamp(0.0, 1.0);
            let bh = dets[bx_off + 3].clamp(0.0, 1.0);
            if bw <= 0.0 || bh <= 0.0 {
                continue;
            }
            let x0 = (cx - bw * 0.5).clamp(0.0, 1.0);
            let y0 = (cy - bh * 0.5).clamp(0.0, 1.0);
            let bbox = BBox {
                x: x0,
                y: y0,
                w: (bw).min(1.0 - x0),
                h: (bh).min(1.0 - y0),
            };

            let label_str = Some(coco_label_for(class_id, n_classes).to_string());

            // Per-detection mask is already assembled; just sigmoid
            // + bilinear-resample the (mask_h × mask_w) logit map
            // to the source resolution. Bbox is just metadata for
            // the panel — the mask itself spans the whole image
            // (RF-DETR-Seg's masks naturally fade to zero outside
            // the detection's spatial extent).
            let mask_off = n * mask_pixels;
            let det_logits = &mask_logits[mask_off..mask_off + mask_pixels];
            let mask = resample_detection_mask(
                det_logits, mask_h, mask_w, src_w, src_h,
            );

            candidates.push(MaskCandidate {
                mask,
                confidence: score,
                bbox: Some(bbox),
                label: label_str,
            });
        }

        // Sort by confidence descending so NMS keeps the highest-
        // confidence one out of every overlapping cluster, and the
        // panel's `first_candidate` lookup picks the strongest
        // detection.
        candidates.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Class-aware NMS to collapse same-class overlapping
        // detections. RF-DETR-Seg-Preview tends to emit several
        // near-duplicate boxes per object cluster (e.g. 9 "bicycle"
        // boxes around one bicycle); NMS collapses those to one.
        // Different classes are kept even when they overlap — a
        // genuine "person on bicycle" should produce both
        // detections.
        if nms_iou_threshold < 1.0 {
            non_max_suppress_class_aware(&mut candidates, nms_iou_threshold);
        }

        Ok(MaskResponse {
            candidates,
            inference_ms: started.elapsed().as_millis() as u64,
        })
    }
}

/// Class-aware NMS — assumes `candidates` is sorted by confidence
/// descending. For each candidate, drop any later same-class
/// candidate whose bbox IoU with the current one exceeds
/// `iou_threshold`. O(N²) but N is bounded by `MAX_CANDIDATES = 64`
/// so the constant is tiny.
#[cfg(feature = "ort")]
fn non_max_suppress_class_aware(
    candidates: &mut Vec<MaskCandidate>,
    iou_threshold: f32,
) {
    let n = candidates.len();
    if n < 2 {
        return;
    }
    let mut keep = vec![true; n];
    for i in 0..n {
        if !keep[i] {
            continue;
        }
        let bi = match candidates[i].bbox {
            Some(b) => b,
            None => continue,
        };
        let class_i = candidates[i].label.as_deref().unwrap_or("");
        for j in (i + 1)..n {
            if !keep[j] {
                continue;
            }
            // Class-aware: only suppress same-class overlaps.
            // Overlapping different-class detections (a person
            // standing on a bicycle) are both kept.
            if candidates[j].label.as_deref().unwrap_or("") != class_i {
                continue;
            }
            let bj = match candidates[j].bbox {
                Some(b) => b,
                None => continue,
            };
            // Use max(IoU, IoMin) so the same threshold catches
            // both overlapping-similar-size cases (high IoU) AND
            // nested cases (small box inside larger same-class
            // box, high IoMin but low IoU).
            let overlap = bbox_iou(&bi, &bj).max(bbox_iomin(&bi, &bj));
            if overlap > iou_threshold {
                keep[j] = false;
            }
        }
    }
    let mut idx = 0;
    candidates.retain(|_| {
        let k = keep[idx];
        idx += 1;
        k
    });
}

/// Intersection-over-minimum: `intersection / min(area_a, area_b)`.
/// Catches the "small box fully inside larger same-class box" case
/// that pure IoU misses (since IoU's union dominates with the
/// larger box's area, masking high containment).
#[cfg(feature = "ort")]
fn bbox_iomin(a: &BBox, b: &BBox) -> f32 {
    let ax2 = a.x + a.w;
    let ay2 = a.y + a.h;
    let bx2 = b.x + b.w;
    let by2 = b.y + b.h;
    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let inter = iw * ih;
    let area_a = a.w * a.h;
    let area_b = b.w * b.h;
    let min_area = area_a.min(area_b);
    if min_area <= 0.0 {
        0.0
    } else {
        (inter / min_area).clamp(0.0, 1.0)
    }
}

/// IoU of two BBoxes in (x, y, w, h) form. Returns 0 for
/// non-overlapping or zero-area boxes — the caller's `>`
/// comparison treats those as "don't suppress" which is correct.
#[cfg(feature = "ort")]
fn bbox_iou(a: &BBox, b: &BBox) -> f32 {
    let ax2 = a.x + a.w;
    let ay2 = a.y + a.h;
    let bx2 = b.x + b.w;
    let by2 = b.y + b.h;
    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let inter = iw * ih;
    let area_a = a.w * a.h;
    let area_b = b.w * b.h;
    let union = area_a + area_b - inter;
    if union <= 0.0 {
        0.0
    } else {
        (inter / union).clamp(0.0, 1.0)
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
        .map_err(|e| MaskError::BackendInit(format!("set optimisation: {e}")))?
        // One thread per core is slower than a few on a many-core machine
        // and leaves nothing for the analyser running alongside.
        .with_intra_threads(crate::ppocr::SESSION_THREADS)
        .map_err(|e| MaskError::BackendInit(format!("set threads: {e}")))?;

    // On the GPU where it loads, what it got recorded (#2250).
    let model = weights.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    builder = crate::provider::on_device(builder, device, &model).map_err(MaskError::BackendInit)?;

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

/// Bilinear-resample a per-detection logit mask from its native
/// `(mask_h × mask_w)` resolution to the source frame's resolution,
/// applying sigmoid per pixel + threshold to u8 alpha.
///
/// RF-DETR-Seg's masks are already image-aligned — they cover the
/// whole frame and naturally fade to zero outside the detection's
/// spatial extent. We don't bbox-crop here because the model
/// already does that implicitly via training.
#[cfg(feature = "ort")]
fn resample_detection_mask(
    logits: &[f32],
    mask_h: usize,
    mask_w: usize,
    src_w: usize,
    src_h: usize,
) -> MaskBuffer {
    let mut alpha = vec![0u8; src_w * src_h];
    let scale_x = (mask_w as f32) / (src_w as f32);
    let scale_y = (mask_h as f32) / (src_h as f32);

    for dy in 0..src_h {
        let sy = ((dy as f32) + 0.5) * scale_y - 0.5;
        let sy0 = sy.floor().max(0.0).min((mask_h - 1) as f32) as usize;
        let sy1 = (sy0 + 1).min(mask_h - 1);
        let fy = (sy - sy0 as f32).clamp(0.0, 1.0);

        for dx in 0..src_w {
            let sx = ((dx as f32) + 0.5) * scale_x - 0.5;
            let sx0 = sx.floor().max(0.0).min((mask_w - 1) as f32) as usize;
            let sx1 = (sx0 + 1).min(mask_w - 1);
            let fx = (sx - sx0 as f32).clamp(0.0, 1.0);

            let p00 = logits[sy0 * mask_w + sx0];
            let p01 = logits[sy0 * mask_w + sx1];
            let p10 = logits[sy1 * mask_w + sx0];
            let p11 = logits[sy1 * mask_w + sx1];
            let top = p00 * (1.0 - fx) + p01 * fx;
            let bot = p10 * (1.0 - fx) + p11 * fx;
            let logit = top * (1.0 - fy) + bot * fy;
            let prob = sigmoid(logit);
            alpha[dy * src_w + dx] = (prob.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }

    MaskBuffer {
        width: src_w as u32,
        height: src_h as u32,
        alpha,
    }
}

/// Pick the index of the largest element in `slice`. Used to find
/// the predicted class for a detection.
#[cfg(feature = "ort")]
fn argmax(slice: &[f32]) -> (usize, f32) {
    slice
        .iter()
        .enumerate()
        .fold((0usize, f32::NEG_INFINITY), |(best_i, best_v), (i, &v)| {
            if v > best_v {
                (i, v)
            } else {
                (best_i, best_v)
            }
        })
}

/// Numerically stable softmax of a single class probability.
/// Subtract the max-logit before exponentiating so large logits
/// don't overflow.
#[cfg(feature = "ort")]
fn softmax_at(slice: &[f32], idx: usize) -> f32 {
    let max_logit = slice
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let denom: f32 = slice.iter().map(|&v| (v - max_logit).exp()).sum();
    if denom > 0.0 {
        (slice[idx] - max_logit).exp() / denom
    } else {
        0.0
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

/// COCO's own category ids, 1 to 90 with the eleven gaps the dataset
/// left when categories were dropped; slot 0 unused. What a model
/// exported with 91 logits means by a class index — the RF-DETR
/// exports in the registry do — and read against the contiguous list
/// above, every label past "fire hydrant" came out wrong: a cat was a
/// horse, a potted plant a mouse.
pub const COCO_91_LABELS: &[&str] = &[
    "", "person", "bicycle", "car", "motorcycle", "airplane", "bus", "train", "truck", "boat",
    "traffic light", "fire hydrant", "", "stop sign", "parking meter", "bench", "bird", "cat", "dog", "horse",
    "sheep", "cow", "elephant", "bear", "zebra", "giraffe", "", "backpack", "umbrella", "",
    "", "handbag", "tie", "suitcase", "frisbee", "skis", "snowboard", "sports ball", "kite", "baseball bat",
    "baseball glove", "skateboard", "surfboard", "tennis racket", "bottle", "", "wine glass", "cup", "fork", "knife",
    "spoon", "bowl", "banana", "apple", "sandwich", "orange", "broccoli", "carrot", "hot dog", "pizza",
    "donut", "cake", "chair", "couch", "potted plant", "bed", "", "dining table", "", "",
    "toilet", "", "tv", "laptop", "mouse", "remote", "keyboard", "cell phone", "microwave", "oven",
    "toaster", "sink", "refrigerator", "", "book", "clock", "vase", "scissors", "teddy bear", "hair drier",
    "toothbrush",
];

/// Look up a class label by RF-DETR's `class_id` in the contiguous
/// COCO-80 list. Returns `"unknown"` for ids outside it.
pub fn coco_label(class_id: usize) -> &'static str {
    COCO_LABELS.get(class_id).copied().unwrap_or("unknown")
}

/// The label for `class_id` given how many classes the model has: 91
/// means COCO's own ids, anything else the contiguous list.
pub fn coco_label_for(class_id: usize, n_classes: usize) -> &'static str {
    if n_classes == 91 {
        match COCO_91_LABELS.get(class_id).copied() {
            Some("") | None => "unknown",
            Some(l) => l,
        }
    } else {
        coco_label(class_id)
    }
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
            tunables: MaskTunables::default(),
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
        // COCO-91: the dataset's own ids, gaps and all.
        assert_eq!(coco_label_for(1, 91), "person");
        assert_eq!(coco_label_for(17, 91), "cat");
        assert_eq!(coco_label_for(64, 91), "potted plant");
        assert_eq!(coco_label_for(90, 91), "toothbrush");
        assert_eq!(coco_label_for(12, 91), "unknown");
        assert_eq!(coco_label_for(0, 91), "unknown");
        assert_eq!(COCO_91_LABELS.len(), 91);
        assert_eq!(coco_label_for(15, 80), "cat");
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

    // ── IoU + NMS unit tests (feature-gated because the impls live
    //    under cfg(feature = "ort")) ─────────────────────────────

    #[test]
    #[cfg(feature = "ort")]
    fn iou_zero_for_disjoint_boxes() {
        use klipp_ml::BBox;
        let a = BBox { x: 0.0, y: 0.0, w: 0.1, h: 0.1 };
        let b = BBox { x: 0.5, y: 0.5, w: 0.1, h: 0.1 };
        assert_eq!(bbox_iou(&a, &b), 0.0);
    }

    #[test]
    #[cfg(feature = "ort")]
    fn iou_one_for_identical_boxes() {
        use klipp_ml::BBox;
        let a = BBox { x: 0.2, y: 0.3, w: 0.4, h: 0.5 };
        assert!((bbox_iou(&a, &a) - 1.0).abs() < 1e-5);
    }

    #[test]
    #[cfg(feature = "ort")]
    fn iou_half_for_half_overlap() {
        // Two unit squares offset by half — overlap area = 0.5,
        // union = 1.5, IoU = 0.5/1.5 = 1/3.
        use klipp_ml::BBox;
        let a = BBox { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
        let b = BBox { x: 0.5, y: 0.0, w: 1.0, h: 1.0 };
        assert!((bbox_iou(&a, &b) - 1.0 / 3.0).abs() < 1e-5);
    }

    #[test]
    #[cfg(feature = "ort")]
    fn iou_zero_for_zero_area_box() {
        use klipp_ml::BBox;
        let a = BBox { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
        let b = BBox { x: 0.0, y: 0.0, w: 0.5, h: 0.5 };
        assert_eq!(bbox_iou(&a, &b), 0.0);
    }

    #[test]
    #[cfg(feature = "ort")]
    fn nms_collapses_overlapping_same_class() {
        use klipp_ml::{BBox, MaskBuffer, MaskCandidate};
        // Three near-identical "bicycle" boxes — keep the highest
        // confidence one, drop the other two.
        let mk = |conf: f32| MaskCandidate {
            mask: MaskBuffer { width: 1, height: 1, alpha: vec![0u8] },
            confidence: conf,
            bbox: Some(BBox { x: 0.1, y: 0.1, w: 0.3, h: 0.3 }),
            label: Some("bicycle".into()),
        };
        let mut candidates = vec![mk(0.99), mk(0.92), mk(0.85)];
        non_max_suppress_class_aware(&mut candidates, 0.5);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].confidence, 0.99);
    }

    #[test]
    #[cfg(feature = "ort")]
    fn nms_keeps_overlapping_different_classes() {
        use klipp_ml::{BBox, MaskBuffer, MaskCandidate};
        // A "person" and a "bicycle" boxes overlap — both kept
        // (a person on a bike).
        let person = MaskCandidate {
            mask: MaskBuffer { width: 1, height: 1, alpha: vec![0u8] },
            confidence: 0.95,
            bbox: Some(BBox { x: 0.1, y: 0.1, w: 0.5, h: 0.5 }),
            label: Some("person".into()),
        };
        let bicycle = MaskCandidate {
            mask: MaskBuffer { width: 1, height: 1, alpha: vec![0u8] },
            confidence: 0.90,
            bbox: Some(BBox { x: 0.1, y: 0.1, w: 0.5, h: 0.5 }),
            label: Some("bicycle".into()),
        };
        let mut candidates = vec![person, bicycle];
        non_max_suppress_class_aware(&mut candidates, 0.5);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    #[cfg(feature = "ort")]
    fn nms_keeps_distant_same_class() {
        use klipp_ml::{BBox, MaskBuffer, MaskCandidate};
        // Two non-overlapping "person" boxes — both kept
        // (e.g. two people in the frame).
        let mk = |x: f32, conf: f32| MaskCandidate {
            mask: MaskBuffer { width: 1, height: 1, alpha: vec![0u8] },
            confidence: conf,
            bbox: Some(BBox { x, y: 0.1, w: 0.2, h: 0.5 }),
            label: Some("person".into()),
        };
        let mut candidates = vec![mk(0.05, 0.92), mk(0.7, 0.88)];
        non_max_suppress_class_aware(&mut candidates, 0.5);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    #[cfg(feature = "ort")]
    fn nms_no_op_below_two_candidates() {
        use klipp_ml::{BBox, MaskBuffer, MaskCandidate};
        let mut candidates = vec![MaskCandidate {
            mask: MaskBuffer { width: 1, height: 1, alpha: vec![0u8] },
            confidence: 0.5,
            bbox: Some(BBox { x: 0.0, y: 0.0, w: 0.1, h: 0.1 }),
            label: Some("car".into()),
        }];
        non_max_suppress_class_aware(&mut candidates, 0.5);
        assert_eq!(candidates.len(), 1);

        let mut empty = Vec::<MaskCandidate>::new();
        non_max_suppress_class_aware(&mut empty, 0.5);
        assert!(empty.is_empty());
    }

    #[test]
    #[cfg(feature = "ort")]
    fn iomin_one_for_fully_contained_box() {
        // Small box fully inside a larger one — IoMin = 1.0
        // (full containment of the smaller), even though IoU
        // would be tiny.
        use klipp_ml::BBox;
        let big = BBox { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
        let small = BBox { x: 0.4, y: 0.4, w: 0.1, h: 0.1 };
        assert!((bbox_iomin(&big, &small) - 1.0).abs() < 1e-5);
        // IoU on the same pair would be tiny — sanity check the
        // distinction.
        assert!(bbox_iou(&big, &small) < 0.05);
    }

    #[test]
    #[cfg(feature = "ort")]
    fn iomin_zero_for_disjoint_boxes() {
        use klipp_ml::BBox;
        let a = BBox { x: 0.0, y: 0.0, w: 0.1, h: 0.1 };
        let b = BBox { x: 0.5, y: 0.5, w: 0.1, h: 0.1 };
        assert_eq!(bbox_iomin(&a, &b), 0.0);
    }

    #[test]
    #[cfg(feature = "ort")]
    fn nms_collapses_nested_same_class_box() {
        // Big "bicycle" + small "bicycle" inside it — the small one
        // pure-IoU passes (0.04) but IoMin catches it (1.0).
        // Higher-confidence (big) survives.
        use klipp_ml::{BBox, MaskBuffer, MaskCandidate};
        let big = MaskCandidate {
            mask: MaskBuffer { width: 1, height: 1, alpha: vec![0u8] },
            confidence: 0.99,
            bbox: Some(BBox { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }),
            label: Some("bicycle".into()),
        };
        let small = MaskCandidate {
            mask: MaskBuffer { width: 1, height: 1, alpha: vec![0u8] },
            confidence: 0.55,
            bbox: Some(BBox { x: 0.4, y: 0.4, w: 0.1, h: 0.1 }),
            label: Some("bicycle".into()),
        };
        let mut candidates = vec![big, small];
        non_max_suppress_class_aware(&mut candidates, 0.5);
        assert_eq!(candidates.len(), 1);
        assert!((candidates[0].confidence - 0.99).abs() < 1e-5);
    }

    #[test]
    fn builder_nms_threshold_clamps_above_zero() {
        // Negative thresholds clamp to 0; large thresholds (>= 1.0)
        // are intentionally allowed since they disable NMS.
        assert_eq!(
            RfDetrSegBuilder::new()
                .nms_iou_threshold(-0.5)
                .nms_iou_threshold,
            0.0,
        );
        assert!(
            RfDetrSegBuilder::new()
                .nms_iou_threshold(2.0)
                .nms_iou_threshold
                >= 1.0,
        );
        assert_eq!(
            RfDetrSegBuilder::new()
                .nms_iou_threshold(0.7)
                .nms_iou_threshold,
            0.7,
        );
    }
}
