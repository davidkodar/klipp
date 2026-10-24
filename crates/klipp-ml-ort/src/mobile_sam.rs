//! MobileSAM — click-to-mask via TinyViT image encoder + SAM prompt
//! encoder + mask decoder, exported to ONNX and run on ONNX Runtime: on
//! the GPU where the runtime has a provider for it (#2245).
//!
//! ## Architecture (2 models)
//!
//! 1. **Image encoder** (TinyViT, ~28 MB) — heavy, ~once per source
//!    frame. Takes the frame as sRGB 0..255, height × width × 3, its
//!    longest side 1024; normalises and pads it itself. Outputs a
//!    `(1, 256, 64, 64)` image embedding.
//! 2. **Prompt encoder + mask decoder** (SAM's, ~16 MB) — lightweight.
//!    Combines the embedding with clicks and boxes → 4 masks (the
//!    single-mask answer, then small / medium / large) with IoU
//!    predictions, at the size asked for.
//!
//! Interactive use (click → mask in a few ms) caches the image
//! embedding per frame and re-runs only the decoder on each click.
//! `&mut self` in the trait carries this cache across `generate`
//! calls.
//!
//! ## Pre-process
//!
//! Bilinear-resample the source linear-RGBA frame so its longest side
//! is 1024, as SAM's points and masks assume; sRGB-encode (linear →
//! gamma) and scale to 0..255; lay out `(H, W, 3)`.
//!
//! ## Post-process
//!
//! With one click the best of the three multi-mask answers (by
//! predicted IoU), else the single-mask answer, at the resized size;
//! bilinear-resampled to the source resolution and thresholded at
//! logit > 0 (= sigmoid > 0.5) → u8 alpha.

use std::path::PathBuf;

use klipp_ml::{
    DevicePreference, MaskCapabilities, MaskError, MaskGenerator, MaskRequest, MaskResponse,
};

#[cfg(feature = "ort")]
use klipp_ml::{MaskBuffer, MaskCandidate};

/// Stable id used in `assets/models.toml` and the registry: the image
/// encoder, which brings the decoder ([`MOBILE_SAM_DECODER_ID`]) with it.
pub const MOBILE_SAM_MODEL_ID: &str = "mobile_sam_onnx";

/// The mask decoder's id in `assets/models.toml`.
pub const MOBILE_SAM_DECODER_ID: &str = "mobile_sam_onnx_decoder";

/// The model as one candle file, before the ONNX encoder and decoder
/// (#2245): its id, size and hash as the manifest gave them, so only
/// that file is removed from the model cache once they are there.
pub const MOBILE_SAM_CANDLE_WEIGHTS: (&str, u64, &str) = ("mobile_sam_v1", 40_613_024, "5ad43a841b57cc798e2837e251c15aa9f602868a1780d195459383d117784cf7");

/// User-facing display name.
pub const MOBILE_SAM_DISPLAY_NAME: &str = "MobileSAM";

/// SAM family input resolution: the longest side of the frame the
/// encoder sees.
pub const SAM_INPUT_SIZE: u32 = 1024;

/// Builder for the MobileSAM generator.
pub struct MobileSamBuilder {
    weights_path: Option<PathBuf>,
    decoder_path: Option<PathBuf>,
    device: DevicePreference,
}

impl MobileSamBuilder {
    pub fn new() -> Self {
        Self {
            weights_path: None,
            decoder_path: None,
            device: DevicePreference::GpuPreferCpu,
        }
    }

    /// Path to the cached image encoder (`.onnx`), from the manifest
    /// entry [`MOBILE_SAM_MODEL_ID`].
    pub fn weights(mut self, path: PathBuf) -> Self {
        self.weights_path = Some(path);
        self
    }

    /// Path to the cached mask decoder (`.onnx`), from the manifest
    /// entry [`MOBILE_SAM_DECODER_ID`].
    pub fn decoder(mut self, path: PathBuf) -> Self {
        self.decoder_path = Some(path);
        self
    }

    /// Override the device. `GpuPreferCpu` is the sensible default;
    /// `Cpu` for slower-but-portable, `GpuRequired` to fail loud
    /// when no GPU provider loads.
    pub fn device(mut self, device: DevicePreference) -> Self {
        self.device = device;
        self
    }

    /// Materialise the generator. Lazy session init — the sessions are
    /// built on the first `generate` call so a user who picks MobileSAM
    /// in the model picker but never runs inference pays nothing.
    pub fn build(self) -> Result<MobileSam, MaskError> {
        Ok(MobileSam {
            weights_path: self.weights_path,
            decoder_path: self.decoder_path,
            device: self.device,
            #[cfg(feature = "ort")]
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
    decoder_path: Option<PathBuf>,
    device: DevicePreference,
    #[cfg(feature = "ort")]
    session: Option<OrtSession>,
}

#[cfg(feature = "ort")]
struct OrtSession {
    encoder: ort::session::Session,
    decoder: ort::session::Session,
    /// The models' names in the provider record.
    encoder_model: String,
    decoder_model: String,
    /// Cached image embedding so click-to-click latency on the same
    /// frame stays at the decoder's (the heavy image encoder doesn't
    /// run again).
    cached_embedding: Option<Vec<f32>>,
    /// Frame fingerprint (cheap hash of dims + sample bytes). New
    /// frame ⇒ new embedding.
    cached_frame_hash: u64,
    /// Resized dimensions (h, w) the cached embedding was made at.
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
        if self.weights_path.is_none() || self.decoder_path.is_none() {
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

        #[cfg(not(feature = "ort"))]
        {
            let _ = req;
            Err(MaskError::BackendInit(
                "MobileSAM needs ONNX Runtime — rebuild with --features ai-ort".into(),
            ))
        }

        #[cfg(feature = "ort")]
        {
            self.generate_with_ort(req)
        }
    }
}

#[cfg(feature = "ort")]
fn session(weights: &std::path::Path, device: DevicePreference) -> Result<(ort::session::Session, String), MaskError> {
    use ort::session::builder::GraphOptimizationLevel;
    crate::runtime_resolve::ensure_runtime_loadable().map_err(MaskError::BackendInit)?;
    let builder = ort::session::Session::builder()
        .map_err(|e| MaskError::BackendInit(format!("session builder: {e}")))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| MaskError::BackendInit(format!("set optimisation: {e}")))?;
    // On the GPU where it loads, what it got recorded (#2245).
    let model = weights.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let session = crate::provider::on_device(builder, device, &model)
        .map_err(MaskError::BackendInit)?
        .commit_from_file(weights)
        .map_err(|e| MaskError::BackendInit(format!("load {}: {e}", weights.display())))?;
    Ok((session, model))
}

/// The candle weights the ONNX models replaced, removed from the model
/// cache once those are there: checked once a process, and only the file
/// the old manifest entry named.
#[cfg(feature = "ort")]
fn remove_candle_weights() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Ok(cache) = klipp_ml::ModelCache::open() else { return };
        let (id, size, sha) = MOBILE_SAM_CANDLE_WEIGHTS;
        match cache.remove_replaced(id, size, sha) {
            Ok(Some(bytes)) => tracing::info!("removed the old MobileSAM weights ({id}, {} MB): the ONNX encoder and decoder replace them", bytes / 1_000_000),
            Ok(None) => {}
            Err(e) => tracing::warn!("the old MobileSAM weights ({id}) could not be removed: {e}"),
        }
    });
}

#[cfg(feature = "ort")]
impl MobileSam {
    /// Lazy session init: the encoder and decoder sessions, each on the
    /// device asked for where it loads.
    // `is_some` + `expect` rather than `if let Some(s) = ..as_mut()`:
    // returning the borrow from inside the `if let` extends it over the
    // whole function, so the `self.session = ..` below is rejected
    // (E0506). The borrow checker cannot see that the two paths are
    // exclusive.
    #[allow(clippy::unnecessary_unwrap)]
    fn ensure_session(&mut self) -> Result<&mut OrtSession, MaskError> {
        if self.session.is_some() {
            return Ok(self.session.as_mut().expect("just-checked Some"));
        }
        let missing = || MaskError::BackendInit("weights path missing".into());
        let (encoder, encoder_model) = session(self.weights_path.as_ref().ok_or_else(missing)?, self.device)?;
        let (decoder, decoder_model) = session(self.decoder_path.as_ref().ok_or_else(missing)?, self.device)?;
        remove_candle_weights();
        self.session = Some(OrtSession {
            encoder,
            decoder,
            encoder_model,
            decoder_model,
            cached_embedding: None,
            cached_frame_hash: 0,
            cached_resized_dims: (0, 0),
        });
        Ok(self.session.as_mut().expect("just-installed Some"))
    }

    fn generate_with_ort(&mut self, req: MaskRequest<'_>) -> Result<MaskResponse, MaskError> {
        let started = std::time::Instant::now();
        let src_w = req.frame.width as usize;
        let src_h = req.frame.height as usize;

        // The longest side to SAM's 1024, aspect ratio kept.
        let (resized_h, resized_w) = fit_in_sam_window(src_h, src_w);

        // Cheap frame fingerprint — dims + sampled corners. Hits the
        // cache when the user clicks repeatedly on the same frame.
        let frame_hash = sample_frame_hash(req.frame.linear_rgba, src_w, src_h);

        let session = self.ensure_session()?;
        let need_new_embedding = session.cached_embedding.is_none()
            || session.cached_frame_hash != frame_hash
            || session.cached_resized_dims != (resized_h, resized_w);

        if need_new_embedding {
            let hwc = build_sam_input_hwc(req.frame.linear_rgba, src_w, src_h, resized_w, resized_h);
            let input = ort::value::Value::from_array(([resized_h as i64, resized_w as i64, 3], hwc))
                .map_err(|e| MaskError::Inference(format!("input tensor: {e}")))?;
            let encoding = std::time::Instant::now();
            let outputs = session
                .encoder
                .run(ort::inputs!["input_image" => input])
                .map_err(|e| MaskError::Inference(format!("image encoder: {e}")))?;
            let (_, emb) = outputs["image_embeddings"]
                .try_extract_tensor::<f32>()
                .map_err(|e| MaskError::Inference(format!("image embeddings: {e}")))?;
            session.cached_embedding = Some(emb.to_vec());
            session.cached_frame_hash = frame_hash;
            session.cached_resized_dims = (resized_h, resized_w);
            crate::provider::timed(&session.encoder_model, 1, encoding.elapsed());
        }
        let embedding = session.cached_embedding.clone().expect("made above");

        // The prompts in the resized frame's pixels: `klipp_ml` carries
        // them in [0, 1] image space. A box is SAM's two corner labels
        // (2, 3); scribbles give up to 3 sample points.
        let (rw, rh) = (resized_w as f32, resized_h as f32);
        let prompts = req.prompts.clicks.len()
            + req.prompts.scribbles.iter().map(|s| s.points.len().min(3)).sum::<usize>()
            + 2 * req.prompts.boxes.len();
        let mut coords: Vec<f32> = Vec::new();
        let mut labels: Vec<f32> = Vec::new();
        let mut point = |x: f32, y: f32, label: f32| {
            coords.extend([x * rw, y * rh]);
            labels.push(label);
        };
        for click in &req.prompts.clicks {
            point(click.x, click.y, if click.positive { 1.0 } else { 0.0 });
        }
        for s in &req.prompts.scribbles {
            let label = if s.positive { 1.0 } else { 0.0 };
            let picks: Vec<&(f32, f32)> = match s.points.len() {
                0 => Vec::new(),
                1 => vec![&s.points[0]],
                2 => vec![&s.points[0], &s.points[1]],
                n => vec![&s.points[0], &s.points[n / 2], &s.points[n - 1]],
            };
            for p in picks {
                point(p.0, p.1, label);
            }
        }
        for b in &req.prompts.boxes {
            point(b.x1.min(b.x2), b.y1.min(b.y2), 2.0);
            point(b.x1.max(b.x2), b.y1.max(b.y2), 3.0);
        }
        if req.prompts.boxes.is_empty() {
            // SAM's padding point, when there is no box.
            point(0.0, 0.0, -1.0);
        }
        let n = labels.len() as i64;

        let value = |shape: Vec<i64>, data: Vec<f32>| {
            ort::value::Value::from_array((shape, data)).map_err(|e| MaskError::Inference(format!("decoder input: {e}")))
        };
        let decoding = std::time::Instant::now();
        let outputs = session
            .decoder
            .run(ort::inputs![
                "image_embeddings" => value(vec![1, 256, 64, 64], embedding)?,
                "point_coords" => value(vec![1, n, 2], coords)?,
                "point_labels" => value(vec![1, n], labels)?,
                "mask_input" => value(vec![1, 1, 256, 256], vec![0.0; 256 * 256])?,
                "has_mask_input" => value(vec![1], vec![0.0])?,
                "orig_im_size" => value(vec![2], vec![rh, rw])?,
            ])
            .map_err(|e| MaskError::Inference(format!("mask decoder: {e}")))?;
        let (_, iou) = outputs["iou_predictions"]
            .try_extract_tensor::<f32>()
            .map_err(|e| MaskError::Inference(format!("iou: {e}")))?;
        let (mask_shape, masks) = outputs["masks"]
            .try_extract_tensor::<f32>()
            .map_err(|e| MaskError::Inference(format!("masks: {e}")))?;
        crate::provider::timed(&session.decoder_model, 1, decoding.elapsed());

        // One click: the best of the multi-mask answers (1..=3), which
        // SAM recommends for an ambiguous prompt; else the single-mask
        // answer (0).
        let (best_idx, best_iou) = if prompts <= 1 && iou.len() >= 4 {
            (1..4).map(|i| (i, iou[i])).max_by(|a, b| a.1.total_cmp(&b.1)).expect("three")
        } else {
            (0, iou.first().copied().unwrap_or(0.0))
        };
        let (mh, mw) = match mask_shape.iter().as_slice() {
            [_, _, h, w] => (*h as usize, *w as usize),
            other => return Err(MaskError::Inference(format!("mask shape {other:?}"))),
        };
        let plane = &masks[best_idx * mh * mw..(best_idx + 1) * mh * mw];
        let alpha = resize_logits_to_alpha(plane, mw, mh, src_w, src_h);

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

/// The resized (h, w) whose longest side is `SAM_INPUT_SIZE`, the
/// source's aspect ratio kept, as SAM's `ResizeLongestSide` gives it:
/// its points and masks assume that frame. Always >= 1 each.
#[cfg(feature = "ort")]
fn fit_in_sam_window(src_h: usize, src_w: usize) -> (usize, usize) {
    let max = SAM_INPUT_SIZE as usize;
    let scale = (max as f64) / (src_h.max(src_w).max(1) as f64);
    let new_h = ((src_h as f64) * scale + 0.5) as usize;
    let new_w = ((src_w as f64) * scale + 0.5) as usize;
    (new_h.clamp(1, max), new_w.clamp(1, max))
}

/// The `(H, W, 3)` f32 buffer the encoder takes: bilinear-resize the
/// source linear-RGBA to (dst_w, dst_h), drop alpha, gamma-encode each
/// channel back to sRGB, scale to 0..255.
#[cfg(feature = "ort")]
fn build_sam_input_hwc(
    src: &[f32],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
) -> Vec<f32> {
    let mut out = vec![0.0_f32; 3 * dst_h * dst_w];
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
            let (i00, i01) = ((sy0 * src_w + sx0) * 4, (sy0 * src_w + sx1) * 4);
            let (i10, i11) = ((sy1 * src_w + sx0) * 4, (sy1 * src_w + sx1) * 4);
            let o = (dy * dst_w + dx) * 3;
            for c in 0..3 {
                let top = src[i00 + c] * (1.0 - fx) + src[i01 + c] * fx;
                let bot = src[i10 + c] * (1.0 - fx) + src[i11 + c] * fx;
                let lin = (top * (1.0 - fy) + bot * fy).clamp(0.0, 1.0);
                out[o + c] = klipp_color::transfer::linear_to_srgb(lin) * 255.0;
            }
        }
    }
    out
}

/// A `(h, w)` plane of mask logits bilinear-resampled to (dst_w, dst_h)
/// and thresholded at 0 (sigmoid > 0.5) to u8 alpha.
#[cfg(feature = "ort")]
fn resize_logits_to_alpha(logits: &[f32], w: usize, h: usize, dst_w: usize, dst_h: usize) -> Vec<u8> {
    let mut alpha = vec![0u8; dst_w * dst_h];
    let scale_x = w as f32 / dst_w as f32;
    let scale_y = h as f32 / dst_h as f32;
    for dy in 0..dst_h {
        let sy = ((dy as f32 + 0.5) * scale_y - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, fy) = (sy.floor() as usize, sy - sy.floor());
        let y1 = (y0 + 1).min(h - 1);
        for dx in 0..dst_w {
            let sx = ((dx as f32 + 0.5) * scale_x - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, fx) = (sx.floor() as usize, sx - sx.floor());
            let x1 = (x0 + 1).min(w - 1);
            let top = logits[y0 * w + x0] * (1.0 - fx) + logits[y0 * w + x1] * fx;
            let bot = logits[y1 * w + x0] * (1.0 - fx) + logits[y1 * w + x1] * fx;
            if top * (1.0 - fy) + bot * fy > 0.0 {
                alpha[dy * dst_w + dx] = 255;
            }
        }
    }
    alpha
}

/// Cheap frame fingerprint: hash dims + 16 sampled corner/edge bytes.
/// Hits the cache for repeated clicks on the same frame; misses on
/// any meaningful pixel change. Hashing the full 30M-float buffer
/// would take 50-100 ms which defeats the whole "click-to-click
/// under 100 ms" point of the cache.
#[cfg(feature = "ort")]
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
    #[cfg(not(feature = "ort"))]
    fn generate_without_ort_feature_says_rebuild_with_flag() {
        let pixels = vec![0.0_f32; 4 * 4 * 4];
        let mut g = MobileSamBuilder::new()
            .weights(PathBuf::from("/tmp/mobile_sam_onnx.bin"))
            .decoder(PathBuf::from("/tmp/mobile_sam_onnx_decoder.bin"))
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
            tunables: MaskTunables::default(),
        };
        let mut g = MobileSamBuilder::new()
            .weights(PathBuf::from("/tmp/dummy"))
            .decoder(PathBuf::from("/tmp/dummy"))
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
            tunables: MaskTunables::default(),
        };
        let mut g = MobileSamBuilder::new()
            .weights(PathBuf::from("/tmp/dummy"))
            .decoder(PathBuf::from("/tmp/dummy"))
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
    #[cfg(feature = "ort")]
    fn fit_in_sam_window_brings_the_longest_side_to_1024() {
        // Smaller frames are scaled up, as SAM's points and masks assume.
        assert_eq!(fit_in_sam_window(512, 768), (683, 1024));
        assert_eq!(fit_in_sam_window(1024, 1024), (1024, 1024));
    }

    #[test]
    #[cfg(feature = "ort")]
    fn fit_in_sam_window_scales_4k_proportionally() {
        let (h, w) = fit_in_sam_window(2160, 3840);
        assert_eq!((h, w), (576, 1024));
    }

    #[test]
    #[cfg(feature = "ort")]
    fn fit_in_sam_window_handles_extreme_aspect_ratio() {
        // Tall narrow: scale by height. Width clamps to >= 1.
        let (h, w) = fit_in_sam_window(8000, 100);
        assert_eq!(h, 1024);
        assert!(w >= 1);
    }

    #[test]
    #[cfg(feature = "ort")]
    fn build_sam_input_hwc_preserves_solid_grey() {
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
        let hwc = build_sam_input_hwc(&src, 4, 4, 8, 8);
        assert_eq!(hwc.len(), 8 * 8 * 3);
        let expected = klipp_color::transfer::linear_to_srgb(0.5) * 255.0;
        for &v in &hwc {
            assert!((v - expected).abs() < 0.5, "expected {expected}, got {v}");
        }
    }

    #[test]
    #[cfg(feature = "ort")]
    fn logits_resize_to_a_thresholded_mask() {
        // The left half positive: the left half of the bigger mask is in.
        let logits = [1.0, -1.0, 1.0, -1.0];
        let alpha = resize_logits_to_alpha(&logits, 2, 2, 8, 4);
        assert_eq!(alpha.len(), 32);
        for y in 0..4 {
            assert_eq!(&alpha[y * 8..y * 8 + 3], &[255, 255, 255]);
            assert_eq!(&alpha[y * 8 + 5..y * 8 + 8], &[0, 0, 0]);
        }
    }

    #[test]
    #[cfg(feature = "ort")]
    fn frame_hash_is_stable_for_same_pixels() {
        let pixels = vec![0.5_f32; 32];
        let h1 = sample_frame_hash(&pixels, 4, 2);
        let h2 = sample_frame_hash(&pixels, 4, 2);
        assert_eq!(h1, h2);
    }

    #[test]
    #[cfg(feature = "ort")]
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
    #[cfg(feature = "ort")]
    fn frame_hash_changes_with_dims() {
        let pixels = vec![0.5_f32; 32];
        assert_ne!(
            sample_frame_hash(&pixels, 4, 2),
            sample_frame_hash(&pixels, 2, 4)
        );
    }

    // ── with the model in the cache ──

    /// The model from the cache on `device`, when it is downloaded.
    #[cfg(feature = "ort")]
    fn cached_model(device: DevicePreference) -> Option<MobileSam> {
        let cache = klipp_ml::ModelCache::open().ok()?;
        let (enc, dec) = (cache.path_for_id(MOBILE_SAM_MODEL_ID), cache.path_for_id(MOBILE_SAM_DECODER_ID));
        if !enc.is_file() || !dec.is_file() || crate::runtime_resolve::ensure_runtime_loadable().is_err() {
            eprintln!("skipping: MobileSAM or ONNX Runtime is missing");
            return None;
        }
        Some(MobileSamBuilder::new().weights(enc).decoder(dec).device(device).build().unwrap())
    }

    /// A 320×180 frame (linear light): a bright disc on the left on a
    /// dark ground, a grey square on the right.
    #[cfg(feature = "ort")]
    fn two_shapes() -> Vec<f32> {
        let (w, h) = (320usize, 180usize);
        let mut px = vec![0.0f32; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - 90.0, y as f32 - 90.0);
                let v = if dx * dx + dy * dy < 50.0 * 50.0 {
                    [0.9, 0.5, 0.1]
                } else if (200..280).contains(&x) && (50..130).contains(&y) {
                    [0.3, 0.3, 0.35]
                } else {
                    [0.02, 0.03, 0.05]
                };
                px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[v[0], v[1], v[2], 1.0]);
            }
        }
        px
    }

    #[cfg(feature = "ort")]
    fn click(px: &[f32], x: f32, y: f32) -> MaskRequest<'_> {
        MaskRequest {
            frame: InputFrame { width: 320, height: 180, linear_rgba: px },
            prompts: MaskPrompts { clicks: vec![klipp_ml::ClickPrompt { x, y, positive: true }], ..Default::default() },
            temporal: None,
            tunables: MaskTunables::default(),
        }
    }

    /// A click on the disc gives the disc: in at its centre, out at the
    /// square and the ground.
    #[test]
    #[cfg(feature = "ort")]
    fn a_click_isolates_the_shape_under_it() {
        let Some(mut sam) = cached_model(DevicePreference::Cpu) else { return };
        let px = two_shapes();
        let mask = sam.generate(click(&px, 90.0 / 320.0, 0.5)).unwrap().candidates.remove(0).mask;
        let at = |x: usize, y: usize| mask.alpha[y * 320 + x];
        assert_eq!(at(90, 90), 255, "the disc's centre");
        assert_eq!(at(240, 90), 0, "the square");
        assert_eq!(at(10, 10), 0, "the ground");
    }

    /// On the GPU the masks are the CPU's (IoU ≥ 0.99 for clicks on each
    /// shape and a box), and the encoder's time is reported (#2245). Runs
    /// where the runtime has a GPU provider; `KLIPP_TEST_REQUIRE_ORT_GPU=1`
    /// makes one required.
    #[test]
    #[cfg(feature = "ort")]
    fn the_gpus_masks_are_the_cpus() {
        let Some(mut gpu) = cached_model(DevicePreference::GpuPreferCpu) else { return };
        let px = two_shapes();
        let ask = |sam: &mut MobileSam, req: MaskRequest<'_>| sam.generate(req).unwrap().candidates.remove(0).mask.alpha;
        let box_req = MaskRequest {
            frame: InputFrame { width: 320, height: 180, linear_rgba: &px },
            prompts: MaskPrompts { boxes: vec![klipp_ml::BoxPrompt { x1: 0.6, y1: 0.25, x2: 0.9, y2: 0.75 }], ..Default::default() },
            temporal: None,
            tunables: MaskTunables::default(),
        };
        // A first frame compiles the GPU's kernels; the time reported is
        // the next frame's.
        let mut warm = px.clone();
        warm[0] = 0.5;
        ask(&mut gpu, click(&warm, 0.5, 0.5));
        let on_gpu = [ask(&mut gpu, click(&px, 90.0 / 320.0, 0.5)), ask(&mut gpu, click(&px, 240.0 / 320.0, 0.5)), ask(&mut gpu, box_req.clone())];
        let on = crate::provider::used_by(MOBILE_SAM_MODEL_ID).map(|u| u.provider).unwrap_or("CPU");
        if on == "CPU" {
            assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_ORT_GPU").as_deref(), Ok("1"), "a GPU provider was required");
            eprintln!("skipping: the runtime has no GPU provider");
            return;
        }
        let line = crate::provider::used_by(MOBILE_SAM_MODEL_ID).map(|u| u.line()).unwrap_or_default();
        assert!(line.contains("ms a frame"), "{line}");
        eprintln!("{line}");
        eprintln!("{}", crate::provider::used_by(MOBILE_SAM_DECODER_ID).map(|u| u.line()).unwrap_or_default());
        let mut cpu = cached_model(DevicePreference::Cpu).unwrap();
        let on_cpu = [ask(&mut cpu, click(&px, 90.0 / 320.0, 0.5)), ask(&mut cpu, click(&px, 240.0 / 320.0, 0.5)), ask(&mut cpu, box_req)];
        for (i, (g, c)) in on_gpu.iter().zip(&on_cpu).enumerate() {
            let both = g.iter().zip(c).filter(|(a, b)| **a > 0 && **b > 0).count();
            let either = g.iter().zip(c).filter(|(a, b)| **a > 0 || **b > 0).count();
            assert!(either > 0, "prompt {i} gave an empty mask");
            let iou = both as f64 / either as f64;
            assert!(iou >= 0.99, "prompt {i}: IoU {iou}");
        }
    }
}
