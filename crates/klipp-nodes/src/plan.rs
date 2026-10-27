//! The executor's plan for a graph: everything about a run that follows
//! from the graph's shape rather than its values, worked out once and
//! kept (#1089, phase 2b M2 step 8).
//!
//! A run used to sort the graph, rewrite its gated corrections (cloning
//! the whole graph), count fan-out, recognise its secondaries and map
//! composite inputs on every frame. They depend only on:
//! - the structure: node ids, kinds, edges, wireless links, the output;
//! - which nodes are switched on at this frame (an `__enabled` key can
//!   switch one off at a frame, which changes what runs);
//! - whether each matte producer is armed (secondary recognition reads
//!   a keyer's on switch and refinements);
//! - the inspection roots: a matte tap, a matte view, visible thumbnails.
//!
//! [`plan_key`] hashes exactly those, so a parameter drag or an animated
//! value hits the cached plan and an edit to the shape compiles a new
//! one. The gated graph is kept with the plan, its structure fixed; each
//! run copies the live nodes' values into it ([`ExecPlan::refresh`]), so
//! the values a run reads are always this frame's.

use crate::executor::{effective_enabled, SecondaryChain};
use crate::graph::{NodeGraph, NodeId};
use crate::kinds::NodeKind;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// What a run needs that follows from the graph's shape.
pub(crate) struct ExecPlan {
    /// The graph with its gated corrections rewritten as blends, when it
    /// has any; `None` runs the graph as given.
    gated: Option<NodeGraph>,
    pub(crate) order: Vec<NodeId>,
    /// The nodes on the way to the output or an inspection root.
    pub(crate) feeding: HashSet<NodeId>,
    /// How many inputs each node's output feeds.
    pub(crate) fan_out: HashMap<NodeId, usize>,
    pub(crate) secondaries: Vec<SecondaryChain>,
    /// Each producer feeding a composite's port 1, to the composite.
    pub(crate) composite_port1: HashMap<NodeId, NodeId>,
    /// Every output the run can produce, numbered: a node's output port
    /// to its slot in [`Values`]. Grade nodes have one output, port 0.
    values: HashMap<(NodeId, u8), ValueId>,
}

/// An output's slot in a run's [`Values`], numbered by the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ValueId(u32);

impl ExecPlan {
    /// Work out the plan for `graph` with these inspection roots.
    pub(crate) fn compile(
        graph: &NodeGraph,
        matte_tap: Option<NodeId>,
        matte_preview_node: Option<NodeId>,
        visible_nodes: Option<&HashSet<NodeId>>,
    ) -> Result<Self, crate::executor::ExecutorError> {
        let gated = graph.with_gates_applied();
        let g = gated.as_ref().unwrap_or(graph);
        let order = g.topo_sort()?;
        let roots = std::iter::once(g.output_node_id)
            .chain(matte_tap)
            .chain(matte_preview_node)
            .chain(visible_nodes.into_iter().flat_map(|v| v.iter().copied()));
        let feeding = g.nodes_feeding(roots);
        let mut fan_out: HashMap<NodeId, usize> = HashMap::new();
        for edge in &g.edges {
            *fan_out.entry(edge.from_node).or_insert(0) += 1;
        }
        let secondaries = SecondaryChain::detect_all(g, &order, &fan_out);
        let composite_port1 = g
            .nodes
            .iter()
            .filter(|n| n.enabled && matches!(n.kind, NodeKind::Blend | NodeKind::Layer))
            .filter_map(|n| g.input_of(n.id, 1).map(|src| (src, n.id)))
            .collect();
        let values = g.nodes.iter().enumerate().map(|(i, n)| ((n.id, 0u8), ValueId(i as u32))).collect();
        Ok(Self { gated, order, feeding, fan_out, secondaries, composite_port1, values })
    }

    /// The slot of `node`'s output `port`, if the plan has that output.
    pub(crate) fn value(&self, node: NodeId, port: u8) -> Option<ValueId> {
        self.values.get(&(node, port)).copied()
    }

    /// An empty value table for one run of this plan.
    pub(crate) fn values(&self) -> Values<'_> {
        Values { plan: self, slots: vec![None; self.values.len()], copies: 0 }
    }

    /// Bring the kept gated graph's values up to `live`'s: every node it
    /// shares with `live` takes that node's parameters, keyframes, switch
    /// and label, and each inserted gate its correction's mix and channel. Its structure (edges, links, the inserted blends) is the
    /// plan's and stays.
    pub(crate) fn refresh(&mut self, live: &NodeGraph) {
        let Some(gated) = self.gated.as_mut() else { return };
        let by_id: HashMap<NodeId, &crate::graph::Node> = live.nodes.iter().map(|n| (n.id, n)).collect();
        let gates: HashMap<NodeId, NodeId> = live.nodes.iter().map(|n| (crate::graph::gate_id(n.id), n.id)).collect();
        for node in &mut gated.nodes {
            if let Some(from) = by_id.get(&node.id) {
                node.params.clone_from(&from.params);
                node.keyframes.clone_from(&from.keyframes);
                node.enabled = from.enabled;
                node.label.clone_from(&from.label);
            } else if let Some(corr) = gates.get(&node.id).and_then(|c| by_id.get(c)) {
                // An inserted gate takes its correction's mix and channel
                // as they are now (M3 step 17a).
                node.params = crate::graph::gate_params(&corr.params);
            }
        }
        gated.working_space_override = live.working_space_override;
    }

    /// The graph a run walks: the gated one, refreshed, or `live`.
    pub(crate) fn graph<'a>(&'a self, live: &'a NodeGraph) -> &'a NodeGraph {
        self.gated.as_ref().unwrap_or(live)
    }
}

/// The outputs a run has produced, by the plan's [`ValueId`]s: a node's
/// image, on the CPU (an empty vector when it is on the GPU chain or
/// kept there). Addressed by node, for the outputs every grade node has
/// (port 0), or by node and port.
pub(crate) struct Values<'p> {
    plan: &'p ExecPlan,
    slots: Vec<Option<std::sync::Arc<Vec<f32>>>>,
    /// Whole images copied between values in the run (none yet: a value
    /// passed through is shared).
    pub(crate) copies: u32,
}

impl Values<'_> {
    /// `node`'s output, if the run has made it.
    pub(crate) fn get(&self, node: &NodeId) -> Option<&std::sync::Arc<Vec<f32>>> {
        self.get_port(*node, 0)
    }

    pub(crate) fn get_port(&self, node: NodeId, port: u8) -> Option<&std::sync::Arc<Vec<f32>>> {
        self.plan.value(node, port).and_then(|v| self.slots[v.0 as usize].as_ref())
    }

    pub(crate) fn contains_key(&self, node: &NodeId) -> bool {
        self.get(node).is_some()
    }

    /// Take `node`'s output out of the table.
    pub(crate) fn remove(&mut self, node: &NodeId) -> Option<std::sync::Arc<Vec<f32>>> {
        self.plan.value(*node, 0).and_then(|v| self.slots[v.0 as usize].take())
    }

    /// Store `node`'s output. A node outside the plan has no output to
    /// keep: the run walks only the plan's nodes.
    pub(crate) fn insert(&mut self, node: NodeId, value: std::sync::Arc<Vec<f32>>) {
        if let Some(v) = self.plan.value(node, 0) {
            self.slots[v.0 as usize] = Some(value);
        } else {
            debug_assert!(false, "an output of a node the plan does not have: {node}");
        }
    }
}

/// The cache key of a run's plan: everything [`ExecPlan::compile`]
/// depends on, and nothing else.
pub(crate) fn plan_key(
    graph: &NodeGraph,
    clip_time_secs: f64,
    matte_tap: Option<NodeId>,
    matte_preview_node: Option<NodeId>,
    visible_nodes: Option<&HashSet<NodeId>>,
) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    graph.output_node_id.hash(&mut h);
    graph.nodes.len().hash(&mut h);
    for n in &graph.nodes {
        n.id.hash(&mut h);
        std::mem::discriminant(&n.kind).hash(&mut h);
        n.enabled.hash(&mut h);
        effective_enabled(n, clip_time_secs).hash(&mut h);
        n.input_links.hash(&mut h);
        n.output_link.hash(&mut h);
        matte_armed(graph, n).hash(&mut h);
        // A correction's mix decides whether it gets a gate (M3 step 17a).
        crate::graph::gets_a_gate(graph, n).hash(&mut h);
    }
    graph.edges.len().hash(&mut h);
    for e in &graph.edges {
        (e.from_node, e.from_port, e.to_node, e.to_port).hash(&mut h);
    }
    matte_tap.hash(&mut h);
    matte_preview_node.hash(&mut h);
    let mut visible: Vec<NodeId> = visible_nodes.map(|v| v.iter().copied().collect()).unwrap_or_default();
    visible.sort_unstable();
    visible_nodes.is_some().hash(&mut h);
    visible.hash(&mut h);
    h.finish()
}

/// What secondary recognition reads of a matte producer's settings
/// (`SecondaryChain::detect_all`), as bits; 0 for any other node.
fn matte_armed(graph: &NodeGraph, n: &crate::graph::Node) -> u8 {
    let p = &n.params;
    match n.kind {
        NodeKind::Qualifier => {
            let q = p.qualifier.as_ref();
            u8::from(q.is_some_and(|q| q.enabled))
                | u8::from(q.is_some_and(|q| q.refines())) << 1
                | u8::from(crate::executor::refinement_keyed(graph, n.id)) << 2
        }
        NodeKind::LumaKey => u8::from(p.luma_key.as_ref().is_some_and(|k| k.enabled)),
        NodeKind::Mask => u8::from(p.masks.as_ref().is_some_and(|ms| ms.masks.iter().any(|m| m.enabled))),
        NodeKind::DifferenceKey => u8::from(p.difference_key.as_ref().is_some_and(|d| d.enabled && d.has_plate())),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, Node};
    use crate::params::NodeParams;

    /// A chain of `n` Exposure nodes between the default graph's source
    /// and output.
    fn chain(n: usize) -> NodeGraph {
        let mut g = NodeGraph::default();
        let out = g.output_node_id;
        let mut prev = g.input_of(out, 0).unwrap();
        g.edges.retain(|e| !(e.to_node == out && e.to_port == 0));
        for i in 0..n {
            let id = NodeId::new_v4();
            g.nodes.push(Node {
                id, kind: NodeKind::Exposure, label: format!("E{i}"), enabled: true,
                params: NodeParams { exposure: Some(0.01 * i as f32), ..Default::default() },
                keyframes: Default::default(), output_link: None, input_links: Vec::new(),
            });
            g.edges.push(Edge { from_node: prev, from_port: 0, to_node: id, to_port: 0 });
            prev = id;
        }
        g.edges.push(Edge { from_node: prev, from_port: 0, to_node: out, to_port: 0 });
        g
    }

    /// L4 (M2 acceptance spec): a 50-node graph's plan compiles in under
    /// a millisecond, and its key, paid every frame, in a fraction of it.
    /// `cargo test --release -p klipp-nodes plan_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a timing probe; run in release"]
    fn plan_cost_for_fifty_nodes() {
        let g = chain(50);
        assert!(g.nodes.len() >= 50);
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let time = |f: &dyn Fn()| {
            median((0..41).map(|_| {
                let t = std::time::Instant::now();
                f();
                t.elapsed().as_secs_f64() * 1e3
            }).collect())
        };
        let compile = time(&|| {
            std::hint::black_box(ExecPlan::compile(&g, None, None, None).unwrap());
        });
        let key = time(&|| {
            std::hint::black_box(plan_key(&g, 0.5, None, None, None));
        });
        println!("50 nodes: compile {compile:.4} ms, key {key:.4} ms (median of 41)");
        assert!(compile < 1.0, "compile {compile} ms");
        assert!(key < 0.25, "key {key} ms");
    }
}
