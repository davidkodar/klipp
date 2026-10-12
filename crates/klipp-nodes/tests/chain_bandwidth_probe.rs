//! Is the GPU chain bandwidth-bound or compute-bound?
//!
//! Measured to answer one question: should the chain store f16 instead
//! of f32? That would halve the bytes every dispatch reads and writes
//! and change nothing about the arithmetic, so it only pays if the
//! dispatches are waiting on memory. The `pack` / `unpack` hooks in the
//! shaders are identity functions that exist as the seam for exactly
//! that change.
//!
//! The test is a node with almost no maths (Exposure — one multiply)
//! against one with a lot (Corrector — matrices and curves), at
//! identical memory traffic per pixel.
//!
//! **The answer was no, and this is the evidence.** At 4K, per extra
//! chain node:
//!
//! | node | cost | what it is |
//! |---|---|---|
//! | Exposure | 0.44-0.62 ms | ~66 MB moved — the memory floor |
//! | Corrector | ~3.0 ms | the same traffic plus the arithmetic |
//!
//! (Two runs, hence the range on the cheap node; the expensive one was
//! 3.09 and 3.00.) So a real grade node spends between a seventh and a
//! fifth of its time on memory and the rest on maths. Halving the
//! traffic would take a Corrector from ~3.0 ms to ~2.7 — roughly 10%,
//! not the 50% the change sounds like — and a ten-node stack at 4K from
//! ~30 ms to ~27 ms.
//!
//! Against that: f16 carries a 10-bit mantissa, which is a ~1e-3
//! relative step, and the colour parity tests this pipeline is held
//! together by assert between 1e-3 and 1e-7. Every one of those
//! tolerances would have to be loosened, in a grading application,
//! across 143 buffer declarations and 25 shaders, to buy 10% off the
//! part of the frame the chain accounts for. That is the wrong trade,
//! and it is only obvious once the two numbers above sit next to each
//! other.
//!
//! If chain time needs to come down, this says the lever is arithmetic —
//! fewer, fatter dispatches, or cheaper maths inside them — not bytes.
//! The executor already fuses contrast+clarity and
//! saturation+vibrance into single dispatches for that reason.
//!
//! Re-run it before revisiting the question; a GPU with a different
//! compute-to-bandwidth ratio could reach a different answer.
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 3840;
const H: u32 = 2160;

fn stack(kind: NodeKind, params: NodeParams, n: usize) -> NodeGraph {
    let src = NodeId::new_v4();
    let out = NodeId::new_v4();
    let mids: Vec<NodeId> = (0..n).map(|_| NodeId::new_v4()).collect();
    let mk = |id: NodeId, k: NodeKind, p: NodeParams| Node {
        id, kind: k, label: format!("{k:?}"), enabled: true, params: p,
        keyframes: Default::default(), output_link: None, input_links: Vec::new(),
    };
    let mut nodes = vec![mk(src, NodeKind::Source, Default::default())];
    for id in &mids {
        nodes.push(mk(*id, kind, params.clone()));
    }
    nodes.push(mk(out, NodeKind::Output, Default::default()));
    let mut edges = Vec::new();
    let mut prev = src;
    for id in &mids {
        edges.push(Edge { from_node: prev, from_port: 0, to_node: *id, to_port: 0 });
        prev = *id;
    }
    edges.push(Edge { from_node: prev, from_port: 0, to_node: out, to_port: 0 });
    let node_positions = nodes.iter().enumerate()
        .map(|(i, nd)| (nd.id, NodePos { x: i as f32 * 160.0, y: 0.0 })).collect();
    NodeGraph {
        nodes, edges, output_node_id: out, node_positions,
        working_space_override: None, frames: Vec::new(), sticky_notes: Vec::new(),
    }
}

/// Whether a run's timing is the GPU's: its GPU is still up and the
/// chain carried the run (#1158). A chain readback that times out marks
/// the GPU lost and the run goes on on the CPU; a time taken then is the
/// CPU's, never a GPU figure.
fn ran_on_the_gpu(gpu_up: bool, stats: klipp_nodes::executor::ChainStats) -> bool {
    gpu_up && stats.extends + stats.flushes > 0
}

/// The executor's GPU is attached and not marked lost.
fn gpu_up(exec: &mut GraphExecutor) -> bool {
    exec.gpu_pipeline_mut().is_some_and(|g| !g.is_device_lost())
}

/// A run the CPU carried, or one with no GPU, is never taken for a GPU
/// timing (#1158).
#[test]
fn a_cpu_fallback_is_no_gpu_timing() {
    use klipp_nodes::executor::ChainStats;
    let gpu_run = ChainStats { extends: 9, flushes: 1, ..Default::default() };
    assert!(ran_on_the_gpu(true, gpu_run));
    assert!(!ran_on_the_gpu(false, gpu_run), "the GPU was lost (a readback timed out)");
    assert!(!ran_on_the_gpu(true, ChainStats::default()), "the chain never ran");
}

#[test]
fn chain_is_bandwidth_or_compute_bound() {
    let _ = tracing_subscriber::fmt().with_max_level(tracing::Level::DEBUG)
        .with_target(false).without_time().try_init();
    let frame = vec![0.35f32; (W * H * 4) as usize];
    let mut exec = GraphExecutor::new();

    let cases: Vec<(&str, NodeKind, NodeParams)> = vec![
        ("exposure (1 multiply)", NodeKind::Exposure,
         NodeParams { exposure: Some(0.35), ..Default::default() }),
        ("corrector (matrices+curves)", NodeKind::Corrector,
         NodeParams {
             temperature: Some(-15.0), tint: Some(3.0), shadows: Some(0.006),
             contrast: Some(0.03), highlights: Some(-0.015), clarity: Some(0.05),
             ..Default::default()
         }),
    ];
    for (name, kind, params) in cases {
        let mut prev = 0.0;
        for n in [2usize, 10] {
            let g = stack(kind, params.clone(), n);
            exec.execute_linear(&frame, &g, W, H, 0.0, 0, None, None).expect("the warm-up runs");
            // The executor takes its GPU on its first run.
            if !gpu_up(&mut exec) {
                eprintln!("=== unavailable: no GPU, so no chain timing to report");
                return;
            }
            // Only samples the GPU carried are timed.
            let mut best = f64::MAX;
            let mut degraded = 0;
            for _ in 0..3 {
                let t = std::time::Instant::now();
                let r = exec.execute_linear(&frame, &g, W, H, 0.0, 0, None, None);
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                assert!(r.is_ok(), "{name}, {n} nodes: {:?}", r.err());
                if ran_on_the_gpu(gpu_up(&mut exec), exec.last_run_chain_stats()) {
                    best = best.min(ms);
                } else {
                    degraded += 1;
                }
            }
            if best == f64::MAX {
                eprintln!("=== degraded: {name} at {n} nodes fell back to the CPU in all {degraded} samples; no GPU timing reported");
                break;
            }
            if degraded > 0 {
                eprintln!("=== {name} at {n} nodes: {degraded} of 3 samples fell back to the CPU and were left out");
            }
            if n == 2 { prev = best; } else {
                let per_node = (best - prev) / 8.0;
                eprintln!("=== {name}: {prev:.1} ms at 2 nodes, {best:.1} at 10 \
                           -> {per_node:.2} ms per extra chain node at 4K");
            }
        }
    }
}
