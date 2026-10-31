//! Perf scenarios. Each one runs the grade executor (or other measured
//! unit) in a tight loop against synthetic frames and returns per-frame
//! wall-clock times.
//!
//! Only the grade-pipeline scenarios live here for now. Full-engine
//! scenarios (slider drag during real playback, watchdog, UI tick p99)
//! need a headless eframe + fixture video harness and are deferred until
//! after the eframe-wgpu port in phase 1 of the GPU-residency plan.

use std::collections::HashMap;
use std::time::Instant;

use klipp_nodes::{
    executor::GraphExecutor,
    graph::{Edge, Node, NodeGraph, NodeId, NodePos},
    kinds::NodeKind,
    params::{NodeKeyframeTracks, NodeParams},
};

use crate::{
    fixtures::{gradient_frame_linear, gradient_frame_u8},
    stats::{summarise, ScenarioResult},
};

/// Frame + warmup counts. Reduced from the 120 in the plan because the
/// current gpu.rs `apply_color_balance_gpu` (non-chain path) accumulates
/// descriptors in NVIDIA Vulkan's deferred-destruction queue and
/// triggers `device lost (Dropped)` after ~85 dispatches — documented
/// in the comment at `klipp-effects/src/gpu.rs:275`. Sizing to 60
/// keeps us inside the pre-drop window on the current codebase; plan B
/// phase 4 folds the non-chain dispatch into the chain infra which
/// already avoids the accumulation via `chain_bg_3binding`.
const FRAMES: usize = 60;
const WARMUP: usize = 10;

/// Wall-clock floor for warmup, on top of [`WARMUP`] frames.
///
/// A frame count alone does not absorb what needs absorbing. Pipeline
/// creation and shader compilation are one-off, but GPU clock ramp is a
/// wall-clock effect, and ten frames of a cheap graph buys a fraction of
/// the time ten frames of an expensive one does. Warming by duration
/// makes the scenarios comparable to each other instead of each getting
/// a warmup proportional to its own cost.
const WARMUP_MIN_MS: f64 = 750.0;

/// Hard stop so a pathologically slow scenario cannot spin here forever.
const WARMUP_MAX_FRAMES: usize = 600;

pub struct Scenario {
    pub name: &'static str,
    pub desc: &'static str,
    pub run: fn(bool) -> ScenarioResult,
}

pub const ALL: &[Scenario] = &[
    Scenario {
        name: "s01_colorbalance_1node_1080p",
        desc: "Single ColorBalance node at 1080p, 60 frames",
        run: s01_colorbalance_1node_1080p,
    },
    Scenario {
        name: "s02_grade_chain_5node_1080p",
        desc: "5-node grade chain at 1080p, 60 frames",
        run: s02_grade_chain_5node_1080p,
    },
    Scenario {
        name: "s03_grade_chain_10node_1080p",
        desc: "10-node grade chain at 1080p, 60 frames",
        run: s03_grade_chain_10node_1080p,
    },
    Scenario {
        name: "s04_colorbalance_1node_1080p_gpu_primed",
        desc: "s01's graph fed from a GPU-resident u8 buffer (skips the CPU upload)",
        run: s04_colorbalance_1node_1080p_gpu_primed,
    },
    Scenario {
        name: "s10_composite_uhd_clip_credits",
        desc: "Portrait UHD clip with a scrolling credits title, composite + display encode",
        run: crate::composite::s10_uhd_clip_credits,
    },
    Scenario {
        name: "s15_composite_uhd_single_clip",
        desc: "Portrait UHD clip alone: the direct-present frame through the stack",
        run: crate::composite::s15_uhd_single_clip,
    },
    Scenario {
        name: "s11_composite_uhd_clip_credits_grade3",
        desc: "s10 with a three-node grade on the clip",
        run: crate::composite::s11_uhd_clip_credits_grade3,
    },
    Scenario {
        name: "s12_composite_uhd_two_layers_screen",
        desc: "Two UHD video layers, the top at Screen and 50 % opacity",
        run: crate::composite::s12_uhd_two_layers_blend,
    },
    Scenario {
        name: "s13_composite_uhd_luma_matte",
        desc: "Three UHD video layers, the middle one masked by a luma matte",
        run: crate::composite::s13_uhd_luma_matte,
    },
    Scenario {
        name: "s14_composite_1080p_five_layers",
        desc: "1080p: two titles over three video layers, one graded, one at Overlay",
        run: crate::composite::s14_hd_five_layers,
    },
    Scenario {
        name: "s22_composite_uhd_terminal_card_busy_24fps",
        desc: "#3179: UHD at 24 fps: Terminal Card over a blurred layer, a graded clip and music",
        run: crate::composite::s22_uhd_terminal_card_busy,
    },
    Scenario {
        name: "s22_composite_uhd_terminal_card_busy_24fps_played_at_1080p",
        desc: "#3205: s22's UHD timeline drawn at the 1080p playback size, its pictures decoded at 1080p",
        run: crate::composite::s22_uhd_terminal_card_busy_at_playback_size,
    },
    Scenario {
        name: "s20_flatten_1_instances",
        desc: "#3073: flatten 1 template instance(s)",
        run: crate::nesting::flatten_1,
    },
    Scenario {
        name: "s20_flatten_1_instances_overrides",
        desc: "#3073: flatten 1 template instance(s) with overrides",
        run: crate::nesting::flatten_1_o,
    },
    Scenario {
        name: "s20_flatten_10_instances",
        desc: "#3073: flatten 10 template instance(s)",
        run: crate::nesting::flatten_10,
    },
    Scenario {
        name: "s20_flatten_10_instances_overrides",
        desc: "#3073: flatten 10 template instance(s) with overrides",
        run: crate::nesting::flatten_10_o,
    },
    Scenario {
        name: "s20_flatten_50_instances",
        desc: "#3073: flatten 50 template instance(s)",
        run: crate::nesting::flatten_50,
    },
    Scenario {
        name: "s20_flatten_50_instances_overrides",
        desc: "#3073: flatten 50 template instance(s) with overrides",
        run: crate::nesting::flatten_50_o,
    },
    Scenario {
        name: "s20_flatten_200_instances",
        desc: "#3073: flatten 200 template instance(s)",
        run: crate::nesting::flatten_200,
    },
    Scenario {
        name: "s20_flatten_200_instances_overrides",
        desc: "#3073: flatten 200 template instance(s) with overrides",
        run: crate::nesting::flatten_200_o,
    },
    Scenario {
        name: "s20_edit_50_instances_move",
        desc: "#3117: a clip moved a frame among 50 instances, then the cached flattening",
        run: crate::nesting::edit_50_move,
    },
    Scenario {
        name: "s20_edit_200_instances_move",
        desc: "#3117: a clip moved a frame among 200 instances, then the cached flattening",
        run: crate::nesting::edit_200_move,
    },
    Scenario {
        name: "s20_edit_50_instances_retext",
        desc: "#3117: one instance's text changed among 50, then the cached flattening",
        run: crate::nesting::edit_50_retext,
    },
    Scenario {
        name: "s20_edit_200_instances_retext",
        desc: "#3117: one instance's text changed among 200, then the cached flattening",
        run: crate::nesting::edit_200_retext,
    },
    Scenario {
        name: "s21_nest_1_levels_1080p",
        desc: "#3073: precomposition nested 1 level(s), 1080p",
        run: crate::nesting::nest_1_hd,
    },
    Scenario {
        name: "s21_nest_2_levels_1080p",
        desc: "#3073: precomposition nested 2 level(s), 1080p",
        run: crate::nesting::nest_2_hd,
    },
    Scenario {
        name: "s21_nest_3_levels_1080p",
        desc: "#3073: precomposition nested 3 level(s), 1080p",
        run: crate::nesting::nest_3_hd,
    },
    Scenario {
        name: "s21_nest_4_levels_1080p",
        desc: "#3073: precomposition nested 4 level(s), 1080p",
        run: crate::nesting::nest_4_hd,
    },
    Scenario {
        name: "s21_nest_1_levels_uhd",
        desc: "#3073: precomposition nested 1 level(s), uhd",
        run: crate::nesting::nest_1_uhd,
    },
    Scenario {
        name: "s21_nest_2_levels_uhd",
        desc: "#3073: precomposition nested 2 level(s), uhd",
        run: crate::nesting::nest_2_uhd,
    },
    Scenario {
        name: "s21_nest_3_levels_uhd",
        desc: "#3073: precomposition nested 3 level(s), uhd",
        run: crate::nesting::nest_3_uhd,
    },
    Scenario {
        name: "s21_nest_4_levels_uhd",
        desc: "#3073: precomposition nested 4 level(s), uhd",
        run: crate::nesting::nest_4_uhd,
    },
    Scenario {
        name: "s21_nest_1_levels_1080p_overrides",
        desc: "#3073: precomposition nested 1 level(s), 1080p, overrides at every level",
        run: crate::nesting::nest_1_hd_o,
    },
    Scenario {
        name: "s21_nest_2_levels_1080p_overrides",
        desc: "#3073: precomposition nested 2 level(s), 1080p, overrides at every level",
        run: crate::nesting::nest_2_hd_o,
    },
    Scenario {
        name: "s21_nest_3_levels_1080p_overrides",
        desc: "#3073: precomposition nested 3 level(s), 1080p, overrides at every level",
        run: crate::nesting::nest_3_hd_o,
    },
    Scenario {
        name: "s21_nest_4_levels_1080p_overrides",
        desc: "#3073: precomposition nested 4 level(s), 1080p, overrides at every level",
        run: crate::nesting::nest_4_hd_o,
    },
    Scenario {
        name: "s21_nest_1_levels_uhd_overrides",
        desc: "#3073: precomposition nested 1 level(s), uhd, overrides at every level",
        run: crate::nesting::nest_1_uhd_o,
    },
    Scenario {
        name: "s21_nest_2_levels_uhd_overrides",
        desc: "#3073: precomposition nested 2 level(s), uhd, overrides at every level",
        run: crate::nesting::nest_2_uhd_o,
    },
    Scenario {
        name: "s21_nest_3_levels_uhd_overrides",
        desc: "#3073: precomposition nested 3 level(s), uhd, overrides at every level",
        run: crate::nesting::nest_3_uhd_o,
    },
    Scenario {
        name: "s21_nest_4_levels_uhd_overrides",
        desc: "#3073: precomposition nested 4 level(s), uhd, overrides at every level",
        run: crate::nesting::nest_4_uhd_o,
    },
];

// ─── Scenario implementations ───────────────────────────────────────

fn s01_colorbalance_1node_1080p(cpu_only: bool) -> ScenarioResult {
    let (w, h) = (1920u32, 1080u32);
    let graph = graph_colorbalance_only();
    run_grade_scenario("s01_colorbalance_1node_1080p", w, h, &graph, FRAMES, WARMUP, 4.0, cpu_only)
}

fn s02_grade_chain_5node_1080p(cpu_only: bool) -> ScenarioResult {
    let (w, h) = (1920u32, 1080u32);
    let graph = graph_chain_5node();
    run_grade_scenario("s02_grade_chain_5node_1080p", w, h, &graph, FRAMES, WARMUP, 8.0, cpu_only)
}

/// The measurement the plan's headline claim actually rests on.
///
/// Identical graph, resolution and frame count to `s01` — the only
/// difference is where the pixels come from. `s01` hands
/// `execute_linear` a CPU linear-f32 buffer, so every frame pays an
/// upload plus the u8→linear conversion. This one seeds the GPU chain
/// straight from a `wgpu::Buffer` of u8 RGBA via
/// `prime_chain_from_gpu_u8_buffer`, which is what a hardware decoder
/// leaves behind in `LinearFrame.gpu_source`.
///
/// `s01` minus `s04` is therefore the size of the prize for making the
/// engine consume `gpu_source` instead of calling `ensure_cpu_data()`.
/// Before this scenario existed the harness only ever exercised the
/// CPU-upload path, so the claim was untested either way.
///
/// The input buffer is created once and re-primed per frame on purpose:
/// in the real path the decoder has already written it on the GPU, so
/// charging this scenario for a CPU→GPU upload would measure a cost the
/// production path does not pay.
fn s04_colorbalance_1node_1080p_gpu_primed(cpu_only: bool) -> ScenarioResult {
    let (w, h) = (1920u32, 1080u32);
    let name = "s04_colorbalance_1node_1080p_gpu_primed";
    let graph = graph_colorbalance_only();

    if cpu_only {
        eprintln!("{name}: skipped — the primed path is GPU-only by definition");
        return summarise(name, w, h, &[], 0, 4.0);
    }
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
        eprintln!("{name}: skipped — no usable GpuContext");
        return summarise(name, w, h, &[], 0, 4.0);
    };

    let bytes = gradient_frame_u8(w, h);
    // No CPU-side source: the engine hands the executor an empty slice
    // when the decoder produced a GPU-resident frame, so this scenario
    // does the same. Passing `linear_cpu` here would have the executor
    // copy a frame it never reads (33 MB at 1080p, 132 MB at 4K, RGBA
    // f32) and measure a memcpy the real primed path does not perform.
    const NO_CPU_SOURCE: &[f32] = &[];

    let input_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("perf_s04_gpu_u8_input"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.queue.write_buffer(&input_buf, 0, &bytes);
    ctx.device.poll(wgpu::Maintain::Wait);

    // No `has_gpu()` pre-check: the executor builds its GPU pipeline
    // lazily, and `prime_chain_from_gpu_u8_buffer` calls `ensure_gpu`
    // itself. Asking before priming reports "no GPU" on an executor that
    // is about to have one.
    let mut exec = GraphExecutor::new();
    if !exec.prime_chain_from_gpu_u8_buffer(&input_buf, w, h) {
        eprintln!(
            "{name}: skipped — prime_chain_from_gpu_u8_buffer refused \
             (has_gpu={})",
            exec.has_gpu(),
        );
        return summarise(name, w, h, &[], 0, 4.0);
    }

    let warm_start = Instant::now();
    let mut warm_frames = 0usize;
    while (warm_frames < WARMUP
        || warm_start.elapsed().as_secs_f64() * 1000.0 < WARMUP_MIN_MS)
        && warm_frames < WARMUP_MAX_FRAMES
    {
        exec.prime_chain_from_gpu_u8_buffer(&input_buf, w, h);
        let r = exec
            .execute_linear(NO_CPU_SOURCE, &graph, w, h, 0.0, warm_frames as u64, None, None)
            .expect("primed execute_linear failed during warmup");
        std::hint::black_box(r.0.len());
        warm_frames += 1;
    }

    let mut samples = Vec::with_capacity(FRAMES);
    let mut prime_failures = 0usize;
    for i in 0..FRAMES {
        let t = Instant::now();
        // Re-prime each frame: one decoded frame, one seed. The return
        // value matters — without a seed there is no source at all now,
        // so a failure cannot be papered over by a CPU upload. Count it
        // and stop; the verdict is refused below either way.
        if !exec.prime_chain_from_gpu_u8_buffer(&input_buf, w, h) {
            prime_failures += 1;
            break;
        }
        let r = exec
            .execute_linear(NO_CPU_SOURCE, &graph, w, h, 0.0, i as u64, None, None)
            .expect("primed execute_linear failed");
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
        std::hint::black_box(r.0.len());
    }

    if prime_failures > 0 {
        eprintln!(
            "{name}: {prime_failures}/{FRAMES} frames failed to prime and fell back to \
             the CPU upload path — these timings are NOT the primed path"
        );
        // Refuse the verdict rather than publish a fallback measurement
        // under a primed-path name.
        let mut res = summarise(name, w, h, &[], 0, 4.0);
        res.warmup_frames = warm_frames;
        return res;
    }

    let mut res = summarise(name, w, h, &samples, 0, 4.0);
    res.warmup_frames = warm_frames;
    res
}

fn s03_grade_chain_10node_1080p(cpu_only: bool) -> ScenarioResult {
    let (w, h) = (1920u32, 1080u32);
    let graph = graph_chain_10node();
    run_grade_scenario("s03_grade_chain_10node_1080p", w, h, &graph, FRAMES, WARMUP, 15.0, cpu_only)
}

// ─── Shared runner ──────────────────────────────────────────────────

fn run_grade_scenario(
    name: &str,
    width: u32,
    height: u32,
    graph: &NodeGraph,
    frames: usize,
    warmup: usize,
    budget_p99_ms: f64,
    cpu_only: bool,
) -> ScenarioResult {
    let source = gradient_frame_linear(width, height);
    let mut exec = if cpu_only {
        GraphExecutor::new_cpu_only()
    } else {
        GraphExecutor::new()
    };
    // ── Warmup: discarded, and bounded by frames AND duration ──
    let warm_start = Instant::now();
    let mut warm_frames = 0usize;
    while (warm_frames < warmup
        || warm_start.elapsed().as_secs_f64() * 1000.0 < WARMUP_MIN_MS)
        && warm_frames < WARMUP_MAX_FRAMES
    {
        let result = exec
            .execute_linear(&source, graph, width, height, 0.0, warm_frames as u64, None, None)
            .expect("execute_linear failed during warmup");
        std::hint::black_box(result.0.len());
        warm_frames += 1;
    }

    // ── Measurement: every sample counts, none discarded ──
    // Warmup is a separate loop now, so `summarise` is handed a window
    // that is entirely steady-state and skips nothing. Its drift check
    // is what catches the case where that assumption is wrong.
    let mut samples = Vec::with_capacity(frames);
    for i in 0..frames {
        let t = Instant::now();
        let result = exec
            .execute_linear(&source, graph, width, height, 0.0, i as u64, None, None)
            .expect("execute_linear failed");
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
        // Prevent DCE.
        std::hint::black_box(result.0.len());
    }

    let mut r = summarise(name, width, height, &samples, 0, budget_p99_ms);
    r.warmup_frames = warm_frames;
    r
}

// ─── Graph fixtures ─────────────────────────────────────────────────
//
// Kept as small helper builders so each scenario's graph is one
// function we can read and reason about. Params use non-default values
// so nothing bypasses.

fn mk_node(id: NodeId, kind: NodeKind, label: &str, params: NodeParams) -> Node {
    Node {
        id,
        kind,
        label: label.into(),
        enabled: true,
        params,
        keyframes: NodeKeyframeTracks::default(),
        output_link: None,
        input_links: Vec::new(),
    }
}

fn mk_edge(from: NodeId, to: NodeId) -> Edge {
    Edge { from_node: from, from_port: 0, to_node: to, to_port: 0 }
}

fn finalise(nodes: Vec<Node>, edges: Vec<Edge>, output_id: NodeId) -> NodeGraph {
    let mut positions = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        positions.insert(n.id, NodePos { x: (i as f32) * 180.0, y: 0.0 });
    }
    NodeGraph {
        nodes,
        edges,
        output_node_id: output_id,
        node_positions: positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    }
}

fn graph_colorbalance_only() -> NodeGraph {
    let source = NodeId::new_v4();
    let colorbal = NodeId::new_v4();
    let output = NodeId::new_v4();

    let p = NodeParams {
        hue: Some(0.15), // non-zero → forces execution
        global_mix: Some(1.0),
        ..Default::default()
    };

    let nodes = vec![
        mk_node(source, NodeKind::Source, "Source", NodeParams::default()),
        mk_node(colorbal, NodeKind::ColorBalance, "ColorBalance", p),
        mk_node(output, NodeKind::Output, "Output", NodeParams::default()),
    ];
    let edges = vec![mk_edge(source, colorbal), mk_edge(colorbal, output)];
    finalise(nodes, edges, output)
}

fn graph_chain_5node() -> NodeGraph {
    let source = NodeId::new_v4();
    let exposure = NodeId::new_v4();
    let contrast = NodeId::new_v4();
    let saturation = NodeId::new_v4();
    let wheels = NodeId::new_v4();
    let colorbal = NodeId::new_v4();
    let output = NodeId::new_v4();

    let p_exp = NodeParams {
        exposure: Some(0.3),
        ..Default::default()
    };

    let p_con = NodeParams {
        contrast: Some(0.15),
        clarity: Some(0.10),
        ..Default::default()
    };

    let p_sat = NodeParams {
        saturation: Some(1.10),
        vibrance: Some(0.10),
        ..Default::default()
    };

    let p_wheel = NodeParams {
        lift_master: Some(0.02),
        gamma_master: Some(-0.05),
        gain_master: Some(0.03),
        ..Default::default()
    };

    let p_cb = NodeParams {
        temperature: Some(0.05),
        tint: Some(-0.02),
        hue: Some(0.08),
        global_mix: Some(1.0),
        ..Default::default()
    };

    let nodes = vec![
        mk_node(source, NodeKind::Source, "Source", NodeParams::default()),
        mk_node(exposure, NodeKind::Exposure, "Exposure", p_exp),
        mk_node(contrast, NodeKind::Contrast, "Contrast", p_con),
        mk_node(saturation, NodeKind::Saturation, "Saturation", p_sat),
        mk_node(wheels, NodeKind::ColorWheels, "Wheels", p_wheel),
        mk_node(colorbal, NodeKind::ColorBalance, "ColorBalance", p_cb),
        mk_node(output, NodeKind::Output, "Output", NodeParams::default()),
    ];
    let edges = vec![
        mk_edge(source, exposure),
        mk_edge(exposure, contrast),
        mk_edge(contrast, saturation),
        mk_edge(saturation, wheels),
        mk_edge(wheels, colorbal),
        mk_edge(colorbal, output),
    ];
    finalise(nodes, edges, output)
}

fn graph_chain_10node() -> NodeGraph {
    // Extends the 5-node chain with 5 more GPU-capable filter nodes.
    let source = NodeId::new_v4();
    let exposure = NodeId::new_v4();
    let tone = NodeId::new_v4();
    let wheels = NodeId::new_v4();
    let wheel_sb = NodeId::new_v4();
    let contrast = NodeId::new_v4();
    let saturation = NodeId::new_v4();
    let curves = NodeId::new_v4();
    let huesat = NodeId::new_v4();
    let warper = NodeId::new_v4();
    let colorbal = NodeId::new_v4();
    let output = NodeId::new_v4();

    let p_exp = NodeParams {
        exposure: Some(0.3),
        ..Default::default()
    };

    let p_tone = NodeParams {
        highlights: Some(-0.2),
        shadows: Some(0.2),
        whites: Some(0.05),
        blacks: Some(-0.05),
        ..Default::default()
    };

    let p_wheels = NodeParams {
        lift_master: Some(0.02),
        gamma_master: Some(-0.05),
        gain_master: Some(0.03),
        ..Default::default()
    };

    let p_wheel_sb = NodeParams {
        lift_sat: Some(1.02),
        gain_sat: Some(1.05),
        gain_bri: Some(0.03),
        ..Default::default()
    };

    let p_con = NodeParams {
        contrast: Some(0.15),
        clarity: Some(0.10),
        ..Default::default()
    };

    let p_sat = NodeParams {
        saturation: Some(1.10),
        vibrance: Some(0.10),
        ..Default::default()
    };

    let p_curves = NodeParams::default(); // will be a bypass tunnel — still traversed
    let p_huesat = NodeParams::default();
    let p_warper = NodeParams::default();

    let p_cb = NodeParams {
        temperature: Some(0.05),
        tint: Some(-0.02),
        hue: Some(0.08),
        global_mix: Some(1.0),
        ..Default::default()
    };

    let nodes = vec![
        mk_node(source, NodeKind::Source, "Source", NodeParams::default()),
        mk_node(exposure, NodeKind::Exposure, "Exposure", p_exp),
        mk_node(tone, NodeKind::ToneControls, "Tone", p_tone),
        mk_node(wheels, NodeKind::ColorWheels, "Wheels", p_wheels),
        mk_node(wheel_sb, NodeKind::WheelSatBri, "WheelSB", p_wheel_sb),
        mk_node(contrast, NodeKind::Contrast, "Contrast", p_con),
        mk_node(saturation, NodeKind::Saturation, "Saturation", p_sat),
        mk_node(curves, NodeKind::RGBCurves, "Curves", p_curves),
        mk_node(huesat, NodeKind::HueSatCurves, "HueSat", p_huesat),
        mk_node(warper, NodeKind::ColorWarper, "Warper", p_warper),
        mk_node(colorbal, NodeKind::ColorBalance, "ColorBalance", p_cb),
        mk_node(output, NodeKind::Output, "Output", NodeParams::default()),
    ];
    let edges = vec![
        mk_edge(source, exposure),
        mk_edge(exposure, tone),
        mk_edge(tone, wheels),
        mk_edge(wheels, wheel_sb),
        mk_edge(wheel_sb, contrast),
        mk_edge(contrast, saturation),
        mk_edge(saturation, curves),
        mk_edge(curves, huesat),
        mk_edge(huesat, warper),
        mk_edge(warper, colorbal),
        mk_edge(colorbal, output),
    ];
    finalise(nodes, edges, output)
}
