//! A key node's matte is the same on the CPU and the GPU, whatever comes
//! in (#1037):
//!
//! * a qualifier that is switched off selects the whole frame, as
//!   `NodeParams::apply_override` documents, rather than keying its
//!   window regardless;
//! * a luma key's matte is opaque (alpha 1), as the CPU writes it. The
//!   GPU used to pass the input's alpha through, so over a
//!   part-transparent picture (a title) the two disagreed. A difference
//!   key's per-node matte was already opaque on both (the GPU path builds
//!   it with the same CPU code); its test pins that.

use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 64;
const H: u32 = 48;

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node {
        id: NodeId::new_v4(),
        kind,
        label: format!("{kind:?}"),
        enabled: true,
        params,
        keyframes: Default::default(),
        output_link: None,
        input_links: Vec::new(),
    }
}

/// Source → `kind` → Output.
fn key_graph(kind: NodeKind, params: NodeParams) -> NodeGraph {
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(kind, params), node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// A colourful ramp, half transparent: what a title brings to a key.
fn frame() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[fx, fy * 0.8, 1.0 - fx * 0.7, 0.5]);
        }
    }
    v
}

/// The graph on the CPU and on the GPU, or `None` with no usable GPU.
fn both(graph: &NodeGraph) -> Option<(Vec<f32>, Vec<f32>)> {
    let run = |mut exec: GraphExecutor| exec.execute_linear(&frame(), graph, W, H, 0.0, 0, None, None).expect("graph runs").0;
    let cpu = run(GraphExecutor::new_cpu_only());
    let mut gpu_exec = GraphExecutor::new();
    gpu_exec.gpu_pipeline_or_init()?;
    Some((cpu, run(gpu_exec)))
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn a_switched_off_qualifier_selects_the_whole_frame_on_both() {
    let q = klipp_core::QualifierSettings {
        enabled: false,
        hue_center: 200.0,
        hue_width: 20.0,
        hue_soft: 5.0,
        sat_low: 0.2,
        sat_high: 0.9,
        sat_soft: 0.05,
        lum_low: 0.1,
        lum_high: 0.5,
        lum_soft: 0.04,
        ..Default::default()
    };
    let graph = key_graph(NodeKind::Qualifier, NodeParams { qualifier: Some(q), ..Default::default() });
    let run = |mut exec: GraphExecutor| exec.execute_linear(&frame(), &graph, W, H, 0.0, 0, None, None).expect("graph runs").0;
    let cpu = run(GraphExecutor::new_cpu_only());
    assert!(cpu.chunks_exact(4).all(|p| p[0] == 1.0), "the CPU keyed a window with the qualifier off");
    if let Some((_, gpu)) = both(&graph) {
        assert!(gpu.chunks_exact(4).all(|p| p[0] == 1.0), "the GPU keyed a window with the qualifier off");
    }
}

#[test]
fn a_luma_keys_matte_is_opaque_on_both() {
    let lk = klipp_core::LumaKeySettings { enabled: true, low: 0.2, high: 0.7, soft: 0.1, ..Default::default() };
    let graph = key_graph(NodeKind::LumaKey, NodeParams { luma_key: Some(lk.clone()), ..Default::default() });
    let Some((cpu, gpu)) = both(&graph) else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    assert!(cpu.chunks_exact(4).all(|p| p[3] == 1.0), "the CPU matte is opaque");
    assert!(gpu.chunks_exact(4).all(|p| p[3] == 1.0), "the GPU matte carried the input's alpha");
    // The CPU keys the 8-bit encoded picture (`generate_luma_key_matte`
    // speaks u8) and the shader keys floats, so one 8-bit step of input
    // can move the matte by a step over the soft edge: (1/255)/soft. That
    // precision gap predates #1037 (it is the same with an opaque input)
    // and is held to its bound here; the alpha above is #1037's point.
    let bound = (1.0 / 255.0) / lk.soft + 1e-4;
    let d = worst(&cpu, &gpu);
    assert!(d <= bound, "luma key CPU and GPU differ by {d}, beyond the 8-bit keying bound {bound}");
}

fn plate() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp-key-parity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("plate.png");
    let mut img = image::RgbaImage::new(W, H);
    for (x, _y, px) in img.enumerate_pixels_mut() {
        let v = (x * 255 / (W - 1)) as u8;
        *px = image::Rgba([v, 128, 255 - v, 255]);
    }
    img.save(&path).expect("write plate");
    path
}

#[test]
fn a_difference_keys_matte_is_opaque_on_both() {
    let dk = klipp_core::DifferenceKeySettings { enabled: true, plate_path: Some(plate()), tolerance: 0.1, soft: 0.1, ..Default::default() };
    let graph = key_graph(NodeKind::DifferenceKey, NodeParams { difference_key: Some(dk), ..Default::default() });
    let Some((cpu, gpu)) = both(&graph) else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    assert!(cpu.chunks_exact(4).all(|p| p[3] == 1.0), "the CPU matte is opaque");
    assert!(gpu.chunks_exact(4).all(|p| p[3] == 1.0), "the GPU matte carried the input's alpha");
    let d = worst(&cpu, &gpu);
    assert!(d < 2.0 / 255.0, "difference key CPU and GPU differ by {d}");
}
