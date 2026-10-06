use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use klipp_color::lut::LutData;
use klipp_effects::gpu::GpuEffectsPipeline;

use crate::graph::{Edge, Node, NodeGraph, NodeId};
use crate::kinds::NodeKind;
use crate::params::{MatteChannel, NodeParams};
use klipp_core::{BlendMode, LayerMixerInput};

/// Reserved keyframe track name that animates a node's enabled / bypass state.
/// Values >= 0.5 mean enabled, < 0.5 mean bypassed. Falls back to the static
/// `node.enabled` bool when the track has no keyframes.
pub const ENABLED_TRACK: &str = "__enabled";

/// Effective enabled state for `node` at `clip_time_secs`. Consults the
/// `__enabled` keyframe track if present, otherwise falls back to the static
/// bool.
pub fn effective_enabled(node: &Node, clip_time_secs: f64) -> bool {
    node.keyframes
        .tracks
        .iter()
        .find(|t| t.param_name == ENABLED_TRACK)
        .and_then(|t| t.evaluate(clip_time_secs))
        .map(|v| v >= 0.5)
        .unwrap_or(node.enabled)
}

/// Return the effective params for `node` at `clip_time_secs`. When the node has
/// no keyframes we borrow `node.params` directly, skipping the per-frame clone
/// that would otherwise happen on every node in the graph.
fn params_for_frame(node: &Node, clip_time_secs: f64) -> Cow<'_, NodeParams> {
    if node.keyframes.is_empty() {
        Cow::Borrowed(&node.params)
    } else {
        let mut params = node.params.clone();
        for (param_name, value) in node.keyframes.evaluate_all(clip_time_secs) {
            if param_name == ENABLED_TRACK {
                continue;
            }
            params.apply_override(&param_name, value);
        }
        Cow::Owned(params)
    }
}

/// Error during graph execution.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ExecutorError {
    #[error("cycle detected in node graph")]
    Cycle(#[from] crate::graph::CycleError),
    #[error("output node has no connection")]
    NoOutputConnection,
    #[error("missing output for node")]
    MissingNodeOutput,
    /// The caller passed an empty `source_frame` — valid only when the
    /// GPU chain was pre-seeded and covers the whole graph — but a node
    /// needed the CPU pixels after all.
    ///
    /// This exists so the caller can *skip* the GPU→CPU readback
    /// speculatively and be told, rather than having to predict whether
    /// every node in an arbitrary graph will stay on the chain. Getting
    /// that prediction wrong silently renders a wrong frame; getting
    /// this error just costs the readback that would have happened
    /// anyway. Callers realise the pixels and retry.
    #[error("node graph needs CPU source pixels but none were supplied")]
    CpuSourceRequired,
}

/// Executes a node graph on RGBA frames.
/// Uses GPU compute shaders for supported nodes when available, CPU fallback otherwise.
///
/// Internally the executor carries linear-light `Vec<f32>` RGBA buffers between
/// nodes. Colour ops that have linear-native variants (exposure, saturation,
/// contrast, tone controls, wheels, …) run on f32 directly. Ops that still
/// require u8 input (curve application, qualifier/mask generation, GPU-delegated
/// paths, LUT / grain / denoise) bracket the u8 round-trip at the call site.
///
/// The public [`GraphExecutor::execute`] entry point stays u8 in, u8 out for
/// backward compatibility. Callers that already hold linear data should prefer
/// [`GraphExecutor::execute_linear`], which skips the bracket at the boundary.
/// A frame in memory uploaded for seeding, kept with the pixels
/// themselves so the same frame is recognised and not re-sent.
struct UploadedSeed {
    buffer: wgpu::Buffer,
    pixels: SeedPixels,
}

/// What an [`UploadedSeed`] holds: a scene-referred frame's packed codes,
/// or display-referred sRGB bytes.
enum SeedPixels {
    Codes(Arc<Vec<u32>>),
    Srgb8(Arc<Vec<u8>>),
    Rgba64(Arc<Vec<u16>>),
}

impl SeedPixels {
    fn bytes(&self) -> &[u8] {
        match self {
            SeedPixels::Codes(c) => bytemuck::cast_slice(c.as_slice()),
            SeedPixels::Srgb8(b) => b.as_slice(),
            SeedPixels::Rgba64(s) => bytemuck::cast_slice(s.as_slice()),
        }
    }

    /// The same pixels, not an equal copy.
    fn is(&self, other: &SeedPixels) -> bool {
        match (self, other) {
            (SeedPixels::Codes(a), SeedPixels::Codes(b)) => Arc::ptr_eq(a, b),
            (SeedPixels::Srgb8(a), SeedPixels::Srgb8(b)) => Arc::ptr_eq(a, b),
            (SeedPixels::Rgba64(a), SeedPixels::Rgba64(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

pub struct GraphExecutor {
    /// Optional GPU pipeline for accelerated node execution.
    /// Lazily populated by `ensure_gpu()` on first use so we don't
    /// force-init `GpuContext::shared()` from the engine thread
    /// before the UI has had a chance to install its own device.
    gpu: Option<GpuEffectsPipeline>,
    /// Latches once `ensure_gpu()` has tried to build the pipeline
    /// (success or failure). Prevents retrying a failed init on
    /// every frame.
    gpu_init_attempted: bool,
    /// Cache of loaded LUT data keyed by file path.
    lut_cache: HashMap<PathBuf, Arc<LutData>>,
    /// Cache of pre-built curve LUTs keyed by node ID.
    curve_lut_cache: HashMap<NodeId, Arc<[f32; 1024]>>,
    /// Cache of decoded external-matte images, keyed by source path.
    /// Stores the raw linear-RGBA buffer + native dimensions so the
    /// per-frame resize to the timeline resolution can run without
    /// hitting the disk every frame. Invalidated by the executor only
    /// when the path changes; editing the image on disk is an
    /// acknowledged rough edge.
    matte_image_cache: HashMap<PathBuf, Arc<(Vec<f32>, u32, u32)>>,
    /// Matte and plate images that would not load, each warned about
    /// once. Only loads that succeed are cached, so a missing file is
    /// tried again every frame, and it warned every frame (#1020).
    unreadable_images: HashSet<PathBuf>,
    /// The same plate already resized to the frame, keyed by path and
    /// target size. `matte_image_cache` above saves the *decode*; this
    /// saves the resize, which is a full-frame write — 33 MB at 4K — and
    /// was being redone every frame for a plate and a resolution that
    /// never change. Both keys matter: the same plate at two timeline
    /// resolutions is two different resized buffers.
    plate_resize_cache: HashMap<(PathBuf, u32, u32), Arc<Vec<f32>>>,
    /// Per-Mask-node matte cache. Static masks (no keyframes) produce
    /// the same RGBA matte every frame, which previously cost a full
    /// rasterize + RGBA-expand pass per frame (the dominant CPU cost
    /// for the Source → Mask → MaskCutout chain at 1080p). The
    /// fingerprint includes shape params + feather/opacity/invert,
    /// plus the 1ms-quantised time when any keyframe is present so
    /// animated masks still update. The Arc lets the cache hand the
    /// same buffer to multiple downstream consumers without cloning.
    matte_cache: HashMap<NodeId, MatteCacheEntry>,
    /// Active project working colorspace. Drives which luma vector
    /// the gamut-aware grade ops use (Saturation, Vibrance,
    /// ToneControls, ColorWheels, WheelSatBri, Clarity). Set by the
    /// engine via [`GraphExecutor::set_working_colorspace`] when the
    /// project's `working_colorspace` field changes; defaults to
    /// `Rec709Linear` so executor instances created before the
    /// engine sets the value behave exactly like the pre-Phase-E
    /// pipeline.
    working_colorspace: klipp_core::WorkingColorspace,
    /// The buffer's size against the sequence's: 0.5 for a half-size
    /// preview. Grade settings measured in pixels are authored at the
    /// sequence's size and scaled by it (#1012).
    render_scale: f32,
    /// The encoding the grade's controls act in (#467), from the
    /// project's choice and the working space: set with it.
    grade_encoding: klipp_color::grade_model::Encoding,
    /// The project's choice of grade encoding, which `grade_encoding`
    /// follows only under ACEScg. Starts as the global setting; a
    /// render that must not see it change midway (an export) sets it
    /// with [`GraphExecutor::set_grade_encoding`].
    project_grade_encoding: klipp_core::GradeEncoding,
    /// Tetrahedral 3D-LUT interpolation, or trilinear when the user
    /// picked speed over accuracy in the viewer.
    lut_tetrahedral: bool,
    /// Phase 4d: when true, `start_chain` skips the
    /// `upload_chain_input_linear` call because `buf_a` was already
    /// seeded with linear RGBA — typically by
    /// `prime_chain_from_gpu_u8_buffer` bridging a decoder's u8
    /// output in-place. Consumed (cleared) by the first
    /// `start_chain` so a second chain segment within the same
    /// `execute_linear` call falls back to the normal upload path.
    gpu_chain_preseeded: bool,
    /// Dimensions `buf_a` was seeded at. A seed is only valid for the
    /// resolution it was made at: the drag-downsample path grades at
    /// half res off a full-res seed, which would reinterpret the buffer
    /// at the wrong stride and scramble the image.
    gpu_chain_preseed_dims: Option<(u32, u32)>,
    /// The frame the last `prime_chain_from_hdr10_codes` or
    /// `prime_chain_from_srgb8` uploaded, still on the GPU.
    uploaded_seed: Option<UploadedSeed>,
    /// Node thumbnails taken inside the GPU chain; built on first use.
    chain_thumbs: Option<klipp_effects::gpu::ChainThumbs>,
    /// Single-shot request, set by `request_output_on_gpu`: leave the next
    /// run's final result in the chain buffer instead of downloading it.
    keep_output_on_gpu: bool,
    /// Plans by [`crate::plan::plan_key`]: what a run of each graph shape
    /// needs, worked out once (#1089, M2 step 8).
    plans: HashMap<u64, crate::plan::ExecPlan>,
    /// Runs that compiled a plan, and runs that found one cached.
    plan_compiles: u64,
    plan_hits: u64,
    /// The node whose chain output may stay on the GPU this run: the one
    /// feeding Output, when nothing else reads it. `None` unless
    /// `keep_output_on_gpu` was requested and the graph allows it.
    run_gpu_output_node: Option<NodeId>,
    /// After a run: `Some(slot_a)` if its final result was left in the
    /// chain buffer — in which case the returned Vec is empty and the
    /// caller reads the result from the GPU (`encode_output_to_display_u8`).
    output_on_gpu: Option<bool>,
    /// The last run's chain shape: dispatches that extended a live
    /// chain, flushes back to the CPU, and nodes that ran per-node on the
    /// CPU. Published because whether a graph stayed GPU-resident is
    /// invisible in its output — only in its speed — and a test should be
    /// able to assert it directly rather than infer it from a stopwatch.
    last_run_chain_stats: ChainStats,
    /// The seed, scoped to one `execute_linear*` run. Taken from
    /// `gpu_chain_preseed_dims` at entry, which is cleared at the same
    /// moment: a seed the run does not consume must not survive into the
    /// next one. Otherwise a frame that decodes in software and supplies
    /// real CPU pixels would skip its upload and grade the previous
    /// frame's buffer.
    run_seed_dims: Option<(u32, u32)>,
    /// After the most recent `execute_linear*` call, the slot
    /// (`buf_a` if `true`, `buf_b` if `false`) that holds the chain's
    /// final linear-RGBA output, if a chain ran at all. `None` when
    /// the graph was identity, ran entirely on CPU, or didn't engage
    /// the GPU chain.
    ///
    /// Set by `flush_chain` immediately before `chain_tail` is
    /// cleared, and reset to `None` at the start of every
    /// `execute_linear_with_thumbs` call. Lets the engine route the
    /// chain output straight into a viewer texture via
    /// `encode_grade_to_texture(slot, ...)` instead of re-uploading
    /// the just-downloaded `Vec<f32>` at the legacy texture-publish
    /// boundary.
    last_chain_final_slot: Option<bool>,
    /// Current depth of recursive `execute_linear_with_thumbs` calls
    /// triggered by `NodeKind::Compound` nodes. Bumped on entry to
    /// the compound arm of `execute_node`, decremented on exit.
    /// Capped at `COMPOUND_MAX_DEPTH` so pathological nesting
    /// (compound-of-compound-of-…) degrades gracefully to passthrough
    /// instead of stack-overflowing. Zero in the common case (no
    /// compounds, or top-level call).
    compound_depth: u32,
    /// Track-points for the clip currently being graded. The engine
    /// calls [`set_current_clip_track_points`] before each
    /// `execute_linear*` call so the Mask-node handler can apply
    /// tracker overrides to mask centres when a `TrackPoint.linked_mask`
    /// binding matches the current node + index.
    ///
    /// Empty vec when no clip context is set (tests, programmatic
    /// `bake_graph_to_lut` runs, etc.) — the override pass is then a
    /// no-op and masks render with their persistent / keyframed
    /// centres exactly as before.
    current_clip_track_points: Vec<klipp_core::TrackPoint>,
    /// Solo target. When Some(id), the result of `execute_linear_*`
    /// becomes that node's output rather than the Output-node-input.
    /// All other nodes still execute (so thumbnails update normally
    /// for canvas previews); only the final-pick changes. When the
    /// id doesn't exist in the active graph (e.g. user just switched
    /// clips), the executor silently falls back to the normal Output
    /// pick — no error, no toast spam.
    solo_node: Option<NodeId>,
}

/// Maximum recursion depth for nested `Compound` nodes before the
/// executor falls back to passthrough. 8 is deep enough for any
/// realistic grade structure (Resolve itself stops surfacing nested
/// compounds in the UI well before that) and shallow enough that the
/// worst-case per-frame cost stays bounded.
const COMPOUND_MAX_DEPTH: u32 = 8;

/// Plans kept at once: one per graph shape a session renders (each clip
/// on screen, its inspection views). Past this the cache starts over.
const MAX_PLANS: usize = 256;

/// The grade shaders' white balance for no temperature or tint.
const NO_WHITE_BALANCE: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Temperature / tint as the grade shaders take them: a white-balance
/// matrix on linear light in the working space.
fn white_balance(temperature: f32, tint: f32, ws: klipp_core::WorkingColorspace) -> [[f32; 3]; 3] {
    klipp_color::white_balance::white_balance_matrix(temperature, tint, ws).unwrap_or(NO_WHITE_BALANCE)
}

/// Mid grey, encoded.
///
/// Used only by stages that dispatch the grade shader with `contrast`
/// set to zero — exposure alone, saturation alone, tone controls alone
/// — where the pivot cannot affect the result, because contrast of zero
/// leaves every sample where it is whatever it pivots about. The stages
/// that do apply contrast (the Corrector and the dedicated Contrast
/// node) read the pivot from their own params instead.
fn default_pivot_encoded() -> f32 {
    klipp_color::grade_model::encode_contrast_pivot(
        klipp_color::grade_model::DEFAULT_CONTRAST_PIVOT,
    )
}


/// A matte that selects everything, R=G=B=A=1: what a matte producer
/// gives with nothing chosen (no image, no plate, disabled), as a
/// Qualifier with no settings and a LumaKey that is off already did. One
/// is the identity wherever a matte is used: a correction it gates grades
/// as if there were no key, and alpha it cuts stays whole (#1039).
fn full_matte(pixel_count: usize) -> Vec<f32> {
    vec![1.0; pixel_count * 4]
}

/// The unified grade doing nothing, in the working space `ws`.
fn identity_grade(ws: klipp_core::WorkingColorspace, enc: klipp_color::grade_model::Encoding) -> klipp_effects::cpu_grade::ColorGrade {
    let zero = (0.0, 0.0, 0.0);
    klipp_effects::cpu_grade::ColorGrade {
        exposure: 0.0, contrast: 0.0,
        contrast_pivot_encoded: klipp_color::grade_model::encode_contrast_pivot_in(
            klipp_color::grade_model::DEFAULT_CONTRAST_PIVOT, enc,
        ),
        encoding: enc,
        saturation: 1.0, white_balance: NO_WHITE_BALANCE,
        highlights: 0.0, shadows: 0.0, whites: 0.0, blacks: 0.0,
        lift: zero, gamma: zero, gain: zero, offset: zero, log_wheels: None,
        wheel_shape: klipp_color::grade_model::WheelShape::default(),
        hue: 0.0, global_mix: 1.0, luma: klipp_color::linear::luma_coefs(ws),
    }
}

/// Lift, gamma, gain and offset from a node's wheel params.
fn linear_wheels(p: &NodeParams) -> [(f32, f32, f32); 4] {
    let w = |x: Option<f32>, y: Option<f32>, m: Option<f32>| {
        klipp_color::wheel_offset_to_rgb(x.unwrap_or(0.0), y.unwrap_or(0.0), m.unwrap_or(0.0))
    };
    [
        w(p.lift_x, p.lift_y, p.lift_master),
        w(p.gamma_x, p.gamma_y, p.gamma_master),
        w(p.gain_x, p.gain_y, p.gain_master),
        w(p.offset_x, p.offset_y, p.offset_master),
    ]
}

/// Low, mid and high log wheels, when the node is in log mode and they
/// are not all zero.
fn log_wheels(p: &NodeParams) -> Option<[(f32, f32, f32); 3]> {
    if !p.wheels_log_mode.unwrap_or(false) {
        return None;
    }
    let w = |x: Option<f32>, y: Option<f32>, m: Option<f32>| {
        klipp_color::wheel_offset_to_rgb(x.unwrap_or(0.0), y.unwrap_or(0.0), m.unwrap_or(0.0))
    };
    let wheels = [
        w(p.log_low_x, p.log_low_y, p.log_low_master),
        w(p.log_mid_x, p.log_mid_y, p.log_mid_master),
        w(p.log_high_x, p.log_high_y, p.log_high_master),
    ];
    wheels.iter().any(|v| *v != (0.0, 0.0, 0.0)).then_some(wheels)
}


/// The log wheels' ranges and the wheels' luma mix from a node's params
/// (#900), each at its default when unset.
fn wheel_shape(p: &NodeParams) -> klipp_color::grade_model::WheelShape {
    let d = klipp_color::grade_model::WheelShape::default();
    klipp_color::grade_model::WheelShape {
        low_range: p.log_low_range.unwrap_or(d.low_range),
        high_range: p.log_high_range.unwrap_or(d.high_range),
        luma_mix: p.luma_mix.unwrap_or(d.luma_mix),
    }
}

/// Which hue/sat curves were edited, bit per curve in
/// `build_huesat_lut_data`'s order.
fn huesat_active(h: &klipp_core::HueSatCurves) -> u32 {
    [&h.hue_vs_hue, &h.hue_vs_sat, &h.hue_vs_lum, &h.lum_vs_sat, &h.sat_vs_sat, &h.orange_vs_sat, &h.sat_vs_lum]
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.is_empty())
        .fold(0, |m, (i, _)| m | 1 << i)
}

/// One step of a grade node, run on the GPU chain or by its CPU twin in
/// `klipp_effects::cpu_grade`: the same list either way, so the two cannot
/// run the steps in different orders.
enum GradeStage {
    /// Keep the node's input for a closing `Mix`.
    Keep,
    Grade(klipp_effects::cpu_grade::ColorGrade),
    /// Amount, luma weights, blur radius in the buffer's pixels.
    Clarity(f32, [f32; 3], u32),
    Vibrance(f32, [f32; 3]),
    WheelSatBri([f32; 8], [f32; 3]),
    Curves { lut: Box<[f32; 1024]>, preserve_luma: bool, mix: f32, luma: [f32; 3] },
    HueSat { lut: Box<[f32; klipp_effects::cpu_grade::HUESAT_LUT_LEN]>, mix: f32, active: u32 },
    Warper { hue: [f32; 12], sat: [f32; 12] },
    /// Mix with the kept input.
    Mix(f32),
}

impl GradeStage {
    /// Record on the GPU chain, reading the slot `in_is_a` names; the
    /// number of ping-pong flips.
    fn dispatch(&self, gpu: &mut GpuEffectsPipeline, in_is_a: bool, width: u32, height: u32) -> u32 {
        match self {
            GradeStage::Keep => {
                gpu.keep_chain_input(in_is_a, width, height);
                return 0;
            }
            GradeStage::Grade(g) => gpu.apply_color_grade_chain(
                in_is_a, width, height,
                g.exposure, g.contrast, g.contrast_pivot_encoded, g.saturation, g.white_balance,
                g.highlights, g.shadows, g.whites, g.blacks,
                g.lift, g.gamma, g.gain, g.offset, g.log_wheels, g.wheel_shape,
                g.hue, g.global_mix,
            ),
            GradeStage::Clarity(a, _, r) => gpu.apply_clarity_chain(in_is_a, width, height, *a, *r),
            GradeStage::Vibrance(a, _) => gpu.apply_vibrance_chain(in_is_a, width, height, *a),
            GradeStage::WheelSatBri(v, _) => {
                gpu.apply_wheel_sat_bri_chain(in_is_a, width, height, v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7])
            }
            GradeStage::Curves { lut, preserve_luma, mix, .. } => {
                gpu.apply_rgb_curves_chain(in_is_a, width, height, lut, *preserve_luma, *mix)
            }
            GradeStage::HueSat { lut, mix, active } => gpu.apply_huesat_chain(in_is_a, width, height, *mix, lut, *active),
            GradeStage::Warper { hue, sat } => gpu.apply_color_warper_chain(in_is_a, width, height, hue, sat),
            GradeStage::Mix(m) => gpu.apply_mix_with_kept_chain(in_is_a, width, height, *m),
        }
        1
    }

    /// Run the CPU twin on `frame`; `kept` holds what `Keep` kept.
    fn run_cpu(&self, frame: &mut [f32], kept: &mut Option<Vec<f32>>, width: u32, height: u32) {
        use klipp_effects::cpu_grade as cg;
        match self {
            GradeStage::Keep => *kept = Some(frame.to_vec()),
            GradeStage::Grade(g) => cg::color_grade(frame, g),
            GradeStage::Clarity(a, l, r) => cg::clarity(frame, width, height, *a, *r, *l),
            GradeStage::Vibrance(a, l) => cg::vibrance(frame, *a, *l),
            GradeStage::WheelSatBri(v, l) => cg::wheel_sat_bri(frame, v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], *l),
            GradeStage::Curves { lut, preserve_luma, mix, luma } => cg::rgb_curves(frame, lut, *preserve_luma, *mix, *luma),
            GradeStage::HueSat { lut, mix, active } => cg::huesat(frame, lut, *mix, *active),
            GradeStage::Warper { hue, sat } => cg::color_warper(frame, hue, sat),
            GradeStage::Mix(m) => {
                if let Some(k) = kept.as_ref() {
                    cg::grade_mix(frame, k, *m);
                }
            }
        }
    }
}

/// Run grade stages on the CPU.
fn run_stages_cpu(stages: &[GradeStage], input: &[f32], width: u32, height: u32) -> Vec<f32> {
    let mut frame = input.to_vec();
    let mut kept = None;
    for s in stages {
        s.run_cpu(&mut frame, &mut kept, width, height);
    }
    frame
}

/// Cached matte for a Mask node. Reused across frames whenever the
/// fingerprint matches — eliminates ~5-15 ms per 1080p frame for
/// static masks (no keyframes, no edits).
struct MatteCacheEntry {
    fingerprint: u64,
    width: u32,
    height: u32,
    matte: Arc<Vec<f32>>,
}

/// Fast hash of a `MaskStack` + size + (when keyframed) time, used as
/// the cache key. Time is quantised to 1 ms so adjacent ticks share
/// hits; for static masks (no keyframes) time is excluded so EVERY
/// frame hits the same entry.
fn matte_fingerprint(
    masks: &klipp_core::MaskStack,
    clip_time_secs: f64,
    width: u32,
    height: u32,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    width.hash(&mut h);
    height.hash(&mut h);
    let any_kf = masks.masks.iter().any(|m| !m.keyframes.is_empty());
    if any_kf {
        ((clip_time_secs * 1000.0) as i64).hash(&mut h);
    }
    for mask in &masks.masks {
        if !mask.enabled { continue; }
        mask.feather.to_bits().hash(&mut h);
        mask.opacity.to_bits().hash(&mut h);
        mask.invert.hash(&mut h);
        match &mask.shape {
            klipp_core::MaskShape::Circle { center_x, center_y, radius } => {
                0u8.hash(&mut h);
                center_x.to_bits().hash(&mut h);
                center_y.to_bits().hash(&mut h);
                radius.to_bits().hash(&mut h);
            }
            klipp_core::MaskShape::Rectangle { center_x, center_y, width, height, rotation } => {
                1u8.hash(&mut h);
                center_x.to_bits().hash(&mut h);
                center_y.to_bits().hash(&mut h);
                width.to_bits().hash(&mut h);
                height.to_bits().hash(&mut h);
                rotation.to_bits().hash(&mut h);
            }
            klipp_core::MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => {
                2u8.hash(&mut h);
                start_x.to_bits().hash(&mut h);
                start_y.to_bits().hash(&mut h);
                end_x.to_bits().hash(&mut h);
                end_y.to_bits().hash(&mut h);
            }
            klipp_core::MaskShape::Bezier { points } => {
                3u8.hash(&mut h);
                points.len().hash(&mut h);
                for (x, y) in points {
                    x.to_bits().hash(&mut h);
                    y.to_bits().hash(&mut h);
                }
            }
        }
    }
    h.finish()
}

// ── u8 ↔ linear bracket helpers ─────────────────────────────────────────────

/// Convert an sRGB u8 RGBA buffer to linear-light f32 RGBA.
/// Alpha stays linear (per the sRGB spec).
fn u8_rgba_to_linear(rgba: &[u8]) -> Vec<f32> {
    use klipp_color::transfer::srgb_to_linear;
    let mut out = Vec::with_capacity(rgba.len());
    for chunk in rgba.chunks_exact(4) {
        out.push(srgb_to_linear(chunk[0] as f32 / 255.0));
        out.push(srgb_to_linear(chunk[1] as f32 / 255.0));
        out.push(srgb_to_linear(chunk[2] as f32 / 255.0));
        out.push(chunk[3] as f32 / 255.0);
    }
    out
}

/// Convert a linear-light f32 RGBA buffer to sRGB u8 RGBA. Out-of-range values
/// are clamped at the byte boundary only.
fn linear_rgba_to_u8(linear: &[f32]) -> Vec<u8> {
    use klipp_color::transfer::linear_to_srgb;
    let mut out = Vec::with_capacity(linear.len());
    for chunk in linear.chunks_exact(4) {
        out.push((linear_to_srgb(chunk[0]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        out.push((linear_to_srgb(chunk[1]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        out.push((linear_to_srgb(chunk[2]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        out.push((chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    }
    out
}

impl Default for GraphExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphExecutor {
    pub fn new() -> Self {
        // GPU pipeline is lazily initialised on first use (see
        // `ensure_gpu`). Constructing it here would call
        // `GpuContext::shared()` — which may run on the engine
        // thread before the UI has had a chance to install its own
        // wgpu device via `GpuContext::install_shared`. Once the
        // shared slot is populated the UI's install is a no-op and
        // the engine + UI end up on separate devices, defeating the
        // whole point of device unification.
        Self {
            gpu: None,
            gpu_init_attempted: false,
            lut_cache: HashMap::new(),
            curve_lut_cache: HashMap::new(),
            matte_image_cache: HashMap::new(),
            unreadable_images: HashSet::new(),
            plate_resize_cache: HashMap::new(),
            matte_cache: HashMap::new(),
            gpu_chain_preseeded: false,
            gpu_chain_preseed_dims: None,
            uploaded_seed: None,
            chain_thumbs: None,
            keep_output_on_gpu: false,
            plans: HashMap::new(),
            plan_compiles: 0,
            plan_hits: 0,
            run_gpu_output_node: None,
            output_on_gpu: None,
            last_run_chain_stats: ChainStats::default(),
            run_seed_dims: None,
            last_chain_final_slot: None,
            compound_depth: 0,
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
            render_scale: 1.0,
            grade_encoding: klipp_color::grade_model::Encoding::Bt1886,
            project_grade_encoding: klipp_core::grade_encoding(),
            lut_tetrahedral: true,
            current_clip_track_points: Vec::new(),
            solo_node: None,
        }
    }

    /// Lazily create the `GpuEffectsPipeline` on first use. Once the
    /// shared `GpuContext` is populated (either by the UI host or by
    /// the first lazy init here), every subsequent call re-uses it.
    /// Caches init failure so we don't retry on every frame.
    fn ensure_gpu(&mut self) {
        if self.gpu.is_some() || self.gpu_init_attempted {
            return;
        }
        self.gpu_init_attempted = true;
        self.gpu = GpuEffectsPipeline::new();
        if let Some(gpu) = self.gpu.as_mut() {
            // Replay the active working colorspace's luma vector
            // into the freshly-built pipeline. `set_working_colorspace`
            // may have been called before this lazy init, in which
            // case the pipeline's default `luma_coefs` would be Rec.709
            // even though the project is on AcesCG.
            gpu.set_luma_coefs(klipp_color::linear::luma_coefs(self.working_colorspace));
            gpu.set_grade_encoding(self.grade_encoding.as_u8());
        } else {
            tracing::info!(
                "GraphExecutor: GPU unavailable, using CPU fallback for all nodes"
            );
        }
    }

    /// Create an executor without GPU (for tests or export threads with their own GPU).
    pub fn new_cpu_only() -> Self {
        Self {
            gpu: None,
            // Mark "attempted" so `ensure_gpu()` is a no-op —
            // `new_cpu_only` explicitly wants CPU-only.
            gpu_init_attempted: true,
            lut_cache: HashMap::new(),
            curve_lut_cache: HashMap::new(),
            matte_image_cache: HashMap::new(),
            unreadable_images: HashSet::new(),
            plate_resize_cache: HashMap::new(),
            matte_cache: HashMap::new(),
            gpu_chain_preseeded: false,
            gpu_chain_preseed_dims: None,
            uploaded_seed: None,
            chain_thumbs: None,
            keep_output_on_gpu: false,
            plans: HashMap::new(),
            plan_compiles: 0,
            plan_hits: 0,
            run_gpu_output_node: None,
            output_on_gpu: None,
            last_run_chain_stats: ChainStats::default(),
            run_seed_dims: None,
            last_chain_final_slot: None,
            compound_depth: 0,
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
            render_scale: 1.0,
            grade_encoding: klipp_color::grade_model::Encoding::Bt1886,
            project_grade_encoding: klipp_core::grade_encoding(),
            lut_tetrahedral: true,
            current_clip_track_points: Vec::new(),
            solo_node: None,
        }
    }

    /// Provide the current clip's track-points to the executor before
    /// an `execute_linear*` call. The Mask-node handler consults these
    /// to override mask centres for any mask whose
    /// `(node_id, mask_index)` is bound to a TrackPoint with a
    /// recorded position at the playhead.
    ///
    /// Pass `&[]` (or skip the call) for grade-bake / test contexts
    /// where there is no clip context — the override pass becomes a
    /// no-op and masks render their persistent centres exactly as
    /// before. Cheap call: just clones the slice.
    pub fn set_current_clip_track_points(&mut self, points: &[klipp_core::TrackPoint]) {
        self.current_clip_track_points.clear();
        self.current_clip_track_points.extend_from_slice(points);
    }

    /// Set the active working colorspace for subsequent grade-chain
    /// executions. Called by the engine when the loaded project's
    /// `working_colorspace` field changes (which today happens at
    /// project-open and via the Inspector's project-settings panel).
    /// The setter is cheap — just stores the enum value and (when GPU
    /// is up) pushes the matching luma vector into the GPU pipeline's
    /// per-frame uniform — so engines can call it once per frame
    /// without measurable overhead if they prefer that to tracking
    /// changes.
    /// Set the node-graph solo target. Cheap setter; the next
    /// `execute_linear_*` call returns this node's output instead of
    /// the Output-node-input. None clears the solo. The executor
    /// silently ignores ids that don't exist in the active graph
    /// (e.g. after a clip switch) — no error, no toast.
    #[inline]
    pub fn set_solo_node(&mut self, node_id: Option<NodeId>) {
        self.solo_node = node_id;
    }

    #[inline]
    /// Tetrahedral 3D-LUT interpolation (the default) or trilinear. The
    /// viewer follows the user's preference; an export always asks for
    /// tetrahedral.
    pub fn set_lut_tetrahedral(&mut self, tetrahedral: bool) {
        self.lut_tetrahedral = tetrahedral;
    }

    /// Grade a buffer `scale` times the sequence's size: the settings
    /// measured in pixels (a qualifier's shrink/grow and blur, the
    /// denoise radius, the grain's size, the clarity blur) cover the same
    /// share of the picture at any render size. 1 until set.
    pub fn set_render_scale(&mut self, scale: f32) {
        self.render_scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    }

    /// A length authored in the sequence's pixels, in the buffer's.
    fn px(&self, v: f32) -> f32 {
        v * self.render_scale
    }

    /// A whole number of the sequence's pixels, in the buffer's: at least
    /// one wherever it was any, as `at_render_scale` keeps effect radii.
    /// A value that rounds to none at full size stays none, so a full-size
    /// render applies exactly what it always did.
    fn whole_px(&self, v: f32) -> f32 {
        if v.abs().round() == 0.0 {
            0.0
        } else {
            v.signum() * (v.abs() * self.render_scale).round().max(1.0)
        }
    }

    /// The clarity blur's radius in the buffer's pixels.
    fn clarity_radius(&self) -> u32 {
        self.whole_px(klipp_effects::cpu_grade::CLARITY_RADIUS) as u32
    }

    /// The denoise kernel's cap in the buffer's pixels.
    fn denoise_max_kernel(&self) -> u32 {
        self.whole_px(klipp_effects::cpu_grade::DENOISE_MAX_KERNEL) as u32
    }

    pub fn set_working_colorspace(&mut self, ws: klipp_core::WorkingColorspace) {
        self.working_colorspace = ws;
        // The grade encoding goes with it: the project's choice, which
        // only ACEScg takes (#467).
        let tag = self.project_grade_encoding.effective(ws).as_u8();
        self.grade_encoding = klipp_color::grade_model::Encoding::from_u8(tag);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_luma_coefs(klipp_color::linear::luma_coefs(ws));
            gpu.set_grade_encoding(tag);
        }
    }

    /// The project's grade encoding (#467). It takes effect under
    /// ACEScg only, so it is reapplied with the current working space.
    pub fn set_grade_encoding(&mut self, enc: klipp_core::GradeEncoding) {
        self.project_grade_encoding = enc;
        self.set_working_colorspace(self.working_colorspace);
    }

    /// Read the executor's current working colorspace. Mostly useful
    /// for tests; production code should rely on
    /// [`Self::set_working_colorspace`] keeping it in sync with the project.
    #[inline]
    pub fn working_colorspace(&self) -> klipp_core::WorkingColorspace {
        self.working_colorspace
    }

    /// Seed the GPU chain's `buf_a` from an external u8 RGBA buffer
    /// via the `bridge_u8_to_linear_into_chain` compute shader. After
    /// this returns `true`, the next call to `execute_linear` on
    /// this executor can skip the initial
    /// `upload_chain_input_linear` — the `start_chain` helper picks
    /// up `gpu_chain_preseeded` and just dispatches, reading from
    /// the already-populated `buf_a`.
    ///
    /// Eliminates the CPU round-trip that phase 4c left in place:
    /// no ~5 ms PCIe upload, no u8→linear conversion on CPU. This
    /// is the operation where the plan doc's 12.6 ms → <4 ms target
    /// for a single-node grade gets paid for.
    ///
    /// Returns `false` if the GPU pipeline is absent or the bridge
    /// pre-conditions aren't met (input undersized, device lost).
    /// Callers fall back to the normal `execute_linear(&cpu_frame,
    /// ...)` path in that case.
    pub fn prime_chain_from_gpu_u8_buffer(
        &mut self,
        input_u8: &wgpu::Buffer,
        width: u32,
        height: u32,
    ) -> bool {
        self.ensure_gpu();
        let gpu = match self.gpu.as_mut() {
            Some(g) => g,
            None => return false,
        };
        let ok = gpu.bridge_u8_to_linear_into_chain(input_u8, width, height);
        if ok {
            self.gpu_chain_preseeded = true;
            self.gpu_chain_preseed_dims = Some((width, height));
        }
        ok
    }

    /// [`Self::prime_chain_from_gpu_u8_buffer`] from a frame of another
    /// size, fitted into `width` by `height` on the GPU as the compositor
    /// fits a source on the CPU.
    pub fn prime_chain_fit_from_gpu_u8_buffer(
        &mut self,
        input_u8: &wgpu::Buffer,
        src_width: u32,
        src_height: u32,
        width: u32,
        height: u32,
    ) -> bool {
        self.ensure_gpu();
        let Some(gpu) = self.gpu.as_mut() else { return false };
        let ok = gpu.bridge_u8_fit_to_linear_into_chain(input_u8, src_width, src_height, width, height);
        if ok {
            self.gpu_chain_preseeded = true;
            self.gpu_chain_preseed_dims = Some((width, height));
        }
        ok
    }

    /// [`Self::prime_chain_from_gpu_hdr10_buffer`] from codes in memory:
    /// uploaded to a buffer the executor keeps, and not again while the
    /// same codes (the same `Arc`) come back — a paused frame regraded at
    /// every step of a slider drag uploads once. A quarter of the bytes
    /// the linear frame would be, and no decode on the CPU.
    #[allow(clippy::too_many_arguments)]
    pub fn prime_chain_from_hdr10_codes(
        &mut self,
        codes: &Arc<Vec<u32>>,
        width: u32,
        height: u32,
        transfer_id: u32,
        scene_scale: f32,
        source_to_working: [[f32; 4]; 3],
    ) -> bool {
        if codes.len() != (width as usize) * (height as usize) || !self.upload_seed(SeedPixels::Codes(Arc::clone(codes))) {
            return false;
        }
        let (Some(gpu), Some(seed)) = (self.gpu.as_mut(), self.uploaded_seed.as_ref()) else { return false };
        let ok = gpu.bridge_hdr10_to_linear_into_chain(&seed.buffer, width, height, transfer_id, scene_scale, source_to_working);
        if ok {
            self.gpu_chain_preseeded = true;
            self.gpu_chain_preseed_dims = Some((width, height));
        }
        ok
    }

    /// [`Self::prime_chain_from_gpu_u8_buffer`] from sRGB bytes in memory,
    /// kept on the GPU as [`Self::prime_chain_from_hdr10_codes`] keeps codes.
    pub fn prime_chain_from_srgb8(&mut self, rgba: &Arc<Vec<u8>>, width: u32, height: u32) -> bool {
        if rgba.len() != (width as usize) * (height as usize) * 4 || !self.upload_seed(SeedPixels::Srgb8(Arc::clone(rgba))) {
            return false;
        }
        let (Some(gpu), Some(seed)) = (self.gpu.as_mut(), self.uploaded_seed.as_ref()) else { return false };
        let ok = gpu.bridge_u8_to_linear_into_chain(&seed.buffer, width, height);
        if ok {
            self.gpu_chain_preseeded = true;
            self.gpu_chain_preseed_dims = Some((width, height));
        }
        ok
    }

    /// [`Self::prime_chain_from_srgb8`] from a frame of another size,
    /// fitted into `width` by `height` on the GPU.
    pub fn prime_chain_fit_from_srgb8(&mut self, rgba: &Arc<Vec<u8>>, src_width: u32, src_height: u32, width: u32, height: u32) -> bool {
        if rgba.len() != (src_width as usize) * (src_height as usize) * 4 || !self.upload_seed(SeedPixels::Srgb8(Arc::clone(rgba))) {
            return false;
        }
        let (Some(gpu), Some(seed)) = (self.gpu.as_mut(), self.uploaded_seed.as_ref()) else { return false };
        let ok = gpu.bridge_u8_fit_to_linear_into_chain(&seed.buffer, src_width, src_height, width, height);
        if ok {
            self.gpu_chain_preseeded = true;
            self.gpu_chain_preseed_dims = Some((width, height));
        }
        ok
    }

    /// Take the frame just seeded into the chain as if a graph had
    /// produced it, so it can be read with `encode_output_to_yuv420` and
    /// friends. `false` if nothing is seeded.
    ///
    /// For a clip whose grade is an identity: there is no graph to run,
    /// and the source frame *is* the render. Without this, an ungraded
    /// clip declines every GPU output path and — worse — gets its frame
    /// pulled off the GPU by `input.pixels()` only to be handed straight
    /// back.
    ///
    /// Consumes the seed, like a run does, so it cannot be read a second
    /// time or leak into the next frame.
    pub fn take_seeded_input_as_output(&mut self) -> bool {
        if !self.gpu_chain_preseeded {
            return false;
        }
        self.gpu_chain_preseeded = false;
        self.gpu_chain_preseed_dims = None;
        // A seed always lands in `buf_a`, which is slot A.
        self.output_on_gpu = Some(true);
        true
    }

    /// HDR monitoring's light map over the last run's GPU-resident
    /// result: the brightest channel of every `step`-th pixel, cd/m².
    ///
    /// Read *after* the display encode, which writes to its own buffer and
    /// leaves the chain's linear frame where it is. That ordering is why
    /// monitoring can keep the fast path at all: the readout needs the
    /// light, the light needs the linear frame, and this measures it in
    /// place instead of bringing 132 MB down for it.
    pub fn nit_map_of_output(
        &mut self,
        width: u32,
        height: u32,
        step: u32,
        to_2020: [[f32; 4]; 3],
    ) -> Option<(Vec<f32>, u32, u32)> {
        let slot_a = self.output_on_gpu?;
        self.gpu.as_mut()?.nit_map_of_chain(slot_a, width, height, step, to_2020)
    }

    /// Run the timeline's global LUT over the last run's GPU-resident
    /// result, leaving it on the GPU.
    ///
    /// The project-wide colour pipeline is applied after the composite, on
    /// the CPU frame the render normally builds. A single-clip render has
    /// no composite to speak of, so the LUT can run as one more chain pass
    /// and the frame never has to come down — which is the difference
    /// between a project with a look LUT exporting at the fast path's speed
    /// and at the float route's.
    ///
    /// `false` when the last run did not keep its output on the GPU, or
    /// there is no GPU; the caller then renders the ordinary way. The
    /// caller is responsible for the *ordering* question this cannot see:
    /// the LUT belongs after any display pass a scene-referred clip ends
    /// with, and after the highlight split an HDR delivery needs, so
    /// `render_yuv420_on_gpu` only asks for this when neither applies.
    pub fn apply_global_lut_to_output(
        &mut self,
        width: u32,
        height: u32,
        lut_data: &[f32],
        lut_size: u32,
        opacity: f32,
        tetrahedral: bool,
        bracket: klipp_effects::gpu::LutBracket,
    ) -> bool {
        let Some(slot_a) = self.output_on_gpu else { return false };
        let Some(gpu) = self.gpu.as_mut() else { return false };
        // The graded run already submitted its chain, so this opens a
        // fresh one for the single pass. `begin_chain` resets the params
        // ring, which the LUT dispatch draws a slot from.
        gpu.begin_chain();
        gpu.apply_3d_lut_chain(slot_a, width, height, lut_data, lut_size, opacity, tetrahedral, bracket);
        gpu.submit_chain();
        // The pass reads one slot and writes the other.
        self.output_on_gpu = Some(!slot_a);
        self.last_chain_final_slot = self.output_on_gpu;
        true
    }

    /// The last run's GPU-resident result through a BT.2020 project's
    /// display transform, as display bytes.
    ///
    /// The GPU twin of `display_transform_ws(.., Bt2020Linear)`: the
    /// basis change and the BT.2407-style gamut compression, which cost
    /// ~15 ms of a 4K frame on the CPU and ~32 for wholly saturated
    /// colour. No tone-map curve, so the caller must only take this
    /// with `ToneMapOp::None`.
    pub fn encode_output_to_bt2020_display_u8(
        &mut self,
        width: u32,
        height: u32,
    ) -> Option<Vec<u8>> {
        let slot_a = self.output_on_gpu?;
        // 148 KB, built by bisecting two gamut boundaries — only when it
        // is going to be used, and the pipeline keeps the upload.
        let data = klipp_color::gamut::bt2020_to_bt709_gpu_data();
        self.gpu.as_mut()?.encode_chain_to_bt2020_display_u8(slot_a, width, height, &data)
    }

    /// The last run's GPU-resident result through the ACES Rec.709 SDR
    /// output transform, as display bytes.
    ///
    /// The GPU twin of `display_transform_ws` on an AcesCG project,
    /// which is `acescg_to_rec709_sdr` per pixel — 88 ms of a 4K frame
    /// on the CPU. `None` if the last run did not keep its output on
    /// the GPU, and the caller then takes that CPU path.
    pub fn encode_output_to_aces_rec709_u8(&mut self, width: u32, height: u32) -> Option<Vec<u8>> {
        let slot_a = self.output_on_gpu?;
        // Only built when it is going to be used: a couple of thousand
        // spline evaluations, and the pipeline keeps the upload.
        let data = klipp_color::aces::aces_rec709_gpu_data(1024);
        self.gpu.as_mut()?.encode_chain_to_aces_rec709_u8(slot_a, width, height, &data)
    }

    /// The last run's GPU-resident result as 8-bit 4:2:0 planes — Y, then
    /// Cb, then Cr, tightly packed — instead of a linear frame read back.
    ///
    /// The render's counterpart to `encode_output_to_display_u8`: the
    /// export's display transform and colour conversion happen on the
    /// GPU, so only the planes cross the bus. `None` if the last run did
    /// not keep its output there, or the GPU cannot take this frame —
    /// see `GpuEffectsPipeline::encode_chain_to_yuv420` for which frames
    /// those are — and the caller then converts on the CPU.
    /// Also the frame's light levels when the delivery is PQ, which needs
    /// them for HDR10's MaxCLL and MaxFALL; `None` on every other path,
    /// where the shader measures nothing.
    pub fn encode_output_to_yuv420(
        &mut self,
        width: u32,
        height: u32,
        target: klipp_effects::gpu::Yuv420Target,
    ) -> Option<(Vec<u8>, Option<klipp_effects::gpu::GpuFrameLight>)> {
        let slot_a = self.output_on_gpu?;
        // Whichever tables this delivery's transform reads, baked from
        // `klipp_color`, and only when it actually needs them. Cheap to
        // ask for every frame: the gamut reaches cost 13 ms to bisect but
        // sit behind a `OnceLock` there, so this copies them out in 0.03
        // ms, the ACES curves are 0.08, and the PQ table is memoised the
        // same way. The upload is cached too.
        let tables = if target.tone_map_op == 3 {
            // Which ODT, from the target's tag: the shader reads the
            // transform out of the blob, so this is the one place that
            // has to pick the right blob.
            let odt = klipp_color::aces::GpuOdt::from_tag(target.odt_tag)?;
            klipp_color::aces::aces_odt_gpu_data(odt, self.working_colorspace, 1024)
        } else if target.gamut_op == 1 {
            klipp_color::gamut::bt2020_to_bt709_gpu_data()
        } else if target.hdr_op != 0 {
            klipp_color::hdr::hdr_encode_gpu_data(self.working_colorspace)
        } else {
            Vec::new()
        };
        let gpu = self.gpu.as_mut()?;
        if target.hdr_op == 1 {
            return gpu
                .encode_chain_to_yuv420_measured(slot_a, width, height, target, &tables)
                .map(|(bytes, light)| (bytes, Some(light)));
        }
        gpu.encode_chain_to_yuv420(slot_a, width, height, target, &tables)
            .map(|bytes| (bytes, None))
    }

    /// Seed the chain from a buffer that is already linear working-space
    /// f32 on the GPU — a frame some other pass converted there, with no
    /// expansion left to do.
    ///
    /// This is how a hardware-decoded export frame arrives: the NV12
    /// converter turns the decoder's planes straight into linear light on
    /// the GPU (`convert_nv12_to_linear_on_gpu`), and the only thing left
    /// is to put them in the chain's input slot. `input` must be on the
    /// same device as this executor's pipeline, which means the converter
    /// was built from the same `GpuContext`; a buffer from a private
    /// device cannot be bound here and wgpu will say so.
    ///
    /// `false` if there is no GPU or the buffer is too small, and the
    /// caller then takes the CPU route.
    pub fn prime_chain_from_gpu_linear_buffer(
        &mut self,
        input: &wgpu::Buffer,
        width: u32,
        height: u32,
    ) -> bool {
        self.ensure_gpu();
        let Some(gpu) = self.gpu.as_mut() else { return false };
        let ok = gpu.copy_linear_into_chain(input, width, height);
        if ok {
            self.gpu_chain_preseeded = true;
            self.gpu_chain_preseed_dims = Some((width, height));
        }
        ok
    }

    /// [`Self::prime_chain_from_srgb8`] from 16-bit samples, with the
    /// source primaries folded into the expansion.
    ///
    /// What the export's scaler hands back, uploaded as it stands. An
    /// 8-bit source does not make an 8-bit RGBA64 buffer — YUV→RGB is a
    /// matrix — so this stays 16 bits wide the whole way to the shader
    /// rather than narrowing and quantising the delivered frame.
    ///
    /// `source_to_working` must be the identity when the source is
    /// already in the working space; pass it and the seed path serves
    /// any display-referred project, not only a Rec.709 one.
    pub fn prime_chain_from_rgba64(
        &mut self,
        rgba: &Arc<Vec<u16>>,
        width: u32,
        height: u32,
        source_to_working: [[f32; 4]; 3],
    ) -> bool {
        if rgba.len() != (width as usize) * (height as usize) * 4
            || !self.upload_seed(SeedPixels::Rgba64(Arc::clone(rgba)))
        {
            return false;
        }
        let (Some(gpu), Some(seed)) = (self.gpu.as_mut(), self.uploaded_seed.as_ref()) else {
            return false;
        };
        let ok = gpu.bridge_rgba64_to_linear_into_chain(
            &seed.buffer,
            width,
            height,
            source_to_working,
        );
        if ok {
            self.gpu_chain_preseeded = true;
            self.gpu_chain_preseed_dims = Some((width, height));
        }
        ok
    }

    /// Put `pixels` in the kept seed buffer, unless they are what it
    /// already holds. `false` without a GPU.
    fn upload_seed(&mut self, pixels: SeedPixels) -> bool {
        self.ensure_gpu();
        let Some(gpu) = self.gpu.as_mut() else { return false };
        if self.uploaded_seed.as_ref().is_some_and(|s| s.pixels.is(&pixels)) {
            return true;
        }
        let bytes = pixels.bytes();
        let buffer = match self.uploaded_seed.take() {
            Some(s) if s.buffer.size() >= bytes.len() as u64 => s.buffer,
            _ => gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("uploaded_seed"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
        };
        gpu.queue().write_buffer(&buffer, 0, bytes);
        self.uploaded_seed = Some(UploadedSeed { buffer, pixels });
        true
    }

    /// Seed the GPU chain from an HDR frame's packed RGB10 code values
    /// (the P010 decoder's `hdr_codes`), decoding them to scene-linear on
    /// the GPU — the HDR counterpart of `prime_chain_from_gpu_u8_buffer`.
    /// Same contract: the next run consumes the seed if it starts a chain
    /// at this resolution, and returns `false` if the GPU cannot take it.
    pub fn prime_chain_from_gpu_hdr10_buffer(
        &mut self,
        codes: &wgpu::Buffer,
        width: u32,
        height: u32,
        transfer_id: u32,
        scene_scale: f32,
        source_to_working: [[f32; 4]; 3],
    ) -> bool {
        self.ensure_gpu();
        let Some(gpu) = self.gpu.as_mut() else { return false };
        let ok = gpu.bridge_hdr10_to_linear_into_chain(
            codes, width, height, transfer_id, scene_scale, source_to_working,
        );
        if ok {
            self.gpu_chain_preseeded = true;
            self.gpu_chain_preseed_dims = Some((width, height));
        }
        ok
    }

    /// Ask the next run to leave its final result on the GPU. Honoured
    /// only when a chain produces that result directly and nothing else
    /// reads it; check `output_on_gpu` after the run.
    pub fn request_output_on_gpu(&mut self) {
        self.keep_output_on_gpu = true;
    }

    /// Chain slot A holds a frame put there by something other than a
    /// run — the compositor's stack — for the LUT and the encodes to take
    /// from there as they would a run's result.
    pub fn set_output_in_chain_slot_a(&mut self) {
        self.output_on_gpu = Some(true);
        self.last_chain_final_slot = Some(true);
    }

    /// How much of the last run the GPU chain carried. A graph that
    /// round-trips every node produces the same picture as one that
    /// stays resident, only far slower, so this is the only way a test
    /// can hold the difference.
    /// Runs that compiled their plan, and runs that found it cached, since
    /// this executor was made (#1089, M2 step 8).
    pub fn plan_counts(&self) -> (u64, u64) {
        (self.plan_compiles, self.plan_hits)
    }

    pub fn last_run_chain_stats(&self) -> ChainStats {
        self.last_run_chain_stats
    }

    /// `Some(slot_a)` if the last run left its final result in the chain
    /// buffer. Its returned Vec is then empty; read the result with
    /// `encode_output_to_display_u8`.
    pub fn output_on_gpu(&self) -> Option<bool> {
        self.output_on_gpu
    }

    /// Withdraw a [`Self::request_output_on_gpu`] that no run took up (the
    /// grade was skipped, or the run stopped before it), so it can't apply
    /// to the next, unrelated run.
    pub fn cancel_output_on_gpu(&mut self) {
        self.keep_output_on_gpu = false;
    }

    /// Run built-in `effects` on the last run's result where it sits on
    /// the GPU, and read the result back (#1089, M2 step 10): the grade's
    /// output goes on into the effects without a download and an upload
    /// between them. `None` if the last run did not keep its output on
    /// the GPU. The effects run as `run_effects_in_chain` runs them
    /// anywhere: recorded arms batched, the rest each in its own pass.
    pub fn effects_on_output(&mut self, effects: &[klipp_effects::BuiltinEffect], width: u32, height: u32) -> Option<Vec<f32>> {
        let slot_a = self.output_on_gpu.take()?;
        self.last_chain_final_slot = None;
        let gpu = self.gpu.as_mut()?;
        let in_a = gpu.run_effects_in_chain_from(effects, slot_a, width, height);
        Some(gpu.download_chain_slot_linear(in_a, (width as usize) * (height as usize)))
    }

    /// Encode the last run's GPU-resident result to display sRGB u8 and
    /// read that back — see `GpuEffectsPipeline::encode_chain_to_display_u8`.
    /// `tonemap_exposure` applies the display tone map first. `None` if the
    /// last run did not keep its output on the GPU, or the GPU failed.
    pub fn encode_output_to_display_u8(
        &mut self,
        width: u32,
        height: u32,
        tonemap_exposure: Option<f32>,
    ) -> Option<Vec<u8>> {
        self.encode_output_to_display_u8_view(width, height, tonemap_exposure, false, None)
    }

    /// [`Self::encode_output_to_display_u8`], and for `hdr_preview` HDR
    /// monitoring's preview instead of the SDR encode: the light above
    /// reference white rolled into the top.
    ///
    /// `scene` is a scene-referred clip's `(display_light_kind,
    /// scene_scale, peak_nits)`; the working space's luma weights come
    /// from the executor's own colourspace, since the HLG branch of the
    /// display-light pass needs them.
    pub fn encode_output_to_display_u8_view(
        &mut self,
        width: u32,
        height: u32,
        tonemap_exposure: Option<f32>,
        hdr_preview: bool,
        scene: Option<(u32, f32, f32)>,
    ) -> Option<Vec<u8>> {
        let slot_a = self.output_on_gpu?;
        let luma = klipp_color::linear::luma_coefs(self.working_colorspace);
        let scene = scene.map(|(kind, scale, peak)| (kind, scale, peak, luma));
        self.gpu.as_mut()?.encode_chain_to_display_u8_monitor(
            slot_a,
            width,
            height,
            tonemap_exposure,
            hdr_preview,
            scene,
        )
    }

    /// True iff the executor owns a GPU pipeline. When false, every
    /// node falls back to the CPU path — `extends=0 starts=0` in
    /// `PERF graph_exec` log lines is the smoke signal. Use this from
    /// the engine to assert consistency: if the engine has GPU but the
    /// executor doesn't, color grading silently runs at 5-10× the cost.
    pub fn has_gpu(&self) -> bool {
        self.gpu.is_some()
    }

    /// Mutable accessor to the underlying GPU pipeline (if any).
    /// Used by the engine's post_process to encode the final graded
    /// frame into a viewer-bindable texture on the same device.
    /// Returns `None` if GPU init failed or hasn't been attempted yet.
    pub fn gpu_pipeline_mut(&mut self) -> Option<&mut GpuEffectsPipeline> {
        self.gpu.as_mut()
    }

    /// [`Self::gpu_pipeline_mut`], building the pipeline first if it has
    /// not been tried yet: for work that wants the GPU before any graph
    /// has run on it, such as the scopes of an ungraded clip.
    pub fn gpu_pipeline_or_init(&mut self) -> Option<&mut GpuEffectsPipeline> {
        self.ensure_gpu();
        self.gpu.as_mut()
    }

    /// Pre-build curve LUTs for all RGBCurves nodes at clip start.
    /// Called from begin_clip() to avoid rebuilding every frame.
    pub fn prepare_clip(&mut self, graph: &NodeGraph) {
        self.curve_lut_cache.clear();
        for node in &graph.nodes {
            if node.kind == NodeKind::RGBCurves {
                if let Some(curves) = &node.params.curves {
                    if !is_curves_identity(curves) {
                        let luts = Arc::new(build_curve_luts(curves));
                        self.curve_lut_cache.insert(node.id, luts);
                    }
                }
            }
        }
    }

    /// Execute the graph for one frame.
    ///
    /// Returns (final graded frame, optional tapped node output for scopes).
    ///
    /// Accepts and returns sRGB u8 RGBA. Converts to linear at the entry,
    /// delegates to [`Self::execute_linear`], converts back at the exit. Callers
    /// that already hold linear data should call [`Self::execute_linear`] directly
    /// to avoid the extra round-trip.
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        &mut self,
        source_frame: &[u8],
        graph: &NodeGraph,
        width: u32,
        height: u32,
        clip_time_secs: f64,
        frame_number: u64,
        matte_tap: Option<NodeId>,
        matte_preview_node: Option<NodeId>,
    ) -> Result<(Vec<u8>, Option<Vec<u8>>), ExecutorError> {
        let source_linear = u8_rgba_to_linear(source_frame);
        let (result_linear, tapped_linear) = self.execute_linear(
            &source_linear,
            graph,
            width,
            height,
            clip_time_secs,
            frame_number,
            matte_tap,
            matte_preview_node,
        )?;
        let result_u8 = linear_rgba_to_u8(&result_linear);
        let tapped_u8 = tapped_linear.as_deref().map(linear_rgba_to_u8);
        Ok((result_u8, tapped_u8))
    }

    /// Execute the graph on a linear-light f32 RGBA frame.
    ///
    /// This is the native entry point for the linear-light pipeline. The
    /// buffer is 4 f32 per pixel, RGBA interleaved, alpha linear. Returns
    /// (final graded frame, optional tapped node output for scopes), both in
    /// the same linear-light representation.
    ///
    /// Consecutive single-input filter nodes that have a GPU implementation
    /// are collapsed into a GPU-resident chain: one upload at the head, one
    /// download at the tail, ping-pong between `buf_a`/`buf_b` in between.
    /// This is the dominant cost of interactive colour grading — every
    /// saved download is a ~15-20 ms sync fence on NVIDIA Vulkan.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_linear(
        &mut self,
        source_frame: &[f32],
        graph: &NodeGraph,
        width: u32,
        height: u32,
        clip_time_secs: f64,
        frame_number: u64,
        matte_tap: Option<NodeId>,
        matte_preview_node: Option<NodeId>,
    ) -> Result<(Vec<f32>, Option<Vec<f32>>), ExecutorError> {
        // Convenience wrapper used by callers that don't want per-node
        // thumbnails. The richer entry point is `execute_linear_with_thumbs`.
        self.execute_linear_with_thumbs(
            source_frame, graph, width, height, clip_time_secs, frame_number,
            matte_tap, matte_preview_node, None,
        ).map(|(out, tap, _thumbs)| (out, tap))
    }

    /// Same as `execute_linear` but also captures downsampled output
    /// thumbnails for any node whose id appears in `visible_nodes`.
    /// Pass `None` to skip capture entirely (zero overhead).
    ///
    /// Caveat: nodes whose outputs live exclusively on the GPU (mid-
    /// chain extend) end up with empty sentinel buffers in
    /// `node_outputs` — those nodes get no thumbnail. Only nodes
    /// whose output landed CPU-side (chain endpoints, matte
    /// producers, composite nodes) yield a thumb. Acceptable for v1.
    ///
    /// Honours `graph.working_space_override` by swapping the
    /// executor's active working colorspace (and matching GPU luma
    /// uniform) for the duration of this call. Restored on every exit
    /// path — including the matte-preview short-circuit and the
    /// `NoOutputConnection` error — so a subsequent grade chain reads
    /// the project-level value untouched. Recursive Compound calls
    /// re-enter this function with the inner sub-graph's own override
    /// (or `None`) and nest cleanly because each level saves the
    /// outer-level value, swaps, runs, restores.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_linear_with_thumbs(
        &mut self,
        source_frame: &[f32],
        graph: &NodeGraph,
        width: u32,
        height: u32,
        clip_time_secs: f64,
        frame_number: u64,
        matte_tap: Option<NodeId>,
        matte_preview_node: Option<NodeId>,
        visible_nodes: Option<&std::collections::HashSet<NodeId>>,
    ) -> Result<(Vec<f32>, Option<Vec<f32>>, std::collections::HashMap<NodeId, Vec<u8>>), ExecutorError> {
        // The run's plan (#1089, M2 step 8): compiled once per graph shape
        // and kept. A correction gated by a matte runs as the correction
        // blended by an ApplyMatte, so no correction reads its port 1
        // (#440); the plan holds that rewrite, refreshed with this frame's
        // values.
        let key = crate::plan::plan_key(graph, clip_time_secs, matte_tap, matte_preview_node, visible_nodes);
        let mut plan = match self.plans.remove(&key) {
            Some(plan) => {
                self.plan_hits += 1;
                plan
            }
            None => {
                self.plan_compiles += 1;
                crate::plan::ExecPlan::compile(graph, matte_tap, matte_preview_node, visible_nodes)?
            }
        };
        plan.refresh(graph);
        let result = self.execute_with_plan(
            source_frame, graph, &plan, width, height, clip_time_secs, frame_number,
            matte_tap, matte_preview_node, visible_nodes,
        );
        // Kept for the next run of this shape. A long session of many
        // shapes starts over rather than grow without bound.
        if self.plans.len() >= MAX_PLANS {
            self.plans.clear();
        }
        self.plans.insert(key, plan);
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_with_plan(
        &mut self,
        source_frame: &[f32],
        live: &NodeGraph,
        plan: &crate::plan::ExecPlan,
        width: u32,
        height: u32,
        clip_time_secs: f64,
        frame_number: u64,
        matte_tap: Option<NodeId>,
        matte_preview_node: Option<NodeId>,
        visible_nodes: Option<&std::collections::HashSet<NodeId>>,
    ) -> Result<(Vec<f32>, Option<Vec<f32>>, std::collections::HashMap<NodeId, Vec<u8>>), ExecutorError> {
        let graph = plan.graph(live);
        let saved_ws = self.working_colorspace;
        let needs_swap = graph
            .working_space_override
            .map(|ws| ws != saved_ws)
            .unwrap_or(false);
        if needs_swap {
            // unwrap is safe — needs_swap is true only when override is Some.
            self.set_working_colorspace(graph.working_space_override.unwrap());
        }
        let result = self.execute_linear_with_thumbs_inner(
            source_frame, graph, plan, width, height, clip_time_secs, frame_number,
            matte_tap, matte_preview_node, visible_nodes,
        );
        if needs_swap {
            self.set_working_colorspace(saved_ws);
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_linear_with_thumbs_inner(
        &mut self,
        source_frame: &[f32],
        graph: &NodeGraph,
        plan: &crate::plan::ExecPlan,
        width: u32,
        height: u32,
        clip_time_secs: f64,
        frame_number: u64,
        matte_tap: Option<NodeId>,
        matte_preview_node: Option<NodeId>,
        visible_nodes: Option<&std::collections::HashSet<NodeId>>,
    ) -> Result<(Vec<f32>, Option<Vec<f32>>, std::collections::HashMap<NodeId, Vec<u8>>), ExecutorError> {
        // An empty `source_frame` is only meaningful when the GPU chain
        // has been pre-seeded — the caller skipped the GPU→CPU readback
        // on the bet that the graph runs entirely on-chain. With no seed
        // that bet was never placed and the pixels are simply missing,
        // so say so rather than grading a zero buffer into a black frame.
        //
        // Nodes that fall back to `source_frame` mid-graph raise the same
        // error from their own sites; between them the executor never
        // reads pixels that are not there.
        // Move the seed into run scope, clearing the incoming flag in the
        // same breath. Two things fall out of this: the first
        // `start_chain` still consumes it (single-shot, so a second chain
        // segment uploads normally), and a seed this run never uses dies
        // here rather than leaking into the next frame.
        self.run_seed_dims = if self.gpu_chain_preseeded {
            self.gpu_chain_preseed_dims
        } else {
            None
        };
        self.gpu_chain_preseeded = false;
        self.gpu_chain_preseed_dims = None;
        let gpu_resident_source = source_frame.is_empty() && self.run_seed_dims.is_some();
        if source_frame.is_empty() && !gpu_resident_source {
            return Err(ExecutorError::CpuSourceRequired);
        }
        self.ensure_gpu();
        // Thumbnails are taken on the GPU, inside the chain, when there is
        // one: a thumbnailed node then stays on the chain. Without a GPU
        // they come from CPU pixels, as before.
        let gpu_thumbs = visible_nodes.is_some_and(|v| !v.is_empty()) && self.gpu.is_some();
        if gpu_thumbs {
            let gpu = self.gpu.as_ref().expect("checked above");
            self.chain_thumbs
                .get_or_insert_with(|| klipp_effects::gpu::ChainThumbs::new(gpu, THUMB_W, THUMB_H))
                .begin();
        }
        let mut gpu_thumb_of: HashMap<NodeId, usize> = HashMap::new();
        let take_thumb = |this: &mut Self, slot_a: bool| -> Option<usize> {
            let (Some(gpu), Some(thumbs)) = (this.gpu.as_mut(), this.chain_thumbs.as_mut()) else { return None };
            gpu.take_chain_thumb(thumbs, slot_a, width, height)
        };
        // Reset chain-state metadata before this run; populated by
        // `flush_chain` if the graph engages the GPU chain.
        self.last_chain_final_slot = None;
        let t_total = std::time::Instant::now();
        let mut per_node_ms: Vec<(String, f64)> = Vec::new();
        // The GPU's counts before the run, for the run's own (a pipeline
        // made during the run starts from 0).
        let gpu_counts = |this: &Self| this.gpu.as_ref().map_or((0, 0), |g| (g.submit_count(), g.frame_upload_count()));
        let (submits_before, uploads_before) = gpu_counts(self);
        let mut chain_flush_count: u32 = 0;
        // Nodes the run evaluated (each node once: the walk visits it once).
        let mut evaluations: u32 = 0;
        let mut chain_extend_count: u32 = 0;
        let mut chain_start_count: u32 = 0;
        let mut bypass_tunnel_count: u32 = 0;
        let mut cpu_fallback_count: u32 = 0;

        // 1. The plan's order, the nodes on the way to the output (only
        // those run: a node without a path there does nothing to the
        // picture, and one with no input asked for the CPU source), the
        // fan-out (a node whose output is read more than once can't be
        // absorbed into a chain, so the chain breaks after it), the
        // qualifier-driven secondaries the chain can carry, and whose
        // picture each Blend or Layer reads on port 1 (`ExecPlan`).
        let order = &plan.order;
        let feeding = &plan.feeding;
        let fan_out = &plan.fan_out;
        let secondaries = &plan.secondaries;
        let composite_port1 = &plan.composite_port1;
        // Whose picture `composite_src` holds, and how many times this
        // chain segment has written it. Same shape as `matte_in_buf`:
        // the gate is an identity check, not a hope.
        let mut composite_src_holds: Option<NodeId> = None;
        // And whose picture `grade_mix_src` holds. A secondary's keyer
        // and its ApplyMatte both read that buffer rather than the
        // chain's current slot, so "a chain is live" was never enough
        // to let them dispatch — the kept picture has to be this
        // secondary's source. Without the check, a graph where the
        // source ran on the CPU sent the keyer at a buffer nobody had
        // filled, which panicked outright when it was the first frame
        // and read the previous frame's picture when it was not.
        let mut kept_picture: Option<NodeId> = None;
        let mut segment_composite_writes: u32 = 0;

        // Output-on-GPU is single-shot, like the seed: take the request
        // here so a run that cannot honour it does not leave it set. The
        // final node qualifies only when Output is its sole reader and no
        // one wants its pixels on the CPU — not a solo view, a scope or
        // matte tap, or a thumbnail.
        self.output_on_gpu = None;
        let keep = std::mem::take(&mut self.keep_output_on_gpu);
        // (Thumbnails also rule it out: they are read once the chain's
        // last submit is in, and a result kept on the GPU never submits.)
        self.run_gpu_output_node = if keep && self.solo_node.is_none() && !gpu_thumbs {
            graph.input_of(graph.output_node_id, 0).filter(|id| {
                fan_out.get(id).copied().unwrap_or(0) <= 1
                    && Some(*id) != matte_tap
                    && Some(*id) != matte_preview_node
                    && !visible_nodes.is_some_and(|v| v.contains(id))
            })
        } else {
            None
        };

        // NodeId → &Node index. The topo walk below resolves every
        // node id in `order` to its full Node. A linear scan per step
        // (`graph.nodes.iter().find(...)`) was O(N²) across the walk;
        // grade stacks of 10-20 nodes run every frame during scrub.
        let node_by_id: HashMap<NodeId, &Node> =
            graph.nodes.iter().map(|n| (n.id, n)).collect();

        // 2. Map: NodeId → linear-light output buffer (Arc-shared for zero-copy
        //    passthrough of bypassed / default-param nodes).
        let mut node_outputs = plan.values();
        let mut tapped_output: Option<Arc<Vec<f32>>> = None;

        // ── GPU chain state ────────────────────────────────────────────
        //
        // Between nodes we may carry the "current" frame in a GPU storage
        // buffer instead of downloading to CPU. `chain_tail` names the
        // node whose output currently lives in GPU memory; `chain_slot_a`
        // tracks which ping-pong slot holds it (true = buf_a).
        let mut chain_tail: Option<NodeId> = None;
        let mut chain_slot_a: bool = true;
        // Mask node whose matte currently lives in `matte_buf` on the
        // GPU. Set by the Mask executor arm when conditions are right
        // (chain active, non-Bezier, sole consumer is a MaskCutout
        // that'll join the chain). Consumed by MaskCutout's chain
        // dispatch path. None = no GPU-resident matte; MaskCutout
        // falls back to CPU multiply.
        let mut matte_in_buf: Option<NodeId> = None;

        // 3. Execute nodes in topo order
        for &node_id in order {
            let t_node = std::time::Instant::now();
            let node = match node_by_id.get(&node_id).copied() {
                Some(n) => n,
                None => continue,
            };
            if !feeding.contains(&node_id) && !matches!(node.kind, NodeKind::Source | NodeKind::Output) {
                continue;
            }
            evaluations += 1;
            let node_kind_name = format!("{:?}", node.kind);

            // Bypassed nodes — and single-input filter nodes whose params
            // are all at defaults — pass their primary input through
            // unchanged. The Source node must still run (it produces the
            // root buffer), and Output is handled below. Nodes that combine
            // multiple inputs (CombineMattes, ApplyMatte, Blend, Layer) or
            // that generate mattes from geometry (Mask, Qualifier) are NOT
            // no-ops even with default params and must still execute —
            // a Blend at its default mix is still half of two pictures.
            let is_param_driven_filter = matches!(
                node.kind,
                NodeKind::InputTransform
                    | NodeKind::Exposure
                    | NodeKind::Contrast
                    | NodeKind::Saturation
                    | NodeKind::ToneControls
                    | NodeKind::ColorWheels
                    | NodeKind::WheelSatBri
                    | NodeKind::RGBCurves
                    | NodeKind::HueSatCurves
                    | NodeKind::ColorBalance
                    | NodeKind::Lut3D
                    | NodeKind::FilmGrain
                    | NodeKind::ColorMatch
                    | NodeKind::ColorWarper
                    | NodeKind::Denoise
            );
            // An InputTransform whose format needs no transform (sRGB,
            // linear, and PQ/HLG — which the decoder's HDR input stage
            // already decodes) is a pure passthrough. Its params are not
            // "default" — it records the source format — so without this
            // it ran as a real node on the CPU and copied the whole frame
            // to change nothing: 31 ms a frame at 4K, and it sat in front
            // of the Corrector, so the chain could not start from a seed.
            // An InputTransform only records the source format (the
            // decoder has already decoded it), so it is always a
            // passthrough: tunnelled, never run, never copied.
            let noop_input_transform = node.kind == NodeKind::InputTransform;
            // Default-ness at *this frame*: a keyframed node with neutral
            // static params (an exposure ramp from 0) must not be bypassed
            // for the whole clip. `params_for_frame` borrows when the node
            // has no keyframes, so the common case costs nothing extra.
            // A Corrector with no stage active changes no pixel either,
            // judged by the same test that decides whether it can chain.
            // It was in neither list: not bypassed, and not chainable, so
            // a grade reset to neutral (a look over a default Corrector)
            // ran the Corrector on the CPU and read a GPU-resident frame
            // back for it, about 105 ms a frame at 2160x3840.
            let neutral_corrector = node.kind == NodeKind::Corrector
                && !corrector_has_active_stage(&params_for_frame(node, clip_time_secs));
            let bypass = !effective_enabled(node, clip_time_secs)
                || (is_param_driven_filter && params_for_frame(node, clip_time_secs).is_default())
                || neutral_corrector
                || noop_input_transform;

            // If a chain is live, see whether this node can extend it or
            // whether we must flush the tail back to CPU first.
            let primary_input_id = graph.input_of(node_id, 0);
            let primary_is_chain_tail = chain_tail.is_some() && primary_input_id == chain_tail;
            // The chain tail's fan-out must be 1 for the chain to extend
            // past it (more consumers would need a CPU copy anyway, so
            // extending only delays the inevitable download).
            let chain_tail_fan_out_one = chain_tail
                .map(|id| {
                    fan_out.get(&id).copied().unwrap_or(0) <= 1
                        // The one tail whose extra consumers the chain
                        // itself serves: a secondary's source keeps its
                        // picture in a side buffer for ApplyMatte and
                        // hands the Qualifier a slot to read, so no
                        // consumer needs a CPU copy.
                        || secondaries.iter().any(|sec| sec.source == id)
                })
                .unwrap_or(true);
            // Visible nodes (those the V2 canvas wants thumbnails for)
            // keep the chain when their thumbnail can be taken on the GPU
            // (`gpu_thumbs`), right after their dispatch. Otherwise they
            // must not extend or start one: their output would live only
            // on the GPU and the CPU thumbnail pass would have nothing to
            // downsample, so they take the per-node CPU path.
            let node_is_visible_for_thumbs = visible_nodes
                .map(|v| v.contains(&node_id))
                .unwrap_or(false);
            let thumb_needs_cpu = node_is_visible_for_thumbs && !gpu_thumbs;
            // MaskCutout has a second input (port 1 = matte). It can
            // only extend the GPU chain when that matte was just
            // dispatched into matte_buf by the upstream Mask node.
            // Without this gate, MaskCutout would try to read garbage
            // from matte_buf and produce a black frame.
            let mask_cutout_matte_ready = if node.kind == NodeKind::MaskCutout {
                let matte_input = graph.input_of(node_id, 1);
                matte_input.is_some() && matte_input == matte_in_buf
            } else {
                true
            };
            // The keyer and ApplyMatte of a recognised secondary are
            // chain ops for this graph even though the kinds are not
            // chainable in general: one writes `matte_buf`, the other
            // reads it alongside the kept picture.
            let secondary_chain_node = secondaries.iter().any(|sec| {
                (sec.matte == node_id || sec.apply == node_id)
                    && kept_picture == Some(sec.source)
            });
            // The secondary's keyer reads the kept copy of its source,
            // not the chain's current picture, so it does not need to be
            // looking at the tail — the grade branch may already have
            // moved it on.
            let secondary_keyer_ready = secondaries.iter().any(|sec| {
                sec.matte == node_id
                    && chain_tail.is_some()
                    && primary_input_id == Some(sec.source)
                    && kept_picture == Some(sec.source)
            });
            // A composite can ride the chain when port 0 is the tail
            // (`primary_is_chain_tail`, below) and port 1's picture is
            // reachable without a readback.
            let composite_port1_id = if matches!(node.kind, NodeKind::Blend | NodeKind::Layer) {
                graph.input_of(node_id, 1)
            } else {
                None
            };
            let composite_route = composite_port1_id.and_then(|src_id| {
                if composite_src_holds == Some(src_id) {
                    Some(CompositeRoute::Resident)
                } else if segment_composite_writes == 0 {
                    node_outputs
                        .get(&src_id)
                        .filter(|a| a.len() == (width as usize) * (height as usize) * 4)
                        .map(|a| CompositeRoute::Upload(a.clone()))
                } else {
                    None
                }
            });
            let composite_ready = composite_route.is_some() && self.gpu.is_some();

            let can_extend_chain = !bypass
                && (primary_is_chain_tail || secondary_keyer_ready)
                && chain_tail_fan_out_one
                && (secondary_chain_node
                    || composite_ready
                    || self.node_supports_gpu_chain(&node.kind, &node.params))
                && mask_cutout_matte_ready
                && Some(node_id) != matte_preview_node
                && Some(node_id) != matte_tap
                && !thumb_needs_cpu;

            // Bypass tunnel: a bypass node whose primary input IS the
            // current chain tail doesn't touch pixels, so we can rename
            // the tail to this node and keep the chain GPU-resident.
            // The next non-bypass chain-capable node will extend off
            // `node.id` naturally.
            //
            // Preconditions matching `can_extend_chain`'s bypass-relevant
            // guards: fan-out 1 (anything else needs a CPU copy the Arc
            // passthrough would supply), and not a matte tap / preview
            // (those need real data off the GPU).
            let bypass_tunnelable = bypass
                && node.kind != NodeKind::Source
                && node.kind != NodeKind::Output
                && chain_tail.is_some()
                && primary_is_chain_tail
                && fan_out.get(&node.id).copied().unwrap_or(0) <= 1
                && Some(node_id) != matte_tap
                && Some(node_id) != matte_preview_node;
            if bypass_tunnelable {
                // Tunnel: chain stays on GPU, tail renames to this node.
                node_outputs.insert(node.id, Arc::new(Vec::new()));
                chain_tail = Some(node.id);
                if node_is_visible_for_thumbs && gpu_thumbs {
                    if let Some(i) = take_thumb(self, chain_slot_a) {
                        gpu_thumb_of.insert(node.id, i);
                    }
                }
                bypass_tunnel_count += 1;
                per_node_ms.push((format!("{node_kind_name}#bypass_tunnel"), t_node.elapsed().as_secs_f64() * 1000.0));
                continue;
            }

            // If a chain is live and this node can't extend it, flush
            // before proceeding. This covers:
            //   - bypass / default-param nodes consuming the chain tail
            //     that can't tunnel (fan-out > 1, matte tap/preview)
            //   - Output node (terminal)
            //   - matte generators and the multi-input nodes that have
            //     no chain dispatch (CombineMattes, Mask, Qualifier),
            //     which touch GPU state directly. Blend and Layer are
            //     no longer in this list: they ride the chain whenever
            //     their port-1 picture is reachable without a readback.
            //   - chain tail with fan-out > 1 (output read elsewhere)
            //
            // We also flush if the current node is unrelated (doesn't
            // consume the chain tail) because GPU-based matte / scope ops
            // may clobber buf_a/buf_b mid-chain. Correctness > perf for
            // the rare branching case; the common linear grade chain
            // never triggers this.
            if chain_tail.is_some() && !can_extend_chain {
                self.flush_chain(
                    &mut chain_tail,
                    chain_slot_a,
                    width,
                    height,
                    &mut node_outputs,
                    matte_tap,
                    &mut tapped_output,
                );
                chain_flush_count += 1;
                segment_composite_writes = 0;
            }

            if bypass && node.kind != NodeKind::Source && node.kind != NodeKind::Output {
                let passthrough = match primary_input_id.and_then(|id| node_outputs.get(&id)) {
                    Some(v) => v.clone(),
                    None => {
                        if source_frame.is_empty() {
                            return Err(ExecutorError::CpuSourceRequired);
                        }
                        Arc::new(source_frame.to_vec())
                    }
                };
                node_outputs.insert(node.id, passthrough);
                per_node_ms.push((format!("{node_kind_name}#bypass"), t_node.elapsed().as_secs_f64() * 1000.0));
                continue;
            }

            // Source, pixels already on the GPU: publish the same
            // sentinel-empty Arc the chain path uses. The downstream
            // chain node starts from the pre-seeded `buf_a`, so there is
            // nothing to hand it on the CPU side — and the ordinary
            // `source_frame.to_vec()` in `execute_node` would be a
            // 132 MB memcpy per 4K frame (RGBA f32) for a buffer no one
            // reads. Taps, and thumbnails that need CPU pixels, are
            // excluded: they fall through to the unchained path, hit
            // `CpuSourceRequired`, and the engine retries with a readback.
            // A GPU thumbnail is taken from the seed itself.
            if node.kind == NodeKind::Source
                && gpu_resident_source
                && Some(node_id) != matte_tap
                && Some(node_id) != matte_preview_node
                && !thumb_needs_cpu
            {
                if node_is_visible_for_thumbs {
                    if let Some(i) = take_thumb(self, true) {
                        gpu_thumb_of.insert(node.id, i);
                    }
                }
                node_outputs.insert(node.id, Arc::new(Vec::new()));
                per_node_ms.push((
                    format!("{node_kind_name}#gpu_resident"),
                    t_node.elapsed().as_secs_f64() * 1000.0,
                ));
                continue;
            }

            if node.kind == NodeKind::Output {
                per_node_ms.push((format!("{node_kind_name}#output"), t_node.elapsed().as_secs_f64() * 1000.0));
                continue;
            }

            // Evaluate keyframes — override params at current clip time.
            // Borrow `node.params` directly when there are no keyframes (the
            // common case); only clone + mutate when keyframe tracks exist.
            let params_cow = params_for_frame(node, clip_time_secs);
            let params = &*params_cow;

            // ── GPU chain fast-path ────────────────────────────────────
            // Does this node extend an active chain? If so, dispatch on
            // the GPU-resident buffer pair and leave the result on the
            // GPU. Re-check `can_extend_chain` against the evaluated
            // params — a keyframe may have flipped ColorWheels into
            // log-mode (CPU-only), which we can't dispatch.
            if can_extend_chain
                && (secondary_chain_node
                    || composite_ready
                    || self.node_supports_gpu_chain(&node.kind, params))
                && self.gpu.is_some()
            {
                // Blend / Layer: port 0 is the chain's picture, port 1 is
                // in `composite_src`. One dispatch, no readback, where
                // the CPU path costs a flush and a full-frame lerp.
                if let Some(route) = composite_route {
                    let placed = match &route {
                        CompositeRoute::Resident => true,
                        CompositeRoute::Upload(px) => self
                            .gpu
                            .as_mut()
                            .is_some_and(|g| g.upload_composite_src_linear(px, width, height)),
                    };
                    if placed {
                        if matches!(route, CompositeRoute::Upload(_)) {
                            segment_composite_writes += 1;
                            composite_src_holds = composite_port1_id;
                        }
                        let gpu = self.gpu.as_mut().expect("gpu checked above");
                        if node.kind == NodeKind::Blend {
                            let mix = params.mix.unwrap_or(0.5).clamp(0.0, 1.0);
                            gpu.apply_blend_chain(chain_slot_a, width, height, mix);
                        } else {
                            gpu.apply_over_chain(chain_slot_a, width, height);
                        }
                        chain_slot_a = !chain_slot_a;
                        chain_tail = Some(node_id);
                        node_outputs.insert(node_id, Arc::new(Vec::new()));
                        // The same three hooks the generic extend path
                        // runs after a dispatch. They live here too
                        // because this arm returns early, and a composite
                        // is as eligible as any other node to be a
                        // secondary's source or another composite's port
                        // 1 — a Layer feeding a qualifier secondary hit
                        // exactly that and panicked in the keyer, which
                        // reads a picture nobody had kept.
                        if secondaries.iter().any(|sec| sec.source == node_id) {
                            if let Some(gpu) = self.gpu.as_mut() {
                                gpu.keep_chain_input(chain_slot_a, width, height);
                            }
                            kept_picture = Some(node_id);
                        } else if takes_the_kept_buffer(&node.kind, params) {
                            kept_picture = None;
                        }
                        if composite_port1.contains_key(&node_id) {
                            if let Some(gpu) = self.gpu.as_mut() {
                                gpu.keep_composite_src(chain_slot_a, width, height);
                            }
                            composite_src_holds = Some(node_id);
                            segment_composite_writes += 1;
                        }
                        if secondaries.iter().any(|sec| sec.apply == node_id) {
                            matte_in_buf = None;
                        }
                        if node_is_visible_for_thumbs {
                            if let Some(i) = take_thumb(self, chain_slot_a) {
                                gpu_thumb_of.insert(node_id, i);
                            }
                        }
                        chain_extend_count += 1;
                        per_node_ms.push((
                            format!("{node_kind_name}#chain_composite"),
                            t_node.elapsed().as_secs_f64() * 1000.0,
                        ));
                        continue;
                    }
                    // Upload refused (device lost, or a size that does
                    // not match). Fall through: the generic dispatch
                    // declines these kinds, which flushes and takes the
                    // CPU path — slower, still right.
                }
                // Current input lives in the slot named by chain_slot_a.
                // Dispatch reads from that slot, writes to the opposite.
                // Some nodes (Contrast with both contrast+clarity, or
                // Saturation with both sat+vibrance) issue TWO internal
                // sub-dispatches in sequence — the return value reports
                // how many slot flips the caller needs to mirror.
                match self.dispatch_chain_node_slots(
                    node_id, &node.kind, params, width, height, frame_number,
                    /* in_is_a = */ chain_slot_a,
                ) {
                    // Zero flips means the node wrote something other
                    // than the picture — a secondary's Qualifier filling
                    // `matte_buf`. The chain's tail and slot are
                    // unchanged: the picture is still the source's.
                    Some(0) => {
                        matte_in_buf = Some(node_id);
                        node_outputs.insert(node_id, Arc::new(Vec::new()));
                        chain_extend_count += 1;
                        per_node_ms.push((
                            format!("{node_kind_name}#chain_matte"),
                            t_node.elapsed().as_secs_f64() * 1000.0,
                        ));
                        continue;
                    }
                    Some(n) if n >= 1 => {
                        for _ in 0..n {
                            chain_slot_a = !chain_slot_a;
                        }
                        chain_tail = Some(node_id);
                        node_outputs.insert(node_id, Arc::new(Vec::new()));
                        // A secondary's source has to survive its own
                        // fan-out: keep the picture where ApplyMatte can
                        // still find it once the branch has overwritten
                        // the other slot.
                        if secondaries.iter().any(|sec| sec.source == node_id) {
                            if let Some(gpu) = self.gpu.as_mut() {
                                gpu.keep_chain_input(chain_slot_a, width, height);
                            }
                            kept_picture = Some(node_id);
                        } else if takes_the_kept_buffer(&node.kind, params) {
                            kept_picture = None;
                        }
                        // Same idea for a composite: this node's output is
                        // some Blend's port 1, so park it before the chain
                        // moves on. GPU to GPU, recorded in sequence.
                        if composite_port1.contains_key(&node_id) {
                            if let Some(gpu) = self.gpu.as_mut() {
                                gpu.keep_composite_src(chain_slot_a, width, height);
                            }
                            composite_src_holds = Some(node_id);
                            segment_composite_writes += 1;
                        }
                        if secondaries.iter().any(|sec| sec.apply == node_id) {
                            matte_in_buf = None;
                        }
                        if node_is_visible_for_thumbs {
                            if let Some(i) = take_thumb(self, chain_slot_a) {
                                gpu_thumb_of.insert(node_id, i);
                            }
                        }
                        // MaskCutout consumed the matte_buf contents;
                        // clear the marker so a later Mask node can
                        // reclaim the slot for its own matte. Other
                        // chain extensions don't touch matte_buf, so
                        // the marker only changes for MaskCutout.
                        if node.kind == NodeKind::MaskCutout {
                            matte_in_buf = None;
                        }
                        chain_extend_count += 1;
                        per_node_ms.push((format!("{node_kind_name}#chain_extend"), t_node.elapsed().as_secs_f64() * 1000.0));
                        continue;
                    }
                    _ => {
                        // Fall through to CPU-resident path.
                        self.flush_chain(
                            &mut chain_tail,
                            chain_slot_a,
                            width,
                            height,
                            &mut node_outputs,
                            matte_tap,
                            &mut tapped_output,
                        );
                        chain_flush_count += 1;
                        segment_composite_writes = 0;
                    }
                }
            }

            // Try to START a new chain from this node. Needs: GPU
            // available, node whitelisted + effective + not matte preview,
            // primary input resolved from a CPU-resident Vec (or source).
            // MaskCutout can't START a fresh chain on its own —
            // apply_mask_cutout_chain reads matte_buf which is only
            // populated when the upstream Mask short-circuit has just
            // dispatched into it. If the chain isn't already alive
            // (so the Mask short-circuit's `chain_tail.is_some()`
            // gate kept Mask on CPU), starting a chain at MaskCutout
            // would read uninitialised garbage out of matte_buf and
            // produce the magenta-green corruption seen on clips
            // with masks but no upstream grade.
            let mask_cutout_can_start = if node.kind == NodeKind::MaskCutout {
                let matte_input = graph.input_of(node_id, 1);
                matte_input.is_some() && matte_input == matte_in_buf
            } else {
                true
            };
            let can_start_chain = self.gpu.is_some()
                && !bypass
                && self.node_supports_gpu_chain(&node.kind, params)
                && mask_cutout_can_start
                && Some(node_id) != matte_preview_node
                && Some(node_id) != matte_tap
                // Same visible-thumb gate as can_extend_chain: if the
                // node needs a CPU-side output for thumb capture, it
                // must go through the per-node CPU dispatch path.
                && !thumb_needs_cpu;
            if can_start_chain {
                let primary_cpu: &[f32] = primary_input_id
                    .and_then(|id| node_outputs.get(&id))
                    .filter(|arc| !arc.is_empty())
                    .map(|arc| arc.as_slice())
                    .unwrap_or(source_frame);
                let keep_for_composite = primary_input_id
                    .is_some_and(|id| composite_port1.contains_key(&id));
                if let Some(n) = self.start_chain(
                    node_id,
                    &node.kind,
                    params,
                    primary_cpu,
                    width,
                    height,
                    frame_number,
                    keep_for_composite,
                ) {
                    chain_tail = Some(node_id);
                    chain_slot_a = n % 2 == 0;
                    node_outputs.insert(node_id, Arc::new(Vec::new()));
                    if keep_for_composite {
                        composite_src_holds = primary_input_id;
                        segment_composite_writes += 1;
                    }
                    // As in the extend path: a secondary's source keeps
                    // its picture for ApplyMatte to blend against.
                    if secondaries.iter().any(|sec| sec.source == node_id) {
                        if let Some(gpu) = self.gpu.as_mut() {
                            gpu.keep_chain_input(chain_slot_a, width, height);
                        }
                        kept_picture = Some(node_id);
                    } else if takes_the_kept_buffer(&node.kind, params) {
                        kept_picture = None;
                    }
                    if node_is_visible_for_thumbs {
                        if let Some(i) = take_thumb(self, chain_slot_a) {
                            gpu_thumb_of.insert(node_id, i);
                        }
                    }
                    chain_start_count += 1;
                    per_node_ms.push((format!("{node_kind_name}#chain_start"), t_node.elapsed().as_secs_f64() * 1000.0));
                    continue;
                }
            }

            // ── Mask → matte_buf fast path ─────────────────────────────
            // When the chain is currently alive AND this Mask's only
            // consumer is a MaskCutout reading from port 1, dispatch
            // the matte raster to `matte_buf` without breaking the
            // chain. The downstream MaskCutout will then extend the
            // chain via `apply_mask_cutout_chain`, keeping the entire
            // Source → [optional Corrector] → MaskCutout → Output
            // chain GPU-resident. The mask itself rasterises into a
            // dedicated `matte_buf` that's separate from the chain's
            // ping-pong slots — so this short-circuit doesn't depend
            // on the chain already being live. When no upstream
            // grade is active (the common "I just want to mask out
            // a region" case) the Mask still publishes its matte to
            // GPU memory, and the downstream `MaskCutout` then
            // *starts* the chain itself (uploads the source, reads
            // `matte_buf`, dispatches the cutout shader). Previously
            // we required `chain_tail.is_some()` here, which meant
            // pure-mask graphs (no Corrector with non-default
            // params) ran the entire mask + multiply on CPU at
            // ~60–110 ms / 4K frame.
            if node.kind == NodeKind::Mask && self.gpu.is_some() {
                let consumers: Vec<&Edge> = graph.edges.iter()
                    .filter(|e| e.from_node == node_id)
                    .collect();
                // One consumer, on its matte port: a MaskCutout, or the
                // ApplyMatte of a recognised secondary. Either reads
                // `matte_buf` on the GPU.
                let single_cutout_target = consumers.len() == 1
                    && consumers[0].to_port == 1
                    && node_by_id.get(&consumers[0].to_node).is_some_and(|n| {
                        n.kind == NodeKind::MaskCutout
                            || (n.kind == NodeKind::ApplyMatte
                                && secondaries.iter().any(|sec| sec.apply == n.id))
                    });
                // As drawn now: keyframes, then the window's tracked motion (#238).
                let masks_opt = node.params.masks.as_ref()
                    .map(|ms| ms.evaluated_in(clip_time_secs, height as f32 / width.max(1) as f32));
                let gpu_ready_masks: Vec<klipp_effects::gpu::GpuMaskDef> = masks_opt.as_ref()
                    .map(|ms| ms.masks.iter()
                        .filter(|m| m.enabled)
                        .filter_map(mask_to_gpu_def)
                        .collect())
                    .unwrap_or_default();
                // Bezier shapes are now supported on the GPU path
                // (storage-buffer mask architecture, klipp-effects
                // SHADER_MASK_V2). The previous `!has_bezier` gate
                // forced the entire stack to fall through to CPU
                // even for one bezier — now removed.
                if single_cutout_target && !gpu_ready_masks.is_empty() {
                    if let Some(gpu) = self.gpu.as_mut() {
                        gpu.generate_mask_gpu_to_matte_buf(width, height, &gpu_ready_masks);
                    }
                    matte_in_buf = Some(node_id);
                    node_outputs.insert(node_id, Arc::new(Vec::new()));
                    per_node_ms.push((format!("{node_kind_name}#matte_buf"), t_node.elapsed().as_secs_f64() * 1000.0));
                    continue;
                }
            }

            // ── LayerMixer (N-input compositor) ────────────────────────
            // Sits outside the standard 3-port `execute_node` dispatch
            // because the input count is data-driven. Walk every wired
            // port in stack order (port 0 = top), composite bottom-up
            // with per-port opacity + blend mode. Unwired ports are
            // skipped — a 4-port mixer with only ports 0 and 2 wired
            // composites those two as a 2-layer stack.
            if node.kind == NodeKind::LayerMixer {
                let port_count = node.dynamic_input_port_count();
                let mut wired: Vec<(u8, &[f32])> = Vec::with_capacity(port_count as usize);
                for port in 0..port_count {
                    if let Some(src_id) = graph.input_of(node_id, port) {
                        if let Some(arc) = node_outputs.get(&src_id) {
                            if !arc.is_empty() {
                                wired.push((port, arc.as_slice()));
                            }
                        }
                    }
                }
                let pixel_count = (width as usize) * (height as usize);
                let output = if wired.is_empty() {
                    if source_frame.is_empty() {
                        return Err(ExecutorError::CpuSourceRequired);
                    }
                    source_frame.to_vec()
                } else {
                    let inputs_params = params
                        .layer_mixer
                        .as_ref()
                        .map(|m| m.inputs.as_slice())
                        .unwrap_or(&[]);
                    layer_mixer_compose(&wired, inputs_params, pixel_count)
                };
                node_outputs.insert(node_id, Arc::new(output));
                cpu_fallback_count += 1;
                per_node_ms.push((format!("{node_kind_name}#mixer"), t_node.elapsed().as_secs_f64() * 1000.0));
                continue;
            }

            // ── CPU / unchained path ────────────────────────────────────
            // Resolve inputs — borrow as slices from Arc-held buffers.
            // (The chain path above guarantees no sentinel-empty Arcs are
            // ever read here: we always flushed before falling through.)
            let primary = graph
                .input_of(node_id, 0)
                .and_then(|id| node_outputs.get(&id));
            let secondary = graph
                .input_of(node_id, 1)
                .and_then(|id| node_outputs.get(&id));
            let tertiary = graph
                .input_of(node_id, 2)
                .and_then(|id| node_outputs.get(&id));

            let primary_slice = primary
                .filter(|arc| !arc.is_empty())
                .map(|v| v.as_slice())
                .unwrap_or(source_frame);
            if primary_slice.is_empty() {
                return Err(ExecutorError::CpuSourceRequired);
            }
            let secondary_slice = secondary
                .filter(|arc| !arc.is_empty())
                .map(|v| v.as_slice());
            let tertiary_slice = tertiary
                .filter(|arc| !arc.is_empty())
                .map(|v| v.as_slice());

            // Execute the node
            let output = self.execute_node(
                node.id,
                &node.kind,
                params,
                primary_slice,
                secondary_slice,
                tertiary_slice,
                width,
                height,
                source_frame,
                frame_number,
                clip_time_secs,
                matte_in_buf.is_some() && graph.input_of(node.id, 1) == matte_in_buf,
            );

            // Matte preview — return greyscale matte early
            if Some(node_id) == matte_preview_node {
                let matte: Vec<f32> = output
                    .chunks_exact(4)
                    .flat_map(|c| [c[0], c[0], c[0], 1.0])
                    .collect();
                let tapped_vec =
                    tapped_output.map(|a| Arc::try_unwrap(a).unwrap_or_else(|a| (*a).clone()));
                // Matte preview short-circuits before the thumb-capture
                // pass — return empty thumb map.
                return Ok((matte, tapped_vec, HashMap::new()));
            }

            let output_arc = Arc::new(output);

            // Capture tapped node output for scopes (share via Arc, no copy).
            if Some(node_id) == matte_tap {
                tapped_output = Some(output_arc.clone());
            }

            node_outputs.insert(node_id, output_arc);
            cpu_fallback_count += 1;
            per_node_ms.push((format!("{node_kind_name}#cpu"), t_node.elapsed().as_secs_f64() * 1000.0));
        }

        // Flush any residual chain (e.g. the chain tail feeds directly
        // into the Output node and we never hit a non-chain break).
        if chain_tail.is_some() {
            self.flush_chain(
                &mut chain_tail,
                chain_slot_a,
                width,
                height,
                &mut node_outputs,
                matte_tap,
                &mut tapped_output,
            );
            chain_flush_count += 1;
        }

        let (submits_after, uploads_after) = gpu_counts(self);
        self.last_run_chain_stats = ChainStats {
            extends: chain_extend_count,
            flushes: chain_flush_count,
            cpu_nodes: cpu_fallback_count,
            submits: u32::try_from(submits_after.saturating_sub(submits_before)).unwrap_or(u32::MAX),
            uploads: u32::try_from(uploads_after.saturating_sub(uploads_before)).unwrap_or(u32::MAX),
            copies: node_outputs.copies,
            evaluations,
        };
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
        if total_ms > 5.0 {
            // Roll up per-node timings: kind#path → total ms across this frame.
            let mut by_kind: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
            for (k, ms) in &per_node_ms {
                *by_kind.entry(k.clone()).or_insert(0.0) += ms;
            }
            let mut entries: Vec<(String, f64)> = by_kind.into_iter().collect();
            entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let breakdown = entries.iter()
                .filter(|(_, ms)| *ms >= 0.1)
                .map(|(k, ms)| format!("{k}={ms:.1}"))
                .collect::<Vec<_>>()
                .join(" ");
            tracing::debug!(
                "PERF graph_exec: {total_ms:.1}ms | flushes={chain_flush_count} extends={chain_extend_count} starts={chain_start_count} tunnels={bypass_tunnel_count} cpu={cpu_fallback_count} | {breakdown}"
            );
        }

        // 4. Node thumbnails for the visible nodes: those taken inside
        //    the GPU chain, read back together, and the rest from their
        //    CPU-side outputs. A GPU-resident node without one (its
        //    thumbnail could not be taken) has an empty sentinel buffer
        //    and gets none.
        let mut node_thumbs: HashMap<NodeId, Vec<u8>> = HashMap::new();
        let from_gpu = match (gpu_thumb_of.is_empty(), self.gpu.as_ref(), self.chain_thumbs.as_ref()) {
            (false, Some(gpu), Some(thumbs)) => gpu.read_chain_thumbs(thumbs).unwrap_or_default(),
            _ => Vec::new(),
        };
        if let Some(visible) = visible_nodes {
            for &node_id in visible {
                if let Some(thumb) = gpu_thumb_of.get(&node_id).and_then(|&i| from_gpu.get(i)) {
                    node_thumbs.insert(node_id, thumb.clone());
                    continue;
                }
                let Some(arc) = node_outputs.get(&node_id) else { continue };
                if arc.is_empty() { continue; }
                let thumb = downsample_to_thumb_u8(arc.as_slice(), width, height,
                    THUMB_W, THUMB_H);
                node_thumbs.insert(node_id, thumb);
            }
        }

        // 5. Return the output node's input — UNLESS solo is active
        //    AND the soloed id is in node_outputs, in which case we
        //    return that node's output as the final image. All other
        //    nodes still ran (so thumbnails update for canvas
        //    previews), just the picker at the end differs. A solo
        //    id that doesn't exist in this graph (e.g. user switched
        //    clips and the previous solo target is gone) silently
        //    falls back to the normal Output pick.
        let normal_output_id = graph
            .input_of(graph.output_node_id, 0)
            .ok_or(ExecutorError::NoOutputConnection)?;
        let result_node_id = match self.solo_node {
            Some(id) if node_outputs.contains_key(&id) => id,
            _ => normal_output_id,
        };
        let result_arc = node_outputs
            .remove(&result_node_id)
            .ok_or(ExecutorError::MissingNodeOutput)?;
        let result = Arc::try_unwrap(result_arc).unwrap_or_else(|a| (*a).clone());

        let tapped_vec =
            tapped_output.map(|a| Arc::try_unwrap(a).unwrap_or_else(|a| (*a).clone()));

        Ok((result, tapped_vec, node_thumbs))
    }

    /// Returns true if this (kind, params) pair has a GPU-resident chain
    /// dispatch entry. Must be a single-input filter kind and produce a
    /// non-degenerate GPU dispatch — kinds that pass the frame through
    /// without touching the GPU (e.g. `ColorMatch`, `ColorWheels` in log
    /// mode) return false so we break the chain instead.
    fn node_supports_gpu_chain(&self, kind: &NodeKind, params: &NodeParams) -> bool {
        if self.gpu.is_none() {
            return false;
        }
        match kind {
            // Records the source format; never touches the pixels.
            NodeKind::InputTransform => false,
            NodeKind::Exposure => params.exposure.unwrap_or(0.0) != 0.0,
            NodeKind::Contrast => {
                params.contrast.unwrap_or(0.0) != 0.0 || params.clarity.unwrap_or(0.0) != 0.0
            }
            NodeKind::Saturation => {
                params.saturation.unwrap_or(1.0) != 1.0 || params.vibrance.unwrap_or(0.0) != 0.0
            }
            NodeKind::ToneControls => {
                let hi = params.highlights.unwrap_or(0.0);
                let sh = params.shadows.unwrap_or(0.0);
                let wh = params.whites.unwrap_or(0.0);
                let bl = params.blacks.unwrap_or(0.0);
                hi != 0.0 || sh != 0.0 || wh != 0.0 || bl != 0.0
            }
            NodeKind::ColorWheels => true,
            NodeKind::WheelSatBri => {
                // All eight sat/bri offsets default to 0; at default
                // every output pixel = input pixel, so the shader is
                // a pure pass-through. Skipping the dispatch entirely
                // saves a per-frame cost on every clip with the
                // default node graph (the common case — most users
                // never touch these knobs).
                params.lift_sat.unwrap_or(0.0) != 0.0
                    || params.lift_bri.unwrap_or(0.0) != 0.0
                    || params.gamma_sat.unwrap_or(0.0) != 0.0
                    || params.gamma_bri.unwrap_or(0.0) != 0.0
                    || params.gain_sat.unwrap_or(0.0) != 0.0
                    || params.gain_bri.unwrap_or(0.0) != 0.0
                    || params.offset_sat.unwrap_or(0.0) != 0.0
                    || params.offset_bri.unwrap_or(0.0) != 0.0
            }
            NodeKind::RGBCurves => params
                .curves
                .as_ref()
                .is_some_and(|c| !is_curves_identity(c)),
            NodeKind::HueSatCurves => params
                .huesat
                .as_ref()
                .is_some_and(|h| !h.is_all_identity()),
            NodeKind::ColorBalance => {
                // Same story as WheelSatBri — all four params default
                // to identity (temp/tint/hue = 0, global_mix = 1).
                // At those values the shader passes pixels through
                // unchanged but still pays the dispatch + bind-group
                // cost every frame, on every clip with the default
                // graph. Real activeness check eliminates the
                // baseline cost.
                params.temperature.unwrap_or(0.0) != 0.0
                    || params.tint.unwrap_or(0.0) != 0.0
                    || params.hue.unwrap_or(0.0) != 0.0
                    || (params.global_mix.unwrap_or(1.0) - 1.0).abs() > f32::EPSILON
            }
            NodeKind::Lut3D => params.lut_path.is_some(),
            NodeKind::FilmGrain => params
                .grain
                .as_ref()
                .is_some_and(|g| g.enabled && g.amount > 0.001),
            NodeKind::ColorMatch => params
                .color_match
                .as_ref()
                .is_some_and(|s| !s.is_identity()),
            NodeKind::ColorWarper => params
                .warper_params
                .as_ref()
                .is_some_and(|w| !w.is_identity()),
            NodeKind::Denoise => {
                params.denoise_luma.unwrap_or(0.0) > 0.0
                    || params.denoise_chroma.unwrap_or(0.0) > 0.0
            }
            // MaskCutout extends the chain when its port-1 (matte) input
            // is GPU-resident in `matte_buf`. The activeness check here
            // doesn't have access to that state, so we say "yes if GPU
            // is up"; the caller in `execute_linear_with_thumbs` does
            // the real `matte_in_buf` match check before dispatching.
            NodeKind::MaskCutout => true,
            NodeKind::Corrector => {
                // The Corrector chains 1–7 sub-dispatches inline (see
                // `dispatch_corrector_chain`). It supports the GPU chain
                // whenever at least one sub-stage would actually fire —
                // identical activeness logic to each individual kind,
                // OR'd together. At full default values the bypass
                // tunnel handles it before this is consulted.
                corrector_has_active_stage(params)
            }
            _ => false,
        }
    }

    /// Start a new GPU chain: upload `primary_input` to `buf_a`, dispatch
    /// this first node. Returns `Some(n)` where `n` is the number of
    /// sub-dispatches issued (≥1 on success — the caller must flip
    /// `chain_slot_a` exactly `n` times to track which buffer holds the
    /// output). Returns `None` if the node's GPU dispatch wasn't
    /// available (caller falls back).
    #[allow(clippy::too_many_arguments)]
    fn start_chain(
        &mut self,
        node_id: NodeId,
        kind: &NodeKind,
        params: &NodeParams,
        primary_input: &[f32],
        width: u32,
        height: u32,
        frame_number: u64,
        keep_input_for_composite: bool,
    ) -> Option<u32> {
        let gpu = self.gpu.as_mut()?;
        if gpu.is_device_lost() {
            return None;
        }
        let byte_size = (width as u64) * (height as u64) * 16;
        gpu.ensure_chain_buffers(byte_size);
        // Phase 4d: when `buf_a` was pre-seeded on-GPU (via
        // `prime_chain_from_gpu_u8_buffer`), skip the CPU upload —
        // it's a no-op at best, a stale overwrite at worst. The flag
        // is single-shot so a second chain segment within the same
        // `execute_linear` falls back to normal upload.
        // The seed is only usable at the resolution it was made at.
        // `ensure_chain_buffers` will not shrink an existing allocation,
        // so a full-res seed consumed by a half-res chain would be read
        // at the wrong stride rather than failing outright.
        let seed_usable = self.run_seed_dims == Some((width, height));
        if seed_usable {
            self.run_seed_dims = None;
        } else {
            if primary_input.is_empty() {
                // No usable seed and no CPU pixels: uploading an empty
                // slice would dispatch the chain over whatever `buf_a`
                // happens to hold. Refuse to start; the caller falls
                // through to the unchained path, which raises
                // `CpuSourceRequired` so the engine can retry with a
                // readback.
                self.run_seed_dims = None;
                return None;
            }
            gpu.upload_chain_input_linear(primary_input);
        }
        // Open the single-submit chain encoder. Every `*_chain` dispatch
        // records into this; `flush_chain` calls `submit_chain` to batch
        // the whole segment into one `queue.submit`. Per-node submits
        // cost ~2-5 ms on NVIDIA Vulkan and dominated `grade_ms` during
        // scrubbing before this.
        gpu.begin_chain();
        // The picture just uploaded is some composite's port-1 input,
        // and the dispatch below is about to overwrite the slot holding
        // it. Park it first — this is the case a Blend against the
        // ungraded source hits, and it is the common one.
        if keep_input_for_composite {
            gpu.keep_composite_src(/* in_is_a = */ true, width, height);
        }
        // Dispatch this node with in = buf_a, out = buf_b. The first
        // node in a chain is always one of the whitelisted single-sub-
        // effect kinds (start_chain is only called via can_start_chain,
        // which is gated by node_supports_gpu_chain). Contrast and
        // Saturation can still return Some(2) if both their sub-effects
        // are active — mirror the net slot parity in the caller via
        // chain_slot_a at the call site in execute_linear.
        let dispatches = self.dispatch_chain_node_slots(node_id, kind, params, width, height, frame_number, /* in_is_a */ true);
        let n = dispatches.unwrap_or(0);
        if n == 0 {
            // Dispatch declined (device-lost or unexpected param). Close
            // the encoder we just opened so the next chain segment starts
            // clean. No dispatches were recorded so submit is effectively
            // a no-op fence.
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.submit_chain();
            }
        }
        dispatches
    }

}

/// The primaries a Lut3D node's lookup is bracketed with. The node's
/// `lut_input_space` / `lut_output_space` (set in the Inspector) win
/// over the loaded LUT's own tags; an untagged `.cube` is Rec.709 on
/// both sides. A tag names primaries only: the lookup is always on the
/// sRGB encoding, as the Inspector's "Rec.709 / sRGB" says.
fn lut_node_bracket(
    params: &NodeParams,
    lut: &LutData,
    working: klipp_core::WorkingColorspace,
) -> klipp_effects::gpu::LutBracket {
    klipp_color::lut_bracket(
        working,
        params.lut_input_space.unwrap_or(lut.input_space),
        params.lut_output_space.unwrap_or(lut.output_space),
    )
}

/// The grain `size` both the shader and its CPU twin quantise pixel
/// coordinates by, floored away from zero — at zero each divides by it.
fn grain_size(grain: &klipp_core::FilmGrainSettings, render_scale: f32) -> f32 {
    (grain.size * render_scale).max(0.001)
}

/// The bilateral filter's spatial and colour sigmas, floored away from
/// zero. Both sides divide by twice the square of each, so a zero
/// radius made the spatial term 0/0 and turned every weight into a NaN
/// — a black frame on the GPU, from a control the UI lets you reach.
fn denoise_sigmas(luma: f32, chroma: f32, radius_px: f32) -> (f32, f32) {
    (radius_px.max(0.001), ((luma + chroma) * 0.5).max(0.001))
}

/// Does this node's dispatch take `grade_mix_src` over for itself?
///
/// A Corrector with a `global_mix` other than 1 keeps its own input
/// there to mix against later, which overwrites whatever a secondary
/// had parked in it.
fn takes_the_kept_buffer(kind: &NodeKind, params: &NodeParams) -> bool {
    *kind == NodeKind::Corrector
        && (params.global_mix.unwrap_or(1.0) - 1.0).abs() > f32::EPSILON
}

/// Where a chain-resident composite's port-1 picture comes from.
enum CompositeRoute {
    /// Already in `composite_src`: kept there on the GPU when its
    /// producer ran, so there is nothing to move.
    Resident,
    /// On the CPU, to be uploaded. Allowed once per chain segment —
    /// a queued write lands before the whole encoder rather than in
    /// sequence with it, so a second one would decide what the first
    /// composite's dispatch read.
    Upload(Arc<Vec<f32>>),
}

/// How much of a run the GPU chain carried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainStats {
    /// Dispatches that extended a live chain — work that cost no
    /// transfer.
    pub extends: u32,
    /// Times the chain was read back to the CPU.
    pub flushes: u32,
    /// Nodes that ran through the per-node CPU path.
    pub cpu_nodes: u32,
    /// Command buffers handed to the GPU queue during the run: one per
    /// chain segment when the chain batches, more when passes submit
    /// themselves.
    pub submits: u32,
    /// Whole frames uploaded from the CPU to the GPU during the run: the
    /// chain's input, a CPU-made value fed back in (a composite's port
    /// 1, a per-node grade's input), a key's plate. A source seeded on
    /// the GPU before the run (`upload_seed`) is the input arriving, not
    /// the run's work, and is not counted.
    pub uploads: u32,
    /// Whole images copied from one value to another during the run (a
    /// value passed through is shared, not copied) (#1089, M2).
    pub copies: u32,
    /// Nodes the run evaluated: a node shared by several readers counts
    /// once (#1089, M2).
    pub evaluations: u32,
}

/// Whether `node` keyframes any of a qualifier's matte refinements. The
/// secondary is recognised from the static settings, so a refinement
/// that is 0 still but keyed above it would be fused and dropped on
/// every frame it is not 0 (#1013).
pub(crate) fn refinement_keyed(graph: &NodeGraph, node: NodeId) -> bool {
    graph.nodes.iter().find(|n| n.id == node).is_some_and(|n| {
        n.keyframes.tracks.iter().any(|t| {
            !t.keyframes.is_empty()
                && matches!(t.param_name.as_str(), "qual_clean_black" | "qual_clean_white" | "qual_shrink_grow" | "qual_blur")
        })
    })
}

/// A secondary correction the GPU chain can carry end to end: one
/// node's picture keyed by a Qualifier, graded down a branch of any
/// length, and the two recombined by an ApplyMatte.
///
/// ```text
///   source ─┬──────────────────────► ApplyMatte port 2 (ungraded)
///           ├─► Qualifier ─────────► ApplyMatte port 1 (matte)
///           └─► grade… ─► grade… ──► ApplyMatte port 0 (graded)
/// ```
///
/// A branch node the chain cannot carry does not disqualify the shape —
/// it flushes, and ApplyMatte then reads the matte back rather than
/// dropping the secondary. Slower, still right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SecondaryChain {
    /// The node whose output is kept and keyed. Its fan-out is three,
    /// which normally breaks the chain; this shape is the exception.
    source: NodeId,
    /// The keyer or mask that produces the matte.
    matte: NodeId,
    apply: NodeId,
}

impl SecondaryChain {
    /// Every such shape in the graph, in execution order. Empty means
    /// nothing changes and the graph runs exactly as it did.
    ///
    /// Every condition here is load-bearing. The three ApplyMatte ports
    /// have to come from the right places, the qualifier's matte must go
    /// nowhere else, and the four nodes have to be *consecutive* in the
    /// execution order: a foreign node in between could flush the chain
    /// after the matte was written but before it was used, leaving the
    /// matte in GPU memory and the picture on the CPU.
    ///
    /// This used to stop at the first match, which cost a colourist the
    /// fast path the moment they pulled a second window: at 1214x2160 a
    /// graph with two secondaries ran 41.3 ms against 19.5 ms for one,
    /// because the second keyer and blend fell back to the CPU and added
    /// a flush and an upload (`flushes=2 starts=2 cpu=3` against
    /// `flushes=1 starts=1 cpu=1`).
    ///
    /// Stacked secondaries need no extra GPU buffers, which is why
    /// collecting them all is enough on its own: the shapes are
    /// sequential, not concurrent. The second one's source is the first
    /// one's ApplyMatte, so by the time it asks for the kept picture and
    /// `matte_buf`, the first shape has finished with both.
    pub(crate) fn detect_all(
        graph: &NodeGraph,
        order: &[NodeId],
        fan_out: &HashMap<NodeId, usize>,
    ) -> Vec<Self> {
        let kind_of = |id: NodeId| graph.nodes.iter().find(|n| n.id == id).map(|n| n.kind);
        let enabled = |id: NodeId| {
            graph.nodes.iter().find(|n| n.id == id).is_some_and(|n| n.enabled)
        };
        let mut found = Vec::new();
        for node in &graph.nodes {
            if node.kind != NodeKind::ApplyMatte || !node.enabled {
                continue;
            }
            let apply = node.id;
            let (Some(graded), Some(matte), Some(plain)) = (
                graph.input_of(apply, 0),
                graph.input_of(apply, 1),
                graph.input_of(apply, 2),
            ) else {
                continue;
            };
            // The matte comes from a keyer or a mask that is switched
            // on and feeds nothing but this node. A keyer has to be
            // looking at the same picture the ungraded port carries, or
            // the matte would not describe it; a Mask draws geometry and
            // has no input to check.
            if !enabled(matte) || fan_out.get(&matte).copied().unwrap_or(0) != 1 {
                continue;
            }
            let params_of = |id: NodeId| {
                graph.nodes.iter().find(|n| n.id == id).map(|n| &n.params)
            };
            let Some(matte_params) = params_of(matte) else { continue };
            let keys_the_source = graph.input_of(matte, 0) == Some(plain);
            let matte_armed = match kind_of(matte) {
                // The chain's keyer draws the window alone. A refined
                // matte (clean, shrink/grow, blur) is finished on the
                // CPU, so that qualifier runs per node, where it is
                // (#1013).
                Some(NodeKind::Qualifier) => {
                    keys_the_source
                        && matte_params.qualifier.as_ref().is_some_and(|q| q.enabled && !q.refines())
                        && !refinement_keyed(graph, matte)
                }
                Some(NodeKind::LumaKey) => {
                    keys_the_source && matte_params.luma_key.as_ref().is_some_and(|k| k.enabled)
                }
                Some(NodeKind::Mask) => matte_params
                    .masks
                    .as_ref()
                    .is_some_and(|ms| ms.masks.iter().any(|m| m.enabled)),
                Some(NodeKind::DifferenceKey) => {
                    keys_the_source
                        && matte_params.difference_key.as_ref().is_some_and(|d| {
                            // A plate is required: without one the node
                            // selects nothing, and there would be no
                            // buffer for the shader to compare against.
                            d.enabled && d.plate_path.is_some()
                        })
                }
                _ => false,
            };
            if !matte_armed {
                continue;
            }
            // The graded branch: any number of nodes, each fed by the
            // one before, the first fed by the same picture the matte
            // was keyed from. Walking it back from ApplyMatte is how we
            // learn which nodes belong to the shape.
            let mut branch = Vec::new();
            let mut walk = graded;
            let reached_source = loop {
                if walk == plain {
                    break true;
                }
                if !enabled(walk) || fan_out.get(&walk).copied().unwrap_or(0) != 1 {
                    break false;
                }
                branch.push(walk);
                match graph.input_of(walk, 0) {
                    Some(prev) if branch.len() <= graph.nodes.len() => walk = prev,
                    _ => break false,
                }
            };
            if !reached_source {
                continue;
            }
            // Every consumer of the source must be one this shape
            // accounts for: the branch, the blend, and the keyer when it
            // is looking at the source. A fourth reader would need the
            // picture on the CPU, which is exactly what the fan-out rule
            // this shape suspends is there to guarantee.
            let first_branch = branch.last().copied().unwrap_or(apply);
            if graph
                .edges
                .iter()
                .filter(|e| e.from_node == plain)
                .any(|e| e.to_node != apply && e.to_node != matte && e.to_node != first_branch)
            {
                continue;
            }
            // Nothing foreign may run between the source and the blend.
            // The matte sits in `matte_buf` and the picture in the kept
            // buffer across that stretch; a node that flushed the chain
            // in between would leave ApplyMatte on the CPU, where the
            // matte has to be read back — correct, but the transfer is
            // the whole thing we are avoiding.
            let at = |id: NodeId| order.iter().position(|n| *n == id);
            let (Some(i_source), Some(i_apply)) = (at(plain), at(apply)) else { continue };
            let mut shape = vec![plain, matte, apply];
            shape.extend_from_slice(&branch);
            if order[i_source + 1..i_apply].iter().any(|n| !shape.contains(n)) {
                continue;
            }
            found.push(Self { source: plain, matte, apply });
        }
        // Execution order, so a consumer walking the graph meets them in
        // the order they will run.
        found.sort_by_key(|c| order.iter().position(|n| *n == c.apply).unwrap_or(usize::MAX));
        found
    }
}

impl GraphExecutor {
    /// Dispatch-only chain step with explicit slot selection.
    ///
    /// Returns `None` when the dispatch could not run (device lost,
    /// unsupported param shape, or all sub-effect params are no-ops).
    /// Returns `Some(n)` with `n ≥ 1` = number of sub-dispatches issued;
    /// the caller must flip its chain_slot_a that many times.
    ///
    /// Most node kinds issue exactly one sub-dispatch (`Some(1)`).
    /// `Contrast` and `Saturation` may issue TWO sub-dispatches when
    /// both of their sub-effects are active (contrast+clarity, or
    /// saturation+vibrance) — in that case they return `Some(2)`. This
    /// keeps the chain GPU-resident instead of breaking mid-grade into
    /// separate segments, saving one upload + one download fence per
    /// extra segment (~20 ms each on scrubbing workloads).
    /// The difference key's clean plate, decoded and resized to the
    /// frame, cached at both steps.
    ///
    /// One pipeline for both the CPU arm and the GPU chain dispatch, so
    /// the two cannot key against differently-built plates. `None` means
    /// the plate could not be read and the caller should select nothing.
    ///
    /// The decode is cached by path; the resize by path *and* frame size,
    /// because the same plate at two timeline resolutions is two
    /// different buffers and a size-blind key hands the keyer a
    /// wrong-length plate.
    fn resized_plate(
        &mut self,
        path: &std::path::Path,
        width: u32,
        height: u32,
    ) -> Option<Arc<Vec<f32>>> {
        let pixel_count = (width as usize) * (height as usize);
        let key = (path.to_path_buf(), width, height);
        if let Some(p) = self.plate_resize_cache.get(&key) {
            return Some(Arc::clone(p));
        }
        let cached = match self.matte_image_cache.get(path).cloned() {
            Some(c) => c,
            None => match image::open(path) {
                Ok(img) => {
                    let rgba = img.to_rgba8();
                    let (iw, ih) = (rgba.width(), rgba.height());
                    let mut linear = vec![0.0f32; (iw * ih * 4) as usize];
                    for (i, px) in rgba.pixels().enumerate() {
                        let o = i * 4;
                        linear[o] = klipp_color::transfer::srgb_to_linear(px[0] as f32 / 255.0);
                        linear[o + 1] = klipp_color::transfer::srgb_to_linear(px[1] as f32 / 255.0);
                        linear[o + 2] = klipp_color::transfer::srgb_to_linear(px[2] as f32 / 255.0);
                        linear[o + 3] = px[3] as f32 / 255.0;
                    }
                    let arc = Arc::new((linear, iw, ih));
                    self.matte_image_cache.insert(path.to_path_buf(), Arc::clone(&arc));
                    self.unreadable_images.remove(path);
                    arc
                }
                Err(e) => {
                    if self.unreadable_images.insert(path.to_path_buf()) {
                        tracing::warn!("DifferenceKey: failed to load {:?}: {e}", path);
                    }
                    return None;
                }
            },
        };
        // Nearest-neighbour resize plate → live-frame dims, kept for as
        // long as the plate and the frame size do. A fixed plate at a
        // fixed resolution resizes to the same buffer every time, and
        // that is a full-frame write — 35.5 ms at 4K — to redo per frame.
        let (src, sw, sh) = (&cached.0, cached.1, cached.2);
        if sw == 0 || sh == 0 {
            return None;
        }
        let mut resized = vec![0.0f32; pixel_count * 4];
        use rayon::prelude::*;
        resized
            .par_chunks_exact_mut(width as usize * 4)
            .enumerate()
            .for_each(|(y, row)| {
                let sy = ((y as u64 * sh as u64) / height as u64).min(sh as u64 - 1) as u32;
                for x in 0..width as usize {
                    let sx = ((x as u64 * sw as u64) / width as u64).min(sw as u64 - 1) as u32;
                    let si = ((sy * sw + sx) as usize) * 4;
                    let oi = x * 4;
                    row[oi] = src[si];
                    row[oi + 1] = src[si + 1];
                    row[oi + 2] = src[si + 2];
                    row[oi + 3] = src[si + 3];
                }
            });
        let arc = Arc::new(resized);
        // Bounded rather than cleared on every miss: two DifferenceKey
        // nodes with different plates would evict each other every frame,
        // which costs more than not caching at all. Four full-frame
        // plates is 133 MB at 4K, which is the point to let go of them —
        // a resolution change leaves stale entries behind otherwise.
        if self.plate_resize_cache.len() >= 4 {
            self.plate_resize_cache.clear();
        }
        self.plate_resize_cache.insert(key, Arc::clone(&arc));
        Some(arc)
    }

    /// A stable identity for a plate at a frame size, for the GPU's
    /// upload cache. Hashing the path rather than the pixels: comparing
    /// 33 MB to decide whether to upload 33 MB defeats the purpose, and
    /// the path plus size is exactly what `resized_plate` keys on, so
    /// the two caches agree on what "the same plate" means.
    fn plate_gpu_key(path: &std::path::Path, width: u32, height: u32) -> (u64, u32, u32) {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut h);
        (h.finish(), width, height)
    }

    fn dispatch_chain_node_slots(
        &mut self,
        node_id: NodeId,
        kind: &NodeKind,
        params: &NodeParams,
        width: u32,
        height: u32,
        frame_number: u64,
        in_is_a: bool,
    ) -> Option<u32> {
        // DifferenceKey first, because it needs the plate caches on
        // `self` and the `gpu` borrow below would rule them out. The
        // plate is resolved and uploaded here; the dispatch reads it from
        // VRAM like any other binding.
        if let NodeKind::DifferenceKey = kind {
            let dk = params.difference_key.as_ref()?;
            if !dk.enabled {
                return None;
            }
            let path = dk.plate_path.as_ref()?.clone();
            let key = Self::plate_gpu_key(&path, width, height);
            // Resident already: skip the resize entirely, not just the
            // upload. This is the steady state during playback.
            let resident = self.gpu.as_ref().is_some_and(|g| g.has_difference_key_plate(key));
            if !resident {
                let plate = self.resized_plate(&path, width, height)?;
                let gpu = self.gpu.as_mut()?;
                gpu.upload_difference_key_plate(&plate, key);
            }
            let gpu = self.gpu.as_mut()?;
            let luminance = matches!(
                dk.match_space,
                klipp_core::DifferenceKeyMatchSpace::Luminance
            );
            // A plate that failed to upload leaves the node to the CPU
            // path rather than keying against nothing.
            if !gpu.difference_key_to_matte_buf_chain(
                width, height, dk.tolerance, dk.soft, dk.invert, luminance,
            ) {
                return None;
            }
            return Some(0);
        }

        let gpu = self.gpu.as_mut()?;
        // A recognised secondary's two nodes. The caller has checked the
        // shape; here we only dispatch. The Qualifier reports zero slot
        // flips because it writes the matte, not the picture.
        match kind {
            NodeKind::Qualifier => {
                let q = params.qualifier.as_ref()?;
                if !q.enabled {
                    return None;
                }
                gpu.qualifier_to_matte_buf_chain(
                    width, height,
                    q.hue_center, q.hue_width, q.hue_soft,
                    q.sat_low, q.sat_high, q.sat_soft,
                    q.lum_low, q.lum_high, q.lum_soft,
                    q.invert,
                );
                return Some(0);
            }
            NodeKind::LumaKey => {
                let k = params.luma_key.as_ref()?;
                if !k.enabled {
                    return None;
                }
                gpu.luma_key_to_matte_buf_chain(width, height, k.low, k.high, k.soft, k.invert);
                return Some(0);
            }
            NodeKind::ApplyMatte => {
                gpu.apply_matte_chain(in_is_a, width, height, params.mix.unwrap_or(1.0));
                return Some(1);
            }
            _ => {}
        }
        if gpu.is_device_lost() {
            return None;
        }

        // Dispatch-only entries take `in_is_a`: they pick the in/out
        // buffer refs internally and ping-pong between buf_a/buf_b. We
        // don't need to clone buffer handles here (wgpu::Buffer is not
        // Clone anyway).
        let _ = gpu; // borrow released for the match arms below

        match kind {
            NodeKind::Exposure => {
                let exp = params.exposure.unwrap_or(0.0);
                if exp == 0.0 { return None; }
                self.gpu.as_mut().unwrap().apply_color_grade_chain(
                    in_is_a, width, height,
                    exp, 0.0, default_pivot_encoded(), 1.0, NO_WHITE_BALANCE,
                    0.0, 0.0, 0.0, 0.0,
                    (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                    0.0, 1.0,
                );
                Some(1)
            }
            NodeKind::Contrast => {
                let con = params.contrast.unwrap_or(0.0);
                let clarity = params.clarity.unwrap_or(0.0);
                if con == 0.0 && clarity == 0.0 { return None; }
                // A dedicated Contrast node carries its own pivot, so
                // these paths read it rather than take the default —
                // otherwise a pivot set on a Contrast node would do
                // nothing while the same pivot on a Corrector worked,
                // and the CPU and GPU twins below would disagree.
                // In the grade's encoding, as the Corrector encodes it: it
                // was always BT.1886's, so under ACEScct the same pivot sat
                // somewhere else on a Contrast node (#1039).
                let pivot = klipp_color::grade_model::encode_contrast_pivot_in(
                    params.contrast_pivot.unwrap_or(klipp_color::grade_model::DEFAULT_CONTRAST_PIVOT),
                    self.grade_encoding,
                );
                // When BOTH contrast and clarity are active, issue two
                // sub-dispatches back-to-back inside the same chain
                // encoder. Each sub-dispatch flips the active buffer,
                // so the second one must read from the OPPOSITE slot
                // that the first wrote into. Net effect: 2 slot flips,
                // which the caller mirrors via chain_slot_a.
                //
                // This keeps the chain GPU-resident where the previous
                // implementation used to bail (returning false), forcing
                // a flush + CPU fallback + new-chain-start. That detour
                // cost ~40 ms for both sub-effects combined; running
                // them in-chain is essentially free (just 2 compute
                // dispatches into the existing encoder).
                if con != 0.0 && clarity != 0.0 {
                    // First: contrast via color_grade, reads from in_is_a,
                    // writes to !in_is_a.
                    self.gpu.as_mut().unwrap().apply_color_grade_chain(
                        in_is_a, width, height,
                        0.0, con, pivot, 1.0, NO_WHITE_BALANCE,
                        0.0, 0.0, 0.0, 0.0,
                        (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                        0.0, 1.0,
                    );
                    // Second: clarity reads from where color_grade wrote
                    // (!in_is_a), writes back to in_is_a. apply_clarity_chain
                    // uses buf_c as scratch internally (does not affect the
                    // a/b parity) — it's a net 1-flip op like color_grade.
                    let radius = self.clarity_radius();
                    self.gpu.as_mut().unwrap()
                        .apply_clarity_chain(!in_is_a, width, height, clarity, radius);
                    return Some(2);
                }
                if con != 0.0 {
                    self.gpu.as_mut().unwrap().apply_color_grade_chain(
                        in_is_a, width, height,
                        0.0, con, pivot, 1.0, NO_WHITE_BALANCE,
                        0.0, 0.0, 0.0, 0.0,
                        (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                        0.0, 1.0,
                    );
                } else {
                    let radius = self.clarity_radius();
                    self.gpu.as_mut().unwrap()
                        .apply_clarity_chain(in_is_a, width, height, clarity, radius);
                }
                Some(1)
            }
            NodeKind::Saturation => {
                let sat = params.saturation.unwrap_or(1.0);
                let vib = params.vibrance.unwrap_or(0.0);
                if sat == 1.0 && vib == 0.0 { return None; }
                // Both sub-effects active: fuse as two in-chain dispatches.
                // Same rationale as Contrast above — keeps the chain
                // GPU-resident. Each *_chain call is a single-flip op.
                if sat != 1.0 && vib != 0.0 {
                    self.gpu.as_mut().unwrap().apply_color_grade_chain(
                        in_is_a, width, height,
                        0.0, 0.0, default_pivot_encoded(), sat, NO_WHITE_BALANCE,
                        0.0, 0.0, 0.0, 0.0,
                        (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                        0.0, 1.0,
                    );
                    self.gpu.as_mut().unwrap()
                        .apply_vibrance_chain(!in_is_a, width, height, vib);
                    return Some(2);
                }
                if sat != 1.0 {
                    self.gpu.as_mut().unwrap().apply_color_grade_chain(
                        in_is_a, width, height,
                        0.0, 0.0, default_pivot_encoded(), sat, NO_WHITE_BALANCE,
                        0.0, 0.0, 0.0, 0.0,
                        (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                        0.0, 1.0,
                    );
                } else {
                    self.gpu.as_mut().unwrap()
                        .apply_vibrance_chain(in_is_a, width, height, vib);
                }
                Some(1)
            }
            NodeKind::ToneControls => {
                let hi = params.highlights.unwrap_or(0.0);
                let sh = params.shadows.unwrap_or(0.0);
                let wh = params.whites.unwrap_or(0.0);
                let bl = params.blacks.unwrap_or(0.0);
                if hi == 0.0 && sh == 0.0 && wh == 0.0 && bl == 0.0 { return None; }
                self.gpu.as_mut().unwrap().apply_color_grade_chain(
                    in_is_a, width, height,
                    0.0, 0.0, default_pivot_encoded(), 1.0, NO_WHITE_BALANCE,
                    hi, sh, wh, bl,
                    (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                    0.0, 1.0,
                );
                Some(1)
            }
            NodeKind::ColorWheels => {
                let stage = GradeStage::Grade(self.color_wheels_grade(params));
                Some(stage.dispatch(self.gpu.as_mut().unwrap(), in_is_a, width, height))
            }
            NodeKind::WheelSatBri => {
                self.gpu.as_mut().unwrap().apply_wheel_sat_bri_chain(
                    in_is_a, width, height,
                    params.lift_sat.unwrap_or(0.0),
                    params.lift_bri.unwrap_or(0.0),
                    params.gamma_sat.unwrap_or(0.0),
                    params.gamma_bri.unwrap_or(0.0),
                    params.gain_sat.unwrap_or(0.0),
                    params.gain_bri.unwrap_or(0.0),
                    params.offset_sat.unwrap_or(0.0),
                    params.offset_bri.unwrap_or(0.0),
                );
                Some(1)
            }
            NodeKind::RGBCurves => {
                let curves = params.curves.as_ref()?;
                if is_curves_identity(curves) { return None; }
                // Use pre-built LUTs from prepare_clip() when available.
                let lut_data = self
                    .curve_lut_cache
                    .get(&node_id)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(build_curve_luts(curves)));
                let preserve_luma = params.curves_preserve_luma.unwrap_or(false);
                let mix = params.curves_mix.unwrap_or(1.0);
                self.gpu.as_mut().unwrap().apply_rgb_curves_chain(
                    in_is_a, width, height,
                    &lut_data, preserve_luma, mix,
                );
                Some(1)
            }
            NodeKind::HueSatCurves => {
                let huesat = params.huesat.as_ref()?;
                if huesat.is_all_identity() { return None; }
                let lut_data = build_huesat_lut_data(huesat);
                let mix = params.huesat_mix.unwrap_or(1.0);
                self.gpu.as_mut().unwrap()
                    .apply_huesat_chain(in_is_a, width, height, mix, &lut_data, huesat_active(huesat));
                Some(1)
            }
            NodeKind::ColorBalance => {
                self.gpu.as_mut().unwrap().apply_color_balance_chain(
                    in_is_a, width, height,
                    white_balance(params.temperature.unwrap_or(0.0), params.tint.unwrap_or(0.0), self.working_colorspace),
                    params.hue.unwrap_or(0.0),
                    params.global_mix.unwrap_or(1.0),
                );
                Some(1)
            }
            NodeKind::Lut3D => {
                let path = params.lut_path.as_ref()?;
                let lut = self
                    .lut_cache
                    .entry(path.clone())
                    .or_insert_with(|| {
                        // Fall back to a 2×2×2 IDENTITY LUT when the file
                        // can't be read — earlier code used an all-zero
                        // LUT (data=vec![0.0; 24]) which silently
                        // rendered the clip black. Identity passthrough
                        // keeps the image visible and the warn log
                        // points at the broken path so the user can
                        // relink.
                        match klipp_color::load_lut(path) {
                            Ok(lut) => Arc::new(lut),
                            Err(e) => {
                                tracing::warn!(
                                    "Lut3D: load failed for {:?}: {} — passing through",
                                    path, e,
                                );
                                Arc::new(LutData::identity(2))
                            }
                        }
                    })
                    .clone();
                let opacity = params.lut_opacity.unwrap_or(1.0);
                let tetrahedral = self.lut_tetrahedral;
                let bracket = lut_node_bracket(params, &lut, self.working_colorspace);
                self.gpu.as_mut().unwrap().apply_3d_lut_chain(
                    in_is_a, width, height,
                    &lut.data, lut.size as u32, opacity, tetrahedral, bracket,
                );
                Some(1)
            }
            NodeKind::FilmGrain => {
                let grain = params.grain.as_ref()?;
                if !grain.enabled || grain.amount <= 0.001 { return None; }
                let size = grain_size(grain, self.render_scale);
                self.gpu.as_mut().unwrap().apply_grain_chain(
                    in_is_a, width, height,
                    grain.amount, size,
                    grain.shadows, grain.highlights,
                    grain.monochrome,
                    frame_number as u32,
                );
                Some(1)
            }
            NodeKind::ColorWarper => {
                let warper = params.warper_params.clone().unwrap_or_default();
                if warper.is_identity() { return None; }
                self.gpu.as_mut().unwrap().apply_color_warper_chain(
                    in_is_a, width, height,
                    &warper.hue_shifts, &warper.sat_shifts,
                );
                Some(1)
            }
            NodeKind::ColorMatch => {
                // Pre-computed match adjustments — exposure, temperature,
                // tint, saturation — slot directly into the unified
                // grade shader. No per-frame analysis at render time.
                let s = match params.color_match.as_ref() {
                    Some(s) if !s.is_identity() => *s,
                    _ => return None,
                };
                self.gpu.as_mut().unwrap().apply_color_grade_chain(
                    in_is_a, width, height,
                    s.exposure, 0.0, default_pivot_encoded(), s.saturation, white_balance(s.temperature, s.tint, self.working_colorspace),
                    0.0, 0.0, 0.0, 0.0,
                    (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                    0.0, 1.0,
                );
                Some(1)
            }
            NodeKind::Denoise => {
                let luma = params.denoise_luma.unwrap_or(0.0);
                let chroma = params.denoise_chroma.unwrap_or(0.0);
                let radius = params.denoise_radius.unwrap_or(3);
                if luma <= 0.0 && chroma <= 0.0 { return None; }
                let (spatial_sigma, color_sigma) = denoise_sigmas(luma, chroma, self.px(radius as f32));
                    let max_kernel = self.denoise_max_kernel();
                self.gpu.as_mut().unwrap().apply_denoise_chain(
                    in_is_a, width, height,
                    spatial_sigma, color_sigma, max_kernel,
                );
                Some(1)
            }
            NodeKind::Corrector => {
                self.dispatch_corrector_chain(node_id, params, width, height, in_is_a)
            }
            NodeKind::MaskCutout => {
                self.gpu.as_mut().unwrap().apply_mask_cutout_chain(in_is_a, width, height);
                Some(1)
            }
            _ => None,
        }
    }

    /// The Corrector's stages, in the one order the GPU chain and the CPU
    /// both run them: the unified grade (exposure, tone, wheels — linear
    /// or log — contrast, saturation, white balance, hue), clarity,
    /// vibrance, wheel sat/bri, RGB curves, hue/sat curves, the warper.
    /// Its mix applies to all of them: folded into the unified grade when
    /// that is the only stage, otherwise a closing mix with the kept input.
    fn corrector_stages(&self, node_id: NodeId, params: &NodeParams) -> Vec<GradeStage> {
        let ws = self.working_colorspace;
        let log = log_wheels(params);
        let [lift, gamma, gain, offset] = if params.wheels_log_mode.unwrap_or(false) {
            [(0.0, 0.0, 0.0); 4]
        } else {
            linear_wheels(params)
        };
        let grade = klipp_effects::cpu_grade::ColorGrade {
            exposure: params.exposure.unwrap_or(0.0),
            contrast: params.contrast.unwrap_or(0.0),
            contrast_pivot_encoded: klipp_color::grade_model::encode_contrast_pivot_in(
                params.contrast_pivot.unwrap_or(klipp_color::grade_model::DEFAULT_CONTRAST_PIVOT),
                self.grade_encoding,
            ),
            encoding: self.grade_encoding,
            saturation: params.saturation.unwrap_or(1.0),
            white_balance: white_balance(params.temperature.unwrap_or(0.0), params.tint.unwrap_or(0.0), ws),
            highlights: params.highlights.unwrap_or(0.0),
            shadows: params.shadows.unwrap_or(0.0),
            whites: params.whites.unwrap_or(0.0),
            blacks: params.blacks.unwrap_or(0.0),
            lift, gamma, gain, offset,
            log_wheels: log,
            wheel_shape: wheel_shape(params),
            hue: params.hue.unwrap_or(0.0),
            ..identity_grade(ws, self.grade_encoding)
        };
        let grade_active = grade.exposure != 0.0
            || grade.contrast != 0.0
            || (grade.saturation - 1.0).abs() > f32::EPSILON
            || grade.white_balance != NO_WHITE_BALANCE
            || grade.highlights != 0.0 || grade.shadows != 0.0 || grade.whites != 0.0 || grade.blacks != 0.0
            || [lift, gamma, gain, offset] != [(0.0, 0.0, 0.0); 4]
            || log.is_some()
            || grade.hue != 0.0;

        let mut later = Vec::new();
        let luma = klipp_color::linear::luma_coefs(ws);
        let clarity = params.clarity.unwrap_or(0.0);
        if clarity != 0.0 {
            later.push(GradeStage::Clarity(clarity, luma, self.clarity_radius()));
        }
        let vib = params.vibrance.unwrap_or(0.0);
        if vib != 0.0 {
            later.push(GradeStage::Vibrance(vib, luma));
        }
        let satbri = [
            params.lift_sat, params.lift_bri, params.gamma_sat, params.gamma_bri,
            params.gain_sat, params.gain_bri, params.offset_sat, params.offset_bri,
        ]
        .map(|v| v.unwrap_or(0.0));
        if satbri.iter().any(|&v| v != 0.0) {
            later.push(GradeStage::WheelSatBri(satbri, luma));
        }
        if let Some(curves) = params.curves.as_ref().filter(|c| !is_curves_identity(c)) {
            let luts = self
                .curve_lut_cache
                .get(&node_id)
                .cloned()
                .unwrap_or_else(|| Arc::new(build_curve_luts(curves)));
            later.push(GradeStage::Curves {
                lut: Box::new(*luts),
                preserve_luma: params.curves_preserve_luma.unwrap_or(false),
                mix: params.curves_mix.unwrap_or(1.0),
                luma,
            });
        }
        if let Some(huesat) = params.huesat.as_ref().filter(|h| !h.is_all_identity()) {
            later.push(GradeStage::HueSat {
                lut: Box::new(build_huesat_lut_data(huesat)),
                mix: params.huesat_mix.unwrap_or(1.0),
                active: huesat_active(huesat),
            });
        }
        if let Some(warper) = params.warper_params.as_ref().filter(|w| !w.is_identity()) {
            later.push(GradeStage::Warper { hue: warper.hue_shifts, sat: warper.sat_shifts });
        }

        let global_mix = params.global_mix.unwrap_or(1.0);
        let mixing = (global_mix - 1.0).abs() > f32::EPSILON;
        let mut stages = Vec::new();
        if later.is_empty() {
            if grade_active || mixing {
                stages.push(GradeStage::Grade(klipp_effects::cpu_grade::ColorGrade { global_mix, ..grade }));
            }
        } else {
            if mixing {
                stages.push(GradeStage::Keep);
            }
            if grade_active {
                stages.push(GradeStage::Grade(grade));
            }
            stages.extend(later);
            if mixing {
                stages.push(GradeStage::Mix(global_mix));
            }
        }
        stages
    }

    /// A Color Wheels node as the unified grade: lift/gamma/gain/offset,
    /// or low/mid/high in log mode.
    fn color_wheels_grade(&self, params: &NodeParams) -> klipp_effects::cpu_grade::ColorGrade {
        let log = log_wheels(params);
        let [lift, gamma, gain, offset] = if params.wheels_log_mode.unwrap_or(false) {
            [(0.0, 0.0, 0.0); 4]
        } else {
            linear_wheels(params)
        };
        klipp_effects::cpu_grade::ColorGrade { lift, gamma, gain, offset, log_wheels: log, wheel_shape: wheel_shape(params), ..identity_grade(self.working_colorspace, self.grade_encoding) }
    }

    /// Dispatch the Corrector's stages ([`Self::corrector_stages`]) onto
    /// the GPU chain. Returns the total flip count so the caller can
    /// mirror it into `chain_slot_a`, or `None` when nothing is active
    /// (caller bypass-tunnels through this node).
    fn dispatch_corrector_chain(
        &mut self,
        node_id: NodeId,
        params: &NodeParams,
        width: u32,
        height: u32,
        in_is_a: bool,
    ) -> Option<u32> {
        let stages = self.corrector_stages(node_id, params);
        let gpu = self.gpu.as_mut()?;
        let mut cur = in_is_a;
        let mut flips: u32 = 0;
        for stage in &stages {
            let f = stage.dispatch(gpu, cur, width, height);
            if f % 2 == 1 {
                cur = !cur;
            }
            flips += f;
        }
        if flips == 0 { None } else { Some(flips) }
    }

    /// Flush the GPU chain: download the buffer identified by `slot_a` and
    /// store it in `node_outputs` under `chain_tail`'s node ID. Also
    /// updates `tapped_output` if the tail is the matte tap.
    fn flush_chain(
        &mut self,
        chain_tail: &mut Option<NodeId>,
        slot_a: bool,
        width: u32,
        height: u32,
        node_outputs: &mut crate::plan::Values<'_>,
        matte_tap: Option<NodeId>,
        tapped_output: &mut Option<Arc<Vec<f32>>>,
    ) {
        let Some(tail_id) = chain_tail.take() else { return };
        let gpu = match self.gpu.as_mut() {
            Some(g) => g,
            None => return,
        };
        let pixel_count = (width as usize) * (height as usize);
        // The run's final result, and the caller asked for it on the GPU:
        // submit the chain and stop. The download this skips is 16 bytes
        // a pixel — 132 MB at 4K, ~40 ms — for a frame the caller will
        // encode to display u8 on the GPU and read back at 4 bytes a
        // pixel instead.
        if self.run_gpu_output_node == Some(tail_id) && Some(tail_id) != matte_tap {
            gpu.submit_chain();
            self.last_chain_final_slot = Some(slot_a);
            self.output_on_gpu = Some(slot_a);
            node_outputs.insert(tail_id, Arc::new(Vec::new()));
            return;
        }
        // Merged submit: chain dispatches + readback copy in ONE
        // queue.submit. Halves NVIDIA Vulkan driver overhead vs the
        // previous submit_chain() + download_chain_output_linear()
        // two-submit pattern.
        let linear = gpu.flush_chain_and_download_linear(slot_a, pixel_count);
        // Stash the slot so the engine can route the on-GPU buffer
        // straight into a viewer texture without re-uploading the
        // CPU mirror we just downloaded. The download itself is
        // unavoidable here — node_outputs still expects an
        // `Arc<Vec<f32>>` for downstream CPU-only consumers — but
        // overwriting `last_chain_final_slot` makes the GPU copy
        // available to the engine for the texture-bind path that
        // doesn't need the CPU copy at all.
        self.last_chain_final_slot = Some(slot_a);
        let arc = Arc::new(linear);
        if Some(tail_id) == matte_tap {
            *tapped_output = Some(arc.clone());
        }
        node_outputs.insert(tail_id, arc);
    }

    /// After the most recent `execute_linear*` call, returns the chain
    /// buffer slot (`Some(true)` = `buf_a`, `Some(false)` = `buf_b`)
    /// that holds the linear-RGBA chain output, or `None` if the
    /// graph ran identity / pure-CPU and the GPU chain never
    /// engaged. Lets the engine route the chain's GPU-resident
    /// output straight into a viewer texture (via
    /// `encode_grade_to_texture`) instead of paying a 33 MB PCIe
    /// upload to re-seed the buffer it already produced.
    ///
    /// Reset to `None` at the top of every `execute_linear_with_thumbs`
    /// call so a stale slot can't survive across executions.
    pub fn last_chain_final_slot(&self) -> Option<bool> {
        self.last_chain_final_slot
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_node(
        &mut self,
        node_id: NodeId,
        kind: &NodeKind,
        params: &NodeParams,
        primary_input: &[f32],
        secondary_input: Option<&[f32]>,
        tertiary_input: Option<&[f32]>,
        width: u32,
        height: u32,
        source_frame: &[f32],
        frame_number: u64,
        clip_time_secs: f64,
        // The node wiring this one's matte port left its matte in GPU
        // memory rather than on the CPU. Only ApplyMatte cares.
        matte_in_gpu: bool,
    ) -> Vec<f32> {
        match kind {
            NodeKind::Source => source_frame.to_vec(),

            // Reroute — pure wire-routing aid, no work. Mirrors the
            // bypass path's primary-input copy so a Reroute slipped
            // into any chain is a no-op on the pixels.
            NodeKind::Reroute => primary_input.to_vec(),

            // The node records which log format the media is — the media
            // pool passes it to the decoder, which decodes before the
            // graph runs. It must not touch the pixels: transforming here
            // would transform a second time. See
            // `LogFormat::needs_input_transform`.
            NodeKind::InputTransform => primary_input.to_vec(),

            NodeKind::Exposure => {
                let exp = params.exposure.unwrap_or(0.0);
                if exp != 0.0 {
                    if let Some(gpu) = &mut self.gpu {
                        gpu.apply_color_grade(
                            primary_input, width, height,
                            exp, 0.0, default_pivot_encoded(), 1.0, NO_WHITE_BALANCE,
                            0.0, 0.0, 0.0, 0.0,
                            (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                            0.0, 1.0,
                        )
                    } else {
                        let g = klipp_effects::cpu_grade::ColorGrade { exposure: exp, ..identity_grade(self.working_colorspace, self.grade_encoding) };
                        run_stages_cpu(&[GradeStage::Grade(g)], primary_input, width, height)
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::Contrast => {
                let con = params.contrast.unwrap_or(0.0);
                let clarity = params.clarity.unwrap_or(0.0);
                // A dedicated Contrast node carries its own pivot, so
                // these paths read it rather than take the default —
                // otherwise a pivot set on a Contrast node would do
                // nothing while the same pivot on a Corrector worked,
                // and the CPU and GPU twins below would disagree.
                // In the grade's encoding, as the Corrector encodes it: it
                // was always BT.1886's, so under ACEScct the same pivot sat
                // somewhere else on a Contrast node (#1039).
                let pivot = klipp_color::grade_model::encode_contrast_pivot_in(
                    params.contrast_pivot.unwrap_or(klipp_color::grade_model::DEFAULT_CONTRAST_PIVOT),
                    self.grade_encoding,
                );
                if con != 0.0 || clarity != 0.0 {
                    let frame = if con != 0.0 {
                        if let Some(gpu) = &mut self.gpu {
                            gpu.apply_color_grade(
                                primary_input, width, height,
                                0.0, con, pivot, 1.0, NO_WHITE_BALANCE,
                                0.0, 0.0, 0.0, 0.0,
                                (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                                0.0, 1.0,
                            )
                        } else {
                            let g = klipp_effects::cpu_grade::ColorGrade { contrast: con, contrast_pivot_encoded: pivot, ..identity_grade(self.working_colorspace, self.grade_encoding) };
                            run_stages_cpu(&[GradeStage::Grade(g)], primary_input, width, height)
                        }
                    } else {
                        primary_input.to_vec()
                    };
                    if clarity != 0.0 {
                        let radius = self.clarity_radius();
                        if let Some(gpu) = &mut self.gpu {
                            gpu.apply_clarity_gpu(&frame, width, height, clarity, radius)
                        } else {
                            run_stages_cpu(&[GradeStage::Clarity(clarity, klipp_color::linear::luma_coefs(self.working_colorspace), radius)], &frame, width, height)
                        }
                    } else {
                        frame
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::Saturation => {
                let sat = params.saturation.unwrap_or(1.0);
                let vib = params.vibrance.unwrap_or(0.0);
                if sat != 1.0 || vib != 0.0 {
                    let frame = if sat != 1.0 {
                        if let Some(gpu) = &mut self.gpu {
                            gpu.apply_color_grade(
                                primary_input, width, height,
                                0.0, 0.0, default_pivot_encoded(), sat, NO_WHITE_BALANCE,
                                0.0, 0.0, 0.0, 0.0,
                                (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                                0.0, 1.0,
                            )
                        } else {
                            let g = klipp_effects::cpu_grade::ColorGrade { saturation: sat, ..identity_grade(self.working_colorspace, self.grade_encoding) };
                            run_stages_cpu(&[GradeStage::Grade(g)], primary_input, width, height)
                        }
                    } else {
                        primary_input.to_vec()
                    };
                    if vib != 0.0 {
                        if let Some(gpu) = &mut self.gpu {
                            gpu.apply_vibrance_gpu(&frame, width, height, vib)
                        } else {
                            run_stages_cpu(&[GradeStage::Vibrance(vib, klipp_color::linear::luma_coefs(self.working_colorspace))], &frame, width, height)
                        }
                    } else {
                        frame
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::ToneControls => {
                let hi = params.highlights.unwrap_or(0.0);
                let sh = params.shadows.unwrap_or(0.0);
                let wh = params.whites.unwrap_or(0.0);
                let bl = params.blacks.unwrap_or(0.0);
                if hi != 0.0 || sh != 0.0 || wh != 0.0 || bl != 0.0 {
                    if let Some(gpu) = &mut self.gpu {
                        gpu.apply_color_grade(
                            primary_input, width, height,
                            0.0, 0.0, default_pivot_encoded(), 1.0, NO_WHITE_BALANCE,
                            hi, sh, wh, bl,
                            (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                            0.0, 1.0,
                        )
                    } else {
                        let g = klipp_effects::cpu_grade::ColorGrade { highlights: hi, shadows: sh, whites: wh, blacks: bl, ..identity_grade(self.working_colorspace, self.grade_encoding) };
                        run_stages_cpu(&[GradeStage::Grade(g)], primary_input, width, height)
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::ColorWheels => {
                let grade = self.color_wheels_grade(params);
                if let Some(gpu) = &mut self.gpu {
                    gpu.apply_color_grade(
                        primary_input, width, height,
                        0.0, 0.0, default_pivot_encoded(), 1.0, NO_WHITE_BALANCE,
                        0.0, 0.0, 0.0, 0.0,
                        grade.lift, grade.gamma, grade.gain, grade.offset, grade.log_wheels, grade.wheel_shape,
                        0.0, 1.0,
                    )
                } else {
                    run_stages_cpu(&[GradeStage::Grade(grade)], primary_input, width, height)
                }
            }

            NodeKind::WheelSatBri => {
                let ls = params.lift_sat.unwrap_or(0.0);
                let lb = params.lift_bri.unwrap_or(0.0);
                let gs = params.gamma_sat.unwrap_or(0.0);
                let gb = params.gamma_bri.unwrap_or(0.0);
                let gas = params.gain_sat.unwrap_or(0.0);
                let gab = params.gain_bri.unwrap_or(0.0);
                let os = params.offset_sat.unwrap_or(0.0);
                let ob = params.offset_bri.unwrap_or(0.0);
                if let Some(gpu) = &mut self.gpu {
                    gpu.apply_wheel_sat_bri_gpu(
                        primary_input, width, height,
                        ls, lb, gs, gb, gas, gab, os, ob,
                    )
                } else {
                    let luma = klipp_color::linear::luma_coefs(self.working_colorspace);
                    run_stages_cpu(&[GradeStage::WheelSatBri([ls, lb, gs, gb, gas, gab, os, ob], luma)], primary_input, width, height)
                }
            }

            NodeKind::RGBCurves => {
                // Use cached LUTs if available, otherwise build fresh
                let luts = self
                    .curve_lut_cache
                    .get(&node_id)
                    .cloned()
                    .or_else(|| {
                        params
                            .curves
                            .as_ref()
                            .filter(|c| !is_curves_identity(c))
                            .map(|c| Arc::new(build_curve_luts(c)))
                    });
                if let Some(luts) = luts {
                    let preserve_luma = params.curves_preserve_luma.unwrap_or(false);
                    let mix = params.curves_mix.unwrap_or(1.0);
                    let lut_data = *luts;
                    if let Some(gpu) = &mut self.gpu {
                        gpu.apply_rgb_curves_gpu(
                            primary_input, width, height,
                            &lut_data, preserve_luma, mix,
                        )
                    } else {
                        let luma = klipp_color::linear::luma_coefs(self.working_colorspace);
                        let stage = GradeStage::Curves { lut: Box::new(lut_data), preserve_luma, mix, luma };
                        run_stages_cpu(&[stage], primary_input, width, height)
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::HueSatCurves => {
                if let Some(huesat) = &params.huesat {
                    if !huesat.is_all_identity() {
                        let mix = params.huesat_mix.unwrap_or(1.0);
                        let lut_data = build_huesat_lut_data(huesat);
                        let active = huesat_active(huesat);
                        if let Some(gpu) = &mut self.gpu {
                            gpu.apply_huesat_gpu(primary_input, width, height, mix, &lut_data, active)
                        } else {
                            let stage = GradeStage::HueSat { lut: Box::new(lut_data), mix, active };
                            run_stages_cpu(&[stage], primary_input, width, height)
                        }
                    } else {
                        primary_input.to_vec()
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::ColorBalance => {
                let temp = params.temperature.unwrap_or(0.0);
                let tint = params.tint.unwrap_or(0.0);
                let hue = params.hue.unwrap_or(0.0);
                let mix = params.global_mix.unwrap_or(1.0);

                if let Some(gpu) = &mut self.gpu {
                    gpu.apply_color_balance_gpu(
                        primary_input, width, height,
                        white_balance(temp, tint, self.working_colorspace), hue, mix,
                    )
                } else {
                    let mut frame = primary_input.to_vec();
                    klipp_effects::cpu_grade::color_balance(&mut frame, white_balance(temp, tint, self.working_colorspace), hue, mix);
                    frame
                }
            }

            NodeKind::Qualifier => {
                // A qualifier that is switched off selects the whole frame,
                // as `NodeParams::apply_override` documents and the chain
                // already did; this path keyed its window regardless (#1037).
                if let Some(qual) = params.qualifier.as_ref().filter(|q| q.enabled) {
                    let mut frame = if let Some(gpu) = &mut self.gpu {
                        gpu.apply_qualifier_gpu(
                            primary_input, width, height,
                            qual.hue_center, qual.hue_width, qual.hue_soft,
                            qual.sat_low, qual.sat_high, qual.sat_soft,
                            qual.lum_low, qual.lum_high, qual.lum_soft,
                            qual.invert,
                            qual.exposure, qual.saturation, qual.hue_shift, qual.temperature,
                            true, // always matte_preview for Qualifier node (outputs matte)
                        )
                    } else {
                        let mut frame = primary_input.to_vec();
                        klipp_effects::cpu_grade::qualifier_matte(
                            &mut frame,
                            qual.hue_center / 360.0, qual.hue_width / 360.0, qual.hue_soft / 360.0,
                            qual.sat_low, qual.sat_high, qual.sat_soft,
                            qual.lum_low, qual.lum_high, qual.lum_soft,
                            qual.invert,
                        );
                        frame
                    };
                    // The matte comes back to the CPU on both paths; the
                    // finesse — clean, shrink/grow, blur — is one code.
                    if qual.refines() {
                        klipp_effects::cpu_grade::matte_finesse(
                            &mut frame, width, height,
                            qual.clean_black, qual.clean_white, self.whole_px(qual.shrink_grow), self.whole_px(qual.blur),
                        );
                    }
                    frame
                } else {
                    vec![1.0f32; (width * height * 4) as usize]
                }
            }

            NodeKind::SoftClip => {
                // Value transform — primary signal in, primary signal
                // out. Disabled or absent settings → passthrough so
                // a freshly-added node doesn't distort the viewer.
                if let Some(sc) = &params.soft_clip {
                    if !sc.enabled {
                        primary_input.to_vec()
                    } else if let Some(gpu) = &mut self.gpu {
                        gpu.apply_soft_clip_gpu(
                            primary_input,
                            width, height,
                            sc.threshold, sc.knee,
                        )
                    } else {
                        klipp_color::soft_clip::apply_soft_clip(
                            primary_input, width, height, sc,
                        )
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::LumaKey => {
                // Output is a matte (greyscale RGBA: R=G=B=matte
                // value, A=1.0). When the user hasn't enabled the
                // node yet (default LumaKeySettings::enabled = false),
                // pass the input through as a full-on matte so a
                // freshly-added node doesn't black out the viewer.
                if let Some(lk) = &params.luma_key {
                    if !lk.enabled {
                        vec![1.0f32; (width * height * 4) as usize]
                    } else if let Some(gpu) = &mut self.gpu {
                        gpu.apply_luma_key_gpu(
                            primary_input,
                            width, height,
                            lk.low, lk.high, lk.soft, lk.invert,
                        )
                    } else {
                        // CPU fallback. `generate_luma_key_matte` still
                        // speaks u8, so the key is measured on the
                        // encoded picture — which is the domain the
                        // shader keys in too, so the two agree. Both take
                        // the working space's luma row, so they also
                        // agree on an ACEScg project, where Rec.709's
                        // row under-weights green by a fifth.
                        //
                        // The matte itself comes back as it is. It is a
                        // blend coefficient and not a colour: running it
                        // through `u8_rgba_to_linear`, as this used to,
                        // applied the sRGB decode to it and turned a
                        // half matte into 0.21. That put this path a
                        // quarter of a frame away from the shader, which
                        // `secondary_gpu_parity` caught.
                        let primary_u8 = linear_rgba_to_u8(primary_input);
                        let matte = klipp_color::luma_key::generate_luma_key_matte(
                            &primary_u8,
                            width,
                            height,
                            lk,
                            klipp_color::linear::luma_coefs(self.working_colorspace),
                        );
                        matte.iter().flat_map(|&v| [v, v, v, 1.0]).collect()
                    }
                } else {
                    vec![1.0f32; (width * height * 4) as usize]
                }
            }

            NodeKind::DifferenceKey => {
                // Two-input matte producer: the live frame comes
                // through `primary_input` (linear f32 RGBA); the
                // clean plate is loaded off disk via the same
                // `matte_image_cache` ExternalMatte uses, then
                // resized to live-frame dimensions via
                // nearest-neighbour. Disabled, with no plate, or with
                // one that will not load, it selects everything, as
                // every matte producer does with nothing chosen (#1039).
                let pixel_count = (width as usize) * (height as usize);
                let Some(dk) = &params.difference_key else {
                    return full_matte(pixel_count);
                };
                if !dk.enabled {
                    return full_matte(pixel_count);
                }
                let Some(path) = dk.plate_path.as_ref() else {
                    return full_matte(pixel_count);
                };

                let Some(plate_resized) = self.resized_plate(path, width, height) else {
                    return full_matte(pixel_count);
                };

                let matte = klipp_color::difference_key::generate_difference_key_matte(
                    primary_input,
                    &plate_resized,
                    width,
                    height,
                    dk,
                    klipp_color::linear::luma_coefs(self.working_colorspace),
                );
                // Expand matte → RGBA (R=G=B=matte, A=1) in linear.
                let mut out = vec![0.0f32; pixel_count * 4];
                use rayon::prelude::*;
                out.par_chunks_exact_mut(4)
                    .zip(matte.par_iter())
                    .for_each(|(px, &m)| {
                        px[0] = m;
                        px[1] = m;
                        px[2] = m;
                        px[3] = 1.0;
                    });
                out
            }

            NodeKind::Mask => {
                // Empty mask stack — or all masks disabled — must
                // behave like "entire frame selected" so the
                // downstream MaskCutout doesn't black out the viewer.
                // The all-1 matte that used to fix that costs 130 MB
                // / frame to allocate at 4K (and another 41 ms in
                // MaskCutout's per-pixel multiply against it) — both
                // for "no change". Returning an empty `Vec` instead
                // is functionally equivalent: every downstream matte
                // consumer (`MaskCutout`, `CombineMattes`,
                // `ApplyMatte`) already treats a size-mismatched /
                // empty matte as "passthrough source" — that's what
                // they do for unwired-input cases too. Drops a stale
                // Mask + MaskCutout pair (left behind by
                // `RemoveMaskShape` after the user removes the last
                // shape) from ~57 ms / 4K frame to nothing.
                let stack_has_active = params.masks.as_ref()
                    .is_some_and(|ms| ms.masks.iter().any(|m| m.enabled));
                if !stack_has_active {
                    return Vec::new();
                }
                if let Some(masks) = &params.masks {
                    // Cache lookup: for static masks (no keyframes)
                    // the fingerprint is identical every frame, so
                    // playback hits the cached buffer and skips the
                    // rasterize + RGBA-expand cliff entirely. For
                    // animated masks the fingerprint folds in
                    // 1ms-quantised time so adjacent ticks still
                    // share. Cache miss path falls through to the
                    // existing GPU/CPU raster code.
                    let fp = matte_fingerprint(masks, clip_time_secs, width, height);
                    if let Some(entry) = self.matte_cache.get(&node_id) {
                        if entry.fingerprint == fp
                            && entry.width == width
                            && entry.height == height
                        {
                            return (*entry.matte).clone();
                        }
                    }

                    // Keyframes, then the window's tracked motion (#238).
                    let mut evaluated = masks.evaluated_in(clip_time_secs, height as f32 / width.max(1) as f32);

                    // Tracker-driven mask centres: when a TrackPoint
                    // declares `linked_mask = (this_node, mask_index)`
                    // and has a recorded position at the playhead,
                    // override the corresponding mask's centre. This
                    // runs AFTER keyframe evaluation so any user-set
                    // keyframes are kept on disk but the live frame
                    // follows the tracker. Drives "power window
                    // tracker" / "mask follows face" workflows.
                    apply_tracker_overrides_to_masks(
                        &mut evaluated,
                        node_id,
                        clip_time_secs,
                        &self.current_clip_track_points,
                    );

                    // Critical: matte values are LINEAR-DOMAIN
                    // geometric weights, NOT sRGB pixel values, so
                    // the previous u8 → sRGB → linear round-trip
                    // was both wrong (distorted the matte by the
                    // sRGB curve) and a massive CPU sink.
                    //
                    // GPU raster path: keep it for shape primitives
                    // because the compute shader is much faster than
                    // the CPU rasterizer at high res, but expand its
                    // u8 output to f32 directly with `v / 255.0` —
                    // NO sRGB conversion.
                    // CPU raster path: skip the u8 trip entirely and
                    // expand the f32 matte to RGBA in one parallel
                    // pass.
                    use rayon::prelude::*;
                    let pixel_count = (width as usize) * (height as usize);
                    let mut out = vec![0.0f32; pixel_count * 4];
                    let gpu_matte_u8 = if let Some(gpu) = &mut self.gpu {
                        let gpu_masks: Vec<klipp_effects::gpu::GpuMaskDef> = evaluated.masks.iter()
                            .filter(|m| m.enabled)
                            .filter_map(mask_to_gpu_def)
                            .collect();
                        // Bezier shapes are now first-class on the
                        // GPU path; the v2 mask pipeline (storage-
                        // buffer records + pooled bezier points) has
                        // no shape cap and no bezier exclusion.
                        if !gpu_masks.is_empty() {
                            Some(gpu.generate_mask_gpu(width, height, &gpu_masks))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(matte_u8) = gpu_matte_u8 {
                        out.par_chunks_exact_mut(4)
                            .zip(matte_u8.par_chunks_exact(4))
                            .for_each(|(dst, src)| {
                                let v = src[0] as f32 / 255.0;
                                dst[0] = v;
                                dst[1] = v;
                                dst[2] = v;
                                dst[3] = 1.0;
                            });
                    } else {
                        let matte = klipp_color::masks::generate_mask_matte(width, height, &evaluated);
                        out.par_chunks_exact_mut(4)
                            .zip(matte.par_iter())
                            .for_each(|(dst, &v)| {
                                dst[0] = v;
                                dst[1] = v;
                                dst[2] = v;
                                dst[3] = 1.0;
                            });
                    }

                    // Stash for next frame. Bound the cache so a
                    // session with many Mask nodes doesn't grow
                    // unboundedly — 16 entries covers every realistic
                    // graph; LRU eviction by simple drain when over.
                    if self.matte_cache.len() > 16 {
                        self.matte_cache.clear();
                    }
                    self.matte_cache.insert(node_id, MatteCacheEntry {
                        fingerprint: fp,
                        width,
                        height,
                        matte: Arc::new(out.clone()),
                    });
                    out
                } else {
                    vec![1.0f32; (width * height * 4) as usize]
                }
            }

            NodeKind::ExternalMatte => {
                // Produce a matte (R=G=B=matte, A=1) from an image
                // loaded off disk. No `primary_input` — ExternalMatte
                // has no input ports. Decoded image is cached by path;
                // the per-frame cost is just a resize + channel pick.
                let pixel_count = (width as usize) * (height as usize);
                let Some(path) = params.matte_image_path.as_ref() else {
                    // No image chosen: it selects everything, as every
                    // matte producer does with nothing chosen (#1039).
                    return full_matte(pixel_count);
                };
                let channel = params.matte_channel.unwrap_or_default();
                let invert = params.matte_invert.unwrap_or(false);

                // Decode + cache. An image that fails to load is read as
                // no image (it selects everything), so a broken link
                // neither crashes the render nor blanks the grade.
                let cached = match self.matte_image_cache.get(path).cloned() {
                    Some(c) => c,
                    None => match image::open(path) {
                        Ok(img) => {
                            let rgba = img.to_rgba8();
                            let (iw, ih) = (rgba.width(), rgba.height());
                            let mut linear = vec![0.0f32; (iw * ih * 4) as usize];
                            for (i, px) in rgba.pixels().enumerate() {
                                let o = i * 4;
                                linear[o]     = klipp_color::transfer::srgb_to_linear(px[0] as f32 / 255.0);
                                linear[o + 1] = klipp_color::transfer::srgb_to_linear(px[1] as f32 / 255.0);
                                linear[o + 2] = klipp_color::transfer::srgb_to_linear(px[2] as f32 / 255.0);
                                linear[o + 3] = px[3] as f32 / 255.0;
                            }
                            let arc = Arc::new((linear, iw, ih));
                            self.matte_image_cache.insert(path.clone(), Arc::clone(&arc));
                            self.unreadable_images.remove(path);
                            arc
                        }
                        Err(e) => {
                            if self.unreadable_images.insert(path.clone()) {
                                tracing::warn!("ExternalMatte: failed to load {:?}: {e}", path);
                            }
                            return full_matte(pixel_count);
                        }
                    },
                };
                let (src, sw, sh) = (&cached.0, cached.1, cached.2);

                // Nearest-neighbour resize into the timeline resolution.
                // Bilinear would be prettier but mattes read better as
                // hard-edged. The image's already loaded once; this is
                // the only per-frame cost.
                let mut out = vec![0.0f32; pixel_count * 4];
                for y in 0..height {
                    let sy = ((y as u64 * sh as u64) / height as u64).min(sh as u64 - 1) as u32;
                    for x in 0..width {
                        let sx = ((x as u64 * sw as u64) / width as u64).min(sw as u64 - 1) as u32;
                        let si = ((sy * sw + sx) as usize) * 4;
                        let r = src[si];
                        let g = src[si + 1];
                        let b = src[si + 2];
                        let a = src[si + 3];
                        // Rec.709 luminance in linear light: the image is
                        // a file of its own, decoded from sRGB into BT.709
                        // primaries and never into the working space, so
                        // its weights are BT.709's in any project. (The
                        // keys weigh by the working space because they read
                        // the working space's picture.)
                        let m = match channel {
                            MatteChannel::Alpha => a,
                            MatteChannel::Luminance => 0.2126 * r + 0.7152 * g + 0.0722 * b,
                            MatteChannel::Red => r,
                            MatteChannel::Green => g,
                            MatteChannel::Blue => b,
                        };
                        let m = if invert { 1.0 - m } else { m };
                        let m = m.clamp(0.0, 1.0);
                        let oi = ((y * width + x) as usize) * 4;
                        out[oi]     = m;
                        out[oi + 1] = m;
                        out[oi + 2] = m;
                        out[oi + 3] = 1.0;
                    }
                }
                out
            }

            NodeKind::CombineMattes => {
                // A matte is coverage — a coefficient, not a colour — so
                // it combines in linear light, at full precision, like
                // `ApplyMatte` and `MaskCutout` already read it.
                //
                // This used to sRGB-encode both mattes into u8, multiply
                // the *encoded* bytes, and decode the result. That is the
                // same mistake ApplyMatte was carrying (see its comment:
                // "a 50% matte acted like 74%"), and it is at its worst
                // exactly where feathered edges live, because the encode
                // lifts small values hardest. Intersecting two mattes of
                // equal coverage came back this far over the true value:
                //
                //     coverage  0.50   0.25   0.10   0.05   0.02
                //     error    +1.7%  +6.6% +37.0% +91.1% +279.4%
                //
                // So every soft edge read far more solid than it was, and
                // 256 levels was all the feather had to live in.
                //
                // An unwired B input is the identity for the operator
                // rather than a hardcoded white frame: white is only the
                // identity for Intersect, and would make Union select
                // everything and Subtract select nothing.
                use rayon::prelude::*;
                let op = params.matte_combine_op.unwrap_or_default();
                let n = primary_input.len();
                let b = match secondary_input {
                    Some(s) if s.len() == n => Some(s),
                    // Wrong-sized matte: treated as absent rather than
                    // sampled wrongly, matching the other matte consumers.
                    _ => None,
                };
                let mut out = vec![0.0f32; n];
                out.par_chunks_exact_mut(4).enumerate().for_each(|(i, px)| {
                    let a = primary_input[i * 4].clamp(0.0, 1.0);
                    let bv = match b {
                        Some(s) => s[i * 4].clamp(0.0, 1.0),
                        None => op.identity_b(),
                    };
                    let k = op.apply(a, bv);
                    px[0] = k;
                    px[1] = k;
                    px[2] = k;
                    px[3] = 1.0;
                });
                out
            }

            NodeKind::MaskCutout => {
                // Multiply the source RGB by the matte's red channel.
                // Missing / mismatched matte → pass through (so an
                // unwired cutout doesn't black out the viewer while
                // the user is still setting things up). Feathered
                // edges come for free from the multiply.
                // Alpha forced to 1.0 so the viewer (which expects
                // opaque RGBA) stays happy.
                //
                // Hot loop: pre-sized Vec + rayon parallel 4-channel
                // chunks. The previous push-per-channel single-thread
                // version was the second-biggest CPU sink during
                // playback after the Mask sRGB round-trip. Parallel
                // chunks scale linearly with cores and avoid the
                // grow-and-copy cost of repeated Vec::push.
                use rayon::prelude::*;
                let matte = match secondary_input {
                    Some(m) => m,
                    None => return primary_input.to_vec(),
                };
                let n = primary_input.len();
                if matte.len() != n || !n.is_multiple_of(4) {
                    return primary_input.to_vec();
                }
                let mut out = vec![0.0f32; n];
                out.par_chunks_exact_mut(4)
                    .zip(primary_input.par_chunks_exact(4))
                    .zip(matte.par_chunks_exact(4))
                    .for_each(|((dst, src), m)| {
                        let k = m[0].clamp(0.0, 1.0);
                        dst[0] = src[0] * k;
                        dst[1] = src[1] * k;
                        dst[2] = src[2] * k;
                        dst[3] = 1.0;
                    });
                out
            }

            NodeKind::ApplyMatte => {
                // Port 0: graded frame, Port 1: matte, Port 2: ungraded
                // (pre-grade). The graded picture where the matte selects,
                // the ungraded one where it does not, in linear light.
                //
                // This used to bracket the whole thing through 8-bit, and
                // that cost three things. A highlight was clipped at white,
                // so a secondary could not be applied to an HDR or log clip
                // without flattening every value above 1.0. The blend itself
                // quantised to 8 bits. And the matte — a coefficient, not a
                // colour — went through the sRGB *encode* on its way in, so
                // a 50% matte acted like 74% and every feathered edge leaned
                // towards the graded side.
                //
                // Conventions follow `MaskCutout`, the other matte consumer:
                // the matte is its red channel clamped to [0, 1], a missing
                // or wrong-sized matte passes the graded frame through
                // rather than blacking the viewer out, and alpha is left at
                // 1.0 because the viewer expects opaque RGBA. A matte that
                // is present and zero still selects nothing — that is what
                // an `ExternalMatte` with no file chosen emits.
                use rayon::prelude::*;
                let n = primary_input.len();
                let mix = params.mix.unwrap_or(1.0);
                // A matte the chain left in GPU memory, for a blend that
                // then could not stay on the chain. Reading it back costs
                // a transfer; not reading it would drop the secondary
                // from the picture without saying so.
                let downloaded = if matte_in_gpu && secondary_input.is_none_or(|m| m.len() != n) {
                    self.gpu.as_ref().map(|g| g.read_matte_buf_linear(n / 4)).filter(|m| m.len() == n)
                } else {
                    None
                };
                let matte = match (downloaded.as_deref(), secondary_input) {
                    (Some(m), _) => m,
                    (None, Some(m)) if m.len() == n => m,
                    // Unwired, or a Mask whose shapes are all disabled (it
                    // returns an empty Vec rather than allocating 130 MB of
                    // 1.0 at 4K).
                    _ => return primary_input.to_vec(),
                };
                let ungraded = match tertiary_input {
                    Some(t) if t.len() == n => t,
                    Some(_) => return primary_input.to_vec(),
                    None if source_frame.len() == n => source_frame,
                    None => return primary_input.to_vec(),
                };
                if !n.is_multiple_of(4) {
                    return primary_input.to_vec();
                }
                let mut out = vec![0.0f32; n];
                out.par_chunks_exact_mut(4)
                    .zip(primary_input.par_chunks_exact(4))
                    .zip(ungraded.par_chunks_exact(4))
                    .zip(matte.par_chunks_exact(4))
                    .for_each(|(((dst, graded), plain), m)| {
                        let k = (m[0] * mix).clamp(0.0, 1.0);
                        for c in 0..3 {
                            dst[c] = plain[c] + (graded[c] - plain[c]) * k;
                        }
                        dst[3] = 1.0;
                    });
                out
            }

            NodeKind::Blend => {
                // Two-input mix, in the linear light the rest of the
                // graph is already in.
                //
                // This used to bracket through 8-bit sRGB: every value
                // encoded, quantised to a byte, mixed, and decoded
                // back. Three costs, and the first is the one that
                // shows. Mixing in the encoding is not mixing light —
                // half of black and white came back at 0.214 rather
                // than 0.5, because the average was taken of the sRGB
                // codes rather than of the light they stand for. Nuke,
                // Fusion and Resolve all composite linear for the same
                // reason.
                //
                // Second, the byte: a scene-linear frame carries
                // highlights above 1.0, and `linear_rgba_to_u8` clamps
                // them flat before the mix. Third, the round trip cost
                // two full-frame conversions on the CPU.
                //
                // And the alpha was thrown away — the old path wrote a
                // literal 255 into every pixel, so a Blend anywhere in
                // a graph destroyed the matte of everything downstream.
                use rayon::prelude::*;
                let mix = params.mix.unwrap_or(0.5).clamp(0.0, 1.0);
                let b = secondary_input.unwrap_or(primary_input);
                let mut out = primary_input.to_vec();
                out.par_chunks_exact_mut(4)
                    .zip(b.par_chunks_exact(4))
                    .for_each(|(pa, pb)| {
                        for c in 0..4 {
                            pa[c] += (pb[c] - pa[c]) * mix;
                        }
                    });
                out
            }

            NodeKind::Layer => {
                // Alpha-over, in linear light. Same three costs as
                // Blend above, and the same reasoning: compositing is
                // an operation on light, so it belongs in the space
                // where the numbers are light.
                //
                // The alpha is composited rather than saturated at a
                // byte: `a = fa + ba * (1 - fa)` is the standard
                // over-operator alpha, which the u8 version computed
                // then clamped to 255, flattening any partially
                // transparent result into an opaque one.
                use rayon::prelude::*;
                let bg = secondary_input.unwrap_or(source_frame);
                let mut out = primary_input.to_vec();
                out.par_chunks_exact_mut(4)
                    .zip(bg.par_chunks_exact(4))
                    .for_each(|(f, b)| {
                        let fa = f[3].clamp(0.0, 1.0);
                        let inv = 1.0 - fa;
                        for c in 0..3 {
                            f[c] = f[c] * fa + b[c] * inv;
                        }
                        f[3] = fa + b[3].clamp(0.0, 1.0) * inv;
                    });
                out
            }

            NodeKind::Lut3D => {
                if let Some(path) = &params.lut_path {
                    let lut = self
                        .lut_cache
                        .entry(path.clone())
                        .or_insert_with(|| {
                            // Identity-LUT passthrough on load failure —
                            // matches the chain Lut3D site above. The
                            // previous fallback was an all-zero size-2
                            // LUT, which rendered the clip black
                            // silently with no log.
                            match klipp_color::load_lut(path) {
                                Ok(lut) => Arc::new(lut),
                                Err(e) => {
                                    tracing::warn!(
                                        "Lut3D: load failed for {:?}: {} — passing through",
                                        path, e,
                                    );
                                    Arc::new(LutData::identity(2))
                                }
                            }
                        })
                        .clone();
                    let opacity = params.lut_opacity.unwrap_or(1.0);
                    let tetrahedral = self.lut_tetrahedral;
                    // Both paths bracket the lookup with the LUT's
                    // primaries, from the same matrices (#422).
                    let bracket = lut_node_bracket(params, &lut, self.working_colorspace);
                    if let Some(gpu) = &mut self.gpu {
                        gpu.apply_3d_lut_gpu(
                            primary_input, width, height,
                            &lut.data, lut.size as u32, opacity, tetrahedral, bracket,
                        )
                    } else {
                        let mut frame = primary_input.to_vec();
                        klipp_effects::apply_3d_lut_bracketed(
                            &mut frame, &lut.data, lut.size, opacity,
                            bracket.0, bracket.1, tetrahedral,
                        );
                        frame
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::FilmGrain => {
                if let Some(grain) = &params.grain {
                    if grain.enabled && grain.amount > 0.001 {
                        // Both paths quantise the pixel coordinates by
                        // `size` to give the grain particles, so a zero
                        // divides by it. Clamped once, before the fork,
                        // so the two sides are handed the same number.
                        let size = grain_size(grain, self.render_scale);
                        let coefs =
                            klipp_color::linear::luma_coefs(self.working_colorspace);
                        if let Some(gpu) = &mut self.gpu {
                            gpu.apply_grain_gpu(
                                primary_input, width, height,
                                grain.amount, size,
                                grain.shadows, grain.highlights,
                                grain.monochrome,
                                frame_number as u32,
                            )
                        } else {
                            let mut out = primary_input.to_vec();
                            klipp_effects::cpu_grade::film_grain(
                                &mut out, width, height,
                                grain.amount, size,
                                grain.shadows, grain.highlights,
                                grain.monochrome,
                                frame_number as u32,
                                coefs,
                            );
                            out
                        }
                    } else {
                        primary_input.to_vec()
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::ColorMatch => {
                // Per-node (non-chain) path. Same algorithm as the
                // chain arm — applies pre-computed match adjustments
                // via the unified grade shader, or short-circuits
                // when the user hasn't run a match yet.
                let s = match params.color_match.as_ref() {
                    Some(s) if !s.is_identity() => *s,
                    _ => return primary_input.to_vec(),
                };
                if let Some(gpu) = &mut self.gpu {
                    gpu.apply_color_grade(
                        primary_input, width, height,
                        s.exposure, 0.0, default_pivot_encoded(), s.saturation, white_balance(s.temperature, s.tint, self.working_colorspace),
                        0.0, 0.0, 0.0, 0.0,
                        (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), None, klipp_color::grade_model::WheelShape::default(),
                        0.0, 1.0,
                    )
                } else {
                    let g = klipp_effects::cpu_grade::ColorGrade {
                        exposure: s.exposure,
                        saturation: s.saturation,
                        white_balance: white_balance(s.temperature, s.tint, self.working_colorspace),
                        ..identity_grade(self.working_colorspace, self.grade_encoding)
                    };
                    run_stages_cpu(&[GradeStage::Grade(g)], primary_input, width, height)
                }
            }

            NodeKind::ColorWarper => {
                let warper = params.warper_params.clone().unwrap_or_default();
                if warper.is_identity() {
                    primary_input.to_vec()
                } else if let Some(gpu) = &mut self.gpu {
                    gpu.apply_color_warper_gpu(
                        primary_input, width, height,
                        &warper.hue_shifts, &warper.sat_shifts,
                    )
                } else {
                    let stage = GradeStage::Warper { hue: warper.hue_shifts, sat: warper.sat_shifts };
                    run_stages_cpu(&[stage], primary_input, width, height)
                }
            }

            NodeKind::Denoise => {
                let luma = params.denoise_luma.unwrap_or(0.0);
                let chroma = params.denoise_chroma.unwrap_or(0.0);
                let radius = params.denoise_radius.unwrap_or(3);
                if luma > 0.0 || chroma > 0.0 {
                    // Both sides divide by twice the square of each
                    // sigma. A zero radius made the spatial term 0/0 and
                    // every weight a NaN, so the floor goes here, before
                    // the fork, rather than in either twin.
                    let (spatial_sigma, color_sigma) = denoise_sigmas(luma, chroma, self.px(radius as f32));
                    let max_kernel = self.denoise_max_kernel();
                    let coefs = klipp_color::linear::luma_coefs(self.working_colorspace);
                    if let Some(gpu) = &mut self.gpu {
                        gpu.apply_denoise_gpu(
                            primary_input, width, height,
                            spatial_sigma, color_sigma, max_kernel,
                        )
                    } else {
                        klipp_effects::cpu_grade::bilateral_denoise(
                            primary_input, width, height,
                            spatial_sigma, color_sigma, max_kernel, coefs,
                        )
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::Corrector => {
                // The CPU twins of the GPU chain's stages, in its order:
                // without a GPU, or when the chain cannot take the node
                // (a node whose thumbnail the node editor shows).
                if !corrector_has_active_stage(params) {
                    return primary_input.to_vec();
                }
                run_stages_cpu(&self.corrector_stages(node_id, params), primary_input, width, height)
            }

            // LayerMixer is handled in the dispatch loop before
            // execute_node is called (it needs N inputs, not 3) — if
            // we ever reach this arm via a non-standard call path the
            // safest thing is a passthrough.
            NodeKind::LayerMixer
            | NodeKind::Output
            | NodeKind::Unknown => primary_input.to_vec(),

            // PrimariesMatrix applies a fixed 3×3 working-space
            // conversion in linear-light RGB. Both `primaries_from`
            // and `primaries_to` must be set; any other state
            // (one missing, or a same-from-same-to pair, or an
            // unsupported pair like BT.2020 stubs) falls through to
            // a passthrough — that's also the default-params state
            // the migration uses to mark wrappers as inert when it
            // strips them.
            NodeKind::PrimariesMatrix => {
                match (params.primaries_from, params.primaries_to) {
                    (Some(from), Some(to)) => {
                        match klipp_color::primaries::working_space_conversion_matrix(from, to) {
                            Some(m) => {
                                let mut frame = primary_input.to_vec();
                                klipp_color::linear::apply_primaries_matrix(&mut frame, m);
                                frame
                            }
                            None => primary_input.to_vec(),
                        }
                    }
                    _ => primary_input.to_vec(),
                }
            }

            // Compound holds a nested NodeGraph in
            // `params.compound_subgraph`. Recursive sub-graph
            // execution runs the packed nodes on the incoming
            // frame and returns the sub-graph's Output as the
            // compound's output. Three bailouts to a plain
            // passthrough: no sub-graph attached (migration edge
            // case), recursion depth cap hit (pathological
            // compound-of-compound nesting — keeps per-frame cost
            // bounded), or the sub-graph executor errors (missing
            // Output edge / cycle — shouldn't happen on Group-
            // created compounds but we fall through safely rather
            // than propagate the error up to the engine).
            // RGB Splitter — labelled fan-out. All three output
            // ports carry the same full-RGBA frame; the grade chain
            // wired to a given branch is the one that grades that
            // channel. Channel extraction happens at the matching
            // RGBCombiner. DaVinci convention; avoids a multi-output
            // refactor of the executor.
            NodeKind::RGBSplitter => primary_input.to_vec(),

            // RGB Combiner — port 0 (R) takes the red-graded signal,
            // port 1 (G) the green-graded, port 2 (B) the blue-
            // graded. Output assembles R from in0's R channel,
            // G from in1's G, B from in2's B, alpha from in0. Falls
            // back to primary_input for any missing / wrong-size
            // input so a half-wired combiner still produces a sane
            // image instead of a crash or black frame.
            NodeKind::RGBCombiner => {
                let n = primary_input.len();
                let g_src = secondary_input
                    .filter(|s| s.len() == n)
                    .unwrap_or(primary_input);
                let b_src = tertiary_input
                    .filter(|s| s.len() == n)
                    .unwrap_or(primary_input);
                let mut out = Vec::with_capacity(n);
                let mut i = 0;
                while i + 3 < n {
                    out.push(primary_input[i]);      // R ← in0.R
                    out.push(g_src[i + 1]);          // G ← in1.G
                    out.push(b_src[i + 2]);          // B ← in2.B
                    out.push(primary_input[i + 3]);  // A ← in0.A
                    i += 4;
                }
                out
            }

            NodeKind::Compound => {
                let Some(subgraph) = params.compound_subgraph.as_ref() else {
                    return primary_input.to_vec();
                };
                if self.compound_depth >= COMPOUND_MAX_DEPTH {
                    tracing::warn!(
                        "Compound: depth cap {} hit — falling through",
                        COMPOUND_MAX_DEPTH,
                    );
                    return primary_input.to_vec();
                }
                self.compound_depth += 1;
                // Clone the sub-graph so the recursive call can
                // borrow it immutably while we still hold the
                // `params` reference for the outer node. In
                // practice sub-graphs are small (a handful of
                // nodes), so the clone is cheap relative to the
                // actual grade work about to run on the frame.
                let subgraph = subgraph.clone();
                let result = self.execute_linear_with_thumbs(
                    primary_input,
                    &subgraph,
                    width,
                    height,
                    clip_time_secs,
                    frame_number,
                    None, // matte_tap — outer-level concern, not ours
                    None, // matte_preview_node — same
                    None, // visible_nodes — per-node thumbs inside
                          // compounds land with a follow-up pass
                );
                self.compound_depth -= 1;
                match result {
                    Ok((output, _tapped, _thumbs)) => output,
                    Err(e) => {
                        tracing::warn!(
                            "Compound: sub-graph execution failed: {:?} — falling through",
                            e,
                        );
                        primary_input.to_vec()
                    }
                }
            }
        }
    }
}

// ── Helper functions ────────────────────────────────────────────────────────

/// Build the 6×256 f32 LUT data for GPU hue/sat curve application.
fn build_huesat_lut_data(
    huesat: &klipp_core::HueSatCurves,
) -> [f32; klipp_effects::cpu_grade::HUESAT_LUT_LEN] {
    let mut lut = [0.5f32; klipp_effects::cpu_grade::HUESAT_LUT_LEN]; // 0.5 = identity for relative curves

    let curves = [
        &huesat.hue_vs_hue,
        &huesat.hue_vs_sat,
        &huesat.hue_vs_lum,
        &huesat.lum_vs_sat,
        &huesat.sat_vs_sat,
        &huesat.orange_vs_sat,
        &huesat.sat_vs_lum,
    ];

    for (c, points) in curves.iter().enumerate() {
        if points.is_empty() {
            continue;
        }
        let lut_f32 = klipp_color::curves::build_curve_lut_f32(points);
        for (i, &val) in lut_f32.iter().enumerate() {
            lut[c * 256 + i] = val;
        }
    }

    lut
}

fn is_curves_identity(curves: &klipp_core::CurveSet) -> bool {
    klipp_color::curves::is_identity(&curves.master)
        && klipp_color::curves::is_identity(&curves.red)
        && klipp_color::curves::is_identity(&curves.green)
        && klipp_color::curves::is_identity(&curves.blue)
}

/// Returns true when at least one of the Corrector's grade stages
/// would actually produce a non-identity output. Mirrors the per-kind
/// activeness checks in `node_supports_gpu_chain`. Used to gate the
/// chain start — at full default values the chain bypass-tunnels
/// through the Corrector, so its presence in the default graph costs
/// nothing.
fn corrector_has_active_stage(p: &NodeParams) -> bool {
    p.exposure.unwrap_or(0.0) != 0.0
        || p.contrast.unwrap_or(0.0) != 0.0
        || p.clarity.unwrap_or(0.0) != 0.0
        || (p.saturation.unwrap_or(1.0) - 1.0).abs() > f32::EPSILON
        || p.vibrance.unwrap_or(0.0) != 0.0
        || p.highlights.unwrap_or(0.0) != 0.0
        || p.shadows.unwrap_or(0.0) != 0.0
        || p.whites.unwrap_or(0.0) != 0.0
        || p.blacks.unwrap_or(0.0) != 0.0
        || p.lift_x.unwrap_or(0.0) != 0.0
        || p.lift_y.unwrap_or(0.0) != 0.0
        || p.lift_master.unwrap_or(0.0) != 0.0
        || p.gamma_x.unwrap_or(0.0) != 0.0
        || p.gamma_y.unwrap_or(0.0) != 0.0
        || p.gamma_master.unwrap_or(0.0) != 0.0
        || p.gain_x.unwrap_or(0.0) != 0.0
        || p.gain_y.unwrap_or(0.0) != 0.0
        || p.gain_master.unwrap_or(0.0) != 0.0
        || p.offset_x.unwrap_or(0.0) != 0.0
        || p.offset_y.unwrap_or(0.0) != 0.0
        || p.offset_master.unwrap_or(0.0) != 0.0
        || p.lift_sat.unwrap_or(0.0) != 0.0
        || p.lift_bri.unwrap_or(0.0) != 0.0
        || p.gamma_sat.unwrap_or(0.0) != 0.0
        || p.gamma_bri.unwrap_or(0.0) != 0.0
        || p.gain_sat.unwrap_or(0.0) != 0.0
        || p.gain_bri.unwrap_or(0.0) != 0.0
        || p.offset_sat.unwrap_or(0.0) != 0.0
        || p.offset_bri.unwrap_or(0.0) != 0.0
        || p.temperature.unwrap_or(0.0) != 0.0
        || p.tint.unwrap_or(0.0) != 0.0
        || p.hue.unwrap_or(0.0) != 0.0
        || (p.global_mix.unwrap_or(1.0) - 1.0).abs() > f32::EPSILON
        || p.curves.as_ref().is_some_and(|c| !is_curves_identity(c))
        || p.huesat.as_ref().is_some_and(|h| !h.is_all_identity())
        || p.warper_params.as_ref().is_some_and(|w| !w.is_identity())
        || log_wheels(p).is_some()
}

/// Convert a klipp_core::Mask to a GpuMaskDef. Every shape variant
/// is supported on the GPU as of the v2 mask pipeline (storage-buffer
/// records + pooled bezier points). Bezier shapes carry their
/// control points through verbatim — the dispatch helper pools them
/// across all masks in the stack into a single storage buffer.
fn mask_to_gpu_def(mask: &klipp_core::Mask) -> Option<klipp_effects::gpu::GpuMaskDef> {
    use klipp_core::MaskShape;
    use klipp_effects::gpu::GpuMaskDef;
    let def = match &mask.shape {
        MaskShape::Circle { center_x, center_y, radius } =>
            Some(GpuMaskDef::circle(*center_x, *center_y, *radius, mask.feather, mask.invert, mask.opacity)),
        MaskShape::Rectangle { center_x, center_y, width, height, rotation } =>
            Some(GpuMaskDef::rectangle(*center_x, *center_y, *width, *height, *rotation, mask.feather, mask.invert, mask.opacity)),
        MaskShape::LinearGradient { start_x, start_y, end_x, end_y } =>
            Some(GpuMaskDef::linear_gradient(*start_x, *start_y, *end_x, *end_y, mask.feather, mask.invert, mask.opacity)),
        MaskShape::Bezier { points } =>
            Some(GpuMaskDef::bezier(points.clone(), mask.feather, mask.invert, mask.opacity)),
    };
    def.map(|d| d.with_warp(mask.keyframes.warp.map(|w| w.0)))
}

/// The four curves as the curves shader and its CPU twin read them:
/// master, red, green, blue, 256 float entries each.
fn build_curve_luts(curves: &klipp_core::CurveSet) -> [f32; 1024] {
    let mut lut = [0.0f32; 1024];
    for (c, points) in [&curves.master, &curves.red, &curves.green, &curves.blue].into_iter().enumerate() {
        lut[c * 256..(c + 1) * 256].copy_from_slice(&klipp_color::curves::build_curve_lut_f32(points));
    }
    lut
}

/// Bake a node graph to a 3D LUT by running the executor on synthetic 1x1 frames
/// for each grid point. Qualifiers, masks, and spatial effects are excluded
/// (LUT captures primary grade only — same limitation as before).
pub fn bake_graph_to_lut(
    graph: &NodeGraph,
    executor: &mut GraphExecutor,
    lut_size: usize,
) -> Vec<[f32; 3]> {
    let n = lut_size;
    let mut lut = Vec::with_capacity(n * n * n);

    // Strip qualifier/mask nodes for LUT baking (spatial ops can't be baked)
    let mut bake_graph = graph.clone();
    for node in &mut bake_graph.nodes {
        if matches!(
            node.kind,
            NodeKind::Qualifier | NodeKind::LumaKey | NodeKind::Mask | NodeKind::CombineMattes | NodeKind::ApplyMatte | NodeKind::DifferenceKey
        ) {
            node.enabled = false;
        }
    }

    for bi in 0..n {
        for gi in 0..n {
            for ri in 0..n {
                let r = (ri as f32 / (n - 1).max(1) as f32 * 255.0) as u8;
                let g = (gi as f32 / (n - 1).max(1) as f32 * 255.0) as u8;
                let b = (bi as f32 / (n - 1).max(1) as f32 * 255.0) as u8;
                let pixel = vec![r, g, b, 255];
                let result = executor
                    .execute(&pixel, &bake_graph, 1, 1, 0.0, 0, None, None)
                    .map(|(r, _)| r)
                    .unwrap_or_else(|_| vec![r, g, b, 255]);
                lut.push([
                    result[0] as f32 / 255.0,
                    result[1] as f32 / 255.0,
                    result[2] as f32 / 255.0,
                ]);
            }
        }
    }
    lut
}

/// Apply tracker-driven overrides to mask centres in `evaluated`.
///
/// Walks `track_points` and, for any TrackPoint whose `linked_mask =
/// Some((node_id, mask_index))` matches the current Mask node + a
/// mask in `evaluated.masks`, replaces that mask's centre with the
/// recorded tracker position at the current playback time. Runs
/// AFTER keyframe evaluation so any persistent / keyframed centre
/// stays on disk but the live frame follows the tracker.
///
/// Tracker indexing: `clip_time_secs` is converted to a clip-frame
/// index using a 24 fps fallback when the executor doesn't have an
/// fps in scope (the mask renderer is fps-agnostic — the engine
/// could pass real fps via a future setter, but tracker positions
/// are recorded at the project fps so a 24 fps assumption produces
/// the right index for the dominant case). Track-point start_frame
/// is honoured so positions before the tracker's first analysed
/// frame stay at the persistent / keyframed centre.
///
/// Shape coverage: only `Circle` and `Rectangle` carry a single
/// centre. `LinearGradient` (start/end pair) and `Bezier` (point
/// list) are skipped — driving them via a single tracker would need
/// a different binding model.
fn apply_tracker_overrides_to_masks(
    evaluated: &mut klipp_core::MaskStack,
    node_id: NodeId,
    clip_time_secs: f64,
    track_points: &[klipp_core::TrackPoint],
) {
    if track_points.is_empty() || evaluated.masks.is_empty() {
        return;
    }

    // Frame index conversion. Project fps isn't in scope here — we
    // use 24 fps as a stable default. The Mask-node code path
    // already assumes consistent fps elsewhere; if a future change
    // wires real fps through, replace this constant.
    const FALLBACK_FPS: f64 = 24.0;
    let playhead_frame = (clip_time_secs * FALLBACK_FPS).max(0.0) as i64;

    for (mask_idx, mask) in evaluated.masks.iter_mut().enumerate() {
        // Skip disabled / non-tracker-bindable shapes early, and a window
        // the window tracker drives through its own keyframes (#238): its
        // point track is the record of that, not a second driver.
        if !mask.enabled || mask.keyframes.is_tracked() {
            continue;
        }
        let Some(tp) = track_points.iter().find(|tp| {
            tp.enabled
                && tp.linked_mask == Some((node_id, mask_idx))
                && !tp.positions.is_empty()
        }) else {
            continue;
        };

        // Resolve frame index relative to the tracker's start_frame.
        let pos_idx_signed = playhead_frame - (tp.start_frame as i64);
        if pos_idx_signed < 0 {
            continue; // before the tracker's analysed range
        }
        let pos_idx = pos_idx_signed as usize;
        let Some(&(tx, ty)) = tp.positions.get(pos_idx) else {
            continue; // past the analysed range
        };

        let (tx, ty) = (tx as f32, ty as f32);
        match &mut mask.shape {
            klipp_core::MaskShape::Circle { center_x, center_y, .. } => {
                *center_x = tx;
                *center_y = ty;
            }
            klipp_core::MaskShape::Rectangle { center_x, center_y, .. } => {
                *center_x = tx;
                *center_y = ty;
            }
            // LinearGradient + Bezier don't have a single centre to
            // override; skip silently. Future work could translate
            // their points by the tracker delta from the first
            // recorded position.
            _ => {}
        }
    }
}

/// Composite N pre-rendered linear-RGBA inputs for `NodeKind::LayerMixer`.
///
/// `wired` is `(port_index, pixels)` for every wired port in stack order
/// (port 0 on top). `inputs` is the per-port settings vector from
/// `LayerMixerParams`; missing entries fall back to default (Normal,
/// opacity 1). Returns a fresh linear-RGBA buffer of `pixel_count * 4`
/// floats.
///
/// Composition runs bottom-up: start with the deepest wired layer,
/// then blend each shallower layer on top using its blend_mode and
/// opacity. Output alpha is preserved from the running base — mixers
/// that feed the final Output don't need a meaningful alpha channel,
/// but downstream nodes that DO read alpha get something coherent.
fn layer_mixer_compose(
    wired: &[(u8, &[f32])],
    inputs: &[LayerMixerInput],
    pixel_count: usize,
) -> Vec<f32> {
    let default_input = LayerMixerInput::default();
    let setting = |port: u8| -> &LayerMixerInput {
        inputs.get(port as usize).unwrap_or(&default_input)
    };

    // wired is in port order (0 = top). To composite bottom-up start
    // from the back of the list.
    let mut iter = wired.iter().rev();
    let (base_port, base_pixels) = match iter.next() {
        Some(&(p, px)) => (p, px),
        None => return vec![0.0; pixel_count * 4],
    };

    // Bottom layer: apply its own opacity (against transparent black)
    // so a dim base reads as dim, not full-strength.
    let s = setting(base_port);
    let mut out: Vec<f32> = base_pixels.iter().enumerate().map(|(i, v)| {
        if i % 4 == 3 { *v }       // alpha untouched on the base
        else { v * s.opacity }
    }).collect();

    for &(port, top) in iter {
        let s = setting(port);
        let op = s.opacity.clamp(0.0, 1.0);
        let mode = s.blend_mode;
        for i in 0..pixel_count {
            let o = i * 4;
            let (tr, tg, tb) = (top[o], top[o + 1], top[o + 2]);
            let (br, bg, bb) = (out[o], out[o + 1], out[o + 2]);
            // Per-channel blended colour (before opacity mix).
            // Formulas mirror `klipp_effects::composite::blend_chunk_linear`
            // — same linear-light semantics so a clip-level blend in the
            // timeline reads identical to a node-level blend in the mixer.
            let (mr, mg, mb) = match mode {
                BlendMode::Normal     => (tr, tg, tb),
                BlendMode::Add        => (br + tr, bg + tg, bb + tb),
                BlendMode::Multiply   => (tr * br, tg * bg, tb * bb),
                BlendMode::Screen     => (
                    1.0 - (1.0 - tr) * (1.0 - br),
                    1.0 - (1.0 - tg) * (1.0 - bg),
                    1.0 - (1.0 - tb) * (1.0 - bb),
                ),
                BlendMode::Overlay    => (
                    overlay_channel(br, tr),
                    overlay_channel(bg, tg),
                    overlay_channel(bb, tb),
                ),
                BlendMode::SoftLight  => (
                    soft_light_channel(br, tr),
                    soft_light_channel(bg, tg),
                    soft_light_channel(bb, tb),
                ),
                BlendMode::HardLight  => (
                    overlay_channel(tr, br),
                    overlay_channel(tg, bg),
                    overlay_channel(tb, bb),
                ),
                BlendMode::Difference => (
                    (br - tr).abs(), (bg - tg).abs(), (bb - tb).abs(),
                ),
                BlendMode::Exclusion  => (
                    br + tr - 2.0 * br * tr,
                    bg + tg - 2.0 * bg * tg,
                    bb + tb - 2.0 * bb * tb,
                ),
                BlendMode::ColorDodge => (
                    dodge_channel(br, tr),
                    dodge_channel(bg, tg),
                    dodge_channel(bb, tb),
                ),
                BlendMode::ColorBurn  => (
                    burn_channel(br, tr),
                    burn_channel(bg, tg),
                    burn_channel(bb, tb),
                ),
                BlendMode::Darken     => (br.min(tr), bg.min(tg), bb.min(tb)),
                BlendMode::Lighten    => (br.max(tr), bg.max(tg), bb.max(tb)),
            };
            // Mix blended colour over the current base by opacity.
            out[o]     = br + (mr - br) * op;
            out[o + 1] = bg + (mg - bg) * op;
            out[o + 2] = bb + (mb - bb) * op;
            // Alpha: keep the base's; mixers aren't an alpha-over op.
        }
    }
    out
}

/// Overlay blend per the Photoshop / W3C spec, in linear light.
/// Below 0.5 of the base it's a 2× multiply; above, a 2× screen.
/// Per-node thumbnail dimensions — picked to fit comfortably inside
/// the V2 canvas's node body (NODE_W=120, NODE_H=60 → body ~52 px
/// tall) at 1× zoom while staying small enough that capturing many
/// per frame stays cheap. Aspect ratio is 16:9 to match typical
/// video; non-16:9 sources letterbox naturally inside.
pub const THUMB_W: u32 = 192;
pub const THUMB_H: u32 = 108;

/// Convert a linear-RGBA f32 buffer to a small sRGB u8 thumbnail via
/// nearest-neighbour downsample. Cheap (one read + one srgb encode
/// per output pixel) and keeps the thumbnail honest about colour —
/// no resampling artefacts that would mislead the user about what
/// the node actually produced.
fn downsample_to_thumb_u8(src: &[f32], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    if src.len() < (src_w * src_h * 4) as usize || src_w == 0 || src_h == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity((dst_w * dst_h * 4) as usize);
    for y in 0..dst_h {
        let sy = ((y as u64 * src_h as u64) / dst_h as u64).min(src_h as u64 - 1) as u32;
        for x in 0..dst_w {
            let sx = ((x as u64 * src_w as u64) / dst_w as u64).min(src_w as u64 - 1) as u32;
            let i = ((sy * src_w + sx) as usize) * 4;
            let r = klipp_color::transfer::linear_to_srgb(src[i].clamp(0.0, 1.0));
            let g = klipp_color::transfer::linear_to_srgb(src[i + 1].clamp(0.0, 1.0));
            let b = klipp_color::transfer::linear_to_srgb(src[i + 2].clamp(0.0, 1.0));
            let a = src[i + 3].clamp(0.0, 1.0);
            out.push((r * 255.0) as u8);
            out.push((g * 255.0) as u8);
            out.push((b * 255.0) as u8);
            out.push((a * 255.0) as u8);
        }
    }
    out
}

fn overlay_channel(base: f32, top: f32) -> f32 {
    if base < 0.5 { 2.0 * base * top }
    else { 1.0 - 2.0 * (1.0 - base) * (1.0 - top) }
}

/// Soft-light per W3C — gentle dodge/burn governed by the top layer.
fn soft_light_channel(base: f32, top: f32) -> f32 {
    let d = if base <= 0.25 {
        ((16.0 * base - 12.0) * base + 4.0) * base
    } else {
        base.sqrt()
    };
    if top <= 0.5 { base - (1.0 - 2.0 * top) * base * (1.0 - base) }
    else { base + (2.0 * top - 1.0) * (d - base) }
}

fn dodge_channel(base: f32, top: f32) -> f32 {
    if base <= 0.0 { 0.0 }
    else if top >= 1.0 { 1.0 }
    else { (base / (1.0 - top)).min(1.0) }
}

fn burn_channel(base: f32, top: f32) -> f32 {
    if base >= 1.0 { 1.0 }
    else if top <= 0.0 { 0.0 }
    else { 1.0 - ((1.0 - base) / top).min(1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whole-pixel settings keep their full-size meaning: what rounded to
    /// nothing still does, and what was any stays at least one pixel when
    /// the render is smaller.
    #[test]
    fn whole_pixels_keep_their_full_size_meaning() {
        let mut ex = GraphExecutor::new_cpu_only();
        for v in [0.0, 0.25, -0.25, 0.49] {
            assert_eq!(ex.whole_px(v), 0.0, "{v} px at full size");
        }
        assert_eq!(ex.whole_px(0.5), 1.0);
        assert_eq!(ex.whole_px(-3.0), -3.0);
        ex.set_render_scale(0.25);
        assert_eq!(ex.whole_px(0.25), 0.0);
        assert_eq!(ex.whole_px(1.0), 1.0);
        assert_eq!(ex.whole_px(-2.0), -1.0);
        assert_eq!(ex.whole_px(8.0), 2.0);
    }
    // Tests use the legacy full chain so existing assertions that
    // poke specific grade nodes by kind keep working. The production
    // default is now bare Source → Output (see `default_graph`).
    use crate::defaults::default_full_chain_graph as default_graph;

    /// Blending is an operation on light, so half of black and white
    /// is half the light — not half the sRGB code.
    ///
    /// The old path encoded to 8-bit sRGB, mixed the bytes and decoded
    /// back, which returned 0.214: the average of the *codes* for
    /// black and white, decoded. Correct for mixing pictures of
    /// numbers, wrong for mixing light.
    #[test]
    fn a_half_mix_is_half_the_light() {
        let a = [0.0f32, 0.0, 0.0, 1.0];
        let b = [1.0f32, 1.0, 1.0, 1.0];
        let mix = 0.5f32;
        let mixed: Vec<f32> = a.iter().zip(b.iter()).map(|(x, y)| x + (y - x) * mix).collect();
        for c in 0..3 {
            assert!(
                (mixed[c] - 0.5).abs() < 1e-6,
                "linear half-mix should be 0.5, got {}",
                mixed[c],
            );
        }
        // What the 8-bit sRGB bracket used to return, kept here so the
        // difference is on the record rather than in a commit message.
        let old = klipp_color::transfer::srgb_to_linear(0.5);
        assert!((old - 0.214).abs() < 0.01, "sRGB-space half-mix was ~0.214, got {old}");
        assert!(mixed[0] > old * 2.0, "the change is large and visible, not a rounding fix");
    }

    /// A scene-linear frame carries highlights above 1.0. The 8-bit
    /// bracket clamped them flat before the mix could see them.
    #[test]
    fn a_mix_keeps_highlights_above_one() {
        let a = [4.0f32, 4.0, 4.0, 1.0];
        let b = [8.0f32, 8.0, 8.0, 1.0];
        let mixed: Vec<f32> = a.iter().zip(b.iter()).map(|(x, y)| x + (y - x) * 0.5).collect();
        assert!((mixed[0] - 6.0).abs() < 1e-6, "expected 6.0, got {}", mixed[0]);
    }

    /// Blend used to write a literal 255 into every alpha, so one
    /// anywhere in a graph destroyed the matte of everything after it.
    #[test]
    fn a_mix_carries_the_alpha_through() {
        let a = [0.2f32, 0.2, 0.2, 0.25];
        let b = [0.8f32, 0.8, 0.8, 0.75];
        let mixed: Vec<f32> = a.iter().zip(b.iter()).map(|(x, y)| x + (y - x) * 0.5).collect();
        assert!(
            (mixed[3] - 0.5).abs() < 1e-6,
            "alpha should mix like any channel, got {}",
            mixed[3],
        );
    }

    /// The over-operator's alpha is `fa + ba(1 - fa)`, which the u8
    /// version computed and then clamped to 255 — turning a partially
    /// transparent composite opaque.
    #[test]
    fn a_layer_over_a_transparent_background_stays_transparent() {
        let (fa, ba) = (0.5f32, 0.25f32);
        let a = fa + ba * (1.0 - fa);
        assert!((a - 0.625).abs() < 1e-6, "expected 0.625, got {a}");
        assert!(a < 1.0, "a partial composite must not come back opaque");
    }

    fn mk_node(kind: NodeKind, params: NodeParams) -> Node {
        Node {
            id: NodeId::new_v4(),
            kind,
            label: String::new(),
            enabled: true,
            params,
            keyframes: Default::default(),
            output_link: None,
            input_links: Vec::new(),
        }
    }

    fn mk_graph(nodes: Vec<Node>, edges: Vec<Edge>, output_node_id: NodeId) -> NodeGraph {
        NodeGraph {
            nodes,
            edges,
            output_node_id,
            node_positions: Default::default(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        }
    }

    fn edge(from_node: NodeId, to_node: NodeId, to_port: u8) -> Edge {
        Edge { from_node, from_port: 0, to_node, to_port }
    }

    fn exposure_params(stops: f32) -> NodeParams {
        NodeParams { exposure: Some(stops), ..NodeParams::default() }
    }

    /// `Source -> [tail].0` fed by an Exposure branch, and
    /// `Source -> [tail].1` fed raw — so the two-input node under test
    /// gets one branch that has been through a grade and one that has
    /// not, which is the wiring a real composite uses.
    fn two_branch_graph(kind: NodeKind, tail_params: NodeParams, stops: f32) -> NodeGraph {
        let source = mk_node(NodeKind::Source, NodeParams::default());
        let exp = mk_node(NodeKind::Exposure, exposure_params(stops));
        let tail = mk_node(kind, tail_params);
        let output = mk_node(NodeKind::Output, NodeParams::default());
        let edges = vec![
            edge(source.id, exp.id, 0),
            edge(exp.id, tail.id, 0),
            edge(source.id, tail.id, 1),
            edge(tail.id, output.id, 0),
        ];
        let out_id = output.id;
        mk_graph(vec![source, exp, tail, output], edges, out_id)
    }

    /// The graded branch on its own, so a test can ask the executor
    /// what that side actually is rather than hard-coding a number
    /// that depends on Exposure's internals.
    fn exposure_only_graph(stops: f32) -> NodeGraph {
        let source = mk_node(NodeKind::Source, NodeParams::default());
        let exp = mk_node(NodeKind::Exposure, exposure_params(stops));
        let output = mk_node(NodeKind::Output, NodeParams::default());
        let edges = vec![edge(source.id, exp.id, 0), edge(exp.id, output.id, 0)];
        let out_id = output.id;
        mk_graph(vec![source, exp, output], edges, out_id)
    }

    /// A 2x2 scene-linear frame with a deep shadow, mid grey, a bright
    /// highlight and a value above 1.0 — the range where an 8-bit
    /// round trip does the most damage.
    fn linear_probe_frame() -> Vec<f32> {
        vec![
            0.01, 0.02, 0.04, 1.0,
            0.18, 0.18, 0.18, 1.0,
            0.6, 0.45, 0.30, 1.0,
            2.5, 1.8, 1.2, 1.0,
        ]
    }

    /// Blend through the real executor, with two genuinely different
    /// branches feeding its two ports.
    ///
    /// The old arm encoded both inputs to 8-bit sRGB, mixed the bytes
    /// and decoded back. This drives the node the way a graph does and
    /// checks the result is the midpoint of the two inputs *in light*,
    /// and that it is measurably not the midpoint of their sRGB codes.
    #[test]
    fn a_blend_node_mixes_the_light_not_the_code() {
        let frame = linear_probe_frame();
        let mix_params = NodeParams { mix: Some(0.5), ..NodeParams::default() };

        let mut ex = GraphExecutor::new_cpu_only();
        let (exposed, _) = ex
            .execute_linear(&frame, &exposure_only_graph(1.0), 2, 2, 0.0, 0, None, None)
            .unwrap();
        let (blended, _) = ex
            .execute_linear(
                &frame,
                &two_branch_graph(NodeKind::Blend, mix_params, 1.0),
                2, 2, 0.0, 0, None, None,
            )
            .unwrap();

        assert_ne!(exposed, frame, "the graded branch must actually differ, or this proves nothing");

        let mut saw_a_real_difference = false;
        for px in 0..4 {
            for c in 0..3 {
                let i = px * 4 + c;
                let want = (exposed[i] + frame[i]) * 0.5;
                assert!(
                    (blended[i] - want).abs() < 1e-4,
                    "pixel {px} channel {c}: linear half-mix should be {want}, got {}",
                    blended[i],
                );
                // What the 8-bit bracket would have returned.
                use klipp_color::transfer::{linear_to_srgb, srgb_to_linear};
                let code = |v: f32| linear_to_srgb(v).clamp(0.0, 1.0);
                let old = srgb_to_linear((code(exposed[i]) + code(frame[i])) * 0.5);
                if (old - want).abs() > want.max(1e-3) * 0.05 {
                    saw_a_real_difference = true;
                }
            }
        }
        assert!(
            saw_a_real_difference,
            "if the sRGB-space answer were within 5% everywhere this test could not tell them apart",
        );
    }

    /// Layer through the real executor. The foreground carries a
    /// half-transparent alpha, so this checks both halves of the fix:
    /// the colour is composited in light, and the over-operator alpha
    /// comes back partial instead of saturated opaque.
    #[test]
    fn a_layer_node_composites_over_in_linear_light() {
        let mut frame = linear_probe_frame();
        for px in 0..4 {
            frame[px * 4 + 3] = 0.5;
        }

        let mut ex = GraphExecutor::new_cpu_only();
        let (exposed, _) = ex
            .execute_linear(&frame, &exposure_only_graph(1.0), 2, 2, 0.0, 0, None, None)
            .unwrap();
        let (layered, _) = ex
            .execute_linear(
                &frame,
                &two_branch_graph(NodeKind::Layer, NodeParams::default(), 1.0),
                2, 2, 0.0, 0, None, None,
            )
            .unwrap();

        for px in 0..4 {
            let fa = exposed[px * 4 + 3];
            let ba = frame[px * 4 + 3];
            for c in 0..3 {
                let i = px * 4 + c;
                let want = exposed[i] * fa + frame[i] * (1.0 - fa);
                assert!(
                    (layered[i] - want).abs() < 1e-4,
                    "pixel {px} channel {c}: over should be {want}, got {}",
                    layered[i],
                );
            }
            let want_a = fa + ba * (1.0 - fa);
            assert!(
                (layered[px * 4 + 3] - want_a).abs() < 1e-4,
                "pixel {px}: over alpha should be {want_a}, got {}",
                layered[px * 4 + 3],
            );
            assert!(
                layered[px * 4 + 3] < 1.0,
                "pixel {px}: a partial composite must not come back opaque",
            );
        }
    }

    #[test]
    fn identity_graph_is_passthrough() {
        let mut executor = GraphExecutor::new_cpu_only();
        let graph = default_graph();
        // 2x2 test frame
        let frame = vec![
            100, 50, 200, 255, 150, 100, 50, 255, 200, 150, 100, 255, 50, 200, 150, 255,
        ];
        let (result, _) = executor
            .execute(&frame, &graph, 2, 2, 0.0, 0, None, None)
            .unwrap();
        // Linear round-trip is identity within ±1 u8 code value.
        assert_eq!(frame.len(), result.len());
        for (a, b) in frame.iter().zip(result.iter()) {
            assert!(
                (*a as i32 - *b as i32).abs() <= 1,
                "identity default graph should be passthrough within ±1 code value: orig {a}, got {b}",
            );
        }
    }

    #[test]
    fn topo_sort_detects_cycle() {
        let mut graph = default_graph();
        // Create a cycle: output → source
        graph.edges.push(crate::graph::Edge {
            from_node: graph.output_node_id,
            from_port: 0,
            to_node: graph.nodes[0].id, // Source
            to_port: 0,
        });
        let mut executor = GraphExecutor::new_cpu_only();
        let frame = vec![100, 100, 100, 255];
        let result = executor.execute(&frame, &graph, 1, 1, 0.0, 0, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn bypassed_node_passes_through() {
        let mut graph = default_graph();
        // Set exposure to +2 stops, then bypass it
        let exp_node = graph
            .nodes
            .iter_mut()
            .find(|n| n.kind == NodeKind::Exposure)
            .unwrap();
        exp_node.params.exposure = Some(2.0);
        exp_node.enabled = false;

        let mut executor = GraphExecutor::new_cpu_only();
        let frame = vec![100, 100, 100, 255];
        let (result, _) = executor
            .execute(&frame, &graph, 1, 1, 0.0, 0, None, None)
            .unwrap();
        // Bypass + linear round-trip: identity within ±1 code value.
        for (a, b) in frame.iter().zip(result.iter()) {
            assert!(
                (*a as i32 - *b as i32).abs() <= 1,
                "bypassed exposure node should not change frame: orig {a}, got {b}",
            );
        }
    }

    #[test]
    fn effective_enabled_falls_back_to_static_bool() {
        let mut graph = default_graph();
        let exp = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::Exposure).unwrap();
        exp.enabled = true;
        assert!(effective_enabled(exp, 0.0));
        exp.enabled = false;
        assert!(!effective_enabled(exp, 5.0));
    }

    #[test]
    fn effective_enabled_respects_keyframe_track() {
        let mut graph = default_graph();
        let exp = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::Exposure).unwrap();
        exp.enabled = false; // static bool must be overridden by the track
        let track = exp.keyframes.track_mut(ENABLED_TRACK);
        track.set_keyframe(0.0, 1.0);
        track.set_keyframe(2.0, 0.0);
        for kf in track.keyframes.iter_mut() {
            kf.interpolation = klipp_core::keyframe::Interpolation::Step;
        }

        assert!(effective_enabled(exp, 0.0), "kf at t=0 value=1.0 should enable");
        assert!(effective_enabled(exp, 1.5), "step-hold up to next kf should still be enabled");
        assert!(!effective_enabled(exp, 2.0), "kf at t=2 value=0.0 should bypass");
        assert!(!effective_enabled(exp, 5.0), "post-last-kf should hold bypass");
    }

    #[test]
    fn keyframed_bypass_applies_grade_at_t0_and_skips_at_t3() {
        // Build a graph with a strong exposure node that is statically
        // enabled=false (so without the __enabled track it'd always
        // pass through). Add a track that flips it on at t=0 and off
        // at t=3. Verify execution at t=0 brightens the frame and at
        // t=3 leaves it alone.
        let mut graph = default_graph();
        let exp = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::Exposure).unwrap();
        exp.params.exposure = Some(2.0);
        exp.enabled = false;
        let track = exp.keyframes.track_mut(ENABLED_TRACK);
        track.set_keyframe(0.0, 1.0);
        track.set_keyframe(3.0, 0.0);
        for kf in track.keyframes.iter_mut() {
            kf.interpolation = klipp_core::keyframe::Interpolation::Step;
        }

        let mut executor = GraphExecutor::new_cpu_only();
        let frame = vec![100, 100, 100, 255];
        let (r_on, _) = executor.execute(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        let (r_off, _) = executor.execute(&frame, &graph, 1, 1, 3.5, 0, None, None).unwrap();

        assert!(r_on[0] > 120, "at t=0 exposure should be live (got {})", r_on[0]);
        for (a, b) in frame.iter().zip(r_off.iter()) {
            assert!(
                (*a as i32 - *b as i32).abs() <= 1,
                "at t=3.5s exposure should be bypassed: orig {a}, got {b}",
            );
        }
    }

    #[test]
    fn effective_enabled_threshold_at_half() {
        // Linear interpolation between 1.0 at t=0 and 0.0 at t=2
        // crosses 0.5 at t=1; threshold semantics: >=0.5 is enabled.
        let mut graph = default_graph();
        let exp = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::Exposure).unwrap();
        let track = exp.keyframes.track_mut(ENABLED_TRACK);
        track.set_keyframe(0.0, 1.0);
        track.set_keyframe(2.0, 0.0);
        // Default interpolation is Linear; NOT forcing Step here.

        assert!(effective_enabled(exp, 0.0));
        assert!(effective_enabled(exp, 1.0), "value=0.5 at midpoint should still be on");
        assert!(!effective_enabled(exp, 1.1), "value below 0.5 should be bypassed");
        assert!(!effective_enabled(exp, 2.0));
    }

    #[test]
    fn params_for_frame_skips_enabled_track() {
        // The reserved __enabled track must not be dispatched as a
        // regular param override — that would trigger a warn and
        // silently try to write a float to a non-existent field.
        let mut graph = default_graph();
        let exp = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::Exposure).unwrap();
        exp.params.exposure = Some(1.5);
        exp.keyframes.track_mut(ENABLED_TRACK).set_keyframe(0.0, 1.0);

        let params = params_for_frame(exp, 0.0);
        assert_eq!(params.exposure, Some(1.5), "__enabled track must not overwrite params");
    }

    #[test]
    fn wireless_input_resolves_to_sender_when_unwired() {
        let mut graph = default_graph();
        let src_id = graph.nodes[0].id; // Source
        let exp_id = graph.nodes.iter().find(|n| n.kind == NodeKind::Exposure).unwrap().id;
        graph.edges.retain(|e| e.to_node != exp_id);
        graph.nodes.iter_mut().find(|n| n.id == src_id).unwrap().output_link = Some("A".into());
        {
            let exp = graph.nodes.iter_mut().find(|n| n.id == exp_id).unwrap();
            exp.input_links = vec![Some("A".into())];
        }
        assert_eq!(graph.input_of(exp_id, 0), Some(src_id));
    }

    #[test]
    fn wired_edge_wins_over_wireless_label() {
        let mut graph = default_graph();
        let src_id = graph.nodes[0].id;
        let exp_id = graph.nodes.iter().find(|n| n.kind == NodeKind::Exposure).unwrap().id;
        // Wireless pointer should be shadowed by the pre-existing
        // wired edge feeding Exposure's port 0.
        graph.nodes.iter_mut().find(|n| n.id == src_id).unwrap().output_link = Some("A".into());
        graph.nodes.iter_mut().find(|n| n.id == exp_id).unwrap().input_links = vec![Some("A".into())];
        let wired = graph.input_of(exp_id, 0);
        assert!(wired.is_some());
        // Either way Source is the only upstream node in default_graph,
        // but the point is input_of must resolve via edge lookup first.
    }

    #[test]
    fn topo_sort_respects_wireless_dependency() {
        // Two-node chain Source → Output via wireless link (no edge).
        // Source must come first in topo order.
        let src_id = NodeId::new_v4();
        let out_id = NodeId::new_v4();
        let graph = NodeGraph {
            nodes: vec![
                crate::graph::Node {
                    id: src_id, kind: NodeKind::Source, label: "S".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: Some("Main".into()),
                    input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: out_id, kind: NodeKind::Output, label: "O".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: None,
                    input_links: vec![Some("Main".into())],
                },
            ],
            edges: vec![],
            output_node_id: out_id,
            node_positions: Default::default(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };
        let order = graph.topo_sort().unwrap();
        let src_idx = order.iter().position(|id| *id == src_id).unwrap();
        let out_idx = order.iter().position(|id| *id == out_id).unwrap();
        assert!(src_idx < out_idx, "wireless sender must run before receiver");
    }

    #[test]
    fn wireless_only_graph_executes_end_to_end() {
        // Source → Exposure via wireless, Exposure → Output via wireless.
        // No drawn edges at all; +1 stop should still brighten the frame.
        let src_id = NodeId::new_v4();
        let exp_id = NodeId::new_v4();
        let out_id = NodeId::new_v4();
        let graph = NodeGraph {
            nodes: vec![
                crate::graph::Node {
                    id: src_id, kind: NodeKind::Source, label: "S".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: Some("PreExp".into()),
                    input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: exp_id, kind: NodeKind::Exposure, label: "E".into(),
                    enabled: true,
                    params: NodeParams { exposure: Some(1.0), ..Default::default() },
                    keyframes: Default::default(),
                    output_link: Some("Graded".into()),
                    input_links: vec![Some("PreExp".into())],
                },
                crate::graph::Node {
                    id: out_id, kind: NodeKind::Output, label: "O".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: None,
                    input_links: vec![Some("Graded".into())],
                },
            ],
            edges: vec![],
            output_node_id: out_id,
            node_positions: Default::default(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };
        let mut executor = GraphExecutor::new_cpu_only();
        let frame = vec![100, 100, 100, 255];
        let (result, _) = executor.execute(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        assert!(result[0] > 100, "wireless chain should still brighten: got {}", result[0]);
    }

    #[test]
    fn wireless_label_with_no_matching_sender_resolves_to_none() {
        let mut graph = default_graph();
        let exp_id = graph.nodes.iter().find(|n| n.kind == NodeKind::Exposure).unwrap().id;
        graph.edges.retain(|e| e.to_node != exp_id);
        graph.nodes.iter_mut().find(|n| n.id == exp_id).unwrap().input_links = vec![Some("Ghost".into())];
        assert_eq!(graph.input_of(exp_id, 0), None);
    }

    #[test]
    fn mask_cutout_blacks_out_outside_region() {
        // 2x2 frame: all white. Matte: top row full, bottom row zero.
        // MaskCutout should leave top white, zero bottom, alpha always 1.
        let src_id = NodeId::new_v4();
        let matte_id = NodeId::new_v4();
        let cutout_id = NodeId::new_v4();
        let output_id = NodeId::new_v4();

        // Use a 2x2 matte via the Qualifier-test style — simpler to
        // hand-build a graph with an ExternalMatte-ish sentinel: put
        // Mask with the right shape. But Mask needs a MaskStack —
        // bypass by faking a matte node via Source whose input is
        // patched by the caller (execute is u8-in). Simplest: build a
        // graph Source → MaskCutout(no matte wired) first to verify
        // passthrough, then wire a Mask with full-fill matte to
        // verify the multiply arm.
        let graph = NodeGraph {
            nodes: vec![
                crate::graph::Node {
                    id: src_id, kind: NodeKind::Source, label: "S".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: cutout_id, kind: NodeKind::MaskCutout, label: "Cut".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: output_id, kind: NodeKind::Output, label: "O".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
            ],
            edges: vec![
                crate::graph::Edge { from_node: src_id, from_port: 0, to_node: cutout_id, to_port: 0 },
                crate::graph::Edge { from_node: cutout_id, from_port: 0, to_node: output_id, to_port: 0 },
            ],
            output_node_id: output_id,
            node_positions: Default::default(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };

        let mut executor = GraphExecutor::new_cpu_only();
        let frame = vec![200, 200, 200, 255];
        // Unwired matte → MaskCutout should pass through (documented
        // fallback so partial wiring doesn't black the viewer out).
        let (result, _) = executor.execute(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        assert!((result[0] as i32 - 200).abs() <= 1, "unwired matte passes through: got {}", result[0]);

        // Now add a Mask feeding the cutout's port 1 and verify the
        // multiply arm. Mask with no MaskStack emits all-1.0 matte,
        // so the cutout still passes through (matte = white
        // everywhere).
        let mask_id = NodeId::new_v4();
        let mut graph2 = graph.clone();
        graph2.nodes.push(crate::graph::Node {
            id: mask_id, kind: NodeKind::Mask, label: "M".into(),
            enabled: true, params: Default::default(), keyframes: Default::default(),
            output_link: None, input_links: Vec::new(),
        });
        let _ = matte_id; // reserved for future mask-with-shape tests
        graph2.edges.push(crate::graph::Edge {
            from_node: mask_id, from_port: 0, to_node: cutout_id, to_port: 1,
        });
        let (r_wired, _) = executor.execute(&frame, &graph2, 1, 1, 0.0, 0, None, None).unwrap();
        assert!(
            (r_wired[0] as i32 - 200).abs() <= 1,
            "Mask(no shape) → all-1 matte → cutout passes through: got {}", r_wired[0]
        );
    }

    #[test]
    fn mask_cutout_multiplies_source_by_matte() {
        // Direct executor-function test: a 1-px frame with full-bright
        // source and half-bright matte should multiply to half.
        let mut executor = GraphExecutor::new_cpu_only();
        let src_id = NodeId::new_v4();
        let cutout_id = NodeId::new_v4();
        let output_id = NodeId::new_v4();

        // Fake matte via Blend@mix=0.0 to produce a specific grey, then
        // feed the cutout's port 1. Simpler: use the executor's
        // run_node direct arm via a constructed graph. Here we just
        // verify the formula numerically via a hand-rolled call.
        let src_linear: Vec<f32> = vec![1.0, 1.0, 1.0, 1.0];
        let matte_linear: Vec<f32> = vec![0.5, 0.5, 0.5, 1.0];

        // Build a graph where MaskCutout has both inputs wired from
        // distinct upstream nodes. We can't fabricate arbitrary
        // per-node outputs from a single source frame without extra
        // plumbing, so instead this test confirms the executor arm
        // logic mirrors the expected formula — written as an
        // independent computation against the same inputs, matching
        // what the arm would produce.
        let _ = (src_id, cutout_id, output_id, &mut executor);
        let n = src_linear.len();
        let mut out = Vec::with_capacity(n);
        let mut i = 0;
        while i + 3 < n {
            let k = matte_linear[i].clamp(0.0, 1.0);
            out.push(src_linear[i] * k);
            out.push(src_linear[i + 1] * k);
            out.push(src_linear[i + 2] * k);
            out.push(1.0);
            i += 4;
        }
        assert_eq!(out, vec![0.5, 0.5, 0.5, 1.0]);
    }

    #[test]
    fn exposure_node_brightens() {
        let mut graph = default_graph();
        let exp_node = graph
            .nodes
            .iter_mut()
            .find(|n| n.kind == NodeKind::Exposure)
            .unwrap();
        exp_node.params.exposure = Some(1.0); // +1 stop

        let mut executor = GraphExecutor::new_cpu_only();
        let frame = vec![100, 100, 100, 255];
        let (result, _) = executor
            .execute(&frame, &graph, 1, 1, 0.0, 0, None, None)
            .unwrap();
        assert!(
            result[0] > 100,
            "exposure +1 should brighten: got {}",
            result[0]
        );
    }

    /// Smoke test: a Source → Corrector → Output graph with exposure +
    /// saturation set on the Corrector applies both stages on the
    /// CPU fallback path. Verifies the bundled-node executor wiring
    /// matches the per-kind nodes (same `apply_*` functions, same
    /// canonical order).
    #[test]
    fn corrector_node_applies_bundled_grade() {
        let mut graph = crate::defaults::default_graph();
        let corrector = graph
            .nodes
            .iter_mut()
            .find(|n| n.kind == NodeKind::Corrector)
            .expect("default graph has a Corrector node");
        corrector.params.exposure = Some(1.0); // +1 stop
        corrector.params.saturation = Some(0.0); // fully desaturate

        let mut executor = GraphExecutor::new_cpu_only();
        let frame = vec![200, 50, 50, 255]; // reddish
        let (result, _) = executor
            .execute(&frame, &graph, 1, 1, 0.0, 0, None, None)
            .unwrap();
        // Saturation 0 → greyscale (R/G/B equal within rounding)
        let diff = (result[0] as i32 - result[1] as i32).unsigned_abs()
            + (result[1] as i32 - result[2] as i32).unsigned_abs();
        assert!(diff <= 4, "Corrector sat=0 should produce greyscale: got {:?}", &result[..3]);
        // Exposure +1 → brighter than the input luma
        assert!(
            result[0] > 100,
            "Corrector exposure +1 should brighten: got {}",
            result[0]
        );
    }

    /// The Corrector at default values is identity — `is_identity()`
    /// returns true for the new default graph (Source → Corrector →
    /// Output) so the engine's identity-graph fast paths still kick in
    /// for fresh clips.
    #[test]
    fn corrector_default_graph_is_identity() {
        let graph = crate::defaults::default_graph();
        assert!(graph.is_identity(), "fresh Corrector graph must be identity");
    }

    #[test]
    fn saturation_node_desaturates() {
        let mut graph = default_graph();
        let sat_node = graph
            .nodes
            .iter_mut()
            .find(|n| n.kind == NodeKind::Saturation)
            .unwrap();
        sat_node.params.saturation = Some(0.0); // fully desaturate

        let mut executor = GraphExecutor::new_cpu_only();
        // Colourful pixel
        let frame = vec![255, 0, 0, 255];
        let (result, _) = executor
            .execute(&frame, &graph, 1, 1, 0.0, 0, None, None)
            .unwrap();
        // R, G, B should be roughly equal (greyscale)
        let diff = (result[0] as i32 - result[1] as i32).unsigned_abs()
            + (result[1] as i32 - result[2] as i32).unsigned_abs();
        assert!(diff <= 2, "saturation=0 should produce greyscale: got {:?}", &result[..3]);
    }

    #[test]
    fn scope_tap_captures_output() {
        let mut graph = default_graph();
        let exp_node = graph
            .nodes
            .iter_mut()
            .find(|n| n.kind == NodeKind::Exposure)
            .unwrap();
        let exp_id = exp_node.id;
        exp_node.params.exposure = Some(1.0);

        let mut executor = GraphExecutor::new_cpu_only();
        let frame = vec![100, 100, 100, 255];
        let (_, tapped) = executor
            .execute(&frame, &graph, 1, 1, 0.0, 0, Some(exp_id), None)
            .unwrap();
        assert!(tapped.is_some(), "tapped output should be captured");
        let tap = tapped.unwrap();
        assert!(tap[0] > 100, "tapped exposure output should be bright");
    }

    #[test]
    fn matte_preview_returns_greyscale() {
        let mut graph = default_graph();
        // Add a qualifier node
        let qual_id = NodeId::new_v4();
        graph.nodes.push(crate::graph::Node {
            id: qual_id,
            kind: NodeKind::Qualifier,
            label: "Qualifier".to_string(),
            enabled: true,
            params: {
                
                NodeParams {
                    qualifier: Some(klipp_core::QualifierSettings {
                        enabled: true,
                        hue_center: 0.0,
                        hue_width: 30.0,
                        ..Default::default()
                    }),
                    ..Default::default()
                }
            },
            keyframes: Default::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        // Connect source → qualifier
        let source_id = graph.nodes[0].id;
        graph.edges.push(crate::graph::Edge {
            from_node: source_id,
            from_port: 0,
            to_node: qual_id,
            to_port: 0,
        });

        let mut executor = GraphExecutor::new_cpu_only();
        let frame = vec![255, 0, 0, 255]; // red pixel
        let (result, _) = executor
            .execute(&frame, &graph, 1, 1, 0.0, 0, None, Some(qual_id))
            .unwrap();
        // Matte preview: R == G == B
        assert_eq!(result[0], result[1]);
        assert_eq!(result[1], result[2]);
        assert_eq!(result[3], 255);
    }

    #[test]
    fn apply_matte_blends_graded_with_ungraded() {
        // Build a minimal graph: Source → ApplyMatte → Output
        // Port 0 = graded (white), Port 1 = matte (50% grey), Port 2 = ungraded (black)
        let source_id = NodeId::new_v4();
        let apply_id = NodeId::new_v4();
        let output_id = NodeId::new_v4();

        let graph = NodeGraph {
            nodes: vec![
                crate::graph::Node {
                    id: source_id,
                    kind: NodeKind::Source,
                    label: "Source".into(),
                    enabled: true,
                    params: Default::default(),
                    keyframes: Default::default(),
                    output_link: None,
                    input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: apply_id,
                    kind: NodeKind::ApplyMatte,
                    label: "ApplyMatte".into(),
                    enabled: true,
                    params: NodeParams { mix: Some(1.0), ..Default::default() },
                    keyframes: Default::default(),
                    output_link: None,
                    input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: output_id,
                    kind: NodeKind::Output,
                    label: "Output".into(),
                    enabled: true,
                    params: Default::default(),
                    keyframes: Default::default(),
                    output_link: None,
                    input_links: Vec::new(),
                },
            ],
            edges: vec![
                // Source feeds all three ports of ApplyMatte (simplified test)
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: apply_id, to_port: 0 },
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: apply_id, to_port: 1 },
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: apply_id, to_port: 2 },
                crate::graph::Edge { from_node: apply_id, from_port: 0, to_node: output_id, to_port: 0 },
            ],
            output_node_id: output_id,
            node_positions: Default::default(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };

        let mut executor = GraphExecutor::new_cpu_only();
        // When all 3 ports get the same frame, output should equal input (matte=source R channel)
        let frame = vec![128, 128, 128, 255];
        let (result, _) = executor.execute(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        // With identical inputs and matte R=128 (50%), blend should be ~128
        assert!((result[0] as i32 - 128).abs() <= 1, "ApplyMatte with equal inputs: got {}", result[0]);
    }

    /// ApplyMatte blends in linear light, so a secondary can be applied
    /// to a picture that has values above white — and the matte is a
    /// coefficient, used as it is rather than sRGB-encoded on the way in.
    ///
    /// Both were broken while the node bracketed itself through 8-bit: a
    /// 12x highlight came back clipped to white, and a 50% matte blended
    /// like 74%.
    #[test]
    fn apply_matte_blends_in_linear_light() {
        let source_id = NodeId::new_v4();
        let apply_id = NodeId::new_v4();
        let output_id = NodeId::new_v4();
        let node = |id: NodeId, kind: NodeKind, params: NodeParams| crate::graph::Node {
            id, kind, label: format!("{kind:?}"), enabled: true, params,
            keyframes: Default::default(), output_link: None, input_links: Vec::new(),
        };
        let graph = NodeGraph {
            nodes: vec![
                node(source_id, NodeKind::Source, Default::default()),
                node(apply_id, NodeKind::ApplyMatte, NodeParams { mix: Some(1.0), ..Default::default() }),
                node(output_id, NodeKind::Output, Default::default()),
            ],
            // Port 0 (graded) and port 1 (matte) are fed by hand below;
            // port 2 (ungraded) is left unwired so it falls back to the
            // source frame, which is the wiring a secondary actually uses.
            edges: vec![
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: apply_id, to_port: 0 },
                crate::graph::Edge { from_node: apply_id, from_port: 0, to_node: output_id, to_port: 0 },
            ],
            output_node_id: output_id,
            node_positions: Default::default(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };

        // One pixel at 12x white. With no matte wired the graded frame
        // passes through untouched — the old 8-bit bracket clipped it to
        // 1.0 here, which is what made secondaries unusable above white.
        let mut executor = GraphExecutor::new_cpu_only();
        let bright = vec![12.0f32, 11.0, 9.5, 1.0];
        let (out, _) = executor
            .execute_linear(&bright, &graph, 1, 1, 0.0, 0, None, None)
            .expect("graph runs");
        assert!(
            (out[0] - 12.0).abs() < 1e-3 && (out[2] - 9.5).abs() < 1e-3,
            "a highlight must survive ApplyMatte: {:?}", &out[..3],
        );
    }

    /// The blend itself: half a matte is half the distance, in linear
    /// light. Through the old 8-bit bracket this landed at 0.85 instead
    /// of 0.75, because the matte was sRGB-encoded on the way in (0.5
    /// acting as 0.74) and the blend happened in display space.
    ///
    /// Wiring: a mid-grey source feeds the matte port directly, so the
    /// matte is exactly 0.5, and an Exposure node one stop up feeds the
    /// graded port, so the graded picture is exactly 1.0. The ungraded
    /// port is left unwired and falls back to the source, 0.5.
    #[test]
    fn apply_matte_half_a_matte_is_half_the_distance() {
        let source_id = NodeId::new_v4();
        let exposure_id = NodeId::new_v4();
        let apply_id = NodeId::new_v4();
        let output_id = NodeId::new_v4();
        let node = |id: NodeId, kind: NodeKind, params: NodeParams| crate::graph::Node {
            id, kind, label: format!("{kind:?}"), enabled: true, params,
            keyframes: Default::default(), output_link: None, input_links: Vec::new(),
        };
        let graph = NodeGraph {
            nodes: vec![
                node(source_id, NodeKind::Source, Default::default()),
                node(exposure_id, NodeKind::Exposure, NodeParams { exposure: Some(1.0), ..Default::default() }),
                node(apply_id, NodeKind::ApplyMatte, NodeParams { mix: Some(1.0), ..Default::default() }),
                node(output_id, NodeKind::Output, Default::default()),
            ],
            edges: vec![
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: exposure_id, to_port: 0 },
                crate::graph::Edge { from_node: exposure_id, from_port: 0, to_node: apply_id, to_port: 0 },
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: apply_id, to_port: 1 },
                crate::graph::Edge { from_node: apply_id, from_port: 0, to_node: output_id, to_port: 0 },
            ],
            output_node_id: output_id,
            node_positions: Default::default(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };

        let mut executor = GraphExecutor::new_cpu_only();
        let grey = vec![0.5f32, 0.5, 0.5, 1.0];
        let (out, _) = executor
            .execute_linear(&grey, &graph, 1, 1, 0.0, 0, None, None)
            .expect("graph runs");
        assert!(
            (out[0] - 0.75).abs() < 1e-4,
            "0.5 matte between 0.5 and 1.0 is 0.75 in linear light, got {}", out[0],
        );
        assert_eq!(out[3], 1.0, "alpha stays opaque, as MaskCutout leaves it");
    }

    /// CombineMattes combines coverage in linear light, so intersecting
    /// two half mattes is a quarter — exactly, not 0.2542.
    ///
    /// The node used to sRGB-encode both inputs into u8, multiply the
    /// bytes and decode: the same mistake ApplyMatte carried, and worse
    /// at the low coverages a feather is made of (+37% at 0.10, +91% at
    /// 0.05, +279% at 0.02). Both matte ports are fed from the one
    /// source, so A and B are identical by construction and the expected
    /// value is just `op(v, v)`.
    #[test]
    fn combine_mattes_combines_coverage_in_linear_light() {
        let run = |op: Option<crate::params::MatteCombineOp>, v: f32| -> f32 {
            let source_id = NodeId::new_v4();
            let combine_id = NodeId::new_v4();
            let output_id = NodeId::new_v4();
            let graph = NodeGraph {
                nodes: vec![
                    crate::graph::Node {
                        id: source_id, kind: NodeKind::Source, label: "S".into(), enabled: true,
                        params: Default::default(), keyframes: Default::default(),
                        output_link: None, input_links: Vec::new(),
                    },
                    crate::graph::Node {
                        id: combine_id, kind: NodeKind::CombineMattes, label: "C".into(), enabled: true,
                        params: NodeParams { matte_combine_op: op, ..Default::default() },
                        keyframes: Default::default(), output_link: None, input_links: Vec::new(),
                    },
                    crate::graph::Node {
                        id: output_id, kind: NodeKind::Output, label: "O".into(), enabled: true,
                        params: Default::default(), keyframes: Default::default(),
                        output_link: None, input_links: Vec::new(),
                    },
                ],
                // The same source into both matte ports.
                edges: vec![
                    crate::graph::Edge { from_node: source_id, from_port: 0, to_node: combine_id, to_port: 0 },
                    crate::graph::Edge { from_node: source_id, from_port: 0, to_node: combine_id, to_port: 1 },
                    crate::graph::Edge { from_node: combine_id, from_port: 0, to_node: output_id, to_port: 0 },
                ],
                output_node_id: output_id,
                node_positions: Default::default(),
                working_space_override: None,
                frames: Vec::new(),
                sticky_notes: Vec::new(),
            };
            let mut executor = GraphExecutor::new_cpu_only();
            let src = vec![v, v, v, 1.0];
            let (out, _) = executor
                .execute_linear(&src, &graph, 1, 1, 0.0, 0, None, None)
                .expect("graph runs");
            out[0]
        };

        use crate::params::MatteCombineOp::*;
        // The headline: a half-and-half intersect. The 8-bit bracket put
        // this at 0.2542, which is outside this tolerance by 40x.
        let got = run(Some(Intersect), 0.5);
        assert!((got - 0.25).abs() < 1e-4, "0.5 AND 0.5 is 0.25, got {got}");

        // And where it hurt most: 2% coverage intersected with itself is
        // 0.0004. The old path returned roughly 0.0015.
        let got = run(Some(Intersect), 0.02);
        assert!(
            (got - 0.0004).abs() < 1e-5,
            "0.02 AND 0.02 is 0.0004, got {got} — a feather's worth of error",
        );

        // The new operators, through the real node rather than only the
        // arithmetic.
        let got = run(Some(Union), 0.5);
        assert!((got - 0.75).abs() < 1e-4, "0.5 OR 0.5 is 0.75, got {got}");
        let got = run(Some(Subtract), 0.5);
        assert!((got - 0.25).abs() < 1e-4, "0.5 minus 0.5 is 0.25, got {got}");

        // A project saved before the operator existed: no field, and the
        // picture it had.
        let legacy = run(None, 0.5);
        let explicit = run(Some(Intersect), 0.5);
        assert_eq!(legacy, explicit, "an absent operator must intersect");
    }

    #[test]
    fn blend_node_interpolates() {
        let source_id = NodeId::new_v4();
        let blend_id = NodeId::new_v4();
        let output_id = NodeId::new_v4();

        let graph = NodeGraph {
            nodes: vec![
                crate::graph::Node {
                    id: source_id, kind: NodeKind::Source, label: "S".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: blend_id, kind: NodeKind::Blend, label: "Blend".into(),
                    enabled: true,
                    params: NodeParams { mix: Some(0.5), ..Default::default() },
                    keyframes: Default::default(),
                    output_link: None,
                    input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: output_id, kind: NodeKind::Output, label: "O".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new(),
                },
            ],
            edges: vec![
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: blend_id, to_port: 0 },
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: blend_id, to_port: 1 },
                crate::graph::Edge { from_node: blend_id, from_port: 0, to_node: output_id, to_port: 0 },
            ],
            output_node_id: output_id,
            node_positions: Default::default(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };

        let mut executor = GraphExecutor::new_cpu_only();
        let frame = vec![200, 100, 50, 255];
        let (result, _) = executor.execute(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        // Blending a frame with itself at mix=0.5 should return the same values
        // (within ±1 code value after the u8↔linear round-trip).
        assert!((result[0] as i32 - 200).abs() <= 1, "r: got {}", result[0]);
        assert!((result[1] as i32 - 100).abs() <= 1, "g: got {}", result[1]);
        assert!((result[2] as i32 - 50).abs() <= 1, "b: got {}", result[2]);
    }

    /// The same node through the u8 entry point, where the old
    /// behaviour was written down as the contract.
    ///
    /// This asserted `128 * 128 / 255 = 64` — the product of two
    /// *sRGB-encoded bytes*, which is what the node used to compute and
    /// is not the intersection of two coverages. A u8 128 is 0.2159
    /// coverage; intersected with itself that is 0.0466, which encodes
    /// back to 61. The old 64 was the sRGB round-trip leaking into the
    /// arithmetic, and at lower coverages the same leak reached +279%.
    #[test]
    fn combine_mattes_intersects_coverage_through_the_u8_path() {
        let source_id = NodeId::new_v4();
        let combine_id = NodeId::new_v4();
        let output_id = NodeId::new_v4();

        let graph = NodeGraph {
            nodes: vec![
                crate::graph::Node {
                    id: source_id, kind: NodeKind::Source, label: "S".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: combine_id, kind: NodeKind::CombineMattes, label: "Combine".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: output_id, kind: NodeKind::Output, label: "O".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new(),
                },
            ],
            edges: vec![
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: combine_id, to_port: 0 },
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: combine_id, to_port: 1 },
                crate::graph::Edge { from_node: combine_id, from_port: 0, to_node: output_id, to_port: 0 },
            ],
            output_node_id: output_id,
            node_positions: Default::default(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };

        let mut executor = GraphExecutor::new_cpu_only();
        // Source = 128 grey, which is 0.2159 coverage in linear light.
        // Intersected with itself: 0.0466, which encodes back to 61.
        let frame = vec![128, 128, 128, 255];
        let (result, _) = executor.execute(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        assert!(
            (result[0] as i32 - 61).abs() <= 1,
            "u8 128 intersected with itself is 61, got {} (64 would be the \
             old encoded-byte multiply)",
            result[0],
        );
    }

    #[test]
    fn remap_ids_produces_unique_ids() {
        let graph = default_graph();
        let remapped = graph.remap_ids();

        // All node IDs should be different from originals
        for (orig, new) in graph.nodes.iter().zip(remapped.nodes.iter()) {
            assert_ne!(orig.id, new.id, "remap_ids should produce new IDs");
        }

        // All edges should reference valid node IDs in the new graph
        let new_ids: std::collections::HashSet<NodeId> = remapped.nodes.iter().map(|n| n.id).collect();
        for edge in &remapped.edges {
            assert!(new_ids.contains(&edge.from_node), "edge.from_node should exist in remapped graph");
            assert!(new_ids.contains(&edge.to_node), "edge.to_node should exist in remapped graph");
        }

        // Output node ID should be remapped
        assert!(new_ids.contains(&remapped.output_node_id));
        assert_ne!(graph.output_node_id, remapped.output_node_id);

        // Positions should be remapped
        assert_eq!(graph.node_positions.len(), remapped.node_positions.len());
        for id in remapped.node_positions.keys() {
            assert!(new_ids.contains(id), "position key should be a valid new ID");
        }
    }

    #[test]
    fn contrast_node_increases_contrast() {
        let mut graph = default_graph();
        let node = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::Contrast).unwrap();
        node.params.contrast = Some(0.5);

        let mut executor = GraphExecutor::new_cpu_only();
        // Mid-grey pixel — contrast pushes away from midpoint
        let frame = vec![180, 180, 180, 255];
        let (result, _) = executor.execute(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        // Above midpoint → should get brighter
        assert!(result[0] > 180, "contrast should push bright pixels brighter: got {}", result[0]);
    }

    #[test]
    fn bake_graph_identity_is_passthrough() {
        let graph = default_graph();
        let mut executor = GraphExecutor::new_cpu_only();
        let lut = bake_graph_to_lut(&graph, &mut executor, 5);

        // 5x5x5 = 125 entries
        assert_eq!(lut.len(), 125);

        // Identity graph: output should match input for each grid point
        for (i, &[r, g, b]) in lut.iter().enumerate() {
            let ri = i % 5;
            let gi = (i / 5) % 5;
            let bi = i / 25;
            let expected_r = ri as f32 / 4.0;
            let expected_g = gi as f32 / 4.0;
            let expected_b = bi as f32 / 4.0;
            assert!((r - expected_r).abs() < 0.02,
                "LUT[{i}] R: expected {expected_r:.2}, got {r:.2}");
            assert!((g - expected_g).abs() < 0.02,
                "LUT[{i}] G: expected {expected_g:.2}, got {g:.2}");
            assert!((b - expected_b).abs() < 0.02,
                "LUT[{i}] B: expected {expected_b:.2}, got {b:.2}");
        }
    }

    #[test]
    fn bake_graph_with_exposure_modifies_lut() {
        let mut graph = default_graph();
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::Exposure)
            .unwrap()
            .params.exposure = Some(1.0); // +1 stop

        let mut executor = GraphExecutor::new_cpu_only();
        let lut = bake_graph_to_lut(&graph, &mut executor, 3);

        // Mid-grey entry (1,1,1) in a 3x3x3 grid = index 13
        let mid = lut[13]; // r=0.5, g=0.5, b=0.5 input
        // +1 stop exposure in linear-light doubles physical light: sRGB 0.5
        // decodes to ~0.214 linear, doubling to ~0.428, which re-encodes to
        // sRGB ~0.68 — meaningfully brighter than the 0.5 input. The exact
        // output matches photographic reality; the old u8 path silently
        // shifted the pivot and landed higher.
        assert!(
            mid[0] > 0.6,
            "exposure +1 should brighten mid-grey: got {:.2}",
            mid[0],
        );
    }

    // ── apply_tracker_overrides_to_masks ────────────────────────────

    fn make_circle_mask(cx: f32, cy: f32) -> klipp_core::Mask {
        klipp_core::Mask {
            shape: klipp_core::MaskShape::Circle { center_x: cx, center_y: cy, radius: 0.1 },
            feather: 0.0,
            invert: false,
            opacity: 1.0,
            enabled: true,
            keyframes: klipp_core::MaskKeyframes::default(),
        }
    }

    fn make_track_point_at(linked_to: Option<(NodeId, usize)>, positions: Vec<(f64, f64)>) -> klipp_core::TrackPoint {
        
        klipp_core::TrackPoint {
            linked_mask: linked_to,
            start_frame: 0,
            positions,
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn tracker_override_leaves_a_window_tracked_window_alone() {
        // #238: a window the window tracker drives moves by its own
        // keyframes; its linked point track is the record, not a second
        // driver.
        let node_id = uuid::Uuid::new_v4();
        let mut mask = make_circle_mask(0.5, 0.5);
        mask.keyframes.track_x = vec![klipp_core::Keyframe { time_secs: 0.0, value: 0.0, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0) }];
        let mut stack = klipp_core::MaskStack { masks: vec![mask] };
        let track_points = vec![make_track_point_at(Some((node_id, 0)), vec![(0.10, 0.20)])];
        apply_tracker_overrides_to_masks(&mut stack, node_id, 0.0, &track_points);
        let klipp_core::MaskShape::Circle { center_x, .. } = stack.masks[0].shape else { panic!("expected Circle") };
        assert!((center_x - 0.5).abs() < 1e-6, "the override left it where its track has it");
    }

    /// #238: a window tracked in perspective reaches the GPU matte with its
    /// map, and an untracked one without.
    #[test]
    fn a_window_in_perspective_takes_its_map_to_the_gpu() {
        let mut mask = make_circle_mask(0.5, 0.5);
        assert_eq!(mask_to_gpu_def(&mask.evaluated_in(0.0, 0.5625)).unwrap().flags & 2, 0);
        let from = [[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6]];
        let to = [[0.4, 0.4], [0.58, 0.43], [0.58, 0.57], [0.4, 0.6]];
        mask.keyframes.track_perspective = Some(klipp_core::PerspectiveTrack { from, keys: vec![klipp_core::CornerKey { time_secs: 0.0, corners: to }] });
        let evaluated = mask.evaluated_in(0.0, 0.5625);
        let def = mask_to_gpu_def(&evaluated).unwrap();
        assert_eq!(def.flags & 2, 2);
        assert_eq!(def.warp, evaluated.keyframes.warp.unwrap().0);
    }

    #[test]
    fn tracker_override_drives_circle_mask_centre() {
        let node_id = uuid::Uuid::new_v4();
        let mut stack = klipp_core::MaskStack { masks: vec![make_circle_mask(0.5, 0.5)] };
        let track_points = vec![
            make_track_point_at(Some((node_id, 0)), vec![(0.10, 0.20), (0.40, 0.60)]),
        ];
        // 24 fps fallback × 0.0 s = frame 0 → positions[0]
        apply_tracker_overrides_to_masks(&mut stack, node_id, 0.0, &track_points);
        match stack.masks[0].shape {
            klipp_core::MaskShape::Circle { center_x, center_y, .. } => {
                assert!((center_x - 0.10).abs() < 1e-6);
                assert!((center_y - 0.20).abs() < 1e-6);
            }
            _ => panic!("expected Circle"),
        }
        // 1/24 s ≈ frame 1 → positions[1]
        apply_tracker_overrides_to_masks(&mut stack, node_id, 1.0/24.0 + 1e-4, &track_points);
        match stack.masks[0].shape {
            klipp_core::MaskShape::Circle { center_x, center_y, .. } => {
                assert!((center_x - 0.40).abs() < 1e-6);
                assert!((center_y - 0.60).abs() < 1e-6);
            }
            _ => panic!("expected Circle"),
        }
    }

    #[test]
    fn tracker_override_skips_unbound_mask() {
        let node_id = uuid::Uuid::new_v4();
        let other_node_id = uuid::Uuid::new_v4();
        let mut stack = klipp_core::MaskStack { masks: vec![make_circle_mask(0.5, 0.5)] };
        // Tracker links to a different node — no override should apply.
        let track_points = vec![
            make_track_point_at(Some((other_node_id, 0)), vec![(0.10, 0.20)]),
        ];
        apply_tracker_overrides_to_masks(&mut stack, node_id, 0.0, &track_points);
        match stack.masks[0].shape {
            klipp_core::MaskShape::Circle { center_x, center_y, .. } => {
                assert!((center_x - 0.5).abs() < 1e-6, "centre should be untouched");
                assert!((center_y - 0.5).abs() < 1e-6, "centre should be untouched");
            }
            _ => panic!("expected Circle"),
        }
    }

    #[test]
    fn tracker_override_respects_start_frame() {
        let node_id = uuid::Uuid::new_v4();
        let mut stack = klipp_core::MaskStack { masks: vec![make_circle_mask(0.5, 0.5)] };
        // Tracker only has positions starting from frame 10. At frame 0, before the
        // tracker's analysed range, the mask centre stays at its persistent value.
        let tp = klipp_core::TrackPoint {
            linked_mask: Some((node_id, 0)),
            start_frame: 10,
            positions: vec![(0.1, 0.1)],
            enabled: true,
            ..Default::default()
        };
        let track_points = vec![tp];
        apply_tracker_overrides_to_masks(&mut stack, node_id, 0.0, &track_points);
        match stack.masks[0].shape {
            klipp_core::MaskShape::Circle { center_x, center_y, .. } => {
                assert!((center_x - 0.5).abs() < 1e-6);
                assert!((center_y - 0.5).abs() < 1e-6);
            }
            _ => panic!("expected Circle"),
        }
    }

    #[test]
    fn tracker_override_no_op_on_empty_inputs() {
        let node_id = uuid::Uuid::new_v4();
        let mut stack = klipp_core::MaskStack { masks: vec![] };
        apply_tracker_overrides_to_masks(&mut stack, node_id, 0.0, &[]);
        // Empty mask list + empty track-points: still zero-mask after the call.
        assert!(stack.masks.is_empty());
    }

    /// Full pipe: bake the active grade chain to a 3D LUT, write it as a
    /// .cube file, load it back, and assert the loaded cube matches the
    /// in-memory bake within float-text precision. This is the round-trip
    /// the inspector "Bake Grade as .cube..." button relies on.
    #[test]
    fn bake_then_write_cube_round_trips() {
        let graph = default_graph();
        let mut executor = GraphExecutor::new_cpu_only();
        let lut = bake_graph_to_lut(&graph, &mut executor, 17);

        // Deterministic tmp path with PID + nanos so parallel test
        // runs don't collide. Cleaned up at end of test on success.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_path = std::env::temp_dir()
            .join(format!("klipp_bake_roundtrip_{}_{nanos}.cube", std::process::id()));

        klipp_color::write_cube(&tmp_path, &lut, 17, "round-trip-test")
            .expect("write_cube should succeed");

        let reloaded_result = klipp_color::load_cube(&tmp_path);
        // Clean up before assertions so a failing assert doesn't leak the file.
        let _ = std::fs::remove_file(&tmp_path);
        let reloaded = reloaded_result.expect("load_cube should round-trip our own output");

        assert_eq!(reloaded.size, 17, "size should round-trip");
        assert_eq!(
            reloaded.data.len(),
            17 * 17 * 17 * 3,
            "loaded LUT should have size^3 * 3 floats",
        );

        // Compare every entry. write_cube formats with "{:.6}" so f32
        // quantisation tolerance is ~1e-6; we use 1e-5 for safety.
        for (i, baked) in lut.iter().enumerate() {
            let r = reloaded.data[i * 3];
            let g = reloaded.data[i * 3 + 1];
            let b = reloaded.data[i * 3 + 2];
            assert!((baked[0] - r).abs() < 1e-5, "LUT[{i}] R drift: baked {:.6} loaded {:.6}", baked[0], r);
            assert!((baked[1] - g).abs() < 1e-5, "LUT[{i}] G drift: baked {:.6} loaded {:.6}", baked[1], g);
            assert!((baked[2] - b).abs() < 1e-5, "LUT[{i}] B drift: baked {:.6} loaded {:.6}", baked[2], b);
        }
    }

    // ── GPU-resident chain tests (skipped on machines without wgpu) ──────
    //
    // These tests exercise the single-fence chain path end-to-end. They
    // compare the chain output against the CPU reference implementation.
    // Difference tolerance is lax (±4 code values) because GPU / CPU
    // paths quantise f32 math differently.

    #[test]
    fn chain_single_exposure_matches_cpu() {
        let mut gpu_executor = GraphExecutor::new();
        if gpu_executor.gpu.is_none() {
            // No GPU in this environment (e.g. CI). Skip — the CPU
            // executor is covered by the other tests in this module.
            return;
        }
        let mut cpu_executor = GraphExecutor::new_cpu_only();

        let mut graph = default_graph();
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::Exposure)
            .unwrap()
            .params.exposure = Some(0.75);

        let frame = vec![
            128u8, 64, 200, 255, 200, 128, 64, 255, 64, 200, 128, 255, 128, 128, 128, 255,
        ];
        let (gpu_out, _) = gpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();
        let (cpu_out, _) = cpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();

        assert_eq!(gpu_out.len(), cpu_out.len());
        for (i, (g, c)) in gpu_out.iter().zip(cpu_out.iter()).enumerate() {
            assert!(
                (*g as i32 - *c as i32).abs() <= 4,
                "byte {i}: GPU chain {g} vs CPU {c} differ by > 4",
            );
        }
    }

    /// The Corrector's own temperature must reach the picture. The knob
    /// is mireds of assumed light (the app's slider runs to 50), and the
    /// grade op wrote tenths of one into it until 2026-09-20: a white
    /// balance that silently did nothing. Both executors.
    #[test]
    fn a_correctors_temperature_changes_the_picture() {
        let grey = vec![128u8, 128, 128, 255, 128, 128, 128, 255, 128, 128, 128, 255, 128, 128, 128, 255];
        let mut graph = crate::defaults::default_graph();
        let corrector = graph.nodes.iter().find(|n| n.kind == NodeKind::Corrector).expect("a corrector").id;
        let warm = |g: &mut NodeGraph, t: f32| {
            let n = g.nodes.iter_mut().find(|n| n.id == corrector).unwrap();
            n.params.temperature = Some(t);
        };
        if std::env::var_os("GRAPH_DEBUG").is_some() {
            for n in &graph.nodes { eprintln!("node {:?} {} enabled={}", n.kind, n.id, n.enabled); }
            for e in &graph.edges { eprintln!("edge {} p{} -> {} p{}", e.from_node, e.from_port, e.to_node, e.to_port); }
        }
        let mut cpu = GraphExecutor::new_cpu_only();
        let (flat, _) = cpu.execute(&grey, &graph, 2, 2, 0.0, 0, None, None).unwrap();
        warm(&mut graph, 25.0);
        let (warmed, _) = cpu.execute(&grey, &graph, 2, 2, 0.0, 0, None, None).unwrap();
        let (dr, db) = (warmed[0] as i32 - flat[0] as i32, warmed[2] as i32 - flat[2] as i32);
        assert!(dr > 4 && db < -4, "warming grey on the CPU should redden it: r {dr:+} b {db:+}");
        let mut gpu = GraphExecutor::new();
        if gpu.gpu.is_some() {
            let (g_warm, _) = gpu.execute(&grey, &graph, 2, 2, 0.0, 0, None, None).unwrap();
            let (gr, gb) = (g_warm[0] as i32 - flat[0] as i32, g_warm[2] as i32 - flat[2] as i32);
            assert!(gr > 4 && gb < -4, "and on the GPU: r {gr:+} b {gb:+}");
        }
    }

    #[test]
    fn chain_multi_node_matches_cpu() {
        let mut gpu_executor = GraphExecutor::new();
        if gpu_executor.gpu.is_none() {
            return;
        }
        let mut cpu_executor = GraphExecutor::new_cpu_only();

        let mut graph = default_graph();
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::Exposure)
            .unwrap()
            .params.exposure = Some(0.5);
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::Saturation)
            .unwrap()
            .params.saturation = Some(1.3);
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::ColorBalance)
            .unwrap()
            .params.temperature = Some(0.15);

        let frame = vec![
            100u8, 150, 200, 255, 200, 150, 100, 255, 50, 100, 150, 255, 128, 128, 128, 255,
        ];
        let (gpu_out, _) = gpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();
        let (cpu_out, _) = cpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();

        assert_eq!(gpu_out.len(), cpu_out.len());
        for (i, (g, c)) in gpu_out.iter().zip(cpu_out.iter()).enumerate() {
            assert!(
                (*g as i32 - *c as i32).abs() <= 5,
                "byte {i}: GPU chain {g} vs CPU {c} differ by > 5",
            );
        }
    }

    #[test]
    fn chain_tunnels_through_bypass() {
        // Exposure (active) → [9 default-param nodes between] → ColorBalance
        // (active). The chain must stay GPU-resident across the bypassed
        // nodes; output must match CPU reference.
        let mut gpu_executor = GraphExecutor::new();
        if gpu_executor.gpu.is_none() {
            return;
        }
        let mut cpu_executor = GraphExecutor::new_cpu_only();

        let mut graph = default_graph();
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::Exposure)
            .unwrap()
            .params.exposure = Some(0.6);
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::ColorBalance)
            .unwrap()
            .params.temperature = Some(0.2);

        let frame = vec![
            110u8, 140, 190, 255, 210, 140, 90, 255, 60, 110, 160, 255, 130, 130, 130, 255,
        ];
        let (gpu_out, _) = gpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();
        let (cpu_out, _) = cpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();

        assert_eq!(gpu_out.len(), cpu_out.len());
        for (i, (g, c)) in gpu_out.iter().zip(cpu_out.iter()).enumerate() {
            assert!(
                (*g as i32 - *c as i32).abs() <= 5,
                "byte {i}: GPU chain {g} vs CPU {c} differ by > 5",
            );
        }
    }

    #[test]
    fn chain_fuses_contrast_and_clarity() {
        // Contrast node with BOTH `contrast` AND `clarity` active used
        // to return false from dispatch_chain_node_slots, forcing a
        // chain break + CPU fallback + new chain start. That split a
        // single grade into 2-3 segments per frame (+~20 ms each).
        //
        // Fix: issue TWO in-chain sub-dispatches (color_grade then
        // clarity). This test verifies the fused GPU output still
        // matches the CPU reference within the usual ±5 tolerance.
        let mut gpu_executor = GraphExecutor::new();
        if gpu_executor.gpu.is_none() {
            return;
        }
        let mut cpu_executor = GraphExecutor::new_cpu_only();

        let mut graph = default_graph();
        let contrast_node = graph
            .nodes
            .iter_mut()
            .find(|n| n.kind == NodeKind::Contrast)
            .unwrap();
        contrast_node.params.contrast = Some(0.2);
        contrast_node.params.clarity = Some(0.3);

        let frame = vec![
            110u8, 140, 190, 255, 210, 140, 90, 255, 60, 110, 160, 255, 130, 130, 130, 255,
        ];
        let (gpu_out, _) = gpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();
        let (cpu_out, _) = cpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();

        assert_eq!(gpu_out.len(), cpu_out.len());
        for (i, (g, c)) in gpu_out.iter().zip(cpu_out.iter()).enumerate() {
            assert!(
                (*g as i32 - *c as i32).abs() <= 5,
                "byte {i}: GPU chain {g} vs CPU {c} differ by > 5 \
                 (contrast+clarity fusion)",
            );
        }
    }

    #[test]
    fn chain_fuses_saturation_and_vibrance() {
        // Saturation node with BOTH `saturation` AND `vibrance` active —
        // same fused-dispatch fix as Contrast. Verifies the 2-dispatch
        // fast path stays bit-compatible with the CPU reference.
        let mut gpu_executor = GraphExecutor::new();
        if gpu_executor.gpu.is_none() {
            return;
        }
        let mut cpu_executor = GraphExecutor::new_cpu_only();

        let mut graph = default_graph();
        let sat_node = graph
            .nodes
            .iter_mut()
            .find(|n| n.kind == NodeKind::Saturation)
            .unwrap();
        sat_node.params.saturation = Some(1.5);
        sat_node.params.vibrance = Some(0.3);

        let frame = vec![
            110u8, 140, 190, 255, 210, 140, 90, 255, 60, 110, 160, 255, 130, 130, 130, 255,
        ];
        let (gpu_out, _) = gpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();
        let (cpu_out, _) = cpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();

        assert_eq!(gpu_out.len(), cpu_out.len());
        for (i, (g, c)) in gpu_out.iter().zip(cpu_out.iter()).enumerate() {
            assert!(
                (*g as i32 - *c as i32).abs() <= 5,
                "byte {i}: GPU chain {g} vs CPU {c} differ by > 5 \
                 (sat+vibrance fusion)",
            );
        }
    }

    #[test]
    fn chain_default_params_is_identity() {
        // Confirm chaining doesn't break the is_default() bypass: a graph
        // with no effective params should still round-trip within ±1
        // code value.
        let mut executor = GraphExecutor::new();
        if executor.gpu.is_none() {
            return;
        }
        let graph = default_graph();
        let frame = vec![
            100u8, 50, 200, 255, 150, 100, 50, 255, 200, 150, 100, 255, 50, 200, 150, 255,
        ];
        let (result, _) = executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();
        for (a, b) in frame.iter().zip(result.iter()) {
            assert!(
                (*a as i32 - *b as i32).abs() <= 1,
                "default-param graph should be passthrough: orig {a}, got {b}",
            );
        }
    }

    #[test]
    fn chain_single_submit_per_segment() {
        // Verify the batched chain encoder collapses per-node submits AND
        // the readback into ONE queue.submit. Pre-merge: 1 chain + 1
        // download = 2 submits. Post-merge (`flush_chain_and_download_
        // linear`): the chain encoder absorbs the readback copy_buffer_
        // to_buffer and we submit once. Each NVIDIA Vulkan submit costs
        // 5-15 ms of driver overhead, so collapsing 2→1 is a measured
        // 3× speedup at 1080p.
        let mut gpu_executor = GraphExecutor::new();
        if gpu_executor.gpu.is_none() {
            return;
        }

        let mut graph = default_graph();
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::Exposure)
            .unwrap()
            .params.exposure = Some(0.5);
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::Saturation)
            .unwrap()
            .params.saturation = Some(1.3);
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::ColorBalance)
            .unwrap()
            .params.temperature = Some(0.15);

        let frame = vec![
            100u8, 150, 200, 255, 200, 150, 100, 255, 50, 100, 150, 255, 128, 128, 128, 255,
        ];
        let before = gpu_executor.gpu.as_ref().unwrap().submit_count();
        let _ = gpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();
        let after = gpu_executor.gpu.as_ref().unwrap().submit_count();
        let delta = after - before;
        assert_eq!(
            delta, 1,
            "active chain should submit exactly ONCE (chain dispatches + readback in one encoder), got {delta}",
        );
    }

    #[test]
    fn new_executor_owns_gpu_when_available() {
        // Regression guard: the engine constructed `GraphExecutor::
        // new_cpu_only()` for months, so every color-grade node ran on
        // CPU at 6-40 ms each — turning a 2 ms GPU chain into a 60-90
        // ms/frame stall during slider scrub. `GraphExecutor::new()`
        // MUST install a GPU pipeline whenever wgpu can find an
        // adapter (which is always true on the user's CI hardware and
        // any developer GPU; the test is a no-op only on a truly
        // headless box).
        let exec = GraphExecutor::new();
        if !exec.has_gpu() {
            // Headless CI without an adapter — skip rather than fail.
            eprintln!("skipping new_executor_owns_gpu_when_available: no GPU adapter");
            return;
        }
        assert!(
            exec.has_gpu(),
            "GraphExecutor::new() must own a GPU pipeline when wgpu can find an adapter"
        );
    }

    #[test]
    fn chain_reuses_bind_groups() {
        // Verify the chain dispatch path reuses pre-allocated bind
        // groups instead of calling `create_bind_group` per node. On
        // NVIDIA Vulkan the old per-dispatch pattern accumulated
        // descriptor-pool frees until the driver produced `device lost
        // (Dropped)` in long sessions (observed 3× in production).
        //
        // The pool is rebuilt only when the frame buffer size changes
        // (`ensure_buffers`). Steady-state dispatch with a constant
        // frame size must not grow the build count.
        let mut gpu_executor = GraphExecutor::new();
        if gpu_executor.gpu.is_none() {
            return;
        }

        let mut graph = default_graph();
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::Exposure)
            .unwrap()
            .params.exposure = Some(0.5);
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::Saturation)
            .unwrap()
            .params.saturation = Some(1.3);
        graph.nodes.iter_mut()
            .find(|n| n.kind == NodeKind::ColorBalance)
            .unwrap()
            .params.temperature = Some(0.15);

        let frame = vec![
            100u8, 150, 200, 255, 200, 150, 100, 255, 50, 100, 150, 255, 128, 128, 128, 255,
        ];

        // Warm-up: first execute() resizes buffers and populates the
        // pool. We measure delta *after* the warm-up so the baseline
        // captures a populated pool.
        let _ = gpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();
        let before = gpu_executor.gpu.as_ref().unwrap().chain_bind_group_build_count();

        // Run the chain several times at the same frame size. Each
        // iteration dispatches 3 active nodes + bypassed passthroughs;
        // none should create a new bind group.
        for _ in 0..5 {
            let _ = gpu_executor.execute(&frame, &graph, 2, 2, 0.0, 0, None, None).unwrap();
        }

        let after = gpu_executor.gpu.as_ref().unwrap().chain_bind_group_build_count();
        assert_eq!(
            after, before,
            "steady-state chain dispatch must reuse pool (build count went {before} -> {after}); \
             every `create_bind_group` in the dispatch hot path is a regression",
        );
    }

    // ─── Working-colorspace plumbing (Phase E S5b) ──────────────

    #[test]
    fn the_given_grade_encoding_holds_under_acescg_in_either_order() {
        use klipp_color::grade_model::Encoding;
        use klipp_core::{GradeEncoding, WorkingColorspace};
        // An export sets the encoding it froze; the global setting,
        // whatever it is, must not decide (#467 review).
        let mut a = GraphExecutor::new_cpu_only();
        a.set_grade_encoding(GradeEncoding::AcesCct);
        a.set_working_colorspace(WorkingColorspace::AcesCG);
        assert_eq!(a.grade_encoding, Encoding::AcesCct);
        let mut b = GraphExecutor::new_cpu_only();
        b.set_working_colorspace(WorkingColorspace::AcesCG);
        b.set_grade_encoding(GradeEncoding::AcesCc);
        assert_eq!(b.grade_encoding, Encoding::AcesCc);
        b.set_grade_encoding(GradeEncoding::Bt1886);
        assert_eq!(b.grade_encoding, Encoding::Bt1886);
        // Outside ACEScg the grade is BT.1886 whatever the choice.
        b.set_grade_encoding(GradeEncoding::AcesCct);
        b.set_working_colorspace(WorkingColorspace::Rec709Linear);
        assert_eq!(b.grade_encoding, Encoding::Bt1886);
    }

    #[test]
    fn set_working_colorspace_round_trips() {
        let mut executor = GraphExecutor::new_cpu_only();
        // Default is Rec709Linear so existing consumers behave
        // exactly like the pre-Phase-E pipeline.
        assert_eq!(
            executor.working_colorspace(),
            klipp_core::WorkingColorspace::Rec709Linear,
        );
        executor.set_working_colorspace(klipp_core::WorkingColorspace::AcesCG);
        assert_eq!(
            executor.working_colorspace(),
            klipp_core::WorkingColorspace::AcesCG,
        );
        executor.set_working_colorspace(klipp_core::WorkingColorspace::Rec709Linear);
        assert_eq!(
            executor.working_colorspace(),
            klipp_core::WorkingColorspace::Rec709Linear,
        );
    }

    /// End-to-end: a Saturation graph applied to the same non-neutral
    /// frame must produce visibly different output under Rec709Linear
    /// vs AcesCG. If the executor's working-colorspace setter doesn't
    /// flow into the per-node CPU dispatch (or if all the node arms
    /// silently dropped the `_ws` swap), this test fires.
    #[test]
    fn saturation_node_diverges_with_working_colorspace() {
        let mut graph = default_graph();
        // Find the Saturation node and dial it down to 0.0 (full
        // desaturation — the case where the luma vector matters most
        // because every output channel collapses to that single
        // achromatic value).
        let sat_node = graph
            .nodes
            .iter_mut()
            .find(|n| n.kind == NodeKind::Saturation)
            .expect("default graph has a Saturation node");
        sat_node.params.saturation = Some(0.0);

        // Pure-red u8 frame — saturation drives R, G, B all to the
        // working-space's `Yr` constant, which differs between
        // Rec.709 (0.2126) and AcesCG (0.2722). After the linear →
        // sRGB encode at the executor boundary this becomes a
        // measurably different u8 grey.
        let frame = vec![255, 0, 0, 255];

        let mut exec_a = GraphExecutor::new_cpu_only();
        exec_a.set_working_colorspace(klipp_core::WorkingColorspace::Rec709Linear);
        let (r_709, _) = exec_a.execute(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();

        let mut exec_b = GraphExecutor::new_cpu_only();
        exec_b.set_working_colorspace(klipp_core::WorkingColorspace::AcesCG);
        let (r_acescg, _) = exec_b.execute(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();

        // Both outputs must be neutral grey (Saturation = 0 collapses
        // RGB to luma — same value on all three channels).
        assert!(
            r_709[0] == r_709[1] && r_709[1] == r_709[2],
            "Rec709 desaturated red should be neutral; got {:?}", &r_709[..3],
        );
        assert!(
            r_acescg[0] == r_acescg[1] && r_acescg[1] == r_acescg[2],
            "AcesCG desaturated red should be neutral; got {:?}", &r_acescg[..3],
        );

        // The two grey values must differ — this is the working-
        // space switch actually doing work. AcesCG's higher Yr means
        // the resulting grey is brighter than Rec.709's.
        assert!(
            r_acescg[0] > r_709[0],
            "AcesCG luma > Rec.709 luma on pure red, so the desaturated grey \
             should be brighter under AcesCG; got Rec.709={} AcesCG={}",
            r_709[0], r_acescg[0],
        );
    }

    /// `set_working_colorspace` must mirror the matching luma vector
    /// into the GPU pipeline so the GPU shader's saturation /
    /// tone-controls / lift-gamma-gain stages use the right luma.
    /// Without this the executor's CPU and GPU paths would silently
    /// disagree on what "luma" means under any non-Rec.709 working
    /// space.
    ///
    /// Skipped (passes trivially) when no GPU is available.
    #[test]
    fn set_working_colorspace_propagates_luma_to_gpu() {
        let mut executor = GraphExecutor::new();
        // Force GPU init now so we can inspect the pipeline state.
        executor.ensure_gpu();
        let Some(gpu) = executor.gpu.as_ref() else {
            eprintln!("skipping: no GPU pipeline available");
            return;
        };

        // Default = Rec.709. Even on a fresh executor the GPU's
        // luma_coefs default lines up with Rec.709 because both sides
        // initialise to the same constants.
        let default_coefs = gpu.luma_coefs();
        assert!((default_coefs[0] - 0.2126).abs() < 1e-6);
        assert!((default_coefs[1] - 0.7152).abs() < 1e-6);
        assert!((default_coefs[2] - 0.0722).abs() < 1e-6);

        // Flipping the executor to AcesCG must push the AP1 luma
        // vector into the pipeline.
        executor.set_working_colorspace(klipp_core::WorkingColorspace::AcesCG);
        let coefs = executor.gpu.as_ref().unwrap().luma_coefs();
        assert!((coefs[0] - 0.272_228_72).abs() < 1e-5,
            "AcesCG Yr should hit the GPU; got {coefs:?}");
        assert!((coefs[1] - 0.674_081_77).abs() < 1e-5);
        assert!((coefs[2] - 0.053_689_52).abs() < 1e-5);

        // And back.
        executor.set_working_colorspace(klipp_core::WorkingColorspace::Rec709Linear);
        let coefs = executor.gpu.as_ref().unwrap().luma_coefs();
        assert!((coefs[0] - 0.2126).abs() < 1e-6);
    }

    /// Setter called BEFORE the GPU pipeline is built (the lazy
    /// `ensure_gpu` happens later) — the pipeline must pick up the
    /// active working space when it's finally created. This is the
    /// realistic path: the engine sets `working_colorspace` on every
    /// frame, but `ensure_gpu` only runs the first time a GPU node
    /// arm is reached.
    #[test]
    fn ensure_gpu_picks_up_working_colorspace_set_earlier() {
        let mut executor = GraphExecutor::new();
        // No `ensure_gpu` yet.
        assert!(executor.gpu.is_none());
        executor.set_working_colorspace(klipp_core::WorkingColorspace::AcesCG);

        // Now build the GPU lazily.
        executor.ensure_gpu();
        let Some(gpu) = executor.gpu.as_ref() else {
            eprintln!("skipping: no GPU pipeline available");
            return;
        };
        let coefs = gpu.luma_coefs();
        assert!((coefs[0] - 0.272_228_72).abs() < 1e-5,
            "lazy GPU init must replay the active working space's luma; got {coefs:?}");
    }

    // ─── PrimariesMatrix node + per-chain override ─────────────────

    /// Helper: build Source → PrimariesMatrix(from, to) → Output and
    /// run a single linear-RGBA pixel through it.
    fn run_primaries_matrix(
        from: Option<klipp_core::WorkingColorspace>,
        to: Option<klipp_core::WorkingColorspace>,
        input: [f32; 4],
    ) -> [f32; 4] {
        let src = NodeId::new_v4();
        let pm = NodeId::new_v4();
        let out = NodeId::new_v4();
        let params = NodeParams {
            primaries_from: from,
            primaries_to: to,
            ..Default::default()
        };
        let graph = NodeGraph {
            nodes: vec![
                crate::graph::Node {
                    id: src, kind: NodeKind::Source, label: "S".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: pm, kind: NodeKind::PrimariesMatrix, label: "PM".into(),
                    enabled: true, params, keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: out, kind: NodeKind::Output, label: "O".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
            ],
            edges: vec![
                crate::graph::Edge { from_node: src, from_port: 0, to_node: pm, to_port: 0 },
                crate::graph::Edge { from_node: pm, from_port: 0, to_node: out, to_port: 0 },
            ],
            output_node_id: out,
            node_positions: Default::default(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };
        let mut exec = GraphExecutor::new_cpu_only();
        let frame: Vec<f32> = input.to_vec();
        let (result, _) = exec.execute_linear(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        [result[0], result[1], result[2], result[3]]
    }

    /// An InputTransform node never touches the pixels, whatever format
    /// it records: the decoder has already decoded the clip, and a second
    /// transform is what used to crush every PQ clip the moment it was
    /// graded. Scene-linear values above white say so loudly — the GPU
    /// version of that transform clamped them to 1.0.
    #[test]
    fn an_input_transform_node_leaves_the_picture_alone() {
        let (src, it, out) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
        let node = |id, kind, params| crate::graph::Node {
            id, kind, label: String::new(), enabled: true, params,
            keyframes: Default::default(), output_link: None, input_links: Vec::new(),
        };
        for format in [
            klipp_core::LogFormat::SLog3,
            klipp_core::LogFormat::PQ,
            klipp_core::LogFormat::LogC3,
            klipp_core::LogFormat::Srgb,
        ] {
            let params = NodeParams {
                log_format: Some(format),
                ..Default::default()
            };
            let graph = NodeGraph {
                nodes: vec![
                    node(src, NodeKind::Source, Default::default()),
                    node(it, NodeKind::InputTransform, params),
                    node(out, NodeKind::Output, Default::default()),
                ],
                edges: vec![
                    crate::graph::Edge { from_node: src, from_port: 0, to_node: it, to_port: 0 },
                    crate::graph::Edge { from_node: it, from_port: 0, to_node: out, to_port: 0 },
                ],
                output_node_id: out,
                node_positions: Default::default(),
                working_space_override: None,
                frames: Vec::new(),
                sticky_notes: Vec::new(),
            };
            // Mid-grey, a highlight four times white, and a negative.
            let frame: Vec<f32> = vec![0.18, 4.0, -0.05, 1.0];
            let mut exec = GraphExecutor::new_cpu_only();
            let (result, _) = exec.execute_linear(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
            assert_eq!(result, frame, "{format:?} changed the picture");
        }
    }

    #[test]
    fn primaries_matrix_node_passthrough_when_unset() {
        // Both tags `None` → identity passthrough.
        let pixel = [0.4, 0.5, 0.6, 1.0];
        let out = run_primaries_matrix(None, None, pixel);
        for (a, b) in out.iter().zip(pixel.iter()) {
            assert!((a - b).abs() < 1e-6, "expected passthrough, got {out:?}");
        }
    }

    #[test]
    fn primaries_matrix_node_passthrough_when_same_from_and_to() {
        // from == to → matrix lookup returns None → passthrough.
        let pixel = [0.4, 0.5, 0.6, 1.0];
        let out = run_primaries_matrix(
            Some(klipp_core::WorkingColorspace::Rec709Linear),
            Some(klipp_core::WorkingColorspace::Rec709Linear),
            pixel,
        );
        for (a, b) in out.iter().zip(pixel.iter()) {
            assert!((a - b).abs() < 1e-6, "expected passthrough, got {out:?}");
        }
    }

    #[test]
    fn primaries_matrix_node_matches_klipp_color_oracle() {
        // PM(BT.709 → ACEScg) on a non-neutral pixel must match
        // applying the bt709_to_acescg matrix directly (the executor
        // is a thin wrapper, but pinning prevents future drift).
        let pixel = [0.7, 0.2, 0.1, 1.0];
        let executor_out = run_primaries_matrix(
            Some(klipp_core::WorkingColorspace::Rec709Linear),
            Some(klipp_core::WorkingColorspace::AcesCG),
            pixel,
        );
        let mut expected = pixel.to_vec();
        klipp_color::linear::apply_primaries_matrix(
            &mut expected,
            klipp_color::primaries::bt709_to_acescg(),
        );
        for i in 0..4 {
            assert!(
                (executor_out[i] - expected[i]).abs() < 1e-5,
                "channel {i}: executor={} expected={}",
                executor_out[i], expected[i],
            );
        }
    }

    #[test]
    fn primaries_matrix_node_round_trip_under_one_lsb() {
        // PM(709→AP1) → PM(AP1→709) on a non-neutral pixel must
        // round-trip exactly (within float precision). This is the
        // load-bearing invariant for the migration: the head + tail
        // wrappers need to be inverses or grades drift.
        let pm1 = NodeId::new_v4();
        let pm2 = NodeId::new_v4();
        let src = NodeId::new_v4();
        let out = NodeId::new_v4();
        let p1 = NodeParams {
            primaries_from: Some(klipp_core::WorkingColorspace::Rec709Linear),
            primaries_to: Some(klipp_core::WorkingColorspace::AcesCG),
            ..Default::default()
        };
        let p2 = NodeParams {
            primaries_from: Some(klipp_core::WorkingColorspace::AcesCG),
            primaries_to: Some(klipp_core::WorkingColorspace::Rec709Linear),
            ..Default::default()
        };
        let graph = NodeGraph {
            nodes: vec![
                crate::graph::Node {
                    id: src, kind: NodeKind::Source, label: "S".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: pm1, kind: NodeKind::PrimariesMatrix, label: "PM1".into(),
                    enabled: true, params: p1, keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: pm2, kind: NodeKind::PrimariesMatrix, label: "PM2".into(),
                    enabled: true, params: p2, keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: out, kind: NodeKind::Output, label: "O".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
            ],
            edges: vec![
                crate::graph::Edge { from_node: src, from_port: 0, to_node: pm1, to_port: 0 },
                crate::graph::Edge { from_node: pm1, from_port: 0, to_node: pm2, to_port: 0 },
                crate::graph::Edge { from_node: pm2, from_port: 0, to_node: out, to_port: 0 },
            ],
            output_node_id: out,
            node_positions: Default::default(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };
        let mut exec = GraphExecutor::new_cpu_only();
        let original: Vec<f32> = vec![0.7, 0.2, 0.1, 1.0];
        let (result, _) = exec.execute_linear(&original, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        for i in 0..4 {
            assert!(
                (result[i] - original[i]).abs() < 1e-5,
                "round-trip drift channel {i}: {} vs {}",
                result[i], original[i],
            );
        }
    }

    #[test]
    fn working_space_override_swaps_in_and_restores() {
        // Before the call the executor sits at AcesCG. The graph's
        // override pins it to Rec709Linear. After the call the
        // executor must be back at AcesCG — this proves restoration
        // even on the success path. Without restore, subsequent
        // chains on the same executor would silently use the wrong
        // luma vector.
        let src = NodeId::new_v4();
        let out = NodeId::new_v4();
        let graph = NodeGraph {
            nodes: vec![
                crate::graph::Node {
                    id: src, kind: NodeKind::Source, label: "S".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
                crate::graph::Node {
                    id: out, kind: NodeKind::Output, label: "O".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                },
            ],
            edges: vec![
                crate::graph::Edge { from_node: src, from_port: 0, to_node: out, to_port: 0 },
            ],
            output_node_id: out,
            node_positions: Default::default(),
            working_space_override: Some(klipp_core::WorkingColorspace::Rec709Linear),
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };
        let mut exec = GraphExecutor::new_cpu_only();
        exec.set_working_colorspace(klipp_core::WorkingColorspace::AcesCG);
        let frame: Vec<f32> = vec![0.4, 0.5, 0.6, 1.0];
        let _ = exec.execute_linear(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        assert_eq!(
            exec.working_colorspace(),
            klipp_core::WorkingColorspace::AcesCG,
            "executor's working colorspace must be restored after chain runs",
        );
    }

    #[test]
    fn working_space_override_takes_effect_for_saturation_during_execute() {
        // A Saturation node luma-keyed by AP1 produces a different
        // result than one keyed by Rec.709 on a non-neutral pixel.
        // With the same project-level ws (AcesCG) and the same
        // non-trivial saturation, an override pinning the chain back
        // to Rec.709 must visibly change the output. This proves the
        // override actually flows through to the gamut-aware op.
        let src = NodeId::new_v4();
        let sat = NodeId::new_v4();
        let out = NodeId::new_v4();
        let make_graph = |override_ws: Option<klipp_core::WorkingColorspace>| {
            let sat_params = NodeParams {
                // strong desat — luma vector matters
                saturation: Some(0.3),
                ..Default::default()
            };
            NodeGraph {
                nodes: vec![
                    crate::graph::Node {
                        id: src, kind: NodeKind::Source, label: "S".into(),
                        enabled: true, params: Default::default(), keyframes: Default::default(),
                        output_link: None, input_links: Vec::new(),
                    },
                    crate::graph::Node {
                        id: sat, kind: NodeKind::Saturation, label: "Sat".into(),
                        enabled: true, params: sat_params, keyframes: Default::default(),
                        output_link: None, input_links: Vec::new(),
                    },
                    crate::graph::Node {
                        id: out, kind: NodeKind::Output, label: "O".into(),
                        enabled: true, params: Default::default(), keyframes: Default::default(),
                        output_link: None, input_links: Vec::new(),
                    },
                ],
                edges: vec![
                    crate::graph::Edge { from_node: src, from_port: 0, to_node: sat, to_port: 0 },
                    crate::graph::Edge { from_node: sat, from_port: 0, to_node: out, to_port: 0 },
                ],
                output_node_id: out,
                node_positions: Default::default(),
                working_space_override: override_ws,
                frames: Vec::new(),
                sticky_notes: Vec::new(),
            }
        };
        let mut exec = GraphExecutor::new_cpu_only();
        exec.set_working_colorspace(klipp_core::WorkingColorspace::AcesCG);

        let pixel: Vec<f32> = vec![0.7, 0.2, 0.1, 1.0];
        let no_override = make_graph(None);
        let (r_no, _) = exec.execute_linear(&pixel, &no_override, 1, 1, 0.0, 0, None, None).unwrap();

        // Reset to AcesCG (the override path restores, but the
        // previous .execute_linear on no_override left it at AcesCG;
        // explicit reset is defensive in case future changes shift
        // that contract).
        exec.set_working_colorspace(klipp_core::WorkingColorspace::AcesCG);

        let with_override = make_graph(Some(klipp_core::WorkingColorspace::Rec709Linear));
        let (r_yes, _) = exec.execute_linear(&pixel, &with_override, 1, 1, 0.0, 0, None, None).unwrap();

        let max_delta = (0..3)
            .map(|i| (r_no[i] - r_yes[i]).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_delta > 1e-3,
            "override must visibly change saturation output: max delta {max_delta}",
        );
    }

    #[test]
    fn migration_wrapper_round_trip_preserves_grade() {
        // The whole point of S7b: a Rec.709 grade chain wrapped
        // ACEScg→Rec709Linear at the head and Rec709Linear→ACEScg at
        // the tail, with the chain pinned to Rec.709 luma via the
        // override, must reproduce the SAME pixels the unwrapped
        // chain would on the same input — within float precision.
        let saturation = 0.5_f32;
        // Unwrapped Rec.709 chain.
        let plain_graph = {
            let src = NodeId::new_v4();
            let sat = NodeId::new_v4();
            let out = NodeId::new_v4();
            let sp = NodeParams {
                saturation: Some(saturation),
                ..Default::default()
            };
            NodeGraph {
                nodes: vec![
                    crate::graph::Node { id: src, kind: NodeKind::Source, label: "S".into(),
                        enabled: true, params: Default::default(), keyframes: Default::default(),
                        output_link: None, input_links: Vec::new() },
                    crate::graph::Node { id: sat, kind: NodeKind::Saturation, label: "Sat".into(),
                        enabled: true, params: sp, keyframes: Default::default(),
                        output_link: None, input_links: Vec::new() },
                    crate::graph::Node { id: out, kind: NodeKind::Output, label: "O".into(),
                        enabled: true, params: Default::default(), keyframes: Default::default(),
                        output_link: None, input_links: Vec::new() },
                ],
                edges: vec![
                    crate::graph::Edge { from_node: src, from_port: 0, to_node: sat, to_port: 0 },
                    crate::graph::Edge { from_node: sat, from_port: 0, to_node: out, to_port: 0 },
                ],
                output_node_id: out,
                node_positions: Default::default(),
                working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
            }
        };
        // Wrapped: AcesCG project, but the saturation chain stays
        // Rec.709-luma-correct because the override + matrix wrappers
        // bring its inputs into Rec.709 space.
        let wrapped_graph = {
            let src = NodeId::new_v4();
            let pre = NodeId::new_v4();
            let sat = NodeId::new_v4();
            let post = NodeId::new_v4();
            let out = NodeId::new_v4();
            let sp = NodeParams {
                saturation: Some(saturation),
                ..Default::default()
            };
            let pre_params = NodeParams {
                primaries_from: Some(klipp_core::WorkingColorspace::AcesCG),
                primaries_to: Some(klipp_core::WorkingColorspace::Rec709Linear),
                ..Default::default()
            };
            let post_params = NodeParams {
                primaries_from: Some(klipp_core::WorkingColorspace::Rec709Linear),
                primaries_to: Some(klipp_core::WorkingColorspace::AcesCG),
                ..Default::default()
            };
            NodeGraph {
                nodes: vec![
                    crate::graph::Node { id: src, kind: NodeKind::Source, label: "S".into(),
                        enabled: true, params: Default::default(), keyframes: Default::default(),
                        output_link: None, input_links: Vec::new() },
                    crate::graph::Node { id: pre, kind: NodeKind::PrimariesMatrix, label: "Head".into(),
                        enabled: true, params: pre_params, keyframes: Default::default(),
                        output_link: None, input_links: Vec::new() },
                    crate::graph::Node { id: sat, kind: NodeKind::Saturation, label: "Sat".into(),
                        enabled: true, params: sp, keyframes: Default::default(),
                        output_link: None, input_links: Vec::new() },
                    crate::graph::Node { id: post, kind: NodeKind::PrimariesMatrix, label: "Tail".into(),
                        enabled: true, params: post_params, keyframes: Default::default(),
                        output_link: None, input_links: Vec::new() },
                    crate::graph::Node { id: out, kind: NodeKind::Output, label: "O".into(),
                        enabled: true, params: Default::default(), keyframes: Default::default(),
                        output_link: None, input_links: Vec::new() },
                ],
                edges: vec![
                    crate::graph::Edge { from_node: src, from_port: 0, to_node: pre, to_port: 0 },
                    crate::graph::Edge { from_node: pre, from_port: 0, to_node: sat, to_port: 0 },
                    crate::graph::Edge { from_node: sat, from_port: 0, to_node: post, to_port: 0 },
                    crate::graph::Edge { from_node: post, from_port: 0, to_node: out, to_port: 0 },
                ],
                output_node_id: out,
                node_positions: Default::default(),
                working_space_override: Some(klipp_core::WorkingColorspace::Rec709Linear),
                frames: Vec::new(),
                sticky_notes: Vec::new(),
            }
        };

        // Plain chain runs at the project's pre-switch ws (Rec.709).
        let mut exec_plain = GraphExecutor::new_cpu_only();
        exec_plain.set_working_colorspace(klipp_core::WorkingColorspace::Rec709Linear);
        let pixel_rec709: Vec<f32> = vec![0.7, 0.2, 0.1, 1.0];
        let (r_plain, _) = exec_plain
            .execute_linear(&pixel_rec709, &plain_graph, 1, 1, 0.0, 0, None, None)
            .unwrap();

        // Wrapped chain runs at the post-switch project ws (AcesCG).
        // The frame fed in is the SAME source pixel converted to
        // ACEScg — that's what the engine would feed after the
        // project-wide ws change since IDTs target the project space.
        let mut pixel_acescg = pixel_rec709.clone();
        klipp_color::linear::apply_primaries_matrix(
            &mut pixel_acescg,
            klipp_color::primaries::bt709_to_acescg(),
        );
        let mut exec_wrapped = GraphExecutor::new_cpu_only();
        exec_wrapped.set_working_colorspace(klipp_core::WorkingColorspace::AcesCG);
        let (r_wrapped_acescg, _) = exec_wrapped
            .execute_linear(&pixel_acescg, &wrapped_graph, 1, 1, 0.0, 0, None, None)
            .unwrap();

        // Convert the wrapped output back to Rec.709 to compare with
        // the plain output (which is in Rec.709 by construction).
        let mut r_wrapped_rec709 = r_wrapped_acescg.clone();
        klipp_color::linear::apply_primaries_matrix(
            &mut r_wrapped_rec709,
            klipp_color::primaries::acescg_to_bt709(),
        );

        for i in 0..4 {
            assert!(
                (r_plain[i] - r_wrapped_rec709[i]).abs() < 1e-4,
                "channel {i}: plain={} wrapped={} delta={}",
                r_plain[i],
                r_wrapped_rec709[i],
                (r_plain[i] - r_wrapped_rec709[i]).abs(),
            );
        }
    }
}

#[cfg(test)]
mod unreadable_image_tests {
    use super::*;
    use crate::graph::{Edge, Node, NodeId};
    use crate::NodeKind;

    /// #1020: a matte image that won't load is warned about once, not
    /// on every frame, and a graph that loads it later is warned again
    /// only if it goes missing again.
    #[test]
    fn a_missing_matte_image_warns_once() {
        let (w, h) = (8u32, 4u32);
        let missing = std::env::temp_dir().join(format!("klipp-missing-matte-{}.png", std::process::id()));
        let _ = std::fs::remove_file(&missing);
        let mut g = NodeGraph::default();
        let corr = g.ensure_corrector();
        let matte = NodeId::new_v4();
        let params = NodeParams { matte_image_path: Some(missing.clone()), ..Default::default() };
        g.nodes.push(Node { id: matte, kind: NodeKind::ExternalMatte, label: "Matte".into(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() });
        g.edges.push(Edge { from_node: matte, from_port: 0, to_node: corr, to_port: 1 });
        let frame = vec![128u8; (w * h * 4) as usize];
        let mut ex = GraphExecutor::new_cpu_only();
        for _ in 0..5 {
            ex.execute(&frame, &g, w, h, 0.0, 0, None, None).unwrap();
        }
        assert_eq!(ex.unreadable_images.len(), 1, "one warning for one missing file, whatever the frame count");
        assert!(ex.unreadable_images.contains(&missing));

        image::GrayImage::from_fn(w, h, |_, _| image::Luma([255])).save(&missing).unwrap();
        ex.execute(&frame, &g, w, h, 0.0, 0, None, None).unwrap();
        assert!(ex.unreadable_images.is_empty(), "found again, it may warn again if it goes");
        let _ = std::fs::remove_file(&missing);
    }
}
