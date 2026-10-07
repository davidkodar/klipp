//! An effect node mixed back over its input by its mix and its mask's
//! coverage (#1089, phase 2b M2, step 9c-1; the acceptance spec's C3 node
//! half and C5).
//!
//! Each case is held to `klipp_effects::mask_mix` applied to the pieces
//! the same graph gives without the mix: the input (I), the effect taken
//! whole (E) and the matte (M). On the CPU that is bit for bit; on the
//! GPU it is the CPU's within the effects' own CPU/GPU bar. A missing GPU
//! leaves the GPU cases unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it
//! fails.

use klipp_effects::mask_mix::{MaskChannel, MaskMix};
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::{EffectBlend, NodeParams};
use klipp_nodes::GraphExecutor;

const W: u32 = 48;
const H: u32 = 16;
/// GPU against CPU: the effects' and grade's own float differences.
const GPU_BAR: f32 = 2e-4;

fn frame() -> Vec<f32> {
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = ((i % W) as f32 / W as f32, (i / W) as f32 / H as f32);
            [x * 0.9, y * 0.7, (1.0 - x) * 0.6 + 0.05, 0.4 + 0.6 * y]
        })
        .collect()
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

fn qualifier(enabled: bool) -> Node {
    let mut q = node(NodeKind::Qualifier, NodeParams {
        qualifier: Some(klipp_core::QualifierSettings { enabled: true, hue_center: 30.0, hue_width: 60.0, sat_low: 0.05, ..Default::default() }),
        ..Default::default()
    });
    q.enabled = enabled;
    q
}

/// The pieces a case is built from.
enum Shape {
    /// The input: the grade alone.
    Input,
    /// The effect taken whole.
    Whole,
    /// The matte alone.
    Matte,
    /// The effect mixed by `blend`, its mask from the qualifier (`Some`,
    /// enabled or not) or unwired (`None`).
    Mixed(EffectBlend, Option<bool>),
}

fn effect() -> Vec<E> {
    vec![E::Gamma { gamma: 1.6 }, E::Invert { mix: 0.3 }]
}

/// Source -> Exposure -> [Effect] -> Output, a qualifier off the source.
fn graph(shape: &Shape) -> NodeGraph {
    let s = node(NodeKind::Source, NodeParams::default());
    let e = node(NodeKind::Exposure, NodeParams { exposure: Some(0.3), ..Default::default() });
    let o = node(NodeKind::Output, NodeParams::default());
    let link = |a: &Node, b: &Node, port| Edge { from_node: a.id, from_port: 0, to_node: b.id, to_port: port };
    let mut nodes = vec![s.clone(), e.clone()];
    let mut edges = vec![link(&s, &e, 0)];
    let last = match shape {
        Shape::Input => e,
        Shape::Matte => {
            let q = qualifier(true);
            edges.push(link(&s, &q, 0));
            nodes.push(q.clone());
            q
        }
        Shape::Whole | Shape::Mixed(..) => {
            let blend = match shape {
                Shape::Mixed(b, _) => Some(*b),
                _ => None,
            };
            let fx = node(NodeKind::Effect, NodeParams { effects: effect(), effect_blend: blend, ..Default::default() });
            edges.push(link(&e, &fx, 0));
            if let Shape::Mixed(_, Some(on)) = shape {
                let q = qualifier(*on);
                edges.push(link(&s, &q, 0));
                edges.push(link(&q, &fx, 1));
                nodes.push(q);
            }
            nodes.push(fx.clone());
            fx
        }
    };
    edges.push(link(&last, &o, 0));
    nodes.push(o.clone());
    NodeGraph { output_node_id: o.id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn run(ex: &mut GraphExecutor, shape: &Shape) -> Vec<f32> {
    ex.execute_linear(&frame(), &graph(shape), W, H, 0.5, 12, None, None).unwrap().0
}

fn rows(v: &[f32]) -> Vec<[f32; 4]> {
    v.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect()
}

/// `klipp_effects::mask_mix` on the CPU executor's own pieces.
fn oracle(blend: EffectBlend, masked: bool) -> Vec<f32> {
    let mut cpu = GraphExecutor::new_cpu_only();
    let (i, e, m) = (run(&mut cpu, &Shape::Input), run(&mut cpu, &Shape::Whole), run(&mut cpu, &Shape::Matte));
    let luma = klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::default());
    let mix = MaskMix::new(blend.mix, blend.channel, luma).unwrap();
    let mut out = vec![[0.0f32; 4]; i.len() / 4];
    let m = rows(&m);
    mix.apply(&rows(&i), &rows(&e), masked.then_some(m.as_slice()), &mut out).unwrap();
    out.into_iter().flatten().collect()
}

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn gpu() -> Option<GraphExecutor> {
    let mut ex = GraphExecutor::new();
    let ok = ex.gpu_pipeline_or_init().is_some();
    assert!(ok || !required(), "required, but there is no GPU");
    if !ok {
        eprintln!("UNVERIFIED: no GPU");
    }
    ok.then_some(ex)
}

const CHANNELS: [MaskChannel; 3] = [MaskChannel::Red, MaskChannel::Luminance, MaskChannel::Alpha];

/// C3 (node half): each channel reads the matte as the port says, and the
/// result is the oracle's.
#[test]
fn a_masked_effect_is_the_oracles_on_the_cpu_and_the_gpu() {
    for channel in CHANNELS {
        let blend = EffectBlend { mix: 0.6, channel };
        let want = oracle(blend, true);
        let mut cpu = GraphExecutor::new_cpu_only();
        assert_eq!(bits(&run(&mut cpu, &Shape::Mixed(blend, Some(true)))), bits(&want), "cpu, {channel:?}");
        if let Some(mut ex) = gpu() {
            run(&mut ex, &Shape::Whole);
            let whole_cpu = ex.last_run_chain_stats().cpu_nodes;
            let got = run(&mut ex, &Shape::Mixed(blend, Some(true)));
            let w = worst(&got, &want);
            assert!(w <= GPU_BAR, "gpu, {channel:?}: worst {w}");
            let stats = ex.last_run_chain_stats();
            assert_eq!(stats.effect_breaks, 0, "{channel:?}");
            // The qualifier keys on the chain too (#1266): no CPU node
            // beyond the whole effect's.
            assert_eq!(stats.cpu_nodes, whole_cpu, "{channel:?}: {stats:?}");
        }
    }
}

/// C5: an unwired mask is white (the effect at its mix); a switched-off
/// producer gives no coverage (the input, bit for bit).
#[test]
fn an_unwired_mask_is_white_and_a_disabled_producer_is_zero() {
    let blend = EffectBlend { mix: 0.4, channel: MaskChannel::Luminance };
    let white = oracle(blend, false);
    let mut cpu = GraphExecutor::new_cpu_only();
    let input = run(&mut cpu, &Shape::Input);
    assert_eq!(bits(&run(&mut cpu, &Shape::Mixed(blend, None))), bits(&white), "cpu: unwired is white");
    assert_eq!(bits(&run(&mut cpu, &Shape::Mixed(blend, Some(false)))), bits(&input), "cpu: an off producer leaves the input");
    if let Some(mut ex) = gpu() {
        let w = worst(&run(&mut ex, &Shape::Mixed(blend, None)), &white);
        assert!(w <= GPU_BAR, "gpu: unwired is white: worst {w}");
        let gpu_input = run(&mut ex, &Shape::Input);
        assert_eq!(bits(&run(&mut ex, &Shape::Mixed(blend, Some(false)))), bits(&gpu_input), "gpu: an off producer leaves the input");
    }
}

/// The ends are exact: mix 0 is the input, mix 1 under a white mask the
/// effect taken whole.
#[test]
fn the_mixs_ends_are_the_input_and_the_effect() {
    let none = EffectBlend { mix: 0.0, channel: MaskChannel::Red };
    let all = EffectBlend { mix: 1.0, channel: MaskChannel::Red };
    let mut cpu = GraphExecutor::new_cpu_only();
    assert_eq!(bits(&run(&mut cpu, &Shape::Mixed(none, Some(true)))), bits(&run(&mut cpu, &Shape::Input)));
    assert_eq!(bits(&run(&mut cpu, &Shape::Mixed(all, None))), bits(&run(&mut cpu, &Shape::Whole)));
    if let Some(mut ex) = gpu() {
        let (input, whole) = (run(&mut ex, &Shape::Input), run(&mut ex, &Shape::Whole));
        assert_eq!(bits(&run(&mut ex, &Shape::Mixed(none, Some(true)))), bits(&input), "gpu: mix 0");
        assert_eq!(bits(&run(&mut ex, &Shape::Mixed(all, None))), bits(&whole), "gpu: mix 1, white");
    }
}

/// A grade with a qualifier secondary (its kept picture and its matte in
/// the chain's own buffers) and, after it, a mixed effect whose mask comes
/// from another qualifier (its kept input and matte in pool slots): the
/// values never share a buffer, so the GPU draws the CPU's picture. Its
/// grade runs off the chain, so the chain starts at the mixed effect: the
/// matte reaches a node that starts a chain as one that extends it.
fn graded_with_a_secondary(blend: EffectBlend) -> NodeGraph {
    let mut g = NodeGraph::default();
    let (q, c) = g.ensure_masked_secondary(NodeKind::Corrector);
    for n in &mut g.nodes {
        if n.id == q {
            n.params.qualifier = Some(klipp_core::QualifierSettings { enabled: true, hue_center: 200.0, hue_width: 80.0, sat_low: 0.05, ..Default::default() });
        }
        if n.id == c {
            n.params.saturation = Some(0.3);
        }
    }
    let groups = [(NodeKind::Effect, &effect()[..])];
    g.splice_effects_before_output(&groups, |_| NodeId::new_v4());
    let fx = g.nodes.iter().find(|n| n.kind == NodeKind::Effect).unwrap().id;
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    let mask = qualifier(true);
    g.edges.push(Edge { from_node: source, from_port: 0, to_node: mask.id, to_port: 0 });
    g.edges.push(Edge { from_node: mask.id, from_port: 0, to_node: fx, to_port: 1 });
    g.nodes.push(mask);
    g.nodes.iter_mut().find(|n| n.id == fx).unwrap().params.effect_blend = Some(blend);
    g
}

#[test]
fn a_mixed_effect_beside_a_secondary_keeps_its_own_buffers() {
    let blend = EffectBlend { mix: 0.7, channel: MaskChannel::Red };
    let g = graded_with_a_secondary(blend);
    let want = GraphExecutor::new_cpu_only().execute_linear(&frame(), &g, W, H, 0.5, 12, None, None).unwrap().0;
    if let Some(mut ex) = gpu() {
        for _ in 0..3 {
            let got = ex.execute_linear(&frame(), &g, W, H, 0.5, 12, None, None).unwrap().0;
            let w = worst(&got, &want);
            assert!(w <= GPU_BAR, "worst {w}");
        }
        assert_eq!(ex.last_run_chain_stats().effect_breaks, 0);
    }
}

/// E5: wiring a mask is a change of shape (one compile), and a drag of
/// the mix is a value (none).
#[test]
fn wiring_a_mask_compiles_once_and_a_mix_drag_none() {
    for (mut ex, _) in [(GraphExecutor::new_cpu_only(), 0), (GraphExecutor::new(), 1)] {
        let unwired = graph(&Shape::Mixed(EffectBlend { mix: 0.5, channel: MaskChannel::Red }, None));
        let run_g = |ex: &mut GraphExecutor, g: &NodeGraph| ex.execute_linear(&frame(), g, W, H, 0.5, 12, None, None).unwrap();
        run_g(&mut ex, &unwired);
        let mut dragged = unwired.clone();
        dragged.nodes.iter_mut().find(|n| n.kind == NodeKind::Effect).unwrap().params.effect_blend = Some(EffectBlend { mix: 0.8, channel: MaskChannel::Red });
        run_g(&mut ex, &dragged);
        assert_eq!(ex.plan_counts(), (1, 1), "a mix drag is a value");
        let mut wired = dragged.clone();
        let fx = wired.nodes.iter().find(|n| n.kind == NodeKind::Effect).unwrap().id;
        let source = wired.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
        let q = qualifier(true);
        wired.edges.push(Edge { from_node: source, from_port: 0, to_node: q.id, to_port: 0 });
        wired.edges.push(Edge { from_node: q.id, from_port: 0, to_node: fx, to_port: 1 });
        wired.nodes.push(q);
        run_g(&mut ex, &wired);
        run_g(&mut ex, &wired);
        assert_eq!(ex.plan_counts(), (2, 2), "wiring the mask compiles once");
    }
}

/// D4: a mixed effect whose input also feeds a tap, with every node's
/// thumbnail shown: the tap and each thumbnail see their own values, the
/// output is the oracle's, and the GPU's thumbnails are its CPU twins'
/// within a level.
#[test]
fn fan_out_taps_and_thumbnails_see_their_own_values() {
    let blend = EffectBlend { mix: 0.6, channel: MaskChannel::Luminance };
    let g = graph(&Shape::Mixed(blend, Some(true)));
    let input = g.input_of(g.nodes.iter().find(|n| n.kind == NodeKind::Effect).unwrap().id, 0).unwrap();
    let visible: std::collections::HashSet<NodeId> = g.nodes.iter().map(|n| n.id).collect();
    let run_thumbs = |ex: &mut GraphExecutor| ex.execute_linear_with_thumbs(&frame(), &g, W, H, 0.5, 12, Some(input), None, Some(&visible)).unwrap();
    let (cpu_out, cpu_tap, cpu_thumbs) = run_thumbs(&mut GraphExecutor::new_cpu_only());
    assert_eq!(bits(&cpu_out), bits(&oracle(blend, true)), "cpu output");
    let mut cpu = GraphExecutor::new_cpu_only();
    assert_eq!(bits(&cpu_tap.expect("the tap")), bits(&run(&mut cpu, &Shape::Input)), "cpu tap: the effect's input");
    if let Some(mut ex) = gpu() {
        let (out, tap, thumbs) = run_thumbs(&mut ex);
        let w = worst(&out, &oracle(blend, true));
        assert!(w <= GPU_BAR, "gpu output: worst {w}");
        let tw = worst(&tap.expect("the tap"), &run(&mut ex, &Shape::Input));
        assert!(tw <= GPU_BAR, "gpu tap: worst {tw}");
        assert!(!cpu_thumbs.is_empty(), "the case shows thumbnails");
        assert_eq!(thumbs.keys().collect::<std::collections::BTreeSet<_>>(), cpu_thumbs.keys().collect(), "the same thumbnails");
        for (id, t) in &thumbs {
            let level = t.iter().zip(&cpu_thumbs[id]).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
            assert!(level <= 1, "thumbnail {id}: {level} levels from the cpu's");
        }
    }
}

/// The ratchet D6 holds to: the chain's flushes for the masked effect, the
/// secondary and the Blend in one grade. A change that needs more is a
/// regression to explain; one that needs fewer lowers this.
const D6_FLUSHES: u32 = 2;

/// D6: a masked effect, a qualifier secondary and a Blend in one grade
/// draw the CPU's picture on the GPU, with no break, and no more flushes
/// than the ratchet allows.
#[test]
fn a_masked_effect_inside_a_secondary_with_a_blend() {
    let blend = EffectBlend { mix: 0.8, channel: MaskChannel::Luminance };
    let mut g = graded_with_a_secondary(blend);
    let fx = g.nodes.iter().find(|n| n.kind == NodeKind::Effect).unwrap().id;
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    let before = g.input_of(fx, 0).unwrap();
    let mix = node(NodeKind::Blend, NodeParams { mix: Some(0.35), ..Default::default() });
    g.edges.retain(|e| !(e.to_node == fx && e.to_port == 0));
    g.edges.push(Edge { from_node: before, from_port: 0, to_node: mix.id, to_port: 0 });
    g.edges.push(Edge { from_node: source, from_port: 0, to_node: mix.id, to_port: 1 });
    g.edges.push(Edge { from_node: mix.id, from_port: 0, to_node: fx, to_port: 0 });
    g.nodes.push(mix);
    let want = GraphExecutor::new_cpu_only().execute_linear(&frame(), &g, W, H, 0.5, 12, None, None).unwrap().0;
    if let Some(mut ex) = gpu() {
        let got = ex.execute_linear(&frame(), &g, W, H, 0.5, 12, None, None).unwrap().0;
        let w = worst(&got, &want);
        assert!(w <= GPU_BAR, "worst {w}");
        let stats = ex.last_run_chain_stats();
        assert_eq!(stats.effect_breaks, 0, "{stats:?}");
        assert!(stats.flushes <= D6_FLUSHES, "{stats:?}");
    }
}

/// A mix that can't be read (not finite) gives no coverage: the node
/// passes its input on, bit for bit, and the run counts it. Never the
/// effect taken whole, which would draw a masked effect unmasked.
#[test]
fn an_unreadable_mix_skips_the_effect_and_says_so() {
    let blend = EffectBlend { mix: f32::NAN, channel: MaskChannel::Red };
    let mut runs = vec![GraphExecutor::new_cpu_only()];
    runs.extend(gpu());
    for mut ex in runs {
        let input = run(&mut ex, &Shape::Input);
        let got = run(&mut ex, &Shape::Mixed(blend, Some(true)));
        assert_eq!(bits(&got), bits(&input), "the input, not the effect whole");
        assert_eq!(ex.last_run_chain_stats().effect_mixes_refused, 1);
    }
}
