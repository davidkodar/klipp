//! Mask → MaskCutout: the window's matte rasterised and multiplied on the
//! GPU has to give the CPU's picture (#692). The two are separate code —
//! `SHADER_MASK_V2` into `matte_buf` and `SHADER_MASK_CUTOUT` on the
//! chain, against the CPU rasteriser and the executor's multiply arm —
//! and until this test nothing compared the cutout's two halves.

use klipp_core::{Mask, MaskShape, MaskStack};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 192;
const H: u32 = 108;

fn node(id: NodeId, kind: NodeKind, params: NodeParams) -> Node {
    Node {
        id,
        kind,
        label: format!("{kind:?}"),
        enabled: true,
        params,
        keyframes: Default::default(),
        output_link: None,
        input_links: Vec::new(),
    }
}

/// `Source ─► MaskCutout.0`, `Mask ─► MaskCutout.1`: the source shown
/// only through a soft circle and a turned rectangle.
fn cutout() -> NodeGraph {
    let (src, mask, cut, out) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    let masks = MaskStack {
        masks: vec![
            Mask { shape: MaskShape::Circle { center_x: 0.35, center_y: 0.5, radius: 0.18 }, feather: 0.06, ..Mask::default() },
            Mask { shape: MaskShape::Rectangle { center_x: 0.72, center_y: 0.45, width: 0.2, height: 0.12, rotation: 25.0 }, feather: 0.02, opacity: 0.7, ..Mask::default() },
        ],
    };
    let nodes = vec![
        node(src, NodeKind::Source, Default::default()),
        node(mask, NodeKind::Mask, NodeParams { masks: Some(masks), ..Default::default() }),
        node(cut, NodeKind::MaskCutout, Default::default()),
        node(out, NodeKind::Output, Default::default()),
    ];
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 200.0, y: 0.0 })).collect();
    NodeGraph {
        output_node_id: out,
        nodes,
        edges: vec![
            Edge { from_node: src, from_port: 0, to_node: cut, to_port: 0 },
            Edge { from_node: mask, from_port: 0, to_node: cut, to_port: 1 },
            Edge { from_node: cut, from_port: 0, to_node: out, to_port: 0 },
        ],
        node_positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    }
}

/// A graded ramp with colour in every channel, so a multiply that
/// dropped one channel or read the wrong matte channel would show.
fn frame() -> Vec<f32> {
    let mut px = vec![0.0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let (t, v) = (x as f32 / (W - 1) as f32, y as f32 / (H - 1) as f32);
            px[i] = 0.1 + 0.8 * t;
            px[i + 1] = 0.2 + 0.6 * v;
            px[i + 2] = 0.9 - 0.7 * t * v;
            px[i + 3] = 1.0;
        }
    }
    px
}

fn run(g: &NodeGraph, gpu: bool) -> (Vec<f32>, klipp_nodes::executor::ChainStats) {
    let mut exec = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
    let out = exec.execute_linear(&frame(), g, W, H, 0.0, 0, None, None).expect("graph runs").0;
    (out, exec.last_run_chain_stats())
}

#[test]
fn a_mask_cutout_matches_the_cpu() {
    let g = cutout();
    let (cpu, _) = run(&g, false);
    let (gpu, stats) = run(&g, true);
    if stats.extends == 0 && stats.flushes == 0 {
        eprintln!("skipping: no usable GPU");
        return;
    }
    // The cutout itself rode the chain: Source is the one CPU node.
    assert!(stats.cpu_nodes <= 1, "the cutout fell back to the CPU: {stats:?}");
    assert_eq!(cpu.len(), gpu.len());
    let worst = cpu
        .chunks_exact(4)
        .zip(gpu.chunks_exact(4))
        .map(|(a, b)| (0..4).map(|i| (a[i] - b[i]).abs()).fold(0.0f32, f32::max))
        .fold(0.0f32, f32::max);
    // The matte's soft edge is where the two rasterisers can differ by a
    // rounding step (1.5e-3 measured); the picture away from it agrees.
    eprintln!("mask cutout: worst CPU vs GPU = {worst:e} linear ({stats:?})");
    assert!(worst < 0.004, "CPU and GPU disagree by {worst}");
    // And it cut something: black outside both windows, picture inside.
    let at = |p: &[f32], x: u32, y: u32| p[((y * W + x) * 4) as usize];
    assert!(at(&gpu, 2, 2) < 1e-4 && at(&gpu, (0.35 * W as f32) as u32, H / 2) > 0.1, "the window shows the picture and nothing else");
}
