//! Auto-layout for the node graph — Sugiyama-style hierarchical
//! layered layout for DAGs.
//!
//! Three phases:
//! 1. **Layer assignment** — longest path from source. Each node's
//!    layer = `1 + max(predecessor layer)`. Source nodes (no
//!    incoming edges) sit at layer 0.
//! 2. **Crossing reduction** — median heuristic, sweep top-down then
//!    bottom-up for a few passes. Reorders nodes within each layer
//!    to minimise wire crossings against neighbouring layers.
//! 3. **Coordinate assignment** — `x = layer * H_SPACING`, `y =
//!    rank_in_layer * V_SPACING`, with each layer vertically
//!    centred against the layout's tallest layer so sparse layers
//!    sit balanced rather than top-aligned.
//!
//! Disconnected components are laid out independently and stacked
//! vertically with a margin so they read as separate sub-graphs.
//!
//! Cycles return the original `node_positions` unchanged (treated
//! as no-op). The chain validator rejects cycles upstream so this
//! shouldn't fire in practice; defensive bail-out keeps the
//! function safe to call from any UI context.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::graph::{NodeGraph, NodeId, NodePos};

/// Horizontal spacing between layers in graph coordinates.
const H_SPACING: f32 = 200.0;
/// Vertical spacing between nodes within the same layer.
const V_SPACING: f32 = 80.0;
/// Vertical margin between disconnected components.
const COMPONENT_MARGIN: f32 = 160.0;
/// Crossing-reduction passes (top-down + bottom-up = 1 pass).
/// 4 is well past the convergence point on graphs we'll see.
const CROSSING_PASSES: usize = 4;

/// Recompute `node_positions` for `graph` using a Sugiyama-style
/// hierarchical layout. Returns a fresh map; caller is responsible
/// for installing it (typically via an undoable `SetNodeGraph`
/// edit so the user can revert with one keystroke).
///
/// Empty graphs return an empty map. Cycles return the existing
/// positions unchanged so the auto-layout button is a safe no-op
/// even in pathological state.
pub fn auto_layout(graph: &NodeGraph) -> HashMap<NodeId, NodePos> {
    if graph.nodes.is_empty() {
        return HashMap::new();
    }
    let topo = match graph.topo_sort() {
        Ok(t) => t,
        Err(_) => return graph.node_positions.clone(),
    };

    let mut positions: HashMap<NodeId, NodePos> = HashMap::new();
    let mut y_cursor: f32 = 0.0;

    // Lay out each connected component independently, stacked
    // vertically. The undirected-adjacency component finder treats
    // edges as bidirectional so a Source-less branch (e.g. an
    // ExternalMatte feeding a CombineMattes) is grouped with its
    // consumer instead of stranded as two singletons.
    for component in find_components(graph) {
        let comp_set: HashSet<NodeId> = component.iter().copied().collect();
        let topo_in_comp: Vec<NodeId> = topo
            .iter()
            .filter(|id| comp_set.contains(id))
            .copied()
            .collect();

        let layer_of = assign_layers(graph, &topo_in_comp);
        let max_layer = *layer_of.values().max().unwrap_or(&0);
        let mut layers: Vec<Vec<NodeId>> = vec![Vec::new(); max_layer + 1];
        for (nid, l) in &layer_of {
            layers[*l].push(*nid);
        }
        // Determinism: stable initial within-layer order so the
        // crossing-reduction sweep is reproducible across HashMap
        // iteration orders.
        for layer in layers.iter_mut() {
            layer.sort_by_key(|id| *id);
        }

        reduce_crossings(&mut layers, &graph.edges);

        let max_h = layers.iter().map(|l| l.len()).max().unwrap_or(1);
        // Each layer's nodes are symmetric around the component's
        // centreline (y = y_cursor): a layer of n nodes spans
        // [-(n-1)*V/2, +(n-1)*V/2]. Single-node layers land exactly
        // at the centreline.
        for (lx, layer) in layers.iter().enumerate() {
            let n = layer.len();
            let layer_top_y = -(n.saturating_sub(1) as f32) * V_SPACING * 0.5;
            for (ly, nid) in layer.iter().enumerate() {
                positions.insert(
                    *nid,
                    NodePos {
                        x: lx as f32 * H_SPACING,
                        y: y_cursor + layer_top_y + ly as f32 * V_SPACING,
                    },
                );
            }
        }

        // Advance y_cursor past this component's vertical extent
        // (max_h slots) plus the inter-component margin.
        let component_h = max_h.saturating_sub(1) as f32 * V_SPACING;
        y_cursor += component_h + COMPONENT_MARGIN;
    }

    positions
}

/// BFS over undirected adjacency to find connected components.
/// Returns one Vec<NodeId> per component; each node appears in
/// exactly one component.
fn find_components(graph: &NodeGraph) -> Vec<Vec<NodeId>> {
    let mut adj: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for n in &graph.nodes {
        adj.entry(n.id).or_default();
    }
    for e in &graph.edges {
        adj.entry(e.from_node).or_default().push(e.to_node);
        adj.entry(e.to_node).or_default().push(e.from_node);
    }

    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut components: Vec<Vec<NodeId>> = Vec::new();
    for n in &graph.nodes {
        if visited.contains(&n.id) {
            continue;
        }
        let mut component = Vec::new();
        let mut queue: VecDeque<NodeId> = VecDeque::from([n.id]);
        while let Some(id) = queue.pop_front() {
            if !visited.insert(id) {
                continue;
            }
            component.push(id);
            if let Some(neighbours) = adj.get(&id) {
                for nbr in neighbours {
                    if !visited.contains(nbr) {
                        queue.push_back(*nbr);
                    }
                }
            }
        }
        components.push(component);
    }
    components
}

/// Longest-path layering — each node's layer is one more than the
/// deepest predecessor's layer. Source nodes (no predecessors) get
/// layer 0. Caller passes a topo-sorted slice so predecessors are
/// always assigned before their successors.
fn assign_layers(graph: &NodeGraph, topo: &[NodeId]) -> HashMap<NodeId, usize> {
    let mut layer: HashMap<NodeId, usize> = HashMap::new();
    for nid in topo {
        let l = graph
            .edges
            .iter()
            .filter(|e| e.to_node == *nid)
            .filter_map(|e| layer.get(&e.from_node).copied())
            .max()
            .map(|p| p + 1)
            .unwrap_or(0);
        layer.insert(*nid, l);
    }
    layer
}

/// Median heuristic — for each layer, reorder nodes so each node
/// sits near the median position of its connected nodes in the
/// previous (top-down sweep) or next (bottom-up sweep) layer.
/// Disconnected nodes anchor at the end (median = +infinity).
fn reduce_crossings(layers: &mut Vec<Vec<NodeId>>, edges: &[crate::graph::Edge]) {
    if layers.len() < 2 {
        return;
    }
    for _ in 0..CROSSING_PASSES {
        // Top-down: order layer i by predecessor median in layer i-1.
        for i in 1..layers.len() {
            let prev_pos: HashMap<NodeId, f32> = layers[i - 1]
                .iter()
                .enumerate()
                .map(|(j, n)| (*n, j as f32))
                .collect();
            let mut scored: Vec<(NodeId, f32)> = layers[i]
                .iter()
                .map(|n| {
                    let mut preds: Vec<f32> = edges
                        .iter()
                        .filter(|e| e.to_node == *n)
                        .filter_map(|e| prev_pos.get(&e.from_node).copied())
                        .collect();
                    preds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let med = if preds.is_empty() {
                        f32::MAX
                    } else {
                        preds[preds.len() / 2]
                    };
                    (*n, med)
                })
                .collect();
            scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            layers[i] = scored.into_iter().map(|(n, _)| n).collect();
        }
        // Bottom-up: order layer i by successor median in layer i+1.
        for i in (0..layers.len() - 1).rev() {
            let next_pos: HashMap<NodeId, f32> = layers[i + 1]
                .iter()
                .enumerate()
                .map(|(j, n)| (*n, j as f32))
                .collect();
            let mut scored: Vec<(NodeId, f32)> = layers[i]
                .iter()
                .map(|n| {
                    let mut succs: Vec<f32> = edges
                        .iter()
                        .filter(|e| e.from_node == *n)
                        .filter_map(|e| next_pos.get(&e.to_node).copied())
                        .collect();
                    succs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let med = if succs.is_empty() {
                        f32::MAX
                    } else {
                        succs[succs.len() / 2]
                    };
                    (*n, med)
                })
                .collect();
            scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            layers[i] = scored.into_iter().map(|(n, _)| n).collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, Node};
    use crate::kinds::NodeKind;

    fn make_node(kind: NodeKind) -> Node {
        Node {
            id: NodeId::new_v4(),
            kind,
            label: kind.display_name().to_string(),
            enabled: true,
            params: Default::default(),
            keyframes: Default::default(),
            output_link: None,
            input_links: Vec::new(),
        }
    }

    #[test]
    fn empty_graph_returns_empty() {
        let mut g = NodeGraph::default();
        g.nodes.clear();
        g.edges.clear();
        g.node_positions.clear();
        assert!(auto_layout(&g).is_empty());
    }

    #[test]
    fn linear_chain_lays_out_left_to_right() {
        // Source → A → B → Output
        let mut g = NodeGraph::default();
        let src = make_node(NodeKind::Source);
        let a = make_node(NodeKind::Exposure);
        let b = make_node(NodeKind::Saturation);
        let out = make_node(NodeKind::Output);
        let (sid, aid, bid, oid) = (src.id, a.id, b.id, out.id);
        g.nodes = vec![src, a, b, out];
        g.edges = vec![
            Edge { from_node: sid, from_port: 0, to_node: aid, to_port: 0 },
            Edge { from_node: aid, from_port: 0, to_node: bid, to_port: 0 },
            Edge { from_node: bid, from_port: 0, to_node: oid, to_port: 0 },
        ];
        g.output_node_id = oid;
        let pos = auto_layout(&g);
        // Strictly increasing x — Source leftmost, Output rightmost.
        assert!(pos[&sid].x < pos[&aid].x);
        assert!(pos[&aid].x < pos[&bid].x);
        assert!(pos[&bid].x < pos[&oid].x);
        // Linear chain → all nodes share the same layer y=0 since
        // each layer has exactly one node and centring puts it at 0.
        assert_eq!(pos[&sid].y, 0.0);
        assert_eq!(pos[&aid].y, 0.0);
        assert_eq!(pos[&bid].y, 0.0);
        assert_eq!(pos[&oid].y, 0.0);
    }

    #[test]
    fn parallel_branches_share_layers() {
        // Source → A → Output
        //   ↓  ↗
        //   B
        let mut g = NodeGraph::default();
        let src = make_node(NodeKind::Source);
        let a = make_node(NodeKind::Exposure);
        let b = make_node(NodeKind::Saturation);
        let out = make_node(NodeKind::Output);
        let (sid, aid, bid, oid) = (src.id, a.id, b.id, out.id);
        g.nodes = vec![src, a, b, out];
        g.edges = vec![
            Edge { from_node: sid, from_port: 0, to_node: aid, to_port: 0 },
            Edge { from_node: sid, from_port: 0, to_node: bid, to_port: 0 },
            Edge { from_node: aid, from_port: 0, to_node: oid, to_port: 0 },
            Edge { from_node: bid, from_port: 0, to_node: oid, to_port: 1 },
        ];
        g.output_node_id = oid;
        let pos = auto_layout(&g);
        // Source and Output stay on the centreline because each
        // sits alone in its layer.
        assert_eq!(pos[&sid].y, 0.0);
        assert_eq!(pos[&oid].y, 0.0);
        // A and B share the middle layer; their x is identical and
        // their y values are symmetric around 0.
        assert_eq!(pos[&aid].x, pos[&bid].x);
        assert!((pos[&aid].y + pos[&bid].y).abs() < 0.01);
        // Source x < middle x < Output x
        assert!(pos[&sid].x < pos[&aid].x);
        assert!(pos[&aid].x < pos[&oid].x);
    }

    #[test]
    fn disconnected_components_stack_vertically() {
        // Component 1: Source1 → Output1
        // Component 2: Source2 → Output2
        let mut g = NodeGraph::default();
        let s1 = make_node(NodeKind::Source);
        let o1 = make_node(NodeKind::Output);
        let s2 = make_node(NodeKind::Source);
        let o2 = make_node(NodeKind::Output);
        let (s1id, o1id, s2id, o2id) = (s1.id, o1.id, s2.id, o2.id);
        g.nodes = vec![s1, o1, s2, o2];
        g.edges = vec![
            Edge { from_node: s1id, from_port: 0, to_node: o1id, to_port: 0 },
            Edge { from_node: s2id, from_port: 0, to_node: o2id, to_port: 0 },
        ];
        g.output_node_id = o1id;
        let pos = auto_layout(&g);
        // Each component starts at a different y_cursor, so the
        // two Source nodes have different y values even though they
        // both sit alone in their respective layer 0.
        assert_ne!(pos[&s1id].y, pos[&s2id].y);
        // But within a component, x increases.
        assert!(pos[&s1id].x < pos[&o1id].x);
        assert!(pos[&s2id].x < pos[&o2id].x);
    }

    #[test]
    fn cycle_returns_existing_positions() {
        let mut g = NodeGraph::default();
        let a = make_node(NodeKind::Exposure);
        let b = make_node(NodeKind::Saturation);
        let aid = a.id;
        let bid = b.id;
        g.nodes = vec![a, b];
        g.edges = vec![
            Edge { from_node: aid, from_port: 0, to_node: bid, to_port: 0 },
            Edge { from_node: bid, from_port: 0, to_node: aid, to_port: 0 },
        ];
        // Existing positions to preserve.
        g.node_positions
            .insert(aid, NodePos { x: 17.0, y: 17.0 });
        g.node_positions
            .insert(bid, NodePos { x: 42.0, y: 42.0 });
        let pos = auto_layout(&g);
        // Cycle bail-out: returns the input positions unchanged.
        assert_eq!(pos[&aid].x, 17.0);
        assert_eq!(pos[&bid].x, 42.0);
    }
}
