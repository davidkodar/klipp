//! A window that masks an effect is drawn on the GPU, inside the effect's
//! mix in the chain's encoder, with no matte made (#1242, #1252). Each
//! case is held to the
//! CPU executor, which draws the window on the CPU: within a level, and
//! with the window costing no CPU node on the GPU. The cases are what the
//! GPU path adds: two windows in one encoder (each drawn from its own
//! shapes), one window read by two effects, a keyed window at two times,
//! and an effect off the chain, which draws its window on the CPU. A
//! missing GPU leaves it unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it
//! fails.

use klipp_core::{Interpolation, Keyframe, Mask, MaskShape, MaskStack};
use klipp_effects::mask_mix::MaskChannel;
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::{EffectBlend, NodeParams};
use klipp_nodes::GraphExecutor;

const W: u32 = 96;
const H: u32 = 54;
/// A level of an 8-bit picture.
const BAR: f32 = 1.0 / 255.0;

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

fn link(a: NodeId, b: NodeId, port: u8) -> Edge {
    Edge { from_node: a, from_port: 0, to_node: b, to_port: port }
}

/// Edges and gradients, so a blur and a window's edge both show.
fn frame() -> Vec<f32> {
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = (i % W, i / W);
            let block = if ((x / 6) + (y / 6)) % 2 == 0 { 0.85 } else { 0.15 };
            [block * x as f32 / W as f32, block, 1.0 - block * y as f32 / H as f32, 1.0]
        })
        .collect()
}

fn window(shape: MaskShape, feather: f32) -> Node {
    node(NodeKind::Mask, NodeParams { masks: Some(MaskStack { masks: vec![Mask { shape, feather, ..Mask::default() }] }), ..Default::default() })
}

fn circle(cx: f32) -> MaskShape {
    MaskShape::Circle { center_x: cx, center_y: 0.5, radius: 0.2 }
}

fn masked(effect: E, mix: f32) -> Node {
    node(NodeKind::Effect, NodeParams { effects: vec![effect], effect_blend: Some(EffectBlend { mix, channel: MaskChannel::Luminance }), ..Default::default() })
}

/// Source -> Exposure -> `chain`... -> Output, each of `windows` wired to
/// the effects `reads` names (window index, effect index).
fn graph(chain: Vec<Node>, windows: Vec<Node>, reads: &[(usize, usize)]) -> NodeGraph {
    let s = node(NodeKind::Source, NodeParams::default());
    let e = node(NodeKind::Exposure, NodeParams { exposure: Some(0.2), ..Default::default() });
    let o = node(NodeKind::Output, NodeParams::default());
    let mut edges = vec![link(s.id, e.id, 0)];
    let mut prev = e.id;
    for n in &chain {
        edges.push(link(prev, n.id, 0));
        prev = n.id;
    }
    edges.push(link(prev, o.id, 0));
    for w in &windows {
        edges.push(link(s.id, w.id, 0));
    }
    for &(w, fx) in reads {
        edges.push(link(windows[w].id, chain[fx].id, 1));
    }
    let mut nodes = vec![s, e];
    nodes.extend(chain);
    nodes.extend(windows);
    nodes.push(o.clone());
    NodeGraph { output_node_id: o.id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn gpu() -> Option<GraphExecutor> {
    let mut ex = GraphExecutor::new();
    let ok = ex.gpu_pipeline_or_init().is_some();
    assert!(ok || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
    if !ok {
        eprintln!("UNVERIFIED: no GPU");
    }
    ok.then_some(ex)
}

/// `g` at `t` on the GPU against the CPU executor, within a level; and
/// the GPU run's CPU nodes, which is only the source handing over its
/// pixels when `on_chain`.
fn check(name: &str, g: &NodeGraph, t: f64, on_chain: bool) {
    let Some(mut ex) = gpu() else { return };
    let want = GraphExecutor::new_cpu_only().execute_linear(&frame(), g, W, H, t, 0, None, None).unwrap().0;
    let got = ex.execute_linear(&frame(), g, W, H, t, 0, None, None).unwrap().0;
    let stats = ex.last_run_chain_stats();
    assert!(worst(&got, &want) <= BAR, "{name}: the GPU differs from the CPU by {} ({stats:?})", worst(&got, &want));
    assert!(worst(&got, &frame()) > 0.02, "{name}: the case changes the picture");
    if on_chain {
        assert_eq!((stats.cpu_nodes, stats.effect_breaks, stats.effect_mixes_refused), (1, 0, 0), "{name}: every window drawn on the GPU: {stats:?}");
    }
}

#[test]
fn two_windows_in_one_encoder_each_draw_their_own_shape() {
    let g = graph(
        vec![masked(E::GaussianBlur { radius: 4.0 }, 1.0), masked(E::Invert { mix: 1.0 }, 0.8)],
        vec![window(circle(0.3), 0.1), window(MaskShape::Rectangle { center_x: 0.7, center_y: 0.5, width: 0.3, height: 0.5, rotation: 15.0 }, 0.05)],
        &[(0, 0), (1, 1)],
    );
    check("two windows", &g, 0.0, true);
}

#[test]
fn one_window_read_by_two_effects() {
    let g = graph(vec![masked(E::GaussianBlur { radius: 3.0 }, 1.0), masked(E::Invert { mix: 1.0 }, 0.5)], vec![window(circle(0.5), 0.15)], &[(0, 0), (0, 1)]);
    check("a shared window", &g, 0.0, true);
}

#[test]
fn a_keyed_window_moves_on_the_gpu_as_on_the_cpu() {
    let mut w = window(circle(0.3), 0.1);
    let key = |t: f64, v: f32| Keyframe { time_secs: t, value: v, interpolation: Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() };
    if let Some(ms) = w.params.masks.as_mut() {
        ms.masks[0].keyframes.center_x = vec![key(0.0, 0.25), key(1.0, 0.75)];
    }
    let g = graph(vec![masked(E::GaussianBlur { radius: 4.0 }, 1.0)], vec![w], &[(0, 0)]);
    check("keyed window at 0", &g, 0.0, true);
    check("keyed window at 1", &g, 1.0, true);
    let Some(mut ex) = gpu() else { return };
    let at = |ex: &mut GraphExecutor, t: f64| ex.execute_linear(&frame(), &g, W, H, t, 0, None, None).unwrap().0;
    assert!(worst(&at(&mut ex, 0.0), &at(&mut ex, 1.0)) > 0.02, "the window moved");
}

/// Equalize can't be recorded on the chain: its node runs off it, and its
/// window is drawn on the CPU then.
#[test]
fn an_effect_off_the_chain_draws_its_window_on_the_cpu() {
    let g = graph(vec![masked(E::Equalize { strength: 1.0 }, 1.0)], vec![window(circle(0.5), 0.1)], &[(0, 0)]);
    check("off the chain", &g, 0.0, false);
}
