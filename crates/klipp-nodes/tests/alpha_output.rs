//! A grade's Output takes a matte as the alpha its picture leaves with
//! (#898): the picture's colour as it was, straight, its coverage the
//! matte's, on the CPU and on the GPU (where the result comes back to the
//! CPU to take it). Unwired, the alpha is the picture's own.

use klipp_core::{Mask, MaskShape, MaskStack};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 64;
const H: u32 = 36;

fn node(id: NodeId, kind: NodeKind, params: NodeParams) -> Node {
    Node { id, kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() }
}

/// Source → Exposure → Output, and when `alpha`, a hard circle from the
/// source into the Output's alpha.
fn graph(alpha: bool) -> NodeGraph {
    let (src, exp, mask, out) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    let circle = MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.25 }, feather: 0.0, ..Mask::default() }] };
    let mut nodes = vec![
        node(src, NodeKind::Source, NodeParams::default()),
        node(exp, NodeKind::Exposure, NodeParams { exposure: Some(0.5), ..Default::default() }),
        node(out, NodeKind::Output, NodeParams::default()),
    ];
    let e = |from, to, to_port| Edge { from_node: from, from_port: 0, to_node: to, to_port };
    let mut edges = vec![e(src, exp, 0), e(exp, out, 0)];
    if alpha {
        nodes.push(node(mask, NodeKind::Mask, NodeParams { masks: Some(circle), ..Default::default() }));
        edges.extend([e(src, mask, 0), e(mask, out, 1)]);
    }
    NodeGraph { output_node_id: out, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn frame() -> Vec<f32> {
    (0..W * H).flat_map(|i| [0.2 + 0.5 * (i % W) as f32 / W as f32, 0.3, 0.4, 1.0]).collect()
}

fn px(f: &[f32], x: u32, y: u32) -> [f32; 4] {
    let i = ((y * W + x) * 4) as usize;
    [f[i], f[i + 1], f[i + 2], f[i + 3]]
}

#[test]
fn the_outputs_alpha_is_the_matte_wired_into_it() {
    let mut cpu = GraphExecutor::new_cpu_only();
    let (plain, _) = cpu.execute_linear(&frame(), &graph(false), W, H, 0.0, 0, None, None).unwrap();
    let (keyed, _) = cpu.execute_linear(&frame(), &graph(true), W, H, 0.0, 0, None, None).unwrap();
    // Inside the circle opaque, outside clear; the colour the grade's
    // either way.
    assert_eq!(px(&keyed, W / 2, H / 2)[3], 1.0);
    assert_eq!(px(&keyed, 2, 2)[3], 0.0);
    for (x, y) in [(W / 2, H / 2), (2, 2)] {
        assert_eq!(px(&keyed, x, y)[..3], px(&plain, x, y)[..3], "the colour at ({x}, {y})");
    }
    assert!(plain.chunks_exact(4).all(|p| p[3] == 1.0), "unwired, the picture's own alpha");

    let mut gpu = GraphExecutor::new();
    if gpu.gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    // Asked to keep its result on the GPU, a run with an alpha wired
    // brings it back to take the matte.
    gpu.request_output_on_gpu();
    let (on_gpu, _) = gpu.execute_linear(&frame(), &graph(true), W, H, 0.0, 0, None, None).unwrap();
    assert_eq!(on_gpu.len(), keyed.len(), "the result is on the CPU");
    let worst = on_gpu.iter().zip(&keyed).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(worst < 2e-3, "GPU against CPU: {worst}");
}

/// The matte cuts into the alpha the picture has (#898): a picture half
/// transparent stays half transparent where the matte keeps it, and goes
/// clear where it doesn't, its colour untouched.
#[test]
fn the_matte_multiplies_the_pictures_own_alpha() {
    let half: Vec<f32> = frame().chunks_exact(4).flat_map(|p| [p[0], p[1], p[2], 0.5]).collect();
    let (keyed, _) = GraphExecutor::new_cpu_only().execute_linear(&half, &graph(true), W, H, 0.0, 0, None, None).unwrap();
    assert_eq!(px(&keyed, W / 2, H / 2)[3], 0.5, "kept: its own alpha");
    assert_eq!(px(&keyed, 2, 2)[3], 0.0, "cut");
}

/// A window with nothing chosen keeps everything, as every matte reader
/// takes it: the picture leaves whole.
#[test]
fn an_empty_window_into_the_alpha_keeps_everything() {
    let mut g = graph(true);
    for n in g.nodes.iter_mut().filter(|n| n.kind == NodeKind::Mask) {
        n.params.masks = None;
    }
    let (out, _) = GraphExecutor::new_cpu_only().execute_linear(&frame(), &g, W, H, 0.0, 0, None, None).unwrap();
    assert!(out.chunks_exact(4).all(|p| p[3] == 1.0));
}
