//! A Lut3D node's colour-space tags on the CPU and the GPU.
//!
//! A tag names the primaries the LUT was built in; the lookup itself is
//! always on the sRGB encoding. The CPU bracketed the lookup with the
//! primaries matrices and the shader did not, so in an ACEScg project
//! an untagged `.cube` looked one way in the viewer and another in a
//! CPU export or a baked grade (#422). These run the node itself
//! through the executor, both ways, where the two used to part — on
//! the chained GPU path, which is the one a graph takes; the one-node
//! GPU fallback computes the same bracket and hands it to the same
//! shader.
//!
//! CPU against GPU cannot tell a bracket that goes the wrong way from
//! one that goes the right way, since both sides take the same pair of
//! matrices. So the first test holds them to what the bracket is *for*:
//! the same colour, given in Rec.709 or in ACEScg, gets the same look.

use klipp_core::WorkingColorspace;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 128;
const H: u32 = 64;

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

fn lut_graph(params: NodeParams) -> NodeGraph {
    let (src, n, out) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    let nodes = vec![
        node(src, NodeKind::Source, Default::default()),
        node(n, NodeKind::Lut3D, params),
        node(out, NodeKind::Output, Default::default()),
    ];
    let node_positions = nodes
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id, NodePos { x: i as f32 * 200.0, y: 0.0 }))
        .collect();
    NodeGraph {
        output_node_id: out,
        nodes,
        edges: vec![
            Edge { from_node: src, from_port: 0, to_node: n, to_port: 0 },
            Edge { from_node: n, from_port: 0, to_node: out, to_port: 0 },
        ],
        node_positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    }
}

/// A ramp from black to white across, hue turning down the frame.
fn frame() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let t = x as f32 / (W - 1) as f32;
            let u = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[t * t, t * (0.6 + 0.4 * u), t * (1.0 - 0.5 * u), 1.0]);
        }
    }
    v
}

fn look(input: Option<WorkingColorspace>, output: Option<WorkingColorspace>) -> NodeParams {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/luts/fuji-xtrans/fuji_xtrans_iii_classic_chrome.cube");
    NodeParams {
        lut_path: Some(path),
        lut_opacity: Some(0.8),
        lut_input_space: input,
        lut_output_space: output,
        ..Default::default()
    }
}

fn run_on(input: &[f32], g: &NodeGraph, ws: WorkingColorspace, gpu: bool) -> (Vec<f32>, klipp_nodes::executor::ChainStats) {
    let mut exec = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
    exec.set_working_colorspace(ws);
    let out = exec.execute_linear(input, g, W, H, 0.0, 0, None, None).expect("graph runs").0;
    (out, exec.last_run_chain_stats())
}

/// Each pixel through a 3×3 primaries matrix, alpha kept.
fn convert(px: &[f32], m: [[f32; 3]; 3]) -> Vec<f32> {
    px.chunks_exact(4)
        .flat_map(|p| {
            let o = klipp_color::primaries::matrix3_mul_vec(m, [p[0], p[1], p[2]]);
            [o[0], o[1], o[2], p[3]]
        })
        .collect()
}

fn run(g: &NodeGraph, ws: WorkingColorspace, gpu: bool) -> (Vec<f32>, klipp_nodes::executor::ChainStats) {
    let mut exec = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
    exec.set_working_colorspace(ws);
    let out = exec
        .execute_linear(&frame(), g, W, H, 0.0, 0, None, None)
        .expect("graph runs")
        .0;
    (out, exec.last_run_chain_stats())
}

/// The worst difference in display levels (of 255).
fn worst_levels(a: &[f32], b: &[f32]) -> f32 {
    let enc = |v: f32| klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0));
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .flat_map(|(p, q)| (0..3).map(move |i| (enc(p[i]) - enc(q[i])).abs()))
        .fold(0.0f32, f32::max)
        * 255.0
}

/// Returns the GPU result, or `None` with no usable GPU.
fn same_both_ways(name: &str, g: &NodeGraph, ws: WorkingColorspace) -> Option<Vec<f32>> {
    let (cpu, _) = run(g, ws, false);
    let (gpu, stats) = run(g, ws, true);
    if stats.extends == 0 && stats.flushes == 0 {
        eprintln!("skipping {name}: no usable GPU");
        return None;
    }
    let w = worst_levels(&cpu, &gpu);
    eprintln!("{name}: worst CPU vs GPU = {w:.3} levels");
    assert!(w < 1.0, "{name}: CPU and GPU differ by {w:.2} display levels");
    Some(gpu)
}

/// The bracket's direction: a picture given in Rec.709, and the same
/// picture given in ACEScg, come out as the same look. A swapped,
/// transposed or inverted matrix fails this; CPU against GPU does not
/// see it. Checked on the CPU first, so a machine with no GPU still
/// guards it, then on the GPU when there is one.
#[test]
fn the_same_colour_gets_the_same_look_in_either_project() {
    use klipp_color::primaries::working_space_conversion_matrix as m;
    let (to_ap1, to_709) = (
        m(WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG).unwrap(),
        m(WorkingColorspace::AcesCG, WorkingColorspace::Rec709Linear).unwrap(),
    );
    let g = lut_graph(look(None, None));
    let src = frame();
    let as_ap1 = convert(&src, to_ap1);
    for gpu in [false, true] {
        let (rec709, stats) = run_on(&src, &g, WorkingColorspace::Rec709Linear, gpu);
        if gpu && stats.extends == 0 && stats.flushes == 0 {
            eprintln!("no usable GPU; the CPU half ran");
            return;
        }
        let (aces, _) = run_on(&as_ap1, &g, WorkingColorspace::AcesCG, gpu);
        let w = worst_levels(&rec709, &convert(&aces, to_709));
        eprintln!("same colour, either project (gpu {gpu}): worst {w:.3} levels");
        assert!(w < 0.5, "gpu {gpu}: the look differs by {w:.2} levels between a Rec.709 and an ACEScg project");
    }
}

#[test]
fn an_untagged_look_in_an_acescg_project_is_the_same_both_ways() {
    let g = lut_graph(look(None, None));
    let Some(aces) = same_both_ways("untagged, ACEScg", &g, WorkingColorspace::AcesCG) else { return };
    // And the bracket is really there: the look reads Rec.709 primaries,
    // so in ACEScg it must not be the Rec.709 project's numbers.
    let (rec709, _) = run(&g, WorkingColorspace::Rec709Linear, true);
    assert!(worst_levels(&aces, &rec709) > 2.0, "the GPU ignored the working space");
}

#[test]
fn a_retagged_look_is_the_same_both_ways() {
    let g = lut_graph(look(Some(WorkingColorspace::AcesCG), Some(WorkingColorspace::Bt2020Linear)));
    same_both_ways("ACEScg in, BT.2020 out, Rec.709 project", &g, WorkingColorspace::Rec709Linear);
    same_both_ways("ACEScg in, BT.2020 out, ACEScg project", &g, WorkingColorspace::AcesCG);
}

#[test]
fn an_untagged_look_in_a_rec709_project_is_the_same_both_ways() {
    let g = lut_graph(look(None, None));
    same_both_ways("untagged, Rec.709", &g, WorkingColorspace::Rec709Linear);
}
