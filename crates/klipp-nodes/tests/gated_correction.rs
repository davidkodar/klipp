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

/// C6 (#1089, M2 step 9c): a grade node gated by a matte (a lifted gate
/// carries `legacy_flags.alpha_one`) writes alpha 1 wherever its gate
/// reads, the gate fully closed included, whatever alpha came in. A new
/// mask on an effect lerps alpha instead (`mask_mix_node.rs`).
#[test]
fn a_legacy_gate_keeps_alpha_one_at_both_ends() {
    let mut g = NodeGraph::default();
    let (q, c) = g.ensure_masked_secondary(NodeKind::Corrector);
    for n in &mut g.nodes {
        if n.id == q {
            n.params.qualifier = Some(klipp_core::QualifierSettings { enabled: true, hue_center: 0.0, hue_width: 60.0, sat_low: 0.1, ..Default::default() });
        }
        if n.id == c {
            n.params.saturation = Some(0.2);
        }
    }
    // Half-transparent: a gate that kept the input's alpha would show it.
    let source: Vec<f32> = (0..W * H).flat_map(|i| [(i % W) as f32 / W as f32, 0.3, 0.6, 0.5]).collect();
    let mut runs = vec![("cpu", GraphExecutor::new_cpu_only())];
    let mut gpu = GraphExecutor::new();
    if gpu.gpu_pipeline_or_init().is_some() {
        runs.push(("gpu", gpu));
    }
    for (name, mut ex) in runs {
        let out = ex.execute_linear(&source, &g, W, H, 0.0, 0, None, None).unwrap().0;
        let alphas: std::collections::BTreeSet<u32> = out.chunks_exact(4).map(|p| p[3].to_bits()).collect();
        assert_eq!(alphas.into_iter().collect::<Vec<_>>(), vec![1.0f32.to_bits()], "{name}: every gated pixel's alpha is 1");
    }
}

/// M3 step 17a: a qualifier gating a desaturating corrector, keyed on the
/// left half's red, the corrector given `blend` as its run setting.
fn gated(blend: Option<klipp_nodes::params::EffectBlend>) -> NodeGraph {
    let mut g = NodeGraph::default();
    let (q, c) = g.ensure_masked_secondary(NodeKind::Corrector);
    for n in &mut g.nodes {
        if n.id == q {
            n.params.qualifier = Some(klipp_core::QualifierSettings { enabled: true, hue_center: 0.0, hue_width: 60.0, sat_low: 0.1, ..Default::default() });
        }
        if n.id == c {
            n.params.saturation = Some(0.0);
            n.params.correction_blend = blend;
        }
    }
    g
}

fn blend(mix: f32, channel: klipp_effects::mask_mix::MaskChannel) -> Option<klipp_nodes::params::EffectBlend> {
    Some(klipp_nodes::params::EffectBlend { mix, channel })
}

/// Left half warm, right half cool, in linear light, with alpha a half
/// (a gate's alpha is 1 whatever comes in).
fn linear_frame() -> Vec<f32> {
    (0..W * H).flat_map(|i| if i % W < W / 2 { [0.7, 0.05, 0.05, 0.5] } else { [0.05, 0.05, 0.7, 0.5] }).collect()
}

/// `graph` on the CPU, and on the GPU when there is one.
fn run_linear(graph: &NodeGraph) -> Vec<(&'static str, Vec<f32>)> {
    let mut runs = vec![("cpu", GraphExecutor::new_cpu_only())];
    let mut gpu = GraphExecutor::new();
    if gpu.gpu_pipeline_or_init().is_some() {
        runs.push(("gpu", gpu));
    }
    runs.into_iter().map(|(name, mut ex)| (name, ex.execute_linear(&linear_frame(), graph, W, H, 0.0, 0, None, None).unwrap().0)).collect()
}

fn close(a: &[f32], b: &[f32], what: &str) {
    assert_eq!(a.len(), b.len());
    let worst = a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    assert!(worst < 2e-3, "{what}: worst difference {worst}");
}

/// IC10: a lifted gate (no blend) is the gate it always was, the same node
/// inserted and the same pixels as a whole gate read from red.
#[test]
fn a_lifted_gated_correction_is_unchanged() {
    let lifted = gated(None);
    let gate = lifted.with_gates_applied().unwrap().nodes.into_iter().find(|n| n.kind == NodeKind::ApplyMatte).unwrap();
    assert_eq!(gate.params, NodeParams::default(), "the gate a lifted correction gets, as before");
    let explicit = gated(blend(1.0, klipp_effects::mask_mix::MaskChannel::Red));
    for ((name, a), (_, b)) in run_linear(&lifted).into_iter().zip(run_linear(&explicit)) {
        close(&a, &b, name);
        assert!(a.chunks_exact(4).all(|p| p[3] == 1.0), "{name}: alpha 1");
    }
}

/// C3 for a gate: a half mix lands halfway between the picture without
/// the correction (mix 0) and the whole gate (mix 1).
#[test]
fn a_gates_mix_fades_its_correction() {
    use klipp_effects::mask_mix::MaskChannel::Red;
    let runs = |mix| run_linear(&gated(blend(mix, Red)));
    for (((name, none), (_, half)), (_, whole)) in runs(0.0).into_iter().zip(runs(0.5)).zip(runs(1.0)) {
        let input: Vec<f32> = linear_frame().chunks_exact(4).flat_map(|p| [p[0], p[1], p[2], 1.0]).collect();
        close(&none, &input, &format!("{name}: mix 0 is the input"));
        assert!(whole.chunks_exact(4).any(|p| (p[0] - p[1]).abs() < 1e-3), "{name}: mix 1 greys the key");
        let mid: Vec<f32> = none.iter().zip(&whole).map(|(a, b)| (a + b) / 2.0).collect();
        close(&half, &mid, &format!("{name}: mix 0.5 is halfway"));
    }
}

/// A grade matte is grey: its luminance is its red, so a gate reading
/// luminance draws what one reading red does.
#[test]
fn a_greyscale_gate_reads_the_same_by_luminance() {
    use klipp_effects::mask_mix::MaskChannel::{Luminance, Red};
    for ((name, red), (_, luma)) in run_linear(&gated(blend(1.0, Red))).into_iter().zip(run_linear(&gated(blend(1.0, Luminance)))) {
        close(&luma, &red, name);
    }
}

/// A correction with a mix and no matte fades against its input over the
/// whole frame, and keeps the alpha it was given.
#[test]
fn an_ungated_corrections_mix_fades_it_and_keeps_alpha() {
    let graph = |blend: Option<klipp_nodes::params::EffectBlend>| {
        let mut g = NodeGraph::default();
        let c = g.ensure_corrector();
        for n in &mut g.nodes {
            if n.id == c {
                n.params.saturation = Some(0.0);
                n.params.correction_blend = blend;
            }
        }
        g
    };
    use klipp_effects::mask_mix::MaskChannel::Red;
    assert!(graph(blend(1.0, Red)).with_gates_applied().is_none(), "a whole mix needs no gate");
    let input = linear_frame();
    for (((name, whole), (_, half)), (_, none)) in run_linear(&graph(None)).into_iter().zip(run_linear(&graph(blend(0.5, Red)))).zip(run_linear(&graph(blend(0.0, Red)))) {
        close(&none, &input, &format!("{name}: mix 0 is the input"));
        let mid: Vec<f32> = input.iter().zip(&whole).map(|(a, b)| (a + b) / 2.0).collect();
        close(&half, &mid, &format!("{name}: mix 0.5 is halfway"));
        assert!(half.chunks_exact(4).all(|p| p[3] == 0.5), "{name}: the input's alpha is kept");
    }
}
