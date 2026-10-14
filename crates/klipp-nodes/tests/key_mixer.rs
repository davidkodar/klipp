//! The Key Mixer (#898) mixes its mattes by the one rule, a plain sum
//! clamped once: `clamp(offset + output_gain * Σ gain * c, 0, 1)`, `c`
//! taken as `1 - c` when an input is inverted. Held against that rule
//! written out here, over soft windows that overlap, for 2, 3 and 9
//! inputs, gains either side of 0 and inverted inputs; an input with
//! nothing chosen selects everything, and a mixer with nothing wired
//! selects everything.

use klipp_core::{Mask, MaskShape, MaskStack};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::{GraphExecutor, KeyMixerInput, KeyMixerParams};

const W: u32 = 48;
const H: u32 = 27;

fn node(id: NodeId, kind: NodeKind, params: NodeParams) -> Node {
    Node { id, kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() }
}

/// A soft circle, its centre moved along the frame by `n`; `None` when
/// the window has nothing chosen.
fn window(n: usize) -> Option<MaskStack> {
    (n != usize::MAX).then(|| MaskStack {
        masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.2 + 0.07 * n as f32, center_y: 0.5, radius: 0.25 }, feather: 0.15, ..Mask::default() }],
    })
}

fn frame() -> Vec<f32> {
    (0..W * H).flat_map(|i| [0.1 + 0.6 * (i % W) as f32 / W as f32, 0.3, 0.5, 1.0]).collect()
}

/// Source → each window → the mixer's ports (in order) → Output, or a
/// window alone when `mixer` is `None`.
fn run(windows: &[Option<MaskStack>], mixer: Option<KeyMixerParams>) -> Vec<f32> {
    let (src, out) = (NodeId::new_v4(), NodeId::new_v4());
    let mut nodes = vec![node(src, NodeKind::Source, NodeParams::default()), node(out, NodeKind::Output, NodeParams::default())];
    let mut edges = Vec::new();
    let e = |from, to, to_port| Edge { from_node: from, from_port: 0, to_node: to, to_port };
    let into = match &mixer {
        Some(m) => {
            let id = NodeId::new_v4();
            nodes.push(node(id, NodeKind::KeyMixer, NodeParams { key_mixer: Some(m.clone()), ..Default::default() }));
            edges.push(e(id, out, 0));
            id
        }
        None => out,
    };
    for (port, w) in windows.iter().enumerate() {
        let id = NodeId::new_v4();
        nodes.push(node(id, NodeKind::Mask, NodeParams { masks: w.clone(), ..Default::default() }));
        edges.extend([e(src, id, 0), e(id, into, port as u8)]);
    }
    let graph = NodeGraph { output_node_id: out, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] };
    GraphExecutor::new_cpu_only().execute_linear(&frame(), &graph, W, H, 0.0, 0, None, None).unwrap().0
}

/// Each window's coverage on its own: 1 everywhere for nothing chosen.
fn coverage(w: &Option<MaskStack>) -> Vec<f32> {
    match w {
        Some(_) => run(std::slice::from_ref(w), None).chunks_exact(4).map(|p| p[0]).collect(),
        None => vec![1.0; (W * H) as usize],
    }
}

fn check(windows: Vec<Option<MaskStack>>, inputs: Vec<KeyMixerInput>, output_gain: f32, output_offset: f32, what: &str) {
    let params = KeyMixerParams { inputs: inputs.clone(), output_gain, output_offset };
    let got = run(&windows, Some(params));
    let covs: Vec<Vec<f32>> = windows.iter().map(coverage).collect();
    assert!(covs.iter().flatten().any(|c| *c > 0.05 && *c < 0.95), "{what}: the windows are soft somewhere");
    for i in 0..(W * H) as usize {
        let sum: f32 = covs.iter().enumerate().map(|(n, c)| {
            let input = inputs.get(n).copied().unwrap_or_default();
            input.gain * if input.invert { 1.0 - c[i] } else { c[i] }
        }).sum();
        let want = (output_offset + output_gain * sum).clamp(0.0, 1.0);
        assert!((got[i * 4] - want).abs() < 1e-6, "{what}: pixel {i}: {} vs {want}", got[i * 4]);
        assert_eq!(&got[i * 4..i * 4 + 4], &[got[i * 4], got[i * 4], got[i * 4], 1.0], "{what}: a matte, grey and opaque");
    }
}

fn gains(g: &[(f32, bool)]) -> Vec<KeyMixerInput> {
    g.iter().map(|&(gain, invert)| KeyMixerInput { gain, invert }).collect()
}

#[test]
fn two_mattes_sum_and_clamp() {
    check((0..2).map(window).collect(), vec![], 1.0, 0.0, "two at the defaults");
    check((0..2).map(window).collect(), gains(&[(1.0, false), (-1.0, false)]), 1.0, 0.0, "the second subtracted");
    check((0..2).map(window).collect(), gains(&[(0.5, false), (0.5, true)]), 1.5, -0.1, "halves, one inverted, the output lifted and lowered");
}

#[test]
fn three_and_nine_mattes() {
    check((0..3).map(window).collect(), gains(&[(0.7, false), (-0.4, true), (1.2, false)]), 0.8, 0.05, "three");
    let nine: Vec<_> = (0..9).map(|n| (if n % 3 == 1 { -0.5 } else { 0.4 }, n % 4 == 0)).collect();
    check((0..9).map(window).collect(), gains(&nine), 1.0, 0.0, "nine");
}

#[test]
fn an_input_with_nothing_chosen_selects_everything() {
    check(vec![window(0), window(usize::MAX)], gains(&[(1.0, false), (-0.5, false)]), 1.0, 0.0, "an empty window, subtracted by half");
}

#[test]
fn a_mixer_with_nothing_wired_selects_everything() {
    // No pixels: as a window with nothing chosen, everything.
    let got = run(&[], Some(KeyMixerParams::default()));
    assert!(got.is_empty() || got.chunks_exact(4).all(|p| p[0] == 1.0), "selects everything");
}

/// The names the inspector and `set_node_param` write.
#[test]
fn the_params_are_set_by_name() {
    let mut p = NodeParams::default();
    assert!(p.apply_override("key_mixer_gain_3", -0.5));
    assert!(p.apply_override("key_mixer_invert_1", 1.0));
    assert!(p.apply_override("key_mixer_output_gain", 2.0));
    assert!(p.apply_override("key_mixer_output_offset", -0.25));
    assert!(!p.apply_override("key_mixer_gain_x", 1.0));
    let m = p.key_mixer.expect("set");
    assert_eq!(m.inputs.len(), 4, "grown to the port named");
    assert_eq!((m.input(3).gain, m.input(1).invert, m.input(0)), (-0.5, true, KeyMixerInput::default()));
    assert_eq!((m.output_gain, m.output_offset), (2.0, -0.25));
}

/// Wiring a port past the mixer's count grows its settings to it, and
/// the canvas shows that many ports.
#[test]
fn a_wire_on_a_new_port_grows_the_mixer() {
    let mut n = node(NodeId::new_v4(), NodeKind::KeyMixer, NodeParams::default());
    assert_eq!(n.dynamic_input_port_count(), 2, "two to start");
    n.grow_to_port(4);
    assert_eq!(n.dynamic_input_port_count(), 5);
    assert_eq!(n.params.key_mixer.as_ref().unwrap().inputs, vec![KeyMixerInput::default(); 5]);
}

/// The Source wired straight into a Key Mixer is a matte of its red, on
/// the GPU-seeded route as on the CPU one: seeded, the picture has no
/// pixels on the CPU, which is no empty matte (that selects everything),
/// so the run is refused for the CPU picture, as a Layer Mixer's is.
#[test]
fn a_gpu_seeded_source_into_a_key_mixer_is_never_taken_for_everything() {
    let (src, km, out) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    let graph = NodeGraph {
        output_node_id: out,
        nodes: vec![
            node(src, NodeKind::Source, NodeParams::default()),
            node(km, NodeKind::KeyMixer, NodeParams::default()),
            node(out, NodeKind::Output, NodeParams::default()),
        ],
        edges: vec![Edge { from_node: src, from_port: 0, to_node: km, to_port: 0 }, Edge { from_node: km, from_port: 0, to_node: out, to_port: 0 }],
        node_positions: Default::default(),
        working_space_override: None,
        frames: vec![],
        sticky_notes: vec![],
    };
    let srgb: Vec<u8> = (0..W * H).flat_map(|i| { let v = (20 + (i % W) * 4) as u8; [v, v, v, 255] }).collect();
    let mut ex = GraphExecutor::new();
    if ex.gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    assert!(ex.prime_chain_from_srgb8(&std::sync::Arc::new(srgb), W, H), "seeded");
    match ex.execute_linear(&[], &graph, W, H, 0.0, 0, None, None) {
        Err(klipp_nodes::executor::ExecutorError::CpuSourceRequired) => {}
        Ok((got, _)) => assert!(got.is_empty() || got.chunks_exact(4).any(|p| p[0] < 0.99), "the picture's red, not everything selected"),
        Err(e) => panic!("{e:?}"),
    }
}
