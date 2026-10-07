//! L6, the effect chain's cost (the M2 acceptance spec): an 8-effect
//! chain at 4K, through the legacy pipeline loop
//! (`GpuEffectsPipeline::apply_effects`: upload, `run_effects_in_chain`,
//! download) against the native plan (eight `Effect` nodes in one
//! submit, seeded from the same pixels and read back the same way). The
//! native plan must be no slower: a regression here means effects fell
//! back to per-effect submits (R20).
//!
//! A manual probe, like `composite_cost.rs` and `secondary_cost.rs`: it
//! needs a GPU and prints milliseconds, so it is ignored in the gate.
//! Read the submits as well as the times. Run with
//! `cargo test --release -p klipp-nodes --test effect_chain_cost -- --ignored --nocapture --test-threads=1`
//! (one at a time: the two probes share the GPU, and run together they
//! time each other).

use std::time::Instant;

use klipp_effects::BuiltinEffect as E;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 3840;
const H: u32 = 2160;
const RUNS: usize = 20;

fn chain() -> Vec<E> {
    vec![
        E::Gamma { gamma: 1.1 },
        E::Vibrance { amount: 0.3 },
        E::HSVAdjust { hue_shift: 8.0, saturation: 1.05, value: 0.98 },
        E::Levels { in_black: 0.02, in_white: 0.95, gamma: 1.05, out_black: 0.0, out_white: 1.0 },
        E::Invert { mix: 0.2 },
        E::Posterize { levels: 24.0 },
        E::GaussianBlur { radius: 2.0 },
        E::Vignette { amount: 0.4, size: 0.6, softness: 0.5, roundness: 1.0 },
    ]
}

/// Source → one `Effect` node per effect → Output, ids fixed so the plan
/// is compiled once.
fn plan(fx: &[E]) -> NodeGraph {
    let node = |id: u128, kind, params| Node { id: NodeId::from_u128(id), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let mut nodes = vec![node(1, NodeKind::Source, NodeParams::default())];
    for (i, e) in fx.iter().enumerate() {
        nodes.push(node(100 + i as u128, NodeKind::Effect, NodeParams { effects: vec![e.clone()], ..Default::default() }));
    }
    nodes.push(node(2, NodeKind::Output, NodeParams::default()));
    let edges = (0..nodes.len() - 1).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    NodeGraph { output_node_id: NodeId::from_u128(2), nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
#[ignore = "a manual probe: needs a GPU, prints milliseconds (L6)"]
fn the_native_effect_plan_is_no_slower_than_the_legacy_loop() {
    // KLIPP_PROBE_TRACE=1 prints the executor's own per-node breakdown
    // (`PERF graph_exec`) for each native run: where its time goes, at node
    // granularity, when no sampling profiler is to hand.
    if std::env::var_os("KLIPP_PROBE_TRACE").is_some() {
        let _ = tracing_subscriber::fmt().with_max_level(tracing_subscriber::filter::LevelFilter::DEBUG).with_target(false).try_init();
    }
    let px: Vec<f32> = (0..W * H).flat_map(|i| {
        let (x, y) = ((i % W) as f32 / W as f32, (i / W) as f32 / H as f32);
        [x * 0.9, y * 0.7, (1.0 - x) * 0.6 + 0.05, 1.0]
    }).collect();
    let fx = chain();
    assert!(fx.iter().all(|e| klipp_effects::gpu::slots_needed(e).is_some()), "every effect of the chain records natively");
    let mut legacy = klipp_effects::gpu::GpuEffectsPipeline::new().expect("a GPU");
    let mut ex = GraphExecutor::new();
    assert!(ex.gpu_pipeline_or_init().is_some(), "a GPU");
    let g = plan(&fx);
    let native_run = |ex: &mut GraphExecutor| {
        assert!(ex.prime_chain_from_linear(&px, W, H));
        ex.execute_linear(&[], &g, W, H, 0.0, 0, None, None).expect("plan run").0
    };
    // Warm both: shaders, buffers, the plan.
    for _ in 0..3 {
        let _ = legacy.apply_effects(&fx, &px, W, H);
        let _ = native_run(&mut ex);
    }
    let (mut l, mut n) = (Vec::new(), Vec::new());
    for _ in 0..RUNS {
        let t = Instant::now();
        let a = legacy.apply_effects(&fx, &px, W, H);
        l.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let b = native_run(&mut ex);
        n.push(t.elapsed().as_secs_f64() * 1000.0);
        std::hint::black_box((a, b));
    }
    let stats = ex.last_run_chain_stats();
    let (lm, nm) = (median(l), median(n));
    eprintln!("L6 at {W}x{H}, 8 effects, median of {RUNS}: legacy loop {lm:.2} ms, native plan {nm:.2} ms ({:+.1}%); native submits {}, flushes {}, effect breaks {}", (nm / lm - 1.0) * 100.0, stats.submits, stats.flushes, stats.effect_breaks);
    assert_eq!((stats.submits, stats.effect_breaks), (1, 0), "the native plan is one submit with no breaks: {stats:?}");
    // Both runs move the frame up and back (about 27 of 28.5 ms at 4K), so
    // the medians agree to within about a percent and move by that much
    // between runs; 3% is the noise band. The structural check above is the
    // sharper one: per-effect submits would show there first.
    assert!(nm <= lm * 1.03, "the native plan ({nm:.2} ms) is slower than the legacy loop ({lm:.2} ms)");
}

/// L7, the GPU memory the effects' scratch takes (the M2 acceptance spec):
/// the most pool slots a run holds at once, priced at 4K (one frame-sized
/// slot is 3840 × 2160 × 16 bytes, 126.56 MiB), within the engine's budget
/// of 4 slots (≈ 506 MiB). Measured on the 8-effect chain and on D10's
/// worst shape, six masked effects in series, each with its own matte.
#[test]
#[ignore = "a manual probe: needs a GPU, prints MiB (L7)"]
fn the_effects_scratch_stays_within_the_slot_budget_at_4k() {
    use klipp_core::{Mask, MaskShape, MaskStack};
    use klipp_effects::mask_mix::MaskChannel;
    use klipp_nodes::params::EffectBlend;
    const SLOT_MIB: f64 = (W as f64) * (H as f64) * 16.0 / (1024.0 * 1024.0);
    const BUDGET: u32 = 4;
    let px: Vec<f32> = (0..W * H).flat_map(|i| {
        let (x, y) = ((i % W) as f32 / W as f32, (i / W) as f32 / H as f32);
        [x * 0.9, y * 0.7, (1.0 - x) * 0.6 + 0.05, 1.0]
    }).collect();
    let mut ex = GraphExecutor::new();
    assert!(ex.gpu_pipeline_or_init().is_some(), "a GPU");
    // The 8-effect chain, unmasked.
    let _ = ex.execute_linear(&px, &plan(&chain()), W, H, 0.0, 0, None, None).expect("run");
    let plain = ex.last_run_chain_stats().peak_slots;
    // Six masked effects in series, each read through its own window.
    let node = |id: u128, kind, params| Node { id: NodeId::from_u128(id), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let masked = |n: usize| {
        let mut nodes = vec![node(1, NodeKind::Source, NodeParams::default())];
        let mut edges = Vec::new();
        let mut prev = NodeId::from_u128(1);
        for (i, e) in chain().into_iter().filter(|e| klipp_effects::registry::effect_types().any(|t| t.desc.mixable && std::mem::discriminant(&(t.sample)()) == std::mem::discriminant(e))).take(n).enumerate() {
            let (fx, win) = (NodeId::from_u128(100 + i as u128), NodeId::from_u128(200 + i as u128));
            let shape = MaskShape::Circle { center_x: 0.2 + 0.1 * i as f32, center_y: 0.5, radius: 0.25 };
            nodes.push(node(200 + i as u128, NodeKind::Mask, NodeParams { masks: Some(MaskStack { masks: vec![Mask { shape, ..Mask::default() }] }), ..Default::default() }));
            nodes.push(node(100 + i as u128, NodeKind::Effect, NodeParams { effects: vec![e], effect_blend: Some(EffectBlend { mix: 1.0, channel: MaskChannel::Luminance }), ..Default::default() }));
            edges.push(Edge { from_node: NodeId::from_u128(1), from_port: 0, to_node: win, to_port: 0 });
            edges.push(Edge { from_node: win, from_port: 0, to_node: fx, to_port: 1 });
            edges.push(Edge { from_node: prev, from_port: 0, to_node: fx, to_port: 0 });
            prev = fx;
        }
        nodes.push(node(2, NodeKind::Output, NodeParams::default()));
        edges.push(Edge { from_node: prev, from_port: 0, to_node: NodeId::from_u128(2), to_port: 0 });
        NodeGraph { output_node_id: NodeId::from_u128(2), nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
    };
    let mut peaks = Vec::new();
    for n in [3, 6] {
        let _ = ex.execute_linear(&px, &masked(n), W, H, 0.0, 0, None, None).expect("masked run");
        let s = ex.last_run_chain_stats();
        assert_eq!(s.effect_mixes_refused, 0, "{n} masked: every mix applied");
        peaks.push(s.peak_slots);
    }
    eprintln!(
        "L7 at {W}x{H} ({SLOT_MIB:.2} MiB a slot): 8 effects peak {plain} slots ({:.0} MiB); 3 masked {} ({:.0} MiB); 6 masked {} ({:.0} MiB); budget {BUDGET} ({:.0} MiB)",
        plain as f64 * SLOT_MIB, peaks[0], peaks[0] as f64 * SLOT_MIB, peaks[1], peaks[1] as f64 * SLOT_MIB, BUDGET as f64 * SLOT_MIB
    );
    assert_eq!(peaks[0], peaks[1], "the peak doesn't grow with the number of masked effects");
    assert!(plain.max(peaks[1]) <= BUDGET, "peak slots over the budget");
}
