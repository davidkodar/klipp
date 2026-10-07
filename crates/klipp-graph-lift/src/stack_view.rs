//! A read-only projection of a picture's primary-input spine.
//!
//! Headers annotate processing order; they never sort it. Groups remain
//! one row. Side inputs keep their names, masks become chips, and image
//! branches stop at a shared spine node instead of duplicating its rows.
//! Placement is supplied by the executor, never inferred from effects.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use klipp_graph_format::lifted::{types as lt, Origin};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{
    Graph, Node, NodeId, NodeTypeId, ParamValue, PortCatalog, PortKey, PortType,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Source,
    Colour,
    Effects,
}

/// An executor-resolved boundary in the same addressed graph as the view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlacementBoundary {
    Before(NodePath),
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPlacement {
    pub boundary: PlacementBoundary,
    pub reason: String,
    pub provenance: Option<Origin>,
    pub plan_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Saved mode, without changing it when a view is opened.
    pub mode: Option<u32>,
    /// None means Pending. The row then retains its authored position;
    /// that position is not a claim about the executor's effective cut.
    pub resolved: Option<ResolvedPlacement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub input: PortKey,
    pub source: NodePath,
    pub output: PortKey,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    pub input: PortKey,
    pub source: NodePath,
    pub output: PortKey,
    pub stack: StackView,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub addr: NodePath,
    pub type_id: NodeTypeId,
    pub label: String,
    pub section: Section,
    pub enabled: bool,
    /// Route edits through placed-kind effect-list commands. These retained
    /// cards are not ordinary graph parameter writes on fields-stored clips.
    pub placed: bool,
    pub mixable: bool,
    pub mask_chips: Vec<Chip>,
    /// Includes every named side image input, including multi-input mixers.
    pub sub_stacks: Vec<Branch>,
    pub is_group: bool,
    pub provenance: Option<Origin>,
    pub read_only: bool,
    pub placement: Option<Placement>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Header(Section),
    Node(Box<Row>),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StackView {
    pub items: Vec<Item>,
    /// The shared ancestor at which a collapsed branch joins its parent.
    pub upstream: Option<NodePath>,
}

impl StackView {
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.items.iter().filter_map(|i| match i {
            Item::Node(r) => Some(r.as_ref()),
            Item::Header(_) => None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    DuplicateNode(NodeId),
    MissingNode(NodeId),
    MultipleInputs { node: NodeId, port: PortKey },
    MissingInput { node: NodeId, port: PortKey },
    Cycle,
    PlacementBoundaryNotOnSpine(NodePath),
    MultiplePlacements,
}

impl std::fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ProjectionError {}

type Endpoint = (NodeId, PortKey);

struct Projection<'a> {
    root: Root,
    through: &'a [NodeId],
    nodes: BTreeMap<NodeId, &'a Node>,
    inputs: BTreeMap<NodeId, BTreeMap<PortKey, Endpoint>>,
    catalog: &'a dyn PortCatalog,
}

impl Projection<'_> {
    fn addr(&self, node: NodeId) -> NodePath {
        NodePath::new(self.root, self.through.iter().copied().chain([node]))
    }

    fn stack(&self, output: NodeId, stop: &BTreeSet<NodeId>) -> Result<StackView, ProjectionError> {
        let mut path = Vec::new();
        let mut at = output;
        let mut upstream = None;
        loop {
            if stop.contains(&at) {
                upstream = Some(self.addr(at));
                break;
            }
            let node = self
                .nodes
                .get(&at)
                .ok_or(ProjectionError::MissingNode(at))?;
            path.push(at);
            let Some(port) = self.catalog.primary_input(node) else {
                break;
            };
            at = self
                .inputs
                .get(&at)
                .and_then(|ports| ports.get(&port))
                .ok_or(ProjectionError::MissingInput { node: at, port })?
                .0;
        }
        path.reverse();
        let shared: BTreeSet<_> = stop.iter().copied().chain(path.iter().copied()).collect();
        let mut rows = Vec::new();
        for id in path {
            let node = self.nodes[&id];
            if matches!(
                node.type_id.as_str(),
                lt::SOURCE | lt::OUTPUT | lt::GROUP_INPUT | lt::GROUP_OUTPUT | lt::GROUP_FRAME
            ) {
                continue;
            }
            let section = if self.catalog.needs_source_neighbours(node) {
                Section::Source
            } else if node.type_id.as_str() == lt::GROUP {
                Section::Colour
            } else {
                Section::Effects
            };
            let primary = self.catalog.primary_input(node);
            let spec = self.catalog.node_spec(node);
            let mut row = Row {
                addr: self.addr(id),
                type_id: node.type_id.clone(),
                label: node.label.clone(),
                section,
                enabled: node
                    .legacy_flags
                    .as_ref()
                    .and_then(|f| f.was_enabled)
                    .unwrap_or(node.enabled)
                    && (node.type_id.as_str() != klipp_graph_format::types::LEGACY_OFX
                        || node
                            .legacy
                            .as_ref()
                            .and_then(|l| l.payload.get("enabled"))
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(true)),
                mixable: spec.as_ref().is_some_and(|s| s.mixable),
                placed: matches!(
                    node.type_id.as_str(),
                    klipp_graph_format::types::LEGACY_TRANSFORM
                        | klipp_graph_format::types::LEGACY_OPACITY
                        | klipp_graph_format::types::LEGACY_SPEED_RAMP
                        | lt::FRAME_BLEND
                        | lt::TEMPORAL_DENOISE
                ),
                mask_chips: Vec::new(),
                sub_stacks: Vec::new(),
                is_group: node.group.is_some(),
                provenance: node.origin,
                read_only: node.origin.is_some(),
                placement: (node.type_id.as_str() == lt::PLACEMENT).then(|| Placement {
                    mode: match node.params.get(&"mode".into()) {
                        Some(ParamValue::Enum(m)) => Some(*m),
                        _ => None,
                    },
                    resolved: None,
                }),
            };
            for (port, (source, output)) in self.inputs.get(&id).into_iter().flatten() {
                if primary.as_ref() == Some(port) {
                    continue;
                }
                let is_mask = matches!(port.as_str(), "mask" | "matte")
                    || spec.as_ref().is_some_and(|s| {
                        s.inputs
                            .iter()
                            .any(|p| p.key == port.as_str() && p.ty == PortType::Matte)
                    });
                if is_mask {
                    row.mask_chips.push(Chip {
                        input: port.clone(),
                        source: self.addr(*source),
                        output: output.clone(),
                        label: self.nodes[source].label.clone(),
                    });
                } else {
                    row.sub_stacks.push(Branch {
                        input: port.clone(),
                        source: self.addr(*source),
                        output: output.clone(),
                        stack: self.stack(*source, &shared)?,
                    });
                }
            }
            rows.push(row);
        }
        Ok(with_headers(rows, upstream))
    }
}

fn with_headers(rows: Vec<Row>, upstream: Option<NodePath>) -> StackView {
    let mut view = StackView {
        items: Vec::new(),
        upstream,
    };
    let mut section = None;
    for row in rows {
        if section != Some(row.section) {
            view.items.push(Item::Header(row.section));
            section = Some(row.section);
        }
        view.items.push(Item::Node(Box::new(row)));
    }
    view
}

/// Project a root graph. `resolved` is the executor's result; absent means
/// Pending, never a guessed Auto placement position. No graph data changes.
pub fn project(
    root: Root,
    graph: &Graph,
    catalog: &dyn PortCatalog,
    resolved: Option<&ResolvedPlacement>,
) -> Result<StackView, ProjectionError> {
    project_at(root, &[], graph, catalog, resolved)
}

/// Project an explicitly entered group, preserving its containing path.
pub fn project_at(
    root: Root,
    through: &[NodeId],
    graph: &Graph,
    catalog: &dyn PortCatalog,
    resolved: Option<&ResolvedPlacement>,
) -> Result<StackView, ProjectionError> {
    if graph.is_empty() {
        return Ok(StackView::default());
    }
    let mut p = Projection {
        root,
        through,
        nodes: BTreeMap::new(),
        inputs: BTreeMap::new(),
        catalog,
    };
    for node in &graph.nodes {
        if p.nodes.insert(node.id, node).is_some() {
            return Err(ProjectionError::DuplicateNode(node.id));
        }
    }
    for edge in &graph.edges {
        for id in [edge.from.0, edge.to.0] {
            if !p.nodes.contains_key(&id) {
                return Err(ProjectionError::MissingNode(id));
            }
        }
        if p.inputs
            .entry(edge.to.0)
            .or_default()
            .insert(edge.to.1.clone(), edge.from.clone())
            .is_some()
        {
            return Err(ProjectionError::MultipleInputs {
                node: edge.to.0,
                port: edge.to.1.clone(),
            });
        }
    }
    // As lifted validation and rendering do: a drawn edge wins over a
    // wireless input; the first matching sender supplies an unwired port.
    for node in &graph.nodes {
        for (port, name) in &node.wireless.input_links {
            let inputs = p.inputs.entry(node.id).or_default();
            if inputs.contains_key(port) {
                continue;
            }
            if let Some(sender) = graph
                .nodes
                .iter()
                .find(|n| n.id != node.id && n.wireless.output_link.as_ref() == Some(name))
            {
                inputs.insert(port.clone(), (sender.id, "out".into()));
            }
        }
    }
    // Check all visible-level dependencies, including chips and wireless
    // branches, before walking any spine. No recursive graph DFS is needed.
    let mut incoming: BTreeMap<_, usize> = p.nodes.keys().map(|&n| (n, 0)).collect();
    let mut outgoing: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for (&to, ports) in &p.inputs {
        for (from, _) in ports.values() {
            *incoming.get_mut(&to).unwrap() += 1;
            outgoing.entry(*from).or_default().push(to);
        }
    }
    let mut ready: VecDeque<_> = incoming
        .iter()
        .filter_map(|(&n, &count)| (count == 0).then_some(n))
        .collect();
    let mut visited = 0;
    while let Some(n) = ready.pop_front() {
        visited += 1;
        for next in outgoing.get(&n).into_iter().flatten() {
            let count = incoming.get_mut(next).unwrap();
            *count -= 1;
            if *count == 0 {
                ready.push_back(*next);
            }
        }
    }
    if visited != p.nodes.len() {
        return Err(ProjectionError::Cycle);
    }
    let view = p.stack(graph.output, &BTreeSet::new())?;
    let Some(resolved) = resolved else {
        return Ok(view);
    };
    let mut rows: Vec<_> = view
        .items
        .into_iter()
        .filter_map(|i| match i {
            Item::Node(r) => Some(*r),
            _ => None,
        })
        .collect();
    let positions: Vec<_> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.placement.is_some().then_some(i))
        .collect();
    if positions.len() > 1 {
        return Err(ProjectionError::MultiplePlacements);
    }
    let Some(&position) = positions.first() else {
        return Ok(with_headers(rows, view.upstream));
    };
    let mut row = rows.remove(position);
    row.placement.as_mut().unwrap().resolved = Some(resolved.clone());
    let index = match &resolved.boundary {
        PlacementBoundary::End => rows.len(),
        PlacementBoundary::Before(path) => rows
            .iter()
            .position(|r| &r.addr == path)
            .ok_or_else(|| ProjectionError::PlacementBoundaryNotOnSpine(path.clone()))?,
    };
    rows.insert(index, row);
    Ok(with_headers(rows, view.upstream))
}
