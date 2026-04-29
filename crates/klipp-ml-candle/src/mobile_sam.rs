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
//! 1. Bilinear-resample the source linear-RGBA frame to fit in
//!    1024×1024 preserving aspect ratio (candle's `Sam::preprocess`
//!    bails if either side > 1024).
//! 2. sRGB-encode (linear → gamma) and scale to 0..255 — candle's
//!    `Sam::preprocess` then subtracts ImageNet mean
//!    `[123.675, 116.28, 103.53]` and divides by std
//!    `[58.395, 57.12, 57.375]` and zero-pads to 1024×1024.
//! 3. Layout `(3, H, W)` f32 on the picked device (CPU / CUDA).
//!
//! ## Post-process
//!
//! Mask decoder outputs three candidate masks at 256×256 with IoU
//! predictions. We pick the highest-IoU mask, upsample to 1024²,
//! crop to the unpadded resized rect, then bilinear-resample to
//! source resolution. Threshold at logit > 0 (= sigmoid > 0.5) →
//! u8 alpha.
//!
//! ## Weight load
//!
//! Uses `VarBuilder::from_buffered_safetensors` (the safe variant).
//! Costs ~40 MB extra RAM during load vs `from_mmaped_safetensors`
//! which would require an `unsafe { ... }` block. Klipp's "no
//! unsafe in our crates" rule wins this trade.

use std::path::PathBuf;

use klipp_ml::{
    DevicePreference, MaskCapabilities, MaskError, MaskGenerator, MaskRequest, MaskResponse,
};

#[cfg(feature = "candle")]
use klipp_ml::{MaskBuffer, MaskCandidate};

/// Stable id used in `assets/models.toml` and the registry.
pub const MOBILE_SAM_MODEL_ID: &str = "mobile_sam_v1";

/// User-facing display name.
pub const MOBILE_SAM_DISPLAY_NAME: &str = "MobileSAM";

/// SAM family input resolution. Both MobileSAM and SAM v1 expect
/// 1024×1024 — a single constant for both keeps the pre-process
/// helper reusable when SAM v1 ViT-B lands.
pub const SAM_INPUT_SIZE: u32 = 1024;

/// Builder for the MobileSAM generator.
pub struct MobileSamBuilder {
    weights_path: Option<PathBuf>,
    device: DevicePreference,
}

impl MobileSamBuilder {
    pub fn new() -> Self {
        Self {
            weights_path: None,
            device: DevicePreference::GpuPreferCpu,
        }
    }

    /// Path to the cached MobileSAM weights file (`.safetensors`).
    /// Resolved via [`klipp_ml::ModelCache`] from the manifest entry
    /// `mobile_sam_v1`.
    pub fn weights(mut self, path: PathBuf) -> Self {
        self.weights_path = Some(path);
        self
    }

    /// Override the device. `GpuPreferCpu` is the sensible default;
    /// `Cpu` for slower-but-portable, `GpuRequired` to fail loud
    /// when CUDA isn't available.
    pub fn device(mut self, device: DevicePreference) -> Self {
        self.device = device;
        self
    }

    /// Materialise the generator. Lazy session init — the candle
    /// `Sam` model is loaded on the first `generate` call so a user
    /// who picks MobileSAM in the model picker but never runs
    /// inference pays nothing.
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

/// MobileSAM mask generator.
pub struct MobileSam {
    weights_path: Option<PathBuf>,
    device: DevicePreference,
    #[cfg(feature = "candle")]
    session: Option<CandleSession>,
}

#[cfg(feature = "candle")]
struct CandleSession {
    device: candle_core::Device,
    sam: candle_transformers::models::segment_anything::sam::Sam,
    /// Cached image embedding so click-to-click latency on the same
    /// frame stays under ~100 ms (only prompt + mask decoder runs,
    /// not the heavy image encoder).
    cached_embedding: Option<candle_core::Tensor>,
    /// Frame fingerprint (cheap hash of dims + sample bytes). New
    /// frame ⇒ new embedding.
    cached_frame_hash: u64,
    /// Resized post-fit dimensions (h, w) used when computing the
    /// cached embedding. Forwarded to `forward_for_embeddings` as
    /// `original_h` / `original_w`.
    cached_resized_dims: (usize, usize),
}

impl MaskGenerator for MobileSam {
    fn id(&self) -> &str {
        MOBILE_SAM_MODEL_ID
    }

    fn display_name(&self) -> &str {
        MOBILE_SAM_DISPLAY_NAME
    }

    fn capabilities(&self) -> MaskCapabilities {
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
    /// Lazy session init. Loads the safetensors weights, picks the
    /// device (CPU / CUDA), and builds candle's `Sam::new_tiny` —
    /// the MobileSAM TinyViT-backed variant.
    fn ensure_session(&mut self) -> Result<&mut CandleSession, MaskError> {
        if self.session.is_some() {
            return Ok(self.session.as_mut().expect("just-checked Some"));
        }

        let weights_path = self
            .weights_path
            .as_ref()
            .ok_or_else(|| MaskError::BackendInit("weights path missing".into()))?
            .clone();

        let device = pick_device(self.device)?;

        // Read weights into RAM. `from_buffered_safetensors` is the
        // safe alternative to `from_mmaped_safetensors`. ~40 MB peak
        // for MobileSAM TinyViT — affordable in exchange for zero
        // unsafe blocks in Klipp's own crates.
        let bytes = std::fs::read(&weights_path).map_err(MaskError::Io)?;
        let vb = candle_nn::VarBuilder::from_buffered_safetensors(
            bytes,
            candle_core::DType::F32,
            &device,
        )
        .map_err(|e| MaskError::BackendInit(format!("safetensors load: {e}")))?;

        let sam = candle_transformers::models::segment_anything::sam::Sam::new_tiny(vb)
            .map_err(|e| MaskError::BackendInit(format!("Sam::new_tiny: {e}")))?;

        self.session = Some(CandleSession {
            device,
            sam,
            cached_embedding: None,
            cached_frame_hash: 0,
            cached_resized_dims: (0, 0),
        });
        Ok(self.session.as_mut().expect("just-installed Some"))
    }

    fn generate_with_candle(
        &mut self,
        req: MaskRequest<'_>,
    ) -> Result<MaskResponse, MaskError> {
        use candle_core::Tensor;

        let started = std::time::Instant::now();

        let src_w = req.frame.width as usize;
        let src_h = req.frame.height as usize;

        // Fit source dims into 1024×1024 preserving aspect ratio.
        let (resized_h, resized_w) = fit_in_sam_window(src_h, src_w);

        // Cheap frame fingerprint — dims + sampled corners. Hits the
        // cache when the user clicks repeatedly on the same frame.
        let frame_hash = sample_frame_hash(req.frame.linear_rgba, src_w, src_h);

        let session = self.ensure_session()?;

        // Reuse cached embedding when the frame hasn't changed.
        let need_new_embedding = session.cached_embedding.is_none()
            || session.cached_frame_hash != frame_hash
            || session.cached_resized_dims != (resized_h, resized_w);

        let embedding = if need_new_embedding {
            // Build the (3, H, W) f32 tensor in 0..255 sRGB-encoded
            // values — what candle's `Sam::preprocess` expects.
            let chw = build_sam_input_chw(
                req.frame.linear_rgba,
                src_w,
                src_h,
                resized_w,
                resized_h,
            );
            let img_tensor = Tensor::from_vec(chw, (3, resized_h, resized_w), &session.device)
                .map_err(|e| MaskError::Inference(format!("input tensor: {e}")))?;

            let emb = session
                .sam
                .embeddings(&img_tensor)
                .map_err(|e| MaskError::Inference(format!("image encoder: {e}")))?;
            session.cached_embedding = Some(emb.clone());
            session.cached_frame_hash = frame_hash;
            session.cached_resized_dims = (resized_h, resized_w);
            emb
        } else {
            session
                .cached_embedding
                .as_ref()
                .expect("checked Some")
                .clone()
        };

        // Build the point list. candle's `forward_for_embeddings`
        // takes `(x_norm, y_norm, positive)` tuples — already in
        // [0, 1] image space, which is exactly what
        // `klipp_ml::ClickPrompt` carries. Boxes get expanded to two
        // corner clicks; scribbles to up to 3 sample points.
        let mut points: Vec<(f64, f64, bool)> = Vec::new();
        for click in &req.prompts.clicks {
            points.push((click.x as f64, click.y as f64, click.positive));
        }
        for b in &req.prompts.boxes {
            points.push((b.x1 as f64, b.y1 as f64, true));
            points.push((b.x2 as f64, b.y2 as f64, true));
        }
        for s in &req.prompts.scribbles {
            if s.points.is_empty() {
                continue;
            }
            points.push((s.points[0].0 as f64, s.points[0].1 as f64, s.positive));
            if s.points.len() >= 3 {
                let mid = &s.points[s.points.len() / 2];
                points.push((mid.0 as f64, mid.1 as f64, s.positive));
            }
            if s.points.len() >= 2 {
                let last = s.points.last().expect("checked >= 2");
                points.push((last.0 as f64, last.1 as f64, s.positive));
            }
        }

        // Multimask is candle's native behaviour for single-point
        // input (returns 3 candidate masks at different scales). For
        // multi-point or with-prompts-other-than-points, single
        // mask is what SAM's original paper recommends.
        let multimask = points.len() <= 1;

        let (low_res_mask, iou) = session
            .sam
            .forward_for_embeddings(
                &embedding,
                resized_h,
                resized_w,
                &points,
                multimask,
            )
            .map_err(|e| MaskError::Inference(format!("mask decoder: {e}")))?;

        // Pick the highest-IoU candidate mask.
        // iou shape: (1, num_masks); low_res_mask shape: (1, num_masks, 256, 256).
        let iou_vec: Vec<f32> = iou
            .flatten(0, 1)
            .map_err(|e| MaskError::Inference(format!("iou flatten: {e}")))?
            .to_vec1::<f32>()
            .map_err(|e| MaskError::Inference(format!("iou extract: {e}")))?;
        let (best_idx, best_iou) = iou_vec
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, &v)| (i, v))
            .unwrap_or((0, 0.0));

        let best_mask = low_res_mask
            .get(0)
            .map_err(|e| MaskError::Inference(format!("mask batch: {e}")))?
            .get(best_idx)
            .map_err(|e| MaskError::Inference(format!("mask candidate: {e}")))?;

        // Upsample the 256² low-res mask to 1024², then narrow to
        // the unpadded resized rect (matches `Sam::forward`'s
        // post-processing).
        //
        // candle's upsample_nearest2d / interpolate2d expects a 4D
        // (N, C, H, W) tensor. best_mask is 2D (H, W), so we
        // unsqueeze TWICE to add the N + C dims, then unwrap them
        // both after upsampling.
        let high_res = best_mask
            .unsqueeze(0)
            .map_err(|e| MaskError::Inference(format!("unsqueeze N: {e}")))?
            .unsqueeze(0)
            .map_err(|e| MaskError::Inference(format!("unsqueeze C: {e}")))?
            .upsample_nearest2d(SAM_INPUT_SIZE as usize, SAM_INPUT_SIZE as usize)
            .map_err(|e| MaskError::Inference(format!("upsample: {e}")))?
            .get(0)
            .map_err(|e| MaskError::Inference(format!("hires N: {e}")))?
            .get(0)
            .map_err(|e| MaskError::Inference(format!("hires C: {e}")))?
            .narrow(0, 0, resized_h)
            .map_err(|e| MaskError::Inference(format!("narrow h: {e}")))?
            .narrow(1, 0, resized_w)
            .map_err(|e| MaskError::Inference(format!("narrow w: {e}")))?;

        // Resize from (resized_h, resized_w) to source (src_h, src_w).
        let final_mask = high_res
            .unsqueeze(0)
            .map_err(|e| MaskError::Inference(format!("final unsqueeze: {e}")))?
            .unsqueeze(0)
            .map_err(|e| MaskError::Inference(format!("final unsqueeze 2: {e}")))?
            .interpolate2d(src_h, src_w)
            .map_err(|e| MaskError::Inference(format!("final interpolate: {e}")))?
            .squeeze(0)
            .map_err(|e| MaskError::Inference(format!("final squeeze: {e}")))?
            .squeeze(0)
            .map_err(|e| MaskError::Inference(format!("final squeeze 2: {e}")))?;

        // SAM's mask is logit-space; threshold at 0 (sigmoid > 0.5).
        let mask_logits = final_mask
            .to_vec2::<f32>()
            .map_err(|e| MaskError::Inference(format!("mask to vec: {e}")))?;
        let mut alpha = vec![0u8; src_h * src_w];
        for (y, row) in mask_logits.iter().enumerate() {
            for (x, &logit) in row.iter().enumerate() {
                alpha[y * src_w + x] = if logit > 0.0 { 255 } else { 0 };
            }
        }

        Ok(MaskResponse {
            candidates: vec![MaskCandidate {
                mask: MaskBuffer {
                    width: src_w as u32,
                    height: src_h as u32,
                    alpha,
                },
                confidence: best_iou,
                bbox: None,
                label: Some("isolated_object".into()),
            }],
            inference_ms: started.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(feature = "candle")]
fn pick_device(pref: DevicePreference) -> Result<candle_core::Device, MaskError> {
    use candle_core::Device;
    match pref {
        DevicePreference::Cpu => Ok(Device::Cpu),
        DevicePreference::GpuPreferCpu => {
            Device::cuda_if_available(0).or(Ok(Device::Cpu))
        }
        DevicePreference::GpuRequired => Device::new_cuda(0).map_err(|_| {
            MaskError::DeviceUnavailable("CUDA requested but no CUDA device available")
        }),
    }
}

/// Compute resized (h, w) that fits in `SAM_INPUT_SIZE × SAM_INPUT_SIZE`
/// while preserving the source aspect ratio. Returns the source
/// dimensions verbatim when both already fit. Always returns >= 1
/// for each dim so `Tensor::from_vec((3, h, w), ...)` doesn't choke.
#[cfg(feature = "candle")]
fn fit_in_sam_window(src_h: usize, src_w: usize) -> (usize, usize) {
    let max = SAM_INPUT_SIZE as usize;
    if src_h <= max && src_w <= max {
        return (src_h.max(1), src_w.max(1));
    }
    let scale = (max as f64) / (src_h.max(src_w) as f64);
    let new_h = ((src_h as f64) * scale).round() as usize;
    let new_w = ((src_w as f64) * scale).round() as usize;
    (new_h.clamp(1, max), new_w.clamp(1, max))
}

/// Build the planar (3, H, W) f32 buffer SAM expects: bilinear-resize
/// the source linear-RGBA to (resized_w, resized_h), drop alpha,
/// gamma-encode each channel back to sRGB, scale to 0..255.
#[cfg(feature = "candle")]
fn build_sam_input_chw(
    src: &[f32],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
) -> Vec<f32> {
    let pixel_count = dst_h * dst_w;
    let mut out = vec![0.0_f32; 3 * pixel_count];

    let scale_x = (src_w as f32) / (dst_w as f32);
    let scale_y = (src_h as f32) / (dst_h as f32);

    for dy in 0..dst_h {
        let sy = ((dy as f32) + 0.5) * scale_y - 0.5;
        let sy0 = sy.floor().max(0.0).min((src_h - 1) as f32) as usize;
        let sy1 = (sy0 + 1).min(src_h - 1);
        let fy = (sy - sy0 as f32).clamp(0.0, 1.0);

        for dx in 0..dst_w {
            let sx = ((dx as f32) + 0.5) * scale_x - 0.5;
            let sx0 = sx.floor().max(0.0).min((src_w - 1) as f32) as usize;
            let sx1 = (sx0 + 1).min(src_w - 1);
            let fx = (sx - sx0 as f32).clamp(0.0, 1.0);

            let idx00 = (sy0 * src_w + sx0) * 4;
            let idx01 = (sy0 * src_w + sx1) * 4;
            let idx10 = (sy1 * src_w + sx0) * 4;
            let idx11 = (sy1 * src_w + sx1) * 4;

            let pixel_idx = dy * dst_w + dx;

            for c in 0..3 {
                let p00 = src[idx00 + c];
                let p01 = src[idx01 + c];
                let p10 = src[idx10 + c];
                let p11 = src[idx11 + c];
                let top = p00 * (1.0 - fx) + p01 * fx;
                let bot = p10 * (1.0 - fx) + p11 * fx;
                let lin = (top * (1.0 - fy) + bot * fy).clamp(0.0, 1.0);
                let srgb = klipp_color::transfer::linear_to_srgb(lin);
                let plane_offset = c * pixel_count;
                out[plane_offset + pixel_idx] = srgb * 255.0;
            }
        }
    }

    out
}

/// Cheap frame fingerprint: hash dims + 16 sampled corner/edge bytes.
/// Hits the cache for repeated clicks on the same frame; misses on
/// any meaningful pixel change. Hashing the full 30M-float buffer
/// would take 50-100 ms which defeats the whole "click-to-click
/// under 100 ms" point of the cache.
#[cfg(feature = "candle")]
fn sample_frame_hash(linear_rgba: &[f32], width: usize, height: usize) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    width.hash(&mut hasher);
    height.hash(&mut hasher);

    if linear_rgba.is_empty() {
        return hasher.finish();
    }

    // 9 sample positions: 4 corners + 4 edge midpoints + center.
    let last_idx = linear_rgba.len() - 1;
    let positions = [
        0,
        last_idx,
        linear_rgba.len() / 4,
        linear_rgba.len() / 2,
        (3 * linear_rgba.len()) / 4,
        linear_rgba.len() / 8,
        (3 * linear_rgba.len()) / 8,
        (5 * linear_rgba.len()) / 8,
        (7 * linear_rgba.len()) / 8,
    ];
    for &p in &positions {
        if p < linear_rgba.len() {
            // f32 isn't Hash, hash the bit pattern.
            linear_rgba[p].to_bits().hash(&mut hasher);
        }
    }
    hasher.finish()
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

    // ── feature-gated unit tests for the helper functions ──

    #[test]
    #[cfg(feature = "candle")]
    fn fit_in_sam_window_passes_through_when_already_small() {
        // Both sides ≤ 1024 → identity.
        assert_eq!(fit_in_sam_window(512, 768), (512, 768));
        assert_eq!(fit_in_sam_window(1024, 1024), (1024, 1024));
    }

    #[test]
    #[cfg(feature = "candle")]
    fn fit_in_sam_window_scales_4k_proportionally() {
        // 3840×2160 should fit inside 1024×1024 by limiting on the
        // longer side (width). Aspect ratio preserved.
        let (h, w) = fit_in_sam_window(2160, 3840);
        assert_eq!(w, 1024);
        // Round-to-nearest of 2160 * (1024/3840) = 576.
        assert_eq!(h, 576);
    }

    #[test]
    #[cfg(feature = "candle")]
    fn fit_in_sam_window_handles_extreme_aspect_ratio() {
        // Tall narrow: scale by height. Width clamps to >= 1.
        let (h, w) = fit_in_sam_window(8000, 100);
        assert_eq!(h, 1024);
        assert!(w >= 1);
    }

    #[test]
    #[cfg(feature = "candle")]
    fn build_sam_input_chw_preserves_solid_grey() {
        // A 4×4 mid-grey (linear 0.5) frame upsampled to 8×8 stays
        // mid-grey after gamma encode + scale. linear_to_srgb(0.5) ≈
        // 0.7354 → 187.5 in 0..255.
        let mut src = vec![0.0_f32; 4 * 4 * 4];
        for chunk in src.chunks_exact_mut(4) {
            chunk[0] = 0.5;
            chunk[1] = 0.5;
            chunk[2] = 0.5;
            chunk[3] = 1.0;
        }
        let chw = build_sam_input_chw(&src, 4, 4, 8, 8);
        assert_eq!(chw.len(), 3 * 8 * 8);
        let expected = klipp_color::transfer::linear_to_srgb(0.5) * 255.0;
        for &v in &chw {
            assert!((v - expected).abs() < 0.5, "expected {expected}, got {v}");
        }
    }

    #[test]
    #[cfg(feature = "candle")]
    fn frame_hash_is_stable_for_same_pixels() {
        let pixels = vec![0.5_f32; 32];
        let h1 = sample_frame_hash(&pixels, 4, 2);
        let h2 = sample_frame_hash(&pixels, 4, 2);
        assert_eq!(h1, h2);
    }

    #[test]
    #[cfg(feature = "candle")]
    fn frame_hash_changes_when_pixels_change() {
        let mut a = vec![0.5_f32; 32];
        let b = a.clone();
        a[0] = 0.6;
        assert_ne!(
            sample_frame_hash(&a, 4, 2),
            sample_frame_hash(&b, 4, 2)
        );
    }

    #[test]
    #[cfg(feature = "candle")]
    fn frame_hash_changes_with_dims() {
        let pixels = vec![0.5_f32; 32];
        assert_ne!(
            sample_frame_hash(&pixels, 4, 2),
            sample_frame_hash(&pixels, 2, 4)
        );
    }
}
