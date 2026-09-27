//! A correction gated by a matte on its port 1 changes only what the
//! matte covers: a qualifier's key (the `qualify` op's secondary) and an
//! image matte (the AI mask's "Apply to clip", #440). No correction node
//! reads port 1, so both graded the whole frame until the executor ran
//! the gated correction through an ApplyMatte. Checked on the CPU and
//! through the default executor, which takes the GPU chain when there
//! is a GPU.

use klipp_nodes::graph::{Edge, Node, NodeGraph};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::{MatteChannel, NodeParams};
use klipp_nodes::GraphExecutor;

const W: u32 = 32;
const H: u32 = 8;

/// Left half red, right half blue.
fn frame() -> Vec<u8> {
    (0..W * H)
        .flat_map(|i| if i % W < W / 2 { [220u8, 30, 30, 255] } else { [30, 30, 220, 255] })
        .collect()
}

fn px(out: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [out[i], out[i + 1], out[i + 2]]
}

fn run(graph: &NodeGraph) -> Vec<Vec<u8>> {
    let mut outs = Vec::new();
    for mut ex in [GraphExecutor::new_cpu_only(), GraphExecutor::new()] {
        outs.push(ex.execute(&frame(), graph, W, H, 0.0, 0, None, None).unwrap().0);
    }
    outs
}

fn assert_left_greyed_right_kept(out: &[u8], what: &str) {
    let [r, g, b] = px(out, 2, 4);
    assert!(r.abs_diff(g) <= 2 && g.abs_diff(b) <= 2, "{what}: the covered red is not desaturated: {:?}", [r, g, b]);
    let right = px(out, W - 3, 4);
    for (got, want) in right.iter().zip([30u8, 30, 220]) {
        assert!(got.abs_diff(want) <= 2, "{what}: the uncovered blue changed: {right:?}");
    }
}

#[test]
fn a_keyed_secondary_changes_only_the_key() {
    let mut g = NodeGraph::default();
    let (q, c) = g.ensure_masked_secondary(NodeKind::Corrector);
    for n in &mut g.nodes {
        if n.id == q {
            n.params.qualifier = Some(klipp_core::QualifierSettings { enabled: true, hue_center: 0.0, hue_width: 20.0, sat_low: 0.2, ..Default::default() });
        }
        if n.id == c {
            n.params.saturation = Some(0.0);
        }
    }
    for out in run(&g) {
        assert_left_greyed_right_kept(&out, "qualifier");
    }
}

#[test]
fn an_image_matte_on_the_grade_limits_it() {
    let dir = std::env::temp_dir().join(format!("klipp-gated-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mask.png");
    image::GrayImage::from_fn(W, H, |x, _| image::Luma([if x < W / 2 { 255 } else { 0 }])).save(&path).unwrap();

    // As "Apply to clip" wires it: an ExternalMatte into the first
    // Corrector's port 1.
    let mut g = NodeGraph::default();
    let corr = g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).map(|n| n.id).unwrap_or_else(|| g.ensure_corrector());
    g.nodes.iter_mut().find(|n| n.id == corr).unwrap().params.saturation = Some(0.0);
    let matte = klipp_nodes::graph::NodeId::new_v4();
    let params = NodeParams { matte_image_path: Some(path.clone()), matte_channel: Some(MatteChannel::Luminance), ..Default::default() };
    g.nodes.push(Node { id: matte, kind: NodeKind::ExternalMatte, label: "AI mask".into(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() });
    g.edges.push(Edge { from_node: matte, from_port: 0, to_node: corr, to_port: 1 });
    for out in run(&g) {
        assert_left_greyed_right_kept(&out, "image matte");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two gated corrections in a row: the second's ungraded picture is
/// the first one's blend, so where neither matte reaches, the picture
/// is untouched. Read from the graph before rewriting, the second took
/// the first's whole-frame grade there.
#[test]
fn chained_gates_each_keep_the_picture_outside_their_matte() {
    let dir = std::env::temp_dir().join(format!("klipp-gated-chain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let left = dir.join("left.png");
    let empty = dir.join("empty.png");
    image::GrayImage::from_fn(W, H, |x, _| image::Luma([if x < W / 2 { 255 } else { 0 }])).save(&left).unwrap();
    image::GrayImage::from_fn(W, H, |_, _| image::Luma([0])).save(&empty).unwrap();

    let mut g = NodeGraph::default();
    let out = g.output_node_id;
    let into_out = g.input_of(out, 0).expect("the default chain feeds the output");
    let (c1, c2, m1, m2) = (klipp_nodes::graph::NodeId::new_v4(), klipp_nodes::graph::NodeId::new_v4(), klipp_nodes::graph::NodeId::new_v4(), klipp_nodes::graph::NodeId::new_v4());
    let node = |id, kind, params: NodeParams| Node { id, kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    let matte = |path: &std::path::Path| NodeParams { matte_image_path: Some(path.to_path_buf()), matte_channel: Some(MatteChannel::Luminance), ..Default::default() };
    g.nodes.push(node(c1, NodeKind::Corrector, NodeParams { saturation: Some(0.0), ..Default::default() }));
    g.nodes.push(node(c2, NodeKind::Corrector, NodeParams { saturation: Some(1.5), ..Default::default() }));
    g.nodes.push(node(m1, NodeKind::ExternalMatte, matte(&left)));
    g.nodes.push(node(m2, NodeKind::ExternalMatte, matte(&empty)));
    g.edges.retain(|e| !(e.to_node == out && e.to_port == 0));
    g.edges.extend([
        Edge { from_node: into_out, from_port: 0, to_node: c1, to_port: 0 },
        Edge { from_node: m1, from_port: 0, to_node: c1, to_port: 1 },
        Edge { from_node: c1, from_port: 0, to_node: c2, to_port: 0 },
        Edge { from_node: m2, from_port: 0, to_node: c2, to_port: 1 },
        Edge { from_node: c2, from_port: 0, to_node: out, to_port: 0 },
    ]);
    for out in run(&g) {
        assert_left_greyed_right_kept(&out, "chained gates");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_ungated_graph_runs_as_it_is() {
    assert!(NodeGraph::default().with_gates_applied().is_none());
}
