use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use klipp_color::lut::LutData;
use klipp_color::pipeline::CurveLuts;
use klipp_effects::gpu::GpuEffectsPipeline;

use crate::graph::{NodeGraph, NodeId};
use crate::kinds::NodeKind;
use crate::params::NodeParams;

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
pub struct GraphExecutor {
    /// Optional GPU pipeline for accelerated node execution.
    gpu: Option<GpuEffectsPipeline>,
    /// Cache of loaded LUT data keyed by file path.
    lut_cache: HashMap<PathBuf, Arc<LutData>>,
    /// Cache of pre-built curve LUTs keyed by node ID.
    curve_lut_cache: HashMap<NodeId, Arc<CurveLuts>>,
}

impl GraphExecutor {
    pub fn new() -> Self {
        let gpu = GpuEffectsPipeline::new();
        if gpu.is_none() {
            tracing::info!("GraphExecutor: GPU unavailable, using CPU fallback for all nodes");
        }
        Self {
            gpu,
            lut_cache: HashMap::new(),
            curve_lut_cache: HashMap::new(),
        }
    }

    /// Create an executor without GPU (for tests or export threads with their own GPU).
    pub fn new_cpu_only() -> Self {
        Self {
            gpu: None,
            lut_cache: HashMap::new(),
            curve_lut_cache: HashMap::new(),
        }
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
        // 1. Topological sort
        let order = graph.topo_sort()?;

        // 2. Map: NodeId → output buffer
        let mut node_outputs: HashMap<NodeId, Vec<u8>> = HashMap::new();
        let mut tapped_output: Option<Vec<u8>> = None;

        // 3. Execute nodes in topo order
        for node_id in order {
            let node = match graph.nodes.iter().find(|n| n.id == node_id) {
                Some(n) => n,
                None => continue,
            };

            // Bypassed nodes pass their primary input through unchanged
            if !node.enabled {
                let primary_input_id = graph.input_of(node_id, 0);
                let passthrough = primary_input_id
                    .and_then(|id| node_outputs.get(&id))
                    .cloned()
                    .unwrap_or_else(|| source_frame.to_vec());
                node_outputs.insert(node.id, passthrough);
                continue;
            }

            if node.kind == NodeKind::Output {
                continue;
            }

            // Resolve inputs — borrow as slices
            let empty = Vec::new();
            let primary = graph
                .input_of(node_id, 0)
                .and_then(|id| node_outputs.get(&id));
            let secondary = graph
                .input_of(node_id, 1)
                .and_then(|id| node_outputs.get(&id));
            let tertiary = graph
                .input_of(node_id, 2)
                .and_then(|id| node_outputs.get(&id));

            let primary_slice = primary.map(|v| v.as_slice()).unwrap_or(source_frame);
            let secondary_slice = secondary.map(|v| v.as_slice());
            let tertiary_slice = tertiary.map(|v| v.as_slice());

            // Evaluate keyframes — override params at current clip time
            let mut params = node.params.clone();
            for (param_name, value) in node.keyframes.evaluate_all(clip_time_secs) {
                params.apply_override(&param_name, value);
            }

            // Execute the node
            let output = self.execute_node(
                node.id,
                &node.kind,
                &params,
                primary_slice,
                secondary_slice,
                tertiary_slice,
                width,
                height,
                source_frame,
                frame_number,
                &empty,
            );

            // Capture tapped node output for scopes
            if Some(node_id) == matte_tap {
                tapped_output = Some(output.clone());
            }

            // Matte preview — return greyscale matte early
            if Some(node_id) == matte_preview_node {
                let matte: Vec<u8> = output
                    .chunks_exact(4)
                    .flat_map(|c| [c[0], c[0], c[0], 255])
                    .collect();
                return Ok((matte, tapped_output));
            }

            node_outputs.insert(node_id, output);
        }

        // 4. Return the output node's input
        let output_input_id = graph
            .input_of(graph.output_node_id, 0)
            .ok_or(ExecutorError::NoOutputConnection)?;
        let result = node_outputs
            .remove(&output_input_id)
            .ok_or(ExecutorError::MissingNodeOutput)?;

        Ok((result, tapped_output))
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_node(
        &mut self,
        node_id: NodeId,
        kind: &NodeKind,
        params: &NodeParams,
        primary_input: &[u8],
        secondary_input: Option<&[u8]>,
        tertiary_input: Option<&[u8]>,
        width: u32,
        height: u32,
        source_frame: &[u8],
        frame_number: u64,
        _empty: &[u8],
    ) -> Vec<u8> {
        match kind {
            NodeKind::Source => source_frame.to_vec(),

            NodeKind::InputTransform => {
                let mut frame = primary_input.to_vec();
                if let Some(fmt) = params.log_format {
                    klipp_color::apply_input_transform(&mut frame, fmt);
                }
                frame
            }

            NodeKind::Exposure => {
                let mut frame = primary_input.to_vec();
                let exp = params.exposure.unwrap_or(0.0);
                if exp != 0.0 {
                    klipp_color::apply_exposure(&mut frame, exp);
                }
                frame
            }

            NodeKind::Contrast => {
                let mut frame = primary_input.to_vec();
                let con = params.contrast.unwrap_or(0.0);
                if con != 0.0 {
                    klipp_color::apply_contrast(&mut frame, con);
                }
                let clarity = params.clarity.unwrap_or(0.0);
                if clarity != 0.0 {
                    klipp_color::apply_clarity_inplace(&mut frame, width, height, clarity);
                }
                frame
            }

            NodeKind::Saturation => {
                let mut frame = primary_input.to_vec();
                let sat = params.saturation.unwrap_or(1.0);
                if sat != 1.0 {
                    klipp_color::apply_saturation(&mut frame, sat);
                }
                let vib = params.vibrance.unwrap_or(0.0);
                if vib != 0.0 {
                    klipp_color::apply_vibrance_buffer(&mut frame, vib);
                }
                frame
            }

            NodeKind::ToneControls => {
                let mut frame = primary_input.to_vec();
                klipp_color::apply_tone_controls(
                    &mut frame,
                    params.highlights.unwrap_or(0.0),
                    params.shadows.unwrap_or(0.0),
                    params.whites.unwrap_or(0.0),
                    params.blacks.unwrap_or(0.0),
                );
                frame
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
                    klipp_color::apply_log_wheels(&mut frame, low, mid, high);
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
                        klipp_color::apply_lift_gamma_gain_offset(&mut frame, lift, gamma, gain, offset);
                        frame
                    }
                }
            }

            NodeKind::WheelSatBri => {
                let mut frame = primary_input.to_vec();
                klipp_color::apply_wheel_sat_bri(
                    &mut frame,
                    params.lift_sat.unwrap_or(0.0),
                    params.lift_bri.unwrap_or(0.0),
                    params.gamma_sat.unwrap_or(0.0),
                    params.gamma_bri.unwrap_or(0.0),
                    params.gain_sat.unwrap_or(0.0),
                    params.gain_bri.unwrap_or(0.0),
                    params.offset_sat.unwrap_or(0.0),
                    params.offset_bri.unwrap_or(0.0),
                );
                frame
            }

            NodeKind::RGBCurves => {
                let mut frame = primary_input.to_vec();
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
                    klipp_color::curves::apply_curves(
                        &mut frame,
                        &luts.master,
                        &luts.red,
                        &luts.green,
                        &luts.blue,
                    );
                    // Preserve luma
                    if params.curves_preserve_luma.unwrap_or(false) {
                        apply_preserve_luma(&mut frame, primary_input);
                    }
                    // Curves mix
                    if let Some(mix) = params.curves_mix {
                        if mix < 0.999 {
                            klipp_color::apply_global_mix(&mut frame, primary_input, mix);
                        }
                    }
                }
                frame
            }

            NodeKind::HueSatCurves => {
                if let Some(huesat) = &params.huesat {
                    if !huesat.is_all_identity() {
                        let mix = params.huesat_mix.unwrap_or(1.0);
                        // GPU path: build LUT data and dispatch
                        if let Some(gpu) = &mut self.gpu {
                            let lut_data = build_huesat_lut_data(huesat);
                            gpu.apply_huesat_gpu(primary_input.to_vec(), width, height, mix, &lut_data)
                        } else {
                            let mut frame = primary_input.to_vec();
                            klipp_color::huesat::apply_huesat_curves(&mut frame, huesat, mix);
                            frame
                        }
                    } else {
                        primary_input.to_vec()
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::ColorBalance => {
                let mut frame = primary_input.to_vec();
                let temp = params.temperature.unwrap_or(0.0);
                let tint = params.tint.unwrap_or(0.0);
                if temp != 0.0 || tint != 0.0 {
                    klipp_color::apply_temperature_tint(&mut frame, temp, tint);
                }
                if let Some(hue) = params.hue {
                    if hue.abs() > 0.01 {
                        klipp_color::apply_hue_rotation(&mut frame, hue);
                    }
                }
                if let Some(mix) = params.global_mix {
                    if mix < 0.999 {
                        klipp_color::apply_global_mix(&mut frame, primary_input, mix);
                    }
                }
                frame
            }

            NodeKind::Qualifier => {
                // Output is a matte (greyscale RGBA: R=G=B=matte_byte, A=255)
                if let Some(qual) = &params.qualifier {
                    // GPU path: qualifier generates matte via compute shader
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
                        let matte = klipp_color::qualifier::generate_qualifier_matte(
                            primary_input, width, height, qual,
                        );
                        matte.iter()
                            .flat_map(|&v| { let b = (v * 255.0) as u8; [b, b, b, 255] })
                            .collect()
                    }
                } else {
                    vec![255u8; (width * height * 4) as usize]
                }
            }

            NodeKind::Mask => {
                if let Some(masks) = &params.masks {
                    let matte = klipp_color::masks::generate_mask_matte(width, height, masks);
                    matte
                        .iter()
                        .flat_map(|&v| {
                            let b = (v * 255.0) as u8;
                            [b, b, b, 255]
                        })
                        .collect()
                } else {
                    vec![255u8; (width * height * 4) as usize]
                }
            }

            NodeKind::CombineMattes => {
                let white = vec![255u8; (width * height * 4) as usize];
                let matte_a = primary_input;
                let matte_b = secondary_input.unwrap_or(&white);
                matte_a
                    .chunks_exact(4)
                    .zip(matte_b.chunks_exact(4))
                    .flat_map(|(a, b)| {
                        let combined = (a[0] as u16 * b[0] as u16 / 255) as u8;
                        [combined, combined, combined, 255]
                    })
                    .collect()
            }

            NodeKind::ApplyMatte => {
                // Port 0: graded frame, Port 1: matte, Port 2: ungraded (pre-grade)
                let graded = primary_input;
                let white = vec![255u8; graded.len()];
                let matte_rgba = secondary_input.unwrap_or(&white);
                let ungraded = tertiary_input.unwrap_or(source_frame);
                let mix = params.mix.unwrap_or(1.0);

                graded
                    .chunks_exact(4)
                    .zip(ungraded.chunks_exact(4))
                    .zip(matte_rgba.chunks_exact(4))
                    .flat_map(|((g, u), m)| {
                        let matte_val = m[0] as f32 / 255.0 * mix;
                        let r = (u[0] as f32 + (g[0] as f32 - u[0] as f32) * matte_val) as u8;
                        let g_ch =
                            (u[1] as f32 + (g[1] as f32 - u[1] as f32) * matte_val) as u8;
                        let b = (u[2] as f32 + (g[2] as f32 - u[2] as f32) * matte_val) as u8;
                        [r, g_ch, b, 255]
                    })
                    .collect()
            }

            NodeKind::Blend => {
                let a = primary_input;
                let b = secondary_input.unwrap_or(a);
                let mix = params.mix.unwrap_or(0.5);
                a.chunks_exact(4)
                    .zip(b.chunks_exact(4))
                    .flat_map(|(pa, pb)| {
                        let r = (pa[0] as f32 + (pb[0] as f32 - pa[0] as f32) * mix) as u8;
                        let g = (pa[1] as f32 + (pb[1] as f32 - pa[1] as f32) * mix) as u8;
                        let b_c = (pa[2] as f32 + (pb[2] as f32 - pa[2] as f32) * mix) as u8;
                        [r, g, b_c, 255]
                    })
                    .collect()
            }

            NodeKind::Layer => {
                let fg = primary_input;
                let bg = secondary_input.unwrap_or(source_frame);
                // Simple alpha-over composite
                fg.chunks_exact(4)
                    .zip(bg.chunks_exact(4))
                    .flat_map(|(f, b)| {
                        let fa = f[3] as f32 / 255.0;
                        let r = (f[0] as f32 * fa + b[0] as f32 * (1.0 - fa)) as u8;
                        let g = (f[1] as f32 * fa + b[1] as f32 * (1.0 - fa)) as u8;
                        let bl = (f[2] as f32 * fa + b[2] as f32 * (1.0 - fa)) as u8;
                        let a = (f[3] as f32 + b[3] as f32 * (1.0 - fa)).min(255.0) as u8;
                        [r, g, bl, a]
                    })
                    .collect()
            }

            NodeKind::Lut3D => {
                let mut frame = primary_input.to_vec();
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
                    klipp_color::apply_3d_lut(
                        &mut frame,
                        &lut,
                        params.lut_opacity.unwrap_or(1.0),
                    );
                }
                frame
            }

            NodeKind::FilmGrain => {
                if let Some(grain) = &params.grain {
                    if grain.enabled && grain.amount > 0.001 {
                        // GPU path
                        if let Some(gpu) = &mut self.gpu {
                            gpu.apply_grain_gpu(
                                primary_input.to_vec(), width, height,
                                grain.amount, grain.size,
                                grain.shadows, grain.highlights,
                                grain.monochrome,
                                frame_number as u32,
                            )
                        } else {
                            let mut frame = primary_input.to_vec();
                            apply_grain_cpu(&mut frame, grain, frame_number);
                            frame
                        }
                    } else {
                        primary_input.to_vec()
                    }
                } else {
                    primary_input.to_vec()
                }
            }

            NodeKind::ColorMatch => {
                // Future: histogram matching. For now, pass-through.
                primary_input.to_vec()
            }

            NodeKind::Denoise => {
                let luma = params.denoise_luma.unwrap_or(0.0);
                let chroma = params.denoise_chroma.unwrap_or(0.0);
                let radius = params.denoise_radius.unwrap_or(3);
                if luma > 0.0 || chroma > 0.0 {
                    let mut frame = primary_input.to_vec();
                    klipp_color::denoise::bilateral_denoise(
                        &mut frame, width, height, luma, chroma, radius,
                    );
                    frame
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
        assert_eq!(frame, result, "identity default graph should be passthrough");
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
        assert_eq!(
            frame, result,
            "bypassed exposure node should not change frame"
        );
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

        let mut graph = NodeGraph {
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
        assert_eq!(result[0], 200);
        assert_eq!(result[1], 100);
        assert_eq!(result[2], 50);
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
        // +1 stop exposure should roughly double: 0.5 → ~1.0 (clamped)
        assert!(mid[0] > 0.7, "exposure +1 should brighten mid-grey: got {:.2}", mid[0]);
    }
}
