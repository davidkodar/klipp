//! YuNet: a face detector of a quarter of a megabyte (OpenCV's, MIT),
//! run through ONNX Runtime. A face box with five landmarks — the eyes,
//! the nose, the corners of the mouth — per face, which is what a skin
//! read needs: a person detector's box is a whole body, or a window
//! with a video of one, and the skin is somewhere inside it.

/// A face: its box and landmarks as fractions of the picture it was
/// found in, and how sure the detector is.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    /// Right eye, left eye, nose tip, right mouth corner, left mouth corner.
    pub landmarks: [(f32, f32); 5],
}

/// Without ONNX Runtime in the build it cannot be opened, and says so;
/// the decoding below is plain Rust and always there.
pub struct FaceDetector {
    #[cfg(feature = "ort")]
    session: ort::session::Session,
}

/// Below this the detector's guess is not a face.
pub const SCORE_THRESHOLD: f32 = 0.6;
/// Two boxes overlapping this much are one face.
pub const NMS_IOU: f32 = 0.3;

#[cfg(not(feature = "ort"))]
impl FaceDetector {
    pub fn open(_weights: &std::path::Path) -> Result<Self, String> {
        Err("this build of Klipp has no ONNX Runtime, which face detection needs".into())
    }

    pub fn detect(&mut self, _rgba: &[u8], _width: u32, _height: u32) -> Result<Vec<Face>, String> {
        Err("no ONNX Runtime in this build".into())
    }
}

#[cfg(feature = "ort")]
impl FaceDetector {
    pub fn open(weights: &std::path::Path) -> Result<Self, String> {
        use ort::session::builder::GraphOptimizationLevel;
        crate::runtime_resolve::ensure_runtime_loadable()?;
        let builder = ort::session::Session::builder()
            .map_err(|e| format!("session builder: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| format!("set optimisation: {e}"))?
            .with_intra_threads(4)
            .map_err(|e| format!("set threads: {e}"))?;
        let model = weights.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let session = crate::provider::on_device(builder, klipp_ml::DevicePreference::GpuPreferCpu, &model)?
            .commit_from_file(weights)
            .map_err(|e| format!("load {}: {e}", weights.display()))?;
        Ok(Self { session })
    }

    /// The faces in an RGBA8 picture. The model takes a fixed square,
    /// so the picture is fitted into it whole — scaled to fit, centred,
    /// the rest black — and the boxes and landmarks come back as
    /// fractions of the picture that was given, not of the square.
    pub fn detect(&mut self, rgba: &[u8], width: u32, height: u32) -> Result<Vec<Face>, String> {
        let (w, h) = (width as usize, height as usize);
        if w == 0 || h == 0 || rgba.len() < w * h * 4 {
            return Ok(Vec::new());
        }
        const SIDE: usize = 640;
        let scale = (SIDE as f32 / w as f32).min(SIDE as f32 / h as f32);
        let (sw, sh) = (((w as f32 * scale).round() as usize).clamp(1, SIDE), ((h as f32 * scale).round() as usize).clamp(1, SIDE));
        let (ox, oy) = ((SIDE - sw) / 2, (SIDE - sh) / 2);
        let n = SIDE * SIDE;
        // BGR, raw 0..255, as OpenCV feeds it; the picture bilinear into
        // the middle of the square.
        let mut chw = vec![0f32; 3 * n];
        for y in 0..sh {
            let fy = ((y as f32 + 0.5) / scale - 0.5).clamp(0.0, h as f32 - 1.0);
            let (y0, ty) = (fy.floor() as usize, fy - fy.floor());
            let y1 = (y0 + 1).min(h - 1);
            for x in 0..sw {
                let fx = ((x as f32 + 0.5) / scale - 0.5).clamp(0.0, w as f32 - 1.0);
                let (x0, tx) = (fx.floor() as usize, fx - fx.floor());
                let x1 = (x0 + 1).min(w - 1);
                let o = (oy + y) * SIDE + ox + x;
                for (c, src) in [(0usize, 2usize), (1, 1), (2, 0)] {
                    let p = |yy: usize, xx: usize| rgba[(yy * w + xx) * 4 + src] as f32;
                    let top = p(y0, x0) * (1.0 - tx) + p(y0, x1) * tx;
                    let bot = p(y1, x0) * (1.0 - tx) + p(y1, x1) * tx;
                    chw[c * n + o] = top * (1.0 - ty) + bot * ty;
                }
            }
        }
        let (pw, ph) = (SIDE, SIDE);
        let input = ort::value::Value::from_array(([1i64, 3, ph as i64, pw as i64], chw)).map_err(|e| format!("face input: {e}"))?;
        let outputs = self.session.run(ort::inputs![input]).map_err(|e| format!("face run: {e}"))?;
        let take = |name: &str| -> Result<Vec<f32>, String> {
            let v = outputs.get(name).ok_or_else(|| format!("the face model gave no {name}"))?;
            let (_, data) = v.try_extract_tensor::<f32>().map_err(|e| format!("read {name}: {e}"))?;
            Ok(data.to_vec())
        };
        let mut heads = Vec::new();
        for stride in [8usize, 16, 32] {
            heads.push(StrideHead {
                stride,
                cols: pw / stride,
                rows: ph / stride,
                cls: take(&format!("cls_{stride}"))?,
                obj: take(&format!("obj_{stride}"))?,
                bbox: take(&format!("bbox_{stride}"))?,
                kps: take(&format!("kps_{stride}"))?,
            });
        }
        // Back from the square to the picture that was given.
        let faces = decode(&heads, SIDE as f32, SIDE as f32, SCORE_THRESHOLD);
        let (fx, fy) = (SIDE as f32 / sw as f32, SIDE as f32 / sh as f32);
        let (dx, dy) = (ox as f32 / SIDE as f32, oy as f32 / SIDE as f32);
        Ok(faces
            .into_iter()
            .map(|f| Face {
                x: ((f.x - dx) * fx).clamp(0.0, 1.0),
                y: ((f.y - dy) * fy).clamp(0.0, 1.0),
                w: (f.w * fx).min(1.0),
                h: (f.h * fy).min(1.0),
                score: f.score,
                landmarks: {
                    let mut l = f.landmarks;
                    for p in l.iter_mut() {
                        *p = (((p.0 - dx) * fx).clamp(0.0, 1.0), ((p.1 - dy) * fy).clamp(0.0, 1.0));
                    }
                    l
                },
            })
            .collect())
    }
}

/// One output scale of the model.
pub struct StrideHead {
    pub stride: usize,
    pub cols: usize,
    pub rows: usize,
    pub cls: Vec<f32>,
    pub obj: Vec<f32>,
    pub bbox: Vec<f32>,
    pub kps: Vec<f32>,
}

/// The faces in the model's outputs, as OpenCV decodes them: a cell's
/// score is the root of its class and object scores, its box is the
/// cell's corner plus the offsets, times the stride; then non-maximum
/// suppression. Fractions of a `w`×`h` picture.
pub fn decode(heads: &[StrideHead], w: f32, h: f32, threshold: f32) -> Vec<Face> {
    let mut found: Vec<Face> = Vec::new();
    for head in heads {
        let s = head.stride as f32;
        for r in 0..head.rows {
            for c in 0..head.cols {
                let idx = r * head.cols + c;
                let (Some(cls), Some(obj)) = (head.cls.get(idx), head.obj.get(idx)) else { continue };
                let score = (cls.clamp(0.0, 1.0) * obj.clamp(0.0, 1.0)).sqrt();
                if score < threshold || head.bbox.len() < idx * 4 + 4 || head.kps.len() < idx * 10 + 10 {
                    continue;
                }
                let b = &head.bbox[idx * 4..idx * 4 + 4];
                let cx = (c as f32 + b[0]) * s;
                let cy = (r as f32 + b[1]) * s;
                let bw = b[2].exp() * s;
                let bh = b[3].exp() * s;
                let k = &head.kps[idx * 10..idx * 10 + 10];
                let mut landmarks = [(0.0f32, 0.0f32); 5];
                for (n, l) in landmarks.iter_mut().enumerate() {
                    *l = (((c as f32 + k[n * 2]) * s / w).clamp(0.0, 1.0), ((r as f32 + k[n * 2 + 1]) * s / h).clamp(0.0, 1.0));
                }
                found.push(Face { x: ((cx - bw / 2.0) / w).clamp(0.0, 1.0), y: ((cy - bh / 2.0) / h).clamp(0.0, 1.0), w: (bw / w).min(1.0), h: (bh / h).min(1.0), score, landmarks });
            }
        }
    }
    nms(found, NMS_IOU)
}

fn iou(a: &Face, b: &Face) -> f32 {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w).min(b.x + b.w);
    let y1 = (a.y + a.h).min(b.y + b.h);
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let union = a.w * a.h + b.w * b.h - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// Keep the surest of every group of overlapping boxes.
pub fn nms(mut faces: Vec<Face>, iou_threshold: f32) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Face> = Vec::new();
    for f in faces {
        if kept.iter().all(|k| iou(k, &f) < iou_threshold) {
            kept.push(f);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One confident cell at stride 8 decodes to the box and landmarks
    /// OpenCV would give; a weaker overlapping one is suppressed.
    #[test]
    fn a_cell_decodes_as_opencv_does_and_overlaps_are_suppressed() {
        let (cols, rows) = (4usize, 4usize);
        let mut cls = vec![0.0f32; cols * rows];
        let mut obj = vec![0.0f32; cols * rows];
        let mut bbox = vec![0.0f32; cols * rows * 4];
        let mut kps = vec![0.0f32; cols * rows * 10];
        // Cell (c=2, r=1): centre (2.5*8, 1.5*8) = (20, 12); size e^ln(4)*8 = 32.
        let idx = 1 * cols + 2;
        cls[idx] = 1.0;
        obj[idx] = 0.81;
        bbox[idx * 4..idx * 4 + 4].copy_from_slice(&[0.5, 0.5, 4f32.ln(), 4f32.ln()]);
        for n in 0..5 {
            kps[idx * 10 + n * 2] = 0.5;
            kps[idx * 10 + n * 2 + 1] = 0.5;
        }
        // A weaker neighbour on the same face.
        let idx2 = 1 * cols + 1;
        cls[idx2] = 0.7;
        obj[idx2] = 0.7;
        bbox[idx2 * 4..idx2 * 4 + 4].copy_from_slice(&[1.4, 0.5, 4f32.ln(), 4f32.ln()]);
        let heads = [StrideHead { stride: 8, cols, rows, cls, obj, bbox, kps }];
        let faces = decode(&heads, 32.0, 32.0, 0.6);
        assert_eq!(faces.len(), 1, "{faces:?}");
        let f = &faces[0];
        assert!((f.score - 0.9).abs() < 1e-5);
        assert!((f.x - 4.0 / 32.0).abs() < 1e-5 && (f.y - 0.0).abs() < 1e-5, "{f:?}");
        assert!((f.w - 1.0).abs() < 1e-5 && (f.h - 1.0).abs() < 1e-5);
        assert!((f.landmarks[2].0 - 20.0 / 32.0).abs() < 1e-5);
    }
}

/// Without ONNX Runtime the detector is refused with a reason, rather
/// than the crate not building at all (#537).
#[cfg(all(test, not(feature = "ort")))]
mod without_ort {
    #[test]
    fn opening_the_detector_says_there_is_no_runtime() {
        let err = super::FaceDetector::open(std::path::Path::new("/nowhere/yunet.onnx")).err().expect("no detector without ONNX Runtime");
        assert!(err.contains("no ONNX Runtime"), "{err}");
    }
}

/// The face detector's session goes through the provider ladder, so it
/// runs on the GPU wherever the runtime has a provider for it (#2245).
#[cfg(all(test, feature = "ort"))]
mod on_the_gpu {
    #[test]
    fn the_detector_runs_on_the_gpu_the_runtime_offers() {
        let Ok(cache) = klipp_ml::ModelCache::open() else { return };
        let weights = cache.path_for_id("yunet_face_2023mar");
        if !weights.is_file() || crate::runtime_resolve::ensure_runtime_loadable().is_err() {
            eprintln!("skipped: no face model or no ONNX Runtime");
            return;
        }
        let mut faces = super::FaceDetector::open(&weights).expect("the detector opens");
        faces.detect(&[128; 64 * 36 * 4], 64, 36).expect("it runs");
        let used = crate::provider::used_by("yunet_face_2023mar").expect("its provider is recorded");
        let offered = crate::provider::gpu_provider();
        assert_eq!(used.provider, offered.unwrap_or("CPU"), "{}", used.line());
        if offered.is_none() {
            let why = used.why_cpu.as_deref().unwrap_or_default();
            assert!(why.contains("CUDA") && why.contains("WebGPU"), "both GPU providers are named: {why}");
        }
        // Set where the runtime is a GPU build (ORT_DYLIB_PATH).
        if std::env::var("KLIPP_TEST_REQUIRE_ORT_GPU").as_deref() == Ok("1") {
            assert!(offered.is_some(), "a GPU provider was required: {}", used.line());
        }
    }
}
