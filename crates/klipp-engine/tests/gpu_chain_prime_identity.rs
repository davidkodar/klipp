//! Phase 4d.2 end-to-end test: `prime_chain_from_gpu_u8_buffer` +
//! `execute_linear` with a real single-node grade graph produces
//! byte-close output to the plain `execute_linear` CPU-upload path.
//!
//! Proves the seam works end to end: a decoder-shaped u8 RGBA buffer
//! on GPU → bridge to linear into `buf_a` → single-node ColorBalance
//! chain → download → compare. When this test passes, the engine
//! wiring (phase 4d.3) is purely plumbing — the numerics are already
//! certified.

use std::sync::Arc;

use klipp_effects::{
    gpu_context::GpuContext,
    srgb_u8_to_linear_f32_rgba,
};
use klipp_nodes::{
    executor::GraphExecutor,
    graph::{Edge, Node, NodeGraph, NodeId, NodePos},
    kinds::NodeKind,
    params::{NodeKeyframeTracks, NodeParams},
};

const W: u32 = 32;
const H: u32 = 32;

fn color_balance_graph() -> NodeGraph {
    let source = NodeId::new_v4();
    let colorbal = NodeId::new_v4();
    let output = NodeId::new_v4();

    let mut p = NodeParams::default();
    p.temperature = Some(0.10);
    p.tint = Some(-0.05);
    p.hue = Some(0.12);
    p.global_mix = Some(1.0);

    let nodes = vec![
        Node {
            id: source, kind: NodeKind::Source, label: "Source".into(),
            enabled: true, params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None, input_links: vec![],
        },
        Node {
            id: colorbal, kind: NodeKind::ColorBalance, label: "CB".into(),
            enabled: true, params: p,
            keyframes: NodeKeyframeTracks::default(),
            output_link: None, input_links: vec![],
        },
        Node {
            id: output, kind: NodeKind::Output, label: "Output".into(),
            enabled: true, params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None, input_links: vec![],
        },
    ];
    let edges = vec![
        Edge { from_node: source, from_port: 0, to_node: colorbal, to_port: 0 },
        Edge { from_node: colorbal, from_port: 0, to_node: output, to_port: 0 },
    ];
    let mut positions = std::collections::HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        positions.insert(n.id, NodePos { x: (i as f32) * 180.0, y: 0.0 });
    }
    NodeGraph { nodes, edges, output_node_id: output, node_positions: positions, working_space_override: None, frames: Vec::new(), sticky_notes: Vec::new() }
}

fn pattern_u8(width: u32, height: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        for col in 0..width {
            let r = ((col * 11 + row * 3) & 0xFF) as u8;
            let g = ((col * 5 + row * 17) & 0xFF) as u8;
            let b = ((col * 23 + row * 7) & 0xFF) as u8;
            v.extend_from_slice(&[r, g, b, 255]);
        }
    }
    v
}

#[test]
fn prime_chain_from_u8_matches_cpu_upload_path() {
    let Some(ctx) = GpuContext::shared() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let bytes = pattern_u8(W, H);
    let linear_cpu = srgb_u8_to_linear_f32_rgba(&bytes);
    let graph = color_balance_graph();

    // ─── Path A: classic CPU upload ──────────────────────────────
    // Source linear f32 → execute_linear → chain uploads CPU data.
    let mut exec_a = GraphExecutor::new();
    if !exec_a.has_gpu() {
        eprintln!("skipping: no GPU executor");
        return;
    }
    let (out_a, _) = exec_a
        .execute_linear(&linear_cpu, &graph, W, H, 0.0, 0, None, None)
        .expect("CPU-upload path must succeed");

    // ─── Path B: prime from u8 on-GPU, then execute_linear ───────
    // A real decoder hands off a `wgpu::Buffer` (STORAGE | COPY_SRC)
    // on the shared device; build one here that looks the same.
    let input_buf = Arc::new(ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("chain_prime_test_input"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }));
    ctx.queue.write_buffer(&input_buf, 0, &bytes);
    ctx.device.poll(wgpu::Maintain::Poll);

    let mut exec_b = GraphExecutor::new();
    assert!(
        exec_b.prime_chain_from_gpu_u8_buffer(&input_buf, W, H),
        "prime_chain_from_gpu_u8_buffer must succeed"
    );
    // `linear_cpu` is still passed because execute_linear stores the
    // Source node's output as Arc<Vec<f32>> for any CPU-consuming
    // downstream code. Our graph is chain-only so nothing reads it,
    // but passing a correctly-sized slice keeps the executor's
    // internal invariants happy.
    let (out_b, _) = exec_b
        .execute_linear(&linear_cpu, &graph, W, H, 0.0, 0, None, None)
        .expect("GPU-primed path must succeed");

    assert_eq!(out_a.len(), out_b.len(), "output size mismatch");

    // sRGB→linear via the GPU bridge's pow() vs CPU's f64 pow() can
    // drift by about an LSB. After a grade pass that drift can
    // amplify slightly. 0.005 covers the measured range comfortably
    // without masking a real divergence.
    let eps = 0.005_f32;
    let mut max_delta = 0.0_f32;
    for (i, (a, b)) in out_a.iter().zip(out_b.iter()).enumerate() {
        let d = (a - b).abs();
        if d > max_delta { max_delta = d; }
        assert!(
            d < eps,
            "channel {i}: cpu={a:.6} gpu-primed={b:.6} delta={d:.6} > {eps}"
        );
    }
    eprintln!("prime_chain identity: max channel delta = {max_delta:.6}");
}
