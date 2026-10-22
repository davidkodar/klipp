//! The executor's plan is worked out once per graph shape and kept
//! (#1089, phase 2b M2 step 8; the M2 acceptance spec's section E).
//!
//! A run's order, gate rewrite, fan-out, secondaries and composite map
//! follow from the graph's shape, so a value change (a slider drag, an
//! animated parameter) must find the cached plan, and a change of shape
//! must compile a new one, once. The plan is the only place the graph is
//! sorted, so `plan_counts`' compiles are the sorts. Every frame is also
//! held to a fresh executor, which always compiles: the cache may change
//! the counts, never the picture.

use klipp_core::{Interpolation, Keyframe};
use klipp_nodes::graph::{Edge, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::{NodeKeyframeTrack, NodeParams};
use klipp_nodes::GraphExecutor;

const W: u32 = 32;
const H: u32 = 8;

fn frame() -> Vec<u8> {
    (0..W * H).flat_map(|i| [(i % 251) as u8, (i * 7 % 253) as u8, (i * 13 % 241) as u8, 255]).collect()
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

/// A primary grade and a qualifier-gated correction: the shape that has
/// its correction rewritten as a blend (`with_gates_applied`).
fn gated_graph() -> (NodeGraph, NodeId) {
    let mut g = NodeGraph::default();
    let (q, c) = g.ensure_masked_secondary(NodeKind::Corrector);
    for n in &mut g.nodes {
        if n.id == q {
            n.params.qualifier = Some(klipp_core::QualifierSettings { enabled: true, hue_center: 0.0, hue_width: 40.0, sat_low: 0.1, ..Default::default() });
        }
        if n.id == c {
            n.params.saturation = Some(0.2);
        }
    }
    (g, c)
}

fn node_mut(g: &mut NodeGraph, id: NodeId) -> &mut klipp_nodes::graph::Node {
    g.nodes.iter_mut().find(|n| n.id == id).unwrap()
}

/// `graph` at `t` through `ex`, and the same through a fresh executor.
fn run_against_fresh(ex: &mut GraphExecutor, graph: &NodeGraph, t: f64, tap: Option<NodeId>, fresh: fn() -> GraphExecutor) -> Vec<u8> {
    let got = ex.execute(&frame(), graph, W, H, t, (t * 25.0) as u64, tap, None).unwrap().0;
    let want = fresh().execute(&frame(), graph, W, H, t, (t * 25.0) as u64, tap, None).unwrap().0;
    assert_eq!(got, want, "the cached plan draws what a fresh one draws (t = {t})");
    got
}

fn executors() -> [(GraphExecutor, fn() -> GraphExecutor); 2] {
    [(GraphExecutor::new_cpu_only(), GraphExecutor::new_cpu_only), (GraphExecutor::new(), GraphExecutor::new)]
}

/// E1: ten frames of a still graph compile one plan, sort once and run
/// the gate rewrite once.
#[test]
fn a_still_graph_compiles_once() {
    let (g, _) = gated_graph();
    for (mut ex, fresh) in executors() {
        for f in 0..10 {
            run_against_fresh(&mut ex, &g, f as f64 / 25.0, None, fresh);
        }
        assert_eq!(ex.plan_counts(), (1, 9));
    }
}

/// E2: an animated value is a value, not a shape.
#[test]
fn an_animated_value_does_not_recompile() {
    let (mut g, c) = gated_graph();
    node_mut(&mut g, c).keyframes.tracks.push(NodeKeyframeTrack { param_name: "saturation".into(), keyframes: vec![key(0.0, 0.0), key(1.0, 2.0)] });
    for (mut ex, fresh) in executors() {
        let first = run_against_fresh(&mut ex, &g, 0.0, None, fresh);
        let mut changed = false;
        for f in 1..10 {
            changed |= run_against_fresh(&mut ex, &g, f as f64 / 9.0, None, fresh) != first;
        }
        assert!(changed, "the frames animate");
        assert_eq!(ex.plan_counts(), (1, 9));
    }
}

/// E3: an `__enabled` key switches a node off at a frame: that changes
/// what runs, so it is one compile, then hits.
#[test]
fn an_enabled_keyframe_bypasses_on_its_frame() {
    let (mut g, c) = gated_graph();
    node_mut(&mut g, c).keyframes.tracks.push(NodeKeyframeTrack {
        param_name: klipp_nodes::executor::ENABLED_TRACK.into(),
        keyframes: vec![key(0.0, 1.0), key(0.199, 1.0), key(0.2, 0.0)],
    });
    for (mut ex, fresh) in executors() {
        let on = run_against_fresh(&mut ex, &g, 0.0, None, fresh);
        for f in 1..10 {
            let out = run_against_fresh(&mut ex, &g, f as f64 / 25.0, None, fresh);
            assert_eq!(out == on, f < 5, "frame {f}: on before 0.2 s, off from it");
        }
        assert_eq!(ex.plan_counts(), (2, 8), "one compile per switch state");
    }
}

/// E4: a keyed node at its default at one frame and keyed away at the
/// next is drawn from its value at each frame, not from a plan made when
/// it did nothing.
#[test]
fn a_keyed_node_at_neutral_is_not_tunnelled_into_a_stale_plan() {
    let (mut g, c) = gated_graph();
    let corr = node_mut(&mut g, c);
    corr.params.saturation = Some(1.0);
    corr.keyframes.tracks.push(NodeKeyframeTrack { param_name: "saturation".into(), keyframes: vec![key(0.0, 1.0), key(1.0, 0.0)] });
    for (mut ex, fresh) in executors() {
        let neutral = run_against_fresh(&mut ex, &g, 0.0, None, fresh);
        let keyed = run_against_fresh(&mut ex, &g, 1.0, None, fresh);
        assert_ne!(neutral, keyed);
        assert_eq!(ex.plan_counts(), (1, 1));
    }
}

/// E5 (grade graphs): each change of shape compiles once, then hits; a
/// parameter drag between them only hits.
#[test]
fn a_structural_edit_recompiles_once() {
    let (base, c) = gated_graph();
    let edits: Vec<(&str, Box<dyn Fn(&mut NodeGraph)>)> = vec![
        ("a node added", Box::new(|g| {
            let out = g.output_node_id;
            let prev = g.input_of(out, 0).unwrap();
            let exp = NodeId::new_v4();
            g.nodes.push(klipp_nodes::graph::Node {
                id: exp, kind: NodeKind::Exposure, label: "Exposure".into(), enabled: true,
                params: NodeParams { exposure: Some(0.5), ..Default::default() },
                keyframes: Default::default(), output_link: None, input_links: Vec::new(),
            });
            g.edges.retain(|e| !(e.to_node == out && e.to_port == 0));
            g.edges.push(Edge { from_node: prev, from_port: 0, to_node: exp, to_port: 0 });
            g.edges.push(Edge { from_node: exp, from_port: 0, to_node: out, to_port: 0 });
        })),
        ("the gate's matte disconnected", Box::new(move |g| g.edges.retain(|e| !(e.to_node == c && e.to_port == 1)))),
        ("a node switched off", Box::new(move |g| node_mut(g, c).enabled = false)),
        ("the keyer disarmed", Box::new(|g| {
            for n in g.nodes.iter_mut().filter(|n| n.kind == NodeKind::Qualifier) {
                n.params.qualifier.as_mut().unwrap().enabled = false;
            }
        })),
    ];
    for (name, edit) in edits {
        for (mut ex, fresh) in executors() {
            let mut g = base.clone();
            run_against_fresh(&mut ex, &g, 0.0, None, fresh);
            // A drag: values only.
            node_mut(&mut g, c).params.saturation = Some(0.7);
            run_against_fresh(&mut ex, &g, 0.04, None, fresh);
            assert_eq!(ex.plan_counts(), (1, 1), "{name}: a drag hits");
            edit(&mut g);
            run_against_fresh(&mut ex, &g, 0.08, None, fresh);
            run_against_fresh(&mut ex, &g, 0.12, None, fresh);
            assert_eq!(ex.plan_counts(), (2, 2), "{name}: one compile for the edit, then hits");
        }
    }
}

/// E7: an inspection root (a matte tap) has its own plan, and returning
/// to the view without it finds the cached one.
#[test]
fn inspection_roots_get_their_own_plans() {
    let (g, _) = gated_graph();
    let q = g.nodes.iter().find(|n| n.kind == NodeKind::Qualifier).unwrap().id;
    for (mut ex, fresh) in executors() {
        run_against_fresh(&mut ex, &g, 0.0, None, fresh);
        run_against_fresh(&mut ex, &g, 0.0, Some(q), fresh);
        run_against_fresh(&mut ex, &g, 0.0, None, fresh);
        run_against_fresh(&mut ex, &g, 0.0, Some(q), fresh);
        assert_eq!(ex.plan_counts(), (2, 2));
    }
}

/// E8: two clips' graphs with the same node ids (a duplicated clip, then
/// one edited) never share a plan when their shapes differ, and when
/// their shapes are the same the shared plan draws each with its own
/// values.
#[test]
fn the_cache_never_serves_another_clips_plan() {
    let (a, c) = gated_graph();
    let mut same_shape = a.clone();
    node_mut(&mut same_shape, c).params.saturation = Some(1.8);
    let mut other_shape = a.clone();
    other_shape.edges.retain(|e| !(e.to_node == c && e.to_port == 1));
    for (mut ex, fresh) in executors() {
        let pa = run_against_fresh(&mut ex, &a, 0.0, None, fresh);
        let ps = run_against_fresh(&mut ex, &same_shape, 0.0, None, fresh);
        let po = run_against_fresh(&mut ex, &other_shape, 0.0, None, fresh);
        assert_ne!(pa, ps, "the same shape, its own values");
        assert_ne!(pa, po);
        let again = run_against_fresh(&mut ex, &a, 0.0, None, fresh);
        assert_eq!(again, pa);
        assert_eq!(ex.plan_counts(), (2, 2), "the shared shape hits; the other compiles its own");
    }
}

/// The run's outputs are addressed by the plan's value ids (step 9a): each
/// node on the way to the output is evaluated once, and a value passed
/// through is shared, never copied.
#[test]
fn each_node_is_evaluated_once_and_nothing_is_copied() {
    let (g, _) = gated_graph();
    // The gated graph as the run walks it: the correction becomes a blend.
    let walked = g.with_gates_applied().unwrap();
    let feeding = walked.nodes_feeding_output().len() as u32;
    for (mut ex, _) in executors() {
        ex.execute(&frame(), &g, W, H, 0.0, 0, None, None).unwrap();
        let stats = ex.last_run_chain_stats();
        assert_eq!(stats.evaluations, feeding, "{stats:?}");
        assert_eq!(stats.copies, 0);
    }
}

/// A chain of `n` Exposure nodes, the first `animated` keyed over a second.
fn long_chain(n: usize, animated: usize) -> NodeGraph {
    let mut g = NodeGraph::default();
    let nodes: Vec<_> = (0..n)
        .map(|i| {
            let mut node = klipp_nodes::graph::Node {
                id: NodeId::new_v4(), kind: NodeKind::Exposure, label: "Exposure".into(), enabled: true,
                params: NodeParams { exposure: Some(0.01), ..Default::default() },
                keyframes: Default::default(), output_link: None, input_links: vec![],
            };
            if i < animated {
                node.keyframes.tracks.push(NodeKeyframeTrack { param_name: "exposure".into(), keyframes: vec![key(0.0, -0.05), key(1.0, 0.05)] });
            }
            node
        })
        .collect();
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    let output = g.output_node_id;
    g.nodes.retain(|n| n.id == source || n.id == output);
    g.edges.clear();
    let mut prev = source;
    for node in &nodes {
        g.edges.push(Edge { from_node: prev, from_port: 0, to_node: node.id, to_port: 0 });
        prev = node.id;
    }
    g.edges.push(Edge { from_node: prev, from_port: 0, to_node: output, to_port: 0 });
    g.nodes.extend(nodes);
    g
}

/// The acceptance spec's L4 in counts: fifty nodes, ten of them animated,
/// played ten frames, compile one plan and find it nine times.
#[test]
fn a_long_animated_chain_compiles_once() {
    let g = long_chain(50, 10);
    let mut ex = GraphExecutor::new_cpu_only();
    for f in 0..10 {
        ex.execute(&frame(), &g, W, H, f as f64 / 9.0, f, None, None).unwrap();
    }
    assert_eq!(ex.plan_counts(), (1, 9));
}

/// L4's milliseconds, for the quiet window: a plan's compile, as a first
/// run's time over a cached run's on a 1x1 frame (50 nodes), and a cached
/// run's (parameter evaluation and 20 trivial passes: 20 nodes, 10
/// animated). Reported against < 1 ms and < 0.3 ms, never asserted.
#[test]
#[ignore = "perf probe: run explicitly, release, in a quiet window"]
fn plan_budgets() {
    let px = [128u8, 96, 64, 255];
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let time = |ex: &mut GraphExecutor, g: &NodeGraph, t: f64| {
        let start = std::time::Instant::now();
        ex.execute(&px, g, 1, 1, t, 0, None, None).unwrap();
        start.elapsed().as_secs_f64() * 1000.0
    };
    let fifty = long_chain(50, 10);
    let first = median((0..21).map(|_| time(&mut GraphExecutor::new_cpu_only(), &fifty, 0.5)).collect());
    let mut ex = GraphExecutor::new_cpu_only();
    time(&mut ex, &fifty, 0.5);
    let cached = median((0..21).map(|i| time(&mut ex, &fifty, i as f64 / 20.0)).collect());
    eprintln!("50 nodes: first run {first:.3} ms, cached {cached:.3} ms -> compile ~{:.3} ms (budget < 1 ms)", first - cached);
    let twenty = long_chain(20, 10);
    let mut ex = GraphExecutor::new_cpu_only();
    time(&mut ex, &twenty, 0.5);
    let eval = median((0..21).map(|i| time(&mut ex, &twenty, i as f64 / 20.0)).collect());
    eprintln!("20 nodes, 10 animated: cached run {eval:.3} ms (evaluation budget < 0.3 ms)");
}
