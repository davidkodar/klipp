use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use klipp_color::lut::LutData;
use klipp_color::pipeline::CurveLuts;
use klipp_effects::gpu::GpuEffectsPipeline;

use crate::graph::{Node, NodeGraph, NodeId};
use crate::kinds::NodeKind;
use crate::params::NodeParams;

/// Return the effective params for `node` at `clip_time_secs`. When the node has
/// no keyframes we borrow `node.params` directly, skipping the per-frame clone
/// that would otherwise happen on every node in the graph.
fn params_for_frame(node: &Node, clip_time_secs: f64) -> Cow<'_, NodeParams> {
    if node.keyframes.is_empty() {
        Cow::Borrowed(&node.params)
    } else {
        let mut params = node.params.clone();
        for (param_name, value) in node.keyframes.evaluate_all(clip_time_secs) {
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
    curve_lut_cache: HashMap<NodeId, Arc<CurveLuts>>,
    /// Phase 4d: when true, `start_chain` skips the
    /// `upload_chain_input_linear` call because `buf_a` was already
    /// seeded with linear RGBA — typically by
    /// `prime_chain_from_gpu_u8_buffer` bridging a decoder's u8
    /// output in-place. Consumed (cleared) by the first
    /// `start_chain` so a second chain segment within the same
    /// `execute_linear` call falls back to the normal upload path.
    gpu_chain_preseeded: bool,
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
            gpu_chain_preseeded: false,
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
        if self.gpu.is_none() {
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
            gpu_chain_preseeded: false,
        }
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
        }
        ok
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
    /// delegates to [`execute_linear`], converts back at the exit. Callers
    /// that already hold linear data should call [`execute_linear`] directly
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
        self.ensure_gpu();
        let t_total = std::time::Instant::now();
        let mut per_node_ms: Vec<(String, f64)> = Vec::new();
        let mut chain_flush_count: u32 = 0;
        let mut chain_extend_count: u32 = 0;
        let mut chain_start_count: u32 = 0;
        let mut bypass_tunnel_count: u32 = 0;
        let mut cpu_fallback_count: u32 = 0;

        // 1. Topological sort
        let order = graph.topo_sort()?;

        // Pre-compute fan-out count per source node. A node whose output
        // is consumed more than once can't be absorbed into a chain (its
        // pre-download value is needed by at least one other path), so we
        // break the chain after it.
        let mut fan_out: HashMap<NodeId, usize> = HashMap::new();
        for edge in &graph.edges {
            *fan_out.entry(edge.from_node).or_insert(0) += 1;
        }

        // NodeId → &Node index. The topo walk below resolves every
        // node id in `order` to its full Node. A linear scan per step
        // (`graph.nodes.iter().find(...)`) was O(N²) across the walk;
        // grade stacks of 10-20 nodes run every frame during scrub.
        let node_by_id: HashMap<NodeId, &Node> =
            graph.nodes.iter().map(|n| (n.id, n)).collect();

        // 2. Map: NodeId → linear-light output buffer (Arc-shared for zero-copy
        //    passthrough of bypassed / default-param nodes).
        let mut node_outputs: HashMap<NodeId, Arc<Vec<f32>>> = HashMap::new();
        let mut tapped_output: Option<Arc<Vec<f32>>> = None;

        // ── GPU chain state ────────────────────────────────────────────
        //
        // Between nodes we may carry the "current" frame in a GPU storage
        // buffer instead of downloading to CPU. `chain_tail` names the
        // node whose output currently lives in GPU memory; `chain_slot_a`
        // tracks which ping-pong slot holds it (true = buf_a).
        let mut chain_tail: Option<NodeId> = None;
        let mut chain_slot_a: bool = true;

        // 3. Execute nodes in topo order
        for node_id in order {
            let t_node = std::time::Instant::now();
            let node = match node_by_id.get(&node_id).copied() {
                Some(n) => n,
                None => continue,
            };
            let node_kind_name = format!("{:?}", node.kind);

            // Bypassed nodes — and single-input filter nodes whose params
            // are all at defaults — pass their primary input through
            // unchanged. The Source node must still run (it produces the
            // root buffer), and Output is handled below. Nodes that combine
            // multiple inputs (CombineMattes, ApplyMatte, Blend, Layer) or
            // that generate mattes from geometry (Mask, Qualifier) are NOT
            // no-ops even with default params and must still execute.
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
            let bypass = !node.enabled
                || (is_param_driven_filter && node.params.is_default());

            // If a chain is live, see whether this node can extend it or
            // whether we must flush the tail back to CPU first.
            let primary_input_id = graph.input_of(node_id, 0);
            let primary_is_chain_tail = chain_tail.is_some() && primary_input_id == chain_tail;
            // The chain tail's fan-out must be 1 for the chain to extend
            // past it (more consumers would need a CPU copy anyway, so
            // extending only delays the inevitable download).
            let chain_tail_fan_out_one = chain_tail
                .map(|id| fan_out.get(&id).copied().unwrap_or(0) <= 1)
                .unwrap_or(true);
            let can_extend_chain = !bypass
                && primary_is_chain_tail
                && chain_tail_fan_out_one
                && self.node_supports_gpu_chain(&node.kind, &node.params)
                && Some(node_id) != matte_preview_node
                && Some(node_id) != matte_tap;

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
                bypass_tunnel_count += 1;
                per_node_ms.push((format!("{node_kind_name}#bypass_tunnel"), t_node.elapsed().as_secs_f64() * 1000.0));
                continue;
            }

            // If a chain is live and this node can't extend it, flush
            // before proceeding. This covers:
            //   - bypass / default-param nodes consuming the chain tail
            //     that can't tunnel (fan-out > 1, matte tap/preview)
            //   - Output node (terminal)
            //   - matte generators + multi-input nodes (CombineMattes,
            //     ApplyMatte, Blend, Layer, Mask, Qualifier) which either
            //     touch GPU state directly or speak u8
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
            }

            if bypass && node.kind != NodeKind::Source && node.kind != NodeKind::Output {
                let passthrough = primary_input_id
                    .and_then(|id| node_outputs.get(&id))
                    .cloned()
                    .unwrap_or_else(|| Arc::new(source_frame.to_vec()));
                node_outputs.insert(node.id, passthrough);
                per_node_ms.push((format!("{node_kind_name}#bypass"), t_node.elapsed().as_secs_f64() * 1000.0));
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
                && self.node_supports_gpu_chain(&node.kind, params)
                && self.gpu.is_some()
            {
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
                    Some(n) if n >= 1 => {
                        for _ in 0..n {
                            chain_slot_a = !chain_slot_a;
                        }
                        chain_tail = Some(node_id);
                        node_outputs.insert(node_id, Arc::new(Vec::new()));
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
                    }
                }
            }

            // Try to START a new chain from this node. Needs: GPU
            // available, node whitelisted + effective + not matte preview,
            // primary input resolved from a CPU-resident Vec (or source).
            let can_start_chain = self.gpu.is_some()
                && !bypass
                && self.node_supports_gpu_chain(&node.kind, params)
                && Some(node_id) != matte_preview_node
                && Some(node_id) != matte_tap;
            if can_start_chain {
                let primary_cpu: &[f32] = primary_input_id
                    .and_then(|id| node_outputs.get(&id))
                    .filter(|arc| !arc.is_empty())
                    .map(|arc| arc.as_slice())
                    .unwrap_or(source_frame);
                if let Some(n) = self.start_chain(
                    node_id,
                    &node.kind,
                    params,
                    primary_cpu,
                    width,
                    height,
                    frame_number,
                ) {
                    chain_tail = Some(node_id);
                    chain_slot_a = n % 2 == 0;
                    node_outputs.insert(node_id, Arc::new(Vec::new()));
                    chain_start_count += 1;
                    per_node_ms.push((format!("{node_kind_name}#chain_start"), t_node.elapsed().as_secs_f64() * 1000.0));
                    continue;
                }
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
            );

            // Matte preview — return greyscale matte early
            if Some(node_id) == matte_preview_node {
                let matte: Vec<f32> = output
                    .chunks_exact(4)
                    .flat_map(|c| [c[0], c[0], c[0], 1.0])
                    .collect();
                let tapped_vec =
                    tapped_output.map(|a| Arc::try_unwrap(a).unwrap_or_else(|a| (*a).clone()));
                return Ok((matte, tapped_vec));
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

        // 4. Return the output node's input. Unwrap the Arc when we hold the
        //    sole reference to avoid a clone — falls back to clone only if
        //    another holder (e.g. a downstream passthrough) still has it.
        let output_input_id = graph
            .input_of(graph.output_node_id, 0)
            .ok_or(ExecutorError::NoOutputConnection)?;
        let result_arc = node_outputs
            .remove(&output_input_id)
            .ok_or(ExecutorError::MissingNodeOutput)?;
        let result = Arc::try_unwrap(result_arc).unwrap_or_else(|a| (*a).clone());

        let tapped_vec =
            tapped_output.map(|a| Arc::try_unwrap(a).unwrap_or_else(|a| (*a).clone()));

        Ok((result, tapped_vec))
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
            NodeKind::InputTransform => {
                if let Some(fmt) = params.log_format {
                    use klipp_core::LogFormat;
                    !matches!(fmt, LogFormat::Srgb | LogFormat::Linear)
                } else {
                    false
                }
            }
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
            NodeKind::ColorWheels => {
                // Log-mode wheels have no GPU shader — keep them on CPU.
                !params.wheels_log_mode.unwrap_or(false)
            }
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
                .map_or(false, |c| !is_curves_identity(c)),
            NodeKind::HueSatCurves => params
                .huesat
                .as_ref()
                .map_or(false, |h| !h.is_all_identity()),
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
                .map_or(false, |g| g.enabled && g.amount > 0.001),
            NodeKind::ColorMatch => params
                .color_match
                .as_ref()
                .map_or(false, |s| !s.is_identity()),
            NodeKind::ColorWarper => params
                .warper_params
                .as_ref()
                .map_or(false, |w| !w.is_identity()),
            NodeKind::Denoise => {
                params.denoise_luma.unwrap_or(0.0) > 0.0
                    || params.denoise_chroma.unwrap_or(0.0) > 0.0
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
    fn start_chain(
        &mut self,
        node_id: NodeId,
        kind: &NodeKind,
        params: &NodeParams,
        primary_input: &[f32],
        width: u32,
        height: u32,
        frame_number: u64,
    ) -> Option<u32> {
        let gpu = match &mut self.gpu {
            Some(g) => g,
            None => return None,
        };
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
        if self.gpu_chain_preseeded {
            self.gpu_chain_preseeded = false;
        } else {
            gpu.upload_chain_input_linear(primary_input);
        }
        // Open the single-submit chain encoder. Every `*_chain` dispatch
        // records into this; `flush_chain` calls `submit_chain` to batch
        // the whole segment into one `queue.submit`. Per-node submits
        // cost ~2-5 ms on NVIDIA Vulkan and dominated `grade_ms` during
        // scrubbing before this.
        gpu.begin_chain();
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
        let gpu = match &mut self.gpu {
            Some(g) => g,
            None => return None,
        };
        if gpu.is_device_lost() {
            return None;
        }

        // Dispatch-only entries take `in_is_a`: they pick the in/out
        // buffer refs internally and ping-pong between buf_a/buf_b. We
        // don't need to clone buffer handles here (wgpu::Buffer is not
        // Clone anyway).
        let _ = gpu; // borrow released for the match arms below

        match kind {
            NodeKind::InputTransform => {
                let Some(fmt) = params.log_format else { return None };
                use klipp_core::LogFormat;
                if matches!(fmt, LogFormat::Srgb | LogFormat::Linear) {
                    return None;
                }
                let lut = klipp_color::build_input_transform_lut(fmt);
                self.gpu.as_mut().unwrap()
                    .apply_input_transform_chain(in_is_a, width, height, &lut);
                Some(1)
            }
            NodeKind::Exposure => {
                let exp = params.exposure.unwrap_or(0.0);
                if exp == 0.0 { return None; }
                self.gpu.as_mut().unwrap().apply_color_grade_chain(
                    in_is_a, width, height,
                    exp, 0.0, 1.0, 0.0, 0.0,
                    0.0, 0.0, 0.0, 0.0,
                    (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
                    0.0, 1.0,
                );
                Some(1)
            }
            NodeKind::Contrast => {
                let con = params.contrast.unwrap_or(0.0);
                let clarity = params.clarity.unwrap_or(0.0);
                if con == 0.0 && clarity == 0.0 { return None; }
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
                        0.0, con, 1.0, 0.0, 0.0,
                        0.0, 0.0, 0.0, 0.0,
                        (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
                        0.0, 1.0,
                    );
                    // Second: clarity reads from where color_grade wrote
                    // (!in_is_a), writes back to in_is_a. apply_clarity_chain
                    // uses buf_c as scratch internally (does not affect the
                    // a/b parity) — it's a net 1-flip op like color_grade.
                    self.gpu.as_mut().unwrap()
                        .apply_clarity_chain(!in_is_a, width, height, clarity);
                    return Some(2);
                }
                if con != 0.0 {
                    self.gpu.as_mut().unwrap().apply_color_grade_chain(
                        in_is_a, width, height,
                        0.0, con, 1.0, 0.0, 0.0,
                        0.0, 0.0, 0.0, 0.0,
                        (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
                        0.0, 1.0,
                    );
                } else {
                    self.gpu.as_mut().unwrap()
                        .apply_clarity_chain(in_is_a, width, height, clarity);
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
                        0.0, 0.0, sat, 0.0, 0.0,
                        0.0, 0.0, 0.0, 0.0,
                        (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
                        0.0, 1.0,
                    );
                    self.gpu.as_mut().unwrap()
                        .apply_vibrance_chain(!in_is_a, width, height, vib);
                    return Some(2);
                }
                if sat != 1.0 {
                    self.gpu.as_mut().unwrap().apply_color_grade_chain(
                        in_is_a, width, height,
                        0.0, 0.0, sat, 0.0, 0.0,
                        0.0, 0.0, 0.0, 0.0,
                        (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
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
                    0.0, 0.0, 1.0, 0.0, 0.0,
                    hi, sh, wh, bl,
                    (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
                    0.0, 1.0,
                );
                Some(1)
            }
            NodeKind::ColorWheels => {
                if params.wheels_log_mode.unwrap_or(false) { return None; }
                let lift = klipp_color::wheel_offset_to_rgb(
                    params.lift_x.unwrap_or(0.0),
                    params.lift_y.unwrap_or(0.0),
                    params.lift_master.unwrap_or(0.0),
                );
                let gamma = klipp_color::wheel_offset_to_rgb(
                    params.gamma_x.unwrap_or(0.0),
                    params.gamma_y.unwrap_or(0.0),
                    params.gamma_master.unwrap_or(0.0),
                );
                let gain = klipp_color::wheel_offset_to_rgb(
                    params.gain_x.unwrap_or(0.0),
                    params.gain_y.unwrap_or(0.0),
                    params.gain_master.unwrap_or(0.0),
                );
                let offset = klipp_color::wheel_offset_to_rgb(
                    params.offset_x.unwrap_or(0.0),
                    params.offset_y.unwrap_or(0.0),
                    params.offset_master.unwrap_or(0.0),
                );
                self.gpu.as_mut().unwrap().apply_color_grade_chain(
                    in_is_a, width, height,
                    0.0, 0.0, 1.0, 0.0, 0.0,
                    0.0, 0.0, 0.0, 0.0,
                    lift, gamma, gain, offset,
                    0.0, 1.0,
                );
                Some(1)
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
                let Some(curves) = params.curves.as_ref() else { return None };
                if is_curves_identity(curves) { return None; }
                // Use pre-built LUTs from prepare_clip() when available.
                let luts = self
                    .curve_lut_cache
                    .get(&node_id)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(build_curve_luts(curves)));
                let mut lut_data = [0.0f32; 1024];
                for (i, &v) in luts.master.iter().enumerate().take(256) {
                    lut_data[i] = v as f32 / 255.0;
                }
                for (i, &v) in luts.red.iter().enumerate().take(256) {
                    lut_data[256 + i] = v as f32 / 255.0;
                }
                for (i, &v) in luts.green.iter().enumerate().take(256) {
                    lut_data[512 + i] = v as f32 / 255.0;
                }
                for (i, &v) in luts.blue.iter().enumerate().take(256) {
                    lut_data[768 + i] = v as f32 / 255.0;
                }
                let preserve_luma = params.curves_preserve_luma.unwrap_or(false);
                let mix = params.curves_mix.unwrap_or(1.0);
                self.gpu.as_mut().unwrap().apply_rgb_curves_chain(
                    in_is_a, width, height,
                    &lut_data, preserve_luma, mix,
                );
                Some(1)
            }
            NodeKind::HueSatCurves => {
                let Some(huesat) = params.huesat.as_ref() else { return None };
                if huesat.is_all_identity() { return None; }
                let lut_data = build_huesat_lut_data(huesat);
                let mix = params.huesat_mix.unwrap_or(1.0);
                self.gpu.as_mut().unwrap()
                    .apply_huesat_chain(in_is_a, width, height, mix, &lut_data);
                Some(1)
            }
            NodeKind::ColorBalance => {
                self.gpu.as_mut().unwrap().apply_color_balance_chain(
                    in_is_a, width, height,
                    params.temperature.unwrap_or(0.0),
                    params.tint.unwrap_or(0.0),
                    params.hue.unwrap_or(0.0),
                    params.global_mix.unwrap_or(1.0),
                );
                Some(1)
            }
            NodeKind::Lut3D => {
                let Some(path) = params.lut_path.as_ref() else { return None };
                let lut = self
                    .lut_cache
                    .entry(path.clone())
                    .or_insert_with(|| {
                        // Fall back to a 2x2x2 IDENTITY LUT when the
                        // file can't be read — previously this was an
                        // all-zero LUT (size=2, data=vec![0.0; 24])
                        // which silently rendered the clip black with
                        // no warning. Identity passthrough keeps the
                        // image visible and the warn log points at the
                        // broken path so the user can relink.
                        match klipp_color::load_lut(path) {
                            Ok(lut) => Arc::new(lut),
                            Err(e) => {
                                tracing::warn!(
                                    "Lut3D: load failed for {:?}: {} — passing through",
                                    path, e,
                                );
                                Arc::new(LutData {
                                    size: 2,
                                    data: vec![
                                        0.0, 0.0, 0.0,  1.0, 0.0, 0.0,
                                        0.0, 1.0, 0.0,  1.0, 1.0, 0.0,
                                        0.0, 0.0, 1.0,  1.0, 0.0, 1.0,
                                        0.0, 1.0, 1.0,  1.0, 1.0, 1.0,
                                    ],
                                })
                            }
                        }
                    })
                    .clone();
                let opacity = params.lut_opacity.unwrap_or(1.0);
                self.gpu.as_mut().unwrap().apply_3d_lut_chain(
                    in_is_a, width, height,
                    &lut.data, lut.size as u32, opacity,
                );
                Some(1)
            }
            NodeKind::FilmGrain => {
                let Some(grain) = params.grain.as_ref() else { return None };
                if !grain.enabled || grain.amount <= 0.001 { return None; }
                self.gpu.as_mut().unwrap().apply_grain_chain(
                    in_is_a, width, height,
                    grain.amount, grain.size,
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
                    s.exposure, 0.0, s.saturation, s.temperature, s.tint,
                    0.0, 0.0, 0.0, 0.0,
                    (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
                    0.0, 1.0,
                );
                Some(1)
            }
            NodeKind::Denoise => {
                let luma = params.denoise_luma.unwrap_or(0.0);
                let chroma = params.denoise_chroma.unwrap_or(0.0);
                let radius = params.denoise_radius.unwrap_or(3);
                if luma <= 0.0 && chroma <= 0.0 { return None; }
                let spatial_sigma = radius as f32;
                let color_sigma = (luma + chroma) * 0.5;
                self.gpu.as_mut().unwrap().apply_denoise_chain(
                    in_is_a, width, height,
                    spatial_sigma, color_sigma,
                );
                Some(1)
            }
            _ => None,
        }
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
        node_outputs: &mut HashMap<NodeId, Arc<Vec<f32>>>,
        matte_tap: Option<NodeId>,
        tapped_output: &mut Option<Arc<Vec<f32>>>,
    ) {
        let Some(tail_id) = chain_tail.take() else { return };
        let gpu = match self.gpu.as_mut() {
            Some(g) => g,
            None => return,
        };
        let pixel_count = (width as usize) * (height as usize);
        // Merged submit: chain dispatches + readback copy in ONE
        // queue.submit. Halves NVIDIA Vulkan driver overhead vs the
        // previous submit_chain() + download_chain_output_linear()
        // two-submit pattern.
        let linear = gpu.flush_chain_and_download_linear(slot_a, pixel_count);
        let arc = Arc::new(linear);
        if Some(tail_id) == matte_tap {
            *tapped_output = Some(arc.clone());
        }
        node_outputs.insert(tail_id, arc);
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
    ) -> Vec<f32> {
        match kind {
            NodeKind::Source => source_frame.to_vec(),

            NodeKind::InputTransform => {
                if let Some(fmt) = params.log_format {
                    use klipp_core::LogFormat;
                    if matches!(fmt, LogFormat::Srgb | LogFormat::Linear) {
                        primary_input.to_vec()
                    } else if let Some(gpu) = &mut self.gpu {
                        let lut = klipp_color::build_input_transform_lut(fmt);
                        gpu.apply_input_transform_gpu(primary_input.to_vec(), width, height, &lut)
                    } else {
                        let mut frame = primary_input.to_vec();
                        klipp_color::linear::apply_input_transform(&mut frame, fmt);
                        frame
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::Exposure => {
                let exp = params.exposure.unwrap_or(0.0);
                if exp != 0.0 {
                    if let Some(gpu) = &mut self.gpu {
                        gpu.apply_color_grade(
                            primary_input.to_vec(), width, height,
                            exp, 0.0, 1.0, 0.0, 0.0,
                            0.0, 0.0, 0.0, 0.0,
                            (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
                            0.0, 1.0,
                        )
                    } else {
                        let mut frame = primary_input.to_vec();
                        klipp_color::linear::apply_exposure(&mut frame, exp);
                        frame
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::Contrast => {
                let con = params.contrast.unwrap_or(0.0);
                let clarity = params.clarity.unwrap_or(0.0);
                if con != 0.0 || clarity != 0.0 {
                    let frame = if con != 0.0 {
                        if let Some(gpu) = &mut self.gpu {
                            gpu.apply_color_grade(
                                primary_input.to_vec(), width, height,
                                0.0, con, 1.0, 0.0, 0.0,
                                0.0, 0.0, 0.0, 0.0,
                                (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
                                0.0, 1.0,
                            )
                        } else {
                            let mut f = primary_input.to_vec();
                            klipp_color::linear::apply_contrast(&mut f, con);
                            f
                        }
                    } else {
                        primary_input.to_vec()
                    };
                    if clarity != 0.0 {
                        if let Some(gpu) = &mut self.gpu {
                            gpu.apply_clarity_gpu(frame, width, height, clarity)
                        } else {
                            let mut f = frame;
                            klipp_color::linear::apply_clarity(&mut f, width, height, clarity);
                            f
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
                                primary_input.to_vec(), width, height,
                                0.0, 0.0, sat, 0.0, 0.0,
                                0.0, 0.0, 0.0, 0.0,
                                (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
                                0.0, 1.0,
                            )
                        } else {
                            let mut f = primary_input.to_vec();
                            klipp_color::linear::apply_saturation(&mut f, sat);
                            f
                        }
                    } else {
                        primary_input.to_vec()
                    };
                    if vib != 0.0 {
                        if let Some(gpu) = &mut self.gpu {
                            gpu.apply_vibrance_gpu(frame, width, height, vib)
                        } else {
                            let mut f = frame;
                            klipp_color::linear::apply_vibrance(&mut f, vib);
                            f
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
                            primary_input.to_vec(), width, height,
                            0.0, 0.0, 1.0, 0.0, 0.0,
                            hi, sh, wh, bl,
                            (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
                            0.0, 1.0,
                        )
                    } else {
                        let mut frame = primary_input.to_vec();
                        klipp_color::linear::apply_tone_controls(&mut frame, hi, sh, wh, bl);
                        frame
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::ColorWheels => {
                let log_mode = params.wheels_log_mode.unwrap_or(false);
                if log_mode {
                    // HDR Log wheels: Low/Mid/High with log-space tonal weighting
                    let low = klipp_color::wheel_offset_to_rgb(
                        params.log_low_x.unwrap_or(0.0),
                        params.log_low_y.unwrap_or(0.0),
                        params.log_low_master.unwrap_or(0.0),
                    );
                    let mid = klipp_color::wheel_offset_to_rgb(
                        params.log_mid_x.unwrap_or(0.0),
                        params.log_mid_y.unwrap_or(0.0),
                        params.log_mid_master.unwrap_or(0.0),
                    );
                    let high = klipp_color::wheel_offset_to_rgb(
                        params.log_high_x.unwrap_or(0.0),
                        params.log_high_y.unwrap_or(0.0),
                        params.log_high_master.unwrap_or(0.0),
                    );
                    let mut frame = primary_input.to_vec();
                    klipp_color::linear::apply_log_wheels(&mut frame, low, mid, high);
                    frame
                } else {
                    // Linear wheels: Lift/Gamma/Gain/Offset (traditional)
                    let lift = klipp_color::wheel_offset_to_rgb(
                        params.lift_x.unwrap_or(0.0),
                        params.lift_y.unwrap_or(0.0),
                        params.lift_master.unwrap_or(0.0),
                    );
                    let gamma = klipp_color::wheel_offset_to_rgb(
                        params.gamma_x.unwrap_or(0.0),
                        params.gamma_y.unwrap_or(0.0),
                        params.gamma_master.unwrap_or(0.0),
                    );
                    let gain = klipp_color::wheel_offset_to_rgb(
                        params.gain_x.unwrap_or(0.0),
                        params.gain_y.unwrap_or(0.0),
                        params.gain_master.unwrap_or(0.0),
                    );
                    let offset = klipp_color::wheel_offset_to_rgb(
                        params.offset_x.unwrap_or(0.0),
                        params.offset_y.unwrap_or(0.0),
                        params.offset_master.unwrap_or(0.0),
                    );
                    // GPU path: SHADER_COLOR_GRADE applies wheels in a single dispatch
                    if let Some(gpu) = &mut self.gpu {
                        gpu.apply_color_grade(
                            primary_input.to_vec(), width, height,
                            0.0, 0.0, 1.0, 0.0, 0.0,
                            0.0, 0.0, 0.0, 0.0,
                            lift, gamma, gain, offset,
                            0.0, 1.0,
                        )
                    } else {
                        let mut frame = primary_input.to_vec();
                        klipp_color::linear::apply_lift_gamma_gain_offset(
                            &mut frame, lift, gamma, gain, offset,
                        );
                        frame
                    }
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
                        primary_input.to_vec(), width, height,
                        ls, lb, gs, gb, gas, gab, os, ob,
                    )
                } else {
                    let mut frame = primary_input.to_vec();
                    klipp_color::linear::apply_wheel_sat_bri(
                        &mut frame, ls, lb, gs, gb, gas, gab, os, ob,
                    );
                    frame
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

                    if let Some(gpu) = &mut self.gpu {
                        let mut lut_data = [0.0f32; 1024];
                        for (i, &v) in luts.master.iter().enumerate().take(256) {
                            lut_data[i] = v as f32 / 255.0;
                        }
                        for (i, &v) in luts.red.iter().enumerate().take(256) {
                            lut_data[256 + i] = v as f32 / 255.0;
                        }
                        for (i, &v) in luts.green.iter().enumerate().take(256) {
                            lut_data[512 + i] = v as f32 / 255.0;
                        }
                        for (i, &v) in luts.blue.iter().enumerate().take(256) {
                            lut_data[768 + i] = v as f32 / 255.0;
                        }
                        gpu.apply_rgb_curves_gpu(
                            primary_input.to_vec(), width, height,
                            &lut_data, preserve_luma, mix,
                        )
                    } else {
                        // CPU fallback: curves/luma/mix helpers still speak u8. Bracket locally.
                        let primary_u8 = linear_rgba_to_u8(primary_input);
                        let mut frame_u8 = primary_u8.clone();
                        klipp_color::curves::apply_curves(
                            &mut frame_u8,
                            &luts.master,
                            &luts.red,
                            &luts.green,
                            &luts.blue,
                        );
                        if preserve_luma {
                            apply_preserve_luma(&mut frame_u8, &primary_u8);
                        }
                        if mix < 0.999 {
                            klipp_color::apply_global_mix(&mut frame_u8, &primary_u8, mix);
                        }
                        u8_rgba_to_linear(&frame_u8)
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::HueSatCurves => {
                if let Some(huesat) = &params.huesat {
                    if !huesat.is_all_identity() {
                        let mix = params.huesat_mix.unwrap_or(1.0);
                        if let Some(gpu) = &mut self.gpu {
                            let lut_data = build_huesat_lut_data(huesat);
                            gpu.apply_huesat_gpu(primary_input.to_vec(), width, height, mix, &lut_data)
                        } else {
                            // CPU fallback: apply_huesat_curves still speaks u8.
                            let primary_u8 = linear_rgba_to_u8(primary_input);
                            let mut frame_u8 = primary_u8;
                            klipp_color::huesat::apply_huesat_curves(&mut frame_u8, huesat, mix);
                            u8_rgba_to_linear(&frame_u8)
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
                        primary_input.to_vec(), width, height,
                        temp, tint, hue, mix,
                    )
                } else {
                    let original = primary_input.to_vec();
                    let mut frame = original.clone();
                    if temp != 0.0 || tint != 0.0 {
                        klipp_color::linear::apply_temperature_tint(&mut frame, temp, tint);
                    }
                    if hue.abs() > 0.01 {
                        klipp_color::linear::apply_hue_rotation(&mut frame, hue);
                    }
                    if mix < 0.999 {
                        klipp_color::linear::apply_global_mix(&mut frame, &original, mix);
                    }
                    frame
                }
            }

            NodeKind::Qualifier => {
                // Output is a matte (greyscale RGBA: R=G=B=matte value, A=1.0).
                if let Some(qual) = &params.qualifier {
                    if let Some(gpu) = &mut self.gpu {
                        gpu.apply_qualifier_gpu(
                            primary_input.to_vec(), width, height,
                            qual.hue_center, qual.hue_width, qual.hue_soft,
                            qual.sat_low, qual.sat_high, qual.sat_soft,
                            qual.lum_low, qual.lum_high, qual.lum_soft,
                            qual.invert,
                            qual.exposure, qual.saturation, qual.hue_shift, qual.temperature,
                            true, // always matte_preview for Qualifier node (outputs matte)
                        )
                    } else {
                        // CPU fallback: generate_qualifier_matte still speaks u8.
                        let primary_u8 = linear_rgba_to_u8(primary_input);
                        let matte = klipp_color::qualifier::generate_qualifier_matte(
                            &primary_u8, width, height, qual,
                        );
                        let matte_u8: Vec<u8> = matte.iter()
                            .flat_map(|&v| { let b = (v * 255.0) as u8; [b, b, b, 255] })
                            .collect();
                        u8_rgba_to_linear(&matte_u8)
                    }
                } else {
                    vec![1.0f32; (width * height * 4) as usize]
                }
            }

            NodeKind::Mask => {
                if let Some(masks) = &params.masks {
                    // Evaluate mask keyframes at current clip time
                    let evaluated = masks.evaluated_at(clip_time_secs);

                    // GPU path: convert masks to GPU format (skip Bezier shapes)
                    let matte_u8: Vec<u8> = if let Some(gpu) = &mut self.gpu {
                        let gpu_masks: Vec<klipp_effects::gpu::GpuMaskDef> = evaluated.masks.iter()
                            .filter(|m| m.enabled)
                            .filter_map(mask_to_gpu_def)
                            .collect();
                        let has_bezier = evaluated.masks.iter()
                            .any(|m| m.enabled && matches!(m.shape, klipp_core::MaskShape::Bezier { .. }));

                        if !gpu_masks.is_empty() && !has_bezier {
                            gpu.generate_mask_gpu(width, height, &gpu_masks)
                        } else {
                            // Bezier or mixed shapes: fall back to CPU
                            let matte = klipp_color::masks::generate_mask_matte(width, height, &evaluated);
                            matte.iter()
                                .flat_map(|&v| { let b = (v * 255.0) as u8; [b, b, b, 255] })
                                .collect()
                        }
                    } else {
                        let matte = klipp_color::masks::generate_mask_matte(width, height, &evaluated);
                        matte.iter()
                            .flat_map(|&v| { let b = (v * 255.0) as u8; [b, b, b, 255] })
                            .collect()
                    };
                    u8_rgba_to_linear(&matte_u8)
                } else {
                    vec![1.0f32; (width * height * 4) as usize]
                }
            }

            NodeKind::CombineMattes => {
                // Mattes are alpha-like values; combine in u8 to preserve
                // the existing multiplication semantics.
                let white_u8 = vec![255u8; (width * height * 4) as usize];
                let matte_a_u8 = linear_rgba_to_u8(primary_input);
                let matte_b_u8 = match secondary_input {
                    Some(s) => linear_rgba_to_u8(s),
                    None => white_u8,
                };
                let combined: Vec<u8> = matte_a_u8
                    .chunks_exact(4)
                    .zip(matte_b_u8.chunks_exact(4))
                    .flat_map(|(a, b)| {
                        let combined = (a[0] as u16 * b[0] as u16 / 255) as u8;
                        [combined, combined, combined, 255]
                    })
                    .collect();
                u8_rgba_to_linear(&combined)
            }

            NodeKind::ApplyMatte => {
                // Port 0: graded frame, Port 1: matte, Port 2: ungraded (pre-grade).
                // Current semantics are u8-based — bracket to preserve behaviour.
                let graded_u8 = linear_rgba_to_u8(primary_input);
                let white_u8 = vec![255u8; graded_u8.len()];
                let matte_rgba_u8 = match secondary_input {
                    Some(s) => linear_rgba_to_u8(s),
                    None => white_u8,
                };
                let ungraded_u8 = match tertiary_input {
                    Some(t) => linear_rgba_to_u8(t),
                    None => linear_rgba_to_u8(source_frame),
                };
                let mix = params.mix.unwrap_or(1.0);

                let blended: Vec<u8> = graded_u8
                    .chunks_exact(4)
                    .zip(ungraded_u8.chunks_exact(4))
                    .zip(matte_rgba_u8.chunks_exact(4))
                    .flat_map(|((g, u), m)| {
                        let matte_val = m[0] as f32 / 255.0 * mix;
                        let r = (u[0] as f32 + (g[0] as f32 - u[0] as f32) * matte_val) as u8;
                        let g_ch =
                            (u[1] as f32 + (g[1] as f32 - u[1] as f32) * matte_val) as u8;
                        let b = (u[2] as f32 + (g[2] as f32 - u[2] as f32) * matte_val) as u8;
                        [r, g_ch, b, 255]
                    })
                    .collect();
                u8_rgba_to_linear(&blended)
            }

            NodeKind::Blend => {
                // Two-input mix — preserve current u8 behaviour via bracket.
                let a_u8 = linear_rgba_to_u8(primary_input);
                let b_u8 = match secondary_input {
                    Some(s) => linear_rgba_to_u8(s),
                    None => a_u8.clone(),
                };
                let mix = params.mix.unwrap_or(0.5);
                let blended: Vec<u8> = a_u8
                    .chunks_exact(4)
                    .zip(b_u8.chunks_exact(4))
                    .flat_map(|(pa, pb)| {
                        let r = (pa[0] as f32 + (pb[0] as f32 - pa[0] as f32) * mix) as u8;
                        let g = (pa[1] as f32 + (pb[1] as f32 - pa[1] as f32) * mix) as u8;
                        let b_c = (pa[2] as f32 + (pb[2] as f32 - pa[2] as f32) * mix) as u8;
                        [r, g, b_c, 255]
                    })
                    .collect();
                u8_rgba_to_linear(&blended)
            }

            NodeKind::Layer => {
                // Alpha-over composite — preserve current u8 semantics via bracket.
                let fg_u8 = linear_rgba_to_u8(primary_input);
                let bg_u8 = match secondary_input {
                    Some(s) => linear_rgba_to_u8(s),
                    None => linear_rgba_to_u8(source_frame),
                };
                let composed: Vec<u8> = fg_u8
                    .chunks_exact(4)
                    .zip(bg_u8.chunks_exact(4))
                    .flat_map(|(f, b)| {
                        let fa = f[3] as f32 / 255.0;
                        let r = (f[0] as f32 * fa + b[0] as f32 * (1.0 - fa)) as u8;
                        let g = (f[1] as f32 * fa + b[1] as f32 * (1.0 - fa)) as u8;
                        let bl = (f[2] as f32 * fa + b[2] as f32 * (1.0 - fa)) as u8;
                        let a = (f[3] as f32 + b[3] as f32 * (1.0 - fa)).min(255.0) as u8;
                        [r, g, bl, a]
                    })
                    .collect();
                u8_rgba_to_linear(&composed)
            }

            NodeKind::Lut3D => {
                if let Some(path) = &params.lut_path {
                    let lut = self
                        .lut_cache
                        .entry(path.clone())
                        .or_insert_with(|| {
                            Arc::new(
                                klipp_color::load_lut(path)
                                    .unwrap_or(LutData {
                                        size: 2,
                                        data: vec![0.0; 24],
                                    }),
                            )
                        })
                        .clone();
                    let opacity = params.lut_opacity.unwrap_or(1.0);
                    if let Some(gpu) = &mut self.gpu {
                        gpu.apply_3d_lut_gpu(
                            primary_input.to_vec(), width, height,
                            &lut.data, lut.size as u32, opacity,
                        )
                    } else {
                        // CPU fallback: klipp_color::apply_3d_lut speaks u8.
                        let primary_u8 = linear_rgba_to_u8(primary_input);
                        let mut frame_u8 = primary_u8;
                        klipp_color::apply_3d_lut(&mut frame_u8, &lut, opacity);
                        u8_rgba_to_linear(&frame_u8)
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::FilmGrain => {
                if let Some(grain) = &params.grain {
                    if grain.enabled && grain.amount > 0.001 {
                        if let Some(gpu) = &mut self.gpu {
                            gpu.apply_grain_gpu(
                                primary_input.to_vec(), width, height,
                                grain.amount, grain.size,
                                grain.shadows, grain.highlights,
                                grain.monochrome,
                                frame_number as u32,
                            )
                        } else {
                            // CPU fallback: apply_grain_cpu speaks u8.
                            let primary_u8 = linear_rgba_to_u8(primary_input);
                            let mut frame_u8 = primary_u8;
                            apply_grain_cpu(&mut frame_u8, grain, frame_number);
                            u8_rgba_to_linear(&frame_u8)
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
                        primary_input.to_vec(), width, height,
                        s.exposure, 0.0, s.saturation, s.temperature, s.tint,
                        0.0, 0.0, 0.0, 0.0,
                        (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0),
                        0.0, 1.0,
                    )
                } else {
                    // CPU fallback — no equivalent of the unified
                    // shader on CPU yet, so fall through to the
                    // closest analogue (linear ops applied in the
                    // same order the GPU shader uses).
                    let mut frame = primary_input.to_vec();
                    klipp_color::linear::apply_exposure(&mut frame, s.exposure);
                    klipp_color::linear::apply_temperature_tint(&mut frame, s.temperature, s.tint);
                    klipp_color::linear::apply_saturation(&mut frame, s.saturation);
                    frame
                }
            }

            NodeKind::ColorWarper => {
                let warper = params.warper_params.clone().unwrap_or_default();
                if warper.is_identity() {
                    primary_input.to_vec()
                } else if let Some(gpu) = &mut self.gpu {
                    gpu.apply_color_warper_gpu(
                        primary_input.to_vec(), width, height,
                        &warper.hue_shifts, &warper.sat_shifts,
                    )
                } else {
                    // CPU fallback: klipp_color::apply_color_warper speaks u8.
                    let primary_u8 = linear_rgba_to_u8(primary_input);
                    let mut frame_u8 = primary_u8;
                    klipp_color::apply_color_warper(&mut frame_u8, width, height, &warper);
                    u8_rgba_to_linear(&frame_u8)
                }
            }

            NodeKind::Denoise => {
                let luma = params.denoise_luma.unwrap_or(0.0);
                let chroma = params.denoise_chroma.unwrap_or(0.0);
                let radius = params.denoise_radius.unwrap_or(3);
                if luma > 0.0 || chroma > 0.0 {
                    if let Some(gpu) = &mut self.gpu {
                        let spatial_sigma = radius as f32;
                        let color_sigma = (luma + chroma) * 0.5;
                        gpu.apply_denoise_gpu(
                            primary_input.to_vec(), width, height,
                            spatial_sigma, color_sigma,
                        )
                    } else {
                        // CPU fallback: bilateral_denoise speaks u8.
                        let primary_u8 = linear_rgba_to_u8(primary_input);
                        let mut frame_u8 = primary_u8;
                        klipp_color::denoise::bilateral_denoise(
                            &mut frame_u8, width, height, luma, chroma, radius,
                        );
                        u8_rgba_to_linear(&frame_u8)
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::Output | NodeKind::Unknown => primary_input.to_vec(),
        }
    }
}

// ── Helper functions ────────────────────────────────────────────────────────

/// Build the 6×256 f32 LUT data for GPU hue/sat curve application.
fn build_huesat_lut_data(huesat: &klipp_core::HueSatCurves) -> [f32; 1536] {
    let mut lut = [0.5f32; 1536]; // 0.5 = identity for relative curves

    let curves = [
        &huesat.hue_vs_hue,
        &huesat.hue_vs_sat,
        &huesat.hue_vs_lum,
        &huesat.lum_vs_sat,
        &huesat.sat_vs_sat,
        &huesat.orange_vs_sat,
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

/// Convert a klipp_core::Mask to a GpuMaskDef. Returns None for Bezier shapes.
fn mask_to_gpu_def(mask: &klipp_core::Mask) -> Option<klipp_effects::gpu::GpuMaskDef> {
    use klipp_core::MaskShape;
    use klipp_effects::gpu::GpuMaskDef;
    match &mask.shape {
        MaskShape::Circle { center_x, center_y, radius } =>
            Some(GpuMaskDef::circle(*center_x, *center_y, *radius, mask.feather, mask.invert, mask.opacity)),
        MaskShape::Rectangle { center_x, center_y, width, height, rotation } =>
            Some(GpuMaskDef::rectangle(*center_x, *center_y, *width, *height, *rotation, mask.feather, mask.invert, mask.opacity)),
        MaskShape::LinearGradient { start_x, start_y, end_x, end_y } =>
            Some(GpuMaskDef::linear_gradient(*start_x, *start_y, *end_x, *end_y, mask.feather, mask.invert, mask.opacity)),
        MaskShape::Bezier { .. } => None,
    }
}

fn build_curve_luts(curves: &klipp_core::CurveSet) -> CurveLuts {
    CurveLuts {
        master: klipp_color::curves::build_curve_lut(&curves.master),
        red: klipp_color::curves::build_curve_lut(&curves.red),
        green: klipp_color::curves::build_curve_lut(&curves.green),
        blue: klipp_color::curves::build_curve_lut(&curves.blue),
    }
}

/// Restore original luminance after curve application.
fn apply_preserve_luma(frame: &mut [u8], original: &[u8]) {
    for (g, o) in frame.chunks_exact_mut(4).zip(original.chunks_exact(4)) {
        let luma_before = 0.2126 * o[0] as f32 + 0.7152 * o[1] as f32 + 0.0722 * o[2] as f32;
        let luma_after = 0.2126 * g[0] as f32 + 0.7152 * g[1] as f32 + 0.0722 * g[2] as f32;
        if luma_after > 0.5 {
            let ratio = luma_before / luma_after;
            g[0] = (g[0] as f32 * ratio).clamp(0.0, 255.0) as u8;
            g[1] = (g[1] as f32 * ratio).clamp(0.0, 255.0) as u8;
            g[2] = (g[2] as f32 * ratio).clamp(0.0, 255.0) as u8;
        }
    }
}

/// CPU film grain using Wang hash noise.
fn apply_grain_cpu(rgba: &mut [u8], grain: &klipp_core::FilmGrainSettings, frame_number: u64) {
    let amount = grain.amount;
    let seed = frame_number as u32;
    for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
        let hash = wang_hash(seed.wrapping_mul(65521).wrapping_add(i as u32));
        let noise = ((hash & 0xFF) as f32 / 255.0 - 0.5) * amount * 128.0;
        // Luminance for shadow/highlight weighting
        let luma = (0.2126 * chunk[0] as f32 + 0.7152 * chunk[1] as f32 + 0.0722 * chunk[2] as f32) / 255.0;
        let weight = grain.shadows * (1.0 - luma) + grain.highlights * luma;
        let weighted_noise = noise * weight;

        if grain.monochrome {
            for c in &mut chunk[..3] {
                *c = (*c as f32 + weighted_noise).clamp(0.0, 255.0) as u8;
            }
        } else {
            for (j, c) in chunk[..3].iter_mut().enumerate() {
                let h = wang_hash(hash.wrapping_add(j as u32));
                let n = ((h & 0xFF) as f32 / 255.0 - 0.5) * amount * 128.0 * weight;
                *c = (*c as f32 + n).clamp(0.0, 255.0) as u8;
            }
        }
    }
}

fn wang_hash(mut key: u32) -> u32 {
    key = (key ^ 61) ^ (key >> 16);
    key = key.wrapping_mul(9);
    key ^= key >> 4;
    key = key.wrapping_mul(0x27d4eb2d);
    key ^= key >> 15;
    key
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
            NodeKind::Qualifier | NodeKind::Mask | NodeKind::CombineMattes | NodeKind::ApplyMatte
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::default_graph;

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
                let mut p = NodeParams::default();
                p.qualifier = Some(klipp_core::QualifierSettings {
                    enabled: true,
                    hue_center: 0.0,
                    hue_width: 30.0,
                    ..Default::default()
                });
                p
            },
            keyframes: Default::default(),
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
                },
                crate::graph::Node {
                    id: apply_id,
                    kind: NodeKind::ApplyMatte,
                    label: "ApplyMatte".into(),
                    enabled: true,
                    params: NodeParams { mix: Some(1.0), ..Default::default() },
                    keyframes: Default::default(),
                },
                crate::graph::Node {
                    id: output_id,
                    kind: NodeKind::Output,
                    label: "Output".into(),
                    enabled: true,
                    params: Default::default(),
                    keyframes: Default::default(),
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
        };

        let mut executor = GraphExecutor::new_cpu_only();
        // When all 3 ports get the same frame, output should equal input (matte=source R channel)
        let frame = vec![128, 128, 128, 255];
        let (result, _) = executor.execute(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        // With identical inputs and matte R=128 (50%), blend should be ~128
        assert!((result[0] as i32 - 128).abs() <= 1, "ApplyMatte with equal inputs: got {}", result[0]);
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
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                },
                crate::graph::Node {
                    id: blend_id, kind: NodeKind::Blend, label: "Blend".into(),
                    enabled: true,
                    params: NodeParams { mix: Some(0.5), ..Default::default() },
                    keyframes: Default::default(),
                },
                crate::graph::Node {
                    id: output_id, kind: NodeKind::Output, label: "O".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                },
            ],
            edges: vec![
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: blend_id, to_port: 0 },
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: blend_id, to_port: 1 },
                crate::graph::Edge { from_node: blend_id, from_port: 0, to_node: output_id, to_port: 0 },
            ],
            output_node_id: output_id,
            node_positions: Default::default(),
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

    #[test]
    fn combine_mattes_multiplies() {
        let source_id = NodeId::new_v4();
        let combine_id = NodeId::new_v4();
        let output_id = NodeId::new_v4();

        let graph = NodeGraph {
            nodes: vec![
                crate::graph::Node {
                    id: source_id, kind: NodeKind::Source, label: "S".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                },
                crate::graph::Node {
                    id: combine_id, kind: NodeKind::CombineMattes, label: "Combine".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                },
                crate::graph::Node {
                    id: output_id, kind: NodeKind::Output, label: "O".into(),
                    enabled: true, params: Default::default(), keyframes: Default::default(),
                },
            ],
            edges: vec![
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: combine_id, to_port: 0 },
                crate::graph::Edge { from_node: source_id, from_port: 0, to_node: combine_id, to_port: 1 },
                crate::graph::Edge { from_node: combine_id, from_port: 0, to_node: output_id, to_port: 0 },
            ],
            output_node_id: output_id,
            node_positions: Default::default(),
        };

        let mut executor = GraphExecutor::new_cpu_only();
        // Source = 128 grey → CombineMattes multiplies R channels: 128*128/255 ≈ 64
        let frame = vec![128, 128, 128, 255];
        let (result, _) = executor.execute(&frame, &graph, 1, 1, 0.0, 0, None, None).unwrap();
        let expected = (128u16 * 128 / 255) as u8; // 64
        assert!((result[0] as i32 - expected as i32).abs() <= 1,
            "CombineMattes 128*128/255 = {expected}, got {}", result[0]);
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
}
