//! A clip's built-in effects run as nodes of its grade's plan (#1089,
//! phase 2b M2, step 10b) and draw exactly what the legacy route draws:
//! the grade's picture, then `apply_effects` on it.
//!
//! On the GPU, recorded effects go into the grade's own chain (one
//! submit for the grade and its effects), and an effect not recorded yet
//! is a counted break (`ChainStats::effect_breaks`), never a readback.
//! A missing GPU leaves the GPU cases unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_effects::BuiltinEffect as E;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 48;
const H: u32 = 16;

fn frame() -> Vec<f32> {
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = ((i % W) as f32 / W as f32, (i / W) as f32 / H as f32);
            [x * 0.9, y * 0.7, (1.0 - x) * 0.6 + 0.05, 1.0]
        })
        .collect()
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

/// Source → Exposure → `effects` (one node each, or none) → Output.
fn graph(effects: &[Vec<E>], segment: bool) -> NodeGraph {
    let mut nodes = vec![node(NodeKind::Source, NodeParams::default()), node(NodeKind::Exposure, NodeParams { exposure: Some(0.4), ..Default::default() })];
    for fx in effects {
        let kind = if segment { NodeKind::EffectSegment } else { NodeKind::Effect };
        nodes.push(node(kind, NodeParams { effects: fx.clone(), ..Default::default() }));
    }
    nodes.push(node(NodeKind::Output, NodeParams::default()));
    let edges = nodes.windows(2).map(|p| Edge { from_node: p[0].id, from_port: 0, to_node: p[1].id, to_port: 0 }).collect();
    NodeGraph { output_node_id: nodes.last().unwrap().id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn colour_family() -> Vec<E> {
    vec![
        E::Invert { mix: 0.3 },
        E::Posterize { levels: 9.0 },
        E::Gamma { gamma: 1.2 },
        E::HSVAdjust { hue_shift: 12.0, saturation: 1.1, value: 0.95 },
        E::Levels { in_black: 0.02, in_white: 0.95, gamma: 1.1, out_black: 0.0, out_white: 1.0 },
        E::Vibrance { amount: 0.3 },
        E::GaussianBlur { radius: 2.5 },
    ]
}

fn run(ex: &mut GraphExecutor, g: &NodeGraph) -> Vec<f32> {
    run_on(ex, g, &frame())
}

fn run_on(ex: &mut GraphExecutor, g: &NodeGraph, source: &[f32]) -> Vec<f32> {
    ex.execute_linear(source, g, W, H, 0.5, 12, None, None).unwrap().0
}

/// `frame` with a transparent border, so what spills past it shows.
fn cutout() -> Vec<f32> {
    let mut f = frame();
    for (i, px) in f.chunks_mut(4).enumerate() {
        let (x, y) = (i as u32 % W, i as u32 / W);
        if !(10..W - 10).contains(&x) || !(4..H - 4).contains(&y) {
            px.copy_from_slice(&[0.0, 0.0, 0.0, 0.0]);
        }
    }
    f
}

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

/// The legacy route: the grade alone, then the effects on its picture
/// through a pipeline of their own (the renderer's), as `effects` runs
/// them.
fn legacy(gpu: bool, fx: &[E]) -> Vec<f32> {
    let mut ex = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
    let graded = run(&mut ex, &graph(&[], false));
    let luma = klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::default());
    match gpu.then(klipp_effects::gpu::GpuEffectsPipeline::new).flatten() {
        Some(mut p) if !klipp_effects::has_frei0r(fx) => {
            p.set_luma_coefs(luma);
            p.apply_effects(fx, &graded, W, H)
        }
        _ => klipp_effects::apply_effects_with_luma(fx, graded, W, H, luma),
    }
}

/// The grade alone's CPU nodes (the source is one): effect nodes on the
/// chain add none.
fn grade_cpu_nodes(ex: &mut GraphExecutor) -> u32 {
    run(ex, &graph(&[], false));
    ex.last_run_chain_stats().cpu_nodes
}

fn has_gpu(ex: &mut GraphExecutor) -> bool {
    let gpu = ex.gpu_pipeline_or_init().is_some();
    assert!(gpu || !required(), "required, but there is no GPU");
    if !gpu {
        eprintln!("UNVERIFIED: no GPU");
    }
    gpu
}

#[test]
fn effect_nodes_on_the_cpu_draw_the_legacy_picture() {
    let fx = colour_family();
    let mut ex = GraphExecutor::new_cpu_only();
    assert_eq!(bits(&run(&mut ex, &graph(std::slice::from_ref(&fx), false))), bits(&legacy(false, &fx)));
    // One node per effect is the same picture.
    let each: Vec<Vec<E>> = fx.iter().map(|e| vec![e.clone()]).collect();
    assert_eq!(bits(&run(&mut ex, &graph(&each, false))), bits(&legacy(false, &fx)));
}

#[test]
fn recorded_effects_join_the_grades_chain_in_one_submit() {
    let fx = colour_family();
    let each: Vec<Vec<E>> = fx.iter().map(|e| vec![e.clone()]).collect();
    let mut ex = GraphExecutor::new();
    if !has_gpu(&mut ex) {
        return;
    }
    let base = grade_cpu_nodes(&mut ex);
    let got = run(&mut ex, &graph(&each, false));
    let stats = ex.last_run_chain_stats();
    assert_eq!(bits(&got), bits(&legacy(true, &fx)), "the legacy route's picture, bit for bit");
    assert_eq!(stats.effect_breaks, 0, "{stats:?}");
    assert_eq!(stats.flushes, 1, "one chain, grade and effects: {stats:?}");
    assert_eq!(stats.cpu_nodes, base, "{stats:?}");
    assert_eq!(stats.submits, 1, "{stats:?}");
    assert!(ex.last_run_effects_complete());
}

/// Equalize records its histogram, CDF and map in the chain (#1215), so a
/// node with it between recorded effects is no break, and draws what the
/// effects' own pipeline draws.
#[test]
fn equalize_between_recorded_effects_is_no_break() {
    let fx = vec![E::Gamma { gamma: 1.2 }, E::Equalize { strength: 0.6 }, E::Invert { mix: 0.3 }];
    let each: Vec<Vec<E>> = fx.iter().map(|e| vec![e.clone()]).collect();
    let mut ex = GraphExecutor::new();
    if !has_gpu(&mut ex) {
        return;
    }
    let base = grade_cpu_nodes(&mut ex);
    let got = run(&mut ex, &graph(&each, false));
    let stats = ex.last_run_chain_stats();
    assert_eq!(bits(&got), bits(&legacy(true, &fx)));
    assert_eq!(stats.effect_breaks, 0, "{stats:?}");
    assert_eq!((stats.cpu_nodes, stats.uploads), (base, 1), "the picture stays on the GPU: {stats:?}");
}

#[test]
fn a_segment_of_recorded_effects_is_no_break() {
    let fx = colour_family();
    let mut ex = GraphExecutor::new();
    if !has_gpu(&mut ex) {
        return;
    }
    let got = run(&mut ex, &graph(std::slice::from_ref(&fx), true));
    assert_eq!(bits(&got), bits(&legacy(true, &fx)));
    assert_eq!(ex.last_run_chain_stats().effect_breaks, 0);
}

#[test]
fn a_lost_recording_says_the_run_is_incomplete() {
    let mut ex = GraphExecutor::new();
    if !has_gpu(&mut ex) {
        return;
    }
    ex.gpu_pipeline_mut().unwrap().refuse_recording(true);
    let _ = run(&mut ex, &graph(&[colour_family()], false));
    assert!(!ex.last_run_effects_complete(), "the caller must take another route");
    ex.gpu_pipeline_mut().unwrap().refuse_recording(false);
    let _ = run(&mut ex, &graph(&[colour_family()], false));
    assert!(ex.last_run_effects_complete());
}

/// An effect node whose effects all change nothing leaves the picture
/// where it was (no slot flip), and is never taken for a matte write.
#[test]
fn an_effect_node_that_changes_nothing_passes_the_picture_on() {
    let still = vec![E::Sharpen { amount: 0.0 }, E::Mosaic { block_size: 1.0 }];
    let mut ex = GraphExecutor::new();
    if !has_gpu(&mut ex) {
        return;
    }
    let graded = run(&mut ex, &graph(&[], false));
    let got = run(&mut ex, &graph(&[still, vec![E::Gamma { gamma: 1.3 }]], false));
    assert_eq!(bits(&got), bits(&legacy(true, &[E::Gamma { gamma: 1.3 }])));
    assert_ne!(bits(&got), bits(&graded));
}

/// A spill group (a shadow and a glow) is one layer: each is cast from the
/// same picture and stacked under it, as `apply_effects` groups them. So
/// as a segment or inside one effect list, it records as one run into the
/// grade's chain: no break, one submit.
#[test]
fn a_spill_group_runs_as_one_layer() {
    let group = vec![
        // Sizes are fractions of the frame's width.
        E::DropShadow { offset_x: 0.06, offset_y: 0.04, blur: 0.02, color: [10, 20, 30], opacity: 0.8 },
        E::OuterGlow { radius: 0.05, intensity: 0.7, color: [250, 200, 40] },
    ];
    let mut ex = GraphExecutor::new();
    if !has_gpu(&mut ex) {
        return;
    }
    let graded = run_on(&mut ex, &graph(&[], false), &cutout());
    let mut p = klipp_effects::gpu::GpuEffectsPipeline::new().unwrap();
    p.set_luma_coefs(klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::default()));
    let legacy = p.apply_effects(&group, &graded, W, H);
    let shadowed = p.apply_effects(&group[..1], &graded, W, H);
    let one_at_a_time = p.apply_effects(&group[1..], &shadowed, W, H);
    let worst = legacy.iter().zip(&one_at_a_time).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst > 1e-3, "the case tells a group from its members in turn");
    for (name, g) in [("segment", graph(std::slice::from_ref(&group), true)), ("one list", graph(std::slice::from_ref(&group), false))] {
        let got = run_on(&mut ex, &g, &cutout());
        let stats = ex.last_run_chain_stats();
        assert_eq!(bits(&got), bits(&legacy), "{name}: the legacy group's picture");
        assert_eq!((stats.effect_breaks, stats.submits), (0, 1), "{name}: {stats:?}");
    }
}
