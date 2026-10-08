//! Validated, revision-bound removal and reordering shared by all writers.
//!
//! A proposal is a preview. Applying one re-plans against the live root, so
//! stale branch ownership can never authorize a destructive write.
use crate::topology::effective_edges;
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    ops::{self, GraphEdit, GraphOp},
    paths::Documents,
};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root, TrackPart};
use klipp_graph_format::{
    Edge, Graph, GroupPort, Node, NodeId, NodeSpec, PortCatalog, PortKey, PortType,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Remove,
    MoveAfter(NodeId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub path: NodePath,
    pub action: Action,
}

impl Request {
    fn op(&self) -> GraphOp {
        match self.action {
            Action::Remove => GraphOp::RemoveNode {
                path: self.path.clone(),
            },
            Action::MoveAfter(after) => GraphOp::MoveAfter {
                path: self.path.clone(),
                after,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub revision: u64,
    pub request: Request,
    pub edit: GraphEdit,
    pub ungated: Vec<Ungated>,
}

/// Build and validate on a disposable root; no live document is written.
pub fn propose(
    docs: &Documents,
    request: Request,
    revision: u64,
    catalog: &dyn PortCatalog,
) -> Result<Proposal, Refusal> {
    let refusal = |reason: String| Refusal {
        reason,
        reveal: request.path.clone(),
    };
    let before = ops::snapshot(docs, request.path.root).map_err(refusal)?;
    // Retain the precise boundary path for an Open in node view action.
    // The ops call below validates the complete root as well as this level.
    if let Some((graph, interface)) = level_for(docs, &request.path).map_err(refusal)? {
        rewrite(&mut graph.clone(), &request, Some(catalog), interface)?;
    }
    let mut copy = Documents::default();
    ops::put(&mut copy, request.path.root, &before).map_err(refusal)?;
    let edit = ops::apply_with_catalog(&mut copy, &request.op(), catalog).map_err(refusal)?;
    let ungated = ungated_consumers(&edit, catalog).map_err(refusal)?;
    Ok(Proposal {
        revision,
        request,
        edit,
        ungated,
    })
}

/// Revalidate even when a caller forgot to advance its revision. The returned
/// proposal describes the actual edit, including its live before snapshot.
pub fn apply(
    docs: &mut Documents,
    proposal: &Proposal,
    revision: u64,
    catalog: &dyn PortCatalog,
) -> Result<Proposal, Refusal> {
    let checked = propose(docs, proposal.request.clone(), revision, catalog)?;
    ops::put(docs, checked.edit.root, &checked.edit.after).map_err(|reason| Refusal {
        reason,
        reveal: checked.request.path.clone(),
    })?;
    Ok(checked)
}

fn level_for<'a>(
    docs: &'a Documents,
    path: &NodePath,
) -> Result<Option<(&'a Graph, &'a [GroupPort])>, String> {
    let mut graph = match path.root {
        Root::Clip { clip } => docs.clips.get(&clip).map(|p| &p.graph),
        Root::TimelineGrade { sequence } => docs.timeline.get(&sequence),
        Root::GroupStage { group, stage } => docs.stages.get(&(group, stage)),
        Root::Track {
            track,
            part: TrackPart::Grade,
        } => docs.track_grades.get(&track),
        Root::Track {
            part: TrackPart::Effects,
            ..
        } => return Ok(None),
    }
    .ok_or("the graph root no longer exists")?;
    let mut interface: &[GroupPort] = &[];
    let (_, through) = path
        .nodes
        .split_last()
        .ok_or("an empty path names no node")?;
    for id in through {
        let body = graph
            .nodes
            .iter()
            .find(|n| n.id == *id)
            .and_then(|n| n.group.as_ref())
            .ok_or_else(|| format!("{id} is not a group on this path"))?;
        graph = &body.graph;
        interface = &body.interface;
    }
    Ok(Some((graph, interface)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub reason: String,
    /// The path a UI can select when offering to open the node view.
    pub reveal: NodePath,
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({:?})", self.reason, self.reveal)
    }
}
impl std::error::Error for Refusal {}

struct LegacyCatalog;
impl PortCatalog for LegacyCatalog {
    fn node(&self, _: &str) -> Option<NodeSpec> {
        None
    }
}

fn protected(n: &Node) -> bool {
    n.origin.is_some()
        || matches!(
            n.type_id.as_str(),
            lt::SOURCE
                | lt::SCENE_TO_DISPLAY
                | lt::GROUP
                | lt::OUTPUT
                | lt::GROUP_INPUT
                | lt::GROUP_OUTPUT
                | lt::GROUP_FRAME
                | lt::PLACEMENT
        )
}
fn mask_input(n: &Node, port: &PortKey, catalog: &dyn PortCatalog) -> bool {
    matches!(port.as_str(), "mask" | "matte")
        || catalog.node_spec(n).is_some_and(|s| {
            s.inputs
                .iter()
                .any(|p| p.key == port.as_str() && p.ty == PortType::Matte)
        })
}

/// Replace one input deliberately. Remove its dormant wireless link too, so
/// a later disconnect cannot resurrect the cut connection.
fn connect(graph: &mut Graph, from: (NodeId, PortKey), to: (NodeId, PortKey)) {
    graph.edges.retain(|e| e.to != to);
    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == to.0) {
        node.wireless.input_links.remove(&to.1);
    }
    graph.edges.push(Edge { from, to });
}

/// Rewrite one graph level on the caller's disposable root. Whole-root port
/// and group validation is still performed by the ops transaction.
pub(crate) fn rewrite(
    graph: &mut Graph,
    request: &Request,
    catalog: Option<&dyn PortCatalog>,
    interface: &[GroupPort],
) -> Result<(), Refusal> {
    let catalog = catalog.unwrap_or(&LegacyCatalog);
    let path = &request.path;
    let (target, through) = path.nodes.split_last().ok_or_else(|| Refusal {
        reason: "an empty path names no node".into(),
        reveal: path.clone(),
    })?;
    let address = |id| NodePath::new(path.root, through.iter().copied().chain([id]));
    let refuse = |id, reason: &str| Refusal {
        reason: reason.into(),
        reveal: address(id),
    };
    let shared = |id| refuse(id, "This branch is shared. Open in node view");
    crate::stack_view::project_at(path.root, through, graph, catalog, None)
        .map_err(|e| refuse(*target, &format!("invalid graph: {e}")))?;
    let nodes: BTreeMap<_, _> = graph.nodes.iter().map(|n| (n.id, n)).collect();
    let node = *nodes
        .get(target)
        .ok_or_else(|| refuse(*target, "the node is no longer in this graph"))?;
    if protected(node) {
        return Err(refuse(
            *target,
            &format!(
                "{} [{}]: the graph's ends, structural nodes and Placement cannot be removed or moved",
                node.label, node.type_id
            ),
        ));
    }
    let edges = effective_edges(graph);
    let primary = catalog.primary_input(node);
    let feed = primary
        .as_ref()
        .and_then(|p| edges.iter().find(|e| e.to == (*target, p.clone())))
        .map(|e| e.from.clone());
    let images: Vec<_> = edges
        .iter()
        .filter(|e| e.from.0 == *target && !mask_input(nodes[&e.to.0], &e.to.1, catalog))
        .cloned()
        .collect();
    if images.len() > 1 {
        return Err(shared(*target));
    }
    if !images.is_empty() && feed.is_none() {
        return Err(refuse(*target, "the bypass has no declared primary input"));
    }

    // Main-spine nodes are shared ancestors, not attached dependencies. An
    // exclusive side graph may read them but never owns or deletes them.
    let mut spine = BTreeSet::new();
    let mut at = graph.output;
    loop {
        if !spine.insert(at) {
            return Err(refuse(at, "Cycle"));
        }
        let n = nodes.get(&at).ok_or_else(|| refuse(at, "missing node"))?;
        let Some(p) = catalog.primary_input(n) else {
            break;
        };
        let Some(e) = edges.iter().find(|e| e.to == (at, p.clone())) else {
            break;
        };
        at = e.from.0;
    }
    let mut owned = BTreeSet::from([*target]);
    let mut pending: Vec<_> = edges
        .iter()
        .filter(|e| e.to.0 == *target && primary.as_ref() != Some(&e.to.1))
        .map(|e| e.from.0)
        .collect();
    while let Some(id) = pending.pop() {
        if spine.contains(&id) || protected(nodes[&id]) || !owned.insert(id) {
            continue;
        }
        pending.extend(edges.iter().filter(|e| e.to.0 == id).map(|e| e.from.0));
    }
    // A published port is an external reader even when no wire is drawn at
    // this level. Protect input bindings too: their parent owns that feed.
    if let Some(p) = interface.iter().find(|p| owned.contains(&p.binding.node)) {
        return Err(shared(p.binding.node));
    }
    for e in &edges {
        if e.from.0 != *target && owned.contains(&e.from.0) && !owned.contains(&e.to.0) {
            return Err(shared(e.from.0));
        }
    }
    // A duplicate wireless sender may be dormant only because another
    // sender currently comes first. Moving the owned unit can change that
    // priority, so potential external readers also count as sharing.
    for id in &owned {
        let Some(name) = &nodes[id].wireless.output_link else {
            continue;
        };
        let external = graph.nodes.iter().any(|reader| {
            !owned.contains(&reader.id)
                && reader.wireless.input_links.iter().any(|(port, link)| {
                    link == name
                        && !graph
                            .edges
                            .iter()
                            .any(|e| e.to == (reader.id, port.clone()))
                })
        });
        let duplicate = graph
            .nodes
            .iter()
            .any(|n| n.id != *id && n.wireless.output_link.as_ref() == Some(name));
        if external
            && (*id != *target || (matches!(request.action, Action::MoveAfter(_)) && duplicate))
        {
            return Err(shared(*id));
        }
    }
    let mut after = graph.clone();
    match request.action {
        Action::Remove => {
            after.nodes.retain(|n| !owned.contains(&n.id));
            after
                .edges
                .retain(|e| !owned.contains(&e.from.0) && !owned.contains(&e.to.0));
            for e in edges
                .iter()
                .filter(|e| e.from.0 == *target && mask_input(nodes[&e.to.0], &e.to.1, catalog))
            {
                if let Some(n) = after.nodes.iter_mut().find(|n| n.id == e.to.0) {
                    n.wireless.input_links.remove(&e.to.1);
                    if let Some(flags) = &mut n.legacy_flags {
                        flags.alpha_one = false;
                        if *flags == Default::default() {
                            n.legacy_flags = None;
                        }
                    }
                }
            }
            if let (Some(feed), Some(reader)) = (feed, images.first()) {
                connect(&mut after, feed, reader.to.clone());
            }
        }
        Action::MoveAfter(destination) => {
            if destination == *target {
                return Ok(());
            }
            if owned.contains(&destination) {
                return Err(shared(destination));
            }
            let dest = nodes
                .get(&destination)
                .ok_or_else(|| refuse(destination, "the destination is no longer in this graph"))?;
            if matches!(dest.type_id.as_str(), lt::OUTPUT | lt::GROUP_OUTPUT) {
                return Err(refuse(destination, "nothing can move after Output"));
            }
            let port = primary
                .ok_or_else(|| refuse(*target, "the moved node has no declared primary input"))?;
            if let (Some(feed), Some(reader)) = (feed, images.first()) {
                connect(&mut after, feed, reader.to.clone());
            }
            let live = effective_edges(&after);
            let readers: Vec<_> = live
                .iter()
                .filter(|e| {
                    e.from.0 == destination
                        && e.to.0 != *target
                        && catalog.primary_input(nodes[&e.to.0]).as_ref() == Some(&e.to.1)
                })
                .cloned()
                .collect();
            if readers.len() != 1 {
                return Err(refuse(
                    destination,
                    "the destination has no unambiguous primary successor",
                ));
            }
            let reader = &readers[0];
            connect(
                &mut after,
                (destination, reader.from.1.clone()),
                (*target, port),
            );
            let output = images
                .first()
                .map(|e| e.from.1.clone())
                .or_else(|| {
                    catalog.node_spec(node).and_then(|s| {
                        s.outputs
                            .into_iter()
                            .find(|p| p.ty == PortType::Image)
                            .map(|p| PortKey::new(p.key))
                    })
                })
                .unwrap_or_else(|| "out".into());
            connect(&mut after, (*target, output), reader.to.clone());
            let moving: Vec<_> = after
                .nodes
                .iter()
                .filter(|n| owned.contains(&n.id))
                .cloned()
                .collect();
            after.nodes.retain(|n| !owned.contains(&n.id));
            let at = after
                .nodes
                .iter()
                .position(|n| n.id == destination)
                .unwrap()
                + 1;
            after.nodes.splice(at..at, moving);
        }
    }
    crate::stack_view::project_at(path.root, through, &after, catalog, None)
        .map_err(|e| refuse(*target, &format!("invalid result: {e}")))?;
    if matches!(request.action, Action::MoveAfter(_)) {
        let next_edges = effective_edges(&after);
        for id in &owned {
            let old_domain = picture_domain(graph, &edges, *id, catalog);
            let new_domain = picture_domain(&after, &next_edges, *id, catalog);
            if old_domain != new_domain && !catalog.allows_both_picture_domains(nodes[id]) {
                return Err(refuse(*id, &format!("{} cannot move from {old_domain} to {new_domain}: its catalog does not declare both domains", nodes[id].label)));
            }
        }
    }
    *graph = after;
    Ok(())
}

/// The domain on a node's primary input, relative to this level's explicit
/// scene-to-display boundary. Group boundaries are never crossed by a move.
fn picture_domain(
    graph: &Graph,
    edges: &[Edge],
    node: NodeId,
    catalog: &dyn PortCatalog,
) -> &'static str {
    let mut at = node;
    let mut seen = BTreeSet::new();
    while seen.insert(at) {
        let Some(n) = graph.nodes.iter().find(|n| n.id == at) else {
            break;
        };
        if n.type_id.as_str() == lt::SCENE_TO_DISPLAY {
            return "display-referred";
        }
        let Some(port) = catalog.primary_input(n) else {
            break;
        };
        let Some(edge) = edges.iter().find(|e| e.to == (at, port.clone())) else {
            break;
        };
        at = edge.from.0;
    }
    "scene-referred"
}

/// A compatibility-preserving mask-producer removal must tell the caller
/// which surviving readers are now ungated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ungated {
    pub path: NodePath,
    pub label: String,
}

pub fn ungated_consumers(
    edit: &GraphEdit,
    catalog: &dyn PortCatalog,
) -> Result<Vec<Ungated>, String> {
    if matches!(
        edit.root,
        Root::Track {
            part: TrackPart::Effects,
            ..
        }
    ) {
        return Ok(Vec::new());
    }
    let graph = |v: &serde_json::Value| -> Result<Graph, String> {
        let value = if matches!(edit.root, Root::Clip { .. }) {
            v.get("graph").ok_or("clip has no graph")?
        } else {
            v
        };
        serde_json::from_value(value.clone()).map_err(|e| e.to_string())
    };
    fn walk(
        before: &Graph,
        after: &Graph,
        root: Root,
        through: &[NodeId],
        catalog: &dyn PortCatalog,
        out: &mut Vec<Ungated>,
    ) {
        let old = effective_edges(before);
        let new = effective_edges(after);
        for node in &before.nodes {
            let Some(now) = after.nodes.iter().find(|n| n.id == node.id) else {
                continue;
            };
            let lost = old.iter().any(|e| {
                e.to.0 == node.id
                    && mask_input(node, &e.to.1, catalog)
                    && !new.iter().any(|n| n.to == e.to)
            });
            let path = NodePath::new(root, through.iter().copied().chain([node.id]));
            if lost {
                out.push(Ungated {
                    path: path.clone(),
                    label: match crate::topology::node_label(node) {
                        "" => node.type_id.to_string(),
                        label => label.to_owned(),
                    },
                });
            }
            if let (Some(old), Some(new)) = (&node.group, &now.group) {
                walk(&old.graph, &new.graph, root, &path.nodes, catalog, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &graph(&edit.before)?,
        &graph(&edit.after)?,
        edit.root,
        &[],
        catalog,
        &mut out,
    );
    Ok(out)
}
