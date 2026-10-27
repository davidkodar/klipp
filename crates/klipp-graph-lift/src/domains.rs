//! Colour domains on a picture's nodes as an edit leaves them (#1290):
//! the executor's rule ([`klipp_nodes::domains`]) applied to a lifted
//! graph, so an edit that would leave a node unable to take the pictures
//! it reads (a display step after the picture's own or after a plug-in,
//! scene- and display-referred pictures mixed) is refused before it lands.
//!
//! The picture's Source is scene-referred; so is a grade group's stage and
//! a track's grade, which grade a clip before its display step; the
//! timeline grade grades the composite, which is display-referred. Inside
//! a group its input carries the domain of what the group reads, and the
//! group gives its inside's output. A plug-in (OFX, frei0r), a generator
//! (its picture made from colours authored for it, not from what it
//! reads), and a node its catalog describes nothing of, declare no domain;
//! what is below a clip is the composite, display-referred, and a media
//! source's frame is display-referred too, given its display step as it is
//! read (#1857), so one wired before the picture's display step is
//! refused. A matte, by its
//! port's declared type, carries none. Checked with a catalog only, as the
//! ports and masks are.

use std::collections::HashMap;

use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::{Graph, Node, NodeId, PortCatalog, PortDirection, PortKey, PortType};
use klipp_nodes::domains::{step, DomainStep, PictureDomain};

/// A port of `n`, `out` for an output, is a matte by its declaration: an
/// effect's mask, a key's output, a group's published matte. Without one,
/// only a port named `mask` is.
fn is_matte(n: &Node, port: &PortKey, out: bool, catalog: &dyn PortCatalog) -> bool {
    if let Some(body) = n.group.as_ref().filter(|_| out) {
        if body.interface.iter().any(|p| p.direction == PortDirection::Output && &p.key == port && p.ty == PortType::Matte) {
            return true;
        }
    }
    match catalog.node_spec(n) {
        Some(spec) => {
            let ports = if out { &spec.outputs } else { &spec.inputs };
            ports.iter().any(|p| p.key == port.as_str() && p.ty == PortType::Matte)
        }
        None => !out && port.as_str() == "mask",
    }
}

/// Whether `n` is an effect whose picture isn't the one it reads and
/// declares no domain: a plug-in's (frei0r), or a generator's, made from
/// colours authored for it.
fn makes_its_own(n: &Node) -> bool {
    let ty = crate::effect_params::tag(n)
        .and_then(klipp_effects::registry::effect_type)
        .or_else(|| klipp_effects::registry::effect_types().find(|t| t.desc.id == n.type_id.as_str()));
    ty.is_some_and(|t| t.desc.category == klipp_effects::registry::Category::PlugIn || t.tag == "generator")
}

/// What `n` does to the domain, switched on (a group is walked inside).
/// The picture's own nodes keep it but the display step, which turns it;
/// a plug-in, and any node its catalog declares nothing about, declares
/// no domain.
fn node_step(n: &Node, catalog: &dyn PortCatalog) -> DomainStep {
    match n.type_id.as_str() {
        lt::SCENE_TO_DISPLAY => DomainStep::ToDisplay,
        lt::OFX | lt::FREI0R => DomainStep::Undeclared,
        lt::SOURCE | lt::OUTPUT | lt::GROUP | lt::GROUP_INPUT | lt::GROUP_OUTPUT | lt::GROUP_FRAME | lt::MERGE | lt::PLACEMENT | lt::TEMPORAL_DENOISE | lt::FRAME_BLEND => DomainStep::Keeps,
        // What is below the clip: the composite, display-referred (#1746,
        // #1867). Another media item's frame, given its display step as it
        // is read (#1857). A title, drawn as a title clip is (#2758).
        _ if matches!(crate::effect_params::tag(n), Some("clip_below" | "media_source" | "title_source")) => DomainStep::Gives(PictureDomain::Display),
        _ if makes_its_own(n) => DomainStep::Undeclared,
        _ if catalog.node_spec(n).is_none() => DomainStep::Undeclared,
        _ => DomainStep::Keeps,
    }
}

/// The node of `graph` that can't take the pictures it reads, and why, its
/// Source (or its group input) reading `source`; `None` when every node
/// can. Through groups, the fault names its node with the groups it is in.
pub fn fault(graph: &Graph, source: PictureDomain, catalog: &dyn PortCatalog) -> Option<(Vec<NodeId>, String)> {
    let mut path = Vec::new();
    walk(graph, source, catalog, &mut path).err()
}

/// Each node's output domain on `graph`, or the first fault.
fn walk(graph: &Graph, source: PictureDomain, catalog: &dyn PortCatalog, path: &mut Vec<NodeId>) -> Result<HashMap<NodeId, PictureDomain>, (Vec<NodeId>, String)> {
    let edges = crate::topology::effective_edges(graph);
    let by_id: HashMap<NodeId, &Node> = graph.nodes.iter().map(|n| (n.id, n)).collect();
    let mut out: HashMap<NodeId, PictureDomain> = HashMap::new();
    let mut visiting: Vec<NodeId> = Vec::new();
    fn visit(
        id: NodeId,
        edges: &[klipp_graph_format::Edge],
        by_id: &HashMap<NodeId, &Node>,
        source: PictureDomain,
        catalog: &dyn PortCatalog,
        path: &mut Vec<NodeId>,
        out: &mut HashMap<NodeId, PictureDomain>,
        visiting: &mut Vec<NodeId>,
    ) -> Result<PictureDomain, (Vec<NodeId>, String)> {
        if let Some(d) = out.get(&id) {
            return Ok(*d);
        }
        let Some(n) = by_id.get(&id).copied() else { return Ok(PictureDomain::Unknown) };
        // A loop is the structure check's to refuse; here it reads as
        // unknown.
        if visiting.contains(&id) {
            return Ok(PictureDomain::Unknown);
        }
        visiting.push(id);
        let primary = catalog.primary_input(n);
        let mut read: Vec<(bool, PictureDomain)> = Vec::new();
        for e in edges.iter().filter(|e| e.to.0 == id && !is_matte(n, &e.to.1, false, catalog)) {
            let Some(from) = by_id.get(&e.from.0) else { continue };
            if is_matte(from, &e.from.1, true, catalog) {
                continue;
            }
            let d = visit(e.from.0, edges, by_id, source, catalog, path, out, visiting)?;
            read.push((primary.as_ref() != Some(&e.to.1), d));
        }
        // The primary first.
        read.sort_by_key(|(not_primary, _)| *not_primary);
        let inputs: Vec<PictureDomain> = read.into_iter().map(|(_, d)| d).collect();
        let fault = |why: String| {
            let mut at = path.clone();
            at.push(id);
            (at, why)
        };
        let given = match n.type_id.as_str() {
            lt::SOURCE | lt::GROUP_INPUT | lt::GROUP_FRAME => source,
            _ if !n.enabled => inputs.first().copied().unwrap_or(PictureDomain::Unknown),
            _ => match &n.group {
                Some(body) => {
                    step(DomainStep::Keeps, &inputs).map_err(fault)?;
                    let carried = inputs.first().copied().unwrap_or(PictureDomain::Unknown);
                    path.push(id);
                    let inner = walk(&body.graph, carried, catalog, path);
                    path.pop();
                    let inner = inner?;
                    body.graph
                        .nodes
                        .iter()
                        .find(|m| m.type_id.as_str() == lt::GROUP_OUTPUT)
                        .and_then(|m| inner.get(&m.id).copied())
                        .unwrap_or(carried)
                }
                None => step(node_step(n, catalog), &inputs).map_err(fault)?,
            },
        };
        visiting.pop();
        out.insert(id, given);
        Ok(given)
    }
    for n in &graph.nodes {
        visit(n.id, &edges, &by_id, source, catalog, path, &mut out, &mut visiting)?;
    }
    Ok(out)
}

/// The domain a root's Source reads: scene-referred, but for the timeline
/// grade, which grades the composite after every clip's display step.
pub fn source_of(root: klipp_graph_format::path::Root) -> PictureDomain {
    match root {
        klipp_graph_format::path::Root::TimelineGrade { .. } => PictureDomain::Display,
        _ => PictureDomain::Scene,
    }
}

/// `after` refused when it leaves a node unable to take the pictures it
/// reads and `before` didn't: an edit isn't refused for what the graph
/// was already.
pub fn check_edit(before: &Graph, after: &Graph, source: PictureDomain, catalog: &dyn PortCatalog) -> Result<(), String> {
    match (fault(before, source, catalog), fault(after, source, catalog)) {
        (None, Some((at, why))) => {
            let node = at.last().map_or_else(String::new, |n| n.to_string());
            Err(format!("that would leave node {node} unable to take the pictures it reads: {why}"))
        }
        _ => Ok(()),
    }
}
