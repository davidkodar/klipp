//! What masking an effect costs (the M2 acceptance spec's L3, and the
//! masked case of L7): a graded clip's blur, then the same blur masked by
//! a window at a mix.
//!
//! The mask's lerp is recorded with the blur, in the same chain dispatch
//! and submit, and the window is worked out inside the lerp itself, per
//! pixel, with no matte written or read (#1242, #1252): the chain gains no
//! dispatch, flush, submit or upload, and no node runs on the CPU. The run
//! holds one pool slot, the blur's kept input.
//!
//! The counts are the test; the milliseconds are a probe for the quiet
//! window (`--ignored --nocapture`, release), never asserted: timing under
//! the gate's load is noise. A missing GPU leaves it unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_effects::mask_mix::MaskChannel;
use klipp_effects::BuiltinEffect;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::{EffectBlend, NodeParams};
use klipp_nodes::executor::ChainStats;
use klipp_nodes::GraphExecutor;

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

fn link(a: NodeId, b: NodeId, port: u8) -> Edge {
    Edge { from_node: a, from_port: 0, to_node: b, to_port: port }
}

fn frame(w: u32, h: u32) -> Vec<f32> {
    (0..w * h).flat_map(|i| {
        let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
        [x, 0.2 + 0.6 * y, 1.0 - x, 1.0]
    }).collect()
}

/// Source -> Exposure -> Blur -> Output, the blur masked by a window at a
/// mix when `masked`: a graded clip's effect, on a live chain.
fn graph(masked: bool) -> NodeGraph {
    graph_of(masked, masked)
}

/// [`graph`], mixed when `mixed` and masked by the window when `masked`
/// too: a mix with no window reads an unwired mask, white, which prices
/// the mix's own passes apart from the window's.
fn graph_of(mixed: bool, masked: bool) -> NodeGraph {
    use klipp_core::{Mask, MaskShape, MaskStack};
    let s = node(NodeKind::Source, NodeParams::default());
    let e = node(NodeKind::Exposure, NodeParams { exposure: Some(0.2), ..Default::default() });
    let blend = mixed.then_some(EffectBlend { mix: 0.75, channel: MaskChannel::Luminance });
    let fx = node(NodeKind::Effect, NodeParams { effects: vec![BuiltinEffect::GaussianBlur { radius: 6.0 }], effect_blend: blend, ..Default::default() });
    let o = node(NodeKind::Output, NodeParams::default());
    let mut nodes = vec![s.clone(), e.clone(), fx.clone()];
    let mut edges = vec![link(s.id, e.id, 0), link(e.id, fx.id, 0), link(fx.id, o.id, 0)];
    if masked {
        let masks = MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, feather: 0.2, ..Mask::default() }] };
        let window = node(NodeKind::Mask, NodeParams { masks: Some(masks), ..Default::default() });
        edges.push(link(s.id, window.id, 0));
        edges.push(link(window.id, fx.id, 1));
        nodes.push(window);
    }
    nodes.push(o.clone());
    NodeGraph { output_node_id: o.id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn gpu() -> Option<GraphExecutor> {
    let mut ex = GraphExecutor::new();
    let ok = ex.gpu_pipeline_or_init().is_some();
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    assert!(ok || !required, "required, but there is no GPU");
    if !ok {
        eprintln!("UNVERIFIED: no GPU");
    }
    ok.then_some(ex)
}

/// The second of two runs (the first makes the pool's slots), and its counts.
fn stats(ex: &mut GraphExecutor, g: &NodeGraph, w: u32, h: u32) -> ChainStats {
    let f = frame(w, h);
    for _ in 0..2 {
        ex.execute_linear(&f, g, w, h, 0.5, 12, None, None).unwrap();
    }
    ex.last_run_chain_stats()
}

#[test]
fn a_masked_effect_costs_one_slot_and_nothing_on_the_cpu() {
    let Some(mut ex) = gpu() else { return };
    let plain = stats(&mut ex, &graph(false), 96, 54);
    let masked = stats(&mut ex, &graph(true), 96, 54);
    eprintln!("plain {plain:?}\nmasked {masked:?}");
    let chain = |s: &ChainStats| (s.extends, s.flushes, s.submits, s.uploads);
    assert_eq!(chain(&masked), chain(&plain), "the lerp rides the blur's dispatch: {plain:?} -> {masked:?}");
    assert_eq!(masked.cpu_nodes, plain.cpu_nodes, "the window, drawn on the GPU: {plain:?} -> {masked:?}");
    assert_eq!((masked.effect_breaks, masked.effect_mixes_refused), (0, 0), "{masked:?}");
    assert_eq!((plain.peak_slots, masked.peak_slots), (0, 1), "the blur's input kept for the lerp, and no matte for the window");
}

/// The milliseconds behind L3 and L7 at 4K, for the quiet window. L3:
/// The mix alone (an unwired mask) is timed too, so the window's own
/// share shows. (masked - unmasked) is reported against 1.5 x one Exposure node
/// (`chain_bandwidth_probe`). L7: the peak live set is the chain's two
/// buffers and `peak_slots`, each a 4K RGBA f32 frame (126.56 MiB).
#[test]
#[ignore = "perf probe: run explicitly, release, in a quiet window"]
fn masked_effect_cost_at_4k() {
    let Some(mut ex) = gpu() else { return };
    let (w, h) = (3840, 2160);
    let f = frame(w, h);
    for (name, mixed, masked) in [("plain", false, false), ("mixed, no window", true, false), ("masked", true, true)] {
        let g = graph_of(mixed, masked);
        let s = stats(&mut ex, &g, w, h);
        let mut ms = Vec::new();
        for _ in 0..20 {
            let t = std::time::Instant::now();
            ex.execute_linear(&f, &g, w, h, 0.5, 12, None, None).unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        ms.sort_by(f64::total_cmp);
        let mib = (2 + s.peak_slots) as f64 * (w * h * 16) as f64 / (1024.0 * 1024.0);
        eprintln!("{name}: median {:.2} ms (of 20), peak slots {} -> live set {mib:.1} MiB; {s:?}", ms[10], s.peak_slots);
    }
}

/// The GPU's own time for each case at 4K, for the quiet window: the
/// frame seeded on the GPU, the result left there, and the queue waited
/// on, so neither the harness's CPU copies nor the readback hide the
/// passes. Reported, never asserted.
#[test]
#[ignore = "perf probe: run explicitly, release, in a quiet window"]
fn masked_effect_gpu_time_at_4k() {
    let Some(mut ex) = gpu() else { return };
    let (w, h) = (3840, 2160);
    let f = frame(w, h);
    let device = ex.gpu_pipeline_or_init().expect("a GPU").device();
    // L3's unit: one Exposure node more on the plain chain.
    let mut one_more = graph_of(false, false);
    let extra = node(NodeKind::Exposure, NodeParams { exposure: Some(-0.1), ..Default::default() });
    let into_out = one_more.edges.iter_mut().find(|e| e.to_node == one_more.output_node_id).expect("the output's edge");
    let last = into_out.from_node;
    into_out.from_node = extra.id;
    one_more.edges.push(link(last, extra.id, 0));
    one_more.nodes.push(extra);
    let cases = [("plain", graph_of(false, false)), ("one Exposure more", one_more), ("mixed, no window", graph_of(true, false)), ("masked", graph_of(true, true))];
    for (name, g) in cases {
        let mut ms = Vec::new();
        for i in 0..23 {
            assert!(ex.prime_chain_from_linear(&f, w, h), "seeded");
            let _ = device.poll(wgpu::Maintain::Wait);
            ex.request_output_on_gpu();
            let t = std::time::Instant::now();
            ex.execute_linear(&[], &g, w, h, 0.5, 12, None, None).unwrap();
            let _ = device.poll(wgpu::Maintain::Wait);
            if i >= 3 {
                ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        ms.sort_by(f64::total_cmp);
        eprintln!("{name}: GPU median {:.3} ms (of 20, min {:.3}); {:?}", ms[10], ms[0], ex.last_run_chain_stats());
    }
}
