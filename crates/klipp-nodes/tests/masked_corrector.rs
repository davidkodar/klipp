//! A matte wired to a Corrector's mask input limits the correction to
//! where the matte selects (#1234), whether the frame comes as CPU pixels
//! or already on the GPU (the viewer's and export's seeded source).
//!
//! The gate's blend reads the uncorrected picture, the source. Left on the
//! GPU, the source has no CPU pixels, and a blend run on the CPU took the
//! missing picture for an unwired port and passed the corrected frame
//! through: the whole frame corrected. A run must draw the gated frame or
//! refuse (`CpuSourceRequired`, which the renderer retries with the CPU
//! picture), never the ungated one. A missing GPU leaves the GPU half
//! unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::{ExecutorError, GraphExecutor};

const W: u32 = 64;
const H: u32 = 36;

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

fn link(a: NodeId, b: NodeId, port: u8) -> Edge {
    Edge { from_node: a, from_port: 0, to_node: b, to_port: port }
}

/// The gate producers: a window (it draws its own shapes, so it reads no
/// picture), the keyers, and a combination of a key and a window.
#[derive(Clone, Copy, Debug)]
enum Producer {
    /// A window fed by the source.
    Window,
    /// A window fed by nothing, as Codex's batch left it.
    UnfedWindow,
    Qualifier,
    LumaKey,
    Combined,
}

fn window() -> Node {
    use klipp_core::{Mask, MaskShape, MaskStack};
    let shape = MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.4, height: 0.4, rotation: 0.0 };
    node(NodeKind::Mask, NodeParams { masks: Some(MaskStack { masks: vec![Mask { shape, feather: 0.0, ..Mask::default() }] }), ..Default::default() })
}

fn luma_key() -> Node {
    node(NodeKind::LumaKey, NodeParams { luma_key: Some(klipp_core::LumaKeySettings { enabled: true, low: 0.3, high: 0.5, soft: 0.05, ..Default::default() }), ..Default::default() })
}

/// Source -> Corrector (exposure +1) -> Output, `p`'s matte into the
/// Corrector's port 1.
fn graph(p: Producer) -> NodeGraph {
    let s = node(NodeKind::Source, NodeParams::default());
    let c = node(NodeKind::Corrector, NodeParams { exposure: Some(1.0), ..Default::default() });
    let o = node(NodeKind::Output, NodeParams::default());
    let mut edges = vec![link(s.id, c.id, 0), link(c.id, o.id, 0)];
    let mut extra = Vec::new();
    let matte = match p {
        Producer::Window | Producer::UnfedWindow => {
            let m = window();
            if matches!(p, Producer::Window) {
                edges.push(link(s.id, m.id, 0));
            }
            extra.push(m);
            extra[0].id
        }
        Producer::Qualifier => {
            let q = node(NodeKind::Qualifier, NodeParams {
                qualifier: Some(klipp_core::QualifierSettings { enabled: true, hue_center: 30.0, hue_width: 90.0, sat_low: 0.05, ..Default::default() }),
                ..Default::default()
            });
            edges.push(link(s.id, q.id, 0));
            extra.push(q);
            extra[0].id
        }
        Producer::LumaKey => {
            let k = luma_key();
            edges.push(link(s.id, k.id, 0));
            extra.push(k);
            extra[0].id
        }
        Producer::Combined => {
            let (k, m) = (luma_key(), window());
            let c2 = node(NodeKind::CombineMattes, NodeParams::default());
            edges.extend([link(s.id, k.id, 0), link(s.id, m.id, 0), link(k.id, c2.id, 0), link(m.id, c2.id, 1)]);
            let id = c2.id;
            extra.extend([k, m, c2]);
            id
        }
    };
    edges.push(link(matte, c.id, 1));
    let mut nodes = vec![s, c, o.clone()];
    nodes.extend(extra);
    NodeGraph { output_node_id: o.id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// A warm ramp across the frame, brighter down it, as sRGB bytes (the
/// seed) and as the linear floats they decode to.
fn picture() -> (Vec<u8>, Vec<f32>) {
    let srgb: Vec<u8> = (0..W * H)
        .flat_map(|i| {
            let (x, y) = (i % W, i / W);
            let v = (40 + x * 3 + y * 2) as u8;
            [v, (v as u32 * 4 / 5) as u8, (v as u32 / 2) as u8, 255]
        })
        .collect();
    let linear = srgb.chunks_exact(4).flat_map(|p| {
        let f = |c: u8| klipp_color::transfer::srgb_to_linear(c as f32 / 255.0);
        [f(p[0]), f(p[1]), f(p[2]), 1.0]
    }).collect();
    (srgb, linear)
}

fn px(v: &[f32], x: u32, y: u32) -> [f32; 3] {
    let i = ((y * W + x) * 4) as usize;
    [v[i], v[i + 1], v[i + 2]]
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// `p`'s graph from the CPU picture, on `ex`.
fn from_cpu(ex: &mut GraphExecutor, p: Producer) -> Vec<f32> {
    ex.execute_linear(&picture().1, &graph(p), W, H, 0.0, 0, None, None).unwrap().0
}

/// `p`'s graph from the picture seeded on the GPU: the frame, or `None`
/// for a refusal the renderer retries with the CPU picture.
fn from_seed(p: Producer) -> Option<Vec<f32>> {
    let mut ex = GraphExecutor::new();
    assert!(ex.prime_chain_from_srgb8(&std::sync::Arc::new(picture().0), W, H), "seeded");
    match ex.execute_linear(&[], &graph(p), W, H, 0.0, 0, None, None) {
        Ok((out, _)) => Some(out),
        Err(ExecutorError::CpuSourceRequired) => None,
        Err(e) => panic!("{p:?}: {e:?}"),
    }
}

#[test]
fn a_window_on_a_correctors_mask_limits_it() {
    let (_, source) = picture();
    let mut cases = vec![("cpu", Some(from_cpu(&mut GraphExecutor::new_cpu_only(), Producer::Window)))];
    let mut ex = GraphExecutor::new();
    if ex.gpu_pipeline_or_init().is_some() {
        cases.push(("gpu", Some(from_cpu(&mut ex, Producer::Window))));
        cases.push(("gpu, window fed by nothing", Some(from_cpu(&mut GraphExecutor::new(), Producer::UnfedWindow))));
        cases.push(("gpu seeded", from_seed(Producer::Window)));
        cases.push(("gpu seeded, window fed by nothing", from_seed(Producer::UnfedWindow)));
    } else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
    }
    for (name, out) in cases {
        let Some(out) = out else {
            eprintln!("{name}: refused, retried with the CPU picture");
            continue;
        };
        let (inside, outside) = (px(&out, W / 2, H / 2), px(&out, 2, 2));
        let (plain_in, plain_out) = (px(&source, W / 2, H / 2), px(&source, 2, 2));
        assert!(inside[0] > plain_in[0] * 1.8, "{name}: inside the window the correction applies ({inside:?} from {plain_in:?})");
        assert!((outside[0] - plain_out[0]).abs() < 2e-3, "{name}: outside the window the source ({outside:?}, source {plain_out:?})");
    }
}

/// Every gate producer: from the seeded GPU source, the frame the same
/// graph draws from the CPU picture, or a refusal; never the frame
/// corrected whole.
#[test]
fn every_gate_from_a_seeded_source_draws_the_gated_frame_or_refuses() {
    let mut ex = GraphExecutor::new();
    if ex.gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let (_, source) = picture();
    for p in [Producer::Window, Producer::UnfedWindow, Producer::Qualifier, Producer::LumaKey, Producer::Combined] {
        let want = from_cpu(&mut GraphExecutor::new(), p);
        assert!(worst(&want, &source) > 0.05, "{p:?}: the case corrects something");
        match from_seed(p) {
            Some(got) => assert!(worst(&got, &want) < 3e-3, "{p:?}: the seeded frame differs from the CPU-sourced one by {}", worst(&got, &want)),
            None => eprintln!("{p:?}: refused, retried with the CPU picture"),
        }
    }
}
