//! A freeform mask keyed over time (#74), played: the matte follows its
//! keys every frame on the GPU route as on the CPU, never a matte cached
//! from another moment, and a key edited while the playhead rests shows
//! at once.

use klipp_core::mask_path::{BezierPoint, PathEase, PathKey};
use klipp_core::{Mask, MaskShape, MaskStack};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 160;
const H: u32 = 90;

fn node(id: NodeId, kind: NodeKind, params: NodeParams) -> Node {
    Node { id, kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() }
}

/// A square of curved sides `off` across from the left.
fn shape(off: f32) -> Vec<BezierPoint> {
    [(0.1, 0.3), (0.3, 0.3), (0.3, 0.7), (0.1, 0.7)]
        .into_iter()
        .map(|(x, y)| BezierPoint { x: x + off, y, out_dx: 0.03, out_dy: -0.02, in_dx: -0.03, in_dy: 0.02, smooth: true })
        .collect()
}

fn masks(keys: Vec<PathKey>) -> MaskStack {
    let mut m = Mask { shape: MaskShape::Bezier { points: shape(0.0) }, feather: 0.01, ..Mask::default() };
    m.keyframes.path = keys;
    MaskStack { masks: vec![m] }
}

/// `Source ─► MaskCutout ◄─ Mask`, the source white.
fn graph(stack: MaskStack) -> (NodeGraph, NodeId) {
    let (src, mask, cut, out) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    let nodes = vec![
        node(src, NodeKind::Source, Default::default()),
        node(mask, NodeKind::Mask, NodeParams { masks: Some(stack), ..Default::default() }),
        node(cut, NodeKind::MaskCutout, Default::default()),
        node(out, NodeKind::Output, Default::default()),
    ];
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 200.0, y: 0.0 })).collect();
    let g = NodeGraph {
        output_node_id: out,
        nodes,
        edges: vec![
            Edge { from_node: src, from_port: 0, to_node: cut, to_port: 0 },
            Edge { from_node: mask, from_port: 0, to_node: cut, to_port: 1 },
            Edge { from_node: cut, from_port: 0, to_node: out, to_port: 0 },
        ],
        node_positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    };
    (g, mask)
}

fn white() -> Vec<f32> {
    vec![1.0; (W * H * 4) as usize]
}

/// Where the shown picture's middle is, across.
fn centre_x(px: &[f32]) -> f32 {
    let (mut sum, mut n) = (0.0f64, 0.0f64);
    for (i, p) in px.chunks_exact(4).enumerate() {
        let w = p[0] as f64;
        sum += w * (i as u32 % W) as f64;
        n += w;
    }
    (sum / n.max(1e-9)) as f32 / W as f32
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// Played a second at 24 frames on one executor, as playback runs it:
/// the window moves on every frame, and each frame is the CPU's picture
/// of that moment, from an executor that has drawn nothing before.
#[test]
fn a_keyed_shape_moves_every_frame_on_the_gpu_as_on_the_cpu() {
    let keys = vec![
        PathKey { time_secs: 0.0, points: shape(0.0), ease: PathEase::Linear },
        PathKey { time_secs: 1.0, points: shape(0.5), ease: PathEase::Linear },
    ];
    let (g, _) = graph(masks(keys));
    let mut gpu = GraphExecutor::new();
    let mut last = f32::MIN;
    let mut used_gpu = false;
    for f in 0..=24u64 {
        let t = f as f64 / 24.0;
        let out = gpu.execute_linear(&white(), &g, W, H, t, f, None, None).expect("runs").0;
        let stats = gpu.last_run_chain_stats();
        used_gpu |= stats.extends > 0 || stats.flushes > 0;
        let x = centre_x(&out);
        assert!(x > last + 0.01, "frame {f} at {t:.3} s: the window at {x}, after {last}: it stood still");
        last = x;
        let cpu = GraphExecutor::new_cpu_only().execute_linear(&white(), &g, W, H, t, f, None, None).expect("runs").0;
        assert!(worst(&out, &cpu) < 0.01, "frame {f}: GPU {} off the CPU's", worst(&out, &cpu));
    }
    if !used_gpu {
        eprintln!("no usable GPU: the run was the CPU's on both sides");
    }
}

/// A shape key edited while the playhead rests: the next frame at the
/// same moment draws the new shape, not the matte cached before.
#[test]
fn a_key_edited_at_rest_shows_at_once() {
    let keyed = |off: f32| vec![
        PathKey { time_secs: 0.0, points: shape(0.0), ease: PathEase::Linear },
        PathKey { time_secs: 1.0, points: shape(off), ease: PathEase::Linear },
    ];
    for gpu in [true, false] {
        let mut exec = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
        let (g, mask) = graph(masks(keyed(0.5)));
        let before = exec.execute_linear(&white(), &g, W, H, 1.0, 24, None, None).expect("runs").0;
        let mut edited = g.clone();
        edited.nodes.iter_mut().find(|n| n.id == mask).unwrap().params.masks = Some(masks(keyed(0.2)));
        let after = exec.execute_linear(&white(), &edited, W, H, 1.0, 24, None, None).expect("runs").0;
        assert!((centre_x(&after) - centre_x(&before)).abs() > 0.1, "gpu {gpu}: the edit didn't show: {} then {}", centre_x(&before), centre_x(&after));
    }
}
