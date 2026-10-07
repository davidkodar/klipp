//! What masking an effect costs (the M2 acceptance spec's L3 and L7): a
//! graded clip's blur, then the same blur masked by a window at a mix.
//!
//! What the run does today: the mask's lerp is recorded with the blur, in
//! the same chain dispatch and submit, so the chain gains no dispatch,
//! flush, submit or upload. The window, though, is drawn on the CPU (the
//! GPU window draws only for a MaskCutout or a recognised secondary) and
//! written into a pool slot beside the blur's kept input: one more CPU
//! node and two slots. The spec's "+2 dispatches" assumed a GPU window;
//! moving it there is a follow-up, and this test then changes with it.
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
    use klipp_core::{Mask, MaskShape, MaskStack};
    let s = node(NodeKind::Source, NodeParams::default());
    let e = node(NodeKind::Exposure, NodeParams { exposure: Some(0.2), ..Default::default() });
    let blend = masked.then_some(EffectBlend { mix: 0.75, channel: MaskChannel::Luminance });
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
fn a_masked_effect_costs_a_window_and_two_slots() {
    let Some(mut ex) = gpu() else { return };
    let plain = stats(&mut ex, &graph(false), 96, 54);
    let masked = stats(&mut ex, &graph(true), 96, 54);
    eprintln!("plain {plain:?}\nmasked {masked:?}");
    let chain = |s: &ChainStats| (s.extends, s.flushes, s.submits, s.uploads);
    assert_eq!(chain(&masked), chain(&plain), "the lerp rides the blur's dispatch: {plain:?} -> {masked:?}");
    assert_eq!(masked.cpu_nodes, plain.cpu_nodes + 1, "the window, drawn on the CPU: {plain:?} -> {masked:?}");
    assert_eq!((masked.effect_breaks, masked.effect_mixes_refused), (0, 0), "{masked:?}");
    assert_eq!((plain.peak_slots, masked.peak_slots), (0, 2), "the blur's input kept for the lerp, and the window's matte");
}

/// The milliseconds behind L3 and L7 at 4K, for the quiet window. L3:
/// (masked - unmasked) is reported against 1.5 x one Exposure node
/// (`chain_bandwidth_probe`). L7: the peak live set is the chain's two
/// buffers and `peak_slots`, each a 4K RGBA f32 frame (126.56 MiB).
#[test]
#[ignore = "perf probe: run explicitly, release, in a quiet window"]
fn masked_effect_cost_at_4k() {
    let Some(mut ex) = gpu() else { return };
    let (w, h) = (3840, 2160);
    let f = frame(w, h);
    for masked in [false, true] {
        let g = graph(masked);
        let s = stats(&mut ex, &g, w, h);
        let mut ms = Vec::new();
        for _ in 0..20 {
            let t = std::time::Instant::now();
            ex.execute_linear(&f, &g, w, h, 0.5, 12, None, None).unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        ms.sort_by(f64::total_cmp);
        let mib = (2 + s.peak_slots) as f64 * (w * h * 16) as f64 / (1024.0 * 1024.0);
        eprintln!("{}: median {:.2} ms (of 20), peak slots {} -> live set {mib:.1} MiB; {s:?}", if masked { "masked" } else { "plain" }, ms[10], s.peak_slots);
    }
}
