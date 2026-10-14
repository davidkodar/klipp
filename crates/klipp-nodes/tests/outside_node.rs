//! A correction whose gate takes its key inverted (`key_invert`, #898)
//! grades where the key isn't: an outside node reading the same key as
//! the inside one. Inverting the gate is inverting the matte; the two
//! together grade every pixel once; a key two corrections read draws the
//! same on the GPU as on the CPU; an empty key, which selects everything,
//! inverted grades nothing.

use klipp_core::{Mask, MaskShape, MaskStack};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 64;
const H: u32 = 36;

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

fn link(a: NodeId, b: NodeId, port: u8) -> Edge {
    Edge { from_node: a, from_port: 0, to_node: b, to_port: port }
}

fn window(feather: f32, invert: bool, shapes: bool) -> Node {
    let shape = MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.4, height: 0.4, rotation: 0.0 };
    let masks = shapes.then(|| MaskStack { masks: vec![Mask { shape, feather, invert, ..Mask::default() }] });
    node(NodeKind::Mask, NodeParams { masks, ..Default::default() })
}

fn corrector(exposure: f32, key_invert: bool) -> Node {
    node(NodeKind::Corrector, NodeParams { exposure: Some(exposure), key_invert: key_invert.then_some(true), ..Default::default() })
}

/// Source -> each of `corrections` in turn -> Output, `key` into every
/// one's port 1.
fn graph(key: Node, corrections: Vec<Node>) -> NodeGraph {
    let s = node(NodeKind::Source, NodeParams::default());
    let o = node(NodeKind::Output, NodeParams::default());
    let mut edges = vec![link(s.id, key.id, 0)];
    let mut prev = s.id;
    for c in &corrections {
        edges.extend([link(prev, c.id, 0), link(key.id, c.id, 1)]);
        prev = c.id;
    }
    edges.push(link(prev, o.id, 0));
    let mut nodes = vec![s, key, o.clone()];
    nodes.extend(corrections);
    NodeGraph { output_node_id: o.id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn picture() -> Vec<f32> {
    (0..W * H).flat_map(|i| {
        let (x, y) = ((i % W) as f32 / W as f32, (i / W) as f32 / H as f32);
        [0.05 + 0.5 * x, 0.1 + 0.3 * y, 0.2 + 0.2 * x * y, 1.0]
    }).collect()
}

fn run(ex: &mut GraphExecutor, g: &NodeGraph) -> Vec<f32> {
    ex.execute_linear(&picture(), g, W, H, 0.0, 0, None, None).unwrap().0
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// A gate taking its soft key inverted grades as one whose window is
/// itself inverted: `1 - coverage`, at every soft edge too.
#[test]
fn an_inverted_gate_is_an_inverted_matte() {
    let cpu = &mut GraphExecutor::new_cpu_only();
    let by_gate = run(cpu, &graph(window(0.3, false, true), vec![corrector(1.0, true)]));
    let by_matte = run(cpu, &graph(window(0.3, true, true), vec![corrector(1.0, false)]));
    assert!(worst(&by_gate, &by_matte) < 1e-5, "{}", worst(&by_gate, &by_matte));
    assert!(worst(&by_gate, &picture()) > 0.05, "it grades somewhere");
}

/// Inside and outside, the same hard key: every pixel graded once, a stop
/// up across the whole frame (the window's edge pixels aside).
#[test]
fn inside_and_outside_grade_every_pixel_once() {
    let out = run(&mut GraphExecutor::new_cpu_only(), &graph(window(0.0, false, true), vec![corrector(1.0, false), corrector(1.0, true)]));
    let src = picture();
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
            let edge = [(fx - 0.3).abs(), (fx - 0.7).abs(), (fy - 0.3).abs(), (fy - 0.7).abs()].iter().any(|d| *d < 2.0 / W as f32);
            if edge {
                continue;
            }
            for c in 0..3 {
                assert!((out[i + c] - 2.0 * src[i + c]).abs() < 1e-4, "({x}, {y}) channel {c}: {} for {}", out[i + c], 2.0 * src[i + c]);
            }
        }
    }
}

/// A key two corrections read, the inside one and the outside one: the
/// GPU draws what the CPU draws. With two readers the key leaves the
/// single-reader route a lone gate's key takes on the GPU (it is drawn
/// once and read by both), which this holds.
#[test]
fn a_key_read_inside_and_outside_draws_the_same_on_the_gpu() {
    let key = || node(NodeKind::Qualifier, NodeParams {
        qualifier: Some(klipp_core::QualifierSettings { enabled: true, hue_center: 30.0, hue_width: 90.0, sat_low: 0.05, ..Default::default() }),
        ..Default::default()
    });
    let g = graph(key(), vec![corrector(1.0, false), corrector(-0.5, true)]);
    let cpu = run(&mut GraphExecutor::new_cpu_only(), &g);
    assert!(worst(&cpu, &picture()) > 0.05, "it grades");
    let mut ex = GraphExecutor::new();
    if ex.gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let gpu = run(&mut ex, &g);
    assert!(worst(&cpu, &gpu) < 2e-3, "GPU against CPU: {}", worst(&cpu, &gpu));
    // And the outside one alone, its gate the chain's.
    let alone = graph(key(), vec![corrector(-0.5, true)]);
    let (c, gp) = (run(&mut GraphExecutor::new_cpu_only(), &alone), run(&mut GraphExecutor::new(), &alone));
    assert!(worst(&c, &gp) < 2e-3, "a lone outside node, GPU against CPU: {}", worst(&c, &gp));
}

/// A window with no shapes selects everything, as every matte reader
/// takes it: the inside node grades the whole frame, and the outside one,
/// its key inverted, grades nothing.
#[test]
fn an_empty_key_inverted_grades_nothing() {
    let cpu = &mut GraphExecutor::new_cpu_only();
    let src = picture();
    let outside = run(cpu, &graph(window(0.0, false, false), vec![corrector(1.0, true)]));
    let inside = run(cpu, &graph(window(0.0, false, false), vec![corrector(1.0, false)]));
    let doubled: Vec<f32> = src.chunks_exact(4).flat_map(|p| [2.0 * p[0], 2.0 * p[1], 2.0 * p[2], p[3]]).collect();
    assert!(worst(&inside, &doubled) < 1e-4, "no shapes, as every matte reader takes it: everything, graded");
    assert!(worst(&outside, &src) < 1e-4, "inverted: nothing graded");
}

/// Adding an outside node puts a Corrector straight after the inside one
/// on the chain, reading the same key with the gate the other way round;
/// the chain's reader of the inside node now reads it. Applied to a gated
/// correction, the two grade every pixel once.
#[test]
fn adding_an_outside_node_wires_it_after_the_inside_one() {
    let mut g = graph(window(0.0, false, true), vec![corrector(1.0, false)]);
    let inside = g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().id;
    let key = g.input_of(inside, 1).unwrap();
    let outside = g.add_outside_node(inside).expect("a gated correction on the chain");
    let n = g.nodes.iter().find(|n| n.id == outside).unwrap();
    assert_eq!((n.kind, n.params.key_invert), (NodeKind::Corrector, Some(true)));
    assert_eq!(g.input_of(outside, 0), Some(inside));
    assert_eq!(g.input_of(outside, 1), Some(key), "the same key");
    assert_eq!(g.input_of(g.output_node_id, 0), Some(outside), "the chain's reader moved to it");
    // Its own exposure set, the two grade the frame once.
    g.nodes.iter_mut().find(|n| n.id == outside).unwrap().params.exposure = Some(1.0);
    let out = run(&mut GraphExecutor::new_cpu_only(), &g);
    let i = ((2 * W + 2) * 4) as usize;
    assert!((out[i] - 2.0 * picture()[i]).abs() < 1e-4, "outside the key, graded by the outside node");
    // An outside node's own outside node is inside again.
    let again = g.add_outside_node(outside).unwrap();
    assert_eq!(g.nodes.iter().find(|n| n.id == again).unwrap().params.key_invert, None);
}

/// Only the chain's reader moves: a Layer Mixer reading the inside node
/// on a side input keeps reading it.
#[test]
fn a_side_reader_keeps_reading_the_inside_node() {
    let mut g = graph(window(0.0, false, true), vec![corrector(1.0, false)]);
    let inside = g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().id;
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    let mixer = node(NodeKind::LayerMixer, NodeParams::default());
    let mixer_id = mixer.id;
    g.nodes.push(mixer);
    // Source on top, the inside node below it; the mixer itself feeds
    // nothing, a side branch.
    g.edges.extend([link(source, mixer_id, 0), link(inside, mixer_id, 1)]);
    let outside = g.add_outside_node(inside).unwrap();
    assert_eq!(g.input_of(mixer_id, 1), Some(inside), "the side reader kept");
    assert_eq!(g.input_of(g.output_node_id, 0), Some(outside), "the chain's moved");
}

/// No outside node for a correction no key gates, for a node that is no
/// correction, or for one off the chain.
#[test]
fn an_outside_node_needs_a_keyed_correction_on_the_chain() {
    let mut g = graph(window(0.0, false, true), vec![corrector(1.0, false)]);
    let inside = g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().id;
    let key = g.input_of(inside, 1).unwrap();
    let before = g.clone();
    assert!(g.add_outside_node(key).is_none(), "the key itself is no correction");
    g.edges.retain(|e| !(e.to_node == inside && e.to_port == 1));
    let unkeyed = g.clone();
    assert!(g.add_outside_node(inside).is_none(), "no key");
    assert_eq!(serde_json::to_value(&g).unwrap(), serde_json::to_value(&unkeyed).unwrap(), "left as it was");
    let mut off = before.clone();
    let side = corrector(1.0, false);
    let side_id = side.id;
    off.nodes.push(side);
    off.edges.extend([link(key, side_id, 1), link(off.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id, side_id, 0)]);
    assert!(off.add_outside_node(side_id).is_none(), "off the chain");
}
