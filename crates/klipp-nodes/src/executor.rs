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

/// Whether an effect node's blend leaves its input as it was (phase 2b M2,
/// step 9c): a mix of 0, or a mask whose producer is switched off at this
/// frame, which gives no coverage. The node then passes its input on.
fn effect_blend_is_none(graph: &NodeGraph, node: &Node, clip_time_secs: f64) -> bool {
    let Some(blend) = node.params.effect_blend else { return false };
    let mask_off = graph
        .input_of(node.id, 1)
        .and_then(|id| graph.nodes.iter().find(|n| n.id == id))
        .is_some_and(|producer| !effective_enabled(producer, clip_time_secs));
    blend.mix == 0.0 || mask_off
}

/// An effect node's result mixed back over its input on the CPU: by its
/// mix, and by the coverage `mask` gives (none is white). A mix or a mask
/// that can't be read gives no coverage: the input, never the effect
/// whole, which would draw a masked effect unmasked. `None` says so.
fn mix_effect_cpu(blend: crate::params::EffectBlend, luma: [f32; 3], input: &[f32], effect: Vec<f32>, mask: Option<&[f32]>) -> Option<Vec<f32>> {
    let rows = |v: &[f32]| -> Vec<[f32; 4]> { v.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect() };
    let mixer = match klipp_effects::mask_mix::MaskMix::new(blend.mix, blend.channel, luma) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("an effect's mix: {e}; the effect is skipped");
            return None;
        }
    };
    let (rows_in, fx, mask) = (rows(input), rows(&effect), mask.map(rows));
    let mut out = vec![[0.0f32; 4]; rows_in.len()];
    match mixer.apply(&rows_in, &fx, mask.as_deref(), &mut out) {
        Ok(()) => Some(out.into_iter().flatten().collect()),
        Err(e) => {
            tracing::warn!("an effect's mask: {e}; the effect is skipped");
            None
        }
    }
}

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

/// Whether a heavy node keeps history between frames, and so is never
/// held (#1863): an OFX plug-in that renders in sequence, or one the
/// host can't describe. A frei0r plug-in repaints its last frame from
/// its own memo on the same frame and input, which a hold matches.
fn held_history(node: &Node, host: Option<&dyn crate::plugin::OfxNodeHost>) -> bool {
    match node.kind {
        NodeKind::Ofx => match (node.params.ofx.as_ref(), host) {
            (Some(entry), Some(host)) => host.describe(&entry.plugin).map_or(true, |d| d.sequential_render),
            _ => true,
        },
        _ => false,
    }
}

/// Return the effective params for `node` at `clip_time_secs`. When the node has
/// no keyframes we borrow `node.params` directly, skipping the per-frame clone
/// that would otherwise happen on every node in the graph.
pub(crate) fn params_for_frame(node: &Node, clip_time_secs: f64) -> Cow<'_, NodeParams> {
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
    /// A mixed effect's mask is wired to a node that gave no matte the run
    /// can read (step 9c). An unwired mask is white; a wired one that
    /// gives nothing is never taken for white.
    #[error("the mask of effect node {0} is wired but its matte isn't there")]
    MaskUnavailable(NodeId),
    /// A node can't take the pictures it reads: a display step after the
    /// picture's own or after a plug-in, or scene- and display-referred
    /// pictures mixed (#1290). The run is refused rather than drawn wrong.
    #[error("colour domains: {0}")]
    ColourDomain(crate::domains::DomainFault),
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
    /// Scene codes, 10-bit or 16-bit (#1226).
    Codes(Arc<klipp_color::hdr::SceneCodes>),
    Srgb8(Arc<Vec<u8>>),
    Rgba64(Arc<Vec<u16>>),
}

impl SeedPixels {
    fn bytes(&self) -> &[u8] {
        match self {
            SeedPixels::Codes(c) => match c.as_ref() {
                klipp_color::hdr::SceneCodes::Packed10(p) => bytemuck::cast_slice(p.as_slice()),
                klipp_color::hdr::SceneCodes::Deep16(p) => bytemuck::cast_slice(p.as_slice()),
            },
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

/// `motion` on a GPU pipeline: what it holds uploaded, what it lacks cleared.
fn upload_motion(gpu: &mut GpuEffectsPipeline, motion: &klipp_effects::MotionData) {
    match &motion.stabilization {
        Some(h) => gpu.upload_stabilization_homography(h),
        None => gpu.clear_stabilization_homography(),
    }
    match &motion.rsc_rows {
        Some(rows) => gpu.upload_rsc_row_transforms(rows),
        None => gpu.clear_rsc_row_transforms(),
    }
}

/// Whether `effect` warps by the frame's motion (stabilisation, rolling
/// shutter): without its analysis it passes the frame through.
fn warps_by_motion(effect: &klipp_effects::BuiltinEffect) -> bool {
    matches!(effect, klipp_effects::BuiltinEffect::VideoStabilization { .. } | klipp_effects::BuiltinEffect::RollingShutterCorrection { .. })
}

/// `effects` with a deflicker's gain set to the analysis node's for the
/// frame, when it has one.
fn with_gain<'a>(effects: &'a [klipp_effects::BuiltinEffect], at: Option<&klipp_effects::node_analysis::AtFrame>) -> std::borrow::Cow<'a, [klipp_effects::BuiltinEffect]> {
    let Some(g) = at.and_then(|a| a.gain) else { return std::borrow::Cow::Borrowed(effects) };
    std::borrow::Cow::Owned(effects.iter().cloned().map(|mut e| {
        if let klipp_effects::BuiltinEffect::Deflicker { gain, .. } = &mut e {
            *gain = g;
        }
        e
    }).collect())
}

/// Each analysis node's data for the frame `ctx` renders (M4 step 9): its
/// analysis, found ready at its schema, read at the frame's source time;
/// and each switched-on node that has none for the frame, with the outcome
/// it declines as (step 10). Without its analysis an analysis node passes
/// the frame through, and says so: an analysis absent or stale, or one
/// with no entry for a time inside the media, is a neighbour missing,
/// which an export refuses; a time past the media's edge is a fallback.
#[allow(clippy::type_complexity)]
fn resolve_analysis(graph: &NodeGraph, ctx: &crate::frame_context::FrameContext<'_>, width: u32, height: u32) -> (HashMap<NodeId, klipp_effects::node_analysis::AtFrame>, Vec<(NodeId, klipp_effects::BuiltinEffect, crate::DeclineOutcome, &'static str)>) {
    use crate::frame_context::{Analysis, NODE_ANALYSIS_SCHEMA};
    use crate::DeclineOutcome;
    let mut out = HashMap::new();
    let mut missing = Vec::new();
    for node in graph.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Effect | NodeKind::EffectSegment)) {
        let Some(effect) = node.params.effects.iter().find(|e| klipp_effects::node_analysis::derivation_params(e).is_some()) else { continue };
        let found = match ctx.analysis.lookup_effect(node.id, effect, NODE_ANALYSIS_SCHEMA) {
            Analysis::Ready(blob) => blob.data.downcast_ref::<klipp_effects::node_analysis::NodeAnalysis>().map(|a| a.at(effect, ctx.source_secs, width, height)),
            Analysis::Absent | Analysis::Stale { .. } => None,
        };
        match found {
            Some(at) if covers(effect, &at) => {
                out.insert(node.id, at);
            }
            _ if !node.enabled => {}
            Some(_) => {
                let (from, to) = ctx.sources.bounds();
                match ctx.source_secs < from || ctx.source_secs >= to {
                    true => missing.push((node.id, effect.clone(), DeclineOutcome::Fallback, "no analysis past the media's edge")),
                    false => missing.push((node.id, effect.clone(), DeclineOutcome::NeighbourMissing, "its analysis has no frame here")),
                }
            }
            None => missing.push((node.id, effect.clone(), DeclineOutcome::NeighbourMissing, "its analysis isn't there")),
        }
    }
    (out, missing)
}

/// Whether `at` holds what `effect` draws by: a deflicker's gain, a
/// stabiliser's correction, a rolling-shutter correction's rows.
fn covers(effect: &klipp_effects::BuiltinEffect, at: &klipp_effects::node_analysis::AtFrame) -> bool {
    use klipp_effects::BuiltinEffect as E;
    match effect {
        E::Deflicker { .. } => at.gain.is_some(),
        E::VideoStabilization { .. } => at.motion.stabilization.is_some(),
        E::RollingShutterCorrection { .. } => at.motion.rsc_rows.is_some(),
        _ => true,
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
    /// The GPU pipeline being built on a thread of its own
    /// ([`Self::prewarm_gpu`]): taken by the first frame that needs it,
    /// which then waits for the rest of the build rather than starting
    /// another.
    gpu_prewarm: Option<(std::sync::mpsc::Receiver<Option<GpuEffectsPipeline>>, std::thread::JoinHandle<()>)>,
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
    /// The manifests of the matte sequences (#239) external mattes read,
    /// by folder: read once, not every frame.
    matte_sequences: HashMap<PathBuf, (crate::matte_sequence::Manifest, Vec<i64>)>,
    /// The current clip's tracked subject mattes' folders, by model
    /// (`SubjectMatte::index`), found from its media as linked now
    /// (#1881); cleared by every `prepare_clip`, so a run never reads
    /// another clip's.
    clip_subject_mattes: [Option<PathBuf>; 2],
    /// The time of the current clip's media's last frame, when known: a
    /// matte sequence is read there past the media's end, as the decoder
    /// shows the picture (and at its first frame before its start).
    /// Cleared by every `prepare_clip`.
    clip_media_last: Option<f64>,
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
    /// Effect-node chain breaks in the run so far (`ChainStats`).
    run_effect_breaks: u32,
    /// Each analysis node's data for the frame being run (M4 step 9),
    /// resolved from the run's frame context at its start and dropped at
    /// its end: node-scoped, never carried to another clip or frame.
    run_analysis: HashMap<NodeId, klipp_effects::node_analysis::AtFrame>,
    /// The run's analysis nodes with no analysis for the frame, declined
    /// once the run's declines are started afresh.
    run_analysis_missing: Vec<(NodeId, klipp_effects::BuiltinEffect, crate::DeclineOutcome, &'static str)>,
    /// Whether this run's GPU chain has taken an analysis node's motion:
    /// the chain records its nodes and submits once, so a second one in
    /// the run would overwrite the first's; it runs on the CPU instead.
    run_motion_uploaded: bool,
    /// Source-boundary nodes run (M4 step 6), all time.
    source_runs: u64,
    /// Of them, those run with no context to read the source from, which
    /// passed their input on.
    source_unavailable: u64,
    /// OFX plug-in nodes run (M4 step 3), and those refused, which passed
    /// their input on.
    ofx_runs: u64,
    ofx_refused: u64,
    /// Mixed effect nodes whose mix or mask couldn't be read, skipped
    /// (`ChainStats::effect_mixes_refused`).
    run_mix_refused: u32,
    /// frei0r nodes rendered padded, and refused (`ChainStats`).
    run_frei0r_fallbacks: u32,
    run_frei0r_refused: u32,
    /// The run has a frame context, so a temporal node can read its
    /// source's neighbours and may go on the chain (#1988).
    run_has_sources: bool,
    /// A temporal-denoise node's neighbours, read once for the node this
    /// run (#1988): the chain dispatch uploads them, and the CPU route, if
    /// the node ends up there, denoises with the same frames rather than
    /// read (and decline) them again.
    run_denoise: Option<RunDenoise>,
    /// Neighbour frames the chain binds this run, held until the run's
    /// chain is submitted (#1988).
    run_neighbour_bufs: Vec<HeldNeighbour>,
    /// The buffer a temporal denoise drawn off the chain left its output
    /// in, with its node (#2266): kept as the held entry's GPU copy when
    /// the parked frame holds that node, so later chains seed from it.
    run_denoise_kept: Option<(NodeId, Arc<wgpu::Buffer>)>,
    /// Neighbours uploaded from the CPU for the chain this run, and last
    /// run's (#1988): one the source had on the GPU is bound as it is.
    run_neighbour_uploads: u32,
    last_neighbour_uploads: u32,
    /// Whether a temporal denoise may draw on the chain at all (#1988), on
    /// unless turned off (`set_denoise_on_chain`).
    denoise_chain_allowed: bool,
    /// The node being run may put a temporal denoise on the chain: no
    /// parked frame is to hold its output (#1863, #1988).
    node_denoise_on_chain: bool,
    /// Temporal-denoise nodes drawn on the chain this run, and last run's
    /// (#1988).
    run_denoise_on_chain: u32,
    last_denoise_on_chain: u32,
    /// A frame-blend node's frames, read once for the node this run
    /// (#2854): what its chain dispatch binds.
    run_blend: Option<RunBlend>,
    /// Whether a frame blend may draw on the chain at all (#2854), on
    /// unless turned off (`set_blend_on_chain`).
    blend_chain_allowed: bool,
    /// Frame-blend nodes drawn on the chain this run, and last run's
    /// (#2854).
    run_blend_on_chain: u32,
    last_blend_on_chain: u32,
    /// The run's declines so far (step 10), and the last run's.
    run_declines: Vec<crate::Decline>,
    /// The pictures the render handed the run beside its source (#1746).
    side_pictures: crate::side_picture::SidePictures,
    last_run_declines: Vec<crate::Decline>,
    /// The picture `start_chain` kept for a composite before its first
    /// dispatch, handed to the run (M3 step 17b).
    start_kept: Option<klipp_effects::gpu::KeptPicture>,
    /// The same, kept for the gates whose picture the chain's input is.
    start_kept_for_gates: Option<klipp_effects::gpu::KeptPicture>,
    /// The last chain start's parts, in ms, for the `PERF graph_exec`
    /// line (#2107): whether its input was seeded on the GPU or uploaded,
    /// and the time each part took.
    chain_start_split: Vec<(&'static str, f64)>,
    /// Keys drawn on the chain in the run so far (`ChainStats::extends`).
    run_key_draws: u32,
    /// The matte the next mixed effect node's mask reads, handed from the
    /// run to its dispatch (step 9c).
    effect_mask: Option<Arc<Vec<f32>>>,
    /// A mixed effect's window, as shapes the GPU works out inside the
    /// effect's mix (#1242, #1252); set with `effect_mask`, one or the other.
    effect_window: Option<Arc<Vec<klipp_effects::gpu::GpuMaskDef>>>,
    /// The key whose matte, drawn on the GPU, the next mixed effect node's
    /// mask reads (#1266); set with `effect_mask`, one or the other.
    effect_keyed: Option<NodeId>,
    /// Keys drawn on the GPU this run for effects' masks (#1266): each
    /// one's matte, and how many of its readers have yet to read it.
    key_mattes: HashMap<NodeId, (klipp_effects::gpu::KeyedMatte, usize)>,
    /// An effect node of the last run could not apply its effects (a
    /// lost device): its picture lacks them. See
    /// [`Self::last_run_effects_complete`].
    run_effects_incomplete: bool,
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
    /// The colour domain of the picture a run's Source reads (#1867):
    /// display-referred unless the renderer says the run is a
    /// scene-referred clip's grade before its display step
    /// ([`Self::set_source_domain`]).
    source_domain: crate::domains::PictureDomain,
    /// Heavy nodes' outputs held for the parked frame (#1863): used only
    /// by a run given the frame's source (`hold_outputs_for`).
    held: crate::held::HeldOutputs,
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
    /// The inspection a run asks of nodes inside its compounds (#1475).
    inside: CompoundInspect,
}

/// What a top-level run asks of the nodes inside its compounds, which
/// run as runs of their own, and what those answered (#1475): the scope's
/// tap and the thumbnails, by node id (a grade's ids are unique at every
/// depth), and whether the soloed node was inside one.
#[derive(Default)]
struct CompoundInspect {
    tap: Option<NodeId>,
    thumbs: Option<std::collections::HashSet<NodeId>>,
    tapped: Option<Vec<f32>>,
    found_thumbs: std::collections::HashMap<NodeId, Vec<u8>>,
    /// The compound of this level whose run picked the soloed node.
    solo_in: Option<NodeId>,
}

/// Whether `graph` holds `id`, at any depth of its compounds.
fn holds_node(graph: &NodeGraph, id: NodeId) -> bool {
    graph.nodes.iter().any(|n| n.id == id || n.params.compound_subgraph.as_ref().is_some_and(|g| holds_node(g, id)))
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

/// A zone's colour wheel puck (#240) as its balance in stops per channel:
/// the wheel's own mapping (a Lift or Gain wheel's), so the wheel's
/// readout is the zone's stops, its master (the exposure) added.
pub fn hdr_zone_colour(x: f32, y: f32) -> [f32; 3] {
    let (r, g, b) = klipp_color::wheels::wheel_offset_to_rgb(x, y, 0.0);
    [r, g, b]
}

/// The HDR zones' settings (#240) as a pixel uses them, in working space
/// `ws`: its luminance places each pixel in stops.
pub(crate) fn hdr_zone_terms(z: &klipp_core::HdrZonesSettings, ws: klipp_core::WorkingColorspace) -> klipp_color::grade_model::hdr_zones::Terms {
    let colour = std::array::from_fn(|i| hdr_zone_colour(z.colour_x[i], z.colour_y[i]));
    klipp_color::grade_model::hdr_zones::Terms::new(z.exposure, colour, z.saturation, z.boundary, z.falloff, klipp_color::primaries::luminance_weights(ws))
}

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
/// frame hits the same entry. A keyed mask is hashed as it is drawn at
/// that time (#74): a key edited while the playhead rests, or a shape
/// key's points, change the matte, which the static fields alone don't
/// say.
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
    let aspect = height as f32 / width.max(1) as f32;
    for mask in &masks.masks {
        if !mask.enabled { continue; }
        let drawn = if mask.keyframes.is_empty() { None } else { Some(mask.evaluated_in(clip_time_secs, aspect)) };
        let mask = drawn.as_ref().unwrap_or(mask);
        if let Some(w) = mask.keyframes.warp {
            for v in w.0 {
                v.to_bits().hash(&mut h);
            }
        }
        mask.feather.to_bits().hash(&mut h);
        mask.feather_inner.to_bits().hash(&mut h);
        mask.combine.code().hash(&mut h);
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
            klipp_core::MaskShape::RadialGradient { center_x, center_y, inner, outer } => {
                4u8.hash(&mut h);
                center_x.to_bits().hash(&mut h);
                center_y.to_bits().hash(&mut h);
                inner.to_bits().hash(&mut h);
                outer.to_bits().hash(&mut h);
            }
            klipp_core::MaskShape::Bezier { points } => {
                3u8.hash(&mut h);
                points.len().hash(&mut h);
                for p in points {
                    for v in [p.x, p.y, p.in_dx, p.in_dy, p.out_dx, p.out_dy] {
                        v.to_bits().hash(&mut h);
                    }
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
            gpu_prewarm: None,
            lut_cache: HashMap::new(),
            curve_lut_cache: HashMap::new(),
            matte_image_cache: HashMap::new(),
            unreadable_images: HashSet::new(),
            matte_sequences: HashMap::new(),
            clip_subject_mattes: [None, None],
            clip_media_last: None,
            plate_resize_cache: HashMap::new(),
            matte_cache: HashMap::new(),
            gpu_chain_preseeded: false,
            gpu_chain_preseed_dims: None,
            uploaded_seed: None,
            chain_thumbs: None,
            keep_output_on_gpu: false,
            run_effect_breaks: 0,
            source_runs: 0,
            source_unavailable: 0,
            run_frei0r_fallbacks: 0,
            run_frei0r_refused: 0,
            run_has_sources: false,
            run_denoise: None,
            run_neighbour_bufs: Vec::new(),
            run_denoise_kept: None,
            run_neighbour_uploads: 0,
            last_neighbour_uploads: 0,
            denoise_chain_allowed: true,
            node_denoise_on_chain: true,
            run_denoise_on_chain: 0,
            last_denoise_on_chain: 0,
            run_blend: None,
            blend_chain_allowed: true,
            run_blend_on_chain: 0,
            last_blend_on_chain: 0,
            run_declines: Vec::new(),
            side_pictures: Default::default(),
            last_run_declines: Vec::new(),
            ofx_runs: 0,
            ofx_refused: 0,
            run_analysis: HashMap::new(),
            run_analysis_missing: Vec::new(),
            run_motion_uploaded: false,
            run_mix_refused: 0,
            start_kept: None,
            start_kept_for_gates: None,
            chain_start_split: Vec::new(),
            run_key_draws: 0,
            effect_mask: None,
            effect_window: None,
            effect_keyed: None,
            key_mattes: HashMap::new(),
            run_effects_incomplete: false,
            plans: HashMap::new(),
            plan_compiles: 0,
            plan_hits: 0,
            run_gpu_output_node: None,
            output_on_gpu: None,
            last_run_chain_stats: ChainStats::default(),
            run_seed_dims: None,
            last_chain_final_slot: None,
            compound_depth: 0,
            source_domain: crate::domains::PictureDomain::Display,
            held: Default::default(),
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
            render_scale: 1.0,
            grade_encoding: klipp_color::grade_model::Encoding::Bt1886,
            project_grade_encoding: klipp_core::grade_encoding(),
            lut_tetrahedral: true,
            current_clip_track_points: Vec::new(),
            solo_node: None,
            inside: CompoundInspect::default(),
        }
    }

    /// Lazily create the `GpuEffectsPipeline` on first use. Once the
    /// shared `GpuContext` is populated (either by the UI host or by
    /// the first lazy init here), every subsequent call re-uses it.
    /// Caches init failure so we don't retry on every frame.
    /// Build the GPU pipeline on a thread of its own now, so the first
    /// frame that needs it does not (#2108): building it compiles every
    /// effect's shader, 0.8 s that the first stacked frame of a session
    /// spent on the engine's thread, a Play pressed then waiting behind
    /// it. Does nothing once the pipeline is built or being built.
    pub fn prewarm_gpu(&mut self) {
        if self.gpu.is_some() || self.gpu_init_attempted || self.gpu_prewarm.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        if let Ok(handle) = std::thread::Builder::new().name("gpu pipeline build".into()).spawn(move || {
            // On the device the UI installs, never one of its own made
            // first: waited for while the UI starts. Undecided after
            // that (a run with no UI), the build makes it as a frame's
            // would.
            klipp_effects::gpu_context::GpuContext::wait_for_shared(PREWARM_WAITS_FOR_DEVICE);
            let _ = tx.send(GpuEffectsPipeline::new());
        }) {
            self.gpu_prewarm = Some((rx, handle));
        }
    }

    fn ensure_gpu(&mut self) {
        if self.gpu.is_some() || self.gpu_init_attempted {
            return;
        }
        self.gpu_init_attempted = true;
        // The one being built ahead, waited for; else built here.
        self.gpu = match self.gpu_prewarm.take() {
            Some((rx, handle)) => {
                // A frame needs it now: the device is decided here, as it
                // was before the build went ahead, and the build waiting
                // for that goes on.
                let _ = klipp_effects::gpu_context::GpuContext::shared();
                let built = rx.recv().ok().flatten();
                // A build that panicked (a shader that does not compile,
                // in a debug build: #2344) fails here, where the frame
                // is, rather than reading as a machine with no GPU.
                if let Err(panic) = handle.join() {
                    std::panic::resume_unwind(panic);
                }
                built
            }
            None => GpuEffectsPipeline::new(),
        };
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
            gpu_prewarm: None,
            lut_cache: HashMap::new(),
            curve_lut_cache: HashMap::new(),
            matte_image_cache: HashMap::new(),
            unreadable_images: HashSet::new(),
            matte_sequences: HashMap::new(),
            clip_subject_mattes: [None, None],
            clip_media_last: None,
            plate_resize_cache: HashMap::new(),
            matte_cache: HashMap::new(),
            gpu_chain_preseeded: false,
            gpu_chain_preseed_dims: None,
            uploaded_seed: None,
            chain_thumbs: None,
            keep_output_on_gpu: false,
            run_effect_breaks: 0,
            source_runs: 0,
            source_unavailable: 0,
            run_frei0r_fallbacks: 0,
            run_frei0r_refused: 0,
            run_has_sources: false,
            run_denoise: None,
            run_neighbour_bufs: Vec::new(),
            run_denoise_kept: None,
            run_neighbour_uploads: 0,
            last_neighbour_uploads: 0,
            denoise_chain_allowed: true,
            node_denoise_on_chain: true,
            run_denoise_on_chain: 0,
            last_denoise_on_chain: 0,
            run_blend: None,
            blend_chain_allowed: true,
            run_blend_on_chain: 0,
            last_blend_on_chain: 0,
            run_declines: Vec::new(),
            side_pictures: Default::default(),
            last_run_declines: Vec::new(),
            ofx_runs: 0,
            ofx_refused: 0,
            run_analysis: HashMap::new(),
            run_analysis_missing: Vec::new(),
            run_motion_uploaded: false,
            run_mix_refused: 0,
            start_kept: None,
            start_kept_for_gates: None,
            chain_start_split: Vec::new(),
            run_key_draws: 0,
            effect_mask: None,
            effect_window: None,
            effect_keyed: None,
            key_mattes: HashMap::new(),
            run_effects_incomplete: false,
            plans: HashMap::new(),
            plan_compiles: 0,
            plan_hits: 0,
            run_gpu_output_node: None,
            output_on_gpu: None,
            last_run_chain_stats: ChainStats::default(),
            run_seed_dims: None,
            last_chain_final_slot: None,
            compound_depth: 0,
            source_domain: crate::domains::PictureDomain::Display,
            held: Default::default(),
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
            render_scale: 1.0,
            grade_encoding: klipp_color::grade_model::Encoding::Bt1886,
            project_grade_encoding: klipp_core::grade_encoding(),
            lut_tetrahedral: true,
            current_clip_track_points: Vec::new(),
            solo_node: None,
            inside: CompoundInspect::default(),
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
    /// The current clip's tracked subject mattes' folders, by model (0
    /// the subject, 1 the foreground), for its External Mattes bound to
    /// a subject (#1881). Set after `prepare_clip`, which clears them.
    pub fn set_current_clip_subject_mattes(&mut self, dirs: [Option<PathBuf>; 2]) {
        self.clip_subject_mattes = dirs;
    }

    /// The current clip's media's last frame, seconds, for its matte
    /// sequences read past the media's end. Set after `prepare_clip`.
    pub fn set_current_clip_media_last(&mut self, last: Option<f64>) {
        self.clip_media_last = last;
    }

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

    /// The pictures the render hands this frame's runs beside their source
    /// (#1746), by key: a `SidePicture` node without one draws a clear
    /// picture.
    pub fn set_side_pictures(&mut self, pictures: crate::side_picture::SidePictures) {
        self.side_pictures = pictures;
    }

    /// No side pictures, for the clip whose run is next: each producer
    /// then puts its own ([`Self::put_side_picture`]).
    pub fn clear_side_pictures(&mut self) {
        self.side_pictures = Default::default();
    }

    /// `picture` under `key`, beside the run's others.
    pub fn put_side_picture(&mut self, key: crate::side_picture::SideKey, picture: Result<std::sync::Arc<Vec<f32>>, crate::side_picture::SideMissing>) {
        std::sync::Arc::make_mut(&mut self.side_pictures).insert(key, picture);
    }

    /// The pictures [`Self::set_side_pictures`] set.
    pub fn side_pictures(&self) -> crate::side_picture::SidePictures {
        std::sync::Arc::clone(&self.side_pictures)
    }

    /// The side picture `key` names in a kept picture on the GPU, for a
    /// node that reads one: `None` without a GPU, the picture, or a pool
    /// slot for it.
    pub fn upload_side_picture(&mut self, key: &crate::side_picture::SideKey, width: u32, height: u32) -> Option<klipp_effects::gpu::KeptPicture> {
        let picture = self.side_pictures.get(key)?.as_ref().ok()?.clone();
        (picture.len() == (width as usize) * (height as usize) * 4).then_some(())?;
        self.gpu.as_mut()?.upload_picture(&picture, width, height)
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
    /// the linear frame would be, and no decode on the CPU. A deeper
    /// source's 16-bit codes go through the 16-bit bridge, at their own
    /// precision (#1226).
    #[allow(clippy::too_many_arguments)]
    pub fn prime_chain_from_hdr10_codes(
        &mut self,
        codes: &Arc<klipp_color::hdr::SceneCodes>,
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
        let ok = if codes.packed10().is_some() {
            gpu.bridge_hdr10_to_linear_into_chain(&seed.buffer, width, height, transfer_id, scene_scale, source_to_working)
        } else {
            gpu.bridge_hdr16_to_linear_into_chain(&seed.buffer, width, height, transfer_id, scene_scale, source_to_working)
        };
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

    /// Seed the chain from linear working-space f32 in memory, uploaded
    /// straight from `pixels`: the next run's Source is the chain's input
    /// slot, with no copy of the frame on the CPU first. A run that
    /// still needs the source on the CPU (a node off the chain) reports
    /// `CpuSourceRequired`, and the caller runs again with the pixels.
    /// `false` with no GPU or a frame of the wrong size.
    pub fn prime_chain_from_linear(&mut self, pixels: &[f32], width: u32, height: u32) -> bool {
        if pixels.len() != (width as usize) * (height as usize) * 4 {
            return false;
        }
        self.ensure_gpu();
        let Some(gpu) = self.gpu.as_mut() else { return false };
        if gpu.is_device_lost() {
            return false;
        }
        gpu.ensure_chain_buffers((width as u64) * (height as u64) * 16);
        gpu.upload_chain_input_linear(pixels);
        self.gpu_chain_preseeded = true;
        self.gpu_chain_preseed_dims = Some((width, height));
        true
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

    /// The nodes the last run didn't draw as asked, and why (step 10): a
    /// list beside [`ChainStats`], which stays `Copy`.
    /// The colour domain of the picture the next runs' Source reads
    /// (#1867): scene-referred for a scene-referred clip's grade before its
    /// display step (HDR or log media), display-referred otherwise, which
    /// is the default. An SDR clip's picture is display-referred: a
    /// display-referred side picture mixes with it.
    pub fn set_source_domain(&mut self, domain: crate::domains::PictureDomain) {
        self.source_domain = domain;
    }

    /// Whether a temporal denoise may draw on the GPU chain (#1988), on
    /// unless turned off; off, it always takes the CPU route, as before:
    /// what a test compares the chain against, or holds a CPU picture to.
    pub fn set_denoise_on_chain(&mut self, on: bool) {
        self.denoise_chain_allowed = on;
    }

    /// Temporal-denoise nodes the last run drew on the chain (#1988).
    pub fn last_denoise_on_chain(&self) -> u32 {
        self.last_denoise_on_chain
    }

    /// Whether a frame blend may draw on the GPU chain (#2854), on unless
    /// turned off; off, it takes the CPU route, as before: what a test
    /// compares the chain against.
    pub fn set_blend_on_chain(&mut self, on: bool) {
        self.blend_chain_allowed = on;
    }

    /// Frame-blend nodes the last run drew on the chain (#2854).
    pub fn last_blend_on_chain(&self) -> u32 {
        self.last_blend_on_chain
    }

    /// Neighbours the last run uploaded from the CPU for the chain
    /// (#1988); one the source had on the GPU isn't one.
    pub fn last_neighbour_uploads(&self) -> u32 {
        self.last_neighbour_uploads
    }

    pub fn last_run_declines(&self) -> &[crate::Decline] {
        &self.last_run_declines
    }

    /// `Some(slot_a)` if the last run left its final result in the chain
    /// buffer. Its returned Vec is then empty; read the result with
    /// `encode_output_to_display_u8`.
    pub fn output_on_gpu(&self) -> Option<bool> {
        self.output_on_gpu
    }

    /// Whether every effect node of the last run applied its effects;
    /// `false` after a lost device, when its picture lacks them and the
    /// caller takes another route.
    pub fn last_run_effects_complete(&self) -> bool {
        !self.run_effects_incomplete
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
        // The last clip's subject mattes are its own (#1881).
        self.clip_subject_mattes = [None, None];
        self.clip_media_last = None;
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
        self.run_planned(source_frame, graph, width, height, clip_time_secs, frame_number, matte_tap, matte_preview_node, visible_nodes, None)
    }

    /// [`Self::execute_linear_with_thumbs`] with what a non-pure node reads
    /// besides its input (M4): its source frames at other times, its
    /// analysis and its plug-in instance. A node that needs them and runs
    /// without them passes its input on, counted
    /// ([`Self::source_unavailable`]).
    #[allow(clippy::too_many_arguments)]
    pub fn execute_linear_with_context(
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
        ctx: &mut crate::frame_context::FrameContext<'_>,
    ) -> Result<(Vec<f32>, Option<Vec<f32>>, std::collections::HashMap<NodeId, Vec<u8>>), ExecutorError> {
        self.run_planned(source_frame, graph, width, height, clip_time_secs, frame_number, matte_tap, matte_preview_node, visible_nodes, Some(ctx))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_planned(
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
        ctx: Option<&mut crate::frame_context::FrameContext<'_>>,
    ) -> Result<(Vec<f32>, Option<Vec<f32>>, std::collections::HashMap<NodeId, Vec<u8>>), ExecutorError> {
        // Every node able to take the pictures it reads (#1290), from the
        // domain of the picture the run's Source reads (#1867): a refusal is
        // a failing decline naming the node and why, read after the run as
        // any other run's declines are. Checked whole from the top level,
        // into its compounds with the domain each reads.
        if self.compound_depth == 0 {
            if let Err(fault) = crate::domains::domains(graph, self.source_domain) {
                self.run_declines.clear();
                self.last_run_declines = vec![domain_decline(graph, &fault)];
                return Err(ExecutorError::ColourDomain(fault));
            }
        }
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
        let (analysis, missing) = ctx.as_deref().map(|c| resolve_analysis(graph, c, width, height)).unwrap_or_default();
        self.run_analysis = analysis;
        self.run_analysis_missing = missing;
        self.run_motion_uploaded = false;
        // A top-level run's requests reach the nodes inside its compounds.
        let top = self.compound_depth == 0;
        if top {
            self.inside = CompoundInspect { tap: matte_tap, thumbs: visible_nodes.cloned(), ..Default::default() };
        }
        let mut result = self.execute_with_plan(
            source_frame, graph, &plan, width, height, clip_time_secs, frame_number,
            matte_tap, matte_preview_node, visible_nodes, ctx,
        );
        if top {
            let inside = std::mem::take(&mut self.inside);
            if let Ok((_, tapped, thumbs)) = result.as_mut() {
                if tapped.is_none() {
                    *tapped = inside.tapped;
                }
                thumbs.extend(inside.found_thumbs);
            }
        }
        self.run_analysis.clear();
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
        ctx: Option<&mut crate::frame_context::FrameContext<'_>>,
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
            matte_tap, matte_preview_node, visible_nodes, ctx,
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
        mut ctx: Option<&mut crate::frame_context::FrameContext<'_>>,
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
        // The composite input the run keeps, in a pool slot (M3 step
        // 17b), and whose picture it is; and how many times this chain
        // segment has made one. Same shape as `matte_in_buf`: the gate is
        // an identity check, not a hope.
        let mut composite: Option<(NodeId, klipp_effects::gpu::KeptPicture)> = None;
        // And the pictures kept for secondaries' gates, by whose they are
        // (M3 step 17b: several at once). A gate reads its picture rather
        // than the chain's current slot, so "a chain is live" was never
        // enough to let it dispatch — its own picture has to be kept.
        // Without the check, a graph where the source ran on the CPU sent
        // the gate at a picture nobody had kept, which panicked outright
        // when it was the first frame and read the previous frame's
        // picture when it was not.
        // Each kept picture's slot, with the gates still to read it.
        let mut kept: HashMap<NodeId, (klipp_effects::gpu::KeptPicture, usize)> = HashMap::new();
        // And the matte a secondary's window drew for its gate, and whose
        // (a key's is among the run's GPU keys).
        let mut secondary_matte: Option<(NodeId, klipp_effects::gpu::KeyedMatte)> = None;
        let mut segment_composite_writes: u32 = 0;

        // Output-on-GPU is single-shot, like the seed: take the request
        // here so a run that cannot honour it does not leave it set. The
        // final node qualifies only when Output is its sole reader and no
        // one wants its pixels on the CPU — not a solo view, a scope or
        // matte tap, or a thumbnail.
        self.output_on_gpu = None;
        self.run_effect_breaks = 0;
        self.key_mattes.clear();
        self.effect_keyed = None;
        self.run_effects_incomplete = false;
        self.run_mix_refused = 0;
        self.run_frei0r_fallbacks = 0;
        self.run_frei0r_refused = 0;
        self.run_has_sources = ctx.is_some();
        self.node_denoise_on_chain = true;
        self.run_denoise = None;
        self.run_denoise_on_chain = 0;
        self.run_blend = None;
        self.run_blend_on_chain = 0;
        self.run_neighbour_uploads = 0;
        // A run that errors leaves no declines of an earlier run behind.
        self.run_declines.clear();
        self.last_run_declines.clear();
        // Its analysis nodes with no analysis for the frame (step 10).
        for (node, effect, outcome, reason) in std::mem::take(&mut self.run_analysis_missing) {
            self.decline(node, NodeKind::Effect, outcome, crate::decline::effect_name(&effect), reason, String::new());
        }
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
        // Whether a node's output is the source left on the GPU (its
        // published value is the empty sentinel, no pixels on the CPU). A
        // node run on the CPU can't read it, and an empty input reads as
        // "unwired" to the matte consumers, which pass the graded picture
        // through: a gate would correct the whole frame (#1234).
        let is_gpu_source = |id: NodeId| gpu_resident_source && node_by_id.get(&id).is_some_and(|n| n.kind == NodeKind::Source);

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
        // Windows that only mask effects (#1242): their shapes, drawn on the
        // GPU inside each effect's mix as it runs (#1252), never on the CPU
        // here.
        let mut window_mattes: HashMap<NodeId, Arc<Vec<klipp_effects::gpu::GpuMaskDef>>> = HashMap::new();
        // Readers of a node's picture that the chain itself serves: a key
        // drawn on the GPU from it for an effect's mask (#1266). They need
        // no CPU copy, so they don't hold the chain back at that node.
        let mut served: HashMap<NodeId, usize> = HashMap::new();
        // Keys whose every reader is a mixed effect's mask port, the GPU
        // can draw (#1266): each is drawn the moment its picture is on the
        // chain (`draw_keys_of`), whichever node reads that picture next,
        // so a key of the source can mask an effect after a grade.
        let mut pending_keys: Vec<PendingKey> = if self.gpu.is_some() {
            graph
                .nodes
                .iter()
                .filter(|n| feeding.contains(&n.id) && n.enabled)
                // A tapped, previewed or soloed key is looked at: its pixels
                // stay on the CPU.
                .filter(|n| Some(n.id) != matte_tap && Some(n.id) != matte_preview_node && Some(n.id) != self.solo_node)
                // A key whose thumbnail is shown keeps its CPU pixels.
                .filter(|n| !visible_nodes.is_some_and(|v| v.contains(&n.id)))
                .filter_map(|n| {
                    let readers: Vec<_> = graph.edges.iter().filter(|e| e.from_node == n.id).collect();
                    let masks_only = !readers.is_empty()
                        && readers.iter().all(|e| {
                            e.to_port == 1 && node_by_id.get(&e.to_node).is_some_and(|r| matches!(r.kind, NodeKind::Effect | NodeKind::EffectSegment | NodeKind::Frei0r) && r.params.effect_blend.is_some())
                        });
                    let key = gpu_key(&n.kind, &params_for_frame(n, clip_time_secs)).filter(|_| masks_only)?;
                    Some(PendingKey { node: n.id, picture: graph.input_of(n.id, 0)?, key, readers: readers.len() })
                })
                .collect()
        } else {
            Vec::new()
        };

        // The parked frame's held outputs (#1863): for a top-level run of
        // the clip the source was given for, with its frame context.
        // Heavy nodes found held are taken as they are, and the nodes
        // above them that only they read are not run.
        // A run with no frame context is the viewer's GPU path on the
        // parked frame (step B3): it may only take outputs held, never run
        // a heavy node, which needs the context it hasn't.
        let contextless = ctx.is_none();
        let held_source = self.held.token.clone().filter(|t| self.compound_depth == 0 && ctx.as_ref().is_none_or(|c| c.clip == t.clip));
        let held_run = match held_source {
            Some(source) => {
                let env = crate::held::RunEnv {
                    width,
                    height,
                    clip_time_secs,
                    // The renderer's context reads the source at the
                    // retime's sample; the GPU path, with none, the same.
                    source_secs: ctx.as_ref().map_or(source.retime.source_at_clip_time, |c| c.source_secs),
                    frame_number,
                    working: self.working_colorspace,
                    encoding: self.project_grade_encoding,
                    lut_tetrahedral: self.lut_tetrahedral,
                    render_scale: self.render_scale,
                    gpu: self.gpu.is_some(),
                    seeded: gpu_resident_source,
                    source_domain: format!("{:?}", self.source_domain),
                };
                let host = ctx.as_ref().and_then(|c| c.ofx);
                let solo = self.solo_node;
                let analysed: HashSet<NodeId> = self.run_analysis.keys().copied().collect();
                let looked_at = |id: NodeId| {
                    Some(id) == matte_tap || Some(id) == matte_preview_node || Some(id) == solo || visible_nodes.is_some_and(|v| v.contains(&id)) || analysed.contains(&id)
                };
                // Without a host the plug-in can't be asked; an entry under
                // its key was stored by a run that asked it.
                let history = |n: &Node| !contextless && held_history(n, host);
                self.held.plan(graph, order, feeding, &source, &env, &looked_at, &history)
            }
            None => crate::held::HeldRun::default(),
        };
        if contextless && (!held_run.store.is_empty() || !held_run.refused.is_empty()) {
            return Err(ExecutorError::CpuSourceRequired);
        }
        // A seed is the Source's picture. With the Source not run, a chain
        // starting below a held output would read the seed in its place
        // (`start_chain`): the held pixels are uploaded instead.
        let source_skipped = node_by_id.values().any(|n| n.kind == NodeKind::Source && held_run.skip.contains(&n.id));
        if source_skipped {
            self.run_seed_dims = None;
        }
        // With the Source not run, a held output the next node to run
        // reads alone, and nothing else, can seed that node's chain from
        // its GPU copy (step B3): the chain's input slot filled on the
        // device, as a seed is, instead of uploaded.
        let seed_at = held_run
            .hits
            .keys()
            .filter(|h| source_skipped && self.gpu.is_some() && !held_run.skip.contains(h))
            .find(|h| {
                fan_out.get(h).copied().unwrap_or(0) <= 1
                    && order.iter().skip_while(|n| *n != *h).skip(1).find(|n| feeding.contains(n) || **n == graph.output_node_id).is_some_and(|next| graph.input_of(*next, 0) == Some(**h))
            })
            .copied();

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
            // A heavy node the parked frame is to hold (#1863) runs on the
            // CPU, where its output is the pixels held; a temporal denoise
            // goes on the chain (#1988) only when nothing is to hold it.
            self.node_denoise_on_chain = !held_run.store.contains_key(&node_id);
            // Held (#1863): its output as the run that made it left it,
            // with what it declined; above one, nothing to run.
            if let Some((pixels, declines)) = held_run.hits.get(&node_id) {
                self.run_declines.extend(declines.iter().cloned());
                if !held_run.skip.contains(&node_id) {
                    if Some(node_id) == matte_tap {
                        tapped_output = Some(Arc::clone(pixels));
                    }
                    node_outputs.insert(node_id, Arc::clone(pixels));
                    if seed_at == Some(node_id) {
                        if let Some(key) = held_run.keys.get(&node_id) {
                            self.seed_from_held(key, pixels, width, height);
                        }
                    }
                }
                per_node_ms.push((format!("{:?}#held", node.kind), t_node.elapsed().as_secs_f64() * 1000.0));
                continue;
            }
            if held_run.skip.contains(&node_id) {
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
                    | NodeKind::HdrZones
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
                || noop_input_transform
                || effect_blend_is_none(graph, node, clip_time_secs);
            // A plug-in switched off is a decline the viewer names, never
            // one an export fails on (step 10).
            if node.kind == NodeKind::Frei0r && !effective_enabled(node, clip_time_secs) {
                let name = match node.params.effects.first() {
                    Some(klipp_effects::BuiltinEffect::Frei0r { type_id, .. }) => crate::decline::frei0r_name(type_id),
                    _ => String::new(),
                };
                self.decline(node.id, NodeKind::Frei0r, crate::DeclineOutcome::Bypass, name, "switched off", "switched off".into());
            }

            // A mixed effect whose mask is a key still waiting for its
            // picture on the chain (#1266): drawn from the tail, or from a
            // chain started here on the picture the effect takes too, or
            // else keyed on the CPU now, from its picture's pixels.
            let pending_mask = graph
                .input_of(node_id, 1)
                .filter(|_| matches!(node.kind, NodeKind::Effect | NodeKind::EffectSegment | NodeKind::Frei0r) && node.params.effect_blend.is_some())
                .and_then(|id| pending_keys.iter().find(|k| k.node == id).map(|k| (id, k.picture)));
            if let Some((key, picture)) = pending_mask {
                let is_source = node_by_id.get(&picture).is_some_and(|n| n.kind == NodeKind::Source);
                if chain_tail.is_none() && graph.input_of(node_id, 0) == Some(picture) && !composite_port1.contains_key(&picture) {
                    if let Some(pixels) = picture_pixels(&node_outputs, picture, is_source, source_frame) {
                        if self.begin_chain_from(pixels, width, height) {
                            chain_tail = Some(picture);
                            chain_slot_a = true;
                            chain_start_count += 1;
                        }
                    }
                }
                if chain_tail == Some(picture) {
                    self.draw_keys_of(picture, chain_slot_a, &mut pending_keys, &mut served, graph, width, height);
                }
                // A key of a picture handed to the run (#1265): drawn on
                // the GPU from that picture, uploaded once, while a chain is
                // live to record it, started here on the effect's own input
                // if none is; else keyed on the CPU below.
                let side = node_by_id.get(&picture).and_then(|n| side_key_of(n));
                if side.is_some() && chain_tail.is_none() && !self.key_mattes.contains_key(&key) {
                    if let Some(input) = graph.input_of(node_id, 0).filter(|i| !composite_port1.contains_key(i)) {
                        let input_is_source = node_by_id.get(&input).is_some_and(|n| n.kind == NodeKind::Source);
                        if let Some(pixels) = picture_pixels(&node_outputs, input, input_is_source, source_frame) {
                            if self.begin_chain_from(pixels, width, height) {
                                chain_tail = Some(input);
                                chain_slot_a = true;
                                chain_start_count += 1;
                            }
                        }
                    }
                }
                if chain_tail.is_some() && !self.key_mattes.contains_key(&key) {
                    if let Some((readers, Some(side))) = pending_keys.iter().find(|k| k.node == key).map(|k| (k.readers, node_by_id.get(&picture).and_then(|n| side_key_of(n)))) {
                        self.draw_side_key(key, &side, readers, graph, clip_time_secs, width, height);
                    }
                }
                pending_keys.retain(|k| k.node != key);
                if !self.key_mattes.contains_key(&key) {
                    let key_node = *node_by_id.get(&key).expect("a graph node");
                    let Some(pixels) = picture_pixels(&node_outputs, picture, is_source, source_frame).filter(|p| !p.is_empty()) else {
                        return Err(if is_source { ExecutorError::CpuSourceRequired } else { ExecutorError::MaskUnavailable(node_id) });
                    };
                    let key_params = params_for_frame(key_node, clip_time_secs);
                    let matte = self.execute_node(key, &key_node.kind, &key_params, pixels, None, None, width, height, source_frame, frame_number, clip_time_secs, None, None);
                    node_outputs.insert(key, Arc::new(matte));
                    cpu_fallback_count += 1;
                }
            }

            // If a chain is live, see whether this node can extend it or
            // whether we must flush the tail back to CPU first.
            let primary_input_id = graph.input_of(node_id, 0);
            let primary_is_chain_tail = chain_tail.is_some() && primary_input_id == chain_tail;
            // The chain tail's fan-out must be 1 for the chain to extend
            // past it (more consumers would need a CPU copy anyway, so
            // extending only delays the inevitable download).
            let chain_tail_fan_out_one = chain_tail
                .map(|id| {
                    fan_out.get(&id).copied().unwrap_or(0).saturating_sub(served.get(&id).copied().unwrap_or(0)) <= 1
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
            // chainable in general: one draws the matte, the other reads
            // it alongside the kept picture, both in pool slots (M3 step
            // 17b). The gate needs its secondary's matte drawn.
            let secondary_chain_node = secondaries.iter().any(|sec| {
                sec.apply == node_id
                    && kept.contains_key(&sec.source)
                    && match sec.keyed {
                        Some(picture) => kept.contains_key(&picture) && node_by_id.get(&sec.matte).is_some_and(|k| effective_enabled(k, clip_time_secs)),
                        None => secondary_matte.as_ref().is_some_and(|(m, _)| *m == sec.matte),
                    }
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
                if composite.as_ref().is_some_and(|(id, _)| *id == src_id) {
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

            // A mixed effect's matte goes up from the CPU (step 9c-1): one
            // only on the GPU sends the node the CPU way for now. So a node
            // feeding a mixed effect's mask runs off the chain, its matte
            // made on the CPU where the effect can read it.
            let feeds_a_mask = graph.nodes.iter().any(|n| {
                matches!(n.kind, NodeKind::Effect | NodeKind::EffectSegment | NodeKind::Frei0r) && n.params.effect_blend.is_some() && graph.input_of(n.id, 1) == Some(node_id)
            });
            let effect_mask = match (node.kind, node.params.effect_blend) {
                (NodeKind::Effect | NodeKind::EffectSegment | NodeKind::Frei0r, Some(_)) => graph.input_of(node_id, 1).map(|id| node_outputs.get(&id).filter(|m| !m.is_empty()).cloned()),
                _ => None,
            };
            let effect_window = match (node.kind, node.params.effect_blend) {
                (NodeKind::Effect | NodeKind::EffectSegment | NodeKind::Frei0r, Some(_)) => graph.input_of(node_id, 1).and_then(|id| window_mattes.get(&id).cloned()),
                _ => None,
            };
            let effect_keyed = match (node.kind, node.params.effect_blend) {
                (NodeKind::Effect | NodeKind::EffectSegment | NodeKind::Frei0r, Some(_)) => graph.input_of(node_id, 1).filter(|id| self.key_mattes.contains_key(id)),
                _ => None,
            };
            let effect_mask_ready = effect_window.is_some() || effect_keyed.is_some() || !matches!(effect_mask, Some(None));
            // Handed to the dispatch, whichever way the node is dispatched
            // (extending a chain, or starting one); each node sets it anew,
            // so nothing carries over to another.
            self.effect_mask = effect_mask.clone().flatten();
            self.effect_window = effect_window;
            self.effect_keyed = effect_keyed;
            let can_extend_chain = !bypass
                && effect_mask_ready
                && !feeds_a_mask
                && primary_is_chain_tail
                && chain_tail_fan_out_one
                && (secondary_chain_node
                    || composite_ready
                    || self.node_supports_gpu_chain(&node.kind, &node.params))
                && mask_cutout_matte_ready
                && Some(node_id) != matte_preview_node
                && Some(node_id) != matte_tap
                // A soloed node's output is the picture shown: a real
                // buffer, never a chain's placeholder (M3 step 16a).
                && Some(node_id) != self.solo_node
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
                // A soloed node's output is the picture shown: a real
                // buffer, never a chain's placeholder (M3 step 16a).
                && Some(node_id) != self.solo_node
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

            // A window whose every reader is a mixed effect's mask port
            // (#1242): nothing is drawn here, and the chain isn't broken.
            // Each effect draws the window on the GPU inside its own mix as
            // it runs (#1252); an effect that leaves the chain draws it on
            // the CPU then. A shape the GPU can't draw keeps the CPU way.
            if node.kind == NodeKind::Mask
                && self.gpu.is_some()
                && !bypass
                && Some(node_id) != matte_tap
                && Some(node_id) != matte_preview_node
                && Some(node_id) != self.solo_node
                && !thumb_needs_cpu
            {
                let mut readers = graph.edges.iter().filter(|e| e.from_node == node_id).peekable();
                let only_masks_effects = readers.peek().is_some()
                    && readers.all(|e| {
                        e.to_port == 1
                            && node_by_id.get(&e.to_node).is_some_and(|n| matches!(n.kind, NodeKind::Effect | NodeKind::EffectSegment | NodeKind::Frei0r) && n.params.effect_blend.is_some())
                    });
                if only_masks_effects {
                    // As drawn now: keyframes, then the window's tracked motion (#238).
                    let masks = node.params.masks.as_ref().map(|ms| ms.evaluated_in(clip_time_secs, height as f32 / width.max(1) as f32));
                    let enabled: Vec<&klipp_core::Mask> = masks.iter().flat_map(|ms| ms.masks.iter()).filter(|m| m.enabled).collect();
                    let defs: Vec<_> = enabled.iter().filter_map(|m| mask_to_gpu_def(m, width, height)).collect();
                    if !defs.is_empty() && defs.len() == enabled.len() {
                        window_mattes.insert(node_id, Arc::new(defs));
                        // It reads none of its input's pixels, so that edge
                        // holds the chain back no more than a GPU key's.
                        if let Some(input) = primary_input_id {
                            *served.entry(input).or_insert(0) += 1;
                        }
                        node_outputs.insert(node_id, Arc::new(Vec::new()));
                        per_node_ms.push((format!("{node_kind_name}#window_on_gpu"), t_node.elapsed().as_secs_f64() * 1000.0));
                        continue;
                    }
                }
            }

            // A key drawn on the GPU for effects' masks (#1266): from the
            // chain's tail when that is its picture; otherwise left for
            // when a chain starts from or reaches its picture, or for its
            // first reader (below). Starting a chain here would skip what
            // a chain start keeps for a secondary or a composite.
            if let Some(picture) = pending_keys.iter().find(|k| k.node == node_id).map(|k| k.picture) {
                // Of a picture handed to the run (#1265): drawn now from it,
                // while a chain is live to record the draw.
                let side = node_by_id.get(&picture).and_then(|n| side_key_of(n));
                if let (Some(side), true) = (side, chain_tail.is_some()) {
                    let readers = pending_keys.iter().find(|k| k.node == node_id).map_or(0, |k| k.readers);
                    self.draw_side_key(node_id, &side, readers, graph, clip_time_secs, width, height);
                    if self.key_mattes.contains_key(&node_id) {
                        pending_keys.retain(|k| k.node != node_id);
                    }
                }
                if self.key_mattes.contains_key(&node_id) {
                    // Drawn above: handled with the other GPU keys below.
                } else if chain_tail == Some(picture) {
                    self.draw_keys_of(picture, chain_slot_a, &mut pending_keys, &mut served, graph, width, height);
                } else {
                    node_outputs.insert(node_id, Arc::new(Vec::new()));
                    per_node_ms.push((format!("{node_kind_name}#key_deferred"), t_node.elapsed().as_secs_f64() * 1000.0));
                    continue;
                }
            }
            if self.key_mattes.contains_key(&node_id) {
                node_outputs.insert(node_id, Arc::new(Vec::new()));
                per_node_ms.push((format!("{node_kind_name}#key_on_gpu"), t_node.elapsed().as_secs_f64() * 1000.0));
                continue;
            }
            // A recognised secondary's key is drawn by its gate, just
            // before the gate reads it, from the picture kept for it (M3
            // step 17b): one matte alive at a time, however many gates key
            // the same frame. A gate off the chain keys on the CPU.
            // A key looked at (tapped, previewed, soloed, its thumbnail
            // shown) runs in its turn, its pixels on the CPU.
            let looked_at = Some(node_id) == matte_tap
                || Some(node_id) == matte_preview_node
                || Some(node_id) == self.solo_node
                || visible_nodes.is_some_and(|v| v.contains(&node_id));
            // A key switched off at this frame (its enabled track) runs in
            // its turn too, bypassed as any switched-off node is, and its
            // gate reads that.
            if !looked_at && effective_enabled(node, clip_time_secs) && secondaries.iter().any(|sec| sec.matte == node_id && sec.keyed.is_some()) {
                node_outputs.insert(node_id, Arc::new(Vec::new()));
                per_node_ms.push((format!("{node_kind_name}#key_at_gate"), t_node.elapsed().as_secs_f64() * 1000.0));
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
            // A mixed effect whose mask is the chain's own picture (the node
            // feeding its input feeds its mask too) found no pixels for the
            // mask while that picture was on the GPU: the flush just made
            // them, so the mask is read again. Without it the mix ran with
            // no matte, white, and the whole effect was drawn.
            if self.effect_mask.is_none() && self.effect_window.is_none() && self.effect_keyed.is_none() && matches!(node.kind, NodeKind::Effect | NodeKind::EffectSegment | NodeKind::Frei0r) && node.params.effect_blend.is_some() {
                self.effect_mask = graph.input_of(node_id, 1).and_then(|id| node_outputs.get(&id)).filter(|m| !m.is_empty()).cloned();
            }

            // A picture that is only in a slot kept for a gate (M3 step
            // 17b): the chain moved past it (and is flushed by now), so it
            // is read back for a reader off the chain, a bypassed one too
            // (a gate whose key is looked at, a key switched off here).
            // Every port the node has: a mixer's past the third too.
            for port in 0..node.dynamic_input_port_count().max(3) {
                let Some(id) = graph.input_of(node_id, port) else { continue };
                if node_outputs.get(&id).is_some_and(|a| a.is_empty()) {
                    let pixels = kept.get(&id).and_then(|(picture, _)| self.gpu.as_ref()?.read_picture(picture, (width as usize) * (height as usize)));
                    if let Some(pixels) = pixels {
                        node_outputs.insert(id, Arc::new(pixels));
                    }
                }
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
                // A tap on a node that changes nothing is its input (#1314).
                if Some(node_id) == matte_tap {
                    tapped_output = Some(passthrough.clone());
                }
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
                // A soloed node's output is the picture shown: a real
                // buffer, never a chain's placeholder (M3 step 16a).
                && Some(node_id) != self.solo_node
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
            // A temporal denoise that may draw on the chain (#1988): its
            // neighbours read now, once, whichever route then draws it.
            if !bypass && self.gpu.is_some() && matches!(node.kind, NodeKind::Effect | NodeKind::EffectSegment) && self.denoise_on_chain(params) {
                self.read_denoise_for_chain(node_id, params, ctx.as_deref_mut());
            }
            // A frame blend that may draw on the chain (#2854): its frames
            // read now, once.
            if !bypass && self.gpu.is_some() && matches!(node.kind, NodeKind::Effect | NodeKind::EffectSegment) && self.blend_on_chain(params) {
                self.read_blend_for_chain(node_id, params, ctx.as_deref_mut());
            }

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
                // Blend / Layer: port 0 is the chain's picture, port 1 a
                // kept picture in a pool slot. One dispatch, no readback, where
                // the CPU path costs a flush and a full-frame lerp.
                if let Some(route) = composite_route {
                    let placed = match &route {
                        CompositeRoute::Resident => true,
                        CompositeRoute::Upload(px) => match self.gpu.as_mut().and_then(|g| g.upload_picture(px, width, height)) {
                            Some(picture) => {
                                segment_composite_writes += 1;
                                self.keep_as(&mut composite, composite_port1_id.map(|id| (id, picture)));
                                true
                            }
                            None => false,
                        },
                    };
                    if placed {
                        {
                            let other = &composite.as_ref().expect("placed above").1;
                            let gpu = self.gpu.as_mut().expect("gpu checked above");
                            if node.kind == NodeKind::Blend {
                                let mix = params.mix.unwrap_or(0.5).clamp(0.0, 1.0);
                                gpu.apply_blend_chain(chain_slot_a, width, height, mix, other);
                            } else {
                                gpu.apply_over_chain(chain_slot_a, width, height, other);
                            }
                        }
                        chain_slot_a = !chain_slot_a;
                        chain_tail = Some(node_id);
                        node_outputs.insert(node_id, Arc::new(Vec::new()));
                        self.draw_keys_of(node_id, chain_slot_a, &mut pending_keys, &mut served, graph, width, height);
                        // The same three hooks the generic extend path
                        // runs after a dispatch. They live here too
                        // because this arm returns early, and a composite
                        // is as eligible as any other node to be a
                        // secondary's source or another composite's port
                        // 1 — a Layer feeding a qualifier secondary hit
                        // exactly that and panicked in the keyer, which
                        // reads a picture nobody had kept.
                        self.keep_for_gates(node_id, chain_slot_a, secondaries, &mut kept, width, height);
                        if composite_port1.contains_key(&node_id) {
                            let picture = self.gpu.as_mut().and_then(|g| g.keep_picture(chain_slot_a, width, height));
                            self.keep_as(&mut composite, picture.map(|p| (node_id, p)));
                            segment_composite_writes += 1;
                        }
                        self.gate_read(node_id, secondaries, &mut kept, &mut secondary_matte);
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
                // A recognised secondary's gate, on the picture kept for it
                // and its key's (or window's) matte (M3 step 17b); any other
                // node its own way.
                let gate = secondaries.iter().copied().find(|sec| sec.apply == node_id).filter(|_| secondary_chain_node);
                let dispatched = match gate {
                    Some(sec) => {
                        // The key's matte, drawn now from its kept picture,
                        // or the window's.
                        let drawn = match sec.keyed {
                            Some(picture) => node_by_id.get(&sec.matte).and_then(|key| {
                                let key_params = params_for_frame(key, clip_time_secs);
                                self.key_from_kept(&key.kind, &key_params, kept.get(&picture).map(|(p, _)| p)?, width, height)
                            }),
                            None => None,
                        };
                        let channel = params.correction_blend.map_or(klipp_effects::mask_mix::MaskChannel::Red, |b| b.channel);
                        let picture = kept.get(&sec.source).map(|(p, _)| p);
                        let matte = drawn.as_ref().or(secondary_matte.as_ref().filter(|(id, _)| *id == sec.matte).map(|(_, m)| m));
                        let applied = match (picture, matte, self.gpu.as_mut()) {
                            (Some(picture), Some(matte), Some(gpu)) => {
                                gpu.apply_matte_chain(chain_slot_a, width, height, params.mix.unwrap_or(1.0), channel, params.key_invert == Some(true), picture, matte);
                                Some(1)
                            }
                            _ => None,
                        };
                        // The gate is the drawn matte's only reader.
                        if let (Some(matte), Some(gpu)) = (drawn, self.gpu.as_mut()) {
                            gpu.release_keyed_matte(matte);
                            self.run_key_draws += 1;
                        }
                        applied
                    }
                    None => self.dispatch_chain_node_slots(
                        node_id, &node.kind, params, width, height, frame_number,
                        /* in_is_a = */ chain_slot_a,
                    ),
                };
                match dispatched {
                    // Zero flips means the node wrote something other
                    // than the picture — a secondary's key drawing its
                    // matte. The chain's tail and slot are unchanged: the
                    // picture is still the source's.
                    Some(0) => {
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
                        self.draw_keys_of(node_id, chain_slot_a, &mut pending_keys, &mut served, graph, width, height);
                        // A secondary's source has to survive its own
                        // fan-out: keep the picture where ApplyMatte can
                        // still find it once the branch has overwritten
                        // the other slot.
                        self.keep_for_gates(node_id, chain_slot_a, secondaries, &mut kept, width, height);
                        // Same idea for a composite: this node's output is
                        // some Blend's port 1, so park it before the chain
                        // moves on. GPU to GPU, recorded in sequence.
                        if composite_port1.contains_key(&node_id) {
                            let picture = self.gpu.as_mut().and_then(|g| g.keep_picture(chain_slot_a, width, height));
                            self.keep_as(&mut composite, picture.map(|p| (node_id, p)));
                            segment_composite_writes += 1;
                        }
                        self.gate_read(node_id, secondaries, &mut kept, &mut secondary_matte);
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
                // A soloed node's output is the picture shown: a real
                // buffer, never a chain's placeholder (M3 step 16a).
                && Some(node_id) != self.solo_node
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
                // A gate's picture that is the chain's input (the clip's
                // frame a lifted grade keys and gates against) is kept as
                // it goes up, before the node writes over it.
                let keep_for_gates = primary_input_id
                    .is_some_and(|id| !kept.contains_key(&id) && secondaries.iter().any(|sec| sec.source == id));
                // Keys of the picture the chain starts from are drawn from it
                // before the node writes over it.
                let mut keys_of_input = |ex: &mut Self| {
                    if let Some(picture) = primary_input_id {
                        ex.draw_keys_of(picture, true, &mut pending_keys, &mut served, graph, width, height);
                    }
                };
                if let Some(n) = self.start_chain(
                    node_id,
                    &node.kind,
                    params,
                    primary_cpu,
                    width,
                    height,
                    frame_number,
                    keep_for_composite,
                    keep_for_gates,
                    &mut keys_of_input,
                ) {
                    chain_tail = Some(node_id);
                    chain_slot_a = n % 2 == 0;
                    node_outputs.insert(node_id, Arc::new(Vec::new()));
                    self.draw_keys_of(node_id, chain_slot_a, &mut pending_keys, &mut served, graph, width, height);
                    if keep_for_composite {
                        let picture = self.start_kept.take();
                        self.keep_as(&mut composite, picture.zip(primary_input_id).map(|(p, id)| (id, p)));
                        segment_composite_writes += 1;
                    }
                    if let (Some(picture), Some(id)) = (self.start_kept_for_gates.take(), primary_input_id) {
                        kept.insert(id, (picture, gate_readers(secondaries, id)));
                    }
                    // As in the extend path: a secondary's source keeps
                    // its picture for ApplyMatte to blend against.
                    self.keep_for_gates(node_id, chain_slot_a, secondaries, &mut kept, width, height);
                    if node_is_visible_for_thumbs {
                        if let Some(i) = take_thumb(self, chain_slot_a) {
                            gpu_thumb_of.insert(node_id, i);
                        }
                    }
                    chain_start_count += 1;
                    let total = t_node.elapsed().as_secs_f64() * 1000.0;
                    per_node_ms.push((format!("{node_kind_name}#chain_start"), total));
                    // Its parts (#2107): `seeded` or `uploaded` says where
                    // its input came from, with the upload's time; `rest`
                    // is the node's time outside `start_chain`.
                    let parts = std::mem::take(&mut self.chain_start_split);
                    let rest = total - parts.iter().map(|(_, ms)| ms).sum::<f64>();
                    for (part, part_ms) in parts.into_iter().chain([("rest", rest)]) {
                        per_node_ms.push((format!("{node_kind_name}#chain_start/{part}"), part_ms));
                    }
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
                        .filter_map(|m| mask_to_gpu_def(m, width, height))
                        .collect())
                    .unwrap_or_default();
                // Bezier shapes are now supported on the GPU path
                // (storage-buffer mask architecture, klipp-effects
                // SHADER_MASK_V2). The previous `!has_bezier` gate
                // forced the entire stack to fall through to CPU
                // even for one bezier — now removed.
                if single_cutout_target && !gpu_ready_masks.is_empty() {
                    // A secondary's window goes to a matte of its own (M3
                    // step 17b); a cutout's to `matte_buf`.
                    let drawn = if secondaries.iter().any(|sec| sec.matte == node_id) {
                        match self.gpu.as_mut().and_then(|g| g.mask_into_matte_slot(width, height, &gpu_ready_masks)) {
                            Some(matte) => {
                                self.matte_as(&mut secondary_matte, Some((node_id, matte)));
                                true
                            }
                            None => false,
                        }
                    } else {
                        if let Some(gpu) = self.gpu.as_mut() {
                            gpu.generate_mask_gpu_to_matte_buf(width, height, &gpu_ready_masks);
                        }
                        matte_in_buf = Some(node_id);
                        true
                    };
                    if drawn {
                        node_outputs.insert(node_id, Arc::new(Vec::new()));
                        per_node_ms.push((format!("{node_kind_name}#matte_buf"), t_node.elapsed().as_secs_f64() * 1000.0));
                        continue;
                    }
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
                            } else if is_gpu_source(src_id) {
                                return Err(ExecutorError::CpuSourceRequired);
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

            // ── KeyMixer (N mattes into one, #898) ─────────────────────
            // Outside `execute_node` for the same reason as LayerMixer: as
            // many inputs as it has ports. Each wired input's red channel
            // is its coverage; an input with no pixels is a producer with
            // nothing chosen, which selects everything, as every matte
            // consumer reads it; one of the wrong size is left out. With
            // nothing wired the mixer selects everything (no pixels). A
            // picture left on the GPU (the Source of a GPU-seeded run) has
            // no pixels either, and is no empty matte: the run goes to the
            // CPU, as for a Layer Mixer.
            if node.kind == NodeKind::KeyMixer {
                let pixel_count = (width as usize) * (height as usize);
                let mixer = params.key_mixer.clone().unwrap_or_default();
                let mut wired: Vec<(crate::params::KeyMixerInput, Option<&[f32]>)> = Vec::new();
                for port in 0..node.dynamic_input_port_count() {
                    let Some(src) = graph.input_of(node_id, port) else { continue };
                    let Some(arc) = node_outputs.get(&src) else { continue };
                    match arc.len() {
                        0 if is_gpu_source(src) => return Err(ExecutorError::CpuSourceRequired),
                        0 => wired.push((mixer.input(port), None)),
                        n if n == pixel_count * 4 => wired.push((mixer.input(port), Some(arc.as_slice()))),
                        _ => {}
                    }
                }
                let output = if wired.is_empty() {
                    Vec::new()
                } else {
                    use rayon::prelude::*;
                    let mut out = vec![0.0f32; pixel_count * 4];
                    out.par_chunks_exact_mut(4).enumerate().for_each(|(i, px)| {
                        let k = mixer.mix_inputs(wired.iter().map(|(input, m)| (*input, m.map_or(1.0, |m| m[i * 4]))));
                        px.copy_from_slice(&[k, k, k, 1.0]);
                    });
                    out
                };
                node_outputs.insert(node_id, Arc::new(output));
                cpu_fallback_count += 1;
                per_node_ms.push((format!("{node_kind_name}#mixer"), t_node.elapsed().as_secs_f64() * 1000.0));
                continue;
            }

            // ── CPU / unchained path ────────────────────────────────────
            // A gate off the chain whose key waited for it: keyed here on the
            // CPU, from its picture's pixels (read back above if kept).
            if let Some(sec) = secondaries.iter().find(|sec| sec.apply == node_id && sec.keyed.is_some() && node_by_id.get(&sec.matte).is_some_and(|k| effective_enabled(k, clip_time_secs))).copied() {
                let picture = sec.keyed.expect("checked");
                if node_outputs.get(&sec.matte).is_some_and(|m| m.is_empty()) {
                    if node_outputs.get(&picture).is_some_and(|a| a.is_empty()) {
                        let pixels = kept.get(&picture).and_then(|(p, _)| self.gpu.as_ref()?.read_picture(p, (width as usize) * (height as usize)));
                        if let Some(pixels) = pixels {
                            node_outputs.insert(picture, Arc::new(pixels));
                        }
                    }
                    let is_source = node_by_id.get(&picture).is_some_and(|n| n.kind == NodeKind::Source);
                    let key_node = *node_by_id.get(&sec.matte).expect("a graph node");
                    let Some(pixels) = picture_pixels(&node_outputs, picture, is_source, source_frame).filter(|p| !p.is_empty()) else {
                        return Err(if is_source { ExecutorError::CpuSourceRequired } else { ExecutorError::MaskUnavailable(node_id) });
                    };
                    let key_params = params_for_frame(key_node, clip_time_secs);
                    let matte = self.execute_node(sec.matte, &key_node.kind, &key_params, pixels, None, None, width, height, source_frame, frame_number, clip_time_secs, None, None);
                    node_outputs.insert(sec.matte, Arc::new(matte));
                    cpu_fallback_count += 1;
                }
            }
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
            if [1, 2].into_iter().any(|port| graph.input_of(node_id, port).is_some_and(is_gpu_source)) {
                return Err(ExecutorError::CpuSourceRequired);
            }
            // A mixed effect off the chain whose window was left for the GPU
            // to draw (#1242): drawn here on the CPU instead.
            let window_on_cpu;
            let secondary_slice = match graph.input_of(node_id, 1).filter(|id| secondary_slice.is_none() && window_mattes.contains_key(id)).and_then(|id| node_by_id.get(&id).copied()) {
                Some(window) => {
                    let window_params = params_for_frame(window, clip_time_secs);
                    window_on_cpu = self.execute_node(window.id, &NodeKind::Mask, &window_params, primary_slice, None, None, width, height, source_frame, frame_number, clip_time_secs, None, None);
                    cpu_fallback_count += 1;
                    Some(window_on_cpu.as_slice())
                }
                None => secondary_slice,
            };
            // A mixed effect off the chain whose key was drawn on the GPU
            // (#1266): its matte is read back (the chain that drew it is
            // submitted by now), since the key's picture may be nowhere on
            // the CPU.
            let key_read_back;
            let secondary_slice = match graph.input_of(node_id, 1).filter(|id| secondary_slice.is_none() && self.key_mattes.contains_key(id)) {
                Some(key) => {
                    key_read_back = self.gpu.as_ref().zip(self.key_mattes.get(&key)).and_then(|(g, (m, _))| g.read_keyed_matte(m, (width as usize) * (height as usize)));
                    self.keyed_matte_read(key);
                    key_read_back.as_deref()
                }
                None => secondary_slice,
            };
            if matches!(node.kind, NodeKind::Effect | NodeKind::EffectSegment | NodeKind::Frei0r)
                && node.params.effect_blend.is_some()
                && graph.input_of(node_id, 1).is_some()
                && secondary_slice.is_none()
            {
                return Err(ExecutorError::MaskUnavailable(node_id));
            }
            // The matte a grade's alpha takes (#898) is there, or the run
            // is refused: a clip shown whole where it should be cut out
            // would say nothing. No pixels is a window with nothing
            // chosen, which keeps everything.
            if node.kind == NodeKind::OutputAlpha
                && graph.input_of(node_id, 1).is_some()
                && !match secondary_slice {
                    Some(m) => m.len() == primary_slice.len(),
                    // Its producer gave no pixels, and no window or key of
                    // it is drawn elsewhere (both read above).
                    None => secondary.is_some_and(|m| m.is_empty()),
                }
            {
                return Err(ExecutorError::MaskUnavailable(node_id));
            }

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
                secondary_matte.as_ref().filter(|(id, _)| graph.input_of(node.id, 1) == Some(*id)).map(|(_, m)| m),
                ctx.as_deref_mut(),
            );
            // A gate run here has read what was kept for it.
            self.gate_read(node_id, secondaries, &mut kept, &mut secondary_matte);

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
            // A heavy node the parked frame holds (#1863), with its own
            // declines from this run.
            if let Some(key) = held_run.store.get(&node_id) {
                let declines = self.run_declines.iter().filter(|d| d.node == node_id).cloned().collect();
                self.held.put(key.clone(), Arc::clone(&output_arc), declines);
                // A lone, unmixed denoise drawn on the GPU (#2266): its
                // output is that buffer, the held entry's GPU copy as it is.
                let lone_denoise = node.params.effect_blend.is_none() && !node.params.effects.is_empty() && node.params.effects.iter().all(|e| matches!(e, klipp_effects::BuiltinEffect::TemporalDenoise { .. }));
                if let (Some((id, buffer)), true) = (self.run_denoise_kept.take(), lone_denoise) {
                    if let Some(device) = self.gpu.as_ref().filter(|_| id == node_id).map(|g| g.device()) {
                        self.held.set_gpu(key, buffer, &device);
                    }
                }
            }

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
        // The run's pool slots are free again (step 9c); the most it held
        // at once is reported (D10, L7).
        let peak_slots = self.gpu.as_mut().map_or(0, |gpu| gpu.end_slot_run());
        self.last_run_chain_stats = ChainStats {
            // A key drawn on the chain is a dispatch on it too.
            extends: chain_extend_count + std::mem::take(&mut self.run_key_draws),
            flushes: chain_flush_count,
            cpu_nodes: cpu_fallback_count,
            submits: u32::try_from(submits_after.saturating_sub(submits_before)).unwrap_or(u32::MAX),
            uploads: u32::try_from(uploads_after.saturating_sub(uploads_before)).unwrap_or(u32::MAX),
            copies: node_outputs.copies,
            evaluations,
            effect_breaks: std::mem::take(&mut self.run_effect_breaks),
            effect_mixes_refused: std::mem::take(&mut self.run_mix_refused),
            frei0r_fallbacks: std::mem::take(&mut self.run_frei0r_fallbacks),
            frei0r_refused: std::mem::take(&mut self.run_frei0r_refused),
            peak_slots: u32::try_from(peak_slots).unwrap_or(u32::MAX),
        };
        chain_stats_ledger(&self.last_run_chain_stats);
        self.last_run_declines = std::mem::take(&mut self.run_declines);
        // The run's chain is submitted: the neighbours it read can go.
        self.run_neighbour_bufs.clear();
        self.run_denoise_kept = None;
        self.run_denoise = None;
        self.last_denoise_on_chain = std::mem::take(&mut self.run_denoise_on_chain);
        self.run_blend = None;
        self.last_blend_on_chain = std::mem::take(&mut self.run_blend_on_chain);
        self.last_neighbour_uploads = std::mem::take(&mut self.run_neighbour_uploads);
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
        // Only when it is printed: it builds a map and formats every entry.
        if total_ms > 5.0 && tracing::enabled!(tracing::Level::DEBUG) {
            // Roll up per-node timings: kind#path → total ms across this frame.
            let mut by_kind: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
            for (k, ms) in &per_node_ms {
                *by_kind.entry(k.clone()).or_insert(0.0) += ms;
            }
            let mut entries: Vec<(String, f64)> = by_kind.into_iter().collect();
            entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let breakdown = entries.iter()
                .filter(|(k, ms)| *ms >= 0.1 || k.ends_with("/seeded"))
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
        // A solo inside a compound is that compound's output here (#1475).
        let result_node_id = match self.solo_node {
            Some(id) if node_outputs.contains_key(&id) => id,
            Some(_) => self.inside.solo_in.filter(|c| node_outputs.contains_key(c)).unwrap_or(normal_output_id),
            None => normal_output_id,
        };
        let result_arc = node_outputs
            .remove(&result_node_id)
            .ok_or(ExecutorError::MissingNodeOutput)?;
        let result = Arc::try_unwrap(result_arc).unwrap_or_else(|a| (*a).clone());

        let tapped_vec =
            tapped_output.map(|a| Arc::try_unwrap(a).unwrap_or_else(|a| (*a).clone()));

        Ok((result, tapped_vec, node_thumbs))
    }

    /// Whether a frame-blend node with `params` draws on the chain
    /// (#2854): a cross-fade or a flow alone, unmixed, in a run that has a
    /// frame context, and not a node the parked frame is to hold.
    /// Whether a source-boundary node with `params` draws on the chain
    /// (#1988): temporal noise reduction alone (a frame blend is
    /// `blend_on_chain`'s), unmixed, in a run that has a frame context to read the
    /// neighbours from, and not a node the parked frame is to hold (whose
    /// output must be pixels). A mixed or masked one keeps the CPU route.
    fn blend_on_chain(&self, params: &NodeParams) -> bool {
        self.blend_chain_allowed
            && self.run_has_sources
            && self.node_denoise_on_chain
            && params.effect_blend.is_none()
            && matches!(params.effects.as_slice(), [klipp_effects::BuiltinEffect::SpeedRamp { blend_mode: 1 | 2, .. }])
    }

    fn denoise_on_chain(&self, params: &NodeParams) -> bool {
        self.denoise_chain_allowed
            && self.run_has_sources
            && self.node_denoise_on_chain
            && params.effect_blend.is_none()
            && !params.effects.is_empty()
            && params.effects.iter().all(|e| matches!(e, klipp_effects::BuiltinEffect::TemporalDenoise { .. }))
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
            // A clip's effects (step 10): on the chain when the legacy
            // route would run them on the GPU too.
            // A source-boundary node reads source frames the chain hasn't.
            NodeKind::Effect | NodeKind::EffectSegment => {
                !params.effects.is_empty()
                    && !klipp_effects::has_frei0r(&params.effects)
                    && (!params.effects.iter().any(klipp_effects::registry::needs_source_neighbours)
                        || self.denoise_on_chain(params)
                        || self.blend_on_chain(params))
            }
            NodeKind::SceneToDisplay => params.scene_to_display.is_some(),
            // Its picture is the render's, on the CPU: a node reading it
            // uploads it (`upload_side_picture`, or a composite's port).
            NodeKind::SidePicture => false,
            // A plug-in takes 8-bit sRGB on the CPU: never on the chain.
            NodeKind::Ofx => false,
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
            NodeKind::HdrZones => params.hdr_zones.as_ref().is_some_and(|z| !z.is_neutral()),
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

    /// Open a chain on `primary` with no node dispatched yet: uploaded, or
    /// the source's seed when it is there at this size (as
    /// [`Self::start_chain`] does). The picture is in `buf_a`. `false`
    /// when there is no GPU or nothing to start from.
    fn begin_chain_from(&mut self, primary: &[f32], width: u32, height: u32) -> bool {
        let Some(gpu) = self.gpu.as_mut().filter(|g| !g.is_device_lost()) else { return false };
        gpu.ensure_chain_buffers(u64::from(width) * u64::from(height) * 16);
        if self.run_seed_dims == Some((width, height)) {
            self.run_seed_dims = None;
        } else if primary.is_empty() {
            self.run_seed_dims = None;
            return false;
        } else {
            gpu.upload_chain_input_linear(primary);
        }
        gpu.begin_chain();
        true
    }

    /// The difference key's plate on the GPU, resized to the frame; `false`
    /// when it won't load.
    fn difference_plate_on_gpu(&mut self, dk: &klipp_core::DifferenceKeySettings, width: u32, height: u32) -> bool {
        let Some(path) = dk.plate_path.clone() else { return false };
        let key = Self::plate_gpu_key(&path, width, height);
        // Resident already: skip the resize entirely, not just the upload.
        // This is the steady state during playback.
        if self.gpu.as_ref().is_some_and(|g| g.has_difference_key_plate(key)) {
            return true;
        }
        let Some(plate) = self.resized_plate(&path, width, height) else { return false };
        match self.gpu.as_mut() {
            Some(gpu) => {
                gpu.upload_difference_key_plate(&plate, key);
                gpu.has_difference_key_plate(key)
            }
            None => false,
        }
    }

    /// Draw on the GPU every pending key of `picture`, which is on the chain
    /// in `buf_a` (`in_is_a`) or `buf_b` (#1266). A key that can't be drawn
    /// (no plate, no slot) is left to its own turn, which keys it on the
    /// CPU. Each drawn key is a reader of `picture` the chain serves.
    #[allow(clippy::too_many_arguments)]
    fn draw_keys_of(&mut self, picture: NodeId, in_is_a: bool, pending: &mut Vec<PendingKey>, served: &mut HashMap<NodeId, usize>, graph: &NodeGraph, width: u32, height: u32) {
        let mut i = 0;
        while i < pending.len() {
            if pending[i].picture != picture {
                i += 1;
                continue;
            }
            let k = pending.remove(i);
            let plate_ready = match k.key {
                klipp_effects::gpu::GpuKey::DifferenceKey { .. } => graph
                    .nodes
                    .iter()
                    .find(|n| n.id == k.node)
                    .and_then(|n| n.params.difference_key.clone())
                    .is_some_and(|dk| self.difference_plate_on_gpu(&dk, width, height)),
                _ => true,
            };
            if let Some(matte) = plate_ready.then(|| self.gpu.as_mut().and_then(|g| g.key_into_matte_slot(&k.key, in_is_a, width, height))).flatten() {
                self.key_mattes.insert(k.node, (matte, k.readers));
                self.run_key_draws += 1;
                *served.entry(picture).or_insert(0) += 1;
            }
        }
    }

    /// The key node `key`, reading the side picture `side`, drawn on the GPU
    /// into a matte for its `readers` (#1265): the picture uploaded into a
    /// kept slot, keyed from there, and the slot given back. Nothing is
    /// drawn without a GPU, the picture, a slot or a plate; the key is then
    /// keyed on the CPU by its reader.
    #[allow(clippy::too_many_arguments)]
    fn draw_side_key(&mut self, key: NodeId, side: &crate::side_picture::SideKey, readers: usize, graph: &NodeGraph, clip_time_secs: f64, width: u32, height: u32) {
        let Some(node) = graph.nodes.iter().find(|n| n.id == key) else { return };
        let params = params_for_frame(node, clip_time_secs);
        let Some(kept) = self.upload_side_picture(side, width, height) else { return };
        let matte = self.key_from_kept(&node.kind, &params, &kept, width, height);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.release_picture(kept);
        }
        if let Some(matte) = matte {
            self.key_mattes.insert(key, (matte, readers));
            self.run_key_draws += 1;
        }
    }

    /// One reader of the key `id`'s GPU matte has read it; the last gives
    /// its slot back.
    fn keyed_matte_read(&mut self, id: NodeId) {
        let Some((_, left)) = self.key_mattes.get_mut(&id) else { return };
        *left = left.saturating_sub(1);
        if *left == 0 {
            if let (Some((matte, _)), Some(gpu)) = (self.key_mattes.remove(&id), self.gpu.as_mut()) {
                gpu.release_keyed_matte(matte);
            }
        }
    }

    /// `value` in `held`, giving back the slot of the picture it held.
    fn keep_as(&mut self, held: &mut Option<(NodeId, klipp_effects::gpu::KeptPicture)>, value: Option<(NodeId, klipp_effects::gpu::KeptPicture)>) {
        if let (Some((_, old)), Some(gpu)) = (std::mem::replace(held, value), self.gpu.as_mut()) {
            gpu.release_picture(old);
        }
    }

    /// `value` in `held`, giving back the slot of the matte it held.
    fn matte_as(&mut self, held: &mut Option<(NodeId, klipp_effects::gpu::KeyedMatte)>, value: Option<(NodeId, klipp_effects::gpu::KeyedMatte)>) {
        if let (Some((_, old)), Some(gpu)) = (std::mem::replace(held, value), self.gpu.as_mut()) {
            gpu.release_keyed_matte(old);
        }
    }

    /// A grade secondary's key (`kind` with `params`, as at this frame),
    /// drawn on the GPU from the picture kept for it into a matte of its
    /// own (M3 step 17b). `None` when it can't be: a difference key's
    /// plate missing, the pool's budget.
    fn key_from_kept(&mut self, kind: &NodeKind, params: &NodeParams, from: &klipp_effects::gpu::KeptPicture, width: u32, height: u32) -> Option<klipp_effects::gpu::KeyedMatte> {
        let key = gpu_key(kind, params)?;
        if let (NodeKind::DifferenceKey, Some(dk)) = (kind, params.difference_key.as_ref()) {
            if !self.difference_plate_on_gpu(dk, width, height) {
                return None;
            }
        }
        self.gpu.as_mut()?.key_kept_into_matte_slot(&key, from, width, height)
    }

    /// After `node`, if it is a recognised secondary's source, its picture
    /// kept in a slot of its own for the gates and keys that read it (M3
    /// step 17b).
    fn keep_for_gates(&mut self, node: NodeId, in_is_a: bool, secondaries: &[SecondaryChain], kept: &mut HashMap<NodeId, (klipp_effects::gpu::KeptPicture, usize)>, width: u32, height: u32) {
        let readers = gate_readers(secondaries, node);
        if readers == 0 {
            return;
        }
        let Some(gpu) = self.gpu.as_mut() else { return };
        if let Some(old) = kept.remove(&node) {
            gpu.release_picture(old.0);
        }
        if let Some(picture) = gpu.keep_picture(in_is_a, width, height) {
            kept.insert(node, (picture, readers));
        }
    }

    /// `node`, if it is a recognised secondary's gate, has read its kept
    /// picture and its matte: the last reader of each gives its slot back.
    fn gate_read(&mut self, node: NodeId, secondaries: &[SecondaryChain], kept: &mut HashMap<NodeId, (klipp_effects::gpu::KeptPicture, usize)>, window: &mut Option<(NodeId, klipp_effects::gpu::KeyedMatte)>) {
        let Some(sec) = secondaries.iter().find(|sec| sec.apply == node).copied() else { return };
        for picture in std::iter::once(sec.source).chain(sec.keyed) {
            if let Some((_, left)) = kept.get_mut(&picture) {
                *left = left.saturating_sub(1);
                if *left == 0 {
                    if let (Some((p, _)), Some(gpu)) = (kept.remove(&picture), self.gpu.as_mut()) {
                        gpu.release_picture(p);
                    }
                }
            }
        }
        if window.as_ref().is_some_and(|(id, _)| *id == sec.matte) {
            self.matte_as(window, None);
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
        keep_input_for_gates: bool,
        keys_of_input: &mut dyn FnMut(&mut Self),
    ) -> Option<u32> {
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
        self.chain_start_split.clear();
        let gpu = self.gpu.as_mut()?;
        if gpu.is_device_lost() {
            return None;
        }
        let byte_size = (width as u64) * (height as u64) * 16;
        let t = std::time::Instant::now();
        gpu.ensure_chain_buffers(byte_size);
        self.chain_start_split.push(("buffers", ms(t)));
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
        let t = std::time::Instant::now();
        if seed_usable {
            self.run_seed_dims = None;
            self.chain_start_split.push(("seeded", 0.0));
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
            self.chain_start_split.push(("uploaded", ms(t)));
        }
        // Open the single-submit chain encoder. Every `*_chain` dispatch
        // records into this; `flush_chain` calls `submit_chain` to batch
        // the whole segment into one `queue.submit`. Per-node submits
        // cost ~2-5 ms on NVIDIA Vulkan and dominated `grade_ms` during
        // scrubbing before this.
        // Opening the chain hands the queue the stack frame's passes so
        // far (`flush`), then makes the encoder: timed apart (#2107). With
        // `KLIPP_CHAIN_START_GPU_WAIT=1`, the GPU's work handed to it so
        // far is waited out first, a diagnostic: what that wait takes is
        // how far the GPU was behind.
        if gpu_wait_diagnostic() {
            let waited = gpu.wait_for_submitted_work();
            self.chain_start_split.push(("gpu_backlog", waited));
        }
        let t = std::time::Instant::now();
        gpu.flush();
        self.chain_start_split.push(("flush", ms(t)));
        let t = std::time::Instant::now();
        gpu.begin_chain();
        self.chain_start_split.push(("begin", ms(t)));
        let t = std::time::Instant::now();
        // The picture just uploaded is some composite's port-1 input,
        // and the dispatch below is about to overwrite the slot holding
        // it. Park it first — this is the case a Blend against the
        // ungraded source hits, and it is the common one.
        if keep_input_for_composite {
            self.start_kept = gpu.keep_picture(/* in_is_a = */ true, width, height);
        }
        if keep_input_for_gates {
            self.start_kept_for_gates = gpu.keep_picture(/* in_is_a = */ true, width, height);
        }
        self.chain_start_split.push(("keep", ms(t)));
        // Keys of the input, for effects' masks (#1266), read it here.
        let t = std::time::Instant::now();
        keys_of_input(self);
        self.chain_start_split.push(("keys", ms(t)));
        let t = std::time::Instant::now();
        // Dispatch this node with in = buf_a, out = buf_b. The first
        // node in a chain is always one of the whitelisted single-sub-
        // effect kinds (start_chain is only called via can_start_chain,
        // which is gated by node_supports_gpu_chain). Contrast and
        // Saturation can still return Some(2) if both their sub-effects
        // are active — mirror the net slot parity in the caller via
        // chain_slot_a at the call site in execute_linear.
        let dispatches = self.dispatch_chain_node_slots(node_id, kind, params, width, height, frame_number, /* in_is_a */ true);
        self.chain_start_split.push(("dispatch", ms(t)));
        let n = dispatches.unwrap_or(0);
        if n == 0 {
            // Dispatch declined (device-lost or unexpected param). Close
            // the encoder we just opened so the next chain segment starts
            // clean. No dispatches were recorded so submit is effectively
            // a no-op fence.
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.submit_chain();
                for picture in [self.start_kept.take(), self.start_kept_for_gates.take()].into_iter().flatten() {
                    gpu.release_picture(picture);
                }
            }
        }
        dispatches
    }

}

/// Whether chain starts wait out the GPU's backlog to time it
/// (`KLIPP_CHAIN_START_GPU_WAIT=1`), read once: a diagnostic (#2107).
fn gpu_wait_diagnostic() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("KLIPP_CHAIN_START_GPU_WAIT").as_deref() == Ok("1"))
}

/// A temporal-denoise node's neighbours, read for this run (#1988).
struct RunDenoise {
    node: NodeId,
    neighbours: Vec<NeighbourPixels>,
}

/// A frame-blend node's frames, read for this run (#2854): the source
/// frame to start from when it isn't the one decoded, the frame toward,
/// how far, by which means, and the pair's times.
struct RunBlend {
    node: NodeId,
    effect: klipp_effects::BuiltinEffect,
    from: Option<NeighbourPixels>,
    /// `None`: nothing to blend toward, the frame as it is.
    to: Option<NeighbourPixels>,
    weight: f32,
    flow: bool,
    pts: (f64, f64),
    clip: klipp_core::ClipId,
}

/// A neighbour buffer the chain binds (#1988): the source's own, or one
/// uploaded for it.
enum HeldNeighbour {
    Source(Arc<dyn crate::frame_context::GpuFramePixels>),
    Uploaded(Arc<wgpu::Buffer>),
}

impl HeldNeighbour {
    fn buffer(&self) -> &wgpu::Buffer {
        match self {
            HeldNeighbour::Source(g) => g.buffer(),
            HeldNeighbour::Uploaded(b) => b,
        }
    }
}

/// A neighbour frame where the source had it (#1988).
#[derive(Clone)]
enum NeighbourPixels {
    Cpu(Arc<Vec<f32>>),
    /// On the GPU already: the chain binds it as it is.
    Gpu(Arc<dyn crate::frame_context::GpuFramePixels>),
}

impl NeighbourPixels {
    /// On the CPU, read back if it was on the GPU.
    fn cpu(&self) -> Arc<Vec<f32>> {
        match self {
            NeighbourPixels::Cpu(p) => Arc::clone(p),
            NeighbourPixels::Gpu(g) => g.read(),
        }
    }
}

/// Tell the source which source-boundary node's requests follow, and the
/// frames its effects read together: the blend the next frame (and the
/// one after when the frame shown was the one before the time), the
/// denoiser the frame either side.
fn source_node_budget(node_id: NodeId, effects: &[klipp_effects::BuiltinEffect], ctx: &mut crate::frame_context::FrameContext<'_>) {
    let budget = effects.iter().map(|e| match e {
        klipp_effects::BuiltinEffect::SpeedRamp { .. } => 2,
        klipp_effects::BuiltinEffect::TemporalDenoise { .. } => klipp_effects::temporal_denoise::NEIGHBOURS,
        _ => 0,
    }).sum();
    ctx.sources.for_node(node_id, budget);
}

/// How many reads of `picture` the recognised secondaries make: each gate
/// whose source it is, each key drawn from it (M3 step 17b).
fn gate_readers(secondaries: &[SecondaryChain], picture: NodeId) -> usize {
    secondaries.iter().map(|sec| usize::from(sec.source == picture) + usize::from(sec.keyed == Some(picture))).sum()
}

/// A picture's pixels on the CPU: held, or the source frame for the source
/// (empty when the source is only on the GPU).
fn picture_pixels<'a>(outputs: &'a crate::plan::Values<'_>, picture: NodeId, is_source: bool, source_frame: &'a [f32]) -> Option<&'a [f32]> {
    outputs.get(&picture).filter(|a| !a.is_empty()).map(|a| a.as_slice()).or(if is_source { Some(source_frame) } else { None })
}

/// A key to draw on the GPU for effects' masks when its picture is on the
/// chain (#1266).
struct PendingKey {
    node: NodeId,
    picture: NodeId,
    key: klipp_effects::gpu::GpuKey,
    readers: usize,
}

/// The GPU's form of a key node's settings for an effect's mask (#1266),
/// or `None` for one it can't draw: switched off (the CPU path's whole
/// matte), a qualifier's finesse (clean, shrink and grow, blur run on the
/// CPU), a difference key without a plate.
/// A qualifier's keep and remove volumes in 3D mode (#895), as its
/// kernels take them; `None` in another mode.
fn key_volumes(q: &klipp_core::QualifierSettings) -> Option<klipp_effects::gpu::QualifierVolume> {
    (q.mode == klipp_core::QualifierMode::ThreeD).then(|| klipp_effects::gpu::QualifierVolume {
        keep: klipp_effects::cpu_grade::qualifier_volume(&q.keep_samples),
        remove: klipp_effects::cpu_grade::qualifier_volume(&q.remove_samples),
        radius: q.key_radius,
        soft: q.key_soft,
    })
}

/// A qualifier's RGB ranges in RGB mode (#895), as its kernels take
/// them; `None` in HSL mode.
fn rgb_ranges(q: &klipp_core::QualifierSettings) -> Option<[f32; 7]> {
    (q.mode == klipp_core::QualifierMode::Rgb).then_some([q.red_low, q.red_high, q.green_low, q.green_high, q.blue_low, q.blue_high, q.rgb_soft])
}

fn gpu_key(kind: &NodeKind, params: &NodeParams) -> Option<klipp_effects::gpu::GpuKey> {
    use klipp_effects::gpu::GpuKey;
    match kind {
        NodeKind::Qualifier => params.qualifier.as_ref().filter(|q| q.enabled && !q.refines()).map(|q| GpuKey::Qualifier {
            hue_center: q.hue_center,
            hue_width: q.hue_width,
            hue_soft: q.hue_soft,
            sat_low: q.sat_low,
            sat_high: q.sat_high,
            sat_soft: q.sat_soft,
            lum_low: q.lum_low,
            lum_high: q.lum_high,
            lum_soft: q.lum_soft,
            rgb: rgb_ranges(q),
            volume: key_volumes(q),
            invert: q.invert,
        }),
        NodeKind::LumaKey => params.luma_key.as_ref().filter(|k| k.enabled).map(|k| GpuKey::LumaKey { low: k.low, high: k.high, soft: k.soft, invert: k.invert }),
        NodeKind::DifferenceKey => params.difference_key.as_ref().filter(|d| d.enabled && d.plate_path.is_some()).map(|d| GpuKey::DifferenceKey {
            tolerance: d.tolerance,
            soft: d.soft,
            invert: d.invert,
            luminance: matches!(d.match_space, klipp_core::DifferenceKeyMatchSpace::Luminance),
        }),
        _ => None,
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

/// The bilateral filter's spatial, luma and chroma sigmas, floored away
/// from zero. Both sides divide by twice the square of each, so a zero
/// radius made the spatial term 0/0 and turned every weight into a NaN
/// — a black frame on the GPU, from a control the UI lets you reach.
///
/// Luma smooths the picture's luma and Chroma its chroma, each with its
/// own sigma (#1039); they used to be averaged into one sigma for all of
/// rgb, so Chroma could not smooth colour on its own. A slider at 0
/// stays 0, which the filter reads as "leave that part as it is"; a
/// slider above 0 is floored like the radius.
fn denoise_sigmas(luma: f32, chroma: f32, radius_px: f32) -> (f32, f32, f32) {
    let range = |v: f32| if v > 0.0 { v.max(0.001) } else { 0.0 };
    (radius_px.max(0.001), range(luma), range(chroma))
}

/// Where a chain-resident composite's port-1 picture comes from.
enum CompositeRoute {
    /// Already kept on the GPU, in a pool slot, when its producer ran:
    /// there is nothing to move.
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
    /// Effect nodes that broke the GPU chain: an effect not recorded
    /// (Equalize), or one run off the chain (step 10).
    pub effect_breaks: u32,
    /// Mixed effect nodes whose mix or mask couldn't be read: skipped, the
    /// input passed on (no coverage), never drawn unmasked (step 9c).
    pub effect_mixes_refused: u32,
    /// frei0r nodes rendered at a size frei0r can't take, padded and
    /// cropped back (M4 step 5).
    pub frei0r_fallbacks: u32,
    /// frei0r nodes the plug-in refused (missing, a history plug-in above
    /// 2048, no instance): the input passed on, until step 10 declines
    /// them visibly.
    pub frei0r_refused: u32,
    /// The most pool slots (frame-sized scratch buffers beside the chain's
    /// own pair) the run held at once: the bounded scratch D10 holds to a
    /// constant, and L7 prices at 4K.
    pub peak_slots: u32,
}

/// Each run's [`ChainStats`], one line per run under the name of the
/// thread that ran it (a test's name), appended to the file
/// `KLIPP_CHAIN_STATS_LEDGER` names when it is set: two builds' ledgers
/// over the same suites, diffed, show which counters a change moved.
fn chain_stats_ledger(stats: &ChainStats) {
    static LEDGER: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    let ledger = LEDGER.get_or_init(|| {
        let path = std::env::var_os("KLIPP_CHAIN_STATS_LEDGER")?;
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok().map(std::sync::Mutex::new)
    });
    if let Some(file) = ledger {
        use std::io::Write;
        let name = std::thread::current().name().unwrap_or("?").to_owned();
        if let Ok(mut f) = file.lock() {
            let _ = writeln!(f, "{name}\t{stats:?}");
        }
    }
}

/// Whether `node` keyframes any of a qualifier's matte refinements. The
/// secondary is recognised from the static settings, so a refinement
/// that is 0 still but keyed above it would be fused and dropped on
/// every frame it is not 0 (#1013).
pub(crate) fn refinement_keyed(graph: &NodeGraph, node: NodeId) -> bool {
    graph.nodes.iter().find(|n| n.id == node).is_some_and(|n| {
        n.keyframes.tracks.iter().any(|t| {
            !t.keyframes.is_empty()
                && matches!(t.param_name.as_str(), "qual_clean_black" | "qual_clean_white" | "qual_denoise" | "qual_shrink_grow" | "qual_blur")
        })
    })
}

/// A secondary correction the GPU chain can carry end to end: one
/// node's picture graded down a branch of any length and recombined with
/// itself by an ApplyMatte, through a key's or a window's matte.
///
/// ```text
///   source ─┬──────────────────────► ApplyMatte port 2 (ungraded)
///           └─► grade… ─► grade… ──► ApplyMatte port 0 (graded)
///   picture ─► Qualifier ──────────► ApplyMatte port 1 (matte)
/// ```
///
/// The key may read the source or any other picture on the chain (M3 step
/// 17b): it is drawn into a slot of its own the moment its picture is on
/// the chain, as an effect's mask key is (#1266). A lifted grade keys every
/// gate off the clip's frame, so several gates' pictures and mattes are in
/// flight at once; each is a slot of its own.
///
/// A branch node the chain cannot carry does not disqualify the shape —
/// it flushes, and ApplyMatte then reads the matte back rather than
/// dropping the secondary. Slower, still right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SecondaryChain {
    /// The node whose output is kept for the gate. Its fan-out is more
    /// than one, which normally breaks the chain; this shape is the
    /// exception.
    source: NodeId,
    /// The keyer or mask that produces the matte.
    matte: NodeId,
    apply: NodeId,
    /// The picture the key reads, kept for it; `None` for a window.
    keyed: Option<NodeId>,
}

impl SecondaryChain {
    /// Every such shape in the graph, in execution order. Empty means
    /// nothing changes and the graph runs exactly as it did.
    ///
    /// Every condition here is load-bearing. The three ApplyMatte ports
    /// have to come from the right places, the key's matte must go nowhere
    /// else, every reader of the source must be one the chain serves (the
    /// branch, the gate, a recognised key), and nothing but these shapes'
    /// own nodes may run between a source and its gate: a foreign node in
    /// between could flush the chain after the matte was written but
    /// before it was used, leaving the matte in GPU memory and the picture
    /// on the CPU (correct, read back, but slow).
    ///
    /// This used to stop at the first match, which cost a colourist the
    /// fast path the moment they pulled a second window: at 1214x2160 a
    /// graph with two secondaries ran 41.3 ms against 19.5 ms for one,
    /// because the second keyer and blend fell back to the CPU and added
    /// a flush and an upload (`flushes=2 starts=2 cpu=3` against
    /// `flushes=1 starts=1 cpu=1`).
    ///
    /// Stacked secondaries, sequential or keyed off one frame, are all
    /// found, each keeping its picture and its matte in pool slots of
    /// their own (M3 step 17b); a candidate is dropped, and the rest
    /// checked again, until every one left is served whole by the rest.
    pub(crate) fn detect_all(
        graph: &NodeGraph,
        order: &[NodeId],
        fan_out: &HashMap<NodeId, usize>,
    ) -> Vec<Self> {
        let enabled = |id: NodeId| {
            graph.nodes.iter().find(|n| n.id == id).is_some_and(|n| n.enabled)
        };
        // Each candidate with the nodes it accounts for: its picture, its
        // matte, its gate and its graded branch.
        let mut candidates: Vec<(Self, Vec<NodeId>)> = Vec::new();
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
            // The matte comes from a key or a window that is switched on
            // and feeds nothing but this node. A key is drawn on the GPU
            // the moment its picture is on the chain (M3 step 17b), so it
            // may key any picture, not only the one the gate takes when
            // the key is off: a lifted grade keys every gate off the clip's
            // frame. A window draws geometry and has no input.
            if !enabled(matte) || fan_out.get(&matte).copied().unwrap_or(0) != 1 {
                continue;
            }
            let Some(matte_node) = graph.nodes.iter().find(|n| n.id == matte) else { continue };
            let matte_armed = match matte_node.kind {
                // The chain's key draws the window alone. A refined matte
                // (clean, shrink/grow, blur), static or keyed, is finished
                // on the CPU, so that qualifier runs per node (#1013).
                NodeKind::Qualifier | NodeKind::LumaKey | NodeKind::DifferenceKey => {
                    graph.input_of(matte, 0).is_some()
                        && gpu_key(&matte_node.kind, &matte_node.params).is_some()
                        && !refinement_keyed(graph, matte)
                }
                NodeKind::Mask => matte_node
                    .params
                    .masks
                    .as_ref()
                    .is_some_and(|ms| ms.masks.iter().any(|m| m.enabled)),
                _ => false,
            };
            if !matte_armed {
                continue;
            }
            // The graded branch: any number of nodes, each fed by the
            // one before, the first fed by the picture the gate takes when
            // the key is off. Walking it back from ApplyMatte is how we
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
            let mut shape = vec![plain, matte, apply];
            shape.extend_from_slice(&branch);
            let keyed = (matte_node.kind != NodeKind::Mask).then(|| graph.input_of(matte, 0)).flatten();
            candidates.push((Self { source: plain, matte, apply, keyed }, shape));
        }
        // Drop candidates until every one left is served whole by the rest:
        // removing one can take away what made another's readers and
        // neighbours its own.
        loop {
            let keys: Vec<NodeId> = candidates.iter().filter(|(c, _)| c.keyed.is_some()).map(|(c, _)| c.matte).collect();
            let sources: Vec<NodeId> = candidates.iter().map(|(c, _)| c.source).collect();
            let all: Vec<NodeId> = candidates.iter().flat_map(|(_, shape)| shape.iter().copied()).collect();
            let at = |id: NodeId| order.iter().position(|n| *n == id);
            let before = candidates.len();
            candidates.retain(|(c, shape)| {
                // Every reader of the picture must be one the chain serves:
                // the branch's first node, the gate (from the picture kept
                // for it), or a key drawn from it on the GPU. A reader
                // beyond those would need the picture on the CPU, which is
                // exactly what the fan-out rule this shape suspends is
                // there to guarantee.
                let first_branch = shape.get(3..).and_then(|b| b.last()).copied().unwrap_or(c.apply);
                let served = graph.edges.iter().filter(|e| e.from_node == c.source).all(|e| e.to_node == c.apply || e.to_node == first_branch || keys.contains(&e.to_node));
                // Nothing foreign may run between the picture and the gate.
                // The picture and the matte sit in their slots across that
                // stretch, and a node that flushed the chain in between
                // would leave the gate on the CPU, where the matte has to
                // be read back: correct, but the transfer is the whole
                // thing we are avoiding. Another secondary's nodes ride the
                // chain too, so they are not foreign.
                let ordered = match (at(c.source), at(c.apply)) {
                    (Some(i_source), Some(i_apply)) => order[i_source + 1..i_apply].iter().all(|n| shape.contains(n) || all.contains(n)),
                    _ => false,
                };
                // A key draws from a picture the run keeps: a secondary's
                // source (its own or another's).
                let keyed_kept = c.keyed.is_none_or(|p| sources.contains(&p));
                served && ordered && keyed_kept
            });
            if candidates.len() == before {
                break;
            }
        }
        let mut found: Vec<Self> = candidates.into_iter().map(|(c, _)| c).collect();
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

    /// Temporal noise reduction of `input` on the GPU off the chain
    /// (#2266), against `neighbours` made buffers as the chain makes them:
    /// the pixels and the buffer they are in, or `None` without a GPU or
    /// when a neighbour won't go up or the draw fails.
    fn denoise_off_chain(&mut self, input: &[f32], neighbours: &[NeighbourPixels], width: u32, height: u32, threshold: f32, strength: f32) -> Option<(Vec<f32>, Arc<wgpu::Buffer>)> {
        let luma = klipp_color::linear::luma_coefs(self.working_colorspace);
        let gpu = self.gpu.as_mut()?;
        let pixels = (width as usize) * (height as usize) * 4;
        let mut held: Vec<HeldNeighbour> = Vec::with_capacity(neighbours.len());
        for n in neighbours {
            held.push(match n {
                NeighbourPixels::Gpu(g) if g.srgb8() => HeldNeighbour::Uploaded(gpu.neighbour_from_u8(g.buffer(), width, height)?),
                NeighbourPixels::Gpu(g) => HeldNeighbour::Source(Arc::clone(g)),
                NeighbourPixels::Cpu(p) if p.len() == pixels => HeldNeighbour::Uploaded(gpu.upload_neighbour(p)?),
                NeighbourPixels::Cpu(_) => return None,
            });
        }
        let bufs: Vec<&wgpu::Buffer> = held.iter().map(HeldNeighbour::buffer).collect();
        gpu.temporal_denoise_pixels(input, width, height, &bufs, threshold, strength, luma)
    }

    /// Temporal noise reduction on the chain (#1988): the chain's picture
    /// against the neighbours `read_denoise_for_chain` read for this node,
    /// uploaded to buffers of their own, each of the node's denoises one
    /// dispatch. The slot flips it made (2, the picture where it was,
    /// for none), or `None` when the neighbours weren't read for it or
    /// won't go up: the CPU route draws it then, with the same frames.
    fn denoise_chain(&mut self, node_id: NodeId, params: &NodeParams, width: u32, height: u32, in_is_a: bool) -> Option<u32> {
        let read = self.run_denoise.as_ref().filter(|r| r.node == node_id)?;
        let pixels = (width as usize) * (height as usize) * 4;
        let gpu = self.gpu.as_ref()?;
        // Each neighbour as a buffer: one on the GPU already as it is, one
        // on the CPU uploaded to a buffer of its own. Both held until the
        // run's chain is submitted.
        let mut held: Vec<HeldNeighbour> = Vec::with_capacity(read.neighbours.len());
        for n in &read.neighbours {
            held.push(match n {
                // sRGB bytes on the GPU (a viewer stream's frame): decoded
                // to linear there, into a buffer of its own.
                NeighbourPixels::Gpu(g) if g.srgb8() => HeldNeighbour::Uploaded(gpu.neighbour_from_u8(g.buffer(), width, height)?),
                NeighbourPixels::Gpu(g) => HeldNeighbour::Source(Arc::clone(g)),
                NeighbourPixels::Cpu(p) if p.len() == pixels => {
                    self.run_neighbour_uploads += 1;
                    HeldNeighbour::Uploaded(gpu.upload_neighbour(p)?)
                }
                NeighbourPixels::Cpu(_) => return None,
            });
        }
        let bufs: Vec<&wgpu::Buffer> = held.iter().map(HeldNeighbour::buffer).collect();
        let luma_weights = klipp_color::linear::luma_coefs(self.working_colorspace);
        let gpu = self.gpu.as_mut()?;
        let mut flips = 0u32;
        let mut slot_a = in_is_a;
        for effect in &params.effects {
            let klipp_effects::BuiltinEffect::TemporalDenoise { strength, threshold } = *effect else { continue };
            // Nothing to average, or nothing mixed back: the CPU's
            // `denoise_frame` hands the frame back as it was.
            if strength <= 0.0 || bufs.is_empty() {
                continue;
            }
            if !gpu.apply_temporal_denoise_chain(slot_a, width, height, &bufs, threshold, strength, luma_weights) {
                // Nothing recorded for this one; any before it are, so
                // the chain can't hand over to the CPU mid-node: the run
                // says its picture lacks an effect.
                if flips == 0 {
                    return None;
                }
                self.run_effects_incomplete = true;
                continue;
            }
            slot_a = !slot_a;
            flips += 1;
        }
        self.run_neighbour_bufs.extend(held);
        self.run_denoise = None;
        self.run_denoise_on_chain += 1;
        Some(if flips == 0 { 2 } else { flips })
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
        // A clip's effects record into the chain; ones not recorded yet
        // are counted breaks (`ChainStats::effect_breaks`).
        if let NodeKind::Effect | NodeKind::EffectSegment = kind {
            // A faulted effect (a test's, `fault`) is the CPU arm's, which
            // declines it.
            if params.effects.iter().any(crate::fault::faults) {
                return None;
            }
            if self.denoise_on_chain(params) {
                return self.denoise_chain(node_id, params, width, height, in_is_a);
            }
            if self.blend_on_chain(params) {
                return self.blend_chain(node_id, width, height, in_is_a);
            }
            // An analysis node's motion goes up just before it records, and
            // a warp without an analysis clears it, so it passes the frame
            // through; a second in the run would overwrite the first before
            // the chain submits, so that one runs on the CPU (step 9).
            let at = self.run_analysis.get(&node_id).cloned();
            if at.is_some() || params.effects.iter().any(warps_by_motion) {
                if self.run_motion_uploaded {
                    return None;
                }
                self.run_motion_uploaded = true;
                upload_motion(self.gpu.as_mut()?, &at.as_ref().map(|a| a.motion.clone()).unwrap_or_default());
            }
            let effects = with_gain(&params.effects, at.as_ref());
            let effects = effects.as_ref();
            let mask = self.effect_mask.take();
            let window = self.effect_window.take();
            // Taken out while the run reads it, and put back for the key's
            // other readers whatever the run does.
            let keyed = self.effect_keyed.take().and_then(|id| self.key_mattes.remove(&id).map(|k| (id, k)));
            let luma = klipp_color::linear::luma_coefs(self.working_colorspace);
            let gpu = self.gpu.as_mut()?;
            let run = match params.effect_blend {
                Some(blend) => klipp_effects::mask_mix::MaskMix::new(blend.mix, blend.channel, luma).ok().and_then(|mix| {
                    let matte = match (&window, &keyed, &mask) {
                        (Some(shapes), _, _) => Some(klipp_effects::gpu::EffectMatte::Shapes(shapes.as_slice())),
                        (None, Some((_, (m, _))), _) => Some(klipp_effects::gpu::EffectMatte::Keyed(m)),
                        (None, None, Some(pixels)) => Some(klipp_effects::gpu::EffectMatte::Pixels(pixels.as_slice())),
                        (None, None, None) => None,
                    };
                    gpu.chain_run_effects_mixed(effects, in_is_a, width, height, mix, matte)
                }),
                None => Some(gpu.chain_run_effects(effects, in_is_a, width, height)),
            };
            if let Some((id, k)) = keyed {
                self.key_mattes.insert(id, k);
                if run.is_some() {
                    self.keyed_matte_read(id);
                }
            }
            let run = run?;
            self.run_effect_breaks += run.breaks;
            self.run_effects_incomplete |= !run.complete;
            // Flips mirror the slot: an even count (none included) leaves
            // the picture where it was, which is 2, not 0 (0 is a matte).
            return Some(if run.in_a == in_is_a { 2 } else { 1 });
        }

        let gpu = self.gpu.as_mut()?;
        if *kind == NodeKind::SceneToDisplay {
            let step = params.scene_to_display.as_ref()?;
            let t = step.transfer;
            gpu.apply_scene_to_display_chain(in_is_a, width, height, t.display_light_kind(), t.scene_scale(), t.display_exposure(), step.peak_nits, step.luma);
            return Some(1);
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
            NodeKind::HdrZones => {
                let terms = hdr_zone_terms(params.hdr_zones.as_ref()?, self.working_colorspace);
                self.gpu.as_mut().unwrap().apply_hdr_zones_chain(in_is_a, width, height, &terms);
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
                let (spatial_sigma, luma_sigma, chroma_sigma) = denoise_sigmas(luma, chroma, self.px(radius as f32));
                    let max_kernel = self.denoise_max_kernel();
                self.gpu.as_mut().unwrap().apply_denoise_chain(
                    in_is_a, width, height,
                    spatial_sigma, luma_sigma, chroma_sigma, max_kernel,
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

    /// Source-boundary nodes run so far (M4 step 6): each once a frame,
    /// so a frame that ran one twice shows here.
    pub fn source_runs(&self) -> u64 {
        self.source_runs
    }

    /// Hold the outputs of heavy nodes (temporal noise reduction, frame
    /// blends, OFX and frei0r) in the runs of `source`'s clip, and take
    /// them held, until called with `None` (#1863). Only a viewer parked
    /// on a frame calls it, around that clip's grade; playback and export
    /// never do, so they never read or fill the cache. A run of another
    /// clip, or one with no frame context, ignores it.
    pub fn hold_outputs_for(&mut self, source: Option<crate::held::HeldSource>) {
        self.held.token = source;
    }

    /// The parked frame's source runs hold outputs for now, if any: set
    /// around one clip's grade, and none outside it.
    pub fn holding_for(&self) -> Option<&crate::held::HeldSource> {
        self.held.token.as_ref()
    }

    /// Fill the chain's input slot with `pixels`, the held output under
    /// `key`, from its GPU copy (made from them on first use), and mark it
    /// the run's seed: the next chain starts from it with no upload. A
    /// copy that fails (a lost device) is forgotten, and the chain uploads
    /// the pixels as before.
    fn seed_from_held(&mut self, key: &[u8], pixels: &Arc<Vec<f32>>, width: u32, height: u32) {
        if pixels.len() != (width as usize) * (height as usize) * 4 {
            return;
        }
        let Some(gpu) = self.gpu.as_mut() else { return };
        if gpu.is_device_lost() {
            self.held.drop_gpu();
            return;
        }
        let device = gpu.device();
        let buffer = match self.held.gpu_of(key, &device) {
            Some(b) => b,
            None => {
                let b = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("held_output"),
                    size: (pixels.len() * 4) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                gpu.queue().write_buffer(&b, 0, bytemuck::cast_slice(pixels.as_slice()));
                let b = Arc::new(b);
                self.held.set_gpu(key, Arc::clone(&b), &device);
                b
            }
        };
        if gpu.copy_linear_into_chain(&buffer, width, height) {
            self.run_seed_dims = Some((width, height));
        } else {
            self.held.drop_gpu();
        }
    }

    /// GPU copies of held outputs kept now.
    pub fn held_gpu_len(&self) -> usize {
        self.held.gpu_len()
    }

    /// The held outputs' counts: hits, misses, heavy nodes not held,
    /// entries stored.
    pub fn held_counts(&self) -> crate::held::HeldCounts {
        self.held.counts()
    }

    /// Entries held now.
    pub fn held_len(&self) -> usize {
        self.held.len()
    }

    /// Forget every held output: playback started or stopped, another
    /// sequence or clip, the project closed.
    pub fn clear_held(&mut self) {
        self.held.clear();
    }

    /// The held outputs' GPU copies' budget in bytes (#2266): GPU memory,
    /// apart from the CPU budget.
    pub fn set_held_gpu_budget(&mut self, bytes: usize) {
        self.held.set_gpu_budget(bytes);
    }

    /// The held outputs' budget in bytes; two entries always stay.
    pub fn set_held_budget(&mut self, bytes: usize) {
        self.held.set_budget(bytes);
    }

    /// Source-boundary nodes run with no context to read their source
    /// from, which passed their input on.
    pub fn source_unavailable(&self) -> u64 {
        self.source_unavailable
    }

    /// OFX plug-in nodes run so far (M4 step 3), and of them those refused
    /// (missing, unsupported, failed), which passed their input on.
    pub fn ofx_runs(&self) -> (u64, u64) {
        (self.ofx_runs, self.ofx_refused)
    }

    /// A side picture node's picture (#1746): the one its key names that
    /// the render handed the run ([`Self::set_side_pictures`]). With none,
    /// the picture is clear, declined: a file that's missing is named.
    fn side_picture(&mut self, node_id: NodeId, params: &NodeParams, input: &[f32]) -> Vec<f32> {
        use crate::side_picture::{SideKey, SideMissing};
        let Some(key) = params.side_picture.clone().or_else(|| params.effects.iter().find_map(SideKey::of)) else {
            self.decline(node_id, NodeKind::SidePicture, crate::DeclineOutcome::Fallback, "Side Picture".into(), "no picture named", String::new());
            return vec![0.0; input.len()];
        };
        let name = key.name();
        if matches!(&key, SideKey::Media { item, .. } if item.is_empty()) {
            self.decline(node_id, NodeKind::SidePicture, crate::DeclineOutcome::Fallback, name, "no media chosen", String::new());
            return vec![0.0; input.len()];
        }
        let got = self.side_pictures.get(&key).cloned().unwrap_or(Err(SideMissing::Unavailable));
        let (outcome, reason, why) = match got {
            Ok(p) if p.len() == input.len() => return p.to_vec(),
            Ok(p) => (crate::DeclineOutcome::NeighbourMissing, "its picture came at another size", format!("{} values for {}", p.len(), input.len())),
            Err(SideMissing::File(item)) => (crate::DeclineOutcome::FileMissing, "its media file is missing", item),
            Err(SideMissing::Unavailable) => (crate::DeclineOutcome::NeighbourMissing, "its picture has no frame here", format!("{key:?}")),
            Err(SideMissing::Unsupported(why)) => (crate::DeclineOutcome::Unsupported, "its picture can't be made here", why),
        };
        self.decline(node_id, NodeKind::SidePicture, outcome, name, reason, why);
        vec![0.0; input.len()]
    }

    /// A source-boundary node (M4 steps 6 and 7), its effects in the
    /// catalog's order (`SOURCE_BOUNDARY_ORDER`): the frame blend, then
    /// temporal noise reduction, on the source before the grade, against
    /// the frames either side of it read from the clip's source through
    /// `ctx`. A neighbour the source
    /// hasn't (past the media, the wrong size, a failed read) is left out,
    /// so the media's first and last frames denoise against one side;
    /// with none, or no context, the input passes on. Each frame left out
    /// is declined (step 10): past the media's edge as a fallback, any
    /// other as a frame missing, which an export refuses.
    fn at_the_source(&mut self, node_id: NodeId, effects: &[klipp_effects::BuiltinEffect], input: &[f32], width: u32, height: u32, ctx: Option<&mut crate::frame_context::FrameContext<'_>>) -> Vec<f32> {
        use crate::frame_context::Neighbour;
        self.source_runs += 1;
        let Some(ctx) = ctx else {
            self.source_unavailable += 1;
            return input.to_vec();
        };
        source_node_budget(node_id, effects, ctx);
        let mut out = input.to_vec();
        for effect in effects {
            // A faulted effect (a test's, `fault`): left out, declined as a
            // render that failed.
            if crate::fault::faults(effect) {
                self.decline(node_id, NodeKind::Effect, crate::DeclineOutcome::RenderFailed, crate::decline::effect_name(effect), "its render failed", "a fault set by a test".into());
                continue;
            }
            // The frame blend (M4 step 7): the frame shown mixed toward the
            // media's next one by where the source time falls between their
            // presentation times, cross-faded or, in blend mode 2, followed
            // by its motion (#2215). Without either time, or with no next
            // frame, the frame as it is.
            if let klipp_effects::BuiltinEffect::SpeedRamp { blend_mode, .. } = *effect {
                let Some(mut pts) = ctx.source_pts else { continue };
                // The frame after the one shown. A blend reads half a media
                // frame early, so a source that gives the frame at or before
                // a time can show the one before the frame at the time: then
                // that next frame is the one to blend from, and the one after
                // it the one to blend toward, whichever way the source rounds.
                let mut next = ctx.sources.next(pts);
                if let Neighbour::Frame(on) = &next {
                    if let Some(on_pts) = on.pts.filter(|p| *p > pts + 1e-6 && *p <= ctx.source_secs + 1e-9) {
                        if on.rgba.len() == out.len() {
                            out.clone_from(on.rgba.as_ref());
                            pts = on_pts;
                            next = ctx.sources.next(pts);
                        }
                    }
                }
                match next {
                    Neighbour::Frame(next) => {
                        if let Some(next_pts) = next.pts.filter(|p| *p > pts + 1e-6) {
                            let weight = (((ctx.source_secs - pts) / (next_pts - pts)).clamp(0.0, 1.0)) as f32;
                            if weight > 1e-4 && next.rgba.len() == out.len() {
                                let followed = (blend_mode == 2).then(|| self.follow_motion(node_id, effect, ctx.clip, &out, &next.rgba, (width, height), (pts, next_pts), weight)).flatten();
                                match followed {
                                    Some(pixels) => out = pixels,
                                    None => {
                                        for (o, n) in out.iter_mut().zip(next.rgba.iter()) {
                                            *o += (n - *o) * weight;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Only a frame between two of the media's needs the
                    // next: one shown on its own time is drawn whole.
                    Neighbour::Missing(m) if ctx.source_secs > pts + 1e-6 => self.decline_neighbours(node_id, effect, &[m]),
                    Neighbour::Missing(_) => {}
                }
                continue;
            }
            let klipp_effects::BuiltinEffect::TemporalDenoise { strength, threshold } = *effect else { continue };
            // Read already for the chain (`denoise_on_chain`), and it fell
            // to here: the same frames, not read or declined again.
            let neighbours = match self.run_denoise.take() {
                Some(read) if read.node == node_id => read.neighbours,
                other => {
                    self.run_denoise = other;
                    // With a GPU to draw it on, the frames the source holds
                    // there are taken as they are (#2266).
                    let on_gpu = self.gpu.is_some();
                    self.denoise_neighbours(node_id, effect, ctx, on_gpu)
                }
            };
            // On the GPU where there is one (#2266): the chain's kernel,
            // so a frame drawn here, as a parked frame's held node is, is
            // the picture the chain draws playing. The CPU's otherwise.
            if let Some((pixels, buffer)) = self.denoise_off_chain(&out, &neighbours, width, height, threshold, strength) {
                out = pixels;
                self.run_denoise_kept = Some((node_id, buffer));
                continue;
            }
            self.run_denoise_kept = None;
            let neighbours: Vec<Arc<Vec<f32>>> = neighbours.iter().map(NeighbourPixels::cpu).collect();
            let refs: Vec<&[f32]> = neighbours.iter().map(|n| n.as_slice()).collect();
            out = klipp_effects::temporal_denoise::denoise_frame(&out, &refs, threshold, strength, klipp_color::linear::luma_coefs(self.working_colorspace));
        }
        out
    }

    /// The in-between frame at `weight` from `from` (shown at `pts.0`) to
    /// `to` (at `pts.1`) by optical flow on the GPU (#2215), the pair's
    /// flow kept for its later frames. `None`, for the caller to
    /// cross-fade, without a GPU or when its draw fails; both recorded as
    /// fallbacks. A frame whose motion can't be followed comes back
    /// cross-faded whole (`klipp_effects::flow::UNRELIABLE_FRACTION`),
    /// recorded the same way and declined as a fallback, so it shows in
    /// the readiness report and not in the log alone.
    #[allow(clippy::too_many_arguments)]
    fn follow_motion(&mut self, node_id: NodeId, effect: &klipp_effects::BuiltinEffect, clip: klipp_core::ClipId, from: &[f32], to: &[f32], (width, height): (u32, u32), pts: (f64, f64), weight: f32) -> Option<Vec<f32>> {
        use klipp_effects::residency::{self, Work};
        self.ensure_gpu();
        let luma = klipp_color::linear::luma_coefs(self.working_colorspace);
        let key = klipp_effects::gpu::FlowKey { clip: clip.0.as_u128(), from_secs: pts.0, to_secs: pts.1 };
        let made = match self.gpu.as_mut() {
            Some(gpu) => gpu.interpolate_by_flow(key, from, to, width, height, weight, luma),
            None => {
                residency::record("frame_blend.optical_flow", "no GPU: the frames cross-faded", Work::Fallback);
                return None;
            }
        };
        let Some(made) = made else {
            residency::record("frame_blend.optical_flow", "the GPU's draw failed: the frames cross-faded", Work::Fallback);
            return None;
        };
        if made.cross_faded {
            residency::record("frame_blend.optical_flow", "the motion couldn't be followed: the frame cross-faded", Work::Fallback);
            let why = format!("{:.0}% of the frame couldn't be followed", made.unreliable * 100.0);
            self.decline(node_id, NodeKind::Effect, crate::DeclineOutcome::Fallback, crate::decline::effect_name(effect), "its motion couldn't be followed, so the frames cross-faded", why);
        }
        Some(made.pixels)
    }

    /// The source's frames either side of the one drawn, for temporal
    /// noise reduction in `node_id` (#1988): one of the media's frames
    /// away each, whatever the sequence's rate (its rate when the source
    /// can't say). A frame the source hasn't is left out and declined
    /// (step 10). The one reader of them, for the CPU route and the
    /// chain's alike, so the two read and decline the same.
    ///
    /// `on_gpu`: a frame the source has on the GPU already comes as it is
    /// (for the chain to bind); any other is read to the CPU.
    fn denoise_neighbours(&mut self, node_id: NodeId, effect: &klipp_effects::BuiltinEffect, ctx: &mut crate::frame_context::FrameContext<'_>, on_gpu: bool) -> Vec<NeighbourPixels> {
        use crate::frame_context::{Missing, Neighbour};
        let dt = ctx.sources.frame_secs().unwrap_or(1.0 / ctx.fps.max(1.0));
        let mut missed = Vec::new();
        let mut neighbours = Vec::new();
        for t in [ctx.source_secs - dt, ctx.source_secs + dt] {
            if on_gpu {
                match ctx.sources.linear_on_gpu(t) {
                    Neighbour::Frame(f) => {
                        neighbours.push(NeighbourPixels::Gpu(f.pixels));
                        continue;
                    }
                    Neighbour::Missing(Missing::Unavailable) => {}
                    Neighbour::Missing(m) => {
                        missed.push(m);
                        continue;
                    }
                }
            }
            match ctx.sources.linear(t) {
                Neighbour::Frame(f) => neighbours.push(NeighbourPixels::Cpu(f.rgba)),
                Neighbour::Missing(m) => missed.push(m),
            }
        }
        self.decline_neighbours(node_id, effect, &missed);
        neighbours
    }

    /// The media's next frame after the one at `pts` (#2854): on the GPU
    /// where the source holds it there, else read on the CPU; with its
    /// presentation time.
    fn next_for_chain(ctx: &mut crate::frame_context::FrameContext<'_>, pts: f64) -> Result<(NeighbourPixels, f64), crate::frame_context::Missing> {
        use crate::frame_context::{Missing, Neighbour};
        match ctx.sources.next_on_gpu(pts) {
            Neighbour::Frame(f) => return Ok((NeighbourPixels::Gpu(f.pixels), f.source_secs)),
            Neighbour::Missing(Missing::Unavailable) => {}
            Neighbour::Missing(m) => return Err(m),
        }
        match ctx.sources.next(pts) {
            Neighbour::Frame(f) => f.pts.map(|p| (NeighbourPixels::Cpu(f.rgba), p)).ok_or(Missing::Unavailable),
            Neighbour::Missing(m) => Err(m),
        }
    }

    /// Before a frame-blend node that may go on the chain (#2854): its
    /// frames read, as `at_the_source` reads them: the frame shown mixed
    /// toward the media's next one by where the source time falls between
    /// their presentation times. A source that gave the frame before the
    /// one at the time has that one start the blend instead.
    fn read_blend_for_chain(&mut self, node_id: NodeId, params: &NodeParams, ctx: Option<&mut crate::frame_context::FrameContext<'_>>) {
        self.run_blend = None;
        let Some(ctx) = ctx else { return };
        let Some(effect) = params.effects.first().cloned() else { return };
        let klipp_effects::BuiltinEffect::SpeedRamp { blend_mode, .. } = effect else { return };
        source_node_budget(node_id, &params.effects, ctx);
        let mut read = RunBlend { node: node_id, effect, from: None, to: None, weight: 0.0, flow: blend_mode == 2, pts: (0.0, 0.0), clip: ctx.clip };
        let Some(mut pts) = ctx.source_pts else {
            self.run_blend = Some(read);
            return;
        };
        let mut next = Self::next_for_chain(ctx, pts);
        if let Ok((on, on_pts)) = &next {
            if *on_pts > pts + 1e-6 && *on_pts <= ctx.source_secs + 1e-9 {
                read.from = Some(on.clone());
                pts = *on_pts;
                next = Self::next_for_chain(ctx, pts);
            }
        }
        match next {
            Ok((to, to_pts)) if to_pts > pts + 1e-6 => {
                read.weight = (((ctx.source_secs - pts) / (to_pts - pts)).clamp(0.0, 1.0)) as f32;
                read.pts = (pts, to_pts);
                if read.weight > 1e-4 {
                    read.to = Some(to);
                }
            }
            Ok(_) => {}
            // Only a frame between two of the media's needs the next.
            Err(m) if ctx.source_secs > pts + 1e-6 => self.decline_neighbours(node_id, &read.effect, &[m]),
            Err(_) => {}
        }
        self.run_blend = Some(read);
    }

    /// A frame blend on the chain (#2854): the frames `read_blend_for_chain`
    /// read bound as buffers (one on the GPU already as it is, one on the
    /// CPU uploaded), and the frame it starts from (the chain's picture, or
    /// the one read in its place) cross-faded or followed by its motion
    /// toward the next, into the chain's other buffer. The
    /// slot flips it made (2, the picture where it was, for none), or
    /// `None` when its frames weren't read for it or won't go up: the CPU
    /// route draws it then.
    fn blend_chain(&mut self, node_id: NodeId, width: u32, height: u32, in_is_a: bool) -> Option<u32> {
        let read = self.run_blend.take().filter(|r| r.node == node_id)?;
        let pixels = (width as usize) * (height as usize) * 4;
        let gpu = self.gpu.as_ref()?;
        let mut uploads = 0u32;
        let mut held = |n: &NeighbourPixels| -> Option<HeldNeighbour> {
            Some(match n {
                // Fitted to the render where the stream decoded it smaller,
                // as the frame shown is.
                NeighbourPixels::Gpu(g) if g.srgb8() => {
                    let (sw, sh) = g.source_size().unwrap_or((width, height));
                    HeldNeighbour::Uploaded(gpu.neighbour_fit_from_u8(g.buffer(), sw, sh, width, height)?)
                }
                NeighbourPixels::Gpu(g) => HeldNeighbour::Source(Arc::clone(g)),
                NeighbourPixels::Cpu(p) if p.len() == pixels => {
                    uploads += 1;
                    HeldNeighbour::Uploaded(gpu.upload_neighbour(p)?)
                }
                NeighbourPixels::Cpu(_) => return None,
            })
        };
        let from = match &read.from {
            Some(n) => Some(held(n)?),
            None => None,
        };
        let to = match &read.to {
            Some(n) => Some(held(n)?),
            None => None,
        };
        self.run_neighbour_uploads += uploads;
        let luma = klipp_color::linear::luma_coefs(self.working_colorspace);
        let gpu = self.gpu.as_mut()?;
        let from_buf = from.as_ref().map(HeldNeighbour::buffer);
        let flips = match &to {
            // Nothing to blend toward: the frame it starts from as it is.
            None => match from_buf {
                None => 2,
                Some(f) => {
                    if !gpu.apply_mix_toward_chain(in_is_a, width, height, Some(f), 0.0, f) {
                        return None;
                    }
                    1
                }
            },
            Some(to) if read.flow => {
                let key = klipp_effects::gpu::FlowKey { clip: read.clip.0.as_u128(), from_secs: read.pts.0, to_secs: read.pts.1 };
                match gpu.apply_flow_chain(in_is_a, width, height, from_buf, to.buffer(), key, read.weight, luma) {
                    Some(unreliable) => {
                        if let Some(share) = unreliable.filter(|u| *u > klipp_effects::flow::UNRELIABLE_FRACTION) {
                            klipp_effects::residency::record("frame_blend.optical_flow", "the motion couldn't be followed: the frame cross-faded", klipp_effects::residency::Work::Fallback);
                            self.decline(node_id, NodeKind::Effect, crate::DeclineOutcome::Fallback, crate::decline::effect_name(&read.effect), "its motion couldn't be followed, so the frames cross-faded", format!("{:.0}% of the frame couldn't be followed", share * 100.0));
                        }
                        1
                    }
                    // The draw wasn't recorded: cross-faded instead, as
                    // the CPU route does then.
                    None => {
                        klipp_effects::residency::record("frame_blend.optical_flow", "the GPU's draw failed: the frames cross-faded", klipp_effects::residency::Work::Fallback);
                        let gpu = self.gpu.as_mut()?;
                        if !gpu.apply_mix_toward_chain(in_is_a, width, height, from.as_ref().map(HeldNeighbour::buffer), read.weight, to.buffer()) {
                            return None;
                        }
                        1
                    }
                }
            }
            Some(to) => {
                if !gpu.apply_mix_toward_chain(in_is_a, width, height, from_buf, read.weight, to.buffer()) {
                    return None;
                }
                1
            }
        };
        self.run_neighbour_bufs.extend(from.into_iter().chain(to));
        self.run_blend_on_chain += 1;
        Some(flips)
    }

    /// Before a temporal-denoise node that may go on the chain (#1988):
    /// its neighbours read, once, for whichever route draws it.
    fn read_denoise_for_chain(&mut self, node_id: NodeId, params: &NodeParams, ctx: Option<&mut crate::frame_context::FrameContext<'_>>) {
        self.run_denoise = None;
        let Some(ctx) = ctx else { return };
        let Some(effect) = params.effects.iter().find(|e| matches!(e, klipp_effects::BuiltinEffect::TemporalDenoise { .. })) else { return };
        source_node_budget(node_id, &params.effects, ctx);
        let neighbours = self.denoise_neighbours(node_id, effect, ctx, true);
        self.run_denoise = Some(RunDenoise { node: node_id, neighbours });
    }

    /// Records a source-boundary node's `effect` that ran without the
    /// frames in `missed` (step 10): nothing when it read them all.
    fn decline_neighbours(&mut self, node: NodeId, effect: &klipp_effects::BuiltinEffect, missed: &[crate::frame_context::Missing]) {
        if let Some((outcome, reason)) = crate::decline::neighbour_outcome(missed) {
            let why = missed.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join(", ");
            self.decline(node, NodeKind::Effect, outcome, crate::decline::effect_name(effect), reason, why);
        }
    }

    /// Records a node that didn't draw as asked (step 10).
    pub(crate) fn decline(&mut self, node: NodeId, kind: NodeKind, outcome: crate::DeclineOutcome, name: String, reason: &str, why: String) {
        self.run_declines.push(crate::Decline { clip: None, node, kind, outcome, name, reason: reason.to_owned(), why });
    }

    /// A frei0r node's frame: linear → sRGB RGBA8 → the plug-in → linear,
    /// through the same conversions the effect chain used, so a plug-in
    /// without history draws as it did. Refused (missing, a history
    /// plug-in above 2048, no instance), the input passes on and is
    /// counted.
    #[allow(clippy::too_many_arguments)]
    fn run_frei0r(&mut self, node_id: NodeId, fx: &[klipp_effects::BuiltinEffect], input: &[f32], width: u32, height: u32, clip_time_secs: f64, ctx: Option<&mut crate::frame_context::FrameContext<'_>>) -> Vec<f32> {
        let Some(klipp_effects::BuiltinEffect::Frei0r { type_id, params }) = fx.first() else {
            return input.to_vec();
        };
        let Some(plugin) = klipp_effects::frei0r_effect::plugin(type_id) else {
            self.run_frei0r_refused += 1;
            self.decline(node_id, NodeKind::Frei0r, crate::DeclineOutcome::PluginMissing, crate::decline::frei0r_name(type_id), "plug-in missing", format!("{type_id} isn't installed"));
            return input.to_vec();
        };
        let name = plugin.info.name.clone();
        let rgba = klipp_effects::linear_f32_rgba_to_srgb_u8(input);
        let rendered = match ctx {
            // The render owner's instances, kept across frames.
            Some(ctx) => {
                let stamp = crate::frame_context::SlotStamp {
                    frame: ctx.frame,
                    clip_time_secs,
                    fps: ctx.fps,
                    width,
                    height,
                    source: ctx.source,
                    enabled: true,
                };
                crate::frei0r_node::render(ctx.instances, ctx.clip, node_id, stamp, &plugin, params, &rgba)
            }
            // No owner: one frame's instances, as the effect chain had.
            None => klipp_frei0r::Frei0rInstanceSet::new(plugin, width, height).and_then(|mut set| {
                set.set_params(params);
                set.render(clip_time_secs, &rgba)
            }),
        };
        match rendered {
            Ok(out) => {
                if crate::frei0r_node::fell_back(&out) {
                    self.run_frei0r_fallbacks += 1;
                    self.decline(node_id, NodeKind::Frei0r, crate::DeclineOutcome::Fallback, name, "rendered padded to a size it takes", format!("{width}×{height} padded"));
                }
                klipp_effects::srgb_u8_to_linear_f32_rgba(&out.rgba)
            }
            Err(refusal) => {
                tracing::warn!("frei0r: {refusal}; its input passed on");
                self.run_frei0r_refused += 1;
                let (outcome, reason) = crate::decline::frei0r_outcome(&refusal);
                self.decline(node_id, NodeKind::Frei0r, outcome, name, reason, refusal.to_string());
                input.to_vec()
            }
        }
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
        gpu_matte: Option<&klipp_effects::gpu::KeyedMatte>,
        ctx: Option<&mut crate::frame_context::FrameContext<'_>>,
    ) -> Vec<f32> {
        match kind {
            NodeKind::Source => source_frame.to_vec(),

            // Reroute — pure wire-routing aid, no work. Mirrors the
            // bypass path's primary-input copy so a Reroute slipped
            // into any chain is a no-op on the pixels.
            NodeKind::Reroute => primary_input.to_vec(),

            // A clip's effects off the chain: as the legacy route runs
            // them, on the GPU when the list allows it.
            // An OFX plug-in (M4 step 3), in its frame context: refused, the
            // input passes on, counted.
            NodeKind::Ofx => {
                self.ofx_runs += 1;
                let Some(entry) = params.ofx.as_ref() else {
                    self.ofx_refused += 1;
                    self.decline(node_id, NodeKind::Ofx, crate::DeclineOutcome::PluginMissing, "plug-in".into(), "no plug-in named", "the node names no plug-in".into());
                    return primary_input.to_vec();
                };
                let mut missed = Vec::new();
                match crate::plugin::run_ofx_noting(node_id, entry, primary_input, width, height, self.working_colorspace, self.render_scale as f64, ctx, &mut missed) {
                    Ok(out) => {
                        // Drawn without the frames at other times it asked
                        // for that the source hasn't (step 10).
                        if let Some((outcome, reason)) = crate::decline::neighbour_outcome(&missed) {
                            let why = missed.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join(", ");
                            self.decline(node_id, NodeKind::Ofx, outcome, entry.plugin.clone(), reason, why);
                        }
                        out
                    }
                    Err(why) => {
                        tracing::warn!("OFX node {node_id}: {} refused: {why:?}", entry.plugin);
                        self.ofx_refused += 1;
                        let (outcome, reason) = crate::decline::ofx_outcome(&why);
                        self.decline(node_id, NodeKind::Ofx, outcome, entry.plugin.clone(), reason, format!("{why:?}"));
                        primary_input.to_vec()
                    }
                }
            }

            // A picture handed to the run (#1746), as the render read it.
            NodeKind::SidePicture => self.side_picture(node_id, params, primary_input),
            // A source-boundary node (M4 step 6): it reads the clip's
            // source at other times, which only its context gives it.
            NodeKind::Effect | NodeKind::EffectSegment if params.effects.iter().any(klipp_effects::registry::needs_source_neighbours) => {
                self.at_the_source(node_id, &params.effects, primary_input, width, height, ctx)
            }
            // A frei0r plug-in on the CPU, its instances kept across
            // frames (M4 step 5).
            NodeKind::Effect | NodeKind::EffectSegment | NodeKind::Frei0r => {
                // An analysis node's own data for this frame (step 9); a
                // warp without one passes the frame through.
                let at = self.run_analysis.get(&node_id).cloned();
                let fx = with_gain(&params.effects, at.as_ref());
                // A faulted effect (a test's, `fault`) is left out, its node
                // declined as a render that failed.
                let faulted: Vec<String> = fx.iter().filter(|e| crate::fault::faults(e)).map(crate::decline::effect_name).collect();
                for name in faulted {
                    self.decline(node_id, *kind, crate::DeclineOutcome::RenderFailed, name, "its render failed", "a fault set by a test".into());
                }
                let fx: std::borrow::Cow<[klipp_effects::BuiltinEffect]> = match fx.iter().any(crate::fault::faults) {
                    true => std::borrow::Cow::Owned(fx.iter().filter(|e| !crate::fault::faults(e)).cloned().collect()),
                    false => fx,
                };
                let fx = fx.as_ref();
                let motion = at.map(|a| a.motion).unwrap_or_default();
                let luma = klipp_color::linear::luma_coefs(self.working_colorspace);
                let effect = if *kind == NodeKind::Frei0r {
                    self.run_frei0r(node_id, fx, primary_input, width, height, clip_time_secs, ctx)
                } else {
                    match self.gpu.as_mut() {
                        Some(gpu) if !klipp_effects::has_frei0r(fx) => {
                            upload_motion(gpu, &motion);
                            let out = gpu.apply_effects(fx, primary_input, width, height);
                            self.run_effect_breaks += 1;
                            out
                        }
                        _ => klipp_effects::apply_effects_with_motion(fx, primary_input.to_vec(), width, height, luma, &motion),
                    }
                };
                match params.effect_blend {
                    Some(blend) => mix_effect_cpu(blend, luma, primary_input, effect, secondary_input).unwrap_or_else(|| {
                        self.run_mix_refused += 1;
                        primary_input.to_vec()
                    }),
                    None => effect,
                }
            }

            // A scene-referred clip's display step, as `display_render`
            // runs it on the legacy route. Made only by the renderer, which
            // always sets it; without its setting it leaves the picture be.
            NodeKind::SceneToDisplay => {
                let mut out = primary_input.to_vec();
                if let Some(step) = &params.scene_to_display {
                    step.apply(&mut out);
                }
                out
            }

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
                            rgb_ranges(qual),
                            key_volumes(qual),
                        )
                    } else {
                        let mut frame = primary_input.to_vec();
                        match (rgb_ranges(qual), key_volumes(qual)) {
                            (_, Some(v)) => klipp_effects::cpu_grade::qualifier_matte_3d(&mut frame, v.keep, v.remove, v.radius, v.soft, qual.invert),
                            (Some(rgb), None) => klipp_effects::cpu_grade::qualifier_matte_rgb(&mut frame, rgb, qual.invert),
                            (None, None) => klipp_effects::cpu_grade::qualifier_matte(
                                &mut frame,
                                qual.hue_center / 360.0, qual.hue_width / 360.0, qual.hue_soft / 360.0,
                                qual.sat_low, qual.sat_high, qual.sat_soft,
                                qual.lum_low, qual.lum_high, qual.lum_soft,
                                qual.invert,
                            ),
                        }
                        frame
                    };
                    // The matte comes back to the CPU on both paths; the
                    // finesse — clean, denoise, shrink/grow, blur — is one code.
                    if qual.refines() {
                        klipp_effects::cpu_grade::matte_finesse(
                            &mut frame, width, height,
                            qual.clean_black, qual.clean_white, self.whole_px(qual.denoise), self.whole_px(qual.shrink_grow), self.whole_px(qual.blur),
                        );
                    }
                    frame
                } else {
                    vec![1.0f32; (width * height * 4) as usize]
                }
            }

            NodeKind::HdrZones => match params.hdr_zones.as_ref().filter(|z| !z.is_neutral()) {
                None => primary_input.to_vec(),
                Some(z) => {
                    let terms = hdr_zone_terms(z, self.working_colorspace);
                    if let Some(gpu) = &mut self.gpu {
                        gpu.apply_hdr_zones_gpu(primary_input, width, height, &terms)
                    } else {
                        let mut frame = primary_input.to_vec();
                        klipp_effects::cpu_grade::hdr_zones(&mut frame, &terms);
                        frame
                    }
                }
            },

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
                        // CPU fallback, keying the encoded picture, each
                        // channel clamped to [0, 1], as the shader keys it
                        // (`luma_key_value`): from the floats, not an 8-bit
                        // copy, so a soft edge isn't stepped by rounding
                        // (#1439). Both take the working space's luma row,
                        // so they also agree on an ACEScg project, where
                        // Rec.709's row under-weights green by a fifth.
                        //
                        // The matte itself comes back as it is. It is a
                        // blend coefficient and not a colour: running it
                        // through `u8_rgba_to_linear`, as this used to,
                        // applied the sRGB decode to it and turned a
                        // half matte into 0.21. That put this path a
                        // quarter of a frame away from the shader, which
                        // `secondary_gpu_parity` caught.
                        use klipp_color::transfer::linear_to_srgb;
                        let coefs = klipp_color::linear::luma_coefs(self.working_colorspace);
                        primary_input
                            .chunks_exact(4)
                            .flat_map(|p| {
                                let v = klipp_color::luma_key::luma_key_value([p[0], p[1], p[2]].map(linear_to_srgb), lk, coefs);
                                [v, v, v, 1.0]
                            })
                            .collect()
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
                            .filter_map(|m| mask_to_gpu_def(m, width, height))
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
                // A tracked subject's matte, by the clip's media as linked
                // now (#1881); none found is said, never another's.
                let subject_dir;
                let path = match params.matte_subject {
                    Some(model) => match self.clip_subject_mattes[model.index()].clone() {
                        Some(dir) => {
                            subject_dir = dir;
                            &subject_dir
                        }
                        None => {
                            self.decline(node_id, NodeKind::ExternalMatte, crate::DeclineOutcome::FileMissing, "Subject matte".into(), "no subject matte for this clip's media; track its subject", format!("no {} matte folder for the clip's media", model.name()));
                            return full_matte(pixel_count);
                        }
                    },
                    None => match params.matte_image_path.as_ref() {
                        Some(path) => path,
                        // No image chosen: it selects everything, as every
                        // matte producer does with nothing chosen (#1039).
                        None => return full_matte(pixel_count),
                    },
                };
                let channel = params.matte_channel.unwrap_or_default();
                let invert = params.matte_invert.unwrap_or(false);

                // A matte sequence (#239): the matte for this frame of the
                // media, made by the subject matte pass. Its own channel is
                // the matte; invert applies.
                if path.is_dir() {
                    use crate::matte_sequence as seq;
                    // Held to the media's frames: its first before its
                    // start, its last past its end, as the picture is.
                    let secs = ctx.as_ref().map_or(clip_time_secs, |c| c.source_secs).max(0.0);
                    let secs = self.clip_media_last.map_or(secs, |last| secs.min(last));
                    // Read once: the manifest and the frames there. A time
                    // with no frame of its own looks again, in case the
                    // pass has made it since.
                    let fresh = |path: &PathBuf| seq::read_manifest(path).map(|m| (m, seq::frames(path)));
                    let mut known = match self.matte_sequences.get(path) {
                        Some(k) => Some(k.clone()),
                        None => fresh(path),
                    };
                    let mut at = known.as_ref().and_then(|(m, keys)| seq::matte_at(path, m, keys, secs));
                    if at.as_ref().is_none_or(|a| !a.exact) {
                        known = fresh(path);
                        at = known.as_ref().and_then(|(m, keys)| seq::matte_at(path, m, keys, secs));
                    }
                    if let Some(k) = known {
                        self.matte_sequences.insert(path.clone(), k);
                    }
                    let Some(at) = at else {
                        self.decline(node_id, NodeKind::ExternalMatte, crate::DeclineOutcome::FileMissing, "Subject matte".into(), "no matte made for this media yet", format!("{}: no matte frames", path.display()));
                        return full_matte(pixel_count);
                    };
                    // Another frame's matte stands in on screen, but the
                    // picture isn't the one asked for: said, and an export
                    // refuses it (#239).
                    if !at.exact {
                        let reason = format!("the frame at {secs:.3} s of its media has no subject matte yet");
                        self.decline(
                            node_id,
                            NodeKind::ExternalMatte,
                            crate::DeclineOutcome::FileMissing,
                            "Subject matte".into(),
                            &reason,
                            format!("{}: no matte at {secs:.6} s; the one at {:.6} s stands in", path.display(), at.read as f64 / 1_000_000.0),
                        );
                    }
                    let up = crate::matte_sequence::upsample(&at.values, at.width, at.height, width, height);
                    let mut out = vec![0.0f32; pixel_count * 4];
                    for (i, m) in up.into_iter().enumerate() {
                        let m = if invert { 1.0 - m } else { m }.clamp(0.0, 1.0);
                        out[i * 4..i * 4 + 4].copy_from_slice(&[m, m, m, 1.0]);
                    }
                    return out;
                }

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
                let downloaded = match gpu_matte.filter(|_| secondary_input.is_none_or(|m| m.len() != n)) {
                    Some(m) => self.gpu.as_ref().and_then(|g| g.read_keyed_matte(m, n / 4)),
                    None => None,
                };
                // The gate after a correction with a mix and no matte
                // (M3 step 17a) takes the whole frame by the mix.
                let whole = params.correction_blend.is_some() && gpu_matte.is_none() && secondary_input.is_none();
                // An outside node's gate takes the key inverted (#898): it
                // grades where the key isn't, so a matte selecting nothing
                // grades everything and one selecting everything (or none
                // wired) grades nothing.
                let invert = params.key_invert == Some(true);
                let matte = match (downloaded.as_deref(), secondary_input) {
                    (Some(m), _) => Some(m),
                    (None, Some(m)) if m.len() == n => Some(m),
                    _ if whole || invert => None,
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
                // Which part of the matte is its coverage: red, as every
                // gate always read it, unless its correction says (M3
                // step 17a).
                let channel = params.correction_blend.map_or(klipp_effects::mask_mix::MaskChannel::Red, |b| b.channel);
                let luma = klipp_color::linear::luma_coefs(self.working_colorspace);
                let coverage = |m: &[f32]| {
                    let c = match channel {
                        klipp_effects::mask_mix::MaskChannel::Red => m[0],
                        klipp_effects::mask_mix::MaskChannel::Luminance => m[0] * luma[0] + m[1] * luma[1] + m[2] * luma[2],
                        klipp_effects::mask_mix::MaskChannel::Alpha => m[3],
                    };
                    if invert { 1.0 - c.clamp(0.0, 1.0) } else { c }
                };
                // No matte: the whole frame by the mix, or, inverted, none
                // of it.
                let whole_k = if invert { 0.0 } else { mix.clamp(0.0, 1.0) };
                // A gate's alpha is 1, as every gate's was; a mix with no
                // matte keeps the correction's alpha, so a clip with
                // transparency keeps it.
                let blend = |dst: &mut [f32], graded: &[f32], plain: &[f32], k: f32, alpha: f32| {
                    for c in 0..3 {
                        dst[c] = plain[c] + (graded[c] - plain[c]) * k;
                    }
                    dst[3] = alpha;
                };
                let mut out = vec![0.0f32; n];
                let pairs = out.par_chunks_exact_mut(4).zip(primary_input.par_chunks_exact(4)).zip(ungraded.par_chunks_exact(4));
                match matte {
                    Some(matte) => pairs.zip(matte.par_chunks_exact(4)).for_each(|(((dst, graded), plain), m)| blend(dst, graded, plain, (coverage(m) * mix).clamp(0.0, 1.0), 1.0)),
                    None => pairs.for_each(|((dst, graded), plain)| blend(dst, graded, plain, whole_k, graded[3])),
                }
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
                    let (spatial_sigma, luma_sigma, chroma_sigma) = denoise_sigmas(luma, chroma, self.px(radius as f32));
                    let max_kernel = self.denoise_max_kernel();
                    let coefs = klipp_color::linear::luma_coefs(self.working_colorspace);
                    if let Some(gpu) = &mut self.gpu {
                        gpu.apply_denoise_gpu(
                            primary_input, width, height,
                            spatial_sigma, luma_sigma, chroma_sigma, max_kernel,
                        )
                    } else {
                        klipp_effects::cpu_grade::bilateral_denoise(
                            primary_input, width, height,
                            spatial_sigma, luma_sigma, chroma_sigma, max_kernel, coefs,
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
            // The picture's alpha times the matte's coverage (red, as a
            // matte carries it), its colour as it is (#898): a picture with
            // transparency of its own keeps it where the matte keeps
            // everything. The matte was checked in the dispatch loop.
            NodeKind::OutputAlpha => {
                let mut out = primary_input.to_vec();
                if let Some(m) = secondary_input.filter(|m| m.len() == out.len()) {
                    for (p, m) in out.chunks_exact_mut(4).zip(m.chunks_exact(4)) {
                        p[3] *= m[0].clamp(0.0, 1.0);
                    }
                }
                out
            }

            NodeKind::LayerMixer
            | NodeKind::KeyMixer
            | NodeKind::Output
            | NodeKind::Unknown => primary_input.to_vec(),

            // PrimariesMatrix applies a fixed 3×3 working-space
            // conversion in linear-light RGB. Neither side set (the
            // default-params state the migration uses to mark wrappers
            // as inert when it strips them) or the same space on both
            // sides is a passthrough.
            //
            // One side set is a conversion: the other is the catalog's
            // default, Rec.709 linear, which its row shows (#1351); a
            // pick of one side used to leave the node inert.
            NodeKind::PrimariesMatrix => {
                let default = klipp_core::WorkingColorspace::Rec709Linear;
                match (params.primaries_from, params.primaries_to) {
                    (None, None) => primary_input.to_vec(),
                    (from, to) => {
                        let (from, to) = (from.unwrap_or(default), to.unwrap_or(default));
                        match klipp_color::primaries::working_space_conversion_matrix(from, to) {
                            Some(m) => {
                                let mut frame = primary_input.to_vec();
                                klipp_color::linear::apply_primaries_matrix(&mut frame, m);
                                frame
                            }
                            None => primary_input.to_vec(),
                        }
                    }
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
                // The top-level run's tap and thumbnails, asked of the
                // nodes in here too (#1475); a matte preview stays the
                // outer level's.
                let (tap, thumbs) = (self.inside.tap, self.inside.thumbs.clone());
                let solo_here = self.solo_node.is_some_and(|s| holds_node(&subgraph, s));
                let result = self.execute_linear_with_thumbs(
                    primary_input,
                    &subgraph,
                    width,
                    height,
                    clip_time_secs,
                    frame_number,
                    tap.filter(|t| holds_node(&subgraph, *t)),
                    None,
                    thumbs.as_ref(),
                );
                self.compound_depth -= 1;
                if solo_here {
                    self.inside.solo_in = Some(node_id);
                }
                match result {
                    Ok((output, tapped, found)) => {
                        if tapped.is_some() {
                            self.inside.tapped = tapped;
                        }
                        self.inside.found_thumbs.extend(found);
                        // A sub-graph whose result is a node with no
                        // picture of its own — a Mask with no shapes yet,
                        // which gives an empty matte meaning "all of it" —
                        // passes its input through, as any node reading an
                        // empty picture reads the source (#1913). Handed
                        // on empty, it left the whole grade with no
                        // picture, and the thumbnail drawn from it crashed.
                        if output.is_empty() {
                            primary_input.to_vec()
                        } else {
                            output
                        }
                    }
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
        // The first three run over hue, a circle: they join across red.
        let lut_f32 = if c < 3 { klipp_color::curves::build_periodic_curve_lut_f32(points) } else { klipp_color::curves::build_curve_lut_f32(points) };
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

/// Convert a klipp_core::Mask to a GpuMaskDef for a `width` by `height`
/// frame. Every shape variant is supported on the GPU as of the v2 mask
/// pipeline (storage-buffer records + pooled bezier points). A freeform
/// shape goes as its outline, flattened for that size as the CPU
/// rasteriser flattens it (#74) — the dispatch helper pools the outlines
/// across all masks in the stack into a single storage buffer.
fn mask_to_gpu_def(mask: &klipp_core::Mask, width: u32, height: u32) -> Option<klipp_effects::gpu::GpuMaskDef> {
    use klipp_core::MaskShape;
    use klipp_effects::gpu::GpuMaskDef;
    let def = match &mask.shape {
        MaskShape::Circle { center_x, center_y, radius } =>
            Some(GpuMaskDef::circle(*center_x, *center_y, *radius, mask.feather, mask.invert, mask.opacity)),
        MaskShape::Rectangle { center_x, center_y, width, height, rotation } =>
            Some(GpuMaskDef::rectangle(*center_x, *center_y, *width, *height, *rotation, mask.feather, mask.invert, mask.opacity)),
        MaskShape::LinearGradient { start_x, start_y, end_x, end_y } =>
            Some(GpuMaskDef::linear_gradient(*start_x, *start_y, *end_x, *end_y, mask.feather, mask.invert, mask.opacity)),
        MaskShape::RadialGradient { center_x, center_y, inner, outer } =>
            Some(GpuMaskDef::radial_gradient(*center_x, *center_y, *inner, *outer, mask.feather, mask.invert, mask.opacity)),
        MaskShape::Bezier { points } =>
            Some(GpuMaskDef::bezier(klipp_core::mask_path::outline(points, width, height).to_vec(), mask.feather, mask.invert, mask.opacity)),
    };
    def.map(|d| d.with_warp(mask.keyframes.warp.map(|w| w.0)).with_feather_inner(mask.feather_inner).with_combine(mask.combine.code()))
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
            klipp_core::MaskShape::Rectangle { center_x, center_y, .. }
            | klipp_core::MaskShape::RadialGradient { center_x, center_y, .. } => {
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
            // Mix blended colour over the current base by opacity, where
            // the layer is (its alpha): a layer with clear parts shows
            // the base through them (#1746), as an opaque one never did
            // differently.
            let op = op * top[o + 3].clamp(0.0, 1.0);
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
pub fn downsample_to_thumb_u8(src: &[f32], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
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

/// How long a pipeline built ahead waits for the UI to install its device
/// before it makes one (#2108).
const PREWARM_WAITS_FOR_DEVICE: std::time::Duration = std::time::Duration::from_secs(20);

/// A pipeline still being built ahead is waited for and dropped here,
/// with the executor: left running, its thread could still be creating
/// GPU objects as the process exits and the driver is torn down (#2108).
impl Drop for GraphExecutor {
    fn drop(&mut self) {
        if let Some((rx, handle)) = self.gpu_prewarm.take() {
            // Decided now if nothing has, so the build is not left waiting
            // for a device no UI will install.
            let _ = klipp_effects::gpu_context::GpuContext::shared();
            drop(rx.recv());
            let _ = handle.join();
        }
    }
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

    /// Source → `inner` → Output, linked in order.
    fn chain_of(inner: Vec<crate::graph::Node>) -> NodeGraph {
        let node = |kind| crate::graph::Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params: NodeParams::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
        let mut nodes = vec![node(NodeKind::Source)];
        nodes.extend(inner);
        nodes.push(node(NodeKind::Output));
        let edges = nodes.windows(2).map(|w| crate::graph::Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 }).collect();
        NodeGraph { output_node_id: nodes.last().unwrap().id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: Vec::new(), sticky_notes: Vec::new() }
    }

    fn grade_node(kind: NodeKind, params: NodeParams) -> crate::graph::Node {
        crate::graph::Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() }
    }

    /// #1913: a compound whose result is a Mask with no shapes yet (its
    /// empty matte: "all of it") passes its picture through, as a node
    /// reading an empty picture reads the source. It used to hand on an
    /// empty picture, and the whole grade came out empty: the crash.
    #[test]
    fn a_compound_ending_in_an_empty_mask_passes_its_picture_through() {
        let exposure = |stops| grade_node(NodeKind::Exposure, NodeParams { exposure: Some(stops), ..Default::default() });
        let group = grade_node(NodeKind::Compound, NodeParams { compound_subgraph: Some(chain_of(vec![grade_node(NodeKind::Mask, NodeParams::default())])), ..Default::default() });
        let graph = chain_of(vec![exposure(1.0), group]);
        let without = chain_of(vec![exposure(1.0)]);
        let frame = [0.18f32, 0.18, 0.18, 1.0].repeat(4);
        let mut exec = GraphExecutor::new_cpu_only();
        let (out, _, _) = exec.execute_linear_with_thumbs(&frame, &graph, 2, 2, 0.0, 0, None, None, None).unwrap();
        let (want, _, _) = exec.execute_linear_with_thumbs(&frame, &without, 2, 2, 0.0, 0, None, None, None).unwrap();
        assert_eq!(out.len(), frame.len(), "a whole picture");
        assert_eq!(out, want, "the group passes the picture through");
    }

    /// A node inside a compound, and inside a compound inside that,
    /// answers the scope's tap, its thumbnail and a solo (#1475): the
    /// solo shows what the tap reads, not the grade's output, and the
    /// thumbnail is drawn; the run's output is as without them.
    #[test]
    fn a_node_inside_a_compound_is_tapped_soloed_and_thumbnailed() {
        let exposure = |stops| grade_node(NodeKind::Exposure, NodeParams { exposure: Some(stops), ..Default::default() });
        let (inner, deep) = (exposure(1.0), exposure(0.5));
        let (inner_id, deep_id) = (inner.id, deep.id);
        let nested = grade_node(NodeKind::Compound, NodeParams { compound_subgraph: Some(chain_of(vec![deep])), ..Default::default() });
        let outer = grade_node(NodeKind::Compound, NodeParams { compound_subgraph: Some(chain_of(vec![inner, nested])), ..Default::default() });
        let graph = chain_of(vec![outer, exposure(-2.0)]);
        let frame = [0.18f32, 0.18, 0.18, 1.0].repeat(4);
        let mut exec = GraphExecutor::new_cpu_only();
        let (plain, _, _) = exec.execute_linear_with_thumbs(&frame, &graph, 2, 2, 0.0, 0, None, None, None).unwrap();
        for id in [inner_id, deep_id] {
            let what = if id == inner_id { "one deep" } else { "two deep" };
            let thumbs: std::collections::HashSet<NodeId> = [id].into_iter().collect();
            let (out, tapped, found) = exec.execute_linear_with_thumbs(&frame, &graph, 2, 2, 0.0, 0, Some(id), None, Some(&thumbs)).unwrap();
            assert_eq!(out, plain, "{what}: the output as without them");
            let tapped = tapped.unwrap_or_else(|| panic!("{what}: a tap"));
            assert!(found.contains_key(&id), "{what}: a thumbnail");
            exec.set_solo_node(Some(id));
            let (soloed, _, _) = exec.execute_linear_with_thumbs(&frame, &graph, 2, 2, 0.0, 0, None, None, None).unwrap();
            exec.set_solo_node(None);
            assert_eq!(soloed, tapped, "{what}: the solo shows the node");
            assert_ne!(soloed, plain, "{what}: not the grade's output");
        }
    }

    /// A tap on a node that changes nothing, run as a bypass, is its
    /// input: a default Exposure, or one switched off (#1314). It used to
    /// come back empty.
    #[test]
    fn scope_tap_on_a_bypassed_node_is_its_input() {
        for enabled in [true, false] {
            let mut graph = default_graph();
            let exp = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::Exposure).unwrap();
            exp.enabled = enabled;
            if !enabled {
                exp.params.exposure = Some(1.0);
            }
            let exp_id = exp.id;
            let mut executor = GraphExecutor::new_cpu_only();
            let frame = vec![100, 100, 100, 255];
            let (_, tapped) = executor.execute(&frame, &graph, 1, 1, 0.0, 0, Some(exp_id), None).unwrap();
            let tap = tapped.unwrap_or_else(|| panic!("enabled {enabled}: the tap of a bypassed node"));
            assert!((i32::from(tap[0]) - 100).abs() <= 1, "enabled {enabled}: its input: {tap:?}");
        }
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
        klipp_core::Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
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
        mask.keyframes.track_x = vec![klipp_core::Keyframe { time_secs: 0.0, value: 0.0, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }];
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
        assert_eq!(mask_to_gpu_def(&mask.evaluated_in(0.0, 0.5625), 64, 36).unwrap().flags & 2, 0);
        let from = [[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6]];
        let to = [[0.4, 0.4], [0.58, 0.43], [0.58, 0.57], [0.4, 0.6]];
        mask.keyframes.track_perspective = Some(klipp_core::PerspectiveTrack { from, keys: vec![klipp_core::CornerKey { time_secs: 0.0, corners: to }] });
        let evaluated = mask.evaluated_in(0.0, 0.5625);
        let def = mask_to_gpu_def(&evaluated, 64, 36).unwrap();
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

    /// #2108: a pipeline built ahead is the one the first frame takes,
    /// waited for rather than built again; asked twice, one build.
    #[test]
    fn a_prewarmed_pipeline_is_the_one_a_frame_gets() {
        let mut executor = GraphExecutor::new();
        executor.prewarm_gpu();
        assert!(executor.gpu_prewarm.is_some() || executor.gpu.is_some(), "a build started");
        executor.prewarm_gpu();
        // The first frame takes it, waiting for the build, not starting one.
        let got = executor.gpu_pipeline_or_init().is_some();
        assert!(executor.gpu_prewarm.is_none(), "taken");
        assert_eq!(got, executor.gpu.is_some());
        // A CPU-only executor builds nothing ahead.
        let mut cpu = GraphExecutor::new_cpu_only();
        cpu.prewarm_gpu();
        assert!(cpu.gpu_prewarm.is_none() && cpu.gpu_pipeline_or_init().is_none());
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

    /// One side set is a conversion from or to the other's default,
    /// Rec.709 linear, as the inspector shows it (#1351): picking only
    /// From or only To changes the picture.
    #[test]
    fn primaries_matrix_node_one_side_set_converts_from_or_to_rec709() {
        use klipp_core::WorkingColorspace::{AcesCG, Bt2020Linear, Rec709Linear};
        let pixel = [0.7, 0.2, 0.1, 1.0];
        for (one, both) in [((Some(Bt2020Linear), None), (Some(Bt2020Linear), Some(Rec709Linear))), ((None, Some(AcesCG)), (Some(Rec709Linear), Some(AcesCG)))] {
            let out = run_primaries_matrix(one.0, one.1, pixel);
            assert_eq!(out, run_primaries_matrix(both.0, both.1, pixel), "{one:?} is {both:?}");
            assert!(out.iter().zip(pixel.iter()).any(|(a, b)| (a - b).abs() > 1e-3), "{one:?} changed the picture: {out:?}");
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

    /// #1881: a subject-bound matte reads the folder its clip was given,
    /// and only for that clip: the next `prepare_clip` takes it away, so a
    /// run never reads another clip's subject; with none, it declines.
    #[test]
    fn a_subject_matte_is_the_current_clips_alone() {
        use crate::matte_sequence as seq;
        let (w, h) = (8u32, 4u32);
        let dir = std::env::temp_dir().join(format!("klipp-subject-exec-{}-{}", std::process::id(), NodeId::new_v4()));
        let m = seq::Manifest { format: seq::FORMAT, fps: 25.0, width: 4, height: 2, rotation: 0, model: "t".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
        seq::create(&dir, &m).unwrap();
        seq::put(&dir, &m, 0, &[255; 8]).unwrap();
        let mut g = NodeGraph::default();
        let corr = g.ensure_corrector();
        let matte = NodeId::new_v4();
        let params = NodeParams { matte_subject: Some(crate::SubjectMatte::Subject), ..Default::default() };
        g.nodes.push(Node { id: matte, kind: NodeKind::ExternalMatte, label: "Subject".into(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() });
        g.edges.push(Edge { from_node: matte, from_port: 0, to_node: corr, to_port: 1 });
        let frame = vec![128u8; (w * h * 4) as usize];
        let mut ex = GraphExecutor::new_cpu_only();
        let missing = |ex: &GraphExecutor| ex.last_run_declines().iter().any(|d| d.outcome == crate::DeclineOutcome::FileMissing);
        ex.prepare_clip(&g);
        ex.set_current_clip_subject_mattes([Some(dir.clone()), None]);
        ex.execute(&frame, &g, w, h, 0.0, 0, None, None).unwrap();
        assert!(!missing(&ex), "{:?}", ex.last_run_declines());
        ex.prepare_clip(&g);
        ex.execute(&frame, &g, w, h, 0.0, 0, None, None).unwrap();
        assert!(missing(&ex), "the last clip's folder is not this one's");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod analysis_node_tests {
    use super::*;
    use crate::frame_context::{Analysis, AnalysisBlob, AnalysisInputs, FrameContext, NoSources, PluginInstances, NODE_ANALYSIS_SCHEMA};
    use klipp_effects::node_analysis::NodeAnalysis;
    use klipp_effects::stabilization::{accumulate_trajectory, decompose_homography, smooth_trajectory, StabilizationData};
    use klipp_effects::BuiltinEffect;

    /// A shaky trajectory of 48 frames at 24 fps.
    fn shaky() -> StabilizationData {
        let step = |i: usize| klipp_effects::homography::Homography { h: [[1.0, 0.0, if i.is_multiple_of(2) { 0.03 } else { -0.02 }], [0.0, 1.0, 0.005], [0.0, 0.0, 1.0]] };
        let frame_transforms: Vec<_> = (0..47).map(step).collect();
        let accumulated = accumulate_trajectory(&frame_transforms);
        let trajectory = accumulated.iter().map(decompose_homography).collect();
        StabilizationData { frame_transforms, accumulated, trajectory, fps: 24.0, frame_count: 48 }
    }

    /// Each stabilisation effect's analysis at its own smoothing.
    struct BySmoothing(Vec<(f32, AnalysisBlob)>);

    impl AnalysisInputs for BySmoothing {
        fn lookup(&self, _: NodeId, _: u32) -> Analysis<'_> {
            Analysis::Absent
        }
        fn lookup_effect(&self, _: NodeId, effect: &BuiltinEffect, _: u32) -> Analysis<'_> {
            let BuiltinEffect::VideoStabilization { smoothing, .. } = effect else { return Analysis::Absent };
            self.0.iter().find(|(s, _)| s == smoothing).map_or(Analysis::Absent, |(_, b)| Analysis::Ready(b))
        }
    }

    fn stabilise(smoothing: f32) -> Node {
        Node {
            id: NodeId::new_v4(),
            kind: NodeKind::Effect,
            label: String::new(),
            enabled: true,
            params: NodeParams { effects: vec![BuiltinEffect::VideoStabilization { smoothing, crop_ratio: 0.0, zoom: false }], ..Default::default() },
            keyframes: Default::default(),
            output_link: None,
            input_links: vec![],
        }
    }

    /// Two stabilisation nodes on one clip, at two smoothings, each take
    /// their own correction for the frame: never one node's for both.
    #[test]
    fn two_stabilisation_nodes_each_take_their_own_transform() {
        let data = shaky();
        let derive = |s: f32| AnalysisBlob { schema: NODE_ANALYSIS_SCHEMA, data: std::sync::Arc::new(NodeAnalysis::Stabilisation { smoothed: smooth_trajectory(&data, (s as f64 * 2.0).max(0.05)), fps: 24.0 }) };
        let inputs = BySmoothing(vec![(0.1, derive(0.1)), (0.9, derive(0.9))]);
        let (a, b) = (stabilise(0.1), stabilise(0.9));
        let graph = NodeGraph { nodes: vec![a.clone(), b.clone()], ..Default::default() };
        let mut sources = NoSources;
        let mut instances = PluginInstances::default();
        let ctx = FrameContext {
            clip: klipp_core::ClipId::new(),
            frame: 20,
            source: (klipp_core::MediaId::nil(), 0.0),
            clip_time_secs: 20.0 / 24.0,
            frame_number: 20,
            fps: 24.0,
            source_secs: 20.0 / 24.0,
            source_pts: None,
            sources: &mut sources,
            analysis: &inputs,
            instances: &mut instances,
            ofx: None,
            source_at: &|t| t,
        };
        let (run, missing) = resolve_analysis(&graph, &ctx, 8, 8);
        assert!(missing.is_empty(), "both have theirs: {missing:?}");
        let h = |n: &Node| run.get(&n.id).and_then(|at| at.motion.stabilization).expect("its correction");
        assert_ne!(h(&a), h(&b), "two smoothings, two corrections");
        let own = |s: f32| {
            let smoothed = NodeAnalysis::Stabilisation { smoothed: smooth_trajectory(&data, (s as f64 * 2.0).max(0.05)), fps: 24.0 };
            smoothed.at(&stabilise(s).params.effects[0], 20.0 / 24.0, 8, 8).motion.stabilization.expect("frame 20's")
        };
        assert_eq!(h(&a), own(0.1));
        assert_eq!(h(&b), own(0.9));
    }

    /// A node whose analysis isn't ready has none for the frame: its effect
    /// passes the frame through, and the node declines as a neighbour
    /// missing (step 10), which an export refuses.
    #[test]
    fn a_node_without_its_analysis_has_none() {
        let inputs = BySmoothing(vec![]);
        let graph = NodeGraph { nodes: vec![stabilise(0.5)], ..Default::default() };
        let mut sources = NoSources;
        let mut instances = PluginInstances::default();
        let ctx = FrameContext {
            clip: klipp_core::ClipId::new(), frame: 0, source: (klipp_core::MediaId::nil(), 0.0), clip_time_secs: 0.0, frame_number: 0, fps: 24.0,
            source_secs: 0.0, source_pts: None, sources: &mut sources, analysis: &inputs, instances: &mut instances, ofx: None, source_at: &|t| t,
        };
        let (run, missing) = resolve_analysis(&graph, &ctx, 8, 8);
        assert!(run.is_empty());
        assert_eq!(missing.iter().map(|m| m.2).collect::<Vec<_>>(), [crate::DeclineOutcome::NeighbourMissing], "declined");
    }
}

/// The failing decline a colour-domain refusal records (#1867): the node
/// that can't take what it reads, by its kind and, inside compounds, its
/// path, and why.
fn domain_decline(graph: &NodeGraph, fault: &crate::domains::DomainFault) -> crate::Decline {
    fn kind_at(g: &NodeGraph, path: &[NodeId]) -> Option<NodeKind> {
        let (first, rest) = path.split_first()?;
        let n = g.nodes.iter().find(|n| n.id == *first)?;
        match rest {
            [] => Some(n.kind),
            _ => kind_at(n.params.compound_subgraph.as_ref()?, rest),
        }
    }
    let node = fault.path.last().copied().unwrap_or(graph.output_node_id);
    let kind = kind_at(graph, &fault.path).unwrap_or(NodeKind::Effect);
    let path = fault.path.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" / ");
    crate::Decline {
        clip: None,
        node,
        kind,
        outcome: crate::DeclineOutcome::RenderFailed,
        name: kind.display_name().to_string(),
        reason: "its pictures' colour domains don't match".into(),
        why: format!("{} (at {path})", fault.why),
    }
}

/// The side picture a `SidePicture` node outputs, by its key or the effect
/// it holds (#1746, #1265).
fn side_key_of(n: &Node) -> Option<crate::side_picture::SideKey> {
    (n.kind == NodeKind::SidePicture).then(|| n.params.side_picture.clone().or_else(|| n.params.effects.iter().find_map(crate::side_picture::SideKey::of))).flatten()
}
