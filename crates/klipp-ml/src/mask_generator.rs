//! Trait abstraction for AI-driven mask generation.
//!
//! Klipp's AI Tools system runs multiple model families behind one
//! UI surface: SAM v1 / MobileSAM (click-to-mask), BiRefNet
//! (background removal), RF-DETR-Seg (auto-detect), future SAM2 +
//! XMem video propagation. Each ships as an `impl MaskGenerator`
//! block in a backend crate (`klipp-ml-candle`, `klipp-ml-ort`); the
//! UI never imports specific model code, only `Box<dyn MaskGenerator>`.
//!
//! ## Why this trait shape
//!
//! Pinned by the prior-art spike on 2026-04-28 (six parallel research
//! agents surveyed kalosm, mistral.rs, ort, candle's Module, burn,
//! llama-cpp-rs). Defenses against pitfalls observed:
//!
//! - **Multiple narrow traits** instead of one god-trait. mistral.rs's
//!   `Pipeline` accreted KV-cache + paged-attention into a single
//!   surface; ours stays small per category.
//! - **Sync API.** Mask generation is compute-bound, dispatched onto
//!   a worker thread by the UI (mirrors `model_download` pattern).
//!   Async would infect every caller for no real benefit.
//! - **Single concrete `MaskError`** (not associated type) — keeps
//!   `Box<dyn MaskGenerator>` ergonomic; one match site for UI.
//! - **`&mut self` in `generate`** — allows internal KV / feature
//!   caches across frames. Essential for the future SAM2 video mode;
//!   stateless models simply ignore it.
//! - **`MaskRequest` is a struct of options, not an enum** — adding
//!   `text_prompt` for a future Grounding DINO impl doesn't break
//!   existing impls.
//! - **No generic backend type parameter.** burn's `<B: Backend>`
//!   propagation makes simple call sites unergonomic; we go runtime-
//!   dynamic via `Box<dyn>`.
//! - **`MaskCapabilities` bitflags.** UI gates affordances without
//!   per-model conditionals; SAM has POINT|BOX|SCRIBBLE, RF-DETR has
//!   AUTO_DETECT, SAM2 will have POINT|BOX|TEMPORAL.

use std::path::PathBuf;

use bitflags::bitflags;

/// Linear-light RGBA frame, the input every model receives. Klipp's
/// existing `klipp-engine::LinearFrame` is the runtime instance; we
/// keep this type local to avoid a klipp-engine ↔ klipp-ml dependency
/// at the trait layer (klipp-ml is foundation, klipp-engine sits
/// above). Wiring code in `klipp-ui`'s ai_tools module does the
/// `&LinearFrame` → `&InputFrame` view conversion at the boundary.
#[derive(Debug, Clone)]
pub struct InputFrame<'a> {
    pub width: u32,
    pub height: u32,
    /// Interleaved RGBA, 4 floats per pixel, linear-light.
    pub linear_rgba: &'a [f32],
}

/// Single-channel alpha mask in image space. `width * height` bytes,
/// row-major, top-left origin (matches Klipp's existing image
/// orientation). 0 = fully transparent (not in mask), 255 = fully
/// opaque (in mask). Intermediate values are partial alpha.
#[derive(Debug, Clone)]
pub struct MaskBuffer {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

impl MaskBuffer {
    /// Build a mask of the given size with every pixel `value`.
    /// Useful for tests + the stub generator's circular mask.
    pub fn solid(width: u32, height: u32, value: u8) -> Self {
        Self {
            width,
            height,
            alpha: vec![value; (width as usize) * (height as usize)],
        }
    }

    /// Allocate a zero-filled buffer of the given size.
    pub fn empty(width: u32, height: u32) -> Self {
        Self::solid(width, height, 0)
    }

    /// Total pixel count (`width * height`). Convenience for callers
    /// that want to size buffers.
    pub fn pixel_count(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }

    /// Sample the alpha at integer pixel coordinates. Returns 0 for
    /// out-of-bounds — callers wanting a panic should bounds-check
    /// upstream.
    pub fn at(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let idx = (y as usize) * (self.width as usize) + (x as usize);
        self.alpha[idx]
    }
}

/// Axis-aligned bounding box in normalised `[0,1]` image coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Single positive / negative click prompt. Coordinates in
/// normalised `[0,1]` image space so the same prompt applies across
/// resolutions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClickPrompt {
    pub x: f32,
    pub y: f32,
    /// `true` = "include this region in the mask"; `false` = "exclude
    /// this region". SAM-family models support both; simpler models
    /// may treat all clicks as positive (capability flag advertises).
    pub positive: bool,
}

/// Box prompt: include everything inside this rectangle. Used by
/// SAM-family box mode + by RF-DETR's per-detection bbox.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxPrompt {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

/// Free-form scribble: a polyline of points the user dragged. SAM
/// supports this as a refinement on top of an initial click.
#[derive(Debug, Clone, PartialEq)]
pub struct ScribblePrompt {
    /// Points in normalised `[0,1]` image coordinates, ordered by
    /// stroke time.
    pub points: Vec<(f32, f32)>,
    /// Same semantics as `ClickPrompt::positive`.
    pub positive: bool,
}

/// Bundle of all prompts for one mask request. A struct (not an
/// enum) so adding fields later — `text: Option<String>` for
/// Grounding DINO, future sketch / point-cloud prompts — doesn't
/// break existing impls.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaskPrompts {
    pub clicks: Vec<ClickPrompt>,
    pub boxes: Vec<BoxPrompt>,
    pub scribbles: Vec<ScribblePrompt>,
    /// Text prompt for models that accept one (Grounding DINO,
    /// CLIP-Seg). `None` for the common case.
    pub text: Option<String>,
}

impl MaskPrompts {
    /// `true` when every prompt collection is empty AND there's no
    /// text — the "auto-detect" mode for models that support it
    /// (RF-DETR-Seg, YOLO-Seg).
    pub fn is_empty(&self) -> bool {
        self.clicks.is_empty()
            && self.boxes.is_empty()
            && self.scribbles.is_empty()
            && self.text.is_none()
    }
}

/// Optional temporal context for video-aware models. Most v1 models
/// ignore this; SAM2 video mode + XMem use it. A struct (not enum)
/// for the same forward-compat reason as `MaskPrompts`.
#[derive(Debug, Clone, Default)]
pub struct TemporalHint {
    /// Mask the user accepted on the previous frame. Lets video-aware
    /// models seed the next frame's prediction; static models ignore.
    pub prev_mask: Option<MaskBuffer>,
    /// Frame index in the timeline, useful for caching / diagnostics.
    pub frame_index: Option<u64>,
}

/// Per-call tuning knobs that override generator defaults. Every
/// field is `Option`: `None` means "use whatever the generator was
/// configured with at construction." Generators that don't recognise
/// a knob silently ignore it. The UI carries a per-model map and
/// hands the matching entry to the dispatcher each frame, so the
/// user's slider setting takes effect on the next run without
/// rebuilding the cached generator instance.
#[derive(Debug, Clone, Default)]
pub struct MaskTunables {
    /// Detection-confidence cut for instance-segmentation models
    /// (RF-DETR-Seg, YOLO-Seg). `[0.0, 1.0]` clamped on read.
    pub score_threshold: Option<f32>,
    /// IoU threshold for class-aware non-maximum-suppression in
    /// detection models. Set to `>= 1.0` to disable NMS entirely.
    pub nms_iou_threshold: Option<f32>,
}

/// One request to a `MaskGenerator`. Borrowing the input frame keeps
/// the call zero-copy from Klipp's existing `LinearFrame` (no extra
/// allocations on the per-frame path).
#[derive(Debug, Clone)]
pub struct MaskRequest<'a> {
    pub frame: InputFrame<'a>,
    pub prompts: MaskPrompts,
    pub temporal: Option<TemporalHint>,
    /// Per-call overrides on top of the generator's construction
    /// defaults. See [`MaskTunables`].
    pub tunables: MaskTunables,
}

/// One mask candidate. SAM returns up to 3 (the "small / medium /
/// large" interpretations of an ambiguous click); RF-DETR returns
/// N detections; BiRefNet returns 1 (no ambiguity).
#[derive(Debug, Clone)]
pub struct MaskCandidate {
    pub mask: MaskBuffer,
    /// Confidence in `[0,1]`. Some models output IoU predictions
    /// (SAM); others output detection score (RF-DETR / YOLO). Both
    /// expose as a `confidence` here for uniform UI.
    pub confidence: f32,
    /// Optional bounding box of the mask's coverage. Some models
    /// produce this directly (RF-DETR); for SAM-family we compute it
    /// from the mask post-hoc.
    pub bbox: Option<BBox>,
    /// Class label for instance-segmentation outputs ("person",
    /// "car"). `None` for SAM-family / matting models.
    pub label: Option<String>,
}

/// Full response — one or more candidates plus optional model-
/// specific diagnostics.
#[derive(Debug, Clone)]
pub struct MaskResponse {
    pub candidates: Vec<MaskCandidate>,
    /// Latency from `generate` entry to mask production, in
    /// milliseconds. Surfaces in UI for "this took 2.3s" feedback +
    /// in logs for perf monitoring.
    pub inference_ms: u64,
}

bitflags! {
    /// Capability flags advertised by a `MaskGenerator`. The UI uses
    /// these to gate affordances — don't show a "click" affordance
    /// for an auto-detect-only model. UI flow:
    ///
    /// 1. User picks a task (Remove Background / Isolate Object / Detect).
    /// 2. UI looks up registered models for that task.
    /// 3. UI reads `capabilities()` to decide which prompt
    ///    affordances to surface.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct MaskCapabilities: u32 {
        /// Accepts positive / negative click prompts (SAM family).
        const POINT_PROMPT = 1 << 0;
        /// Accepts a bounding-box prompt (SAM, RF-DETR's box outputs).
        const BOX_PROMPT = 1 << 1;
        /// Accepts polyline scribbles (SAM refinement).
        const SCRIBBLE = 1 << 2;
        /// Accepts a text prompt (Grounding DINO, CLIP-Seg — future).
        const TEXT_PROMPT = 1 << 3;
        /// Auto-detects without prompts (RF-DETR, YOLO-Seg) — call
        /// with `MaskPrompts::default()`.
        const AUTO_DETECT = 1 << 4;
        /// Uses `TemporalHint` to propagate masks across frames
        /// (SAM2 video, XMem, Cutie — future).
        const TEMPORAL = 1 << 5;
        /// Can process N frames in one call (cheaper than N
        /// individual calls). Currently unused; flagged so the
        /// trait is forward-compatible with batched VOS models.
        const BATCH = 1 << 6;
    }
}

/// Hint to the runtime about which device the model would prefer.
/// Backends honour as best-effort: a model that prefers CUDA falls
/// back to CPU if CUDA isn't available, after surfacing a one-time
/// diagnostic log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevicePreference {
    /// CPU is the right home (small models, CPU-optimised
    /// implementations, or where the user explicitly disabled GPU).
    Cpu,
    /// Prefer GPU if available, fall back to CPU. The default for
    /// most large models.
    GpuPreferCpu,
    /// Require GPU; fail explicitly if not available. Reserved for
    /// models that are unusably slow on CPU (large transformers).
    GpuRequired,
}

/// Errors a `MaskGenerator` can surface. Single concrete enum so
/// `Box<dyn MaskGenerator>` callers get one match site for UI error
/// display. Variants chosen to match the failure modes observed
/// across the candidate runtimes (candle, ort, tract).
#[derive(Debug, thiserror::Error)]
pub enum MaskError {
    #[error("model does not support prompt: {0}")]
    UnsupportedPrompt(&'static str),
    #[error("input frame shape mismatch: model expected {expected_w}×{expected_h}, got {actual_w}×{actual_h}")]
    ShapeMismatch {
        expected_w: u32,
        expected_h: u32,
        actual_w: u32,
        actual_h: u32,
    },
    #[error("out of memory: requested {requested_bytes} bytes")]
    OutOfMemory { requested_bytes: u64 },
    #[error("device unavailable: {0}")]
    DeviceUnavailable(&'static str),
    #[error("model weights not found at {0}; download via the model manager first")]
    WeightsNotFound(PathBuf),
    #[error("backend init: {0}")]
    BackendInit(String),
    #[error("inference failed: {0}")]
    Inference(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// The trait Klipp's UI talks to. Every model implementation in any
/// backend crate (`klipp-ml-candle`, `klipp-ml-ort`, future tract /
/// burn / etc.) implements this. Object-safe by design — held as
/// `Box<dyn MaskGenerator>` in the registry.
///
/// See the module-level docs for the design rationale.
pub trait MaskGenerator: Send + Sync {
    /// Stable identifier — must match the `id` field in
    /// `assets/models.toml`. Used by the registry to look up the
    /// implementation; users never see this directly.
    fn id(&self) -> &str;

    /// User-facing label, shown in the model picker UI. May change
    /// across versions without breaking saved projects.
    fn display_name(&self) -> &str;

    /// Capabilities the implementation supports. The UI gates
    /// affordances on these.
    fn capabilities(&self) -> MaskCapabilities;

    /// Hint to the runtime about preferred device.
    fn preferred_device(&self) -> DevicePreference;

    /// Run inference. `&mut self` permits internal caches (KV for
    /// SAM2 video, feature maps for click-to-refine workflows).
    fn generate(&mut self, req: MaskRequest<'_>) -> Result<MaskResponse, MaskError>;
}

// ─── Sister traits (forward-compat scaffolding) ──────────────────

bitflags! {
    /// Capabilities for video tracking models. Keeps the same flag
    /// numbering style as `MaskCapabilities` for consistency.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct TrackerCapabilities: u32 {
        /// Can track from a single annotated frame onwards.
        const FORWARD_PROPAGATION = 1 << 0;
        /// Can track backwards from a frame.
        const BACKWARD_PROPAGATION = 1 << 1;
        /// Maintains internal memory across frames (SAM2, XMem).
        const STATEFUL = 1 << 2;
    }
}

/// Future home for SAM2 video / XMem / Cutie / RVM. Not implemented
/// in v1 — sketch only, so the trait family is established before
/// Phase 4 (`F-97-VIDEOTRACK`) starts.
pub trait VideoTracker: Send + Sync {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    fn capabilities(&self) -> TrackerCapabilities;
    /// Track an initial mask across `frames`. Returns one mask per
    /// frame.
    fn track(
        &mut self,
        initial_mask: &MaskBuffer,
        frames: &[InputFrame<'_>],
    ) -> Result<Vec<MaskBuffer>, MaskError>;
}

/// Future home for object-detection models that don't produce masks
/// (raw YOLO-Detect, DETR). `MaskGenerator` covers seg variants;
/// this trait covers detect-only. Phase 5+.
pub trait ObjectDetector: Send + Sync {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    fn detect(&mut self, frame: InputFrame<'_>) -> Result<Vec<Detection>, MaskError>;
}

/// Single detection (no mask). Used by `ObjectDetector` trait.
#[derive(Debug, Clone)]
pub struct Detection {
    pub bbox: BBox,
    pub confidence: f32,
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trait must be object-safe so we can hold
    /// `Box<dyn MaskGenerator>` in the registry. If a future change
    /// adds a non-object-safe method (generic, `where Self: Sized`,
    /// etc.) this fails at compile time.
    #[test]
    fn mask_generator_is_object_safe() {
        fn assert_object_safe(_: &dyn MaskGenerator) {}
        // Does not need to actually call it; just compiling proves
        // the trait is dyn-compatible.
        let _check: fn(&dyn MaskGenerator) = assert_object_safe;
    }

    #[test]
    fn video_tracker_is_object_safe() {
        fn assert_object_safe(_: &dyn VideoTracker) {}
        let _check: fn(&dyn VideoTracker) = assert_object_safe;
    }

    #[test]
    fn object_detector_is_object_safe() {
        fn assert_object_safe(_: &dyn ObjectDetector) {}
        let _check: fn(&dyn ObjectDetector) = assert_object_safe;
    }

    #[test]
    fn capability_flags_compose_with_or() {
        let sam = MaskCapabilities::POINT_PROMPT
            | MaskCapabilities::BOX_PROMPT
            | MaskCapabilities::SCRIBBLE;
        assert!(sam.contains(MaskCapabilities::POINT_PROMPT));
        assert!(sam.contains(MaskCapabilities::BOX_PROMPT));
        assert!(!sam.contains(MaskCapabilities::AUTO_DETECT));
    }

    #[test]
    fn mask_prompts_default_is_empty() {
        // Auto-detect mode is signaled by `MaskPrompts::default()`
        // having `is_empty() == true`. This test pins that contract
        // so a future field addition (e.g. a default-non-None Option
        // somewhere) doesn't silently break auto-detect dispatch.
        let p = MaskPrompts::default();
        assert!(p.is_empty());
    }

    #[test]
    fn mask_prompts_with_text_is_not_empty() {
        let p = MaskPrompts {
            text: Some("a person".into()),
            ..Default::default()
        };
        assert!(!p.is_empty());
    }

    #[test]
    fn mask_buffer_solid_fill_matches_pixel_count() {
        let m = MaskBuffer::solid(4, 3, 128);
        assert_eq!(m.alpha.len(), 12);
        assert!(m.alpha.iter().all(|&v| v == 128));
        assert_eq!(m.pixel_count(), 12);
    }

    #[test]
    fn mask_buffer_at_returns_zero_for_oob() {
        let m = MaskBuffer::solid(2, 2, 200);
        assert_eq!(m.at(0, 0), 200);
        assert_eq!(m.at(1, 1), 200);
        assert_eq!(m.at(2, 0), 0); // x out of bounds
        assert_eq!(m.at(0, 2), 0); // y out of bounds
        assert_eq!(m.at(99, 99), 0);
    }

    #[test]
    fn mask_error_display_strings_user_friendly() {
        // Quick smoke test that error variants render readable
        // messages — these surface directly in the UI.
        let e = MaskError::UnsupportedPrompt("text");
        assert!(format!("{e}").contains("text"));
        let e = MaskError::ShapeMismatch {
            expected_w: 1024,
            expected_h: 1024,
            actual_w: 512,
            actual_h: 512,
        };
        let s = format!("{e}");
        assert!(s.contains("1024"));
        assert!(s.contains("512"));
    }
}
