//! A grade's mix or opacity set to 0 is a value: the correction drawn at
//! none of its strength, the picture as if ungraded. Zero used to be
//! written as "unset", which every reader takes as the default, so a mix
//! dragged or typed to 0 drew the full correction (a Blend's half).

use klipp_core::color_grade::{CurveSet, HueSatCurves};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 16;
const H: u32 = 4;

/// Ramps of colour across and down, so a temperature, a curve, a hue
/// curve and a LUT each have something to change.
fn frame() -> Vec<u8> {
    (0..H).flat_map(|y| (0..W).flat_map(move |x| [(x * 255 / (W - 1)) as u8, (y * 255 / (H - 1)) as u8, ((W - 1 - x) * 255 / (W - 1)) as u8, 255])).collect()
}

/// Source → a node of `kind` with `params` → Output.
fn graph(kind: NodeKind, params: NodeParams) -> NodeGraph {
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(kind, params), node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let output_node_id = nodes[2].id;
    NodeGraph { nodes, edges, output_node_id, ..Default::default() }
}

/// Each mix with the correction it mixes: (its name, the node, the
/// correction).
fn corrections() -> Vec<(&'static str, NodeKind, NodeParams)> {
    let lut = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/luts/cinematic/D-CINELIKE -0_3 BASE.cube");
    vec![
        ("global_mix", NodeKind::Corrector, NodeParams { temperature: Some(40.0), ..Default::default() }),
        ("curves_mix", NodeKind::Corrector, NodeParams { curves: Some(CurveSet { master: vec![[0.0, 0.0], [0.5, 0.8], [1.0, 1.0]], ..Default::default() }), ..Default::default() }),
        ("huesat_mix", NodeKind::Corrector, NodeParams { huesat: Some(HueSatCurves { hue_vs_sat: vec![[0.0, 0.2], [1.0, 0.2]], ..Default::default() }), ..Default::default() }),
        ("lut_opacity", NodeKind::Lut3D, NodeParams { lut_path: Some(lut), ..Default::default() }),
    ]
}

#[test]
fn a_mix_set_to_zero_draws_the_picture_ungraded() {
    for (path, mut ex) in [("cpu", GraphExecutor::new_cpu_only()), ("default", GraphExecutor::new())] {
        let ungraded = ex.execute(&frame(), &graph(NodeKind::Corrector, NodeParams::default()), W, H, 0.0, 0, None, None).unwrap().0;
        for (mix, kind, correction) in corrections() {
            let full = ex.execute(&frame(), &graph(kind, correction.clone()), W, H, 0.0, 0, None, None).unwrap().0;
            assert_ne!(full, ungraded, "{mix} ({path}): the correction does something at full mix");
            let mut none = correction;
            assert!(none.apply_override(mix, 0.0), "{mix}: a known name");
            let drawn = ex.execute(&frame(), &graph(kind, none), W, H, 0.0, 0, None, None).unwrap().0;
            let worst = drawn.iter().zip(&ungraded).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(worst <= 1, "{mix} ({path}): at 0 the picture is {worst} levels from ungraded");
        }
    }
}

/// Zero is stored as zero, and comes back as zero through the saved form,
/// for every mix and opacity whose default isn't 0 (a Blend's mix among
/// them).
#[test]
fn a_mix_set_to_zero_is_stored_and_saved_as_zero() {
    for name in ["global_mix", "curves_mix", "huesat_mix", "lut_opacity", "mix"] {
        let mut p = NodeParams::default();
        assert!(p.apply_override(name, 0.0), "{name}: a known name");
        let stored = serde_json::to_value(&p).unwrap();
        assert_eq!(stored[name], serde_json::json!(0.0), "{name}: stored as {stored}");
        let back: NodeParams = serde_json::from_value(stored).unwrap();
        assert_eq!(back, p, "{name}: read back as it was stored");
    }
}
