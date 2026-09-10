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
    // No `has_gpu()` gate before the first execute: the executor creates
    // its GPU pipeline lazily, so asking beforehand always answers "no"
    // and skips the test. This one had been silently skipping.
    let mut exec_a = GraphExecutor::new();
    let (out_a, _) = exec_a
        .execute_linear(&linear_cpu, &graph, W, H, 0.0, 0, None, None)
        .expect("CPU-upload path must succeed");
    if !exec_a.has_gpu() {
        eprintln!("skipping: no GPU executor on this machine");
        return;
    }

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

/// The engine now skips the GPU→CPU readback on the playback path and
/// hands the executor an **empty** source, relying on the pre-seeded
/// chain. This proves that produces the same pixels as the CPU-upload
/// path — the whole change is worthless if it renders differently.
///
/// Also pins the safety contract: an empty source *without* a seed must
/// be refused rather than graded into a black frame.
#[test]
fn empty_source_with_primed_chain_matches_the_cpu_upload_path() {
    let Some(ctx) = GpuContext::shared() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let bytes = pattern_u8(W, H);
    let linear_cpu = srgb_u8_to_linear_f32_rgba(&bytes);
    let graph = color_balance_graph();

    // Reference: the ordinary path, CPU pixels uploaded by the executor.
    let mut exec_ref = GraphExecutor::new();
    let (out_ref, _) = exec_ref
        .execute_linear(&linear_cpu, &graph, W, H, 0.0, 0, None, None)
        .expect("CPU-upload path must succeed");
    if !exec_ref.has_gpu() {
        eprintln!("skipping: no GPU executor on this machine");
        return;
    }

    let input_buf = Arc::new(ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("empty_source_prime_test_input"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }));
    ctx.queue.write_buffer(&input_buf, 0, &bytes);
    ctx.device.poll(wgpu::Maintain::Poll);

    // What the engine now does: prime, then pass nothing.
    let mut exec_empty = GraphExecutor::new();
    assert!(
        exec_empty.prime_chain_from_gpu_u8_buffer(&input_buf, W, H),
        "prime must succeed",
    );
    let (out_empty, _) = exec_empty
        .execute_linear(&[], &graph, W, H, 0.0, 0, None, None)
        .expect("primed chain must run without any CPU source");

    assert_eq!(out_ref.len(), out_empty.len(), "output size mismatch");
    let eps = 0.005_f32;
    let mut max_delta = 0.0_f32;
    for (i, (a, b)) in out_ref.iter().zip(out_empty.iter()).enumerate() {
        let d = (a - b).abs();
        if d > max_delta {
            max_delta = d;
        }
        assert!(
            d < eps,
            "channel {i}: cpu-upload={a:.6} empty-source={b:.6} delta={d:.6} > {eps}"
        );
    }
    eprintln!("empty-source identity: max channel delta = {max_delta:.6}");

    // And the contract: no seed, no pixels, no silent black frame.
    let mut exec_unseeded = GraphExecutor::new();
    let err = exec_unseeded
        .execute_linear(&[], &graph, W, H, 0.0, 0, None, None)
        .expect_err("an empty source without a primed chain must be refused");
    assert!(
        matches!(err, klipp_nodes::executor::ExecutorError::CpuSourceRequired),
        "expected CpuSourceRequired, got {err:?}",
    );
}

/// A seed is only valid at the resolution it was made at.
///
/// `ensure_chain_buffers` never shrinks an existing allocation, so a
/// full-res seed consumed by a half-res chain would be read at the wrong
/// stride and scramble the image rather than fail. The drag-downsample
/// path does exactly that: it primes at full res, then grades at half.
/// The executor must refuse the seed instead of reinterpreting it.
#[test]
fn a_seed_made_at_another_resolution_is_refused_not_reinterpreted() {
    let Some(ctx) = GpuContext::shared() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let bytes = pattern_u8(W, H);
    let graph = color_balance_graph();

    let input_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("wrong_res_seed_test_input"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.queue.write_buffer(&input_buf, 0, &bytes);
    ctx.device.poll(wgpu::Maintain::Wait);

    let mut exec = GraphExecutor::new();
    if !exec.prime_chain_from_gpu_u8_buffer(&input_buf, W, H) {
        eprintln!("skipping: prime refused (no GPU)");
        return;
    }
    // Same executor, same seed, half the resolution and no CPU pixels.
    let err = exec
        .execute_linear(&[], &graph, W / 2, H / 2, 0.0, 0, None, None)
        .expect_err("a seed at the wrong resolution must not be consumed");
    assert!(
        matches!(err, klipp_nodes::executor::ExecutorError::CpuSourceRequired),
        "expected CpuSourceRequired, got {err:?}",
    );
}

/// A seed the run does not consume must not survive into the next run.
///
/// Otherwise the next frame — decoded in software, supplying real CPU
/// pixels — would skip its upload and grade the previous frame's buffer.
#[test]
fn an_unconsumed_seed_does_not_leak_into_the_next_run() {
    let Some(ctx) = GpuContext::shared() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let graph = color_balance_graph();

    // Two visibly different frames. `stale` gets seeded onto the GPU and
    // then abandoned; `fresh` arrives as ordinary CPU pixels.
    let stale = pattern_u8(W, H);
    let fresh: Vec<u8> = stale.iter().map(|b| 255u8 - b).collect();
    let fresh_linear = srgb_u8_to_linear_f32_rgba(&fresh);

    // Reference: `fresh` through the ordinary CPU-upload path.
    let mut exec_ref = GraphExecutor::new();
    let (out_ref, _) = exec_ref
        .execute_linear(&fresh_linear, &graph, W, H, 0.0, 0, None, None)
        .expect("CPU-upload path must succeed");
    if !exec_ref.has_gpu() {
        eprintln!("skipping: no GPU executor on this machine");
        return;
    }

    let stale_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("stale_seed_test_input"),
        size: stale.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.queue.write_buffer(&stale_buf, 0, &stale);
    ctx.device.poll(wgpu::Maintain::Wait);

    let mut exec = GraphExecutor::new();
    assert!(
        exec.prime_chain_from_gpu_u8_buffer(&stale_buf, W, H),
        "prime must succeed",
    );
    // Abandon the seed: wrong resolution, so this run cannot consume it.
    let _ = exec.execute_linear(&[], &graph, W / 2, H / 2, 0.0, 0, None, None);

    // Next run, real pixels, right resolution. Must grade `fresh`.
    let (out_next, _) = exec
        .execute_linear(&fresh_linear, &graph, W, H, 0.0, 1, None, None)
        .expect("second run with CPU pixels must succeed");

    let mut max_delta = 0.0_f32;
    for (a, b) in out_ref.iter().zip(out_next.iter()) {
        max_delta = max_delta.max((a - b).abs());
    }
    eprintln!("stale-seed check: max channel delta vs fresh reference = {max_delta:.6}");
    assert!(
        max_delta < 0.005,
        "second run graded stale seeded pixels instead of the supplied frame \
         (max delta {max_delta:.6})",
    );
}
