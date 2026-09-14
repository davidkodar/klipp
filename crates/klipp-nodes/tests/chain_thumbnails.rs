//! Node thumbnails taken inside the GPU chain are the CPU thumbnails.
//!
//! With a GPU, a node the editor wants a thumbnail of stays on the GPU
//! chain and its thumbnail is downsampled there; without one, it runs on
//! the CPU and is downsampled from its pixels. The editor must show the
//! same pictures either way — for a chained grade node, a bypassed node
//! tunnelled through the chain, and a Source seeded straight onto the GPU.

use std::collections::HashSet;
use std::sync::Arc;

use klipp_nodes::{Edge, GraphExecutor, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};

const W: u32 = 96;
const H: u32 = 54;

fn graph() -> NodeGraph {
    let node = |kind, params| Node {
        id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params,
        keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let exposure = NodeParams {
        exposure: Some(0.7),
        ..Default::default()
    };
    let saturation = NodeParams {
        saturation: Some(1.4),
        ..Default::default()
    };
    let mut bypassed = node(NodeKind::Contrast, NodeParams { contrast: Some(0.3), ..Default::default() });
    bypassed.enabled = false;
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::Exposure, exposure),
        bypassed,
        node(NodeKind::Saturation, saturation),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..nodes.len() - 1).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes[4].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn bytes() -> Vec<u8> {
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = (i % W, i / W);
            [(x * 255 / (W - 1)) as u8, (y * 255 / (H - 1)) as u8, ((x + y) * 3 % 256) as u8, 255]
        })
        .collect()
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

#[test]
fn gpu_chain_thumbnails_are_the_cpu_thumbnails() {
    let _ = tracing_subscriber::fmt().with_max_level(tracing::Level::WARN).try_init();
    let graph = graph();
    let visible: HashSet<NodeId> = graph.nodes.iter().filter(|n| n.kind != NodeKind::Output).map(|n| n.id).collect();
    let bytes = Arc::new(bytes());
    let linear = klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes);

    let mut cpu = GraphExecutor::new_cpu_only();
    let (want_out, _, want) = cpu.execute_linear_with_thumbs(&linear, &graph, W, H, 0.0, 0, None, None, Some(&visible)).unwrap();
    assert_eq!(want.len(), visible.len(), "the CPU run thumbnails every visible node");

    // The thumbnails have to be each node's own picture. Everything below
    // compares the GPU's to the CPU's, and would pass just as happily if
    // the thumbnail machinery handed back the *Source's* picture for every
    // node — both paths would do it identically. So pin the shape of the
    // chain first, on the CPU reference, which the GPU is then held to.
    //
    // The graph is Source → Exposure(0.7) → Contrast(bypassed) →
    // Saturation(1.4), which gives three relationships worth stating:
    // each enabled node changes the picture, and the disabled one does
    // not. Measured: 48, 0 and 167 levels.
    {
        let at = |i: usize| &want[&graph.nodes[i].id];
        let (source, exposure, bypassed, saturation) = (at(0), at(1), at(2), at(3));

        let d = worst(source, exposure);
        assert!(
            d > 16,
            "the Exposure thumbnail is only {d} levels from the Source's, so these \
             thumbnails may not be each node's own picture",
        );

        // A bypassed node passes its input through, so its thumbnail is
        // its input's — exactly, not approximately.
        let d = worst(exposure, bypassed);
        assert_eq!(
            d, 0,
            "the bypassed Contrast node's thumbnail is {d} levels from its input's, \
             but a disabled node should tunnel the picture unchanged",
        );

        let d = worst(bypassed, saturation);
        assert!(
            d > 16,
            "the Saturation thumbnail is only {d} levels from the node before it, so \
             a later node's effect is not reaching its thumbnail",
        );
    }

    let mut gpu = GraphExecutor::new();
    // From CPU pixels, and from pixels seeded onto the GPU.
    let from_cpu = gpu.execute_linear_with_thumbs(&linear, &graph, W, H, 0.0, 0, None, None, Some(&visible)).unwrap();
    if gpu.gpu_pipeline_mut().is_none() {
        eprintln!("skipping: no GPU");
        return;
    }
    assert!(gpu.prime_chain_from_srgb8(&bytes, W, H), "the seed was refused");
    let seeded = gpu.execute_linear_with_thumbs(&[], &graph, W, H, 0.0, 0, None, None, Some(&visible)).unwrap();

    for (label, (out, _, thumbs)) in [("CPU source", from_cpu), ("seeded source", seeded)] {
        assert!(worst(
            &klipp_effects::linear_f32_rgba_to_srgb_u8(&out),
            &klipp_effects::linear_f32_rgba_to_srgb_u8(&want_out),
        ) <= 1, "{label}: the graded frame changed");
        for n in graph.nodes.iter().filter(|n| visible.contains(&n.id)) {
            let got = thumbs.get(&n.id).unwrap_or_else(|| panic!("{label}: no thumbnail for {:?}", n.kind));
            let d = worst(got, &want[&n.id]);
            eprintln!("{label}: {:?} thumbnail vs CPU: worst {d}", n.kind);
            assert!(d <= 1, "{label}: the {:?} thumbnail is {d} levels off the CPU's", n.kind);
        }
    }
}
